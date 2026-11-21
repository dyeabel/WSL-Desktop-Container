//! Kubernetes laboratoř na pozadí: stav clusteru, prostředí a toky (`K8sOp::Snapshot`, posílá
//! UI pravidelně, jen když je stránka otevřená) a akce uživatele.

use super::{Msg, Worker};
use crate::docker::Docker;
use crate::host::Host;
use crate::k8s::cluster::{self, ClusterState};
use crate::k8s::flows::{self, Edge};
use crate::k8s::model::{self, Pod, Service};
use crate::k8s::policy::{self, Peer, RuleSet};
use crate::k8s::{kubeconfig, template, LAB_LABEL, MANAGED_LABEL};

#[derive(Debug, Clone)]
pub enum K8sOp {
    /// stav, prostředí a (volitelně) toky namespace
    Snapshot { ns: Option<String>, flows: bool },
    Create,
    Start,
    Stop,
    Delete,
    Kubeconfig,
    LoadImage(String),
    Deploy { ns: String, folder: String },
    DeployDemo { ns: String, with_rules: bool },
    DeleteEnv(String),
    Expose { ns: String, svc: String },
    ApplyRules { ns: String, set: RuleSet },
    Probe { ns: String, from: Peer, to: String, port: u16 },
}

#[derive(Debug, Clone, Default)]
pub struct Env {
    pub ns: String,
    pub pods: Vec<Pod>,
    pub svcs: Vec<Service>,
    pub rules: RuleSet,
    pub foreign: Vec<String>,
    pub edges: Vec<Edge>,
}

#[derive(Debug, Clone)]
pub struct K8sSnap {
    pub state: ClusterState,
    pub namespaces: Vec<String>,
    pub env: Option<Env>,
    /// chyba při čtení prostředí (cluster běží)
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum K8sMsg {
    Snapshot(Box<K8sSnap>),
    /// průběh dlouhé akce (vytvoření, start…); None = hotovo
    Progress(Option<String>),
    Probe { from: Peer, to: String, port: u16, result: Result<bool, String> },
}

/// Volný NodePort z rozsahu.
pub fn pick_nodeport(used: &[u16], range: (u16, u16)) -> Option<u16> {
    (range.0..=range.1).find(|p| !used.contains(p))
}

/// Label komponent pro pravidla: `app`, pokud ho mají všechny pody, jinak `app.kubernetes.io/name`.
pub fn label_key(pods: &[Pod]) -> &'static str {
    if pods.iter().all(|p| p.labels.contains_key("app")) || !pods.iter().any(|p| p.labels.contains_key("app.kubernetes.io/name")) {
        "app"
    } else {
        "app.kubernetes.io/name"
    }
}

/// Manifesty ze složky: jen `*.yaml` / `*.yml` přímo ve složce, seřazené podle jména.
pub fn read_manifests(folder: &std::path::Path) -> Result<String, String> {
    let mut files: Vec<_> = std::fs::read_dir(folder)
        .map_err(|e| format!("{}: {e}", folder.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("yaml") || x.eq_ignore_ascii_case("yml")))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err(tf!("Ve složce {} nejsou soubory .yaml / .yml", "No .yaml / .yml files in {}", folder.display()));
    }
    let mut out = Vec::new();
    for f in files {
        out.push(std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?);
    }
    Ok(out.join("\n---\n"))
}

fn valid_ns(ns: &str) -> Result<(), String> {
    let ok = !ns.is_empty() && ns.len() <= 63 && ns.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !ns.starts_with('-') && !ns.ends_with('-');
    let reserved = ["default", "kube-system", "kube-public", "kube-node-lease"].contains(&ns);
    if ok && !reserved { Ok(()) } else { Err(tf!("Neplatný název prostředí „{ns}“ (malá písmena, číslice, -)", "Invalid environment name “{ns}” (lowercase, digits, -)")) }
}

impl Worker {
    fn k8s_progress(&self, p: Option<String>) {
        self.send(Msg::K8s(K8sMsg::Progress(p)));
    }

