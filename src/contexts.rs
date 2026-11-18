//! Docker kontexty mapované na cíle WLS (distribuce + engine + port + TLS).
//!
//! Uložené v `%APPDATA%\WLS_Desktop\contexts.json` (pro testy `WLS_CONTEXTS_FILE`).
//! Implicitní kontext `default` = aktuální nastavení aplikace (config.json) a v souboru
//! se neukládá. „Aktuální“ kontext je v contexts.json (`current`), ne v nastavení aplikace.
//!
//! Výběr kontextu (od nejvyšší priority): `--context` / `-c`, proměnná `DOCKER_CONTEXT`,
//! `current` v contexts.json, `default`.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT: &str = "default";

/// Kam se příkazy posílají.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub distro: String,
    pub engine: crate::engine::Engine,
    pub port: u16,
    pub tls: bool,
}

impl Target {
    /// Adresa API enginu (jak ji uvidí nástroje ve Windows).
    pub fn endpoint(&self) -> String {
        format!("tcp://127.0.0.1:{}", self.port)
    }
}

/// Jeden uložený kontext.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Context {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub distro: String,
    #[serde(default)]
    pub engine: crate::engine::Engine,
    pub port: u16,
    #[serde(default)]
    pub tls: bool,
}

impl Context {
    pub fn target(&self) -> Target {
        Target { distro: self.distro.clone(), engine: self.engine, port: self.port, tls: self.tls }
    }
}

/// Obsah contexts.json.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Store {
    /// aktuální kontext; None / "default" = výchozí
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    pub contexts: Vec<Context>,
}

/// Cesta k contexts.json (`WLS_CONTEXTS_FILE` má přednost – pro testy).
pub fn path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("WLS_CONTEXTS_FILE").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("WLS_Desktop").join("contexts.json"))
}

impl Store {
    /// Načte soubor; chybějící soubor = prázdný seznam, poškozený = chyba.
    pub fn load_from(p: &std::path::Path) -> Result<Self, String> {
        match std::fs::read_to_string(p) {
            Ok(s) if s.trim().is_empty() => Ok(Self::default()),
            Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{}: {e}", p.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    }

    pub fn save_to(&self, p: &std::path::Path) -> Result<(), String> {
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let s = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        // zápis přes dočasný soubor – souběžné volání shimu nenačte polovinu
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, s).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, p).map_err(|e| e.to_string())
    }

    pub fn get(&self, name: &str) -> Option<&Context> {
        self.contexts.iter().find(|c| c.name == name)
    }

    /// Aktuální kontext podle souboru (bez ohledu na přepínač a proměnnou).
    pub fn current_name(&self) -> &str {
        self.current.as_deref().filter(|c| !c.is_empty()).unwrap_or(DEFAULT)
    }

    /// Cíl pro kontext; `default` = výchozí cíl z nastavení aplikace.
    pub fn resolve(&self, name: &str, default: &Target) -> Result<Target, String> {
        if name == DEFAULT {
            return Ok(default.clone());
        }
        self.get(name).map(|c| c.target()).ok_or_else(|| format!("context \"{name}\" does not exist"))
    }
}

/// Název vybraného kontextu: přepínač > DOCKER_CONTEXT > current v souboru > default.
pub fn select<'a>(flag: Option<&'a str>, env: Option<&'a str>, store: &'a Store) -> &'a str {
    flag.filter(|f| !f.is_empty())
        .or(env.filter(|e| !e.is_empty()))
        .unwrap_or_else(|| store.current_name())
}

/// Globální přepínače Docker CLI s hodnotou (před podpříkazem).
const GLOBAL_VALUE_FLAGS: &[&str] = &["--config", "-c", "--context", "-H", "--host", "-l", "--log-level", "--tlscacert", "--tlscert", "--tlskey"];

