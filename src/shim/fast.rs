//! Rychlá cesta shimu: jednoduché příkazy jen pro čtení (`ps`, `images`, `volume ls`,
//! `network ls` – i s filtry a `--format json`, `inspect` / `container inspect` /
//! `image inspect`, `logs` bez `-f`, `version --format`, `info --format`) odpoví přímo
//! z REST API enginu, bez spouštění `wsl.exe` (to stojí ~0,1–0,3 s).
//!
//! Výstup napodobuje **Docker CLI** (shim se za něj vydává), ne `podman`:
//! tabulka přes tabwriter (min. šířka 10, odsazení 3), 12znaková ID, COMMAND v uvozovkách
//! zkrácený na 20 znaků, velikosti v desítkové soustavě (`149MB`), „3 hours ago“,
//! „familiar“ názvy obrazů (`nginx` místo `docker.io/library/nginx`).
//!
//! Cokoli neznámého (přepínač, pole šablony, funkce šablony, chyba API) vrátí `Err`
//! a shim tiše použije `wsl.exe` – správnost má přednost před rychlostí.
//!
//! Známé rozdíly oproti Docker CLI:
//! - `psFormat` / `imagesFormat` z `~/.docker/config.json` se neberou v úvahu;
//! - `docker images` má klasický tabulkový výstup (ne stromový pohled novějších CLI);
//! - pořadí kontejnerů a obrazů je podle data vytvoření (nejnovější první), jak je vrací Docker;
//! - zkratka časové zóny v `CreatedAt` se bere z tabulky podle názvu zóny Windows;
//! - Podman v kompatibilním API přejmenovává síť `podman` na `bridge`;
//! - `inspect` vypisuje „raw“ JSON z API (jako Docker CLI bez šablony); `{{json .}}` je
//!   kompaktní raw JSON, takže pořadí klíčů je podle enginu, ne podle struktur Dockeru;
//!   pole šablon jen skalární (`{{.State.Pid}}`), mapy / pole / null -> CLI;
//! - `logs`: `-n` znamená `--tail` jako v Docker CLI (Podman CLI by ho bral jako `--names`).

use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

// ================================================================ šablony

/// Část šablony Go: literál, pole (`.A` / `.A.B`) nebo `json .`.
#[derive(Debug, Clone, PartialEq)]
pub enum Seg {
    Lit(String),
    Field(Vec<String>),
    JsonDot,
}

fn ident(s: &str) -> bool {
    let mut ch = s.chars();
    matches!(ch.next(), Some(c) if c.is_ascii_alphabetic() || c == '_') && ch.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Rozebere podmnožinu šablon Go; cokoli jiného (funkce, `if`, `range`, `{{-`) je chyba.
pub fn parse_template(s: &str) -> Result<Vec<Seg>, String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find("{{") {
        if i > 0 {
            out.push(Seg::Lit(rest[..i].to_string()));
        }
        let after = &rest[i + 2..];
        let j = after.find("}}").ok_or("unclosed action")?;
        let inner = &after[..j];
        if inner.starts_with('-') || inner.ends_with('-') || inner.starts_with("/*") {
            return Err(format!("unsupported action {{{{{inner}}}}}"));
        }
        let t = inner.trim();
        if t == "json ." {
            out.push(Seg::JsonDot);
        } else if let Some(p) = t.strip_prefix('.') {
            let parts: Vec<String> = p.split('.').map(String::from).collect();
            if !parts.iter().all(|x| ident(x)) {
                return Err(format!("unsupported action {{{{{inner}}}}}"));
            }
            out.push(Seg::Field(parts));
        } else {
            return Err(format!("unsupported action {{{{{inner}}}}}"));
        }
        rest = &after[j + 2..];
    }
    if !rest.is_empty() {
        out.push(Seg::Lit(rest.to_string()));
    }
    Ok(out)
}

fn render(segs: &[Seg], field: &dyn Fn(&str) -> String, json: &dyn Fn() -> String) -> String {
    let mut s = String::new();
    for seg in segs {
        match seg {
            Seg::Lit(l) => s.push_str(l),
            Seg::Field(f) => s.push_str(&field(&f[0])),
            Seg::JsonDot => s.push_str(&json()),
        }
    }
    s
}

/// Jak vypsat seznam.
#[derive(Debug, Clone, PartialEq)]
pub enum Fmt {
    Table(Vec<Seg>),
    Plain(Vec<Seg>),
    Json,
}

impl Fmt {
    fn segs(&self) -> &[Seg] {
        match self {
            Fmt::Table(s) | Fmt::Plain(s) => s,
            Fmt::Json => &[],
        }
    }
    fn uses(&self, field: &str) -> bool {
        self.segs().iter().any(|s| matches!(s, Seg::Field(f) if f[0] == field))
    }
}

/// `--format` podle Docker CLI: `table`, `table <šablona>`, `json`, `<šablona>`;
/// `\t` a `\n` se nahradí, okolní mezery se ořežou. Pole se ověří proti `headers`.
pub fn resolve_format(format: Option<&str>, quiet: bool, default_table: &str, quiet_tmpl: &str, headers: &[(&str, &str)], json_ok: bool) -> Result<Fmt, String> {
    let prep = |s: &str| s.trim_matches(' ').replace(r"\t", "\t").replace(r"\n", "\n");
    let f = format.unwrap_or("table");
    let fmt = if f == "table" {
        if quiet {
            Fmt::Plain(parse_template(quiet_tmpl)?)
        } else {
            Fmt::Table(parse_template(&prep(default_table))?)
        }
    } else if f == "json" {
        if !json_ok {
            return Err("json format not supported here".into());
        }
        Fmt::Json
    } else if let Some(t) = f.strip_prefix("table") {
        Fmt::Table(parse_template(&prep(t))?)
    } else {
        Fmt::Plain(parse_template(&prep(f))?)
    };
    for s in fmt.segs() {
        match s {
            Seg::Field(p) if p.len() != 1 || !headers.iter().any(|(k, _)| *k == p[0]) => {
                return Err(format!("unsupported field .{}", p.join(".")));
            }
            Seg::JsonDot if !json_ok || matches!(fmt, Fmt::Table(_)) => return Err("{{json .}} not supported here".into()),
            _ => {}
        }
    }
    Ok(fmt)
}

/// Vypíše řádky podle formátu (tabulka s hlavičkou přes tabwriter, jinak řádek po řádku).
fn emit<R>(fmt: &Fmt, headers: &[(&str, &str)], rows: &[R], field: impl Fn(&R, &str) -> String) -> String {
    let json_of = |r: &R| {
        let mut keys: Vec<&str> = headers.iter().map(|(k, _)| *k).collect();
        keys.sort_unstable();
        let parts: Vec<String> = keys.iter().map(|k| format!("{}:{}", go_json_str(k), go_json_str(&field(r, k)))).collect();
        format!("{{{}}}", parts.join(","))
    };
    let mut out = String::new();
    match fmt {
        Fmt::Json => {
            for r in rows {
                out.push_str(&json_of(r));
                out.push('\n');
            }
        }
        Fmt::Plain(segs) => {
            for r in rows {
                out.push_str(&render(segs, &|f| field(r, f), &|| json_of(r)));
                out.push('\n');
            }
        }
        Fmt::Table(segs) => {
            let hdr = |f: &str| headers.iter().find(|(k, _)| *k == f).map(|(_, h)| h.to_string()).unwrap_or_default();
            let mut text = render(segs, &hdr, &String::new);
            text.push('\n');
            for r in rows {
                text.push_str(&render(segs, &|f| field(r, f), &String::new));
                text.push('\n');
            }
            out = tabwrite(&text, 10, 3);
        }
    }
    out
}

/// Zarovnání sloupců jako Go `text/tabwriter` (bez příznaků, výplň mezerami):
/// buňka = text před `\t`, šířka sloupce = max(minwidth, nejdelší buňka + padding)
/// v souvislém bloku řádků, které daný sloupec mají. Text za posledním `\t` se nezarovnává.
pub fn tabwrite(text: &str, minwidth: usize, padding: usize) -> String {
    let lines: Vec<Vec<&str>> = text.split('\n').map(|l| l.split('\t').collect()).collect();
    let ncols = lines.iter().map(|c| c.len().saturating_sub(1)).max().unwrap_or(0);
    // widths[řádek][sloupec]
    let mut widths: Vec<Vec<usize>> = lines.iter().map(|c| vec![0; c.len().saturating_sub(1)]).collect();
    for col in 0..ncols {
        let mut i = 0;
        while i < lines.len() {
            if lines[i].len() <= col + 1 {
                i += 1;
                continue;
            }
            let start = i;
            let mut w = minwidth;
            while i < lines.len() && lines[i].len() > col + 1 {
                w = w.max(lines[i][col].chars().count() + padding);
                i += 1;
            }
            for wl in widths.iter_mut().take(i).skip(start) {
                wl[col] = w;
            }
        }
    }
    let mut out = String::with_capacity(text.len() * 2);
    for (li, cells) in lines.iter().enumerate() {
        for (ci, c) in cells.iter().enumerate() {
            out.push_str(c);
            if ci + 1 < cells.len() {
                let n = c.chars().count();
                out.extend(std::iter::repeat(' ').take(widths[li][ci].saturating_sub(n)));
            }
        }
        if li + 1 < lines.len() {
            out.push('\n');
        }
    }
    out
}

// ================================================================ formátování hodnot (go-units, cli)

/// Go `fmt.Sprintf("%.*g", prec, v)`.
pub fn go_g(v: f64, prec: usize) -> String {
    if v == 0.0 {
        return "0".into();
    }
    let e = format!("{:.*e}", prec - 1, v);
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let trim = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if exp < -4 || exp >= prec as i32 {
        format!("{}e{}{:02}", trim(mant), if exp < 0 { '-' } else { '+' }, exp.abs())
    } else {
        let dec = (prec as i32 - 1 - exp).max(0) as usize;
        trim(&format!("{:.*}", dec, v))
    }
}

