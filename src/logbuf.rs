//! Model prohlížeče logů (bez egui): řádky s časem, omezený buffer, slučování streamů více
//! kontejnerů podle času (reorder buffer), hledání shod, formát pro uložení a vlákna streamů.

use crate::docker::{Container, Docker, LogOpts, StreamCloser};
use std::borrow::Cow;
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Nejvýš řádků v bufferu – starší se zahazují.
pub const MAX_LINES: usize = 50_000;
/// Jak dlouho čekají řádky sloučených streamů na případné starší řádky z jiných kontejnerů.
pub const REORDER: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    /// čas řádku (unixové nanosekundy, UTC); None = engine ho neposlal / nejde přečíst
    pub ts: Option<i64>,
    /// zdroj (index kontejneru / služby)
    pub src: u16,
    pub text: String,
}

/// Řádky logu s omezenou délkou. Řádky mají absolutní pořadová čísla (`first_abs` + index),
/// takže cache zobrazení umí po přidání řádků jen doplnit konec a po zahození začátku ho
/// odříznout. `epoch` se mění při změnách, které posunou čísla (vymazání, vložení doprostřed).
pub struct LogBuf {
    lines: VecDeque<LogLine>,
    cap: usize,
    first_abs: u64,
    epoch: u64,
    /// počet zahozených nejstarších řádků
    pub dropped: u64,
    /// nedokončený řádek pro každý zdroj (TTY stream posílá surové bloky)
    partial: Vec<String>,
    /// Some = slučování více streamů: řádky čekají v `pending` a vydávají se podle času
    reorder: Option<Duration>,
    pending: Vec<(Instant, LogLine)>,
}

