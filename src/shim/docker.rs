//! `docker.exe` pro Windows – předává příkazy Dockeru běžícímu ve WSL.
//!
//! `docker ps` ve Windows = `wsl.exe -d <distribuce> --cd <aktuální složka> -e docker ps`.
//! Distribuce se bere z konfigurace WSL Container Desktop (případně z proměnné WSLCD_DISTRO).
//! Windows cesty v argumentech se přeloží (C:\x -> /mnt/c/x). Pokud se binárka
//! jmenuje `docker-compose.exe`, chová se jako `docker compose`.
//!
//! Jednoduché příkazy jen pro čtení (`ps`, `images`, `volume ls`, `network ls`, `inspect`,
//! `logs` bez `-f`, `version --format`, `info --format`) odpoví rychlá cesta (`fast.rs`) přímo z API enginu
//! bez spouštění wsl.exe; cokoli jiného (nebo chyba API) jde přes wsl.exe.
//! WSLCD_SHIM_FAST=0 rychlou cestu vypne.
//!
//! Docker kontexty (`docker context …`, `--context`, `DOCKER_CONTEXT`) vybírají cíl WSL Container Desktop –
//! distribuci, engine, port a TLS (viz `contexts.rs`, `ctxcmd.rs`).
//!
//! Když aplikace nebo engine neběží, první příkaz je nahodí a počká na API (`autowake.rs`).
//!
//! WSLCD_SHIM_DEBUG=1 vypíše na stderr, co se spouští (včetně vybraného kontextu).

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
#[path = "../wake.rs"]
mod wake;
mod autowake;
mod ctxcmd;
mod envfwd;
mod events;
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
    // přepisy pro ladění / skripty: WSLCD_ENGINE=podman|docker, WSLCD_PORT=2376
    match std::env::var("WSLCD_ENGINE").unwrap_or_default().to_ascii_lowercase().as_str() {
        "podman" => cfg.engine = engine::Engine::Podman,
        "docker" => cfg.engine = engine::Engine::Docker,
        _ => {}
    }
    if let Some(p) = std::env::var("WSLCD_PORT").ok().and_then(|p| p.parse().ok()) {
        cfg.port = p;
    }
    let distro = std::env::var("WSLCD_DISTRO").ok().filter(|d| !d.is_empty()).unwrap_or(cfg.distro.clone());
    let default_target = contexts::Target { distro, engine: cfg.engine, port: cfg.port, tls: cfg.tls };
    let debug = std::env::var("WSLCD_SHIM_DEBUG").is_ok();
    let env_ctx = std::env::var("DOCKER_CONTEXT").ok();
    let store_path = contexts::path();

    // docker context … – vyřídí shim sám
    if args.first().map(|a| a == "context").unwrap_or(false) {
        let Some(store_path) = store_path else {
            eprintln!("wslcd-docker: APPDATA is not set, cannot store contexts");
            return ExitCode::from(1);
        };
        let env = ctxcmd::CtxEnv { store_path, default: default_target, env_ctx: env_ctx.clone(), docker_host: std::env::var("DOCKER_HOST").ok(), distros: &contexts::wsl_distros };
        let r = ctxcmd::run(&args[1..], &env);
        let _ = std::io::stdout().lock().write_all(r.out.as_bytes());
        let _ = std::io::stderr().lock().write_all(r.err.as_bytes());
        return ExitCode::from(r.code);
    }

    // výběr kontextu: --context > DOCKER_CONTEXT > aktuální v contexts.json > default
    let store = match store_path.as_deref().map(contexts::Store::load_from) {
        Some(Ok(s)) => s,
        Some(Err(e)) => {
            eprintln!("wslcd-docker: cannot read contexts: {e}");
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
            "[wslcd-docker] context: {ctx_name} (distro {}, {}, port {}{})",
            target.distro,
            target.engine.name(),
            target.port,
            if target.tls { ", tls" } else { "" }
        );
    }

    // neběží aplikace / engine -> spustit a počkat na API (jen primární cíl aplikace)
    let autostart = cfg.shim_autostart && std::env::var("WSLCD_SHIM_AUTOSTART").map(|v| v != "0").unwrap_or(true);
    if autostart && target.distro == cfg.distro && target.port == cfg.port && autowake::needs_engine(&args) {
        autowake::ensure(&target.distro, target.engine, target.port, cfg.language != "en", debug);
    }

    // rychlá cesta: odpověď přímo z API, bez wsl.exe
    if exe_name != "docker-compose" && std::env::var("WSLCD_SHIM_FAST").map(|v| v != "0").unwrap_or(true) {
        // Podman: `docker events` z API ve schématu Dockeru (podman events má vlastní)
        if target.engine == engine::Engine::Podman {
            if let Some(t) = fast::try_podman_version(&args, target.port, target.tls, &ctx_name) {
                let _ = std::io::stdout().lock().write_all(t.as_bytes());
                return ExitCode::SUCCESS;
            }
            match events::run(&args, target.port, target.tls, debug) {
                Ok(true) => return ExitCode::SUCCESS,
                Ok(false) => {}
                Err(e) => {
                    eprintln!("Error: {e}");
                    return ExitCode::from(1);
                }
            }
        }
        let t = std::time::Instant::now();
        match fast::try_run(&args, target.port, target.tls) {
            Ok(out) => {
                if debug {
                    eprintln!("[wslcd-docker] fast path: GET {} ({} ms)", out.paths.join(", GET "), t.elapsed().as_millis());
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
                    eprintln!("[wslcd-docker] fast path not used: {why}");
                }
            }
        }
    } else if debug {
        eprintln!("[wslcd-docker] fast path disabled");
    }

    // síťové cesty WSL nevidí – srozumitelné varování místo záhadné chyby
    for p in pathmap::unreachable_paths(&args) {
        eprintln!("wslcd-docker: varování: síťovou cestu {p} WSL nevidí – zkopíruj data na lokální disk nebo do WSL");
    }

    // proměnné prostředí, které příkaz potřebuje (-e VAR, ${VAR} v compose, WSLCD_FORWARD_ENV)
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
    // Podman: `compose ps` přes JSON, aby STATUS ukázal i stav healthchecku
    let ps_plan = if target.engine == engine::Engine::Podman { podmanfix::compose_ps(&args) } else { None };
    let args = match &ps_plan {
        Some((a, _)) => a.clone(),
        None => args,
    };

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
        eprintln!("[wslcd-docker] wsl.exe {}", shown.join(" "));
        if !forwarded.is_empty() {
            eprintln!("[wslcd-docker] předávám proměnné: {}", forwarded.join(", "));
        }
    }

    if let Some((_, mode)) = &ps_plan {
        // stdout zachytit a vypsat upravený; stderr (chyby compose) jde rovnou
        cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::inherit());
        return match cmd.output() {
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout);
                let shown = if o.status.success() { podmanfix::render_compose_ps(&text, mode) } else { None };
                let mut so = std::io::stdout().lock();
                let _ = so.write_all(shown.as_deref().unwrap_or(&text).as_bytes()).and_then(|_| so.flush());
                ExitCode::from(o.status.code().unwrap_or(1).clamp(0, 255) as u8)
            }
            Err(e) => {
                eprintln!("wslcd-docker: nelze spustit wsl.exe: {e}");
                ExitCode::from(127)
            }
        };
    }

    match cmd.status() {
        Ok(st) => ExitCode::from(st.code().unwrap_or(1).clamp(0, 255) as u8),
        Err(e) => {
            eprintln!("wslcd-docker: nelze spustit wsl.exe: {e}");
            ExitCode::from(127)
        }
    }
}
