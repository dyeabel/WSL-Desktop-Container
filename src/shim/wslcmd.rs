//! Příkazy navíc jen pro `wslcd.exe` (u `docker.exe` se nepoužijí): ovládání aplikace z příkazové řádky
//! pro skripty a testy – stav, distribuce (včetně přepnutí primární), engine, uvolnění paměti a čítače
//! paměti / CPU. Shim pošle požadavek běžící aplikaci přes řídicí rouru (`worker/control.rs`);
//! když aplikace neběží, spustí ji (schovanou v tray) a počká na rouru.
//!
//! Názvy příkazů (`status`, `distro`, `engine`, `memory`, `metrics`) Docker CLI nemá, takže se nepletou
//! s běžnými příkazy `docker`.

use serde_json::{json, Value};
#[cfg(windows)]
use std::time::{Duration, Instant};

pub const HELP: &str = "\
wslcd – extra commands for WSL Container Desktop (everything else behaves like `docker`)

Usage:
  wslcd status [--json]                      app, primary distribution, engine, API state
  wslcd distro ls [--json]                   WSL distributions (state, primary, remembered engine)
  wslcd distro use <name> [options]          switch the primary distribution (same as the app's
                                             “Set as primary”): the app API moves to <name>
      --engine docker|podman                 engine to use there (default: remembered / installed)
      --sleep-old | --keep-old               terminate the previous primary afterwards, or keep it
                                             running (default: the app setting)
      -v, --verbose                          print the step log (always printed on failure)
  wslcd distro start <name>                  start a distribution (wsl -d <name>)
  wslcd distro stop <name>                   terminate a distribution (wsl --terminate <name>)
  wslcd engine start|stop|restart            the engine (Docker / Podman) in the primary distribution
  wslcd memory free                          free WSL memory (drop Linux caches, compact)
  wslcd metrics [options]                    memory and CPU counters
      --interval <ms>                        CPU sampling window (default 1000, 100–60000)
      --count <n>                            repeat n times (0 = until Ctrl+C), one result per interval
      --no-containers                        skip per-container statistics
      --json                                 one JSON object per sample (JSON Lines)
  wslcd help-wslcd                           this help

