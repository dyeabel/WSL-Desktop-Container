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
    wsl::run_owned(&distro, None, None, &spec.args_for(Engine::Podman, port, false)).expect("podman run");
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
    set_docker_host(Some((2375, false))).expect("set");
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
    wsl::run_owned(&cfg.distro, None, None, &spec.args_for(engine, port, false)).expect("run");
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
    let res = crate::worker::recreate_for_test(cfg.clone(), &det.id, changed, det.net_edit(engine == Engine::Podman));
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
    let res = crate::worker::recreate_for_test(cfg.clone(), &new.id, bad, new.net_edit(engine == Engine::Podman));
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

/// Znovuvytvoření zachová všechny sítě (aliasy, pevnou IP) i anonymní volume s daty.
/// Pracuje jen s vlastními objekty `wls-test-recreate`, `wls-test-net-a`, `wls-test-net-b`
/// a anonymními volume tohoto kontejneru.
/// `WLS_DISTRO=Debian cargo test live_recreate_networks -- --ignored --nocapture`
#[test]
#[ignore]
fn live_recreate_networks() {
    use crate::engine::Engine;
    use serde_json::{json, Value};
    let port: u16 = std::env::var("WLS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375);
    let d = Docker::new(port);
    assert!(d.ping(), "API na {port} neodpovídá");
    let ver: Value = d.get("/version").unwrap();
    let engine = if ver.to_string().contains("Podman") { Engine::Podman } else { Engine::Docker };
    let cfg = crate::config::Config { distro: distro(), engine, port, ..Default::default() };
    let name = "wls-test-recreate";
    let (net_a, net_b, ip_b) = ("wls-test-net-a", "wls-test-net-b", "10.231.77.50");
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
        for c in [name, "wls-test-recreate-wls-old"] {
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
        d.stream_logs(id, 10, |c| {
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
    assert!(d.get::<Value>("/containers/wls-test-recreate-wls-old/json").is_err());
    // anonymní volume po odstranění starého kontejneru stále existuje
    assert!(d.get::<Value>(&format!("/volumes/{}", anon[0])).is_ok());

    cleanup(&anon);
    assert!(d.get::<Value>(&format!("/volumes/{}", anon[0])).is_err(), "volume uklizeno");
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
    let port: u16 = std::env::var("WLS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = Docker::new(port).stream_events(|| {}, |e| tx.send((e.action().to_string(), e.name(), e.triggers_refresh())).is_ok());
    });
    std::thread::sleep(std::time::Duration::from_millis(500));
    let d = Docker::new(port);
    d.post_json("/containers/create?name=wls-test-events", serde_json::json!({"Image": "docker.io/library/alpine:3.20", "Cmd": ["true"]}))
        .expect("create");
    d.delete("/containers/wls-test-events?force=1").expect("remove");
    let mut seen = vec![];
    while let Ok(ev) = rx.recv_timeout(std::time::Duration::from_secs(5)) {
        if ev.1 == "wls-test-events" {
            seen.push(ev);
        }
    }
    eprintln!("{seen:?}");
    assert!(seen.iter().any(|e| e.0 == "create" && e.2), "{seen:?}");
    assert!(seen.iter().any(|e| e.0 == "remove" && e.2), "{seen:?}");
}

/// Sestavení image z Dockerfile stejnou cestou jako aplikace (`worker::build::run_build`):
/// dočasná složka v %TEMP% (windowsová cesta -> wslpath), engine a distribuce z konfigurace
/// (WLS_DISTRO ji přepíše), API na portu WLS_PORT (výchozí 2375). Image `wls-test-build:latest`
/// a složka se na konci smažou.
#[test]
#[ignore]
fn live_build_image() {
    let cfg = crate::config::Config::load();
    let distro = std::env::var("WLS_DISTRO").unwrap_or(cfg.distro.clone());
    let port: u16 = std::env::var("WLS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375);
    let d = Docker::new(port);
    assert!(d.ping(), "API na {port} neodpovídá");

    let dir = std::env::temp_dir().join(format!("wls-test-build-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Dockerfile"), "FROM docker.io/library/alpine:3.20\nRUN echo ok\n").unwrap();

    let spec = crate::worker::BuildSpec {
        context: dir.to_string_lossy().to_string(),
        tags: "wls-test-build:latest".into(),
        ..Default::default()
    };
    let mut lines = Vec::new();
    let res = crate::worker::build::run_build(&distro, cfg.engine, port, false, &spec, |l| {
        eprintln!("| {l}");
        lines.push(l.to_string());
    });
    eprintln!("=> {res:?} ({} řádků výstupu)", lines.len());

    let img: Result<serde_json::Value, crate::docker::ApiError> = d.get("/images/wls-test-build:latest/json");
    let removed = d.delete("/images/wls-test-build:latest");
    eprintln!("odstranění image: {removed:?}");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(res.is_ok(), "{res:?}");
    assert!(lines.len() > 2, "výstup se nestreamoval: {lines:?}");
    let img = img.expect("image v API");
    eprintln!("image Id: {}", img.get("Id").and_then(|v| v.as_str()).unwrap_or("?"));
    assert!(removed.is_ok(), "{removed:?}");
    assert!(d.get::<serde_json::Value>("/images/wls-test-build:latest/json").is_err(), "image zůstal");
    assert!(!dir.exists());
}

/// TLS proxy end-to-end bez zásahu do běžícího enginu: dočasná Podman služba na unix socketu,
/// `tls_proxy` (stejný skript jako aplikace) s jinou složkou a službou na portu 2377,
/// pak klient s certifikátem (musí projít), bez certifikátu a přes HTTP (musí selhat).
/// Potřebuje podman a doinstaluje socat (pokud chybí).
#[test]
#[ignore]
fn live_tls_proxy() {
    let distro = distro();
    let root = |args: &[&str]| wsl::run(&distro, Some("root"), None, args);
    let cleanup = || {
        let _ = root(&["sh", "-c", "systemctl disable --now wls-test-tls-proxy wls-test-podman >/dev/null 2>&1; \
            rm -f /etc/systemd/system/wls-test-tls-proxy.service; systemctl daemon-reload; rm -rf /tmp/wls-test-tls /tmp/wls-test-podman.sock"]);
    };
    cleanup();
    root(&["sh", "-c", "systemd-run --unit wls-test-podman podman system service --time=0 unix:///tmp/wls-test-podman.sock && \
        for i in $(seq 50); do [ -S /tmp/wls-test-podman.sock ] && break; sleep 0.2; done"]).expect("podman socket");
    let script = format!(
        "{}\ntls_proxy /tmp/wls-test-podman.sock wls-test-podman.service && sleep 1 && systemctl is-active wls-test-tls-proxy",
        crate::engine::Engine::Podman.setup_script().split("\nset -e\n").next().unwrap()
    );
    let out = root(&["env", "PORT=2377", "WLS_TLS_DIR=/tmp/wls-test-tls", "WLS_TLS_UNIT=wls-test-tls-proxy", "sh", "-c", &script]);
    eprintln!("{out:?}");
    // certifikáty do dočasné složky ve Windows
    let dir = std::env::temp_dir().join("wls-test-tls");
    let _ = std::fs::create_dir_all(&dir);
    for f in ["ca.pem", "cert.pem", "key.pem"] {
        let pem = root(&["cat", &format!("/tmp/wls-test-tls/{f}")]).expect(f);
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
    let _ = std::fs::remove_dir_all(&dir);
    cleanup();
    let snap = ok.expect("tls_config");
    assert!(out.is_ok(), "{out:?}");
    assert!(snap.connected && snap.error.is_none(), "{:?}", snap.error);
    assert!(!no_cert, "spojení bez klientského certifikátu prošlo");
    assert!(!plain, "HTTP bez TLS prošlo");
}

/// Operace jako root nesmí psát / měnit práva skrz symlink (F-10).
#[test]
#[ignore]
fn live_fs_root_symlink_safe() {
    let d = distro();
    let dir = "/tmp/wls-test-fs-root";
    let root = |a: &[&str]| wsl::run(&d, Some("root"), None, a);
    root(&["sh", "-c", "rm -rf /tmp/wls-test-fs-root && mkdir /tmp/wls-test-fs-root && cd /tmp/wls-test-fs-root && echo tajne > secret && chmod 600 secret && ln -s secret link && echo a > plain && chmod 640 plain"]).unwrap();
    let w = fs::write_text(&d, true, &format!("{dir}/link"), "pwned\n");
    let c = fs::chmod(&d, true, "666", &format!("{dir}/link"));
    let p = fs::write_text(&d, true, &format!("{dir}/plain"), "b\n");
    let state = root(&["sh", "-c", "cd /tmp/wls-test-fs-root && cat secret && stat -c '%a %n' secret plain && cat plain && ls -a"]).unwrap();
    let _ = root(&["rm", "-rf", dir]);
    eprintln!("{w:?} {c:?} {p:?}\n{state}");
    assert!(w.is_err() && c.is_err(), "{w:?} {c:?}");
    assert!(p.is_ok(), "{p:?}");
    assert!(state.contains("tajne") && state.contains("600 secret") && state.contains("640 plain") && state.contains("b\n"), "{state}");
    assert!(!state.contains(".wls-edit"), "{state}");
}

/// Záloha, obnova a klon volume + velikosti (jen volume `wls-test-*`).
/// `WLS_DISTRO=Debian cargo test live_volume_backup_restore -- --ignored --nocapture`
#[test]
#[ignore]
fn live_volume_backup_restore() {
    use crate::engine::Engine;
    use crate::worker::{volume_op_for_test, Cmd, HELPER_IMAGE};
    let port: u16 = std::env::var("WLS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375);
    let d = Docker::new(port);
    assert!(d.ping(), "API na {port} neodpovídá");
    let ver: serde_json::Value = d.get("/version").unwrap();
    let engine = if ver.to_string().contains("Podman") { Engine::Podman } else { Engine::Docker };
    let cfg = crate::config::Config { distro: distro(), engine, port, ..Default::default() };
    let cli = engine.cli(port, false);
    let (src, dst, cln) = ("wls-test-backup", "wls-test-restore", "wls-test-clone");
    // složka s mezerou a apostrofem (uvozovky Windows v názvech nedovolí)
    let dir = std::env::temp_dir().join("wls-test backup it's");
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
    let du = crate::worker::du_sizes(&cfg.distro, &[(mp.clone(), src.to_string())].into());
    eprintln!("du -sb {mp}: {:?}", du.get(src));
    assert!(du.get(src).is_some_and(|s| *s > 0), "{du:?}");

    // záloha do Windows
    let target = dir.join("wls-test-backup 'x'.tar.gz").to_string_lossy().to_string();
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
    eprintln!("úklid hotov, zbývající volume wls-test-*: {}", left.to_string().matches("\"wls-test-").count());
    assert!(!left.to_string().contains("\"wls-test-backup\"") && !dir.exists());
}

/// Zrušení sestavení: `RUN sleep 45` se po zrušení nesmí dál provádět v distribuci.
#[test]
#[ignore]
fn live_build_cancel() {
    let cfg = crate::config::Config::load();
    let distro = std::env::var("WLS_DISTRO").unwrap_or(cfg.distro.clone());
    let dir = std::env::temp_dir().join(format!("wls-test-build-cancel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Dockerfile"), "FROM docker.io/library/alpine:3.20\nRUN sleep 45 && echo wlscancel\n").unwrap();
    let spec = crate::worker::BuildSpec { context: dir.to_string_lossy().to_string(), tags: "wls-test-cancel:latest".into(), ..Default::default() };
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(8));
        crate::worker::cancel_build();
    });
    let t = std::time::Instant::now();
    let res = crate::worker::build::run_build(&distro, cfg.engine, cfg.port, cfg.tls, &spec, |l| eprintln!("| {l}"));
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
/// dočasná složka /tmp/wls-test-compose-*, projekt `wls-test-compose`, port hostitele mimo 6379.
/// Pak `config -q` (soubor i obsah editoru přes stdin), `up -d`, kontrola kontejneru přes API,
/// nakonec `down -v` a smazání složky. Jiných stacků, kontejnerů, volume ani image se nedotýká.
#[test]
#[ignore]
fn live_compose_new_stack() {
    use crate::worker::compose::{create_stack, validate, NewStackSpec};
    let cfg = crate::config::Config::load();
    let distro = std::env::var("WLS_DISTRO").unwrap_or(cfg.distro.clone());
    let port: u16 = std::env::var("WLS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375);
    let d = Docker::new(port);
    assert!(d.ping(), "API na {port} neodpovídá");
    let cli = cfg.engine.cli(port, false);
    let project = "wls-test-compose";
    let dir = format!("/tmp/wls-test-compose-{}", std::process::id());
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
    let created = create_stack(&distro, &cli, &spec);
    eprintln!("create_stack: {created:?}");
    let again = create_stack(&distro, &cli, &spec);
    eprintln!("znovu do stejné složky: {again:?}");
    let files = fs::list(&distro, false, &dir).map(|(_, e)| e.iter().map(|x| format!("{} {}", x.mode, x.name)).collect::<Vec<_>>());
    eprintln!("soubory: {files:?}");
    let compose_file = fs::join(&dir, "compose.yaml");
    let check_file = validate(&distro, &cli, project, &dir, &[compose_file.clone()], None, "");
    eprintln!("config -q (soubor): {check_file:?}");
    let check_bad = validate(&distro, &cli, project, &dir, &["-".into()], None, "services:\n  redis: [\n");
    eprintln!("config -q (rozbitý obsah editoru): {check_bad:?}");
    let check_env = validate(&distro, &cli, project, &dir, &[compose_file.clone()], Some("REDIS_PORT=1\n"), "");
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
    let rm = fs::delete(&distro, false, &[dir.clone()]);
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
    assert!(rm.is_ok() && fs::list(&distro, false, &dir).is_err());
}

/// Správa distribucí: rootfs alpine (export kontejneru přes engine v WLS_DISTRO) -> wsl --import
/// jako WLS-Test-Import, kontrola v `wsl -l -v`, disk (sparse, compact), export, unregister.
/// Sahá jen na distribuci WLS-Test-Import. Stahuje alpine:3.20 (~3 MB).
/// `WLS_DISTRO=Debian cargo test live_distro_manage -- --ignored --nocapture`
#[test]
#[ignore]
fn live_distro_manage() {
    const NAME: &str = "WLS-Test-Import";
    const CTR: &str = "wls-test-rootfs";
    /// Úklid i při panice – odregistruje jen WLS-Test-* distribuci.
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            assert!(NAME.starts_with("WLS-Test-"));
            if wsl::list().map(|l| l.iter().any(|d| d.name == NAME)).unwrap_or(false) {
                let _ = wsl::terminate(NAME);
                let _ = wsl::unregister(NAME);
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let engine_distro = distro();
    let cfg = crate::config::Config::load();
    let cli = cfg.engine.cli(cfg.port, cfg.tls);
    let engine = |args: &[&str]| {
        let mut v = cli.clone();
        v.extend(args.iter().map(|s| s.to_string()));
        wsl::run_owned(&engine_distro, None, None, &v)
    };

    let tmp = std::env::temp_dir().join(format!("wls-test-import-{}", std::process::id()));
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
    let d = list.iter().find(|d| d.name == NAME).expect("WLS-Test-Import v wsl -l -v");
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
    let port: u16 = std::env::var("WLS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375);
    let n = 20;
    let t = std::time::Instant::now();
    for _ in 0..n {
        assert!(Docker::with_tls(port, None).snapshot().connected);
    }
    let fresh = t.elapsed();
    let t = std::time::Instant::now();
    for _ in 0..n {
        assert!(Docker::new(port).snapshot().connected);
    }
    let shared = t.elapsed();
    eprintln!("{n}× snapshot: nový agent {fresh:?}, sdílený {shared:?}");
}
