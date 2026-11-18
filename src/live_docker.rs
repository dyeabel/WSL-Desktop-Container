//! End-to-end test pro **Docker** v čerstvé WSL distribuci.
//!
//! Spuštění (stahuje Debian ~100 MB a balíčky Dockeru, trvá několik minut):
//! `cargo test live_docker_e2e -- --ignored --nocapture`
//!
//! Test vytvoří distribuci `WLS-Test-Docker`, nainstaluje Docker stejnou cestou jako aplikace,
//! projde API bez ověření i s TLS, kontejnery, události, build, compose, Recreate a shim,
//! a nakonec distribuci odregistruje. Nezasahuje do běžícího enginu ani konfigurace:
//! - porty 2385 (místo 2375/2376, které může používat jiný engine – všechny distribuce WSL
//!   sdílí jednu síť),
//! - `iptables: false` v daemon.json – Docker jinak mění pravidla firewallu sdíleného síťového
//!   jmenného prostoru WSL a mohl by rozbít síť kontejnerů jiného enginu,
//! - `WLS_DATA_DIR` = dočasná složka (konfigurace, certifikáty; DOCKER_HOST se nemění).

use crate::config::Config;
use crate::docker::{Docker, Event};
use crate::engine::Engine;
use crate::wsl;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DISTRO: &str = "WLS-Test-Docker";
const PORT: u16 = 2385;

struct Cleanup {
    data: std::path::PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if std::env::var("WLS_E2E_KEEP").is_ok() {
            eprintln!("WLS_E2E_KEEP: distribuce {DISTRO} a {} ponechány", self.data.display());
            return;
        }
        let _ = crate::docker::configure_tls(false);
        let _ = std::process::Command::new("wsl.exe").args(["--unregister", DISTRO]).output();
        let _ = std::fs::remove_dir_all(&self.data);
        eprintln!("úklid: {DISTRO} odregistrována");
    }
}

fn step(t: &Instant, s: &str) {
    eprintln!("\n[{:>5.0} s] === {s} ===", t.elapsed().as_secs_f64());
}

fn root(a: &[&str]) -> Result<String, String> {
    Ok(wsl::run(DISTRO, Some("root"), None, a)?)
}

fn user(a: &[&str]) -> Result<String, String> {
    Ok(wsl::run(DISTRO, None, None, a)?)
}

