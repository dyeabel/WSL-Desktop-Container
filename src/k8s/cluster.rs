//! k3s s jedním uzlem jako kontejner `wslcd-k3s` v primárním enginu (Docker i Podman).
//! Čtení přes `kubectl` spuštěný `exec` v kontejneru (Docker API), zápis manifestů a import
//! image přes CLI enginu v distribuci (potřebuje stdin). Sonda spojení běží v distribuci
//! v síťovém namespace podu (`nsenter`).

use crate::container_fs;
use crate::docker::{enc, Docker};
use crate::engine::Engine;
use crate::wsl;
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub const NAME: &str = "wslcd-k3s";
pub const NETWORK: &str = "wslcd-k3s";
pub const VOLUME: &str = "wslcd-k3s-data";
pub const DEFAULT_IMAGE: &str = "docker.io/rancher/k3s:v1.33.6-k3s1";
pub const READY_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, PartialEq)]
pub enum ClusterState {
    /// kontejner neexistuje
    Missing,
    Stopped,
    /// kontejner běží, API ještě neodpovídá `ok`
    Starting,
    Running,
    Error(String),
}

impl ClusterState {
    pub fn exists(&self) -> bool {
        !matches!(self, ClusterState::Missing)
    }
}

/// Tělo `POST /containers/create` (Docker i kompatibilní API Podmanu).
pub fn create_body(image: &str, api_port: u16, range: (u16, u16)) -> Value {
    let mut exposed = serde_json::Map::new();
    let mut bindings = serde_json::Map::new();
    let mut bind = |port: u16| {
        let key = format!("{port}/tcp");
        exposed.insert(key.clone(), json!({}));
        bindings.insert(key, json!([{ "HostIp": "127.0.0.1", "HostPort": port.to_string() }]));
    };
    bind(api_port);
    for p in range.0..=range.1 {
        bind(p);
    }
    // API uvnitř kontejneru je vždy na 6443 – mění se jen port na hostiteli
    if api_port != 6443 {
        exposed.remove(&format!("{api_port}/tcp"));
        let b = bindings.remove(&format!("{api_port}/tcp")).unwrap_or_default();
        exposed.insert("6443/tcp".into(), json!({}));
        bindings.insert("6443/tcp".into(), b);
    }
    json!({
        "Image": image,
        "Cmd": ["server", "--disable=traefik", "--tls-san=127.0.0.1", "--write-kubeconfig-mode=600"],
        "Labels": { "wslcd.role": "k8s-node" },
        "ExposedPorts": exposed,
        "HostConfig": {
            "Privileged": true,
            "CgroupnsMode": "host",
            "RestartPolicy": { "Name": "no" },
            "NetworkMode": NETWORK,
            "Binds": [format!("{VOLUME}:/var/lib/rancher/k3s")],
            "Tmpfs": { "/run": "", "/var/run": "" },
            "PortBindings": bindings
        }
    })
}

/// Srozumitelná zpráva, když je port obsazený (Docker: „port is already allocated“,
/// Podman: „address already in use“).
pub fn port_conflict_message(err: &str) -> Option<String> {
    let low = err.to_lowercase();
    if !(low.contains("already allocated") || low.contains("address already in use")) {
        return None;
    }
    // poslední „:číslo“ před chybou je port
    let port = err
        .split(|c: char| !(c.is_ascii_digit() || c == ':'))
        .filter_map(|t| t.trim_end_matches(':').rsplit_once(':').and_then(|(_, p)| p.parse::<u16>().ok()))
        .next_back()?;
    Some(tf!(
        "Port {port} na 127.0.0.1 je obsazený (jiný cluster, kind, Docker Desktop?). Uvolni ho nebo změň porty clusteru v config.json.",
        "Port {port} on 127.0.0.1 is in use (another cluster, kind, Docker Desktop?). Free it or change the cluster ports in config.json."
    ))
}

