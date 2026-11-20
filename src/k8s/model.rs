//! Objekty clusteru z `kubectl get … -o json` (pody, služby, namespace).

use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pod {
    pub name: String,
    pub namespace: String,
    /// komponenta = množina podů pro mapu a pravidla (label `app`, …)
    pub component: String,
    pub ip: Option<String>,
    pub phase: String,
    /// (připravené kontejnery, všechny)
    pub ready: (u32, u32),
    pub restarts: u32,
    pub images: Vec<String>,
    pub labels: BTreeMap<String, String>,
    /// důvod čekání prvního nepřipraveného kontejneru (ImagePullBackOff, CrashLoopBackOff…)
    pub waiting: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SvcPort {
    pub name: Option<String>,
    pub port: u16,
    pub node_port: Option<u16>,
    pub proto: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Service {
    pub name: String,
    pub namespace: String,
    pub selector: BTreeMap<String, String>,
    /// ClusterIP | NodePort | LoadBalancer | ExternalName
    pub kind: String,
    pub cluster_ip: Option<String>,
    pub ports: Vec<SvcPort>,
}

impl Service {
    /// Pody, které služba vybírá (všechny labely selektoru se shodují).
    pub fn selects(&self, pod: &Pod) -> bool {
        !self.selector.is_empty() && self.selector.iter().all(|(k, v)| pod.labels.get(k) == Some(v))
    }
}

fn items(json: &str) -> Result<Vec<Value>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("kubectl JSON: {e}"))?;
    Ok(v.get("items").and_then(|i| i.as_array()).cloned().unwrap_or_default())
}

fn s(v: &Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_str().map(str::to_string)
}

fn labels(v: &Value) -> BTreeMap<String, String> {
    v.pointer("/metadata/labels")
        .and_then(|l| l.as_object())
        .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
        .unwrap_or_default()
}

/// Komponenta podu: label `app`, `app.kubernetes.io/name`, jméno vlastníka (ReplicaSet bez
/// hashe šablony), jinak jméno podu.
pub fn component_of(labels: &BTreeMap<String, String>, owner: Option<&str>, pod_name: &str) -> String {
    if let Some(a) = labels.get("app").or_else(|| labels.get("app.kubernetes.io/name")) {
        return a.clone();
    }
    if let Some(o) = owner {
        // Deployment → ReplicaSet `web-7d9f8c6b5`: hash je poslední segment s číslicí
        return match o.rsplit_once('-') {
            Some((base, hash)) if !base.is_empty() && hash.chars().all(|c| c.is_ascii_alphanumeric()) && hash.chars().any(|c| c.is_ascii_digit()) => {
                base.to_string()
            }
            _ => o.to_string(),
        };
    }
    pod_name.to_string()
}