impl LogBuf {
    pub fn new(cap: usize, reorder: Option<Duration>) -> Self {
        Self { lines: VecDeque::new(), cap: cap.max(1), first_abs: 0, epoch: 0, dropped: 0, partial: Vec::new(), reorder, pending: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    #[cfg(test)]
    pub fn get(&self, i: usize) -> Option<&LogLine> {
        self.lines.get(i)
    }

    /// Řádek podle absolutního čísla.
    pub fn get_abs(&self, abs: u64) -> Option<&LogLine> {
        abs.checked_sub(self.first_abs).and_then(|i| self.lines.get(i as usize))
    }

    pub fn iter(&self) -> impl Iterator<Item = &LogLine> {
        self.lines.iter()
    }

    pub fn first_abs(&self) -> u64 {
        self.first_abs
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn clear(&mut self) {
        self.first_abs += self.lines.len() as u64;
        self.lines.clear();
        self.pending.clear();
        self.partial.clear();
        self.dropped = 0;
        self.epoch += 1;
    }

    /// Přidá text ze streamu. `complete` = blok končí na hranici řádku (rámec bez TTY);
    /// jinak se text za posledním `\n` drží do dalšího bloku.
    pub fn push_chunk(&mut self, src: u16, chunk: &str, complete: bool, now: Instant) {
        let i = src as usize;
        if self.partial.len() <= i {
            self.partial.resize(i + 1, String::new());
        }
        let mut text = std::mem::take(&mut self.partial[i]);
        text.push_str(chunk);
        let mut parts: Vec<&str> = text.split('\n').collect();
        let rest = parts.pop().unwrap_or("");
        let rest = if complete {
            if !rest.is_empty() {
                parts.push(rest);
            }
            ""
        } else {
            rest
        };
        let lines: Vec<LogLine> = parts.iter().map(|l| parse_line(src, l)).collect();
        let rest = rest.to_string();
        self.partial[i] = rest;
        for l in lines {
            self.accept(l, now);
        }
        self.flush(now);
    }

    /// Vlastní poznámka (chyba streamu apod.) – bez času, na konec.
    pub fn push_note(&mut self, src: u16, text: &str, now: Instant) {
        self.accept(LogLine { ts: None, src, text: text.to_string() }, now);
        self.flush(now);
    }

    fn accept(&mut self, l: LogLine, now: Instant) {
        if self.reorder.is_some() {
            self.pending.push((now, l));
        } else {
            self.lines.push_back(l);
            self.trim();
        }
    }

    /// Vydá čekající řádky starší než okno reorder bufferu, seřazené podle času.
    pub fn flush(&mut self, now: Instant) {
        let Some(win) = self.reorder else { return };
        if self.pending.is_empty() {
            return;
        }
        let (mut ready, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending).into_iter().partition(|(t, _)| now.saturating_duration_since(*t) >= win);
        self.pending = keep;
        // stabilní řazení: stejný čas zachová pořadí příchodu, řádky bez času na konec
        ready.sort_by_key(|(_, l)| l.ts.unwrap_or(i64::MAX));
        for (_, l) in ready {
            self.insert_sorted(l);
        }
    }

    /// Vydá všechny čekající řádky (konec streamů, testy).
    #[cfg(test)]
    pub fn flush_all(&mut self) {
        let far = Instant::now() + Duration::from_secs(3600);
        self.flush(far);
    }

    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Vloží řádek podle času (řádek, který dorazil po okně reorder bufferu, se zařadí zpětně).
    fn insert_sorted(&mut self, l: LogLine) {
        let mut pos = self.lines.len();
        if let Some(ts) = l.ts {
            while pos > 0 {
                match self.lines[pos - 1].ts {
                    Some(t) if t > ts => pos -= 1,
                    _ => break,
                }
            }
        }
        if pos < self.lines.len() {
            self.lines.insert(pos, l);
            self.epoch += 1;
        } else {
            self.lines.push_back(l);
        }
        self.trim();
    }

    fn trim(&mut self) {
        while self.lines.len() > self.cap {
            self.lines.pop_front();
            self.first_abs += 1;
            self.dropped += 1;
        }
    }
}

/// Řádek ze streamu: bez `\r`, ANSI escape sekvencí a s odděleným časem.
fn parse_line(src: u16, raw: &str) -> LogLine {
    let raw = raw.strip_suffix('\r').unwrap_or(raw);
    let (ts, text) = split_ts(raw);
    LogLine { ts, src, text: strip_ansi(text).into_owned() }
}

/// Oddělí čas RFC3339 na začátku řádku (`timestamps=1`): `(čas v ns, zbytek řádku)`.
pub fn split_ts(line: &str) -> (Option<i64>, &str) {
    let (head, rest) = match line.find(' ') {
        Some(i) => (&line[..i], &line[i + 1..]),
        None => (line, ""),
    };
    match parse_rfc3339(head) {
        Some(ts) => (Some(ts), rest),
        None => (None, line),
    }
}

/// `2026-11-18T10:20:30.123456789Z` / `…+01:00` -> unixové nanosekundy.
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't') || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        if t.bytes().all(|c| c.is_ascii_digit()) {
            t.parse().ok()
        } else {
            None
        }
    };
    let (y, mo, d, h, mi, se) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut i = 19;
    let mut nanos = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            if i - start < 9 {
                nanos = nanos * 10 + (b[i] - b'0') as i64;
            }
            i += 1;
        }
        let digits = i - start;
        if digits == 0 {
            return None;
        }
        for _ in digits..9 {
            nanos *= 10;
        }
    }
    let off = match b.get(i) {
        Some(b'Z' | b'z') if i + 1 == b.len() => 0,
        Some(c @ (b'+' | b'-')) if b.len() == i + 6 && b[i + 3] == b':' => {
            let sign = if *c == b'-' { -1 } else { 1 };
            sign * (num(i + 1..i + 3)? * 3600 + num(i + 4..i + 6)? * 60)
        }
        _ => return None,
    };
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se - off;
    secs.checked_mul(1_000_000_000)?.checked_add(nanos)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

/// Rozložený místní čas: (rok, měsíc, den, hodina, minuta, sekunda, nanosekundy).
fn civil(ns: i64, off: i32) -> (i64, i64, i64, i64, i64, i64, i64) {
    let secs = ns.div_euclid(1_000_000_000) + off as i64;
    let nanos = ns.rem_euclid(1_000_000_000);
    let (y, mo, d) = civil_from_days(secs.div_euclid(86_400));
    let sod = secs.rem_euclid(86_400);
    (y, mo, d, sod / 3600, sod / 60 % 60, sod % 60, nanos)
}

/// Krátký čas pro sloupec: `HH:MM:SS.mmm` (posun `off` v sekundách).
pub fn fmt_short(ns: i64, off: i32) -> String {
    let (_, _, _, h, mi, s, n) = civil(ns, off);
    format!("{h:02}:{mi:02}:{s:02}.{:03}", n / 1_000_000)
}

