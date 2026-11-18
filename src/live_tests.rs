//! Integrační testy proti skutečnému WSL/Dockeru.
//! Spouštěj ručně: `cargo test -- --ignored --test-threads=1`
//! (distribuce se bere z WLS_DISTRO, výchozí Ubuntu)

use crate::{docker::Docker, fs, wsl};

fn distro() -> String {
    std::env::var("WLS_DISTRO").unwrap_or_else(|_| "Ubuntu".into())
}

#[test]
#[ignore]
fn live_wsl_list_contains_distro() {
    let l = wsl::list().expect("wsl -l -v");
    assert!(l.iter().any(|d| d.name == distro()), "{l:?}");
}

#[test]
#[ignore]
fn live_fs_roundtrip() {
    let d = distro();
    let (home, _) = fs::list(&d, false, "~").expect("list ~");
    assert!(home.starts_with('/'), "{home}");
    let dir = fs::join(&home, ".wls-desktop-test");
    let _ = fs::delete(&d, false, &[dir.clone()]);
    fs::mkdir(&d, false, &dir).unwrap();
    let file = fs::join(&dir, "a b.txt");
    fs::write_text(&d, false, &file, "ahoj\nsvěte\n").unwrap();
    assert_eq!(fs::read_text(&d, false, &file).unwrap(), "ahoj\nsvěte\n");
    fs::rename(&d, false, &file, &fs::join(&dir, "c.txt")).unwrap();
    let (cwd, e) = fs::list(&d, false, &dir).unwrap();
    assert_eq!(cwd, dir);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].name, "c.txt");
    assert_eq!(e[0].size, "ahoj\nsvěte\n".len() as u64);
    fs::delete(&d, false, &[dir.clone()]).unwrap();
    assert!(fs::list(&d, false, &dir).is_err());
}

#[test]
#[ignore]
fn live_fs_root_lists_docker_dir() {
    let (cwd, e) = fs::list(&distro(), true, "/var/lib/docker").expect("root list");
    assert_eq!(cwd, "/var/lib/docker");
    assert!(e.iter().any(|x| x.name == "volumes"), "{:?}", e.iter().map(|x| &x.name).collect::<Vec<_>>());
}

/// Podman API (port z WLS_PODMAN_PORT, výchozí 2376): stejné operace jako aplikace.
#[test]
#[ignore]
fn live_podman_api() {
    use crate::engine::Engine;
    use crate::worker::RunSpec;
    let port: u16 = std::env::var("WLS_PODMAN_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2376);
    let d = Docker::new(port);
    assert!(d.ping(), "Podman API na {port} neodpovídá");

    let s = d.snapshot();
    assert!(s.connected && s.error.is_none(), "{:?}", s.error);
    let info = s.info.expect("info");
    assert!(!info.server_version.is_empty());

    // krátké jméno -> docker.io/library/alpine
    d.pull("alpine:3.20").expect("pull");
    let imgs = d.snapshot().images;
    assert!(imgs.iter().any(|i| i.tags().iter().any(|t| t.contains("alpine:3.20"))), "{:?}", imgs.iter().map(|i| i.tags()).collect::<Vec<_>>());

    let found = d.search("nginx", Engine::Podman).expect("search");
    assert!(found.iter().any(|r| r.name == "nginx"), "{:?}", found.iter().map(|r| &r.name).collect::<Vec<_>>());

    // kontejner přes CLI (podman --url) jako v aplikaci
    let distro = distro();
    let _ = wsl::run(&distro, None, None, &["podman", "--url", &format!("tcp://127.0.0.1:{port}"), "rm", "-f", "wls-pod-test"]);
    let spec = RunSpec { image: "alpine:3.20".into(), name: "wls-pod-test".into(), command: "sleep 120".into(), restart: "no".into(), ..Default::default() };
    wsl::run_owned(&distro, None, None, &spec.args_for(Engine::Podman, port)).expect("podman run");
    let c = d.snapshot().containers.into_iter().find(|c| c.name() == "wls-pod-test").expect("kontejner v API");
    assert!(c.running(), "{}", c.status);
    assert!(d.stats(&c.id, "wls-pod-test").is_some(), "stats");
    d.container_action(&c.id, "stop").expect("stop");
    d.container_action(&c.id, "remove").expect("remove");

    d.post_json("/volumes/create", serde_json::json!({"Name": "wls-pod-vol"})).expect("volume");
    d.delete("/volumes/wls-pod-vol").expect("volume rm");
    d.post_json("/networks/create", serde_json::json!({"Name": "wls-pod-net", "Driver": "bridge"})).expect("net");
    d.delete("/networks/wls-pod-net").expect("net rm");
    assert!(d.df().is_some(), "df");
}

/// Přidání distribuce: instalace (Debian jako WLS-Test-Debian), příprava, kontrola, odregistrace.
/// Stahuje ~100 MB – spouštěj jen ručně: `cargo test live_add_distro -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_add_distro() {
    let name = "WLS-Test-Debian";
    let _ = std::process::Command::new("wsl.exe").args(["--unregister", name]).output();
    let out = wsl::install_distro("Debian", name).expect("install");
    eprintln!("install: {}", out.trim());
    let prep = wsl::run(name, Some("root"), None, &["env", "U=tester", "sh", "-c", &crate::engine::distro_prepare_script()]).expect("prepare");
    assert!(prep.contains("systemd=true"), "{prep}");
    assert!(prep.contains("default=tester"), "{prep}");
    let _ = wsl::terminate(name);
    std::thread::sleep(std::time::Duration::from_secs(3));
    // výchozí uživatel a sudo bez hesla
    let who = wsl::run(name, None, None, &["sh", "-c", "id -un; sudo -n true && echo SUDO_OK; test -d /run/systemd/system && echo SYSTEMD_OK"]).expect("run");
    eprintln!("{who}");
    let _ = std::process::Command::new("wsl.exe").args(["--unregister", name]).output();
    assert!(who.contains("tester"), "{who}");
    assert!(who.contains("SUDO_OK"), "{who}");
    assert!(who.contains("SYSTEMD_OK"), "{who}");
}

