//! Síťové informace: parsery výstupů (`/proc/net/dev`, `ss`, `ip addr`, `netstat -ano`),
//! výpočet rychlostí, porovnání naslouchajících a publikovaných portů a hledání konfliktů.
//! Bez I/O – sběr dat dělá `worker::net_overview`.

use std::collections::{HashMap, VecDeque};

/// Oddělovač sekcí ve výstupu sběrného skriptu.
pub const MARK: &str = "@@WSLCD@@";

// ---------------------------------------------------------------- /proc/net/dev

/// Kumulované čítače rozhraní (bajty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfCounters {
    pub name: String,
    pub rx: u64,
    pub tx: u64,
}

/// `/proc/net/dev`: `  eth0: rx_bytes rx_packets … (8 polí) tx_bytes …`.
pub fn parse_proc_net_dev(text: &str) -> Vec<IfCounters> {
    text.lines()
        .filter_map(|l| {
            let (name, rest) = l.split_once(':')?;
            let name = name.trim();
            if name.is_empty() || name.contains(' ') || name.contains('|') {
                return None;
            }
            let f: Vec<u64> = rest.split_whitespace().map(|x| x.parse().ok()).collect::<Option<Vec<_>>>()?;
            (f.len() >= 9).then(|| IfCounters { name: name.to_string(), rx: f[0], tx: f[8] })
        })
        .collect()
}

/// Rychlost (B/s) z rozdílu dvou hodnot kumulovaného čítače; None = čítač se vynuloval
/// (restart rozhraní / kontejneru) nebo neuplynul žádný čas.
pub fn rate(prev: u64, cur: u64, secs: f64) -> Option<f64> {
    (cur >= prev && secs > 0.0).then(|| (cur - prev) as f64 / secs)
}

/// Rychlosti rx / tx podle klíče z po sobě jdoucích vzorků kumulovaných čítačů.
#[derive(Debug, Default)]
pub struct RateTracker {
    prev: HashMap<String, (u64, u64, f64)>,
}

impl RateTracker {
    /// Nový vzorek v čase `t` (s); vrací (rx/s, tx/s), pokud existuje použitelný předchozí.
    pub fn update(&mut self, key: &str, rx: u64, tx: u64, t: f64) -> Option<(f64, f64)> {
        let r = self.prev.get(key).and_then(|&(prx, ptx, pt)| Some((rate(prx, rx, t - pt)?, rate(ptx, tx, t - pt)?)));
        self.prev.insert(key.to_string(), (rx, tx, t));
        r
    }

    /// Zapomene klíče, které v posledním kole nepřišly.
    pub fn retain(&mut self, keys: &[&str]) {
        self.prev.retain(|k, _| keys.contains(&k.as_str()));
    }

    pub fn clear(&mut self) {
        self.prev.clear();
    }
}

// ---------------------------------------------------------------- ip addr

/// První IPv4 adresa z `ip -4 -o addr show eth0` (bez masky).
pub fn parse_ip_addr(text: &str) -> Option<String> {
    let mut it = text.split_whitespace();
    while let Some(w) = it.next() {
        if w == "inet" {
            return it.next().map(|a| a.split('/').next().unwrap_or(a).to_string());
        }
    }
    None
}

// ---------------------------------------------------------------- ss

/// Jeden socket z výstupu `ss`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sock {
    /// tcp / udp
    pub proto: String,
    /// LISTEN, UNCONN, ESTAB… (prázdné, když `ss` stav nevypisuje – filtr `state …`)
    pub state: String,
    /// místní adresa bez závorek a bez `%rozhraní`; „*“ = libovolná
    pub local_ip: String,
    pub local_port: u16,
    pub peer_ip: String,
    /// None = „*“
    pub peer_port: Option<u16>,
    /// názvy procesů (bez duplicit)
    pub procs: Vec<String>,
    /// PID prvního procesu
    pub pid: Option<u32>,
    /// TCP: přijato bajtů (`bytes_received` z `ss -i`); None = ss čítače nevypsal
    pub bytes_in: Option<u64>,
    /// TCP: odesláno a potvrzeno bajtů (`bytes_acked`, jinak `bytes_sent`)
    pub bytes_out: Option<u64>,
    /// inode socketu (`ino:` z `ss -e`) – odliší nové spojení na stejné čtveřici adres
    pub inode: Option<u64>,
}

impl Sock {
    pub fn local(&self) -> String {
        join_addr(&self.local_ip, Some(self.local_port))
    }
    pub fn peer(&self) -> String {
        join_addr(&self.peer_ip, self.peer_port)
    }
    pub fn process(&self) -> String {
        match (self.procs.is_empty(), self.pid) {
            (true, _) => String::new(),
            (false, Some(p)) => format!("{} ({p})", self.procs.join(", ")),
            (false, None) => self.procs.join(", "),
        }
    }
    /// Naslouchá jen na loopbacku (zvenku kontejneru / VM nedostupné).
    pub fn loopback_only(&self) -> bool {
        is_loopback(&self.local_ip)
    }
}

pub fn is_loopback(ip: &str) -> bool {
    ip.starts_with("127.") || ip == "::1" || ip == "localhost"
}

/// Adresa „libovolná“ (0.0.0.0, ::, *, prázdná).
pub fn is_wildcard(ip: &str) -> bool {
    matches!(ip, "" | "*" | "0.0.0.0" | "::" | "[::]")
}

/// `ip:port`, IPv6 v hranatých závorkách.
pub fn join_addr(ip: &str, port: Option<u16>) -> String {
    let p = port.map(|p| p.to_string()).unwrap_or_else(|| "*".into());
    if ip.contains(':') { format!("[{ip}]:{p}") } else { format!("{ip}:{p}") }
}

/// `1.2.3.4:80`, `[::]:80`, `*:22`, `127.0.0.53%lo:53`, `[fe80::1]%eth0:5`, `:::80` -> (ip, port).
pub fn split_addr(s: &str) -> Option<(String, Option<u16>)> {
    let (ip, port) = s.rsplit_once(':')?;
    let port = if port == "*" { None } else { Some(port.parse::<u16>().ok()?) };
    let ip = ip.split('%').next().unwrap_or(ip);
    let ip = ip.trim_start_matches('[').trim_end_matches(']');
    let ip = ip.strip_prefix("::ffff:").filter(|v| v.contains('.')).unwrap_or(ip);
    Some((ip.to_string(), port))
}

/// `users:(("nginx",pid=12,fd=6),("nginx",pid=13,fd=6))` -> (["nginx"], Some(12)).
fn parse_users(s: &str) -> (Vec<String>, Option<u32>) {
    let mut names: Vec<String> = Vec::new();
    let mut pid = None;
    // každý proces je `("název",pid=N,fd=M)`
    for part in s.split("(\"").skip(1) {
        let Some((name, rest)) = part.split_once('"') else { continue };
        if !name.is_empty() && !names.iter().any(|x| x == name) {
            names.push(name.to_string());
        }
        if pid.is_none() {
            pid = rest.split([',', ')']).find_map(|f| f.strip_prefix("pid=")).and_then(|p| p.parse().ok());
        }
    }
    (names, pid)
}

