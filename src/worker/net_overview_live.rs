//! Živý test síťového přehledu proti enginu z konfigurace aplikace:
//! `cargo test live_net -- --ignored --nocapture`.

use super::*;
use crate::netinfo::{compare_ports, published_of, WinState};

const WEB: &str = "wslcd-test-net-web";
const CLIENT: &str = "wslcd-test-net-client";

/// Úklid testovacích kontejnerů (`1` = jejich názvy) i při selhání testu.
struct Cleanup<F: Fn(&[&str]) -> Result<String, String>>(F, [&'static str; 2]);

impl<F: Fn(&[&str]) -> Result<String, String>> Drop for Cleanup<F> {
    fn drop(&mut self) {
        let _ = (self.0)(&["rm", "-f", "-t", "1", self.1[0], self.1[1]]);
    }
}

/// CLI enginu v distribuci jako funkce (pro `Cleanup`).
fn engine_cli(cfg: &Config) -> impl Fn(&[&str]) -> Result<String, String> + use<> {
    let host = crate::host::current(cfg);
    let cli = cfg.engine.cli_for(&host.engine_endpoint(cfg), cfg.tls);
    move |args: &[&str]| {
        let mut v = cli.clone();
        v.extend(args.iter().map(|s| s.to_string()));
        crate::host::HostExt::run_owned(&*host, crate::host::Privilege::User, None, &v).map_err(String::from)
    }
}

/// IPv4 adresa kontejneru z inspect.
fn container_ip(v: &Value) -> String {
    v.pointer("/NetworkSettings/Networks")
        .and_then(|n| n.as_object())
        .and_then(|n| n.values().find_map(|v| v.get("IPAddress").and_then(|i| i.as_str()).filter(|i| !i.is_empty())))
        .expect("IP kontejneru")
        .to_string()
}

const DL_WEB: &str = "wslcd-test-net-dl-web";
const DL_CLIENT: &str = "wslcd-test-net-dl-client";

/// Provoz po spojeních, historie ukončených spojení, graf provozu a konflikty portů
/// s ostatními distribucemi: `cargo test live_net_conn -- --ignored --nocapture`.
#[test]
#[ignore]
fn live_net_conn_traffic() {
    let cfg = Config::load();
    crate::docker::configure_tls(cfg.tls).expect("TLS klient");
    let d = Docker::new(&crate::host::endpoint(&cfg));
    assert!(d.ping(), "engine API na portu {} neodpovídá", cfg.port);
    let guard = Cleanup(engine_cli(&cfg), [DL_WEB, DL_CLIENT]);
    let engine = &guard.0;
    let _ = engine(&["rm", "-f", DL_WEB, DL_CLIENT]);

    // server: nginx s 64 MB souborem a omezením 512 KB/s na spojení (spojení vydrží ~2 min)
    engine(&[
        "run", "-d", "--name", DL_WEB, "docker.io/library/nginx:alpine", "sh", "-c",
        "dd if=/dev/zero of=/usr/share/nginx/html/big bs=1M count=64 2>/dev/null; \
         printf 'server { listen 80; listen [::]:80; root /usr/share/nginx/html; limit_rate 512k; }' > /etc/nginx/conf.d/default.conf; \
         exec nginx -g 'daemon off;'",
    ])
    .expect("run web");
    let web: Value = d.get(&format!("/containers/{DL_WEB}/json")).expect("inspect web");
    let web_id = web["Id"].as_str().unwrap().to_string();
    let web_ip = container_ip(&web);
    // klient: wget ve smyčce do /dev/null (IP jako argument, ne v textu skriptu)
    engine(&[
        "run", "-d", "--name", DL_CLIENT, "docker.io/library/nginx:alpine", "sh", "-c",
        "sleep 1; while true; do wget -q -O /dev/null \"http://$1/big\"; sleep 1; done", "sh", &web_ip,
    ])
    .expect("run client");
    let client: Value = d.get(&format!("/containers/{DL_CLIENT}/json")).expect("inspect client");
    let client_ip = container_ip(&client);
    eprintln!("web {web_ip}, client {client_ip}");

    // stejná cesta jako záložka Síť v detailu kontejneru
    let shared = Arc::new(RwLock::new(cfg.clone()));
    let ns = start_ns_feed(eframe::egui::Context::default(), shared, Some(web_id.clone()));
    let find = |nd: &NsData| nd.live.iter().find(|c| c.sock.peer_ip == client_ip && c.sock.local_port == 80).cloned();
    let mut samples: Vec<netinfo::Conn> = Vec::new();
    for _ in 0..30 {
        ns.touch();
        thread::sleep(Duration::from_millis(500));
        let nd = ns.data.lock().unwrap().clone();
        if let Some(c) = find(&nd) {
            if samples.last().is_none_or(|l| l.last_seen < c.last_seen) {
                eprintln!(
                    "t={:.1}s {} {} <- {} in {:?} B out {:?} B rate {:?} (since {:.1}s)",
                    nd.now,
                    if c.inbound { "in " } else { "out" },
                    c.sock.local(),
                    c.sock.peer(),
                    c.sock.bytes_in,
                    c.sock.bytes_out,
                    c.rate.map(|(a, b)| (netinfo::fmt_rate(a), netinfo::fmt_rate(b))),
                    c.first_seen
                );
                samples.push(c);
            }
        }
        if samples.len() >= 3 {
            break;
        }
    }
    assert!(samples.len() >= 3, "spojení klienta se na serveru neobjevilo dost krát");
    let (a, b) = (&samples[samples.len() - 2], &samples[samples.len() - 1]);
    assert!(b.inbound, "spojení na port 80 je příchozí");
    assert!(b.sock.bytes_out.unwrap() > a.sock.bytes_out.unwrap(), "odeslané bajty rostou");
    let (_, out_rate) = b.rate.expect("rychlost");
    assert!(out_rate > 0.0, "rychlost odesílání > 0");
    let nd = ns.data.lock().unwrap().clone();
    eprintln!("provoz kontejneru: {} vzorků, poslední {:?}", nd.traffic.len(), nd.traffic.last());
    assert!(nd.traffic.iter().any(|s| s.2 > 0.0), "graf provozu: odesláno > 0");

    // klient skončí -> spojení do historie
    engine(&["rm", "-f", "-t", "0", DL_CLIENT]).expect("rm client");
    let mut closed = None;
    for _ in 0..20 {
        ns.touch();
        thread::sleep(Duration::from_millis(500));
        let nd = ns.data.lock().unwrap().clone();
        closed = nd.closed.iter().find(|c| c.sock.peer_ip == client_ip && c.sock.local_port == 80).cloned();
        if closed.is_some() {
            assert!(find(&nd).is_none(), "ukončené spojení už není mezi živými");
            break;
        }
    }
    let c = closed.expect("ukončené spojení v historii");
    eprintln!(
        "historie: {} <- {} od {:.1}s do {:.1}s, odesláno {:?} B, přijato {:?} B",
        c.sock.local(),
        c.sock.peer(),
        c.first_seen,
        c.last_seen,
        c.sock.bytes_out,
        c.sock.bytes_in
    );
    assert!(c.sock.bytes_out.unwrap() >= b.sock.bytes_out.unwrap());
    drop(ns);

    // ostatní distribuce WSL: skutečné běžící distribuce, nepoužitý port bez konfliktu
    let t0 = Instant::now();
    let scan = scan_distros(&*crate::host::current(&cfg), &cfg.distro);
    eprintln!(
        "distribuce: engine netns {:?}, zkontrolováno {:?}, přeskočeno {:?}, chyba {:?} ({} ms)",
        scan.engine_netns,
        scan.others.iter().map(|o| (o.distro.as_str(), o.netns.as_str(), o.listen.len(), o.error.as_deref())).collect::<Vec<_>>(),
        scan.skipped,
        scan.error,
        t0.elapsed().as_millis()
    );
    assert!(scan.error.is_none());
    assert!(scan.engine_netns.starts_with("net:["), "{:?}", scan.engine_netns);
    assert!(!scan.others.iter().any(|o| o.distro.eq_ignore_ascii_case(&cfg.distro)), "distribuce enginu se nekontroluje");
    let unused = netinfo::Published { private: 80, proto: "tcp".into(), host_ip: String::new(), host_port: 18977 };
    assert!(netinfo::distro_conflicts(&unused, &scan.engine_netns, &scan.others).is_empty());
    // a totéž pro skutečně obsazený port enginu: distribuce enginu se nepočítá
    let api = netinfo::Published { private: cfg.port, proto: "tcp".into(), host_ip: String::new(), host_port: cfg.port };
    eprintln!("konflikt API portu {}: {:?}", cfg.port, netinfo::distro_conflicts(&api, &scan.engine_netns, &scan.others));

    drop(guard);
    let left: Vec<Container> = d.get("/containers/json?all=true").unwrap();
    assert!(!left.iter().any(|x| x.name().starts_with("wslcd-test-net-dl")), "úklid");
}

#[test]
#[ignore]
fn live_net_ports_in_out() {
    let cfg = Config::load();
    crate::docker::configure_tls(cfg.tls).expect("TLS klient");
    let d = Docker::new(&crate::host::endpoint(&cfg));
    assert!(d.ping(), "engine API na portu {} neodpovídá", cfg.port);
    let guard = Cleanup(engine_cli(&cfg), [WEB, CLIENT]);
    let engine = &guard.0;
    let _ = engine(&["rm", "-f", WEB, CLIENT]);

    // 1) web: nginx na 80 (publikováno 18190) + nepublikovaný nc na 9999
    engine(&[
        "run", "-d", "--name", WEB, "-p", "18190:80", "docker.io/library/nginx:alpine",
        "sh", "-c", "nc -lk -p 9999 -e cat & exec nginx -g 'daemon off;'",
    ])
    .expect("run web");
    let web: Value = d.get(&format!("/containers/{WEB}/json")).expect("inspect web");
    let web_id = web["Id"].as_str().unwrap().to_string();
    let ip = container_ip(&web);
    eprintln!("web {} IP {ip}", crate::docker::short_id(&web_id));

    // 2) klient s trvale otevřeným spojením na web:9999 (IP jako argument, ne v textu skriptu)
    engine(&[
        "run", "-d", "--name", CLIENT, "docker.io/library/nginx:alpine",
        "sh", "-c", "while true; do sleep 3600 | nc \"$1\" 9999; sleep 1; done", "sh", &ip,
    ])
    .expect("run client");
    let client_id = d.get::<Value>(&format!("/containers/{CLIENT}/json")).expect("inspect client")["Id"].as_str().unwrap().to_string();
    thread::sleep(Duration::from_secs(3));

    // 3) porty dovnitř / ven přes nsenter
    let pw = container_pid(&d, &web_id).expect("pid web");
    let pc = container_pid(&d, &client_id).expect("pid client");
    eprintln!("PID web {pw}, client {pc}");
    assert!(pw > 0 && pc > 0);
    let dump = collect_ns(&*crate::host::current(&cfg), &[(web_id.clone(), pw), (client_id.clone(), pc)], NsMode::Conns).expect("nsenter ss");
    let w = &dump[&web_id];
    let c = &dump[&client_id];
    assert!(w.error.is_none() && c.error.is_none(), "{:?} {:?}", w.error, c.error);
    for s in &w.listen {
        eprintln!("web listen  {} {} {}", s.proto, s.local(), s.process());
    }
    for s in &w.conns {
        eprintln!("web conn    {} {} <- {} {}", s.proto, s.local(), s.peer(), s.process());
    }
    for s in &c.conns {
        eprintln!("client conn {} {} -> {} {}", s.proto, s.local(), s.peer(), s.process());
    }
    let list: Vec<Container> = d.get("/containers/json").unwrap();
    let web_c = list.iter().find(|x| x.id == web_id).expect("web v seznamu");
    let rows = compare_ports(&w.listen, &published_of(web_c.ports.as_deref().unwrap_or(&[])));
    for r in &rows {
        eprintln!("port {}/{}: listen={:?} published={:?} unpublished={} dead={}", r.port, r.proto, r.listen, r.published, r.unpublished(), r.dead());
    }
    let r80 = rows.iter().find(|r| r.port == 80 && r.proto == "tcp").expect("80/tcp");
    assert!(r80.listening() && !r80.published.is_empty() && !r80.unpublished() && !r80.dead());
    assert!(r80.process.contains("nginx"), "{}", r80.process);
    let r9999 = rows.iter().find(|r| r.port == 9999).expect("9999/tcp");
    assert!(r9999.unpublished(), "9999 má být označen jako nepublikovaný");
    assert!(c.conns.iter().any(|s| s.peer_ip == ip && s.peer_port == Some(9999)), "odchozí spojení klienta na {ip}:9999");
    assert!(w.conns.iter().any(|s| s.local_port == 9999), "příchozí spojení na webu");

    // 4) provoz VM, naslouchající sockety v distribuci a ve Windows
    let host = crate::host::current(&cfg);
    let out = crate::host::HostExt::run(&*host, overview_privilege(host.caps()), None, &["sh", "-c", OVERVIEW_SCRIPT]).expect("overview script");
    let mut parts = out.split(netinfo::MARK);
    let eth = netinfo::parse_proc_net_dev(parts.next().unwrap()).into_iter().find(|i| i.name == "eth0").expect("eth0");
    let wsl_ip = netinfo::parse_ip_addr(parts.next().unwrap());
    let listen = netinfo::parse_ss(parts.next().unwrap());
    eprintln!("eth0 rx {} tx {}, IP {wsl_ip:?}", eth.rx, eth.tx);
    assert!(eth.rx > 0 && wsl_ip.is_some());
    for s in &listen {
        eprintln!("distro listen {} {} {} -> {:?}", s.proto, s.local(), s.process(), netinfo::listen_kind(s, cfg.port, &[18190]));
    }
    assert!(listen.iter().any(|s| s.local_port == cfg.port), "API port {} mezi naslouchajícími", cfg.port);
    assert!(listen.iter().any(|s| s.local_port == 18190 && netinfo::listen_kind(s, cfg.port, &[18190]) == netinfo::ListenKind::ContainerProxy));
    let mut names = HashMap::new();
    let pub18190 = published_of(web_c.ports.as_deref().unwrap_or(&[])).into_iter().find(|p| p.host_port == 18190).unwrap();
    let mut state = WinState::NotForwarded;
    for _ in 0..10 {
        let win = windows_listeners(&mut names).expect("Windows porty");
        // běžící aplikace přeposílá IPv6 localhost vlastní relay – v aplikaci je to `own_pid`
        let own = win.iter().find(|x| x.process.eq_ignore_ascii_case("wsl-container-desktop.exe")).map_or(std::process::id(), |x| x.pid);
        state = netinfo::windows_state(&pub18190, &win, own);
        if state != WinState::NotForwarded {
            for x in win.iter().filter(|x| x.port == 18190 || x.port == cfg.port) {
                eprintln!("windows listen {} {} pid {} {}", x.proto, netinfo::join_addr(&x.ip, Some(x.port)), x.pid, x.process);
            }
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }
    eprintln!("Windows stav 18190: {state:?}");
    assert_eq!(state, WinState::Forwarded);

    // 5) feedy stejnou cestou jako UI (vlákna, rychlosti z rozdílů vzorků)
    let shared = Arc::new(RwLock::new(cfg.clone()));
    let ctx = eframe::egui::Context::default();
    let ns = start_ns_feed(ctx.clone(), shared.clone(), Some(web_id.clone()));
    let ov = start_net_overview(ctx, shared);
    for _ in 0..8 {
        ns.touch();
        ov.touch(true);
        thread::sleep(Duration::from_secs(1));
    }
    let nd = ns.data.lock().unwrap().clone();
    assert!(nd.by_id.get(&web_id).is_some_and(|x| x.listen.iter().any(|s| s.local_port == 9999)), "{nd:?}");
    let od = ov.data.lock().unwrap().clone();
    eprintln!("VM rate {:?}, history {} vzorků, mode {:?}", od.vm_rate.map(|(a, b)| (netinfo::fmt_rate(a), netinfo::fmt_rate(b))), od.history.len(), od.wsl_mode);
    for t in od.containers.iter().filter(|t| t.name.starts_with("wslcd-test-net")) {
        eprintln!("traffic {} rate {:?} total rx {} tx {}", t.name, t.rate.map(|(a, b)| (netinfo::fmt_rate(a), netinfo::fmt_rate(b))), t.rx_total, t.tx_total);
    }
    assert!(od.vm_rate.is_some() && !od.history.is_empty());
    assert!(od.containers.iter().any(|t| t.name == WEB && t.rate.is_some()));
    drop((ns, ov));
    drop(guard);
    let left: Vec<Container> = d.get("/containers/json?all=true").unwrap();
    assert!(!left.iter().any(|x| x.name().starts_with("wslcd-test-net")), "úklid");
}
