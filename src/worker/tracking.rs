//! Pamatování naposledy běžících kontejnerů a jejich obnova po připojení k enginu.

use super::{Cmd, Msg, Worker};
use crate::docker::Snapshot;
use std::sync::atomic::Ordering;
use std::thread;

/// Uložený seznam kontejnerů, které naposledy běžely – pro každou distribuci a engine zvlášť.
pub(super) mod last_running {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn path() -> Option<PathBuf> {
        crate::config::data_dir().map(|d| d.join("last_running.json"))
    }

    fn load_all() -> BTreeMap<String, Vec<String>> {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn key(distro: &str, engine: &str) -> String {
        format!("{distro}/{engine}")
    }

    pub fn load(key: &str) -> Vec<String> {
        load_all().remove(key).unwrap_or_default()
    }

    pub fn save(key: &str, ids: &[String]) {
        let mut all = load_all();
        all.insert(key.to_string(), ids.to_vec());
        write_all(&all);
    }

    /// Odebere záznamy distribuce (klíče `"<distro>/<engine>"`). Vrací, zda se něco odebralo.
    pub fn drop_distro(all: &mut BTreeMap<String, Vec<String>>, distro: &str) -> bool {
        let before = all.len();
        all.retain(|k, _| k.split_once('/').map(|(d, _)| !d.eq_ignore_ascii_case(distro)).unwrap_or(true));
        all.len() != before
    }

    /// Zapomene uložené kontejnery odebrané distribuce.
    pub fn forget_distro(distro: &str) {
        let mut all = load_all();
        if drop_distro(&mut all, distro) {
            write_all(&all);
        }
    }

    fn write_all(all: &BTreeMap<String, Vec<String>>) {
        if let Some(p) = path() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(p, serde_json::to_string_pretty(&all).unwrap_or_default());
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn drop_distro_keys() {
            let mut m: super::BTreeMap<String, Vec<String>> = super::BTreeMap::new();
            m.insert("Debian/Podman".into(), vec!["a".into()]);
            m.insert("Debian/Docker".into(), vec![]);
            m.insert("Debian-dev/Podman".into(), vec![]);
            m.insert("Ubuntu/Docker".into(), vec![]);
            assert!(super::drop_distro(&mut m, "debian"));
            assert_eq!(m.keys().cloned().collect::<Vec<_>>(), ["Debian-dev/Podman", "Ubuntu/Docker"]);
            assert!(!super::drop_distro(&mut m, "Fedora"));
        }
    }
}

impl Worker {
    /// Pamatuje si běžící kontejnery; po novém připojení k enginu je případně znovu spustí.
    pub(super) fn track_running(&self, snap: &Snapshot) {
        let cfg = self.cfg();
        let key = last_running::key(&cfg.distro, cfg.engine.name());
        if !snap.connected {
            self.shared.was_connected.store(false, Ordering::SeqCst);
            return;
        }
        let first = !self.shared.was_connected.swap(true, Ordering::SeqCst);
        if first {
            self.invalidate_df();
            // nové připojení: nejdřív obnovit, ukládat až při dalším obnovení
            self.shared.suppress_track.store(false, Ordering::SeqCst);
            if let Ok(mut g) = self.shared.last_tracked.lock() {
                *g = last_running::load(&key);
            }
            if cfg.restore_containers {
                let saved = last_running::load(&key);
                let to_start: Vec<String> = saved
                    .into_iter()
                    .filter(|id| snap.containers.iter().any(|c| &c.id == id && !c.running() && !c.paused()))
                    .collect();
                if !to_start.is_empty() {
                    let w = self.clone();
                    thread::spawn(move || {
                        let d = w.docker();
                        let ok = to_start.iter().filter(|id| d.container_action(id, "start").is_ok()).count();
                        w.send(Msg::Toast(true, tf!("Obnoveno kontejnerů: {ok}/{}", "Containers restored: {ok}/{}", to_start.len())));
                        let _ = w.cmd.send(Cmd::Refresh);
                    });
                }
            }
            return;
        }
        if self.shared.suppress_track.load(Ordering::SeqCst) {
            return;
        }
        let mut ids: Vec<String> = snap.containers.iter().filter(|c| c.running()).map(|c| c.id.clone()).collect();
        ids.sort();
        if let Ok(mut g) = self.shared.last_tracked.lock() {
            if *g != ids {
                last_running::save(&key, &ids);
                *g = ids;
            }
        }
    }

    /// Aplikace sama vypíná engine – seznam běžících kontejnerů nepřepisovat.
    pub(super) fn suppress_tracking(&self) {
        self.shared.suppress_track.store(true, Ordering::SeqCst);
        self.quiet_notifications(std::time::Duration::from_secs(90));
    }
}