    pub(super) fn k8s_op(&self, op: K8sOp) -> Result<String, String> {
        let cfg = self.cfg();
        let d = self.docker();
        let r = match op {
            K8sOp::Snapshot { ns, flows } => {
                let snap = self.k8s_snapshot(&d, ns.as_deref(), flows);
                self.send(Msg::K8s(K8sMsg::Snapshot(Box::new(snap))));
                return Ok(String::new());
            }
            K8sOp::Create => {
                let r = cluster::create(&d, &cfg.k3s_image, cfg.k3s_api_port, cfg.k3s_nodeport_range, |p| self.k8s_progress(Some(p.to_string())))
                    .and_then(|_| cluster::wait_ready(&d, |p| self.k8s_progress(Some(p.to_string()))))
                    .map(|_| self.k8s_after_start(&d));
                self.k8s_progress(None);
                r.map(|kc| tf!("Cluster běží. {kc}", "The cluster is running. {kc}"))
            }
            K8sOp::Start => {
                self.k8s_progress(Some(t!("Spouštím uzel…", "Starting the node…").into()));
                let started = if cluster::needs_recreate(&d) {
                    self.k8s_progress(Some(t!("Převádím uzel na novější nastavení (data zůstanou)…", "Updating the node to the newer settings (data is kept)…").into()));
                    cluster::recreate(&d, &cfg.k3s_image, cfg.k3s_api_port, cfg.k3s_nodeport_range, |p| self.k8s_progress(Some(p.to_string())))
                } else {
                    cluster::start(&d)
                };
                let r = started.and_then(|_| cluster::wait_ready(&d, |p| self.k8s_progress(Some(p.to_string())))).map(|_| self.k8s_after_start(&d));
                self.k8s_progress(None);
                r.map(|kc| tf!("Cluster běží. {kc}", "The cluster is running. {kc}"))
            }
            K8sOp::Stop => cluster::stop(&d).map(|_| t!("Cluster zastaven", "Cluster stopped").into()),
            K8sOp::Delete => cluster::delete(&d).map(|_| t!("Cluster smazán", "Cluster deleted").into()),
            K8sOp::Kubeconfig => self.k8s_kubeconfig(&d),
            K8sOp::LoadImage(image) => {
                self.k8s_progress(Some(tf!("Načítám {image} do clusteru…", "Loading {image} into the cluster…")));
                let r = cluster::load_image(&*self.host(), cfg.engine, &image);
                self.k8s_progress(None);
                r.map(|_| tf!("Image {image} je v clusteru", "Image {image} is in the cluster"))
            }
            K8sOp::Deploy { ns, folder } => {
                valid_ns(&ns)?;
                let text = read_manifests(std::path::Path::new(&folder))?;
                self.k8s_ensure_ns(&d, &ns)?;
                cluster::apply(&*self.host(), cfg.engine, &ns, &text).map(|out| tf!("Nasazeno do {ns}:\n{}", "Deployed to {ns}:\n{}", out.trim()))
            }
            K8sOp::DeployDemo { ns, with_rules } => {
                valid_ns(&ns)?;
                self.k8s_ensure_ns(&d, &ns)?;
                let pw = crate::compose_templates::password(20);
                cluster::apply(&*self.host(), cfg.engine, &ns, &template::demo_manifests(&pw))?;
                let url = self.k8s_expose(&d, &ns, "fe")?;
                if with_rules {
                    self.k8s_apply_rules(&d, &ns, &RuleSet::demo())?;
                }
                Ok(tf!(
                    "Šablona nasazena do {ns}. Až pody naběhnou (první stažení image chvíli trvá), otevři {url}",
                    "Template deployed to {ns}. Once the pods are up (the first image pull takes a while), open {url}"
                ))
            }
            K8sOp::DeleteEnv(ns) => {
                valid_ns(&ns)?;
                cluster::kubectl(&d, &["delete", "namespace", &ns, "--wait=false"]).map(|_| tf!("Prostředí {ns} se maže", "Environment {ns} is being deleted"))
            }
            K8sOp::Expose { ns, svc } => self.k8s_expose(&d, &ns, &svc).map(|url| tf!("Služba {svc} je na {url}", "Service {svc} is at {url}")),
            K8sOp::ApplyRules { ns, set } => self.k8s_apply_rules(&d, &ns, &set).map(|n| tf!("Pravidla použita ({n} politik)", "Rules applied ({n} policies)")),
            K8sOp::Probe { ns, from, to, port } => {
                let result = self.k8s_probe(&d, &*self.host(), &ns, &from, &to, port);
                self.send(Msg::K8s(K8sMsg::Probe { from, to, port, result }));
                return Ok(String::new());
            }
        };
        r
    }

