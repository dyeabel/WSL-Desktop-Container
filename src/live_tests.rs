//! Integrační testy proti skutečnému WSL/Dockeru.
//! Spouštěj ručně: `cargo test -- --ignored --test-threads=1`
//! (distribuce se bere z WSLCD_DISTRO, výchozí Ubuntu)

use crate::host::wsl::WslHost;
use crate::{docker::Docker, fs, wsl};
use crate::http::Endpoint;

fn host() -> WslHost {
    WslHost::new(distro())
}

fn distro() -> String {
    std::env::var("WSLCD_DISTRO").unwrap_or_else(|_| "Ubuntu".into())
}

/// API pro živý test: port z proměnné `var` (jinak `def`), TLS podle `WSLCD_TLS=1`
/// (klientský certifikát z testovací kopie dat, viz `config::data_dir`), jinak bez TLS.
fn live_api(var: &str, def: u16) -> (Docker, u16, bool) {
    let port: u16 = std::env::var(var).ok().and_then(|p| p.parse().ok()).unwrap_or(def);
    let tls = std::env::var("WSLCD_TLS").is_ok_and(|v| v == "1");
    crate::docker::configure_tls(tls).expect("TLS klient");
    (Docker::new(&crate::http::Endpoint::Tcp { port }), port, tls)
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
    let (home, _) = fs::list(&host(), false, "~").expect("list ~");
    assert!(home.starts_with('/'), "{home}");
    let dir = fs::join(&home, ".wls-desktop-test");
    let _ = fs::delete(&host(), false, std::slice::from_ref(&dir));
    fs::mkdir(&host(), false, &dir).unwrap();
    let file = fs::join(&dir, "a b.txt");
    fs::write_text(&host(), false, &file, "ahoj\nsvěte\n").unwrap();
    assert_eq!(fs::read_text(&host(), false, &file).unwrap(), "ahoj\nsvěte\n");
    fs::rename(&host(), false, &file, &fs::join(&dir, "c.txt")).unwrap();
    let (cwd, e) = fs::list(&host(), false, &dir).unwrap();
    assert_eq!(cwd, dir);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].name, "c.txt");
    assert_eq!(e[0].size, "ahoj\nsvěte\n".len() as u64);
    fs::delete(&host(), false, std::slice::from_ref(&dir)).unwrap();
    assert!(fs::list(&host(), false, &dir).is_err());
}

#[test]
#[ignore]
fn live_fs_root_lists_docker_dir() {
    let (cwd, e) = fs::list(&host(), true, "/var/lib/docker").expect("root list");
    assert_eq!(cwd, "/var/lib/docker");
    assert!(e.iter().any(|x| x.name == "volumes"), "{:?}", e.iter().map(|x| &x.name).collect::<Vec<_>>());
}

/// Podman API (port z WSLCD_PODMAN_PORT, výchozí 2376): stejné operace jako aplikace.
#[test]
#[ignore]
fn live_podman_api() {
    use crate::engine::Engine;
    use crate::worker::RunSpec;
    let (d, port, tls) = live_api("WSLCD_PODMAN_PORT", 2376);
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
    let mut rm = Engine::Podman.cli_for(&Endpoint::Tcp { port }, tls);
    rm.extend(["rm", "-f", "wslcd-pod-test"].map(String::from));
    let _ = wsl::run_owned(&distro, None, None, &rm);
    let spec = RunSpec { image: "alpine:3.20".into(), name: "wslcd-pod-test".into(), command: "sleep 120".into(), restart: "no".into(), ..Default::default() };
    wsl::run_owned(&distro, None, None, &spec.args_for(Engine::Podman, &Endpoint::Tcp { port }, tls)).expect("podman run");
    let c = d.snapshot().containers.into_iter().find(|c| c.name() == "wslcd-pod-test").expect("kontejner v API");
    assert!(c.running(), "{}", c.status);
    assert!(d.stats(&c.id, "wslcd-pod-test").is_some(), "stats");
    d.container_action(&c.id, "stop").expect("stop");
    d.container_action(&c.id, "remove").expect("remove");

    d.post_json("/volumes/create", serde_json::json!({"Name": "wslcd-pod-vol"})).expect("volume");
    d.delete("/volumes/wslcd-pod-vol").expect("volume rm");
    d.post_json("/networks/create", serde_json::json!({"Name": "wslcd-pod-net", "Driver": "bridge"})).expect("net");
    d.delete("/networks/wslcd-pod-net").expect("net rm");
    assert!(d.df().is_some(), "df");
}

/// Přidání distribuce: instalace (Debian jako WSLCD-Test-Debian), příprava, kontrola, odregistrace.
/// Stahuje ~100 MB – spouštěj jen ručně: `cargo test live_add_distro -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_add_distro() {
    let name = "WSLCD-Test-Debian";
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
/// Potřebuje Podman API na portu z WSLCD_PORT (výchozí 2375). Stahuje image (~250 MB).
#[test]
#[ignore]
fn live_pkg_helpers() {
    let port = std::env::var("WSLCD_PORT").unwrap_or_else(|_| "2375".into());
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
    set_docker_host(Some((2375, false))).expect("set");
    assert_eq!(status().docker_host.as_deref(), Some("tcp://127.0.0.1:2375"));
    set_docker_host(None).expect("clear");
    assert_eq!(status().docker_host, None);
}

