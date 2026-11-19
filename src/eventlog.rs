//! Historie problémů kontejnerů: pády, nedostatek paměti, selhávající healthcheck
//! a restartovací smyčky – i ty, které se ve Windows neoznámily (a proč).
//!
//! Ukládá se do `<datová složka>\container_events.jsonl` (JSON Lines, jeden záznam na řádek),
//! nejvýš `MAX` záznamů a `KEEP_SECS`. Zapisuje se celý soubor přes dočasný soubor
//! a přejmenování, s odkladem (`save_soon`), aby se dávka událostí zapsala jednou.
//! Opakování stejného problému u stejného kontejneru se sloučí do jednoho záznamu (`count`).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Nejvýš tolik záznamů.
pub const MAX: usize = 500;
/// Jak dlouho záznamy držet (s).
pub const KEEP_SECS: i64 = 30 * 86_400;
const FILE: &str = "container_events.jsonl";
/// Do detailu se uloží tolik posledních řádků logu…
pub const LOG_LINES: u32 = 40;
/// …a nejvýš tolik bajtů (konec logu).
pub const LOG_BYTES: usize = 6000;
/// Předem načtený detail (při `die`) se zahodí, když si ho do této doby nikdo nevyzvedne.
const PREFETCH_TTL: Duration = Duration::from_secs(60);
/// Odklad zápisu na disk.
const SAVE_DELAY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Crash,
    Oom,
    Health,
    Loop,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Crash, Kind::Oom, Kind::Health, Kind::Loop];

    /// Oznamuje se tento typ ve Windows?
    pub fn enabled(self, c: &crate::config::ContainerNotify) -> bool {
        match self {
            Kind::Crash => c.crash,
            Kind::Oom => c.oom,
            Kind::Health => c.health,
            Kind::Loop => c.restart_loop,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Crash => t!("Pád", "Crash"),
            Kind::Oom => t!("Nedostatek paměti", "Out of memory"),
            Kind::Health => "Healthcheck",
            Kind::Loop => t!("Restarty", "Restart loop"),
        }
    }
}

/// Proč se záznam ve Windows neoznámil.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quiet {
    /// ukončen signálem SIGTERM / SIGINT / SIGHUP – obvykle řádné vypnutí zvenku
    Signal,
    /// dočasný kontejner (`--rm`), engine ho hned po skončení smazal
    Ephemeral,
    /// kontejner Testcontainers (štítek `org.testcontainers`)
    Testcontainers,
    /// kontejner je v Nastavení ztlumený
    Muted,
    /// tento typ oznámení je v Nastavení vypnutý
    KindOff,
    /// stejný problém už byl nedávno oznámen
    Repeat,
    /// oznámení ve Windows jsou vypnutá
    Disabled,
}

impl Quiet {
    /// Běžná situace, ne chyba – nepočítá se do nepřečtených.
    pub fn benign(self) -> bool {
        matches!(self, Quiet::Signal | Quiet::Ephemeral | Quiet::Testcontainers)
    }

    pub fn label(self) -> &'static str {
        match self {
            Quiet::Signal => t!("ukončen signálem – obvykle řádné vypnutí", "terminated by a signal – usually a regular shutdown"),
            Quiet::Ephemeral => t!("dočasný kontejner (--rm)", "temporary container (--rm)"),
            Quiet::Testcontainers => "Testcontainers",
            Quiet::Muted => t!("kontejner je ztlumený", "container is muted"),
            Quiet::KindOff => t!("typ oznámení je vypnutý", "notification type is off"),
            Quiet::Repeat => t!("opakování – už oznámeno", "repeat – already notified"),
            Quiet::Disabled => t!("oznámení jsou vypnutá", "notifications are off"),
        }
    }
}

