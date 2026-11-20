//! Výkonnostní a zátěžový test (benchmark) **Docker vs Podman** v distribucích spravovaných aplikací.
//!
//! Spuštění (nejlépe přes `scripts\bench.ps1`, který napřed sestaví release shim):
//! `cargo test --release live_bench -- --ignored --nocapture --test-threads=1`
//!
//! Pro každý engine (distribuce z konfigurace aplikace – `distro_engines`, nebo `WSLCD_BENCH_DOCKER` /
//! `WSLCD_BENCH_PODMAN` = název distribuce) změří:
//! - **funkční kontroly** (ping, info, pull, run + logy, volume, síť, build, statistiky, události, shim),
//! - **latenci** běžných operací po čtyřech cestách: TLS klient aplikace (to, co používá GUI),
//!   `docker.exe` shim z Windows, `wsl.exe -e docker|podman …` z Windows (bez aplikace) a CLI přímo
//!   uvnitř distribuce (základ bez hranice Windows ↔ WSL),
//! - **zátěž API**: souběžní klienti volají `GET /containers/json` (1 / 4 / 16 / 64 vláken),
//! - **dávku kontejnerů**: N kontejnerů vytvořit a spustit paralelně, snapshot UI s nimi, události,
//!   paměť na kontejner, odstranění,
//! - **klidovou spotřebu** démona enginu (RSS) a aplikace (RAM, CPU, velikost EXE).
//!
//! Výsledek: `docs/bench/results.json` a `docs/BENCHMARK.md` (publikovatelné na GitHubu).
//!
//! Přepínání prostředí: API může mít v jednu chvíli jen jedna distribuce (všechny distribuce WSL
//! sdílí síť), proto test pro každý engine přepne primární distribuci stejnou cestou jako
//! *Nastavit jako primární* v aplikaci (`set_primary`, předchozí distribuce se neuspí), změří dobu
//! přepnutí a na konci vrátí původní primární distribuci (a uspí tu, která na začátku neběžela).
//! Skutečná konfigurace aplikace se nemění (`WSLCD_DATA_DIR` = dočasná složka); na konci se ověří,
//! že běžící aplikace se svým certifikátem na API zase dosáhne.
//! `WSLCD_BENCH_SWITCH=0` přepínání vypne: když pak API dané distribuce neodpovídá, spustí se dočasná
//! kopie `wslcd-tls-proxy` na `WSLCD_BENCH_PORT` (výchozí 2386, …), která se na konci zastaví.
//! Kontejnery, volume a sítě testu mají předponu `wslcd-bench` a na konci se odstraní.

use crate::config::Config;
use crate::docker::Docker;
use crate::engine::Engine;
use crate::wsl;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const IMAGE: &str = "docker.io/library/alpine:3.20";
const PREFIX: &str = "wslcd-bench";
/// Dlouho běžící kontejner, který na SIGTERM skončí hned (holé `sleep` jako PID 1 signál ignoruje
/// a Podman by při stop / force remove čekal celý 10s timeout – měřil by se timeout, ne engine).
const IDLE: &[&str] = &["sh", "-c", "trap 'exit 0' TERM; sleep 600 & wait"];

// ---------------------------------------------------------------- výsledky

/// Statistika latence v milisekundách.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Stat {
    pub n: usize,
    pub errors: usize,
    pub min: f64,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
    pub mean: f64,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub ms: f64,
    pub note: String,
}

