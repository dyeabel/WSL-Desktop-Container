//! Živý test Kubernetes laboratoře proti enginu z konfigurace aplikace.
//!
//! Spuštění (stáhne k3s a image šablony, trvá několik minut):
//! `$env:WSLCD_K8S_DATA="C:\WSLContainerDesktop\data"; cargo test live_k8s -- --ignored --nocapture --test-threads=1`
//!
//! Pozor: běží proti **skutečnému primárnímu enginu** z `WSLCD_K8S_DATA\config.json`. Konfigurace
//! a certifikáty TLS se zkopírují do izolované složky `target\k8s-live-data` (nic se nepřepíše),
//! cluster `wslcd-k3s` se na začátku i na konci smaže (kromě `WSLCD_E2E_KEEP=1`).

use crate::config::Config;
use crate::k8s::cluster::ClusterState;
use crate::k8s::flows::Node;
use crate::k8s::policy::Peer;
use crate::worker::testkit::k8s;
use crate::worker::{K8sMsg, K8sOp};
use std::time::{Duration, Instant};

fn snapshot(cfg: &Config, ns: Option<&str>, flows: bool) -> crate::worker::k8s::K8sSnap {
    let (r, msgs) = k8s(cfg.clone(), K8sOp::Snapshot { ns: ns.map(str::to_string), flows });
    r.unwrap();
    msgs.into_iter()
        .find_map(|m| if let K8sMsg::Snapshot(s) = m { Some(*s) } else { None })
        .expect("snapshot")
}

fn probe(cfg: &Config, from: Peer, to: &str, port: u16) -> Result<bool, String> {
    let (_, msgs) = k8s(cfg.clone(), K8sOp::Probe { ns: "wslcd-live".into(), from, to: to.into(), port });
    msgs.into_iter().find_map(|m| if let K8sMsg::Probe { result, .. } = m { Some(result) } else { None }).expect("probe")
}

struct Cleanup(Config);

impl Drop for Cleanup {
    fn drop(&mut self) {
        if std::env::var("WSLCD_E2E_KEEP").is_ok() {
            eprintln!("WSLCD_E2E_KEEP: cluster ponechán");
            return;
        }
        let (r, _) = k8s(self.0.clone(), K8sOp::Delete);
        eprintln!("úklid: cluster smazán: {r:?}");
    }
}

/// Spustí (případně převede) už existující cluster stejně jako tlačítko Spustit – nic nemaže.
/// `$env:WSLCD_K8S_DATA="C:\WSLContainerDesktop\data"; cargo test live_k8s_start_existing -- --ignored --nocapture`
#[test]
#[ignore]
fn live_k8s_start_existing() {
    let cfg = isolated_cfg();
    let (r, msgs) = k8s(cfg.clone(), K8sOp::Start);
    eprintln!("start: {r:?}\n{msgs:?}");
    r.unwrap();
    assert_eq!(snapshot(&cfg, None, false).state, ClusterState::Running);
}

/// Konfigurace a certifikáty z `WSLCD_K8S_DATA` zkopírované do `target\k8s-live-data`.
fn isolated_cfg() -> Config {
    let src = std::path::PathBuf::from(std::env::var("WSLCD_K8S_DATA").expect("WSLCD_K8S_DATA = datová složka aplikace"));
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("k8s-live-data");
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(data.join("tls")).unwrap();
    std::fs::copy(src.join("config.json"), data.join("config.json")).unwrap();
    for f in std::fs::read_dir(src.join("tls")).into_iter().flatten().flatten() {
        let _ = std::fs::copy(f.path(), data.join("tls").join(f.file_name()));
    }
    std::env::set_var("WSLCD_DATA_DIR", &data);
    let cfg = Config::load();
    crate::docker::configure_tls(cfg.tls).unwrap();
    eprintln!("engine {} v {} (port {})", cfg.engine.name(), cfg.distro, cfg.port);
    cfg
}

