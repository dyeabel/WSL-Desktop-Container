//! Pro živé testy: operace workeru stejnou cestou jako z UI, bez běžící aplikace.
//! Konfigurace se zapisuje do `WLS_DATA_DIR` (test ji musí nastavit, jinak by se přepsala
//! skutečná konfigurace).

use super::{Shared, Worker};
use crate::config::Config;
use crate::engine::Engine;
use std::sync::{Arc, RwLock};

fn worker(cfg: Config) -> Worker {
    assert!(crate::config::isolated(), "testkit potřebuje WLS_DATA_DIR");
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