/// go-units `HumanSizeWithPrecision` (desítkové jednotky, `149MB`).
pub fn human_size(size: f64, prec: usize) -> String {
    const U: [&str; 9] = ["B", "kB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
    let mut s = size;
    let mut i = 0;
    while s >= 1000.0 && i < U.len() - 1 {
        s /= 1000.0;
        i += 1;
    }
    format!("{}{}", go_g(s, prec), U[i])
}

/// go-units `HumanDuration` (vstup v sekundách).
pub fn human_duration(secs: i64) -> String {
    let s = secs;
    if s < 1 {
        return "Less than a second".into();
    }
    if s == 1 {
        return "1 second".into();
    }
    if s < 60 {
        return format!("{s} seconds");
    }
    let minutes = s / 60;
    if minutes == 1 {
        return "About a minute".into();
    }
    if minutes < 60 {
        return format!("{minutes} minutes");
    }
    // d.Round(time.Hour) – půlky od nuly
    let hours = (s + 1800) / 3600;
    if hours == 1 {
        return "About an hour".into();
    }
    if hours < 48 {
        return format!("{hours} hours");
    }
    if hours < 24 * 7 * 2 {
        return format!("{} days", hours / 24);
    }
    if hours < 24 * 30 * 2 {
        return format!("{} weeks", hours / 24 / 7);
    }
    if hours < 24 * 365 * 2 {
        return format!("{} months", hours / 24 / 30);
    }
    format!("{} years", s / 3600 / 24 / 365)
}

fn ago(now: i64, created: i64) -> String {
    format!("{} ago", human_duration(now - created))
}

/// `stringid.TruncateID`: bez prefixu `algo:`, nejvýš 12 znaků.
pub fn truncate_id(id: &str) -> String {
    let id = id.split_once(':').map(|(_, r)| r).unwrap_or(id);
    id.chars().take(12).collect()
}

/// `formatter.Ellipsis` pro šířku znaků (široké znaky se nepočítají zvlášť).
pub fn ellipsis(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut r: String = s.chars().take(max - 1).collect();
    r.push('…');
    r
}

/// Go `strconv.Quote`.
pub fn go_quote(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            '\r' => o.push_str("\\r"),
            '\x07' => o.push_str("\\a"),
            '\x08' => o.push_str("\\b"),
            '\x0c' => o.push_str("\\f"),
            '\x0b' => o.push_str("\\v"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => o.push_str(&format!("\\x{:02x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Řetězec v JSON jako Go `encoding/json` (escapuje `<`, `>`, `&`, U+2028/9).
pub fn go_json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => o.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Přirozené řazení (`sortorder.NaturalLess`): čísla se porovnávají číselně.
pub fn natural_less(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (da, db) = (a[i].is_ascii_digit(), b[j].is_ascii_digit());
        if da && db {
            let si = i;
            while i < a.len() && a[i] == b'0' {
                i += 1;
            }
            let ni = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j] == b'0' {
                j += 1;
            }
            let nj = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let (x, y) = (&a[ni..i], &b[nj..j]);
            if x.len() != y.len() {
                return x.len() < y.len();
            }
            if x != y {
                return x < y;
            }
            if ni - si != nj - sj {
                return ni - si < nj - sj;
            }
        } else {
            if a[i] != b[j] {
                return a[i] < b[j];
            }
            i += 1;
            j += 1;
        }
    }
    a.len() - i < b.len() - j
}

// ---------------------------------------------------------------- čas

/// (posun v sekundách, zkratka zóny) pro daný unixový čas.
pub type TzFn = fn(i64) -> (i32, String);

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

/// Go `time.Unix(ts, 0).String()`: `2026-11-18 19:47:27 +0100 CET`.
pub fn go_time_string(ts: i64, tz: TzFn) -> String {
    let (off, abbr) = tz(ts);
    let t = ts + off as i64;
    let (y, mo, d) = civil_from_days(t.div_euclid(86400));
    let sod = t.rem_euclid(86400);
    let sign = if off < 0 { '-' } else { '+' };
    let oa = off.abs() / 60;
    let zone = if abbr.is_empty() { format!("{sign}{:02}{:02}", oa / 60, oa % 60) } else { abbr };
    format!(
        "{y:04}-{mo:02}-{d:02} {:02}:{:02}:{:02} {sign}{:02}{:02} {zone}",
        sod / 3600,
        sod / 60 % 60,
        sod % 60,
        oa / 60,
        oa % 60
    )
}

/// Zkratky zón jako Go na Windows (podle anglického názvu zóny); jinak velká písmena názvu.
fn zone_abbr(key: &str, dst: bool) -> String {
    const T: &[(&str, &str, &str)] = &[
        ("Central Europe Standard Time", "CET", "CEST"),
        ("Central European Standard Time", "CET", "CEST"),
        ("Romance Standard Time", "CET", "CEST"),
        ("W. Europe Standard Time", "CET", "CEST"),
        ("GMT Standard Time", "GMT", "BST"),
        ("Greenwich Standard Time", "GMT", "GMT"),
        ("UTC", "UTC", "UTC"),
        ("Coordinated Universal Time", "UTC", "UTC"),
        ("E. Europe Standard Time", "EET", "EEST"),
        ("FLE Standard Time", "EET", "EEST"),
        ("GTB Standard Time", "EET", "EEST"),
        ("Russian Standard Time", "MSK", "MSK"),
        ("Eastern Standard Time", "EST", "EDT"),
        ("Central Standard Time", "CST", "CDT"),
        ("Mountain Standard Time", "MST", "MDT"),
        ("Pacific Standard Time", "PST", "PDT"),
        ("Alaskan Standard Time", "AKST", "AKDT"),
        ("Hawaiian Standard Time", "HST", "HST"),
        ("Atlantic Standard Time", "AST", "ADT"),
        ("China Standard Time", "CST", "CST"),
        ("Tokyo Standard Time", "JST", "JST"),
        ("Korea Standard Time", "KST", "KST"),
        ("India Standard Time", "IST", "IST"),
        ("Israel Standard Time", "IST", "IDT"),
        ("AUS Eastern Standard Time", "AEST", "AEDT"),
        ("New Zealand Standard Time", "NZST", "NZDT"),
        ("South Africa Standard Time", "SAST", "SAST"),
    ];
    if let Some((_, s, d)) = T.iter().find(|(k, _, _)| *k == key) {
        return if dst { d } else { s }.to_string();
    }
    let name = if dst { key.replace("Standard", "Daylight") } else { key.to_string() };
    name.chars().filter(|c| c.is_ascii_uppercase()).collect()
}

/// Místní časová zóna Windows (jako Docker CLI pro Windows).
#[cfg(windows)]
pub fn local_tz(ts: i64) -> (i32, String) {
    use windows_sys::Win32::Foundation::SYSTEMTIME;
    use windows_sys::Win32::System::Time::{GetDynamicTimeZoneInformation, SystemTimeToTzSpecificLocalTimeEx, DYNAMIC_TIME_ZONE_INFORMATION, TIME_ZONE_ID_INVALID};
    let to_st = |t: i64| {
        let (y, m, d) = civil_from_days(t.div_euclid(86400));
        let s = t.rem_euclid(86400);
        SYSTEMTIME {
            wYear: y as u16,
            wMonth: m as u16,
            wDayOfWeek: 0,
            wDay: d as u16,
            wHour: (s / 3600) as u16,
            wMinute: (s / 60 % 60) as u16,
            wSecond: (s % 60) as u16,
            wMilliseconds: 0,
        }
    };
    // SAFETY: struktury jsou plně inicializované nulami a předané ukazatele platí po dobu volání
    unsafe {
        let mut tz: DYNAMIC_TIME_ZONE_INFORMATION = std::mem::zeroed();
        if GetDynamicTimeZoneInformation(&mut tz) == TIME_ZONE_ID_INVALID {
            return (0, "UTC".into());
        }
        let utc = to_st(ts);
        let mut loc: SYSTEMTIME = std::mem::zeroed();
        if SystemTimeToTzSpecificLocalTimeEx(&tz, &utc, &mut loc) == 0 {
            return (0, "UTC".into());
        }
        let lt = days_from_civil(loc.wYear as i64, loc.wMonth as i64, loc.wDay as i64) * 86400
            + loc.wHour as i64 * 3600
            + loc.wMinute as i64 * 60
            + loc.wSecond as i64;
        let off = (lt - ts) as i32;
        let key_len = tz.TimeZoneKeyName.iter().position(|&c| c == 0).unwrap_or(tz.TimeZoneKeyName.len());
        let key = String::from_utf16_lossy(&tz.TimeZoneKeyName[..key_len]);
        let dst = off / 60 != -(tz.Bias + tz.StandardBias);
        (off, zone_abbr(&key, dst))
    }
}

#[cfg(not(windows))]
pub fn local_tz(_ts: i64) -> (i32, String) {
    (0, "UTC".into())
}

pub struct Ctx {
    pub trunc: bool,
    pub now: i64,
    pub tz: TzFn,
}

// ================================================================ obrazy – názvy

/// `reference.FamiliarName`: `docker.io/library/nginx` -> `nginx`, `docker.io/x/y` -> `x/y`.
pub fn familiar_name(name: &str) -> String {
    for p in ["docker.io/", "index.docker.io/"] {
        if let Some(rest) = name.strip_prefix(p) {
            if let Some(r) = rest.strip_prefix("library/") {
                if !r.contains('/') {
                    return r.to_string();
                }
            }
            return rest.to_string();
        }
    }
    name.to_string()
}

/// Rozdělí referenci na (název, tag, digest). None = neplatná reference (např. `<none>:<none>`).
pub fn split_ref(r: &str) -> Option<(String, Option<String>, Option<String>)> {
    let (rest, digest) = match r.split_once('@') {
        Some((a, d)) => (a, Some(d.to_string())),
        None => (r, None),
    };
    let slash = rest.rfind('/').map(|i| i + 1).unwrap_or(0);
    let (name, tag) = match rest[slash..].rfind(':') {
        Some(i) => (&rest[..slash + i], Some(rest[slash + i + 1..].to_string())),
        None => (rest, None),
    };
    let path_ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-/:".contains(c));
    // doména může obsahovat velká písmena a port, cesta ne
    let path = match name.split_once('/') {
        Some((d, p)) if d.contains('.') || d.contains(':') || d == "localhost" => p,
        _ => name,
    };
    if !path_ok(path) || name.starts_with('<') || tag.as_deref().is_some_and(|t| t.is_empty() || t.starts_with('<')) {
        return None;
    }
    // čisté 64znakové hex ID není reference
    if digest.is_none() && tag.is_none() && name.len() == 64 && name.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some((name.to_string(), tag, digest))
}

// ================================================================ ps

pub const PS_HEADERS: &[(&str, &str)] = &[
    ("ID", "CONTAINER ID"),
    ("Image", "IMAGE"),
    ("Command", "COMMAND"),
    ("CreatedAt", "CREATED AT"),
    ("RunningFor", "CREATED"),
    ("Ports", "PORTS"),
    ("State", "STATE"),
    ("Status", "STATUS"),
    ("Size", "SIZE"),
    ("Names", "NAMES"),
    ("Labels", "LABELS"),
    ("Mounts", "MOUNTS"),
    ("LocalVolumes", "LOCAL VOLUMES"),
    ("Networks", "NETWORKS"),
];
const PS_TABLE: &str = r"{{.ID}}\t{{.Image}}\t{{.Command}}\t{{.RunningFor}}\t{{.Status}}\t{{.Ports}}\t{{.Names}}";

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}
fn n(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64().or_else(|| x.as_f64().map(|f| f as i64))).unwrap_or(0)
}

fn labels_text(v: &Value) -> String {
    let mut l: Vec<String> = v
        .as_object()
        .map(|m| m.iter().map(|(k, x)| format!("{k}={}", x.as_str().unwrap_or(""))).collect())
        .unwrap_or_default();
    l.sort();
    l.join(",")
}

fn join_host_port(ip: &str, port: &str) -> String {
    if ip.contains(':') {
        format!("[{ip}]:{port}")
    } else {
        format!("{ip}:{port}")
    }
}

/// `formatter.DisplayablePorts`: seskupí po sobě jdoucí porty, mapování na hostitele na konec.
pub fn displayable_ports(ports: &[Value]) -> String {
    struct P {
        ip: String,
        private: u32,
        public: u32,
        typ: String,
    }
    let mut ps: Vec<P> = ports
        .iter()
        .map(|p| P { ip: s(p, "IP").to_string(), private: n(p, "PrivatePort") as u32, public: n(p, "PublicPort") as u32, typ: s(p, "Type").to_string() })
        .collect();
    ps.sort_by(|a, b| (a.private, &a.ip, a.public, &a.typ).cmp(&(b.private, &b.ip, b.public, &b.typ)));
    let form = |key: &str, first: u32, last: u32| -> String {
        let (ip, typ) = match key.rsplit_once('/') {
            Some((ip, t)) => (ip, t),
            None => ("", key),
        };
        let mut g = first.to_string();
        if first != last {
            g = format!("{g}-{last}");
        }
        if !ip.is_empty() {
            g = format!("{}->{g}", join_host_port(ip, &g));
        }
        format!("{g}/{typ}")
    };
    let mut groups: Vec<(String, u32, u32)> = Vec::new();
    let mut result = Vec::new();
    let mut host = Vec::new();
    for p in &ps {
        let mut key = p.typ.clone();
        if !p.ip.is_empty() {
            if p.public != p.private {
                host.push(format!("{}->{}/{}", join_host_port(&p.ip, &p.public.to_string()), p.private, p.typ));
                continue;
            }
            key = format!("{}/{}", p.ip, p.typ);
        }
        match groups.iter_mut().find(|g| g.0 == key) {
            None => groups.push((key, p.private, p.private)),
            Some(g) => {
                if p.private == g.2 + 1 {
                    g.2 = p.private;
                } else {
                    result.push(form(&g.0, g.1, g.2));
                    g.1 = p.private;
                    g.2 = p.private;
                }
            }
        }
    }
    for g in &groups {
        result.push(form(&g.0, g.1, g.2));
    }
    result.extend(host);
    result.join(", ")
}

pub fn ps_field(c: &Value, f: &str, x: &Ctx) -> String {
    match f {
        "ID" => {
            let id = s(c, "Id");
            if x.trunc { truncate_id(id) } else { id.to_string() }
        }
        "Image" => {
            let img = s(c, "Image");
            if img.is_empty() {
                return "<no image>".into();
            }
            if !x.trunc {
                return img.to_string();
            }
            let tid = truncate_id(s(c, "ImageID"));
            if !tid.is_empty() && tid == truncate_id(img) {
                return tid;
            }
            match split_ref(img) {
                Some((name, Some(tag), _)) => format!("{}:{tag}", familiar_name(&name)),
                Some((name, None, _)) => familiar_name(&name),
                None => img.to_string(),
            }
        }
        "Command" => {
            let cmd = s(c, "Command");
            go_quote(&if x.trunc { ellipsis(cmd, 20) } else { cmd.to_string() })
        }
        "CreatedAt" => go_time_string(n(c, "Created"), x.tz),
        "RunningFor" => ago(x.now, n(c, "Created")),
        "Ports" => displayable_ports(c.get("Ports").and_then(|p| p.as_array()).map(|a| a.as_slice()).unwrap_or(&[])),
        "State" => s(c, "State").to_string(),
        "Status" => s(c, "Status").to_string(),
        "Size" => {
            let rw = human_size(n(c, "SizeRw") as f64, 3);
            let root = n(c, "SizeRootFs");
            if root > 0 { format!("{rw} (virtual {})", human_size(root as f64, 3)) } else { rw }
        }
        "Names" => {
            let names: Vec<String> = c
                .get("Names")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| x.strip_prefix('/').unwrap_or(x).to_string()).collect())
                .unwrap_or_default();
            if x.trunc {
                if let Some(first) = names.iter().find(|nm| !nm.contains('/')) {
                    return first.clone();
                }
            }
            names.join(",")
        }
        "Labels" => c.get("Labels").map(labels_text).unwrap_or_default(),
        "Mounts" => {
            let ms = c.get("Mounts").and_then(|m| m.as_array()).cloned().unwrap_or_default();
            ms.iter()
                .map(|m| {
                    let name = if s(m, "Name").is_empty() { s(m, "Source") } else { s(m, "Name") };
                    if x.trunc { ellipsis(name, 15) } else { name.to_string() }
                })
                .collect::<Vec<_>>()
                .join(",")
        }
        "LocalVolumes" => {
            let ms = c.get("Mounts").and_then(|m| m.as_array()).cloned().unwrap_or_default();
            ms.iter().filter(|m| s(m, "Driver") == "local").count().to_string()
        }
        "Networks" => {
            let mut nets: Vec<String> = c
                .get("NetworkSettings")
                .and_then(|ns| ns.get("Networks"))
                .and_then(|m| m.as_object())
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            nets.sort();
            nets.join(",")
        }
        _ => String::new(),
    }
}

// ================================================================ images

pub const IMAGE_HEADERS: &[(&str, &str)] = &[
    ("ID", "IMAGE ID"),
    ("Repository", "REPOSITORY"),
    ("Tag", "TAG"),
    ("CreatedSince", "CREATED"),
    ("CreatedAt", "CREATED AT"),
    ("Size", "SIZE"),
    ("Containers", "CONTAINERS"),
    ("Digest", "DIGEST"),
    ("SharedSize", "SHARED SIZE"),
    ("UniqueSize", "UNIQUE SIZE"),
    ("VirtualSize", "VIRTUAL SIZE"),
];
const IMAGE_TABLE: &str = r"{{.Repository}}\t{{.Tag}}\t{{.ID}}\t{{.CreatedSince}}\t{{.Size}}";

pub struct ImageRow {
    pub repo: String,
    pub tag: String,
    pub id: String,
    pub created: i64,
    pub size: i64,
    pub shared: i64,
    pub containers: i64,
    /// `<none>` u řádků s tagem a visících obrazů, prázdné u řádků jen s digestem
    pub digest: String,
}

