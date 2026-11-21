//! Abstrakce hostitele: vše, co se dnes spouští přes `wsl.exe`, půjde přes trait [`Host`].

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub mod discover;
pub mod wsl;

use crate::activity;
use crate::config::Config;
pub use crate::error::HostError;

/// Pod jakým uživatelem se příkaz spustí.
///
/// Pravidlo pro `Engine` a `Root`:
/// - `Engine`: příkaz jen volá CLI enginu (`docker`, `podman`, `kubectl` přes `docker exec`)
///   nebo testuje či čte jeho soubory (marker API, `/var/lib/docker` a `/var/lib/containers`
///   jen pro čtení).
/// - `Root`: cokoli jiného. Patří sem `systemctl`, zápis do `/etc` nebo `/usr/local`, `nsenter`,
///   `ss -p`, `conntrack`, `iptables`, `sysctl` a `drop_caches`, soubory v režimu root a
///   instalační a nastavovací skripty z `engine.rs` a `lite/mod.rs`.
/// - Skript, který dělá obojí, je `Root`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Privilege {
    /// výchozí uživatel distribuce (bez `-u`)
    User,
    /// uživatel, pod kterým běží engine (ve WSL root)
    Engine,
    Root,
}

/// Požadavek na spuštění programu u hostitele.
pub struct ExecReq<'a> {
    pub args: &'a [&'a str],
    pub privilege: Privilege,
    pub cwd: Option<&'a str>,
    /// bez vstupu = `Stdio::null()`
    pub stdin: Option<&'a [u8]>,
    /// zapsat do přehledu „Na pozadí“ (výchozí ano)
    pub track: bool,
}

impl<'a> ExecReq<'a> {
    pub fn new(args: &'a [&'a str]) -> Self {
        ExecReq { args, privilege: Privilege::User, cwd: None, stdin: None, track: true }
    }

    pub fn privilege(mut self, p: Privilege) -> Self {
        self.privilege = p;
        self
    }

    pub fn cwd(mut self, cwd: Option<&'a str>) -> Self {
        self.cwd = cwd;
        self
    }

    /// Bez záznamu do přehledu „Na pozadí“ (výpisy, čtení).
    pub fn quiet(mut self) -> Self {
        self.track = false;
        self
    }

    pub fn stdin(mut self, input: &'a [u8]) -> Self {
        self.stdin = Some(input);
        self
    }
}

/// Surový výsledek spuštěného procesu.
pub struct ExecOut {
    pub code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Druh hostitele.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostKind {
    Wsl,
    Local,
}

/// Co hostitel umí; UI podle toho skrývá funkce, které dávají smysl jen ve WSL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// seznam a správa distribucí, průvodce přidáním distribuce
    pub distros: bool,
    /// instalace, API a odinstalace enginu
    pub engine_install: bool,
    pub api_tls: bool,
    pub daemon_json: bool,
    /// VmmemWSL a uvolňování paměti
    pub vm_memory: bool,
    /// VHDX, export, import a kompaktace
    pub vm_disk: bool,
    pub wslconfig: bool,
    /// shim `docker` v PATH
    pub shim: bool,
    /// pojmenovaná roura Dockeru
    pub npipe: bool,
    pub ipv6_relay: bool,
    /// sloupce ze `ss -p` jako root
    pub root_sampling: bool,
    /// přepínač „Pracovat jako root“ v Souborech
    pub files_root: bool,
    /// portable a WSLCD Lite
    pub lite: bool,
    pub contexts: bool,
}

impl Caps {
    pub const WSL: Caps = Caps::all(true);
    #[allow(dead_code)] // použije místní (Linux) hostitel
    pub const LOCAL: Caps = Caps::all(false);

    const fn all(v: bool) -> Caps {
        Caps {
            distros: v,
            engine_install: v,
            api_tls: v,
            daemon_json: v,
            vm_memory: v,
            vm_disk: v,
            wslconfig: v,
            shim: v,
            npipe: v,
            ipv6_relay: v,
            root_sampling: v,
            files_root: v,
            lite: v,
            contexts: v,
        }
    }
}

pub trait Host: Send + Sync {
    fn kind(&self) -> HostKind;

    fn caps(&self) -> Caps;

    #[allow(dead_code)] // použije se u dalších hostitelů (Linux, Windows)
    fn label(&self) -> String;

    /// `Err` jen když se proces nespustil; nenulový kód je `Ok`.
    fn exec(&self, req: &ExecReq) -> Result<ExecOut, HostError>;

