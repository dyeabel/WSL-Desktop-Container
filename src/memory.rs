//! Přehled paměti WSL: VmmemWSL ve Windows, /proc/meminfo v Linuxu, kontejnery a engine,
//! historie za poslední hodinu a výsledky uvolnění paměti.
//!
//! Čistá logika (parsování, výpočty, historie) je testovatelná bez WSL; dotazy na Windows
//! (Win32) a na distribuci volá worker.

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Jak dlouho držet historii (s).
pub const HISTORY_SECS: f64 = 3600.0;
/// Vzorky bližší než tolik sekund se slučují do jednoho bodu (průměr).
pub const BUCKET_SECS: f64 = 10.0;
/// Minimální vzorek pro historii, když přehled není vidět (s).
pub const IDLE_SAMPLE_SECS: f64 = 60.0;
/// Záložní dotaz na VmmemWSL přes PowerShell nejvýš jednou za tolik sekund.
const PS_FALLBACK_SECS: u64 = 10;

/// Procesy enginu v distribuci (`ps -o comm=` je zkrácené na 15 znaků).
const ENGINE_PROCS: [&str; 10] =
    ["dockerd", "containerd", "docker-proxy", "podman", "conmon", "socat", "rootlessport", "aardvark-dns", "netavark", "catatonit"];

/// Příkaz pro jedno volání `wsl -e sh -c …`: /proc/meminfo, oddělovač, RSS procesů
/// a čítače rozhraní (provoz VM do historie).
/// Na konci řádek `cpu` z /proc/stat (vytížení CPU VM v kartě enginu).
pub const SAMPLE_SCRIPT: &str =
    "cat /proc/meminfo; echo @@ps; ps -e -o comm=,rss= 2>/dev/null; echo @@net; cat /proc/net/dev 2>/dev/null; echo @@cpu; head -n 1 /proc/stat; true";
/// Minimální vzorek – /proc/meminfo, čítače rozhraní a řádek `cpu`.
pub const SAMPLE_SCRIPT_MIN: &str = "cat /proc/meminfo; echo @@net; cat /proc/net/dev 2>/dev/null; echo @@cpu; head -n 1 /proc/stat; true";

pub fn now_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

// ---------------------------------------------------------------- /proc/meminfo

/// Vybrané hodnoty z /proc/meminfo v bajtech.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MemInfo {
    pub total: u64,
    pub free: u64,
    pub available: u64,
    pub buffers: u64,
    pub cached: u64,
    pub sreclaimable: u64,
}

impl MemInfo {
    /// Použito procesy = MemTotal − MemFree − Buffers − Cached − SReclaimable.
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free).saturating_sub(self.cache())
    }
    /// Souborová cache = Cached + Buffers + SReclaimable.
    pub fn cache(&self) -> u64 {
        self.cached + self.buffers + self.sreclaimable
    }
}

/// Načte /proc/meminfo (hodnoty v kB); bez MemTotal vrací None.
pub fn parse_meminfo(text: &str) -> Option<MemInfo> {
    let mut m = MemInfo::default();
    let mut has_total = false;
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else { continue };
        let mut it = v.split_whitespace();
        let Some(n) = it.next().and_then(|n| n.parse::<u64>().ok()) else { continue };
        let bytes = match it.next() {
            Some(u) if u.eq_ignore_ascii_case("kB") => n * 1024,
            Some(_) => continue,
            None => n,
        };
        match k.trim() {
            "MemTotal" => {
                m.total = bytes;
                has_total = true;
            }
            "MemFree" => m.free = bytes,
            "MemAvailable" => m.available = bytes,
            "Buffers" => m.buffers = bytes,
            "Cached" => m.cached = bytes,
            "SReclaimable" => m.sreclaimable = bytes,
            _ => {}
        }
    }
    has_total.then_some(m)
}

