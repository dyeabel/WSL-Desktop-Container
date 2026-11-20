//! `localhost` pro publikované porty kontejnerů i přes IPv6 (`::1`).
//!
//! V režimu NAT poslouchá `wslrelay` ve Windows na publikovaných portech jen na IPv4
//! `127.0.0.1`. Windows ale `localhost` přeloží nejdřív na `::1` a spojení tam není odmítnuto,
//! jen visí do vypršení časového limitu – klienti s krátkým limitem (health checky, testy)
//! selžou. Aplikace proto pro každý publikovaný TCP port běžícího kontejneru poslouchá na
//! `[::1]:<port>` (nikdy ne `[::]` – žádné vystavení do sítě) a spojení přeposílá na
//! `127.0.0.1:<port>`, podobně jako Docker Desktop.
//!
//! Sada relayí se srovnává s každým obnovením stavu (to je řízené proudem událostí), takže
//! start / stop kontejneru relay přidá / odebere. V režimu `networkingMode=mirrored` funguje
//! `::1` samo a relaye se nespouští. UDP se nepřeposílá (jen TCP).
//!
//! Styl jako `npipe`: std vlákna, jedno vlákno na příjem (neblokující accept s krátkým
//! spánkem, takže jde zastavit), na každé spojení vlákno pro každý směr.

use crate::docker::Container;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Jak často přijímací vlákno kontroluje příznak zastavení.
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// Limit pro spojení na `127.0.0.1:<port>`.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Obsazený `[::1]:<port>` zkusit znovu nejdřív po této době (žádné opakování v těsné smyčce).
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// Host IP publikovaného portu, pro kterou má relay smysl (port je na Windows na 127.0.0.1).
pub fn relayable_ip(ip: &str) -> bool {
    matches!(ip.trim(), "" | "0.0.0.0" | "127.0.0.1")
}

/// Publikované TCP porty kontejneru, pro které se relay na `::1` hodí (seřazené, bez duplicit).
pub fn container_ports(c: &Container) -> Vec<u16> {
    let set: BTreeSet<u16> = c
        .ports
        .iter()
        .flatten()
        .filter(|p| p.typ.eq_ignore_ascii_case("tcp") && relayable_ip(p.ip.as_deref().unwrap_or("")))
        .filter_map(|p| p.public_port)
        .filter(|&p| p != 0)
        .collect();
    set.into_iter().collect()
}

/// Požadovaná sada portů: publikované TCP porty běžících kontejnerů.
pub fn desired_ports(containers: &[Container]) -> BTreeSet<u16> {
    containers.iter().filter(|c| c.running()).flat_map(container_ports).collect()
}

/// Rozdíl požadované a aktivní sady: (přidat, odebrat).
pub fn diff(desired: &BTreeSet<u16>, active: &BTreeSet<u16>) -> (Vec<u16>, Vec<u16>) {
    (desired.difference(active).copied().collect(), active.difference(desired).copied().collect())
}

/// Je WSL v režimu `networkingMode=mirrored` (podle textu `.wslconfig`)?
pub fn mirrored_in(wslconfig: &str) -> bool {
    crate::wslconfig::get(wslconfig, "wsl2", "networkingMode").is_some_and(|v| v.trim().eq_ignore_ascii_case("mirrored"))
}

// ---------------------------------------------------------------- relay jednoho portu

/// Otevřená spojení (kopie socketů), aby je zastavení relaye mohlo zavřít.
type Conns = Arc<Mutex<HashMap<u64, Vec<TcpStream>>>>;

/// Relay `[::1]:<port>` -> `127.0.0.1:<port>`. Drop ho zastaví: přijímací vlákno skončí
/// (do ~50 ms), naslouchající socket se zavře a otevřená spojení se ukončí.
pub struct Relay {
    stop: Arc<AtomicBool>,
    conns: Conns,
    thread: Option<JoinHandle<()>>,
}

