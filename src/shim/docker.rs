//! `docker.exe` pro Windows – předává příkazy Dockeru běžícímu ve WSL.
//!
//! `docker ps` ve Windows = `wsl.exe -d <distribuce> --cd <aktuální složka> -e docker ps`.
//! Distribuce se bere z konfigurace WLS Desktop (případně z proměnné WLS_DISTRO).
//! Windows cesty v argumentech se přeloží (C:\x -> /mnt/c/x). Pokud se binárka
//! jmenuje `docker-compose.exe`, chová se jako `docker compose`.
//!
//! Jednoduché příkazy jen pro čtení (`ps`, `images`, `volume ls`, `network ls`, `inspect`,
//! `logs` bez `-f`, `version --format`, `info --format`) odpoví rychlá cesta (`fast.rs`) přímo z API enginu
//! bez spouštění wsl.exe; cokoli jiného (nebo chyba API) jde přes wsl.exe.
//! WLS_SHIM_FAST=0 rychlou cestu vypne.
//!
//! Docker kontexty (`docker context …`, `--context`, `DOCKER_CONTEXT`) vybírají cíl WLS –
//! distribuci, engine, port a TLS (viz `contexts.rs`, `ctxcmd.rs`).
//!
//! WLS_SHIM_DEBUG=1 vypíše na stderr, co se spouští (včetně vybraného kontextu).

#[path = "../config.rs"]
#[allow(dead_code)]
mod config;
#[path = "../engine.rs"]
#[allow(dead_code)]
mod engine;
#[path = "../pathmap.rs"]
mod pathmap;
#[path = "../apitls.rs"]
mod apitls;
#[path = "../dpapi.rs"]
#[allow(dead_code, unused_imports)] // shim jen dešifruje
mod dpapi;
#[path = "../contexts.rs"]
mod contexts;
mod ctxcmd;
mod envfwd;
mod fast;
mod podmanfix;