/// Výstup `ss -H` (`-ltnup`, `-tunp state established`, s Netid i bez něj, se stavem i bez něj).
/// S `-i` / `-e` přečte i čítače bajtů a inode – z odsazeného pokračovacího řádku
/// i z jednořádkového výstupu (`-O`). Neznámé řádky (chyby, hlavičky) se přeskočí.
pub fn parse_ss(text: &str) -> Vec<Sock> {
    let mut v: Vec<Sock> = Vec::new();
    for l in text.lines() {
        push_ss_line(&mut v, l);
    }
    v
}

/// Přidá řádek `ss`: nový socket, nebo odsazené podrobnosti (`-i`) k poslednímu.
fn push_ss_line(v: &mut Vec<Sock>, l: &str) {
    if let Some(s) = parse_ss_line(l) {
        v.push(s);
    } else if l.starts_with([' ', '\t']) {
        if let Some(last) = v.last_mut() {
            apply_info(last, l.split_whitespace());
        }
    }
}

/// Čítače TCP (`bytes_received`, `bytes_acked`, `bytes_sent`) a inode (`ino:`) z tokenů `ss -ie`.
/// ss vypisuje jen nenulové čítače: když je vidět blok TCP info (`rto:`, `mss:`…),
/// chybějící čítač znamená 0. Bez bloku info zůstanou čítače None.
fn apply_info<'a>(s: &mut Sock, toks: impl Iterator<Item = &'a str>) {
    let (mut sent, mut acked, mut recv, mut info) = (None, None, None, false);
    for t in toks {
        let Some((k, v)) = t.split_once(':') else { continue };
        match k {
            "ino" => s.inode = v.parse().ok().or(s.inode),
            "bytes_sent" => sent = v.parse::<u64>().ok(),
            "bytes_acked" => acked = v.parse::<u64>().ok(),
            "bytes_received" => recv = v.parse::<u64>().ok(),
            "rto" | "rtt" | "mss" | "cwnd" | "segs_out" | "segs_in" => info = true,
            _ => {}
        }
    }
    if info || sent.is_some() || acked.is_some() || recv.is_some() {
        s.bytes_in = Some(recv.unwrap_or(0));
        // bytes_acked počítá i SYN (o 1 víc než odeslaná data); bytes_sent i opakovaná vysílání
        s.bytes_out = Some(match (acked, sent) {
            (Some(a), Some(b)) => a.min(b),
            (a, b) => a.or(b).unwrap_or(0),
        });
    }
}

fn parse_ss_line(line: &str) -> Option<Sock> {
    let tok: Vec<&str> = line.split_whitespace().collect();
    let mut i = 0;
    let mut proto = String::new();
    if let Some(t) = tok.first() {
        if matches!(*t, "tcp" | "udp" | "raw" | "u_str" | "u_dgr" | "u_seq" | "p_raw" | "p_dgr" | "nl" | "mptcp" | "sctp") {
            proto = t.to_string();
            i = 1;
        }
    }
    let mut state = String::new();
    if tok.get(i).is_some_and(|t| t.parse::<u64>().is_err()) {
        state = tok[i].to_string();
        i += 1;
    }
    // Recv-Q, Send-Q
    tok.get(i)?.parse::<u64>().ok()?;
    tok.get(i + 1)?.parse::<u64>().ok()?;
    let (local_ip, lp) = split_addr(tok.get(i + 2)?)?;
    let (peer_ip, peer_port) = split_addr(tok.get(i + 3)?)?;
    let rest = tok[i + 4..].join(" ");
    let (procs, pid) = rest.find("users:(").map(|p| parse_users(&rest[p..])).unwrap_or_default();
    if proto.is_empty() {
        // bez sloupce Netid (jen jeden protokol): odhad podle stavu
        proto = if state == "UNCONN" { "udp".into() } else { "tcp".into() };
    }
    let mut s = Sock { proto, state, local_ip, local_port: lp?, peer_ip, peer_port, procs, pid, ..Default::default() };
    // jednořádkový výstup (`-O`) nebo `-e` na hlavním řádku
    apply_info(&mut s, tok[i + 4..].iter().copied());
    Some(s)
}

/// Spojení je příchozí (místní port je port, na kterém kontejner naslouchá).
pub fn is_inbound(s: &Sock, listen: &[Sock]) -> bool {
    listen.iter().any(|l| l.proto == s.proto && l.local_port == s.local_port)
}

/// Výpis socketů v síťových jmenných prostorech kontejnerů (skript `ns_script`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NsDump {
    pub listen: Vec<Sock>,
    pub conns: Vec<Sock>,
    pub error: Option<String>,
    /// součet čítačů rozhraní kromě `lo` (rx, tx) v síťovém prostoru kontejneru
    pub dev: Option<(u64, u64)>,
}

/// Rozdělí výstup skriptu na části podle PID: `@@PID <pid>`, pak naslouchající sockety,
/// volitelně `@@EST` a navázaná spojení (i s odsazenými řádky `ss -i`) a `@@DEV`
/// (`/proc/net/dev`). Řádky `nsenter: …` jsou chyba.
pub fn parse_ns_dump(text: &str) -> HashMap<u32, NsDump> {
    #[derive(PartialEq)]
    enum Part {
        Listen,
        Est,
        Dev,
    }
    let mut out: HashMap<u32, NsDump> = HashMap::new();
    let mut cur: Option<u32> = None;
    let mut part = Part::Listen;
    for l in text.lines() {
        if let Some(p) = l.strip_prefix("@@PID ") {
            cur = p.trim().parse().ok();
            part = Part::Listen;
            if let Some(p) = cur {
                out.entry(p).or_default();
            }
            continue;
        }
        match l.trim() {
            "@@EST" => {
                part = Part::Est;
                continue;
            }
            "@@DEV" => {
                part = Part::Dev;
                continue;
            }
            _ => {}
        }
        let Some(d) = cur.and_then(|p| out.get_mut(&p)) else { continue };
        if l.starts_with("nsenter:") || l.starts_with("ss:") || l.starts_with("cat:") {
            d.error.get_or_insert_with(|| l.trim().to_string());
            continue;
        }
        match part {
            Part::Listen => push_ss_line(&mut d.listen, l),
            Part::Est => push_ss_line(&mut d.conns, l),
            Part::Dev => {
                for i in parse_proc_net_dev(l).into_iter().filter(|i| i.name != "lo") {
                    let (rx, tx) = d.dev.unwrap_or((0, 0));
                    d.dev = Some((rx + i.rx, tx + i.tx));
                }
            }
        }
    }
    out
}

/// Skript pro `sh -c`: `$1` = `listen` (jen naslouchající), `all` (i navázaná spojení
/// s čítači TCP) nebo `detail` (navíc čítače rozhraní), další argumenty = PID kontejnerů.
/// PID se předávají jako argumenty (nikdy se nevkládají do textu skriptu) a ještě se ověří.
pub const NS_SCRIPT: &str = r#"mode=$1; shift
for p in "$@"; do
  case "$p" in ''|*[!0-9]*) continue;; esac
  echo "@@PID $p"
  nsenter -t "$p" -n ss -ltnupH 2>&1
  if [ "$mode" != listen ]; then echo "@@EST"; nsenter -t "$p" -n ss -tuinpeH state established 2>&1; fi
  if [ "$mode" = detail ]; then echo "@@DEV"; nsenter -t "$p" -n cat /proc/net/dev 2>&1; fi
done"#;

// ---------------------------------------------------------------- spojení: rychlosti a historie

/// Klíč spojení: protokol + místní a vzdálená adresa s porty.
pub fn conn_key(s: &Sock) -> String {
    format!("{} {} {}", s.proto, s.local(), s.peer())
}