    /// Po startu: kubeconfig pro Windows (chyba je jen upozornění).
    fn k8s_after_start(&self, d: &Docker) -> String {
        self.k8s_kubeconfig(d).unwrap_or_else(|e| tf!("Kubeconfig se nepodařilo zapsat: {e}", "Could not write the kubeconfig: {e}"))
    }

    fn k8s_kubeconfig(&self, d: &Docker) -> Result<String, String> {
        let raw = cluster::kubectl(d, &["config", "view", "--raw", "-o", "json"])?;
        let cfg = kubeconfig::rewrite(&raw, self.cfg().k3s_api_port)?;
        let p = kubeconfig::write(&cfg)?;
        Ok(tf!("Kubeconfig: {}", "Kubeconfig: {}", p.display()))
    }

    fn k8s_ensure_ns(&self, d: &Docker, ns: &str) -> Result<(), String> {
        if let Err(e) = cluster::kubectl(d, &["create", "namespace", ns]) {
            if !e.contains("AlreadyExists") {
                return Err(e);
            }
        }
        cluster::kubectl(d, &["label", "namespace", ns, &format!("{LAB_LABEL}=1"), "--overwrite"]).map(|_| ())
    }

    fn k8s_expose(&self, d: &Docker, ns: &str, svc: &str) -> Result<String, String> {
        let all = model::parse_services(&cluster::kubectl(d, &["get", "svc", "-A", "-o", "json"])?)?;
        let s = all.iter().find(|s| s.namespace == ns && s.name == svc).ok_or_else(|| tf!("Služba {svc} neexistuje", "Service {svc} does not exist"))?;
        let first = s.ports.first().ok_or_else(|| tf!("Služba {svc} nemá porty", "Service {svc} has no ports"))?;
        let range = self.cfg().k3s_nodeport_range;
        let np = match first.node_port.filter(|p| (range.0..=range.1).contains(p)) {
            Some(p) => p,
            None => {
                let used: Vec<u16> = all.iter().flat_map(|s| s.ports.iter().filter_map(|p| p.node_port)).collect();
                let np = pick_nodeport(&used, range).ok_or_else(|| {
                    tf!("Všechny porty {}–{} jsou obsazené", "All ports {}–{} are taken", range.0, range.1)
                })?;
                let patch = serde_json::json!({ "spec": { "type": "NodePort", "ports": [{ "port": first.port, "nodePort": np }] } }).to_string();
                cluster::kubectl(d, &["patch", "service", svc, "-n", ns, "-p", &patch])?;
                np
            }
        };
        Ok(format!("http://localhost:{np}"))
    }

    fn k8s_apply_rules(&self, d: &Docker, ns: &str, set: &RuleSet) -> Result<usize, String> {
        let pods = model::parse_pods(&cluster::kubectl(d, &["get", "pods", "-n", ns, "-o", "json"])?)?;
        let mut gws = model::parse_node_gateways(&cluster::kubectl(d, &["get", "nodes", "-o", "json"])?)?;
        if let Some(g) = cluster::network_gateway(d) {
            gws.push(format!("{g}/32"));
        }
        let manifests = policy::to_manifests(ns, set, label_key(&pods), &gws);
        cluster::kubectl(d, &["delete", "networkpolicy", "-n", ns, "-l", &format!("{MANAGED_LABEL}=1"), "--ignore-not-found"])?;
        if !manifests.is_empty() {
            let cfg = self.cfg();
            cluster::apply(&*self.host(), cfg.engine, ns, &policy::as_list(&manifests))?;
        }
        Ok(manifests.len())
    }

