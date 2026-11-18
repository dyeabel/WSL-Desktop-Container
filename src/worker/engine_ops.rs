//! Řízení enginu a distribuce: keepalive, start/stop, instalace, API, daemon.json.

use super::{Msg, Worker};
use crate::config::Config;
use crate::engine::Engine;
use crate::wsl;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

impl Worker {
    pub(super) fn ensure_keepalive(&self, distro: &str) -> Result<(), String> {
        let mut g = self.shared.keepalive.lock().map_err(|e| e.to_string())?;
        if let Some((c, started)) = g.as_mut() {
            if matches!(c.try_wait(), Ok(None)) {
                return Ok(());
            }
            // proces skončil (typicky zámek drží jiná instance) – nezkoušet znovu hned
            if started.elapsed() < Duration::from_secs(60) {
                return Ok(());
            }
        }
        *g = Some((wsl::keepalive(distro)?, Instant::now()));
        Ok(())
    }

    /// Ukončí udržovací proces – náš i případný z jiné instance aplikace.
    pub(super) fn kill_keepalive_all(&self, distro: &str) {
        self.kill_keepalive();
        let _ = wsl::run(distro, Some("root"), None, &["sh", "-c", "pkill -f 'flock -n /tmp/.wls-desktop-keepalive.lock' || true"]);
    }