/// Navázané spojení sledované mezi vzorky.
#[derive(Debug, Clone, PartialEq)]
pub struct Conn {
    pub sock: Sock,
    pub inbound: bool,
    /// čas prvního a posledního vzorku, ve kterém spojení existovalo (s)
    pub first_seen: f64,
    pub last_seen: f64,
    /// (dovnitř B/s, ven B/s); None = první vzorek nebo spojení bez čítačů
    pub rate: Option<(f64, f64)>,
}

impl Conn {
    pub fn key(&self) -> String {
        conn_key(&self.sock)
    }
}

/// Je to pořád stejné spojení? Jiný inode nebo čítač, který klesl = čtveřice adres
/// se mezi vzorky znovu použila pro nové spojení.
fn same_conn(old: &Sock, new: &Sock) -> bool {
    let dropped = |a: Option<u64>, b: Option<u64>| matches!((a, b), (Some(a), Some(b)) if b < a);
    let other_inode = matches!((old.inode, new.inode), (Some(a), Some(b)) if a != b);
    !other_inode && !dropped(old.bytes_in, new.bytes_in) && !dropped(old.bytes_out, new.bytes_out)
}

/// Rychlosti spojení podle klíče, historie ukončených spojení a provoz kontejneru
/// za posledních `window` sekund. Paměť je omezená (nejvýš `cap` ukončených spojení).
#[derive(Debug, Clone)]
pub struct ConnTracker {
    live: HashMap<String, Conn>,
    /// ukončená spojení, nejstarší první
    pub closed: VecDeque<Conn>,
    /// (t, přijato B/s, odesláno B/s) z čítačů rozhraní kontejneru
    pub traffic: VecDeque<(f64, f64, f64)>,
    dev: Option<(u64, u64, f64)>,
    window: f64,
    cap: usize,
}

/// Nejvýš tolik vzorků provozu (pojistka pro velmi časté vzorky).
const TRAFFIC_CAP: usize = 4096;

impl ConnTracker {
    pub fn new(window: f64, cap: usize) -> Self {
        Self { live: HashMap::new(), closed: VecDeque::new(), traffic: VecDeque::new(), dev: None, window, cap: cap.max(1) }
    }

    /// Nový vzorek navázaných spojení `(příchozí, socket)` v čase `t` (s).
    pub fn update(&mut self, t: f64, conns: &[(bool, Sock)]) {
        let mut next: HashMap<String, Conn> = HashMap::with_capacity(conns.len());
        for (inbound, s) in conns {
            let key = conn_key(s);
            if next.contains_key(&key) {
                continue;
            }
            let fresh = Conn { sock: s.clone(), inbound: *inbound, first_seen: t, last_seen: t, rate: None };
            let c = match self.live.remove(&key) {
                Some(old) if same_conn(&old.sock, s) => {
                    let dt = t - old.last_seen;
                    let r = match (old.sock.bytes_in, s.bytes_in, old.sock.bytes_out, s.bytes_out) {
                        (Some(a), Some(b), Some(c), Some(d)) => rate(a, b, dt).zip(rate(c, d, dt)),
                        _ => None,
                    };
                    Conn { first_seen: old.first_seen, rate: r, ..fresh }
                }
                Some(old) => {
                    // stejná čtveřice adres, ale nové spojení: staré do historie
                    self.close(old);
                    fresh
                }
                None => fresh,
            };
            next.insert(key, c);
        }
        let mut gone: Vec<Conn> = std::mem::replace(&mut self.live, next).into_values().collect();
        gone.sort_by(|a, b| a.first_seen.total_cmp(&b.first_seen).then_with(|| a.key().cmp(&b.key())));
        for c in gone {
            self.close(c);
        }
        self.evict(t);
    }

    /// Kumulované čítače rozhraní kontejneru v čase `t` -> vzorek provozu
    /// (po vynulování čítačů – restart kontejneru – se jeden vzorek vynechá).
    pub fn push_dev(&mut self, t: f64, rx: u64, tx: u64) {
        if let Some((prx, ptx, pt)) = self.dev {
            if let Some((r, w)) = rate(prx, rx, t - pt).zip(rate(ptx, tx, t - pt)) {
                self.traffic.push_back((t, r, w));
            }
        }
        self.dev = Some((rx, tx, t));
        self.evict(t);
    }

    fn close(&mut self, c: Conn) {
        self.closed.push_back(c);
        while self.closed.len() > self.cap {
            self.closed.pop_front();
        }
    }

    fn evict(&mut self, t: f64) {
        while self.closed.front().is_some_and(|c| c.last_seen < t - self.window) {
            self.closed.pop_front();
        }
        while self.traffic.front().is_some_and(|s| s.0 < t - self.window) || self.traffic.len() > TRAFFIC_CAP {
            self.traffic.pop_front();
        }
    }

    /// Právě navázaná spojení (seřazeno podle klíče).
    pub fn live(&self) -> Vec<Conn> {
        let mut v: Vec<Conn> = self.live.values().cloned().collect();
        v.sort_by_key(|c| c.key());
        v
    }
}

// ---------------------------------------------------------------- porty kontejneru

/// Jeden řádek porovnání „naslouchá“ × „publikováno“ pro port kontejneru.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PortCheck {
    pub port: u16,
    pub proto: String,
    /// adresy, na kterých proces v kontejneru naslouchá (prázdné = nenaslouchá)
    pub listen: Vec<String>,
    /// proces(y)
    pub process: String,
    /// publikace na hostiteli (`0.0.0.0:8080`), prázdné = nepublikováno
    pub published: Vec<String>,
}

impl PortCheck {
    pub fn listening(&self) -> bool {
        !self.listen.is_empty()
    }
    /// naslouchá, ale není publikovaný (dostupný jen z jiných kontejnerů)
    pub fn unpublished(&self) -> bool {
        self.listening() && self.published.is_empty()
    }
    /// publikovaný, ale nikdo na něm nenaslouchá
    pub fn dead(&self) -> bool {
        !self.listening() && !self.published.is_empty()
    }
    /// publikovaný, ale proces naslouchá jen na loopbacku kontejneru (zvenku nedostupný)
    pub fn loopback_only(&self) -> bool {
        !self.published.is_empty() && !self.listen.is_empty() && self.listen.iter().all(|a| a.split(':').next().is_some_and(is_loopback) || a.starts_with("[::1]"))
    }
}

/// Publikovaný port kontejneru: (port v kontejneru, protokol, host IP, host port).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub private: u16,
    pub proto: String,
    pub host_ip: String,
    pub host_port: u16,
}

