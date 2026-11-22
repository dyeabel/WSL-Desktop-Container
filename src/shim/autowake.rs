//! Automatické nahození: první příkaz přes shim, když aplikace nebo engine neběží,
//! spustí aplikaci (schovanou v tray), ta nastartuje WSL a engine, a shim počká na API.
//!
//! Hotový stav (aplikace běží, port API odpovídá) stojí jen otevření mutexu a TCP spojení
//! na 127.0.0.1. Vypnutí: `shim_autostart: false` v konfiguraci nebo WSLCD_SHIM_AUTOSTART=0.
//! WSLCD_SHIM_WAIT=<s> mění, jak dlouho se na API čeká (výchozí 120 s).

use crate::engine::Engine;
use crate::wake;
use std::net::{SocketAddr, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Potřebuje příkaz engine? (nápověda, `context`, `completion` ani prázdný příkaz ne)
pub fn needs_engine(args: &[String]) -> bool {
    for a in args {
        if a.starts_with('-') {
            if matches!(a.as_str(), "-h" | "--help" | "-v" | "--version") {
                return false;
            }
            continue;
        }
        return !matches!(a.as_str(), "help" | "context" | "completion");
    }
    false
}

fn port_open(port: u16, ms: u64) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(ms)).is_ok()
}

/// Std handly shimu nesmí zdědit aplikace: držela by rouru (`docker ps | findstr` by
/// nikdy neskončil). Pozdější `wsl.exe` je dostane jako děděné duplikáty (Stdio::inherit).
#[cfg(windows)]
fn stop_std_inheritance() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
    for h in [std::io::stdin().as_raw_handle(), std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()] {
        if !h.is_null() {
            unsafe { SetHandleInformation(h as _, HANDLE_FLAG_INHERIT, 0) };
        }
    }
}

pub fn launch_app() -> Result<(), String> {
    let exe = wake::app_exe().ok_or("WSL Container Desktop not found")?;
    #[cfg(windows)]
    stop_std_inheritance();
    let mut c = Command::new(&exe);
    c.arg(wake::SHIM_ARG).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Some(d) = exe.parent() {
        c.current_dir(d);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        c.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    c.spawn().map(|_| ()).map_err(|e| format!("{}: {e}", exe.display()))
}

/// Aplikace není k dispozici: aspoň nahodit distribuci a služby enginu přímo.
fn start_engine_directly(distro: &str, engine: Engine) {
    let mut c = Command::new("wsl.exe");
    c.args(["-d", distro, "-u", "root", "-e", "systemctl", "start"]).args(engine.services());
    let _ = c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// Zajistí běžící aplikaci a engine na `port` (jen primární cíl aplikace).
pub fn ensure(distro: &str, engine: Engine, port: u16, cs: bool, debug: bool) {
    let up = port_open(port, 300);
    let app = wake::app_running();
    if debug {
        eprintln!("[wslcd-docker] autostart: app {}, API :{port} {}", if app { "running" } else { "not running" }, if up { "up" } else { "down" });
    }
    if up && app {
        return;
    }
    let msg = |c: &str, e: &str| if cs { c.to_string() } else { e.to_string() };
    // signál běžící aplikaci; spuštěná ze shimu nastartuje engine sama
    let mut signaled = false;
    let mut have_app = true;
    if app {
        signaled = wake::signal_start();
    } else if let Err(e) = launch_app() {
        have_app = false;
        if debug || !up {
            eprintln!("wslcd-docker: {}: {e}", msg("aplikaci nelze spustit", "cannot start the app"));
        }
    }
    if up {
        return;
    }
    eprintln!(
        "wslcd-docker: {}",
        msg("engine neběží – spouštím WSL a engine…", "the engine is not running – starting WSL and the engine…")
    );
    if !have_app {
        start_engine_directly(distro, engine);
    }
    let limit = std::env::var("WSLCD_SHIM_WAIT").ok().and_then(|s| s.parse().ok()).unwrap_or(120u64);
    let t = Instant::now();
    loop {
        if port_open(port, 500) {
            eprintln!("wslcd-docker: {} ({:.0} s)", msg("engine běží", "the engine is running"), t.elapsed().as_secs_f64().ceil());
            return;
        }
        if t.elapsed() >= Duration::from_secs(limit) {
            eprintln!(
                "wslcd-docker: {}",
                msg(
                    &format!("API na 127.0.0.1:{port} neodpovědělo do {limit} s – zkouším příkaz i tak"),
                    &format!("the API on 127.0.0.1:{port} did not respond within {limit} s – trying the command anyway")
                )
            );
            return;
        }
        // aplikace mohla být zrovna ve startu (mutex už je, událost ještě ne)
        if app && !signaled {
            signaled = wake::signal_start();
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::needs_engine;

    fn v(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn which_commands_wake_the_engine() {
        for c in ["ps", "compose up -d", "--log-level debug ps", "run --rm alpine --help", "version", "info"] {
            assert!(needs_engine(&v(c)), "{c}");
        }
        for c in ["", "--help", "-v", "help", "context ls", "completion powershell", "--debug --version"] {
            assert!(!needs_engine(&v(c)), "{c}");
        }
    }
}
