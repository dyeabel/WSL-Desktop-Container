//! Hledání lokálního enginu na Linuxu: `DOCKER_HOST`, aktivní Docker context, výchozí sockety.
//! Čistá funkce (prostředí, home i test existence se předávají), takže běží i v testech na Windows.

use std::path::{Path, PathBuf};

use crate::http::Endpoint;

/// Co se našlo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Endpoint(Endpoint),
    /// Vzdálený engine (nepodporovaný), text je původní adresa.
    Remote(String),
    None,
}

/// Výsledek rozboru adresy enginu.
enum Parsed {
    Local(Endpoint),
    Remote(String),
}

/// `unix://<cesta>`, `tcp://127.0.0.1:<p>` a `tcp://localhost:<p>` jsou lokální, vše ostatní vzdálené.
fn parse_host(host: &str) -> Parsed {
    let h = host.trim();
    if let Some(path) = h.strip_prefix("unix://") {
        if !path.is_empty() {
            return Parsed::Local(Endpoint::Unix(PathBuf::from(path)));
        }
    }
    if let Some(rest) = h.strip_prefix("tcp://") {
        let rest = rest.trim_end_matches('/');
        for pre in ["127.0.0.1:", "localhost:"] {
            if let Some(p) = rest.strip_prefix(pre) {
                if let Ok(port) = p.parse::<u16>() {
                    return Parsed::Local(Endpoint::Tcp { port });
                }
            }
        }
    }
    Parsed::Remote(mask_userinfo(h))
}

/// Adresa bez hesla (`ssh://user:pass@host` → `ssh://user:***@host`) pro hlášky.
fn mask_userinfo(addr: &str) -> String {
    let Some((scheme, rest)) = addr.split_once("://") else {
        return addr.to_string();
    };
    // userinfo je jen v autoritě (do prvního `/`)
    let auth_end = rest.find('/').unwrap_or(rest.len());
    let (auth, path) = rest.split_at(auth_end);
    match auth.rsplit_once('@') {
        Some((info, host)) => {
            let user = info.split_once(':').map(|(u, _)| format!("{u}:***")).unwrap_or_else(|| info.to_string());
            format!("{scheme}://{user}@{host}{path}")
        }
        None => addr.to_string(),
    }
}