/// Jedna operace jednou cestou (api / shim / wsl.exe / in-wsl).
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Latency {
    pub op: String,
    pub path: String,
    pub stat: Stat,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Load {
    pub concurrency: usize,
    pub secs: f64,
    pub requests: usize,
    pub errors: usize,
    pub rps: f64,
    pub lat: Stat,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Burst {
    pub containers: usize,
    pub parallel: usize,
    pub create_start_s: f64,
    pub running: usize,
    pub snapshot: Stat,
    pub start_events: usize,
    pub remove_events: usize,
    pub mem_before_mb: f64,
    pub mem_after_mb: f64,
    pub remove_s: f64,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct EngineResult {
    pub engine: String,
    pub distro: String,
    pub version: String,
    pub os: String,
    /// RSS démona enginu v klidu (MB) – dockerd + containerd, resp. služba podman
    pub daemon_rss_mb: f64,
    /// port API použitý testem a zda šlo o dočasnou proxy
    pub port: u16,
    pub temp_proxy: bool,
    /// doba přepnutí primární distribuce na tento engine (s); 0 = už byla primární
    pub switch_s: f64,
    pub checks: Vec<Check>,
    pub latency: Vec<Latency>,
    pub load: Vec<Load>,
    pub burst: Burst,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Footprint {
    pub app_version: String,
    pub app_running: bool,
    pub exe_mb: f64,
    pub shim_mb: f64,
    pub working_set_mb: f64,
    pub private_mb: f64,
    pub threads: u64,
    pub handles: u64,
    pub cpu_pct: f64,
    pub sample_s: f64,
    pub vmmem_mb: f64,
    /// okno aplikace při měření viditelné (Some(false) = v tray); obnova je pak častější
    #[serde(default)]
    pub window_visible: Option<bool>,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Host {
    pub cpu: String,
    pub cores: u64,
    pub ram_gb: f64,
    pub windows: String,
    pub wsl: String,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct Report {
    pub date: String,
    /// původní primární distribuce obnovena a dosažitelná certifikátem běžící aplikace
    pub restored: bool,
    pub app_commit: String,
    pub host: Host,
    pub footprint: Footprint,
    pub engines: Vec<EngineResult>,
}

// ---------------------------------------------------------------- statistika

/// Percentil metodou nejbližšího pořadí (vstup seřazený vzestupně).
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

pub fn stat(mut v: Vec<f64>, errors: usize) -> Stat {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    let r = |x: f64| (x * 10.0).round() / 10.0;
    Stat {
        n,
        errors,
        min: r(v.first().copied().unwrap_or(0.0)),
        p50: r(percentile(&v, 50.0)),
        p95: r(percentile(&v, 95.0)),
        max: r(v.last().copied().unwrap_or(0.0)),
        mean: r(if n == 0 { 0.0 } else { v.iter().sum::<f64>() / n as f64 }),
    }
}

/// `n` měření po `warm` zahřívacích; f vrací Ok(()) / Err.
fn measure(n: usize, warm: usize, mut f: impl FnMut() -> Result<(), String>) -> Stat {
    for _ in 0..warm {
        let _ = f();
    }
    let (mut v, mut errors) = (Vec::with_capacity(n), 0);
    for _ in 0..n {
        let t = Instant::now();
        match f() {
            Ok(()) => v.push(t.elapsed().as_secs_f64() * 1000.0),
            Err(e) => {
                errors += 1;
                if errors == 1 {
                    eprintln!("    chyba: {}", e.lines().next().unwrap_or_default());
                }
            }
        }
    }
    stat(v, errors)
}

// ---------------------------------------------------------------- pomocné

fn root(distro: &str, a: &[&str]) -> Result<String, String> {
    Ok(wsl::run(distro, Some("root"), None, a)?)
}

fn ps_json(script: &str) -> serde_json::Value {
    let o = wsl::hidden("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", script]).output();
    o.ok()
        .and_then(|o| serde_json::from_slice(String::from_utf8_lossy(&o.stdout).trim().as_bytes()).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn run_ok(cmd: &mut Command) -> Result<(), String> {
    let o = cmd.output().map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(())
    } else {
        Err(format!("{} {}", o.status, String::from_utf8_lossy(&o.stderr)))
    }
}

fn shim_path() -> Option<PathBuf> {
    let t = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    ["release", "debug"].iter().map(|p| t.join(p).join("docker.exe")).find(|p| p.exists())
}

/// Cíl testu: distribuce + engine + API (port, TLS klient) + izolovaná datová složka pro shim.
struct Target {
    distro: String,
    engine: Engine,
    port: u16,
    temp_proxy: bool,
    tls: Arc<rustls::ClientConfig>,
    data: PathBuf,
}

impl Target {
    fn api(&self) -> Docker {
        Docker::with_tls(self.port, Some(self.tls.clone()))
    }
    /// CLI enginu uvnitř distribuce (Podman v TLS režimu přes unix socket aplikace).
    fn cli(&self) -> Vec<String> {
        self.engine.cli(self.port, true)
    }
    fn shim(&self, exe: &Path) -> Command {
        let mut c = wsl::hidden(&exe.to_string_lossy());
        c.env("WSLCD_DATA_DIR", &self.data).env_remove("DOCKER_HOST").env_remove("DOCKER_CONTEXT");
        c
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        cleanup(&self.api());
        if self.temp_proxy {
            let _ = root(&self.distro, &["systemctl", "stop", "wslcd-bench-proxy"]);
            eprintln!("  dočasná proxy v {} zastavena", self.distro);
        }
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

/// Připraví cíl: certifikáty z distribuce, API na portu z konfigurace, nebo dočasná proxy.
fn prepare(distro: &str, engine: Engine, cfg_port: u16, bench_port: u16) -> Result<Target, String> {
    let data = std::env::temp_dir().join(format!("wslcd-bench-{}-{}", distro, std::process::id()));
    let tls_dir = data.join("tls");
    std::fs::create_dir_all(&tls_dir).map_err(|e| e.to_string())?;
    let src = "/etc/wsl-container-desktop/tls";
    for f in ["ca.pem", "cert.pem", "key.pem"] {
        let pem = root(distro, &["cat", &format!("{src}/{f}")]).map_err(|e| format!("{distro}: {src}/{f}: {e} (je zapnuté TLS API?)"))?;
        std::fs::write(tls_dir.join(f), pem).map_err(|e| e.to_string())?;
    }
    let tls = Arc::new(crate::docker::tls_config(&tls_dir)?);
    let mut port = cfg_port;
    let mut temp_proxy = false;
    if !Docker::with_tls(port, Some(tls.clone())).ping() {
        // port drží jiná distribuce → kopie wslcd-tls-proxy na jiném portu
        let unit = root(distro, &["systemctl", "cat", "wslcd-tls-proxy"])?;
        let exec = unit.lines().find_map(|l| l.strip_prefix("ExecStart=")).ok_or("wslcd-tls-proxy: ExecStart nenalezen")?;
        let mut args: Vec<String> = exec.split_whitespace().map(String::from).collect();
        let listen = args.iter_mut().find(|a| a.starts_with("OPENSSL-LISTEN:")).ok_or("OPENSSL-LISTEN nenalezen")?;
        let rest = listen.split_once(',').map(|(_, r)| r.to_string()).unwrap_or_default();
        *listen = format!("OPENSSL-LISTEN:{bench_port},{rest}");
        let _ = root(distro, &["systemctl", "stop", "wslcd-bench-proxy"]);
        let _ = root(distro, &["systemctl", "reset-failed", "wslcd-bench-proxy"]);
        let mut a = vec!["systemd-run".to_string(), "--unit=wslcd-bench-proxy".into(), "--collect".into()];
        a.extend(args);
        let a: Vec<&str> = a.iter().map(String::as_str).collect();
        root(distro, &a)?;
        port = bench_port;
        temp_proxy = true;
        let t = Instant::now();
        while !Docker::with_tls(port, Some(tls.clone())).ping() {
            if t.elapsed() > Duration::from_secs(30) {
                let _ = root(distro, &["systemctl", "stop", "wslcd-bench-proxy"]);
                return Err(format!("{distro}: dočasná proxy na portu {port} neodpovídá"));
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }
    // konfigurace pro shim (izolovaná, nic se nezapisuje do skutečné)
    let cfg = Config { distro: distro.into(), engine, port, tls: true, keep_alive: false, ..Default::default() };
    std::fs::write(data.join("config.json"), serde_json::to_string_pretty(&cfg).unwrap()).map_err(|e| e.to_string())?;
    Ok(Target { distro: distro.into(), engine, port, temp_proxy, tls, data })
}

fn cleanup(d: &Docker) {
    if let Ok(list) = d.get::<Vec<serde_json::Value>>(&format!("/containers/json?all=1&filters={}", crate::docker::enc(r#"{"label":["wslcd-bench"]}"#))) {
        for c in list {
            if let Some(id) = c["Id"].as_str() {
                let _ = d.delete(&format!("/containers/{id}?force=1&v=1"));
            }
        }
    }
    let _ = d.delete(&format!("/volumes/{PREFIX}-vol?force=1"));
    let _ = d.delete(&format!("/networks/{PREFIX}-net"));
    let _ = d.delete(&format!("/images/{PREFIX}-build:1?force=1"));
}

fn create(d: &Docker, name: &str, cmd: &[&str]) -> Result<String, String> {
    let body = json!({ "Image": IMAGE, "Cmd": cmd, "Labels": { "wslcd-bench": "1" } });
    let r = d.post_json(&format!("/containers/create?name={name}"), body).map_err(|e| e.to_string())?;
    let v: serde_json::Value = serde_json::from_str(&r).map_err(|e| e.to_string())?;
    v["Id"].as_str().map(String::from).ok_or_else(|| format!("create: {r}"))
}

// ---------------------------------------------------------------- měření

fn checks(t: &Target, shim: Option<&Path>) -> Vec<Check> {
    let d = t.api();
    let mut out = Vec::new();
    let mut check = |name: &str, f: &mut dyn FnMut() -> Result<String, String>| {
        let s = Instant::now();
        let r = f();
        let ms = (s.elapsed().as_secs_f64() * 10000.0).round() / 10.0;
        let (ok, note) = match r {
            Ok(n) => (true, n),
            Err(e) => (false, e.lines().next().unwrap_or_default().chars().take(140).collect()),
        };
        eprintln!("  {} {name} ({ms} ms) {note}", if ok { "OK  " } else { "FAIL" });
        out.push(Check { name: name.into(), ok, ms, note });
    };
    check("API ping (TLS, client certificate)", &mut || d.ping().then(String::new).ok_or("ping".into()));
    check("Plain HTTP without certificate is rejected", &mut || {
        (!Docker::with_tls(t.port, None).ping()).then(String::new).ok_or("plain HTTP answered".into())
    });
    check("Engine info", &mut || {
        let v: serde_json::Value = d.get("/info").map_err(|e| e.to_string())?;
        Ok(format!("{} CPU, {} MB", v["NCPU"], v["MemTotal"].as_u64().unwrap_or(0) / 1_048_576))
    });
    check("Pull image", &mut || d.pull(IMAGE).map(|_| IMAGE.to_string()));
    check("Run container + read logs", &mut || {
        let id = create(&d, &format!("{PREFIX}-logs"), &["sh", "-c", "echo hello-bench"])?;
        d.container_action(&id, "start").map_err(|e| e.to_string())?;
        d.post(&format!("/containers/{id}/wait")).map_err(|e| e.to_string())?;
        let logs = d.request("GET", &format!("/containers/{id}/logs?stdout=1&stderr=1")).call().map_err(|e| e.to_string())?.into_string().map_err(|e| e.to_string())?;
        let _ = d.delete(&format!("/containers/{id}?force=1"));
        logs.contains("hello-bench").then(String::new).ok_or(format!("logs: {logs:?}"))
    });
    check("Stop / start / restart", &mut || {
        let id = create(&d, &format!("{PREFIX}-life"), IDLE)?;
        for a in ["start", "stop?t=0", "start", "restart?t=0"] {
            d.post(&format!("/containers/{id}/{a}")).map_err(|e| format!("{a}: {e}"))?;
        }
        let v: serde_json::Value = d.get(&format!("/containers/{id}/json")).map_err(|e| e.to_string())?;
        let running = v.pointer("/State/Running").and_then(|x| x.as_bool()) == Some(true);
        let st = d.stats(&id, "life");
        let _ = d.delete(&format!("/containers/{id}?force=1"));
        if !running {
            return Err("not running after restart".into());
        }
        Ok(st.map(|s| format!("stats: {} KB RAM", s.mem / 1024)).unwrap_or_else(|| "stats unavailable".into()))
    });
    check("Volume create / remove", &mut || {
        d.post_json("/volumes/create", json!({ "Name": format!("{PREFIX}-vol"), "Labels": { "wslcd-bench": "1" } })).map_err(|e| e.to_string())?;
        d.delete(&format!("/volumes/{PREFIX}-vol")).map(|_| String::new()).map_err(|e| e.to_string())
    });
    check("Network create / remove", &mut || {
        d.post_json("/networks/create", json!({ "Name": format!("{PREFIX}-net") })).map_err(|e| e.to_string())?;
        d.delete(&format!("/networks/{PREFIX}-net")).map(|_| String::new()).map_err(|e| e.to_string())
    });
    check("Build from Dockerfile (app build path)", &mut || {
        let dir = t.data.join("ctx");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("Dockerfile"), format!("FROM {IMAGE}\nRUN echo built > /built\nLABEL wslcd-bench=1\n")).map_err(|e| e.to_string())?;
        let spec = crate::worker::BuildSpec { context: dir.to_string_lossy().into(), tags: format!("{PREFIX}-build:1"), ..Default::default() };
        crate::worker::build::run_build(&t.distro, t.engine, t.port, true, &spec, |_| {})?;
        let _ = d.delete(&format!("/images/{PREFIX}-build:1?force=1"));
        Ok(String::new())
    });
    if let Some(exe) = shim {
        check("docker.exe shim: ps (fast path)", &mut || {
            let o = t.shim(exe).args(["ps", "-a", "--format", "{{.Names}}"]).output().map_err(|e| e.to_string())?;
            o.status.success().then(String::new).ok_or(String::from_utf8_lossy(&o.stderr).into())
        });
        check("docker.exe shim: run --rm (via wsl.exe)", &mut || {
            let o = t.shim(exe).args(["run", "--rm", IMAGE, "echo", "shim-ok"]).output().map_err(|e| e.to_string())?;
            String::from_utf8_lossy(&o.stdout).contains("shim-ok").then(String::new).ok_or(String::from_utf8_lossy(&o.stderr).into())
        });
    }
    out
}

fn latency(t: &Target, shim: Option<&Path>, n: usize, n_run: usize) -> Vec<Latency> {
    let d = t.api();
    let cli = t.cli();
    let mut out = Vec::new();
    let mut push = |op: &str, path: &str, s: Stat| {
        eprintln!("  {op:<10} {path:<14} p50 {:>8.1} ms  p95 {:>8.1} ms  err {}", s.p50, s.p95, s.errors);
        out.push(Latency { op: op.into(), path: path.into(), stat: s });
    };
    let ops: [(&str, &[&str], &str); 4] = [
        ("version", &["version", "--format", "{{.Server.Version}}"], "/version"),
        ("ps", &["ps", "-a"], "/containers/json?all=1"),
        ("images", &["images"], "/images/json"),
        ("run --rm", &["run", "--rm", IMAGE, "true"], ""),
    ];
    for (op, args, api) in ops {
        let cnt = if api.is_empty() { n_run } else { n };
        // 1) TLS klient aplikace
        let s = if api.is_empty() {
            measure(cnt, 1, || {
                let id = create(&d, &format!("{PREFIX}-lat"), &["true"])?;
                d.container_action(&id, "start").map_err(|e| e.to_string())?;
                d.post(&format!("/containers/{id}/wait")).map_err(|e| e.to_string())?;
                d.delete(&format!("/containers/{id}?force=1")).map(|_| ()).map_err(|e| e.to_string())
            })
        } else {
            measure(cnt, 2, || d.get::<serde_json::Value>(api).map(|_| ()).map_err(|e| e.to_string()))
        };
        push(op, "app API (TLS)", s);
        // 2) docker.exe shim
        if let Some(exe) = shim {
            push(op, "docker.exe shim", measure(cnt, 1, || run_ok(t.shim(exe).args(args))));
        }
        // 3) wsl.exe -e <cli> (bez aplikace)
        push(op, "wsl.exe -e CLI", measure(cnt, 1, || run_ok(wsl::base(&t.distro, Some("root"), None).args(&cli).args(args))));
        // 4) CLI uvnitř distribuce (smyčka v jednom wsl.exe, měřeno date +%s%N)
        let script = "n=$1; shift; i=0; while [ $i -lt $n ]; do s=$(date +%s%N); \"$@\" >/dev/null 2>&1 || echo E; e=$(date +%s%N); echo $(( (e-s)/1000 )); i=$((i+1)); done";
        let mut a: Vec<String> = vec!["sh".into(), "-c".into(), script.into(), "sh".into(), (cnt + 1).to_string()];
        a.extend(cli.iter().cloned());
        a.extend(args.iter().map(|s| s.to_string()));
        let o = wsl::run_owned(&t.distro, Some("root"), None, &a).unwrap_or_default();
        let lines: Vec<&str> = o.lines().skip(1).collect(); // první = zahřátí
        let errors = lines.iter().filter(|l| l.trim() == "E").count();
        let v: Vec<f64> = lines.iter().filter_map(|l| l.trim().parse::<f64>().ok()).map(|us| us / 1000.0).collect();
        push(op, "CLI inside WSL", stat(v, errors));
    }
    out
}

fn load(t: &Target, levels: &[usize], secs: f64) -> Vec<Load> {
    let mut out = Vec::new();
    for &c in levels {
        let stop = Arc::new(AtomicBool::new(false));
        let errors = Arc::new(AtomicUsize::new(0));
        let start = Instant::now();
        let lats: Vec<f64> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..c)
                .map(|_| {
                    let d = t.api();
                    let (stop, errors) = (stop.clone(), errors.clone());
                    s.spawn(move || {
                        let mut v = Vec::new();
                        while !stop.load(Ordering::Relaxed) {
                            let q = Instant::now();
                            match d.get::<serde_json::Value>("/containers/json?all=1") {
                                Ok(_) => v.push(q.elapsed().as_secs_f64() * 1000.0),
                                Err(e) => {
                                    if errors.fetch_add(1, Ordering::Relaxed) == 0 {
                                        eprintln!("    první chyba: {e}");
                                    }
                                }
                            }
                        }
                        v
                    })
                })
                .collect();
            std::thread::sleep(Duration::from_secs_f64(secs));
            stop.store(true, Ordering::Relaxed);
            hs.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
        });
        let el = start.elapsed().as_secs_f64();
        let e = errors.load(Ordering::Relaxed);
        let n = lats.len();
        let l = Load { concurrency: c, secs: (el * 10.0).round() / 10.0, requests: n, errors: e, rps: (n as f64 / el * 10.0).round() / 10.0, lat: stat(lats, e) };
        eprintln!("  {c:>3} klientů: {:>8.1} req/s  p50 {:>7.1} ms  p95 {:>7.1} ms  p99≈max {:>7.1}  chyb {e}", l.rps, l.lat.p50, l.lat.p95, l.lat.max);
        if e > 0 && std::env::var("WSLCD_BENCH_DIAG").is_ok() {
            let diag = "pgrep -c socat; ss -tn state established '( sport = :2376 )' | wc -l; ss -ltn | grep -E ':23[0-9]{2}'; \
                        systemctl status wslcd-tls-proxy --no-pager -n 15 2>&1 | tail -20; journalctl -u wslcd-tls-proxy -n 10 --no-pager";
            eprintln!("{}", root(&t.distro, &["sh", "-c", diag]).unwrap_or_else(|e| e));
        }
        out.push(l);
    }
    out
}

fn used_mb(distro: &str) -> f64 {
    root(distro, &["sh", "-c", "free -m | awk '/^Mem:/{print $3}'"]).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0.0)
}

fn burst(t: &Target, n: usize, par: usize) -> Burst {
    let d = t.api();
    let mut b = Burst { containers: n, parallel: par, ..Default::default() };
    let _ = root(&t.distro, &["sh", "-c", "sync; echo 3 > /proc/sys/vm/drop_caches"]);
    std::thread::sleep(Duration::from_secs(2));
    b.mem_before_mb = used_mb(&t.distro);
    // události
    let starts = Arc::new(AtomicUsize::new(0));
    let removes = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));
    {
        let (s, r, done, d) = (starts.clone(), removes.clone(), done.clone(), t.api());
        std::thread::spawn(move || {
            let _ = d.stream_events(|| {}, |e| {
                if e.name().starts_with(PREFIX) {
                    match e.action() {
                        "start" => s.fetch_add(1, Ordering::Relaxed),
                        "destroy" | "remove" => r.fetch_add(1, Ordering::Relaxed),
                        _ => 0,
                    };
                }
                !done.load(Ordering::Relaxed)
            });
        });
    }
    std::thread::sleep(Duration::from_millis(500));
    let next = AtomicUsize::new(0);
    let ids = std::sync::Mutex::new(Vec::new());
    let t0 = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..par {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= n {
                    break;
                }
                if let Ok(id) = create(&d, &format!("{PREFIX}-burst-{i}"), IDLE) {
                    if d.container_action(&id, "start").is_ok() {
                        ids.lock().unwrap().push(id);
                    }
                }
            });
        }
    });
    b.create_start_s = (t0.elapsed().as_secs_f64() * 100.0).round() / 100.0;
    let ids = ids.into_inner().unwrap();
    b.running = ids.len();
    eprintln!("  {} / {n} kontejnerů vytvořeno a spuštěno za {:.2} s ({par} paralelně)", ids.len(), b.create_start_s);
    std::thread::sleep(Duration::from_secs(3));
    b.mem_after_mb = used_mb(&t.distro);
    b.snapshot = measure(10, 1, || {
        let s = d.snapshot();
        s.error.map_or(Ok(()), Err)
    });
    eprintln!("  snapshot UI s {} kontejnery: p50 {:.1} ms, p95 {:.1} ms; RAM {:.0} → {:.0} MB", ids.len(), b.snapshot.p50, b.snapshot.p95, b.mem_before_mb, b.mem_after_mb);
    let next = AtomicUsize::new(0);
    let t1 = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..par {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(id) = ids.get(i) else { break };
                let _ = d.delete(&format!("/containers/{id}?force=1"));
            });
        }
    });
    b.remove_s = (t1.elapsed().as_secs_f64() * 100.0).round() / 100.0;
    std::thread::sleep(Duration::from_secs(2));
    done.store(true, Ordering::Relaxed);
    b.start_events = starts.load(Ordering::Relaxed);
    b.remove_events = removes.load(Ordering::Relaxed);
    eprintln!("  odstraněno za {:.2} s; události start {}, remove/destroy {}", b.remove_s, b.start_events, b.remove_events);
    b
}