/// Odebere globální `--context <n>` / `--context=<n>` / `-c <n>` (jen před podpříkazem).
/// Vrací (kontext, zbylé argumenty). Chybějící hodnota = Err.
pub fn strip_context_flag(args: &[String]) -> Result<(Option<String>, Vec<String>), String> {
    let mut out = Vec::with_capacity(args.len());
    let mut ctx = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !a.starts_with('-') || a == "--" {
            out.extend_from_slice(&args[i..]);
            break;
        }
        if a == "--context" || a == "-c" {
            ctx = Some(args.get(i + 1).ok_or_else(|| format!("flag needs an argument: {a}"))?.clone());
            i += 2;
            continue;
        }
        if let Some(v) = a.strip_prefix("--context=").or_else(|| a.strip_prefix("-c=")) {
            ctx = Some(v.to_string());
            i += 1;
            continue;
        }
        out.push(a.clone());
        if GLOBAL_VALUE_FLAGS.contains(&a.as_str()) {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    Ok((ctx, out))
}

/// Název kontextu podle Docker CLI: `^[a-zA-Z0-9][a-zA-Z0-9_.+-]+$`.
pub fn valid_name(n: &str) -> bool {
    let b = n.as_bytes();
    b.len() >= 2
        && b[0].is_ascii_alphanumeric()
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || b"_.+-".contains(c))
}

