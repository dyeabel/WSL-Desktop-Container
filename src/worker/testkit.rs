//! Pro živé testy: operace workeru stejnou cestou jako z UI, bez běžící aplikace.
//! Konfigurace se zapisuje do `WSLCD_DATA_DIR` (test ji musí nastavit, jinak by se přepsala
//! skutečná konfigurace).

use super::{Shared, Worker};
use crate::config::Config;
use crate::engine::Engine;
use std::sync::{Arc, RwLock};

fn worker(cfg: Config) -> Worker {
    assert!(crate::config::isolated(), "testkit potřebuje WSLCD_DATA_DIR");
    let (msg, _m) = std::sync::mpsc::channel();
    let (cmd, _c) = std::sync::mpsc::channel();
    Worker { ctx: eframe::egui::Context::default(), cfg: Arc::new(RwLock::new(cfg)), msg, cmd, shared: Arc::new(Shared::default()) }
}

/// Instalace enginu + nastavení API (jako „Nainstalovat a nastavit“). Vrací výsledek a log.
pub fn install_engine(cfg: Config, engine: Engine, tls: bool) -> (Result<String, String>, String) {
    let w = worker(cfg);
    let mut log = String::new();
    let r = w.install_engine(engine, tls, &mut log);
    w.kill_keepalive();
    (r, log)
}

/// Zapnutí / vypnutí TLS (jako tlačítko na stránce WSL engine).
pub fn set_tls(cfg: Config, on: bool) -> Result<String, String> {
    let w = worker(cfg);
    let r = w.set_tls(on);
    w.kill_keepalive();
    r
}

/// Přegenerování certifikátů.
pub fn regen_certs(cfg: Config) -> Result<String, String> {
    let w = worker(cfg);
    let r = w.regen_certs();
    w.kill_keepalive();
    r
}

/// Uvolnění paměti WSL (jako „Uvolnit paměť WSL“).
pub fn free_memory(cfg: Config) -> Result<String, String> {
    worker(cfg).free_memory(false)
}

/// Jeden vzorek do historie zdrojů stejnou cestou jako aplikace: paměť a provoz eth0
/// (`memory_sample`), statistiky kontejnerů (CPU z rozdílu jako při obnovení, úložiště enginu)
/// a volitelně kontrola disku. `prev` = předchozí kumulované CPU kontejnerů.
pub fn history_sample(cfg: Config, prev: &mut std::collections::HashMap<String, (std::time::Instant, u64)>, disk: bool) {
    let w = worker(cfg.clone());
    let d = w.docker();
    let snap = d.snapshot();
    let ncpu = snap.info.as_ref().map(|i| i.ncpu.max(1)).unwrap_or(1) as f64;
    let mut rows: Vec<crate::docker::StatRow> = snap.containers.iter().filter(|c| c.running()).filter_map(|c| d.stats(&c.id, &c.name())).collect();
    let now = std::time::Instant::now();
    for r in &mut rows {
        if let Some((t, total)) = prev.get(&r.id) {
            let dt = now.duration_since(*t).as_nanos() as f64;
            if dt > 0.0 && r.cpu_total >= *total {
                r.cpu = (r.cpu_total - total) as f64 / dt * 100.0;
            }
        }
    }
    prev.clear();
    prev.extend(rows.iter().map(|r| (r.id.clone(), (now, r.cpu_total))));
    let df = d.df();
    w.history_stats(&rows, df.as_ref(), ncpu);
    w.memory_sample(Some(&rows));
    if disk {
        w.history_disk(&super::alerts::collect_disk(&cfg, Some(&d)));
    }
}

/// Přepnutí primární distribuce (jako „Nastavit jako primární“). Vrací výsledek, log a novou konfiguraci.
pub fn set_primary(cfg: Config, distro: &str, engine: Option<Engine>, sleep_old: bool) -> (Result<String, String>, String, Config) {
    let w = worker(cfg);
    let mut log = String::new();
    let r = w.set_primary(distro, engine, sleep_old, &mut log);
    w.kill_keepalive();
    let c = w.cfg();
    (r, log, c)
}

/// Operace Kubernetes laboratoře stejnou cestou jako z UI; vrací výsledek a zprávy pro UI.
pub fn k8s(cfg: Config, op: super::K8sOp) -> (Result<String, String>, Vec<super::K8sMsg>) {
    assert!(crate::config::isolated(), "testkit potřebuje WSLCD_DATA_DIR");
    let (msg, rx) = std::sync::mpsc::channel();
    let (cmd, _c) = std::sync::mpsc::channel();
    let w = Worker { ctx: eframe::egui::Context::default(), cfg: Arc::new(RwLock::new(cfg)), msg, cmd, shared: Arc::new(Shared::default()) };
    let r = w.k8s_op(op);
    let msgs = rx.try_iter().filter_map(|m| if let super::Msg::K8s(k) = m { Some(k) } else { None }).collect();
    (r, msgs)
}

/// Přidání distribuce (jako dialog „Přidat distribuci“); vrací výsledek, log a konfiguraci po něm.
pub fn add_distro(cfg: Config, spec: super::AddDistroSpec) -> (Result<String, String>, String, Config) {
    let w = worker(cfg);
    let mut log = String::new();
    let r = w.add_distro(spec, &mut log);
    w.kill_keepalive();
    let cfg = w.cfg();
    (r, log, cfg)
}
