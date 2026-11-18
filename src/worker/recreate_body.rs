//! Tělo `POST /containers/create` pro „Znovu vytvořit se změnami“: vychází z původního
//! inspectu (Config + HostConfig) a mění jen pole z formuláře. Zachová se tak uživatel,
//! capabilities, security-opt, read-only rootfs, privileged, zařízení, limity, tmpfs,
//! healthcheck, labely, entrypoint, log options… (bezpečnostní nález F-07).

use super::spec::split_args;
use super::RunSpec;
use crate::inspect::{bind_dest, is_anonymous_volume, is_default_net, NetEdit};
use serde_json::{json, Map, Value};

/// Port z formuláře: `8080:80`, `127.0.0.1:8080:80/tcp`, `80`, `53:53/udp`.
/// Vrací (kontejnerový port s protokolem, host IP, host port).
pub fn parse_port(p: &str) -> Result<(String, String, String), String> {
    let p = p.trim();
    let (main, proto) = match p.rsplit_once('/') {
        Some((m, pr)) if !pr.contains(':') => (m, pr),
        _ => (p, "tcp"),
    };
    // IPv6 adresa v hranatých závorkách: [::1]:8080:80
    let (ip, rest) = if let Some(r) = main.strip_prefix('[') {
        let (ip, rest) = r.split_once(']').ok_or_else(|| tf!("Neplatný port: {p}", "Invalid port: {p}"))?;
        (ip.to_string(), rest.trim_start_matches(':').to_string())
    } else {
        (String::new(), main.to_string())
    };
    let parts: Vec<&str> = rest.split(':').collect();
    let (ip, host, cont) = match parts.as_slice() {
        [c] => (ip, String::new(), c.to_string()),
        [h, c] => (ip, h.to_string(), c.to_string()),
        [i, h, c] if ip.is_empty() => (i.to_string(), h.to_string(), c.to_string()),
        _ => return Err(tf!("Neplatný port: {p}", "Invalid port: {p}")),
    };
    let num = |s: &str| s.is_empty() || s.parse::<u16>().is_ok();
    if cont.is_empty() || !num(&cont) || !num(&host) {
        return Err(tf!("Neplatný port (rozsahy nejsou podporované): {p}", "Invalid port (ranges are not supported): {p}"));
    }
    Ok((format!("{cont}/{proto}"), ip, host))
}

fn lines(s: &str) -> impl Iterator<Item = &str> {
    s.lines().map(str::trim).filter(|l| !l.is_empty())
}

/// Režim bez vlastních sítí (host, none, container:…) – žádné další sítě.
fn special_mode(m: &str) -> bool {
    m == "host" || m == "none" || m.starts_with("container:")
}

/// Nastavení koncového bodu sítě (`EndpointSettings`) z původního inspectu: aliasy (bez
/// automatického aliasu s ID kontejneru) a u sítí v `net.pin` adresy – pevné z `IPAMConfig`,
/// a když je engine nehlásí (Podman), současná IP / IPv6 / MAC. Síť, ve které kontejner
/// nebyl, dostane prázdné nastavení.
pub fn endpoint(orig: &Value, name: &str, net: &NetEdit) -> Value {
    let mut ep = Map::new();
    let n = orig.pointer("/NetworkSettings/Networks").and_then(|n| n.get(name));
    let (Some(n), false) = (n, is_default_net(name)) else { return Value::Object(ep) };
    let id = orig.get("Id").and_then(|v| v.as_str()).unwrap_or("");
    let st = |p: &str| n.pointer(p).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let aliases: Vec<String> = n
        .get("Aliases")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_str())
        .filter(|a| !a.is_empty() && !(a.len() >= 12 && id.starts_with(a)))
        .map(String::from)
        .collect();
    if !aliases.is_empty() {
        ep.insert("Aliases".into(), json!(aliases));
    }
    let pinned = net.pin.iter().any(|p| p == name);
    let (mut ip4, mut ip6) = if pinned { (st("/IPAMConfig/IPv4Address"), st("/IPAMConfig/IPv6Address")) } else { Default::default() };
    if pinned && ip4.is_empty() && ip6.is_empty() {
        // engine pevné adresy nehlásí (Podman) – ponechat současné
        ip4 = st("/IPAddress");
        ip6 = st("/GlobalIPv6Address");
        let mac = st("/MacAddress");
        if !mac.is_empty() {
            ep.insert("MacAddress".into(), json!(mac));
        }
    }
    let mut ipam = Map::new();
    if !ip4.is_empty() {
        ipam.insert("IPv4Address".into(), json!(ip4));
    }
    if !ip6.is_empty() {
        ipam.insert("IPv6Address".into(), json!(ip6));
    }
    if !ipam.is_empty() {
        ep.insert("IPAMConfig".into(), Value::Object(ipam));
    }
    Value::Object(ep)
}