/// Čas pro uložený soubor: `YYYY-MM-DD HH:MM:SS.mmm`.
pub fn fmt_medium(ns: i64, off: i32) -> String {
    let (y, mo, d, h, mi, s, n) = civil(ns, off);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{:03}", n / 1_000_000)
}

/// Úplný čas pro tooltip: `YYYY-MM-DD HH:MM:SS.nnnnnnnnn +HH:MM`.
pub fn fmt_full(ns: i64, off: i32) -> String {
    let (y, mo, d, h, mi, s, n) = civil(ns, off);
    let sign = if off < 0 { '-' } else { '+' };
    let oa = off.abs() / 60;
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}.{n:09} {sign}{:02}:{:02}", oa / 60, oa % 60)
}

/// Posun místního času od UTC (sekundy) pro daný unixový čas; cache po hodinách.
pub fn local_offset(secs: i64) -> i32 {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static CACHE: RefCell<HashMap<i64, i32>> = RefCell::new(HashMap::new());
    }
    let hour = secs.div_euclid(3600);
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if let Some(o) = c.get(&hour) {
            return *o;
        }
        if c.len() > 4096 {
            c.clear();
        }
        let o = tz_offset(hour * 3600);
        c.insert(hour, o);
        o
    })
}

#[cfg(windows)]
fn tz_offset(ts: i64) -> i32 {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::Time::{GetDynamicTimeZoneInformation, SystemTimeToTzSpecificLocalTimeEx, DYNAMIC_TIME_ZONE_INFORMATION, TIME_ZONE_ID_INVALID};
    let (y, m, d) = civil_from_days(ts.div_euclid(86_400));
    let s = ts.rem_euclid(86_400);
    let utc = SYSTEMTIME {
        wYear: y as u16,
        wMonth: m as u16,
        wDayOfWeek: 0,
        wDay: d as u16,
        wHour: (s / 3600) as u16,
        wMinute: (s / 60 % 60) as u16,
        wSecond: (s % 60) as u16,
        wMilliseconds: 0,
    };
    // SAFETY: struktury jsou plně inicializované a předané ukazatele platí po dobu volání
    unsafe {
        let mut tz: DYNAMIC_TIME_ZONE_INFORMATION = std::mem::zeroed();
        if GetDynamicTimeZoneInformation(&mut tz) == TIME_ZONE_ID_INVALID {
            return 0;
        }
        let mut loc: SYSTEMTIME = std::mem::zeroed();
        if SystemTimeToTzSpecificLocalTimeEx(&tz, &utc, &mut loc) == 0 {
            return 0;
        }
        let lt = days_from_civil(loc.wYear as i64, loc.wMonth as i64, loc.wDay as i64) * 86_400
            + loc.wHour as i64 * 3600
            + loc.wMinute as i64 * 60
            + loc.wSecond as i64;
        (lt - ts) as i32
    }
}

#[cfg(not(windows))]
fn tz_offset(_ts: i64) -> i32 {
    0
}

/// Teď v unixových nanosekundách.
pub fn now_ns() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