/// Součet RSS procesů enginu z výstupu `ps -e -o comm=,rss=` (rss v kB).
pub fn engine_rss(ps: &str) -> u64 {
    ps.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (comm, rss) = (it.next()?, it.next()?.parse::<u64>().ok()?);
            ENGINE_PROCS.iter().any(|p| comm == *p || (*p == "containerd" && comm.starts_with("containerd"))).then_some(rss * 1024)
        })
        .sum()
}

/// Rozdělí výstup `SAMPLE_SCRIPT` na meminfo a (volitelně) RSS enginu.
pub fn parse_sample(out: &str) -> Option<(MemInfo, Option<u64>)> {
    let out = out.split_once("@@net").map_or(out, |(a, _)| a);
    match out.split_once("@@ps") {
        Some((mi, ps)) => Some((parse_meminfo(mi)?, Some(engine_rss(ps)))),
        None => Some((parse_meminfo(out)?, None)),
    }
}

/// Kumulované bajty eth0 VM (rx, tx) z části `@@net` výstupu vzorku.
pub fn parse_net(out: &str) -> Option<(u64, u64)> {
    let (_, dev) = out.split_once("@@net")?;
    let dev = dev.split_once("@@cpu").map_or(dev, |(a, _)| a);
    crate::netinfo::parse_proc_net_dev(dev).into_iter().find(|i| i.name == "eth0").map(|i| (i.rx, i.tx))
}

/// Kumulované tiky CPU (zaneprázdněno, celkem) z řádku `cpu` v části `@@cpu` výstupu vzorku.
/// Celkem = user..steal (guest je už v user); nečinnost = idle + iowait.
pub fn parse_cpu(out: &str) -> Option<(u64, u64)> {
    let (_, rest) = out.split_once("@@cpu")?;
    let line = rest.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line.split_whitespace().skip(1).take(8).map(|x| x.parse().unwrap_or(0)).collect();
    if v.len() < 4 {
        return None;
    }
    let total: u64 = v.iter().sum();
    let idle = v[3] + v.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

/// Vytížení CPU (0–100 %) mezi dvěma vzorky `parse_cpu`; None bez přírůstku (nebo po restartu VM).
pub fn cpu_pct(prev: (u64, u64), cur: (u64, u64)) -> Option<f64> {
    let total = cur.1.checked_sub(prev.1).filter(|t| *t > 0)?;
    let busy = cur.0.checked_sub(prev.0)?;
    Some((busy as f64 / total as f64 * 100.0).clamp(0.0, 100.0))
}

/// Velikost z .wslconfig (`8GB`, `4096MB`, `512 KB`, `2G`, bajty bez jednotky).
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mul = match unit.trim().to_ascii_uppercase().as_str() {
        "" | "B" => 1.0,
        "K" | "KB" => 1024.0,
        "M" | "MB" => 1024.0 * 1024.0,
        "G" | "GB" => 1024.0 * 1024.0 * 1024.0,
        "T" | "TB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let v = (n * mul) as u64;
    (v > 0).then_some(v)
}

/// Limit paměti WSL: `memory` z .wslconfig, jinak 50 % RAM.
pub fn limit_from(wslconfig: &str, ram: u64) -> (u64, bool) {
    match crate::wslconfig::get(wslconfig, "wsl2", "memory").and_then(|v| parse_size(&v)) {
        Some(v) => (v, true),
        None => (ram / 2, false),
    }
}

// ---------------------------------------------------------------- rozpad

/// Kam jde paměť WSL (bajty).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Breakdown {
    /// pracovní sada VmmemWSL ve Windows (None = nezjištěno)
    pub vmmem: Option<u64>,
    /// limit WSL (.wslconfig nebo 50 % RAM)
    pub limit: u64,
    /// limit pochází z .wslconfig
    pub limit_set: bool,
    /// celková RAM Windows
    pub ram: u64,
    /// MemTotal v Linuxu
    pub total: u64,
    /// použito procesy
    pub used: u64,
    /// souborová cache
    pub cache: u64,
    /// volná v Linuxu
    pub free: u64,
    /// VmmemWSL − (used + cache), ≥ 0 – zatím nevráceno Windows
    pub unreturned: u64,
    /// součet paměti kontejnerů (bez neaktivní cache, jako statistiky)
    pub containers: u64,
    /// RSS procesů enginu
    pub engine: u64,
    /// zbytek distribuce = used − containers − engine
    pub rest: u64,
}

