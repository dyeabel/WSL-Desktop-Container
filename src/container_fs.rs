//! Soubory uvnitř kontejneru přes Docker / Podman API.
//!
//! - výpis: u běžícího kontejneru `exec` POSIX skriptu (busybox i GNU, záloha přes `ls -la`),
//!   u zastaveného nebo kontejneru bez shellu archiv `GET /containers/{id}/archive?path=<dir>/.`
//!   (projde se jen první úroveň, čtení je omezené);
//! - stažení: archiv (tar) rozbalený do složky ve Windows s ochranou proti `..`, absolutním
//!   cestám a symlinkům mimo cíl;
//! - nahrání: tar sestavený z vybraných souborů / složek a `PUT /containers/{id}/archive`;
//! - malé úpravy: čtení / zápis textu (archiv), nová složka (archiv), smazání a přejmenování (exec).

use crate::docker::{enc, ApiError, Docker};
use crate::fs::{join, parent, Entry, MAX_EDIT};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Výsledek výpisu adresáře.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    pub cwd: String,
    pub entries: Vec<Entry>,
    /// výpis z archivu (zastavený kontejner nebo bez shellu) – exec akce nejsou k dispozici
    pub via_archive: bool,
    /// archiv byl příliš velký, výpis není úplný
    pub truncated: bool,
}

// ---------------------------------------------------------------- cesty