    /// Průběžně předává řádky výstupu, vrací návratový kód.
    fn exec_stream(
        &self,
        req: &ExecReq,
        cancel: Option<&AtomicBool>,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<i32, HostError>;

    fn open_terminal(&self, title: &str, req: &ExecReq) -> Result<(), HostError>;

    fn to_guest_path(&self, path: &str) -> Result<String, HostError>;

    fn engine_endpoint(&self, cfg: &Config) -> crate::http::Endpoint;

    /// Konkrétní WSL hostitel (pro kód, který potřebuje věci mimo trait).
    fn wsl(&self) -> Option<&wsl::WslHost> {
        None
    }
}

/// Aktuální hostitel podle konfigurace (nic se necachuje).
pub fn current(cfg: &Config) -> Arc<dyn Host> {
    Arc::new(wsl::WslHost::new(&cfg.distro))
}

/// Kudy se z aplikace dostaneme k API enginu aktuálního hostitele.
pub fn endpoint(cfg: &Config) -> crate::http::Endpoint {
    current(cfg).engine_endpoint(cfg)
}

/// Pohodlné obálky nad [`Host::exec`] se záznamem do přehledu „Na pozadí“.
pub trait HostExt: Host {
    fn run(&self, p: Privilege, cwd: Option<&str>, args: &[&str]) -> Result<String, HostError>;
    fn run_owned(&self, p: Privilege, cwd: Option<&str>, args: &[String]) -> Result<String, HostError>;
    fn run_stdin(&self, p: Privilege, args: &[&str], input: &str) -> Result<String, HostError>;
}

impl<T: Host + ?Sized> HostExt for T {
    fn run(&self, p: Privilege, cwd: Option<&str>, args: &[&str]) -> Result<String, HostError> {
        // záznam do přehledu dělá `exec`
        self.exec(&ExecReq::new(args).privilege(p).cwd(cwd)).and_then(finish)
    }

    fn run_owned(&self, p: Privilege, cwd: Option<&str>, args: &[String]) -> Result<String, HostError> {
        let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        self.run(p, cwd, &a)
    }