impl Breakdown {
    /// Celková šířka pruhu: limit, nebo víc, když ho VmmemWSL / Linux přesahuje.
    pub fn scale(&self) -> u64 {
        self.limit.max(self.vmmem.unwrap_or(0)).max(self.used + self.cache + self.unreturned).max(1)
    }
}

/// Spočítá rozpad. Kontejnery a engine se ořežou tak, aby nikdy nepřesáhly „used“
/// (statistiky kontejnerů obsahují i aktivní souborovou cache, kterou Linux počítá do Cached).
pub fn breakdown(mi: &MemInfo, vmmem: Option<u64>, (limit, limit_set): (u64, bool), ram: u64, containers: u64, engine: u64) -> Breakdown {
    let used = mi.used();
    let cache = mi.cache();
    let containers = containers.min(used);
    let engine = engine.min(used - containers);
    Breakdown {
        vmmem,
        limit,
        limit_set,
        ram,
        total: mi.total,
        used,
        cache,
        free: mi.free,
        unreturned: vmmem.map(|v| v.saturating_sub(used + cache)).unwrap_or(0),
        containers,
        engine,
        rest: used - containers - engine,
    }
}

/// Aktuální stav (plný vzorek, když je přehled vidět).
#[derive(Debug, Clone, Default)]
pub struct MemNow {
    pub b: Breakdown,
}

// ---------------------------------------------------------------- historie

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MemPoint {
    /// unixový čas (s) začátku bucketu
    pub t: f64,
    pub vmmem: Option<u64>,
    pub used: u64,
    pub cache: u64,
    /// počet sloučených vzorků
    pub n: u32,
}

impl MemPoint {
    pub fn new(t: f64, vmmem: Option<u64>, used: u64, cache: u64) -> Self {
        Self { t, vmmem, used, cache, n: 1 }
    }
}

/// Jedno uvolnění paměti (ruční nebo automatické).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FreeEvent {
    pub t: f64,
    pub auto: bool,
    pub vmmem_before: Option<u64>,
    pub vmmem_after: Option<u64>,
    pub cache_before: u64,
    pub cache_after: u64,
}

#[derive(Debug, Clone, Default)]
pub struct History {
    pub points: VecDeque<MemPoint>,
    pub events: VecDeque<FreeEvent>,
}

fn avg(a: u64, n: u32, b: u64) -> u64 {
    ((a as u128 * n as u128 + b as u128) / (n as u128 + 1)) as u64
}

impl History {
    /// Přidá vzorek; vzorky do `BUCKET_SECS` od začátku posledního bodu se zprůměrují.
    pub fn push(&mut self, p: MemPoint) {
        if let Some(last) = self.points.back_mut() {
            if p.t >= last.t && p.t - last.t < BUCKET_SECS {
                let n = last.n;
                last.used = avg(last.used, n, p.used);
                last.cache = avg(last.cache, n, p.cache);
                last.vmmem = match (last.vmmem, p.vmmem) {
                    (Some(a), Some(b)) => Some(avg(a, n, b)),
                    (a, b) => b.or(a),
                };
                last.n = n.saturating_add(1);
                self.trim(p.t);
                return;
            }
        }
        self.points.push_back(p);
        self.trim(p.t);
    }

    /// Doplní body a uvolnění načtené z disku před ta, která už v historii jsou.
    pub fn prefill(&mut self, points: Vec<MemPoint>, events: Vec<FreeEvent>, now: f64) {
        let first = self.points.front().map(|p| p.t).unwrap_or(f64::INFINITY);
        for p in points.into_iter().rev().filter(|p| p.t < first) {
            self.points.push_front(p);
        }
        let first = self.events.front().map(|e| e.t).unwrap_or(f64::INFINITY);
        for e in events.into_iter().rev().filter(|e| e.t < first) {
            self.events.push_front(e);
        }
        self.trim(now);
    }

