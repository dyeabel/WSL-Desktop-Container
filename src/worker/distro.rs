//! Instalace a příprava nové WSL distribuce.

use super::{AddDistroSpec, Msg, Worker};
use crate::wsl;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

impl Worker {
    /// Nainstaluje novou WSL distribuci a připraví ji (systemd, uživatel, volitelně engine).
    pub(super) fn add_distro(&self, spec: AddDistroSpec, log: &mut String) -> Result<String, String> {
        let name = if spec.name.trim().is_empty() { spec.distro.clone() } else { spec.name.trim().to_string() };
        let step = |log: &mut String, s: &str| log.push_str(&format!("\n=== {s} ===\n"));
        if wsl::list().map(|l| l.iter().any(|d| d.name.eq_ignore_ascii_case(&name))).unwrap_or(false) {
            return Err(tf!("Distribuce {name} už existuje – zvol jiný název", "Distribution {name} already exists – choose a different name"));
        }

        // 1) instalace
        step(log, &tf!("Instalace {} jako {name}", "Installing {} as {name}", spec.distro));
        self.send(Msg::Toast(
            true,
            tf!("Instaluji distribuci {name}… (stahování může trvat několik minut)", "Installing distribution {name}… (the download may take several minutes)"),
        ));
        match wsl::install_distro(&spec.distro, &name) {
            Ok(o) => log.push_str(&o),
            Err(e) => {
                log.push_str(&e);
                return Err(tf!("Instalace distribuce {name} selhala – viz výstup", "Installation of distribution {name} failed – see output"));
            }
        }

        // 2) systemd, uživatel, sudo
        step(log, t!("Příprava (systemd, uživatel, sudo)", "Preparation (systemd, user, sudo)"));
        let user = spec.user.trim().to_string();
        match wsl::run(&name, Some("root"), None, &["env", &format!("U={user}"), "sh", "-c", &crate::engine::distro_prepare_script()]) {
            Ok(o) => log.push_str(&o),
            Err(e) => {
                log.push_str(&e);
                return Err(t!("Příprava distribuce selhala – viz výstup", "Distribution preparation failed – see output").into());
            }
        }
        if spec.password.contains(['\n', '\r']) {
            log.push_str(t!("Heslo obsahuje konec řádku – nenastaveno\n", "The password contains a line break – not set\n"));
        } else if !user.is_empty() && !spec.password.is_empty() {
            match wsl::run_stdin(&name, Some("root"), &["chpasswd"], &format!("{user}:{}\n", spec.password)) {
                Ok(_) => log.push_str(t!("Heslo nastaveno\n", "Password set\n")),
                Err(e) => log.push_str(&tf!("Heslo se nepodařilo nastavit: {e}\n", "Failed to set the password: {e}\n")),
            }
        }

        // 3) restart kvůli systemd a výchozímu uživateli
        step(log, t!("Restart distribuce", "Restarting the distribution"));
        let _ = wsl::terminate(&name);
        thread::sleep(Duration::from_secs(3));
        wsl::start(&name)?;
        thread::sleep(Duration::from_secs(3));
        let systemd = wsl::run(&name, Some("root"), None, &["test", "-d", "/run/systemd/system"]).is_ok();
        log.push_str(if systemd {
            t!("systemd běží\n", "systemd is running\n")
        } else {
            t!("systemd zatím neběží – může pomoci „Vypnout celé WSL“\n", "systemd is not running yet – “Shut down all of WSL” may help\n")
        });

        // 4) přepnout aplikaci a/nebo nainstalovat engine
        if spec.use_for_app {
            step(log, t!("Přepnutí aplikace na novou distribuci", "Switching the app to the new distribution"));
            self.kill_keepalive();
            if let Ok(mut g) = self.cfg.write() {
                g.distro = name.clone();
                if let Some(e) = spec.engine {
                    g.engine = e;
                }
                let _ = g.save();
            }
            self.shared.tcp_checked.store(false, Ordering::SeqCst);
            self.send(Msg::ConfigChanged);
            if let Some(e) = spec.engine {
                return self.install_engine(e, spec.tls, log).map(|_| {
                    tf!(
                        "Distribuce {name} je připravená s {} a aplikace ji používá",
                        "Distribution {name} is ready with {} and the app is using it",
                        e.name()
                    )
                });
            }
            return Ok(tf!(
                "Distribuce {name} je připravená a aplikace ji používá – nainstaluj engine na stránce WSL engine",
                "Distribution {name} is ready and the app is using it – install an engine on the WSL engine page"
            ));
        }
        if let Some(e) = spec.engine {
            step(log, &tf!("Instalace {} (bez nastavení API)", "Installing {} (without API setup)", e.name()));
            match wsl::run(&name, Some("root"), None, &["sh", "-c", &e.install_script()]) {
                Ok(o) => log.push_str(&o),
                Err(err) => {
                    log.push_str(&err);
                    return Err(tf!("Instalace {} selhala – viz výstup", "Installation of {} failed – see output", e.name()));
                }
            }
        }
        Ok(tf!("Distribuce {name} je nainstalovaná a připravená", "Distribution {name} is installed and ready"))
    }
}
