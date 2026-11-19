//! `docker events` u Podmanu přes kompatibilní API (`/events`): to posílá zprávy ve schématu
//! Dockeru (`Type`, `Action`, `Actor.ID`, `Actor.Attributes`, `scope`, `timeNano`), kdežto
//! `podman events` vypisuje vlastní (`Status`, `Name`, …). Výstup jako Docker CLI: výchozí
//! řádek, `--format json` / `{{json .}}` (zpráva z API beze změny) a jednoduché šablony
//! (`{{.Action}} {{.Actor.Attributes.name}}`). Složitější šablony (`if`, `index` …) jdou dál přes
//! `podman events` s přeloženými poli (`podmanfix`).
//!
//! Bez `--until` proud běží, dokud ho uživatel neukončí (Ctrl+C) nebo engine nespadne.

use crate::fast::{self, Seg};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, PartialEq)]
pub enum Out {
    /// výchozí řádek Docker CLI
    Default,
    /// zpráva z API (JSON) beze změny
    Json,
    Template(Vec<Seg>),
}

#[derive(Debug, PartialEq)]
pub struct Plan {
    pub path: String,
    pub out: Out,
}

/// Rozbor `events` / `system events`; None = nechat na `podman events`.
pub fn parse(args: &[String], now_ns: i128) -> Option<Plan> {
    let rest = match args {
        [a, rest @ ..] if a == "events" => rest,
        [a, b, rest @ ..] if a == "system" && b == "events" => rest,
        _ => return None,
    };
    let mut q: Vec<String> = Vec::new();
    let mut filters: Vec<(String, String)> = Vec::new();
    let mut format: Option<String> = None;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let (k, inline) = match a.split_once('=') {
            Some((k, v)) if k.starts_with("--") => (k, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        let mut val = || inline.clone().or_else(|| it.next().cloned());
        match k {
            "--since" => q.push(format!("since={}", fast::api_timestamp(&val()?, now_ns).ok()?)),
            "--until" => q.push(format!("until={}", fast::api_timestamp(&val()?, now_ns).ok()?)),
            "-f" | "--filter" => {
                let v = val()?;
                let (fk, fv) = v.split_once('=')?;
                filters.push((fk.trim().to_ascii_lowercase(), fv.trim().to_string()));
            }
            "--format" => format = Some(val()?),
            _ => return None,
        }
    }
    if !filters.is_empty() {
        q.push(format!("filters={}", enc(&fast::filters_json(&filters))));
    }
    let out = match format.as_deref().map(str::trim) {
        None | Some("") => Out::Default,
        Some("json") | Some("{{json .}}") => Out::Json,
        Some(t) => {
            let segs = fast::parse_template(&t.replace(r"\t", "\t").replace(r"\n", "\n")).ok()?;
            // jen pole, která zpráva má (jinak Go šablona selže – ať to řekne podman)
            for s in &segs {
                if let Seg::Field(p) = s {
                    lookup_path(p)?;
                }
            }
            Out::Template(segs)
        }
    };
    let path = if q.is_empty() { "/events".to_string() } else { format!("/events?{}", q.join("&")) };
    Some(Plan { path, out })
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{:02X}", b),
        })
        .collect()
}

/// Pole Go struktury `events.Message` -> cesta v JSON z API.
fn lookup_path(p: &[String]) -> Option<Vec<String>> {
    let first = match p.first()?.as_str() {
        "Status" => "status",
        "ID" => "id",
        "From" => "from",
        "Type" => "Type",
        "Action" => "Action",
        "Actor" => "Actor",
        "Scope" => "scope",
        "Time" => "time",
        "TimeNano" => "timeNano",
        _ => return None,
    };
    let mut out = vec![first.to_string()];
    match (first, &p[1..]) {
        (_, []) => {}
        ("Actor", [id]) if id == "ID" => out.push("ID".into()),
        ("Actor", [a, key]) if a == "Attributes" => {
            out.push("Attributes".into());
            out.push(key.clone());
        }
        _ => return None,
    }
    Some(out)
}

fn scalar(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
    }
}