    pub fn add_event(&mut self, e: FreeEvent) {
        self.events.push_back(e);
        self.trim(e.t);
    }

    /// Zahodí body a události starší než hodinu před `now`.
    pub fn trim(&mut self, now: f64) {
        let min = now - HISTORY_SECS;
        while self.points.front().is_some_and(|p| p.t < min) {
            self.points.pop_front();
        }
        while self.events.front().is_some_and(|e| e.t < min) {
            self.events.pop_front();
        }
    }
}

/// Krátký souhrn posledního vzorku pro kartu enginu v bočním panelu.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Glance {
    /// použito procesy v Linuxu
    pub used: u64,
    /// MemTotal v Linuxu (≈ limit WSL)
    pub total: u64,
    pub cache: u64,
    /// VmmemWSL – kolik RAM Windows si celá WSL VM opravdu bere
    pub vmmem: Option<u64>,
    /// fyzická RAM Windows (0 = neznámá)
    pub ram: u64,
    /// z toho obsazeno ve Windows celkem (všechny procesy včetně VmmemWSL)
    pub win_used: u64,
    /// vytížení CPU VM (%) od předchozího vzorku
    pub cpu: Option<f64>,
}

/// Stav sdílený workerem a UI.
#[derive(Debug, Default)]
pub struct MemState {
    pub history: History,
    pub now: Option<MemNow>,
    /// čas posledního vzorku (unix s)
    pub last_sample: f64,
    /// souhrn pro kartu enginu
    pub glance: Option<Glance>,
    /// tiky CPU z předchozího vzorku (zaneprázdněno, celkem)
    pub cpu_ticks: Option<(u64, u64)>,
}

// ---------------------------------------------------------------- Windows

/// Zjištění VmmemWSL: přímo přes Win32, jinak PowerShell nejvýš jednou za 10 s.
#[derive(Debug, Default)]
pub struct VmmemProbe {
    last_ps: Option<(Instant, Option<u64>)>,
}

impl VmmemProbe {
    /// Levná cesta (Win32); když selže, záložní PowerShell s omezenou frekvencí.
    pub fn get(&mut self) -> Option<u64> {
        if let Some(v) = vmmem_direct() {
            return v;
        }
        if let Some((t, v)) = self.last_ps {
            if t.elapsed() < Duration::from_secs(PS_FALLBACK_SECS) {
                return v;
            }
        }
        let v = vmmem_powershell();
        self.last_ps = Some((Instant::now(), v));
        v
    }
}

/// VmmemWSL hned (bez omezení frekvence) – pro měření před/po uvolnění.
pub fn vmmem_now() -> Option<u64> {
    vmmem_direct().unwrap_or_else(vmmem_powershell)
}

