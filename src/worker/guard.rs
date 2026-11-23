//! Strop paměti pro kontejnery: použití v Nastavení a evidence distribucí bez stropu.

use super::*;

/// Distribuce, kde po přepnutí primární engine běží bez stropu: dosavadní a nově označené,
/// bez duplicit, bez uspané a bez nové primární.
pub(super) fn unguarded_list(prev: &[String], marked: Vec<String>, slept: Option<&str>, primary: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for d in prev.iter().cloned().chain(marked) {
        if d != primary && Some(d.as_str()) != slept && !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

impl Worker {
    /// Nainstaluje, změní nebo odstraní strop paměti podle nastavení.
    pub(super) fn apply_guard(&self) -> Result<String, String> {
        let cfg = self.cfg();
        let host = self.host();
        crate::lite::refresh_shim(host.as_ref());
        let [g, r] = crate::guard::env_args(&cfg, crate::host::current_caps().vm_memory);
        let out = host.run(Privilege::Root, None, &["env", &g, &r, "sh", "-c", &crate::guard::apply_script()])?;
        if out.contains("VAROVANI") {
            self.send(Msg::Output(t!("Strop paměti pro kontejnery", "Memory cap for containers").into(), out));
        }
        self.memory_sample(None);
        Ok(if cfg.container_guard {
            t!("Strop paměti pro kontejnery nastaven", "Memory cap for containers set").into()
        } else {
            t!("Strop paměti pro kontejnery vypnut", "Memory cap for containers turned off").into()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::unguarded_list;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn unguarded_after_switch() {
        assert_eq!(unguarded_list(&[], s(&["A", "B"]), Some("A"), "C"), ["B"]);
        assert_eq!(unguarded_list(&[], s(&["A", "C"]), None, "C"), ["A"]);
        assert_eq!(unguarded_list(&s(&["D", "C"]), s(&["A"]), None, "C"), ["D", "A"]);
        assert_eq!(unguarded_list(&s(&["A"]), s(&["A"]), None, "C"), ["A"]);
        assert!(unguarded_list(&s(&["A"]), vec![], None, "A").is_empty());
        assert!(unguarded_list(&[], vec![], None, "C").is_empty());
    }
}