/// Chyba po vypršení čekání na připravenost: s posledními řádky logu k3s.
pub fn timeout_error(log_tail: &str) -> String {
    let tail: Vec<&str> = log_tail.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = tail[tail.len().saturating_sub(8)..].join("\n");
    tf!(
        "Cluster nenaběhl do {} s. Poslední řádky logu k3s:\n{tail}",
        "The cluster did not become ready within {} s. Last lines of the k3s log:\n{tail}",
        READY_TIMEOUT.as_secs()
    )
}

// ---------------------------------------------------------------- I/O

pub fn state(d: &Docker) -> ClusterState {
    match d.get::<Value>(&format!("/containers/{}/json", enc(NAME))) {
        Err(e) if e.is_not_found() => ClusterState::Missing,
        Err(e) => ClusterState::Error(e.to_string()),
        Ok(v) => {
            if v.pointer("/State/Running").and_then(|r| r.as_bool()).unwrap_or(false) {
                if ready(d) { ClusterState::Running } else { ClusterState::Starting }
            } else {
                ClusterState::Stopped
            }
        }
    }
}

/// `kubectl …` v uzlu; chyba = stderr.
pub fn kubectl(d: &Docker, args: &[&str]) -> Result<String, String> {
    let mut cmd = vec!["kubectl"];
    cmd.extend_from_slice(args);
    let out = container_fs::exec(d, NAME, &cmd).map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if out.code == 0 {
        Ok(stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { stdout } else { err })
    }
}

pub fn ready(d: &Docker) -> bool {
    kubectl(d, &["get", "--raw", "/readyz"]).map(|s| s.trim() == "ok").unwrap_or(false)
}

fn logs_tail(d: &Docker) -> String {
    d.request("GET", &format!("/containers/{}/logs?stdout=1&stderr=1&tail=20", enc(NAME)))
        .call()
        .ok()
        .and_then(|r| {
            let mut buf = Vec::new();
            std::io::Read::read_to_end(&mut r.into_reader(), &mut buf).ok()?;
            let (o, e) = container_fs::demux(&buf);
            Some(format!("{}{}", String::from_utf8_lossy(&o), String::from_utf8_lossy(&e)))
        })
        .unwrap_or_default()
}

