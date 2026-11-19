//! Detail kontejneru z `GET /containers/{id}/json` (Docker i Podman) a převod
//! na `RunSpec` pro „Znovu vytvořit se změnami“.

use crate::worker::RunSpec;
use serde_json::Value;

/// Publikovaný (nebo jen vystavený) port.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PortInfo {
    /// port v kontejneru včetně protokolu („80/tcp“)
    pub container: String,
    pub host_ip: String,
    /// prázdné = port není publikovaný
    pub host_port: String,
}

impl PortInfo {
    pub fn text(&self) -> String {
        if self.host_port.is_empty() {
            return self.container.clone();
        }
        let ip = if self.host_ip.is_empty() { "0.0.0.0" } else { &self.host_ip };
        format!("{ip}:{} \u{2192} {}", self.host_port, self.container)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MountInfo {
    /// volume / bind / tmpfs …
    pub typ: String,
    pub name: String,
    pub source: String,
    pub destination: String,
    pub rw: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NetInfo {
    pub name: String,
    pub ip: String,
    pub prefix: i64,
    pub gateway: String,
    pub ip6: String,
    pub mac: String,
    pub aliases: Vec<String>,
    /// pevné adresy z `IPAMConfig` (Docker; Podman 5.4 je v inspectu nehlásí)
    pub static_ip4: String,
    pub static_ip6: String,
}

impl NetInfo {
    /// Pevná adresa známá z inspectu (`IPAMConfig`).
    pub fn has_static(&self) -> bool {
        !self.static_ip4.is_empty() || !self.static_ip6.is_empty()
    }
}

impl MountInfo {
    /// Anonymní volume (z `VOLUME` v image nebo `-v /cesta`) – název je vygenerovaný hash.
    pub fn is_anonymous(&self) -> bool {
        self.typ == "volume" && is_anonymous_volume(&self.name)
    }
}

/// Vygenerovaný název anonymního volume (64 hex znaků – Docker i Podman).
pub fn is_anonymous_volume(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Výchozí síť enginu (Docker `bridge`, Podman `podman`) – nepodporuje aliasy ani pevné IP.
pub fn is_default_net(name: &str) -> bool {
    matches!(name, "bridge" | "podman" | "default")
}

/// Cíl v kontejneru z řádku `-v` (`zdroj:/cil[:volby]`, `/cil`, `C:\data:/cil`), bez koncového `/`.
pub fn bind_dest(line: &str) -> String {
    let parts: Vec<&str> = line.trim().split(':').collect();
    let dest = match parts.as_slice() {
        [d] => *d,
        // windowsová cesta s písmenem disku
        [drive, _, d, ..] if drive.len() == 1 && d.starts_with('/') => *d,
        [_, d, ..] => *d,
        [] => "",
    };
    let t = dest.trim_end_matches('/');
    if t.is_empty() { dest.to_string() } else { t.to_string() }
}

/// Úpravy sítí pro „Znovu vytvořit“ (hlavní síť je `RunSpec::network`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NetEdit {
    /// další původní sítě, ke kterým se nový kontejner připojí (vedle hlavní)
    pub keep: Vec<String>,
    /// sítě, ve kterých se zachová současná adresa (IP, IPv6, MAC), i když ji engine nehlásí jako pevnou
    pub pin: Vec<String>,
}

/// Rozebraný výsledek inspect.
#[derive(Debug, Clone, Default)]
pub struct Detail {
    pub id: String,
    pub name: String,
    /// image tak, jak byl zadán (Config.Image)
    pub image: String,
    pub image_id: String,
    pub status: String,
    pub running: bool,
    pub paused: bool,
    pub exit_code: i64,
    pub error: String,
    pub health: Option<String>,
    pub created: String,
    pub started: String,
    pub finished: String,
    pub cmd: Vec<String>,
    pub entrypoint: Vec<String>,
    /// „no“, „always“, „unless-stopped“, „on-failure“, „on-failure:3“
    pub restart: String,
    pub ports: Vec<PortInfo>,
    /// konfigurované publikování portů ve tvaru pro `-p`
    pub port_specs: Vec<String>,
    pub mounts: Vec<MountInfo>,
    pub networks: Vec<NetInfo>,
    pub network_mode: String,
    pub env: Vec<(String, String)>,
    pub labels: Vec<(String, String)>,
    pub hostname: String,
    pub working_dir: String,
    pub user: String,
    pub tty: bool,
    /// výchozí hodnoty z image (doplní worker z `/images/{id}/json`)
    pub image_env: Vec<String>,
    pub image_cmd: Vec<String>,
    pub image_entrypoint: Vec<String>,
    /// limity prostředků z HostConfig (paměť, CPU, PIDs)
    pub limits: crate::limits::Limits,
    /// celé JSON (pro kopírování)
    pub raw: String,
}

fn s(v: &Value, p: &str) -> String {
    v.pointer(p).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn strs(v: &Value, p: &str) -> Vec<String> {
    match v.pointer(p) {
        Some(Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
        // Docker umí vrátit i řetězec (starší API)
        Some(Value::String(x)) if !x.is_empty() => vec![x.clone()],
        _ => Vec::new(),
    }
}

/// Klíč proměnné, jejíž hodnotu v přehledu skrýváme.
pub fn is_secret(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    [
        "PASS", "PWD", "SECRET", "TOKEN", "KEY", "AUTH", "CREDENTIAL", "PRIVATE", "DSN", "DATABASE_URL", "CONNECTION_STRING",
        "COOKIE", "SESSION", "SALT", "SIGNATURE",
    ]
    .iter()
    .any(|w| k.contains(w))
}

fn port_num(p: &str) -> u32 {
    p.split('/').next().and_then(|n| n.parse().ok()).unwrap_or(u32::MAX)
}

/// `{"80/tcp": [{"HostIp": "", "HostPort": "8080"}], "443/tcp": null}` -> seznam portů.
fn port_map(v: Option<&Value>) -> Vec<PortInfo> {
    let mut out = Vec::new();
    let Some(m) = v.and_then(|m| m.as_object()) else { return out };
    for (cport, binds) in m {
        let list = binds.as_array().filter(|a| !a.is_empty());
        match list {
            None => out.push(PortInfo { container: cport.clone(), ..Default::default() }),
            Some(a) => {
                for b in a {
                    let p = PortInfo {
                        container: cport.clone(),
                        host_ip: s(b, "/HostIp"),
                        host_port: s(b, "/HostPort"),
                    };
                    if !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| port_num(&a.container).cmp(&port_num(&b.container)).then(a.host_ip.cmp(&b.host_ip)));
    out
}

/// Port ve tvaru pro `docker run -p` (`[ip:]hostport:port[/udp]`).
fn port_spec(p: &PortInfo) -> Option<String> {
    let (num, proto) = p.container.split_once('/').unwrap_or((&p.container, "tcp"));
    let cport = if proto == "tcp" { num.to_string() } else { format!("{num}/{proto}") };
    let ip = match p.host_ip.as_str() {
        "" | "0.0.0.0" | "::" => "",
        x => x,
    };
    if p.host_port.is_empty() && ip.is_empty() {
        // publikováno na náhodný port
        return Some(cport);
    }
    let ip = if ip.contains(':') { format!("[{ip}]") } else { ip.to_string() };
    Some(if ip.is_empty() { format!("{}:{cport}", p.host_port) } else { format!("{ip}:{}:{cport}", p.host_port) })
}

pub fn parse(v: &Value) -> Detail {
    let mut d = Detail {
        id: s(v, "/Id"),
        name: s(v, "/Name").trim_start_matches('/').to_string(),
        image: s(v, "/Config/Image"),
        image_id: s(v, "/Image"),
        status: s(v, "/State/Status"),
        running: v.pointer("/State/Running").and_then(|x| x.as_bool()).unwrap_or(false),
        paused: v.pointer("/State/Paused").and_then(|x| x.as_bool()).unwrap_or(false),
        exit_code: v.pointer("/State/ExitCode").and_then(|x| x.as_i64()).unwrap_or(0),
        error: s(v, "/State/Error"),
        created: s(v, "/Created"),
        started: s(v, "/State/StartedAt"),
        finished: s(v, "/State/FinishedAt"),
        cmd: strs(v, "/Config/Cmd"),
        entrypoint: strs(v, "/Config/Entrypoint"),
        network_mode: s(v, "/HostConfig/NetworkMode"),
        hostname: s(v, "/Config/Hostname"),
        working_dir: s(v, "/Config/WorkingDir"),
        user: s(v, "/Config/User"),
        tty: v.pointer("/Config/Tty").and_then(|x| x.as_bool()).unwrap_or(false),
        limits: v.get("HostConfig").map(crate::limits::Limits::from_host_config).unwrap_or_default(),
        raw: serde_json::to_string_pretty(v).unwrap_or_default(),
        ..Default::default()
    };
    if d.image.is_empty() {
        d.image = d.image_id.clone();
    }
    // zdraví: Docker State.Health, starší Podman State.Healthcheck
    let h = s(v, "/State/Health/Status");
    let h = if h.is_empty() { s(v, "/State/Healthcheck/Status") } else { h };
    d.health = if h.is_empty() { None } else { Some(h) };

    let rp = s(v, "/HostConfig/RestartPolicy/Name");
    let max = v.pointer("/HostConfig/RestartPolicy/MaximumRetryCount").and_then(|x| x.as_i64()).unwrap_or(0);
    d.restart = match rp.as_str() {
        "" | "no" => "no".into(),
        "on-failure" if max > 0 => format!("on-failure:{max}"),
        x => x.into(),
    };

    // skutečné porty (bez IPv6 duplicit)
    let actual = port_map(v.pointer("/NetworkSettings/Ports"));
    d.ports = actual
        .iter()
        .filter(|p| !(p.host_ip.contains(':') && actual.iter().any(|q| q.container == p.container && q.host_port == p.host_port && !q.host_ip.contains(':'))))
        .cloned()
        .collect();
    // konfigurované porty (HostConfig), jinak skutečné
    let configured = port_map(v.pointer("/HostConfig/PortBindings"));
    let src = if configured.iter().any(|p| !p.host_port.is_empty() || !p.host_ip.is_empty()) { configured } else { d.ports.clone() };
    for p in src.iter().filter(|p| !p.host_port.is_empty() || !p.host_ip.is_empty()) {
        if let Some(sp) = port_spec(p) {
            if !d.port_specs.contains(&sp) {
                d.port_specs.push(sp);
            }
        }
    }

    for m in v.get("Mounts").and_then(|m| m.as_array()).into_iter().flatten() {
        d.mounts.push(MountInfo {
            typ: s(m, "/Type"),
            name: s(m, "/Name"),
            source: s(m, "/Source"),
            destination: s(m, "/Destination"),
            rw: m.get("RW").and_then(|x| x.as_bool()).unwrap_or(true),
        });
    }
    d.mounts.sort_by(|a, b| a.destination.cmp(&b.destination));

    if let Some(nets) = v.pointer("/NetworkSettings/Networks").and_then(|n| n.as_object()) {
        for (name, n) in nets {
            d.networks.push(NetInfo {
                name: name.clone(),
                ip: s(n, "/IPAddress"),
                prefix: n.get("IPPrefixLen").and_then(|x| x.as_i64()).unwrap_or(0),
                gateway: s(n, "/Gateway"),
                ip6: s(n, "/GlobalIPv6Address"),
                mac: s(n, "/MacAddress"),
                aliases: strs(n, "/Aliases"),
                static_ip4: s(n, "/IPAMConfig/IPv4Address"),
                static_ip6: s(n, "/IPAMConfig/IPv6Address"),
            });
        }
    }
    d.networks.sort_by(|a, b| a.name.cmp(&b.name));

    d.env = strs(v, "/Config/Env")
        .into_iter()
        .map(|e| match e.split_once('=') {
            Some((k, val)) => (k.to_string(), val.to_string()),
            None => (e, String::new()),
        })
        .collect();
    if let Some(l) = v.pointer("/Config/Labels").and_then(|l| l.as_object()) {
        d.labels = l.iter().map(|(k, x)| (k.clone(), x.as_str().unwrap_or("").to_string())).collect();
        d.labels.sort();
    }
    d
}

/// Výchozí nastavení image, `GET /images/{id}/json`.
pub fn set_image_defaults(d: &mut Detail, img: &Value) {
    d.image_env = strs(img, "/Config/Env");
    d.image_cmd = strs(img, "/Config/Cmd");
    d.image_entrypoint = strs(img, "/Config/Entrypoint");
}

/// Shellové uvozovky pro argument příkazu (jen když jsou potřeba).
pub fn quote_arg(a: &str) -> String {
    if !a.is_empty() && a.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@,+%".contains(c)) {
        return a.to_string();
    }
    format!("'{}'", a.replace('\'', r"'\''"))
}

impl Detail {
    pub fn compose_project(&self) -> Option<&str> {
        self.labels.iter().find(|(k, _)| k == "com.docker.compose.project" || k == "io.podman.compose.project").map(|(_, v)| v.as_str())
    }

    /// Síť pro `--network` (prázdné = výchozí bridge).
    pub fn network_for_spec(&self) -> String {
        let m = self.network_mode.as_str();
        if m == "host" || m == "none" || m.starts_with("container:") {
            return m.to_string();
        }
        let named: Vec<&str> = self.networks.iter().map(|n| n.name.as_str()).filter(|n| !matches!(*n, "bridge" | "podman")).collect();
        if !matches!(m, "" | "default" | "bridge" | "podman" | "slirp4netns" | "pasta" | "private") && named.contains(&m) {
            return m.to_string();
        }
        named.first().map(|s| s.to_string()).unwrap_or_default()
    }

    /// Proměnné zadané uživatelem (bez výchozích z image a těch, které přidává engine).
    pub fn user_env(&self) -> Vec<String> {
        self.env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .filter(|e| !self.image_env.contains(e))
            .filter(|e| e != "container=podman" && e != "HOME=/root")
            .filter(|e| !(self.tty && e == "TERM=xterm"))
            .filter(|e| !(e.starts_with("HOSTNAME=") && e[9..] == self.hostname))
            .collect()
    }

    /// Volume / bind mounty pro `-v` (bez anonymních volume – ty se při znovuvytvoření
    /// připojí samy, viz `anonymous_volumes`).
    pub fn volume_specs(&self) -> Vec<String> {
        self.mounts
            .iter()
            .filter(|m| !m.is_anonymous())
            .filter_map(|m| {
                let src = match m.typ.as_str() {
                    "volume" if !m.name.is_empty() => &m.name,
                    "bind" => &m.source,
                    _ => return None,
                };
                Some(format!("{src}:{}{}", m.destination, if m.rw { "" } else { ":ro" }))
            })
            .collect()
    }

    /// Anonymní volume, které se při znovuvytvoření připojí podle názvu – ty, jejichž cíl
    /// formulář (`volumes`) sám neuvádí.
    pub fn anonymous_volumes(&self, volumes: &str) -> Vec<&MountInfo> {
        let dests: Vec<String> = volumes.lines().filter(|l| !l.trim().is_empty()).map(bind_dest).collect();
        self.mounts.iter().filter(|m| m.is_anonymous() && !dests.contains(&bind_dest(&m.destination))).collect()
    }

    /// Síťový režim bez vlastních sítí (host, none, container:…).
    pub fn special_network_mode(&self) -> bool {
        let m = self.network_mode.as_str();
        m == "host" || m == "none" || m.starts_with("container:")
    }

    /// Výchozí úpravy sítí: zachovat všechny původní sítě a adresy, které jsou známé jako
    /// pevné. Podman pevné adresy v inspectu nehlásí – u něj se současná adresa ponechá
    /// ve všech vlastních sítích.
    pub fn net_edit(&self, podman: bool) -> NetEdit {
        if self.special_network_mode() {
            return NetEdit::default();
        }
        let primary = self.network_for_spec();
        NetEdit {
            keep: self
                .networks
                .iter()
                .map(|n| n.name.clone())
                .filter(|n| *n != primary && !(primary.is_empty() && is_default_net(n)))
                .collect(),
            pin: self
                .networks
                .iter()
                .filter(|n| !is_default_net(&n.name) && (n.has_static() || (podman && !n.ip.is_empty())))
                .map(|n| n.name.clone())
                .collect(),
        }
    }

    /// Příkaz liší od výchozího z image.
    fn custom_cmd(&self) -> bool {
        self.cmd != self.image_cmd || self.entrypoint != self.image_entrypoint
    }

    /// Kontejner má jiný entrypoint než image – `RunSpec` ho nepřenese.
    pub fn entrypoint_differs(&self) -> bool {
        !self.entrypoint.is_empty() && self.entrypoint != self.image_entrypoint
    }

    /// Formulář „Znovu vytvořit“ předvyplněný z kontejneru.
    pub fn to_run_spec(&self) -> RunSpec {
        RunSpec {
            image: crate::engine::short_image(&self.image),
            name: self.name.clone(),
            ports: self.port_specs.join(", "),
            env: self.user_env().join("\n"),
            volumes: self.volume_specs().join("\n"),
            network: self.network_for_spec(),
            restart: self.restart.clone(),
            command: if self.custom_cmd() { self.cmd.iter().map(|a| quote_arg(a)).collect::<Vec<_>>().join(" ") } else { String::new() },
            interactive: false,
        }
    }
}

/// RFC 3339 (`2026-11-18T18:36:14.457Z`, `…+01:00`) -> Unix čas. Nulové datum (rok 1) -> None.
pub fn parse_time(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| t.get(r).and_then(|x| x.parse::<i64>().ok());
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    if y < 1971 {
        return None;
    }
    // časové pásmo za zlomkem sekund
    let rest = &t[19..];
    let tz = rest.find(['Z', '+', '-']).map(|i| &rest[i..]).unwrap_or("Z");
    let off = if tz.starts_with('Z') || tz.len() < 6 {
        0
    } else {
        let sign = if tz.starts_with('-') { -1 } else { 1 };
        sign * (tz[1..3].parse::<i64>().ok()? * 3600 + tz[4..6].parse::<i64>().ok()? * 60)
    };
    // dny od 1970-01-01 (Howard Hinnant, days_from_civil)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + h * 3600 + mi * 60 + se - off)
}

/// `2026-11-18T18:36:14.457Z` -> `2026-11-18 18:36:14 UTC`; nulové datum -> prázdné.
pub fn fmt_time(t: &str) -> String {
    if parse_time(t).is_none() {
        return String::new();
    }
    let base = t[..19].replace('T', " ");
    let rest = &t[19..];
    match rest.find(['Z', '+', '-']).map(|i| &rest[i..]) {
        Some(z) if !z.starts_with('Z') => format!("{base} {z}"),
        _ => format!("{base} UTC"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zkrácený výstup Podmanu 5.4 (`podman run --network wls-test-net -p 18080:80
    /// -p 127.0.0.1:18443:443/udp -e DB_PASSWORD=… -v wls-test-vol:/data -v /tmp:/host-tmp:ro …`).
    const PODMAN: &str = r#"{
      "Id": "deb4928f5176ccb536ef6fa2aa14a3d807ac2016b3e09688c56a2263d0c91754",
      "Created": "2026-11-18T18:36:14.457324752Z",
      "Path": "sh", "Args": ["-c", "echo \"hi there\"; sleep 300"],
      "State": {"Status": "running", "Running": true, "Paused": false, "ExitCode": 0, "Error": "",
                "StartedAt": "2026-11-18T18:36:14.605588542Z", "FinishedAt": "0001-01-01T00:00:00Z"},
      "Image": "sha256:bf8527eb54c3680e728d5b4b383a8ba730d72dae7236fbc8dff97ed6b224a731",
      "Name": "/wls-test-shape",
      "HostConfig": {
        "PortBindings": {"443/udp": [{"HostIp": "127.0.0.1", "HostPort": "18443"}], "80/tcp": [{"HostIp": "0.0.0.0", "HostPort": "18080"}]},
        "RestartPolicy": {"Name": "unless-stopped", "MaximumRetryCount": 0},
        "NetworkMode": "bridge",
        "Binds": ["/tmp:/host-tmp:ro,rprivate,nosuid,nodev,rbind", "wls-test-vol:/data:rw,rprivate,nosuid,nodev,rbind"],
        "Mounts": null
      },
      "Mounts": [
        {"Type": "volume", "Name": "wls-test-vol", "Source": "/var/lib/containers/storage/volumes/wls-test-vol/_data", "Destination": "/data", "Driver": "local", "Mode": "", "RW": true},
        {"Type": "bind", "Source": "/tmp", "Destination": "/host-tmp", "Mode": "", "RW": false}
      ],
      "Config": {
        "Hostname": "deb4928f5176",
        "Env": ["container=podman", "DB_PASSWORD=secret1", "PLAIN=v", "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin", "HOME=/root", "HOSTNAME=deb4928f5176"],
        "Cmd": ["sh", "-c", "echo \"hi there\"; sleep 300"],
        "Image": "docker.io/library/alpine:3.20",
        "Entrypoint": [],
        "Labels": {"a.b": "c"},
        "Tty": false, "WorkingDir": "/", "User": ""
      },
      "NetworkSettings": {
        "Ports": {"443/udp": [{"HostIp": "127.0.0.1", "HostPort": "18443"}], "80/tcp": [{"HostIp": "0.0.0.0", "HostPort": "18080"}]},
        "Networks": {"wls-test-net": {"Aliases": ["deb4928f5176"], "MacAddress": "f2:30:38:e9:2d:c4", "Gateway": "10.89.0.1",
                     "IPAddress": "10.89.0.2", "IPPrefixLen": 24, "GlobalIPv6Address": ""}}
      }
    }"#;

    /// Typický výstup Dockeru (compose služba s healthcheckem, IPv6 duplicitní port, null pole).
    const DOCKER: &str = r#"{
      "Id": "4f1c2a",
      "Created": "2025-03-01T10:00:00.5Z",
      "State": {"Status": "running", "Running": true, "Paused": false, "ExitCode": 0, "Error": "",
                "StartedAt": "2025-03-01T10:00:01Z", "FinishedAt": "0001-01-01T00:00:00Z",
                "Health": {"Status": "healthy", "FailingStreak": 0, "Log": []}},
      "Image": "sha256:aaaa",
      "Name": "/site-web-1",
      "HostConfig": {
        "PortBindings": {"80/tcp": [{"HostIp": "", "HostPort": "8080"}]},
        "RestartPolicy": {"Name": "on-failure", "MaximumRetryCount": 3},
        "NetworkMode": "site_default"
      },
      "Mounts": [
        {"Type": "volume", "Name": "site_data", "Source": "/var/lib/docker/volumes/site_data/_data", "Destination": "/usr/share/nginx/html", "Driver": "local", "Mode": "z", "RW": true},
        {"Type": "tmpfs", "Source": "", "Destination": "/run", "RW": true}
      ],
      "Config": {
        "Hostname": "4f1c2a",
        "Env": ["API_KEY=abc", "PATH=/usr/local/sbin:/usr/bin", "NGINX_VERSION=1.27"],
        "Cmd": ["nginx", "-g", "daemon off;"],
        "Image": "nginx:alpine",
        "Entrypoint": ["/docker-entrypoint.sh"],
        "Labels": {"com.docker.compose.project": "site", "com.docker.compose.service": "web"},
        "Tty": false
      },
      "NetworkSettings": {
        "Ports": {"80/tcp": [{"HostIp": "0.0.0.0", "HostPort": "8080"}, {"HostIp": "::", "HostPort": "8080"}], "443/tcp": null},
        "Networks": {"site_default": {"Aliases": ["web"], "Gateway": "172.18.0.1", "IPAddress": "172.18.0.2", "IPPrefixLen": 16, "MacAddress": "02:42:ac:12:00:02"}}
      }
    }"#;

    #[test]
    fn podman_shape() {
        let v: Value = serde_json::from_str(PODMAN).unwrap();
        let mut d = parse(&v);
        set_image_defaults(&mut d, &serde_json::json!({"Config": {"Env": ["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"], "Cmd": ["/bin/sh"]}}));
        assert_eq!(d.name, "wls-test-shape");
        assert_eq!(d.image, "docker.io/library/alpine:3.20");
        assert!(d.running && d.health.is_none());
        assert_eq!(d.restart, "unless-stopped");
        assert_eq!(d.ports.iter().map(|p| p.text()).collect::<Vec<_>>(), vec!["0.0.0.0:18080 \u{2192} 80/tcp", "127.0.0.1:18443 \u{2192} 443/udp"]);
        assert_eq!(d.networks[0].ip, "10.89.0.2");
        assert_eq!(d.networks[0].prefix, 24);
        assert_eq!(fmt_time(&d.created), "2026-11-18 18:36:14 UTC");
        assert_eq!(fmt_time(&d.finished), "");
        assert!(d.env.iter().any(|(k, v)| k == "DB_PASSWORD" && v == "secret1"));

        let r = d.to_run_spec();
        assert_eq!(r.image, "alpine:3.20");
        assert_eq!(r.name, "wls-test-shape");
        assert_eq!(r.ports, "18080:80, 127.0.0.1:18443:443/udp");
        assert_eq!(r.env, "DB_PASSWORD=secret1\nPLAIN=v");
        assert_eq!(r.volumes, "wls-test-vol:/data\n/tmp:/host-tmp:ro");
        assert_eq!(r.network, "wls-test-net");
        assert_eq!(r.restart, "unless-stopped");
        assert_eq!(r.command, r#"sh -c 'echo "hi there"; sleep 300'"#);
        assert!(!d.entrypoint_differs());
        // příkaz se po rozdělení vrátí do původních argumentů
        let a = r.args();
        assert_eq!(&a[a.len() - 3..], &["sh".to_string(), "-c".into(), "echo \"hi there\"; sleep 300".into()]);
    }

    #[test]
    fn docker_shape() {
        let v: Value = serde_json::from_str(DOCKER).unwrap();
        let mut d = parse(&v);
        set_image_defaults(
            &mut d,
            &serde_json::json!({"Config": {"Env": ["PATH=/usr/local/sbin:/usr/bin", "NGINX_VERSION=1.27"], "Cmd": ["nginx", "-g", "daemon off;"], "Entrypoint": ["/docker-entrypoint.sh"]}}),
        );
        assert_eq!(d.health.as_deref(), Some("healthy"));
        assert_eq!(d.restart, "on-failure:3");
        // IPv6 duplicita vynechána, nepublikovaný port zůstává
        assert_eq!(d.ports.iter().map(|p| p.text()).collect::<Vec<_>>(), vec!["0.0.0.0:8080 \u{2192} 80/tcp", "443/tcp"]);
        assert_eq!(d.compose_project(), Some("site"));
        assert_eq!(parse_time(&d.created), Some(1_740_823_200));

        let r = d.to_run_spec();
        assert_eq!(r.image, "nginx:alpine");
        assert_eq!(r.ports, "8080:80");
        assert_eq!(r.env, "API_KEY=abc");
        assert_eq!(r.volumes, "site_data:/usr/share/nginx/html");
        assert_eq!(r.network, "site_default");
        assert_eq!(r.command, "", "výchozí příkaz image se nepřenáší");
    }

    #[test]
    fn docker_nulls_and_defaults() {
        let v = serde_json::json!({"Id": "x", "Name": "/a", "Config": {"Env": null, "Cmd": null, "Entrypoint": null, "Labels": null, "Image": "alpine"},
            "HostConfig": {"NetworkMode": "default", "PortBindings": {}, "RestartPolicy": {"Name": ""}}, "Mounts": null,
            "NetworkSettings": {"Ports": {}, "Networks": {"bridge": {"IPAddress": "172.17.0.2"}}}});
        let d = parse(&v);
        let r = d.to_run_spec();
        assert_eq!(r.restart, "no");
        assert_eq!(r.network, "");
        assert!(r.ports.is_empty() && r.env.is_empty() && r.volumes.is_empty() && r.command.is_empty());
    }

    #[test]
    fn secrets_and_time() {
        assert!(is_secret("DB_PASSWORD") && is_secret("api_key") && is_secret("GITHUB_TOKEN") && is_secret("SECRET"));
        assert!(!is_secret("PATH") && !is_secret("HOME"));
        assert_eq!(parse_time("1970-01-02T00:00:00Z"), None);
        assert_eq!(parse_time("2024-02-29T12:00:00+01:00"), Some(1_709_204_400));
        assert_eq!(fmt_time("2024-02-29T12:00:00.1+01:00"), "2024-02-29 12:00:00 +01:00");
        assert_eq!(quote_arg("it's"), r"'it'\''s'");
    }
}
