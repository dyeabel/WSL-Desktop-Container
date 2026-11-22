//! Hostitel Linux: příkazy běží jako místní procesy, engine se hledá přes [`discover`].

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::discover::{discover, Found};
use super::{output_with_stdin, track_req, Caps, ExecOut, ExecReq, Host, HostError, HostKind, Privilege};
use crate::config::Config;
use crate::http::Endpoint;

/// Název hostitele v přehledu „Na pozadí“.
const LABEL: &str = "localhost";

/// Výchozí socket, když se engine nenašel (UI pak ukáže nedostupnost s hláškou z `note()`).
const DEFAULT_SOCK: &str = "/var/run/docker.sock";

/// Jak dlouho platí výsledek hledání enginu (UI hostitele vytváří v každém snímku).
const DISCOVERY_TTL: Duration = Duration::from_secs(5);

type Discovery = (Found, Option<String>);

/// Poslední výsledek hledání enginu pro celý proces.
static DISCOVERY: Mutex<Option<(Instant, Discovery)>> = Mutex::new(None);

/// Výsledek z `slot`, je-li mladší než `ttl`; jinak zavolá `run` a uloží ho.
fn cached(slot: &Mutex<Option<(Instant, Discovery)>>, now: Instant, ttl: Duration, run: impl FnOnce() -> Discovery) -> Discovery {
    let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, d)) = g.as_ref() {
        if now.saturating_duration_since(*at) < ttl {
            return d.clone();
        }
    }
    let d = run();
    *g = Some((now, d.clone()));
    d
}

/// Zahodí uložený výsledek hledání (obnovení, start enginu).
pub(crate) fn refresh_discovery() {
    *DISCOVERY.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub struct LocalHost {
    found: Found,
    note: Option<String>,
}

impl LocalHost {
    /// Hledá engine ve skutečném prostředí procesu (`DOCKER_HOST`, kontext, sockety);
    /// výsledek se pro celý proces drží [`DISCOVERY_TTL`].
    pub fn new() -> Self {
        let (found, note) = cached(&DISCOVERY, Instant::now(), DISCOVERY_TTL, || {
            // bez HOME zůstane cesta prázdná a discover kontext přeskočí
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            discover(&|k| std::env::var(k).ok(), &home, &|p: &Path| p.exists())
        });
        LocalHost { found, note }
    }

    /// Hláška z hledání enginu pro UI (vzdálený engine, nic nenalezeno).
    #[allow(dead_code)] // zobrazí ji UI (další úkol)
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }
}

/// Běží proces jako root? (Nečitelné `/proc/self` = ne.)
fn is_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").map(|m| m.uid() == 0).unwrap_or(false)
}

