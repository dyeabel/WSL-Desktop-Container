//! Minimální klient Docker Engine API přes TCP (http://127.0.0.1:<port>).

use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Port {
    #[serde(rename = "IP", default)]
    pub ip: Option<String>,
    #[serde(rename = "PrivatePort", default)]
    pub private_port: u16,
    #[serde(rename = "PublicPort", default)]
    pub public_port: Option<u16>,
    #[serde(rename = "Type", default)]
    pub typ: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Mount {
    #[serde(rename = "Type", default)]
    pub typ: String,
    #[serde(rename = "Name", default)]
    pub name: Option<String>,
    #[serde(rename = "Source", default)]
    pub source: String,
    #[serde(rename = "Destination", default)]
    pub destination: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct NetSettings {
    #[serde(rename = "Networks", default)]
    pub networks: Option<HashMap<String, Value>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Container {
    pub id: String,
    pub names: Option<Vec<String>>,
    pub image: String,
    #[serde(rename = "ImageID")]
    pub image_id: String,
    pub state: String,
    pub status: String,
    pub created: i64,
    pub ports: Option<Vec<Port>>,
    pub labels: Option<HashMap<String, String>>,
    pub network_settings: Option<NetSettings>,
    pub mounts: Option<Vec<Mount>>,
}

impl Container {
    pub fn name(&self) -> String {
        self.names
            .as_ref()
            .and_then(|n| n.first())
            .map(|n| n.trim_start_matches('/').to_string())
            .unwrap_or_else(|| short_id(&self.id).to_string())
    }
    pub fn label(&self, k: &str) -> Option<&str> {
        self.labels.as_ref().and_then(|l| l.get(k)).map(|s| s.as_str())
    }
    pub fn project(&self) -> Option<&str> {
        self.label("com.docker.compose.project")
    }
    pub fn networks(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .network_settings
            .as_ref()
            .and_then(|n| n.networks.as_ref())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        v.sort();
        v
    }
    pub fn ports_text(&self) -> String {
        let mut seen = Vec::new();
        for p in self.ports.iter().flatten() {
            let s = match p.public_port {
                Some(pp) => {
                    let ip = p.ip.as_deref().unwrap_or("");
                    if ip.contains(':') {
                        continue; // IPv6 duplicita
                    }
                    format!("{}:{}->{}", if ip.is_empty() { "0.0.0.0" } else { ip }, pp, p.private_port)
                }
                None => format!("{}/{}", p.private_port, p.typ),
            };
            if !seen.contains(&s) {
                seen.push(s);
            }
        }
        seen.join(", ")
    }
    pub fn running(&self) -> bool {
        self.state == "running"
    }
    pub fn paused(&self) -> bool {
        self.state == "paused"
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Image {
    pub id: String,
    pub repo_tags: Option<Vec<String>>,
    pub repo_digests: Option<Vec<String>>,
    pub size: i64,
    pub created: i64,
    pub containers: i64,
}

impl Image {
    pub fn tags(&self) -> Vec<String> {
        let t: Vec<String> = self
            .repo_tags
            .clone()
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t != "<none>:<none>")
            .collect();
        if t.is_empty() {
            let d = self
                .repo_digests
                .as_ref()
                .and_then(|d| d.first())
                .map(|d| d.split('@').next().unwrap_or("").to_string() + ":<none>");
            return vec![d.unwrap_or_else(|| "<none>".into())];
        }
        t
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Volume {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    pub created_at: Option<String>,
    pub labels: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct VolumesResp {
    volumes: Option<Vec<Volume>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct IpamConfig {
    pub subnet: Option<String>,
    pub gateway: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Ipam {
    pub config: Option<Vec<IpamConfig>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Network {
    pub name: String,
    pub id: String,
    pub driver: String,
    pub scope: String,
    pub internal: bool,
    #[serde(rename = "IPAM")]
    pub ipam: Option<Ipam>,
}

impl Network {
    pub fn subnet(&self) -> String {
        self.ipam
            .as_ref()
            .and_then(|i| i.config.as_ref())
            .map(|c| c.iter().filter_map(|c| c.subnet.clone()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default()
    }
    pub fn builtin(&self) -> bool {
        matches!(self.name.as_str(), "bridge" | "host" | "none")
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct Info {
    pub containers: i64,
    pub containers_running: i64,
    pub containers_paused: i64,
    pub containers_stopped: i64,
    pub images: i64,
    pub server_version: String,
    #[serde(rename = "NCPU")]
    pub ncpu: i64,
    pub mem_total: i64,
    pub operating_system: String,
    pub kernel_version: String,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub connected: bool,
    pub error: Option<String>,
    pub info: Option<Info>,
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub volumes: Vec<Volume>,
    pub networks: Vec<Network>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct StatRow {
    pub id: String,
    pub name: String,
    pub cpu: f64,
    pub mem: u64,
    pub mem_limit: u64,
    pub net_rx: u64,
    pub net_tx: u64,
    pub blk_read: u64,
    pub blk_write: u64,
}

#[derive(Debug, Clone, Default)]
pub struct DfSummary {
    pub images: i64,
    pub containers: i64,
    pub volumes: i64,
    pub build_cache: i64,
}

/// Výsledek hledání na Docker Hubu (`/images/search`).
/// Docker vrací `snake_case` a `is_official: true`, Podman `PascalCase` a `Official: "[OK]"`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct HubRepo {
    #[serde(alias = "Name")]
    pub name: String,
    #[serde(alias = "Description")]
    pub description: String,
    #[serde(alias = "Stars")]
    pub star_count: i64,
    #[serde(alias = "Official", deserialize_with = "de_official")]
    pub is_official: bool,
}

fn de_official<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Bool(b) => b,
        Value::String(s) => !s.trim().is_empty(),
        _ => false,
    })
}

/// Tag image z Docker Hub API.
#[derive(Debug, Clone, Default)]
pub struct HubTag {
    pub name: String,
    /// velikost image pro linux/amd64 (případně souhrnná)
    pub size: u64,
    /// datum poslední aktualizace (YYYY-MM-DD)
    pub updated: String,
}

/// Načte tagy z Docker Hubu (nejnovější první). Funguje jen pro image z docker.io.
pub fn hub_tags(repo: &str) -> Result<Vec<HubTag>, String> {
    let path = if repo.contains('/') { repo.to_string() } else { format!("library/{repo}") };
    let url = format!("https://hub.docker.com/v2/repositories/{path}/tags?page_size=100&ordering=last_updated");
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).build();
    let v: Value = agent
        .get(&url)
        .call()
        .map_err(|e| format!("Docker Hub: {}", Docker::err(e)))?
        .into_json()
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for t in v.get("results").and_then(|r| r.as_array()).into_iter().flatten() {
        let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
        if name.is_empty() {
            continue;
        }
        let amd64 = t
            .get("images")
            .and_then(|i| i.as_array())
            .and_then(|imgs| {
                imgs.iter().find(|i| {
                    i.get("architecture").and_then(|a| a.as_str()) == Some("amd64")
                        && i.get("os").and_then(|o| o.as_str()) == Some("linux")
                })
            })
            .and_then(|i| i.get("size").and_then(|s| s.as_u64()));
        let size = amd64.or_else(|| t.get("full_size").and_then(|s| s.as_u64())).unwrap_or(0);
        let updated = t
            .get("last_updated")
            .or_else(|| t.get("tag_last_pushed"))
            .and_then(|d| d.as_str())
            .map(|d| d.chars().take(10).collect())
            .unwrap_or_default();
        out.push(HubTag { name, size, updated });
    }
    // „latest“ vždy nahoru
    if let Some(i) = out.iter().position(|t| t.name == "latest") {
        let t = out.remove(i);
        out.insert(0, t);
    }
    Ok(out)
}

pub fn short_id(id: &str) -> &str {
    let id = id.trim_start_matches("sha256:");
    &id[..id.len().min(12)]
}

pub fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{:02X}", b),
        })
        .collect()
}

/// Událost z `/events` (Docker i Podman compat API).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Event {
    #[serde(rename = "Type", default)]
    pub kind: String,
    #[serde(rename = "Action", default)]
    action: String,
    #[serde(default)]
    status: String,
    #[serde(rename = "Actor", default)]
    pub actor: Actor,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Actor {
    #[serde(rename = "ID", default)]
    pub id: String,
    #[serde(rename = "Attributes", default)]
    pub attributes: HashMap<String, Value>,
}

impl Event {
    /// Akce bez parametrů – Docker posílá např. `health_status: unhealthy`.
    pub fn action(&self) -> &str {
        let a = if self.action.is_empty() { &self.status } else { &self.action };
        a.split(':').next().unwrap_or("").trim()
    }

    /// Parametr akce (`health_status: unhealthy` -> `unhealthy`).
    pub fn action_arg(&self) -> Option<&str> {
        let a = if self.action.is_empty() { &self.status } else { &self.action };
        a.split_once(':').map(|(_, v)| v.trim()).filter(|v| !v.is_empty())
    }

    pub fn attr(&self, k: &str) -> Option<String> {
        match self.actor.attributes.get(k)? {
            Value::String(s) => Some(s.clone()),
            Value::Null => None,
            v => Some(v.to_string()),
        }
    }

    pub fn name(&self) -> String {
        self.attr("name").unwrap_or_else(|| short_id(&self.actor.id).to_string())
    }

    pub fn exit_code(&self) -> Option<i64> {
        self.attr("exitCode").or_else(|| self.attr("containerExitCode")).and_then(|c| c.trim().parse().ok())
    }

    /// Mění událost něco, co aplikace zobrazuje? (Podman posílá `sync` při každém výpisu
    /// kontejnerů – na ten se reagovat nesmí, jinak by obnovování běželo pořád dokola.)
    pub fn triggers_refresh(&self) -> bool {
        const IGNORE: &[&str] = &[
            "sync", "health_status", "attach", "detach", "resize", "top", "archive-path", "extract-to-dir", "copy",
            "export", "save", "push", "mount", "unmount", "init", "cleanup", "checkpoint", "exec",
        ];
        let a = self.action();
        if a.is_empty() || a.starts_with("exec") || IGNORE.contains(&a) {
            return false;
        }
        matches!(self.kind.as_str(), "container" | "image" | "volume" | "network")
    }
}

// ---------------------------------------------------------------- TLS

/// TLS konfigurace klienta (CA enginu + klientský certifikát); None = obyčejné HTTP.
static TLS: std::sync::RwLock<Option<std::sync::Arc<rustls::ClientConfig>>> = std::sync::RwLock::new(None);

/// Složka s klientskými certifikáty (Docker konvence: ca.pem, cert.pem, key.pem) –
/// stejná se dá použít jako DOCKER_CERT_PATH.
pub fn tls_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(|p| std::path::PathBuf::from(p).join("WLS_Desktop").join("tls"))
}

/// Sestaví TLS klienta z PEM souborů ve složce (ověřuje server proti CA, posílá klientský certifikát).
pub fn tls_config(dir: &std::path::Path) -> Result<rustls::ClientConfig, String> {
    use rustls_pki_types::pem::PemObject;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer};
    let f = |n: &str| dir.join(n);
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_file_iter(f("ca.pem")).map_err(|e| format!("ca.pem: {e}"))? {
        roots.add(c.map_err(|e| format!("ca.pem: {e}"))?).map_err(|e| format!("ca.pem: {e}"))?;
    }
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(f("cert.pem"))
        .map_err(|e| format!("cert.pem: {e}"))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("cert.pem: {e}"))?;
    let key = PrivateKeyDer::from_pem_file(f("key.pem")).map_err(|e| format!("key.pem: {e}"))?;
    rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_client_auth_cert(certs, key)
        .map_err(|e| e.to_string())
}

/// Zapne / vypne TLS pro všechna další spojení s enginem.
pub fn configure_tls(on: bool) -> Result<(), String> {
    let cfg = if on {
        let dir = tls_dir().ok_or("APPDATA")?;
        Some(std::sync::Arc::new(tls_config(&dir)?))
    } else {
        None
    };
    if let Ok(mut g) = TLS.write() {
        *g = cfg;
    }
    Ok(())
}

fn tls() -> Option<std::sync::Arc<rustls::ClientConfig>> {
    TLS.read().ok().and_then(|g| g.clone())
}

fn agent_builder(tls: &Option<std::sync::Arc<rustls::ClientConfig>>) -> ureq::AgentBuilder {
    let b = ureq::AgentBuilder::new();
    match tls {
        Some(c) => b.tls_config(c.clone()),
        None => b,
    }
}

#[derive(Clone)]
pub struct Docker {
    agent: ureq::Agent,
    base: String,
    port: u16,
    tls: Option<std::sync::Arc<rustls::ClientConfig>>,
}

impl Docker {
    pub fn new(port: u16) -> Self {
        Self::with_tls(port, tls())
    }

    pub fn with_tls(port: u16, tls: Option<std::sync::Arc<rustls::ClientConfig>>) -> Self {
        let agent = agent_builder(&tls)
            .timeout_connect(Duration::from_millis(800))
            .timeout_read(Duration::from_secs(600))
            .build();
        let scheme = if tls.is_some() { "https" } else { "http" };
        Self { agent, base: format!("{scheme}://127.0.0.1:{port}"), port, tls }
    }

    fn url(&self, p: &str) -> String {
        format!("{}{}", self.base, p)
    }

    fn err(e: ureq::Error) -> String {
        match e {
            ureq::Error::Status(code, r) => {
                let body = r.into_string().unwrap_or_default();
                let msg = serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
                    .unwrap_or(body);
                format!("HTTP {code}: {}", msg.trim())
            }
            ureq::Error::Transport(t) => t.to_string(),
        }
    }

    pub fn get<T: DeserializeOwned>(&self, p: &str) -> Result<T, String> {
        self.agent
            .get(&self.url(p))
            .call()
            .map_err(Self::err)?
            .into_json()
            .map_err(|e| e.to_string())
    }

    pub fn post(&self, p: &str) -> Result<String, String> {
        self.agent
            .post(&self.url(p))
            .call()
            .map_err(Self::err)?
            .into_string()
            .map_err(|e| e.to_string())
    }

    pub fn post_json(&self, p: &str, body: Value) -> Result<String, String> {
        self.agent
            .post(&self.url(p))
            .send_json(body)
            .map_err(Self::err)?
            .into_string()
            .map_err(|e| e.to_string())
    }

    pub fn delete(&self, p: &str) -> Result<String, String> {
        self.agent
            .delete(&self.url(p))
            .call()
            .map_err(Self::err)?
            .into_string()
            .map_err(|e| e.to_string())
    }

    pub fn ping(&self) -> bool {
        self.agent
            .get(&self.url("/_ping"))
            .timeout(Duration::from_millis(1500))
            .call()
            .is_ok()
    }

    pub fn snapshot(&self) -> Snapshot {
        if !self.ping() {
            return Snapshot {
                error: Some(tf!("API enginu není dostupné na 127.0.0.1:{}", "Engine API is not available on 127.0.0.1:{}", self.port)),
                ..Default::default()
            };
        }
        let mut s = Snapshot { connected: true, ..Default::default() };
        let mut errs = Vec::new();
        match self.get::<Info>("/info") {
            Ok(i) => s.info = Some(i),
            Err(e) => errs.push(e),
        }
        match self.get::<Vec<Container>>("/containers/json?all=1") {
            Ok(mut c) => {
                c.sort_by_key(|c| c.name());
                s.containers = c
            }
            Err(e) => errs.push(e),
        }
        match self.get::<Vec<Image>>("/images/json") {
            Ok(mut i) => {
                i.sort_by_key(|i| i.tags().join(","));
                s.images = i
            }
            Err(e) => errs.push(e),
        }
        match self.get::<VolumesResp>("/volumes") {
            Ok(v) => {
                let mut v = v.volumes.unwrap_or_default();
                v.sort_by(|a, b| a.name.cmp(&b.name));
                s.volumes = v
            }
            Err(e) => errs.push(e),
        }
        match self.get::<Vec<Network>>("/networks") {
            Ok(mut n) => {
                n.sort_by(|a, b| a.name.cmp(&b.name));
                s.networks = n
            }
            Err(e) => errs.push(e),
        }
        if !errs.is_empty() {
            s.error = Some(errs.join("; "));
        }
        s
    }

    pub fn container_action(&self, id: &str, action: &str) -> Result<String, String> {
        match action {
            "remove" => self.delete(&format!("/containers/{id}?force=1")),
            "stop" | "restart" => self.post(&format!("/containers/{id}/{action}?t=10")),
            _ => self.post(&format!("/containers/{id}/{action}")),
        }
    }

    pub fn pull(&self, reference: &str) -> Result<String, String> {
        let shown = reference.trim().to_string();
        // plný název (docker.io/library/…) – Podman krátká jména bez registru odmítá
        let full = crate::engine::qualify_image(&shown);
        let r = full.as_str();
        let (image, tag) = if r.contains('@') {
            (r.to_string(), String::new())
        } else {
            let slash = r.rfind('/').map(|i| i + 1).unwrap_or(0);
            match r[slash..].rfind(':') {
                Some(c) => (r[..slash + c].to_string(), r[slash + c + 1..].to_string()),
                None => (r.to_string(), "latest".to_string()),
            }
        };
        let mut url = format!("/images/create?fromImage={}", enc(&image));
        if !tag.is_empty() {
            url.push_str(&format!("&tag={}", enc(&tag)));
        }
        let body = self.post(&url)?;
        for line in body.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let Some(e) = v.get("error").and_then(|e| e.as_str()) {
                    return Err(e.to_string());
                }
            }
        }
        Ok(tf!("Image {shown} stažen", "Image {shown} pulled"))
    }

    /// Hledání image na Docker Hubu přes démona (jako `docker search`).
    pub fn search(&self, term: &str, engine: crate::engine::Engine) -> Result<Vec<HubRepo>, String> {
        let t = term.trim();
        // Podman bez nastaveného registru neví, kde hledat – hledáme výslovně na Docker Hubu
        let t = if engine == crate::engine::Engine::Podman && !t.contains('/') { format!("docker.io/{t}") } else { t.to_string() };
        let mut v: Vec<HubRepo> = self.get(&format!("/images/search?term={}&limit=25", enc(&t)))?;
        for r in &mut v {
            r.name = crate::engine::short_image(&r.name);
        }
        v.sort_by(|a, b| b.is_official.cmp(&a.is_official).then(b.star_count.cmp(&a.star_count)));
        Ok(v)
    }

    pub fn prune(&self, what: &str, filters: Option<&str>) -> Result<String, String> {
        let mut url = format!("/{what}/prune");
        if let Some(f) = filters {
            url.push_str(&format!("?filters={}", enc(f)));
        }
        let body = self.post(&url)?;
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let n = ["ContainersDeleted", "ImagesDeleted", "VolumesDeleted", "NetworksDeleted"]
            .iter()
            .filter_map(|k| v.get(*k).and_then(|a| a.as_array()).map(|a| a.len()))
            .sum::<usize>();
        let space = v.get("SpaceReclaimed").and_then(|s| s.as_u64()).unwrap_or(0);
        Ok(tf!("Odstraněno položek: {n}, uvolněno {}", "Items removed: {n}, reclaimed {}", crate::ui::fmt_bytes(space)))
    }

    pub fn is_tty(&self, id: &str) -> bool {
        self.get::<Value>(&format!("/containers/{id}/json"))
            .ok()
            .and_then(|v| v.pointer("/Config/Tty").and_then(|t| t.as_bool()))
            .unwrap_or(false)
    }

    /// Streamuje logy, dokud `on_chunk` vrací true.
    pub fn stream_logs(&self, id: &str, tail: u32, mut on_chunk: impl FnMut(&str) -> bool) -> Result<(), String> {
        let tty = self.is_tty(id);
        let agent = agent_builder(&self.tls).timeout_connect(Duration::from_secs(2)).build();
        let resp = agent
            .get(&self.url(&format!(
                "/containers/{id}/logs?stdout=1&stderr=1&follow=1&tail={tail}"
            )))
            .call()
            .map_err(Self::err)?;
        let mut r = resp.into_reader();
        if tty {
            let mut buf = [0u8; 8192];
            loop {
                let n = r.read(&mut buf).map_err(|e| e.to_string())?;
                if n == 0 || !on_chunk(&String::from_utf8_lossy(&buf[..n])) {
                    break;
                }
            }
        } else {
            let mut h = [0u8; 8];
            loop {
                if r.read_exact(&mut h).is_err() {
                    break;
                }
                let size = u32::from_be_bytes([h[4], h[5], h[6], h[7]]) as usize;
                let mut data = vec![0u8; size];
                r.read_exact(&mut data).map_err(|e| e.to_string())?;
                if !on_chunk(&String::from_utf8_lossy(&data)) {
                    break;
                }
            }
        }
        Ok(())
    }

    /// Odebírá `/events`. `on_connected` se zavolá po navázání spojení, `on_event` pro každou
    /// událost (vrací false = skončit). Při 120 s ticha vrátí Ok(()) – volající se připojí znovu,
    /// aby se nezasekl na spojení, které zaniklo bez ukončení (např. vypnuté WSL).
    pub fn stream_events(&self, on_connected: impl FnOnce(), mut on_event: impl FnMut(Event) -> bool) -> Result<(), String> {
        use std::io::BufRead;
        let agent = agent_builder(&self.tls)
            .timeout_connect(Duration::from_secs(2))
            .timeout_read(Duration::from_secs(120))
            .build();
        let resp = agent.get(&self.url("/events")).call().map_err(Self::err)?;
        on_connected();
        let r = std::io::BufReader::new(resp.into_reader());
        for line in r.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => return Ok(()),
                Err(e) => return Err(e.to_string()),
            };
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<Event>(&line) {
                if !on_event(ev) {
                    break;
                }
            }
        }
        Ok(())
    }

    pub fn stats(&self, id: &str, name: &str) -> Option<StatRow> {
        let v: Value = self.get(&format!("/containers/{id}/stats?stream=false")).ok()?;
        let u = |p: &str| v.pointer(p).and_then(|x| x.as_u64()).unwrap_or(0);
        let cpu_delta = u("/cpu_stats/cpu_usage/total_usage") as f64 - u("/precpu_stats/cpu_usage/total_usage") as f64;
        let sys_delta = u("/cpu_stats/system_cpu_usage") as f64 - u("/precpu_stats/system_cpu_usage") as f64;
        let online = u("/cpu_stats/online_cpus").max(1) as f64;
        let cpu = if sys_delta > 0.0 && cpu_delta > 0.0 { cpu_delta / sys_delta * online * 100.0 } else { 0.0 };
        let cache = v
            .pointer("/memory_stats/stats/inactive_file")
            .and_then(|x| x.as_u64())
            .or_else(|| v.pointer("/memory_stats/stats/total_inactive_file").and_then(|x| x.as_u64()))
            .unwrap_or(0);
        let mem = u("/memory_stats/usage").saturating_sub(cache);
        let (mut rx, mut tx) = (0, 0);
        if let Some(n) = v.get("networks").and_then(|n| n.as_object()) {
            for i in n.values() {
                rx += i.get("rx_bytes").and_then(|x| x.as_u64()).unwrap_or(0);
                tx += i.get("tx_bytes").and_then(|x| x.as_u64()).unwrap_or(0);
            }
        }
        let (mut br, mut bw) = (0, 0);
        if let Some(a) = v.pointer("/blkio_stats/io_service_bytes_recursive").and_then(|x| x.as_array()) {
            for e in a {
                let val = e.get("value").and_then(|x| x.as_u64()).unwrap_or(0);
                match e.get("op").and_then(|x| x.as_str()).unwrap_or("").to_ascii_lowercase().as_str() {
                    "read" => br += val,
                    "write" => bw += val,
                    _ => {}
                }
            }
        }
        Some(StatRow {
            id: id.to_string(),
            name: name.to_string(),
            cpu,
            mem,
            mem_limit: u("/memory_stats/limit"),
            net_rx: rx,
            net_tx: tx,
            blk_read: br,
            blk_write: bw,
        })
    }

    pub fn df(&self) -> Option<DfSummary> {
        let v: Value = self.get("/system/df").ok()?;
        let sum = |arr: &str, field: &str| -> i64 {
            v.get(arr)
                .and_then(|a| a.as_array())
                .map(|a| a.iter().filter_map(|e| e.pointer(field).and_then(|x| x.as_i64())).filter(|x| *x > 0).sum())
                .unwrap_or(0)
        };
        Some(DfSummary {
            images: v.get("LayersSize").and_then(|x| x.as_i64()).unwrap_or_else(|| sum("Images", "/Size")),
            containers: sum("Containers", "/SizeRw"),
            volumes: sum("Volumes", "/UsageData/Size"),
            build_cache: sum("BuildCache", "/Size"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_container_list() {
        let j = r#"[{"Id":"abcdef1234567890","Names":["/web"],"Image":"nginx","ImageID":"sha256:1","State":"running","Status":"Up","Created":1,
            "Ports":[{"IP":"0.0.0.0","PrivatePort":80,"PublicPort":8080,"Type":"tcp"},{"IP":"::","PrivatePort":80,"PublicPort":8080,"Type":"tcp"}],
            "Labels":{"com.docker.compose.project":"site"},"NetworkSettings":{"Networks":{"site_default":{}}},"Mounts":[]}]"#;
        let c: Vec<Container> = serde_json::from_str(j).unwrap();
        assert_eq!(c[0].name(), "web");
        assert_eq!(c[0].project(), Some("site"));
        assert_eq!(c[0].ports_text(), "0.0.0.0:8080->80");
        assert_eq!(c[0].networks(), vec!["site_default".to_string()]);
    }

    #[test]
    fn parses_nulls() {
        let j = r#"[{"Id":"x","Names":null,"Labels":null,"Ports":null,"Mounts":null}]"#;
        let c: Vec<Container> = serde_json::from_str(j).unwrap();
        assert_eq!(c[0].ports_text(), "");
    }

    #[test]
    fn search_docker_and_podman_formats() {
        let docker = r#"[{"star_count":5,"is_official":true,"name":"nginx","description":"x"}]"#;
        let podman = r#"[{"Index":"docker.io","Name":"docker.io/library/nginx","Description":"x","Stars":5,"Official":"[OK]"},{"Name":"docker.io/a/b","Official":""}]"#;
        let d: Vec<HubRepo> = serde_json::from_str(docker).unwrap();
        let p: Vec<HubRepo> = serde_json::from_str(podman).unwrap();
        assert!(d[0].is_official && d[0].star_count == 5 && d[0].name == "nginx");
        assert!(p[0].is_official && p[0].star_count == 5 && p[0].name == "docker.io/library/nginx");
        assert!(!p[1].is_official);
    }

    #[test]
    fn parses_events() {
        // Podman compat
        let p: Event = serde_json::from_str(
            r#"{"status":"die","id":"abc","Type":"container","Action":"die","Actor":{"ID":"abc123","Attributes":{"name":"web","exitCode":"3","containerExitCode":"3"}},"time":1}"#,
        )
        .unwrap();
        assert_eq!((p.action(), p.name().as_str(), p.exit_code()), ("die", "web", Some(3)));
        assert!(p.triggers_refresh());
        // Docker: parametr v akci
        let d: Event = serde_json::from_str(
            r#"{"Type":"container","Action":"health_status: unhealthy","Actor":{"ID":"x","Attributes":{"name":"db"}}}"#,
        )
        .unwrap();
        assert_eq!((d.action(), d.action_arg()), ("health_status", Some("unhealthy")));
        assert!(!d.triggers_refresh());
        let s: Event = serde_json::from_str(r#"{"Type":"container","Action":"sync","Actor":{"ID":"x"}}"#).unwrap();
        assert!(!s.triggers_refresh());
        let e: Event = serde_json::from_str(r#"{"Type":"container","Action":"exec_start: sh","Actor":{"ID":"x"}}"#).unwrap();
        assert!(!e.triggers_refresh());
        let v: Event = serde_json::from_str(r#"{"Type":"volume","Action":"create","Actor":{"ID":"data"}}"#).unwrap();
        assert!(v.triggers_refresh());
    }

    #[test]
    fn encodes_query() {
        assert_eq!(enc("{\"dangling\":[\"false\"]}"), "%7B%22dangling%22%3A%5B%22false%22%5D%7D");
    }
}