/// Odstraní ANSI escape sekvence (barvy apod.).
pub fn strip_ansi(s: &str) -> Cow<'_, str> {
    if !s.contains('\x1b') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.next() {
            // CSI: parametry a ukončovací znak 0x40–0x7E
            Some('[') => {
                for c in it.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: do BEL nebo ESC \
            Some(']') => {
                while let Some(c) = it.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        it.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    Cow::Owned(out)
}

// ---------------------------------------------------------------- hledání

/// Všechny (nepřekrývající se) výskyty `needle` v `text` jako bajtové rozsahy v `text`
/// (vždy na hranicích znaků). Bez rozlišení velikosti písmen se porovnávají malá písmena.
pub fn find_matches(text: &str, needle: &str, case_sensitive: bool) -> Vec<Range<usize>> {
    if needle.is_empty() || text.is_empty() {
        return Vec::new();
    }
    if case_sensitive {
        return text.match_indices(needle).map(|(i, m)| i..i + m.len()).collect();
    }
    if needle.is_ascii() {
        // bajty shody odpovídají ASCII bajtům hledaného textu -> hranice znaků sedí
        let (t, n) = (text.as_bytes(), needle.as_bytes());
        let mut out = Vec::new();
        let mut i = 0;
        while i + n.len() <= t.len() {
            if t[i..i + n.len()].eq_ignore_ascii_case(n) {
                out.push(i..i + n.len());
                i += n.len();
            } else {
                i += 1;
            }
        }
        return out;
    }
    // obecně: malá písmena po znacích + mapa bajtů zpět na původní znaky
    let mut folded = String::with_capacity(text.len());
    let mut map: Vec<(usize, usize)> = Vec::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        let end = i + c.len_utf8();
        for lc in c.to_lowercase() {
            let before = folded.len();
            folded.push(lc);
            map.extend(std::iter::repeat_n((i, end), folded.len() - before));
        }
    }
    let n: String = needle.chars().flat_map(char::to_lowercase).collect();
    let mut out: Vec<Range<usize>> = Vec::new();
    for (fi, m) in folded.match_indices(&n) {
        let (start, end) = (map[fi].0, map[fi + m.len() - 1].1);
        match out.last_mut() {
            Some(prev) if start < prev.end => prev.end = prev.end.max(end),
            _ => out.push(start..end),
        }
    }
    out
}

fn contains_ci(text: &str, needle: &str) -> bool {
    needle.is_empty() || !find_matches(text, needle, false).is_empty()
}

/// Zobrazený řádek: absolutní číslo řádku v bufferu a shody hledání v jeho textu.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub abs: u64,
    pub hits: Vec<Range<usize>>,
}

/// Cache zobrazených řádků (filtr řádků, skryté služby) a shod hledání. Při přidání řádků
/// zpracuje jen nové, při zahození nejstarších odřízne začátek; vše znovu jen po změně
/// dotazu nebo `epoch` bufferu.
#[derive(Default)]
pub struct ViewCache {
    key: Option<(u64, String, Vec<bool>, String, bool)>,
    next_abs: u64,
    pub rows: VecDeque<Row>,
    pub hit_count: usize,
    /// nejdelší zobrazený řádek (znaky) – šířka obsahu pro vodorovné posouvání
    pub max_chars: usize,
}

impl ViewCache {
    pub fn update(&mut self, buf: &LogBuf, filter: &str, hidden: &[bool], query: &str, case: bool) {
        let same = matches!(&self.key, Some((e, f, h, q, c)) if *e == buf.epoch() && f == filter && h == hidden && q == query && *c == case);
        if !same {
            self.key = Some((buf.epoch(), filter.to_string(), hidden.to_vec(), query.to_string(), case));
            self.rows.clear();
            self.hit_count = 0;
            self.max_chars = 0;
            self.next_abs = buf.first_abs();
        }
        while self.rows.front().is_some_and(|r| r.abs < buf.first_abs()) {
            if let Some(r) = self.rows.pop_front() {
                self.hit_count -= r.hits.len();
            }
        }
        let end = buf.first_abs() + buf.len() as u64;
        for abs in self.next_abs.max(buf.first_abs())..end {
            let Some(l) = buf.get_abs(abs) else { continue };
            if hidden.get(l.src as usize).copied().unwrap_or(false) || !contains_ci(&l.text, filter) {
                continue;
            }
            let hits = find_matches(&l.text, query, case);
            self.hit_count += hits.len();
            self.max_chars = self.max_chars.max(l.text.len());
            self.rows.push_back(Row { abs, hits });
        }
        self.next_abs = end;
    }

    /// Pořadí shody (abs. řádek, index shody v řádku) mezi všemi shodami.
    pub fn hit_index(&self, cur: (u64, usize)) -> Option<usize> {
        let mut n = 0;
        for r in &self.rows {
            if r.abs == cur.0 {
                return (cur.1 < r.hits.len()).then_some(n + cur.1);
            }
            n += r.hits.len();
        }
        None
    }

    /// n-tá shoda: (index zobrazeného řádku, abs. řádek, index shody v řádku).
    pub fn nth_hit(&self, mut n: usize) -> Option<(usize, u64, usize)> {
        for (i, r) in self.rows.iter().enumerate() {
            if n < r.hits.len() {
                return Some((i, r.abs, n));
            }
            n -= r.hits.len();
        }
        None
    }

