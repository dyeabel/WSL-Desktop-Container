//! Předávání proměnných prostředí z Windows do WSL (přes WSLENV).
//!
//! Předávají se jen proměnné, které příkaz opravdu potřebuje:
//! - `-e VAR` / `--env VAR` / `--env=VAR` bez hodnoty (Docker ji bere z prostředí),
//! - proměnné použité v compose souborech (`${VAR}`, `${VAR:-x}`, `$VAR`),
//! - ručně vyjmenované v `WSLCD_FORWARD_ENV` (oddělené čárkou nebo středníkem).
//!
//! Proměnné, které by uvnitř WSL rozbily CLI (DOCKER_HOST, PATH…), se nepředávají.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Nikdy nepředávat – měly by ve WSL jiný (špatný) význam.
const BLOCKED: &[&str] = &[
    "PATH", "HOME", "USER", "SHELL", "PWD", "TMP", "TEMP", "WSLENV", "LANG", "TERM",
    "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG", "DOCKER_CERT_PATH", "DOCKER_TLS_VERIFY",
    "CONTAINER_HOST", "CONTAINER_CONNECTION",
];

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.chars().next().map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false)
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Názvy z `-e VAR` (bez `=hodnota`).
pub fn env_flag_names(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut prev = "";
    for a in args {
        let candidate = if matches!(prev, "-e" | "--env") {
            Some(a.as_str())
        } else {
            a.strip_prefix("--env=")
        };
        if let Some(c) = candidate {
            if !c.contains('=') && valid_name(c) {
                out.push(c.to_string());
            }
        }
        prev = a.as_str();
    }
    out
}

/// Názvy proměnných použitých v textu compose souboru.
pub fn compose_var_names(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$' {
            // $$ = escapovaný dolar
            if i + 1 < b.len() && b[i + 1] == b'$' {
                i += 2;
                continue;
            }
            let start = if i + 1 < b.len() && b[i + 1] == b'{' { i + 2 } else { i + 1 };
            let mut j = start;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            if j > start {
                let name = &text[start..j];
                if valid_name(name) {
                    out.insert(name.to_string());
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    out.into_iter().collect()
}

/// Compose soubory, které příkaz použije: `-f` / `--file`, jinak výchozí názvy v cwd.
fn compose_files(args: &[String], cwd: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut prev = "";
    for a in args {
        if matches!(prev, "-f" | "--file") {
            files.push(cwd.join(a));
        } else if let Some(v) = a.strip_prefix("--file=") {
            files.push(cwd.join(v));
        }
        prev = a.as_str();
    }
    if files.is_empty() {
        for n in ["compose.yaml", "compose.yml", "docker-compose.yml", "docker-compose.yaml", "docker-compose.override.yml", "compose.override.yaml"] {
            let p = cwd.join(n);
            if p.exists() {
                files.push(p);
            }
        }
    }
    files
}

/// Sestaví hodnotu WSLENV: stávající + proměnné, které je potřeba předat a ve Windows existují.
pub fn build_wslenv(args: &[String], is_compose: bool, cwd: &Path, existing: &str, lookup: impl Fn(&str) -> bool) -> (String, Vec<String>) {
    let mut names: BTreeSet<String> = env_flag_names(args).into_iter().collect();
    if is_compose {
        for f in compose_files(args, cwd) {
            if let Ok(t) = std::fs::read_to_string(&f) {
                names.extend(compose_var_names(&t));
            }
        }
    }
    if let Ok(extra) = std::env::var("WSLCD_FORWARD_ENV") {
        names.extend(extra.split([',', ';', ' ']).map(|s| s.trim().to_string()).filter(|s| valid_name(s)));
    }
    let already: BTreeSet<String> = existing
        .split(':')
        .map(|e| e.split('/').next().unwrap_or("").to_ascii_uppercase())
        .collect();
    let fwd: Vec<String> = names
        .into_iter()
        .filter(|n| !BLOCKED.iter().any(|b| b.eq_ignore_ascii_case(n)))
        .filter(|n| !already.contains(&n.to_ascii_uppercase()))
        .filter(|n| lookup(n))
        .collect();
    let mut parts: Vec<String> = existing.split(':').filter(|s| !s.is_empty()).map(String::from).collect();
    parts.extend(fwd.iter().cloned());
    (parts.join(":"), fwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn env_flags() {
        let a = s(&["run", "-e", "TOKEN", "-e", "A=1", "--env", "B", "--env=C", "--env=D=2", "alpine"]);
        assert_eq!(env_flag_names(&a), s(&["TOKEN", "B", "C"]));
    }

    #[test]
    fn compose_vars() {
        let t = "image: app:${TAG:-latest}\nenv: $DB_URL\nport: \"${PORT}\"\nescaped: $$NOT\nbad: ${1X}\n";
        assert_eq!(compose_var_names(t), s(&["DB_URL", "PORT", "TAG"]));
    }

    #[test]
    fn wslenv_merge_and_block() {
        let args = s(&["run", "-e", "TOKEN", "-e", "DOCKER_HOST", "-e", "MISSING", "-e", "OLD"]);
        let (w, fwd) = build_wslenv(&args, false, Path::new("."), "OLD/p:X", |n| n != "MISSING");
        assert_eq!(fwd, s(&["TOKEN"]));
        assert_eq!(w, "OLD/p:X:TOKEN");
    }
}