/// Stav kontejneru v okamžiku problému (z `inspect` a logu).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Detail {
    pub image: String,
    pub project: String,
    pub service: String,
    /// RFC3339
    pub started: String,
    pub finished: String,
    pub restart_policy: String,
    pub restart_count: i64,
    /// `State.Error` – chyba enginu (např. nenalezený příkaz při startu)
    pub error: String,
    pub oom_killed: bool,
    /// poslední výstup healthchecku
    pub health: String,
    /// posledních `LOG_LINES` řádků logu
    pub logs: String,
    /// kontejner už neexistoval (detail se nepodařilo načíst)
    pub gone: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    /// unixový čas prvního výskytu
    pub ts: i64,
    /// unixový čas posledního výskytu (při sloučení opakování)
    #[serde(default)]
    pub last: i64,
    #[serde(default = "one")]
    pub count: u32,
    pub id: String,
    pub name: String,
    pub kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts: Option<usize>,
    /// oznámeno ve Windows
    #[serde(default)]
    pub toast: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet: Option<Quiet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Detail>,
}

fn one() -> u32 {
    1
}

impl Entry {
    pub fn benign(&self) -> bool {
        self.quiet.is_some_and(|q| q.benign())
    }

    /// Krátký popis („kód 1 – obecná chyba aplikace“, „3× spuštěn během minuty“…).
    pub fn summary(&self) -> String {
        match self.kind {
            Kind::Crash => match self.code {
                Some(c) => {
                    let m = exit_meaning(c);
                    tf!("kód {c} – {m}", "code {c} – {m}")
                }
                None => String::new(),
            },
            Kind::Oom => t!("proces ukončen kvůli nedostatku paměti (OOM)", "process killed because it ran out of memory (OOM)").into(),
            Kind::Health => t!("kontrola stavu selhává", "the health check is failing").into(),
            Kind::Loop => match self.starts {
                Some(n) => tf!("{n}× spuštěn během minuty", "started {n}× within a minute"),
                None => String::new(),
            },
        }
    }
}

/// Význam kódu ukončení procesu v kontejneru.
pub fn exit_meaning(code: i64) -> String {
    if let Some(m) = known_meaning(code) {
        return m.to_string();
    }
    match code {
        0 => t!("úspěšné ukončení", "success").into(),
        3..=124 => t!("chyba aplikace – význam kódu určuje aplikace", "application error – the application defines the meaning").into(),
        129..=192 => match signal_name(code - 128) {
            Some(s) => tf!("{s} – ukončen signálem {}", "{s} – terminated by signal {}", code - 128),
            None => tf!("ukončen signálem {}", "terminated by signal {}", code - 128),
        },
        _ => t!("neobvyklý kód (mimo rozsah 0–255 nebo vyhrazený)", "unusual code (outside 0–255 or reserved)").into(),
    }
}

/// Název signálu Linuxu (x86_64).
fn signal_name(n: i64) -> Option<&'static str> {
    const NAMES: [&str; 31] = [
        "SIGHUP", "SIGINT", "SIGQUIT", "SIGILL", "SIGTRAP", "SIGABRT", "SIGBUS", "SIGFPE", "SIGKILL", "SIGUSR1", "SIGSEGV", "SIGUSR2",
        "SIGPIPE", "SIGALRM", "SIGTERM", "SIGSTKFLT", "SIGCHLD", "SIGCONT", "SIGSTOP", "SIGTSTP", "SIGTTIN", "SIGTTOU", "SIGURG",
        "SIGXCPU", "SIGXFSZ", "SIGVTALRM", "SIGPROF", "SIGWINCH", "SIGIO", "SIGPWR", "SIGSYS",
    ];
    usize::try_from(n - 1).ok().and_then(|i| NAMES.get(i)).copied()
}

