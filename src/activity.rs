//! Přehled toho, co aplikace dělá na pozadí: akce (instalace, nastavení API, compose, build…)
//! a příkazy spuštěné ve WSL. UI z toho kreslí okno „Na pozadí“.
//!
//! Krátké příkazy WSL (obnovování stavu, statistiky) se do historie nezapisují – zahltily by ji;
//! dokud běží, v seznamu běžících jsou.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// akce aplikace (příkaz workeru)
    Action,
    /// příkaz spuštěný ve WSL (`wsl.exe -d …`)
    Wsl,
}

/// Právě běžící činnost.
#[derive(Debug, Clone)]
pub struct Running {
    pub id: u64,
    pub kind: Kind,
    pub title: String,
    pub detail: String,
    pub started: Instant,
}

/// Dokončená činnost (historie).
#[derive(Debug, Clone)]
pub struct Done {
    pub kind: Kind,
    pub title: String,
    pub detail: String,
    pub ended: Instant,
    pub took: Duration,
    pub ok: bool,
}

struct State {
    next: u64,
    running: Vec<Running>,
    done: VecDeque<Done>,
}

static STATE: Mutex<State> = Mutex::new(State { next: 1, running: Vec::new(), done: VecDeque::new() });

/// Kolik dokončených činností si pamatovat.
const KEEP: usize = 60;
/// Příkazy WSL kratší než tohle se do historie nezapisují.
pub const WSL_MIN: Duration = Duration::from_millis(1000);

/// Běžící činnost; konec se zapíše, když se zahodí (`set_ok(false)` = skončila chybou).
pub struct Guard {
    id: u64,
    ok: bool,
}

impl Guard {
    pub fn set_ok(&mut self, ok: bool) {
        self.ok = ok;
    }
}

pub fn start(kind: Kind, title: impl Into<String>, detail: impl Into<String>) -> Guard {
    let Ok(mut s) = STATE.lock() else { return Guard { id: 0, ok: true } };
    let id = s.next;
    s.next += 1;
    s.running.push(Running { id, kind, title: title.into(), detail: detail.into(), started: Instant::now() });
    Guard { id, ok: true }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let Ok(mut s) = STATE.lock() else { return };
        let Some(i) = s.running.iter().position(|r| r.id == self.id) else { return };
        let r = s.running.remove(i);
        let took = r.started.elapsed();
        if r.kind == Kind::Action || took >= WSL_MIN || !self.ok {
            s.done.push_front(Done { kind: r.kind, title: r.title, detail: r.detail, ended: Instant::now(), took, ok: self.ok });
            s.done.truncate(KEEP);
        }
    }
}

/// Běžící (nejstarší první) a dokončené (nejnovější první).
pub fn snapshot() -> (Vec<Running>, Vec<Done>) {
    match STATE.lock() {
        Ok(s) => (s.running.clone(), s.done.iter().cloned().collect()),
        Err(_) => (Vec::new(), Vec::new()),
    }
}

/// Počet činností, které běží déle než `min` (krátké obnovování stavu se nepočítá).
pub fn running_longer_than(min: Duration) -> usize {
    STATE.lock().map(|s| s.running.iter().filter(|r| r.started.elapsed() >= min).count()).unwrap_or(0)
}

pub fn clear_done() {
    if let Ok(mut s) = STATE.lock() {
        s.done.clear();
    }
}

/// Krátký popis příkazu WSL: u `sh -c <skript>` první smysluplný řádek skriptu
/// (bez komentářů a definic funkcí), jinak argumenty za sebou; nejvýš `max` znaků.
pub fn describe_cmd(args: &[&str], max: usize) -> String {
    let text = match args {
        [sh, c, script, ..] if (*sh == "sh" || *sh == "bash") && *c == "-c" => {
            // definice funkcí (pkg_install() { … }) přeskočit – zajímá nás, co skript dělá
            let mut in_func = false;
            let mut first = None;
            for l in script.lines() {
                let t = l.trim();
                if in_func {
                    in_func = l != "}";
                    continue;
                }
                if t.ends_with("() {") {
                    in_func = true;
                    continue;
                }
                if !t.is_empty() && !t.starts_with('#') && t != "set -e" {
                    first = Some(t);
                    break;
                }
            }
            first.unwrap_or("sh -c …").to_string()
        }
        [env, rest @ ..] if *env == "env" => {
            // env A=1 B=2 sh -c … – proměnné přeskočit
            let i = rest.iter().position(|a| !a.contains('=')).unwrap_or(rest.len());
            return describe_cmd(&rest[i..], max);
        }
        _ => args.join(" "),
    };
    if text.chars().count() > max {
        let cut: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_records_and_filters_short_wsl() {
        {
            let _a = start(Kind::Action, "Test akce", "x");
            let (run, _) = snapshot();
            assert!(run.iter().any(|r| r.title == "Test akce"));
        }
        {
            let _w = start(Kind::Wsl, "test-kratky-wsl", "true");
        }
        {
            let mut w = start(Kind::Wsl, "test-chybny-wsl", "false");
            w.set_ok(false);
        }
        let (run, done) = snapshot();
        assert!(!run.iter().any(|r| r.title == "Test akce"));
        assert!(done.iter().any(|d| d.title == "Test akce" && d.ok));
        assert!(!done.iter().any(|d| d.title == "test-kratky-wsl"), "krátký úspěšný příkaz WSL se nezapisuje");
        assert!(done.iter().any(|d| d.title == "test-chybny-wsl" && !d.ok), "chyba se zapíše vždy");
    }

    #[test]
    fn describe() {
        assert_eq!(describe_cmd(&["sh", "-c", "set -e\n# komentář\npkg_install() {\n  apt-get install x\n}\nsystemctl restart docker\n"], 40), "systemctl restart docker");
        assert_eq!(describe_cmd(&["env", "PORT=2376", "TLS=1", "sh", "-c", "systemctl restart docker"], 40), "systemctl restart docker");
        assert_eq!(describe_cmd(&["podman", "ps", "-a"], 40), "podman ps -a");
        assert_eq!(describe_cmd(&["echo", "abcdefghij"], 8), "echo ab…");
    }
}