    pub(super) fn kill_keepalive(&self) {
        if let Ok(mut g) = self.shared.keepalive.lock() {
            if let Some((mut c, _)) = g.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }

    /// Zkopíruje klientský certifikát, klíč a CA z distribuce do %APPDATA%\WLS_Desktop\tls.
    fn fetch_client_certs(&self, distro: &str) -> Result<(), String> {
        let dir = crate::docker::tls_dir().ok_or("APPDATA")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for f in ["ca.pem", "cert.pem", "key.pem"] {
            let src = format!("{}/{f}", crate::engine::TLS_DIR);
            let pem = wsl::run(distro, Some("root"), None, &["cat", &src]).map_err(|e| format!("{src}: {e}"))?;
            if !pem.contains("-----BEGIN") {
                return Err(tf!("{src} není platný PEM soubor", "{src} is not a valid PEM file"));
            }
            if f == "key.pem" {
                // klíč jen šifrovaně (DPAPI) – /mnt/c čte každý uživatel každé distribuce
                crate::docker::store_client_key(pem.as_bytes())?;
            } else {
                std::fs::write(dir.join(f), pem).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }

    /// Zapne / vypne zabezpečení API (TLS s klientským certifikátem, port 2376 / bez ověření, 2375).
    pub(super) fn set_tls(&self, on: bool) -> Result<String, String> {
        let mut cfg = self.cfg();
        let old = cfg.clone();
        cfg.tls = on;
        cfg.port = match (on, cfg.port) {
            (true, 2375) => 2376,
            (false, 2376) => 2375,
            (_, p) => p,
        };
        self.suppress_tracking();
        let r = self.setup_api(&cfg);
        match &r {
            Ok(_) => {
                if let Ok(mut g) = self.cfg.write() {
                    g.tls = cfg.tls;
                    g.port = cfg.port;
                    let _ = g.save();
                }
                self.shared.was_connected.store(false, Ordering::SeqCst);
                self.send(Msg::ConfigChanged);
                // DOCKER_HOST nastavený aplikací převést na nový režim
                if crate::shim_install::status().docker_host.is_some() {
                    let _ = crate::shim_install::set_docker_host(Some((cfg.port, cfg.tls)));
                }
            }
            Err(_) => {
                // vrátit původní režim (a jeho TLS klienta)
                let _ = crate::docker::configure_tls(old.tls);
            }
        }
        let (title, out) = match &r {
            Ok((o, _)) => (t!("Zabezpečení API", "API security"), o.clone()),
            Err(e) => (t!("Zabezpečení API", "API security"), e.clone()),
        };
        self.send(Msg::Output(title.into(), out));
        r.map(|_| {
            if on {
                tf!(
                    "API je zabezpečené: tcp://127.0.0.1:{} jen s klientským certifikátem",
                    "The API is secured: tcp://127.0.0.1:{} with a client certificate only",
                    cfg.port
                )
            } else {
                tf!("API běží bez ověření na tcp://127.0.0.1:{}", "The API runs without authentication on tcp://127.0.0.1:{}", cfg.port)
            }
        })
        .map_err(|_| t!("Nastavení API selhalo – viz výstup", "API setup failed – see output").into())
    }

    /// Spustí skript, který zpřístupní API enginu na TCP, a počká na odezvu.
    /// Vrací (výstup skriptu, API odpovídá).
    pub(super) fn setup_api(&self, cfg: &Config) -> Result<(String, bool), String> {
        self.setup_api_with(cfg, false)
    }

    /// Přegeneruje TLS certifikáty (nová CA, server i klient) a znovu nastaví API.
    pub(super) fn regen_certs(&self) -> Result<String, String> {
        let cfg = self.cfg();
        if !cfg.tls {
            return Err(t!("TLS není zapnuté", "TLS is not enabled").into());
        }
        self.suppress_tracking();
        let r = self.setup_api_with(&cfg, true);
        let out = match &r {
            Ok((o, _)) => o.clone(),
            Err(e) => e.clone(),
        };
        self.send(Msg::Output(t!("Zabezpečení API", "API security").into(), out));
        if r.is_ok() && crate::shim_install::status().docker_host.is_some() {
            // nástroje s DOCKER_CERT_PATH potřebují nový klíč
            let _ = crate::shim_install::set_docker_host(Some((cfg.port, true)));
        }
        r.map(|_| t!("Certifikáty přegenerovány", "Certificates regenerated").into())
    }

    fn setup_api_with(&self, cfg: &Config, regen: bool) -> Result<(String, bool), String> {
        let port = cfg.port.to_string();
        // výchozí uživatel distribuce – v režimu TLS dostane přístup k unix socketu enginu
        let user = wsl::run(&cfg.distro, None, None, &["id", "-un"]).map(|u| u.trim().to_string()).unwrap_or_default();
        let out = wsl::run(
            &cfg.distro,
            Some("root"),
            None,
            &[
                "env",
                &format!("PORT={port}"),
                &format!("TLS={}", if cfg.tls { 1 } else { 0 }),
                &format!("U={user}"),
                &format!("REGEN={}", if regen { 1 } else { 0 }),
                "sh",
                "-c",
                &cfg.engine.setup_script(),
            ],
        );
        self.check_engine();
        let mut out = out?;
        if cfg.tls {
            self.fetch_client_certs(&cfg.distro)?;
            out.push_str(&tf!(
                "\nKlientský certifikát uložen do {}",
                "\nClient certificate saved to {}",
                crate::docker::tls_dir().map(|d| d.display().to_string()).unwrap_or_default()
            ));
        }
        crate::docker::configure_tls(cfg.tls)?;
        if self.wait_ping_on(cfg.port, 25) {
            Ok((out, true))
        } else {
            Err(tf!(
                "{out}\nAPI zatím neodpovídá z Windows – zkontroluj networkingMode / localhostForwarding.",
                "{out}\nThe API is not responding from Windows yet – check networkingMode / localhostForwarding."
            ))
        }
    }

    /// Nainstaluje engine, zapne systemd (s restartem distribuce), nastaví API.
    pub(super) fn install_engine(&self, engine: Engine, log: &mut String) -> Result<String, String> {
        let mut cfg = self.cfg();
        let distro = cfg.distro.clone();
        let step = |log: &mut String, s: &str| {
            log.push_str(&format!("\n=== {s} ===\n"));
        };

        // 1) instalace balíčků
        step(log, &tf!("Instalace {}", "Installing {}", engine.name()));
        self.send(Msg::Toast(
            true,
            tf!("Instaluji {} do {distro}… (může trvat několik minut)", "Installing {} into {distro}… (may take several minutes)", engine.name()),
        ));
        match wsl::run(&distro, Some("root"), None, &["sh", "-c", &engine.install_script()]) {
            Ok(o) => log.push_str(&o),
            Err(e) => {
                log.push_str(&e);
                return Err(tf!("Instalace {} selhala – viz výstup", "Installation of {} failed – see output", engine.name()));
            }
        }

        // 2) systemd
        step(log, t!("Kontrola systemd", "Checking systemd"));
        if wsl::run(&distro, Some("root"), None, &["test", "-d", "/run/systemd/system"]).is_err() {
            log.push_str(t!(
                "systemd neběží – zapínám v /etc/wsl.conf a restartuji distribuci\n",
                "systemd is not running – enabling it in /etc/wsl.conf and restarting the distribution\n"
            ));
            let script = "grep -q '^systemd=true' /etc/wsl.conf 2>/dev/null || printf '\\n[boot]\\nsystemd=true\\n' >> /etc/wsl.conf";
            wsl::run(&distro, Some("root"), None, &["sh", "-c", script]).map_err(|e| {
                log.push_str(&e);
                t!("Nelze upravit /etc/wsl.conf", "Cannot modify /etc/wsl.conf").to_string()
            })?;
            self.kill_keepalive();
            let _ = wsl::terminate(&distro);
            thread::sleep(Duration::from_secs(3));
            if cfg.keep_alive {
                self.ensure_keepalive(&distro)?;
            } else {
                wsl::start(&distro)?;
            }
            thread::sleep(Duration::from_secs(3));
            if wsl::run(&distro, Some("root"), None, &["test", "-d", "/run/systemd/system"]).is_err() {
                log.push_str(t!(
                    "systemd stále neběží – zkus „Vypnout celé WSL“ a znovu.\n",
                    "systemd is still not running – try “Shut down all of WSL” and retry.\n"
                ));
                return Err(t!("systemd se nepodařilo zapnout", "Failed to enable systemd").into());
            }
            log.push_str(t!("systemd běží\n", "systemd is running\n"));
        } else {
            log.push_str(t!("systemd běží\n", "systemd is running\n"));
        }

        // 3) přepnout engine v konfiguraci a nastavit API
        cfg.engine = engine;
        if let Ok(mut g) = self.cfg.write() {
            g.engine = engine;
            let _ = g.save();
        }
        self.shared.tcp_checked.store(false, Ordering::SeqCst);
        step(log, t!("Nastavení TCP API", "Setting up the TCP API"));
        match self.setup_api(&cfg) {
            Ok((o, _)) => {
                log.push_str(&o);
                log.push_str(t!("\nAPI odpovídá – hotovo.\n", "\nThe API is responding – done.\n"));
                Ok(tf!("{} nainstalován a nastaven", "{} installed and configured", engine.name()))
            }
            Err(e) => {
                log.push_str(&e);
                Err(t!(
                    "Engine je nainstalovaný, ale API neodpovídá – viz výstup",
                    "The engine is installed, but the API is not responding – see output"
                )
                .into())
            }
        }
    }

    pub(super) fn engine_start(&self) -> Result<String, String> {
        let cfg = self.cfg();
        let distro = cfg.distro.clone();
        if cfg.keep_alive {
            self.ensure_keepalive(&distro)?;
        } else {
            wsl::start(&distro)?;
        }
        let name = cfg.engine.name();
        let mut a = vec!["systemctl", "start"];
        a.extend(cfg.engine.services());
        wsl::run(&distro, Some("root"), None, &a)?;
        if self.wait_ping(20) {
            Ok(tf!("{name} běží", "{name} is running"))
        } else {
            self.check_engine();
            Err(tf!(
                "{name} se nespustil nebo API na 127.0.0.1:{} neodpovídá. Použij „Nastavit jen API“ nebo „Nainstalovat a nastavit“ na stránce WSL engine.",
                "{name} did not start or the API on 127.0.0.1:{} is not responding. Use “Set up API only” or “Install and set up” on the WSL engine page.",
                cfg.port
            ))
        }
    }

    pub(super) fn engine_stop(&self) -> Result<String, String> {
        let cfg = self.cfg();
        let distro = cfg.distro.clone();
        self.suppress_tracking();
        let mut a = vec!["systemctl", "stop"];
        a.extend(cfg.engine.stop_units());
        wsl::run(&distro, Some("root"), None, &a)?;
        Ok(tf!("{} zastaven", "{} stopped", cfg.engine.name()))
    }

    pub(super) fn engine_restart(&self) -> Result<String, String> {
        let cfg = self.cfg();
        let distro = cfg.distro.clone();
        self.suppress_tracking();
        let mut a = vec!["systemctl", "restart"];
        a.extend(cfg.engine.services());
        wsl::run(&distro, Some("root"), None, &a)?;
        if self.wait_ping(20) {
            // další obnovení bere jako nové připojení (obnova kontejnerů + zapnutí sledování)
            self.shared.was_connected.store(false, Ordering::SeqCst);
            Ok(tf!("{} restartován", "{} restarted", cfg.engine.name()))
        } else {
            Err(tf!("{} po restartu neodpovídá", "{} is not responding after restart", cfg.engine.name()))
        }
    }

    pub(super) fn uninstall_engine(&self, engine: Engine, purge: bool) -> Result<String, String> {
        let distro = self.cfg().distro;
        self.send(Msg::Toast(true, tf!("Odinstalovávám {}…", "Uninstalling {}…", engine.name())));
        let r = wsl::run(
            &distro,
            Some("root"),
            None,
            &["env", &format!("PURGE={}", if purge { 1 } else { 0 }), "sh", "-c", &engine.uninstall_script()],
        );
        self.shared.tcp_checked.store(false, Ordering::SeqCst);
        self.check_engine();
        match r {
            Ok(o) => {
                self.send(Msg::Output(tf!("Odinstalace: {}", "Uninstall: {}", engine.name()), o));
                Ok(tf!(
                    "{} odinstalován{}",
                    "{} uninstalled{}",
                    engine.name(),
                    if purge { t!(" i s daty", " including data") } else { "" }
                ))
            }
            Err(e) => {
                self.send(Msg::Output(tf!("Odinstalace: {}", "Uninstall: {}", engine.name()), e));
                Err(t!("Odinstalace selhala – viz výstup", "Uninstall failed – see output").into())
            }
        }
    }

    pub(super) fn sleep_distro(&self) -> Result<String, String> {
        let distro = self.cfg().distro;
        self.suppress_tracking();
        self.kill_keepalive();
        wsl::terminate(&distro)?;
        Ok(tf!(
            "{distro} uspána (ukončena). Probudíš ji tlačítkem Spustit.",
            "{distro} put to sleep (terminated). Wake it up with the Start button."
        ))
    }

    pub(super) fn set_keep_alive(&self, on: bool) -> Result<String, String> {
        let distro = self.cfg().distro;
        if on {
            self.ensure_keepalive(&distro)?;
            Ok(t!("Udržování distribuce zapnuto", "Keep-alive for the distribution enabled").into())
        } else {
            self.kill_keepalive_all(&distro);
            Ok(t!(
                "Udržování distribuce vypnuto – WSL se může po nečinnosti zastavit",
                "Keep-alive for the distribution disabled – WSL may stop after inactivity"
            )
            .into())
        }
    }

    pub(super) fn load_daemon_json(&self) -> Result<String, String> {
        let distro = self.cfg().distro;
        let t = wsl::run(&distro, Some("root"), None, &["sh", "-c", "cat /etc/docker/daemon.json 2>/dev/null || echo '{}'"])?;
        self.send(Msg::DaemonJson(t));
        Ok(String::new())
    }

    pub(super) fn save_daemon_json(&self, text: String) -> Result<String, String> {
        let distro = self.cfg().distro;
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| tf!("Neplatný JSON: {e}", "Invalid JSON: {e}"))?;
        if v.get("hosts").is_some() {
            return Err(t!(
                "daemon.json nesmí obsahovat \"hosts\" – koliduje s -H v systemd. Port se nastavuje v Nastavení.",
                "daemon.json must not contain \"hosts\" – it conflicts with -H in systemd. The port is set in Settings."
            )
            .into());
        }
        let pretty = serde_json::to_string_pretty(&v).unwrap_or(text);
        wsl::run_stdin(&distro, Some("root"), &["sh", "-c", "mkdir -p /etc/docker && cat > /etc/docker/daemon.json"], &pretty)?;
        wsl::run(&distro, Some("root"), None, &["systemctl", "restart", "docker"])?;
        if self.wait_ping(20) {
            Ok(t!("daemon.json uložen, Docker restartován", "daemon.json saved, Docker restarted").into())
        } else {
            Err(t!(
                "Uloženo, ale Docker po restartu neodpovídá – zkontroluj daemon.json",
                "Saved, but Docker is not responding after restart – check daemon.json"
            )
            .into())
        }
    }

}
