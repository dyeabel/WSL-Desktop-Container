//! Živý test síťového přehledu proti enginu z konfigurace aplikace:
//! `cargo test live_net -- --ignored --nocapture`.

use super::*;
use crate::netinfo::{compare_ports, published_of, WinState};

const WEB: &str = "wls-test-net-web";
const CLIENT: &str = "wls-test-net-client";

struct Cleanup<F: Fn(&[&str]) -> Result<String, String>>(F);

impl<F: Fn(&[&str]) -> Result<String, String>> Drop for Cleanup<F> {
    fn drop(&mut self) {
        let _ = (self.0)(&["rm", "-f", "-t", "1", WEB, CLIENT]);
    }
}

#[test]
#[ignore]
fn live_net_ports_in_out() {
    let cfg = Config::load();
    crate::docker::configure_tls(cfg.tls).expect("TLS klient");
    let d = Docker::new(cfg.port);
    assert!(d.ping(), "engine API na portu {} neodpovídá", cfg.port);
    let cli = cfg.engine.cli(cfg.port, cfg.tls);
    let distro = cfg.distro.clone();
    let guard = Cleanup(move |args: &[&str]| {
        let mut v = cli.clone();
        v.extend(args.iter().map(|s| s.to_string()));
        wsl::run_owned(&distro, None, None, &v).map_err(String::from)
    });
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
    let ip = web
        .pointer("/NetworkSettings/Networks")
        .and_then(|n| n.as_object())
        .and_then(|n| n.values().find_map(|v| v.get("IPAddress").and_then(|i| i.as_str()).filter(|i| !i.is_empty())))
        .expect("IP webu")
        .to_string();
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
    let dump = collect_ns(&cfg.distro, &[(web_id.clone(), pw), (client_id.clone(), pc)], true).expect("nsenter ss");
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
    let out = wsl::run(&cfg.distro, Some("root"), None, &["sh", "-c", OVERVIEW_SCRIPT]).expect("overview script");
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
        state = netinfo::windows_state(&pub18190, &win, std::process::id());
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
    for t in od.containers.iter().filter(|t| t.name.starts_with("wls-test-net")) {
        eprintln!("traffic {} rate {:?} total rx {} tx {}", t.name, t.rate.map(|(a, b)| (netinfo::fmt_rate(a), netinfo::fmt_rate(b))), t.rx_total, t.tx_total);
    }
    assert!(od.vm_rate.is_some() && !od.history.is_empty());
    assert!(od.containers.iter().any(|t| t.name == WEB && t.rate.is_some()));
    drop((ns, ov));
    drop(guard);
    let left: Vec<Container> = d.get("/containers/json?all=true").unwrap();
    assert!(!left.iter().any(|x| x.name().starts_with("wls-test-net")), "úklid");
}