/// Řádky jako `docker images` (bez `--digests`): jeden na tag; repozitář jen s digestem
/// má jeden řádek s tagem `<none>` a prázdným digestem; visící obraz `<none>`/`<none>`.
pub fn image_rows(images: &[Value]) -> Vec<ImageRow> {
    let mut imgs: Vec<&Value> = images.iter().collect();
    imgs.sort_by_key(|i| std::cmp::Reverse(n(i, "Created")));
    let mut out = Vec::new();
    for i in imgs {
        let strs = |k: &str| -> Vec<String> { i.get(k).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
        let row = |repo: &str, tag: &str, digest: &str| ImageRow {
            repo: repo.to_string(),
            tag: tag.to_string(),
            id: s(i, "Id").to_string(),
            created: n(i, "Created"),
            size: n(i, "Size"),
            shared: i.get("SharedSize").and_then(|c| c.as_i64()).unwrap_or(-1),
            containers: i.get("Containers").and_then(|c| c.as_i64()).unwrap_or(-1),
            digest: digest.to_string(),
        };
        let (tags, digests) = (strs("RepoTags"), strs("RepoDigests"));
        let mut repos: Vec<(String, Vec<String>)> = Vec::new();
        for t in &tags {
            if let Some((name, Some(tag), _)) = split_ref(t) {
                let f = familiar_name(&name);
                match repos.iter_mut().find(|r| r.0 == f) {
                    Some(r) => r.1.push(tag),
                    None => repos.push((f, vec![tag])),
                }
            }
        }
        let mut digest_repos: Vec<String> = Vec::new();
        for d in &digests {
            if let Some((name, _, Some(_))) = split_ref(d) {
                let f = familiar_name(&name);
                if !repos.iter().any(|r| r.0 == f) && !digest_repos.contains(&f) {
                    digest_repos.push(f);
                }
            }
        }
        let before = out.len();
        for (repo, ts) in &repos {
            for t in ts {
                out.push(row(repo, t, "<none>"));
            }
        }
        for repo in &digest_repos {
            // bez --digests jeden řádek na repozitář (Docker CLI 27: addImage(repo, "<none>", ""))
            out.push(row(repo, "<none>", ""));
        }
        if out.len() == before {
            out.push(row("<none>", "<none>", "<none>"));
        }
    }
    out
}

pub fn image_field(r: &ImageRow, f: &str, x: &Ctx) -> String {
    match f {
        "ID" => {
            if x.trunc { truncate_id(&r.id) } else { r.id.clone() }
        }
        "Repository" => r.repo.clone(),
        "Tag" => r.tag.clone(),
        "CreatedSince" => {
            if r.created == 0 { "N/A".into() } else { ago(x.now, r.created) }
        }
        "CreatedAt" => go_time_string(r.created, x.tz),
        "Size" => human_size(r.size as f64, 3),
        "Containers" => {
            if r.containers == -1 { "N/A".into() } else { r.containers.to_string() }
        }
        "Digest" => r.digest.clone(),
        "SharedSize" => {
            if r.shared == -1 { "N/A".into() } else { human_size(r.shared as f64, 4) }
        }
        "UniqueSize" => {
            if r.shared == -1 || r.size == -1 { "N/A".into() } else { human_size((r.size - r.shared) as f64, 4) }
        }
        "VirtualSize" => human_size(r.size as f64, 4),
        _ => String::new(),
    }
}

// ================================================================ volume / network

pub const VOLUME_HEADERS: &[(&str, &str)] = &[
    ("Name", "VOLUME NAME"),
    ("Driver", "DRIVER"),
    ("Scope", "SCOPE"),
    ("Mountpoint", "MOUNTPOINT"),
    ("Labels", "LABELS"),
    ("Links", "LINKS"),
    ("Size", "SIZE"),
    ("Group", "GROUP"),
    ("Availability", "AVAILABILITY"),
    ("Status", "STATUS"),
];
const VOLUME_TABLE: &str = r"{{.Driver}}\t{{.Name}}";

pub fn volume_field(v: &Value, f: &str) -> String {
    let usage = v.get("UsageData").filter(|u| u.is_object());
    match f {
        "Labels" => v.get("Labels").map(labels_text).unwrap_or_default(),
        "Links" => usage.map(|u| n(u, "RefCount").to_string()).unwrap_or_else(|| "N/A".into()),
        "Size" => usage.map(|u| human_size(n(u, "Size") as f64, 4)).unwrap_or_else(|| "N/A".into()),
        // jen pro clusterové svazky (Swarm CSI)
        "Group" | "Availability" | "Status" => "N/A".into(),
        k => s(v, k).to_string(),
    }
}

pub const NETWORK_HEADERS: &[(&str, &str)] = &[
    ("ID", "NETWORK ID"),
    ("Name", "NAME"),
    ("Driver", "DRIVER"),
    ("Scope", "SCOPE"),
    ("IPv6", "IPV6"),
    ("Internal", "INTERNAL"),
    ("Labels", "LABELS"),
    ("CreatedAt", "CREATED AT"),
];
const NETWORK_TABLE: &str = r"{{.ID}}\t{{.Name}}\t{{.Driver}}\t{{.Scope}}";

pub fn network_field(v: &Value, f: &str, x: &Ctx) -> String {
    let b = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false).to_string();
    match f {
        "ID" => {
            if x.trunc { truncate_id(s(v, "Id")) } else { s(v, "Id").to_string() }
        }
        "IPv6" => b("EnableIPv6"),
        "Internal" => b("Internal"),
        "Labels" => v.get("Labels").map(labels_text).unwrap_or_default(),
        "CreatedAt" => go_time_rfc3339(s(v, "Created"), x.tz),
        k => s(v, k).to_string(),
    }
}

/// Go `time.Time.String()` pro čas z JSON (RFC 3339): `2026-11-18 20:15:52.769554013 +0100 CET`.
/// Posun shodný s místní zónou dostane její zkratku, `Z` je `UTC`, jinak se zóna vypíše číselně.
pub fn go_time_rfc3339(t: &str, tz: TzFn) -> String {
    let parse = || -> Option<String> {
        let b = t.as_bytes();
        if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || (b[10] != b'T' && b[10] != b't') || b[13] != b':' || b[16] != b':' {
            return None;
        }
        let num = |r: std::ops::Range<usize>| t.get(r)?.parse::<i64>().ok();
        let (y, mo, d, h, mi, se) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
        let mut i = 19;
        let mut frac = String::new();
        if b.get(i) == Some(&b'.') {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                frac.push(b[i] as char);
                i += 1;
            }
        }
        let zone = &t[i..];
        let (off, utc) = if zone == "Z" || zone == "z" {
            (0, true)
        } else {
            let sign = match zone.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let (zh, zm) = zone[1..].split_once(':')?;
            (sign * (zh.parse::<i64>().ok()? * 3600 + zm.parse::<i64>().ok()? * 60), false)
        };
        let local = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + se;
        let unix = local - off;
        let abbr = if utc {
            "UTC".to_string()
        } else {
            let (loff, labbr) = tz(unix);
            if loff as i64 == off && !labbr.is_empty() { labbr } else { String::new() }
        };
        let frac = frac.trim_end_matches('0');
        let sign = if off < 0 { '-' } else { '+' };
        let oa = off.abs() / 60;
        let num_zone = format!("{sign}{:02}{:02}", oa / 60, oa % 60);
        let zone_name = if abbr.is_empty() { num_zone.clone() } else { abbr };
        let dot = if frac.is_empty() { "" } else { "." };
        Some(format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{se:02}{dot}{frac} {num_zone} {zone_name}"))
    };
    parse().unwrap_or_else(|| t.to_string())
}

// ================================================================ version / info

/// Hodnota pole šablony jako ji vypíše Go (`{{.X}}`); jen skaláry, jinak None.
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => n.as_i64().map(|i| i.to_string()).or_else(|| n.as_u64().map(|u| u.to_string())),
        _ => None,
    }
}

const VERSION_SERVER_FIELDS: &[&str] = &["Version", "APIVersion", "MinAPIVersion", "GitCommit", "GoVersion", "Os", "Arch", "KernelVersion", "BuildTime", "Experimental"];

fn version_lookup(v: &Value, path: &[String]) -> Option<String> {
    match path {
        [srv, f] if srv == "Server" && VERSION_SERVER_FIELDS.contains(&f.as_str()) => {
            let key = if f == "APIVersion" { "ApiVersion" } else { f.as_str() };
            v.get(key).and_then(scalar)
        }
        [srv, p, nm] if srv == "Server" && p == "Platform" && nm == "Name" => v.get("Platform").and_then(|p| p.get("Name")).and_then(scalar),
        _ => None,
    }
}

fn info_lookup(v: &Value, path: &[String]) -> Option<String> {
    match path {
        [f] => v.get(f.as_str()).and_then(scalar),
        _ => None,
    }
}

fn render_single(segs: &[Seg], lookup: impl Fn(&[String]) -> Option<String>) -> Result<String, String> {
    let mut out = String::new();
    for seg in segs {
        match seg {
            Seg::Lit(l) => out.push_str(l),
            Seg::Field(p) => out.push_str(&lookup(p).ok_or_else(|| format!("unsupported field .{}", p.join(".")))?),
            Seg::JsonDot => return Err("{{json .}} not supported here".into()),
        }
    }
    out.push('\n');
    Ok(out)
}

// ================================================================ přepínače

/// Co se po rozboru argumentů zavolá.
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Ps { all: bool, size: bool, limit: Option<i64>, filters: Vec<(String, String)>, trunc: bool, fmt: Fmt },
    Images { all: bool, trunc: bool, filters: Vec<(String, String)>, fmt: Fmt },
    Volumes { filters: Vec<(String, String)>, fmt: Fmt },
    Networks { trunc: bool, filters: Vec<(String, String)>, fmt: Fmt },
    Version(Vec<Seg>),
    Info(Vec<Seg>),
    /// `inspect` / `container inspect` / `image inspect`; None = JSON pole
    Inspect { kind: InspectKind, size: bool, names: Vec<String>, tmpl: Option<Vec<Seg>> },
    /// `logs` bez `-f`
    Logs { name: String, tail: Option<String>, timestamps: bool, since: Option<String>, until: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InspectKind {
    /// `docker inspect`: kontejner, jinak obraz
    Any,
    Container,
    Image,
}

struct Flags {
    bools: Vec<String>,
    values: Vec<(String, String)>,
    pos: Vec<String>,
}

impl Flags {
    fn has(&self, f: &str) -> bool {
        self.bools.iter().any(|b| b == f)
    }
    fn last(&self, f: &str) -> Option<&str> {
        self.values.iter().rev().find(|(k, _)| k == f).map(|(_, v)| v.as_str())
    }
}

/// pflag: `--x`, `--x=v`, `--x v`, `-abc`, `-n5`, `-n=5`, `-n 5`. Žádné poziční argumenty.
fn parse_flags(args: &[String], bools: &[(Option<char>, &str)], values: &[(Option<char>, &str)]) -> Result<Flags, String> {
    let f = parse_flags_pos(args, bools, values)?;
    match f.pos.first() {
        Some(a) => Err(format!("unsupported argument {a:?}")),
        None => Ok(f),
    }
}

/// Jako `parse_flags`, ale poziční argumenty (kdekoli mezi přepínači) se sbírají do `pos`.
fn parse_flags_pos(args: &[String], bools: &[(Option<char>, &str)], values: &[(Option<char>, &str)]) -> Result<Flags, String> {
    let mut f = Flags { bools: Vec::new(), values: Vec::new(), pos: Vec::new() };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        if let Some(long) = a.strip_prefix("--") {
            if long.is_empty() {
                return Err("`--` not supported".into());
            }
            let (name, val) = match long.split_once('=') {
                Some((nm, v)) => (nm, Some(v.to_string())),
                None => (long, None),
            };
            if bools.iter().any(|(_, l)| *l == name) {
                match val.as_deref() {
                    None | Some("true") => f.bools.push(name.to_string()),
                    Some("false") => f.bools.retain(|b| b != name),
                    Some(_) => return Err(format!("bad value for --{name}")),
                }
            } else if values.iter().any(|(_, l)| *l == name) {
                let v = match val {
                    Some(v) => v,
                    None => {
                        let v = args.get(i).ok_or(format!("--{name} needs a value"))?.clone();
                        i += 1;
                        v
                    }
                };
                f.values.push((name.to_string(), v));
            } else {
                return Err(format!("unsupported flag --{name}"));
            }
        } else if let Some(short) = a.strip_prefix('-').filter(|s| !s.is_empty()) {
            let chars: Vec<char> = short.chars().collect();
            let mut k = 0;
            while k < chars.len() {
                let c = chars[k];
                k += 1;
                if let Some((_, l)) = bools.iter().find(|(sc, _)| *sc == Some(c)) {
                    f.bools.push(l.to_string());
                } else if let Some((_, l)) = values.iter().find(|(sc, _)| *sc == Some(c)) {
                    let rest: String = chars[k..].iter().collect();
                    let v = if !rest.is_empty() {
                        rest.strip_prefix('=').unwrap_or(&rest).to_string()
                    } else {
                        let v = args.get(i).ok_or(format!("-{c} needs a value"))?.clone();
                        i += 1;
                        v
                    };
                    f.values.push((l.to_string(), v));
                    break;
                } else {
                    return Err(format!("unsupported flag -{c}"));
                }
            }
        } else {
            f.pos.push(a.clone());
        }
    }
    Ok(f)
}

const PS_FILTER_KEYS: &[&str] = &["status", "name", "label", "ancestor"];