/// Co kód obvykle znamená a kde hledat příčinu (detail události, oznámení).
pub fn exit_hint(code: i64) -> &'static str {
    match code {
        1 => t!("Aplikace skončila chybou – příčina bývá na konci logu.", "The application exited with an error – the cause is usually at the end of the log."),
        2 => t!("Chybné argumenty nebo chyba ve skriptu – zkontroluj command / entrypoint.", "Wrong arguments or a script error – check the command / entrypoint."),
        125 => t!(
            "Kontejner se vůbec nespustil – chybná volba (port, volume, síť) nebo chyba enginu.",
            "The container did not start at all – a wrong option (port, volume, network) or an engine error."
        ),
        126 => t!(
            "Soubor existuje, ale nejde spustit – chybí právo ke spuštění, jiná architektura, nebo skript s konci řádků CRLF.",
            "The file exists but cannot be executed – missing execute permission, another architecture, or a script with CRLF line endings."
        ),
        127 => t!(
            "Příkaz nebo interpret (sh, bash, python…) v image chybí – zkontroluj CMD / ENTRYPOINT a PATH.",
            "The command or interpreter (sh, bash, python…) is missing in the image – check CMD / ENTRYPOINT and PATH."
        ),
        129 => t!("Proces dostal SIGHUP – zavřený terminál nebo požadavek na znovunačtení.", "The process got SIGHUP – a closed terminal or a reload request."),
        130 => t!("Přerušeno zvenku (Ctrl+C / SIGINT) – obvykle nejde o chybu.", "Interrupted from outside (Ctrl+C / SIGINT) – usually not an error."),
        134 => t!(
            "Aplikace sama zavolala abort – často selhaná kontrola (assert) nebo chyba knihovny; viz log.",
            "The application called abort itself – often a failed assertion or a library error; see the log."
        ),
        137 => t!(
            "Proces zabil signál SIGKILL – docker kill, jiný proces, nebo stop po vypršení timeoutu. Při nedostatku paměti by šlo o záznam „Nedostatek paměti“.",
            "SIGKILL ended the process – docker kill, another process, or stop after the timeout expired. Running out of memory would show as an „Out of memory“ record."
        ),
        139 => t!(
            "Pád aplikace (neplatný přístup do paměti) – chyba v programu, nekompatibilní knihovna nebo architektura.",
            "Application crash (invalid memory access) – a bug, an incompatible library or architecture."
        ),
        141 => t!("Zápis do zavřené roury nebo spojení – druhá strana skončila dřív.", "Write to a closed pipe or connection – the other side ended first."),
        143 => t!("Řádné ukončení na žádost (docker stop, vypnutí systému) – obvykle nejde o chybu.", "A regular termination request (docker stop, system shutdown) – usually not an error."),
        255 => t!("Aplikace vrátila -1 nebo kód mimo rozsah 0–255; viz log.", "The application returned -1 or a code outside 0–255; see the log."),
        c if (3..=124).contains(&c) => t!("Kód si definuje aplikace – příčinu hledej v logu nebo v její dokumentaci.", "The application defines the code – look for the cause in the log or its documentation."),
        c if (129..=192).contains(&c) => t!("Proces ukončil signál – obvykle zvenku (jiný proces, systém).", "A signal ended the process – usually from outside (another process, the system)."),
        _ => t!("Neobvyklý kód – podívej se do logu.", "Unusual code – check the log."),
    }
}