/// Počká, až API uzlu odpoví `ok` a uzel bude Ready.
pub fn wait_ready(d: &Docker, mut progress: impl FnMut(&str)) -> Result<(), String> {
    let start = Instant::now();
    progress(t!("Čekám na API clusteru…", "Waiting for the cluster API…"));
    while start.elapsed() < READY_TIMEOUT {
        if ready(d) {
            if kubectl(d, &["get", "nodes", "--no-headers"]).map(|s| s.contains(" Ready")).unwrap_or(false) {
                return Ok(());
            }
            progress(t!("API běží, čekám na uzel…", "API is up, waiting for the node…"));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    Err(timeout_error(&logs_tail(d)))
}

/// Vytvoří síť, svazek a kontejner uzlu (image stáhne, pokud chybí) a spustí ho.
pub fn create(d: &Docker, image: &str, api_port: u16, range: (u16, u16), mut progress: impl FnMut(&str)) -> Result<(), String> {
    if d.get::<Value>(&format!("/images/{}/json", enc(image))).is_err() {
        progress(&tf!("Stahuji {image} (~300 MB)…", "Pulling {image} (~300 MB)…"));
        d.pull(image)?;
    }
    progress(t!("Vytvářím kontejner uzlu…", "Creating the node container…"));
    let ok_or_conflict = |r: Result<String, crate::error::ApiError>| match r {
        Ok(_) => Ok(()),
        Err(e) if e.is_conflict() => Ok(()),
        Err(e) => Err(e.to_string()),
    };
    ok_or_conflict(d.post_json("/networks/create", json!({ "Name": NETWORK, "Driver": "bridge" })))?;
    ok_or_conflict(d.post_json("/volumes/create", json!({ "Name": VOLUME })))?;
    d.post_json(&format!("/containers/create?name={}", enc(NAME)), create_body(image, api_port, range)).map_err(|e| e.to_string())?;
    start(d)
}

pub fn start(d: &Docker) -> Result<(), String> {
    d.container_action(NAME, "start").map(|_| ()).map_err(|e| {
        let e = e.to_string();
        port_conflict_message(&e).unwrap_or(e)
    })
}

pub fn stop(d: &Docker) -> Result<(), String> {
    d.post(&format!("/containers/{}/stop?t=20", enc(NAME))).map(|_| ()).or_else(|e| if e.status() == Some(304) { Ok(()) } else { Err(e.to_string()) })
}

/// Smaže kontejner, svazek s daty clusteru i síť.
pub fn delete(d: &Docker) -> Result<(), String> {
    let ignore_404 = |r: Result<String, crate::error::ApiError>| match r {
        Err(e) if !e.is_not_found() => Err(e.to_string()),
        _ => Ok(()),
    };
    ignore_404(d.delete(&format!("/containers/{}?force=1&v=1", enc(NAME))))?;
    ignore_404(d.delete(&format!("/volumes/{}?force=1", enc(VOLUME))))?;
    ignore_404(d.delete(&format!("/networks/{}", enc(NETWORK))))
}

/// PID kontejneru uzlu v distribuci (pro cestu k síťovým namespace podů).
pub fn node_pid(d: &Docker) -> Option<u32> {
    let v: Value = d.get(&format!("/containers/{}/json", enc(NAME))).ok()?;
    v.pointer("/State/Pid").and_then(|p| p.as_u64()).filter(|p| *p > 0).map(|p| p as u32)
}

fn cli(engine: Engine) -> &'static str {
    match engine {
        Engine::Docker => "docker",
        Engine::Podman => "podman",
    }
}

/// `kubectl apply -n <ns> -f -` s manifestem na stdin (CLI enginu jako root v distribuci).
pub fn apply(distro: &str, engine: Engine, ns: &str, manifest: &str) -> Result<String, String> {
    wsl::run_stdin(distro, Some("root"), &[cli(engine), "exec", "-i", NAME, "kubectl", "apply", "-n", ns, "-f", "-"], manifest).map_err(|e| e.to_string())
}

/// Načte image z enginu do containerd clusteru (bez registry).
pub fn load_image(distro: &str, engine: Engine, image: &str) -> Result<String, String> {
    let script = r#"set -e; "$1" save "$2" | "$1" exec -i wslcd-k3s ctr -n k8s.io images import -"#;
    wsl::run(distro, Some("root"), None, &["sh", "-c", script, "_", cli(engine), image]).map_err(|e| e.to_string())
}

/// Cesta k síťovému namespace podu uvnitř uzlu (`/var/run/netns/cni-…`).
pub fn pod_netns(d: &Docker, ns: &str, pod: &str) -> Result<String, String> {
    let out = container_fs::exec(d, NAME, &["crictl", "pods", "--namespace", ns, "--name", pod, "--state", "ready", "-q"]).map_err(|e| e.to_string())?;
    let id = String::from_utf8_lossy(&out.stdout).lines().next().unwrap_or_default().trim().to_string();
    if id.is_empty() {
        return Err(tf!("Pod {pod} neběží", "Pod {pod} is not running"));
    }
    let tmpl = r#"{{range .info.runtimeSpec.linux.namespaces}}{{if eq .type "network"}}{{.path}}{{end}}{{end}}"#;
    let out = container_fs::exec(d, NAME, &["crictl", "inspectp", "-o", "go-template", "--template", tmpl, &id]).map_err(|e| e.to_string())?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if path.starts_with("/var/run/netns/") && !path.contains("..") { Ok(path) } else { Err(format!("netns: {path}")) }
}

/// Skript sondy: `$1` PID uzlu, `$2` cesta k netns, `$3` IP, `$4` port. Výsledek `OPEN` / `CLOSED`.
pub const PROBE_SCRIPT: &str = r#"case "$1$4" in *[!0-9]*) exit 2;; esac
NS="/proc/$1/root$2"
if command -v bash >/dev/null 2>&1; then
  nsenter --net="$NS" timeout 3 bash -c 'exec 3<>/dev/tcp/$0/$1' "$3" "$4" 2>/dev/null && echo OPEN || echo CLOSED
else
  nsenter --net="$NS" nc -z -w 3 "$3" "$4" 2>/dev/null && echo OPEN || echo CLOSED
fi"#;

/// Zkusí TCP spojení z podu (v jeho síťovém namespace, takže platí jeho politiky).
pub fn probe(distro: &str, node_pid: u32, netns: &str, ip: &str, port: u16) -> Result<bool, String> {
    let (pid, port) = (node_pid.to_string(), port.to_string());
    let out = wsl::run(distro, Some("root"), None, &["sh", "-c", PROBE_SCRIPT, "_", &pid, netns, ip, &port]).map_err(|e| e.to_string())?;
    Ok(out.trim_end().ends_with("OPEN"))
}

/// Sonda z Windows na `127.0.0.1:<port>` (NodePort).
pub fn probe_localhost(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_secs(3)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_body_shape() {
        let b = create_body(DEFAULT_IMAGE, 6443, (30080, 30099));
        assert_eq!(b["HostConfig"]["Privileged"], true);
        assert_eq!(b["HostConfig"]["CgroupnsMode"], "host");
        assert_eq!(b["HostConfig"]["NetworkMode"], NETWORK);
        assert_eq!(b["HostConfig"]["PortBindings"]["6443/tcp"][0], json!({ "HostIp": "127.0.0.1", "HostPort": "6443" }));
        assert_eq!(b["HostConfig"]["PortBindings"].as_object().unwrap().len(), 21);
        assert_eq!(b["HostConfig"]["PortBindings"]["30099/tcp"][0]["HostPort"], "30099");
        assert_eq!(b["HostConfig"]["Binds"][0], "wslcd-k3s-data:/var/lib/rancher/k3s");
        assert!(b["HostConfig"]["Tmpfs"].get("/run").is_some() && b["HostConfig"]["Tmpfs"].get("/var/run").is_some());
        assert_eq!(b["Cmd"], json!(["server", "--disable=traefik", "--tls-san=127.0.0.1", "--write-kubeconfig-mode=600"]));
        // jiný port API na hostiteli míří na 6443 v kontejneru
        let b = create_body(DEFAULT_IMAGE, 16443, (30080, 30080));
        assert_eq!(b["HostConfig"]["PortBindings"]["6443/tcp"][0]["HostPort"], "16443");
        assert!(b["HostConfig"]["PortBindings"].get("16443/tcp").is_none());
    }

    #[test]
    fn port_conflicts() {
        let d = port_conflict_message("driver failed programming external connectivity: Bind for 127.0.0.1:6443 failed: port is already allocated").unwrap();
        assert!(d.contains("6443"), "{d}");
        let p = port_conflict_message("rootlessport listen tcp 127.0.0.1:30085: bind: address already in use").unwrap();
        assert!(p.contains("30085"), "{p}");
        assert!(port_conflict_message("no such image").is_none());
    }

    #[test]
    fn ready_timeout_message() {
        let log: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        let m = timeout_error(&log);
        assert!(m.contains("line 20") && m.contains("line 13") && !m.contains("line 12\n"));
        assert!(m.contains("90"));
    }

    #[test]
    fn probe_script_rejects_non_numeric() {
        // PID a port jdou jako argumenty a skript je ověří
        assert!(PROBE_SCRIPT.starts_with(r#"case "$1$4" in *[!0-9]*) exit 2;; esac"#));
        assert!(!PROBE_SCRIPT.contains("{"), "nic se nevkládá do textu skriptu");
    }
}