/// Další sítě, ke kterým se nový kontejner připojí po vytvoření (`POST /networks/{id}/connect`
/// před startem – starší Docker API přijme při vytvoření jen jeden koncový bod).
pub fn extra_networks(orig: &Value, spec: &RunSpec, net: &NetEdit) -> Vec<(String, Value)> {
    let primary = spec.network.trim();
    if special_mode(primary) {
        return Vec::new();
    }
    let mut out: Vec<(String, Value)> = Vec::new();
    for n in &net.keep {
        // výchozí síť už připojí samotný NetworkMode
        if n == primary || (primary.is_empty() && is_default_net(n)) || out.iter().any(|(o, _)| o == n) {
            continue;
        }
        out.push((n.clone(), endpoint(orig, n, net)));
    }
    out
}

/// Sestaví tělo požadavku. `image` je už plně kvalifikovaný název.
pub fn create_body(orig: &Value, spec: &RunSpec, image: &str, net: &NetEdit) -> Result<Value, String> {
    let mut config = orig.get("Config").cloned().unwrap_or_else(|| json!({}));
    let mut host = orig.get("HostConfig").cloned().unwrap_or_else(|| json!({}));
    let c = config.as_object_mut().ok_or("Config")?;
    let h = host.as_object_mut().ok_or("HostConfig")?;

    // hostname odvozený od ID starého kontejneru by nový kontejner zdědil – nechat engine zvolit
    let old_id = orig.get("Id").and_then(|v| v.as_str()).unwrap_or("");
    if let Some(hn) = c.get("Hostname").and_then(|v| v.as_str()) {
        if !old_id.is_empty() && old_id.starts_with(hn) {
            c.remove("Hostname");
        }
    }
    c.insert("Image".into(), json!(image));
    // Env: engine k nim sám přidá výchozí proměnné image
    c.insert("Env".into(), json!(lines(&spec.env).collect::<Vec<_>>()));
    let cmd = split_args(&spec.command);
    c.insert("Cmd".into(), if cmd.is_empty() { Value::Null } else { json!(cmd) });

    // porty
    let mut exposed = Map::new();
    let mut bindings: Map<String, Value> = Map::new();
    for p in spec.ports.split([',', ' ', '\n']).map(str::trim).filter(|s| !s.is_empty()) {
        let (cport, ip, hport) = parse_port(p)?;
        exposed.insert(cport.clone(), json!({}));
        if !hport.is_empty() || !ip.is_empty() {
            let list = bindings.entry(cport).or_insert_with(|| json!([]));
            if let Some(a) = list.as_array_mut() {
                a.push(json!({"HostIp": ip, "HostPort": hport}));
            }
        }
    }
    // porty, které image jen deklaruje (EXPOSE), ponechat
    if let Some(Value::Object(old)) = c.get("ExposedPorts") {
        for k in old.keys() {
            exposed.entry(k.clone()).or_insert_with(|| json!({}));
        }
    }
    c.insert("ExposedPorts".into(), Value::Object(exposed));
    h.insert("PortBindings".into(), Value::Object(bindings));
    h.insert("PublishAllPorts".into(), json!(false));

    // svazky: formulář je úplný seznam (pojmenované volume i bind mounty) -> Binds;
    // původní Mounts (--mount) by se zdvojily
    let mut binds: Vec<String> = lines(&spec.volumes).map(String::from).collect();
    // anonymní volume (VOLUME v image, `-v /cesta`) připojit znovu podle názvu, ať data
    // přežijí – pokud formulář stejný cíl neuvádí jinak
    let dests: Vec<String> = binds.iter().map(|b| bind_dest(b)).collect();
    for m in orig.get("Mounts").and_then(|m| m.as_array()).into_iter().flatten() {
        let (name, dest) = (m.get("Name").and_then(|v| v.as_str()).unwrap_or(""), m.get("Destination").and_then(|v| v.as_str()).unwrap_or(""));
        if m.get("Type").and_then(|v| v.as_str()) == Some("volume") && is_anonymous_volume(name) && !dest.is_empty() && !dests.contains(&bind_dest(dest)) {
            let ro = if m.get("RW").and_then(|v| v.as_bool()) == Some(false) { ":ro" } else { "" };
            binds.push(format!("{name}:{dest}{ro}"));
        }
    }
    h.insert("Binds".into(), json!(binds));
    h.remove("Mounts");

    let restart = spec.restart.trim();
    h.insert(
        "RestartPolicy".into(),
        json!({"Name": if restart.is_empty() || restart == "no" { "no" } else { restart }, "MaximumRetryCount": 0}),
    );
    // sítě: hlavní přes NetworkMode + jeden koncový bod, ostatní viz `extra_networks`
    let mut networking = None;
    let primary = spec.network.trim();
    if primary.is_empty() {
        // výchozí síť; původní vlastní síť (ze které uživatel přešel na výchozí) nezdědit
        let m = h.get("NetworkMode").and_then(|v| v.as_str()).unwrap_or("");
        let keep_mode = m.is_empty() || is_default_net(m) || ["slirp4netns", "pasta", "private"].iter().any(|p| m.starts_with(p));
        if !keep_mode {
            h.insert("NetworkMode".into(), json!("bridge"));
        }
    } else {
        h.insert("NetworkMode".into(), json!(primary));
        if !special_mode(primary) && !is_default_net(primary) {
            let mut eps = Map::new();
            eps.insert(primary.to_string(), endpoint(orig, primary, net));
            networking = Some(json!({ "EndpointsConfig": eps }));
        }
    }
    // null hodnoty vynechat (engine použije výchozí)
    h.retain(|_, v| !v.is_null());
    // Podman v inspectu hlásí MemorySwappiness 0 i když není nastavené – na cgroup v2 ho
    // crun odmítne („cannot set memory swappiness“); -1/0 = výchozí
    if h.get("MemorySwappiness").and_then(|v| v.as_i64()).is_some_and(|n| n <= 0) {
        h.remove("MemorySwappiness");
    }
    let mut body = json!({ "Config": config, "HostConfig": host });
    if let Some(n) = networking {
        body["NetworkingConfig"] = n;
    }
    Ok(body)
}

