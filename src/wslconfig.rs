//! Jednoduchá úprava %USERPROFILE%\.wslconfig se zachováním ostatních řádků.

use std::path::PathBuf;

pub fn path() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".wslconfig"))
}

pub fn read() -> String {
    path().and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default()
}

pub fn get(text: &str, section: &str, key: &str) -> Option<String> {
    let mut cur = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            cur = t[1..t.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        if cur == section.to_ascii_lowercase() {
            if let Some((k, v)) = t.split_once('=') {
                if k.trim().eq_ignore_ascii_case(key) {
                    return Some(v.trim().to_string());
                }
            }
        }
    }
    None
}

/// Nastaví (Some) nebo odstraní (None) klíč v sekci.
pub fn set(text: &str, section: &str, key: &str, value: Option<&str>) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut found_section = false;
    let mut done = false;
    let sec = section.to_ascii_lowercase();
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            if cur == sec && !done {
                if let Some(v) = value {
                    out.push(format!("{key}={v}"));
                }
                done = true;
            }
            cur = t[1..t.len() - 1].trim().to_ascii_lowercase();
            if cur == sec {
                found_section = true;
            }
            out.push(line.to_string());
            continue;
        }
        if cur == sec && !done {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim().eq_ignore_ascii_case(key) {
                    if let Some(v) = value {
                        out.push(format!("{key}={v}"));
                    }
                    done = true;
                    continue;
                }
            }
        }
        out.push(line.to_string());
        let _ = i;
    }
    if !done {
        if let Some(v) = value {
            if !found_section {
                if !out.is_empty() && !out.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
                    out.push(String::new());
                }
                out.push(format!("[{section}]"));
            }
            out.push(format!("{key}={v}"));
        }
    }
    let mut s = out.join("\r\n");
    s.push_str("\r\n");
    s
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Draft {
    pub memory: String,
    pub processors: String,
    pub swap: String,
    pub idle_timeout: String,
    pub networking: String,
}

pub const KEYS: [&str; 5] = ["memory", "processors", "swap", "vmIdleTimeout", "networkingMode"];

impl Draft {
    pub fn load() -> Self {
        let t = read();
        let g = |k| get(&t, "wsl2", k).unwrap_or_default();
        Self {
            memory: g("memory"),
            processors: g("processors"),
            swap: g("swap"),
            idle_timeout: g("vmIdleTimeout"),
            networking: g("networkingMode"),
        }
    }

    pub fn save(&self) -> Result<(), String> {
        let p = path().ok_or("USERPROFILE není nastaveno")?;
        let mut t = read();
        let vals = [&self.memory, &self.processors, &self.swap, &self.idle_timeout, &self.networking];
        for (k, v) in KEYS.iter().zip(vals) {
            let v = v.trim();
            t = set(&t, "wsl2", k, if v.is_empty() { None } else { Some(v) });
        }
        std::fs::write(p, t).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_adds_section_and_key() {
        let t = set("", "wsl2", "memory", Some("8GB"));
        assert_eq!(get(&t, "wsl2", "memory").as_deref(), Some("8GB"));
    }

    #[test]
    fn set_replaces_and_preserves_other_sections() {
        let src = "[wsl2]\nmemory=4GB\nswap=0\n\n[experimental]\nautoMemoryReclaim=gradual\n";
        let t = set(src, "wsl2", "memory", Some("8GB"));
        assert_eq!(get(&t, "wsl2", "memory").as_deref(), Some("8GB"));
        assert_eq!(get(&t, "wsl2", "swap").as_deref(), Some("0"));
        assert_eq!(get(&t, "experimental", "autoMemoryReclaim").as_deref(), Some("gradual"));
    }

    #[test]
    fn set_none_removes_key() {
        let t = set("[wsl2]\nmemory=4GB\nswap=0\n", "wsl2", "memory", None);
        assert_eq!(get(&t, "wsl2", "memory"), None);
        assert_eq!(get(&t, "wsl2", "swap").as_deref(), Some("0"));
    }

    #[test]
    fn set_adds_key_into_existing_section_before_next() {
        let t = set("[wsl2]\nswap=0\n[x]\na=1\n", "wsl2", "memory", Some("2GB"));
        assert_eq!(get(&t, "wsl2", "memory").as_deref(), Some("2GB"));
        assert_eq!(get(&t, "x", "memory"), None);
    }
}