/// pkg_install / pkg_installed / pkg_remove v dočasných kontejnerech Fedory (dnf),
/// openSUSE (zypper) a Archu (pacman); Debian (apt) jen detekce bez změn.
/// Potřebuje Podman API na portu z WLS_PORT (výchozí 2375). Stahuje image (~250 MB).
#[test]
#[ignore]
fn live_pkg_helpers() {
    let port = std::env::var("WLS_PORT").unwrap_or_else(|_| "2375".into());
    let url = format!("tcp://127.0.0.1:{port}");
    let script = format!(
        "{}\npkg_install less >/dev/null 2>&1; echo \"inst:$(pkg_installed less bash neexistuje-xyz)\"; pkg_remove less >/dev/null 2>&1; echo \"after:$(pkg_installed less bash)\"",
        crate::engine::pkg_helpers()
    );
    for img in [
        "registry.fedoraproject.org/fedora:latest",
        "registry.opensuse.org/opensuse/tumbleweed:latest",
        "docker.io/library/archlinux:latest",
    ] {
        let out = wsl::run(&distro(), None, None, &["podman", "--url", &url, "run", "--rm", img, "sh", "-c", &script])
            .unwrap_or_else(|e| panic!("{img}: {e}"));
        eprintln!("{img}: {}", out.trim().replace('\n', " | "));
        assert!(out.contains("inst: less bash") || out.contains("inst: bash less"), "{img}: {out}");
        assert!(out.contains("after: bash"), "{img}: {out}");
    }
    // Debian (apt): jen detekce
    let det = wsl::run(&distro(), None, None, &["sh", "-c", &format!("{}\necho \"det:$(pkg_installed bash neexistuje-xyz)\"", crate::engine::pkg_helpers())]).expect("debian");
    assert!(det.contains("det: bash"), "{det}");
}

/// Syntaktická kontrola všech skriptů enginu (`sh -n`) přímo v distribuci.
#[test]
#[ignore]
fn live_engine_scripts_syntax() {
    use crate::engine::Engine;
    for e in Engine::ALL {
        for (what, s) in [("setup", e.setup_script().to_string()), ("install", e.install_script()), ("uninstall", e.uninstall_script().to_string()), ("prepare", crate::engine::distro_prepare_script())] {
            let r = wsl::run(&distro(), None, None, &["sh", "-n", "-c", &s]);
            assert!(r.is_ok(), "{} {what}: {:?}", e.name(), r);
        }
    }
}

/// Nastavení a odebrání uživatelské proměnné DOCKER_HOST (jen když předtím nebyla nastavená).
#[test]
#[ignore]
fn live_docker_host_roundtrip() {
    use crate::shim_install::{set_docker_host, status};
    if status().docker_host.is_some() {
        eprintln!("DOCKER_HOST už je nastavené – test přeskočen, aby se nepřepsalo");
        return;
    }
    set_docker_host(Some(2375)).expect("set");
    assert_eq!(status().docker_host.as_deref(), Some("tcp://127.0.0.1:2375"));
    set_docker_host(None).expect("clear");
    assert_eq!(status().docker_host, None);
}

