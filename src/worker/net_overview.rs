//! Síťový přehled: provoz VM a kontejnerů, naslouchající sockety v distribuci a ve Windows,
//! porty dovnitř / ven v síťových jmenných prostorech kontejnerů.
//!
//! Sběr běží ve vlastních vláknech jen dokud UI daný pohled kreslí („tep“ – `touch()`);
//! bez tepu se vlákno uspí a při zahození feedu skončí. UI vlákno nikdy nečeká.

use crate::config::Config;
use crate::docker::{Container, Docker};
use crate::host::{Host, HostExt, Privilege};
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
/// Nejkratší interval kontroly portů ostatních distribucí WSL (drahé – wsl.exe na distribuci).
const DISTRO_EVERY: Duration = Duration::from_secs(10);
/// Nejvýš tolik ukončených spojení v historii detailu kontejneru.
pub const CLOSED_CAP: usize = 300;
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
        // 0 = „ještě nikdy“ (viz `active`)
        self.last.store(self.now_ms().max(1), Ordering::Relaxed);
    }
    fn stale(&self) -> bool {
        self.now_ms().saturating_sub(self.last.load(Ordering::Relaxed)) > STALE_MS
    }
    /// Pohled se kreslí teď (na rozdíl od `stale` je nový tep bez doteku neaktivní).
    fn active(&self) -> bool {
        self.last.load(Ordering::Relaxed) > 0 && !self.stale()
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
    /// naslouchající porty ostatních běžících distribucí (jen při otevřené záložce Porty)
    pub distros: DistroScan,
    pub error: Option<String>,
    pub updated: Option<Instant>,
}

/// Výsledek kontroly portů ostatních běžících distribucí WSL.
#[derive(Debug, Clone, Default)]
pub struct DistroScan {
    /// síťový prostor distribuce enginu (`net:[…]`)
    pub engine_netns: String,
    /// zkontrolované běžící distribuce WSL 2 (kromě distribuce enginu)
    pub others: Vec<netinfo::DistroListen>,
    /// nezkontrolované: zastavené (nespouští se) a WSL 1
    pub skipped: Vec<String>,
    pub error: Option<String>,
    pub at: Option<Instant>,
}