/// Rozebere argumenty shimu. `Err` = použij wsl.exe.
pub fn parse(args: &[String]) -> Result<Cmd, String> {
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let (kind, rest) = match a.as_slice() {
        ["ps", ..] => ("ps", &args[1..]),
        ["container", "ls" | "list" | "ps", ..] => ("ps", &args[2..]),
        ["images", ..] => ("images", &args[1..]),
        ["image", "ls" | "list", ..] => ("images", &args[2..]),
        ["volume", "ls" | "list", ..] => ("volumes", &args[2..]),
        ["network", "ls" | "list", ..] => ("networks", &args[2..]),
        ["version", ..] => ("version", &args[1..]),
        ["info", ..] => ("info", &args[1..]),
        ["system", "info", ..] => ("info", &args[2..]),
        ["inspect", ..] => ("inspect", &args[1..]),
        ["container", "inspect", ..] => ("container inspect", &args[2..]),
        ["image", "inspect", ..] => ("image inspect", &args[2..]),
        ["logs", ..] => ("logs", &args[1..]),
        ["container", "logs", ..] => ("logs", &args[2..]),
        _ => return Err("command not handled by the fast path".into()),
    };
    // --filter klíč=hodnota; povolené klíče se předají API beze změny (jako Docker CLI)
    let filters = |fl: &Flags, keys: &[&str]| -> Result<Vec<(String, String)>, String> {
        let mut out = Vec::new();
        for (_, v) in fl.values.iter().filter(|(k, _)| k == "filter") {
            let (fk, fv) = v.split_once('=').ok_or("bad filter")?;
            let fk = fk.trim().to_ascii_lowercase();
            if !keys.contains(&fk.as_str()) {
                return Err(format!("unsupported filter {fk}"));
            }
            out.push((fk, fv.trim().to_string()));
        }
        Ok(out)
    };
    match kind {
        "ps" => {
            let fl = parse_flags(
                rest,
                &[(Some('a'), "all"), (Some('q'), "quiet"), (Some('l'), "latest"), (Some('s'), "size"), (None, "no-trunc")],
                &[(Some('n'), "last"), (Some('f'), "filter"), (None, "format")],
            )?;
            let mut limit = match fl.last("last") {
                Some(v) => Some(v.trim().parse::<i64>().map_err(|_| format!("bad --last {v:?}"))?),
                None => None,
            };
            if fl.has("latest") && limit.is_none_or(|l| l == -1) {
                limit = Some(1);
            }
            let limit = limit.filter(|l| *l > 0);
            let filters = filters(&fl, PS_FILTER_KEYS)?;
            let fmt = resolve_format(fl.last("format"), fl.has("quiet"), &format!("{PS_TABLE}{}", if fl.has("size") { r"\t{{.Size}}" } else { "" }), "{{.ID}}", PS_HEADERS, true)?;
            let size = fl.has("size") || fmt.uses("Size");
            Ok(Cmd::Ps { all: fl.has("all") || limit.is_some(), size, limit, filters, trunc: !fl.has("no-trunc"), fmt })
        }
        "images" => {
            let fl = parse_flags(rest, &[(Some('a'), "all"), (Some('q'), "quiet"), (None, "no-trunc")], &[(None, "format"), (Some('f'), "filter")])?;
            let filters = filters(&fl, &["dangling", "reference", "label"])?;
            let fmt = resolve_format(fl.last("format"), fl.has("quiet"), IMAGE_TABLE, "{{.ID}}", IMAGE_HEADERS, true)?;
            Ok(Cmd::Images { all: fl.has("all"), trunc: !fl.has("no-trunc"), filters, fmt })
        }
        "volumes" => {
            let fl = parse_flags(rest, &[(Some('q'), "quiet")], &[(None, "format"), (Some('f'), "filter")])?;
            let filters = filters(&fl, &["dangling", "name", "label", "driver"])?;
            let fmt = resolve_format(fl.last("format"), fl.has("quiet"), VOLUME_TABLE, "{{.Name}}", VOLUME_HEADERS, true)?;
            Ok(Cmd::Volumes { filters, fmt })
        }
        "networks" => {
            let fl = parse_flags(rest, &[(Some('q'), "quiet"), (None, "no-trunc")], &[(None, "format"), (Some('f'), "filter")])?;
            let filters = filters(&fl, &["name", "driver", "id"])?;
            let fmt = resolve_format(fl.last("format"), fl.has("quiet"), NETWORK_TABLE, "{{.ID}}", NETWORK_HEADERS, true)?;
            Ok(Cmd::Networks { trunc: !fl.has("no-trunc"), filters, fmt })
        }
        "inspect" | "container inspect" | "image inspect" => {
            let fl = parse_flags_pos(rest, &[(Some('s'), "size")], &[(Some('f'), "format"), (None, "type")])?;
            if fl.pos.is_empty() {
                return Err("inspect needs a name".into());
            }
            let mut kind = match kind {
                "container inspect" => InspectKind::Container,
                "image inspect" => InspectKind::Image,
                _ => InspectKind::Any,
            };
            if let Some(t) = fl.last("type") {
                kind = match (kind, t) {
                    (InspectKind::Any, "") => InspectKind::Any,
                    (InspectKind::Any, "container") => InspectKind::Container,
                    (InspectKind::Any, "image") => InspectKind::Image,
                    _ => return Err(format!("unsupported --type {t}")),
                };
            }
            // inspect šablonu nijak neupravuje (`\t` zůstává doslova, jako v Docker CLI)
            let tmpl = match fl.last("format") {
                None | Some("") => None,
                Some(f) if f == "json" || f.starts_with("table") => return Err("format not supported".into()),
                Some(f) => Some(parse_template(f)?),
            };
            Ok(Cmd::Inspect { kind, size: fl.has("size"), names: fl.pos, tmpl })
        }
        "logs" => {
            let fl = parse_flags_pos(rest, &[(Some('t'), "timestamps")], &[(Some('n'), "tail"), (None, "since"), (None, "until")])?;
            let [name] = fl.pos.as_slice() else {
                return Err("logs needs exactly one container".into());
            };
            let tail = fl.last("tail").map(|t| t.trim().to_string());
            if let Some(t) = &tail {
                if t != "all" && t.parse::<i64>().is_err() {
                    return Err(format!("bad --tail {t:?}"));
                }
            }
            // --since / --until se převedou až při spuštění (potřebují aktuální čas); ověří se hned
            for k in ["since", "until"] {
                if let Some(v) = fl.last(k) {
                    api_timestamp(v, 0)?;
                }
            }
            Ok(Cmd::Logs {
                name: name.clone(),
                tail,
                timestamps: fl.has("timestamps"),
                since: fl.last("since").map(String::from),
                until: fl.last("until").map(String::from),
            })
        }
        _ => {
            // version / info: jen s jednoduchou šablonou (bez ní je potřeba i klientská část)
            let fl = parse_flags(rest, &[], &[(Some('f'), "format")])?;
            let f = fl.last("format").ok_or("plain output needs the CLI")?;
            if f == "json" || f.starts_with("table") {
                return Err("format not supported".into());
            }
            let segs = parse_template(&f.trim_matches(' ').replace(r"\t", "\t").replace(r"\n", "\n"))?;
            for sg in &segs {
                match sg {
                    Seg::Field(p) if kind == "version" && !is_version_field(p) => {
                        return Err(format!("unsupported field .{}", p.join(".")))
                    }
                    Seg::Field(p) if kind == "info" && p.len() != 1 => return Err(format!("unsupported field .{}", p.join("."))),
                    Seg::JsonDot => return Err("{{json .}} not supported".into()),
                    _ => {}
                }
            }
            Ok(if kind == "version" { Cmd::Version(segs) } else { Cmd::Info(segs) })
        }
    }
}

fn is_version_field(p: &[String]) -> bool {
    match p {
        [srv, f] => srv == "Server" && VERSION_SERVER_FIELDS.contains(&f.as_str()),
        [srv, a, b] => srv == "Server" && a == "Platform" && b == "Name",
        _ => false,
    }
}

/// `filters` pro API ve tvaru, který posílá Docker CLI: `{"status":{"running":true}}`.
pub fn filters_json(filters: &[(String, String)]) -> String {
    let mut m: BTreeMap<&str, BTreeMap<&str, bool>> = BTreeMap::new();
    for (k, v) in filters {
        m.entry(k).or_default().insert(v, true);
    }
    serde_json::to_string(&m).unwrap_or_default()
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{:02X}", b),
        })
        .collect()
}

fn with_query(base: &str, q: &[String]) -> String {
    if q.is_empty() { base.to_string() } else { format!("{base}?{}", q.join("&")) }
}

fn filter_query(filters: &[(String, String)]) -> Vec<String> {
    if filters.is_empty() { Vec::new() } else { vec![format!("filters={}", enc(&filters_json(filters)))] }
}

/// Název objektu v cestě URL (lomítka a dvojtečky referencí obrazů zůstávají, jako u Go klienta).
fn enc_name(s: &str) -> String {
    s.split('/').map(|p| p.split(':').map(enc).collect::<Vec<_>>().join(":")).collect::<Vec<_>>().join("/")
}

fn by_name(a: &Value, b: &Value) -> std::cmp::Ordering {
    if natural_less(s(a, "Name"), s(b, "Name")) {
        std::cmp::Ordering::Less
    } else if natural_less(s(b, "Name"), s(a, "Name")) {
        std::cmp::Ordering::Greater
    } else {
        std::cmp::Ordering::Equal
    }
}

impl Cmd {
    /// Cesta API pro příkazy s jedním dotazem (seznamy, version, info).
    pub fn api_path(&self) -> String {
        match self {
            Cmd::Ps { all, size, limit, filters, .. } => {
                let mut q = Vec::new();
                if *all {
                    q.push("all=1".to_string());
                }
                if let Some(l) = limit {
                    q.push(format!("limit={l}"));
                }
                if *size {
                    q.push("size=1".into());
                }
                q.extend(filter_query(filters));
                with_query("/containers/json", &q)
            }
            Cmd::Images { all, filters, .. } => {
                let mut q = Vec::new();
                if *all {
                    q.push("all=1".to_string());
                }
                q.extend(filter_query(filters));
                with_query("/images/json", &q)
            }
            Cmd::Volumes { filters, .. } => with_query("/volumes", &filter_query(filters)),
            Cmd::Networks { filters, .. } => with_query("/networks", &filter_query(filters)),
            Cmd::Version(_) => "/version".into(),
            Cmd::Info(_) => "/info".into(),
            Cmd::Inspect { .. } | Cmd::Logs { .. } => String::new(),
        }
    }

    /// Z odpovědi API sestaví výstup (stdout). `Err` = použij wsl.exe.
    pub fn output(&self, body: &Value, now: i64, tz: TzFn) -> Result<String, String> {
        let arr = |v: &Value| v.as_array().cloned().ok_or_else(|| "unexpected API response".to_string());
        match self {
            Cmd::Ps { trunc, fmt, .. } => {
                let mut rows = arr(body)?;
                rows.sort_by_key(|c| std::cmp::Reverse(n(c, "Created")));
                let x = Ctx { trunc: *trunc, now, tz };
                Ok(emit(fmt, PS_HEADERS, &rows, |c, f| ps_field(c, f, &x)))
            }
            Cmd::Images { trunc, fmt, .. } => {
                let rows = image_rows(&arr(body)?);
                let x = Ctx { trunc: *trunc, now, tz };
                Ok(emit(fmt, IMAGE_HEADERS, &rows, |r, f| image_field(r, f, &x)))
            }
            Cmd::Volumes { fmt, .. } => {
                let mut rows = match body.get("Volumes") {
                    Some(Value::Null) | None => Vec::new(),
                    Some(v) => arr(v)?,
                };
                rows.sort_by(by_name);
                Ok(emit(fmt, VOLUME_HEADERS, &rows, volume_field))
            }
            Cmd::Networks { trunc, fmt, .. } => {
                let mut rows = arr(body)?;
                rows.sort_by(by_name);
                let x = Ctx { trunc: *trunc, now, tz };
                Ok(emit(fmt, NETWORK_HEADERS, &rows, |v, f| network_field(v, f, &x)))
            }
            Cmd::Version(segs) => render_single(segs, |p| version_lookup(body, p)),
            Cmd::Info(segs) => render_single(segs, |p| info_lookup(body, p)),
            Cmd::Inspect { .. } | Cmd::Logs { .. } => Err("needs execute()".into()),
        }
    }
}

// ================================================================ inspect

/// Go `json.Indent(dst, src, "", indent)`: přeformátuje JSON beze změny řetězců
/// (escapování z API zůstává), prázdné `{}` / `[]` zůstanou na jednom řádku.
/// `indent == None` = kompaktní výstup (bez mezer).
pub fn json_reformat(src: &[u8], indent: Option<&str>) -> Result<String, String> {
    let mut out: Vec<u8> = Vec::with_capacity(src.len() * 2);
    let mut depth = 0usize;
    let mut need_indent = false;
    let newline = |out: &mut Vec<u8>, depth: usize| {
        if let Some(ind) = indent {
            out.push(b'\n');
            for _ in 0..depth {
                out.extend_from_slice(ind.as_bytes());
            }
        }
    };
    let mut i = 0;
    while i < src.len() {
        let c = src[i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' => {
                i += 1;
                continue;
            }
            b'{' | b'[' => {
                if need_indent {
                    newline(&mut out, depth);
                }
                out.push(c);
                depth += 1;
                need_indent = true;
            }
            b'}' | b']' => {
                depth = depth.checked_sub(1).ok_or("bad JSON")?;
                if need_indent {
                    need_indent = false;
                } else {
                    newline(&mut out, depth);
                }
                out.push(c);
            }
            b',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            b':' => {
                out.push(c);
                if indent.is_some() {
                    out.push(b' ');
                }
            }
            _ => {
                if need_indent {
                    newline(&mut out, depth);
                    need_indent = false;
                }
                if c == b'"' {
                    // řetězec beze změny
                    let start = i;
                    i += 1;
                    while i < src.len() && src[i] != b'"' {
                        if src[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                    if i >= src.len() {
                        return Err("unterminated string".into());
                    }
                    out.extend_from_slice(&src[start..=i]);
                } else {
                    out.push(c);
                }
            }
        }
        i += 1;
    }
    if depth != 0 {
        return Err("bad JSON".into());
    }
    String::from_utf8(out).map_err(|_| "bad UTF-8".into())
}

