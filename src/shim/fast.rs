//! Rychlá cesta shimu: jednoduché příkazy jen pro čtení (`ps`, `images`, `volume ls`,
//! `network ls`, `version --format`, `info --format`) odpoví přímo z REST API enginu,
//! bez spouštění `wsl.exe` (to stojí ~0,1–0,3 s).
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
//! - Podman v kompatibilním API přejmenovává síť `podman` na `bridge`.

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
];
const IMAGE_TABLE: &str = r"{{.Repository}}\t{{.Tag}}\t{{.ID}}\t{{.CreatedSince}}\t{{.Size}}";

pub struct ImageRow {
    pub repo: String,
    pub tag: String,
    pub id: String,
    pub created: i64,
    pub size: i64,
    pub containers: i64,
}

/// Řádky jako `docker images`: jeden na tag; obraz jen s digestem má tag `<none>`.
pub fn image_rows(images: &[Value]) -> Vec<ImageRow> {
    let mut imgs: Vec<&Value> = images.iter().collect();
    imgs.sort_by_key(|i| std::cmp::Reverse(n(i, "Created")));
    let mut out = Vec::new();
    for i in imgs {
        let strs = |k: &str| -> Vec<String> { i.get(k).and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
        let row = |repo: &str, tag: &str| ImageRow {
            repo: repo.to_string(),
            tag: tag.to_string(),
            id: s(i, "Id").to_string(),
            created: n(i, "Created"),
            size: n(i, "Size"),
            containers: i.get("Containers").and_then(|c| c.as_i64()).unwrap_or(-1),
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
                out.push(row(repo, t));
            }
        }
        for repo in &digest_repos {
            // jeden řádek na digest
            let cnt = digests.iter().filter(|d| split_ref(d).is_some_and(|(nm, _, dg)| dg.is_some() && familiar_name(&nm) == *repo)).count();
            for _ in 0..cnt {
                out.push(row(repo, "<none>"));
            }
        }
        if out.len() == before {
            out.push(row("<none>", "<none>"));
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
        _ => String::new(),
    }
}

// ================================================================ volume / network

pub const VOLUME_HEADERS: &[(&str, &str)] = &[("Name", "VOLUME NAME"), ("Driver", "DRIVER"), ("Scope", "SCOPE"), ("Mountpoint", "MOUNTPOINT"), ("Labels", "LABELS")];
const VOLUME_TABLE: &str = r"{{.Driver}}\t{{.Name}}";

pub fn volume_field(v: &Value, f: &str) -> String {
    match f {
        "Labels" => v.get("Labels").map(labels_text).unwrap_or_default(),
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
        k => s(v, k).to_string(),
    }
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
    Images { all: bool, trunc: bool, fmt: Fmt },
    Volumes { fmt: Fmt },
    Networks { trunc: bool, fmt: Fmt },
    Version(Vec<Seg>),
    Info(Vec<Seg>),
}

struct Flags {
    bools: Vec<String>,
    values: Vec<(String, String)>,
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
    let mut f = Flags { bools: Vec::new(), values: Vec::new() };
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
            return Err(format!("unsupported argument {a:?}"));
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
        _ => return Err("command not handled by the fast path".into()),
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
            let mut filters = Vec::new();
            for (_, v) in fl.values.iter().filter(|(k, _)| k == "filter") {
                let (fk, fv) = v.split_once('=').ok_or("bad filter")?;
                let fk = fk.trim().to_ascii_lowercase();
                if !PS_FILTER_KEYS.contains(&fk.as_str()) {
                    return Err(format!("unsupported filter {fk}"));
                }
                filters.push((fk, fv.trim().to_string()));
            }
            let fmt = resolve_format(fl.last("format"), fl.has("quiet"), &format!("{PS_TABLE}{}", if fl.has("size") { r"\t{{.Size}}" } else { "" }), "{{.ID}}", PS_HEADERS, true)?;
            let size = fl.has("size") || fmt.uses("Size");
            Ok(Cmd::Ps { all: fl.has("all") || limit.is_some(), size, limit, filters, trunc: !fl.has("no-trunc"), fmt })
        }
        "images" => {
            let fl = parse_flags(rest, &[(Some('a'), "all"), (Some('q'), "quiet"), (None, "no-trunc")], &[(None, "format")])?;
            let fmt = resolve_format(fl.last("format"), fl.has("quiet"), IMAGE_TABLE, "{{.ID}}", IMAGE_HEADERS, false)?;
            Ok(Cmd::Images { all: fl.has("all"), trunc: !fl.has("no-trunc"), fmt })
        }
        "volumes" => {
            let fl = parse_flags(rest, &[(Some('q'), "quiet")], &[(None, "format")])?;
            let fmt = resolve_format(fl.last("format"), fl.has("quiet"), VOLUME_TABLE, "{{.Name}}", VOLUME_HEADERS, false)?;
            Ok(Cmd::Volumes { fmt })
        }
        "networks" => {
            let fl = parse_flags(rest, &[(Some('q'), "quiet"), (None, "no-trunc")], &[(None, "format")])?;
            let fmt = resolve_format(fl.last("format"), fl.has("quiet"), NETWORK_TABLE, "{{.ID}}", NETWORK_HEADERS, false)?;
            Ok(Cmd::Networks { trunc: !fl.has("no-trunc"), fmt })
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

impl Cmd {
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
                if !filters.is_empty() {
                    q.push(format!("filters={}", enc(&filters_json(filters))));
                }
                if q.is_empty() { "/containers/json".into() } else { format!("/containers/json?{}", q.join("&")) }
            }
            Cmd::Images { all, .. } => if *all { "/images/json?all=1" } else { "/images/json" }.into(),
            Cmd::Volumes { .. } => "/volumes".into(),
            Cmd::Networks { .. } => "/networks".into(),
            Cmd::Version(_) => "/version".into(),
            Cmd::Info(_) => "/info".into(),
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
            Cmd::Volumes { fmt } => {
                let mut rows = body.get("Volumes").map(arr).unwrap_or_else(|| Ok(Vec::new()))?;
                rows.sort_by(|a, b| if natural_less(s(a, "Name"), s(b, "Name")) { std::cmp::Ordering::Less } else if natural_less(s(b, "Name"), s(a, "Name")) { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Equal });
                Ok(emit(fmt, VOLUME_HEADERS, &rows, |v, f| volume_field(v, f)))
            }
            Cmd::Networks { trunc, fmt } => {
                let mut rows = arr(body)?;
                rows.sort_by(|a, b| if natural_less(s(a, "Name"), s(b, "Name")) { std::cmp::Ordering::Less } else if natural_less(s(b, "Name"), s(a, "Name")) { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Equal });
                let x = Ctx { trunc: *trunc, now, tz };
                Ok(emit(fmt, NETWORK_HEADERS, &rows, |v, f| network_field(v, f, &x)))
            }
            Cmd::Version(segs) => render_single(segs, |p| version_lookup(body, p)),
            Cmd::Info(segs) => render_single(segs, |p| info_lookup(body, p)),
        }
    }
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