/// Detail kontejneru: inspect -> RunSpec, statistiky, logy a znovuvytvoření (úspěch i návrat
/// původního). Pracuje jen s vlastními objekty `wls-test-detail*`.
/// `WLS_DISTRO=Debian cargo test live_container_detail -- --ignored --nocapture`
#[test]
#[ignore]
fn live_container_detail() {
    use crate::engine::Engine;
    use crate::worker::RunSpec;
    use std::time::Duration;
    let port: u16 = std::env::var("WLS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375);
    let d = Docker::new(port);
    assert!(d.ping(), "API na {port} neodpovídá");
    let ver: serde_json::Value = d.get("/version").unwrap();
    let engine = if ver.to_string().contains("Podman") { Engine::Podman } else { Engine::Docker };
    let cfg = crate::config::Config { distro: distro(), engine, port, ..Default::default() };
    let name = "wls-test-detail";
    let vol = "wls-test-detail-vol";
    let cleanup = || {
        for n in [name, "wls-test-detail-wls-old"] {
            let _ = d.container_action(n, "remove");
        }
        let _ = d.delete(&format!("/volumes/{vol}"));
    };
    cleanup();
    let inspect = |id: &str| {
        let v: serde_json::Value = d.get(&format!("/containers/{id}/json")).expect("inspect");
        let mut det = crate::inspect::parse(&v);
        let img: serde_json::Value = d.get(&format!("/images/{}/json", crate::docker::enc(&det.image_id))).expect("image");
        crate::inspect::set_image_defaults(&mut det, &img);
        det
    };

    let spec = RunSpec {
        image: "alpine:3.20".into(),
        name: name.into(),
        ports: "18081:80".into(),
        env: "SECRET_TOKEN=x\nA=1".into(),
        volumes: format!("{vol}:/data"),
        restart: "unless-stopped".into(),
        command: r#"sh -c 'echo "hello detail"; sleep 600'"#.into(),
        ..Default::default()
    };
    wsl::run_owned(&cfg.distro, None, None, &spec.args_for(engine, port)).expect("run");
    let det = inspect(name);
    eprintln!("inspect: {} {} {:?} ports={:?} nets={:?}", det.name, det.image, det.cmd, det.port_specs, det.networks);
    assert!(det.running, "{}", det.status);
    let r = det.to_run_spec();
    assert_eq!(r.name, name);
    assert_eq!(r.ports, "18081:80");
    assert_eq!(r.env, "SECRET_TOKEN=x\nA=1");
    assert_eq!(r.volumes, format!("{vol}:/data"));
    assert_eq!(r.restart, "unless-stopped");
    assert_eq!(r.command, spec.command);

    // statistiky: ~3 vzorky, CPU z rozdílu
    let feed = crate::worker::start_stats(eframe::egui::Context::default(), port, det.id.clone());
    std::thread::sleep(Duration::from_millis(5500));
    let samples: Vec<_> = feed.samples.lock().unwrap().iter().copied().collect();
    drop(feed);
    eprintln!("stats: {samples:?}");
    assert!(samples.len() >= 2, "{samples:?}");
    assert!(samples.iter().any(|s| s.cpu.is_some()) && samples.last().is_some_and(|s| s.mem > 0));

    // logy (stejný stream jako okno logů)
    let mut logs = String::new();
    d.stream_logs(&det.id, 10, |c| {
        logs.push_str(c);
        !logs.contains("hello detail")
    })
    .expect("logs");
    assert!(logs.contains("hello detail"), "{logs}");

    // znovuvytvoření se změnou -> nové ID, změna se projeví, záloha zmizí
    let mut changed = r.clone();
    changed.env.push_str("\nB=2");
    changed.ports = "18082:80".into();
    let res = crate::worker::recreate_for_test(cfg.clone(), &det.id, changed);
    eprintln!("recreate: {res:?}");
    assert!(res.is_ok(), "{res:?}");
    let new = inspect(name);
    assert_ne!(new.id, det.id);
    assert!(new.running);
    assert!(new.env.iter().any(|(k, v)| k == "B" && v == "2"));
    assert_eq!(new.port_specs, vec!["18082:80".to_string()]);
    assert!(d.get::<serde_json::Value>("/containers/wls-test-detail-wls-old/json").is_err());

    // nový kontejner hned skončí -> odstraní se a původní se vrátí a znovu spustí
    let bad = RunSpec { command: "false".into(), restart: "no".into(), ..new.to_run_spec() };
    let res = crate::worker::recreate_for_test(cfg.clone(), &new.id, bad);
    eprintln!("recreate (fail): {res:?}");
    assert!(res.is_err());
    let back = inspect(name);
    assert_eq!(back.id, new.id, "vrátil se původní kontejner");
    assert!(back.running, "{}", back.status);
    assert!(d.get::<serde_json::Value>("/containers/wls-test-detail-wls-old/json").is_err());
    let all: Vec<crate::docker::Container> = d.get("/containers/json?all=1").unwrap();
    assert_eq!(all.iter().filter(|c| c.name().starts_with(name)).count(), 1);

    cleanup();
}

#[test]
#[ignore]
fn live_docker_snapshot() {
    let s = Docker::new(2375).snapshot();
    assert!(s.connected, "{:?}", s.error);
    assert!(s.error.is_none(), "{:?}", s.error);
    assert!(s.info.is_some());
    assert!(!s.networks.is_empty());
    let df = Docker::new(2375).df();
    assert!(df.is_some());
}