fn known_meaning(code: i64) -> Option<&'static str> {
    Some(match code {
        1 => t!("obecná chyba aplikace", "general application error"),
        2 => t!("chybné použití (argumenty, skript)", "misuse (arguments, script)"),
        125 => t!("engine kontejner nespustil", "the engine failed to run the container"),
        126 => t!("příkaz nelze spustit (práva, formát)", "command cannot be executed (permissions, format)"),
        127 => t!("příkaz nenalezen", "command not found"),
        129 => t!("SIGHUP – zavřený terminál / reload", "SIGHUP – terminal closed / reload"),
        130 => t!("SIGINT – přerušeno (Ctrl+C)", "SIGINT – interrupted (Ctrl+C)"),
        131 => t!("SIGQUIT", "SIGQUIT"),
        134 => t!("SIGABRT – aplikace se sama ukončila (abort)", "SIGABRT – the application aborted"),
        135 => t!("SIGBUS – chyba přístupu do paměti", "SIGBUS – memory access error"),
        136 => t!("SIGFPE – aritmetická chyba", "SIGFPE – arithmetic error"),
        137 => t!("SIGKILL – násilně ukončen", "SIGKILL – killed"),
        139 => t!("SIGSEGV – neplatný přístup do paměti", "SIGSEGV – segmentation fault"),
        141 => t!("SIGPIPE – zápis do zavřené roury", "SIGPIPE – write to a closed pipe"),
        143 => t!("SIGTERM – požadavek na ukončení", "SIGTERM – termination request"),
        255 => t!("kód mimo rozsah (exit -1)", "exit status out of range (exit -1)"),
        _ => return None,
    })
}

/// Kódy, které znamenají ukončení signálem na žádost (ne chybu aplikace).
pub fn is_signal_exit(code: i64) -> bool {
    matches!(code, 129 | 130 | 143)
}

/// Nový záznam od workeru.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub code: Option<i64>,
    pub starts: Option<usize>,
    pub quiet: Option<Quiet>,
}

#[derive(Default)]
pub struct EventLog {
    pub entries: Vec<Entry>,
    next_seq: u64,
    /// nové problémy (ne běžné situace) od posledního zobrazení stránky Události
    pub unread: usize,
    /// detail načtený předem (při `die`, než engine kontejner smaže): id → (kdy, detail)
    prefetched: HashMap<String, (Instant, Detail)>,
    /// záznamy čekající na detail: id → seq
    awaiting: HashMap<String, u64>,
    path: Option<PathBuf>,
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn default_path() -> Option<PathBuf> {
    crate::config::data_dir().map(|d| d.join(FILE))
}

impl EventLog {
    /// Načte historii z datové složky aplikace.
    pub fn load() -> Self {
        Self::load_from(default_path())
    }

    pub fn load_from(path: Option<PathBuf>) -> Self {
        let mut entries: Vec<Entry> = path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
            .unwrap_or_default();
        entries.sort_by_key(|e| e.seq);
        let next_seq = entries.last().map(|e| e.seq + 1).unwrap_or(1);
        let mut log = Self { entries, next_seq, path, ..Default::default() };
        log.prune(now_unix());
        log
    }

    fn prune(&mut self, now: i64) {
        self.entries.retain(|e| now - e.last.max(e.ts) < KEEP_SECS);
        if self.entries.len() > MAX {
            let n = self.entries.len() - MAX;
            self.entries.drain(..n);
        }
    }

    /// Přidá záznam; opakování (stejný kontejner a typ, poslední výskyt před méně než
    /// `merge_within` s) jen zvýší počet. Vrací seq nového záznamu (None = sloučeno).
    pub fn add(&mut self, r: Record, toast: bool, now: i64, merge_within: i64) -> Option<u64> {
        if let Some(e) = self.entries.iter_mut().rev().find(|e| e.id == r.id && e.kind == r.kind) {
            if now - e.last.max(e.ts) < merge_within {
                e.count += 1;
                e.last = now;
                if r.code.is_some() {
                    e.code = r.code;
                }
                if r.starts.is_some() {
                    e.starts = r.starts;
                }
                if toast {
                    e.toast = true;
                    e.quiet = None;
                }
                return None;
            }
        }
        let seq = self.next_seq.max(1);
        self.next_seq = seq + 1;
        let e = Entry {
            seq,
            ts: now,
            last: now,
            count: 1,
            id: r.id,
            name: r.name,
            kind: r.kind,
            code: r.code,
            starts: r.starts,
            toast,
            quiet: r.quiet,
            detail: None,
        };
        if !e.benign() {
            self.unread += 1;
        }
        // detail už mohl dorazit (načtený předem při `die`)
        self.prefetched.retain(|_, (t, _)| t.elapsed() < PREFETCH_TTL);
        let id = e.id.clone();
        let mut e = e;
        if let Some((_, d)) = self.prefetched.remove(&id) {
            e.detail = Some(d);
        } else {
            self.awaiting.insert(id, seq);
        }
        self.entries.push(e);
        self.prune(now);
        Some(seq)
    }

