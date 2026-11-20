//! Pravidla „kdo smí na koho“ → NetworkPolicy. Aplikace spravuje jen politiky s labelem
//! `wslcd.managed=1`; každá nese v anotaci `wslcd.rule` své pravidlo, takže formulář jde
//! z clusteru zase sestavit. Manifesty jsou JSON (kubectl ho přijímá stejně jako YAML).

use super::MANAGED_LABEL;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const RULE_ANNOTATION: &str = "wslcd.rule";
pub const DENY_NAME: &str = "wslcd-default-deny";
pub const DNS_NAME: &str = "wslcd-allow-dns";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Peer {
    Component(String),
    /// provoz z Windows přes NodePort (po SNAT z brány podů na uzlu)
    Localhost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub from: Peer,
    /// cílová komponenta
    pub to: String,
    /// port kontejneru cíle (TCP)
    pub port: u16,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleSet {
    pub deny_all: bool,
    pub rules: Vec<Rule>,
}

impl RuleSet {
    /// Vzorová pravidla šablony FE + API + DB.
    pub fn demo() -> Self {
        let c = |s: &str| Peer::Component(s.into());
        RuleSet {
            deny_all: true,
            rules: vec![
                Rule { from: Peer::Localhost, to: "fe".into(), port: 80 },
                Rule { from: c("fe"), to: "api".into(), port: 3000 },
                Rule { from: c("api"), to: "db".into(), port: 5432 },
            ],
        }
    }

    /// Smí `from` na `to:port`? (pro zvýraznění povolených cest v mapě)
    pub fn allows(&self, from: &Peer, to: &str, port: u16) -> bool {
        !self.deny_all || self.rules.iter().any(|r| &r.from == from && r.to == to && r.port == port)
    }
}

/// Jméno objektu Kubernetes: malá písmena, číslice a `-`, max. 63 znaků.
fn dns_name(s: &str) -> String {
    let mut out: String = s.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    out.truncate(63);
    out.trim_matches('-').to_string()
}

pub fn rule_name(r: &Rule) -> String {
    let from = match &r.from {
        Peer::Component(c) => c.as_str(),
        Peer::Localhost => "localhost",
    };
    dns_name(&format!("wslcd-allow-{from}-{}-{}", r.to, r.port))
}

fn policy(ns: &str, name: &str, annotation: &str, spec: Value) -> Value {
    json!({
        "apiVersion": "networking.k8s.io/v1",
        "kind": "NetworkPolicy",
        "metadata": {
            "name": name,
            "namespace": ns,
            "labels": { MANAGED_LABEL: "1" },
            "annotations": { RULE_ANNOTATION: annotation }
        },
        "spec": spec
    })
}

/// NetworkPolicy pro sadu pravidel. `label_key` = label komponent (`app`), `node_cidrs` =
/// zdroj provozu z NodePort (brána podů na uzlu, `10.42.0.1/32`).
pub fn to_manifests(ns: &str, set: &RuleSet, label_key: &str, node_cidrs: &[String]) -> Vec<Value> {
    let mut out = Vec::new();
    if set.deny_all {
        out.push(policy(ns, DENY_NAME, "deny", json!({ "podSelector": {}, "policyTypes": ["Ingress", "Egress"] })));
        out.push(policy(
            ns,
            DNS_NAME,
            "dns",
            json!({
                "podSelector": {},
                "policyTypes": ["Egress"],
                "egress": [{
                    "to": [{
                        "namespaceSelector": { "matchLabels": { "kubernetes.io/metadata.name": "kube-system" } },
                        "podSelector": { "matchLabels": { "k8s-app": "kube-dns" } }
                    }],
                    "ports": [{ "protocol": "UDP", "port": 53 }, { "protocol": "TCP", "port": 53 }]
                }]
            }),
        ));
    }
    for r in &set.rules {
        let ann = serde_json::to_string(r).unwrap_or_default();
        let sel = |c: &str| json!({ "matchLabels": { label_key: c } });
        let ports = json!([{ "protocol": "TCP", "port": r.port }]);
        let name = rule_name(r);
        match &r.from {
            Peer::Component(from) => {
                out.push(policy(
                    ns,
                    &format!("{name}-in"),
                    &ann,
                    json!({ "podSelector": sel(&r.to), "policyTypes": ["Ingress"], "ingress": [{ "from": [{ "podSelector": sel(from) }], "ports": ports }] }),
                ));
                out.push(policy(
                    ns,
                    &format!("{name}-out"),
                    &ann,
                    json!({ "podSelector": sel(from), "policyTypes": ["Egress"], "egress": [{ "to": [{ "podSelector": sel(&r.to) }], "ports": ports }] }),
                ));
            }
            Peer::Localhost => {
                let from: Vec<Value> = node_cidrs.iter().map(|c| json!({ "ipBlock": { "cidr": c } })).collect();
                out.push(policy(
                    ns,
                    &format!("{name}-in"),
                    &ann,
                    json!({ "podSelector": sel(&r.to), "policyTypes": ["Ingress"], "ingress": [{ "from": from, "ports": ports }] }),
                ));
            }
        }
    }
    out
}

/// Seznam pro `kubectl apply -f -`.
pub fn as_list(manifests: &[Value]) -> String {
    serde_json::to_string_pretty(&json!({ "apiVersion": "v1", "kind": "List", "items": manifests })).unwrap_or_default()
}

/// Formulář z politik v namespace (`kubectl get networkpolicy -o json`):
/// (sada pravidel aplikace, jména cizích politik, jména politik aplikace).
pub fn from_cluster(json: &str) -> Result<(RuleSet, Vec<String>, Vec<String>), String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("kubectl JSON: {e}"))?;
    let mut set = RuleSet::default();
    let (mut foreign, mut ours) = (Vec::new(), Vec::new());
    for p in v.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default() {
        let name = p.pointer("/metadata/name").and_then(|n| n.as_str()).unwrap_or_default().to_string();
        let managed = p.pointer(&format!("/metadata/labels/{MANAGED_LABEL}")).and_then(|l| l.as_str()) == Some("1");
        if !managed {
            foreign.push(name);
            continue;
        }
        ours.push(name);
        match p.pointer(&format!("/metadata/annotations/{RULE_ANNOTATION}")).and_then(|a| a.as_str()) {
            Some("deny") => set.deny_all = true,
            Some("dns") | None => {}
            Some(a) => {
                if let Ok(r) = serde_json::from_str::<Rule>(a) {
                    if !set.rules.contains(&r) {
                        set.rules.push(r);
                    }
                }
            }
        }
    }
    Ok((set, foreign, ours))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(m: &[Value]) -> Vec<String> {
        m.iter().map(|p| p.pointer("/metadata/name").unwrap().as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn deny_all_adds_dns() {
        let m = to_manifests("demo", &RuleSet { deny_all: true, rules: vec![] }, "app", &[]);
        assert_eq!(names(&m), vec![DENY_NAME, DNS_NAME]);
        assert_eq!(m[0].pointer("/spec/policyTypes").unwrap(), &json!(["Ingress", "Egress"]));
        let ports = m[1].pointer("/spec/egress/0/ports").unwrap();
        assert_eq!(ports, &json!([{ "protocol": "UDP", "port": 53 }, { "protocol": "TCP", "port": 53 }]));
        assert_eq!(m[1].pointer("/spec/egress/0/to/0/podSelector/matchLabels/k8s-app").unwrap(), "kube-dns");
        assert!(to_manifests("demo", &RuleSet::default(), "app", &[]).is_empty());
    }

    #[test]
    fn rule_makes_two_policies() {
        let set = RuleSet { deny_all: false, rules: vec![Rule { from: Peer::Component("fe".into()), to: "api".into(), port: 3000 }] };
        let m = to_manifests("demo", &set, "app", &[]);
        assert_eq!(names(&m), vec!["wslcd-allow-fe-api-3000-in", "wslcd-allow-fe-api-3000-out"]);
        assert_eq!(m[0].pointer("/spec/podSelector/matchLabels/app").unwrap(), "api");
        assert_eq!(m[0].pointer("/spec/ingress/0/from/0/podSelector/matchLabels/app").unwrap(), "fe");
        assert_eq!(m[0].pointer("/spec/ingress/0/ports/0/port").unwrap(), 3000);
        assert_eq!(m[1].pointer("/spec/podSelector/matchLabels/app").unwrap(), "fe");
        assert_eq!(m[1].pointer("/spec/egress/0/to/0/podSelector/matchLabels/app").unwrap(), "api");
        assert_eq!(m[0].pointer("/metadata/namespace").unwrap(), "demo");
    }

    #[test]
    fn localhost_rule_uses_ipblock() {
        let set = RuleSet { deny_all: false, rules: vec![Rule { from: Peer::Localhost, to: "fe".into(), port: 80 }] };
        let m = to_manifests("demo", &set, "app.kubernetes.io/name", &["10.42.0.1/32".into()]);
        assert_eq!(names(&m), vec!["wslcd-allow-localhost-fe-80-in"]);
        assert_eq!(m[0].pointer("/spec/ingress/0/from/0/ipBlock/cidr").unwrap(), "10.42.0.1/32");
        assert_eq!(m[0].pointer("/spec/podSelector/matchLabels/app.kubernetes.io~1name").unwrap(), "fe");
    }

    #[test]
    fn managed_label_and_roundtrip() {
        let set = RuleSet::demo();
        let m = to_manifests("demo", &set, "app", &["10.42.0.1/32".into()]);
        assert!(m.iter().all(|p| p.pointer("/metadata/labels/wslcd.managed").unwrap() == "1"));
        let mut items = m.clone();
        items.push(json!({ "metadata": { "name": "theirs" } }));
        let (back, foreign, ours) = from_cluster(&json!({ "items": items }).to_string()).unwrap();
        assert_eq!(back, set);
        assert_eq!(foreign, vec!["theirs"]);
        assert_eq!(ours.len(), m.len());
        assert!(from_cluster("{").is_err());
    }

    #[test]
    fn allows_and_names() {
        let set = RuleSet::demo();
        assert!(set.allows(&Peer::Component("fe".into()), "api", 3000));
        assert!(!set.allows(&Peer::Component("fe".into()), "db", 5432));
        assert!(RuleSet::default().allows(&Peer::Component("fe".into()), "db", 5432), "bez default deny je povoleno vše");
        let long = Rule { from: Peer::Component("A_b".repeat(30)), to: "x".into(), port: 1 };
        let n = rule_name(&long);
        assert!(n.len() <= 63 && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
        assert!(as_list(&to_manifests("d", &set, "app", &[])).contains("\"kind\": \"List\""));
    }
}