/// GET na API enginu (http, nebo https s klientským certifikátem).
pub fn http_get(port: u16, tls: Option<Arc<rustls::ClientConfig>>, path: &str) -> Result<Value, String> {
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
    let body = parse_http(&raw)?;
    serde_json::from_slice(&body).map_err(|e| format!("bad JSON: {e}"))
}

/// Celá rychlá cesta: rozbor, dotaz na API, výstup. `Err(důvod)` = použij wsl.exe.
pub fn try_run(args: &[String], port: u16, tls: bool) -> Result<(String, String), String> {
    let cmd = parse(args)?;
    let tls = if tls {
        let dir = crate::apitls::tls_dir().ok_or("APPDATA not set")?;
        Some(Arc::new(crate::apitls::tls_config(&dir)?))
    } else {
        None
    };
    let path = cmd.api_path();
    let body = http_get(port, tls, &path)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    Ok((cmd.output(&body, now, local_tz)?, path))
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
            &["images", "--filter", "dangling=true"],
            &["images", "--format", "json"],
            &["images", "--format", "{{.Digest}}"],
            &["volume", "ls", "-f", "dangling=true"],
            &["volume", "ls", "--format", "{{.Size}}"],
            &["network", "ls", "--filter", "driver=bridge"],
            &["volume", "inspect", "x"],
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
            let (o, _) = try_run(&a(args), port, false).expect("fast path");
            times.push(t.elapsed().as_secs_f64() * 1000.0);
            out = o.lines().map(String::from).collect();
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
        let (t, _) = live_fast(&["ps", "-a"]);
        println!("{}", t.join("\n"));
        let (t, _) = live_fast(&["images"]);
        println!("{}", t.join("\n"));
    }
}