/// Docker API chce Config pole na nejvyšší úrovni těla (create = Config + HostConfig + NetworkingConfig).
pub fn flatten(body: Value) -> Value {
    let mut out = body.get("Config").cloned().unwrap_or_else(|| json!({}));
    if let Some(o) = out.as_object_mut() {
        for k in ["HostConfig", "NetworkingConfig"] {
            if let Some(v) = body.get(k) {
                o.insert(k.into(), v.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports() {
        assert_eq!(parse_port("8080:80").unwrap(), ("80/tcp".into(), "".into(), "8080".into()));
        assert_eq!(parse_port("127.0.0.1:53:53/udp").unwrap(), ("53/udp".into(), "127.0.0.1".into(), "53".into()));
        assert_eq!(parse_port("80").unwrap(), ("80/tcp".into(), "".into(), "".into()));
        assert_eq!(parse_port("[::1]:8080:80").unwrap(), ("80/tcp".into(), "::1".into(), "8080".into()));
        assert!(parse_port("8000-8010:80").is_err());
        assert!(parse_port("a:b:c:d").is_err());
    }

    #[test]
    fn keeps_security_settings_and_applies_changes() {
        let orig = json!({
            "Id": "abcdef1234567890",
            "Config": {"Hostname": "abcdef123456", "User": "1000", "Image": "old", "Env": ["A=1"], "Cmd": ["x"],
                       "Entrypoint": ["/ep"], "Labels": {"l": "v"}, "ExposedPorts": {"9000/tcp": {}}},
            "HostConfig": {"CapDrop": ["ALL"], "ReadonlyRootfs": true, "SecurityOpt": ["no-new-privileges"],
                           "Memory": 1048576, "Mounts": [{"Type": "volume"}], "NetworkMode": "bridge",
                           "PortBindings": {"1/tcp": [{"HostPort": "1"}]}, "ContainerIDFile": null}
        });
        let spec = RunSpec {
            env: "B=2\n".into(),
            ports: "8080:80".into(),
            volumes: "data:/data:ro".into(),
            restart: "unless-stopped".into(),
            command: "sh -c 'echo hi'".into(),
            ..Default::default()
        };
        let b = flatten(create_body(&orig, &spec, "docker.io/library/nginx:latest", &NetEdit::default()).unwrap());
        assert_eq!(b["User"], "1000");
        assert_eq!(b["Entrypoint"], json!(["/ep"]));
        assert_eq!(b["Labels"]["l"], "v");
        assert!(b.get("Hostname").is_none());
        assert_eq!(b["Image"], "docker.io/library/nginx:latest");
        assert_eq!(b["Env"], json!(["B=2"]));
        assert_eq!(b["Cmd"], json!(["sh", "-c", "echo hi"]));
        assert!(b["ExposedPorts"].get("9000/tcp").is_some() && b["ExposedPorts"].get("80/tcp").is_some());
        let h = &b["HostConfig"];
        assert_eq!(h["CapDrop"], json!(["ALL"]));
        assert_eq!(h["ReadonlyRootfs"], true);
        assert_eq!(h["SecurityOpt"], json!(["no-new-privileges"]));
        assert_eq!(h["Memory"], 1048576);
        assert_eq!(h["PortBindings"], json!({"80/tcp": [{"HostIp": "", "HostPort": "8080"}]}));
        assert_eq!(h["Binds"], json!(["data:/data:ro"]));
        assert!(h.get("Mounts").is_none() && h.get("ContainerIDFile").is_none());
        assert_eq!(h["RestartPolicy"]["Name"], "unless-stopped");
        // prázdný příkaz -> výchozí z image
        let s2 = RunSpec { command: String::new(), ..spec };
        assert!(flatten(create_body(&orig, &s2, "x", &NetEdit::default()).unwrap())["Cmd"].is_null());
    }

    const ID: &str = "4f1c2a9b8c7d6e5f4f1c2a9b8c7d6e5f4f1c2a9b8c7d6e5f4f1c2a9b8c7d6e5f";

    /// Kontejner ve třech sítích: a (alias), b (alias + pevná IPv4/IPv6 v IPAMConfig – Docker),
    /// c (bez IPAMConfig – jako Podman); NetworkMode ukazuje na a.
    fn multi_net() -> Value {
        json!({
            "Id": ID,
            "Config": {"Image": "alpine", "Hostname": "4f1c2a9b8c7d"},
            "HostConfig": {"NetworkMode": "net-a"},
            "Mounts": [],
            "NetworkSettings": {"Networks": {
                "net-a": {"Aliases": ["web", "4f1c2a9b8c7d"], "IPAMConfig": null, "IPAddress": "10.89.0.5", "MacAddress": "aa:aa:aa:aa:aa:01"},
                "net-b": {"Aliases": ["db-client", "4f1c2a9b8c7d"], "IPAMConfig": {"IPv4Address": "10.231.7.50", "IPv6Address": "fd00::50"},
                          "IPAddress": "10.231.7.50", "GlobalIPv6Address": "fd00::50", "MacAddress": "aa:aa:aa:aa:aa:02"},
                "net-c": {"Aliases": ["4f1c2a9b8c7d"], "IPAMConfig": null, "IPAddress": "10.90.0.9", "MacAddress": "02:11:22:33:44:55"}
            }}
        })
    }

    #[test]
    fn multiple_networks_with_aliases_and_static_ips() {
        let orig = multi_net();
        let det = crate::inspect::parse(&orig);
        let spec = RunSpec { image: "alpine".into(), network: det.network_for_spec(), ..Default::default() };
        assert_eq!(spec.network, "net-a");
        // Docker: pevná adresa jen u net-b (IPAMConfig), Podman: i současné adresy ostatních
        let docker = det.net_edit(false);
        assert_eq!(docker.keep, vec!["net-b".to_string(), "net-c".into()]);
        assert_eq!(docker.pin, vec!["net-b".to_string()]);
        assert_eq!(det.net_edit(true).pin, vec!["net-a".to_string(), "net-b".into(), "net-c".into()]);

        let b = flatten(create_body(&orig, &spec, "alpine", &docker).unwrap());
        assert_eq!(b["HostConfig"]["NetworkMode"], "net-a");
        // hlavní síť: alias bez automatického aliasu s ID, bez adresy (není pevná)
        assert_eq!(b["NetworkingConfig"]["EndpointsConfig"], json!({"net-a": {"Aliases": ["web"]}}));
        let extra = extra_networks(&orig, &spec, &docker);
        assert_eq!(
            extra,
            vec![
                ("net-b".to_string(), json!({"Aliases": ["db-client"], "IPAMConfig": {"IPv4Address": "10.231.7.50", "IPv6Address": "fd00::50"}})),
                ("net-c".to_string(), json!({})),
            ]
        );

        // Podman (IPAMConfig v inspectu chybí): ponechá se současná IP i MAC
        let podman = det.net_edit(true);
        let extra = extra_networks(&orig, &spec, &podman);
        assert_eq!(extra[1].1, json!({"IPAMConfig": {"IPv4Address": "10.90.0.9"}, "MacAddress": "02:11:22:33:44:55"}));
        let b = flatten(create_body(&orig, &spec, "alpine", &podman).unwrap());
        assert_eq!(b["NetworkingConfig"]["EndpointsConfig"]["net-a"]["IPAMConfig"]["IPv4Address"], "10.89.0.5");

        // odškrtnutá síť se nepřipojí, odškrtnutá adresa se nepřenese
        let edit = NetEdit { keep: vec!["net-b".into()], pin: vec![] };
        assert_eq!(extra_networks(&orig, &spec, &edit), vec![("net-b".to_string(), json!({"Aliases": ["db-client"]}))]);
    }

    #[test]
    fn primary_network_change() {
        let orig = multi_net();
        let det = crate::inspect::parse(&orig);
        let mut edit = det.net_edit(false);
        // uživatel zvolil jinou hlavní síť: ta nahradí net-a, net-b a net-c zůstanou
        let spec = RunSpec { image: "alpine".into(), network: "net-new".into(), ..Default::default() };
        edit.keep.retain(|n| n != "net-new");
        let b = flatten(create_body(&orig, &spec, "alpine", &edit).unwrap());
        assert_eq!(b["HostConfig"]["NetworkMode"], "net-new");
        assert_eq!(b["NetworkingConfig"]["EndpointsConfig"], json!({"net-new": {}}));
        let names: Vec<String> = extra_networks(&orig, &spec, &edit).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["net-b", "net-c"]);

        // jedna z původních sítí jako hlavní: dostane svoje aliasy a pevnou IP a nepřipojuje se podruhé
        let spec = RunSpec { network: "net-b".into(), ..spec };
        let edit = NetEdit { keep: vec!["net-b".into(), "net-c".into()], pin: vec!["net-b".into()] };
        let b = flatten(create_body(&orig, &spec, "alpine", &edit).unwrap());
        assert_eq!(b["NetworkingConfig"]["EndpointsConfig"]["net-b"]["IPAMConfig"]["IPv4Address"], "10.231.7.50");
        let names: Vec<String> = extra_networks(&orig, &spec, &edit).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["net-c"]);

        // výchozí síť: NetworkMode z vlastní sítě se nezdědí, výchozí síť se nepřipojuje znovu
        let spec = RunSpec { network: String::new(), ..spec };
        let edit = NetEdit { keep: vec!["podman".into(), "net-c".into()], pin: vec![] };
        let b = flatten(create_body(&orig, &spec, "alpine", &edit).unwrap());
        assert_eq!(b["HostConfig"]["NetworkMode"], "bridge");
        assert!(b.get("NetworkingConfig").is_none());
        assert_eq!(extra_networks(&orig, &spec, &edit).len(), 1);

        // host: žádné další sítě
        let spec = RunSpec { network: "host".into(), ..spec };
        let b = flatten(create_body(&orig, &spec, "alpine", &edit).unwrap());
        assert_eq!(b["HostConfig"]["NetworkMode"], "host");
        assert!(b.get("NetworkingConfig").is_none() && extra_networks(&orig, &spec, &edit).is_empty());
    }

    fn with_anon() -> Value {
        let anon = "0d69e91fbc88c29db791105c0a2c7caf011e6203290b45472907c5ccd12bafbf";
        json!({
            "Id": ID,
            "Config": {"Image": "postgres", "Volumes": {"/var/lib/postgresql/data": {}}},
            "HostConfig": {"NetworkMode": "bridge", "Binds": [format!("{anon}:/var/lib/postgresql/data:rw,rprivate,nosuid,nodev,rbind")]},
            "Mounts": [
                {"Type": "volume", "Name": anon, "Destination": "/var/lib/postgresql/data", "RW": true},
                {"Type": "volume", "Name": "1111111111111111111111111111111111111111111111111111111111111111", "Destination": "/cache/", "RW": false},
                {"Type": "volume", "Name": "named", "Destination": "/named", "RW": true},
                {"Type": "bind", "Source": "/tmp", "Destination": "/host-tmp", "RW": true}
            ]
        })
    }

    #[test]
    fn anonymous_volumes_are_kept() {
        let orig = with_anon();
        let det = crate::inspect::parse(&orig);
        let spec = det.to_run_spec();
        // formulář anonymní volume neuvádí (jsou vidět jen v informačním řádku)
        assert_eq!(spec.volumes, "/tmp:/host-tmp\nnamed:/named");
        assert_eq!(det.anonymous_volumes(&spec.volumes).len(), 2);
        let b = flatten(create_body(&orig, &spec, "postgres", &NetEdit::default()).unwrap());
        assert_eq!(
            b["HostConfig"]["Binds"],
            json!([
                "/tmp:/host-tmp",
                "named:/named",
                "0d69e91fbc88c29db791105c0a2c7caf011e6203290b45472907c5ccd12bafbf:/var/lib/postgresql/data",
                "1111111111111111111111111111111111111111111111111111111111111111:/cache/:ro"
            ])
        );
        // pojmenované volume odebrané z formuláře se nevrací
        let s2 = RunSpec { volumes: String::new(), ..spec };
        let binds = flatten(create_body(&orig, &s2, "postgres", &NetEdit::default()).unwrap())["HostConfig"]["Binds"].clone();
        assert_eq!(binds.as_array().unwrap().len(), 2);
        assert!(!binds.to_string().contains("named:/named"));
    }

    #[test]
    fn form_destination_overrides_anonymous_volume() {
        let orig = with_anon();
        let det = crate::inspect::parse(&orig);
        // cíl uvedený ve formuláři (i s koncovým lomítkem nebo volbami) má přednost
        let spec = RunSpec { volumes: "pgdata:/var/lib/postgresql/data/:z\nC:\\cache:/cache".into(), ..det.to_run_spec() };
        assert!(det.anonymous_volumes(&spec.volumes).is_empty());
        let b = flatten(create_body(&orig, &spec, "postgres", &NetEdit::default()).unwrap());
        assert_eq!(b["HostConfig"]["Binds"], json!(["pgdata:/var/lib/postgresql/data/:z", "C:\\cache:/cache"]));
        assert_eq!(crate::inspect::bind_dest("/anon"), "/anon");
        assert_eq!(crate::inspect::bind_dest("x:/"), "/");
    }
}
