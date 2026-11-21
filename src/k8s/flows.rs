//! Mapa toků: spojení z `conntrack -L` v síťovém namespace uzlu k3s složená do hran mezi
//! komponentami, službami, localhostem (NodePort z Windows) a cíli mimo cluster.
//!
//! Conntrack drží i nedávno ukončená spojení (TIME_WAIT ~2 min), takže mapa vidí i krátké
//! HTTP požadavky, které `ss` v okamžiku měření nezachytí (viz spike).

use super::model::{Pod, Service};
use std::collections::{BTreeMap, HashMap};

/// Jedno spojení z conntracku: původní směr a adresa, ze které přišla odpověď
/// (u ClusterIP služby = skutečný pod po DNAT).
#[derive(Debug, Clone, PartialEq)]
pub struct CtEntry {
    pub proto: String,
    /// stav TCP (ESTABLISHED, TIME_WAIT…); UDP nemá
    pub state: Option<String>,
    pub src: String,
    pub dst: String,
    pub dport: u16,
    pub reply_src: String,
}

impl CtEntry {
    /// Spojení ještě trvá (ne TIME_WAIT / CLOSE…).
    pub fn live(&self) -> bool {
        match self.state.as_deref() {
            None => true,
            Some(s) => matches!(s, "ESTABLISHED" | "SYN_SENT" | "SYN_RECV"),
        }
    }
}

const STATES: [&str; 10] = ["ESTABLISHED", "SYN_SENT", "SYN_RECV", "FIN_WAIT", "CLOSE_WAIT", "LAST_ACK", "TIME_WAIT", "CLOSE", "LISTEN", "NONE"];