/// Sloučí naslouchající sockety kontejneru s publikovanými porty (seřazeno podle portu).
pub fn compare_ports(listen: &[Sock], published: &[Published]) -> Vec<PortCheck> {
    let mut rows: Vec<PortCheck> = Vec::new();
    let mut at = |port: u16, proto: &str| -> usize {
        match rows.iter().position(|r| r.port == port && r.proto == proto) {
            Some(i) => i,
            None => {
                rows.push(PortCheck { port, proto: proto.to_string(), ..Default::default() });
                rows.len() - 1
            }
        }
    };
    let mut idx = Vec::new();
    for s in listen.iter().filter(|s| s.proto == "tcp" || s.proto == "udp") {
        idx.push((at(s.local_port, &s.proto), s));
    }
    let mut pub_idx = Vec::new();
    for p in published {
        pub_idx.push((at(p.private, &p.proto), p));
    }
    for (i, s) in idx {
        let a = s.local();
        if !rows[i].listen.contains(&a) {
            rows[i].listen.push(a);
        }
        let pr = s.procs.join(", ");
        if rows[i].process.is_empty() {
            rows[i].process = pr;
        }
    }
    for (i, p) in pub_idx {
        let ip = if p.host_ip.is_empty() { "0.0.0.0" } else { p.host_ip.as_str() };
        let a = join_addr(ip, Some(p.host_port));
        if !rows[i].published.contains(&a) {
            rows[i].published.push(a);
        }
    }
    rows.sort_by(|a, b| a.port.cmp(&b.port).then_with(|| a.proto.cmp(&b.proto)));
    rows
}

/// Publikované porty kontejneru ze seznamu kontejnerů (API `/containers/json`).
pub fn published_of(ports: &[crate::docker::Port]) -> Vec<Published> {
    let mut v: Vec<Published> = Vec::new();
    for p in ports {
        let Some(hp) = p.public_port.filter(|p| *p > 0) else { continue };
        let x = Published {
            private: p.private_port,
            proto: if p.typ.is_empty() { "tcp".into() } else { p.typ.clone() },
            host_ip: p.ip.clone().unwrap_or_default(),
            host_port: hp,
        };
        if !v.contains(&x) {
            v.push(x);
        }
    }
    v
}

// ---------------------------------------------------------------- konflikty publikovaných portů

/// Publikovaný port s kontejnerem (pro tabulku a hledání konfliktů).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostBinding {
    pub container_id: String,
    pub container: String,
    pub stack: String,
    pub running: bool,
    pub p: Published,
}

/// Překrývají se dvě adresy hostitele? (stejná, nebo jedna z nich „libovolná“)
pub fn ip_overlaps(a: &str, b: &str) -> bool {
    a == b || is_wildcard(a) || is_wildcard(b)
}

/// Pro každý publikovaný port: názvy jiných kontejnerů se stejným portem a protokolem
/// na překrývající se adrese (prázdné = bez konfliktu).
pub fn container_conflicts(list: &[HostBinding]) -> Vec<Vec<String>> {
    list.iter()
        .map(|a| {
            let mut v: Vec<String> = list
                .iter()
                .filter(|b| {
                    b.container_id != a.container_id && b.p.host_port == a.p.host_port && b.p.proto == a.p.proto && ip_overlaps(&a.p.host_ip, &b.p.host_ip)
                })
                .map(|b| b.container.clone())
                .collect();
            v.dedup();
            v
        })
        .collect()
}

// ---------------------------------------------------------------- Windows

/// Naslouchající port ve Windows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WinListen {
    pub proto: String,
    pub ip: String,
    pub port: u16,
    pub pid: u32,
    /// název procesu (doplní se podle PID, když jde zjistit)
    pub process: String,
}

/// `netstat -ano`: řádky `TCP 0.0.0.0:135 0.0.0.0:0 LISTENING 2144` a `UDP 0.0.0.0:5353 *:* 1234`.
pub fn parse_netstat(text: &str) -> Vec<WinListen> {
    text.lines()
        .filter_map(|l| {
            let t: Vec<&str> = l.split_whitespace().collect();
            let proto = t.first()?.to_ascii_lowercase();
            let (addr, pid) = match (proto.as_str(), t.len()) {
                ("tcp", 5) if t[3].eq_ignore_ascii_case("LISTENING") => (t[1], t[4]),
                ("udp", 4) => (t[1], t[3]),
                _ => return None,
            };
            let (ip, port) = split_addr(addr)?;
            Some(WinListen { proto, ip, port: port?, pid: pid.parse().ok()?, process: String::new() })
        })
        .collect()
}

/// Jak je publikovaný port vidět ve Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WinState {
    /// přeposílá ho WSL (wslrelay.exe) nebo relay aplikace
    Forwarded,
    /// port ve Windows drží jiný proces (název, PID)
    Conflict(String, u32),
    /// ve Windows na portu nikdo nenaslouchá (localhost forwarding nefunguje / kontejner neběží)
    NotForwarded,
}

/// Procesy, které ve Windows přeposílají porty z WSL (nejsou konflikt).
pub fn is_wsl_forwarder(process: &str) -> bool {
    let p = process.to_ascii_lowercase();
    let p = p.rsplit(['\\', '/']).next().unwrap_or(&p);
    matches!(p, "wslrelay.exe" | "wslservice.exe" | "wslhost.exe" | "vmcompute.exe")
}

/// Stav publikovaného portu ve Windows. `own_pid` = tato aplikace (její relay není konflikt).
pub fn windows_state(p: &Published, win: &[WinListen], own_pid: u32) -> WinState {
    let hits: Vec<&WinListen> = win.iter().filter(|w| w.port == p.host_port && w.proto == p.proto).collect();
    if let Some(w) = hits.iter().find(|w| !is_wsl_forwarder(&w.process) && w.pid != own_pid && ip_overlaps_win(&w.ip, &p.host_ip)) {
        let name = if w.process.is_empty() { format!("PID {}", w.pid) } else { w.process.clone() };
        return WinState::Conflict(name, w.pid);
    }
    if hits.iter().any(|w| is_wsl_forwarder(&w.process) || w.pid == own_pid) {
        WinState::Forwarded
    } else {
        WinState::NotForwarded
    }
}

/// Adresa ve Windows koliduje s publikací? Publikace 0.0.0.0 / 127.0.0.1 se do Windows
/// dostane jako 127.0.0.1 (NAT) – koliduje s 0.0.0.0, 127.0.0.1 i libovolnou IPv4.
fn ip_overlaps_win(win_ip: &str, host_ip: &str) -> bool {
    if is_wildcard(win_ip) || is_loopback(win_ip) {
        return true;
    }
    !host_ip.is_empty() && win_ip == host_ip
}

// ---------------------------------------------------------------- ostatní distribuce WSL

/// Naslouchající TCP sockety v jiné běžící distribuci WSL.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DistroListen {
    pub distro: String,
    /// síťový jmenný prostor (`net:[4026531840]`), prázdné = neznámý
    pub netns: String,
    pub listen: Vec<Sock>,
    pub error: Option<String>,
}

/// Skript pro distribuci: síťový jmenný prostor a naslouchající TCP sockety
/// (`ss`, v distribucích bez iproute2 záložně `netstat`).
pub const DISTRO_SCRIPT: &str = "readlink /proc/self/ns/net; echo @@WSLCD@@; ss -ltnpH 2>/dev/null || netstat -ltn 2>/dev/null";

/// Výstup `DISTRO_SCRIPT` -> (síťový prostor, naslouchající sockety).
pub fn parse_distro_listen(text: &str) -> (String, Vec<Sock>) {
    let (ns, ss) = text.split_once(MARK).unwrap_or(("", text));
    let ns = ns.trim();
    let ns = if ns.starts_with("net:") { ns.to_string() } else { String::new() };
    (ns, parse_ss(ss).into_iter().filter(|s| s.proto == "tcp").collect())
}