    /// Detail kontejneru je načtený: patří čekajícímu záznamu, jinak se podrží pro záznam,
    /// který teprve vznikne.
    pub fn detail_ready(&mut self, id: &str, d: Detail) {
        if let Some(seq) = self.awaiting.remove(id) {
            if let Some(e) = self.entries.iter_mut().find(|e| e.seq == seq) {
                e.detail = Some(d);
                return;
            }
        }
        self.prefetched.insert(id.to_string(), (Instant::now(), d));
    }

    /// Čeká záznam na detail? (aby se pro něj detail načetl)
    pub fn awaits(&self, id: &str) -> bool {
        self.awaiting.contains_key(id)
    }

    #[cfg(test)]
    pub fn get(&self, seq: u64) -> Option<&Entry> {
        self.entries.iter().find(|e| e.seq == seq)
    }

    pub fn remove(&mut self, seq: u64) {
        self.entries.retain(|e| e.seq != seq);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.unread = 0;
    }

    fn to_jsonl(&self) -> String {
        let mut s = String::new();
        for e in &self.entries {
            if let Ok(l) = serde_json::to_string(e) {
                s.push_str(&l);
                s.push('\n');
            }
        }
        s
    }

    #[cfg(test)]
    pub fn save(&self) -> Result<(), String> {
        let Some(p) = self.path.as_deref() else { return Ok(()) };
        write_atomic(p, &self.to_jsonl())
    }
}

fn write_atomic(p: &Path, data: &str) -> Result<(), String> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = p.with_extension("jsonl.tmp");
    std::fs::write(&tmp, data).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, p).map_err(|e| e.to_string())
}

static SAVE_PENDING: AtomicBool = AtomicBool::new(false);

/// Uloží historii s odkladem; víc změn během `SAVE_DELAY` vede k jednomu zápisu.
pub fn save_soon(log: &Arc<Mutex<EventLog>>) {
    if SAVE_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let log = log.clone();
    std::thread::spawn(move || {
        std::thread::sleep(SAVE_DELAY);
        SAVE_PENDING.store(false, Ordering::SeqCst);
        // text se připraví pod zámkem, zápis až mimo něj
        let job = log.lock().ok().map(|g| (g.path.clone(), g.to_jsonl()));
        if let Some((Some(p), data)) = job {
            if let Err(e) = write_atomic(&p, &data) {
                eprintln!("container events: {e}");
            }
        }
    });
}