Common options: --json prints the raw response {\"ok\",\"message\",\"log\",\"data\"}.
Exit codes: 0 ok, 1 the action failed, 2 invalid usage, 3 the app is not reachable.
The app must be installed; if it is not running, wslcd starts it hidden in the tray.
";

/// Rozpoznaný příkaz.
#[derive(Debug, PartialEq)]
pub struct Action {
    /// požadavek pro aplikaci (None = jen nápověda)
    pub req: Option<Value>,
    pub kind: Kind,
    pub json: bool,
    pub verbose: bool,
    /// metrics: počet opakování (0 = bez konce)
    pub count: u64,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Kind {
    Help,
    Status,
    DistroLs,
    /// akce s textovou odpovědí (use, start, stop, engine, memory)
    Message,
    Metrics,
}

/// Je to příkaz navíc? None = obyčejný docker příkaz. Err = chybné použití (kód 2).
pub fn parse(args: &[String]) -> Option<Result<Action, String>> {
    let first = args.first()?.as_str();
    if !matches!(first, "status" | "distro" | "engine" | "memory" | "metrics" | "help-wslcd") {
        return None;
    }
    Some(parse_inner(args))
}

fn parse_inner(args: &[String]) -> Result<Action, String> {
    let mut json = false;
    let mut verbose = false;
    let mut help = false;
    let mut engine: Option<String> = None;
    let mut sleep_old: Option<bool> = None;
    let mut interval: u64 = 1000;
    let mut count: u64 = 1;
    let mut containers = true;
    let mut pos: Vec<&str> = Vec::new();
    let mut it = args.iter();
    let num = |v: Option<&String>, f: &str| -> Result<u64, String> {
        v.ok_or(format!("{f} needs a value"))?.parse::<u64>().map_err(|_| format!("{f}: not a number"))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "-v" | "--verbose" => verbose = true,
            "-h" | "--help" | "help" => help = true,
            "--engine" => engine = Some(it.next().ok_or("--engine needs a value (docker | podman)")?.clone()),
            "--sleep-old" => sleep_old = Some(true),
            "--keep-old" => sleep_old = Some(false),
            "--interval" => interval = num(it.next(), "--interval")?,
            "--count" => count = num(it.next(), "--count")?,
            "--no-containers" => containers = false,
            s if s.starts_with("--engine=") => engine = Some(s["--engine=".len()..].to_string()),
            s if s.starts_with("--interval=") => interval = num(Some(&s["--interval=".len()..].to_string()), "--interval")?,
            s if s.starts_with("--count=") => count = num(Some(&s["--count=".len()..].to_string()), "--count")?,
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s => pos.push(s),
        }
    }
    let act = |req: Value, kind: Kind| Ok(Action { req: Some(req), kind, json, verbose, count: 1 });
    if help || pos[0] == "help-wslcd" {
        return Ok(Action { req: None, kind: Kind::Help, json, verbose, count: 1 });
    }
    let name = |i: usize, what: &str| pos.get(i).map(|s| s.to_string()).ok_or(format!("{what}: missing distribution name"));
    let extra = |n: usize| if pos.len() > n { Err(format!("unexpected argument {}", pos[n])) } else { Ok(()) };
    match (pos[0], pos.get(1).copied()) {
        ("status", None) => act(json!({"cmd": "status"}), Kind::Status),
        ("distro", Some("ls" | "list")) => {
            extra(2)?;
            act(json!({"cmd": "distro-ls"}), Kind::DistroLs)
        }
        ("distro", Some("use")) => {
            let d = name(2, "distro use")?;
            extra(3)?;
            let mut r = json!({"cmd": "distro-use", "distro": d});
            if let Some(e) = engine {
                if !matches!(e.to_ascii_lowercase().as_str(), "docker" | "podman") {
                    return Err(format!("--engine: unknown engine '{e}' (docker | podman)"));
                }
                r["engine"] = json!(e.to_ascii_lowercase());
            }
            if let Some(s) = sleep_old {
                r["sleep_old"] = json!(s);
            }
            act(r, Kind::Message)
        }
        ("distro", Some(op @ ("start" | "stop" | "terminate"))) => {
            let d = name(2, &format!("distro {op}"))?;
            extra(3)?;
            let cmd = if op == "start" { "distro-start" } else { "distro-stop" };
            act(json!({"cmd": cmd, "distro": d}), Kind::Message)
        }
        ("engine", Some(a @ ("start" | "stop" | "restart"))) => {
            extra(2)?;
            act(json!({"cmd": "engine", "action": a}), Kind::Message)
        }
        ("memory", Some("free")) => {
            extra(2)?;
            act(json!({"cmd": "memory-free"}), Kind::Message)
        }
        ("metrics", None) => {
            if !(100..=60_000).contains(&interval) {
                return Err("--interval: 100–60000 ms".into());
            }
            Ok(Action {
                req: Some(json!({"cmd": "metrics", "interval_ms": interval, "containers": containers})),
                kind: Kind::Metrics,
                json,
                verbose,
                count,
            })
        }
        (c, Some(s)) => Err(format!("unknown command: {c} {s} (wslcd help-wslcd)")),
        (c, None) => Err(format!("{c}: missing subcommand (wslcd help-wslcd)")),
    }
}

/// Chyba volání: kód návratu + zpráva.
pub struct CallError(pub u8, pub String);

/// Pošle požadavek aplikaci; když aplikace neběží, spustí ji a počká na rouru.
#[cfg(windows)]
pub fn call(req: &Value, debug: bool) -> Result<Value, CallError> {
    use std::io::{Read, Write};
    let name = crate::wake::control_pipe_name();
    let t = Instant::now();
    let app_was_running = crate::wake::app_running();
    let mut launched = false;
    let wait = std::env::var("WSLCD_SHIM_WAIT").ok().and_then(|s| s.parse().ok()).unwrap_or(60u64);
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(&name) {
            Ok(mut f) => {
                if debug {
                    eprintln!("[wslcd] {name} <- {req}");
                }
                let mut line = serde_json::to_vec(req).map_err(|e| CallError(1, e.to_string()))?;
                line.push(b'\n');
                f.write_all(&line).map_err(|e| CallError(3, format!("{name}: {e}")))?;
                let mut out = String::new();
                f.read_to_string(&mut out).map_err(|e| CallError(3, format!("{name}: {e}")))?;
                let first = out.lines().next().unwrap_or("");
                return serde_json::from_str(first).map_err(|e| CallError(3, format!("invalid response from the app: {e}")));
            }
            // všechny instance obsazené – za chvíli bude volná
            Err(e) if e.raw_os_error() == Some(231) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !crate::wake::app_running() && !launched {
                    eprintln!("wslcd: starting WSL Container Desktop…");
                    crate::autowake::launch_app().map_err(|e| CallError(3, format!("cannot start the app: {e}")))?;
                    launched = true;
                }
                // běžící starší verze aplikace rouru nemá – nečekat dlouho
                let limit = if app_was_running { 5 } else { wait };
                if t.elapsed() > Duration::from_secs(limit) {
                    return Err(CallError(
                        3,
                        if app_was_running {
                            "the running app does not offer the control pipe – restart / update WSL Container Desktop".into()
                        } else {
                            format!("the app did not open the control pipe within {limit} s")
                        },
                    ));
                }
                std::thread::sleep(Duration::from_millis(300));
            }
            Err(e) => return Err(CallError(3, format!("{name}: {e}"))),
        }
    }
}

