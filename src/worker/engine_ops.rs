//! Řízení enginu a distribuce: keepalive, start/stop, instalace, API, daemon.json.

use super::{Msg, Worker};
use crate::config::Config;
use crate::engine::Engine;
use crate::host::{Host, HostExt, Privilege};
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
    pub(super) fn kill_keepalive_all(&self, host: &dyn Host) {
        self.kill_keepalive();
        // udržovací proces existuje jen ve WSL
        if host.wsl().is_none() {
            return;
        }
        let _ = host.run(Privilege::Root, None, &["sh", "-c", "pkill -f 'flock -n /tmp/\\.w[a-z-]*-desktop-keepalive\\.lock' || true"]);
    }

    pub(super) fn kill_keepalive(&self) {
        if let Ok(mut g) = self.shared.keepalive.lock() {
            if let Some((mut c, _)) = g.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }

    /// Uvolní paměť WSL: zahodí cache Linuxu a zhutní paměť, aby ji VM mohl vrátit Windows
    /// (VmmemWSL). `quiet` = automatické uvolnění bez oznámení.
    /// Výsledek se zapíše do historie paměti a pošle UI (`Msg::MemoryFreed`).
    pub(super) fn free_memory(&self, quiet: bool) -> Result<String, String> {
        let r = self.free_memory_inner(quiet);
        self.memory_freed(quiet, r.clone());
        let e = r?;
        if quiet {
            return Ok(String::new());
        }
        let mb = |v: u64| v / 1024 / 1024;
        let (c0, c1) = (mb(e.cache_before), mb(e.cache_after));
        Ok(match (e.vmmem_before.map(mb), e.vmmem_after.map(mb)) {
            (Some(b), Some(a)) => tf!(
                "Paměť WSL uvolněna: VmmemWSL {b} MB → {a} MB (cache Linuxu {c0} MB → {c1} MB)",
                "WSL memory freed: VmmemWSL {b} MB → {a} MB (Linux cache {c0} MB → {c1} MB)"
            ),
            _ => tf!("Cache Linuxu uvolněna: {c0} MB → {c1} MB", "Linux cache freed: {c0} MB → {c1} MB"),
        })
    }

    fn free_memory_inner(&self, auto: bool) -> Result<crate::memory::FreeEvent, String> {
        let t = crate::memory::now_secs();
        let before = crate::memory::vmmem_now();
        let out = self.host().run(Privilege::Root, None, &["sh", "-c", FREE_MEMORY_SCRIPT])?;
        let cached = |tag: &str| -> u64 {
            out.lines().find_map(|l| l.strip_prefix(tag)).and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0) * 1024
        };
        // VM vrací stránky Windows postupně – chvíli počkat
        thread::sleep(Duration::from_secs(4));
        Ok(crate::memory::FreeEvent {
            t,
            auto,
            vmmem_before: before,
            vmmem_after: crate::memory::vmmem_now(),
            cache_before: cached("before:"),
            cache_after: cached("after:"),
        })
    }

    /// Automatické uvolnění (nastavení): nejvýš jednou za 10 minut, jen když je cache nad 1 GB.
    pub(super) fn auto_free_memory(&self) {
        // periodické: uvolnění paměti VM (root) jen tam, kde hostitel umí vm_memory
        if !self.cfg().auto_free_memory || !crate::host::current_caps().vm_memory {
            return;
        }
        let due = self
            .shared
            .last_free_memory
            .lock()
            .map(|mut g| {
                let due = g.is_none_or(|t| t.elapsed() >= Duration::from_secs(600));
                if due {
                    *g = Some(Instant::now());
                }
                due
            })
            .unwrap_or(false);
        if !due {
            return;
        }
        let w = self.clone();
        thread::spawn(move || {
            let cached = w.host().run(Privilege::User, None, &["sh", "-c", "awk '/^Cached:/{print $2}' /proc/meminfo"])
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(0);
            if cached > 1024 * 1024 {
                let _ = w.free_memory(true);
            }
        });
    }

    /// Zkopíruje klientský certifikát, klíč a CA z distribuce do %APPDATA%\WSL_Container_Desktop\tls.
    fn fetch_client_certs(&self, host: &dyn Host) -> Result<(), String> {
        let dir = crate::docker::tls_dir().ok_or("APPDATA")?;
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for f in ["ca.pem", "cert.pem", "key.pem"] {
            let src = format!("{}/{f}", crate::engine::TLS_DIR);
            let pem = host.run(Privilege::Root, None, &["cat", &src]).map_err(|e| format!("{src}: {e}"))?;
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
        cfg.port = api_port(on, cfg.port);
        self.suppress_tracking();
        let r = self.setup_api(&cfg);
        self.commit_api_mode(&cfg, old.tls, r.is_ok());
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

    /// Po nastavení API v novém režimu (TLS / bez ověření): při úspěchu uloží režim a port do
    /// konfigurace a převede DOCKER_HOST nastavený aplikací (tcp://…); při chybě vrátí TLS klienta
    /// původního režimu.
    fn commit_api_mode(&self, cfg: &Config, old_tls: bool, ok: bool) {
        if !ok {
            let _ = crate::docker::configure_tls(old_tls);
            return;
        }
        if let Ok(mut g) = self.cfg.write() {
            g.tls = cfg.tls;
            g.port = cfg.port;
            let _ = g.save();
        }
        self.shared.was_connected.store(false, Ordering::SeqCst);
        self.send(Msg::ConfigChanged);
        // DOCKER_HOST=npipe://… se nemění – roura vždy míří na aktuální API
        if docker_host_is_tcp() {
            let _ = crate::shim_install::set_docker_host(Some((cfg.port, cfg.tls)));
        }
    }

    /// Přepne primární distribuci: v ostatních distribucích uvolní TCP port API (engine
    /// a kontejnery tam běží dál), přepne konfiguraci a nastaví API v nové distribuci.
    pub(super) fn set_primary(&self, distro: &str, engine: Option<Engine>, sleep_old: bool, log: &mut String) -> Result<String, String> {
        let old = self.cfg();
        let step = |log: &mut String, s: &str| log.push_str(&format!("\n=== {s} ===\n"));

        // Kubernetes laboratoř předchozího enginu drží porty 6443 a NodePort (síť WSL je
        // sdílená) – zastavit, nesmazat; po návratu jde znovu spustit
        if distro != old.distro {
            use crate::k8s::cluster::{self, ClusterState};
            let d = self.docker();
            if matches!(cluster::state(&d), ClusterState::Running | ClusterState::Starting) {
                step(log, t!("Zastavení clusteru Kubernetes v předchozí distribuci", "Stopping the Kubernetes cluster in the previous distribution"));
                match cluster::stop(&d) {
                    Ok(()) => log.push_str(t!("cluster zastaven\n", "cluster stopped\n")),
                    Err(e) => log.push_str(&format!("{e}\n")),
                }
            }
        }

        step(log, &tf!("Enginy v distribuci {distro}", "Engines in {distro}"));
        let wh = self.wsl_host()?;
        let target = wh.for_distro(distro);
        let out = target
            .run(Privilege::Root, None, &["sh", "-c", crate::engine::DETECT_ENGINES])
            .map_err(|e| tf!("Distribuci {distro} nelze spustit: {e}", "Cannot start {distro}: {e}"))?;
        let installed = crate::engine::parse_engines(&out);
        log.push_str(&if installed.is_empty() {
            t!("žádný (Docker ani Podman)\n", "none (neither Docker nor Podman)\n").to_string()
        } else {
            installed.iter().map(|e| e.name()).collect::<Vec<_>>().join(", ") + "\n"
        });
        let engine = pick_engine(engine, old.distro_engines.get(distro).copied(), &installed)?;

        self.suppress_tracking();
        step(log, t!("Uvolnění API v ostatních distribucích", "Releasing the API in other distributions"));
        let mut others: Vec<String> = wsl::list().unwrap_or_default().into_iter().filter(|d| d.running() && d.name != distro).map(|d| d.name).collect();
        // dosavadní primární i zastavenou – jinak by si port vzala při příštím startu
        if old.distro != distro && !others.contains(&old.distro) {
            others.push(old.distro.clone());
        }
        for o in &others {
            let script = format!("{}\n{}", crate::engine::LEGACY_CLEANUP, crate::engine::API_RELEASE);
            match wh.for_distro(o).run(Privilege::Root, None, &["sh", "-c", &script]) {
                Ok(r) if r.trim().is_empty() => log.push_str(&tf!("{o}: API tu nebylo\n", "{o}: no API here\n")),
                Ok(r) => log.push_str(&format!("{o}: {}\n", r.trim().replace('\n', ", "))),
                Err(e) => log.push_str(&format!("{o}: {e}\n")),
            }
        }

        self.kill_keepalive();
        let mut slept = false;
        if sleep_old && old.distro != distro {
            step(log, &tf!("Uspání {}", "Putting {} to sleep", old.distro));
            // i udržovací proces jiné instance aplikace, jinak by distribuci hned znovu probudil
            self.kill_keepalive_all(&wh.for_distro(&old.distro));
            match wsl::terminate(&old.distro) {
                Ok(_) => {
                    slept = true;
                    log.push_str(&tf!("{} ukončena (wsl --terminate)\n", "{} terminated (wsl --terminate)\n", old.distro));
                }
                Err(e) => log.push_str(&format!("{}: {e}\n", old.distro)),
            }
        }
        let slept_note = if slept { tf!(", {} uspána", ", {} is asleep", old.distro) } else { String::new() };
        let mut cfg = old.clone();
        cfg.distro_engines.insert(old.distro.clone(), old.engine);
        cfg.distro = distro.to_string();
        if let Some(e) = engine {
            cfg.engine = e;
            cfg.distro_engines.insert(distro.to_string(), e);
        }
        if let Ok(mut g) = self.cfg.write() {
            g.distro = cfg.distro.clone();
            g.engine = cfg.engine;
            g.distro_engines = cfg.distro_engines.clone();
            g.keep_previous_primary = !sleep_old;
            let _ = g.save();
        }
        self.shared.tcp_checked.store(false, Ordering::SeqCst);
        self.shared.was_connected.store(false, Ordering::SeqCst);
        self.send(Msg::ConfigChanged);
        if cfg.keep_alive {
            let _ = self.ensure_keepalive(distro);
        }

        let Some(e) = engine else {
            return Ok(tf!(
                "{distro} je primární distribuce{slept_note} – nainstaluj engine na stránce WSL engine",
                "{distro} is the primary distribution{slept_note} – install an engine on the WSL engine page"
            ));
        };
        step(log, &tf!("Nastavení API ({})", "Setting up the API ({})", e.name()));
        let r = self.setup_api(&cfg);
        self.commit_api_mode(&cfg, old.tls, r.is_ok());
        match r {
            Ok((o, _)) => {
                log.push_str(&o);
                Ok(tf!(
                    "{distro} je primární distribuce ({}) – API na tcp://127.0.0.1:{}{}",
                    "{distro} is the primary distribution ({}) – API on tcp://127.0.0.1:{}{}",
                    e.name(),
                    cfg.port,
                    slept_note
                ))
            }
            Err(err) => {
                log.push_str(&err);
                Err(tf!("{distro}: API se nepodařilo nastavit – viz výstup", "{distro}: API setup failed – see output"))
            }
        }
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
        if r.is_ok() && docker_host_is_tcp() {
            // nástroje s DOCKER_CERT_PATH potřebují nový klíč
            let _ = crate::shim_install::set_docker_host(Some((cfg.port, true)));
        }
        r.map(|_| t!("Certifikáty přegenerovány", "Certificates regenerated").into())
    }

    fn setup_api_with(&self, cfg: &Config, regen: bool) -> Result<(String, bool), String> {
        let port = cfg.port.to_string();
        let host = crate::host::current(cfg);
        crate::lite::refresh_shim(host.as_ref());
        // výchozí uživatel distribuce – v režimu TLS dostane přístup k unix socketu enginu
        let user = host.run(Privilege::User, None, &["id", "-un"]).map(|u| u.trim().to_string()).unwrap_or_default();
        let out = host.run(
            Privilege::Root,
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
            self.fetch_client_certs(host.as_ref())?;
            out.push_str(&tf!(
                "\nKlientský certifikát uložen do {}",
                "\nClient certificate saved to {}",
                crate::docker::tls_dir().map(|d| d.display().to_string()).unwrap_or_default()
            ));
        }
        crate::docker::configure_tls(cfg.tls)?;
        if self.wait_ping_on(&crate::host::endpoint(cfg), 25) {
            Ok((out, true))
        } else {
            Err(tf!(
                "{out}\nAPI zatím neodpovídá z Windows – zkontroluj networkingMode / localhostForwarding.",
                "{out}\nThe API is not responding from Windows yet – check networkingMode / localhostForwarding."
            ))
        }
    }

    /// Nainstaluje engine, zapne systemd (s restartem distribuce), nastaví API
    /// (`tls` = zabezpečit API pomocí TLS s klientským certifikátem, jinak bez ověření).
    pub(super) fn install_engine(&self, engine: Engine, tls: bool, log: &mut String) -> Result<String, String> {
        let mut cfg = self.cfg();
        let old_tls = cfg.tls;
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
        let host = self.host();
        match host.run(Privilege::Root, None, &["sh", "-c", &engine.install_script()]) {
            Ok(o) => log.push_str(&o),
            Err(e) => {
                log.push_str(&e);
                return Err(tf!("Instalace {} selhala – viz výstup", "Installation of {} failed – see output", engine.name()));
            }
        }

        // 2) systemd
        step(log, t!("Kontrola systemd", "Checking systemd"));
        if host.run(Privilege::Root, None, &["sh", "-c", crate::lite::HAS_INIT]).is_err() {
            log.push_str(t!(
                "systemd neběží – zapínám v /etc/wsl.conf a restartuji distribuci\n",
                "systemd is not running – enabling it in /etc/wsl.conf and restarting the distribution\n"
            ));
            let script = "grep -q '^systemd=true' /etc/wsl.conf 2>/dev/null || printf '\\n[boot]\\nsystemd=true\\n' >> /etc/wsl.conf";
            host.run(Privilege::Root, None, &["sh", "-c", script]).map_err(|e| {
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
            if host.run(Privilege::Root, None, &["sh", "-c", crate::lite::HAS_INIT]).is_err() {
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
        cfg.tls = tls;
        cfg.port = api_port(tls, cfg.port);
        step(
            log,
            if tls { t!("Nastavení API (TLS s klientským certifikátem)", "Setting up the API (TLS with a client certificate)") } else { t!("Nastavení TCP API", "Setting up the TCP API") },
        );
        let r = self.setup_api(&cfg);
        self.commit_api_mode(&cfg, old_tls, r.is_ok());
        match r {
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
        if self.host().wsl().is_none() {
            return self.local_engine(SvcOp::Start);
        }
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
        self.host().run(Privilege::Root, None, &a)?;
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
        self.suppress_tracking();
        if self.host().wsl().is_none() {
            return self.local_engine(SvcOp::Stop);
        }
        let mut a = vec!["systemctl", "stop"];
        a.extend(cfg.engine.stop_units());
        self.host().run(Privilege::Root, None, &a)?;
        Ok(tf!("{} zastaven", "{} stopped", cfg.engine.name()))
    }

    pub(super) fn engine_restart(&self) -> Result<String, String> {
        let cfg = self.cfg();
        self.suppress_tracking();
        if self.host().wsl().is_none() {
            return self.local_engine(SvcOp::Restart);
        }
        let mut a = vec!["systemctl", "restart"];
        a.extend(cfg.engine.services());
        self.host().run(Privilege::Root, None, &a)?;
        if self.wait_ping(20) {
            // další obnovení bere jako nové připojení (obnova kontejnerů + zapnutí sledování)
            self.shared.was_connected.store(false, Ordering::SeqCst);
            Ok(tf!("{} restartován", "{} restarted", cfg.engine.name()))
        } else {
            Err(tf!("{} po restartu neodpovídá", "{} is not responding after restart", cfg.engine.name()))
        }
    }

    /// Linux: start / stop / restart služby enginu přes `systemctl` – rootless Podman jako
    /// uživatelská služba, jinak systémová přes `pkexec` (jen na kliknutí, `interactive`).
    /// Nic se neinstaluje ani nekonfiguruje.
    fn local_engine(&self, op: SvcOp) -> Result<String, String> {
        let cfg = self.cfg();
        let name = cfg.engine.name();
        let rt = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()).map(std::path::PathBuf::from);
        let (args, root) = local_engine_command(op, crate::host::found_endpoint().as_ref(), cfg.engine, rt.as_deref());
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let p = if root { Privilege::Root } else { Privilege::User };
        let host = self.host();
        host.exec(&crate::host::ExecReq::new(&a).privilege(p).interactive()).and_then(crate::host::finish)?;
        if op == SvcOp::Stop {
            return Ok(tf!("{name} zastaven", "{name} stopped"));
        }
        // socket mohl vzniknout až teď – endpoint hledat při každém pokusu znovu
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(20) {
            crate::host::refresh_discovery();
            if crate::docker::Docker::new(&crate::host::endpoint(&cfg)).ping() {
                self.shared.was_connected.store(false, Ordering::SeqCst);
                return Ok(match op {
                    SvcOp::Restart => tf!("{name} restartován", "{name} restarted"),
                    _ => tf!("{name} běží", "{name} is running"),
                });
            }
            thread::sleep(Duration::from_millis(500));
        }
        let note = crate::host::engine_note().map(|n| format!(" {n}")).unwrap_or_default();
        Err(tf!("{name} po spuštění neodpovídá.{note}", "{name} is not responding after the start.{note}"))
    }

    pub(super) fn uninstall_engine(&self, engine: Engine, purge: bool) -> Result<String, String> {
        self.send(Msg::Toast(true, tf!("Odinstalovávám {}…", "Uninstalling {}…", engine.name())));
        let r = self.host().run(
            Privilege::Root,
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
                self.send(Msg::Output(tf!("Odinstalace: {}", "Uninstall: {}", engine.name()), e.into()));
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
            self.kill_keepalive_all(self.host().as_ref());
            Ok(t!(
                "Udržování distribuce vypnuto – WSL se může po nečinnosti zastavit",
                "Keep-alive for the distribution disabled – WSL may stop after inactivity"
            )
            .into())
        }
    }

    pub(super) fn load_daemon_json(&self) -> Result<String, String> {
        let t = self.host().run(Privilege::Root, None, &["sh", "-c", "cat /etc/docker/daemon.json 2>/dev/null || echo '{}'"])?;
        self.send(Msg::DaemonJson(t));
        Ok(String::new())
    }

    pub(super) fn save_daemon_json(&self, text: String) -> Result<String, String> {
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| tf!("Neplatný JSON: {e}", "Invalid JSON: {e}"))?;
        if v.get("hosts").is_some() {
            return Err(t!(
                "daemon.json nesmí obsahovat \"hosts\" – koliduje s -H v systemd. Port se nastavuje v Nastavení.",
                "daemon.json must not contain \"hosts\" – it conflicts with -H in systemd. The port is set in Settings."
            )
            .into());
        }
        let pretty = serde_json::to_string_pretty(&v).unwrap_or(text);
        let host = self.host();
        host.run_stdin(Privilege::Root, &["sh", "-c", "mkdir -p /etc/docker && cat > /etc/docker/daemon.json"], &pretty)?;
        host.run(Privilege::Root, None, &["systemctl", "restart", "docker"])?;
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

/// Zahodí cache Linuxu a zhutní paměť; vypíše Cached (kB) před a po.
const FREE_MEMORY_SCRIPT: &str = r#"echo "before: $(awk '/^Cached:/{print $2}' /proc/meminfo)"
sync
echo 3 > /proc/sys/vm/drop_caches
echo 1 > /proc/sys/vm/compact_memory 2>/dev/null || true
echo "after: $(awk '/^Cached:/{print $2}' /proc/meminfo)""#;

/// Operace se službou místního enginu (Linux).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SvcOp {
    Start,
    Stop,
    Restart,
}

/// Příkaz `systemctl` pro službu místního enginu; druhá složka = spustit jako root.
/// - socket pod `runtime_dir` (`$XDG_RUNTIME_DIR`) = rootless: `systemctl --user`, bez roota;
/// - socket s „podman“ v cestě = služba `podman.socket` / `podman.service`, jinak `docker`;
/// - bez nalezeného socketu (nebo TCP) rozhoduje typ enginu z konfigurace: Podman se bere
///   jako rootless (bez hesla), Docker jako systémová služba.
pub(crate) fn local_engine_command(op: SvcOp, ep: Option<&crate::http::Endpoint>, engine: Engine, runtime_dir: Option<&std::path::Path>) -> (Vec<String>, bool) {
    let (podman, rootless) = match ep {
        Some(crate::http::Endpoint::Unix(p)) => (
            p.to_string_lossy().contains("podman"),
            runtime_dir.is_some_and(|d| d.has_root() && p.starts_with(d)),
        ),
        _ => (engine == Engine::Podman, engine == Engine::Podman),
    };
    let units: &[&str] = match (podman, op) {
        (true, SvcOp::Start) => &["podman.socket"],
        (true, SvcOp::Stop) => &["podman.socket", "podman.service"],
        (true, SvcOp::Restart) => &["podman.service"],
        // systémový Docker se socket aktivací: bez docker.socket by ho další dotaz hned probudil
        (false, SvcOp::Stop) if !rootless => &["docker.socket", "docker"],
        (false, _) => &["docker"],
    };
    let verb = match op {
        SvcOp::Start => "start",
        SvcOp::Stop => "stop",
        SvcOp::Restart => "restart",
    };
    let mut v: Vec<String> = vec!["systemctl".into()];
    if rootless {
        v.push("--user".into());
    }
    v.push(verb.into());
    v.extend(units.iter().map(|u| u.to_string()));
    (v, !rootless)
}

/// Port API pro režim: výchozí 2375 bez ověření, 2376 s TLS; vlastní port zůstává.
/// Engine pro novou primární distribuci: vyžádaný (musí být nainstalovaný), jinak naposledy
/// použitý, jinak jediný (první) nainstalovaný. Ok(None) = žádný engine v distribuci není.
pub fn pick_engine(requested: Option<Engine>, remembered: Option<Engine>, installed: &[Engine]) -> Result<Option<Engine>, String> {
    if let Some(e) = requested {
        return if installed.contains(&e) {
            Ok(Some(e))
        } else {
            Err(tf!("{} v distribuci není nainstalovaný", "{} is not installed in the distribution", e.name()))
        };
    }
    Ok(remembered.filter(|e| installed.contains(e)).or_else(|| installed.first().copied()))
}

pub fn api_port(tls: bool, port: u16) -> u16 {
    match (tls, port) {
        (true, 2375) => 2376,
        (false, 2376) => 2375,
        (_, p) => p,
    }
}

/// Uživatelská proměnná DOCKER_HOST míří na TCP API (nastavila ji aplikace) – při změně
/// režimu nebo certifikátů se má převést. npipe://… se nemění.
fn docker_host_is_tcp() -> bool {
    if crate::config::isolated() {
        return false;
    }
    crate::shim_install::status().docker_host.is_some_and(|h| h.starts_with("tcp://"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn engine_start_stop_commands() {
        use super::{local_engine_command as cmd, SvcOp::*};
        use crate::engine::Engine::{Docker, Podman};
        use crate::http::Endpoint;
        use std::path::{Path, PathBuf};
        let rt = Some(Path::new("/run/user/1000"));
        let s = |(v, root): (Vec<String>, bool)| (v.join(" "), root);
        // rootless Podman: uživatelská služba, bez roota (i když je v konfiguraci Docker)
        let rootless = Endpoint::Unix(PathBuf::from("/run/user/1000/podman/podman.sock"));
        assert_eq!(s(cmd(Start, Some(&rootless), Docker, rt)), ("systemctl --user start podman.socket".into(), false));
        assert_eq!(s(cmd(Stop, Some(&rootless), Docker, rt)), ("systemctl --user stop podman.socket podman.service".into(), false));
        assert_eq!(s(cmd(Restart, Some(&rootless), Podman, rt)), ("systemctl --user restart podman.service".into(), false));
        // systémový Docker: přes pkexec
        let docker = Endpoint::Unix(PathBuf::from("/var/run/docker.sock"));
        assert_eq!(s(cmd(Start, Some(&docker), Podman, rt)), ("systemctl start docker".into(), true));
        assert_eq!(s(cmd(Stop, Some(&docker), Docker, rt)), ("systemctl stop docker.socket docker".into(), true));
        assert_eq!(s(cmd(Restart, Some(&docker), Docker, None)), ("systemctl restart docker".into(), true));
        // systémový Podman
        let rootful = Endpoint::Unix(PathBuf::from("/run/podman/podman.sock"));
        assert_eq!(s(cmd(Start, Some(&rootful), Podman, rt)), ("systemctl start podman.socket".into(), true));
        // rootless Docker
        let rl_docker = Endpoint::Unix(PathBuf::from("/run/user/1000/docker.sock"));
        assert_eq!(s(cmd(Stop, Some(&rl_docker), Docker, rt)), ("systemctl --user stop docker".into(), false));
        // nic nenalezeno / TCP: podle typu enginu
        assert_eq!(s(cmd(Start, None, Podman, rt)), ("systemctl --user start podman.socket".into(), false));
        assert_eq!(s(cmd(Start, None, Docker, rt)), ("systemctl start docker".into(), true));
        assert_eq!(s(cmd(Start, Some(&Endpoint::Tcp { port: 2375 }), Docker, None)), ("systemctl start docker".into(), true));
        // bez XDG_RUNTIME_DIR (nebo relativní) není nic rootless
        assert_eq!(s(cmd(Start, Some(&rootless), Docker, None)), ("systemctl start podman.socket".into(), true));
        assert_eq!(s(cmd(Start, Some(&rootless), Docker, Some(Path::new("")))), ("systemctl start podman.socket".into(), true));
    }

    #[test]
    fn pick_engine_order() {
        use crate::engine::Engine::{Docker, Podman};
        use super::pick_engine;
        assert_eq!(pick_engine(None, None, &[]), Ok(None));
        assert_eq!(pick_engine(None, None, &[Podman]), Ok(Some(Podman)));
        assert_eq!(pick_engine(None, Some(Podman), &[Docker, Podman]), Ok(Some(Podman)));
        assert_eq!(pick_engine(None, Some(Podman), &[Docker]), Ok(Some(Docker)));
        assert_eq!(pick_engine(Some(Docker), Some(Podman), &[Docker, Podman]), Ok(Some(Docker)));
        assert!(pick_engine(Some(Podman), None, &[Docker]).is_err());
    }

    #[test]
    fn api_port_follows_mode() {
        assert_eq!(super::api_port(true, 2375), 2376);
        assert_eq!(super::api_port(false, 2376), 2375);
        assert_eq!(super::api_port(true, 2376), 2376);
        assert_eq!(super::api_port(false, 2375), 2375);
        assert_eq!(super::api_port(true, 3000), 3000);
    }
}