/// Řádky `conntrack -L` (i s volbou `-o extended`). Nečitelné řádky se přeskočí.
pub fn parse_conntrack(text: &str) -> Vec<CtEntry> {
    text.lines()
        .filter_map(|l| {
            let toks: Vec<&str> = l.split_whitespace().collect();
            let proto = toks.iter().find(|t| matches!(**t, "tcp" | "udp"))?.to_string();
            let state = toks.iter().find(|t| STATES.contains(t)).map(|s| s.to_string());
            let vals = |key: &str| toks.iter().filter_map(move |t| t.strip_prefix(key)).collect::<Vec<_>>();
            let src = vals("src=");
            let dst = vals("dst=");
            let dport = vals("dport=");
            Some(CtEntry {
                proto,
                state,
                src: src.first()?.to_string(),
                dst: dst.first()?.to_string(),
                dport: dport.first()?.parse().ok()?,
                reply_src: src.get(1)?.to_string(),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Node {
    /// komponenta podů (label `app`, …)
    Component(String),
    /// služba (ClusterIP / NodePort)
    Service(String),
    /// přístup z Windows přes NodePort
    Localhost,
    /// DNS clusteru (kube-dns)
    Dns,
    /// adresa mimo namespace / cluster
    Outside(String),
}

impl Node {
    pub fn label(&self) -> String {
        match self {
            Node::Component(c) => c.clone(),
            Node::Service(s) => format!("svc/{s}"),
            Node::Localhost => "localhost (Windows)".into(),
            Node::Dns => "DNS".into(),
            Node::Outside(ip) => ip.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    pub from: Node,
    pub to: Node,
    pub port: u16,
    /// živá spojení
    pub live: u32,
    /// nedávno ukončená (TIME_WAIT…)
    pub recent: u32,
}

/// Složí hrany pro jeden namespace. `node_ips` = adresy uzlu (cíl NodePort provozu),
/// `dns_ip` = ClusterIP kube-dns.
pub fn build_edges(pods: &[Pod], svcs: &[Service], entries: &[CtEntry], node_ips: &[String], dns_ip: Option<&str>) -> Vec<Edge> {
    let by_ip: HashMap<&str, &Pod> = pods.iter().filter_map(|p| Some((p.ip.as_deref()?, p))).collect();
    let by_cip: HashMap<&str, &Service> = svcs.iter().filter_map(|s| Some((s.cluster_ip.as_deref()?, s))).collect();
    let by_nodeport: HashMap<u16, &Service> = svcs.iter().flat_map(|s| s.ports.iter().filter_map(move |p| Some((p.node_port?, s)))).collect();
    let mut acc: BTreeMap<(Node, Node, u16), (u32, u32)> = BTreeMap::new();
    let mut add = |from: Node, to: Node, port: u16, live: bool| {
        if from == to {
            return;
        }
        let e = acc.entry((from, to, port)).or_default();
        if live { e.0 += 1 } else { e.1 += 1 }
    };
    for e in entries {
        let live = e.live();
        let backend = by_ip.get(e.reply_src.as_str()).map(|p| Node::Component(p.component.clone()));
        // NodePort z Windows: cíl = uzel a port patří službě v namespace
        if node_ips.contains(&e.dst) && !by_ip.contains_key(e.src.as_str()) {
            if let (Some(svc), Some(b)) = (by_nodeport.get(&e.dport), backend.clone()) {
                add(Node::Localhost, Node::Service(svc.name.clone()), e.dport, live);
                add(Node::Service(svc.name.clone()), b, svc.ports.iter().find(|p| p.node_port == Some(e.dport)).map(|p| p.port).unwrap_or(e.dport), live);
            }
            continue;
        }
        let Some(src) = by_ip.get(e.src.as_str()) else { continue };
        let from = Node::Component(src.component.clone());
        if let Some(svc) = by_cip.get(e.dst.as_str()) {
            add(from, Node::Service(svc.name.clone()), e.dport, live);
            if let Some(b) = backend {
                add(Node::Service(svc.name.clone()), b, e.dport, live);
            }
        } else if let Some(dst) = by_ip.get(e.dst.as_str()) {
            add(from, Node::Component(dst.component.clone()), e.dport, live);
        } else if dns_ip == Some(e.dst.as_str()) || e.dport == 53 {
            add(from, Node::Dns, 53, live);
        } else {
            add(from, Node::Outside(e.dst.clone()), e.dport, live);
        }
    }
    acc.into_iter().map(|((from, to, port), (live, recent))| Edge { from, to, port, live, recent }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::k8s::model::SvcPort;

    fn pod(name: &str, comp: &str, ip: &str) -> Pod {
        Pod { name: name.into(), component: comp.into(), ip: Some(ip.into()), ..Default::default() }
    }

    fn svc(name: &str, cip: &str, port: u16, np: Option<u16>) -> Service {
        Service { name: name.into(), cluster_ip: Some(cip.into()), ports: vec![SvcPort { port, node_port: np, proto: "TCP".into(), name: None }], ..Default::default() }
    }

    // skutečné řádky ze spiku (k3s v1.33 na Podmanu)
    const CT: &str = "tcp      6 105 TIME_WAIT src=10.42.0.31 dst=10.43.96.24 sport=34568 dport=80 src=10.42.0.30 dst=10.42.0.31 sport=80 dport=34568 [ASSURED] mark=0 use=1
ipv4     2 tcp      6 86396 ESTABLISHED src=10.42.0.31 dst=10.42.0.40 sport=46370 dport=5432 src=10.42.0.40 dst=10.42.0.31 sport=5432 dport=46370 [ASSURED] mark=0 use=1
tcp      6 60 TIME_WAIT src=10.89.0.1 dst=10.89.0.2 sport=50000 dport=30080 src=10.42.0.30 dst=10.42.0.1 sport=80 dport=50000 [ASSURED] mark=0 use=1
udp      17 25 src=10.42.0.31 dst=10.43.0.10 sport=40000 dport=53 src=10.42.0.5 dst=10.42.0.31 sport=53 dport=40000 mark=0 use=1
tcp      6 50 ESTABLISHED src=10.42.0.31 dst=1.2.3.4 sport=1 dport=443 src=1.2.3.4 dst=10.89.0.2 sport=443 dport=1 [ASSURED] mark=0 use=1
tcp      6 86398 ESTABLISHED src=10.42.0.1 dst=10.42.0.11 sport=46370 dport=10250 src=10.42.0.11 dst=10.42.0.1 sport=10250 dport=46370 [ASSURED] mark=0 use=1
garbage line";

    fn fixture() -> (Vec<Pod>, Vec<Service>) {
        (vec![pod("fe-1", "fe", "10.42.0.30"), pod("api-1", "api", "10.42.0.31"), pod("db-0", "db", "10.42.0.40")], vec![svc("fe", "10.43.96.24", 80, Some(30080))])
    }

    #[test]
    fn parses_conntrack_lines() {
        let e = parse_conntrack(CT);
        assert_eq!(e.len(), 6, "nečitelný řádek se přeskočí");
        assert_eq!(e[0].state.as_deref(), Some("TIME_WAIT"));
        assert_eq!((e[0].src.as_str(), e[0].dst.as_str(), e[0].dport, e[0].reply_src.as_str()), ("10.42.0.31", "10.43.96.24", 80, "10.42.0.30"));
        assert!(!e[0].live());
        assert!(e[1].live(), "extended formát (ipv4 2 tcp …)");
        assert_eq!(e[3].proto, "udp");
        assert!(e[3].live());
    }

    #[test]
    fn edges_via_service_pod_dns_outside_and_localhost() {
        let (pods, svcs) = fixture();
        let edges = build_edges(&pods, &svcs, &parse_conntrack(CT), &["10.89.0.2".into()], Some("10.43.0.10"));
        let has = |f: Node, t: Node, port: u16| edges.iter().find(|e| e.from == f && e.to == t && e.port == port).cloned();
        let c = |s: &str| Node::Component(s.into());
        let api_fe = has(c("api"), Node::Service("fe".into()), 80).expect("api → svc/fe");
        assert_eq!((api_fe.live, api_fe.recent), (0, 1));
        assert!(has(Node::Service("fe".into()), c("fe"), 80).is_some(), "svc/fe → pod za DNAT");
        assert_eq!(has(c("api"), c("db"), 5432).unwrap().live, 1);
        assert!(has(c("api"), Node::Dns, 53).is_some());
        assert!(has(c("api"), Node::Outside("1.2.3.4".into()), 443).is_some());
        assert!(has(Node::Localhost, Node::Service("fe".into()), 30080).is_some(), "NodePort z Windows");
        // kubelet (10.42.0.1) a cizí pody (10.42.0.11) se nekreslí
        assert!(edges.iter().all(|e| !matches!(&e.to, Node::Outside(ip) if ip.starts_with("10.42."))));
        assert_eq!(edges.len(), 6);
    }

    #[test]
    fn missing_pod_ip_is_skipped() {
        // pod se restartuje a ještě nemá IP – jeho spojení se přeskočí, ostatní zůstanou
        let (mut pods, svcs) = fixture();
        pods[2].ip = None;
        let edges = build_edges(&pods, &svcs, &parse_conntrack(CT), &[], None);
        assert!(edges.iter().any(|e| e.from == Node::Component("api".into())));
        assert!(edges.iter().all(|e| e.to != Node::Component("db".into())));
    }
}