/// Detail kontejneru: inspect -> RunSpec, statistiky, logy a znovuvytvoření (úspěch i návrat
/// původního). Pracuje jen s vlastními objekty `wslcd-test-detail*`.
/// `WSLCD_DISTRO=Debian cargo test live_container_detail -- --ignored --nocapture`
#[test]
#[ignore]
fn live_container_detail() {
    use crate::engine::Engine;
    use crate::worker::RunSpec;
    use std::time::Duration;
    let (d, port, tls) = live_api("WSLCD_PORT", 2375);
    assert!(d.ping(), "API na {port} neodpovídá");
    let ver: serde_json::Value = d.get("/version").unwrap();
    let engine = if ver.to_string().contains("Podman") { Engine::Podman } else { Engine::Docker };
    let cfg = crate::config::Config { distro: distro(), engine, port, tls, ..Default::default() };
    let name = "wslcd-test-detail";
    let vol = "wslcd-test-detail-vol";
    let cleanup = || {
        for n in [name, "wslcd-test-detail-wslcd-old"] {
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
    wsl::run_owned(&cfg.distro, None, None, &spec.args_for(engine, &Endpoint::Tcp { port }, tls)).expect("run");
    let det = inspect(name);
    eprintln!("inspect: {} {} {:?} ports={:?} nets={:?}", det.name, det.image, det.cmd, det.port_specs, det.networks);
    assert!(det.running, "{}", det.status);
    let r = det.to_run_spec();
    assert_eq!(r.name, name);
    assert_eq!(r.ports, "18081:80");
    // pořadí proměnných v inspectu Podman nezaručuje
    let mut env: Vec<&str> = r.env.lines().collect();
    env.sort();
    assert_eq!(env, vec!["A=1", "SECRET_TOKEN=x"]);
    assert_eq!(r.volumes, format!("{vol}:/data"));
    assert_eq!(r.restart, "unless-stopped");
    assert_eq!(r.command, spec.command);

    // statistiky: ~3 vzorky, CPU z rozdílu
    let feed = crate::worker::start_stats(eframe::egui::Context::default(), crate::http::Endpoint::Tcp { port }, det.id.clone());
    std::thread::sleep(Duration::from_millis(5500));
    let samples: Vec<_> = feed.samples.lock().unwrap().iter().copied().collect();
    drop(feed);
    eprintln!("stats: {samples:?}");
    assert!(samples.len() >= 2, "{samples:?}");
    assert!(samples.iter().any(|s| s.cpu.is_some()) && samples.last().is_some_and(|s| s.mem > 0));

    // logy (stejný stream jako okno logů)
    let mut logs = String::new();
    d.stream_logs(&det.id, &crate::docker::LogOpts::follow(10), |c, _| {
        logs.push_str(c);
        !logs.contains("hello detail")
    })
    .expect("logs");
    assert!(logs.contains("hello detail"), "{logs}");

    // znovuvytvoření se změnou -> nové ID, změna se projeví, záloha zmizí
    let mut changed = r.clone();
    changed.env.push_str("\nB=2");
    changed.ports = "18082:80".into();
    let res = crate::worker::recreate_for_test(cfg.clone(), &det.id, changed, det.net_edit(engine == Engine::Podman));
    eprintln!("recreate: {res:?}");
    assert!(res.is_ok(), "{res:?}");
    let new = inspect(name);
    assert_ne!(new.id, det.id);
    assert!(new.running);
    assert!(new.env.iter().any(|(k, v)| k == "B" && v == "2"));
    assert_eq!(new.port_specs, vec!["18082:80".to_string()]);
    assert!(d.get::<serde_json::Value>("/containers/wslcd-test-detail-wslcd-old/json").is_err());

    // nový kontejner hned skončí -> odstraní se a původní se vrátí a znovu spustí
    let bad = RunSpec { command: "false".into(), restart: "no".into(), ..new.to_run_spec() };
    let res = crate::worker::recreate_for_test(cfg.clone(), &new.id, bad, new.net_edit(engine == Engine::Podman));
    eprintln!("recreate (fail): {res:?}");
    assert!(res.is_err());
    let back = inspect(name);
    assert_eq!(back.id, new.id, "vrátil se původní kontejner");
    assert!(back.running, "{}", back.status);
    assert!(d.get::<serde_json::Value>("/containers/wslcd-test-detail-wslcd-old/json").is_err());
    let all: Vec<crate::docker::Container> = d.get("/containers/json?all=1").unwrap();
    assert_eq!(all.iter().filter(|c| c.name().starts_with(name)).count(), 1);

    cleanup();
}

/// Znovuvytvoření zachová všechny sítě (aliasy, pevnou IP) i anonymní volume s daty.
/// Pracuje jen s vlastními objekty `wslcd-test-recreate`, `wslcd-test-net-a`, `wslcd-test-net-b`
/// a anonymními volume tohoto kontejneru.
/// `WSLCD_DISTRO=Debian cargo test live_recreate_networks -- --ignored --nocapture`
#[test]
#[ignore]
fn live_recreate_networks() {
    use crate::engine::Engine;
    use serde_json::{json, Value};
    let port: u16 = std::env::var("WSLCD_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375);
    let d = Docker::new(&crate::http::Endpoint::Tcp { port });
    assert!(d.ping(), "API na {port} neodpovídá");
    let ver: Value = d.get("/version").unwrap();
    let engine = if ver.to_string().contains("Podman") { Engine::Podman } else { Engine::Docker };
    let cfg = crate::config::Config { distro: distro(), engine, port, ..Default::default() };
    let name = "wslcd-test-recreate";
    let (net_a, net_b, ip_b) = ("wslcd-test-net-a", "wslcd-test-net-b", "10.231.77.50");
    // anonymní volume našich kontejnerů (podle inspectu, ať se nesáhne na cizí)
    let anon_of = |c: &str| -> Vec<String> {
        d.get::<Value>(&format!("/containers/{c}/json"))
            .ok()
            .and_then(|v| v.get("Mounts").cloned())
            .and_then(|m| m.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|m| m.get("Name").and_then(|n| n.as_str()).map(String::from))
            .filter(|n| crate::inspect::is_anonymous_volume(n))
            .collect()
    };
    let cleanup = |extra: &[String]| {
        let mut vols: Vec<String> = extra.to_vec();
        for c in [name, "wslcd-test-recreate-wslcd-old"] {
            vols.extend(anon_of(c));
            let _ = d.container_action(c, "remove");
        }
        for v in vols {
            let _ = d.delete(&format!("/volumes/{v}"));
        }
        for n in [net_a, net_b] {
            let _ = d.delete(&format!("/networks/{n}"));
        }
    };
    cleanup(&[]);

    d.post_json("/networks/create", json!({"Name": net_a})).expect("síť a");
    d.post_json("/networks/create", json!({"Name": net_b, "IPAM": {"Config": [{"Subnet": "10.231.77.0/24"}]}})).expect("síť b");
    // první běh zapíše soubor do anonymního volume, další běh ho najde a vypíše
    let script = "if [ -f /anon/f.txt ]; then echo found $(cat /anon/f.txt); else echo keep-me > /anon/f.txt; echo created; fi; sleep 600";
    let create = json!({
        "Image": "docker.io/library/alpine:3.20",
        "Cmd": ["sh", "-c", script],
        "Env": ["A=1"],
        "Volumes": {"/anon": {}},
        "HostConfig": {"NetworkMode": net_a},
        "NetworkingConfig": {"EndpointsConfig": {net_a: {"Aliases": ["web"]}}}
    });
    let url = format!("/containers/create?name={name}");
    if d.post_json(&url, create.clone()).is_err() {
        d.pull("alpine:3.20").expect("pull");
        d.post_json(&url, create).expect("create");
    }
    d.post_json(&format!("/networks/{net_b}/connect"), json!({"Container": name, "EndpointConfig": {"Aliases": ["dbc"], "IPAMConfig": {"IPv4Address": ip_b}}}))
        .expect("connect b");
    d.container_action(name, "start").expect("start");
    let wait_log = |id: &str| {
        let mut logs = String::new();
        d.stream_logs(id, &crate::docker::LogOpts::follow(10), |c, _| {
            logs.push_str(c);
            !(logs.contains("found") || logs.contains("created"))
        })
        .expect("logs");
        logs
    };
    let first = wait_log(name);
    assert!(first.contains("created"), "{first}");

    let inspect = |id: &str| {
        let v: Value = d.get(&format!("/containers/{id}/json")).expect("inspect");
        let mut det = crate::inspect::parse(&v);
        let img: Value = d.get(&format!("/images/{}/json", crate::docker::enc(&det.image_id))).expect("image");
        crate::inspect::set_image_defaults(&mut det, &img);
        det
    };
    let det = inspect(name);
    let anon = anon_of(name);
    eprintln!("původní: nets={:?} anon={anon:?}", det.networks);
    assert_eq!(anon.len(), 1, "{:?}", det.mounts);

    // formulář jako v UI + změna proměnné
    let mut spec = det.to_run_spec();
    assert!(spec.volumes.is_empty(), "anonymní volume se ve formuláři neuvádí: {}", spec.volumes);
    assert_eq!(det.anonymous_volumes(&spec.volumes).len(), 1);
    spec.env.push_str("\nB=2");
    let edit = det.net_edit(engine == Engine::Podman);
    eprintln!("formulář: network={} edit={edit:?}", spec.network);
    assert!(edit.pin.iter().any(|n| n == net_b), "{edit:?}");
    let res = crate::worker::recreate_for_test(cfg.clone(), &det.id, spec, edit);
    eprintln!("recreate: {res:?}");
    assert!(res.is_ok(), "{res:?}");

    let new = inspect(name);
    eprintln!("nový: nets={:?} mounts={:?}", new.networks, new.mounts);
    assert_ne!(new.id, det.id);
    assert!(new.running, "{}", new.status);
    assert!(new.env.iter().any(|(k, v)| k == "B" && v == "2"));
    let a = new.networks.iter().find(|n| n.name == net_a).expect("síť a");
    let b = new.networks.iter().find(|n| n.name == net_b).expect("síť b");
    assert!(a.aliases.iter().any(|x| x == "web"), "{a:?}");
    assert!(b.aliases.iter().any(|x| x == "dbc"), "{b:?}");
    assert_eq!(b.ip, ip_b);
    assert_eq!(anon_of(name), anon, "stejné anonymní volume");
    let second = wait_log(name);
    eprintln!("log nového: {second:?}");
    assert!(second.contains("found keep-me"), "{second}");
    assert!(d.get::<Value>("/containers/wslcd-test-recreate-wslcd-old/json").is_err());
    // anonymní volume po odstranění starého kontejneru stále existuje
    assert!(d.get::<Value>(&format!("/volumes/{}", anon[0])).is_ok());

    cleanup(&anon);
    assert!(d.get::<Value>(&format!("/volumes/{}", anon[0])).is_err(), "volume uklizeno");
}

#[test]
#[ignore]
fn live_docker_snapshot() {
    let s = Docker::new(&crate::http::Endpoint::Tcp { port: 2375 }).snapshot();
    assert!(s.connected, "{:?}", s.error);
    assert!(s.error.is_none(), "{:?}", s.error);
    assert!(s.info.is_some());
    assert!(!s.networks.is_empty());
    let df = Docker::new(&crate::http::Endpoint::Tcp { port: 2375 }).df();
    assert!(df.is_some());
}

/// Zápis a odebrání hodnoty Run v HKCU (jen když autostart předtím nebyl zapnutý).
#[test]
#[ignore]
fn live_autostart_roundtrip() {
    use crate::autostart;
    if autostart::is_enabled() {
        eprintln!("autostart už je zapnutý – test přeskočen");
        return;
    }
    autostart::set(true).expect("set");
    assert!(autostart::is_enabled());
    autostart::set(false).expect("clear");
    assert!(!autostart::is_enabled());
}

/// Proud událostí: vytvoření a smazání testovacího kontejneru musí přijít jako události.
#[test]
#[ignore]
fn live_events_stream() {
    let (d, _, _) = live_api("WSLCD_PORT", 2375);
    let (tx, rx) = std::sync::mpsc::channel();
    let ev = d.clone();
    std::thread::spawn(move || {
        let _ = ev.stream_events(|| {}, |e| tx.send((e.action().to_string(), e.name(), e.triggers_refresh())).is_ok());
    });
    std::thread::sleep(std::time::Duration::from_millis(500));
    d.post_json("/containers/create?name=wslcd-test-events", serde_json::json!({"Image": "docker.io/library/alpine:3.20", "Cmd": ["true"]}))
        .expect("create");
    d.delete("/containers/wslcd-test-events?force=1").expect("remove");
    let mut seen = vec![];
    while let Ok(ev) = rx.recv_timeout(std::time::Duration::from_secs(5)) {
        if ev.1 == "wslcd-test-events" {
            seen.push(ev);
        }
    }
    eprintln!("{seen:?}");
    assert!(seen.iter().any(|e| e.0 == "create" && e.2), "{seen:?}");
    assert!(seen.iter().any(|e| e.0 == "remove" && e.2), "{seen:?}");
}

/// Sestavení image z Dockerfile stejnou cestou jako aplikace (`worker::build::run_build`):
/// dočasná složka v %TEMP% (windowsová cesta -> wslpath), engine a distribuce z konfigurace
/// (WSLCD_DISTRO ji přepíše), API na portu WSLCD_PORT (výchozí 2375). Image `wslcd-test-build:latest`
/// a složka se na konci smažou.
#[test]
#[ignore]
fn live_build_image() {
    let cfg = crate::config::Config::load();
    let distro = std::env::var("WSLCD_DISTRO").unwrap_or(cfg.distro.clone());
    let (d, port, tls) = live_api("WSLCD_PORT", 2375);
    assert!(d.ping(), "API na {port} neodpovídá");

    let dir = std::env::temp_dir().join(format!("wslcd-test-build-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Dockerfile"), "FROM docker.io/library/alpine:3.20\nRUN echo ok\n").unwrap();

    let spec = crate::worker::BuildSpec {
        context: dir.to_string_lossy().to_string(),
        tags: "wslcd-test-build:latest".into(),
        ..Default::default()
    };
    let mut lines = Vec::new();
    let res = crate::worker::build::run_build(&WslHost::new(&distro), cfg.engine, &Endpoint::Tcp { port }, tls, &spec, |l| {
        eprintln!("| {l}");
        lines.push(l.to_string());
    });
    eprintln!("=> {res:?} ({} řádků výstupu)", lines.len());

    let img: Result<serde_json::Value, crate::docker::ApiError> = d.get("/images/wslcd-test-build:latest/json");
    let removed = d.delete("/images/wslcd-test-build:latest");
    eprintln!("odstranění image: {removed:?}");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(res.is_ok(), "{res:?}");
    assert!(lines.len() > 2, "výstup se nestreamoval: {lines:?}");
    let img = img.expect("image v API");
    eprintln!("image Id: {}", img.get("Id").and_then(|v| v.as_str()).unwrap_or("?"));
    assert!(removed.is_ok(), "{removed:?}");
    assert!(d.get::<serde_json::Value>("/images/wslcd-test-build:latest/json").is_err(), "image zůstal");
    assert!(!dir.exists());
}

/// TLS proxy end-to-end bez zásahu do běžícího enginu: dočasná Podman služba na unix socketu,
/// `tls_proxy` (stejný skript jako aplikace) s jinou složkou a službou na portu 2377,
/// pak klient s certifikátem (musí projít), bez certifikátu a přes HTTP (musí selhat).
/// Potřebuje podman a doinstaluje socat (pokud chybí).
#[cfg(windows)]
#[test]
#[ignore]
fn live_tls_proxy() {
    let distro = distro();
    let root = |args: &[&str]| wsl::run(&distro, Some("root"), None, args);
    let cleanup = || {
        let _ = root(&["sh", "-c", "systemctl disable --now wslcd-test-tls-proxy wslcd-test-podman >/dev/null 2>&1; \
            rm -f /etc/systemd/system/wslcd-test-tls-proxy.service; systemctl daemon-reload; rm -rf /tmp/wslcd-test-tls /tmp/wslcd-test-podman.sock"]);
    };
    cleanup();
    root(&["sh", "-c", "systemd-run --unit wslcd-test-podman podman system service --time=0 unix:///tmp/wslcd-test-podman.sock && \
        for i in $(seq 50); do [ -S /tmp/wslcd-test-podman.sock ] && break; sleep 0.2; done"]).expect("podman socket");
    let script = format!(
        "{}\ntls_proxy /tmp/wslcd-test-podman.sock wslcd-test-podman.service && sleep 1 && systemctl is-active wslcd-test-tls-proxy",
        crate::engine::Engine::Podman.setup_script().split("\nset -e\n").next().unwrap()
    );
    let out = root(&["env", "PORT=2377", "WSLCD_TLS_DIR=/tmp/wslcd-test-tls", "WSLCD_TLS_UNIT=wslcd-test-tls-proxy", "sh", "-c", &script]);
    eprintln!("{out:?}");
    // certifikáty do dočasné složky ve Windows
    let dir = std::env::temp_dir().join("wslcd-test-tls");
    let _ = std::fs::create_dir_all(&dir);
    for f in ["ca.pem", "cert.pem", "key.pem"] {
        let pem = root(&["cat", &format!("/tmp/wslcd-test-tls/{f}")]).expect(f);
        std::fs::write(dir.join(f), pem).unwrap();
    }
    let cfg = crate::docker::tls_config(&dir);
    let ok = cfg.as_ref().map(|c| Docker::with_tls(2377, Some(std::sync::Arc::new(c.clone()))).snapshot());
    // bez klientského certifikátu: stejná CA, ale žádný cert
    let no_cert = {
        use rustls_pki_types::pem::PemObject;
        let mut roots = rustls::RootCertStore::empty();
        for c in rustls_pki_types::CertificateDer::pem_file_iter(dir.join("ca.pem")).unwrap() {
            roots.add(c.unwrap()).unwrap();
        }
        let c = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Docker::with_tls(2377, Some(std::sync::Arc::new(c))).ping()
    };
    let plain = Docker::with_tls(2377, None).ping();
    // druhé nové spojení se stejnou konfigurací (dřív selhalo na obnově TLS relace)
    let second = cfg.as_ref().map(|c| Docker::with_tls(2377, Some(std::sync::Arc::new(c.clone()))).ping()).unwrap_or(false);
    // relay pojmenované roury -> TLS proxy na 2377 (testovací roura, ne roura aplikace)
    let via_pipe = cfg.as_ref().ok().map(|c| {
        let name = format!(r"\\.\pipe\wslcd_test_tls_{}", std::process::id());
        let c = std::sync::Arc::new(c.clone());
        let relay = crate::npipe::Relay::start(&name, std::sync::Arc::new(move || Ok(crate::npipe::Target::Tls(2377, c.clone())))).expect("relay");
        let r = (crate::npipe::tests::ping_pipe(&name), crate::npipe::tests::get_via_pipe(&name, "/version"));
        drop(relay);
        r
    });
    let _ = std::fs::remove_dir_all(&dir);
    cleanup();
    let snap = ok.expect("tls_config");
    assert!(out.is_ok(), "{out:?}");
    assert!(snap.connected && snap.error.is_none(), "{:?}", snap.error);
    assert!(!no_cert, "spojení bez klientského certifikátu prošlo");
    assert!(!plain, "HTTP bez TLS prošlo");
    assert!(second, "druhé TLS spojení se stejnou konfigurací selhalo");
    let (ping, version) = via_pipe.expect("tls_config");
    eprintln!("npipe -> TLS /_ping: {ping:?}");
    assert!(ping.starts_with("HTTP/1.1 200") && ping.ends_with("OK"), "{ping}");
    assert!(version.starts_with("HTTP/1.1 200") && version.contains("\"ApiVersion\""), "{version}");
}

/// Relay pojmenované roury proti běžícímu API bez TLS na 127.0.0.1:2375 (jen čte /_ping),
/// na testovací rouře `\\.\pipe\wslcd_test_engine_<pid>` – roura aplikace se nepoužije.
/// `cargo test live_npipe_plain -- --ignored --nocapture`
#[cfg(windows)]
#[test]
#[ignore]
fn live_npipe_plain() {
    let name = format!(r"\\.\pipe\wslcd_test_engine_{}", std::process::id());
    // WSLCD_TLS=1: relay přes TLS s klientským certifikátem (jako roura aplikace), port z WSLCD_PORT
    let (_, port, tls) = live_api("WSLCD_PORT", 2375);
    let target = match crate::docker::current_tls() {
        Some(c) if tls => crate::npipe::Target::Tls(port, c),
        _ => crate::npipe::Target::Plain(port),
    };
    let relay = crate::npipe::Relay::start(&name, std::sync::Arc::new(move || Ok(target.clone()))).expect("relay");
    let r = crate::npipe::tests::ping_pipe(&name);
    let version = crate::npipe::tests::get_via_pipe(&name, "/version");
    drop(relay);
    eprintln!("npipe /_ping: {r:?}");
    assert!(r.starts_with("HTTP/1.1 200") && r.ends_with("OK"), "{r}");
    assert!(version.contains("\"ApiVersion\""), "{version}");
    // po zastavení relay roura zmizí
    assert!(std::fs::OpenOptions::new().read(true).write(true).open(&name).is_err());
}

/// Operace jako root nesmí psát / měnit práva skrz symlink (F-10).
#[test]
#[ignore]
fn live_fs_root_symlink_safe() {
    let d = distro();
    let dir = "/tmp/wslcd-test-fs-root";
    let root = |a: &[&str]| wsl::run(&d, Some("root"), None, a);
    root(&["sh", "-c", "rm -rf /tmp/wslcd-test-fs-root && mkdir /tmp/wslcd-test-fs-root && cd /tmp/wslcd-test-fs-root && echo tajne > secret && chmod 600 secret && ln -s secret link && echo a > plain && chmod 640 plain"]).unwrap();
    let w = fs::write_text(&host(), true, &format!("{dir}/link"), "pwned\n");
    let c = fs::chmod(&host(), true, "666", &format!("{dir}/link"));
    let p = fs::write_text(&host(), true, &format!("{dir}/plain"), "b\n");
    let state = root(&["sh", "-c", "cd /tmp/wslcd-test-fs-root && cat secret && stat -c '%a %n' secret plain && cat plain && ls -a"]).unwrap();
    let _ = root(&["rm", "-rf", dir]);
    eprintln!("{w:?} {c:?} {p:?}\n{state}");
    assert!(w.is_err() && c.is_err(), "{w:?} {c:?}");
    assert!(p.is_ok(), "{p:?}");
    assert!(state.contains("tajne") && state.contains("600 secret") && state.contains("640 plain") && state.contains("b\n"), "{state}");
    assert!(!state.contains(".wslcd-edit"), "{state}");
}

/// Záloha, obnova a klon volume + velikosti (jen volume `wslcd-test-*`).
/// `WSLCD_DISTRO=Debian cargo test live_volume_backup_restore -- --ignored --nocapture`
#[test]
#[ignore]
fn live_volume_backup_restore() {
    use crate::engine::Engine;
    use crate::worker::{volume_op_for_test, Cmd, HELPER_IMAGE};
    let (d, port, tls) = live_api("WSLCD_PORT", 2375);
    assert!(d.ping(), "API na {port} neodpovídá");
    let ver: serde_json::Value = d.get("/version").unwrap();
    let engine = if ver.to_string().contains("Podman") { Engine::Podman } else { Engine::Docker };
    let cfg = crate::config::Config { distro: distro(), engine, port, tls, ..Default::default() };
    let cli = engine.cli_for(&Endpoint::Tcp { port }, tls);
    let (src, dst, cln) = ("wslcd-test-backup", "wslcd-test-restore", "wslcd-test-clone");
    // složka s mezerou a apostrofem (uvozovky Windows v názvech nedovolí)
    let dir = std::env::temp_dir().join("wslcd-test backup it's");
    let cleanup = || {
        for v in [src, dst, cln] {
            let _ = d.delete(&format!("/volumes/{v}?force=true"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    };
    cleanup();
    std::fs::create_dir_all(&dir).unwrap();
    // spustí alpine s volume na /data a vrátí výstup skriptu (skript je konstanta testu)
    let sh = |vol: &str, ro: bool, script: &str| {
        let mut a = cli.clone();
        a.extend(["run", "--rm", "-v"].map(String::from));
        a.push(format!("{vol}:/data{}", if ro { ":ro" } else { "" }));
        a.extend([HELPER_IMAGE, "sh", "-c", script].map(String::from));
        wsl::run_owned(&cfg.distro, None, None, &a).expect("alpine")
    };
    let listing = |vol: &str| {
        sh(vol, true, r#"cd /data && find . | sort && cat a.txt .dot "with space.txt" sub/n && stat -c '%n %s %a' a.txt .dot "with space.txt""#)
    };

    d.post_json("/volumes/create", serde_json::json!({ "Name": src })).expect("create");
    sh(src, false, r#"echo a > /data/a.txt && echo hidden > /data/.dot && echo mezera > "/data/with space.txt" && chmod 600 /data/.dot && mkdir /data/sub && echo n > /data/sub/n"#);
    let orig = listing(src);
    eprintln!("--- obsah {src}:\n{orig}");
    assert!(orig.contains("./.dot") && orig.contains("./with space.txt") && orig.contains(".dot 7 600"), "{orig}");

    // velikosti: /system/df (Podman ji vyplňuje) i záložní du
    let sizes = crate::worker::volume_sizes_for_test(cfg.clone());
    eprintln!("velikost (worker): {:?}", sizes.get(src));
    assert!(sizes.get(src).is_some_and(|s| *s > 0), "{sizes:?}");
    let mp: serde_json::Value = d.get(&format!("/volumes/{src}")).unwrap();
    let mp = mp["Mountpoint"].as_str().unwrap().to_string();
    let du = crate::worker::du_sizes(&*crate::host::current(&cfg), &[(mp.clone(), src.to_string())].into());
    eprintln!("du -sb {mp}: {:?}", du.get(src));
    assert!(du.get(src).is_some_and(|s| *s > 0), "{du:?}");

    // záloha do Windows
    let target = dir.join("wslcd-test-backup 'x'.tar.gz").to_string_lossy().to_string();
    let r = volume_op_for_test(cfg.clone(), Cmd::BackupVolume { volume: src.into(), target: target.clone(), stop: false });
    eprintln!("záloha: {r:?}");
    assert!(r.is_ok(), "{r:?}");
    assert!(std::fs::metadata(&target).expect("soubor zálohy").len() > 0);

    // obnova do nového volume
    let r = volume_op_for_test(cfg.clone(), Cmd::RestoreVolume { source: target.clone(), volume: dst.into(), create: true, clear: false });
    eprintln!("obnova: {r:?}");
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(listing(dst), orig);

    // obnova do existujícího s vyprázdněním: přidané soubory (i skryté) zmizí
    sh(dst, false, "echo x > /data/extra && echo y > /data/.extra && echo z > /data/..extra");
    let r = volume_op_for_test(cfg.clone(), Cmd::RestoreVolume { source: target.clone(), volume: dst.into(), create: false, clear: true });
    eprintln!("obnova s vyprázdněním: {r:?}");
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(listing(dst), orig);
    // nový volume se jménem existujícího nejde
    let r = volume_op_for_test(cfg.clone(), Cmd::RestoreVolume { source: target.clone(), volume: dst.into(), create: true, clear: false });
    eprintln!("obnova do existujícího jako nový: {r:?}");
    assert!(r.is_err(), "{r:?}");

    // klon
    let r = volume_op_for_test(cfg.clone(), Cmd::CloneVolume { from: src.into(), to: cln.into() });
    eprintln!("klon: {r:?}");
    assert!(r.is_ok(), "{r:?}");
    assert_eq!(listing(cln), orig);

    cleanup();
    let left: serde_json::Value = d.get("/volumes").unwrap();
    eprintln!("úklid hotov, zbývající volume wslcd-test-*: {}", left.to_string().matches("\"wslcd-test-").count());
    assert!(!left.to_string().contains("\"wslcd-test-backup\"") && !dir.exists());
}

/// Zrušení sestavení: `RUN sleep 45` se po zrušení nesmí dál provádět v distribuci.
#[test]
#[ignore]
fn live_build_cancel() {
    let cfg = crate::config::Config::load();
    let distro = std::env::var("WSLCD_DISTRO").unwrap_or(cfg.distro.clone());
    let dir = std::env::temp_dir().join(format!("wslcd-test-build-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Dockerfile"), "FROM docker.io/library/alpine:3.20\nRUN sleep 45 && echo wlscancel\n").unwrap();
    let spec = crate::worker::BuildSpec { context: dir.to_string_lossy().to_string(), tags: "wslcd-test-cancel:latest".into(), ..Default::default() };
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(8));
        crate::worker::cancel_build();
    });
    let t = std::time::Instant::now();
    let res = crate::worker::build::run_build(&WslHost::new(&distro), cfg.engine, &Endpoint::Tcp { port: cfg.port }, cfg.tls, &spec, |l| eprintln!("| {l}"));
    let took = t.elapsed();
    std::thread::sleep(std::time::Duration::from_secs(3));
    let ps = wsl::run(&distro, Some("root"), None, &["sh", "-c", "ps -eo args | grep 'sleep 45' | grep -v grep || true"]).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("=> {res:?} za {took:?}; procesy: {ps:?}");
    assert!(res.is_err(), "{res:?}");
    assert!(took < std::time::Duration::from_secs(30), "{took:?}");
    assert!(ps.trim().is_empty(), "build běží dál: {ps}");
}

/// Nový stack ze šablony Redis stejnou cestou jako aplikace (`worker::compose::create_stack`):
/// dočasná složka /tmp/wslcd-test-compose-*, projekt `wslcd-test-compose`, port hostitele mimo 6379.
/// Pak `config -q` (soubor i obsah editoru přes stdin), `up -d`, kontrola kontejneru přes API,
/// nakonec `down -v` a smazání složky. Jiných stacků, kontejnerů, volume ani image se nedotýká.
#[test]
#[ignore]
fn live_compose_new_stack() {
    use crate::worker::compose::{create_stack, validate, NewStackSpec};
    let cfg = crate::config::Config::load();
    let distro = std::env::var("WSLCD_DISTRO").unwrap_or(cfg.distro.clone());
    let port: u16 = std::env::var("WSLCD_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375);
    let d = Docker::new(&crate::http::Endpoint::Tcp { port });
    assert!(d.ping(), "API na {port} neodpovídá");
    let cli = cfg.engine.cli_for(&Endpoint::Tcp { port }, false);
    let project = "wslcd-test-compose";
    let dir = format!("/tmp/wslcd-test-compose-{}", std::process::id());
    let t = crate::compose_templates::TEMPLATES.iter().find(|t| t.id == "redis").unwrap();
    let host_port = 16379 + std::process::id() % 1000;
    let env = crate::ui::compose_new::set_env_line(&t.render_env(project), "REDIS_PORT", &host_port.to_string());
    let spec = NewStackSpec { project: project.into(), dir: dir.clone(), compose: t.compose.into(), env, start: false };

    let compose_cmd = |args: &[&str]| {
        let mut a = cli.clone();
        a.extend(["compose".into(), "-p".into(), project.into()]);
        a.extend(args.iter().map(|s| s.to_string()));
        wsl::run_owned(&distro, None, Some(&dir), &a)
    };
    let created = create_stack(&WslHost::new(&distro), &cli, &spec);
    eprintln!("create_stack: {created:?}");
    let again = create_stack(&WslHost::new(&distro), &cli, &spec);
    eprintln!("znovu do stejné složky: {again:?}");
    let files = fs::list(&WslHost::new(&distro), false, &dir).map(|(_, e)| e.iter().map(|x| format!("{} {}", x.mode, x.name)).collect::<Vec<_>>());
    eprintln!("soubory: {files:?}");
    let compose_file = fs::join(&dir, "compose.yaml");
    let check_file = validate(&WslHost::new(&distro), &cli, project, &dir, std::slice::from_ref(&compose_file), None, "");
    eprintln!("config -q (soubor): {check_file:?}");
    let check_bad = validate(&WslHost::new(&distro), &cli, project, &dir, &["-".into()], None, "services:\n  redis: [\n");
    eprintln!("config -q (rozbitý obsah editoru): {check_bad:?}");
    let check_env = validate(&WslHost::new(&distro), &cli, project, &dir, std::slice::from_ref(&compose_file), Some("REDIS_PORT=1\n"), "");
    eprintln!("config -q (.env z editoru bez hesla): {check_env:?}");
    let up = compose_cmd(&["up", "-d"]);
    eprintln!("up -d: {up:?}");
    std::thread::sleep(std::time::Duration::from_secs(2));
    let c = d
        .snapshot()
        .containers
        .into_iter()
        .find(|c| c.project() == Some(project) && c.label("com.docker.compose.service") == Some("redis"));
    if let Some(c) = &c {
        eprintln!("kontejner: {} {} {} | {}", c.name(), c.image, c.status, c.ports_text());
    }
    let down = compose_cmd(&["down", "-v"]);
    eprintln!("down -v: {down:?}");
    let rm = fs::delete(&WslHost::new(&distro), false, std::slice::from_ref(&dir));
    eprintln!("rm složky: {rm:?}");
    let left = d.snapshot().containers.into_iter().any(|c| c.project() == Some(project));

    assert_eq!(created.as_deref(), Ok(dir.as_str()));
    assert!(again.as_ref().is_err_and(|e| e.contains("compose.yaml")), "{again:?}");
    let files = files.unwrap();
    assert!(files.iter().any(|f| f == "600 .env"), "{files:?}");
    assert!(files.iter().any(|f| f.ends_with(" compose.yaml")), "{files:?}");
    assert!(check_file.is_ok(), "{check_file:?}");
    assert!(check_bad.is_err(), "{check_bad:?}");
    assert!(check_env.as_ref().is_ok_and(|w| w.contains("REDIS_PASSWORD")), "{check_env:?}");
    assert!(up.is_ok(), "{up:?}");
    let c = c.expect("kontejner stacku v API");
    assert!(c.running(), "{}", c.status);
    assert!(c.ports_text().contains(&host_port.to_string()), "{}", c.ports_text());
    assert!(down.is_ok(), "{down:?}");
    assert!(!left, "kontejnery stacku zůstaly");
    assert!(rm.is_ok() && fs::list(&WslHost::new(&distro), false, &dir).is_err());
}

/// Správa distribucí: rootfs alpine (export kontejneru přes engine v WSLCD_DISTRO) -> wsl --import
/// jako WSLCD-Test-Import, kontrola v `wsl -l -v`, disk (sparse, compact), export, unregister.
/// Sahá jen na distribuci WSLCD-Test-Import. Stahuje alpine:3.20 (~3 MB).
/// `WSLCD_DISTRO=Debian cargo test live_distro_manage -- --ignored --nocapture`
#[test]
#[ignore]
fn live_distro_manage() {
    const NAME: &str = "WSLCD-Test-Import";
    const CTR: &str = "wslcd-test-rootfs";
    /// Úklid i při panice – odregistruje jen WSLCD-Test-* distribuci.
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            assert!(NAME.starts_with("WSLCD-Test-"));
            if wsl::list().map(|l| l.iter().any(|d| d.name == NAME)).unwrap_or(false) {
                let _ = wsl::terminate(NAME);
                let _ = wsl::unregister(NAME);
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let engine_distro = distro();
    let cfg = crate::config::Config::load();
    let cli = cfg.engine.cli_for(&crate::host::endpoint(&cfg), cfg.tls);
    let engine = |args: &[&str]| {
        let mut v = cli.clone();
        v.extend(args.iter().map(|s| s.to_string()));
        wsl::run_owned(&engine_distro, None, None, &v)
    };

    let tmp = std::env::temp_dir().join(format!("wslcd-test-import-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let guard = Cleanup(tmp.clone());
    if wsl::list().unwrap().iter().any(|d| d.name == NAME) {
        let _ = wsl::unregister(NAME);
    }

    // 1) rootfs.tar z kontejneru alpine
    let rootfs = tmp.join("rootfs.tar");
    let _ = engine(&["rm", "-f", CTR]);
    engine(&["create", "--name", CTR, "docker.io/library/alpine:3.20", "/bin/true"]).expect("create");
    let lin = wsl::to_linux_path(&engine_distro, &rootfs.display().to_string()).expect("wslpath");
    let exp = engine(&["export", "-o", &lin, CTR]);
    let _ = engine(&["rm", "-f", CTR]);
    exp.expect("container export");
    let size = std::fs::metadata(&rootfs).expect("rootfs.tar").len();
    eprintln!("rootfs.tar: {size} B");
    assert!(size > 1_000_000, "rootfs.tar je podezřele malý");

    // 2) import
    let folder = tmp.join("disk");
    let out = wsl::import(NAME, &folder.display().to_string(), &rootfs.display().to_string()).expect("import");
    eprintln!("import: {}", out.trim());
    let list = wsl::list().unwrap();
    let d = list.iter().find(|d| d.name == NAME).expect("WSLCD-Test-Import v wsl -l -v");
    eprintln!("wsl -l -v: {} {} v{}", d.name, d.state, d.version);
    let rel = wsl::run(NAME, Some("root"), None, &["cat", "/etc/alpine-release"]).expect("run");
    eprintln!("alpine-release: {}", rel.trim());
    assert!(rel.trim().starts_with("3.20"), "{rel}");

    // 3) disk
    let vhd = wsl::vhd_path(NAME).expect("vhd_path");
    eprintln!("vhd: {} ({} B)", vhd.display(), std::fs::metadata(&vhd).map(|m| m.len()).unwrap_or(0));
    assert!(vhd.starts_with(&folder), "{}", vhd.display());
    let meta = || std::fs::metadata(&vhd).map(|m| (m.len(), wsl::is_sparse(&m))).unwrap_or_default();
    eprintln!("manage support (sparse, compact): {:?}", wsl::manage_support());
    eprintln!("disk (size, sparse) po importu: {:?}", meta());
    let _ = wsl::terminate(NAME);
    // zhuštěný disk komprimovat nejde – nejdřív vypnout sparse (jen v testu, přímým voláním)
    // (na WSL 3.0.5 zůstává disk připojený k VM až do wsl --shutdown – ERROR_SHARING_VIOLATION
    // i po 60 s; shutdown by zastavil i ostatní distribuce, proto se jen vypíše výsledek)
    let o = wsl::hidden("wsl.exe").args(["--manage", NAME, "--set-sparse", "false"]).output().unwrap();
    eprintln!("set-sparse false: {} {:?} -> {:?}", o.status.success(), wsl::decode(&o.stdout).trim(), meta());
    let c = wsl::compact(NAME);
    eprintln!("compact: {c:?} -> {:?}", meta());
    let s = wsl::set_sparse(NAME);
    eprintln!("set-sparse true: {s:?} -> {:?}", meta());
    assert!(s.is_ok(), "{s:?}");
    assert!(meta().1, "disk není sparse");

    // 4) export (tar)
    let out_tar = tmp.join("export.tar");
    let out = wsl::export(NAME, &out_tar.display().to_string(), crate::wsl::ExportFormat::Tar).expect("export");
    eprintln!("export: {} -> {} B", out.trim(), std::fs::metadata(&out_tar).map(|m| m.len()).unwrap_or(0));
    assert!(std::fs::metadata(&out_tar).map(|m| m.len() > 1_000_000).unwrap_or(false));

    // 5) unregister
    let _ = wsl::terminate(NAME);
    wsl::unregister(NAME).expect("unregister");
    assert!(!wsl::list().unwrap().iter().any(|d| d.name == NAME), "distribuce zůstala registrovaná");
    drop(guard);
    assert!(!tmp.exists(), "dočasná složka zůstala");
}

/// Sdílený HTTP agent (keep-alive) proti novému agentovi pro každé volání.
#[test]
#[ignore]
fn live_shared_agent_speed() {
    let port: u16 = std::env::var("WSLCD_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375);
    let n = 20;
    let t = std::time::Instant::now();
    for _ in 0..n {
        assert!(Docker::with_tls(port, None).snapshot().connected);
    }
    let fresh = t.elapsed();
    let t = std::time::Instant::now();
    for _ in 0..n {
        assert!(Docker::new(&crate::http::Endpoint::Tcp { port }).snapshot().connected);
    }
    let shared = t.elapsed();
    eprintln!("{n}× snapshot: nový agent {fresh:?}, sdílený {shared:?}");
}

/// F-05: stack z labelů – labely z image a neexistující složka se odmítnou, skutečný projekt projde.
#[test]
#[ignore]
fn live_compose_label_trust() {
    let mut cfg = crate::config::Config::load();
    if let Ok(d) = std::env::var("WSLCD_DISTRO") {
        cfg.distro = d;
    }
    let d = Docker::new(&crate::host::endpoint(&cfg));
    let cli = cfg.engine.cli_for(&crate::host::endpoint(&cfg), cfg.tls);
    let run = |extra: &[&str]| {
        let mut a: Vec<String> = cli.clone();
        a.extend(extra.iter().map(|s| s.to_string()));
        wsl::run_owned(&cfg.distro, None, None, &a)
    };
    let _ = run(&["rm", "-f", "wslcd-test-trust-a", "wslcd-test-trust-b"]);
    // image, který si sám nastaví labely compose
    let dir = std::env::temp_dir().join(format!("wslcd-test-trust-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Dockerfile"),
        "FROM docker.io/library/alpine:3.20\nLABEL com.docker.compose.project=wslcd-test-evil com.docker.compose.project.working_dir=/tmp\n",
    )
    .unwrap();
    let spec = crate::worker::BuildSpec { context: dir.to_string_lossy().to_string(), tags: "wslcd-test-trust:latest".into(), ..Default::default() };
    crate::worker::build::run_build(&WslHost::new(&cfg.distro), cfg.engine, &Endpoint::Tcp { port: cfg.port }, cfg.tls, &spec, |_| {}).expect("build");
    run(&["create", "--name", "wslcd-test-trust-a", "wslcd-test-trust:latest", "true"]).expect("create a");
    let evil = crate::worker::check_stack_for_test(cfg.clone(), "/tmp", "wslcd-test-evil");
    // label na kontejneru (ne z image), ale složka neexistuje
    run(&["create", "--name", "wslcd-test-trust-b", "--label", "com.docker.compose.project=wslcd-test-gone", "docker.io/library/alpine:3.20", "true"]).expect("create b");
    let gone = crate::worker::check_stack_for_test(cfg.clone(), "/nonexistent/wslcd-test", "wslcd-test-gone");
    // stejný projekt, existující složka s compose souborem -> projde
    wsl::run(&cfg.distro, None, None, &["sh", "-c", "mkdir -p /tmp/wslcd-test-trust-ok && echo 'services: {}' > /tmp/wslcd-test-trust-ok/compose.yaml"]).unwrap();
    let ok = crate::worker::check_stack_for_test(cfg.clone(), "/tmp/wslcd-test-trust-ok", "wslcd-test-gone");
    let _ = run(&["rm", "-f", "wslcd-test-trust-a", "wslcd-test-trust-b"]);
    let _ = d.delete("/images/wslcd-test-trust:latest");
    let _ = wsl::run(&cfg.distro, None, None, &["rm", "-rf", "/tmp/wslcd-test-trust-ok"]);
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("evil: {evil:?}\ngone: {gone:?}\nok: {ok:?}");
    assert!(evil.is_err() && gone.is_err() && ok.is_ok());
}

/// F-11: proměnné prostředí se při spuštění předají souborem, ne v příkazové řádce.
#[test]
#[ignore]
fn live_run_env_file() {
    let mut cfg = crate::config::Config::load();
    if let Ok(d) = std::env::var("WSLCD_DISTRO") {
        cfg.distro = d;
    }
    let d = Docker::new(&crate::host::endpoint(&cfg));
    let _ = d.delete("/containers/wslcd-test-envfile?force=1");
    let spec = crate::worker::RunSpec {
        image: "alpine:3.20".into(),
        name: "wslcd-test-envfile".into(),
        env: "SECRET_TOKEN=tajne heslo=1\nPLAIN=x".into(),
        command: "sleep 60".into(),
        ..Default::default()
    };
    let r = crate::worker::run_for_test(cfg.clone(), spec);
    let v: serde_json::Value = d.get("/containers/wslcd-test-envfile/json").expect("inspect");
    let env: Vec<String> = serde_json::from_value(v.pointer("/Config/Env").cloned().unwrap_or_default()).unwrap_or_default();
    let left = wsl::run(&cfg.distro, None, None, &["sh", "-c", "ls ${TMPDIR:-/tmp}/wslcd-env.* 2>/dev/null | wc -l"]).unwrap_or_default();
    let _ = d.delete("/containers/wslcd-test-envfile?force=1");
    eprintln!("{r:?} env={env:?} zbylé soubory={}", left.trim());
    assert!(r.is_ok(), "{r:?}");
    assert!(env.iter().any(|e| e == "SECRET_TOKEN=tajne heslo=1") && env.iter().any(|e| e == "PLAIN=x"), "{env:?}");
    assert_eq!(left.trim(), "0");
}

/// Sloučené logy compose stacku: dva kontejnery s labely projektu `wslcd-test-logs` (jeden s TTY,
/// druhý po pár řádcích zmlkne), stejné streamy jako okno logů stacku (`logbuf::Streams`),
/// reorder buffer podle času a ukončení vláken po zastavení (i u mlčícího kontejneru).
/// `WSLCD_DISTRO=Debian cargo test live_stack_logs -- --ignored --nocapture`
#[test]
#[ignore]
fn live_stack_logs() {
    use crate::logbuf::{self, LogBuf, Streams, MAX_LINES, REORDER};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    const PROJECT: &str = "wslcd-test-logs";
    let port: u16 = std::env::var("WSLCD_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375);
    let d = Docker::new(&crate::http::Endpoint::Tcp { port });
    assert!(d.ping(), "API na {port} neodpovídá");
    let ver: serde_json::Value = d.get("/version").unwrap();
    let podman = ver.to_string().contains("Podman");
    let url = format!("tcp://127.0.0.1:{port}");
    let cli: Vec<&str> = if podman { vec!["podman", "--url", &url] } else { vec!["docker", "-H", &url] };
    let distro = distro();
    let names = ["wslcd-test-logs-web", "wslcd-test-logs-worker"];
    let cleanup = || {
        for n in names {
            let _ = d.container_action(n, "remove");
        }
    };
    cleanup();
    let run = |name: &str, svc: &str, tty: bool, script: &str| {
        let (pl, sl) = (format!("com.docker.compose.project={PROJECT}"), format!("com.docker.compose.service={svc}"));
        let mut a = cli.clone();
        a.extend(["run", "-d", "--name", name, "--label", &pl, "--label", &sl]);
        if tty {
            a.push("-t");
        }
        a.extend(["docker.io/library/alpine:3.20", "sh", "-c", script]);
        wsl::run(&distro, None, None, &a).unwrap_or_else(|e| panic!("run {name}: {e}"));
    };
    run(names[0], "web", false, "i=0; while [ $i -lt 400 ]; do i=$((i+1)); echo web line $i; sleep 0.2; done");
    run(names[1], "worker", true, "i=0; while [ $i -lt 15 ]; do i=$((i+1)); echo worker line $i; sleep 0.13; done; sleep 600");
    // historie pro tail (řádky obou služeb proložené v čase)
    std::thread::sleep(Duration::from_millis(1500));

    let snap = d.snapshot();
    let targets = logbuf::stack_targets(&snap.containers, PROJECT);
    eprintln!("targets: {:?}", targets.iter().map(|t| &t.1).collect::<Vec<_>>());
    assert_eq!(targets.iter().map(|t| t.1.as_str()).collect::<Vec<_>>(), ["web", "worker"]);
    let ids: Vec<String> = targets.iter().map(|t| t.0.clone()).collect();
    let buf = Arc::new(Mutex::new(LogBuf::new(MAX_LINES, Some(REORDER))));
    let streams = Streams::start(&crate::http::Endpoint::Tcp { port }, buf.clone(), &ids, 500);
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(100));
        buf.lock().unwrap().flush(Instant::now());
    }
    // worker už mlčí -> jeho vlákno visí na čtení; skončit musí díky zavření spojení
    let t = Instant::now();
    let ended = streams.stop_and_join(Duration::from_secs(3));
    eprintln!("vlákna skončila: {ended} za {:?}", t.elapsed());

    let mut g = buf.lock().unwrap();
    g.flush_all();
    let lines: Vec<_> = g.iter().cloned().collect();
    drop(g);
    cleanup();
    let svc_names = ["web".to_string(), "worker".to_string()];
    let shown = logbuf::format_lines(lines.iter(), Some(&svc_names), true, logbuf::local_offset);
    let all: Vec<&str> = shown.lines().collect();
    eprintln!("{} řádků, prvních 8:\n{}\n…\nposlední 4:\n{}", all.len(), all[..8.min(all.len())].join("\n"), all[all.len().saturating_sub(4)..].join("\n"));

    assert!(ended, "vlákna streamů po zastavení neskončila");
    let web = lines.iter().filter(|l| l.src == 0).count();
    let worker = lines.iter().filter(|l| l.src == 1).count();
    eprintln!("web: {web}, worker: {worker}");
    assert!(web >= 20, "web: {web}");
    assert_eq!(worker, 15, "worker (TTY) má poslat přesně 15 řádků");
    assert!(lines.iter().all(|l| l.ts.is_some()), "řádek bez času: {:?}", lines.iter().find(|l| l.ts.is_none()));
    assert!(lines.iter().all(|l| l.text.starts_with("web line ") || l.text.starts_with("worker line ")), "{lines:?}");
    let ts: Vec<i64> = lines.iter().filter_map(|l| l.ts).collect();
    assert!(ts.windows(2).all(|w| w[0] <= w[1]), "pořadí podle času není monotónní");
    // služby jsou proložené (řazení podle času, ne podle streamu)
    let switches = lines.windows(2).filter(|w| w[0].src != w[1].src).count();
    eprintln!("přepnutí mezi službami: {switches}");
    assert!(switches >= 4, "{switches}");
    // pořadí řádků v rámci služby zůstalo
    for (src, prefix) in [(0u16, "web line "), (1, "worker line ")] {
        let nums: Vec<u32> = lines.iter().filter(|l| l.src == src).filter_map(|l| l.text.strip_prefix(prefix)?.trim().parse().ok()).collect();
        assert!(nums.windows(2).all(|w| w[1] == w[0] + 1), "{prefix}{nums:?}");
    }
    let all_now: Vec<crate::docker::Container> = d.get("/containers/json?all=1").unwrap();
    assert!(!all_now.iter().any(|c| c.name().starts_with("wslcd-test-logs-")), "úklid");
}

/// Uvolnění paměti WSL: zahodí cache Linuxu a nahlásí VmmemWSL před a po.
#[test]
#[ignore]
fn live_free_memory() {
    let data = std::env::temp_dir().join(format!("wslcd-test-mem-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    // SAFETY (env): test běží samostatně
    unsafe { std::env::set_var("WSLCD_DATA_DIR", &data) };
    let cfg = crate::config::Config { distro: distro(), ..Default::default() };
    let r = crate::worker::testkit::free_memory(cfg);
    let _ = std::fs::remove_dir_all(&data);
    eprintln!("{r:?}");
    assert!(r.as_ref().is_ok_and(|s| s.contains("VmmemWSL") || s.contains("cache")), "{r:?}");
}

/// Sloučené logy stacku: kontejner, který naběhne později, se připojí za běhu (Streams::add)
/// a jeho řádky dorazí, aniž by se přerušil stream prvního.
#[test]
#[ignore]
fn live_stack_logs_attach_later() {
    use crate::logbuf::{self, LogBuf, Streams, MAX_LINES, REORDER};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    const PROJECT: &str = "wslcd-test-follow";
    let cfg = crate::config::Config::load();
    crate::docker::configure_tls(cfg.tls).expect("tls");
    let d = Docker::new(&crate::host::endpoint(&cfg));
    assert!(d.ping(), "API na {} neodpovídá", cfg.port);
    let cli = cfg.engine.cli_for(&crate::host::endpoint(&cfg), cfg.tls);
    let distro = std::env::var("WSLCD_DISTRO").unwrap_or(cfg.distro.clone());
    let run = |extra: &[&str]| {
        let mut a: Vec<String> = cli.clone();
        a.extend(extra.iter().map(|s| s.to_string()));
        wsl::run_owned(&distro, None, None, &a)
    };
    let start = |svc: &str| {
        let name = format!("{PROJECT}-{svc}");
        run(&[
            "run", "-d", "--name", &name,
            "--label", &format!("com.docker.compose.project={PROJECT}"),
            "--label", &format!("com.docker.compose.service={svc}"),
            "docker.io/library/alpine:3.20", "sh", "-c", &format!("i=0; while true; do i=$((i+1)); echo {svc} line $i; sleep 0.3; done"),
        ])
        .expect("run");
    };
    let _ = run(&["rm", "-f", "wslcd-test-follow-a", "wslcd-test-follow-b"]);
    start("a");
    std::thread::sleep(Duration::from_millis(800));
    let t0 = logbuf::stack_targets(&d.snapshot().containers, PROJECT);
    assert_eq!(t0.len(), 1, "{t0:?}");
    let buf = Arc::new(Mutex::new(LogBuf::new(MAX_LINES, Some(REORDER))));
    let ids: Vec<String> = t0.iter().map(|t| t.0.clone()).collect();
    let mut streams = Streams::start(&crate::host::endpoint(&cfg), buf.clone(), &ids, 50);
    std::thread::sleep(Duration::from_secs(2));
    // druhá služba naběhne až teď – připojit stejně jako okno logů (attach_new)
    start("b");
    std::thread::sleep(Duration::from_millis(800));
    let t1 = logbuf::stack_targets(&d.snapshot().containers, PROJECT);
    let new: Vec<_> = t1.iter().filter(|t| !ids.contains(&t.0)).cloned().collect();
    assert_eq!(new.len(), 1, "{t1:?}");
    streams.add(&crate::host::endpoint(&cfg), buf.clone(), &new[0].0, 1, 50);
    let a_before = buf.lock().unwrap().iter().filter(|l| l.src == 0).count();
    std::thread::sleep(Duration::from_secs(3));
    {
        let mut g = buf.lock().unwrap();
        g.flush(Instant::now() + Duration::from_secs(5));
    }
    let g = buf.lock().unwrap();
    let a = g.iter().filter(|l| l.src == 0).count();
    let b = g.iter().filter(|l| l.src == 1 && l.text.contains("b line")).count();
    drop(g);
    let stopped = streams.stop_and_join(Duration::from_secs(5));
    let _ = run(&["rm", "-f", "wslcd-test-follow-a", "wslcd-test-follow-b"]);
    eprintln!("a: {a_before} -> {a} řádků, b (připojeno později): {b}, vlákna skončila: {stopped}");
    assert!(a > a_before, "stream prvního kontejneru se přerušil");
    assert!(b >= 5, "řádky nového kontejneru nedorazily");
    assert!(stopped);
}

/// Přepnutí primární distribuce tam a zpět (WSLCD_PRIMARY_A = výchozí Ubuntu, WSLCD_PRIMARY_B = Debian):
/// API na stejném portu pokaždé obslouží engine zvolené distribuce, v té druhé se TCP API vypne;
/// při návratu na A se B uspí (wsl --terminate).
#[test]
#[ignore]
fn live_switch_primary() {
    let a = std::env::var("WSLCD_PRIMARY_A").unwrap_or_else(|_| "Ubuntu".into());
    let b = std::env::var("WSLCD_PRIMARY_B").unwrap_or_else(|_| "Debian".into());
    let data = std::env::temp_dir().join(format!("wslcd-test-primary-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    // SAFETY (env): test běží samostatně
    unsafe { std::env::set_var("WSLCD_DATA_DIR", &data) };
    let mut cfg = crate::config::Config { distro: a.clone(), port: 2376, tls: true, keep_alive: false, ..Default::default() };
    let engine_on_api = |port: u16| -> String {
        let v: serde_json::Value = Docker::new(&crate::http::Endpoint::Tcp { port }).get("/version").expect("/version");
        v["Components"][0]["Name"].as_str().unwrap_or("").to_string()
    };
    for target in [b.clone(), a.clone()] {
        let (r, log, c) = crate::worker::testkit::set_primary(cfg.clone(), &target, None, target == a);
        eprintln!("--- {target} ---\n{log}\n{r:?}");
        assert!(r.is_ok(), "{r:?}");
        assert_eq!(c.distro, target);
        crate::docker::configure_tls(c.tls).expect("tls");
        let name = engine_on_api(c.port);
        eprintln!("API: {name} ({})", c.engine.name());
        // Docker se hlásí jako „Engine“, Podman jako „Podman Engine“
        assert_eq!(name.contains("Podman"), c.engine == crate::engine::Engine::Podman, "{name} vs {:?}", c.engine);
        cfg = c;
    }
    // při návratu na A se B uspala
    let l = wsl::list().expect("wsl -l");
    assert!(l.iter().any(|d| d.name == b && !d.running()), "{b} běží: {l:?}");
    let _ = std::fs::remove_dir_all(&data);
}

/// Nastavení Docker API (jako „Nastavit API“) a překlad host.docker.internal v kontejneru –
/// na výchozím bridge i ve vlastní síti (compose). WSLCD_DISTRO s Dockerem, WSLCD_TLS=1 pro TLS.
#[test]
#[ignore]
fn live_docker_host_internal() {
    let d = distro();
    let tls = std::env::var("WSLCD_TLS").is_ok_and(|v| v == "1");
    let port = if tls { "2376" } else { "2375" };
    let user = wsl::run(&d, None, None, &["id", "-un"]).map(|u| u.trim().to_string()).unwrap_or_default();
    let script = crate::engine::Engine::Docker.setup_script();
    let out = wsl::run(
        &d,
        Some("root"),
        None,
        &["env", &format!("PORT={port}"), &format!("TLS={}", tls as u8), &format!("U={user}"), "REGEN=0", "sh", "-c", &script],
    )
    .expect("setup");
    eprintln!("{out}");
    assert!(out.contains("host.docker.internal -> 172.17.0.1"), "{out}");
    let probe = "getent hosts host.docker.internal && nslookup example.com >/dev/null && echo DNS-OK";
    let a = wsl::run(&d, Some("root"), None, &["docker", "run", "--rm", "docker.io/library/alpine:3.20", "sh", "-c", probe]).expect("bridge");
    let _ = wsl::run(&d, Some("root"), None, &["docker", "network", "create", "wslcd-test-hdi"]);
    let b = wsl::run(&d, Some("root"), None, &["docker", "run", "--rm", "--network", "wslcd-test-hdi", "docker.io/library/alpine:3.20", "sh", "-c", probe]);
    let _ = wsl::run(&d, Some("root"), None, &["docker", "network", "rm", "wslcd-test-hdi"]);
    let b = b.expect("user network");
    eprintln!("bridge: {a}\nnetwork: {b}");
    for r in [&a, &b] {
        assert!(r.contains("172.17.0.1") && r.contains("host.docker.internal") && r.contains("DNS-OK"), "{r}");
    }
}

/// Převod distribucí po přejmenování aplikace: v běžících ostatních distribucích odstraní služby
/// a soubory `wls-*` a uvolní API, v primární nastaví API pod novými názvy (`wslcd-*`).
/// Spouštěj s WSLCD_DATA_DIR = datová složka aplikace (certifikáty se uloží tam).
#[test]
#[ignore]
fn live_migrate_rename() {
    assert!(crate::config::isolated(), "nastav WSLCD_DATA_DIR");
    let cfg = crate::config::Config::load();
    let (r, log, _) = crate::worker::testkit::set_primary(cfg.clone(), &cfg.distro, Some(cfg.engine), false);
    eprintln!("{log}\n{r:?}");
    assert!(r.is_ok(), "{r:?}");
    for d in wsl::list().expect("wsl -l").iter().filter(|d| d.running()) {
        let left = wsl::run(&d.name, Some("root"), None, &["sh", "-c", "ls /etc/systemd/system/wls-* /etc/wls-desktop 2>/dev/null; getent group wls-api; true"]).unwrap_or_default();
        assert!(left.trim().is_empty(), "{}: {left}", d.name);
    }
}

// ------------------------------------------------------------------ live_container_guard

const GUARD_PREFIX: &str = "WSLCD-Guard-";

/// Úklid testu stropu paměti: odregistruje jen distribuce, které test sám vytvořil
/// (kromě `WSLCD_E2E_KEEP=1`), a po Dockeru vrátí sdílené iptables FORWARD na ACCEPT.
struct GuardCleanup {
    created: std::sync::Mutex<Vec<String>>,
    docker: std::sync::Mutex<Option<String>>,
}

impl Drop for GuardCleanup {
    fn drop(&mut self) {
        if let Some(d) = self.docker.lock().unwrap().as_deref() {
            let r = wsl::run(d, Some("root"), None, &["sh", "-c", "docker rm -f $(docker ps -aq --filter name=wslcd-test-guard) 2>/dev/null; iptables -P FORWARD ACCEPT; iptables -S FORWARD | head -1"]);
            eprintln!("úklid: FORWARD {r:?}");
        }
        if std::env::var("WSLCD_E2E_KEEP").is_ok() {
            eprintln!("WSLCD_E2E_KEEP: distribuce ponechány: {:?}", self.created.lock().unwrap());
            return;
        }
        for n in self.created.lock().unwrap().iter() {
            assert!(n.starts_with(GUARD_PREFIX));
            let _ = std::process::Command::new("wsl.exe").args(["--unregister", n]).output();
            eprintln!("úklid: {n} odregistrována");
        }
    }
}

fn guard_root(d: &str, script: &str) -> String {
    match wsl::run(d, Some("root"), None, &["sh", "-c", script]) {
        Ok(o) => o.trim().to_string(),
        Err(e) => format!("CHYBA: {e}"),
    }
}

fn guard_events_max(d: &str, slice: &str) -> u64 {
    let o = guard_root(d, &format!("awk '/^(max|high) /{{s+=$2}} END{{print s+0}}' /sys/fs/cgroup/{slice}/memory.events"));
    o.parse().unwrap_or(0)
}

/// Jeden scénář stropu na jedné distribuci: `docker` / `podman`, `lite` = WSLCD Lite.
fn guard_scenario(c: &GuardCleanup, name: &str, engine: crate::engine::Engine, lite: bool, port: u16, data: &std::path::Path) {
    use crate::engine::Engine;
    assert!(name.starts_with(GUARD_PREFIX), "test smí vytvářet jen distribuce {GUARD_PREFIX}*, ne {name}");
    let docker = engine == Engine::Docker;
    let cli = if docker { "docker" } else { "podman" };
    let img = "docker.io/library/alpine:3.20";
    eprintln!("=========== {name} ({cli}{}) ===========", if lite { ", Lite" } else { "" });
    let exists = wsl::list().expect("wsl -l").iter().any(|d| d.name == name);
    // Docker se nejdřív nainstaluje bez stropu (kontejner „vzniklý před --cgroup-parent“)
    let mut cfg = crate::config::Config { distro: name.into(), engine, port, tls: false, keep_alive: false, container_guard: !docker, ..Default::default() };
    if !exists {
        c.created.lock().unwrap().push(name.into());
        if lite {
            let spec = crate::worker::AddDistroSpec { distro: crate::lite::LITE_ID.into(), name: name.into(), engine: Some(engine), use_for_app: true, tls: false, ..Default::default() };
            let (r, log, _) = crate::worker::testkit::add_distro(cfg.clone(), spec);
            eprintln!("{log}\n{r:?}");
            r.expect("add_distro");
        } else {
            let loc = data.join("distros").join(name);
            std::fs::create_dir_all(&loc).unwrap();
            let out = std::process::Command::new("wsl.exe").args(["--install", "Ubuntu", "--no-launch", "--name", name, "--location"]).arg(&loc).output().expect("wsl --install");
            assert!(out.status.success(), "wsl --install: {}", String::from_utf8_lossy(&out.stdout));
            let prep = wsl::run(name, Some("root"), None, &["env", "U=tester", "sh", "-c", &crate::engine::distro_prepare_script()]).expect("příprava");
            assert!(prep.contains("systemd=true"), "{prep}");
            let _ = wsl::terminate(name);
            std::thread::sleep(std::time::Duration::from_secs(3));
            wsl::start(name).expect("start");
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
    }
    if docker {
        *c.docker.lock().unwrap() = Some(name.into());
    }
    if exists || !lite {
        let (r, log) = crate::worker::testkit::install_engine(cfg.clone(), engine, false);
        assert!(r.is_ok(), "{r:?}\n{log}");
    }
    let mut keep = wsl::keepalive(name).unwrap();
    let kill = |k: &mut std::process::Child| {
        let _ = k.kill();
        let _ = k.wait();
    };
    std::thread::sleep(std::time::Duration::from_secs(2));
    let _ = guard_root(name, &format!("{cli} rm -f wslcd-test-guard wslcd-test-guard-old"));
    let _ = guard_root(name, &format!("{cli} pull {img}"));

    // kontejner vzniklý před stropem (jen Docker)
    if docker {
        eprintln!("{}", guard_root(name, &format!("docker run -d --name wslcd-test-guard-old {img} sleep 3000")));
    }

    // 1) setup se stropem
    cfg.container_guard = true;
    let (r, log) = crate::worker::testkit::install_engine(cfg.clone(), engine, false);
    assert!(r.is_ok(), "{r:?}\n{log}");
    let direct = guard_root(name, "sh /usr/local/lib/wsl-container-desktop/guard.sh 0");
    eprintln!("1) guard.sh: {direct}");
    assert!(direct.contains("wslcd-guard: max="), "{direct}");
    let slice = if lite {
        if docker { "wslcd-containers" } else { "libpod_parent" }
    } else if docker {
        "wslcd.slice/wslcd-containers.slice"
    } else {
        "machine.slice"
    };
    let max = guard_root(name, &format!("cat /sys/fs/cgroup/{slice}/memory.max"));
    eprintln!("1) {slice}/memory.max = {max}");
    assert!(max.parse::<u64>().is_ok(), "{max}");

    // 2) kontejner je pod stropem
    let probe = format!(
        "{cli} run -d --name wslcd-test-guard {img} sleep 300 >/dev/null; pid=$({cli} inspect -f '{{{{.State.Pid}}}}' wslcd-test-guard); cat /proc/$pid/cgroup; p=$(sed -n 's/^0:://p' /proc/$pid/cgroup); echo PARENT=$(dirname $p); echo PARENT_MAX=$(cat /sys/fs/cgroup$(dirname $p)/memory.max); echo OOMADJ=$(cat /proc/$pid/oom_score_adj)"
    );
    let out = guard_root(name, &probe);
    eprintln!("2) {out}");
    if docker {
        assert!(out.contains(if lite { "/wslcd-containers/" } else { "wslcd-containers.slice" }), "{out}");
    } else {
        assert!(out.contains(if lite { "/libpod_parent/" } else { "/machine.slice/" }), "{out}");
    }
    let pm = out.lines().find_map(|l| l.strip_prefix("PARENT_MAX=")).unwrap_or("");
    if docker {
        assert!(pm.parse::<u64>().is_ok(), "memory.max rodiče kontejneru: {out}");
    }

    // 3) malý strop a spotřeba nad ním (Docker)
    if docker {
        let before = guard_events_max(name, slice);
        let mt: u64 = guard_root(name, "awk '/^MemTotal:/{print int($2/1024)}' /proc/meminfo").parse().unwrap();
        eprintln!("3) {}", guard_root(name, &format!("sh /usr/local/lib/wsl-container-desktop/guard.sh {}", mt - 768)));
        let run = guard_root(name, &format!("docker run --rm {img} sh -c 'head -c 1500m /dev/zero | tail >/dev/null'; echo exit=$?"));
        let after = guard_events_max(name, slice);
        let state = guard_root(name, "systemctl is-active docker");
        let oom = guard_root(name, &format!("cat /sys/fs/cgroup/{slice}/memory.events /sys/fs/cgroup/{slice}/memory.swap.current | tr '\\n' ' '"));
        eprintln!("3) run: {run}\n3) memory.events max+high {before} -> {after}, {oom}, docker: {state}");
        assert!(after > before, "čítač max ve slice nevzrostl");
        assert_eq!(state, "active");
        let r = guard_root(name, &format!("systemctl restart wslcd-guard; sleep 1; cat /sys/fs/cgroup/{slice}/memory.max"));
        eprintln!("3) po restartu wslcd-guard: {r}");
    }

    // 4) terminate + start: pořadí jednotek, strop hned po startu
    kill(&mut keep);
    let _ = guard_root(name, &format!("{cli} rm -f wslcd-test-guard"));
    let _ = wsl::terminate(name);
    std::thread::sleep(std::time::Duration::from_secs(3));
    wsl::start(name).expect("start");
    let mut keep = wsl::keepalive(name).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(if lite { 2 } else { 6 }));
    let max = guard_root(name, &format!("cat /sys/fs/cgroup/{slice}/memory.max"));
    eprintln!("4) po startu {slice}/memory.max = {max}");
    assert!(max.parse::<u64>().is_ok(), "{max}");
    if !lite {
        let unit = if docker { "docker" } else { "wslcd-podman-api" };
        let t = guard_root(name, &format!("echo $(systemctl show -p ExecMainExitTimestampMonotonic --value wslcd-guard) $(systemctl show -p ExecMainStartTimestampMonotonic --value {unit})"));
        eprintln!("4) guard exit / {unit} start: {t}");
        let v: Vec<u64> = t.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        assert!(v.len() == 2 && v[0] <= v[1], "{t}");
    }

    // 5) docker restart kontejneru vzniklého před --cgroup-parent (KNOWN-ISSUES 1)
    if docker {
        let o = guard_root(name, "docker restart wslcd-test-guard-old >/dev/null; pid=$(docker inspect -f '{{.State.Pid}}' wslcd-test-guard-old); cat /proc/$pid/cgroup");
        eprintln!("5) starý kontejner po docker restart: {o} (pod stropem: {})", o.contains("wslcd-containers"));
    }
    let _ = guard_root(name, &format!("{cli} rm -f $({cli} ps -aq --filter name=wslcd-test-guard) 2>/dev/null"));
    kill(&mut keep);
}

/// Živý test stropu paměti pro kontejnery. Vytvoří si vlastní distribuce `WSLCD-Guard-*`
/// (Ubuntu z obchodu pod `target\guard-live-data`, Lite přes testkit), izolovaná data, porty 2391–2394.
/// Proměnné: WSLCD_GUARD_DOCKER, WSLCD_GUARD_PODMAN, WSLCD_GUARD_LITE, WSLCD_GUARD_LITE_PODMAN (jména).
#[test]
#[ignore]
fn live_container_guard() {
    use crate::engine::Engine;
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("guard-live-data");
    std::fs::create_dir_all(&data).unwrap();
    // SAFETY (env): test běží samostatně
    unsafe { std::env::set_var("WSLCD_DATA_DIR", &data) };
    let c = GuardCleanup { created: Default::default(), docker: Default::default() };
    let parts = [
        ("WSLCD_GUARD_DOCKER", Engine::Docker, false, 2391u16),
        ("WSLCD_GUARD_PODMAN", Engine::Podman, false, 2392),
        ("WSLCD_GUARD_LITE", Engine::Docker, true, 2393),
        ("WSLCD_GUARD_LITE_PODMAN", Engine::Podman, true, 2394),
    ];
    for (var, engine, lite, port) in parts {
        match std::env::var(var) {
            Ok(n) if !n.is_empty() => guard_scenario(&c, &n, engine, lite, port, &data),
            _ => eprintln!("{var} není nastaveno – přeskočeno"),
        }
    }
    drop(c);
    let real = std::fs::read_to_string(r"C:\WSLContainerDesktop\data\config.json").unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&real).unwrap_or_default();
    assert_eq!(v["distro"], "WSLCD-Lite-Docker", "skutečná konfigurace se nesmí změnit");
}