/// Skutečný příkazový řádek: `Root` mimo roota přes `pkexec`. `pkexec` mění pracovní složku
/// na domovskou složku roota, proto se při `cwd` do ní nejdřív přepne `sh`.
pub(crate) fn command_line(req: &ExecReq, root: bool) -> Result<Vec<String>, HostError> {
    if req.args.is_empty() {
        return Err(HostError::from(crate::t!("prázdný příkaz", "empty command").to_string()));
    }
    let args = req.args.iter().map(|a| a.to_string());
    if req.privilege != Privilege::Root || root {
        return Ok(args.collect());
    }
    let mut v: Vec<String> = vec!["pkexec".into()];
    if let Some(d) = req.cwd {
        v.extend(["sh".into(), "-c".into(), r#"cd -- "$0" && exec "$@""#.into(), d.to_string()]);
    }
    v.extend(args);
    Ok(v)
}

/// Příkaz připravený ke spuštění (program, argumenty, pracovní složka).
fn command(req: &ExecReq) -> Result<Command, HostError> {
    let line = command_line(req, is_root())?;
    let (prog, rest) = line.split_first().expect("command_line vrací neprázdný příkaz");
    let mut cmd = Command::new(prog);
    cmd.args(rest);
    if let Some(d) = req.cwd {
        cmd.current_dir(d);
    }
    Ok(cmd)
}

/// Terminály v pořadí, v jakém se zkouší: (program, argumenty před příkazem).
pub(crate) fn terminal_candidates(terminal: Option<&str>) -> Vec<(String, Vec<String>)> {
    let mut v: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(t) = terminal.map(str::trim).filter(|t| !t.is_empty()) {
        v.push((t.to_string(), vec!["-e".into()]));
    }
    for (p, pre) in [("x-terminal-emulator", "-e"), ("gnome-terminal", "--"), ("konsole", "-e"), ("xterm", "-e")] {
        v.push((p.to_string(), vec![pre.to_string()]));
    }
    v
}

impl Host for LocalHost {
    fn kind(&self) -> HostKind {
        HostKind::Local
    }

    fn caps(&self) -> Caps {
        Caps::LOCAL
    }

    fn label(&self) -> String {
        LABEL.to_string()
    }

    fn exec(&self, req: &ExecReq) -> Result<ExecOut, HostError> {
        let mut g = track_req(LABEL, req);
        let r = command(req).and_then(|mut cmd| output_with_stdin(&mut cmd, req.stdin));
        if let Some(g) = g.as_mut() {
            g.set_ok(matches!(&r, Ok(o) if o.code == 0));
        }
        r
    }

    fn exec_stream(
        &self,
        req: &ExecReq,
        cancel: Option<&AtomicBool>,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<i32, HostError> {
        let mut g = track_req(LABEL, req);
        let r = command(req).and_then(|mut cmd| Ok(crate::wsl::stream_command(&mut cmd, cancel, on_line)?));
        if let Some(g) = g.as_mut() {
            g.set_ok(matches!(r, Ok(0)));
        }
        r
    }

    fn open_terminal(&self, _title: &str, req: &ExecReq) -> Result<(), HostError> {
        let line = command_line(req, is_root())?;
        let terminal = std::env::var("TERMINAL").ok();
        let mut last = String::new();
        for (prog, pre) in terminal_candidates(terminal.as_deref()) {
            let mut c = Command::new(&prog);
            c.args(&pre).args(&line);
            if let Some(d) = req.cwd {
                c.current_dir(d);
            }
            match c.spawn() {
                Ok(mut child) => {
                    // ukončený terminál uklidit (žádný zombie proces)
                    std::thread::spawn(move || {
                        let _ = child.wait();
                    });
                    return Ok(());
                }
                Err(e) => last = format!("{prog}: {e}"),
            }
        }
        Err(HostError::Spawn(crate::tf!(
            "Nenalezen žádný terminál (nastavte proměnnou TERMINAL). Poslední chyba: {last}",
            "No terminal found (set the TERMINAL variable). Last error: {last}"
        )))
    }

    fn to_guest_path(&self, path: &str) -> Result<String, HostError> {
        Ok(path.to_string())
    }

    fn engine_endpoint(&self, _cfg: &Config) -> Endpoint {
        match &self.found {
            Found::Endpoint(e) => e.clone(),
            _ => Endpoint::Unix(PathBuf::from(DEFAULT_SOCK)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::HostExt;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    fn entries(detail: &str) -> usize {
        crate::activity::snapshot().1.iter().filter(|d| d.title.starts_with(LABEL) && d.detail == detail).count()
    }

    #[test]
    fn kind_caps_label() {
        let h = LocalHost::new();
        assert_eq!(h.kind(), HostKind::Local);
        assert_eq!(h.caps(), Caps::LOCAL);
        assert_eq!(h.label(), "localhost");
        assert!(h.wsl().is_none());
        assert_eq!(h.to_guest_path("/a b/c").unwrap(), "/a b/c");
    }

    #[test]
    fn current_is_local() {
        assert_eq!(crate::host::current(&Config::default()).kind(), HostKind::Local);
    }

    #[test]
    fn endpoint_falls_back_to_docker_sock() {
        let h = LocalHost { found: Found::None, note: Some("x".into()) };
        assert_eq!(h.engine_endpoint(&Config::default()), Endpoint::Unix("/var/run/docker.sock".into()));
        assert_eq!(h.note(), Some("x"));
        let h = LocalHost { found: Found::Remote("ssh://h".into()), note: None };
        assert_eq!(h.engine_endpoint(&Config::default()), Endpoint::Unix("/var/run/docker.sock".into()));
        let h = LocalHost { found: Found::Endpoint(Endpoint::Tcp { port: 2375 }), note: None };
        assert_eq!(h.engine_endpoint(&Config::default()), Endpoint::Tcp { port: 2375 });
    }

    #[test]
    fn terminal_candidates_order() {
        let names = |t| terminal_candidates(t).into_iter().map(|(p, a)| format!("{p} {}", a.join(" "))).collect::<Vec<_>>();
        assert_eq!(
            names(Some("alacritty")),
            ["alacritty -e", "x-terminal-emulator -e", "gnome-terminal --", "konsole -e", "xterm -e"]
        );
        assert_eq!(names(None), ["x-terminal-emulator -e", "gnome-terminal --", "konsole -e", "xterm -e"]);
        assert_eq!(names(Some(" ")).len(), 4);
    }

    #[test]
    fn root_uses_pkexec_when_not_root() {
        let args = ["systemctl", "start", "docker"];
        let root = ExecReq::new(&args).privilege(Privilege::Root);
        assert_eq!(command_line(&root, false).unwrap(), ["pkexec", "systemctl", "start", "docker"]);
        assert_eq!(command_line(&root, true).unwrap(), ["systemctl", "start", "docker"]);
        let with_cwd = ExecReq::new(&args).privilege(Privilege::Root).cwd(Some("/srv/x"));
        assert_eq!(
            command_line(&with_cwd, false).unwrap(),
            ["pkexec", "sh", "-c", r#"cd -- "$0" && exec "$@""#, "/srv/x", "systemctl", "start", "docker"]
        );
        for p in [Privilege::User, Privilege::Engine] {
            assert_eq!(command_line(&ExecReq::new(&args).privilege(p), false).unwrap(), args);
        }
        // prázdný příkaz se odmítne (žádné holé `pkexec`)
        for root in [false, true] {
            assert!(command_line(&ExecReq::new(&[]).privilege(Privilege::Root), root).is_err());
        }
        assert!(LocalHost::new().exec(&ExecReq::new(&[])).is_err());
    }

    /// Složka s mezerou, uvozovkou a `$` jde do `sh` jako samostatný argument `$0`,
    /// skript ji jen cituje. Spuštění stejného skriptu bez pkexec ověří, že se nic nerozvine.
    #[test]
    fn pkexec_cwd_is_not_interpreted() {
        let dir = std::env::temp_dir().join(format!("wslcd local 'q\" $HOME {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();
        let req = ExecReq::new(&["pwd"]).privilege(Privilege::Root).cwd(Some(d));
        let line = command_line(&req, false).unwrap();
        assert_eq!(line[..5], ["pkexec", "sh", "-c", r#"cd -- "$0" && exec "$@""#, d]);
        // totéž bez `pkexec` a bez current_dir: `sh` se do složky přepne sám
        let out = Command::new(&line[1]).args(&line[2..]).current_dir("/").output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'), d);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovery_cached_within_ttl() {
        let slot = Mutex::new(None);
        let calls = std::cell::Cell::new(0);
        let run = || {
            calls.set(calls.get() + 1);
            (Found::None, None)
        };
        let t0 = Instant::now();
        let ttl = Duration::from_secs(5);
        cached(&slot, t0, ttl, run);
        cached(&slot, t0 + Duration::from_secs(4), ttl, run);
        assert_eq!(calls.get(), 1);
        cached(&slot, t0 + Duration::from_secs(6), ttl, run);
        assert_eq!(calls.get(), 2);
        *slot.lock().unwrap() = None;
        cached(&slot, t0 + Duration::from_secs(7), ttl, run);
        assert_eq!(calls.get(), 3);
        // globální cache: LocalHost::new ji naplní (jiné testy ji mohou souběžně jen plnit)
        refresh_discovery();
        let _ = LocalHost::new();
        assert!(DISCOVERY.lock().unwrap().is_some());
    }

    #[test]
    fn contract() {
        let h = LocalHost::new();
        let e = h.run(Privilege::User, None, &["sh", "-c", "echo out; echo err >&2; exit 3"]).unwrap_err();
        assert_eq!(e, HostError::Failed { code: 3, output: "err\n".into() });
        assert_eq!(h.run(Privilege::User, None, &["sh", "-c", "echo out; echo err >&2"]).unwrap(), "out\nerr\n");
        assert_eq!(h.run(Privilege::User, Some("/tmp"), &["pwd"]).unwrap().trim(), "/tmp");
        assert_eq!(h.run_stdin(Privilege::User, &["cat"], "x\ny").unwrap(), "x\ny");
        // vstup větší než roura, proces ho zároveň vypisuje: zápis i čtení musí běžet souběžně
        let big = "0123456789abcdef".repeat(64 * 1024);
        assert_eq!(h.run_stdin(Privilege::User, &["cat"], &big).unwrap().len(), big.len());
        // proces skončí bez čtení vstupu (broken pipe se ignoruje)
        assert_eq!(h.run_stdin(Privilege::User, &["true"], &big).unwrap(), "");
        assert!(matches!(h.exec(&ExecReq::new(&["/nonexistent/wslcd-x"])), Err(HostError::Spawn(_))));

        let mut lines = Vec::new();
        let req = ExecReq::new(&["printf", "a\\rb\\nc\\n"]);
        let code = h.exec_stream(&req, None, &mut |l| lines.push(l.to_string())).unwrap();
        assert_eq!(code, 0);
        assert_eq!(lines, ["b", "c"]);
        let mut lines = Vec::new();
        let req = ExecReq::new(&["sh", "-c", "pwd; exit 4"]).cwd(Some("/tmp"));
        assert_eq!(h.exec_stream(&req, None, &mut |l| lines.push(l.to_string())).unwrap(), 4);
        assert_eq!(lines, ["/tmp"]);
    }

    #[test]
    fn stream_cancel() {
        let h = LocalHost::new();
        let cancel = AtomicBool::new(false);
        let t = Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(300));
                cancel.store(true, Ordering::SeqCst);
            });
            let _ = h.exec_stream(&ExecReq::new(&["sleep", "60"]), Some(&cancel), &mut |_| {});
        });
        assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    }

    #[test]
    fn activity_one_entry_and_quiet_none() {
        // krátké úspěšné příkazy se do historie nezapisují (activity::WSL_MIN), neúspěšné ano
        let h = LocalHost::new();
        let _ = h.run(Privilege::User, None, &["false", "wslcd-local-loud"]);
        assert_eq!(entries("false wslcd-local-loud"), 1);
        let _ = h.exec(&ExecReq::new(&["false", "wslcd-local-quiet"]).quiet());
        assert_eq!(entries("false wslcd-local-quiet"), 0);
        let _ = h.exec_stream(&ExecReq::new(&["false", "wslcd-local-stream"]), None, &mut |_| {});
        assert_eq!(entries("false wslcd-local-stream"), 1);
    }

    /// Živý test proti enginu z prostředí (`scripts/linux-test.ps1 live host::local`).
    #[test]
    #[ignore]
    fn local_engine_ping() {
        let h = LocalHost::new();
        let ep = h.engine_endpoint(&Config::default());
        assert!(crate::docker::Docker::new(&ep).ping(), "{ep:?} {:?}", h.note());
    }
}