use std::io::Write;
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let exe = std::env::current_exe().ok();
    let exe_name = exe
        .as_ref()
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();

    // globální --context / -c se do WSL nepředává (Podman ho nezná, kontext řeší shim)
    let (ctx_flag, mut args) = match contexts::strip_context_flag(&args) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(125);
        }
    };
    if exe_name == "docker-compose" && args.first().map(|a| a != "compose").unwrap_or(true) {
        args.insert(0, "compose".into());
    }

    let mut cfg = config::Config::load();
    // přepisy pro ladění / skripty: WLS_ENGINE=podman|docker, WLS_PORT=2376
    match std::env::var("WLS_ENGINE").unwrap_or_default().to_ascii_lowercase().as_str() {
        "podman" => cfg.engine = engine::Engine::Podman,
        "docker" => cfg.engine = engine::Engine::Docker,
        _ => {}
    }
    if let Some(p) = std::env::var("WLS_PORT").ok().and_then(|p| p.parse().ok()) {
        cfg.port = p;
    }
    let distro = std::env::var("WLS_DISTRO").ok().filter(|d| !d.is_empty()).unwrap_or(cfg.distro.clone());
    let default_target = contexts::Target { distro, engine: cfg.engine, port: cfg.port, tls: cfg.tls };
    let debug = std::env::var("WLS_SHIM_DEBUG").is_ok();
    let env_ctx = std::env::var("DOCKER_CONTEXT").ok();
    let store_path = contexts::path();

    // docker context … – vyřídí shim sám
    if args.first().map(|a| a == "context").unwrap_or(false) {
        let Some(store_path) = store_path else {
            eprintln!("wls-docker: APPDATA is not set, cannot store contexts");
            return ExitCode::from(1);
        };
        let env = ctxcmd::CtxEnv { store_path, default: default_target, env_ctx: env_ctx.clone(), distros: &contexts::wsl_distros };
        let r = ctxcmd::run(&args[1..], &env);
        let _ = std::io::stdout().lock().write_all(r.out.as_bytes());
        let _ = std::io::stderr().lock().write_all(r.err.as_bytes());
        return ExitCode::from(r.code);
    }

    // výběr kontextu: --context > DOCKER_CONTEXT > aktuální v contexts.json > default
    let store = match store_path.as_deref().map(contexts::Store::load_from) {
        Some(Ok(s)) => s,
        Some(Err(e)) => {
            eprintln!("wls-docker: cannot read contexts: {e}");
            return ExitCode::from(1);
        }
        None => contexts::Store::default(),
    };
    let ctx_name = contexts::select(ctx_flag.as_deref(), env_ctx.as_deref(), &store).to_string();
    let target = match store.resolve(&ctx_name, &default_target) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(1);
        }
    };
    if debug {
        eprintln!(
            "[wls-docker] context: {ctx_name} (distro {}, {}, port {}{})",
            target.distro,
            target.engine.name(),
            target.port,
            if target.tls { ", tls" } else { "" }
        );
    }

    // rychlá cesta: odpověď přímo z API, bez wsl.exe
    if exe_name != "docker-compose" && std::env::var("WLS_SHIM_FAST").map(|v| v != "0").unwrap_or(true) {
        let t = std::time::Instant::now();
        match fast::try_run(&args, target.port, target.tls) {
            Ok(out) => {
                if debug {
                    eprintln!("[wls-docker] fast path: GET {} ({} ms)", out.paths.join(", GET "), t.elapsed().as_millis());
                }
                // zavřená roura (`| head`) není chyba
                for (stream, data) in &out.chunks {
                    if *stream == 2 {
                        let mut se = std::io::stderr().lock();
                        let _ = se.write_all(data).and_then(|_| se.flush());
                    } else {
                        let mut so = std::io::stdout().lock();
                        let _ = so.write_all(data).and_then(|_| so.flush());
                    }
                }
                return ExitCode::SUCCESS;
            }
            Err(why) => {
                if debug {
                    eprintln!("[wls-docker] fast path not used: {why}");
                }
            }
        }
    } else if debug {
        eprintln!("[wls-docker] fast path disabled");
    }

    // síťové cesty WSL nevidí – srozumitelné varování místo záhadné chyby
    for p in pathmap::unreachable_paths(&args) {
        eprintln!("wls-docker: varování: síťovou cestu {p} WSL nevidí – zkopíruj data na lokální disk nebo do WSL");
    }

    // proměnné prostředí, které příkaz potřebuje (-e VAR, ${VAR} v compose, WLS_FORWARD_ENV)
    let cwd_win = std::env::current_dir().ok();
    let is_compose = args.iter().find(|a| !a.starts_with('-')).map(|a| a == "compose").unwrap_or(false);
    let (wslenv, forwarded) = envfwd::build_wslenv(
        &args,
        is_compose,
        cwd_win.as_deref().unwrap_or(std::path::Path::new(".")),
        &std::env::var("WSLENV").unwrap_or_default(),
        |n| std::env::var_os(n).is_some(),
    );

    let args = pathmap::translate_args(&args);
    // Podman: pole šablon, které se liší od Dockeru (`events --format '{{.Action}}'`)
    let args = if target.engine == engine::Engine::Podman { podmanfix::rewrite(&args) } else { args };
    // Docker -> `docker …`, Podman -> `podman --url tcp://127.0.0.1:<port> …`
    let cli = target.engine.cli(target.port, target.tls);
    // Podman: buildery buildx obslouží skutečný buildx (je-li v distribuci), viz podmanfix
    let cli = if target.engine == engine::Engine::Podman { podmanfix::buildx_cli(&args, &cli).unwrap_or(cli) } else { cli };

    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-d", &target.distro]);
    if let Some(cwd) = &cwd_win {
        let cwd = cwd.display().to_string();
        let cwd = cwd.strip_prefix(r"\\?\").unwrap_or(&cwd).to_string();
        let linux = pathmap::to_wsl_path(&cwd);
        if linux.starts_with('/') {
            cmd.args(["--cd", &linux]);
        }
    }
    cmd.arg("-e").args(&cli).args(&args);
    if !forwarded.is_empty() {
        cmd.env("WSLENV", &wslenv);
    }

    if debug {
        let shown: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        eprintln!("[wls-docker] wsl.exe {}", shown.join(" "));
        if !forwarded.is_empty() {
            eprintln!("[wls-docker] předávám proměnné: {}", forwarded.join(", "));
        }
    }

    match cmd.status() {
        Ok(st) => ExitCode::from(st.code().unwrap_or(1).clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("wls-docker: nelze spustit wsl.exe: {e}");
            ExitCode::from(127)
        }
    }
}