/// Hodnota pro `{{.A.B}}` v šabloně inspect: jen skaláry (řetězec, bool, celé číslo),
/// jinak None (Go by vypsal `map[…]`, `[…]`, `<nil>` – to nechá na skutečném CLI).
fn inspect_lookup(v: &Value, path: &[String]) -> Option<String> {
    let mut cur = v;
    for (i, k) in path.iter().enumerate() {
        let obj = cur.as_object()?;
        cur = match obj.get(k.as_str()) {
            Some(x) => x,
            // typovaná struktura Dockeru má `ID` (JSON `Id`)
            None if i == 0 && k == "ID" => obj.get("Id")?,
            None => return None,
        };
    }
    scalar(cur)
}

/// Výstup `docker inspect` z „raw“ JSON objektů (jak je vrátilo API).
pub fn inspect_output(raws: &[Vec<u8>], tmpl: Option<&[Seg]>) -> Result<String, String> {
    match tmpl {
        None => {
            if raws.is_empty() {
                return Ok("[]\n".into());
            }
            let mut joined = Vec::new();
            joined.push(b'[');
            for (i, r) in raws.iter().enumerate() {
                if i > 0 {
                    joined.push(b',');
                }
                joined.extend_from_slice(r);
            }
            joined.push(b']');
            Ok(json_reformat(&joined, Some("    "))? + "\n")
        }
        Some(segs) => {
            let mut out = String::new();
            for r in raws {
                let v: Value = serde_json::from_slice(r).map_err(|e| format!("bad JSON: {e}"))?;
                for seg in segs {
                    match seg {
                        Seg::Lit(l) => out.push_str(l),
                        Seg::JsonDot => out.push_str(&json_reformat(r, None)?),
                        Seg::Field(p) => out.push_str(&inspect_lookup(&v, p).ok_or_else(|| format!("unsupported field .{}", p.join(".")))?),
                    }
                }
                out.push('\n');
            }
            Ok(out)
        }
    }
}

// ================================================================ logs

/// Go `time.ParseDuration` -> nanosekundy (bez přetečení pro rozumné hodnoty).
pub fn go_duration_ns(v: &str) -> Option<i128> {
    let (neg, mut rest) = match v.as_bytes().first()? {
        b'-' => (true, &v[1..]),
        b'+' => (false, &v[1..]),
        _ => (false, v),
    };
    if rest == "0" {
        return Some(0);
    }
    if rest.is_empty() {
        return None;
    }
    let mut total: f64 = 0.0;
    while !rest.is_empty() {
        let num_end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
        let num = &rest[..num_end];
        if num.is_empty() || num == "." {
            return None;
        }
        let x: f64 = num.parse().ok()?;
        rest = &rest[num_end..];
        let unit_end = rest.find(|c: char| c.is_ascii_digit() || c == '.').unwrap_or(rest.len());
        let mult = match &rest[..unit_end] {
            "ns" => 1.0,
            "us" | "µs" | "μs" => 1e3,
            "ms" => 1e6,
            "s" => 1e9,
            "m" => 60e9,
            "h" => 3600e9,
            _ => return None,
        };
        rest = &rest[unit_end..];
        total += x * mult;
    }
    let ns = total.round() as i128;
    Some(if neg { -ns } else { ns })
}

/// `timetypes.GetTimestamp` z Docker CLI pro `--since` / `--until`: doba trvání (`10m`)
/// -> unixové sekundy před `now_ns`, číslo (`1700000000[.5]`) beze změny.
/// Data (RFC 3339 apod.) -> Err (použije se CLI).
pub fn api_timestamp(v: &str, now_ns: i128) -> Result<String, String> {
    if v != "0" {
        if let Some(d) = go_duration_ns(v) {
            return Ok((now_ns - d).div_euclid(1_000_000_000).to_string());
        }
    }
    let (secs, frac) = v.split_once('.').unwrap_or((v, ""));
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
    if digits(secs) && (frac.is_empty() && !v.ends_with('.') || digits(frac)) {
        return Ok(v.to_string());
    }
    Err(format!("timestamp {v:?} not supported by the fast path"))
}

/// Rozdělí multiplexovaný stream logů (8bajtové hlavičky) na (1 = stdout, 2 = stderr, data).
pub fn demux(mut b: &[u8]) -> Result<Vec<(u8, Vec<u8>)>, String> {
    let mut out: Vec<(u8, Vec<u8>)> = Vec::new();
    while !b.is_empty() {
        if b.len() < 8 {
            return Err("truncated log frame header".into());
        }
        let stream = match b[0] {
            0 | 1 => 1,
            2 => 2,
            _ => return Err("log stream error frame".into()),
        };
        let size = u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as usize;
        if b.len() < 8 + size {
            return Err("truncated log frame".into());
        }
        let data = &b[8..8 + size];
        match out.last_mut() {
            Some((st, d)) if *st == stream => d.extend_from_slice(data),
            _ => out.push((stream, data.to_vec())),
        }
        b = &b[8 + size..];
    }
    Ok(out)
}

// ================================================================ spuštění

/// Výstup rychlé cesty: úseky pro stdout (1) / stderr (2) v pořadí + dotazy API (pro ladění).
#[derive(Debug, Default, PartialEq)]
pub struct Output {
    pub chunks: Vec<(u8, Vec<u8>)>,
    pub paths: Vec<String>,
}

/// Provede příkaz; `fetch(cesta)` vrací tělo odpovědi při HTTP 200, jinak Err.
pub fn execute(cmd: &Cmd, fetch: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>, now_ns: i128, tz: TzFn) -> Result<Output, String> {
    let mut paths = Vec::new();
    let mut get = |p: String| {
        let r = fetch(&p);
        paths.push(p);
        r
    };
    let chunks = match cmd {
        Cmd::Inspect { kind, size, names, tmpl } => {
            let mut raws = Vec::new();
            for name in names {
                let kinds: &[InspectKind] = match kind {
                    InspectKind::Any => &[InspectKind::Container, InspectKind::Image],
                    k => std::slice::from_ref(k),
                };
                let mut found = None;
                for k in kinds {
                    let p = match k {
                        InspectKind::Image => format!("/images/{}/json", enc_name(name)),
                        _ => format!("/containers/{}/json{}", enc_name(name), if *size { "?size=1" } else { "" }),
                    };
                    if let Ok(b) = get(p) {
                        found = Some(b);
                        break;
                    }
                }
                // neexistující objekt: chybovou hlášku a návratový kód ať vypíše CLI
                raws.push(found.ok_or_else(|| format!("no such object {name:?}"))?);
            }
            vec![(1, inspect_output(&raws, tmpl.as_deref())?.into_bytes())]
        }
        Cmd::Logs { name, tail, timestamps, since, until } => {
            let info: Value = serde_json::from_slice(&get(format!("/containers/{}/json", enc_name(name)))?).map_err(|e| format!("bad JSON: {e}"))?;
            let id = s(&info, "Id");
            if id.is_empty() {
                return Err("container without Id".into());
            }
            let tty = info.get("Config").and_then(|c| c.get("Tty")).and_then(|t| t.as_bool()).unwrap_or(false);
            let mut q = vec!["stdout=1".to_string(), "stderr=1".to_string()];
            if let Some(v) = since {
                q.push(format!("since={}", enc(&api_timestamp(v, now_ns)?)));
            }
            if let Some(v) = until {
                q.push(format!("until={}", enc(&api_timestamp(v, now_ns)?)));
            }
            if *timestamps {
                q.push("timestamps=1".into());
            }
            q.push(format!("tail={}", enc(tail.as_deref().unwrap_or("all"))));
            let body = get(with_query(&format!("/containers/{id}/logs"), &q))?;
            if tty {
                vec![(1, body)]
            } else {
                demux(&body)?
            }
        }
        _ => {
            let body: Value = serde_json::from_slice(&get(cmd.api_path())?).map_err(|e| format!("bad JSON: {e}"))?;
            let now = now_ns.div_euclid(1_000_000_000) as i64;
            vec![(1, cmd.output(&body, now, tz)?.into_bytes())]
        }
    };
    Ok(Output { chunks, paths })
}

// ================================================================ HTTP

fn find(h: &[u8], needle: &[u8]) -> Option<usize> {
    h.windows(needle.len()).position(|w| w == needle)
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let e = find(b, b"\r\n").ok_or("bad chunk")?;
        let line = std::str::from_utf8(&b[..e]).map_err(|_| "bad chunk")?;
        let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| "bad chunk size")?;
        b = &b[e + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size + 2 {
            return Err("truncated chunk".into());
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

/// Rozebere odpověď HTTP/1.1; tělo jen při 200.
pub fn parse_http(raw: &[u8]) -> Result<Vec<u8>, String> {
    let he = find(raw, b"\r\n\r\n").ok_or("incomplete HTTP response")?;
    let head = std::str::from_utf8(&raw[..he]).map_err(|_| "bad HTTP header")?;
    let mut lines = head.split("\r\n");
    let status = lines.next().unwrap_or("");
    let code: u16 = status.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or("bad status line")?;
    if code != 200 {
        return Err(format!("HTTP {code}"));
    }
    let mut chunked = false;
    let mut len = None;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            } else if k == "content-length" {
                len = v.parse::<usize>().ok();
            }
        }
    }
    let body = &raw[he + 4..];
    if chunked {
        return dechunk(body);
    }
    match len {
        Some(l) if body.len() < l => Err("truncated body".into()),
        Some(l) => Ok(body[..l].to_vec()),
        None => Ok(body.to_vec()),
    }
}

fn read_all(r: &mut impl Read) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16384];
    loop {
        match r.read(&mut chunk) {
            Ok(0) => return Ok(buf),
            Ok(k) => buf.extend_from_slice(&chunk[..k]),
            // TLS proxy (socat) může zavřít bez close_notify – úplnost ověří parse_http
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !buf.is_empty() => return Ok(buf),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// GET na API enginu (http, nebo https s klientským certifikátem); tělo při HTTP 200.
pub fn http_get_raw(port: u16, tls: Option<Arc<rustls::ClientConfig>>, path: &str) -> Result<Vec<u8>, String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_millis(200)).map_err(|e| e.to_string())?;
    let _ = tcp.set_nodelay(true);
    tcp.set_read_timeout(Some(Duration::from_secs(60))).map_err(|e| e.to_string())?;
    tcp.set_write_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: wls-docker-shim\r\nAccept: application/json\r\nConnection: close\r\n\r\n");
    let raw = match tls {
        None => {
            let mut t = tcp;
            t.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
            read_all(&mut t)?
        }
        Some(cfg) => {
            let name = rustls_pki_types::ServerName::IpAddress(std::net::Ipv4Addr::LOCALHOST.into());
            let conn = rustls::ClientConnection::new(cfg, name).map_err(|e| e.to_string())?;
            let mut t = rustls::StreamOwned::new(conn, tcp);
            t.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
            t.flush().map_err(|e| e.to_string())?;
            read_all(&mut t)?
        }
    };
    parse_http(&raw)
}

/// GET s JSON odpovědí.
#[cfg(test)]
pub fn http_get(port: u16, tls: Option<Arc<rustls::ClientConfig>>, path: &str) -> Result<Value, String> {
    let body = http_get_raw(port, tls, path)?;
    serde_json::from_slice(&body).map_err(|e| format!("bad JSON: {e}"))
}

/// Celá rychlá cesta: rozbor, dotazy na API, výstup. `Err(důvod)` = použij wsl.exe.
pub fn try_run(args: &[String], port: u16, tls: bool) -> Result<Output, String> {
    let cmd = parse(args)?;
    let tls = if tls {
        let dir = crate::apitls::tls_dir().ok_or("APPDATA not set")?;
        Some(Arc::new(crate::apitls::tls_config(&dir)?))
    } else {
        None
    };
    let now_ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0);
    execute(&cmd, &mut |p| http_get_raw(port, tls.clone(), p), now_ns, local_tz)
}