/// Zkrátí log na posledních `LOG_BYTES` bajtů (na hranici řádku).
pub fn tail_text(s: &str) -> String {
    if s.len() <= LOG_BYTES {
        return s.to_string();
    }
    let mut start = s.len() - LOG_BYTES;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    let cut = &s[start..];
    match cut.find('\n') {
        Some(i) if i + 1 < cut.len() => cut[i + 1..].to_string(),
        _ => cut.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, kind: Kind, quiet: Option<Quiet>) -> Record {
        Record { id: id.into(), name: format!("c-{id}"), kind, code: Some(1), starts: None, quiet }
    }

    #[test]
    fn every_exit_code_has_a_description() {
        assert_eq!(exit_meaning(137), "SIGKILL – násilně ukončen");
        assert!(exit_hint(137).contains("docker kill"));
        // neznámý aplikační kód, neznámý signál, mimo rozsah
        assert!(exit_meaning(42).starts_with("chyba aplikace"));
        assert!(exit_hint(42).contains("logu"));
        assert_eq!(exit_meaning(138), "SIGUSR1 – ukončen signálem 10");
        assert_eq!(exit_meaning(128 + 64), "ukončen signálem 64");
        assert!(exit_meaning(-1).starts_with("neobvyklý"));
        for c in [-5, 0, 1, 2, 3, 124, 125, 126, 127, 128, 129, 143, 150, 192, 200, 255, 300] {
            assert!(!exit_meaning(c).is_empty() && !exit_hint(c).is_empty(), "{c}");
        }
        let e = Entry {
            seq: 1, ts: 0, last: 0, count: 1, id: "a".into(), name: "a".into(), kind: Kind::Crash,
            code: Some(42), starts: None, toast: true, quiet: None, detail: None,
        };
        assert_eq!(e.summary(), "kód 42 – chyba aplikace – význam kódu určuje aplikace");
    }

    #[test]
    fn merge_repeats_and_unread() {
        let mut l = EventLog::default();
        assert_eq!(l.add(rec("a", Kind::Crash, None), true, 1000, 600), Some(1));
        // opakování do 10 min se sloučí
        assert_eq!(l.add(rec("a", Kind::Crash, Some(Quiet::Repeat)), false, 1100, 600), None);
        assert_eq!(l.entries[0].count, 2);
        assert_eq!(l.entries[0].last, 1100);
        assert!(l.entries[0].toast);
        // jiný typ i jiný kontejner = nový záznam; běžná situace se nepočítá do nepřečtených
        assert_eq!(l.add(rec("a", Kind::Oom, None), true, 1100, 600), Some(2));
        assert_eq!(l.add(rec("b", Kind::Crash, Some(Quiet::Signal)), false, 1100, 600), Some(3));
        assert_eq!(l.unread, 2);
        // po intervalu nový záznam
        assert_eq!(l.add(rec("a", Kind::Crash, None), true, 1800, 600), Some(4));
    }

    #[test]
    fn detail_before_and_after_record() {
        let mut l = EventLog::default();
        let d = Detail { image: "nginx".into(), ..Default::default() };
        // předem načtený (při die)
        l.detail_ready("a", d.clone());
        let s = l.add(rec("a", Kind::Crash, None), true, 10, 600).unwrap();
        assert_eq!(l.get(s).unwrap().detail.as_ref(), Some(&d));
        // načtený až po vzniku záznamu
        let s = l.add(rec("b", Kind::Crash, None), true, 10, 600).unwrap();
        assert!(l.awaits("b"));
        l.detail_ready("b", d.clone());
        assert_eq!(l.get(s).unwrap().detail.as_ref(), Some(&d));
        assert!(!l.awaits("b"));
    }

    #[test]
    fn roundtrip_and_prune() {
        let dir = std::env::temp_dir().join(format!("wls-evlog-{}", std::process::id()));
        let p = dir.join(FILE);
        let now = now_unix();
        let mut l = EventLog::load_from(Some(p.clone()));
        l.add(rec("old", Kind::Crash, None), true, now - KEEP_SECS - 10, 600);
        l.add(rec("a", Kind::Health, Some(Quiet::Muted)), false, now, 600);
        l.save().unwrap();
        // poškozený řádek se přeskočí
        std::fs::write(&p, format!("{}{{nesmysl\n", std::fs::read_to_string(&p).unwrap())).unwrap();
        let l2 = EventLog::load_from(Some(p.clone()));
        assert_eq!(l2.entries.len(), 1);
        assert_eq!(l2.entries[0].quiet, Some(Quiet::Muted));
        assert_eq!(l2.next_seq, 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn tail_keeps_end_on_line_boundary() {
        let s: String = (0..2000).map(|i| format!("řádek {i}\n")).collect();
        let t = tail_text(&s);
        assert!(t.len() <= LOG_BYTES);
        assert!(t.starts_with("řádek "));
        assert!(t.ends_with("řádek 1999\n"));
    }
}