#[test]
#[ignore]
fn live_k8s_lab() {
    let src = std::path::PathBuf::from(std::env::var("WSLCD_K8S_DATA").expect("WSLCD_K8S_DATA = datová složka aplikace"));
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("k8s-live-data");
    let _ = std::fs::remove_dir_all(&data);
    std::fs::create_dir_all(data.join("tls")).unwrap();
    std::fs::copy(src.join("config.json"), data.join("config.json")).unwrap();
    for f in std::fs::read_dir(src.join("tls")).into_iter().flatten().flatten() {
        let _ = std::fs::copy(f.path(), data.join("tls").join(f.file_name()));
    }
    std::env::set_var("WSLCD_DATA_DIR", &data);
    let cfg = Config::load();
    crate::docker::configure_tls(cfg.tls).unwrap();
    eprintln!("engine {} v {} (port {})", cfg.engine.name(), cfg.distro, cfg.port);

    let _ = k8s(cfg.clone(), K8sOp::Delete);
    let _clean = Cleanup(cfg.clone());
    let t = Instant::now();
    let (r, msgs) = k8s(cfg.clone(), K8sOp::Create);
    eprintln!("create ({:?}): {r:?}\n{msgs:?}", t.elapsed());
    let out = r.unwrap();
    assert!(out.contains("kube"), "{out}");
    assert!(data.join("kube").join("config").exists(), "kubeconfig v datové složce");
    assert_eq!(snapshot(&cfg, None, false).state, ClusterState::Running);

    let (r, _) = k8s(cfg.clone(), K8sOp::DeployDemo { ns: "wslcd-live".into(), with_rules: true });
    eprintln!("deploy: {r:?}");
    r.unwrap();
    let t = Instant::now();
    let env = loop {
        let s = snapshot(&cfg, Some("wslcd-live"), false);
        if let Some(env) = s.env.filter(|e| e.pods.len() >= 3 && e.pods.iter().all(|p| p.ready.1 > 0 && p.ready.0 == p.ready.1)) {
            break env;
        }
        assert!(t.elapsed() < Duration::from_secs(400), "pody nenaběhly: {:?}", snapshot(&cfg, Some("wslcd-live"), false).env.map(|e| e.pods));
        std::thread::sleep(Duration::from_secs(5));
    };
    eprintln!("pody připravené za {:?}", t.elapsed());
    assert!(env.rules.deny_all && env.rules.rules.len() == 3, "{:?}", env.rules);
    let np = env.svcs.iter().find(|s| s.name == "fe").and_then(|s| s.ports[0].node_port).expect("fe zpřístupněné");

    // FE → API → DB přes NodePort z Windows (PostgREST chvíli čeká na databázi)
    let url = format!("http://127.0.0.1:{np}/api/items");
    let t = Instant::now();
    let body = loop {
        match ureq::get(&url).timeout(Duration::from_secs(5)).call() {
            Ok(r) => break r.into_string().unwrap(),
            Err(e) => {
                assert!(t.elapsed() < Duration::from_secs(120), "{url}: {e}");
                std::thread::sleep(Duration::from_secs(3));
            }
        }
    };
    eprintln!("{url}: {body}");
    assert!(body.contains("položka"), "{body}");

    let c = |s: &str| Peer::Component(s.into());
    assert_eq!(probe(&cfg, c("fe"), "api", 3000), Ok(true), "fe → api povoleno");
    assert_eq!(probe(&cfg, c("api"), "db", 5432), Ok(true), "api → db povoleno");
    assert_eq!(probe(&cfg, c("fe"), "db", 5432), Ok(false), "fe → db zakázáno default deny");
    assert_eq!(probe(&cfg, Peer::Localhost, "fe", 80), Ok(true), "localhost → fe");

    // mapa: stránka se ptá API každé 2 s – po pár sekundách musí být vidět celá cesta
    std::thread::sleep(Duration::from_secs(4));
    let _ = ureq::get(&url).call();
    let env = snapshot(&cfg, Some("wslcd-live"), true).env.unwrap();
    let edges = crate::ui::k8s_map::collapse(&env.edges);
    for e in &edges {
        eprintln!("  {} → {} :{} via {:?} live {} recent {}", e.from.label(), e.to.label(), e.port, e.via, e.live, e.recent);
    }
    let has = |f: Node, t: Node| edges.iter().any(|e| e.from == f && e.to == t);
    let n = |s: &str| Node::Component(s.into());
    assert!(has(Node::Localhost, n("fe")), "localhost → fe");
    assert!(has(n("fe"), n("api")), "fe → api");
    assert!(has(n("api"), n("db")), "api → db");
    assert!(!has(n("fe"), n("db")), "fe → db nesmí být");
}