fn vmmem_powershell() -> Option<u64> {
    let out = crate::wsl::hidden("powershell.exe")
        .args(["-NoProfile", "-Command", "(Get-Process vmmemWSL,vmmem -ErrorAction SilentlyContinue | Measure-Object WorkingSet64 -Sum).Sum"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse::<u64>().ok().filter(|b| *b > 0)
}

fn is_vmmem(name: &str) -> bool {
    let n = name.strip_suffix(".exe").unwrap_or(name);
    n.eq_ignore_ascii_case("vmmemWSL") || n.eq_ignore_ascii_case("vmmem")
}

/// Pracovní sada VmmemWSL přes Win32. `Some(None)` = proces neběží (VM je vypnutý),
/// `None` = nepodařilo se zjistit (přístup odepřen apod.).
#[cfg(windows)]
pub fn vmmem_direct() -> Option<Option<u64>> {
    use std::sync::atomic::{AtomicBool, Ordering};
    // OpenProcess na vmmemWSL bývá odepřeno – po prvním neúspěchu rovnou seznam procesů
    static TOOLHELP_DENIED: AtomicBool = AtomicBool::new(false);
    if !TOOLHELP_DENIED.load(Ordering::Relaxed) {
        match vmmem_toolhelp() {
            Some(v) => return Some(v),
            None => TOOLHELP_DENIED.store(true, Ordering::Relaxed),
        }
    }
    vmmem_ntquery()
}

#[cfg(not(windows))]
pub fn vmmem_direct() -> Option<Option<u64>> {
    None
}

/// ToolHelp + OpenProcess + K32GetProcessMemoryInfo (u minimálního procesu vmmem bývá odepřeno).
#[cfg(windows)]
fn vmmem_toolhelp() -> Option<Option<u64>> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let mut pids = Vec::new();
    // SAFETY: standardní volání ToolHelp; struktura má nastavenou velikost, handle se zavírá.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e) != 0;
        while ok {
            let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
            if is_vmmem(&String::from_utf16_lossy(&e.szExeFile[..len])) {
                pids.push(e.th32ProcessID);
            }
            ok = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
    }
    if pids.is_empty() {
        return Some(None);
    }
    let mut sum = 0u64;
    for pid in pids {
        // SAFETY: handle z OpenProcess se vždy zavře; struktura má nastavenou velikost.
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return None;
            }
            let mut c: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
            c.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
            let ok = K32GetProcessMemoryInfo(h, &mut c, c.cb) != 0;
            CloseHandle(h);
            if !ok {
                return None;
            }
            sum += c.WorkingSetSize as u64;
        }
    }
    Some(Some(sum))
}

/// Seznam procesů z NtQuerySystemInformation – nepotřebuje handle procesu (stejně jako
/// Get-Process), takže funguje i u chráněného vmmemWSL.
#[cfg(windows)]
fn vmmem_ntquery() -> Option<Option<u64>> {
    use windows_sys::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};
    use windows_sys::Win32::System::WindowsProgramming::SYSTEM_PROCESS_INFORMATION;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
    let mut buf: Vec<u64> = vec![0; 64 * 1024];
    for _ in 0..4 {
        let bytes = (buf.len() * 8) as u32;
        let mut need = 0u32;
        // SAFETY: buffer je zarovnaný (u64) a velikost odpovídá; při nedostatku se zvětší.
        let st = unsafe { NtQuerySystemInformation(SystemProcessInformation, buf.as_mut_ptr().cast(), bytes, &mut need) };
        if st == STATUS_INFO_LENGTH_MISMATCH {
            buf = vec![0; (need as usize + 64 * 1024) / 8 + 1];
            continue;
        }
        if st < 0 {
            return None;
        }
        let base = buf.as_ptr() as *const u8;
        let mut off = 0usize;
        let (mut found, mut sum) = (false, 0u64);
        loop {
            if off + std::mem::size_of::<SYSTEM_PROCESS_INFORMATION>() > bytes as usize {
                break;
            }
            // SAFETY: záznamy leží v bufferu za sebou podle NextEntryOffset (kontrola mezí výše).
            let p = unsafe { &*(base.add(off) as *const SYSTEM_PROCESS_INFORMATION) };
            let name = &p.ImageName;
            if !name.Buffer.is_null() && name.Length > 0 {
                // SAFETY: ImageName ukazuje do stejného bufferu, délka je v bajtech.
                let s = unsafe { std::slice::from_raw_parts(name.Buffer, name.Length as usize / 2) };
                if is_vmmem(&String::from_utf16_lossy(s)) {
                    found = true;
                    sum += p.WorkingSetSize as u64;
                }
            }
            if p.NextEntryOffset == 0 {
                break;
            }
            off += p.NextEntryOffset as usize;
        }
        return Some(found.then_some(sum));
    }
    None
}

/// Celková fyzická RAM Windows.
pub fn total_ram() -> u64 {
    win_ram().0
}