/// Distribuce, které drží publikovaný port kontejneru (konflikt – wslrelay do Windows
/// přeposílá jejich socket, ne kontejner).
///
/// Distribuce WSL 2 v režimu NAT i mirrored sdílejí jeden síťový prostor VM, takže `ss`
/// v jiné distribuci vidí i sockety enginu. Socket se proto té distribuci přisoudí jen,
/// když v ní `ss` ukáže vlastnící proces (procesy jiných distribucí nevidí), nebo když
/// distribuce má prokazatelně vlastní síťový prostor (`engine_netns` se liší).
pub fn distro_conflicts(p: &Published, engine_netns: &str, others: &[DistroListen]) -> Vec<String> {
    others
        .iter()
        .filter(|d| {
            let separate = !d.netns.is_empty() && !engine_netns.is_empty() && d.netns != engine_netns;
            d.listen.iter().any(|s| {
                s.proto == p.proto && s.local_port == p.host_port && ip_overlaps(&s.local_ip, &p.host_ip) && (separate || !s.procs.is_empty())
            })
        })
        .map(|d| d.distro.clone())
        .collect()
}

// ---------------------------------------------------------------- naslouchající sockety v distribuci

/// Co je naslouchající socket v distribuci.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenKind {
    /// API enginu (dockerd / podman na TCP)
    EngineApi,
    /// TLS proxy před API (`wslcd-tls-proxy`, socat)
    TlsProxy,
    Dns,
    /// přesměrování publikovaného portu kontejneru (conmon, docker-proxy, rootlessport…)
    ContainerProxy,
    Other,
}

/// Zařadí naslouchající socket distribuce. `api_port` = port API enginu,
/// `published` = porty publikované kontejnery.
pub fn listen_kind(s: &Sock, api_port: u16, published: &[u16]) -> ListenKind {
    let has = |n: &str| s.procs.iter().any(|p| p == n);
    if s.local_port == api_port && s.proto == "tcp" {
        // v režimu TLS drží port socat (wslcd-tls-proxy), jinak přímo dockerd / podman
        return if has("socat") { ListenKind::TlsProxy } else { ListenKind::EngineApi };
    }
    if s.local_port == 53 || has("systemd-resolve") || has("dnsmasq") || has("aardvark-dns") {
        return ListenKind::Dns;
    }
    if has("conmon") || has("docker-proxy") || has("rootlessport") || has("rootlessport-child") || has("pasta") || has("slirp4netns") {
        return ListenKind::ContainerProxy;
    }
    if s.procs.is_empty() && published.contains(&s.local_port) {
        return ListenKind::ContainerProxy;
    }
    ListenKind::Other
}

// ---------------------------------------------------------------- formát