    fn run_stdin(&self, p: Privilege, args: &[&str], input: &str) -> Result<String, HostError> {
        self.exec(&ExecReq::new(args).privilege(p).stdin(input.as_bytes())).and_then(finish)
    }
}

/// wsl.exe vrací podle verze buď UTF-8, nebo UTF-16LE.
pub fn decode(bytes: &[u8]) -> String {
    let zeros = bytes.iter().filter(|b| **b == 0).count();
    if bytes.len() >= 2 && zeros * 4 > bytes.len() {
        let u16s: Vec<u16> = bytes
            .as_chunks::<2>().0.iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u16s).replace('\u{feff}', "")
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Nulový kód = stdout (+stderr); jinak chyba s textem stderr, případně stdout.
pub fn finish(out: ExecOut) -> Result<String, HostError> {
    let stdout = decode(&out.stdout);
    let stderr = decode(&out.stderr);
    if out.code == 0 {
        let mut s = stdout;
        if !stderr.trim().is_empty() {
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str(&stderr);
        }
        Ok(s)
    } else {
        let output = if stderr.trim().is_empty() { stdout } else { stderr };
        Err(HostError::Failed { code: out.code, output })
    }
}

/// Záznam pro požadavek: žádný, když je `quiet`.
pub(crate) fn track_req(label: &str, req: &ExecReq) -> Option<activity::Guard> {
    req.track.then(|| track(label, req.privilege, req.args))
}

/// Záznam do přehledu „Na pozadí“ (hostitel, oprávnění, zkrácený příkaz).
pub(crate) fn track(label: &str, p: Privilege, args: &[&str]) -> activity::Guard {
    let title = match p {
        Privilege::User => label.to_string(),
        _ => format!("{label} (root)"),
    };
    activity::start(activity::Kind::Wsl, title, activity::describe_cmd(args, 160))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Všechny příznaky v poli (destrukturace hlídá, že žádný nechybí).
    fn flags(c: Caps) -> [bool; 14] {
        let Caps {
            distros, engine_install, api_tls, daemon_json, vm_memory, vm_disk, wslconfig, shim,
            npipe, ipv6_relay, root_sampling, files_root, lite, contexts,
        } = c;
        [
            distros, engine_install, api_tls, daemon_json, vm_memory, vm_disk, wslconfig, shim,
            npipe, ipv6_relay, root_sampling, files_root, lite, contexts,
        ]
    }

    #[test]
    fn wsl_caps_all_true() {
        let h = wsl::WslHost::new("x");
        assert_eq!(h.kind(), HostKind::Wsl);
        assert_eq!(h.caps(), Caps::WSL);
        assert!(flags(h.caps()).iter().all(|v| *v));
    }

    #[test]
    fn local_caps_all_false() {
        assert!(flags(Caps::LOCAL).iter().all(|v| !*v));
    }

    struct FakeHost {
        out: Mutex<Option<ExecOut>>,
        seen_stdin: Mutex<Option<Vec<u8>>>,
    }

    impl FakeHost {
        fn new(code: i32, stdout: &[u8], stderr: &[u8]) -> Self {
            FakeHost {
                out: Mutex::new(Some(ExecOut { code, stdout: stdout.to_vec(), stderr: stderr.to_vec() })),
                seen_stdin: Mutex::new(None),
            }
        }
    }

    impl Host for FakeHost {
        fn kind(&self) -> HostKind {
            HostKind::Wsl
        }
        fn caps(&self) -> Caps {
            Caps::WSL
        }
        fn label(&self) -> String {
            "fake".into()
        }
        fn exec(&self, req: &ExecReq) -> Result<ExecOut, HostError> {
            *self.seen_stdin.lock().unwrap() = req.stdin.map(|s| s.to_vec());
            Ok(self.out.lock().unwrap().take().expect("jedno volání"))
        }
        fn exec_stream(&self, _: &ExecReq, _: Option<&AtomicBool>, _: &mut dyn FnMut(&str)) -> Result<i32, HostError> {
            Ok(0)
        }
        fn open_terminal(&self, _: &str, _: &ExecReq) -> Result<(), HostError> {
            Ok(())
        }
        fn to_guest_path(&self, path: &str) -> Result<String, HostError> {
            Ok(path.to_string())
        }
        fn engine_endpoint(&self, _: &Config) -> crate::http::Endpoint {
            crate::http::Endpoint::Tcp { port: 1 }
        }
    }

    /// Hostitel, který zapisuje aktivitu jako `WslHost` (v `exec`), a vždy skončí chybou,
    /// aby se záznam uložil i u krátkého příkazu.
    struct TrackingHost;

    impl Host for TrackingHost {
        fn kind(&self) -> HostKind {
            HostKind::Wsl
        }
        fn caps(&self) -> Caps {
            Caps::WSL
        }
        fn label(&self) -> String {
            "tracking-test-host".into()
        }
        fn exec(&self, req: &ExecReq) -> Result<ExecOut, HostError> {
            let mut g = track_req(&self.label(), req);
            if let Some(g) = g.as_mut() {
                g.set_ok(false);
            }
            Ok(ExecOut { code: 1, stdout: vec![], stderr: b"x".to_vec() })
        }
        fn exec_stream(&self, _: &ExecReq, _: Option<&AtomicBool>, _: &mut dyn FnMut(&str)) -> Result<i32, HostError> {
            Ok(0)
        }
        fn open_terminal(&self, _: &str, _: &ExecReq) -> Result<(), HostError> {
            Ok(())
        }
        fn to_guest_path(&self, path: &str) -> Result<String, HostError> {
            Ok(path.to_string())
        }
        fn engine_endpoint(&self, _: &Config) -> crate::http::Endpoint {
            crate::http::Endpoint::Tcp { port: 1 }
        }
    }

    fn entries(title: &str, detail: &str) -> usize {
        activity::snapshot().1.iter().filter(|d| d.title == title && d.detail == detail).count()
    }

    #[test]
    fn run_records_exactly_one_entry() {
        let h = TrackingHost;
        let _ = h.run(Privilege::User, None, &["uniq-run-cmd"]);
        let _ = h.run_owned(Privilege::User, None, &["uniq-owned-cmd".to_string()]);
        let _ = h.run_stdin(Privilege::User, &["uniq-stdin-cmd"], "x");
        for c in ["uniq-run-cmd", "uniq-owned-cmd", "uniq-stdin-cmd"] {
            assert_eq!(entries("tracking-test-host", c), 1, "{c}");
        }
    }

    #[test]
    fn quiet_records_nothing() {
        let h = TrackingHost;
        let _ = h.exec(&ExecReq::new(&["uniq-quiet-cmd"]).quiet());
        assert_eq!(entries("tracking-test-host", "uniq-quiet-cmd"), 0);
        let _ = h.exec(&ExecReq::new(&["uniq-loud-cmd"]));
        assert_eq!(entries("tracking-test-host", "uniq-loud-cmd"), 1);
    }

    #[test]
    fn run_ok_appends_stderr() {
        let h = FakeHost::new(0, b"a", b"w");
        assert_eq!(h.run(Privilege::User, None, &["x"]), Ok("a\nw".to_string()));
    }

    #[test]
    fn run_err_prefers_stderr() {
        let h = FakeHost::new(2, b"x", b"boom");
        assert_eq!(
            h.run(Privilege::User, None, &["x"]),
            Err(HostError::Failed { code: 2, output: "boom".into() })
        );
    }

    #[test]
    fn run_err_falls_back_to_stdout() {
        let h = FakeHost::new(1, b"only out", b"");
        assert_eq!(
            h.run(Privilege::Root, None, &["x"]),
            Err(HostError::Failed { code: 1, output: "only out".into() })
        );
    }

    #[test]
    fn utf16_stderr_is_readable() {
        let mut err = vec![0xff, 0xfe];
        for u in "Neexistuje".encode_utf16() {
            err.extend_from_slice(&u.to_le_bytes());
        }
        let h = FakeHost::new(1, b"", &err);
        let e = h.run(Privilege::User, None, &["x"]).unwrap_err();
        assert_eq!(e, HostError::Failed { code: 1, output: "Neexistuje".into() });
    }

    #[test]
    fn run_stdin_passes_input() {
        let h = FakeHost::new(0, b"", b"");
        h.run_stdin(Privilege::User, &["cat"], "data").unwrap();
        assert_eq!(h.seen_stdin.lock().unwrap().as_deref(), Some(&b"data"[..]));
    }
}