/// Docker neposílá `status`/`id`/`from` u jiných typů než kontejner/obraz – Podman je posílá
/// vždy; ve výchozím řádku se nepoužívají, takže na tom nezáleží.
pub fn format_line(raw: &[u8], out: &Out, tz: fast::TzFn) -> Option<String> {
    let mut v: Value = serde_json::from_slice(raw).ok()?;
    // Podman: `Action: health_status` + `HealthStatus: healthy`; Docker: `health_status: healthy`
    let fixed;
    let raw = match (v.get("Action").and_then(|a| a.as_str()), v.get("HealthStatus").and_then(|h| h.as_str())) {
        (Some("health_status"), Some(h)) if !h.is_empty() => {
            let a = format!("health_status: {h}");
            let text = String::from_utf8_lossy(raw)
                .replace(r#""Action":"health_status""#, &format!(r#""Action":"{a}""#))
                .replace(r#""status":"health_status""#, &format!(r#""status":"{a}""#));
            v["Action"] = Value::String(a.clone());
            v["status"] = Value::String(a);
            fixed = text.into_bytes();
            &fixed[..]
        }
        _ => raw,
    };
    Some(match out {
        Out::Json => String::from_utf8_lossy(raw).trim().to_string(),
        Out::Default => {
            let ns = v.get("timeNano").and_then(|t| t.as_i64()).unwrap_or_else(|| v.get("time").and_then(|t| t.as_i64()).unwrap_or(0) * 1_000_000_000);
            let mut s = format!(
                "{} {} {} {}",
                rfc3339_nano(ns, tz),
                scalar(v.get("Type")),
                scalar(v.get("Action")),
                scalar(v.pointer("/Actor/ID"))
            );
            if let Some(Value::Object(a)) = v.pointer("/Actor/Attributes") {
                if !a.is_empty() {
                    let mut kv: Vec<String> = a.iter().map(|(k, x)| format!("{k}={}", scalar(Some(x)))).collect();
                    kv.sort();
                    s.push_str(&format!(" ({})", kv.join(", ")));
                }
            }
            s
        }
        Out::Template(segs) => {
            let mut s = String::new();
            for seg in segs {
                match seg {
                    Seg::Lit(l) => s.push_str(l),
                    Seg::JsonDot => s.push_str(String::from_utf8_lossy(raw).trim()),
                    Seg::Field(p) => {
                        let path = lookup_path(p)?;
                        let mut cur = Some(&v);
                        for k in &path {
                            cur = cur.and_then(|c| c.get(k));
                        }
                        // chybějící klíč atributu: Go vypíše `<no value>`
                        match cur {
                            None if path.len() == 3 => s.push_str("<no value>"),
                            Some(Value::Object(m)) => {
                                let mut kv: Vec<String> = m.iter().map(|(k, x)| format!("{k}:{}", scalar(Some(x)))).collect();
                                kv.sort();
                                s.push_str(&format!("map[{}]", kv.join(" ")));
                            }
                            c => s.push_str(&scalar(c)),
                        }
                    }
                }
            }
            s
        }
    })
}

/// `2006-01-02T15:04:05.000000000Z07:00` v místním čase (formát Docker CLI).
pub fn rfc3339_nano(ns: i64, tz: fast::TzFn) -> String {
    let secs = ns.div_euclid(1_000_000_000);
    let frac = ns.rem_euclid(1_000_000_000);
    let (off, _) = tz(secs);
    let t = secs + off as i64;
    let (y, mo, d) = fast::civil_from_days(t.div_euclid(86400));
    let sod = t.rem_euclid(86400);
    let zone = if off == 0 {
        "Z".to_string()
    } else {
        let oa = off.abs() / 60;
        format!("{}{:02}:{:02}", if off < 0 { '-' } else { '+' }, oa / 60, oa % 60)
    };
    format!("{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}.{frac:09}{zone}", sod / 3600, sod / 60 % 60, sod % 60)
}

trait Rw: Read + Write {}
impl<T: Read + Write> Rw for T {}

/// Chyba před prvním řádkem (lze zkusit `podman events`) vs. po něm.
pub enum Fail {
    Before(String),
    After(String),
}

/// GET s proudem řádků (chunked i bez); `on_line` pro každý řádek těla.
pub fn stream(port: u16, tls: Option<Arc<rustls::ClientConfig>>, path: &str, mut on_line: impl FnMut(&[u8])) -> Result<(), Fail> {
    let b = |e: String| Fail::Before(e);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).map_err(|e| b(e.to_string()))?;
    let _ = tcp.set_nodelay(true);
    let io: Box<dyn Rw> = match tls {
        None => Box::new(tcp),
        Some(cfg) => {
            let name = rustls_pki_types::ServerName::IpAddress(std::net::Ipv4Addr::LOCALHOST.into());
            let conn = rustls::ClientConnection::new(cfg, name).map_err(|e| b(e.to_string()))?;
            Box::new(rustls::StreamOwned::new(conn, tcp))
        }
    };
    let mut r = BufReader::new(io);
    let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: wls-docker-shim\r\nConnection: close\r\n\r\n");
    r.get_mut().write_all(req.as_bytes()).and_then(|_| r.get_mut().flush()).map_err(|e| b(e.to_string()))?;
    let mut line = String::new();
    r.read_line(&mut line).map_err(|e| b(e.to_string()))?;
    let code: u16 = line.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or_else(|| b("bad status line".into()))?;
    let mut chunked = false;
    loop {
        line.clear();
        r.read_line(&mut line).map_err(|e| b(e.to_string()))?;
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            }
        }
    }
    if code != 200 {
        return Err(b(format!("HTTP {code}")));
    }
    let a = |e: std::io::Error| Fail::After(e.to_string());
    let mut pending: Vec<u8> = Vec::new();
    let emit = |pending: &mut Vec<u8>, on_line: &mut dyn FnMut(&[u8])| {
        while let Some(i) = pending.iter().position(|&c| c == b'\n') {
            let l: Vec<u8> = pending.drain(..=i).collect();
            let l = &l[..l.len() - 1];
            if !l.iter().all(|c| c.is_ascii_whitespace()) {
                on_line(l);
            }
        }
    };
    if chunked {
        loop {
            line.clear();
            if r.read_line(&mut line).map_err(a)? == 0 {
                break;
            }
            let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16).map_err(|_| Fail::After("bad chunk size".into()))?;
            if size == 0 {
                break;
            }
            let mut buf = vec![0u8; size];
            r.read_exact(&mut buf).map_err(a)?;
            let mut crlf = [0u8; 2];
            r.read_exact(&mut crlf).map_err(a)?;
            pending.extend_from_slice(&buf);
            emit(&mut pending, &mut on_line);
        }
    } else {
        let mut buf = Vec::new();
        while r.read_until(b'\n', &mut buf).map_err(a)? > 0 {
            pending.append(&mut buf);
            emit(&mut pending, &mut on_line);
        }
    }
    pending.push(b'\n');
    emit(&mut pending, &mut on_line);
    Ok(())
}

