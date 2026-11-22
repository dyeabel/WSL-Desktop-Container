//! Probuzení aplikace a enginu z docker shimu (sdílí aplikace i `docker.exe`).
//!
//! - aplikace běží = existuje pojmenovaný mutex jedné instance ([`MUTEX`]);
//! - běžící aplikace čeká na pojmenovanou událost [`EVENT`] a po jejím nastavení
//!   nastartuje engine (stejně jako „Spustit / probudit engine“ v tray);
//! - neběžící aplikaci shim spustí s [`SHIM_ARG`] – schová se do tray a engine nastartuje;
//! - aplikace si při startu zapíše cestu ke svému EXE do datové složky ([`APP_PATH_FILE`]),
//!   aby ji našel i shim zkopírovaný mimo složku aplikace.

#![allow(dead_code)] // každá binárka používá jen část

use std::path::PathBuf;

pub const MUTEX: &str = "Local\\WSL-Container-Desktop-single-instance";
pub const EVENT: &str = "Local\\WSL-Container-Desktop-start-engine";
/// Aplikaci spustil shim: start schovaný v tray + nastartovat engine.
pub const SHIM_ARG: &str = "--from-shim";
pub const APP_PATH_FILE: &str = "app-path.txt";

/// Řídicí roura běžící aplikace pro `wslcd status | distro | engine | memory | metrics`
/// (název obsahuje jméno uživatele; přístup má jen on, viz DACL v `npipe.rs`).
pub fn control_pipe_name() -> String {
    let user: String = std::env::var("USERNAME")
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    format!(r"\\.\pipe\wsl-container-desktop-control-{user}")
}

/// Spustil aplikaci docker shim?
pub fn launched_from_shim() -> bool {
    std::env::args().any(|a| a == SHIM_ARG)
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Běží aplikace (v této relaci)?
#[cfg(windows)]
pub fn app_running() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    let n = wide(MUTEX);
    let h = unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, 0, n.as_ptr()) };
    if h.is_null() {
        return false;
    }
    unsafe { CloseHandle(h) };
    true
}

/// Požádá běžící aplikaci o start enginu; false = aplikace (ještě) na událost nečeká.
#[cfg(windows)]
pub fn signal_start() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};
    let n = wide(EVENT);
    let h = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, n.as_ptr()) };
    if h.is_null() {
        return false;
    }
    let ok = unsafe { SetEvent(h) } != 0;
    unsafe { CloseHandle(h) };
    ok
}

/// Vlákno, které při každém signálu ze shimu zavolá `f` (handle se nezavírá – patří
/// procesu až do konce, stejně jako mutex).
#[cfg(windows)]
pub fn listen(f: impl Fn() + Send + 'static) {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
    let n = wide(EVENT);
    // automatický reset: jeden signál = jeden start
    let h = unsafe { CreateEventW(std::ptr::null(), 0, 0, n.as_ptr()) };
    if h.is_null() {
        return;
    }
    let h = h as usize;
    std::thread::spawn(move || loop {
        if unsafe { WaitForSingleObject(h as _, INFINITE) } != WAIT_OBJECT_0 {
            return;
        }
        f();
    });
}

#[cfg(not(windows))]
pub fn app_running() -> bool {
    false
}
#[cfg(not(windows))]
pub fn signal_start() -> bool {
    false
}
#[cfg(not(windows))]
pub fn listen(_f: impl Fn() + Send + 'static) {}

fn app_path_file() -> Option<PathBuf> {
    crate::config::data_dir().map(|d| d.join(APP_PATH_FILE))
}

/// Zapíše cestu k EXE aplikace (jen když se změnila).
pub fn record_app_path() {
    let (Some(f), Ok(exe)) = (app_path_file(), std::env::current_exe()) else { return };
    let s = exe.display().to_string();
    if std::fs::read_to_string(&f).ok().as_deref() == Some(s.as_str()) {
        return;
    }
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, s);
}

/// EXE aplikace: zapsaná cesta, jinak `wsl-container-desktop.exe` vedle shimu.
pub fn app_exe() -> Option<PathBuf> {
    let recorded = app_path_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| is_app_exe(p));
    recorded.or_else(|| {
        let p = std::env::current_exe().ok()?.parent()?.join("wsl-container-desktop.exe");
        p.is_file().then_some(p)
    })
}

fn is_app_exe(p: &std::path::Path) -> bool {
    p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}
