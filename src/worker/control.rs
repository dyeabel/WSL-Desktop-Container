//! Řídicí roura aplikace pro příkazy `wslcd status | distro … | engine … | memory free | metrics`.
//!
//! `wslcd.exe` (shim) se připojí na [`crate::wake::control_pipe_name`], pošle jeden řádek JSON
//! ([`Request`]) a přečte jeden řádek JSON ([`Response`]). Akce jdou stejnou cestou jako tlačítka
//! v UI (worker), takže je vidí i okno aplikace (oznámení, přehled „Na pozadí“, obnovení stavu).
//! Roura je jen pro aktuálního uživatele (DACL jako `docker_engine`, viz `npipe.rs`).
//! Roura existuje jen na Windows; na Linuxu modul slouží jen testům.
#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use super::{Cmd, Msg, Worker, WslCmd};
use crate::docker::Docker;
use crate::engine::Engine;
use crate::host::{HostExt, Privilege};
use crate::memory;
use crate::wsl;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Požadavek z `wslcd` (pole `cmd` určuje variantu).
#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Request {
    Status,
    DistroLs,
    DistroUse {
        distro: String,
        #[serde(default)]
        engine: Option<String>,
        /// None = jako v UI (podle `keep_previous_primary`)
        #[serde(default)]
        sleep_old: Option<bool>,
    },
    DistroStart {
        distro: String,
    },
    DistroStop {
        distro: String,
    },
    Engine {
        action: String,
    },
    MemoryFree,
    Metrics {
        #[serde(default = "default_interval")]
        interval_ms: u64,
        #[serde(default = "yes")]
        containers: bool,
    },
}

fn default_interval() -> u64 {
    1000
}
fn yes() -> bool {
    true
}

/// Odpověď pro `wslcd`.
#[derive(Debug, Serialize, Default)]
pub struct Response {
    pub ok: bool,
    pub message: String,
    /// průběh dlouhé akce (přepnutí distribuce)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub log: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

impl Response {
    fn from(r: Result<String, String>) -> Self {
        match r {
            Ok(m) => Response { ok: true, message: m, ..Default::default() },
            Err(e) => Response { ok: false, message: e, ..Default::default() },
        }
    }
    fn data(data: Value) -> Self {
        Response { ok: true, data, ..Default::default() }
    }
    fn err(msg: impl Into<String>) -> Self {
        Response { ok: false, message: msg.into(), ..Default::default() }
    }
}

/// Zpracuje jeden řádek požadavku (JSON) a vrátí řádek odpovědi.
pub(super) fn handle_line(w: &Worker, line: &[u8]) -> Vec<u8> {
    let resp = match serde_json::from_slice::<Request>(line) {
        Ok(r) => w.control(r),
        Err(e) => Response::err(format!("bad request: {e}")),
    };
    let mut out = serde_json::to_vec(&resp).unwrap_or_else(|_| b"{\"ok\":false,\"message\":\"serialize\"}".to_vec());
    out.push(b'\n');
    out
}

fn parse_engine(s: &str) -> Result<Engine, String> {
    match s.to_ascii_lowercase().as_str() {
        "docker" => Ok(Engine::Docker),
        "podman" => Ok(Engine::Podman),
        _ => Err(format!("unknown engine '{s}' (docker | podman)")),
    }
}

/// Přesné jméno distribuce z `wsl -l -v` (bez ohledu na velikost písmen).
fn find_distro(list: &[wsl::Distro], name: &str) -> Result<String, String> {
    list.iter()
        .find(|d| d.name.eq_ignore_ascii_case(name))
        .map(|d| d.name.clone())
        .ok_or_else(|| format!("distribution '{name}' not found (wslcd distro ls)"))
}

impl Worker {
    fn control(&self, r: Request) -> Response {
        match r {
            Request::Status => self.ctl_status(),
            Request::DistroLs => self.ctl_distro_ls(),
            Request::DistroUse { distro, engine, sleep_old } => self.ctl_distro_use(&distro, engine.as_deref(), sleep_old),
            Request::DistroStart { distro } => self.ctl_wsl(&distro, true),
            Request::DistroStop { distro } => self.ctl_wsl(&distro, false),
            Request::Engine { action } => {
                let cmd = match action.as_str() {
                    "start" => Cmd::EngineStart,
                    "stop" => Cmd::EngineStop,
                    "restart" => Cmd::EngineRestart,
                    a => return Response::err(format!("unknown engine action '{a}' (start | stop | restart)")),
                };
                Response::from(self.exec_report(cmd))
            }
            Request::MemoryFree => Response::from(self.exec_report(Cmd::FreeMemory)),
            Request::Metrics { interval_ms, containers } => self.ctl_metrics(interval_ms.clamp(100, 60_000), containers),
        }
    }