#[test]
#[ignore]
fn live_docker_e2e() {
    let t = Instant::now();
    let data = std::env::temp_dir().join(format!("wls-e2e-docker-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    // SAFETY (env): test běží samostatně (--test-threads=1 / jen tento test)
    unsafe { std::env::set_var("WLS_DATA_DIR", &data) };
    assert!(crate::config::isolated());
    let _ = std::process::Command::new("wsl.exe").args(["--unregister", DISTRO]).output();
    let _cleanup = Cleanup { data: data.clone() };

    // ------------------------------------------------------------ distribuce
    step(&t, "instalace distribuce");
    let out = wsl::install_distro("Debian", DISTRO).expect("wsl --install");
    eprintln!("{}", out.trim());
    let prep = root(&["env", "U=tester", "sh", "-c", &crate::engine::distro_prepare_script()]).expect("příprava");
    assert!(prep.contains("systemd=true"), "{prep}");
    let _ = wsl::terminate(DISTRO);
    std::thread::sleep(Duration::from_secs(3));
    wsl::start(DISTRO).expect("start");
    std::thread::sleep(Duration::from_secs(3));
    assert!(root(&["test", "-d", "/run/systemd/system"]).is_ok(), "systemd neběží");
    assert_eq!(user(&["id", "-un"]).unwrap().trim(), "tester");

    // Docker nesmí sahat na iptables sdíleného síťového jmenného prostoru WSL
    root(&["sh", "-c", r#"mkdir -p /etc/docker && printf '{"iptables": false, "ip6tables": false}\n' > /etc/docker/daemon.json"#]).unwrap();

    // ------------------------------------------------------------ instalace Dockeru
    step(&t, "instalace Dockeru + API bez ověření (port 2385)");
    let cfg = Config { distro: DISTRO.into(), engine: Engine::Docker, port: PORT, tls: false, keep_alive: false, ..Default::default() };
    cfg.save().unwrap();
    let (r, log) = crate::worker::testkit::install_engine(cfg, Engine::Docker, false);
    eprintln!("{}", log.lines().filter(|l| l.contains("===") || l.contains("Docker") || l.contains("docker")).collect::<Vec<_>>().join("\n"));
    assert!(r.is_ok(), "instalace: {r:?}\n{log}");
    let cfg = Config::load();
    assert_eq!((cfg.engine, cfg.port, cfg.tls, cfg.distro.as_str()), (Engine::Docker, PORT, false, DISTRO));

    let d = Docker::with_tls(PORT, None);
    let snap = d.snapshot();
    assert!(snap.connected, "{:?}", snap.error);
    let info = snap.info.clone().expect("info");
    eprintln!("Docker {} · {}", info.server_version, user(&["docker", "--version"]).unwrap_or_default().trim());
    assert!(user(&["docker", "info", "--format", "{{.ServerVersion}}"]).is_ok(), "CLI uživatele (skupina docker)");

    // ------------------------------------------------------------ události
    let events: Arc<Mutex<Vec<Event>>> = Arc::default();
    {
        let ev = events.clone();
        std::thread::spawn(move || {
            let _ = Docker::with_tls(PORT, None).stream_events(|| {}, |e| {
                ev.lock().unwrap().push(e);
                true
            });
        });
    }
    std::thread::sleep(Duration::from_millis(500));

    // ------------------------------------------------------------ kontejnery
    step(&t, "pull, run (env přes --env-file), stop/start, pád s kódem");
    d.pull("alpine:3.20").expect("pull");
    let spec = crate::worker::RunSpec {
        image: "alpine:3.20".into(),
        name: "e2e-app".into(),
        env: "SECRET_TOKEN=tajne heslo\nA=1".into(),
        ports: "18085:80".into(),
        restart: "unless-stopped".into(),
        command: "sleep 600".into(),
        ..Default::default()
    };
    let r = crate::worker::run_for_test(Config::load(), spec);
    assert!(r.is_ok(), "{r:?}");
    let v: serde_json::Value = d.get("/containers/e2e-app/json").unwrap();
    let env: Vec<String> = serde_json::from_value(v.pointer("/Config/Env").cloned().unwrap()).unwrap();
    assert!(env.contains(&"SECRET_TOKEN=tajne heslo".to_string()), "{env:?}");
    assert_eq!(v.pointer("/HostConfig/RestartPolicy/Name").and_then(|x| x.as_str()), Some("unless-stopped"));
    let id = v["Id"].as_str().unwrap().to_string();
    d.container_action(&id, "stop").unwrap();
    d.container_action(&id, "start").unwrap();
    user(&["docker", "run", "--name", "e2e-crash", "docker.io/library/alpine:3.20", "sh", "-c", "exit 3"]).ok();

    // ------------------------------------------------------------ build
    step(&t, "build z Dockerfile");
    let dir = data.join("ctx");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Dockerfile"), "FROM docker.io/library/alpine:3.20\nRUN echo built > /built\nLABEL e2e=yes\n").unwrap();
    let bspec = crate::worker::BuildSpec { context: dir.to_string_lossy().to_string(), tags: "e2e-build:1".into(), ..Default::default() };
    let mut lines = 0;
    let b = crate::worker::build::run_build(DISTRO, Engine::Docker, PORT, false, &bspec, |_| lines += 1);
    assert!(b.is_ok(), "{b:?}");
    let img: serde_json::Value = d.get("/images/e2e-build:1/json").expect("image");
    assert_eq!(img.pointer("/Config/Labels/e2e").and_then(|x| x.as_str()), Some("yes"));
    eprintln!("build: {lines} řádků výstupu");

    // ------------------------------------------------------------ compose
    step(&t, "compose up / kontrola stacku / down");
    root(&["sh", "-c", "mkdir -p /srv/e2e && printf 'services:\\n  web:\\n    image: docker.io/library/alpine:3.20\\n    command: sleep 600\\n' > /srv/e2e/compose.yaml && chmod -R a+rX /srv/e2e"]).unwrap();
    let up = wsl::run(DISTRO, None, Some("/srv/e2e"), &["docker", "compose", "-p", "e2e", "up", "-d"]);
    assert!(up.is_ok(), "compose up: {up:?}");
    let snap = d.snapshot();
    let svc = snap.containers.iter().find(|c| c.project().as_deref() == Some("e2e")).expect("kontejner stacku");
    eprintln!("stack: {} ({})", svc.name(), svc.status);
    assert!(crate::worker::check_stack_for_test(Config::load(), "/srv/e2e", "e2e").is_ok());
    assert!(wsl::run(DISTRO, None, Some("/srv/e2e"), &["docker", "compose", "-p", "e2e", "down"]).is_ok());

    // ------------------------------------------------------------ události
    std::thread::sleep(Duration::from_secs(1));
    {
        let ev = events.lock().unwrap();
        let has = |n: &str, a: &str| ev.iter().any(|e| e.name() == n && e.action() == a);
        let crash = ev.iter().find(|e| e.name() == "e2e-crash" && e.action() == "die").map(|e| e.exit_code());
        eprintln!("události: {} (start e2e-app: {}, pád e2e-crash: {crash:?})", ev.len(), has("e2e-app", "start"));
        assert!(has("e2e-app", "create") && has("e2e-app", "start") && has("e2e-app", "die"));
        assert_eq!(crash, Some(Some(3)));
        assert!(ev.iter().any(|e| e.kind == "image" && e.triggers_refresh()));
    }

    // ------------------------------------------------------------ recreate
    step(&t, "Recreate se změnou (zachová restart policy, porty)");
    let v: serde_json::Value = d.get(&format!("/containers/{id}/json")).unwrap();
    let det = crate::inspect::parse(&v);
    let mut form = det.to_run_spec();
    form.env.push_str("\nB=2");
    let r = crate::worker::recreate_for_test(Config::load(), &id, form, det.net_edit(false));
    assert!(r.is_ok(), "{r:?}");
    let v2: serde_json::Value = d.get("/containers/e2e-app/json").unwrap();
    let env2: Vec<String> = serde_json::from_value(v2.pointer("/Config/Env").cloned().unwrap()).unwrap();
    assert_ne!(v2["Id"], v["Id"]);
    assert!(env2.contains(&"B=2".to_string()) && env2.contains(&"SECRET_TOKEN=tajne heslo".to_string()), "{env2:?}");
    assert_eq!(v2.pointer("/HostConfig/RestartPolicy/Name").and_then(|x| x.as_str()), Some("unless-stopped"));
    assert_eq!(v2.pointer("/HostConfig/PortBindings/80~1tcp/0/HostPort").and_then(|x| x.as_str()), Some("18085"));

    // ------------------------------------------------------------ shim
    let shim = Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("debug").join("docker.exe")).filter(|p| p.exists());
    let shim_ps = |label: &str| {
        if let Some(s) = &shim {
            let o = std::process::Command::new(s).args(["ps", "--format", "{{.Names}}"]).env("WLS_SHIM_DEBUG", "1").output().unwrap();
            let out = String::from_utf8_lossy(&o.stdout).to_string();
            let err = String::from_utf8_lossy(&o.stderr).to_string();
            eprintln!("shim ({label}): {:?} | {}", out.trim(), err.lines().filter(|l| l.contains("fast path") || l.contains("context")).collect::<Vec<_>>().join(" | "));
            assert!(out.contains("e2e-app"), "shim {label}: {out}\n{err}");
        } else {
            eprintln!("shim: target\\debug\\docker.exe nenalezen – přeskočeno (cargo build --bin docker)");
        }
    };
    step(&t, "shim docker.exe (bez TLS)");
    shim_ps("bez TLS");

    // ------------------------------------------------------------ TLS
    step(&t, "zapnutí TLS (proxy s klientským certifikátem)");
    let r = crate::worker::testkit::set_tls(Config::load(), true);
    assert!(r.is_ok(), "{r:?}");
    let cfg = Config::load();
    assert!(cfg.tls && cfg.port == PORT);
    assert!(data.join("tls").join("key.pem.dpapi").exists() && !data.join("tls").join("key.pem").exists(), "klíč jen šifrovaně");
    let tls = crate::docker::tls_config(&data.join("tls")).expect("tls config");
    let s1 = Docker::with_tls(PORT, Some(Arc::new(tls.clone()))).snapshot();
    let s2 = Docker::with_tls(PORT, Some(Arc::new(tls))).snapshot();
    assert!(s1.connected && s2.connected, "{:?} {:?}", s1.error, s2.error);
    assert!(!Docker::with_tls(PORT, None).ping(), "HTTP bez TLS nesmí projít");
    let listen = root(&["sh", "-c", "ss -ltnp | grep ':2385' || true"]).unwrap();
    eprintln!("naslouchá: {}", listen.trim());
    assert!(listen.contains("socat") && !listen.contains("dockerd"), "na portu smí být jen TLS proxy: {listen}");
    assert!(user(&["docker", "ps", "-q"]).is_ok(), "CLI přes unix socket");
    shim_ps("TLS");

    step(&t, "přegenerování certifikátů");
    let r = crate::worker::testkit::regen_certs(Config::load());
    assert!(r.is_ok(), "{r:?}");
    let tls = crate::docker::tls_config(&data.join("tls")).expect("nový tls config");
    assert!(Docker::with_tls(PORT, Some(Arc::new(tls))).ping());

    step(&t, "vypnutí TLS");
    let r = crate::worker::testkit::set_tls(Config::load(), false);
    assert!(r.is_ok(), "{r:?}");
    assert!(Docker::with_tls(PORT, None).ping());
    assert!(root(&["sh", "-c", "systemctl is-active wls-tls-proxy || true"]).unwrap().trim() != "active");

    step(&t, &format!("hotovo za {:.0} s", t.elapsed().as_secs_f64()));
}
