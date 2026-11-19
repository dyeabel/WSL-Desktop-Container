//! Síťový přehled: provoz VM a kontejnerů, naslouchající sockety v distribuci a ve Windows,
//! porty dovnitř / ven v síťových jmenných prostorech kontejnerů.
//!
//! Sběr běží ve vlastních vláknech jen dokud UI daný pohled kreslí („tep“ – `touch()`);
//! bez tepu se vlákno uspí a při zahození feedu skončí. UI vlákno nikdy nečeká.

use crate::config::Config;
use crate::docker::{Container, Docker};
use crate::netinfo::{self, NsDump, RateTracker, Sock, WinListen};
use crate::wsl;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

/// Historie provozu VM (s).
pub const NET_WINDOW: f64 = 900.0;
/// Interval sběru přehledu.
const OVERVIEW_EVERY: Duration = Duration::from_secs(2);
/// Interval sběru portů jednoho kontejneru (detail).
const ONE_EVERY: Duration = Duration::from_secs(2);
/// Interval sběru portů všech běžících kontejnerů (seznam kontejnerů).
const ALL_EVERY: Duration = Duration::from_secs(5);
/// Bez tepu z UI déle než tohle se sběr pozastaví.
const STALE_MS: u64 = 4000;

/// Tep z UI: kdy naposledy pohled data zobrazil.
#[derive(Clone)]
struct Beat {
    start: Instant,
    last: Arc<AtomicU64>,
}

impl Beat {
    fn new() -> Self {
        Self { start: Instant::now(), last: Arc::new(AtomicU64::new(0)) }
    }
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
    fn touch(&self) {
        self.last.store(self.now_ms(), Ordering::Relaxed);
    }
    fn stale(&self) -> bool {
        self.now_ms().saturating_sub(self.last.load(Ordering::Relaxed)) > STALE_MS
    }
}