    fn ctl_status(&self) -> Response {
        let cfg = self.cfg();
        let connected = self.docker().ping();
        let list = wsl::list().ok();
        let running = list.as_ref().and_then(|l| l.iter().find(|d| d.name == cfg.distro)).map(|d| d.running());
        Response::data(json!({
            "app_version": env!("CARGO_PKG_VERSION"),
            "app_pid": std::process::id(),
            "primary_distro": cfg.distro,
            "distro_running": running,
            "engine": cfg.engine.name(),
            "port": cfg.port,
            "tls": cfg.tls,
            "api_connected": connected,
            "keep_alive": cfg.keep_alive,
            "keep_previous_primary": cfg.keep_previous_primary,
            "busy_actions": self.shared.busy.load(Ordering::SeqCst),
        }))
    }

    fn ctl_distro_ls(&self) -> Response {
        let cfg = self.cfg();
        let list = match wsl::list() {
            Ok(l) => l,
            Err(e) => return Response::err(e),
        };
        let items: Vec<Value> = list
            .iter()
            .map(|d| {
                let primary = d.name == cfg.distro;
                let engine = if primary { Some(cfg.engine) } else { cfg.distro_engines.get(&d.name).copied() };
                json!({
                    "name": d.name,
                    "state": d.state,
                    "version": d.version,
                    "wsl_default": d.default,
                    "primary": primary,
                    "engine": engine.map(|e| e.name()),
                })
            })
            .collect();
        Response::data(Value::Array(items))
    }

    fn ctl_distro_use(&self, name: &str, engine: Option<&str>, sleep_old: Option<bool>) -> Response {
        let engine = match engine.map(parse_engine).transpose() {
            Ok(e) => e,
            Err(e) => return Response::err(e),
        };
        let distro = match wsl::list().and_then(|l| find_distro(&l, name)) {
            Ok(d) => d,
            Err(e) => return Response::err(e),
        };
        let sleep_old = sleep_old.unwrap_or_else(|| !self.cfg().keep_previous_primary);
        let label = tf!("Primární distribuce: {distro}", "Primary distribution: {distro}");
        let mut log = String::new();
        let r = self.tracked(label.clone(), true, || self.set_primary(&distro, engine, sleep_old, &mut log));
        self.send(Msg::Output(label, log.clone()));
        let cfg = self.cfg();
        let mut resp = Response::from(r);
        resp.log = log;
        resp.data = json!({ "primary_distro": cfg.distro, "engine": cfg.engine.name(), "port": cfg.port, "tls": cfg.tls });
        resp
    }

    fn ctl_wsl(&self, name: &str, start: bool) -> Response {
        let distro = match wsl::list().and_then(|l| find_distro(&l, name)) {
            Ok(d) => d,
            Err(e) => return Response::err(e),
        };
        Response::from(self.exec_report(Cmd::Wsl(if start { WslCmd::Start(distro) } else { WslCmd::Terminate(distro) })))
    }

    /// Akce mimo `exec` (potřebuje log): stejné sledování jako `exec` – počítadlo rozpracovaných
    /// akcí, přehled „Na pozadí“, oznámení výsledku a obnovení stavu.
    fn tracked(&self, label: String, df: bool, f: impl FnOnce() -> Result<String, String>) -> Result<String, String> {
        self.shared.busy.fetch_add(1, Ordering::SeqCst);
        self.ctx.request_repaint();
        let mut act = crate::activity::start(crate::activity::Kind::Action, label, String::new());
        let r = f();
        if df {
            self.invalidate_df();
        }
        act.set_ok(r.is_ok());
        drop(act);
        self.shared.busy.fetch_sub(1, Ordering::SeqCst);
        match &r {
            Ok(s) if !s.trim().is_empty() => self.send(Msg::Toast(true, s.trim().to_string())),
            Ok(_) => {}
            Err(e) => self.send(Msg::Toast(false, e.trim().to_string())),
        }
        let _ = self.cmd.send(Cmd::Refresh);
        r
    }