/// Celé `docker events` přes API. Ok(true) = hotovo; Ok(false) = nechat na `podman events`.
pub fn run(args: &[String], port: u16, tls: bool, debug: bool) -> Result<bool, String> {
    let now_ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0);
    let Some(plan) = parse(args, now_ns) else { return Ok(false) };
    let cfg = if tls {
        let dir = crate::apitls::tls_dir().ok_or("APPDATA not set")?;
        Some(Arc::new(crate::apitls::tls_config(&dir)?))
    } else {
        None
    };
    if debug {
        eprintln!("[wls-docker] events: GET {} (API, schéma Dockeru)", plan.path);
    }
    let mut out = std::io::stdout();
    let r = stream(port, cfg, &plan.path, |l| {
        if let Some(s) = format_line(l, &plan.out, fast::local_tz) {
            // zavřená roura (`| head`) – konec
            if writeln!(out, "{s}").and_then(|_| out.flush()).is_err() {
                std::process::exit(0);
            }
        }
    });
    match r {
        Ok(()) => Ok(true),
        Err(Fail::Before(e)) => {
            if debug {
                eprintln!("[wls-docker] events přes API nejdou ({e}) – podman events");
            }
            Ok(false)
        }
        Err(Fail::After(e)) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }
    fn cet(_: i64) -> (i32, String) {
        (3600, "CET".into())
    }
    const MSG: &[u8] = br#"{"status":"die","id":"abc123","from":"docker.io/library/alpine:latest","Type":"container","Action":"die","Actor":{"ID":"abc123","Attributes":{"name":"web","image":"docker.io/library/alpine:latest","exitCode":"1"}},"scope":"local","time":1795085833,"timeNano":1795085833111156718}"#;

    #[test]
    fn parse_flags() {
        let p = parse(&a(&["events", "--since", "1700000000", "--until=1700000060", "-f", "type=container", "--format", "{{.Action}}"]), 0).unwrap();
        assert_eq!(p.path, "/events?since=1700000000&until=1700000060&filters=%7B%22type%22%3A%7B%22container%22%3Atrue%7D%7D");
        assert_eq!(p.out, Out::Template(vec![Seg::Field(vec!["Action".into()])]));
        assert_eq!(parse(&a(&["system", "events", "--format", "json"]), 0).unwrap().out, Out::Json);
        assert_eq!(parse(&a(&["events"]), 0).unwrap(), Plan { path: "/events".into(), out: Out::Default });
        // relativní čas
        assert_eq!(parse(&a(&["events", "--since", "1m"]), 120_000_000_000).unwrap().path, "/events?since=60");
        // neznámé pole, složitá šablona, neznámý přepínač -> podman
        assert!(parse(&a(&["events", "--format", "{{.Nope}}"]), 0).is_none());
        assert!(parse(&a(&["events", "--format", "{{if eq .Action \"die\"}}x{{end}}"]), 0).is_none());
        assert!(parse(&a(&["events", "--stream"]), 0).is_none());
        assert!(parse(&a(&["ps"]), 0).is_none());
    }

    #[test]
    fn output_like_docker() {
        assert_eq!(
            format_line(MSG, &Out::Default, cet).unwrap(),
            "2026-11-19T11:57:13.111156718+01:00 container die abc123 (exitCode=1, image=docker.io/library/alpine:latest, name=web)"
        );
        assert_eq!(format_line(MSG, &Out::Json, cet).unwrap().as_bytes(), MSG);
        let t = |s: &str| format_line(MSG, &parse(&a(&["events", "--format", s]), 0).unwrap().out, cet).unwrap();
        assert_eq!(t("{{.Type}} {{.Action}} {{.Actor.ID}} {{.Actor.Attributes.name}} {{.Scope}} {{.From}}"), "container die abc123 web local docker.io/library/alpine:latest");
        assert_eq!(t("{{.Actor.Attributes.nope}}|{{.TimeNano}}"), "<no value>|1795085833111156718");
        assert_eq!(rfc3339_nano(0, |_| (0, "UTC".into())), "1970-01-01T00:00:00.000000000Z");
        // healthcheck: akce jako u Dockeru
        let hc = br#"{"status":"health_status","Type":"container","Action":"health_status","Actor":{"ID":"x","Attributes":{}},"timeNano":0,"HealthStatus":"healthy"}"#;
        let tmpl = parse(&a(&["events", "--format", "{{.Action}}|{{.Status}}"]), 0).unwrap().out;
        assert_eq!(format_line(hc, &tmpl, cet).unwrap(), "health_status: healthy|health_status: healthy");
        assert!(format_line(hc, &Out::Json, cet).unwrap().contains(r#""Action":"health_status: healthy""#));
    }

    #[test]
    fn stream_chunked_from_local_server() {
        use std::net::TcpListener;
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let body1 = b"{\"a\":1}\n{\"a\"";
            let body2 = b":2}\n";
            let resp = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n", body1.len());
            s.write_all(resp.as_bytes()).unwrap();
            s.write_all(body1).unwrap();
            s.write_all(format!("\r\n{:x}\r\n", body2.len()).as_bytes()).unwrap();
            s.write_all(body2).unwrap();
            s.write_all(b"\r\n0\r\n\r\n").unwrap();
        });
        let mut lines = Vec::new();
        assert!(stream(port, None, "/events", |l| lines.push(String::from_utf8(l.to_vec()).unwrap())).is_ok());
        srv.join().unwrap();
        assert_eq!(lines, vec![r#"{"a":1}"#, r#"{"a":2}"#]);
    }
}