impl Relay {
    /// Začne poslouchat na `[::1]:port` (synchronně, takže obsazený port se vrátí hned jako chyba).
    pub fn start(port: u16) -> io::Result<Relay> {
        let l = TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port)))?;
        l.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let conns: Conns = Arc::default();
        let (st, cn) = (stop.clone(), conns.clone());
        let thread = thread::Builder::new()
            .name(format!("v6relay-{port}"))
            .spawn(move || accept_loop(l, port, st, cn))?;
        Ok(Relay { stop, conns, thread: Some(thread) })
    }

    /// Jen nastaví příznak zastavení (víc relayí se tak zastavuje souběžně, Drop pak jen počká).
    fn signal(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.signal();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // spojení, která se zaregistrují až teď, si příznak zkontrolují sama
        if let Ok(mut m) = self.conns.lock() {
            for s in m.drain().flat_map(|(_, v)| v) {
                let _ = s.shutdown(Shutdown::Both);
            }
        }
    }
}

fn accept_loop(l: TcpListener, port: u16, stop: Arc<AtomicBool>, conns: Conns) {
    let mut next_id = 0u64;
    while !stop.load(Ordering::SeqCst) {
        match l.accept() {
            Ok((client, _)) => {
                next_id += 1;
                let (id, st, cn) = (next_id, stop.clone(), conns.clone());
                let _ = thread::Builder::new().name(format!("v6relay-{port}-conn")).spawn(move || serve(client, port, id, st, cn));
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            // WouldBlock (nic nečeká) i jiné chyby – krátce počkat, žádná těsná smyčka
            Err(_) => thread::sleep(ACCEPT_POLL),
        }
    }
    // `l` se tady zavře – port je po návratu z Drop volný
}

fn serve(client: TcpStream, port: u16, id: u64, stop: Arc<AtomicBool>, conns: Conns) {
    // přijatý socket dědí neblokující režim od naslouchajícího
    let _ = client.set_nonblocking(false);
    let _ = client.set_nodelay(true);
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let up = match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
        Ok(s) => s,
        Err(_) => {
            // cíl nedostupný – klient dostane okamžité zavření místo visení
            let _ = client.shutdown(Shutdown::Both);
            return;
        }
    };
    let _ = up.set_nodelay(true);
    let (Ok(c1), Ok(u1), Ok(c2), Ok(u2)) = (client.try_clone(), up.try_clone(), client.try_clone(), up.try_clone()) else {
        let _ = client.shutdown(Shutdown::Both);
        let _ = up.shutdown(Shutdown::Both);
        return;
    };
    if let Ok(mut m) = conns.lock() {
        m.insert(id, vec![c1, u1]);
    }
    if stop.load(Ordering::SeqCst) {
        let _ = client.shutdown(Shutdown::Both);
        let _ = up.shutdown(Shutdown::Both);
    }
    // klient -> cíl ve vlastním vlákně, cíl -> klient v tomto; konec jednoho směru se
    // předá druhé straně jako polovina zavření (FIN), ať protokoly typu HTTP/1.0 doběhnou
    let t = thread::spawn(move || {
        let _ = crate::npipe::pump(&c2, &u2);
        let _ = u2.shutdown(Shutdown::Write);
    });
    let _ = crate::npipe::pump(&up, &client);
    let _ = client.shutdown(Shutdown::Write);
    let _ = t.join();
    if let Ok(mut m) = conns.lock() {
        m.remove(&id);
    }
}

// ---------------------------------------------------------------- správa sady relayí

/// Proč relaye (ne)běží.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Mode {
    /// zatím nesynchronizováno
    #[default]
    Idle,
    /// vypnuto v nastavení
    Disabled,
    /// WSL v režimu mirrored – `::1` funguje bez relaye
    Mirrored,
    /// relaye běží pro publikované porty
    Active,
}