    /// Čítače paměti a CPU: proces aplikace, Windows, primární distribuce (VM WSL), engine
    /// a kontejnery. CPU = průměr za `interval_ms` mezi dvěma vzorky (100 % = jedno jádro,
    /// jako `docker stats`; u distribuce 100 % = všechna jádra VM).
    fn ctl_metrics(&self, interval_ms: u64, with_containers: bool) -> Response {
        let cfg = self.cfg();
        let wsl_host = self.host().wsl().is_some();
        // stav distribuce bez jejího probouzení
        let running = if wsl_host {
            wsl::list().ok().and_then(|l| l.into_iter().find(|d| d.name == cfg.distro)).map(|d| d.running()).unwrap_or(false)
        } else {
            true
        };
        let ep = crate::host::endpoint(&cfg);
        let d = Docker::new(&ep);
        let connected = running && d.ping();
        let snap = if connected { Some(d.snapshot()) } else { None };
        let ids: Vec<(String, String)> = match (&snap, with_containers) {
            (Some(s), true) => s.containers.iter().filter(|c| c.running()).map(|c| (c.id.clone(), c.name())).collect(),
            _ => Vec::new(),
        };

        let t0 = Instant::now();
        let app0 = proc_stats();
        let (sample0, rows0) = std::thread::scope(|s| {
            let h = s.spawn(|| container_round(&ep, &ids));
            let out = if running { self.host().run(Privilege::User, None, &["sh", "-c", memory::SAMPLE_SCRIPT]).ok() } else { None };
            (out, h.join().unwrap_or_default())
        });
        let rest = Duration::from_millis(interval_ms).saturating_sub(t0.elapsed());
        std::thread::sleep(rest);
        let (cpu1, rows1) = std::thread::scope(|s| {
            let h = s.spawn(|| container_round(&ep, &ids));
            let out = if running { self.host().run(Privilege::User, None, &["sh", "-c", "echo @@cpu; head -n 1 /proc/stat"]).ok() } else { None };
            (out, h.join().unwrap_or_default())
        });
        let app1 = proc_stats();
        let elapsed = t0.elapsed();

        // proces aplikace
        let app = match (app0, app1) {
            (Some(a), Some(b)) => {
                // 100 ns jednotky -> ns, podíl z uplynulého času (100 % = jedno jádro)
                let cpu = (b.cpu_100ns.saturating_sub(a.cpu_100ns)) as f64 * 100.0 / (elapsed.as_nanos() as f64).max(1.0) * 100.0;
                json!({
                    "pid": std::process::id(),
                    "cpu_pct": round2(cpu),
                    "cpu_time_ms": b.cpu_100ns / 10_000,
                    "working_set": b.working_set,
                    "private_bytes": b.private_bytes,
                })
            }
            _ => json!({ "pid": std::process::id() }),
        };
        let (ram, win_used) = memory::win_ram();
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

        // VM WSL / distribuce
        let vmmem = if wsl_host { self.shared.mem_probe.lock().ok().and_then(|mut p| p.get()) } else { None };
        let mut wsl = json!({ "distro": cfg.distro, "running": running, "vmmem": vmmem });
        if let Some(out) = &sample0 {
            if let Some((mi, engine_rss)) = memory::parse_sample(out) {
                wsl["mem_total"] = json!(mi.total);
                wsl["mem_used"] = json!(mi.used());
                wsl["mem_cache"] = json!(mi.cache());
                wsl["engine_rss"] = json!(engine_rss);
            }
            let cpu = match (memory::parse_cpu(out), cpu1.as_deref().and_then(memory::parse_cpu)) {
                (Some(a), Some(b)) => memory::cpu_pct(a, b),
                _ => None,
            };
            wsl["cpu_pct"] = json!(cpu.map(round2));
        }

        // engine a kontejnery
        let info = snap.as_ref().and_then(|s| s.info.as_ref());
        let engine = json!({
            "name": cfg.engine.name(),
            "connected": connected,
            "containers_running": info.map(|i| i.containers_running),
            "containers_total": info.map(|i| i.containers),
            "ncpu": info.map(|i| i.ncpu),
        });
        let containers: Vec<Value> = rows1
            .iter()
            .map(|(t1, r)| {
                let cpu = rows0.iter().find(|(_, p)| p.id == r.id).and_then(|(t0, p)| {
                    let dt = t1.duration_since(*t0).as_nanos() as f64;
                    (dt > 0.0 && r.cpu_total >= p.cpu_total).then(|| (r.cpu_total - p.cpu_total) as f64 / dt * 100.0)
                });
                json!({
                    "id": r.id.chars().take(12).collect::<String>(),
                    "name": r.name,
                    "cpu_pct": cpu.map(round2),
                    "mem": r.mem,
                    "mem_limit": r.mem_limit,
                    "net_rx": r.net_rx,
                    "net_tx": r.net_tx,
                    "blk_read": r.blk_read,
                    "blk_write": r.blk_write,
                })
            })
            .collect();

        Response::data(json!({
            "time": memory::now_secs(),
            "interval_ms": elapsed.as_millis() as u64,
            "app": app,
            "windows": { "ram_total": ram, "ram_used": win_used, "logical_cpus": cpus },
            "wsl": wsl,
            "engine": engine,
            "containers": containers,
        }))
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Statistiky běžících kontejnerů souběžně (každá odpověď s časem příchodu).
fn container_round(ep: &crate::http::Endpoint, ids: &[(String, String)]) -> Vec<(Instant, crate::docker::StatRow)> {
    std::thread::scope(|s| {
        let hs: Vec<_> = ids
            .iter()
            .map(|(id, name)| s.spawn(move || Docker::new(ep).stats(id, name).map(|r| (Instant::now(), r))))
            .collect();
        hs.into_iter().filter_map(|h| h.join().ok().flatten()).collect()
    })
}

struct ProcStats {
    working_set: u64,
    private_bytes: u64,
    /// kernel + user čas v jednotkách 100 ns
    cpu_100ns: u64,
}

#[cfg(windows)]
fn proc_stats() -> Option<ProcStats> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    // SAFETY: pseudo-handle vlastního procesu, struktury mají nastavenou velikost a žijí po dobu volání.
    unsafe {
        let h = GetCurrentProcess();
        let mut m: PROCESS_MEMORY_COUNTERS_EX = std::mem::zeroed();
        m.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
        if GetProcessMemoryInfo(h, &mut m as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS, m.cb) == 0 {
            return None;
        }
        let (mut c, mut e, mut k, mut u): (FILETIME, FILETIME, FILETIME, FILETIME) = std::mem::zeroed();
        if GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u) == 0 {
            return None;
        }
        let ft = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
        Some(ProcStats { working_set: m.WorkingSetSize as u64, private_bytes: m.PrivateUsage as u64, cpu_100ns: ft(k) + ft(u) })
    }
}

#[cfg(not(windows))]
fn proc_stats() -> Option<ProcStats> {
    None
}

/// Spustí řídicí rouru (jen Windows; běží, dokud běží aplikace).
#[cfg(windows)]
pub(super) fn start(w: Worker) {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    static SERVER: Mutex<Option<crate::npipe::Relay>> = Mutex::new(None);
    let name = crate::wake::control_pipe_name();
    let on_conn: crate::npipe::ConnFn = Arc::new(move |pipe| {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        while !buf.contains(&b'\n') && buf.len() < 64 * 1024 {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        let line = buf.split(|b| *b == b'\n').next().unwrap_or(&[]);
        let out = handle_line(&w, line);
        let _ = (&*pipe).write_all(&out);
        pipe.flush();
        pipe.abort();
    });
    match crate::npipe::Relay::serve_with(&name, on_conn) {
        Ok(r) => {
            if let Ok(mut g) = SERVER.lock() {
                *g = Some(r);
            }
        }
        Err(e) => eprintln!("control pipe {name}: {e}"),
    }
}

#[cfg(not(windows))]
pub(super) fn start(_w: Worker) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_requests() {
        let r: Request = serde_json::from_str(r#"{"cmd":"status"}"#).unwrap();
        assert_eq!(r, Request::Status);
        let r: Request = serde_json::from_str(r#"{"cmd":"distro-use","distro":"Ubuntu"}"#).unwrap();
        assert_eq!(r, Request::DistroUse { distro: "Ubuntu".into(), engine: None, sleep_old: None });
        let r: Request = serde_json::from_str(r#"{"cmd":"distro-use","distro":"X","engine":"podman","sleep_old":true}"#).unwrap();
        assert_eq!(r, Request::DistroUse { distro: "X".into(), engine: Some("podman".into()), sleep_old: Some(true) });
        let r: Request = serde_json::from_str(r#"{"cmd":"metrics"}"#).unwrap();
        assert_eq!(r, Request::Metrics { interval_ms: 1000, containers: true });
        let r: Request = serde_json::from_str(r#"{"cmd":"engine","action":"restart"}"#).unwrap();
        assert_eq!(r, Request::Engine { action: "restart".into() });
        assert!(serde_json::from_str::<Request>(r#"{"cmd":"rm-rf"}"#).is_err());
    }

    #[test]
    fn engine_names() {
        assert_eq!(parse_engine("Docker"), Ok(Engine::Docker));
        assert_eq!(parse_engine("podman"), Ok(Engine::Podman));
        assert!(parse_engine("containerd").is_err());
    }

    #[test]
    fn distro_lookup_is_case_insensitive() {
        let l = vec![wsl::Distro { name: "Ubuntu-24.04".into(), state: "Running".into(), version: "2".into(), default: true }];
        assert_eq!(find_distro(&l, "ubuntu-24.04"), Ok("Ubuntu-24.04".into()));
        assert!(find_distro(&l, "Debian").is_err());
    }

    #[test]
    fn response_json_shape() {
        let v = serde_json::to_value(Response::from(Err("x".into()))).unwrap();
        assert_eq!(v, json!({ "ok": false, "message": "x" }));
        let v = serde_json::to_value(Response::data(json!([1]))).unwrap();
        assert_eq!(v, json!({ "ok": true, "message": "", "data": [1] }));
    }
}