/// Fyzická RAM Windows: (celkem, obsazeno všemi procesy včetně VmmemWSL); (0, 0) = neznámá.
#[cfg(windows)]
pub fn win_ram() -> (u64, u64) {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: struktura má nastavenou velikost.
    unsafe {
        let mut m: MEMORYSTATUSEX = std::mem::zeroed();
        m.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if GlobalMemoryStatusEx(&mut m) != 0 { (m.ullTotalPhys, m.ullTotalPhys.saturating_sub(m.ullAvailPhys)) } else { (0, 0) }
    }
}

#[cfg(not(windows))]
pub fn win_ram() -> (u64, u64) {
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO: &str = "MemTotal:        8134304 kB\nMemFree:         7410380 kB\nMemAvailable:    7508088 kB\nBuffers:            9516 kB\nCached:           225196 kB\nSwapCached:            0 kB\nSReclaimable:      15776 kB\nHugePages_Total:       0\n";
    const K: u64 = 1024;

    #[test]
    fn meminfo_parses_kb_to_bytes() {
        let m = parse_meminfo(MEMINFO).unwrap();
        assert_eq!(m.total, 8_134_304 * K);
        assert_eq!(m.free, 7_410_380 * K);
        assert_eq!(m.available, 7_508_088 * K);
        assert_eq!(m.buffers, 9516 * K);
        assert_eq!(m.cached, 225_196 * K);
        assert_eq!(m.sreclaimable, 15_776 * K);
        assert_eq!(m.cache(), (9516 + 225_196 + 15_776) * K);
        assert_eq!(m.used(), (8_134_304 - 7_410_380 - 9516 - 225_196 - 15_776) * K);
    }

    #[test]
    fn meminfo_requires_total_and_ignores_garbage() {
        assert!(parse_meminfo("").is_none());
        assert!(parse_meminfo("MemFree: 10 kB\n").is_none());
        let m = parse_meminfo("junk\nMemTotal: 100 kB\nCached: x kB\nBuffers: 5 MB\n").unwrap();
        assert_eq!((m.total, m.cached, m.buffers), (100 * K, 0, 0));
    }

    #[test]
    fn used_saturates_when_counters_disagree() {
        // Cached + Buffers > MemTotal − MemFree (např. při přepočtech) nesmí podtéct
        let m = MemInfo { total: 100, free: 90, cached: 30, ..Default::default() };
        assert_eq!(m.used(), 0);
    }

    #[test]
    fn engine_rss_sums_known_processes() {
        let ps = "podman          74176\nsystemd 14880\nconmon 2392\ncontainerd-shim 1000\nsocat 8\nnginx 9248\nbad line\n";
        assert_eq!(engine_rss(ps), (74_176 + 2392 + 1000 + 8) * K);
        assert_eq!(engine_rss(""), 0);
    }

    #[test]
    fn sample_splits_meminfo_and_ps() {
        let (m, e) = parse_sample(&format!("{MEMINFO}@@ps\ndockerd 100\n")).unwrap();
        assert_eq!(m.total, 8_134_304 * K);
        assert_eq!(e, Some(100 * K));
        let (_, e) = parse_sample(MEMINFO).unwrap();
        assert_eq!(e, None);
        assert!(parse_sample("@@ps\n").is_none());
    }

    #[test]
    fn sizes_and_limit() {
        assert_eq!(parse_size("8GB"), Some(8 * K * K * K));
        assert_eq!(parse_size("4096MB"), Some(4 * K * K * K));
        assert_eq!(parse_size(" 2g "), Some(2 * K * K * K));
        assert_eq!(parse_size("1.5GB"), Some(3 * K * K * K / 2));
        assert_eq!(parse_size("512 KB"), Some(512 * K));
        assert_eq!(parse_size("1000"), Some(1000));
        assert_eq!(parse_size("0GB"), None);
        assert_eq!(parse_size("lots"), None);
        assert_eq!(parse_size("8XB"), None);
        let ram = 32 * K * K * K;
        assert_eq!(limit_from("[wsl2]\nmemory=8GB\n", ram), (8 * K * K * K, true));
        assert_eq!(limit_from("[wsl2]\nswap=0\n", ram), (16 * K * K * K, false));
        assert_eq!(limit_from("[wsl2]\nmemory=nonsense\n", ram), (16 * K * K * K, false));
    }

    fn mi(total: u64, free: u64, cached: u64) -> MemInfo {
        MemInfo { total, free, cached, ..Default::default() }
    }

    #[test]
    fn cpu_from_proc_stat() {
        let a = "MemTotal: 1 kB
@@net
eth0: 1 2
@@cpu
cpu  100 0 50 800 50 0 0 0 0 0
";
        let b = "@@cpu
cpu  200 0 100 1100 50 0 0 0 0 0
";
        let (pa, pb) = (parse_cpu(a).unwrap(), parse_cpu(b).unwrap());
        assert_eq!(pa, (150, 1000));
        assert_eq!(pb, (300, 1450));
        // 150 zaneprázdněných tiků ze 450
        assert!((cpu_pct(pa, pb).unwrap() - 33.333).abs() < 0.01);
        // restart VM (čítače klesnou) ani nulový přírůstek nedají číslo
        assert_eq!(cpu_pct(pb, pa), None);
        assert_eq!(cpu_pct(pa, pa), None);
        assert_eq!(parse_cpu("cpu  1 2 3 4"), None);
        // část @@cpu nesmí rozbít čítače rozhraní
        assert_eq!(parse_net("@@net
  eth0: 10 0 0 0 0 0 0 0 20 0 0 0 0 0 0 0
@@cpu
cpu  1 2 3 4
"), Some((10, 20)));
    }

    #[test]
    fn breakdown_arithmetic() {
        // used = 1000 − 400 − 200 = 400
        let b = breakdown(&mi(1000, 400, 200), Some(900), (1000, true), 4000, 150, 50);
        assert_eq!((b.used, b.cache, b.free), (400, 200, 400));
        assert_eq!((b.containers, b.engine, b.rest), (150, 50, 200));
        assert_eq!(b.unreturned, 300);
        assert_eq!(b.scale(), 1000);
    }

    #[test]
    fn breakdown_clamps_negative_parts() {
        // VmmemWSL menší než used + cache -> nevráceno 0
        let b = breakdown(&mi(1000, 400, 200), Some(100), (1000, false), 0, 0, 0);
        assert_eq!(b.unreturned, 0);
        // kontejnery víc než used (obsahují aktivní cache) -> oříznout, zbytek 0
        let b = breakdown(&mi(1000, 400, 200), None, (1000, false), 0, 999, 50);
        assert_eq!((b.containers, b.engine, b.rest, b.unreturned), (400, 0, 0, 0));
        // engine přesahuje zbytek
        let b = breakdown(&mi(1000, 400, 200), None, (1000, false), 0, 300, 500);
        assert_eq!((b.containers, b.engine, b.rest), (300, 100, 0));
        // prázdné meminfo
        let b = breakdown(&MemInfo::default(), None, (0, false), 0, 10, 10);
        assert_eq!((b.used, b.containers, b.engine, b.rest), (0, 0, 0, 0));
        assert_eq!(b.scale(), 1);
        // VmmemWSL nad limitem -> pruh se roztáhne
        let b = breakdown(&mi(1000, 0, 0), Some(1500), (1000, true), 0, 0, 0);
        assert_eq!(b.scale(), 1500);
    }

    #[test]
    fn history_aggregates_within_bucket() {
        let mut h = History::default();
        h.push(MemPoint::new(100.0, Some(100), 10, 20));
        h.push(MemPoint::new(103.0, Some(200), 20, 40));
        h.push(MemPoint::new(106.0, None, 30, 60));
        assert_eq!(h.points.len(), 1);
        let p = h.points[0];
        assert_eq!((p.t, p.n, p.used, p.cache, p.vmmem), (100.0, 3, 20, 40, Some(150)));
        // nový bucket
        h.push(MemPoint::new(110.0, None, 1, 1));
        assert_eq!(h.points.len(), 2);
        h.push(MemPoint::new(111.0, Some(7), 1, 1));
        assert_eq!(h.points[1].vmmem, Some(7));
        // čas jde pozpátku (změna hodin) -> nový bod, ne sloučení
        h.push(MemPoint::new(50.0, None, 1, 1));
        assert_eq!(h.points.len(), 3);
    }

    #[test]
    fn history_trims_last_hour() {
        let mut h = History::default();
        for i in 0..800 {
            h.push(MemPoint::new(i as f64 * 10.0, None, i, 0));
        }
        let last = 7990.0;
        assert!(h.points.iter().all(|p| p.t >= last - HISTORY_SECS));
        assert_eq!(h.points.len(), 361);
        let e = |t| FreeEvent { t, auto: true, vmmem_before: None, vmmem_after: None, cache_before: 0, cache_after: 0 };
        h.add_event(e(100.0));
        h.add_event(e(7000.0));
        assert_eq!(h.events.len(), 1);
        h.trim(last + HISTORY_SECS + 1.0);
        assert!(h.events.is_empty() && h.points.is_empty());
    }

    /// Skutečná čísla (jen čtení): `cargo test live_memory_breakdown -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_memory_breakdown() {
        let cfg = crate::config::Config::load();
        crate::docker::configure_tls(cfg.tls).expect("TLS");
        let d = crate::docker::Docker::new(cfg.port);
        let snap = d.snapshot();
        let rows: Vec<crate::docker::StatRow> =
            snap.containers.iter().filter(|c| c.running()).filter_map(|c| d.stats(&c.id, &c.name())).collect();
        let t0 = Instant::now();
        let out = crate::host::HostExt::run(&*crate::host::current(&cfg), crate::host::Privilege::User, None, &["sh", "-c", SAMPLE_SCRIPT]).expect("wsl");
        let t_wsl = t0.elapsed();
        let (mi, engine) = parse_sample(&out).expect("meminfo");
        let t1 = Instant::now();
        let th = vmmem_toolhelp();
        let t_th = t1.elapsed();
        let t2 = Instant::now();
        let nt = vmmem_ntquery();
        let t_nt = t2.elapsed();
        let t3 = Instant::now();
        let ps = vmmem_powershell();
        let t_ps = t3.elapsed();
        println!("vmmem: toolhelp/K32 {th:?} ({t_th:?}), NtQuerySystemInformation {nt:?} ({t_nt:?}), PowerShell {ps:?} ({t_ps:?})");
        println!("wsl sample: {t_wsl:?}");
        let ram = total_ram();
        let limit = limit_from(&crate::wslconfig::read(), ram);
        let vmmem = VmmemProbe::default().get();
        let sum: u64 = rows.iter().map(|r| r.mem).sum();
        let b = breakdown(&mi, vmmem, limit, ram, sum, engine.unwrap_or(0));
        let g = |v: u64| format!("{:.2} GB", v as f64 / (1u64 << 30) as f64);
        println!("Windows RAM {}, WSL limit {} (.wslconfig: {}), VmmemWSL {}", g(b.ram), g(b.limit), b.limit_set, b.vmmem.map(g).unwrap_or_default());
        println!("Linux MemTotal {}, used {}, cache {}, free {}, not returned {}", g(b.total), g(b.used), g(b.cache), g(b.free), g(b.unreturned));
        println!("containers {} (raw sum {}), engine {} (raw {}), rest {}", g(b.containers), g(sum), g(b.engine), g(engine.unwrap_or(0)), g(b.rest));
        for r in &rows {
            println!("  {:<40} {}", r.name, g(r.mem));
        }
        assert!(b.total > 0 && ram > 0);
        assert_eq!(b.containers + b.engine + b.rest, b.used);
    }
}