/// Výstup `wsl.exe -l -q` (UTF-16LE, případně s BOM) -> názvy distribucí.
pub fn parse_wsl_list(raw: &[u8]) -> Vec<String> {
    let text = if raw.len() >= 2 && raw.len().is_multiple_of(2) && raw.iter().skip(1).step_by(2).filter(|b| **b == 0).count() * 2 >= raw.len() / 2 {
        let u: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16_lossy(&u)
    } else {
        String::from_utf8_lossy(raw).into_owned()
    };
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\0').to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Nainstalované WSL distribuce.
pub fn wsl_distros() -> Result<Vec<String>, String> {
    let mut cmd = std::process::Command::new("wsl.exe");
    cmd.args(["-l", "-q"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let o = cmd.output().map_err(|e| format!("cannot run wsl.exe: {e}"))?;
    if !o.status.success() {
        return Err("wsl.exe -l -q failed".into());
    }
    Ok(parse_wsl_list(&o.stdout))
}

/// Najde distribuci (WSL nerozlišuje velikost písmen) a vrátí její přesný název.
pub fn match_distro(name: &str, installed: &[String]) -> Result<String, String> {
    installed
        .iter()
        .find(|d| d.eq_ignore_ascii_case(name))
        .cloned()
        .ok_or_else(|| {
            let list = if installed.is_empty() { "none".to_string() } else { installed.join(", ") };
            format!("WSL distribution \"{name}\" does not exist (installed: {list})")
        })
}

/// `--docker host=tcp://127.0.0.1:PORT[,skip-tls-verify=…]` -> port.
pub fn parse_docker_endpoint(v: &str) -> Result<u16, String> {
    let mut port = None;
    for kv in v.split(',').filter(|s| !s.is_empty()) {
        let (k, val) = kv.split_once('=').ok_or_else(|| format!("invalid --docker value {v:?}: expected key=value"))?;
        match k.trim() {
            "host" => {
                let rest = val
                    .trim()
                    .strip_prefix("tcp://")
                    .ok_or_else(|| format!("unsupported docker host {val:?}: only tcp://127.0.0.1:PORT is supported"))?;
                let (h, p) = rest.rsplit_once(':').ok_or_else(|| format!("docker host {val:?} has no port"))?;
                if !matches!(h, "127.0.0.1" | "localhost") {
                    return Err(format!("unsupported docker host {val:?}: only 127.0.0.1 / localhost is supported"));
                }
                port = Some(p.trim_end_matches('/').parse::<u16>().map_err(|_| format!("invalid port in {val:?}"))?);
            }
            "skip-tls-verify" => {}
            other => return Err(format!("unsupported --docker option {other:?} (WLS contexts use --tls and the app's certificates)")),
        }
    }
    port.ok_or_else(|| "--docker needs host=tcp://127.0.0.1:PORT".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn store() -> Store {
        Store {
            current: Some("work".into()),
            contexts: vec![
                Context { name: "work".into(), description: "Ubuntu docker".into(), distro: "Ubuntu".into(), engine: Engine::Docker, port: 2380, tls: false },
                Context { name: "sec".into(), description: String::new(), distro: "Debian".into(), engine: Engine::Podman, port: 2376, tls: true },
            ],
        }
    }

    #[test]
    fn strip_flag() {
        let (c, a) = strip_context_flag(&s(&["--context", "work", "ps", "-a"])).unwrap();
        assert_eq!((c.as_deref(), a), (Some("work"), s(&["ps", "-a"])));
        let (c, a) = strip_context_flag(&s(&["-l", "debug", "-c", "x", "images"])).unwrap();
        assert_eq!((c.as_deref(), a), (Some("x"), s(&["-l", "debug", "images"])));
        let (c, a) = strip_context_flag(&s(&["--context=y", "--config", "C:\\cfg", "ps"])).unwrap();
        assert_eq!((c.as_deref(), a), (Some("y"), s(&["--config", "C:\\cfg", "ps"])));
        // za podpříkazem se nic neodebírá (`docker run -c 512` = cpu-shares)
        let v = s(&["run", "-c", "512", "alpine"]);
        assert_eq!(strip_context_flag(&v).unwrap(), (None, v.clone()));
        let v = s(&["--host", "-c", "ps"]);
        assert_eq!(strip_context_flag(&v).unwrap().0, None);
        assert!(strip_context_flag(&s(&["--context"])).is_err());
    }

    #[test]
    fn precedence() {
        let st = store();
        assert_eq!(select(Some("sec"), Some("x"), &st), "sec");
        assert_eq!(select(None, Some("x"), &st), "x");
        assert_eq!(select(Some(""), Some(""), &st), "work");
        assert_eq!(select(None, None, &Store::default()), "default");
        let mut st2 = st.clone();
        st2.current = Some("default".into());
        assert_eq!(select(None, None, &st2), "default");
    }

    #[test]
    fn resolve_targets() {
        let st = store();
        let def = Target { distro: "Debian".into(), engine: Engine::Podman, port: 2375, tls: false };
        assert_eq!(st.resolve("default", &def).unwrap(), def);
        let t = st.resolve("work", &def).unwrap();
        assert_eq!((t.distro.as_str(), t.engine, t.port, t.tls), ("Ubuntu", Engine::Docker, 2380, false));
        assert_eq!(st.resolve("nope", &def).unwrap_err(), "context \"nope\" does not exist");
        assert_eq!(t.endpoint(), "tcp://127.0.0.1:2380");
    }

    #[test]
    fn json_roundtrip() {
        let dir = std::env::temp_dir().join(format!("wls-ctx-test-{}", std::process::id()));
        let p = dir.join("contexts.json");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Store::load_from(&p).unwrap(), Store::default());
        let st = store();
        st.save_to(&p).unwrap();
        assert_eq!(Store::load_from(&p).unwrap(), st);
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("\"engine\": \"podman\""), "{text}");
        // chybějící nepovinná pole
        std::fs::write(&p, r#"{"contexts":[{"name":"a1","distro":"X","port":1}]}"#).unwrap();
        let l = Store::load_from(&p).unwrap();
        assert_eq!(l.current_name(), "default");
        assert_eq!(l.contexts[0].engine, Engine::Docker);
        std::fs::write(&p, "{broken").unwrap();
        assert!(Store::load_from(&p).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_and_distros() {
        assert!(valid_name("work") && valid_name("a-b.c_d+1") && !valid_name("x") && !valid_name("-x") && !valid_name("a b"));
        let raw: Vec<u8> = "\u{feff}Debian\r\nUbuntu-24.04\r\n\r\n".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let l = parse_wsl_list(&raw);
        assert_eq!(l, s(&["Debian", "Ubuntu-24.04"]));
        assert_eq!(parse_wsl_list(b"Debian\nArch\n"), s(&["Debian", "Arch"]));
        assert_eq!(match_distro("debian", &l).unwrap(), "Debian");
        assert!(match_distro("Fedora", &l).unwrap_err().contains("installed: Debian, Ubuntu-24.04"));
    }

    #[test]
    fn docker_endpoint() {
        assert_eq!(parse_docker_endpoint("host=tcp://127.0.0.1:2380").unwrap(), 2380);
        assert_eq!(parse_docker_endpoint("host=tcp://localhost:2375,skip-tls-verify=true").unwrap(), 2375);
        assert!(parse_docker_endpoint("host=unix:///var/run/docker.sock").is_err());
        assert!(parse_docker_endpoint("host=tcp://10.0.0.1:2375").is_err());
        assert!(parse_docker_endpoint("ca=C:\\x").is_err());
        assert!(parse_docker_endpoint("").is_err());
    }
}