fn daemon_rss_mb(distro: &str, engine: Engine) -> f64 {
    let names: &[&str] = match engine {
        Engine::Docker => &["dockerd", "containerd"],
        Engine::Podman => &["podman"],
    };
    let o = root(distro, &["ps", "-eo", "rss=,comm="]).unwrap_or_default();
    let kb: f64 = o
        .lines()
        .filter_map(|l| l.trim().split_once(' ').map(|(r, c)| (r.trim().parse::<f64>().unwrap_or(0.0), c.trim())))
        .filter(|(_, c)| names.contains(c))
        .map(|(r, _)| r)
        .sum();
    (kb / 1024.0 * 10.0).round() / 10.0
}

fn bench_engine(distro: &str, engine: Engine, cfg_port: u16, bench_port: u16, shim: Option<&Path>, quick: bool, switch_s: f64) -> Result<EngineResult, String> {
    let _ = wsl::start(distro);
    let t = prepare(distro, engine, cfg_port, bench_port)?;
    eprintln!("  API 127.0.0.1:{}{}", t.port, if t.temp_proxy { " (dočasná proxy)" } else { "" });
    let d = t.api();
    cleanup(&d);
    let ver: serde_json::Value = d.get("/version").map_err(|e| e.to_string())?;
    let os = root(distro, &["sh", "-c", ". /etc/os-release; echo $PRETTY_NAME"]).unwrap_or_default().trim().to_string();
    let mut r = EngineResult {
        engine: engine.name().into(),
        distro: distro.into(),
        version: ver["Version"].as_str().unwrap_or_default().into(),
        os,
        port: t.port,
        temp_proxy: t.temp_proxy,
        switch_s,
        ..Default::default()
    };
    eprintln!("  {} {} · {}", r.engine, r.version, r.os);
    eprintln!("-- funkční kontroly");
    r.checks = checks(&t, shim);
    std::thread::sleep(Duration::from_secs(3));
    r.daemon_rss_mb = daemon_rss_mb(distro, engine);
    eprintln!("-- RSS démona v klidu: {} MB", r.daemon_rss_mb);
    eprintln!("-- latence");
    r.latency = if quick { latency(&t, shim, 5, 3) } else { latency(&t, shim, 20, 8) };
    eprintln!("-- zátěž API");
    // WSLCD_BENCH_LOAD="1,4,16,64" a WSLCD_BENCH_LOAD_SECS=8 přepíšou úrovně a délku zátěže
    let levels: Vec<usize> = std::env::var("WSLCD_BENCH_LOAD")
        .ok()
        .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_else(|| if quick { vec![1, 16] } else { vec![1, 4, 16, 64] });
    let secs: f64 = std::env::var("WSLCD_BENCH_LOAD_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(if quick { 3.0 } else { 8.0 });
    r.load = load(&t, &levels, secs);
    eprintln!("-- dávka kontejnerů");
    r.burst = if quick { burst(&t, 10, 5) } else { burst(&t, 50, 10) };
    Ok(r)
}

fn host() -> Host {
    let v = ps_json(
        "$c=Get-CimInstance Win32_Processor|select -First 1; $o=Get-CimInstance Win32_OperatingSystem; \
         [pscustomobject]@{cpu=$c.Name.Trim(); cores=[int]$c.NumberOfLogicalProcessors; ram=[math]::Round($o.TotalVisibleMemorySize/1MB,1); os=\"$($o.Caption) $($o.BuildNumber)\"}|ConvertTo-Json -Compress",
    );
    let wsl_v = wsl::hidden("wsl.exe").arg("--version").output().map(|o| wsl::decode(&o.stdout)).unwrap_or_default();
    Host {
        cpu: v["cpu"].as_str().unwrap_or_default().into(),
        cores: v["cores"].as_u64().unwrap_or(0),
        ram_gb: v["ram"].as_f64().unwrap_or(0.0),
        windows: v["os"].as_str().unwrap_or_default().into(),
        wsl: wsl_v.lines().next().unwrap_or_default().split(':').next_back().unwrap_or_default().trim().into(),
    }
}

/// Spotřeba běžící aplikace (tak, jak ji uživatel má spuštěnou) + velikosti EXE.
fn footprint(sample_s: u64, shim: Option<&Path>) -> Footprint {
    let script = format!(
        "$p=Get-Process wsl-container-desktop -ErrorAction SilentlyContinue|select -First 1; \
         $vm=(Get-Process vmmemWSL,vmmem -ErrorAction SilentlyContinue|Measure-Object WorkingSet64 -Sum).Sum; \
         if(!$p){{[pscustomobject]@{{run=$false;vm=$vm}}|ConvertTo-Json -Compress;exit}}; \
         $t0=$p.TotalProcessorTime.TotalMilliseconds; Start-Sleep {sample_s}; $p.Refresh(); $t1=$p.TotalProcessorTime.TotalMilliseconds; \
         Add-Type -Name W -Namespace WslcdBench -MemberDefinition '[DllImport(\"user32.dll\")] public static extern bool IsWindowVisible(IntPtr h);'; \
         $vis=($p.MainWindowHandle -ne 0) -and [WslcdBench.W]::IsWindowVisible($p.MainWindowHandle); \
         [pscustomobject]@{{run=$true;path=$p.Path;ver=$p.FileVersion;ws=$p.WorkingSet64;priv=$p.PrivateMemorySize64;thr=$p.Threads.Count;h=$p.HandleCount;cpu=($t1-$t0);vm=$vm;vis=$vis}}|ConvertTo-Json -Compress"
    );
    let v = ps_json(&script);
    let mb = |b: f64| (b / 1_048_576.0 * 10.0).round() / 10.0;
    let size = |p: &Path| std::fs::metadata(p).map(|m| mb(m.len() as f64)).unwrap_or(0.0);
    let exe = v["path"].as_str().map(PathBuf::from).unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/release/wsl-container-desktop.exe"));
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f64;
    Footprint {
        app_version: v["ver"].as_str().filter(|s| !s.is_empty()).unwrap_or(env!("CARGO_PKG_VERSION")).into(),
        app_running: v["run"].as_bool().unwrap_or(false),
        exe_mb: size(&exe),
        shim_mb: shim.map(size).unwrap_or(0.0),
        working_set_mb: mb(v["ws"].as_f64().unwrap_or(0.0)),
        private_mb: mb(v["priv"].as_f64().unwrap_or(0.0)),
        threads: v["thr"].as_u64().unwrap_or(0),
        handles: v["h"].as_u64().unwrap_or(0),
        // % jednoho jádra → % celého CPU (jako Správce úloh)
        cpu_pct: ((v["cpu"].as_f64().unwrap_or(0.0) / (sample_s as f64 * 1000.0) * 100.0 / cores) * 100.0).round() / 100.0,
        sample_s: sample_s as f64,
        vmmem_mb: mb(v["vm"].as_f64().unwrap_or(0.0)),
        window_visible: v["vis"].as_bool(),
    }
}

/// Konfigurace běžící aplikace (portable `data\` vedle EXE, jinak %APPDATA%) a její datová složka – jen pro čtení.
fn app_config() -> (Config, Option<PathBuf>) {
    let running = ps_json("(Get-Process wsl-container-desktop -ErrorAction SilentlyContinue|select -First 1).Path|ConvertTo-Json");
    let portable = running.as_str().map(PathBuf::from).and_then(|p| p.parent().map(Path::to_path_buf)).filter(|d| d.join(crate::config::PORTABLE_MARKER).exists());
    let dir = portable.map(|d| d.join("data")).or_else(crate::config::data_dir);
    let cfg = dir.as_ref().and_then(|d| std::fs::read_to_string(d.join("config.json")).ok()).and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    (cfg, dir)
}

/// Přepínání primární distribuce; při dropu (i po panice) vrátí původní.
struct Restore {
    orig: Config,
    cur: Config,
    was_running: Vec<String>,
    app_tls: Option<PathBuf>,
    ok: bool,
}

impl Restore {
    fn switch(&mut self, distro: &str, engine: Engine, sleep_old: bool) -> Result<f64, String> {
        let t = Instant::now();
        let (r, log, c) = crate::worker::testkit::set_primary(self.cur.clone(), distro, Some(engine), sleep_old);
        if let Err(e) = r {
            return Err(format!("přepnutí na {distro}: {e}\n{log}"));
        }
        self.cur = Config { keep_alive: false, ..c };
        Ok((t.elapsed().as_secs_f64() * 10.0).round() / 10.0)
    }

    fn restore(&mut self) {
        if self.cur.distro != self.orig.distro {
            let sleep = !self.was_running.contains(&self.cur.distro);
            eprintln!("\n-- návrat primární distribuce na {} ({}){}", self.orig.distro, self.orig.engine.name(), if sleep { format!(", {} se uspí", self.cur.distro) } else { String::new() });
            let (d, e) = (self.orig.distro.clone(), self.orig.engine);
            match self.switch(&d, e, sleep) {
                Ok(s) => eprintln!("  hotovo za {s} s"),
                Err(e) => eprintln!("!! {e}"),
            }
        }
        // běžící aplikace musí na API dosáhnout se svým certifikátem
        let port = self.orig.port;
        self.ok = self.app_tls.as_ref().and_then(|d| crate::docker::tls_config(d).ok()).is_some_and(|c| {
            let c = Arc::new(c);
            let t = Instant::now();
            loop {
                if Docker::with_tls(port, Some(c.clone())).ping() {
                    break true;
                }
                if t.elapsed() > Duration::from_secs(30) {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        eprintln!("  API pro běžící aplikaci ({}:{}): {}", self.orig.distro, port, if self.ok { "OK" } else { "NEDOSTUPNÉ" });
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        if self.cur.distro != self.orig.distro {
            self.restore();
        }
    }
}

#[test]
#[ignore]
fn live_bench() {
    let t = Instant::now();
    let quick = std::env::var("WSLCD_BENCH_QUICK").is_ok_and(|v| v == "1");
    let switch = std::env::var("WSLCD_BENCH_SWITCH").map_or(true, |v| v != "0");
    let (cfg, app_data) = app_config();
    let pick = |var: &str, e: Engine| {
        std::env::var(var).ok().filter(|s| !s.is_empty()).or_else(|| cfg.distro_engines.iter().find(|(_, v)| **v == e).map(|(k, _)| k.clone()))
    };
    let mut targets: Vec<(String, Engine)> = [(pick("WSLCD_BENCH_DOCKER", Engine::Docker), Engine::Docker), (pick("WSLCD_BENCH_PODMAN", Engine::Podman), Engine::Podman)]
        .into_iter()
        .filter_map(|(d, e)| d.map(|d| (d, e)))
        .collect();
    // WSLCD_BENCH_ONLY=docker|podman – jen jeden engine
    if let Ok(only) = std::env::var("WSLCD_BENCH_ONLY") {
        targets.retain(|(_, e)| e.name().eq_ignore_ascii_case(&only));
    }
    assert!(!targets.is_empty(), "žádná distribuce s Dockerem ani Podmanem (distro_engines v konfiguraci nebo WSLCD_BENCH_DOCKER / WSLCD_BENCH_PODMAN)");
    // nejdřív současná primární (bez přepnutí), pak ostatní
    targets.sort_by_key(|(d, _)| *d != cfg.distro);
    let base_port: u16 = std::env::var("WSLCD_BENCH_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(2386);
    let shim = shim_path();
    eprintln!("shim: {:?}; cíle: {targets:?}; primární: {} ({}); přepínání: {switch}; rychlý režim: {quick}", shim, cfg.distro, cfg.engine.name());

    let mut report = Report {
        date: ps_json("(Get-Date -Format 'yyyy-MM-dd')|ConvertTo-Json").as_str().unwrap_or_default().into(),
        app_commit: Command::new("git").args(["rev-parse", "--short", "HEAD"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default(),
        host: host(),
        ..Default::default()
    };
    eprintln!("-- spotřeba aplikace (vzorek {} s)", if quick { 5 } else { 30 });
    report.footprint = footprint(if quick { 5 } else { 30 }, shim.as_deref());
    eprintln!("  {:?}", report.footprint);
    // izolovaná datová složka pro přepínání (skutečná konfigurace aplikace zůstane beze změny)
    let data = std::env::temp_dir().join(format!("wslcd-bench-data-{}", std::process::id()));
    std::fs::create_dir_all(&data).unwrap();
    // SAFETY (env): test běží samostatně (--test-threads=1)
    unsafe { std::env::set_var("WSLCD_DATA_DIR", &data) };
    let was_running: Vec<String> = wsl::list().unwrap_or_default().into_iter().filter(|d| d.running()).map(|d| d.name).collect();
    let mut rs = Restore { orig: cfg.clone(), cur: Config { keep_alive: false, ..cfg.clone() }, was_running, app_tls: app_data.map(|d| d.join("tls")), ok: false };
    for (i, (distro, engine)) in targets.iter().enumerate() {
        eprintln!("\n===== {} v {distro} =====", engine.name());
        let mut switch_s = 0.0;
        if switch && rs.cur.distro != *distro {
            eprintln!("-- přepnutí primární distribuce {} → {distro}", rs.cur.distro);
            match rs.switch(distro, *engine, false) {
                Ok(s) => {
                    switch_s = s;
                    eprintln!("  hotovo za {s} s");
                }
                Err(e) => {
                    eprintln!("!! {e}");
                    continue;
                }
            }
        }
        // keep-alive jako v aplikaci – jinak WSL nečinnou distribuci po pár sekundách bez procesu
        // wsl.exe vypne (provoz přes TCP se nepočítá) a zátěžový test by měřil restart distribuce;
        // u distribuce běžící aplikace už ho drží ona (zámek flock → druhý proces hned skončí)
        let mut ka = wsl::keepalive(distro).ok();
        let r = bench_engine(distro, *engine, rs.cur.port, base_port + i as u16, shim.as_deref(), quick, switch_s);
        if let Some(c) = ka.as_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        match r {
            Ok(r) => report.engines.push(r),
            Err(e) => eprintln!("!! {} v {distro}: {e}", engine.name()),
        }
    }
    rs.restore();
    report.restored = rs.ok;
    let _ = std::fs::remove_dir_all(&data);
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
    std::fs::create_dir_all(docs.join("bench")).unwrap();
    std::fs::write(docs.join("bench").join("results.json"), serde_json::to_string_pretty(&report).unwrap()).unwrap();
    std::fs::write(docs.join("BENCHMARK.md"), render(&report)).unwrap();
    eprintln!("\nhotovo za {:.0} s → docs/BENCHMARK.md, docs/bench/results.json", t.elapsed().as_secs_f64());
    assert!(!report.engines.is_empty(), "žádný engine se nepodařilo změřit");
    assert!(report.restored, "původní primární distribuce není dosažitelná pro běžící aplikaci");
    let failed: Vec<String> = report.engines.iter().flat_map(|e| e.checks.iter().filter(|c| !c.ok).map(move |c| format!("{}: {}", e.engine, c.name))).collect();
    assert!(failed.is_empty(), "neprošlé kontroly: {failed:?}");
}

// ---------------------------------------------------------------- report (Markdown pro GitHub)

fn f1(x: f64) -> String {
    if x >= 100.0 {
        format!("{x:.0}")
    } else {
        format!("{x:.1}")
    }
}

/// Stav okna při měření CPU (viditelné okno = obnova v kratším intervalu než v tray).
fn window_note(fp: &Footprint) -> &'static str {
    match fp.window_visible {
        Some(true) => ", window open",
        Some(false) => ", in the tray",
        None => "",
    }
}

fn find<'a>(e: &'a EngineResult, op: &str, path: &str) -> Option<&'a Stat> {
    e.latency.iter().find(|l| l.op == op && l.path == path).map(|l| &l.stat)
}

/// Vygeneruje `docs/BENCHMARK.md` z výsledků.
pub fn render(r: &Report) -> String {
    let mut s = String::new();
    let w = &mut s;
    use std::fmt::Write;
    let _ = writeln!(w, "# Benchmark: Docker vs Podman in WSL Container Desktop\n");
    let _ = writeln!(
        w,
        "> Generated by `scripts/bench.ps1` (`cargo test --release live_bench -- --ignored`) on {} from commit `{}`. \
         Raw data: [`bench/results.json`](bench/results.json). Numbers come from one machine and are meant for comparing \
         the access paths and the two engines with each other, not as absolute figures.\n",
        r.date, r.app_commit
    );

    // TL;DR
    let _ = writeln!(w, "## Key results\n");
    let fp = &r.footprint;
    if fp.app_running {
        let _ = writeln!(
            w,
            "- **The app itself is light:** {} MB EXE, {} MB working set and **{} % of total CPU** ({:.2} % of one core) while idle{}, {} threads.",
            f1(fp.exe_mb),
            f1(fp.working_set_mb),
            fp.cpu_pct,
            fp.cpu_pct * r.host.cores.max(1) as f64,
            window_note(fp),
            fp.threads
        );
    }
    for e in &r.engines {
        if let (Some(shim), Some(wsl)) = (find(e, "ps", "docker.exe shim"), find(e, "ps", "wsl.exe -e CLI")) {
            if shim.p50 > 0.0 {
                let _ = writeln!(
                    w,
                    "- **{}:** `docker ps` from PowerShell through the app's shim takes **{} ms** (p50) vs **{} ms** with plain `wsl.exe -e {} ps` – **{:.1}× faster**.",
                    e.engine, f1(shim.p50), f1(wsl.p50), e.engine.to_lowercase(), wsl.p50 / shim.p50
                );
            }
        }
    }
    if let Some(best) = r.engines.iter().filter_map(|e| e.load.iter().max_by(|a, b| a.rps.total_cmp(&b.rps)).map(|l| (e, l))).max_by(|a, b| a.1.rps.total_cmp(&b.1.rps)) {
        let _ = writeln!(w, "- **API under load:** up to **{} requests/s** (`GET /containers/json`, {}, {} parallel clients over mutual TLS).", f1(best.1.rps), best.0.engine, best.1.concurrency);
    }
    let all: usize = r.engines.iter().map(|e| e.checks.len()).sum();
    let ok: usize = r.engines.iter().map(|e| e.checks.iter().filter(|c| c.ok).count()).sum();
    let _ = writeln!(w, "- **Functional checks:** {ok} / {all} passed.\n");

    // stroj
    let h = &r.host;
    let _ = writeln!(w, "## Test machine\n\n| | |\n|---|---|");
    let _ = writeln!(w, "| CPU | {} ({} logical cores) |\n| RAM | {} GB |\n| Windows | {} |\n| WSL | {} |", h.cpu, h.cores, h.ram_gb, h.windows, h.wsl);
    let _ = writeln!(w, "| App | WSL Container Desktop {} |", fp.app_version);
    for e in &r.engines {
        let _ = writeln!(w, "| {} | {} in distribution `{}` ({}) |", e.engine, e.version, e.distro, e.os);
    }
    let _ = writeln!(
        w,
        "\nBoth engines were measured one after another. Only one distribution can own the API port at a time (all WSL 2 \
         distributions share one network), so the benchmark switches the primary distribution the same way as *Make primary* \
         in the app, measures the engine through its TLS API with a client certificate (the app's default) and switches back at the end.\n"
    );

    // spotřeba
    let _ = writeln!(w, "## Resource footprint\n\n| Metric | Value |\n|---|---|");
    let _ = writeln!(w, "| `wsl-container-desktop.exe` size | {} MB |\n| `docker.exe` shim size | {} MB |", f1(fp.exe_mb), f1(fp.shim_mb));
    if fp.app_running {
        let _ = writeln!(
            w,
            "| App working set / private memory | {} MB / {} MB |\n| App CPU while idle | {} % of total CPU = {:.2} % of one core ({} s sample{}) |\n| Threads / handles | {} / {} |",
            f1(fp.working_set_mb),
            f1(fp.private_mb),
            fp.cpu_pct,
            fp.cpu_pct * r.host.cores.max(1) as f64,
            fp.sample_s,
            window_note(fp),
            fp.threads,
            fp.handles
        );
    }
    if fp.vmmem_mb > 0.0 {
        let _ = writeln!(w, "| WSL VM (`vmmemWSL`) working set, all distributions | {} MB |", f1(fp.vmmem_mb));
    }
    for e in &r.engines {
        let what = if e.engine == "Docker" { "dockerd + containerd" } else { "podman API service" };
        let _ = writeln!(w, "| {} daemon RSS while idle ({what}) | {} MB |", e.engine, f1(e.daemon_rss_mb));
    }
    for e in &r.engines {
        if e.switch_s > 0.0 {
            let _ = writeln!(w, "| Switch the primary engine to {} in `{}` (*Make primary*: API moved, certificates, keep-alive) | {:.1} s |", e.engine, e.distro, e.switch_s);
        }
    }
    let _ = writeln!(w, "\nNo extra helper distribution, background service or browser engine is installed on Windows – the engine runs in a distribution you already have.\n");

    // latence
    let paths = ["app API (TLS)", "docker.exe shim", "wsl.exe -e CLI", "CLI inside WSL"];
    let _ = writeln!(w, "## Latency of everyday operations\n");
    let _ = writeln!(
        w,
        "Four ways to reach the same engine:\n\n\
         - **app API (TLS)** – the Rust client the GUI uses (mutual TLS, keep-alive);\n\
         - **docker.exe shim** – the `docker` command the app puts on PATH in Windows (read-only commands answered directly from the API, the rest through `wsl.exe`);\n\
         - **wsl.exe -e CLI** – what you would type without the app: `wsl -d <distro> -e docker …`;\n\
         - **CLI inside WSL** – the engine's own CLI run inside the distribution (no Windows ↔ WSL boundary; baseline).\n"
    );
    let mut hdr = String::from("| Operation | Path |");
    let mut sep = String::from("|---|---|");
    for e in &r.engines {
        let _ = write!(hdr, " {} p50 | {} p95 |", e.engine, e.engine);
        sep.push_str("--:|--:|");
    }
    let _ = writeln!(w, "{hdr}\n{sep}");
    let ops: Vec<String> = r.engines.first().map(|e| {
        let mut v: Vec<String> = Vec::new();
        for l in &e.latency {
            if !v.contains(&l.op) {
                v.push(l.op.clone());
            }
        }
        v
    }).unwrap_or_default();
    for op in &ops {
        for p in paths {
            if r.engines.iter().all(|e| find(e, op, p).is_none()) {
                continue;
            }
            let mut row = format!("| `{op}` | {p} |");
            for e in &r.engines {
                match find(e, op, p) {
                    Some(st) if st.n > 0 => {
                        let _ = write!(row, " {} ms | {} ms |", f1(st.p50), f1(st.p95));
                    }
                    _ => row.push_str(" – | – |"),
                }
            }
            let _ = writeln!(w, "{row}");
        }
    }
    let _ = writeln!(w, "\nAll times in milliseconds; read operations 20 runs, `run --rm` 8 runs, after warm-up.\n");
    for e in &r.engines {
        let vals: Vec<(String, f64)> = paths.iter().filter_map(|p| find(e, "ps", p).filter(|s| s.n > 0).map(|s| (p.to_string(), s.p50))).collect();
        if vals.len() < 2 {
            continue;
        }
        let max = vals.iter().map(|v| v.1).fold(0.0, f64::max);
        let _ = writeln!(w, "```mermaid\nxychart-beta\n    title \"{}: 'ps' latency p50 (ms, lower is better)\"", e.engine);
        let _ = writeln!(w, "    x-axis [{}]", vals.iter().map(|v| format!("\"{}\"", v.0)).collect::<Vec<_>>().join(", "));
        let _ = writeln!(w, "    y-axis \"ms\" 0 --> {}", (max * 1.15).ceil());
        let _ = writeln!(w, "    bar [{}]\n```\n", vals.iter().map(|v| f1(v.1)).collect::<Vec<_>>().join(", "));
    }

    // zátěž
    let _ = writeln!(w, "## Load test: engine API\n\nParallel clients calling `GET /containers/json?all=1` through the app's TLS client for a fixed time.\n");
    let mut hdr = String::from("| Parallel clients |");
    let mut sep = String::from("|--:|");
    for e in &r.engines {
        let _ = write!(hdr, " {0} req/s | {0} p50 | {0} p95 | {0} errors |", e.engine);
        sep.push_str("--:|--:|--:|--:|");
    }
    let _ = writeln!(w, "{hdr}\n{sep}");
    let levels: Vec<usize> = r.engines.first().map(|e| e.load.iter().map(|l| l.concurrency).collect()).unwrap_or_default();
    for c in &levels {
        let mut row = format!("| {c} |");
        for e in &r.engines {
            match e.load.iter().find(|l| l.concurrency == *c) {
                Some(l) => {
                    let _ = write!(row, " {} | {} ms | {} ms | {} |", f1(l.rps), f1(l.lat.p50), f1(l.lat.p95), l.errors);
                }
                None => row.push_str(" – | – | – | – |"),
            }
        }
        let _ = writeln!(w, "{row}");
    }
    if r.engines.len() >= 2 && !levels.is_empty() {
        let _ = writeln!(w, "\n```mermaid\nxychart-beta\n    title \"API throughput (req/s, higher is better)\"\n    x-axis \"parallel clients\" [{}]", levels.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(", "));
        let max = r.engines.iter().flat_map(|e| e.load.iter().map(|l| l.rps)).fold(0.0, f64::max);
        let _ = writeln!(w, "    y-axis \"req/s\" 0 --> {}", (max * 1.15).ceil());
        for e in &r.engines {
            let _ = writeln!(w, "    line [{}]", e.load.iter().map(|l| f1(l.rps)).collect::<Vec<_>>().join(", "));
        }
        let _ = writeln!(w, "```\n\nLines in order: {}.", r.engines.iter().map(|e| e.engine.as_str()).collect::<Vec<_>>().join(", then "));
    }

    // dávka
    let _ = writeln!(w, "\n## Load test: many containers at once\n");
    let mut hdr = String::from("| Metric |");
    let mut sep = String::from("|---|");
    for e in &r.engines {
        let _ = write!(hdr, " {} |", e.engine);
        sep.push_str("--:|");
    }
    let _ = writeln!(w, "{hdr}\n{sep}");
    type Row = (&'static str, fn(&Burst) -> String);
    let rows: [Row; 8] = [
        ("Containers created + started (parallel)", |b| format!("{} / {} ({}×)", b.running, b.containers, b.parallel)),
        ("Time to create + start all", |b| format!("{:.2} s", b.create_start_s)),
        ("Throughput", |b| format!("{:.1} containers/s", if b.create_start_s > 0.0 { b.running as f64 / b.create_start_s } else { 0.0 })),
        ("App refresh (snapshot) with all running, p50", |b| format!("{} ms", f1(b.snapshot.p50))),
        ("App refresh (snapshot) with all running, p95", |b| format!("{} ms", f1(b.snapshot.p95))),
        ("Memory per idle container (approx.)", |b| format!("{:.1} MB", if b.running > 0 { (b.mem_after_mb - b.mem_before_mb).max(0.0) / b.running as f64 } else { 0.0 })),
        ("Events received (start / remove)", |b| format!("{} / {} of {}", b.start_events, b.remove_events, b.running)),
        ("Time to force-remove all", |b| format!("{:.2} s", b.remove_s)),
    ];
    for (name, f) in rows {
        let mut row = format!("| {name} |");
        for e in &r.engines {
            let _ = write!(row, " {} |", f(&e.burst));
        }
        let _ = writeln!(w, "{row}");
    }

    // funkční kontroly
    let _ = writeln!(w, "\n## Functional checks\n");
    let mut hdr = String::from("| Check |");
    let mut sep = String::from("|---|");
    for e in &r.engines {
        let _ = write!(hdr, " {} |", e.engine);
        sep.push_str(":-:|");
    }
    let _ = writeln!(w, "{hdr}\n{sep}");
    let names: Vec<String> = r.engines.iter().flat_map(|e| e.checks.iter().map(|c| c.name.clone())).fold(Vec::new(), |mut v, n| {
        if !v.contains(&n) {
            v.push(n);
        }
        v
    });
    for n in &names {
        let mut row = format!("| {n} |");
        for e in &r.engines {
            match e.checks.iter().find(|c| &c.name == n) {
                Some(c) if c.ok => {
                    let _ = write!(row, " ✅ {} ms |", f1(c.ms));
                }
                Some(c) => {
                    let _ = write!(row, " ❌ {} |", c.note.replace('|', "/"));
                }
                None => row.push_str(" – |"),
            }
        }
        let _ = writeln!(w, "{row}");
    }

    // srovnání a metodika
    let _ = writeln!(
        w,
        "\n## Docker or Podman?\n\n\
         WSL Container Desktop runs either engine with the same UI, the same `docker` command in Windows, the same TLS-protected API \
         and the same named pipe for IDEs – you can keep both in two distributions side by side and switch the primary one with one click. \
         Use the tables above to pick: Docker is the reference implementation of the API and of Compose; Podman has no long-running \
         containerd stack and is daemonless by design (the app runs its API service only so that Windows tools can reach it).\n\n\
         ## Why use WSL Container Desktop\n\n\
         - **No Docker Desktop required** – no helper WSL distributions, no Windows services, no web-based UI; one native EXE of a few MB with near-zero idle CPU (see *Resource footprint*).\n\
         - **The `docker` command just works from PowerShell** – and the shim answers read-only commands straight from the API, several times faster than going through `wsl.exe` (see *Latency*).\n\
         - **Docker and Podman with one tool** – the same features for both engines, both verified by the functional checks above.\n\
         - **Secure by default** – the API is reachable only with the client certificate the app generates; plain HTTP is rejected (functional check).\n\
         - **Free for any use** – no per-seat subscription for the GUI.\n\n\
         ## Method and reproducing\n\n\
         ```powershell\n\
         powershell -ExecutionPolicy Bypass -File scripts\\bench.ps1          # full run (~10 minutes)\n\
         powershell -ExecutionPolicy Bypass -File scripts\\bench.ps1 -Quick   # smoke run (~2 minutes)\n\
         ```\n\n\
         - Distributions are taken from the app configuration (`distro_engines`), or from `WSLCD_BENCH_DOCKER` / `WSLCD_BENCH_PODMAN`.\n\
         - For each engine the test makes its distribution primary (as *Make primary* does, with an isolated copy of the app configuration); \
           at the end it restores the original primary and checks that the running app reaches its API again. \
           Containers, volumes, networks and images created by the test are prefixed `wslcd-bench` and removed.\n\
         - Test containers trap `SIGTERM`, so stop and removal times measure the engine, not the 10 s stop timeout that a plain `sleep` as PID 1 causes.\n\
         - App footprint is read from the running `wsl-container-desktop.exe` as it is (window or tray). CPU is the process CPU time over the sample \
           divided by wall time and by the number of logical cores – a share of the whole CPU like Task Manager. The method was checked against \
           the Windows performance counter (`Win32_PerfRawData_PerfProc_Process`, identical result) and a calibration load (one busy core = 6.24 % \
           measured vs 6.25 % expected on 16 cores). With the window in the tray the app refreshes less often, so idle CPU is lower still.\n\
         - Memory per container is the difference of `free -m` inside the WSL VM before and after starting the batch, divided by the number of containers.\n\
         - Docker Desktop and Podman Desktop were **not** installed on the test machine, so they are not part of the measurements.\n"
    );
    s
}

/// Jen spotřeba běžící aplikace (rychlá kontrola měření): `cargo test live_footprint -- --ignored --nocapture`
#[test]
#[ignore]
fn live_footprint() {
    let f = footprint(5, shim_path().as_deref());
    eprintln!("{f:?}");
    assert!(f.app_running, "aplikace neběží");
    assert!(f.window_visible.is_some(), "stav okna se nepodařilo zjistit");
}

/// Znovu vygeneruje `docs/BENCHMARK.md` z `docs/bench/results.json` bez měření
/// (po změně šablony reportu): `cargo test bench_render -- --ignored`
#[test]
#[ignore]
fn bench_render() {
    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
    let r: Report = serde_json::from_str(&std::fs::read_to_string(docs.join("bench").join("results.json")).unwrap()).unwrap();
    std::fs::write(docs.join("BENCHMARK.md"), render(&r)).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_nearest_rank() {
        let v: Vec<f64> = (1..=100).map(|x| x as f64).collect();
        assert_eq!(percentile(&v, 50.0), 50.0);
        assert_eq!(percentile(&v, 95.0), 95.0);
        assert_eq!(percentile(&v, 100.0), 100.0);
        assert_eq!(percentile(&[7.0], 95.0), 7.0);
        assert_eq!(percentile(&[], 50.0), 0.0);
    }

    #[test]
    fn stat_sorts_and_rounds() {
        let s = stat(vec![3.0, 1.0, 2.04, 10.0], 1);
        assert_eq!((s.n, s.errors, s.min, s.max), (4, 1, 1.0, 10.0));
        assert_eq!(s.p50, 2.0);
        assert_eq!(s.mean, 4.0);
        assert_eq!(stat(vec![], 2), Stat { errors: 2, ..Default::default() });
    }

    fn sample() -> Report {
        let lat = |op: &str, path: &str, p50: f64| Latency { op: op.into(), path: path.into(), stat: Stat { n: 20, p50, p95: p50 * 1.5, ..Default::default() } };
        let eng = |name: &str, f: f64| EngineResult {
            engine: name.into(),
            distro: "D".into(),
            version: "1.0".into(),
            checks: vec![Check { name: "API ping".into(), ok: true, ms: 1.0, note: String::new() }, Check { name: "Build".into(), ok: name == "Docker", ms: 5.0, note: "x|y".into() }],
            latency: vec![lat("ps", "app API (TLS)", 2.0 * f), lat("ps", "docker.exe shim", 20.0 * f), lat("ps", "wsl.exe -e CLI", 200.0 * f), lat("ps", "CLI inside WSL", 40.0 * f)],
            load: vec![Load { concurrency: 1, rps: 300.0 / f, ..Default::default() }, Load { concurrency: 16, rps: 1200.0 / f, ..Default::default() }],
            burst: Burst { containers: 10, running: 10, parallel: 5, create_start_s: 2.0, mem_before_mb: 100.0, mem_after_mb: 120.0, ..Default::default() },
            ..Default::default()
        };
        Report { date: "2026-01-01".into(), app_commit: "abc".into(), footprint: Footprint { app_running: true, exe_mb: 12.0, working_set_mb: 40.0, ..Default::default() }, engines: vec![eng("Docker", 1.0), eng("Podman", 2.0)], ..Default::default() }
    }

    #[test]
    fn render_contains_comparison() {
        let md = render(&sample());
        assert!(md.starts_with("# Benchmark"));
        assert!(md.contains("| `ps` | docker.exe shim | 20.0 ms | 30.0 ms | 40.0 ms | 60.0 ms |"), "{md}");
        assert!(md.contains("**10.0× faster**"));
        assert!(md.contains("up to **1200 requests/s**"));
        assert!(md.contains("3 / 4 passed"));
        assert!(md.contains("❌ x/y"), "svislítko v poznámce nesmí rozbít tabulku");
        assert!(md.contains("| Memory per idle container (approx.) | 2.0 MB | 2.0 MB |"));
        assert_eq!(md.matches("```mermaid").count(), 3);
    }

    #[test]
    fn report_json_roundtrip() {
        let r = sample();
        let back: Report = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.engines.len(), 2);
        assert_eq!(back.engines[1].latency[0].stat, r.engines[1].latency[0].stat);
    }
}
