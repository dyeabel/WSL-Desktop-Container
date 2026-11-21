//! Živý test WSLCD Lite: `cargo test live_lite -- --ignored --nocapture --test-threads=1`
//!
//! Vytvoří distribuci `WSLCD-Lite-Test` (Alpine z ověřeného rootfs) v izolované datové složce
//! `target\lite-live-data`, nainstaluje Podman s TLS na portu 2387 (ne 2375/2376 – distribuce
//! sdílí síť), změří start a volitelně (`WSLCD_LITE_K8S=1`) spustí v ní Kubernetes laboratoř.
//! Na konci distribuci odregistruje (kromě `WSLCD_E2E_KEEP=1`).

use crate::config::Config;
use crate::docker::Docker;
use crate::engine::Engine;
use crate::worker::testkit;
use crate::worker::AddDistroSpec;
use crate::wsl;
use std::time::{Duration, Instant};

const NAME: &str = "WSLCD-Lite-Test";
const PORT: u16 = 2387;

/// Udržovací proces distribuce; při konci (i při panice) se ukončí a počká se na něj.
struct Keep(std::process::Child);

impl Drop for Keep {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Cleanup;

impl Drop for Cleanup {
    fn drop(&mut self) {
        if std::env::var("WSLCD_E2E_KEEP").is_ok() {
            eprintln!("WSLCD_E2E_KEEP: {NAME} ponechána");
            return;
        }
        let _ = std::process::Command::new("wsl.exe").args(["--unregister", NAME]).output();
        eprintln!("úklid: {NAME} odregistrována");
        if std::env::var("WSLCD_LITE_ENGINE").as_deref() == Ok("docker") {
            // Docker nechal ve sdílené síti WSL politiku FORWARD DROP – vrátit pro ostatní enginy
            let r = std::process::Command::new("wsl.exe").args(["-u", "root", "-e", "sh", "-c", "iptables -P FORWARD ACCEPT; iptables -S FORWARD | head -1"]).output();
            eprintln!("úklid: FORWARD {:?}", r.map(|o| String::from_utf8_lossy(&o.stdout).to_string()));
        }
    }
}

fn wait_api(d: &Docker, secs: u64) -> Option<Duration> {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(secs) {
        if d.ping() {
            return Some(t.elapsed());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

#[test]
#[ignore]
fn live_lite_distro() {
    let data = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("lite-live-data");
    let _ = std::process::Command::new("wsl.exe").args(["--unregister", NAME]).output();
    let _ = std::fs::remove_dir_all(data.join("distros"));
    std::fs::create_dir_all(&data).unwrap();
    std::env::set_var("WSLCD_DATA_DIR", &data);
    let _clean = Cleanup;
    // WSLCD_LITE_ENGINE=docker: Docker místo Podmanu (pozor: Docker nastaví ve sdílené síti WSL
    // iptables FORWARD DROP – test ji na konci vrátí na ACCEPT, viz spike)
    let engine = if std::env::var("WSLCD_LITE_ENGINE").as_deref() == Ok("docker") { Engine::Docker } else { Engine::Podman };
    let cfg = Config { port: PORT, keep_alive: false, ..Config::default() };
    let spec = AddDistroSpec { distro: crate::lite::LITE_ID.into(), name: NAME.into(), engine: Some(engine), use_for_app: true, tls: true, ..Default::default() };
    let t = Instant::now();
    let (r, log, cfg) = testkit::add_distro(cfg, spec);
    eprintln!("{log}\n--- přidání za {:?}: {r:?}", t.elapsed());
    r.unwrap();
    assert_eq!(cfg.distro, NAME);
    assert!(data.join("distros").join(NAME).join("ext4.vhdx").exists(), "distribuce v datové složce (portable)");
    crate::docker::configure_tls(true).unwrap();
    let d = Docker::new(&crate::http::Endpoint::Tcp { port: PORT });
    // drž distribuci naživu (jinak ji WSL po chvíli bez relace uspí)
    let keep = Keep(wsl::keepalive(NAME).unwrap());
    assert!(wait_api(&d, 30).is_some(), "API po instalaci");

    // studený start: terminate → první příkaz → API
    drop(keep);
    let _ = wsl::terminate(NAME);
    std::thread::sleep(Duration::from_secs(2));
    let t = Instant::now();
    let keep = Keep(wsl::keepalive(NAME).unwrap());
    let up = wait_api(&d, 60).expect("API po studeném startu");
    eprintln!("studený start distribuce → API přes TLS: {:?} (od spuštění {:?})", up, t.elapsed());
    let size = std::fs::metadata(data.join("distros").join(NAME).join("ext4.vhdx")).map(|m| m.len()).unwrap_or(0);
    let mem = wsl::run(NAME, Some("root"), None, &["sh", "-c", "ps -eo rss= | awk '{s+=$1} END {print s}'; systemctl list-units"]).unwrap_or_default();
    eprintln!("ext4.vhdx: {} MB, RSS procesů v distribuci (kB) + služby:\n{mem}", size / 1024 / 1024);

    // nástroje, které používají stránky aplikace (Soubory, velikosti volume, síť, sondy)
    let tools = wsl::run(NAME, Some("root"), None, &["sh", "-c", "cd / && find . -maxdepth 1 -name etc -printf '%y %f\n' && du -sb /etc | cut -f2 && ss -H -ltn | head -1 && command -v nsenter bash"]).unwrap();
    eprintln!("nástroje: {tools}");
    assert!(tools.contains("d etc") && tools.contains("/etc") && tools.contains("nsenter"), "{tools}");

    // kontejner přes API (jako aplikace)
    assert!(d.pull("docker.io/library/alpine:3.20").is_ok());
    let cli = if engine == Engine::Docker { "docker" } else { "podman" };
    let out = wsl::run(NAME, Some("root"), None, &[cli, "run", "--rm", "docker.io/library/alpine:3.20", "echo", "lite-ok"]).unwrap();
    assert!(out.contains("lite-ok"), "{out}");

    if std::env::var("WSLCD_LITE_K8S").is_ok() {
        let (r, msgs) = testkit::k8s(cfg.clone(), crate::worker::K8sOp::Create);
        eprintln!("k3s v WSLCD Lite: {r:?} {msgs:?}");
        r.unwrap();
        let (r, _) = testkit::k8s(cfg.clone(), crate::worker::K8sOp::Delete);
        r.unwrap();
    }
    drop(keep);
}