    /// Další (`dir` = 1) / předchozí (-1) shoda od aktuální, dokola.
    pub fn step(&self, cur: Option<(u64, usize)>, dir: i32) -> Option<(usize, u64, usize)> {
        let total = self.hit_count;
        if total == 0 {
            return None;
        }
        let idx = match cur.and_then(|c| self.hit_index(c)) {
            Some(i) => (i as i64 + dir as i64).rem_euclid(total as i64) as usize,
            None if dir < 0 => total - 1,
            None => 0,
        };
        self.nth_hit(idx)
    }
}

// ---------------------------------------------------------------- uložení

/// Text pro uložení / kopírování: řádek = [čas] [služba |] text.
/// `prefix` = názvy zdrojů (log stacku), `tz` = posun místního času pro unixový čas.
pub fn format_lines<'a>(lines: impl Iterator<Item = &'a LogLine>, prefix: Option<&[String]>, with_ts: bool, tz: impl Fn(i64) -> i32) -> String {
    let w = prefix.map(|p| p.iter().map(|s| s.chars().count()).max().unwrap_or(0)).unwrap_or(0);
    let mut out = String::new();
    for l in lines {
        if with_ts {
            match l.ts {
                Some(ns) => out.push_str(&fmt_medium(ns, tz(ns.div_euclid(1_000_000_000)))),
                None => out.push_str(&" ".repeat(23)),
            }
            out.push(' ');
        }
        if let Some(p) = prefix {
            let name = p.get(l.src as usize).map(String::as_str).unwrap_or("?");
            out.push_str(&format!("{name:<w$} | "));
        }
        out.push_str(&l.text);
        out.push('\n');
    }
    out
}

/// Výchozí název souboru: `<název>-<YYYYMMDD-HHMM>.log` (znaky nepovolené ve Windows -> `_`).
pub fn save_file_name(name: &str, ns: i64, off: i32) -> String {
    let (y, mo, d, h, mi, _, _) = civil(ns, off);
    let safe: String = name.chars().map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() { '_' } else { c }).collect();
    format!("{safe}-{y:04}{mo:02}{d:02}-{h:02}{mi:02}.log")
}

// ---------------------------------------------------------------- streamy