pub fn parse_pods(json: &str) -> Result<Vec<Pod>, String> {
    Ok(items(json)?
        .iter()
        .map(|p| {
            let name = s(p, &["metadata", "name"]).unwrap_or_default();
            let labels = labels(p);
            let owner = p.pointer("/metadata/ownerReferences/0/name").and_then(|n| n.as_str()).map(str::to_string);
            let statuses = p.pointer("/status/containerStatuses").and_then(|c| c.as_array()).cloned().unwrap_or_default();
            let total = p.pointer("/spec/containers").and_then(|c| c.as_array()).map(|c| c.len() as u32).unwrap_or(statuses.len() as u32);
            let ready = statuses.iter().filter(|c| c.get("ready").and_then(|r| r.as_bool()).unwrap_or(false)).count() as u32;
            let restarts = statuses.iter().filter_map(|c| c.get("restartCount").and_then(|r| r.as_u64())).sum::<u64>() as u32;
            let waiting = statuses.iter().find_map(|c| c.pointer("/state/waiting/reason").and_then(|r| r.as_str()).map(str::to_string));
            let images = p
                .pointer("/spec/containers")
                .and_then(|c| c.as_array())
                .map(|c| c.iter().filter_map(|c| c.get("image")?.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            Pod {
                component: component_of(&labels, owner.as_deref(), &name),
                name,
                namespace: s(p, &["metadata", "namespace"]).unwrap_or_default(),
                ip: s(p, &["status", "podIP"]).filter(|i| !i.is_empty()),
                phase: s(p, &["status", "phase"]).unwrap_or_default(),
                ready: (ready, total),
                restarts,
                images,
                labels,
                waiting,
            }
        })
        .collect())
}

pub fn parse_services(json: &str) -> Result<Vec<Service>, String> {
    Ok(items(json)?
        .iter()
        .map(|v| Service {
            name: s(v, &["metadata", "name"]).unwrap_or_default(),
            namespace: s(v, &["metadata", "namespace"]).unwrap_or_default(),
            selector: v
                .pointer("/spec/selector")
                .and_then(|l| l.as_object())
                .map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect())
                .unwrap_or_default(),
            kind: s(v, &["spec", "type"]).unwrap_or_else(|| "ClusterIP".into()),
            cluster_ip: s(v, &["spec", "clusterIP"]).filter(|i| !i.is_empty() && i != "None"),
            ports: v
                .pointer("/spec/ports")
                .and_then(|p| p.as_array())
                .map(|ps| {
                    ps.iter()
                        .filter_map(|p| {
                            Some(SvcPort {
                                name: p.get("name").and_then(|n| n.as_str()).map(str::to_string),
                                port: p.get("port")?.as_u64()? as u16,
                                node_port: p.get("nodePort").and_then(|n| n.as_u64()).map(|n| n as u16),
                                proto: p.get("protocol").and_then(|n| n.as_str()).unwrap_or("TCP").to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect())
}

/// Jména namespace s labelem prostředí (`kubectl get ns -l wslcd.lab=1 -o json`).
pub fn parse_names(json: &str) -> Result<Vec<String>, String> {
    let mut v: Vec<String> = items(json)?.iter().filter_map(|n| s(n, &["metadata", "name"])).collect();
    v.sort();
    Ok(v)
}

/// IP uzlů (InternalIP) z `kubectl get nodes -o json`.
pub fn parse_node_ips(json: &str) -> Result<Vec<String>, String> {
    Ok(items(json)?
        .iter()
        .flat_map(|n| n.pointer("/status/addresses").and_then(|a| a.as_array()).cloned().unwrap_or_default())
        .filter(|a| a.get("type").and_then(|t| t.as_str()) == Some("InternalIP"))
        .filter_map(|a| a.get("address").and_then(|x| x.as_str()).map(str::to_string))
        .collect())
}

/// Brány podsítí podů na uzlech (`spec.podCIDR` `10.42.0.0/24` → `10.42.0.1/32`): odtud
/// přichází provoz z NodePort po SNAT, a tedy i přístup z Windows.
pub fn parse_node_gateways(json: &str) -> Result<Vec<String>, String> {
    Ok(items(json)?
        .iter()
        .filter_map(|n| s(n, &["spec", "podCIDR"]))
        .filter_map(|cidr| {
            let (net, _) = cidr.split_once('/')?;
            let mut o: Vec<u8> = net.split('.').map(|x| x.parse().ok()).collect::<Option<_>>()?;
            if o.len() != 4 {
                return None;
            }
            o[3] = o[3].checked_add(1)?;
            Some(format!("{}.{}.{}.{}/32", o[0], o[1], o[2], o[3]))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_gateways() {
        let n = r#"{"items":[{"spec":{"podCIDR":"10.42.0.0/24"}},{"spec":{}},{"spec":{"podCIDR":"fd00::/64"}}]}"#;
        assert_eq!(parse_node_gateways(n).unwrap(), vec!["10.42.0.1/32"]);
    }

    const PODS: &str = r#"{"items":[
      {"metadata":{"name":"fe-6b7c9d8f5-abcde","namespace":"demo","labels":{"app":"fe","tier":"frontend"},
        "ownerReferences":[{"kind":"ReplicaSet","name":"fe-6b7c9d8f5"}]},
       "spec":{"containers":[{"name":"nginx","image":"nginx:1.27-alpine"}]},
       "status":{"phase":"Running","podIP":"10.42.0.12","containerStatuses":[{"ready":true,"restartCount":2}]}},
      {"metadata":{"name":"web-7d9f8c6b5-x1y2z","namespace":"demo","ownerReferences":[{"name":"web-7d9f8c6b5"}]},
       "spec":{"containers":[{"name":"a","image":"x"},{"name":"b","image":"y"}]},
       "status":{"phase":"Pending","containerStatuses":[{"ready":false,"restartCount":0,"state":{"waiting":{"reason":"ImagePullBackOff"}}},{"ready":true,"restartCount":1}]}}
    ]}"#;

    #[test]
    fn parse_pods_fixture() {
        let p = parse_pods(PODS).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].component, "fe");
        assert_eq!(p[0].ready, (1, 1));
        assert_eq!(p[0].restarts, 2);
        assert_eq!(p[0].ip.as_deref(), Some("10.42.0.12"));
        assert_eq!(p[1].component, "web");
        assert_eq!(p[1].ready, (1, 2));
        assert_eq!(p[1].ip, None);
        assert_eq!(p[1].waiting.as_deref(), Some("ImagePullBackOff"));
    }

    #[test]
    fn component_fallbacks() {
        let l = |k: &str, v: &str| BTreeMap::from([(k.to_string(), v.to_string())]);
        assert_eq!(component_of(&l("app", "x"), None, "p"), "x");
        assert_eq!(component_of(&l("app.kubernetes.io/name", "y"), None, "p"), "y");
        assert_eq!(component_of(&BTreeMap::new(), Some("web-7d9f8c6b5"), "p"), "web");
        // StatefulSet / vlastník bez hashe
        assert_eq!(component_of(&BTreeMap::new(), Some("postgres"), "postgres-0"), "postgres");
        assert_eq!(component_of(&BTreeMap::new(), None, "solo"), "solo");
    }

    #[test]
    fn parse_services_nodeport() {
        let j = r#"{"items":[{"metadata":{"name":"fe","namespace":"demo"},
          "spec":{"type":"NodePort","clusterIP":"10.43.1.2","selector":{"app":"fe"},
          "ports":[{"port":80,"targetPort":80,"nodePort":30080,"protocol":"TCP"}]}},
          {"metadata":{"name":"hl","namespace":"demo"},"spec":{"clusterIP":"None","ports":[{"port":5432}]}}]}"#;
        let s = parse_services(j).unwrap();
        assert_eq!(s[0].kind, "NodePort");
        assert_eq!(s[0].ports[0].node_port, Some(30080));
        assert_eq!(s[0].cluster_ip.as_deref(), Some("10.43.1.2"));
        assert_eq!(s[1].kind, "ClusterIP");
        assert_eq!(s[1].cluster_ip, None);
        let pods = parse_pods(PODS).unwrap();
        assert!(s[0].selects(&pods[0]));
        assert!(!s[0].selects(&pods[1]));
        assert!(!s[1].selects(&pods[0]), "prázdný selektor nevybírá nic");
    }

    #[test]
    fn names_and_node_ips() {
        assert_eq!(parse_names(r#"{"items":[{"metadata":{"name":"b"}},{"metadata":{"name":"a"}}]}"#).unwrap(), vec!["a", "b"]);
        let n = r#"{"items":[{"status":{"addresses":[{"type":"InternalIP","address":"10.89.0.2"},{"type":"Hostname","address":"x"}]}}]}"#;
        assert_eq!(parse_node_ips(n).unwrap(), vec!["10.89.0.2"]);
        assert!(parse_pods("not json").is_err());
    }
}