#[cfg(not(windows))]
pub fn call(_req: &Value, _debug: bool) -> Result<Value, CallError> {
    Err(CallError(3, "wslcd control commands are available on Windows only".into()))
}

/// Provede příkaz a vrátí kód návratu.
pub fn run(a: Action, debug: bool) -> u8 {
    let Some(req) = a.req.clone() else {
        print!("{HELP}");
        return 0;
    };
    let mut n = 0u64;
    loop {
        let resp = match call(&req, debug) {
            Ok(v) => v,
            Err(CallError(code, msg)) => {
                eprintln!("wslcd: {msg}");
                return code;
            }
        };
        let ok = resp.get("ok").and_then(Value::as_bool).unwrap_or(false);
        if a.json {
            if a.kind == Kind::Metrics && a.count != 1 {
                println!("{}", serde_json::to_string(&resp).unwrap_or_default());
            } else {
                println!("{}", serde_json::to_string_pretty(&resp).unwrap_or_default());
            }
        } else {
            if n > 0 {
                println!();
            }
            print!("{}", render(a.kind, &resp, a.verbose));
        }
        if !ok {
            return 1;
        }
        n += 1;
        if a.kind != Kind::Metrics || (a.count != 0 && n >= a.count) {
            return 0;
        }
    }
}

// ---------------------------------------------------------------- výpis pro lidi

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Null) | None => "-".into(),
        Some(x) => x.to_string(),
    }
}