fn sha256_hex(s: &str) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, s.as_bytes());
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Adresa Dockeru z aktivního contextu (`None` = výchozí kontext nebo nečitelná konfigurace).
fn context_host(env: &dyn Fn(&str) -> Option<String>, home: &Path) -> Option<String> {
    // DOCKER_CONFIG přesouvá celou konfigurační složku (jako u CLI Dockeru)
    let docker = match env("DOCKER_CONFIG").filter(|s| !s.is_empty()) {
        Some(d) => PathBuf::from(d),
        // bez (absolutního) HOME by se četla relativní `.docker` z pracovní složky
        None if home.is_absolute() => home.join(".docker"),
        None => return None,
    };
    let name = match env("DOCKER_CONTEXT").filter(|s| !s.is_empty()) {
        Some(n) => n,
        None => {
            let txt = std::fs::read_to_string(docker.join("config.json")).ok()?;
            let v: serde_json::Value = serde_json::from_str(&txt).ok()?;
            v.get("currentContext")?.as_str()?.to_string()
        }
    };
    if name.is_empty() || name == "default" {
        return None;
    }
    let meta = docker.join("contexts").join("meta").join(sha256_hex(&name)).join("meta.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(meta).ok()?).ok()?;
    v.pointer("/Endpoints/docker/Host")?.as_str().map(str::to_string)
}

/// Najde endpoint enginu. Druhá složka je volitelná hláška pro UI.
pub fn discover(
    env: &dyn Fn(&str) -> Option<String>,
    home: &Path,
    exists: &dyn Fn(&Path) -> bool,
) -> (Found, Option<String>) {
    let mut remote: Option<String> = None;
    let hosts = [env("DOCKER_HOST").filter(|s| !s.trim().is_empty()), context_host(env, home)];
    for h in hosts.into_iter().flatten() {
        match parse_host(&h) {
            Parsed::Local(ep) => return (Found::Endpoint(ep), None),
            Parsed::Remote(r) => {
                remote.get_or_insert(r);
            }
        }
    }

    let mut socks: Vec<PathBuf> = vec![PathBuf::from("/var/run/docker.sock")];
    if let Some(x) = env("XDG_RUNTIME_DIR").filter(|s| !s.is_empty()) {
        socks.push(Path::new(&x).join("podman").join("podman.sock"));
    }
    socks.push(PathBuf::from("/run/podman/podman.sock"));
    if let Some(s) = socks.into_iter().find(|p| exists(p)) {
        let msg = remote.map(|r| {
            let sock = s.display();
            crate::tf!(
                "DOCKER_HOST/kontext ukazuje na vzdálený engine ({r}), který aplikace na Linuxu nepodporuje; používá se lokální {sock}.",
                "DOCKER_HOST/context points to a remote engine ({r}) that the app does not support on Linux; using local {sock}."
            )
        });
        return (Found::Endpoint(Endpoint::Unix(s)), msg);
    }

    match remote {
        Some(r) => {
            let msg = crate::tf!(
                "DOCKER_HOST/kontext ukazuje na vzdálený engine ({r}), který aplikace na Linuxu nepodporuje, a lokální engine nebyl nalezen.",
                "DOCKER_HOST/context points to a remote engine ({r}) that the app does not support on Linux, and no local engine was found."
            );
            (Found::Remote(r), Some(msg))
        }
        None => (
            Found::None,
            Some(
                crate::t!(
                    "Engine nenalezen. Spusťte Docker (sudo systemctl enable --now docker), nebo rootless Podman (systemctl --user enable --now podman.socket).",
                    "Engine not found. Start Docker (sudo systemctl enable --now docker) or rootless Podman (systemctl --user enable --now podman.socket)."
                )
                .to_string(),
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn envf(m: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> = m.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    fn only(paths: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |p| paths.iter().any(|x| Path::new(x) == p)
    }

    fn no_home() -> PathBuf {
        PathBuf::from("target/test-data/discover-no-such-home")
    }

    fn tmp_home(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wslcd-discover-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write_ctx(home: &Path, name: &str, host: &str) {
        let dir = home.join(".docker/contexts/meta").join(sha256_hex(name));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), format!(r#"{{"Name":"{name}","Endpoints":{{"docker":{{"Host":"{host}"}}}}}}"#)).unwrap();
    }

    fn write_cfg(home: &Path, cur: &str) {
        std::fs::create_dir_all(home.join(".docker")).unwrap();
        std::fs::write(home.join(".docker/config.json"), format!(r#"{{"currentContext":"{cur}"}}"#)).unwrap();
    }

    #[test]
    fn sha_known_value() {
        assert_eq!(sha256_hex("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn docker_host_unix() {
        let (f, m) = discover(&envf(&[("DOCKER_HOST", "unix:///run/user/1000/docker.sock")]), &no_home(), &|_| false);
        assert_eq!(f, Found::Endpoint(Endpoint::Unix("/run/user/1000/docker.sock".into())));
        assert!(m.is_none());
    }

    #[test]
    fn docker_host_tcp_local() {
        let (f, _) = discover(&envf(&[("DOCKER_HOST", "tcp://127.0.0.1:2375")]), &no_home(), &|_| false);
        assert_eq!(f, Found::Endpoint(Endpoint::Tcp { port: 2375 }));
        let (f, _) = discover(&envf(&[("DOCKER_HOST", "tcp://localhost:2376")]), &no_home(), &|_| false);
        assert_eq!(f, Found::Endpoint(Endpoint::Tcp { port: 2376 }));
    }

    #[test]
    fn docker_host_remote_falls_back() {
        let (f, m) = discover(&envf(&[("DOCKER_HOST", "tcp://10.0.0.5:2375")]), &no_home(), &only(&["/var/run/docker.sock"]));
        assert_eq!(f, Found::Endpoint(Endpoint::Unix("/var/run/docker.sock".into())));
        let m = m.unwrap();
        assert!(m.contains("10.0.0.5") && m.contains("/var/run/docker.sock"), "{m}");
        // bez lokálního socketu zůstane Remote
        let (f, m) = discover(&envf(&[("DOCKER_HOST", "tcp://10.0.0.5:2375")]), &no_home(), &|_| false);
        assert_eq!(f, Found::Remote("tcp://10.0.0.5:2375".into()));
        assert!(m.is_some());
    }

    #[test]
    fn ssh_is_remote() {
        let (f, _) = discover(&envf(&[("DOCKER_HOST", "ssh://user@host")]), &no_home(), &|_| false);
        assert_eq!(f, Found::Remote("ssh://user@host".into()));
    }

    #[test]
    fn context_from_config() {
        let home = tmp_home("cfg");
        write_cfg(&home, "mine");
        write_ctx(&home, "mine", "unix:///custom/docker.sock");
        let (f, _) = discover(&envf(&[]), &home, &only(&["/var/run/docker.sock"]));
        assert_eq!(f, Found::Endpoint(Endpoint::Unix("/custom/docker.sock".into())));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn default_context_ignored() {
        let home = tmp_home("def");
        write_cfg(&home, "default");
        let (f, m) = discover(&envf(&[]), &home, &only(&["/var/run/docker.sock"]));
        assert_eq!(f, Found::Endpoint(Endpoint::Unix("/var/run/docker.sock".into())));
        assert!(m.is_none());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn docker_context_env_overrides_config() {
        let home = tmp_home("envctx");
        write_cfg(&home, "a");
        write_ctx(&home, "a", "unix:///a.sock");
        write_ctx(&home, "b", "tcp://127.0.0.1:2999");
        let (f, _) = discover(&envf(&[("DOCKER_CONTEXT", "b")]), &home, &|_| false);
        assert_eq!(f, Found::Endpoint(Endpoint::Tcp { port: 2999 }));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn default_docker_sock() {
        let (f, _) = discover(&envf(&[("XDG_RUNTIME_DIR", "/run/user/1000")]), &no_home(), &only(&["/var/run/docker.sock", "/run/podman/podman.sock"]));
        assert_eq!(f, Found::Endpoint(Endpoint::Unix("/var/run/docker.sock".into())));
    }

    #[test]
    fn rootless_podman() {
        let e = envf(&[("XDG_RUNTIME_DIR", "/run/user/1000")]);
        let (f, _) = discover(&e, &no_home(), &only(&["/run/user/1000/podman/podman.sock", "/run/podman/podman.sock"]));
        assert_eq!(f, Found::Endpoint(Endpoint::Unix("/run/user/1000/podman/podman.sock".into())));
        let (f, _) = discover(&e, &no_home(), &only(&["/run/podman/podman.sock"]));
        assert_eq!(f, Found::Endpoint(Endpoint::Unix("/run/podman/podman.sock".into())));
    }

    #[test]
    fn remote_credentials_masked() {
        assert_eq!(mask_userinfo("ssh://user:pass@host:22"), "ssh://user:***@host:22");
        assert_eq!(mask_userinfo("tcp://a:b@10.0.0.5:2375/x@y"), "tcp://a:***@10.0.0.5:2375/x@y");
        assert_eq!(mask_userinfo("ssh://user@host"), "ssh://user@host");
        assert_eq!(mask_userinfo("tcp://10.0.0.5:2375"), "tcp://10.0.0.5:2375");
        let e = envf(&[("DOCKER_HOST", "ssh://me:s3cret@far")]);
        let (f, m) = discover(&e, &no_home(), &only(&["/var/run/docker.sock"]));
        assert_eq!(f, Found::Endpoint(Endpoint::Unix("/var/run/docker.sock".into())));
        let m = m.unwrap();
        assert!(!m.contains("s3cret") && m.contains("ssh://me:***@far"), "{m}");
        let (f, m) = discover(&e, &no_home(), &|_| false);
        assert_eq!(f, Found::Remote("ssh://me:***@far".into()));
        assert!(!m.unwrap().contains("s3cret"));
    }

    #[test]
    fn docker_config_env_used_for_context() {
        let home = tmp_home("dcfg-home");
        let cfgdir = tmp_home("dcfg-dir");
        // v <home>/.docker je jiný kontext, který se nesmí použít
        write_cfg(&home, "wrong");
        write_ctx(&home, "wrong", "unix:///wrong.sock");
        std::fs::write(cfgdir.join("config.json"), r#"{"currentContext":"right"}"#).unwrap();
        let dir = cfgdir.join("contexts/meta").join(sha256_hex("right"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), r#"{"Endpoints":{"docker":{"Host":"unix:///right.sock"}}}"#).unwrap();
        let c = cfgdir.to_string_lossy().to_string();
        let (f, _) = discover(&envf(&[("DOCKER_CONFIG", c.as_str())]), &home, &|_| false);
        assert_eq!(f, Found::Endpoint(Endpoint::Unix("/right.sock".into())));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&cfgdir);
    }

    /// Prázdný nebo relativní HOME: kontext z `.docker` v pracovní složce se nečte.
    #[test]
    #[cfg(unix)]
    fn relative_home_skips_context() {
        let home = tmp_home("relhome");
        write_cfg(&home, "rel");
        write_ctx(&home, "rel", "unix:///rel.sock");
        assert_eq!(discover(&envf(&[]), &home, &|_| false).0, Found::Endpoint(Endpoint::Unix("/rel.sock".into())));
        // stejná složka zadaná relativně k pracovní složce
        let cwd = std::env::current_dir().unwrap();
        let up = "../".repeat(cwd.components().count() - 1);
        let rel = PathBuf::from(format!("{up}{}", home.strip_prefix("/").unwrap().display()));
        assert!(rel.join(".docker/config.json").exists());
        assert_eq!(discover(&envf(&[]), &rel, &|_| false).0, Found::None);
        assert_eq!(discover(&envf(&[]), Path::new(""), &|_| false).0, Found::None);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn nothing_found() {
        let (f, m) = discover(&envf(&[]), &no_home(), &|_| false);
        assert_eq!(f, Found::None);
        assert!(m.unwrap().contains("systemctl"));
    }
}