/// Běžící sběr síťového přehledu; zastaví se při zahození.
pub struct NetFeed {
    pub data: Arc<Mutex<NetData>>,
    pub started: Instant,
    beat: Beat,
    /// tep záložky Porty – jen pak se kontrolují ostatní distribuce
    ports: Beat,
    traffic: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl NetFeed {
    /// Pohled je vidět; `traffic` = sbírat i statistiky kontejnerů (dražší).
    pub fn touch(&self, traffic: bool) {
        self.beat.touch();
        self.traffic.store(traffic, Ordering::Relaxed);
    }

    /// Záložka Porty je vidět: kontrolovat i porty ostatních distribucí (nejvýš po ~10 s).
    pub fn touch_ports(&self) {
        self.ports.touch();
    }
}

impl Drop for NetFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Skript pro distribuci: čítače rozhraní, IP adresa eth0, naslouchající sockety.
const OVERVIEW_SCRIPT: &str = "cat /proc/net/dev; echo @@WSLCD@@; ip -4 -o addr show eth0 2>/dev/null; echo @@WSLCD@@; ss -ltnupH";

/// Spustí sběr síťového přehledu (~každé 2 s, historie 15 min) ve vlastním vlákně.
pub fn start_net_overview(ctx: eframe::egui::Context, cfg: Arc<RwLock<Config>>) -> NetFeed {
    let data = Arc::new(Mutex::new(NetData::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let traffic = Arc::new(AtomicBool::new(true));
    let beat = Beat::new();
    beat.touch();
    let ports = Beat::new();
    let started = Instant::now();
    let (buf, st, tr, b, pb) = (data.clone(), stop.clone(), traffic.clone(), beat.clone(), ports.clone());
    thread::spawn(move || {
        let mut vm = RateTracker::default();
        let mut ctr = RateTracker::default();
        let mut names: HashMap<u32, String> = HashMap::new();
        let mut paused = false;
        let scanning = Arc::new(AtomicBool::new(false));
        let mut last_scan: Option<Instant> = None;
        while !st.load(Ordering::SeqCst) {
            // ostatní distribuce: vlastní vlákno, ať pomalé wsl.exe nebrzdí přehled
            // jen WSL: jiné distribuce, root
            let can_scan = crate::host::current_caps().distros;
            if can_scan && pb.active() && !scanning.load(Ordering::SeqCst) && last_scan.is_none_or(|t| t.elapsed() >= DISTRO_EVERY) {
                last_scan = Some(Instant::now());
                scanning.store(true, Ordering::SeqCst);
                let c = cfg.read().map(|c| c.clone()).unwrap_or_default();
                let (host, engine) = (crate::host::current(&c), c.distro.clone());
                let (fl, cx, out) = (scanning.clone(), ctx.clone(), buf.clone());
                thread::spawn(move || {
                    let r = scan_distros(&*host, &engine);
                    if let Ok(mut g) = out.lock() {
                        g.distros = r;
                    }
                    fl.store(false, Ordering::SeqCst);
                    cx.request_repaint();
                });
            }
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
            let c = cfg.read().map(|c| c.clone()).unwrap_or_default();
            let host = crate::host::current(&c);
            let ep = host.engine_endpoint(&c);
            let mut next = NetData::default();
            let t = started.elapsed().as_secs_f64();
            overview_round(&*host, &mut vm, t, &mut next);
            next.wsl_mode = crate::wslconfig::get(&crate::wslconfig::read(), "wsl2", "networkingMode").unwrap_or_default();
            // porty Windows jen na Windows (WSL); jinde nic a bez chyby
            if cfg!(windows) && crate::host::current_caps().distros {
                match windows_listeners(&mut names) {
                    Ok(w) => next.win_listen = w,
                    Err(e) => next.win_error = Some(e),
                }
            }
            if tr.load(Ordering::Relaxed) {
                next.containers = container_traffic(&Docker::new(&ep), &mut ctr, t);
            } else if let Ok(g) = buf.lock() {
                next.containers = g.containers.clone();
            }
            if let Ok(mut g) = buf.lock() {
                // výsledek kontroly distribucí zapisuje vlastní vlákno přímo sem
                next.distros = std::mem::take(&mut g.distros);
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
    NetFeed { data, started, beat, ports, traffic, stop }
}

/// Oprávnění pro skript přehledu: jako root jen s `root_sampling` (periodický sběr nesmí
/// na Linuxu žádat o heslo). Bez roota `ss` jen nevyplní procesy (UI pak sloupec skryje).
pub(crate) fn overview_privilege(caps: crate::host::Caps) -> Privilege {
    if caps.root_sampling { Privilege::Root } else { Privilege::User }
}

/// Jedno kolo přehledu u hostitele: čítače eth0, IP adresa, naslouchající sockety.
fn overview_round(host: &dyn Host, vm: &mut RateTracker, t: f64, next: &mut NetData) {
    match host.run(overview_privilege(host.caps()), None, &["sh", "-c", OVERVIEW_SCRIPT]) {
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
}

/// Zkontroluje naslouchající TCP porty ve všech běžících distribucích WSL 2 kromě `engine`
/// (paralelně). Zastavené distribuce se přeskočí – `wsl -d` by je spustil.
/// Spouští se jako root: distribuce sdílejí síťový prostor VM a vlastníka socketu
/// (proces) `ss -p` ukáže jen s právy roota; nic se nemění.
pub fn scan_distros(host: &dyn Host, engine: &str) -> DistroScan {
    let mut out = DistroScan { at: Some(Instant::now()), ..Default::default() };
    // jen WSL a jen se sběrem jako root (readlink netns, DISTRO_SCRIPT)
    if !host.caps().root_sampling || host.wsl().is_none() {
        out.error = Some(crate::host::unavailable());
        return out;
    }
    let list = match wsl::list() {
        Ok(l) => l,
        Err(e) => {
            out.error = Some(e);
            return out;
        }
    };
    let mut handles = Vec::new();
    let mut engine_ns = None;
    for d in list {
        if d.name.eq_ignore_ascii_case(engine) {
            if d.running() {
                if let Some(h) = host.wsl().map(|w| w.for_distro(&d.name)) {
                    engine_ns = Some(thread::spawn(move || h.run(Privilege::Root, None, &["readlink", "/proc/self/ns/net"]).map(|s| s.trim().to_string()).unwrap_or_default()));
                }
            }
            continue;
        }
        if !d.running() || d.version.trim() != "2" {
            out.skipped.push(d.name);
            continue;
        }
        // bez WSL hostitele není kde skript spustit
        let Some(h) = host.wsl().map(|w| w.for_distro(&d.name)) else { continue };
        handles.push(thread::spawn(move || {
            let run = |p| h.run(p, None, &["sh", "-c", netinfo::DISTRO_SCRIPT]);
            // distribuce bez uživatele root (neobvyklé): aspoň jako výchozí uživatel
            let r = run(Privilege::Root).or_else(|_| run(Privilege::User));
            match r {
                Ok(text) => {
                    let (netns, listen) = netinfo::parse_distro_listen(&text);
                    netinfo::DistroListen { distro: d.name, netns, listen, error: None }
                }
                Err(e) => netinfo::DistroListen { distro: d.name, error: Some(String::from(e)), ..Default::default() },
            }
        }));
    }
    out.others = handles.into_iter().filter_map(|h| h.join().ok()).collect();
    out.others.sort_by(|a, b| a.distro.cmp(&b.distro));
    out.engine_netns = engine_ns.and_then(|h| h.join().ok()).unwrap_or_default();
    out
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
    /// jen detail jednoho kontejneru: sledovaná navázaná spojení s rychlostmi
    pub live: Vec<netinfo::Conn>,
    /// ukončená spojení za posledních 15 min (nejstarší první)
    pub closed: Vec<netinfo::Conn>,
    /// provoz kontejneru: (t, přijato B/s, odesláno B/s)
    pub traffic: Vec<(f64, f64, f64)>,
    /// čas posledního vzorku (s od spuštění sběru; stejná osa jako `live`, `closed`, `traffic`)
    pub now: f64,
}

/// Co sbírat v síťových prostorech kontejnerů.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NsMode {
    /// naslouchající sockety a navázaná spojení (s čítači TCP)
    Conns,
    /// navíc čítače rozhraní (detail kontejneru)
    Detail,
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
pub fn collect_ns(host: &dyn Host, pids: &[(String, u32)], mode: NsMode) -> Result<HashMap<String, NsDump>, String> {
    // nsenter do jmenných prostorů kontejnerů jen jako root – bez schopnosti nic nespouštět
    if !host.caps().root_sampling {
        return Err(crate::host::unavailable());
    }
    let valid: Vec<&(String, u32)> = pids.iter().filter(|(_, p)| *p > 0).collect();
    if valid.is_empty() {
        return Ok(HashMap::new());
    }
    let mut args: Vec<String> = vec!["sh".into(), "-c".into(), netinfo::NS_SCRIPT.into(), "sh".into()];
    args.push(match mode {
        NsMode::Conns => "all".into(),
        NsMode::Detail => "detail".into(),
    });
    args.extend(valid.iter().map(|(_, p)| p.to_string()));
    let out = host.run_owned(Privilege::Root, None, &args).map_err(String::from)?;
    let mut dump = netinfo::parse_ns_dump(&out);
    Ok(valid.iter().filter_map(|(id, p)| dump.remove(p).map(|d| (id.clone(), d))).collect())
}

/// Spustí sběr portů: `id` = jeden kontejner (~2 s, navázaná spojení s rychlostmi,
/// historie ukončených spojení a provoz kontejneru za 15 min – dokud feed žije),
/// None = všechny běžící kontejnery (~5 s, pro přehled v seznamu kontejnerů).
pub fn start_ns_feed(ctx: eframe::egui::Context, cfg: Arc<RwLock<Config>>, id: Option<String>) -> NsFeed {
    let data = Arc::new(Mutex::new(NsData::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let beat = Beat::new();
    beat.touch();
    let (buf, st, b) = (data.clone(), stop.clone(), beat.clone());
    let every = if id.is_some() { ONE_EVERY } else { ALL_EVERY };
    let mode = if id.is_some() { NsMode::Detail } else { NsMode::Conns };
    thread::spawn(move || {
        let started = Instant::now();
        let mut tracker = netinfo::ConnTracker::new(NET_WINDOW, CLOSED_CAP);
        while !st.load(Ordering::SeqCst) {
            let t0 = Instant::now();
            if b.stale() {
                wait(t0, Duration::from_millis(500), &st);
                continue;
            }
            let c = cfg.read().map(|c| c.clone()).unwrap_or_default();
            let host = crate::host::current(&c);
            let d = Docker::new(&host.engine_endpoint(&c));
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
                    match collect_ns(&*host, &pids, mode) {
                        Ok(m) => next.by_id = m,
                        Err(e) => next.error = Some(e),
                    }
                }
                Err(e) => next.error = Some(e),
            }
            if let Some(id) = &id {
                let t = started.elapsed().as_secs_f64();
                match next.by_id.get(id) {
                    // chyba sběru nic neukončuje – počká se na další vzorek
                    Some(dump) if dump.error.is_none() && next.error.is_none() => {
                        let conns: Vec<(bool, Sock)> = dump.conns.iter().map(|s| (netinfo::is_inbound(s, &dump.listen), s.clone())).collect();
                        tracker.update(t, &conns);
                        if let Some((rx, tx)) = dump.dev {
                            tracker.push_dev(t, rx, tx);
                        }
                    }
                    // kontejner neběží: všechna spojení skončila
                    None if next.error.is_none() && next.pid.get(id) == Some(&0) => tracker.update(t, &[]),
                    _ => {}
                }
                next.live = tracker.live();
                next.closed = tracker.closed.iter().cloned().collect();
                next.traffic = tracker.traffic.iter().copied().collect();
                next.now = t;
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
        // jiný systém než Windows: netstat.exe neexistuje
        Err(e) if !cfg!(windows) => return Err(e),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::fake::RecordingHost;
    use crate::host::Caps;

    /// Bez `root_sampling` periodický sběr nikdy nevolá hostitele jako root (Linux by žádal
    /// o heslo); s ním (WSL) běží přehled i porty kontejnerů jako root jako dosud.
    #[test]
    fn root_sampling_skipped_without_cap() {
        let pids = [("c1".to_string(), 42u32)];
        let local = RecordingHost::new(Caps::LOCAL);
        let mut next = NetData::default();
        overview_round(&local, &mut RateTracker::default(), 0.0, &mut next);
        let scan = scan_distros(&local, "x");
        let ns = collect_ns(&local, &pids, NsMode::Detail);
        assert!(local.root_calls().is_empty(), "{:?}", local.root_calls());
        // přehled běží bez roota, distribuce ani jmenné prostory se nesbírají
        assert_eq!(local.calls.lock().unwrap().len(), 1);
        assert_eq!(scan.error, Some(crate::host::unavailable()));
        assert_eq!(ns, Err(crate::host::unavailable()));
        assert_eq!(overview_privilege(Caps::LOCAL), Privilege::User);

        let wsl = RecordingHost::new(Caps::WSL);
        overview_round(&wsl, &mut RateTracker::default(), 0.0, &mut next);
        let _ = collect_ns(&wsl, &pids, NsMode::Detail);
        assert_eq!(wsl.root_calls().len(), 2);
        assert_eq!(overview_privilege(Caps::WSL), Privilege::Root);
    }
}

/// Otevře `http://127.0.0.1:<port>` ve výchozím prohlížeči.
pub fn open_browser(port: u16) -> Result<(), String> {
    std::process::Command::new("explorer.exe").arg(format!("http://127.0.0.1:{port}/")).spawn().map(|_| ()).map_err(|e| e.to_string())
}