/// Bajty čitelně (binární jednotky jako `docker stats`).
pub fn bytes(v: Option<&Value>) -> String {
    let Some(b) = v.and_then(Value::as_u64) else { return "-".into() };
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut x = b as f64;
    let mut i = 0;
    while x >= 1024.0 && i < units.len() - 1 {
        x /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else { format!("{x:.1} {}", units[i]) }
}

fn pct(v: Option<&Value>) -> String {
    v.and_then(Value::as_f64).map(|p| format!("{p:.1} %")).unwrap_or_else(|| "-".into())
}

fn table(rows: &[Vec<String>]) -> String {
    let n = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let w: Vec<usize> = (0..n).map(|i| rows.iter().map(|r| r.get(i).map(|c| c.chars().count()).unwrap_or(0)).max().unwrap_or(0)).collect();
    let mut out = String::new();
    for r in rows {
        let line: Vec<String> = r.iter().enumerate().map(|(i, c)| format!("{c:<width$}", width = w[i])).collect();
        out.push_str(line.join("   ").trim_end());
        out.push('\n');
    }
    out
}

pub fn render(kind: Kind, resp: &Value, verbose: bool) -> String {
    let ok = resp.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let msg = resp.get("message").and_then(Value::as_str).unwrap_or("");
    let log = resp.get("log").and_then(Value::as_str).unwrap_or("");
    let d = resp.get("data").cloned().unwrap_or(Value::Null);
    let mut out = String::new();
    if !ok {
        if !log.trim().is_empty() {
            out.push_str(log.trim_end());
            out.push('\n');
        }
        out.push_str(&format!("Error: {msg}\n"));
        return out;
    }
    match kind {
        Kind::Status => {
            let rows = [
                ("App version", s(&d, "app_version")),
                ("App PID", s(&d, "app_pid")),
                ("Primary distribution", s(&d, "primary_distro")),
                ("Distribution running", s(&d, "distro_running")),
                ("Engine", s(&d, "engine")),
                ("API", format!("tcp://127.0.0.1:{}{}", s(&d, "port"), if d["tls"] == json!(true) { " (TLS)" } else { "" })),
                ("API connected", s(&d, "api_connected")),
                ("Keep-alive", s(&d, "keep_alive")),
                ("Actions in progress", s(&d, "busy_actions")),
            ];
            for (k, v) in rows {
                out.push_str(&format!("{k:<22} {v}\n"));
            }
        }
        Kind::DistroLs => {
            let mut rows = vec![vec!["NAME".into(), "STATE".into(), "WSL".into(), "PRIMARY".into(), "ENGINE".into()]];
            for x in d.as_array().into_iter().flatten() {
                let mut name = s(x, "name");
                if x["wsl_default"] == json!(true) {
                    name.push_str(" (default)");
                }
                rows.push(vec![name, s(x, "state"), s(x, "version"), if x["primary"] == json!(true) { "*".into() } else { String::new() }, s(x, "engine")]);
            }
            out.push_str(&table(&rows));
        }
        Kind::Message => {
            if verbose && !log.trim().is_empty() {
                out.push_str(log.trim_end());
                out.push('\n');
            }
            if !msg.is_empty() {
                out.push_str(msg);
                out.push('\n');
            }
        }
        Kind::Metrics => {
            let app = &d["app"];
            let win = &d["windows"];
            let w = &d["wsl"];
            let e = &d["engine"];
            let wsl_label = format!("WSL ({})", s(w, "distro"));
            out.push_str(&format!(
                "{:<26} CPU {}   working set {}   private {}\n",
                format!("App (PID {})", s(app, "pid")),
                pct(app.get("cpu_pct")),
                bytes(app.get("working_set")),
                bytes(app.get("private_bytes"))
            ));
            out.push_str(&format!(
                "{:<26} RAM {} / {} used   {} logical CPUs\n",
                "Windows",
                bytes(win.get("ram_used")),
                bytes(win.get("ram_total")),
                s(win, "logical_cpus")
            ));
            if w["running"] == json!(true) {
                out.push_str(&format!(
                    "{:<26} CPU {}   used {}   cache {}   total {}   VmmemWSL {}   engine RSS {}\n",
                    wsl_label,
                    pct(w.get("cpu_pct")),
                    bytes(w.get("mem_used")),
                    bytes(w.get("mem_cache")),
                    bytes(w.get("mem_total")),
                    bytes(w.get("vmmem")),
                    bytes(w.get("engine_rss"))
                ));
            } else {
                out.push_str(&format!("{wsl_label:<26} not running\n"));
            }
            out.push_str(&format!(
                "{:<26} {} {}   containers {}/{} running\n",
                "Engine",
                s(e, "name"),
                if e["connected"] == json!(true) { "connected" } else { "not connected" },
                s(e, "containers_running"),
                s(e, "containers_total")
            ));
            let cs = d["containers"].as_array().cloned().unwrap_or_default();
            if !cs.is_empty() {
                let mut rows = vec![vec!["CONTAINER".into(), "NAME".into(), "CPU %".into(), "MEM".into(), "LIMIT".into(), "NET RX / TX".into(), "BLOCK R / W".into()]];
                for c in &cs {
                    rows.push(vec![
                        s(c, "id"),
                        s(c, "name"),
                        pct(c.get("cpu_pct")),
                        bytes(c.get("mem")),
                        bytes(c.get("mem_limit")),
                        format!("{} / {}", bytes(c.get("net_rx")), bytes(c.get("net_tx"))),
                        format!("{} / {}", bytes(c.get("blk_read")), bytes(c.get("blk_write"))),
                    ]);
                }
                out.push('\n');
                out.push_str(&table(&rows));
            }
        }
        Kind::Help => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }
    fn req(s: &str) -> Value {
        parse(&v(s)).unwrap().unwrap().req.unwrap()
    }

    #[test]
    fn docker_commands_pass_through() {
        for c in ["ps", "stats", "run --rm alpine", "context ls", "compose up", "system df", "top x"] {
            assert!(parse(&v(c)).is_none(), "{c}");
        }
        assert!(parse(&[]).is_none());
    }

    #[test]
    fn parses_commands() {
        assert_eq!(req("status"), json!({"cmd": "status"}));
        assert_eq!(req("distro ls"), json!({"cmd": "distro-ls"}));
        assert_eq!(req("distro list --json"), json!({"cmd": "distro-ls"}));
        assert_eq!(req("distro use Ubuntu"), json!({"cmd": "distro-use", "distro": "Ubuntu"}));
        assert_eq!(
            req("distro use Ubuntu --engine Podman --sleep-old"),
            json!({"cmd": "distro-use", "distro": "Ubuntu", "engine": "podman", "sleep_old": true})
        );
        assert_eq!(req("distro use X --keep-old"), json!({"cmd": "distro-use", "distro": "X", "sleep_old": false}));
        assert_eq!(req("distro start Debian"), json!({"cmd": "distro-start", "distro": "Debian"}));
        assert_eq!(req("distro stop Debian"), json!({"cmd": "distro-stop", "distro": "Debian"}));
        assert_eq!(req("distro terminate Debian"), json!({"cmd": "distro-stop", "distro": "Debian"}));
        assert_eq!(req("engine restart"), json!({"cmd": "engine", "action": "restart"}));
        assert_eq!(req("memory free"), json!({"cmd": "memory-free"}));
        assert_eq!(req("metrics"), json!({"cmd": "metrics", "interval_ms": 1000, "containers": true}));
        assert_eq!(req("metrics --interval 500 --no-containers"), json!({"cmd": "metrics", "interval_ms": 500, "containers": false}));
        let a = parse(&v("metrics --count 0 --json")).unwrap().unwrap();
        assert_eq!((a.count, a.json, a.kind), (0, true, Kind::Metrics));
        let a = parse(&v("distro use X -v")).unwrap().unwrap();
        assert!(a.verbose);
    }

    #[test]
    fn help_and_errors() {
        for c in ["help-wslcd", "distro --help", "metrics -h", "status help"] {
            let a = parse(&v(c)).unwrap().unwrap();
            assert_eq!(a.kind, Kind::Help, "{c}");
            assert!(a.req.is_none());
        }
        for c in [
            "distro",
            "distro use",
            "distro use A B",
            "distro rm X",
            "engine",
            "engine kill",
            "memory",
            "status extra",
            "metrics --interval",
            "metrics --interval 10",
            "distro use X --engine containerd",
            "distro ls --bogus",
        ] {
            assert!(parse(&v(c)).unwrap().is_err(), "{c}");
        }
    }

    #[test]
    fn renders_human_output() {
        let r = json!({"ok": true, "message": "", "data": [
            {"name": "Ubuntu", "state": "Running", "version": "2", "wsl_default": true, "primary": true, "engine": "Docker"},
            {"name": "Debian", "state": "Stopped", "version": "2", "wsl_default": false, "primary": false, "engine": null}
        ]});
        let t = render(Kind::DistroLs, &r, false);
        assert!(t.starts_with("NAME"), "{t}");
        assert!(t.contains("Ubuntu (default)") && t.contains("Docker"), "{t}");
        let e = render(Kind::Message, &json!({"ok": false, "message": "boom", "log": "step 1"}), false);
        assert_eq!(e, "step 1\nError: boom\n");
        let m = render(Kind::Message, &json!({"ok": true, "message": "done", "log": "step"}), false);
        assert_eq!(m, "done\n");
        assert_eq!(bytes(Some(&json!(512))), "512 B");
        assert_eq!(bytes(Some(&json!(1536))), "1.5 KiB");
        assert_eq!(bytes(Some(&json!(null))), "-");
    }

    #[test]
    fn renders_metrics() {
        let r = json!({"ok": true, "message": "", "data": {
            "app": {"pid": 7, "cpu_pct": 1.5, "working_set": 1048576, "private_bytes": 2097152},
            "windows": {"ram_total": 17179869184u64, "ram_used": 8589934592u64, "logical_cpus": 8},
            "wsl": {"distro": "Ubuntu", "running": true, "cpu_pct": 3.0, "mem_used": 1073741824, "mem_cache": 0, "mem_total": 2147483648u64, "vmmem": null, "engine_rss": 1024},
            "engine": {"name": "Docker", "connected": true, "containers_running": 1, "containers_total": 2},
            "containers": [{"id": "abc", "name": "web", "cpu_pct": 0.5, "mem": 1024, "mem_limit": 2048, "net_rx": 1, "net_tx": 2, "blk_read": 3, "blk_write": 4}]
        }});
        let t = render(Kind::Metrics, &r, false);
        assert!(t.contains("App (PID 7)") && t.contains("CPU 1.5 %"), "{t}");
        assert!(t.contains("WSL (Ubuntu)") && t.contains("1.0 GiB"), "{t}");
        assert!(t.contains("containers 1/2 running"), "{t}");
        assert!(t.contains("web") && t.contains("0.5 %"), "{t}");
    }
}