// ================================================================ testy

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }
    fn utc(_: i64) -> (i32, String) {
        (0, "UTC".into())
    }
    fn cet(_: i64) -> (i32, String) {
        (3600, "CET".into())
    }
    const NOW: i64 = 1_795_030_000;

    fn containers() -> Value {
        json!([
            {"Id":"2a1a08b4b90a3044e18e9a9db36da2037fddc13ac75c6d4af2ad26f3b8fae054","Names":["/web"],
             "Image":"docker.io/library/nginx:latest","ImageID":"sha256:c05939e13a61aaaa","Command":"/docker-entrypoint.sh nginx -g 'daemon off;'",
             "Created": NOW - 3 * 3600,"Ports":[{"IP":"0.0.0.0","PrivatePort":80,"PublicPort":8080,"Type":"tcp"},{"IP":"::","PrivatePort":80,"PublicPort":8080,"Type":"tcp"}],
             "Labels":{"b":"2","a":"1"},"State":"running","Status":"Up 3 hours",
             "NetworkSettings":{"Networks":{"podman":{},"backend":{}}},"Mounts":[{"Name":"data","Source":"/var/lib/x","Driver":"local"}]},
            {"Id":"502bc544a072089c43ca27a9828509883cf74858d4a77019d49f6b3ade739f42","Names":["/db"],
             "Image":"redis","ImageID":"sha256:9782","Command":"redis-server",
             "Created": NOW - 90,"Ports":[{"PrivatePort":6379,"Type":"tcp"}],"Labels":{},"State":"exited","Status":"Exited (0) 1 minute ago",
             "NetworkSettings":{"Networks":{}},"Mounts":[]}
        ])
    }

    #[test]
    fn ps_default_table() {
        let cmd = parse(&a(&["ps", "-a"])).unwrap();
        let out = cmd.output(&containers(), NOW, utc).unwrap();
        let exp = "\
CONTAINER ID   IMAGE          COMMAND                  CREATED              STATUS                    PORTS                                     NAMES
502bc544a072   redis          \"redis-server\"           About a minute ago   Exited (0) 1 minute ago   6379/tcp                                  db
2a1a08b4b90a   nginx:latest   \"/docker-entrypoint.…\"   3 hours ago          Up 3 hours                0.0.0.0:8080->80/tcp, [::]:8080->80/tcp   web
";
        assert_eq!(out, exp);
    }

    #[test]
    fn tabwriter_minwidth_and_trailing_cell() {
        assert_eq!(tabwrite("A\tBB\tC\nlonger value\tx\ty\n", 10, 3), "A              BB        C\nlonger value   x         y\n");
        // prázdný sloupec má minimální šířku
        assert_eq!(tabwrite("PORTS\tNAMES\n\tx\n", 10, 3), "PORTS     NAMES\n          x\n");
    }

    #[test]
    fn ps_quiet_size_and_no_trunc() {
        let c = containers();
        assert_eq!(parse(&a(&["ps", "-aq"])).unwrap().output(&c, NOW, utc).unwrap(), "502bc544a072\n2a1a08b4b90a\n");
        let full = parse(&a(&["container", "ls", "--all", "--quiet", "--no-trunc"])).unwrap().output(&c, NOW, utc).unwrap();
        assert!(full.starts_with("502bc544a072089c43ca27a9828509883cf74858d4a77019d49f6b3ade739f42\n"));
        let cmd = parse(&a(&["ps", "-s"])).unwrap();
        assert!(matches!(&cmd, Cmd::Ps { size: true, .. }));
        let mut c2 = c.clone();
        c2[0]["SizeRw"] = json!(2);
        c2[0]["SizeRootFs"] = json!(187_000_000);
        let out = cmd.output(&c2, NOW, utc).unwrap();
        assert!(out.lines().next().unwrap().ends_with("NAMES     SIZE"), "{out}");
        assert!(out.contains("2B (virtual 187MB)"), "{out}");
        assert!(out.contains("0B\n"), "{out}");
    }

    #[test]
    fn go_templates() {
        let c = containers();
        let out = parse(&a(&["ps", "-a", "--format", r"{{.Names}}\t{{.Status}}"])).unwrap().output(&c, NOW, utc).unwrap();
        assert_eq!(out, "db\tExited (0) 1 minute ago\nweb\tUp 3 hours\n");
        let out = parse(&a(&["ps", "--format=table {{.Names}}: {{.State}}"])).unwrap().output(&c, NOW, utc).unwrap();
        assert_eq!(out, "NAMES: STATE\ndb: exited\nweb: running\n");
        let out = parse(&a(&["ps", "--format", r"table {{.ID}}\t{{.Labels}}\t{{.Networks}}\t{{.Mounts}}\t{{.LocalVolumes}}"])).unwrap().output(&c, NOW, utc).unwrap();
        assert_eq!(
            out,
            format!(
                "CONTAINER ID   LABELS    NETWORKS         MOUNTS    LOCAL VOLUMES\n502bc544a072{}0\n2a1a08b4b90a   a=1,b=2   backend,podman   data      1\n",
                " ".repeat(3 + 10 + 17 + 10)
            )
        );
        let out = parse(&a(&["ps", "--format", "{{.CreatedAt}}|{{.Command}}|{{.Image}}"])).unwrap().output(&c, NOW, cet).unwrap();
        assert_eq!(out.lines().nth(1).unwrap(), "2026-11-18 17:26:40 +0100 CET|\"/docker-entrypoint.…\"|nginx:latest");
        // --no-trunc: celý příkaz i obraz
        let out = parse(&a(&["ps", "--no-trunc", "--format", "{{.Command}} {{.Image}}"])).unwrap().output(&c, NOW, utc).unwrap();
        assert_eq!(out.lines().nth(1).unwrap(), "\"/docker-entrypoint.sh nginx -g 'daemon off;'\" docker.io/library/nginx:latest");
    }

    #[test]
    fn ps_json() {
        let out = parse(&a(&["ps", "--format", "json"])).unwrap().output(&containers(), NOW, utc).unwrap();
        let first = out.lines().next().unwrap();
        assert!(first.starts_with("{\"Command\":\"\\\"redis-server\\\"\",\"CreatedAt\":"), "{first}");
        assert!(first.contains("\"Size\":\"0B\",\"State\":\"exited\""), "{first}");
        let v: Value = serde_json::from_str(first).unwrap();
        assert_eq!(v["Names"], "db");
        assert_eq!(v.as_object().unwrap().len(), PS_HEADERS.len());
        assert_eq!(parse(&a(&["ps", "--format", "{{json .}}"])).unwrap().output(&containers(), NOW, utc).unwrap(), out);
        let esc = go_json_str("a<b>&\"\n");
        assert!(!esc.contains('<') && !esc.contains('>') && !esc.contains('&'), "{esc}");
        assert_eq!(esc.matches("u00").count(), 3, "{esc}");
        assert!(esc.ends_with("\\\"\\n\""), "{esc}");
    }

    #[test]
    fn sizes_and_durations() {
        assert_eq!(human_size(148_588_077.0, 3), "149MB");
        assert_eq!(human_size(8_104_044.0, 3), "8.1MB");
        assert_eq!(human_size(0.0, 3), "0B");
        assert_eq!(human_size(999.9, 3), "1e+03B");
        assert_eq!(human_size(1_234_567_890.0, 3), "1.23GB");
        assert_eq!(go_g(0.0001234, 3), "0.000123");
        assert_eq!(human_duration(0), "Less than a second");
        assert_eq!(human_duration(45), "45 seconds");
        assert_eq!(human_duration(90), "About a minute");
        assert_eq!(human_duration(3599), "59 minutes");
        assert_eq!(human_duration(5400), "2 hours");
        assert_eq!(human_duration(13 * 86400), "13 days");
        assert_eq!(human_duration(20 * 86400), "2 weeks");
        assert_eq!(human_duration(200 * 86400), "6 months");
        assert_eq!(human_duration(800 * 86400), "2 years");
        assert_eq!(go_time_string(0, utc), "1970-01-01 00:00:00 +0000 UTC");
        assert_eq!(days_from_civil(2026, 11, 18), 20775);
        assert_eq!(civil_from_days(20775), (2026, 11, 18));
        assert_eq!(zone_abbr("Central Europe Standard Time", true), "CEST");
        assert_eq!(zone_abbr("Foo Standard Time", false), "FST");
    }

    #[test]
    fn ports_grouping() {
        let p = json!([
            {"PrivatePort":8001,"Type":"tcp"},{"PrivatePort":8000,"Type":"tcp"},{"PrivatePort":8002,"Type":"tcp"},
            {"PrivatePort":53,"Type":"udp"},
            {"IP":"127.0.0.1","PrivatePort":9000,"PublicPort":9000,"Type":"tcp"}
        ]);
        assert_eq!(displayable_ports(p.as_array().unwrap()), "53/udp, 8000-8002/tcp, 127.0.0.1:9000->9000/tcp");
    }

    #[test]
    fn images_table_and_rows() {
        let imgs = json!([
            {"Id":"sha256:c05939e13a616cd46ade","RepoTags":["docker.io/library/nginx:latest"],"RepoDigests":["docker.io/library/nginx@sha256:1"],"Created":NOW - 13*86400,"Size":166_931_294,"Containers":1},
            {"Id":"sha256:978293674c6b207a799a","RepoTags":["docker.io/library/redis:latest","ghcr.io/me/redis:7"],"Created":NOW - 13*86400 + 1000,"Size":148_588_077},
            {"Id":"sha256:aaaaaaaaaaaabbbb","RepoTags":null,"RepoDigests":["quay.io/x/y@sha256:2"],"Created":NOW - 60,"Size":1000},
            {"Id":"sha256:ddddddddddddeeee","RepoTags":["<none>:<none>"],"RepoDigests":["<none>@<none>"],"Created":0,"Size":5}
        ]);
        let out = parse(&a(&["images"])).unwrap().output(&imgs, NOW, utc).unwrap();
        let exp = "\
REPOSITORY        TAG       IMAGE ID       CREATED              SIZE
quay.io/x/y       <none>    aaaaaaaaaaaa   About a minute ago   1kB
redis             latest    978293674c6b   13 days ago          149MB
ghcr.io/me/redis  7         978293674c6b   13 days ago          149MB
nginx             latest    c05939e13a61   13 days ago          167MB
<none>            <none>    dddddddddddd   N/A                  5B
";
        // tabwriter: šířka = nejdelší + 3
        let exp = exp.replace("ghcr.io/me/redis  7", "ghcr.io/me/redis   7").replace("REPOSITORY        TAG", "REPOSITORY         TAG").replace("quay.io/x/y       <none>", "quay.io/x/y        <none>").replace("redis             latest", "redis              latest").replace("nginx             latest", "nginx              latest").replace("<none>            <none>", "<none>             <none>");
        assert_eq!(out, exp);
        let q = parse(&a(&["image", "ls", "-q"])).unwrap().output(&imgs, NOW, utc).unwrap();
        assert_eq!(q.lines().count(), 5);
        let nt = parse(&a(&["images", "--no-trunc", "--format", "{{.Repository}}:{{.Tag}} {{.ID}}"])).unwrap().output(&imgs, NOW, utc).unwrap();
        assert!(nt.contains("nginx:latest sha256:c05939e13a616cd46ade\n"), "{nt}");
    }

    #[test]
    fn volumes_and_networks() {
        let vols = json!({"Volumes":[{"Name":"v10","Driver":"local"},{"Name":"v9","Driver":"local"},{"Name":"a","Driver":"nfs"}]});
        let out = parse(&a(&["volume", "ls"])).unwrap().output(&vols, NOW, utc).unwrap();
        assert_eq!(out, "DRIVER    VOLUME NAME\nnfs       a\nlocal     v9\nlocal     v10\n");
        assert_eq!(parse(&a(&["volume", "ls", "-q"])).unwrap().output(&vols, NOW, utc).unwrap(), "a\nv9\nv10\n");
        let nets = json!([{"Name":"podman","Id":"2f259bab93aaaaa2542ba43ef","Driver":"bridge","Scope":"local","Internal":false}]);
        let out = parse(&a(&["network", "ls"])).unwrap().output(&nets, NOW, utc).unwrap();
        assert_eq!(out, "NETWORK ID     NAME      DRIVER    SCOPE\n2f259bab93aa   podman    bridge    local\n");
        let out = parse(&a(&["network", "ls", "--format", "{{.Name}} {{.Internal}} {{.IPv6}}"])).unwrap().output(&nets, NOW, utc).unwrap();
        assert_eq!(out, "podman false false\n");
    }

    #[test]
    fn version_and_info() {
        let v = json!({"Version":"5.4.2","ApiVersion":"1.41","Os":"linux","Platform":{"Name":"linux/amd64"}});
        let out = parse(&a(&["version", "--format", "{{.Server.Version}} {{.Server.APIVersion}} {{.Server.Platform.Name}}"])).unwrap().output(&v, NOW, utc).unwrap();
        assert_eq!(out, "5.4.2 1.41 linux/amd64\n");
        let i = json!({"ServerVersion":"28.1.1","NCPU":8,"MemoryLimit":true,"Warnings":["x"]});
        assert_eq!(parse(&a(&["info", "-f", "{{.ServerVersion}}/{{.NCPU}}/{{.MemoryLimit}}"])).unwrap().output(&i, NOW, utc).unwrap(), "28.1.1/8/true\n");
        // chybějící nebo neskalární pole -> fallback
        assert!(parse(&a(&["info", "--format", "{{.Warnings}}"])).unwrap().output(&i, NOW, utc).is_err());
        assert!(parse(&a(&["info", "--format", "{{.Nope}}"])).unwrap().output(&i, NOW, utc).is_err());
    }

    #[test]
    fn flags_supported() {
        for args in [
            &["ps"][..],
            &["ps", "-a"],
            &["ps", "-aqs"],
            &["ps", "--all", "--quiet", "--no-trunc", "--size"],
            &["ps", "-n", "3"],
            &["ps", "-n3"],
            &["ps", "--last=3"],
            &["ps", "-l"],
            &["ps", "--format", "table"],
            &["ps", "--format", "json"],
            &["ps", "--format", "{{.ID}}\\t{{.Names}}"],
            &["ps", "-f", "status=running", "--filter", "name=web", "--filter=label=a=b", "-f", "ancestor=nginx"],
            &["container", "ls", "-a"],
            &["images"],
            &["images", "-a", "-q", "--no-trunc"],
            &["image", "ls", "--format", "{{.Repository}}:{{.Tag}}"],
            &["volume", "ls", "-q"],
            &["volume", "ls", "--format", "{{.Name}} {{.Driver}}"],
            &["network", "ls", "--format", "{{.Name}}"],
            &["version", "--format", "{{.Server.Version}}"],
            &["version", "-f", "{{.Server.APIVersion}}"],
            &["info", "--format", "{{.ServerVersion}}"],
            &["system", "info", "-f", "{{.NCPU}}"],
            &["images", "--filter", "dangling=true", "-f", "reference=ngi*", "--filter=label=a=b"],
            &["images", "--format", "json"],
            &["volume", "ls", "-f", "dangling=true", "--filter", "name=x", "-f", "label=a", "-f", "driver=local"],
            &["volume", "ls", "--format", "{{.Size}} {{.Links}}"],
            &["network", "ls", "--filter", "driver=bridge", "-f", "name=x", "-f", "id=abc", "--format", "json"],
            &["inspect", "web"],
            &["inspect", "-s", "--type=container", "web", "db"],
            &["container", "inspect", "--format", "{{.State.Status}}", "web"],
            &["image", "inspect", "-f", "{{json .}}", "nginx"],
            &["logs", "web"],
            &["logs", "-n", "5", "-t", "--since", "10m", "--until", "1700000000.5", "web"],
            &["container", "logs", "--tail=all", "web"],
        ] {
            assert!(parse(&a(args)).is_ok(), "{args:?}: {:?}", parse(&a(args)));
        }
    }

    #[test]
    fn flags_fallback() {
        for args in [
            &[][..],
            &["run", "nginx"],
            &["--log-level", "debug", "ps"],
            &["ps", "--help"],
            &["ps", "-x"],
            &["ps", "web"],
            &["ps", "--filter", "health=healthy"],
            &["ps", "--filter", "status"],
            &["ps", "--format", "{{.Label \"x\"}}"],
            &["ps", "--format", "{{upper .Names}}"],
            &["ps", "--format", "{{range .}}{{end}}"],
            &["ps", "--format", "{{.Nope}}"],
            &["ps", "--format", "{{- .Names}}"],
            &["ps", "--format", "{{.Names"],
            &["ps", "-n"],
            &["ps", "-n", "x"],
            &["ps", "--"],
            &["images", "nginx"],
            &["images", "--digests"],
            &["images", "--format", "{{.Foo}}"],
            &["volume", "inspect", "x"],
            &["images", "--filter", "before=nginx"],
            &["volume", "ls", "--filter", "scope=local"],
            &["network", "ls", "--filter", "type=custom"],
            &["inspect"],
            &["inspect", "--type", "volume", "x"],
            &["inspect", "--format", "json", "x"],
            &["inspect", "-f", "{{index .Config.Labels \"a\"}}", "x"],
            &["image", "inspect", "--platform", "linux/amd64", "x"],
            &["logs", "-f", "web"],
            &["logs", "--follow", "web"],
            &["logs", "--details", "web"],
            &["logs"],
            &["logs", "a", "b"],
            &["logs", "--tail", "x", "web"],
            &["logs", "--since", "2024-01-01T00:00:00Z", "web"],
            &["version"],
            &["version", "--format", "{{.Client.Version}}"],
            &["version", "--format", "json"],
            &["info"],
            &["info", "--format", "{{json .}}"],
            &["info", "--format", "{{.Plugins.Volume}}"],
            &["compose", "ps"],
        ] {
            assert!(parse(&a(args)).is_err(), "{args:?} should fall back");
        }
    }

    #[test]
    fn filters_and_api_path() {
        let f = vec![("status".to_string(), "running".to_string()), ("label".into(), "a=b".into()), ("status".into(), "exited".into())];
        assert_eq!(filters_json(&f), r#"{"label":{"a=b":true},"status":{"exited":true,"running":true}}"#);
        let cmd = parse(&a(&["ps", "-f", "status=running", "-n", "2", "--format", "{{.Size}}"])).unwrap();
        assert_eq!(cmd.api_path(), "/containers/json?all=1&limit=2&size=1&filters=%7B%22status%22%3A%7B%22running%22%3Atrue%7D%7D");
        assert_eq!(parse(&a(&["ps", "-l"])).unwrap().api_path(), "/containers/json?all=1&limit=1");
        assert_eq!(parse(&a(&["ps"])).unwrap().api_path(), "/containers/json");
        assert_eq!(parse(&a(&["images", "-a"])).unwrap().api_path(), "/images/json?all=1");
    }

    #[test]
    fn references() {
        assert_eq!(familiar_name("docker.io/library/nginx"), "nginx");
        assert_eq!(familiar_name("docker.io/bitnami/redis"), "bitnami/redis");
        assert_eq!(familiar_name("quay.io/podman/stable"), "quay.io/podman/stable");
        assert_eq!(split_ref("localhost:5000/app:1.0"), Some(("localhost:5000/app".into(), Some("1.0".into()), None)));
        assert_eq!(split_ref("<none>:<none>"), None);
        assert_eq!(split_ref("nginx@sha256:abc"), Some(("nginx".into(), None, Some("sha256:abc".into()))));
        assert!(natural_less("v9", "v10") && !natural_less("v10", "v9") && natural_less("a", "b"));
    }

    #[test]
    fn http_parsing() {
        assert_eq!(parse_http(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n[]").unwrap(), b"[]");
        assert_eq!(parse_http(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n[1\r\n1\r\n]\r\n0\r\n\r\n").unwrap(), b"[1]");
        assert!(parse_http(b"HTTP/1.1 500 Internal\r\nContent-Length: 0\r\n\r\n").is_err());
        assert!(parse_http(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n[]").is_err());
    }

    #[test]
    fn images_json_and_filters() {
        let imgs = json!([
            {"Id":"sha256:c05939e13a616cd46ade","RepoTags":["docker.io/library/nginx:latest"],"RepoDigests":["docker.io/library/nginx@sha256:1"],"Created":NOW - 3600,"Size":166_931_294,"SharedSize":0,"Containers":1},
            {"Id":"sha256:aaaaaaaaaaaabbbb","RepoTags":null,"RepoDigests":["quay.io/x/y@sha256:2","quay.io/x/y@sha256:3"],"Created":NOW - 60,"Size":1000,"SharedSize":-1}
        ]);
        let cmd = parse(&a(&["images", "--format", "json"])).unwrap();
        let out = cmd.output(&imgs, NOW, utc).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        // repozitář jen s digesty = jeden řádek s prázdným digestem
        assert_eq!(lines.len(), 2, "{out}");
        assert_eq!(
            lines[0],
            r#"{"Containers":"N/A","CreatedAt":"2026-11-18 19:25:40 +0000 UTC","CreatedSince":"About a minute ago","Digest":"","ID":"aaaaaaaaaaaa","Repository":"quay.io/x/y","SharedSize":"N/A","Size":"1kB","Tag":"\u003cnone\u003e","UniqueSize":"N/A","VirtualSize":"1kB"}"#
        );
        assert!(lines[1].contains(r#""Containers":"1","#) && lines[1].contains(r#""Digest":"\u003cnone\u003e""#) && lines[1].contains(r#""SharedSize":"0B","Size":"167MB","Tag":"latest","UniqueSize":"166.9MB","VirtualSize":"166.9MB"}"#), "{}", lines[1]);
        let cmd = parse(&a(&["images", "-a", "-f", "dangling=true", "--filter", "reference=ngi*"])).unwrap();
        assert_eq!(cmd.api_path(), "/images/json?all=1&filters=%7B%22dangling%22%3A%7B%22true%22%3Atrue%7D%2C%22reference%22%3A%7B%22ngi%2A%22%3Atrue%7D%7D");
    }

    #[test]
    fn volumes_networks_json_and_filters() {
        let vols = json!({"Volumes":[{"Name":"data","Driver":"local","Mountpoint":"/v/data","Labels":{"a":"1"},"Scope":"local"}]});
        let out = parse(&a(&["volume", "ls", "--format", "json"])).unwrap().output(&vols, NOW, utc).unwrap();
        assert_eq!(out, "{\"Availability\":\"N/A\",\"Driver\":\"local\",\"Group\":\"N/A\",\"Labels\":\"a=1\",\"Links\":\"N/A\",\"Mountpoint\":\"/v/data\",\"Name\":\"data\",\"Scope\":\"local\",\"Size\":\"N/A\",\"Status\":\"N/A\"}\n");
        assert_eq!(parse(&a(&["volume", "ls"])).unwrap().output(&json!({"Volumes":null}), NOW, utc).unwrap(), "DRIVER    VOLUME NAME\n");
        let cmd = parse(&a(&["volume", "ls", "-f", "dangling=true", "-f", "driver=local"])).unwrap();
        assert_eq!(cmd.api_path(), "/volumes?filters=%7B%22dangling%22%3A%7B%22true%22%3Atrue%7D%2C%22driver%22%3A%7B%22local%22%3Atrue%7D%7D");
        let nets = json!([{"Name":"bridge","Id":"2f259bab93aaaaa2542ba43ef","Driver":"bridge","Scope":"local","Internal":false,"EnableIPv6":false,"Created":"2026-11-18T20:15:52.769554013+01:00","Labels":{}}]);
        let out = parse(&a(&["network", "ls", "--format", "json"])).unwrap().output(&nets, NOW, cet).unwrap();
        assert_eq!(out, "{\"CreatedAt\":\"2026-11-18 20:15:52.769554013 +0100 CET\",\"Driver\":\"bridge\",\"ID\":\"2f259bab93aa\",\"IPv6\":\"false\",\"Internal\":\"false\",\"Labels\":\"\",\"Name\":\"bridge\",\"Scope\":\"local\"}\n");
        assert_eq!(parse(&a(&["network", "ls", "-f", "name=br"])).unwrap().api_path(), "/networks?filters=%7B%22name%22%3A%7B%22br%22%3Atrue%7D%7D");
        assert_eq!(go_time_rfc3339("2026-11-18T19:15:52Z", cet), "2026-11-18 19:15:52 +0000 UTC");
        assert_eq!(go_time_rfc3339("2026-11-18T19:15:52.500+05:30", cet), "2026-11-18 19:15:52.5 +0530 +0530");
        assert_eq!(go_time_rfc3339("garbage", cet), "garbage");
    }

    #[test]
    fn json_indent_like_go() {
        let raw = br#"{"Id":"abc","Args":[],"Labels":{},"State":{"Running":true,"Pid":7},"Env":["A=\u003cb\u003e","Q=\"x\" , {"],"N":null}"#;
        let exp = "[\n    {\n        \"Id\": \"abc\",\n        \"Args\": [],\n        \"Labels\": {},\n        \"State\": {\n            \"Running\": true,\n            \"Pid\": 7\n        },\n        \"Env\": [\n            \"A=\\u003cb\\u003e\",\n            \"Q=\\\"x\\\" , {\"\n        ],\n        \"N\": null\n    }\n]\n";
        assert_eq!(inspect_output(&[raw.to_vec()], None).unwrap(), exp);
        assert_eq!(inspect_output(&[], None).unwrap(), "[]\n");
        assert_eq!(json_reformat(b" { \"a\" : [ 1 , 2 ] }\n", None).unwrap(), "{\"a\":[1,2]}");
        assert!(json_reformat(b"{\"a\":1", None).is_err());
    }

    #[test]
    fn inspect_execute() {
        let ctr = br#"{"Id":"502bc544a072","Name":"/db","State":{"Status":"running","Pid":42,"Running":true},"Config":{"Tty":false,"Labels":{"a":"b"}},"SizeRw":null}
"#.to_vec();
        let img = br#"{"Id":"sha256:9782","RepoTags":["docker.io/library/redis:latest"],"Size":148588077}"#.to_vec();
        let mut fetch = |p: &str| -> Result<Vec<u8>, String> {
            match p {
                "/containers/db/json" | "/containers/db/json?size=1" => Ok(ctr.clone()),
                "/images/docker.io/library/redis:latest/json" => Ok(img.clone()),
                _ => Err("HTTP 404".into()),
            }
        };
        let run = |args: &[&str], f: &mut dyn FnMut(&str) -> Result<Vec<u8>, String>| execute(&parse(&a(args)).unwrap(), f, 0, utc).map(|o| (String::from_utf8(o.chunks[0].1.clone()).unwrap(), o.paths));
        let (out, _) = run(&["inspect", "-f", "{{.Name}} {{.ID}} {{.State.Status}} {{.State.Pid}} {{.State.Running}}", "db"], &mut fetch).unwrap();
        assert_eq!(out, "/db 502bc544a072 running 42 true
");
        // druhý objekt je obraz bez pole .Name -> fallback
        assert!(run(&["inspect", "-f", "{{.Name}}", "db", "docker.io/library/redis:latest"], &mut fetch).is_err());
        let (out, paths) = run(&["inspect", "-f", "{{.Name}} {{.State.Pid}}", "db"], &mut fetch).unwrap();
        assert_eq!((out.as_str(), paths), ("/db 42\n", vec!["/containers/db/json".to_string()]));
        let (out, paths) = run(&["inspect", "--format", "{{.Id}}|{{.Size}}", "docker.io/library/redis:latest"], &mut fetch).unwrap();
        assert_eq!(out, "sha256:9782|148588077\n");
        assert_eq!(paths, vec!["/containers/docker.io/library/redis:latest/json".to_string(), "/images/docker.io/library/redis:latest/json".to_string()]);
        let (out, _) = run(&["container", "inspect", "-s", "db"], &mut fetch).unwrap();
        assert!(out.starts_with("[\n    {\n        \"Id\": \"502bc544a072\",\n"), "{out}");
        assert!(out.ends_with("        \"SizeRw\": null\n    }\n]\n"), "{out}");
        let (out, _) = run(&["image", "inspect", "-f", "{{json .}}", "docker.io/library/redis:latest"], &mut fetch).unwrap();
        assert_eq!(out, format!("{}\n", String::from_utf8(img.clone()).unwrap()));
        // neexistující objekt, kontejner jako obraz, nebo neskalární pole -> wsl.exe
        assert!(run(&["inspect", "db", "nope"], &mut fetch).is_err());
        assert!(run(&["image", "inspect", "db"], &mut fetch).is_err());
        assert!(run(&["inspect", "-f", "{{.Config.Labels}}", "db"], &mut fetch).is_err());
        assert!(run(&["inspect", "-f", "{{.SizeRw}}", "db"], &mut fetch).is_err());
        assert!(run(&["inspect", "-f", "{{.Nope}}", "db"], &mut fetch).is_err());
    }

    fn frame(stream: u8, data: &[u8]) -> Vec<u8> {
        let mut v = vec![stream, 0, 0, 0];
        v.extend_from_slice(&(data.len() as u32).to_be_bytes());
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn logs_execute() {
        let mut body = frame(1, b"out1\n");
        body.extend(frame(1, b"out2\n"));
        body.extend(frame(2, b"err1\n"));
        body.extend(frame(1, b"out3\n"));
        assert_eq!(demux(&body).unwrap(), vec![(1, b"out1\nout2\n".to_vec()), (2, b"err1\n".to_vec()), (1, b"out3\n".to_vec())]);
        assert!(demux(&body[..body.len() - 1]).is_err());
        assert!(demux(&frame(3, b"boom")).is_err());
        assert_eq!(demux(b"").unwrap(), vec![]);

        let now_ns: i128 = 1_700_000_600 * 1_000_000_000 + 250_000_000;
        for (tty, expect) in [(false, vec![(1u8, b"out1\nout2\n".to_vec()), (2, b"err1\n".to_vec()), (1, b"out3\n".to_vec())]), (true, vec![(1u8, body.clone())])] {
            let info = format!(r#"{{"Id":"abc123","Config":{{"Tty":{tty}}}}}"#).into_bytes();
            let mut seen = Vec::new();
            let mut fetch = |p: &str| -> Result<Vec<u8>, String> {
                seen.push(p.to_string());
                if p == "/containers/web/json" {
                    Ok(info.clone())
                } else if p.starts_with("/containers/abc123/logs?") {
                    Ok(body.clone())
                } else {
                    Err("HTTP 404".into())
                }
            };
            let o = execute(&parse(&a(&["logs", "-n", "5", "-t", "--since", "10m", "--until", "1700000000.5", "web"])).unwrap(), &mut fetch, now_ns, utc).unwrap();
            assert_eq!(o.chunks, expect);
            assert_eq!(o.paths[1], "/containers/abc123/logs?stdout=1&stderr=1&since=1700000000&until=1700000000.5&timestamps=1&tail=5");
        }
        let mut missing = |_: &str| -> Result<Vec<u8>, String> { Err("HTTP 404".into()) };
        assert!(execute(&parse(&a(&["logs", "nope"])).unwrap(), &mut missing, 0, utc).is_err());
    }

    #[test]
    fn timestamps_and_durations() {
        assert_eq!(go_duration_ns("10m"), Some(600_000_000_000));
        assert_eq!(go_duration_ns("1h30m"), Some(5_400_000_000_000));
        assert_eq!(go_duration_ns("1.5s"), Some(1_500_000_000));
        assert_eq!(go_duration_ns("300ms"), Some(300_000_000));
        assert_eq!(go_duration_ns("0"), Some(0));
        assert_eq!(go_duration_ns("10"), None);
        assert_eq!(go_duration_ns("2024-01-01"), None);
        let now: i128 = 1_000_000_000 * 1_000_000_000;
        assert_eq!(api_timestamp("1m", now).unwrap(), "999999940");
        assert_eq!(api_timestamp("0", now).unwrap(), "0");
        assert_eq!(api_timestamp("1700000000", now).unwrap(), "1700000000");
        assert_eq!(api_timestamp("1700000000.123", now).unwrap(), "1700000000.123");
        assert!(api_timestamp("2024-01-01", now).is_err());
        assert!(api_timestamp("17.", now).is_err());
        assert!(api_timestamp("", now).is_err());
    }

    // ------------------------------------------------------------ živý test

    fn live_cli(args: &[&str]) -> (Vec<String>, f64) {
        let distro = std::env::var("WLS_DISTRO").unwrap_or_else(|_| "Debian".into());
        let port = std::env::var("WLS_PORT").unwrap_or_else(|_| "2375".into());
        let url = format!("tcp://127.0.0.1:{port}");
        let mut times = Vec::new();
        let mut out = Vec::new();
        for _ in 0..5 {
            let t = std::time::Instant::now();
            let o = std::process::Command::new("wsl.exe").args(["-d", &distro, "-e", "podman", "--url", &url]).args(args).output().expect("wsl.exe");
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
            out = String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect();
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        (out, times[2])
    }

    fn live_fast(args: &[&str]) -> (Vec<String>, f64) {
        let port: u16 = std::env::var("WLS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375);
        let mut times = Vec::new();
        let mut out = Vec::new();
        for _ in 0..5 {
            let t = std::time::Instant::now();
            let o = try_run(&a(args), port, false).expect("fast path");
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            let text: Vec<u8> = o.chunks.iter().filter(|(st, _)| *st == 1).flat_map(|(_, d)| d.clone()).collect();
            out = String::from_utf8_lossy(&text).lines().map(String::from).collect();
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        (out, times[2])
    }

    /// Rychlá cesta přes TLS bez zásahu do běžícího enginu (jako `live_tls_proxy` v aplikaci):
    /// dočasná Podman služba na unix socketu + TLS proxy na portu 2377 s vlastními certifikáty.
    /// `cargo test --bin docker -- --ignored live_fast_tls --nocapture`
    #[test]
    #[ignore]
    fn live_fast_tls_proxy() {
        let distro = std::env::var("WLS_DISTRO").unwrap_or_else(|_| "Debian".into());
        let root = |script: &str, env: &[&str]| {
            let o = std::process::Command::new("wsl.exe").args(["-d", &distro, "-u", "root", "-e", "env"]).args(env).args(["sh", "-c", script]).output().expect("wsl.exe");
            (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr))
        };
        let cleanup = || {
            root("systemctl disable --now wls-test-tls-proxy wls-test-podman >/dev/null 2>&1; rm -f /etc/systemd/system/wls-test-tls-proxy.service; systemctl daemon-reload; systemctl reset-failed wls-test-tls-proxy wls-test-podman >/dev/null 2>&1; rm -rf /tmp/wls-test-tls /tmp/wls-test-podman.sock", &[]);
        };
        cleanup();
        let (ok, out) = root("systemd-run --unit wls-test-podman podman system service --time=0 unix:///tmp/wls-test-podman.sock && for i in $(seq 50); do [ -S /tmp/wls-test-podman.sock ] && break; sleep 0.2; done", &[]);
        assert!(ok, "{out}");
        let script = format!(
            "{}
tls_proxy /tmp/wls-test-podman.sock wls-test-podman.service && sleep 1 && systemctl is-active wls-test-tls-proxy",
            crate::engine::Engine::Podman.setup_script().split("
set -e
").next().unwrap()
        );
        let (ok, out) = root(&script, &["PORT=2377", "WLS_TLS_DIR=/tmp/wls-test-tls", "WLS_TLS_UNIT=wls-test-tls-proxy"]);
        let dir = std::env::temp_dir().join(format!("wls-shim-tls-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        for f in ["ca.pem", "cert.pem", "key.pem"] {
            let (_, pem) = root(&format!("cat /tmp/wls-test-tls/{f}"), &[]);
            std::fs::write(dir.join(f), pem).unwrap();
        }
        let cfg = crate::apitls::tls_config(&dir).map(Arc::new);
        let res = cfg.as_ref().map(|c| {
            let cmd = parse(&a(&["ps", "-a", "--format", "{{.Names}}"])).unwrap();
            let t = std::time::Instant::now();
            let body = http_get(2377, Some(c.clone()), &cmd.api_path());
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            (body.map(|b| cmd.output(&b, 0, utc)), ms)
        });
        let plain = http_get(2377, None, "/_ping");
        let _ = std::fs::remove_dir_all(&dir);
        cleanup();
        assert!(ok, "{out}");
        let (body, ms) = res.expect("tls_config");
        let names = body.expect("https request").expect("output");
        println!("TLS fast path: {ms:.1} ms
{names}");
        assert!(plain.is_err(), "plain HTTP to the TLS port succeeded");
    }

    /// `cargo test --bin docker -- --ignored live_fast_path --nocapture`
    /// (WLS_DISTRO, výchozí Debian; WLS_PORT, výchozí 2375; Podman bez TLS)
    #[test]
    #[ignore]
    fn live_fast_path_matches_cli() {
        use std::collections::BTreeSet;
        let cases: &[(&str, &[&str], &[&str])] = &[
            ("ps names", &["ps", "-a", "--format", "{{.Names}}"], &["ps", "-a", "--format", "{{.Names}}"]),
            ("image ids", &["images", "--format", "{{.ID}}"], &["images", "--format", "{{.ID}}"]),
            ("volume names", &["volume", "ls", "--format", "{{.Name}}"], &["volume", "ls", "--format", "{{.Name}}"]),
            // Podman v compat API přejmenovává síť `podman` na `bridge` – porovnávají se ID
            ("network ids", &["network", "ls", "--format", "{{.ID}}"], &["network", "ls", "--format", "{{.ID}}"]),
        ];
        for (name, cli, fast) in cases {
            let (c, tc) = live_cli(cli);
            let (f, tf) = live_fast(fast);
            let cs: BTreeSet<String> = c.into_iter().collect();
            let fs: BTreeSet<String> = f.into_iter().collect();
            println!("{name:14} wsl.exe median {tc:7.1} ms | fast path median {tf:6.1} ms | {} items", fs.len());
            assert_eq!(cs, fs, "{name}");
        }
        // filtry (jen čtení, existující objekty)
        let filtered: &[(&str, &[&str])] = &[
            ("images dangling", &["images", "--filter", "dangling=false", "--format", "{{.ID}}"]),
            ("images label", &["images", "--filter", "label=maintainer", "--format", "{{.ID}}"]),
            ("volumes dangling", &["volume", "ls", "--filter", "dangling=true", "--format", "{{.Name}}"]),
            ("volumes driver", &["volume", "ls", "--filter", "driver=local", "--format", "{{.Name}}"]),
            ("networks driver", &["network", "ls", "--filter", "driver=bridge", "--format", "{{.ID}}"]),
        ];
        for (name, args) in filtered {
            let (c, tc) = live_cli(args);
            let (f, tf) = live_fast(args);
            let cs: BTreeSet<String> = c.into_iter().collect();
            let fs: BTreeSet<String> = f.into_iter().collect();
            println!("{name:16} wsl.exe median {tc:7.1} ms | fast path median {tf:6.1} ms | {} items", fs.len());
            assert_eq!(cs, fs, "{name}");
        }

        // inspect a logs nad existujícími kontejnery / obrazy
        let (ctrs, _) = live_fast(&["ps", "-a", "--format", "{{.Names}}"]);
        for ctr in ctrs.iter().take(3) {
            // .Name: Docker má "/název", Podman CLI bez lomítka – neporovnává se
            let tmpl = "{{.Id}} {{.State.Status}} {{.State.Running}} {{.Config.Image}} {{.State.Pid}}";
            let (c, tc) = live_cli(&["container", "inspect", "--format", tmpl, ctr]);
            let (f, tf) = live_fast(&["container", "inspect", "--format", tmpl, ctr]);
            println!("inspect {ctr:12} wsl.exe {tc:7.1} ms | fast {tf:6.1} ms | {}", f.join(" "));
            assert_eq!(c, f, "inspect {ctr}");
            // celé JSON: pole s jedním objektem, správné Id
            let (j, _) = live_fast(&["inspect", ctr]);
            let v: Value = serde_json::from_str(&j.join("\n")).expect("inspect JSON");
            assert_eq!(v.as_array().map(|a| a.len()), Some(1));
            assert!(c[0].starts_with(v[0]["Id"].as_str().unwrap()), "{ctr}");
            assert!(j[1].starts_with("    {") && j[2].starts_with("        \""), "4-space indent");

            for args in [&["logs", "--tail", "20", ctr.as_str()][..], &["logs", "--tail", "5", "-t", ctr.as_str()], &["logs", "--since", "24h", "--tail", "3", ctr.as_str()]] {
                let (c, tc) = live_cli(args);
                let (f, tf) = live_fast(args);
                println!("{:30} wsl.exe {tc:7.1} ms | fast {tf:6.1} ms | {} lines", args.join(" "), f.len());
                assert_eq!(c, f, "{args:?}");
            }
            // stderr kontejneru musí jít na stderr (nginx loguje přístupy na stdout, chyby na stderr)
            let distro = std::env::var("WLS_DISTRO").unwrap_or_else(|_| "Debian".into());
            let url = format!("tcp://127.0.0.1:{}", std::env::var("WLS_PORT").unwrap_or_else(|_| "2375".into()));
            let o = std::process::Command::new("wsl.exe").args(["-d", &distro, "-e", "podman", "--url", &url, "logs", "--tail", "50", ctr]).output().expect("wsl.exe");
            let port: u16 = std::env::var("WLS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375);
            let fo = try_run(&a(&["logs", "--tail", "50", ctr]), port, false).expect("fast logs");
            let pick = |st: u8| -> Vec<u8> { fo.chunks.iter().filter(|(x, _)| *x == st).flat_map(|(_, d)| d.clone()).collect() };
            println!("logs streams {ctr}: stdout {} B, stderr {} B", pick(1).len(), pick(2).len());
            assert_eq!(String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&pick(1)), "stdout {ctr}");
            assert_eq!(String::from_utf8_lossy(&o.stderr), String::from_utf8_lossy(&pick(2)), "stderr {ctr}");
        }
        let (imgs, _) = live_fast(&["images", "--format", "{{.Repository}}:{{.Tag}}"]);
        for img in imgs.iter().filter(|i| !i.contains("<none>")).take(3) {
            // Podman CLI vypisuje Id bez `sha256:` – porovnávají se pole shodná v obou formátech
            let tmpl = "{{.Os}} {{.Architecture}} {{.Size}}";
            let (c, _) = live_cli(&["image", "inspect", "--format", tmpl, img]);
            let (f, _) = live_fast(&["image", "inspect", "--format", tmpl, img]);
            println!("image inspect {img}: {}", f.join(" "));
            assert_eq!(c, f, "image inspect {img}");
        }
        // JSON formát seznamů: každý řádek je JSON objekt se všemi poli
        for (args, n) in [(&["images", "--format", "json"][..], IMAGE_HEADERS.len()), (&["network", "ls", "--format", "json"], NETWORK_HEADERS.len()), (&["volume", "ls", "--format", "json"], VOLUME_HEADERS.len())] {
            let (f, _) = live_fast(args);
            for l in &f {
                let v: Value = serde_json::from_str(l).expect("json line");
                assert_eq!(v.as_object().unwrap().len(), n, "{l}");
            }
        }
        // neexistující objekt -> fallback (chybu vypíše CLI)
        assert!(try_run(&a(&["inspect", "wls-test-does-not-exist"]), std::env::var("WLS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375), false).is_err());

        let (t, _) = live_fast(&["ps", "-a"]);
        println!("{}", t.join("\n"));
        let (t, _) = live_fast(&["images"]);
        println!("{}", t.join("\n"));
        let (t, _) = live_fast(&["network", "ls", "--format", "json"]);
        println!("{}", t.join("\n"));
    }
}