/// Kontejnery compose projektu pro sloučený log: (id, popisek). Popisek je název služby,
/// u více replik `služba-N`. Seřazeno podle popisku.
pub fn stack_targets(containers: &[Container], project: &str) -> Vec<(String, String)> {
    // stejně jako stránka Compose: podman-compose nastavuje i io.podman.compose.*
    let cs: Vec<&Container> = containers.iter().filter(|c| c.project().or_else(|| c.label("io.podman.compose.project")) == Some(project)).collect();
    let svc = |c: &Container| {
        c.label("com.docker.compose.service").or_else(|| c.label("io.podman.compose.service")).map(String::from).unwrap_or_else(|| c.name())
    };
    let mut out: Vec<(String, String)> = cs
        .iter()
        .map(|c| {
            let s = svc(c);
            let replicas = cs.iter().filter(|o| svc(o) == s).count();
            let label = if replicas > 1 {
                let n = c.label("com.docker.compose.container-number").map(String::from).unwrap_or_else(|| crate::docker::short_id(&c.id).to_string());
                format!("{s}-{n}")
            } else {
                s
            };
            (c.id.clone(), label)
        })
        .collect();
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// Vlákna streamů logů (jedno na kontejner) zapisující do společného bufferu.
/// Při `stop()` / dropu se nastaví příznak (kontroluje ho `on_chunk`) a zavřou se spojení,
/// takže vlákna skončí i u kontejnerů, které zrovna nic nevypisují.
pub struct Streams {
    stop: Arc<AtomicBool>,
    closers: Vec<StreamCloser>,
    /// vlákna se nečekají (skončí sama po zavření spojení); join jen v testech
    #[cfg_attr(not(test), allow(dead_code))]
    handles: Vec<std::thread::JoinHandle<()>>,
}

impl Streams {
    /// `ids[i]` se zapisuje jako zdroj `i`.
    pub fn start(port: u16, buf: Arc<Mutex<LogBuf>>, ids: &[String], tail: u32) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let mut closers = Vec::new();
        let mut handles = Vec::new();
        for (src, id) in ids.iter().enumerate() {
            let src = src as u16;
            let opts = LogOpts::follow(tail);
            closers.push(opts.closer.clone());
            let (buf, stop, id) = (buf.clone(), stop.clone(), id.clone());
            let h = std::thread::Builder::new().name(format!("logs-{src}")).spawn(move || {
                let d = Docker::new(port);
                let res = d.stream_logs(&id, &opts, |chunk, complete| {
                    if stop.load(Ordering::Relaxed) {
                        return false;
                    }
                    if let Ok(mut g) = buf.lock() {
                        g.push_chunk(src, chunk, complete, Instant::now());
                    }
                    true
                });
                if let Err(e) = res {
                    if !stop.load(Ordering::Relaxed) {
                        if let Ok(mut g) = buf.lock() {
                            g.push_note(src, &tf!("[chyba streamu logů: {e}]", "[log stream error: {e}]"), Instant::now());
                        }
                    }
                }
            });
            if let Ok(h) = h {
                handles.push(h);
            }
        }
        Self { stop, closers, handles }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        for c in &self.closers {
            c.close();
        }
    }

    /// Zastaví streamy a počká, až vlákna skončí (nejvýš `timeout`); true = všechna skončila.
    #[cfg(test)]
    pub fn stop_and_join(mut self, timeout: Duration) -> bool {
        self.stop();
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            if self.handles.iter().all(|h| h.is_finished()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let done = self.handles.iter().all(|h| h.is_finished());
        for h in std::mem::take(&mut self.handles) {
            if h.is_finished() {
                let _ = h.join();
            }
        }
        done
    }
}

impl Drop for Streams {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: &str = "2026-11-18T10:20:30.123456789Z";

    #[test]
    fn rfc3339_parsing() {
        // 2026-11-18T10:20:30Z
        let base = 1_794_997_230i64 * 1_000_000_000;
        assert_eq!(parse_rfc3339("2026-11-18T10:20:30Z"), Some(base));
        assert_eq!(parse_rfc3339(T0), Some(base + 123_456_789));
        assert_eq!(parse_rfc3339("2026-11-18T10:20:30.5Z"), Some(base + 500_000_000));
        // více než 9 číslic se ořízne
        assert_eq!(parse_rfc3339("2026-11-18T10:20:30.1234567891Z"), Some(base + 123_456_789));
        // posun zóny (Podman posílá místní čas serveru)
        assert_eq!(parse_rfc3339("2026-11-18T11:20:30.123456789+01:00"), Some(base + 123_456_789));
        assert_eq!(parse_rfc3339("2026-11-18T05:20:30-05:00"), Some(base));
        for bad in ["", "hello world", "2026-11-18 10:20:30Z", "2026-11-18T10:20:30", "2026-13-18T10:20:30Z", "2026-11-18T10:20:30.Z", "2026-11-18T10:20:30Zx", "2026-11-18T1a:20:30Z"] {
            assert_eq!(parse_rfc3339(bad), None, "{bad}");
        }
        assert_eq!(split_ts(&format!("{T0} hello world")), (Some(base + 123_456_789), "hello world"));
        assert_eq!(split_ts(&format!("{T0} ")), (Some(base + 123_456_789), ""));
        assert_eq!(split_ts(T0), (Some(base + 123_456_789), ""));
        assert_eq!(split_ts("no timestamp here"), (None, "no timestamp here"));
    }

    #[test]
    fn time_formatting() {
        let ns = parse_rfc3339(T0).unwrap();
        assert_eq!(fmt_short(ns, 0), "10:20:30.123");
        assert_eq!(fmt_short(ns, 3600), "11:20:30.123");
        assert_eq!(fmt_medium(ns, 3600), "2026-11-18 11:20:30.123");
        assert_eq!(fmt_full(ns, 3600), "2026-11-18 11:20:30.123456789 +01:00");
        assert_eq!(fmt_full(ns, -(5 * 3600 + 1800)), "2026-11-18 04:50:30.123456789 -05:30");
        // přes půlnoc
        assert_eq!(fmt_medium(parse_rfc3339("2026-12-31T23:30:00Z").unwrap(), 3600), "2027-01-01 00:30:00.000");
        assert_eq!(save_file_name("web", ns, 3600), "web-20261118-1120.log");
        assert_eq!(save_file_name("a/b:c", ns, 0), "a_b_c-20261118-1020.log");
    }

    #[test]
    fn ansi_stripped() {
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("\x1b[32mINFO\x1b[0m ok"), "INFO ok");
        assert_eq!(strip_ansi("\x1b]0;title\x07x"), "x");
    }

    #[test]
    fn matches_case_insensitive_multi_and_utf8() {
        assert_eq!(find_matches("Error error ERROR", "error", false), vec![0..5, 6..11, 12..17]);
        assert_eq!(find_matches("Error error ERROR", "error", true), vec![6..11]);
        assert_eq!(find_matches("aaaa", "aa", false), vec![0..2, 2..4]);
        assert!(find_matches("abc", "", false).is_empty());
        assert!(find_matches("", "x", false).is_empty());
        // ASCII hledání v textu s diakritikou: rozsahy na hranicích znaků
        let t = "Žluťoučký kůň: OK, ok";
        let m = find_matches(t, "ok", false);
        assert_eq!(m.len(), 2);
        for r in &m {
            assert!(t.is_char_boundary(r.start) && t.is_char_boundary(r.end));
            assert_eq!(t[r.clone()].to_lowercase(), "ok");
        }
        // ne-ASCII hledání bez ohledu na velikost
        let t = "KŮŇ kůň Kůň";
        let m = find_matches(t, "kůň", false);
        assert_eq!(m.iter().map(|r| &t[r.clone()]).collect::<Vec<_>>(), vec!["KŮŇ", "kůň", "Kůň"]);
        assert_eq!(find_matches(t, "Kůň", true).iter().map(|r| &t[r.clone()]).collect::<Vec<_>>(), vec!["Kůň"]);
        // znak, jehož malé písmeno má víc bajtů / znaků (İ -> i̇): rozsah vždy na hranicích
        let t = "xİy İ";
        for r in find_matches(t, "i̇", false) {
            assert!(t.is_char_boundary(r.start) && t.is_char_boundary(r.end), "{r:?}");
        }
        let t = "ÉCOLE école";
        assert_eq!(find_matches(t, "éc", false).iter().map(|r| &t[r.clone()]).collect::<Vec<_>>(), vec!["ÉC", "éc"]);
    }

    fn ts(s: u32) -> String {
        format!("2026-11-18T10:00:{s:02}.000000000Z")
    }

    #[test]
    fn chunks_partial_lines_and_timestamps() {
        let now = Instant::now();
        let mut b = LogBuf::new(100, None);
        // TTY: řádek rozdělený do bloků, CRLF
        b.push_chunk(0, &format!("{} one\r\n{} tw", ts(1), ts(2)), false, now);
        assert_eq!(b.len(), 1);
        b.push_chunk(0, "o\n", false, now);
        assert_eq!(b.len(), 2);
        assert_eq!(b.get(1).unwrap().text, "two");
        assert_eq!(b.get(1).unwrap().ts, parse_rfc3339(&ts(2)));
        // rámec bez TTY bez koncového \n je celý řádek
        b.push_chunk(1, &format!("{} three", ts(3)), true, now);
        assert_eq!(b.get(2).unwrap(), &LogLine { ts: parse_rfc3339(&ts(3)), src: 1, text: "three".into() });
        // prázdný řádek zůstane, řádek bez času taky
        b.push_chunk(0, "\nplain\n", true, now);
        assert_eq!(b.get(3).unwrap().text, "");
        assert_eq!(b.get(4).unwrap(), &LogLine { ts: None, src: 0, text: "plain".into() });
    }

    #[test]
    fn ring_buffer_trims_oldest() {
        let now = Instant::now();
        let mut b = LogBuf::new(3, None);
        for i in 0..5 {
            b.push_chunk(0, &format!("line {i}\n"), true, now);
        }
        assert_eq!(b.len(), 3);
        assert_eq!(b.dropped, 2);
        assert_eq!(b.first_abs(), 2);
        assert_eq!(b.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["line 2", "line 3", "line 4"]);
        assert_eq!(b.get_abs(1), None);
        assert_eq!(b.get_abs(4).unwrap().text, "line 4");
        // cache zobrazení: zahozené řádky odřízne, nové doplní
        let mut c = ViewCache::default();
        c.update(&b, "", &[], "line", false);
        assert_eq!((c.rows.len(), c.hit_count), (3, 3));
        b.push_chunk(0, "line 5\nline 6\n", true, now);
        c.update(&b, "", &[], "line", false);
        assert_eq!(c.rows.iter().map(|r| r.abs).collect::<Vec<_>>(), [4, 5, 6]);
        assert_eq!(c.hit_count, 3);
        b.clear();
        assert_eq!((b.len(), b.dropped), (0, 0));
        c.update(&b, "", &[], "line", false);
        assert!(c.rows.is_empty());
        assert_eq!(c.hit_count, 0);
    }

    #[test]
    fn reorder_buffer_merges_by_time() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut b = LogBuf::new(100, Some(REORDER));
        b.push_chunk(0, &format!("{} a1\n{} a3\n", ts(1), ts(3)), true, ms(0));
        b.push_chunk(1, &format!("{} b2\n", ts(2)), true, ms(100));
        // ještě v okně – nic se nevydalo
        b.flush(ms(250));
        assert_eq!((b.len(), b.pending()), (0, 3));
        // a1, a3 vydány; b2 čeká (dorazil později), ale a3 ho nepředběhne trvale
        b.flush(ms(310));
        assert_eq!(b.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["a1", "a3"]);
        b.flush(ms(410));
        // b2 dorazil po okně -> zařazen zpětně podle času
        assert_eq!(b.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["a1", "b2", "a3"]);
        assert_eq!(b.epoch(), 1, "vložení doprostřed mění epochu");
        // v rámci okna se řadí podle času i opačné pořadí příchodu
        b.push_chunk(1, &format!("{} b9\n", ts(9)), true, ms(500));
        b.push_chunk(0, &format!("{} a8\n", ts(8)), true, ms(520));
        b.push_note(0, "[note]", ms(530));
        b.flush(ms(900));
        assert_eq!(b.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["a1", "b2", "a3", "a8", "b9", "[note]"]);
        assert_eq!(b.iter().map(|l| l.src).collect::<Vec<_>>(), [0, 1, 0, 0, 1, 0]);
        let tss: Vec<i64> = b.iter().filter_map(|l| l.ts).collect();
        assert!(tss.windows(2).all(|w| w[0] <= w[1]), "{tss:?}");
    }

    #[test]
    fn view_cache_filter_hidden_and_navigation() {
        let now = Instant::now();
        let mut b = LogBuf::new(100, None);
        b.push_chunk(0, "GET /a ok\nPOST /b fail\n", true, now);
        b.push_chunk(1, "get get\nnothing\n", true, now);
        let mut c = ViewCache::default();
        c.update(&b, "", &[], "get", false);
        assert_eq!(c.rows.len(), 4);
        assert_eq!(c.hit_count, 3);
        assert_eq!(c.rows[2].hits, vec![0..3, 4..7]);
        // filtr řádků a skrytá služba
        c.update(&b, "GET", &[false, true], "get", false);
        assert_eq!(c.rows.iter().map(|r| r.abs).collect::<Vec<_>>(), [0]);
        c.update(&b, "", &[], "get", true);
        assert_eq!(c.hit_count, 2);
        // navigace dokola
        c.update(&b, "", &[], "get", false);
        let first = c.step(None, 1).unwrap();
        assert_eq!(first, (0, 0, 0));
        let last = c.step(None, -1).unwrap();
        assert_eq!(last, (2, 2, 1));
        assert_eq!(c.step(Some((last.1, last.2)), 1), Some(first));
        assert_eq!(c.step(Some((first.1, first.2)), -1), Some(last));
        assert_eq!(c.step(Some((2, 0)), 1), Some((2, 2, 1)));
        assert_eq!(c.hit_index((2, 1)), Some(2));
        assert_eq!(c.hit_index((1, 0)), None);
    }

    #[test]
    fn save_formatting() {
        let a = parse_rfc3339(&ts(1)).unwrap();
        let lines = [
            LogLine { ts: Some(a), src: 0, text: "hello".into() },
            LogLine { ts: None, src: 1, text: "no time".into() },
            LogLine { ts: Some(a + 5_000_000), src: 1, text: "Žluťoučký".into() },
        ];
        assert_eq!(format_lines(lines.iter(), None, false, |_| 0), "hello\nno time\nŽluťoučký\n");
        assert_eq!(
            format_lines(lines.iter(), None, true, |_| 3600),
            "2026-11-18 11:00:01.000 hello\n                        no time\n2026-11-18 11:00:01.005 Žluťoučký\n"
        );
        let names = vec!["web".to_string(), "worker".to_string()];
        assert_eq!(format_lines(lines.iter(), Some(&names), false, |_| 0), "web    | hello\nworker | no time\nworker | Žluťoučký\n");
    }
}