/// Stav pro UI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub mode: Mode,
    /// porty, na kterých relay běží
    pub relayed: Vec<u16>,
    /// porty, kde `[::1]:<port>` nešel otevřít (port, chyba)
    pub conflicts: Vec<(u16, String)>,
}

/// Dostupnost publikovaného portu přes `localhost`.
#[derive(Debug, Clone, PartialEq)]
pub enum Reach {
    /// `localhost` funguje (relay nebo režim mirrored); text pro tooltip
    Localhost(String),
    /// funguje jen `127.0.0.1`; text pro tooltip
    Ipv4Only(String),
}

impl Status {
    /// Dostupnost portu přes `localhost`; None = nevíme (relay se neřeší, port neběží…).
    pub fn reach(&self, port: u16) -> Option<Reach> {
        match self.mode {
            Mode::Idle => None,
            Mode::Mirrored => Some(Reach::Localhost(tf!(
                "localhost:{port} funguje (WSL v režimu mirrored)",
                "localhost:{port} works (WSL in mirrored mode)"
            ))),
            Mode::Disabled => Some(Reach::Ipv4Only(tf!(
                "Spolehlivě jen 127.0.0.1:{port} – localhost (::1) může čekat do vypršení limitu (relay na ::1 je vypnutý v Nastavení)",
                "Reliable only on 127.0.0.1:{port} – localhost (::1) may hang until a timeout (the ::1 relay is off in Settings)"
            ))),
            Mode::Active => {
                if self.relayed.contains(&port) {
                    Some(Reach::Localhost(tf!(
                        "localhost:{port} i 127.0.0.1:{port} fungují (aplikace přeposílá [::1]:{port})",
                        "localhost:{port} and 127.0.0.1:{port} both work (the app relays [::1]:{port})"
                    )))
                } else {
                    self.conflicts.iter().find(|(p, _)| *p == port).map(|(_, e)| {
                        Reach::Ipv4Only(tf!(
                            "Spolehlivě jen 127.0.0.1:{port} – [::1]:{port} je obsazený jiným programem ({e})",
                            "Reliable only on 127.0.0.1:{port} – [::1]:{port} is taken by another program ({e})"
                        ))
                    })
                }
            }
        }
    }
}

#[derive(Default)]
struct Inner {
    relays: BTreeMap<u16, Relay>,
    /// neúspěšné pokusy: port -> (chyba, kdy)
    failed: HashMap<u16, (String, Instant)>,
}

/// Sada relayí srovnávaná s požadovanými porty.
#[derive(Default)]
pub struct Manager {
    inner: Mutex<Inner>,
    /// stav pro UI – oddělený zámek, čtení z UI vlákna neblokuje na start / zastavení relayí
    status: Mutex<Status>,
}

impl Manager {
    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Srovná běžící relaye s požadovanou sadou portů.
    pub fn sync(&self, enabled: bool, mirrored: bool, desired: &BTreeSet<u16>) {
        let Ok(mut g) = self.inner.lock() else { return };
        let empty = BTreeSet::new();
        let (mode, want) = if !enabled {
            (Mode::Disabled, &empty)
        } else if mirrored {
            (Mode::Mirrored, &empty)
        } else {
            (Mode::Active, desired)
        };
        let active: BTreeSet<u16> = g.relays.keys().copied().collect();
        let (add, remove) = diff(want, &active);
        // zastavit souběžně: nejdřív příznaky, pak počkat
        let stopped: Vec<Relay> = remove.iter().filter_map(|p| g.relays.remove(p)).collect();
        stopped.iter().for_each(Relay::signal);
        drop(stopped);
        g.failed.retain(|p, _| want.contains(p));
        for p in add {
            if g.failed.get(&p).is_some_and(|(_, at)| at.elapsed() < RETRY_AFTER) {
                continue;
            }
            match Relay::start(p) {
                Ok(r) => {
                    g.relays.insert(p, r);
                    g.failed.remove(&p);
                }
                Err(e) => {
                    g.failed.insert(p, (e.to_string(), Instant::now()));
                }
            }
        }
        let mut conflicts: Vec<(u16, String)> = g.failed.iter().map(|(p, (e, _))| (*p, e.clone())).collect();
        conflicts.sort();
        let st = Status { mode, relayed: g.relays.keys().copied().collect(), conflicts };
        drop(g);
        if let Ok(mut s) = self.status.lock() {
            *s = st;
        }
    }

