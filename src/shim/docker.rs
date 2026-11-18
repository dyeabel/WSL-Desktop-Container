//! `docker.exe` pro Windows – předává příkazy Dockeru běžícímu ve WSL.
//!
//! `docker ps` ve Windows = `wsl.exe -d <distribuce> --cd <aktuální složka> -e docker ps`.
//! Distribuce se bere z konfigurace WLS Desktop (případně z proměnné WLS_DISTRO).
//! Windows cesty v argumentech se přeloží (C:\x -> /mnt/c/x). Pokud se binárka
//! jmenuje `docker-compose.exe`, chová se jako `docker compose`.
//!
//! WLS_SHIM_DEBUG=1 vypíše na stderr, co se spouští.

#[path = "../config.rs"]
#[allow(dead_code)]
mod config;
#[path = "../engine.rs"]
#[allow(dead_code)]
mod engine;
#[path = "../pathmap.rs"]
mod pathmap;
mod envfwd;

use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let exe = std::env::current_exe().ok();
    let exe_name = exe
        .as_ref()
        .and_then(|p| p.file_stem())
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    let mut args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    if exe_name == "docker-compose" {
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
    let debug = std::env::var("WLS_SHIM_DEBUG").is_ok();

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
    // Docker -> `docker …`, Podman -> `podman --url tcp://127.0.0.1:<port> …`
    let cli = cfg.engine.cli(cfg.port, cfg.tls);

    let mut cmd = Command::new("wsl.exe");
    cmd.args(["-d", &distro]);
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
