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

/// Syntaktická kontrola všech skriptů enginu (`sh -n`) přímo v distribuci.
#[test]
#[ignore]
fn live_engine_scripts_syntax() {
    use crate::engine::Engine;
    for e in Engine::ALL {
        for (what, s) in [("setup", e.setup_script()), ("install", e.install_script()), ("uninstall", e.uninstall_script())] {
            let r = wsl::run(&distro(), None, None, &["sh", "-n", "-c", s]);
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