/// Normalizuje cestu v kontejneru: vždy absolutní, bez `.`, `..` a zdvojených lomítek.
pub fn normalize_path(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for c in p.trim().split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Poslední složka cesty (`/etc/hosts` -> `hosts`).
pub fn base_name(p: &str) -> &str {
    let t = p.trim_end_matches('/');
    t.rsplit('/').next().unwrap_or(t)
}

/// Cesta archivu pro obsah adresáře (`/etc` -> `/etc/.`, `/` -> `/.`).
fn contents_path(dir: &str) -> String {
    join(dir, ".")
}

fn archive_url(id: &str, path: &str) -> String {
    format!("/containers/{}/archive?path={}", enc(id), enc(path))
}

// ---------------------------------------------------------------- exec

/// Výstup příkazu spuštěného v kontejneru.
#[derive(Debug, Clone, Default)]
pub struct ExecOut {
    pub code: i64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl ExecOut {
    /// Chybový text (stderr, jinak stdout, jinak kód).
    pub fn message(&self) -> String {
        let e = String::from_utf8_lossy(&self.stderr).trim().to_string();
        if !e.is_empty() {
            return e;
        }
        let o = String::from_utf8_lossy(&self.stdout).trim().to_string();
        if !o.is_empty() {
            return o;
        }
        tf!("příkaz skončil s kódem {}", "the command exited with code {}", self.code)
    }

    /// Kontejner nemá shell / příkaz (126 = nelze spustit, 127 = nenalezen).
    pub fn not_found(&self) -> bool {
        matches!(self.code, 126 | 127)
    }
}

/// Max. velikost výstupu exec (výpis obřího adresáře).
const MAX_EXEC_OUT: u64 = 64 * 1024 * 1024;

/// Rozdělí multiplexovaný proud (`[typ, 0, 0, 0, délka BE u32] + data`) na stdout a stderr.
/// Proud bez hlaviček (TTY) se vrátí celý jako stdout.
pub fn demux(buf: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut i = 0;
    while i < buf.len() {
        let h = &buf[i..];
        if h.len() < 8 || h[0] > 2 || h[1] != 0 || h[2] != 0 || h[3] != 0 {
            if i == 0 {
                return (buf.to_vec(), Vec::new());
            }
            out.extend_from_slice(h);
            break;
        }
        let n = u32::from_be_bytes([h[4], h[5], h[6], h[7]]) as usize;
        let data = &h[8..(8 + n).min(h.len())];
        if h[0] == 2 { err.extend_from_slice(data) } else { out.extend_from_slice(data) }
        i += 8 + n;
    }
    (out, err)
}

/// Spustí příkaz v kontejneru (jako root, bez TTY) a počká na konec.
/// `Err` s 409 = kontejner neběží / je pozastavený.
pub fn exec(d: &Docker, id: &str, cmd: &[&str]) -> Result<ExecOut, ApiError> {
    let body = json!({ "AttachStdout": true, "AttachStderr": true, "Tty": false, "User": "0", "Cmd": cmd });
    let v: Value = d.request("POST", &format!("/containers/{}/exec", enc(id))).send_json(body)?.into_json()?;
    let eid = v.get("Id").and_then(|x| x.as_str()).unwrap_or_default().to_string();
    if eid.is_empty() {
        return Err(ApiError::Decode(format!("exec: {v}")));
    }
    let resp = d.request("POST", &format!("/exec/{eid}/start")).send_json(json!({ "Detach": false, "Tty": false }))?;
    let mut raw = Vec::new();
    resp.into_reader().take(MAX_EXEC_OUT).read_to_end(&mut raw)?;
    let (stdout, stderr) = demux(&raw);
    // kód je k dispozici po skončení procesu – po uzavření proudu zpravidla hned
    let mut code = -1;
    for _ in 0..40 {
        let s: Value = d.get(&format!("/exec/{eid}/json"))?;
        if !s.get("Running").and_then(|r| r.as_bool()).unwrap_or(false) {
            code = s.get("ExitCode").and_then(|c| c.as_i64()).unwrap_or(-1);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(ExecOut { code, stdout, stderr })
}

/// Běží kontejner (a není pozastavený)?
pub fn is_running(d: &Docker, id: &str) -> Result<bool, ApiError> {
    let v: Value = d.get(&format!("/containers/{}/json", enc(id)))?;
    let b = |p: &str| v.pointer(p).and_then(|x| x.as_bool()).unwrap_or(false);
    Ok(b("/State/Running") && !b("/State/Paused"))
}

// ---------------------------------------------------------------- výpis přes exec

/// Výpis adresáře `$1` (cesta se předává jako argument, nikdy se nevkládá do skriptu).
/// Řádky: 1. `pwd`, pak záznamy
/// - `F\t<typ a práva hex>\t<velikost>\t<mtime>\t<práva>\t<vlastník>\t<název>` (stat -c),
/// - `L\t<řádek ls -la>` (záloha bez `stat -c`),
/// - `D\t<název>` symlink, který vede na adresář.
pub const LIST_SCRIPT: &str = r#"cd -- "$1" 2>/dev/null || { printf '%s: %s\n' "$1" "cannot open directory" >&2; exit 2; }
pwd
T=$(printf '\t')
F="F$T%f$T%s$T%Y$T%a$T%U$T%n"
if stat -c '%n' . >/dev/null 2>&1; then
  if find . -maxdepth 0 >/dev/null 2>&1; then
    find . -mindepth 1 -maxdepth 1 -exec stat -c "$F" -- {} + 2>/dev/null
  else
    stat -c "$F" -- .[!.]* ..?* * 2>/dev/null
  fi
else
  { ls -la --full-time 2>/dev/null || ls -la; } | while IFS= read -r l; do printf 'L\t%s\n' "$l"; done
fi
for f in .[!.]* ..?* *; do
  if [ -L "$f" ] && [ -d "$f" ]; then printf 'D\t%s\n' "$f"; fi
done
exit 0"#;

/// Typ položky z `st_mode`.
fn kind_from_mode(mode: u32) -> char {
    match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        0o020000 => 'c',
        0o060000 => 'b',
        0o010000 => 'p',
        0o140000 => 's',
        _ => 'f',
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Čas z `ls -la`: `2026-11-18 20:04:12[.frac] +0000` (--full-time) nebo `Nov 18 20:04` / `Apr 15  2026`.
fn ls_time(a: &str, b: &str, c: &str) -> i64 {
    let num = |s: &str| s.parse::<i64>().ok();
    if let (Some(y), Some(mo), Some(d)) = (num(a.get(0..4).unwrap_or("")), num(a.get(5..7).unwrap_or("")), num(a.get(8..10).unwrap_or(""))) {
        let hms: Vec<i64> = b.split('.').next().unwrap_or("").split(':').filter_map(num).collect();
        let secs = hms.first().unwrap_or(&0) * 3600 + hms.get(1).unwrap_or(&0) * 60 + hms.get(2).unwrap_or(&0);
        let off = if c.len() == 5 && (c.starts_with('+') || c.starts_with('-')) {
            let s = if c.starts_with('-') { -1 } else { 1 };
            s * (num(&c[1..3]).unwrap_or(0) * 3600 + num(&c[3..5]).unwrap_or(0) * 60)
        } else {
            0
        };
        return days_from_civil(y, mo, d) * 86_400 + secs - off;
    }
    const MON: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let Some(mo) = MON.iter().position(|m| a.to_ascii_lowercase().starts_with(m)) else { return 0 };
    let d = num(b).unwrap_or(1);
    let now = now_secs();
    if let Some((h, mi)) = c.split_once(':') {
        // bez roku = posledních ~6 měsíců
        let year = 1970 + now / 31_556_952;
        let t = |y: i64| days_from_civil(y, mo as i64 + 1, d) * 86_400 + num(h).unwrap_or(0) * 3600 + num(mi).unwrap_or(0) * 60;
        let v = t(year);
        if v > now + 86_400 { t(year - 1) } else { v }
    } else {
        days_from_civil(num(c).unwrap_or(1970), mo as i64 + 1, d) * 86_400
    }
}

/// Práva `rwxr-sr-x` -> osmičkově (`2755`).
fn perm_octal(p: &str) -> String {
    let b: Vec<char> = p.chars().collect();
    if b.len() < 9 {
        return p.to_string();
    }
    let mut m = 0u32;
    let mut special = 0u32;
    for (i, sh) in [(0usize, 6u32), (3, 3), (6, 0)] {
        if b[i] == 'r' {
            m |= 4 << sh;
        }
        if b[i + 1] == 'w' {
            m |= 2 << sh;
        }
        match b[i + 2] {
            'x' => m |= 1 << sh,
            's' | 't' => {
                m |= 1 << sh;
                special |= 4 >> (i / 3);
            }
            'S' | 'T' => special |= 4 >> (i / 3),
            _ => {}
        }
    }
    if special > 0 { format!("{special}{m:03o}") } else { format!("{m:o}") }
}

/// Jeden řádek `ls -la` (busybox i GNU, i s --full-time). `.`, `..` a `total` -> None.
pub fn parse_ls_line(line: &str) -> Option<Entry> {
    // tokeny s pozicemi, aby název zůstal i s mezerami
    let mut toks: Vec<(usize, &str)> = Vec::new();
    let mut start = None;
    for (i, ch) in line.char_indices() {
        if ch == ' ' || ch == '\t' {
            if let Some(s) = start.take() {
                toks.push((s, &line[s..i]));
                if toks.len() == 10 {
                    break;
                }
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        if toks.len() < 10 {
            toks.push((s, &line[s..]));
        }
    }
    let perms = toks.first()?.1;
    if perms.len() < 10 || !"-dlcbps".contains(perms.chars().next()?) {
        return None;
    }
    // zařízení: „1,   3“ místo velikosti
    let dev = toks.get(4)?.1.ends_with(',');
    let si = if dev { 5 } else { 4 };
    let (a, b, c) = (toks.get(si + 1)?.1, toks.get(si + 2)?.1, toks.get(si + 3)?);
    let name_start = c.0 + c.1.len() + 1;
    let mut name = line.get(name_start..)?.to_string();
    let kind = match perms.chars().next()? {
        '-' => 'f',
        k => k,
    };
    if kind == 'l' {
        if let Some(i) = name.find(" -> ") {
            name.truncate(i);
        }
    }
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    Some(Entry {
        kind,
        target_kind: kind,
        size: if dev { 0 } else { toks[si].1.parse().unwrap_or(0) },
        mtime: ls_time(a, b, c.1),
        owner: toks.get(2)?.1.to_string(),
        mode: perm_octal(&perms[1..]),
        name,
    })
}

/// Rozebere výstup [`LIST_SCRIPT`]. Řádky, které nezačínají značkou, patří k předchozímu
/// názvu (název s novým řádkem).
pub fn parse_exec_listing(out: &str) -> (String, Vec<Entry>) {
    let (cwd, rest) = out.split_once('\n').unwrap_or((out, ""));
    // spojit pokračovací řádky
    let mut recs: Vec<String> = Vec::new();
    for line in rest.split('\n') {
        if line.starts_with("F\t") || line.starts_with("L\t") || line.starts_with("D\t") || recs.is_empty() {
            recs.push(line.to_string());
        } else if let Some(last) = recs.last_mut() {
            last.push('\n');
            last.push_str(line);
        }
    }
    // poslední prázdný řádek (konec výstupu) nepatří k názvu
    if let Some(last) = recs.last_mut() {
        while last.ends_with('\n') {
            last.pop();
        }
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut dir_links: Vec<String> = Vec::new();
    for r in &recs {
        if let Some(n) = r.strip_prefix("D\t") {
            dir_links.push(n.to_string());
        } else if let Some(l) = r.strip_prefix("L\t") {
            entries.extend(parse_ls_line(l));
        } else if let Some(f) = r.strip_prefix("F\t") {
            let p: Vec<&str> = f.splitn(6, '\t').collect();
            if p.len() < 6 {
                continue;
            }
            let raw = u32::from_str_radix(p[0], 16).unwrap_or(0o100000);
            let name = p[5].strip_prefix("./").unwrap_or(p[5]).to_string();
            if name.is_empty() || name == "." || name == ".." {
                continue;
            }
            let kind = kind_from_mode(raw);
            entries.push(Entry {
                kind,
                target_kind: kind,
                size: p[1].parse().unwrap_or(0),
                mtime: p[2].parse().unwrap_or(0),
                mode: p[3].to_string(),
                owner: p[4].to_string(),
                name,
            });
        }
    }
    for e in &mut entries {
        if e.kind == 'l' {
            e.target_kind = if dir_links.contains(&e.name) { 'd' } else { 'f' };
        }
    }
    sort_entries(&mut entries);
    (cwd.trim_end_matches('\r').to_string(), entries)
}

fn sort_entries(v: &mut [Entry]) {
    v.sort_by(|a, b| b.is_dir().cmp(&a.is_dir()).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
}

// ---------------------------------------------------------------- archiv: stat, výpis

/// `X-Docker-Container-Path-Stat` (base64 JSON).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PathStat {
    pub name: String,
    pub size: u64,
    /// Go `os.FileMode` (adresář = bit 31, symlink = bit 27)
    pub mode: u32,
    pub mtime: String,
    #[serde(rename = "linkTarget")]
    pub link_target: String,
    #[serde(rename = "isDir")]
    pub is_dir_field: Option<bool>,
}

impl PathStat {
    pub fn is_dir(&self) -> bool {
        self.is_dir_field.unwrap_or(self.mode & (1 << 31) != 0)
    }
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.trim().bytes().filter(|c| *c != b'=') {
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub fn decode_stat(header: &str) -> Option<PathStat> {
    serde_json::from_slice(&b64_decode(header)?).ok()
}

/// Stat cesty přes `HEAD …/archive` (funguje i u zastaveného kontejneru a bez shellu).
pub fn stat(d: &Docker, id: &str, path: &str) -> Result<PathStat, ApiError> {
    let r = d.request("HEAD", &archive_url(id, path)).call()?;
    let h = r.header("X-Docker-Container-Path-Stat").unwrap_or_default().to_string();
    decode_stat(&h).ok_or_else(|| ApiError::Decode("X-Docker-Container-Path-Stat".into()))
}

/// Čtení s limitem – po vyčerpání vrací konec souboru (ten se pak hlásí jako neúplný výpis).
struct Limited<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        let n = buf.len().min(self.left.min(usize::MAX as u64) as usize);
        let r = self.inner.read(&mut buf[..n])?;
        self.left -= r as u64;
        Ok(r)
    }
}

/// Kolik bajtů archivu nejvýš přečteme kvůli výpisu jedné úrovně.
pub const MAX_ARCHIVE_LIST: u64 = 256 * 1024 * 1024;

/// Název položky z tar hlavičky bez `./` a `/` na začátku a `/` na konci.
fn tar_name(raw: &str) -> &str {
    let mut n = raw;
    loop {
        if let Some(r) = n.strip_prefix("./") {
            n = r;
        } else if let Some(r) = n.strip_prefix('/') {
            n = r;
        } else {
            break;
        }
    }
    n.trim_end_matches('/')
}

/// Z taru obsahu adresáře (`…/archive?path=<dir>/.`) vybere jen první úroveň.
/// Vrací (položky, neúplné).
pub fn list_tar<R: Read>(r: R, limit: u64) -> (Vec<Entry>, bool) {
    let mut ar = tar::Archive::new(Limited { inner: r, left: limit });
    let mut v: Vec<Entry> = Vec::new();
    let mut broken = false;
    match ar.entries() {
        Ok(it) => {
            for e in it {
                let Ok(e) = e else {
                    broken = true;
                    break;
                };
                let raw = String::from_utf8_lossy(&e.path_bytes()).into_owned();
                let name = tar_name(&raw);
                if name.is_empty() || name == "." || name.contains('/') {
                    continue;
                }
                let h = e.header();
                let kind = match h.entry_type() {
                    tar::EntryType::Directory => 'd',
                    tar::EntryType::Symlink => 'l',
                    tar::EntryType::Char => 'c',
                    tar::EntryType::Block => 'b',
                    tar::EntryType::Fifo => 'p',
                    _ => 'f',
                };
                let owner = h.username().ok().flatten().filter(|u| !u.is_empty()).map(String::from).or_else(|| h.uid().ok().map(|u| u.to_string())).unwrap_or_default();
                let link = if kind == 'l' { e.link_name().ok().flatten().map(|l| l.to_string_lossy().into_owned()) } else { None };
                v.push(Entry {
                    kind,
                    target_kind: if kind == 'l' { '?' } else { kind },
                    size: if kind == 'f' { h.size().unwrap_or(0) } else { 0 },
                    mtime: h.mtime().unwrap_or(0) as i64,
                    owner,
                    mode: format!("{:o}", h.mode().unwrap_or(0) & 0o7777),
                    // cíl odkazu si dočasně schováme do názvu za \0 (doplní se typ cíle níže)
                    name: match link {
                        Some(l) => format!("{name}\0{l}"),
                        None => name.to_string(),
                    },
                });
            }
        }
        Err(_) => broken = true,
    }
    let left = ar.into_inner().left;
    // typ cíle symlinku, pokud vede na položku ve stejném adresáři
    let dirs: Vec<String> = v.iter().filter(|e| e.kind == 'd').map(|e| e.name.clone()).collect();
    let files: Vec<String> = v.iter().filter(|e| e.kind == 'f').map(|e| e.name.clone()).collect();
    for e in &mut v {
        if let Some((n, l)) = e.name.split_once('\0') {
            let t = l.trim_end_matches('/');
            let t = t.strip_prefix("./").unwrap_or(t);
            if !t.contains('/') {
                if dirs.iter().any(|d| d == t) {
                    e.target_kind = 'd';
                } else if files.iter().any(|f| f == t) {
                    e.target_kind = 'f';
                }
            }
            e.name = n.to_string();
        }
    }
    sort_entries(&mut v);
    (v, broken || left == 0)
}

fn list_archive(d: &Docker, id: &str, dir: &str) -> Result<Listing, String> {
    let st = stat(d, id, dir).map_err(|e| path_err(dir, e))?;
    if !st.is_dir() {
        return Err(tf!("{dir}: není adresář", "{dir}: not a directory"));
    }
    let resp = d.request("GET", &archive_url(id, &contents_path(dir))).call().map_err(|e| path_err(dir, e.into()))?;
    let (entries, truncated) = list_tar(resp.into_reader(), MAX_ARCHIVE_LIST);
    Ok(Listing { cwd: dir.to_string(), entries, via_archive: true, truncated })
}

fn path_err(path: &str, e: ApiError) -> String {
    if e.is_not_found() {
        tf!("{path}: neexistuje", "{path}: does not exist")
    } else {
        format!("{path}: {e}")
    }
}

/// Výpis adresáře: běžící kontejner přes exec, jinak (nebo bez shellu) přes archiv.
pub fn list(d: &Docker, id: &str, path: &str) -> Result<Listing, String> {
    let dir = normalize_path(path);
    if is_running(d, id).map_err(String::from)? {
        match exec(d, id, &["sh", "-c", LIST_SCRIPT, "sh", &dir]) {
            Ok(o) if o.code == 0 => {
                let (cwd, entries) = parse_exec_listing(&String::from_utf8_lossy(&o.stdout));
                let cwd = if cwd.starts_with('/') { cwd } else { dir };
                return Ok(Listing { cwd, entries, via_archive: false, truncated: false });
            }
            Ok(o) if o.not_found() => {} // bez shellu (distroless, scratch) -> archiv
            Ok(o) => return Err(o.message()),
            Err(e) if e.is_conflict() => {} // mezitím zastaven / pozastaven
            Err(e) => return Err(e.into()),
        }
    }
    list_archive(d, id, &dir)
}

// ---------------------------------------------------------------- čtení / zápis

/// Načte textový soubor (max. 2 MB, UTF-8). Symlink se dohledá; vrací (skutečná cesta, text).
pub fn read_text(d: &Docker, id: &str, path: &str) -> Result<(String, String), String> {
    let mut p = normalize_path(path);
    for _ in 0..8 {
        let st = stat(d, id, &p).map_err(|e| path_err(&p, e))?;
        if st.is_dir() {
            return Err(tf!("{p} je adresář", "{p} is a directory"));
        }
        if st.size as usize > MAX_EDIT {
            return Err(t!("Soubor je větší než 2 MB – stáhni ho do Windows", "The file is larger than 2 MB – download it to Windows").into());
        }
        let resp = d.request("GET", &archive_url(id, &p)).call().map_err(|e| path_err(&p, e.into()))?;
        let mut ar = tar::Archive::new(resp.into_reader());
        let mut ents = ar.entries().map_err(|e| e.to_string())?;
        let mut e = ents.next().ok_or_else(|| tf!("{p}: prázdný archiv", "{p}: empty archive"))?.map_err(|e| e.to_string())?;
        match e.header().entry_type() {
            tar::EntryType::Symlink => {
                let l = e.link_name().ok().flatten().map(|l| l.to_string_lossy().into_owned()).unwrap_or_default();
                p = if l.starts_with('/') { normalize_path(&l) } else { normalize_path(&join(&parent(&p), &l)) };
                continue;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {}
            _ => return Err(tf!("{p}: není obyčejný soubor", "{p}: not a regular file")),
        }
        let mut buf = Vec::new();
        (&mut e).take(MAX_EDIT as u64 + 1).read_to_end(&mut buf).map_err(|e| e.to_string())?;
        if buf.len() > MAX_EDIT {
            return Err(t!("Soubor je větší než 2 MB – stáhni ho do Windows", "The file is larger than 2 MB – download it to Windows").into());
        }
        if buf.contains(&0) {
            return Err(t!("Binární soubor nelze upravit v textovém editoru", "A binary file cannot be edited in the text editor").into());
        }
        let text = String::from_utf8(buf).map_err(|_| t!("Soubor není v UTF-8", "The file is not UTF-8").to_string())?;
        return Ok((p, text));
    }
    Err(tf!("{p}: příliš mnoho odkazů", "{p}: too many symlinks"))
}

/// Vlastník a práva existujícího souboru (pro zachování při zápisu).
struct Meta {
    mode: u32,
    uid: u64,
    gid: u64,
    user: Option<String>,
    group: Option<String>,
}

fn file_meta(d: &Docker, id: &str, path: &str) -> Option<Meta> {
    let resp = d.request("GET", &archive_url(id, path)).call().ok()?;
    let mut ar = tar::Archive::new(resp.into_reader());
    let mut ents = ar.entries().ok()?;
    let e = ents.next()?.ok()?;
    let h = e.header();
    if h.entry_type() != tar::EntryType::Regular {
        return None;
    }
    Some(Meta {
        mode: h.mode().ok()? & 0o7777,
        uid: h.uid().ok()?,
        gid: h.gid().ok()?,
        user: h.username().ok().flatten().map(String::from),
        group: h.groupname().ok().flatten().map(String::from),
    })
}

/// Tar s jedním souborem (`name` = jen název, ne cesta); `owner` = (uid, gid, uživatel, skupina).
pub fn single_file_tar(name: &str, data: &[u8], mode: u32, owner: Option<(u64, u64, Option<&str>, Option<&str>)>) -> Result<Vec<u8>, String> {
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Regular);
    h.set_size(data.len() as u64);
    h.set_mode(mode);
    h.set_mtime(now_secs().max(0) as u64);
    if let Some((u, g, un, gn)) = owner {
        h.set_uid(u);
        h.set_gid(g);
        if let Some(un) = un {
            let _ = h.set_username(un);
        }
        if let Some(gn) = gn {
            let _ = h.set_groupname(gn);
        }
    }
    b.append_data(&mut h, name, data).map_err(|e| e.to_string())?;
    b.into_inner().map_err(|e| e.to_string())
}

/// `PUT …/archive?path=<dir>` s tělem tar.
fn put_archive(d: &Docker, id: &str, dir: &str, body: impl Read, len: u64) -> Result<(), String> {
    d.request("PUT", &archive_url(id, dir))
        .set("Content-Type", "application/x-tar")
        .set("Content-Length", &len.to_string())
        .send(body)
        .map(|_| ())
        .map_err(|e| path_err(dir, e.into()))
}

/// Uloží text do souboru (tar s jedním souborem přes `PUT archive`); práva a vlastník
/// existujícího souboru se zachovají.
pub fn write_text(d: &Docker, id: &str, path: &str, text: &str) -> Result<(), String> {
    let p = normalize_path(path);
    let name = base_name(&p);
    if name.is_empty() {
        return Err(tf!("{p}: neplatná cesta", "{p}: invalid path"));
    }
    let meta = file_meta(d, id, &p);
    let owner = meta.as_ref().map(|m| (m.uid, m.gid, m.user.as_deref(), m.group.as_deref()));
    let data = single_file_tar(name, text.as_bytes(), meta.as_ref().map(|m| m.mode).unwrap_or(0o644), owner)?;
    let len = data.len() as u64;
    put_archive(d, id, &parent(&p), std::io::Cursor::new(data), len)
}

/// Nová složka (tar se záznamem adresáře) – funguje i u zastaveného kontejneru a bez shellu.
pub fn mkdir(d: &Docker, id: &str, path: &str) -> Result<(), String> {
    let p = normalize_path(path);
    let name = base_name(&p);
    if name.is_empty() {
        return Err(tf!("{p}: neplatná cesta", "{p}: invalid path"));
    }
    if stat(d, id, &p).is_ok() {
        return Err(tf!("{p} už existuje", "{p} already exists"));
    }
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Directory);
    h.set_size(0);
    h.set_mode(0o755);
    h.set_mtime(now_secs().max(0) as u64);
    b.append_data(&mut h, format!("{name}/"), std::io::empty()).map_err(|e| e.to_string())?;
    let data = b.into_inner().map_err(|e| e.to_string())?;
    let len = data.len() as u64;
    put_archive(d, id, &parent(&p), std::io::Cursor::new(data), len)
}

fn exec_ok(d: &Docker, id: &str, cmd: &[&str]) -> Result<(), String> {
    match exec(d, id, cmd) {
        Ok(o) if o.code == 0 => Ok(()),
        Ok(o) if o.not_found() => Err(t!("Kontejner nemá potřebné příkazy (shell, rm, mv)", "The container lacks the required commands (shell, rm, mv)").into()),
        Ok(o) => Err(o.message()),
        Err(e) if e.is_conflict() => Err(t!("Kontejner neběží", "The container is not running").into()),
        Err(e) => Err(e.into()),
    }
}

/// Smaže položky (`rm -rf`, exec). Kořen se smazat nedá.
pub fn delete(d: &Docker, id: &str, paths: &[String]) -> Result<(), String> {
    let norm: Vec<String> = paths.iter().map(|p| normalize_path(p)).collect();
    if norm.iter().any(|p| p == "/") || norm.is_empty() {
        return Err(t!("Mazání kořenového adresáře není povoleno", "Deleting the root directory is not allowed").into());
    }
    let mut cmd: Vec<&str> = vec!["rm", "-rf", "--"];
    cmd.extend(norm.iter().map(|s| s.as_str()));
    exec_ok(d, id, &cmd)
}

/// Přejmenuje / přesune (exec, cíl nesmí existovat).
const RENAME_SCRIPT: &str = r#"if [ -e "$2" ] || [ -L "$2" ]; then printf '%s: already exists\n' "$2" >&2; exit 3; fi
exec mv -- "$1" "$2""#;

pub fn rename(d: &Docker, id: &str, from: &str, to: &str) -> Result<(), String> {
    let (f, t) = (normalize_path(from), normalize_path(to));
    if f == "/" {
        return Err(t!("Kořenový adresář nelze přejmenovat", "The root directory cannot be renamed").into());
    }
    exec_ok(d, id, &["sh", "-c", RENAME_SCRIPT, "sh", &f, &t])
}

// ---------------------------------------------------------------- nahrání z Windows

/// Sestaví tar z Windows souborů a složek (každá položka pod svým názvem). Vrací zapisovač
/// a počet souborů.
pub fn build_upload_tar<W: Write>(w: W, items: &[PathBuf]) -> Result<(W, usize), String> {
    let mut b = tar::Builder::new(w);
    b.mode(tar::HeaderMode::Complete);
    let mut files = 0usize;
    for p in items {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| tf!("{}: neplatná cesta", "{}: invalid path", p.display()))?;
        let md = std::fs::metadata(p).map_err(|e| format!("{}: {e}", p.display()))?;
        if md.is_dir() {
            files += count_files(p);
            b.append_dir_all(&name, p).map_err(|e| format!("{}: {e}", p.display()))?;
        } else {
            files += 1;
            b.append_path_with_name(p, &name).map_err(|e| format!("{}: {e}", p.display()))?;
        }
    }
    let w = b.into_inner().map_err(|e| e.to_string())?;
    Ok((w, files))
}

fn count_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    let p = e.path();
                    if p.is_dir() { count_files(&p) } else { 1 }
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Čtení s hlášením průběhu (přečteno, celkem).
struct Progress<R, F> {
    inner: R,
    done: u64,
    total: u64,
    report: F,
}

impl<R: Read, F: FnMut(u64, u64)> Read for Progress<R, F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.done += n as u64;
        (self.report)(self.done, self.total);
        Ok(n)
    }
}

/// Nahraje Windows soubory / složky do adresáře `dir` v kontejneru. `progress(odesláno, celkem)`.
/// Vrací (počet souborů, velikost taru).
pub fn upload(d: &Docker, id: &str, items: &[PathBuf], dir: &str, progress: impl FnMut(u64, u64)) -> Result<(usize, u64), String> {
    let dir = normalize_path(dir);
    let st = stat(d, id, &dir).map_err(|e| path_err(&dir, e))?;
    if !st.is_dir() {
        return Err(tf!("{dir}: není adresář", "{dir}: not a directory"));
    }
    let tmp = std::env::temp_dir().join(format!("wls-upload-{}-{}.tar", std::process::id(), now_nanos()));
    let res: Result<(usize, u64), String> = (|| {
        let f = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        let (f, files) = build_upload_tar(std::io::BufWriter::new(f), items)?;
        f.into_inner().map_err(|e| e.to_string())?.sync_all().map_err(|e| e.to_string())?;
        let len = std::fs::metadata(&tmp).map_err(|e| e.to_string())?.len();
        let f = std::fs::File::open(&tmp).map_err(|e| e.to_string())?;
        let body = Progress { inner: std::io::BufReader::new(f), done: 0, total: len, report: progress };
        put_archive(d, id, &dir, body, len)?;
        Ok((files, len))
    })();
    let _ = std::fs::remove_file(&tmp);
    res
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

// ---------------------------------------------------------------- stažení do Windows

/// Výsledek rozbalení.
#[derive(Debug, Clone, Default)]
pub struct ExtractReport {
    pub files: usize,
    pub dirs: usize,
    pub bytes: u64,
    /// přeskočené položky (název – důvod)
    pub skipped: Vec<String>,
}

/// Složka názvu použitelná ve Windows (zakázané znaky -> `_`, rezervovaná jména).
fn win_component(c: &str) -> String {
    let mut s: String = c.chars().map(|ch| if ch < ' ' || "<>:\"\\|?*".contains(ch) { '_' } else { ch }).collect();
    if s.ends_with('.') || s.ends_with(' ') {
        s.pop();
        s.push('_');
    }
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&stem.as_str()) {
        s.insert(0, '_');
    }
    s
}

/// Bezpečná relativní cesta z názvu v taru. `None` = absolutní cesta nebo `..` (odmítnuto).
/// Prázdná cesta = kořen archivu (`.`).
pub fn safe_rel_path(name: &str) -> Option<PathBuf> {
    if name.starts_with('/') || name.starts_with('\\') {
        return None;
    }
    let b = name.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() && matches!(b.get(2), None | Some(b'/') | Some(b'\\')) {
        return None; // C:/…
    }
    let mut p = PathBuf::new();
    for c in name.split('/') {
        match c {
            "" | "." => {}
            ".." => return None,
            c => p.push(win_component(c)),
        }
    }
    Some(p)
}

/// Vede symlink `target` (uložený v `entry` – relativní cesta v archivu) dovnitř archivu?
pub fn link_stays_inside(entry: &Path, target: &str) -> bool {
    if target.starts_with('/') || target.starts_with('\\') || target.as_bytes().get(1) == Some(&b':') {
        return false;
    }
    let mut depth: i64 = entry.parent().map(|p| p.components().count() as i64).unwrap_or(0);
    for c in target.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => depth += 1,
        }
    }
    true
}

#[cfg(windows)]
fn make_symlink(target: &Path, link: &Path, dir: bool) -> std::io::Result<()> {
    if dir { std::os::windows::fs::symlink_dir(target, link) } else { std::os::windows::fs::symlink_file(target, link) }
}

#[cfg(not(windows))]
fn make_symlink(target: &Path, link: &Path, _dir: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// Rozbalí tar do složky `dest` (ochrana: `..`, absolutní cesty, symlinky ven se přeskočí).
pub fn extract_tar<R: Read>(r: R, dest: &Path) -> Result<ExtractReport, String> {
    let mut rep = ExtractReport::default();
    let mut ar = tar::Archive::new(r);
    let entries = ar.entries().map_err(|e| e.to_string())?;
    for e in entries {
        let mut e = e.map_err(|e| e.to_string())?;
        let raw = String::from_utf8_lossy(&e.path_bytes()).into_owned();
        let Some(rel) = safe_rel_path(&raw) else {
            rep.skipped.push(tf!("{raw} – cesta mimo cíl", "{raw} – path outside the target"));
            continue;
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target = dest.join(&rel);
        let kind = e.header().entry_type();
        let mkparent = |t: &Path| t.parent().map(std::fs::create_dir_all).transpose().map_err(|e| format!("{}: {e}", t.display()));
        match kind {
            tar::EntryType::Directory => {
                std::fs::create_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
                rep.dirs += 1;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous | tar::EntryType::GNUSparse => {
                mkparent(&target)?;
                // existující symlink v cíli nahradit, nepsat skrz něj
                if std::fs::symlink_metadata(&target).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
                    let _ = std::fs::remove_file(&target);
                }
                let mut f = std::fs::File::create(&target).map_err(|e| format!("{}: {e}", target.display()))?;
                rep.bytes += std::io::copy(&mut e, &mut f).map_err(|e| format!("{}: {e}", target.display()))?;
                if let Ok(m) = e.header().mtime() {
                    let _ = f.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(m));
                }
                rep.files += 1;
            }
            tar::EntryType::Symlink | tar::EntryType::Link => {
                let link = e.link_name_bytes().map(|l| String::from_utf8_lossy(&l).into_owned()).unwrap_or_default();
                if kind == tar::EntryType::Link {
                    // pevný odkaz: cíl je cesta v archivu – zkopírovat už rozbalený soubor
                    match safe_rel_path(&link).map(|p| dest.join(p)) {
                        Some(src) if src.is_file() => {
                            mkparent(&target)?;
                            rep.bytes += std::fs::copy(&src, &target).map_err(|e| format!("{}: {e}", target.display()))?;
                            rep.files += 1;
                        }
                        _ => rep.skipped.push(tf!("{raw} – pevný odkaz mimo archiv", "{raw} – hard link outside the archive")),
                    }
                    continue;
                }
                if !link_stays_inside(&rel, &link) {
                    rep.skipped.push(tf!("{raw} – symlink mimo cílovou složku ({link})", "{raw} – symlink outside the target folder ({link})"));
                    continue;
                }
                mkparent(&target)?;
                let t: PathBuf = link.split('/').filter(|c| !c.is_empty() && *c != ".").map(|c| if c == ".." { "..".to_string() } else { win_component(c) }).collect();
                let is_dir = target.parent().map(|p| p.join(&t).is_dir()).unwrap_or(false);
                if make_symlink(&t, &target, is_dir).is_err() {
                    rep.skipped.push(tf!("{raw} – symlink (ve Windows nelze vytvořit)", "{raw} – symlink (cannot be created on Windows)"));
                }
            }
            _ => rep.skipped.push(tf!("{raw} – speciální soubor", "{raw} – special file")),
        }
    }
    Ok(rep)
}

/// Stáhne položky z kontejneru do Windows složky.
pub fn download(d: &Docker, id: &str, paths: &[String], dest: &Path) -> Result<ExtractReport, String> {
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let mut total = ExtractReport::default();
    for p in paths {
        let p = normalize_path(p);
        if p == "/" {
            return Err(t!("Celý kořenový adresář nelze stáhnout", "The whole root directory cannot be downloaded").into());
        }
        let resp = d.request("GET", &archive_url(id, &p)).call().map_err(|e| path_err(&p, e.into()))?;
        let r = extract_tar(resp.into_reader(), dest)?;
        total.files += r.files;
        total.dirs += r.dirs;
        total.bytes += r.bytes;
        total.skipped.extend(r.skipped);
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_helpers() {
        assert_eq!(normalize_path(""), "/");
        assert_eq!(normalize_path("/"), "/");
        assert_eq!(normalize_path("etc//apk/"), "/etc/apk");
        assert_eq!(normalize_path("/etc/./apk/../ssl"), "/etc/ssl");
        assert_eq!(normalize_path("/../.."), "/");
        assert_eq!(normalize_path("/a b/č"), "/a b/č");
        assert_eq!(base_name("/etc/hosts"), "hosts");
        assert_eq!(base_name("/etc/"), "etc");
        assert_eq!(contents_path("/"), "/.");
        assert_eq!(contents_path("/etc"), "/etc/.");
        assert_eq!(join("/", "a b č.txt"), "/a b č.txt");
        assert_eq!(join("/tmp", "x"), "/tmp/x");
        assert_eq!(parent("/tmp/x"), "/tmp");
        assert_eq!(archive_url("abc", "/a b/č"), "/containers/abc/archive?path=%2Fa%20b%2F%C4%8D");
    }

    #[test]
    fn demux_frames() {
        let mut s = vec![1, 0, 0, 0, 0, 0, 0, 3];
        s.extend_from_slice(b"abc");
        s.extend_from_slice(&[2, 0, 0, 0, 0, 0, 0, 2]);
        s.extend_from_slice(b"er");
        s.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 1]);
        s.extend_from_slice(b"d");
        let (o, e) = demux(&s);
        assert_eq!((o.as_slice(), e.as_slice()), (&b"abcd"[..], &b"er"[..]));
        // TTY – bez rámců
        assert_eq!(demux(b"hello").0, b"hello");
    }

    #[test]
    fn parses_gnu_stat_listing() {
        let out = "/etc\nF\t41ed\t4096\t1700000000\t755\troot\t./apk\nF\t81a4\t12\t1700000001\t644\troot\t./a b č.txt\nF\ta1ff\t7\t1700000002\t777\troot\t./link\nF\t81a4\t3\t1\t600\tuser\t./tab\there\nF\t81a4\t3\t1\t600\tuser\t./new\nline\nD\tlink\n";
        let (cwd, e) = parse_exec_listing(out);
        assert_eq!(cwd, "/etc");
        let names: Vec<&str> = e.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["apk", "link", "a b č.txt", "new\nline", "tab\there"]);
        assert!(e[0].is_dir() && !e[0].is_link());
        assert!(e[1].is_link() && e[1].is_dir());
        assert_eq!((e[2].size, e[2].mtime, e[2].mode.as_str(), e[2].owner.as_str()), (12, 1700000001, "644", "root"));
    }

    #[test]
    fn parses_busybox_stat_listing() {
        // busybox stat bez find: názvy bez ./
        let out = "/\nF\t41ed\t4096\t1776269392\t755\troot\tbin\nF\ta1ff\t12\t1776269392\t777\troot\tsbin\nF\t21b6\t0\t1\t666\troot\tnull\n";
        let (_, e) = parse_exec_listing(out);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].name, "bin");
        assert_eq!(e[1].kind, 'c');
        assert_eq!(e[2].name, "sbin");
        assert!(e[2].is_link() && !e[2].is_dir());
    }

    #[test]
    fn parses_ls_fallback() {
        let out = concat!(
            "/data\n",
            "L\ttotal 160\n",
            "L\tdrwxr-xr-x    1 root     root          4096 2026-11-18 20:04:12 +0000 .\n",
            "L\tdr-xr-xr-x    1 root     root          4096 2026-11-18 20:04:11 +0000 ..\n",
            "L\t-rw-r--r--    1 root     root             8 2026-04-15 07:41:00 +0000 a  b č.txt\n",
            "L\tlrwxrwxrwx    1 root     root            12 2026-04-15 07:41:00 +0000 sh -> /bin/busybox\n",
            "L\tcrw-rw-rw-    1 root     root       1,   3 2026-04-15 07:41:00 +0000 null\n",
            "L\tdrwxr-sr-x    2 app      app           4096 2026-04-15 07:41:00.123456789 +0200 dir\twith tab\n",
            "D\tnone\n",
        );
        let (cwd, e) = parse_exec_listing(out);
        assert_eq!(cwd, "/data");
        let names: Vec<&str> = e.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["dir\twith tab", "a  b č.txt", "null", "sh"]);
        assert_eq!(e[0].mode, "2755");
        assert_eq!(e[0].owner, "app");
        assert_eq!(e[1].size, 8);
        assert_eq!(e[1].mtime, days_from_civil(2026, 4, 15) * 86_400 + 7 * 3600 + 41 * 60);
        assert_eq!(e[0].mtime, days_from_civil(2026, 4, 15) * 86_400 + 5 * 3600 + 41 * 60);
        assert_eq!(e[2].kind, 'c');
        assert_eq!(e[3].kind, 'l');
    }

    #[test]
    fn parses_classic_ls_line() {
        let e = parse_ls_line("-rw-r--r--    1 root     root          89 May  6  2024 fstab").unwrap();
        assert_eq!((e.name.as_str(), e.size, e.mode.as_str()), ("fstab", 89, "644"));
        assert_eq!(e.mtime, days_from_civil(2024, 5, 6) * 86_400);
        assert!(parse_ls_line("total 12").is_none());
        let e = parse_ls_line("-rwsr-xr-x 1 root root 100 Jan  1 10:00 su do").unwrap();
        assert_eq!((e.name.as_str(), e.mode.as_str()), ("su do", "4755"));
    }

    #[test]
    fn decodes_stat_header() {
        // z Podmanu pro /etc
        let s = decode_stat("eyJuYW1lIjoiZXRjIiwic2l6ZSI6NDA5NiwibW9kZSI6MjE0NzQ4NDE0MSwibXRpbWUiOiIyMDI2LTExLTE4VDIxOjA0OjEyLjA0ODcxNzY2NCswMTowMCIsImlzRGlyIjp0cnVlLCJsaW5rVGFyZ2V0IjoiL2V0YyJ9").unwrap();
        assert_eq!(s.name, "etc");
        assert!(s.is_dir());
        assert_eq!(s.mode & 0o777, 0o755);
        let f = PathStat { mode: 0o644, ..Default::default() };
        assert!(!f.is_dir());
        assert!(PathStat { mode: 1 << 31, ..Default::default() }.is_dir());
    }

    fn tar_of(entries: &[(&str, tar::EntryType, &[u8], &str)]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        for (name, kind, data, link) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(*kind);
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_mtime(1_700_000_000);
            if !link.is_empty() {
                h.set_link_name(link).unwrap();
            }
            // názvy s `..` a absolutní cesty Builder odmítá – zapíšou se přímo do hlavičky
            let nb = name.as_bytes();
            let gnu = h.as_gnu_mut().unwrap();
            gnu.name[..nb.len()].copy_from_slice(nb);
            h.set_cksum();
            b.append(&h, *data).unwrap();
        }
        b.into_inner().unwrap()
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wls-cfs-test-{tag}-{}-{}", std::process::id(), now_nanos()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rel_path_guard() {
        assert!(safe_rel_path("../x").is_none());
        assert!(safe_rel_path("a/../../x").is_none());
        assert!(safe_rel_path("/etc/passwd").is_none());
        assert!(safe_rel_path("C:/Windows/x").is_none());
        assert!(safe_rel_path("\\x").is_none());
        assert_eq!(safe_rel_path("./a/b.txt").unwrap(), PathBuf::from("a").join("b.txt"));
        assert_eq!(safe_rel_path("a/con.txt").unwrap(), PathBuf::from("a").join("_con.txt"));
        assert_eq!(safe_rel_path("a:b?").unwrap(), PathBuf::from("a_b_"));
        assert!(link_stays_inside(Path::new("d/link"), "../d/x"));
        assert!(link_stays_inside(Path::new("d/link"), "x"));
        assert!(!link_stays_inside(Path::new("d/link"), "../../x"));
        assert!(!link_stays_inside(Path::new("link"), "/etc/passwd"));
    }

    #[test]
    fn extraction_rejects_traversal_and_absolute() {
        let dest = tmp_dir("extract");
        let data = tar_of(&[
            ("ok/", tar::EntryType::Directory, b"", ""),
            ("ok/a b č.txt", tar::EntryType::Regular, b"hello", ""),
            ("../evil.txt", tar::EntryType::Regular, b"x", ""),
            ("/abs.txt", tar::EntryType::Regular, b"x", ""),
            ("ok/../../evil2.txt", tar::EntryType::Regular, b"x", ""),
            ("ok/out", tar::EntryType::Symlink, b"", "../../outside"),
            ("ok/abs", tar::EntryType::Symlink, b"", "/etc/passwd"),
            ("ok/hard", tar::EntryType::Link, b"", "ok/a b č.txt"),
        ]);
        let rep = extract_tar(std::io::Cursor::new(data), &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("ok").join("a b č.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(dest.join("ok").join("hard")).unwrap(), b"hello");
        assert_eq!((rep.files, rep.dirs), (2, 1));
        assert_eq!(rep.skipped.len(), 5, "{:?}", rep.skipped);
        assert!(!dest.parent().unwrap().join("evil.txt").exists());
        assert!(!dest.join("abs.txt").exists());
        assert!(!dest.join("ok").join("out").exists() && !dest.join("ok").join("abs").exists());
        std::fs::remove_dir_all(&dest).unwrap();
    }

    #[test]
    fn lists_first_level_of_tar() {
        let data = tar_of(&[
            ("./", tar::EntryType::Directory, b"", ""),
            ("./apk/", tar::EntryType::Directory, b"", ""),
            ("./apk/arch", tar::EntryType::Regular, b"x86", ""),
            ("./hosts", tar::EntryType::Regular, b"127.0.0.1", ""),
            ("./ssl", tar::EntryType::Symlink, b"", "apk"),
            ("./a b č.txt", tar::EntryType::Regular, b"1", ""),
        ]);
        let (v, trunc) = list_tar(std::io::Cursor::new(data.clone()), u64::MAX);
        assert!(!trunc);
        let names: Vec<&str> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["apk", "ssl", "a b č.txt", "hosts"]);
        assert!(v[1].is_link() && v[1].is_dir());
        assert_eq!(v[3].size, 9);
        assert_eq!(v[3].mode, "644");
        // limit -> neúplný výpis
        let (_, trunc) = list_tar(std::io::Cursor::new(data), 1024);
        assert!(trunc);
    }

    #[test]
    fn builds_upload_tar() {
        let src = tmp_dir("upload");
        let dir = src.join("složka s mezerou");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a b č.txt"), "ahoj").unwrap();
        std::fs::write(dir.join("sub").join("x.bin"), [0u8, 1, 2]).unwrap();
        let single = src.join("ž.txt");
        std::fs::write(&single, "1").unwrap();
        let (buf, files) = build_upload_tar(Vec::new(), &[dir.clone(), single.clone()]).unwrap();
        assert_eq!(files, 3);
        let mut ar = tar::Archive::new(std::io::Cursor::new(buf.clone()));
        let names: Vec<String> = ar.entries().unwrap().map(|e| String::from_utf8(e.unwrap().path_bytes().into_owned()).unwrap()).collect();
        assert!(names.contains(&"složka s mezerou/a b č.txt".to_string()), "{names:?}");
        assert!(names.contains(&"složka s mezerou/sub/x.bin".to_string()), "{names:?}");
        assert!(names.contains(&"ž.txt".to_string()), "{names:?}");
        // zpětné rozbalení dá stejné bajty
        let out = tmp_dir("upload-out");
        extract_tar(std::io::Cursor::new(buf), &out).unwrap();
        assert_eq!(std::fs::read(out.join("složka s mezerou").join("a b č.txt")).unwrap(), b"ahoj");
        assert_eq!(std::fs::read(out.join("složka s mezerou").join("sub").join("x.bin")).unwrap(), [0u8, 1, 2]);
        std::fs::remove_dir_all(&src).unwrap();
        std::fs::remove_dir_all(&out).unwrap();
    }

    #[test]
    fn single_file_tar_roundtrip() {
        let t = single_file_tar("a b.txt", "č\n".as_bytes(), 0o600, Some((1000, 1000, Some("app"), None))).unwrap();
        let mut ar = tar::Archive::new(std::io::Cursor::new(t));
        let mut e = ar.entries().unwrap().next().unwrap().unwrap();
        assert_eq!(e.path_bytes().as_ref(), b"a b.txt");
        assert_eq!(e.header().mode().unwrap(), 0o600);
        assert_eq!(e.header().uid().unwrap(), 1000);
        let mut s = String::new();
        e.read_to_string(&mut s).unwrap();
        assert_eq!(s, "č\n");
    }

    /// Živý test proti enginu (WLS_PORT, výchozí 2375): kontejner `wls-test-cfs` z alpine:3.20.
    /// `cargo test live_container_fs -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_container_fs() {
        let port: u16 = std::env::var("WLS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2375);
        let d = Docker::new(port);
        assert!(d.ping(), "API na {port} neodpovídá");
        const NAME: &str = "wls-test-cfs";
        let cleanup = || {
            let _ = d.container_action(NAME, "remove");
        };
        cleanup();
        d.pull("alpine:3.20").expect("pull");
        let v: Value = serde_json::from_str(
            &d.post_json(&format!("/containers/create?name={NAME}"), json!({ "Image": "docker.io/library/alpine:3.20", "Cmd": ["sleep", "300"] })).expect("create"),
        )
        .unwrap();
        let id = v["Id"].as_str().unwrap().to_string();
        d.container_action(&id, "start").expect("start");
        let src = tmp_dir("live-src");
        let out = tmp_dir("live-out");
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // výpis přes exec
            let l = list(&d, &id, "/etc").expect("list /etc");
            println!("[exec] /etc: {} položek, via_archive={}, např. {:?}", l.entries.len(), l.via_archive, l.entries.iter().take(6).map(|e| (&e.name, e.kind, e.size, &e.mode, &e.owner)).collect::<Vec<_>>());
            assert!(!l.via_archive && l.cwd == "/etc");
            assert!(l.entries.iter().any(|e| e.name == "passwd" && e.kind == 'f' && e.size > 0 && e.mtime > 1_000_000_000 && e.owner == "root"));
            assert!(l.entries.iter().any(|e| e.name == "apk" && e.is_dir()));
            let bin = list(&d, &id, "/bin").expect("list /bin");
            let sh = bin.entries.iter().find(|e| e.name == "sh").expect("sh");
            assert!(sh.is_link() && !sh.is_dir(), "{sh:?}");
            println!("[exec] /bin: {} položek, sh = {:?}", bin.entries.len(), (sh.kind, sh.target_kind));
            assert!(list(&d, &id, "/neexistuje").is_err());

            // nahrání složky se souborem „a b č.txt“
            let folder = src.join("wls cfs č");
            std::fs::create_dir_all(folder.join("sub")).unwrap();
            let content: Vec<u8> = "ahoj světe\n".bytes().chain((0u8..=255).cycle().take(70_000)).collect();
            std::fs::write(folder.join("a b č.txt"), &content).unwrap();
            std::fs::write(folder.join("sub").join("x.txt"), "x").unwrap();
            let mut ticks = 0;
            let (files, size) = upload(&d, &id, std::slice::from_ref(&folder), "/tmp", |_, _| ticks += 1).expect("upload");
            println!("[upload] {files} souborů, tar {size} B, průběh hlášen {ticks}×");
            assert_eq!(files, 2);
            let l = list(&d, &id, "/tmp/wls cfs č").expect("list uploaded");
            let f = l.entries.iter().find(|e| e.name == "a b č.txt").expect("a b č.txt");
            println!("[exec] /tmp/wls cfs č: {:?}", l.entries.iter().map(|e| (&e.name, e.size)).collect::<Vec<_>>());
            assert_eq!(f.size, content.len() as u64);

            // stažení zpět a porovnání bajtů
            let rep = download(&d, &id, &["/tmp/wls cfs č".to_string()], &out).expect("download");
            println!("[download] {} souborů, {} složek, {} B, přeskočeno {:?}", rep.files, rep.dirs, rep.bytes, rep.skipped);
            assert_eq!(std::fs::read(out.join("wls cfs č").join("a b č.txt")).unwrap(), content);
            assert_eq!(std::fs::read(out.join("wls cfs č").join("sub").join("x.txt")).unwrap(), b"x");

            // editor: nový soubor, uložení, načtení, druhé uložení (práva se zachovají)
            let ed = "/tmp/wls cfs č/edit me.txt";
            write_text(&d, &id, ed, "ahoj\nsvěte\n").expect("write");
            assert_eq!(read_text(&d, &id, ed).expect("read").1, "ahoj\nsvěte\n");
            exec_ok(&d, &id, &["chmod", "600", "--", ed]).unwrap();
            write_text(&d, &id, ed, "změna\n").expect("write 2");
            let (p, t) = read_text(&d, &id, ed).expect("read 2");
            assert_eq!((p.as_str(), t.as_str()), (ed, "změna\n"));
            let m = list(&d, &id, "/tmp/wls cfs č").unwrap().entries.into_iter().find(|e| e.name == "edit me.txt").unwrap();
            println!("[edit] {ed}: {:?}, práva {}", t, m.mode);
            assert_eq!(m.mode, "600");
            assert!(read_text(&d, &id, "/tmp/wls cfs č/a b č.txt").is_err(), "binární soubor");

            // nová složka, přejmenování
            mkdir(&d, &id, "/tmp/wls cfs č/nová složka").expect("mkdir");
            assert!(mkdir(&d, &id, "/tmp/wls cfs č/nová složka").is_err());
            rename(&d, &id, "/tmp/wls cfs č/nová složka", "/tmp/wls cfs č/přejmenovaná").expect("rename");
            assert!(rename(&d, &id, "/tmp/wls cfs č/sub", "/tmp/wls cfs č/přejmenovaná").is_err(), "cíl existuje");
            let names: Vec<String> = list(&d, &id, "/tmp/wls cfs č").unwrap().entries.into_iter().map(|e| e.name).collect();
            println!("[mkdir+rename] {names:?}");
            assert!(names.contains(&"přejmenovaná".to_string()) && !names.contains(&"nová složka".to_string()));

            // smazání
            delete(&d, &id, &["/tmp/wls cfs č".to_string()]).expect("delete");
            assert!(list(&d, &id, "/tmp/wls cfs č").is_err());
            assert!(delete(&d, &id, &["/".to_string()]).is_err());
            println!("[delete] /tmp/wls cfs č smazána");

            // zastavený kontejner: výpis a stažení přes archiv
            upload(&d, &id, std::slice::from_ref(&folder), "/tmp", |_, _| {}).expect("upload 2");
            d.container_action(&id, "stop").expect("stop");
            assert!(!is_running(&d, &id).unwrap());
            let l = list(&d, &id, "/etc").expect("archive list /etc");
            println!("[archive] /etc: {} položek, via_archive={}, truncated={}", l.entries.len(), l.via_archive, l.truncated);
            assert!(l.via_archive && !l.truncated);
            assert!(l.entries.iter().any(|e| e.name == "passwd" && e.kind == 'f' && e.size > 0));
            assert!(l.entries.iter().any(|e| e.name == "apk" && e.is_dir()));
            let root = list(&d, &id, "/").expect("archive list /");
            println!("[archive] /: {:?} truncated={}", root.entries.iter().map(|e| &e.name).collect::<Vec<_>>(), root.truncated);
            assert!(root.entries.iter().any(|e| e.name == "etc" && e.is_dir()));
            let l = list(&d, &id, "/tmp/wls cfs č").expect("archive list uploaded");
            assert!(l.via_archive && l.entries.iter().any(|e| e.name == "a b č.txt" && e.size == content.len() as u64));
            let out2 = out.join("stopped");
            let rep = download(&d, &id, &["/tmp/wls cfs č/a b č.txt".to_string(), "/tmp/wls cfs č/sub".to_string()], &out2).expect("download stopped");
            println!("[archive download] {} souborů, {} B", rep.files, rep.bytes);
            assert_eq!(std::fs::read(out2.join("a b č.txt")).unwrap(), content);
            assert_eq!(std::fs::read(out2.join("sub").join("x.txt")).unwrap(), b"x");
            assert!(delete(&d, &id, &["/tmp/x".to_string()]).is_err(), "exec na zastaveném kontejneru");
        }));
        cleanup();
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&out);
        assert!(d.get::<Value>(&format!("/containers/{NAME}/json")).is_err(), "kontejner odstraněn");
        println!("[cleanup] {NAME} odstraněn");
        if let Err(e) = res {
            std::panic::resume_unwind(e);
        }
    }
}