/// Rychlost čitelně: `0 B/s`, `12.3 KB/s`…
pub fn fmt_rate(bps: f64) -> String {
    const U: [&str; 4] = ["B/s", "KB/s", "MB/s", "GB/s"];
    let mut v = bps.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{v:.0} {}", U[0]) } else { format!("{v:.1} {}", U[i]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEV: &str = "Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 32442839   76404    0    0    0     0          0         0 32442839   76404    0    0    0     0       0          0
  eth0: 1683225892 1050775    0    0    0     0          0       283 19239598  258814    0    0    0     0       0          0
podman0: 4528281   74061    0    0    0     0          0       178 362268730   93802    0    0    0     0       0          0
";

    #[test]
    fn proc_net_dev() {
        let v = parse_proc_net_dev(DEV);
        assert_eq!(v.len(), 3);
        let eth = v.iter().find(|i| i.name == "eth0").unwrap();
        assert_eq!((eth.rx, eth.tx), (1683225892, 19239598));
        assert_eq!(v[2], IfCounters { name: "podman0".into(), rx: 4528281, tx: 362268730 });
    }

    #[test]
    fn rates() {
        assert_eq!(rate(100, 300, 2.0), Some(100.0));
        assert_eq!(rate(300, 100, 2.0), None);
        assert_eq!(rate(1, 2, 0.0), None);
        let mut r = RateTracker::default();
        assert_eq!(r.update("a", 1000, 50, 10.0), None);
        assert_eq!(r.update("a", 3000, 250, 12.0), Some((1000.0, 100.0)));
        // čítač se vynuloval (restart kontejneru) -> bez hodnoty, další vzorek už ano
        assert_eq!(r.update("a", 10, 10, 14.0), None);
        assert_eq!(r.update("a", 20, 30, 15.0), Some((10.0, 20.0)));
        r.update("b", 1, 1, 15.0);
        r.retain(&["b"]);
        assert_eq!(r.update("a", 40, 40, 16.0), None);
        assert_eq!(fmt_rate(512.0), "512 B/s");
        assert_eq!(fmt_rate(1536.0), "1.5 KB/s");
    }

    #[test]
    fn ip_addr() {
        let t = "2: eth0    inet 192.168.73.86/20 brd 192.168.79.255 scope global eth0\\       valid_lft forever preferred_lft forever";
        assert_eq!(parse_ip_addr(t).as_deref(), Some("192.168.73.86"));
        assert_eq!(parse_ip_addr(""), None);
    }

    #[test]
    fn addresses() {
        assert_eq!(split_addr("127.0.0.1:2376"), Some(("127.0.0.1".into(), Some(2376))));
        assert_eq!(split_addr("[::]:80"), Some(("::".into(), Some(80))));
        assert_eq!(split_addr("*:9999"), Some(("*".into(), Some(9999))));
        assert_eq!(split_addr("0.0.0.0:*"), Some(("0.0.0.0".into(), None)));
        assert_eq!(split_addr("127.0.0.53%lo:53"), Some(("127.0.0.53".into(), Some(53))));
        assert_eq!(split_addr("[fe80::1%eth0]:546"), Some(("fe80::1".into(), Some(546))));
        assert_eq!(split_addr("[::ffff:10.0.0.2]:443"), Some(("10.0.0.2".into(), Some(443))));
        assert_eq!(split_addr(":::80"), Some(("::".into(), Some(80))));
        assert_eq!(join_addr("::1", Some(5)), "[::1]:5");
        assert_eq!(join_addr("1.2.3.4", None), "1.2.3.4:*");
    }

    #[test]
    fn ss_listen_v4_v6_udp_and_processes() {
        let t = r#"udp UNCONN 0      0      10.255.255.254:53   0.0.0.0:*
udp UNCONN 0      0               [::1]:323     [::]:*
tcp LISTEN 0      5           127.0.0.1:2376 0.0.0.0:* users:(("socat",pid=33919,fd=6))
tcp LISTEN 0      511    0.0.0.0:80   0.0.0.0:* users:(("nginx",pid=43659,fd=6),("nginx",pid=43658,fd=6),("nginx",pid=43644,fd=6))
tcp LISTEN 0      1            *:9999       *:* users:(("nc",pid=43648,fd=3))
tcp LISTEN 0      4096     [::]:8080     [::]:* users:(("docker-proxy",pid=7,fd=4),("x y",pid=8,fd=1))
nsenter: cannot open /proc/1/ns/net: No such file
"#;
        let v = parse_ss(t);
        assert_eq!(v.len(), 6);
        assert_eq!(v[0].proto, "udp");
        assert_eq!(v[0].state, "UNCONN");
        assert_eq!(v[0].local(), "10.255.255.254:53");
        assert_eq!(v[0].peer_port, None);
        assert!(v[0].procs.is_empty() && v[0].pid.is_none());
        assert_eq!(v[1].local(), "[::1]:323");
        assert!(v[1].loopback_only());
        assert_eq!(v[2].procs, vec!["socat"]);
        assert_eq!(v[2].pid, Some(33919));
        assert_eq!(v[3].procs, vec!["nginx"]);
        assert_eq!(v[3].pid, Some(43659));
        assert_eq!(v[3].process(), "nginx (43659)");
        assert_eq!(v[4].local_ip, "*");
        assert_eq!(v[4].local_port, 9999);
        assert_eq!(v[5].local_ip, "::");
        assert_eq!(v[5].procs, vec!["docker-proxy", "x y"]);
    }

    #[test]
    fn ss_established_without_state_column() {
        // s filtrem `state established` ss sloupec State vynechá
        let t = "tcp 0      0      127.0.0.1:2376  127.0.0.1:36494 users:((\"socat\",pid=43049,fd=7))
tcp 0      0      10.88.0.5:41234  10.88.0.4:80
udp 0      0      10.88.0.5:5353  1.1.1.1:53 users:((\"dig\",pid=9,fd=3))
tcp ESTAB 0 0 [2001:db8::2]:5000 [2001:db8::1]:443
";
        let v = parse_ss(t);
        assert_eq!(v.len(), 4);
        assert_eq!(v[0].state, "");
        assert_eq!(v[0].peer(), "127.0.0.1:36494");
        assert_eq!(v[1].peer_ip, "10.88.0.4");
        assert_eq!(v[1].peer_port, Some(80));
        assert_eq!(v[2].proto, "udp");
        assert_eq!(v[2].procs, vec!["dig"]);
        assert_eq!(v[3].state, "ESTAB");
        assert_eq!(v[3].peer(), "[2001:db8::1]:443");
        // bez Netid: odhad protokolu podle stavu
        let w = parse_ss("LISTEN 0 128 0.0.0.0:22 0.0.0.0:*\nUNCONN 0 0 0.0.0.0:68 0.0.0.0:*\n");
        assert_eq!((w[0].proto.as_str(), w[1].proto.as_str()), ("tcp", "udp"));
    }

    #[test]
    fn ns_dump() {
        let t = "@@PID 100
tcp LISTEN 0 511 0.0.0.0:80 0.0.0.0:* users:((\"nginx\",pid=101,fd=6))
@@EST
tcp 0 0 10.88.0.4:80 10.88.0.5:41234 users:((\"nginx\",pid=102,fd=9))
@@PID 200
nsenter: cannot open /proc/200/ns/net: No such file or directory
@@PID 300
@@EST
";
        let d = parse_ns_dump(t);
        assert_eq!(d.len(), 3);
        assert_eq!(d[&100].listen.len(), 1);
        assert_eq!(d[&100].conns.len(), 1);
        assert_eq!(d[&100].conns[0].peer_port, Some(41234));
        assert!(d[&200].error.as_deref().unwrap().contains("nsenter"));
        assert_eq!(d[&300], NsDump::default());
    }

    fn sock(proto: &str, ip: &str, port: u16, proc_: &str) -> Sock {
        Sock { proto: proto.into(), state: "LISTEN".into(), local_ip: ip.into(), local_port: port, procs: vec![proc_.into()], ..Default::default() }
    }
    fn publ(private: u16, host_ip: &str, host_port: u16) -> Published {
        Published { private, proto: "tcp".into(), host_ip: host_ip.into(), host_port }
    }

    #[test]
    fn compare_listening_and_published() {
        let listen = [sock("tcp", "0.0.0.0", 80, "nginx"), sock("tcp", "::", 80, "nginx"), sock("tcp", "*", 9999, "nc"), sock("tcp", "127.0.0.1", 5432, "postgres")];
        let pubs = [publ(80, "", 18190), publ(80, "::", 18190), publ(443, "127.0.0.1", 8443), publ(5432, "", 5432)];
        let rows = compare_ports(&listen, &pubs);
        assert_eq!(rows.iter().map(|r| r.port).collect::<Vec<_>>(), vec![80, 443, 5432, 9999]);
        let r80 = &rows[0];
        assert!(r80.listening() && !r80.unpublished() && !r80.dead() && !r80.loopback_only());
        assert_eq!(r80.listen, vec!["0.0.0.0:80", "[::]:80"]);
        assert_eq!(r80.published, vec!["0.0.0.0:18190", "[::]:18190"]);
        assert_eq!(r80.process, "nginx");
        assert!(rows[1].dead());
        assert!(rows[2].loopback_only());
        assert!(rows[3].unpublished());
    }

    #[test]
    fn published_from_api_ports() {
        use crate::docker::Port;
        let p = |ip: Option<&str>, pp: u16, hp: Option<u16>| Port { ip: ip.map(String::from), private_port: pp, public_port: hp, typ: "tcp".into() };
        let v = published_of(&[p(Some("0.0.0.0"), 80, Some(8080)), p(Some("0.0.0.0"), 80, Some(8080)), p(None, 443, None)]);
        assert_eq!(v, vec![publ(80, "0.0.0.0", 8080)]);
    }

    #[test]
    fn conflicts_between_containers() {
        let b = |id: &str, ip: &str, port: u16| HostBinding { container_id: id.into(), container: format!("c-{id}"), stack: String::new(), running: true, p: publ(80, ip, port) };
        let list = [b("a", "0.0.0.0", 8080), b("b", "127.0.0.1", 8080), b("c", "127.0.0.1", 9090), b("d", "10.0.0.1", 9090), b("a", "::", 8080)];
        let c = container_conflicts(&list);
        assert_eq!(c[0], vec!["c-b"]);
        assert_eq!(c[1], vec!["c-a"]);
        assert!(c[2].is_empty() && c[3].is_empty());
        assert!(ip_overlaps("", "1.2.3.4") && !ip_overlaps("127.0.0.1", "10.0.0.1"));
    }

    #[test]
    fn netstat_and_windows_state() {
        let t = "
Active Connections

  Proto  Local Address          Foreign Address        State           PID
  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       2144
  TCP    127.0.0.1:18190        0.0.0.0:0              LISTENING       26520
  TCP    127.0.0.1:50000        127.0.0.1:2376         ESTABLISHED     11
  TCP    [::]:8080              [::]:0                 LISTENING       777
  UDP    0.0.0.0:5353           *:*                                    1234
  UDP    [::1]:1900             *:*                                    55
";
        let mut w = parse_netstat(t);
        assert_eq!(w.len(), 5);
        assert_eq!(w[0], WinListen { proto: "tcp".into(), ip: "0.0.0.0".into(), port: 135, pid: 2144, process: String::new() });
        assert_eq!(w[2].ip, "::");
        assert_eq!(w[3].proto, "udp");
        assert_eq!(w[4].ip, "::1");
        w[1].process = r"C:\Program Files\WSL\wslrelay.exe".into();
        w[2].process = "java.exe".into();
        assert_eq!(windows_state(&publ(80, "", 18190), &w, 1), WinState::Forwarded);
        assert_eq!(windows_state(&publ(80, "", 8080), &w, 1), WinState::Conflict("java.exe".into(), 777));
        assert_eq!(windows_state(&publ(80, "", 8080), &w, 777), WinState::Forwarded);
        assert_eq!(windows_state(&publ(80, "", 9), &w, 1), WinState::NotForwarded);
        assert_eq!(windows_state(&publ(80, "", 135), &w, 1), WinState::Conflict("PID 2144".into(), 2144));
    }

    /// Skutečný výstup `nsenter -t <pid> -n ss -tinpH state established` (nginx:alpine,
    /// klient wget v druhém kontejneru, wget po IPv6 loopbacku a nečinné `nc`).
    const SS_TI: &str = "0      0      10.88.0.37:80    10.88.0.38:54358 users:((\"nginx\",pid=48549,fd=18))
\t cubic wscale:10,10 rto:204 rtt:0.066/0.019 ato:40 mss:1448 pmtu:1500 rcvmss:536 advmss:1448 cwnd:94 ssthresh:75 bytes_sent:12058881 bytes_acked:12058881 bytes_received:89 segs_out:8433 segs_in:452 data_segs_out:8432 data_segs_in:1 send 16498424242bps lastsnd:176 lastrcv:22220 lastack:176 pacing_rate 19649251120bps delivery_rate 12790666664bps delivered:8433 app_limited busy:32ms rcv_space:14480 rcv_ssthresh:64088 minrtt:0.009 snd_wnd:3636224 rcv_wnd:65536
0      0       127.0.0.1:80     127.0.0.1:42719 users:((\"nginx\",pid=48548,fd=18))
\t cubic wscale:10,10 rto:204 rtt:0.045/0.022 mss:32768 pmtu:65535 rcvmss:536 advmss:65483 cwnd:10 segs_in:2 send 58254222222bps lastsnd:3044 lastrcv:3044 lastack:3044 pacing_rate 116508444440bps delivered:1 app_limited rcv_space:65483 rcv_ssthresh:65483 minrtt:0.045 snd_wnd:65536 rcv_wnd:65483
0      0       127.0.0.1:42719  127.0.0.1:80    users:((\"nc\",pid=48781,fd=3))
\t cubic wscale:10,10 rto:204 rtt:0.039/0.019 mss:32741 pmtu:65535 rcvmss:536 advmss:65483 cwnd:10 bytes_acked:1 segs_out:2 segs_in:1 send 67161025641bps lastsnd:3044 lastrcv:3044 lastack:3044 pacing_rate 134322051280bps delivered:1 app_limited rcv_space:65495 rcv_ssthresh:65495 minrtt:0.039 snd_wnd:65483 rcv_wnd:65536
0      0           [::1]:59342      [::1]:80    users:((\"wget\",pid=48764,fd=3))
\t cubic wscale:10,10 rto:204 rtt:0.481/0.282 ato:40 mss:32768 pmtu:65536 rcvmss:65464 advmss:65464 cwnd:10 bytes_sent:84 bytes_acked:85 bytes_received:2097409 segs_out:54 segs_in:58 data_segs_out:1 data_segs_in:56 send 5449979210bps lastsnd:3132 lastrcv:128 lastack:128 pacing_rate 10885813648bps delivery_rate 15420235288bps delivered:2 app_limited rcv_rtt:125.012 rcv_space:458752 rcv_ssthresh:2966023 minrtt:0.017 snd_wnd:65536 rcv_wnd:2966528
0      0           [::1]:80         [::1]:59342 users:((\"nginx\",pid=48549,fd=20))
\t cubic wscale:10,10 rto:204 rtt:0.026/0.01 ato:40 mss:65464 pmtu:65536 rcvmss:536 advmss:65464 cwnd:10 bytes_sent:2097409 bytes_acked:2097409 bytes_received:84 segs_out:57 segs_in:54 data_segs_out:56 data_segs_in:1 send 201427692308bps lastsnd:128 lastrcv:3132 lastack:128 pacing_rate 395254339616bps delivery_rate 43642666664bps delivered:57 busy:8ms rcv_space:65464 rcv_ssthresh:65464 minrtt:0.005 snd_wnd:2966528 rcv_wnd:65536
";

    #[test]
    fn ss_tcp_info_counters() {
        let v = parse_ss(SS_TI);
        assert_eq!(v.len(), 5);
        // server posílá soubor
        assert_eq!(v[0].peer(), "10.88.0.38:54358");
        assert_eq!(v[0].procs, vec!["nginx"]);
        assert_eq!((v[0].bytes_in, v[0].bytes_out), (Some(89), Some(12058881)));
        // nečinné spojení bez čítačů v bloku info = 0
        assert_eq!((v[1].bytes_in, v[1].bytes_out), (Some(0), Some(0)));
        // bytes_acked:1 (jen SYN), bytes_sent chybí
        assert_eq!((v[2].bytes_in, v[2].bytes_out), (Some(0), Some(1)));
        // IPv6; bytes_acked o 1 větší než bytes_sent -> bere se menší
        assert_eq!(v[3].local(), "[::1]:59342");
        assert_eq!(v[3].peer(), "[::1]:80");
        assert_eq!((v[3].bytes_in, v[3].bytes_out), (Some(2097409), Some(84)));
        assert_eq!((v[4].bytes_in, v[4].bytes_out), (Some(84), Some(2097409)));
        // bez -i: žádné čítače
        let w = parse_ss("0 0 10.88.0.37:80 10.88.0.38:54358 users:((\"nginx\",pid=1,fd=18))\n");
        assert_eq!((w[0].bytes_in, w[0].bytes_out, w[0].inode), (None, None, None));
        // jednořádkový výstup (-O) s -e
        let o = parse_ss("tcp 0 0 10.0.0.2:5 10.0.0.1:443 users:((\"curl\",pid=7,fd=3)) uid:1000 ino:4242 sk:1 <-> cubic rto:204 mss:1448 bytes_acked:10 bytes_received:2048\n");
        assert_eq!((o[0].bytes_in, o[0].bytes_out, o[0].inode), (Some(2048), Some(10), Some(4242)));
        // starší ss / jádro: blok info bez bytes_* -> 0, ne chyba; chybový řádek se ignoruje
        let x = parse_ss("ss: unknown option\n0 0 1.1.1.1:1 2.2.2.2:2\n\t cubic rto:204 mss:1448\n");
        assert_eq!((x.len(), x[0].bytes_in), (1, Some(0)));
    }

    #[test]
    fn ns_dump_with_info_and_dev() {
        let t = format!(
            "@@PID 100\ntcp LISTEN 0 511 0.0.0.0:80 0.0.0.0:* users:((\"nginx\",pid=101,fd=6))\n@@EST\n{}@@DEV\n{DEV}",
            SS_TI.replace("0      0", "tcp 0 0")
        );
        let d = parse_ns_dump(&t);
        let x = &d[&100];
        assert_eq!(x.listen.len(), 1);
        assert_eq!(x.conns.len(), 5);
        assert_eq!(x.conns[0].bytes_out, Some(12058881));
        assert_eq!(x.conns[3].bytes_in, Some(2097409));
        assert!(is_inbound(&x.conns[0], &x.listen) && !is_inbound(&x.conns[3], &x.listen));
        // lo se nepočítá: eth0 + podman0
        assert_eq!(x.dev, Some((1683225892 + 4528281, 19239598 + 362268730)));
    }

    fn conn(local: u16, peer: u16, rx: Option<u64>, tx: Option<u64>, ino: Option<u64>) -> Sock {
        Sock {
            proto: "tcp".into(),
            local_ip: "10.0.0.2".into(),
            local_port: local,
            peer_ip: "10.0.0.1".into(),
            peer_port: Some(peer),
            bytes_in: rx,
            bytes_out: tx,
            inode: ino,
            ..Default::default()
        }
    }

    #[test]
    fn conn_rates_reuse_and_resets() {
        let mut t = ConnTracker::new(900.0, 100);
        let a = |rx, tx| (false, conn(5000, 80, Some(rx), Some(tx), Some(1)));
        t.update(0.0, &[a(100, 10), (true, conn(80, 6000, None, None, None))]);
        assert!(t.live().iter().all(|c| c.rate.is_none()));
        t.update(2.0, &[a(2100, 410), (true, conn(80, 6000, None, None, None))]);
        let live = t.live();
        let c = live.iter().find(|c| c.sock.local_port == 5000).unwrap();
        assert_eq!(c.rate, Some((1000.0, 200.0)));
        assert_eq!((c.first_seen, c.last_seen), (0.0, 2.0));
        // spojení bez čítačů: bez rychlosti, ale sleduje se
        let n = live.iter().find(|c| c.sock.local_port == 80).unwrap();
        assert!(n.inbound && n.rate.is_none() && n.first_seen == 0.0);
        // čítač klesl = stejná čtveřice, nové spojení -> staré do historie
        t.update(4.0, &[a(50, 5)]);
        assert_eq!(t.closed.len(), 2);
        assert_eq!(t.closed[0].sock.bytes_in, Some(2100));
        assert_eq!((t.closed[0].first_seen, t.closed[0].last_seen), (0.0, 2.0));
        assert_eq!(t.closed[1].sock.local_port, 80);
        let c = &t.live()[0];
        assert_eq!((c.first_seen, c.rate), (4.0, None));
        // jiný inode při vyšších čítačích = také nové spojení
        t.update(6.0, &[(false, conn(5000, 80, Some(9000), Some(90), Some(2)))]);
        assert_eq!(t.closed.len(), 3);
        assert_eq!(t.live()[0].first_seen, 6.0);
        t.update(8.0, &[(false, conn(5000, 80, Some(13000), Some(90), Some(2)))]);
        assert_eq!(t.live()[0].rate, Some((2000.0, 0.0)));
        // spojení zmizelo
        t.update(10.0, &[]);
        assert!(t.live().is_empty());
        let last = t.closed.back().unwrap();
        assert_eq!((last.first_seen, last.last_seen, last.sock.bytes_in), (6.0, 8.0, Some(13000)));
    }

    #[test]
    fn conn_history_eviction_and_cap() {
        let mut t = ConnTracker::new(60.0, 3);
        for i in 0..5u16 {
            let f = f64::from(i) * 2.0;
            t.update(f, &[(false, conn(1000 + i, 80, Some(1), Some(1), None))]);
        }
        t.update(10.0, &[]);
        // 5 ukončených, ale strop 3 -> zůstanou nejnovější
        assert_eq!(t.closed.iter().map(|c| c.sock.local_port).collect::<Vec<_>>(), vec![1002, 1003, 1004]);
        // starší než okno se zahodí
        t.update(66.5, &[]);
        assert_eq!(t.closed.iter().map(|c| c.sock.local_port).collect::<Vec<_>>(), vec![1004]);
        t.update(80.0, &[]);
        assert!(t.closed.is_empty());
        // provoz rozhraní: rychlost z rozdílu, reset čítače vynechá vzorek, okno ořízne
        t.push_dev(0.0, 1000, 100);
        t.push_dev(2.0, 3000, 300);
        t.push_dev(4.0, 10, 10);
        t.push_dev(6.0, 210, 50);
        assert_eq!(t.traffic.iter().copied().collect::<Vec<_>>(), vec![(2.0, 1000.0, 100.0), (6.0, 100.0, 20.0)]);
        t.push_dev(64.0, 210, 50);
        assert_eq!(t.traffic.iter().map(|s| s.0).collect::<Vec<_>>(), vec![6.0, 64.0]);
    }

    #[test]
    fn distro_port_conflicts() {
        let p = publ(80, "", 8080);
        let lis = |port: u16, procs: &[&str]| Sock {
            proto: "tcp".into(),
            local_ip: "0.0.0.0".into(),
            local_port: port,
            procs: procs.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let d = |name: &str, ns: &str, listen: Vec<Sock>| DistroListen { distro: name.into(), netns: ns.into(), listen, error: None };
        let engine = "net:[4026531840]";
        let others = [
            // sdílený prostor: socket enginu (proces z jiné distribuce není vidět) – ne konflikt
            d("Ubuntu", engine, vec![lis(8080, &[]), lis(22, &["sshd"])]),
            // sdílený prostor a vlastní proces – konflikt
            d("Fedora", engine, vec![lis(8080, &["python3"])]),
            // vlastní síťový prostor – konflikt i bez procesu
            d("Alpine", "net:[4026532000]", vec![lis(8080, &[])]),
            // neznámý prostor bez procesu – nepřisuzovat
            d("Arch", "", vec![lis(8080, &[])]),
        ];
        assert_eq!(distro_conflicts(&p, engine, &others), vec!["Fedora", "Alpine"]);
        assert!(distro_conflicts(&publ(80, "", 9999), engine, &others).is_empty());
        // jiná adresa hostitele (10.0.0.1 × 127.0.0.1) se nepřekrývá
        let mut lo = lis(8080, &["node"]);
        lo.local_ip = "127.0.0.1".into();
        assert!(distro_conflicts(&publ(80, "10.0.0.1", 8080), engine, &[d("X", engine, vec![lo])]).is_empty());
        let (ns, socks) = parse_distro_listen("net:[4026531840]\n@@WSLCD@@\nLISTEN 0 4096 0.0.0.0:8080 0.0.0.0:* users:((\"python3\",pid=9,fd=3))\n");
        assert_eq!(ns, engine);
        assert_eq!(socks[0].procs, vec!["python3"]);
        // záložní netstat bez procesů
        let (ns, socks) = parse_distro_listen("readlink: x\n@@WSLCD@@\nActive Internet connections\ntcp 0 0 0.0.0.0:22 0.0.0.0:* LISTEN\ntcp6 0 0 :::80 :::* LISTEN\n");
        assert_eq!(ns, "");
        assert_eq!(socks.iter().map(|s| s.local_port).collect::<Vec<_>>(), vec![22, 80]);
    }

    #[test]
    fn distro_listener_kinds() {
        let mut s = sock("tcp", "127.0.0.1", 2376, "socat");
        assert_eq!(listen_kind(&s, 2376, &[]), ListenKind::TlsProxy);
        s.procs = vec!["podman".into()];
        assert_eq!(listen_kind(&s, 2375, &[]), ListenKind::Other);
        s.local_port = 2375;
        assert_eq!(listen_kind(&s, 2375, &[]), ListenKind::EngineApi);
        let dns = Sock { proto: "udp".into(), local_ip: "10.255.255.254".into(), local_port: 53, ..Default::default() };
        assert_eq!(listen_kind(&dns, 2376, &[]), ListenKind::Dns);
        assert_eq!(listen_kind(&sock("tcp", "0.0.0.0", 18190, "conmon"), 2376, &[]), ListenKind::ContainerProxy);
        let anon = Sock { proto: "tcp".into(), local_ip: "0.0.0.0".into(), local_port: 8080, ..Default::default() };
        assert_eq!(listen_kind(&anon, 2376, &[8080]), ListenKind::ContainerProxy);
        assert_eq!(listen_kind(&sock("tcp", "0.0.0.0", 22, "sshd"), 2376, &[]), ListenKind::Other);
    }
}
