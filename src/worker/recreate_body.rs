//! Tělo `POST /containers/create` pro „Znovu vytvořit se změnami“: vychází z původního
//! inspectu (Config + HostConfig) a mění jen pole z formuláře. Zachová se tak uživatel,
//! capabilities, security-opt, read-only rootfs, privileged, zařízení, limity, tmpfs,
//! healthcheck, labely, entrypoint, log options… (bezpečnostní nález F-07).

use super::spec::split_args;
use super::RunSpec;
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

/// Sestaví tělo požadavku. `image` je už plně kvalifikovaný název.
pub fn create_body(orig: &Value, spec: &RunSpec, image: &str) -> Result<Value, String> {
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
    h.insert("Binds".into(), json!(lines(&spec.volumes).collect::<Vec<_>>()));
    h.remove("Mounts");

    let restart = spec.restart.trim();
    h.insert(
        "RestartPolicy".into(),
        json!({"Name": if restart.is_empty() || restart == "no" { "no" } else { restart }, "MaximumRetryCount": 0}),
    );
    if !spec.network.trim().is_empty() {
        h.insert("NetworkMode".into(), json!(spec.network.trim()));
    }
    // null hodnoty vynechat (engine použije výchozí)
    h.retain(|_, v| !v.is_null());
    // Podman v inspectu hlásí MemorySwappiness 0 i když není nastavené – na cgroup v2 ho
    // crun odmítne („cannot set memory swappiness“); -1/0 = výchozí
    if h.get("MemorySwappiness").and_then(|v| v.as_i64()).is_some_and(|n| n <= 0) {
        h.remove("MemorySwappiness");
    }
    Ok(json!({ "Config": config, "HostConfig": host }))
}

/// Docker API chce Config pole na nejvyšší úrovni těla (create = Config + HostConfig + NetworkingConfig).
pub fn flatten(body: Value) -> Value {
    let mut out = body.get("Config").cloned().unwrap_or_else(|| json!({}));
    if let (Some(o), Some(h)) = (out.as_object_mut(), body.get("HostConfig")) {
        o.insert("HostConfig".into(), h.clone());
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
        let b = flatten(create_body(&orig, &spec, "docker.io/library/nginx:latest").unwrap());
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
        assert!(flatten(create_body(&orig, &s2, "x").unwrap())["Cmd"].is_null());
    }
}