/// Počká do dalšího kola (po 100 ms kontroluje zastavení).
fn wait(t0: Instant, every: Duration, stop: &AtomicBool) {
    while t0.elapsed() < every && !stop.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------- přehled

/// Vzorek provozu VM (eth0) v B/s.
#[derive(Debug, Clone, Copy, Default)]
pub struct NetSample {
    /// sekundy od spuštění sběru
    pub t: f64,
    pub rx: f64,
    pub tx: f64,
}

/// Provoz jednoho kontejneru.
#[derive(Debug, Clone, Default)]
pub struct CtrTraffic {
    pub name: String,
    /// compose projekt (prázdné = samostatný kontejner)
    pub stack: String,
    /// B/s (None = zatím jen jeden vzorek)
    pub rate: Option<(f64, f64)>,
    pub rx_total: u64,
    pub tx_total: u64,
}

#[derive(Debug, Clone, Default)]
pub struct NetData {
    pub history: VecDeque<NetSample>,
    /// kumulovaně přijato / odesláno přes eth0 od startu VM
    pub vm_total: Option<(u64, u64)>,
    pub vm_rate: Option<(f64, f64)>,
    pub containers: Vec<CtrTraffic>,
    /// IPv4 adresa distribuce (eth0)
    pub wsl_ip: Option<String>,
    /// `networkingMode` z .wslconfig (prázdné = výchozí NAT)
    pub wsl_mode: String,
    /// naslouchající sockety v distribuci (kořenový netns)
    pub distro_listen: Vec<Sock>,
    /// naslouchající porty ve Windows
    pub win_listen: Vec<WinListen>,
    pub win_error: Option<String>,
    pub error: Option<String>,
    pub updated: Option<Instant>,
}

/// Běžící sběr síťového přehledu; zastaví se při zahození.
pub struct NetFeed {
    pub data: Arc<Mutex<NetData>>,
    pub started: Instant,
    beat: Beat,
    traffic: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl NetFeed {
    /// Pohled je vidět; `traffic` = sbírat i statistiky kontejnerů (dražší).
    pub fn touch(&self, traffic: bool) {
        self.beat.touch();
        self.traffic.store(traffic, Ordering::Relaxed);
    }
}

impl Drop for NetFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Skript pro distribuci: čítače rozhraní, IP adresa eth0, naslouchající sockety.
const OVERVIEW_SCRIPT: &str = "cat /proc/net/dev; echo @@WLS@@; ip -4 -o addr show eth0 2>/dev/null; echo @@WLS@@; ss -ltnupH";

/// Spustí sběr síťového přehledu (~každé 2 s, historie 15 min) ve vlastním vlákně.
pub fn start_net_overview(ctx: eframe::egui::Context, cfg: Arc<RwLock<Config>>) -> NetFeed {
    let data = Arc::new(Mutex::new(NetData::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let traffic = Arc::new(AtomicBool::new(true));
    let beat = Beat::new();
    beat.touch();
    let started = Instant::now();
    let (buf, st, tr, b) = (data.clone(), stop.clone(), traffic.clone(), beat.clone());
    thread::spawn(move || {
        let mut vm = RateTracker::default();
        let mut ctr = RateTracker::default();
        let mut names: HashMap<u32, String> = HashMap::new();
        let mut paused = false;
        while !st.load(Ordering::SeqCst) {
            let t0 = Instant::now();
            if b.stale() {
                // pohled není vidět – rychlosti po návratu počítat od nového vzorku
                if !paused {
                    vm.clear();
                    ctr.clear();
                    paused = true;
                }
                wait(t0, Duration::from_millis(500), &st);
                continue;
            }
            paused = false;
            let (distro, port) = cfg.read().map(|c| (c.distro.clone(), c.port)).unwrap_or_default();
            let mut next = NetData::default();
            let t = started.elapsed().as_secs_f64();
            match wsl::run(&distro, Some("root"), None, &["sh", "-c", OVERVIEW_SCRIPT]) {
                Ok(out) => {
                    let mut parts = out.split(netinfo::MARK);
                    let dev = parts.next().unwrap_or("");
                    let ip = parts.next().unwrap_or("");
                    let ss = parts.next().unwrap_or("");
                    if let Some(eth) = netinfo::parse_proc_net_dev(dev).into_iter().find(|i| i.name == "eth0") {
                        next.vm_total = Some((eth.rx, eth.tx));
                        next.vm_rate = vm.update("eth0", eth.rx, eth.tx, t);
                    }
                    next.wsl_ip = netinfo::parse_ip_addr(ip);
                    next.distro_listen = netinfo::parse_ss(ss);
                }
                Err(e) => next.error = Some(String::from(e)),
            }
            next.wsl_mode = crate::wslconfig::get(&crate::wslconfig::read(), "wsl2", "networkingMode").unwrap_or_default();
            match windows_listeners(&mut names) {
                Ok(w) => next.win_listen = w,
                Err(e) => next.win_error = Some(e),
            }
            if tr.load(Ordering::Relaxed) {
                next.containers = container_traffic(&Docker::new(port), &mut ctr, t);
            } else if let Ok(g) = buf.lock() {
                next.containers = g.containers.clone();
            }
            if let Ok(mut g) = buf.lock() {
                next.history = std::mem::take(&mut g.history);
                if let Some((rx, tx)) = next.vm_rate {
                    next.history.push_back(NetSample { t, rx, tx });
                }
                while next.history.front().is_some_and(|s| s.t < t - NET_WINDOW) {
                    next.history.pop_front();
                }
                next.updated = Some(Instant::now());
                *g = next;
            }
            ctx.request_repaint();
            wait(t0, OVERVIEW_EVERY, &st);
        }
    });
    NetFeed { data, started, beat, traffic, stop }
}

/// Statistiky sítě běžících kontejnerů (paralelně) -> rychlosti z rozdílu vzorků.
fn container_traffic(d: &Docker, rates: &mut RateTracker, t: f64) -> Vec<CtrTraffic> {
    let list: Vec<Container> = d.get("/containers/json").unwrap_or_default();
    let handles: Vec<_> = list
        .into_iter()
        .filter(|c| c.running())
        .map(|c| {
            let d = d.clone();
            thread::spawn(move || {
                let row = d.stats(&c.id, &c.name())?;
                Some((c.project().unwrap_or("").to_string(), row))
            })
        })
        .collect();
    let rows: Vec<(String, crate::docker::StatRow)> = handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect();
    let keys: Vec<&str> = rows.iter().map(|(_, r)| r.id.as_str()).collect();
    rates.retain(&keys);
    let mut out: Vec<CtrTraffic> = rows
        .iter()
        .map(|(stack, r)| CtrTraffic {
            name: r.name.clone(),
            stack: stack.clone(),
            rate: rates.update(&r.id, r.net_rx, r.net_tx, t),
            rx_total: r.net_rx,
            tx_total: r.net_tx,
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

// ---------------------------------------------------------------- porty kontejnerů

/// Sockety v síťových jmenných prostorech kontejnerů (podle ID kontejneru).
#[derive(Debug, Clone, Default)]
pub struct NsData {
    pub by_id: HashMap<String, NsDump>,
    /// PID hlavního procesu (State.Pid) podle ID
    pub pid: HashMap<String, u32>,
    pub error: Option<String>,
    pub updated: Option<Instant>,
}

/// Běžící sběr portů dovnitř / ven; zastaví se při zahození.
pub struct NsFeed {
    pub data: Arc<Mutex<NsData>>,
    beat: Beat,
    stop: Arc<AtomicBool>,
}

impl NsFeed {
    pub fn touch(&self) {
        self.beat.touch();
    }
}

impl Drop for NsFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// PID hlavního procesu kontejneru z inspect (0 = neběží).
fn container_pid(d: &Docker, id: &str) -> Result<u32, String> {
    let v: Value = d.get(&format!("/containers/{}/json", crate::docker::enc(id))).map_err(String::from)?;
    Ok(v.pointer("/State/Pid").and_then(|p| p.as_u64()).and_then(|p| u32::try_from(p).ok()).unwrap_or(0))
}

/// Jedno kolo sběru: sockety pro dané kontejnery (ID -> PID) přes `nsenter` v distribuci.
/// PID se předávají jako argumenty skriptu, nikdy jako jeho text.
pub fn collect_ns(distro: &str, pids: &[(String, u32)], with_conns: bool) -> Result<HashMap<String, NsDump>, String> {
    let valid: Vec<&(String, u32)> = pids.iter().filter(|(_, p)| *p > 0).collect();
    if valid.is_empty() {
        return Ok(HashMap::new());
    }
    let mut args: Vec<String> = vec!["sh".into(), "-c".into(), netinfo::NS_SCRIPT.into(), "sh".into()];
    args.push(if with_conns { "all".into() } else { "listen".into() });
    args.extend(valid.iter().map(|(_, p)| p.to_string()));
    let out = wsl::run_owned(distro, Some("root"), None, &args).map_err(String::from)?;
    let mut dump = netinfo::parse_ns_dump(&out);
    Ok(valid.iter().filter_map(|(id, p)| dump.remove(p).map(|d| (id.clone(), d))).collect())
}

/// Spustí sběr portů: `id` = jeden kontejner (~2 s, s navázanými spojeními),
/// None = všechny běžící kontejnery (~5 s, pro přehled v seznamu kontejnerů).
pub fn start_ns_feed(ctx: eframe::egui::Context, cfg: Arc<RwLock<Config>>, id: Option<String>) -> NsFeed {
    let data = Arc::new(Mutex::new(NsData::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let beat = Beat::new();
    beat.touch();
    let (buf, st, b) = (data.clone(), stop.clone(), beat.clone());
    let every = if id.is_some() { ONE_EVERY } else { ALL_EVERY };
    thread::spawn(move || {
        while !st.load(Ordering::SeqCst) {
            let t0 = Instant::now();
            if b.stale() {
                wait(t0, Duration::from_millis(500), &st);
                continue;
            }
            let (distro, port) = cfg.read().map(|c| (c.distro.clone(), c.port)).unwrap_or_default();
            let d = Docker::new(port);
            let ids: Result<Vec<String>, String> = match &id {
                Some(i) => Ok(vec![i.clone()]),
                None => d
                    .get::<Vec<Container>>("/containers/json")
                    .map(|l| l.into_iter().filter(|c| c.running()).map(|c| c.id).collect())
                    .map_err(String::from),
            };
            let mut next = NsData::default();
            match ids {
                Ok(ids) => {
                    let mut pids = Vec::new();
                    for i in ids {
                        match container_pid(&d, &i) {
                            Ok(p) => pids.push((i, p)),
                            Err(e) => next.error = Some(e),
                        }
                    }
                    next.pid = pids.iter().cloned().collect();
                    match collect_ns(&distro, &pids, true) {
                        Ok(m) => next.by_id = m,
                        Err(e) => next.error = Some(e),
                    }
                }
                Err(e) => next.error = Some(e),
            }
            next.updated = Some(Instant::now());
            if let Ok(mut g) = buf.lock() {
                *g = next;
            }
            ctx.request_repaint();
            wait(t0, every, &st);
        }
    });
    NsFeed { data, beat, stop }
}

// ---------------------------------------------------------------- Windows

/// Naslouchající TCP a UDP porty ve Windows (IPv4 i IPv6) s názvy procesů.
/// `names` = mezipaměť názvů procesů podle PID.
pub fn windows_listeners(names: &mut HashMap<u32, String>) -> Result<Vec<WinListen>, String> {
    let mut v = match win::listeners() {
        Ok(v) => v,
        Err(e) => {
            // záložní cesta: netstat -ano
            let out = wsl::hidden("netstat.exe").args(["-ano"]).output().map_err(|x| format!("{e}; netstat: {x}"))?;
            netinfo::parse_netstat(&String::from_utf8_lossy(&out.stdout))
        }
    };
    let live: Vec<u32> = v.iter().map(|w| w.pid).collect();
    names.retain(|p, _| live.contains(p));
    for w in &mut v {
        w.process = names.entry(w.pid).or_insert_with(|| win::process_name(w.pid)).clone();
    }
    v.sort_by(|a, b| a.port.cmp(&b.port).then_with(|| a.proto.cmp(&b.proto)).then_with(|| a.ip.cmp(&b.ip)));
    v.dedup();
    Ok(v)
}

#[cfg(windows)]
mod win {
    use crate::netinfo::WinListen;
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP6TABLE_OWNER_PID, MIB_TCPTABLE_OWNER_PID, MIB_UDP6TABLE_OWNER_PID,
        MIB_UDPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER, UDP_TABLE_OWNER_PID,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION};

    const AF_INET: u32 = 2;
    const AF_INET6: u32 = 23;

    /// Zavolá funkci tabulky s dostatečně velkým bufferem (zarovnaným na u64).
    fn table(f: impl Fn(*mut core::ffi::c_void, *mut u32) -> u32) -> Result<Vec<u64>, String> {
        let mut size: u32 = 0;
        for _ in 0..5 {
            let mut buf = vec![0u64; (size as usize).div_ceil(8).max(1)];
            let mut sz = (buf.len() * 8) as u32;
            let r = f(buf.as_mut_ptr().cast(), &mut sz);
            if r == NO_ERROR {
                return Ok(buf);
            }
            if r != ERROR_INSUFFICIENT_BUFFER {
                return Err(format!("IP Helper: chyba {r}"));
            }
            size = sz + 1024;
        }
        Err("IP Helper: tabulka se stále mění".into())
    }

    fn port(p: u32) -> u16 {
        u16::from_be((p & 0xffff) as u16)
    }

    fn v6(a: [u8; 16]) -> String {
        std::net::Ipv6Addr::from(a).to_string()
    }

    pub fn listeners() -> Result<Vec<WinListen>, String> {
        let mut out = Vec::new();
        // SAFETY: buffer má velikost vrácenou API a je zarovnaný; čte se jen dwNumEntries řádků
        unsafe {
            let b = table(|p, s| GetExtendedTcpTable(p, s, 0, AF_INET, TCP_TABLE_OWNER_PID_LISTENER, 0))?;
            let t = &*(b.as_ptr() as *const MIB_TCPTABLE_OWNER_PID);
            for r in std::slice::from_raw_parts(t.table.as_ptr(), t.dwNumEntries as usize) {
                let ip = std::net::Ipv4Addr::from(u32::from_be(r.dwLocalAddr)).to_string();
                out.push(WinListen { proto: "tcp".into(), ip, port: port(r.dwLocalPort), pid: r.dwOwningPid, process: String::new() });
            }
            let b = table(|p, s| GetExtendedTcpTable(p, s, 0, AF_INET6, TCP_TABLE_OWNER_PID_LISTENER, 0))?;
            let t = &*(b.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID);
            for r in std::slice::from_raw_parts(t.table.as_ptr(), t.dwNumEntries as usize) {
                out.push(WinListen { proto: "tcp".into(), ip: v6(r.ucLocalAddr), port: port(r.dwLocalPort), pid: r.dwOwningPid, process: String::new() });
            }
            let b = table(|p, s| GetExtendedUdpTable(p, s, 0, AF_INET, UDP_TABLE_OWNER_PID, 0))?;
            let t = &*(b.as_ptr() as *const MIB_UDPTABLE_OWNER_PID);
            for r in std::slice::from_raw_parts(t.table.as_ptr(), t.dwNumEntries as usize) {
                let ip = std::net::Ipv4Addr::from(u32::from_be(r.dwLocalAddr)).to_string();
                out.push(WinListen { proto: "udp".into(), ip, port: port(r.dwLocalPort), pid: r.dwOwningPid, process: String::new() });
            }
            let b = table(|p, s| GetExtendedUdpTable(p, s, 0, AF_INET6, UDP_TABLE_OWNER_PID, 0))?;
            let t = &*(b.as_ptr() as *const MIB_UDP6TABLE_OWNER_PID);
            for r in std::slice::from_raw_parts(t.table.as_ptr(), t.dwNumEntries as usize) {
                out.push(WinListen { proto: "udp".into(), ip: v6(r.ucLocalAddr), port: port(r.dwLocalPort), pid: r.dwOwningPid, process: String::new() });
            }
        }
        Ok(out)
    }

    /// Název spustitelného souboru procesu (`wslrelay.exe`); prázdné, když nejde zjistit.
    pub fn process_name(pid: u32) -> String {
        match pid {
            0 => return "System Idle".into(),
            4 => return "System".into(),
            _ => {}
        }
        // SAFETY: handle se po použití zavře; buffer má délku `len`
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return String::new();
            }
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
            CloseHandle(h);
            if ok == 0 {
                return String::new();
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            path.rsplit('\\').next().unwrap_or(&path).to_string()
        }
    }
}

#[cfg(not(windows))]
mod win {
    pub fn listeners() -> Result<Vec<crate::netinfo::WinListen>, String> {
        Err("jen Windows".into())
    }
    pub fn process_name(_: u32) -> String {
        String::new()
    }
}

#[cfg(test)]
#[path = "net_overview_live.rs"]
mod live;

/// Otevře `http://127.0.0.1:<port>` ve výchozím prohlížeči.
pub fn open_browser(port: u16) -> Result<(), String> {
    std::process::Command::new("explorer.exe").arg(format!("http://127.0.0.1:{port}/")).spawn().map(|_| ()).map_err(|e| e.to_string())
}