    fn k8s_probe(&self, d: &Docker, host: &dyn Host, ns: &str, from: &Peer, to: &str, port: u16) -> Result<bool, String> {
        let pods = model::parse_pods(&cluster::kubectl(d, &["get", "pods", "-n", ns, "-o", "json"])?)?;
        let target = pods.iter().find(|p| p.component == to && p.ip.is_some()).ok_or_else(|| tf!("Komponenta {to} nemá běžící pod", "Component {to} has no running pod"))?;
        match from {
            Peer::Localhost => {
                let svcs = model::parse_services(&cluster::kubectl(d, &["get", "svc", "-n", ns, "-o", "json"])?)?;
                let np = svcs
                    .iter()
                    .filter(|s| s.selects(target))
                    .flat_map(|s| s.ports.iter())
                    .find_map(|p| p.node_port)
                    .ok_or_else(|| tf!("{to} není zpřístupněná na localhost", "{to} is not exposed on localhost"))?;
                Ok(cluster::probe_localhost(np))
            }
            Peer::Component(c) => {
                let src = pods.iter().find(|p| &p.component == c && p.ready.0 > 0).ok_or_else(|| tf!("Komponenta {c} nemá připravený pod", "Component {c} has no ready pod"))?;
                let netns = cluster::pod_netns(d, ns, &src.name)?;
                let pid = cluster::node_pid(d).ok_or("node pid")?;
                cluster::probe(host, pid, &netns, target.ip.as_deref().unwrap_or_default(), port)
            }
        }
    }

    fn k8s_snapshot(&self, d: &Docker, ns: Option<&str>, with_flows: bool) -> K8sSnap {
        let state = cluster::state(d);
        let mut snap = K8sSnap { state: state.clone(), namespaces: Vec::new(), env: None, error: None };
        if state != ClusterState::Running {
            return snap;
        }
        let r = (|| -> Result<(), String> {
            snap.namespaces = model::parse_names(&cluster::kubectl(d, &["get", "namespaces", "-l", &format!("{LAB_LABEL}=1"), "-o", "json"])?)?;
            let Some(ns) = ns.filter(|n| snap.namespaces.iter().any(|x| x == n)) else { return Ok(()) };
            let pods = model::parse_pods(&cluster::kubectl(d, &["get", "pods", "-n", ns, "-o", "json"])?)?;
            let svcs = model::parse_services(&cluster::kubectl(d, &["get", "svc", "-n", ns, "-o", "json"])?)?;
            let (rules, foreign, _) = policy::from_cluster(&cluster::kubectl(d, &["get", "networkpolicy", "-n", ns, "-o", "json"])?)?;
            let edges = if with_flows {
                let node_ips = model::parse_node_ips(&cluster::kubectl(d, &["get", "nodes", "-o", "json"])?)?;
                let out = crate::container_fs::exec(d, cluster::NAME, &["conntrack", "-L"]).map_err(|e| e.to_string())?;
                flows::build_edges(&pods, &svcs, &flows::parse_conntrack(&String::from_utf8_lossy(&out.stdout)), &node_ips, None)
            } else {
                Vec::new()
            };
            snap.env = Some(Env { ns: ns.to_string(), pods, svcs, rules, foreign, edges });
            Ok(())
        })();
        snap.error = r.err();
        snap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nodeport_picking() {
        assert_eq!(pick_nodeport(&[30080, 30081], (30080, 30099)), Some(30082));
        assert_eq!(pick_nodeport(&[30080], (30080, 30080)), None);
    }

    #[test]
    fn deploy_folder_reads_only_yaml() {
        let target = std::env::var_os("CARGO_TARGET_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
        let dir = target.join("test-k8s-folder");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("b.yml"), "B").unwrap();
        std::fs::write(dir.join("a.yaml"), "A").unwrap();
        std::fs::write(dir.join("c.txt"), "C").unwrap();
        std::fs::write(dir.join("sub").join("d.yaml"), "D").unwrap();
        assert_eq!(read_manifests(&dir).unwrap(), "A\n---\nB");
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(read_manifests(&dir).is_err());
    }

    #[test]
    fn label_key_choice() {
        let p = |k: &str| Pod { labels: [(k.to_string(), "x".to_string())].into(), ..Default::default() };
        assert_eq!(label_key(&[p("app"), p("app")]), "app");
        assert_eq!(label_key(&[p("app.kubernetes.io/name")]), "app.kubernetes.io/name");
        assert_eq!(label_key(&[]), "app");
    }

    #[test]
    fn namespace_validation() {
        assert!(valid_ns("demo-1").is_ok());
        for bad in ["", "Demo", "a_b", "-a", "kube-system", "default", &"a".repeat(64)] {
            assert!(valid_ns(bad).is_err(), "{bad}");
        }
    }
}