    /// Zastaví všechny relaye (odpojený engine, konec aplikace).
    pub fn stop_all(&self) {
        let Ok(mut g) = self.inner.lock() else { return };
        let all = std::mem::take(&mut g.relays);
        all.values().for_each(Relay::signal);
        drop(all);
        g.failed.clear();
        drop(g);
        if let Ok(mut s) = self.status.lock() {
            s.relayed.clear();
            s.conflicts.clear();
        }
    }
}

static APP: LazyLock<Manager> = LazyLock::new(Manager::default);

/// Relaye aplikace.
pub fn app() -> &'static Manager {
    &APP
}

/// Srovná relaye aplikace se snímkem stavu (volá worker při každém obnovení).
pub fn sync_app(snap: &crate::docker::Snapshot, enabled: bool) {
    let desired = if snap.connected { desired_ports(&snap.containers) } else { BTreeSet::new() };
    let mirrored = enabled && mirrored_in(&crate::wslconfig::read());
    app().sync(enabled, mirrored, &desired);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docker::Port;
    use std::io::{Read, Write};

    fn port(ip: Option<&str>, public: Option<u16>, typ: &str) -> Port {
        Port { ip: ip.map(String::from), private_port: 80, public_port: public, typ: typ.into() }
    }

    fn container(state: &str, ports: Vec<Port>) -> Container {
        Container { id: "x".into(), state: state.into(), ports: Some(ports), ..Default::default() }
    }

    #[test]
    fn desired_filters_ip_proto_and_dedupes() {
        let cs = vec![
            container(
                "running",
                vec![
                    port(Some("0.0.0.0"), Some(8080), "tcp"),
                    port(Some("::"), Some(8080), "tcp"), // IPv6 duplicita
                    port(Some(""), Some(8081), "tcp"),
                    port(None, Some(8082), "tcp"),
                    port(Some("127.0.0.1"), Some(5432), "tcp"),
                    port(Some("192.168.1.5"), Some(9000), "tcp"), // konkrétní IP – ne
                    port(Some("0.0.0.0"), Some(5353), "udp"),     // UDP – ne
                    port(None, None, "tcp"),                      // nepublikovaný
                    port(Some("0.0.0.0"), Some(0), "tcp"),
                ],
            ),
            container("running", vec![port(Some("0.0.0.0"), Some(8080), "tcp")]),
            container("exited", vec![port(Some("0.0.0.0"), Some(7000), "tcp")]),
        ];
        let d = desired_ports(&cs);
        assert_eq!(d.into_iter().collect::<Vec<_>>(), vec![5432, 8080, 8081, 8082]);
        assert_eq!(container_ports(&cs[2]), vec![7000]);
    }

    #[test]
    fn diff_adds_and_removes() {
        let want: BTreeSet<u16> = [1, 2, 3].into();
        let have: BTreeSet<u16> = [2, 3, 4, 5].into();
        assert_eq!(diff(&want, &have), (vec![1], vec![4, 5]));
        assert_eq!(diff(&want, &want), (vec![], vec![]));
        assert_eq!(diff(&BTreeSet::new(), &have), (vec![], vec![2, 3, 4, 5]));
    }

    #[test]
    fn mirrored_detection() {
        assert!(mirrored_in("[wsl2]\r\nnetworkingMode=Mirrored\r\n"));
        assert!(!mirrored_in("[wsl2]\nnetworkingMode=nat\n"));
        assert!(!mirrored_in("[experimental]\nnetworkingMode=mirrored\n"));
        assert!(!mirrored_in(""));
    }

    #[test]
    fn reach_by_mode() {
        let mut s = Status { mode: Mode::Active, relayed: vec![80], conflicts: vec![(81, "x".into())] };
        assert!(matches!(s.reach(80), Some(Reach::Localhost(_))));
        assert!(matches!(s.reach(81), Some(Reach::Ipv4Only(_))));
        assert_eq!(s.reach(82), None);
        s.mode = Mode::Disabled;
        assert!(matches!(s.reach(80), Some(Reach::Ipv4Only(_))));
        s.mode = Mode::Mirrored;
        assert!(matches!(s.reach(81), Some(Reach::Localhost(_))));
        s.mode = Mode::Idle;
        assert_eq!(s.reach(80), None);
    }

    /// Echo server na 127.0.0.1:<náhodný port>; vrací port.
    fn echo_server() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let p = l.local_addr().unwrap().port();
        thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { return };
                thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = s.read(&mut buf) {
                        if n == 0 || s.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        p
    }

    fn v6(port: u16) -> SocketAddr {
        SocketAddr::from((Ipv6Addr::LOCALHOST, port))
    }

    /// `[::1]:port` spojení odmítne. Windows po RST zkouší SYN znovu (odmítnutí trvá ~2 s),
    /// proto delší limit.
    fn assert_refused(port: u16) {
        let t = Instant::now();
        let e = TcpStream::connect_timeout(&v6(port), Duration::from_secs(6)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::ConnectionRefused, "{e} po {:?}", t.elapsed());
    }

    fn echo_via(port: u16, msg: &[u8]) -> Vec<u8> {
        let mut s = TcpStream::connect_timeout(&v6(port), Duration::from_secs(2)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        s.write_all(msg).unwrap();
        let mut got = vec![0u8; msg.len()];
        s.read_exact(&mut got).unwrap();
        got
    }

    #[test]
    fn relay_roundtrip_and_stop() {
        let p = echo_server();
        let relay = Relay::start(p).unwrap();
        // obsazený [::1]:p -> chyba hned
        assert!(Relay::start(p).is_err());
        assert_eq!(echo_via(p, b"hello"), b"hello");
        // otevřené spojení se při zastavení ukončí
        let mut open = TcpStream::connect(v6(p)).unwrap();
        open.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        open.write_all(b"x").unwrap();
        let mut b = [0u8; 1];
        open.read_exact(&mut b).unwrap();
        drop(relay);
        assert_eq!(open.read(&mut b).unwrap_or(0), 0, "spojení mělo skončit");
        assert_refused(p);
    }

    #[test]
    fn manager_sync_and_conflicts() {
        let p = echo_server();
        // cizí program drží [::1] na druhém portu
        let other = echo_server();
        let _taken = TcpListener::bind(v6(other)).unwrap();
        let m = Manager::default();
        let want: BTreeSet<u16> = [p, other].into();
        m.sync(true, false, &want);
        let st = m.status();
        assert_eq!(st.mode, Mode::Active);
        assert_eq!(st.relayed, vec![p]);
        assert_eq!(st.conflicts.len(), 1);
        assert_eq!(st.conflicts[0].0, other);
        assert_eq!(echo_via(p, b"abc"), b"abc");
        // konflikt se hned znovu nezkouší
        m.sync(true, false, &want);
        assert_eq!(m.status().conflicts.len(), 1);
        // mirrored -> nic neběží
        m.sync(true, true, &want);
        assert_eq!(m.status().mode, Mode::Mirrored);
        assert!(m.status().relayed.is_empty() && m.status().conflicts.is_empty());
        assert!(TcpStream::connect_timeout(&v6(p), Duration::from_secs(2)).is_err());
        // zpět, pak odebrání portu
        m.sync(true, false, &[p].into());
        assert_eq!(m.status().relayed, vec![p]);
        m.sync(true, false, &BTreeSet::new());
        assert!(m.status().relayed.is_empty());
        m.sync(true, false, &[p].into());
        m.sync(false, false, &[p].into());
        assert_eq!(m.status().mode, Mode::Disabled);
        assert!(m.status().relayed.is_empty());
        m.sync(true, false, &[p].into());
        m.stop_all();
        assert!(m.status().relayed.is_empty());
        assert!(TcpStream::connect_timeout(&v6(p), Duration::from_secs(2)).is_err());
    }

    /// Živý test proti enginu z konfigurace aplikace (Podman v distribuci `Debian`):
    /// `cargo test live_ipv6_localhost_relay -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_ipv6_localhost_relay() {
        use crate::docker::Docker;
        const NAME: &str = "wslcd-test-v6";
        const P: u16 = 18290;
        let cfg = crate::config::Config::load();
        crate::docker::configure_tls(cfg.tls).unwrap();
        let d = Docker::new(cfg.port);
        assert!(d.ping(), "API na {} neodpovídá", cfg.port);
        let podman = |args: &[&str]| {
            let mut a = vec!["podman", "--url", "unix:///run/wslcd-podman/podman.sock"];
            a.extend_from_slice(args);
            crate::wsl::run(&cfg.distro, Some("root"), None, &a)
        };
        let _ = podman(&["rm", "-f", NAME]);
        podman(&["run", "-d", "--name", NAME, "-p", &format!("{P}:80"), "docker.io/library/nginx:alpine"]).expect("podman run");
        let m = Manager::default();
        let snap = d.snapshot();
        let desired = desired_ports(&snap.containers);
        eprintln!("požadované porty: {desired:?}");
        assert!(desired.contains(&P));
        // jen náš port – porty cizích kontejnerů v testu neobsazujeme
        m.sync(true, false, &[P].into());
        eprintln!("stav: {:?}", m.status());
        assert_eq!(m.status().relayed, vec![P]);
        let get = || -> Result<String, String> {
            let t = Instant::now();
            let mut s = TcpStream::connect_timeout(&v6(P), Duration::from_secs(2)).map_err(|e| e.to_string())?;
            s.set_read_timeout(Some(Duration::from_secs(2))).map_err(|e| e.to_string())?;
            s.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").map_err(|e| e.to_string())?;
            let mut out = String::new();
            s.read_to_string(&mut out).map_err(|e| e.to_string())?;
            let line = out.lines().next().unwrap_or("").to_string();
            eprintln!("http://[::1]:{P}/ -> {line} ({} ms)", t.elapsed().as_millis());
            Ok(line)
        };
        // nginx může chvíli startovat
        let mut line = String::new();
        for _ in 0..20 {
            match get() {
                Ok(l) if l.contains(" 200") => {
                    line = l;
                    break;
                }
                r => eprintln!("zatím: {r:?}"),
            }
            thread::sleep(Duration::from_millis(250));
        }
        // podruhé (nginx už běží) musí odpověď přijít hned
        let t = Instant::now();
        let again = get();
        let fast = t.elapsed() < Duration::from_millis(500);
        let _ = podman(&["rm", "-f", NAME]);
        assert!(line.contains(" 200"), "odpověď: {line}");
        assert!(again.as_deref().is_ok_and(|l| l.contains(" 200")) && fast, "{again:?} za {:?}", t.elapsed());
        let snap = d.snapshot();
        let desired = desired_ports(&snap.containers);
        assert!(!desired.contains(&P));
        m.sync(true, false, &desired.intersection(&[P].into()).copied().collect());
        eprintln!("po odebrání: {:?}", m.status());
        assert!(m.status().relayed.is_empty());
        assert_refused(P);
        eprintln!("[::1]:{P} po odebrání: spojení odmítnuto");
    }
}
