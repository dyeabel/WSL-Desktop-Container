#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod i18n;
mod activity;
mod app;
mod apitls;
mod autostart;
mod compose_templates;
mod config;
mod container_fs;
mod docker;
mod dpapi;
mod engine;
mod error;
mod eventlog;
mod fs;
mod history;
mod host;
mod http;
mod icon_art;
mod notify;
mod npipe;
mod inspect;
mod k8s;
mod limits;
mod lite;
mod logbuf;
mod memory;
mod netinfo;
#[allow(dead_code)] // používá docker shim, tady jen kvůli testům
mod pathmap;
mod portable_acl;
mod portrelay;
mod shim_install;
mod tray;
mod theme;
mod ui;
mod wake;
mod worker;
mod wsl;
mod wslconfig;
#[cfg(test)]
mod bench;
#[cfg(test)]
mod live_docker;
#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod glyph_check;

/// Vytvoří pojmenovaný mutex pro aktuální relaci; false = aplikace už běží.
/// Handle se záměrně nezavírá – mutex patří procesu až do jeho konce.
#[cfg(windows)]
fn single_instance() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = wake::MUTEX.encode_utf16().chain(std::iter::once(0)).collect();
    let h = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    !(h.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS)
}

#[cfg(not(windows))]
fn single_instance() -> bool {
    true
}

fn main() -> eframe::Result<()> {
    // jen jedna instance: pojmenovaný mutex (okno se dá podvrhnout, F-14); když už běží
    // (třeba schovaná v tray), jen zobrazíme její okno
    if !single_instance() {
        // spuštěno ze shimu: okno neukazovat, jen běžící instanci požádat o start enginu
        if wake::launched_from_shim() {
            wake::signal_start();
        } else {
            tray::activate_existing("WSL Container Desktop");
        }
        return Ok(());
    }
    std::thread::spawn(|| {
        notify::register();
        autostart::fix_path();
        wake::record_app_path();
    });
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_title("WSL Container Desktop")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([900.0, 560.0])
        .with_icon(theme::app_icon());
    // Linux: app id musí odpovídat StartupWMClass v .desktop souboru (seskupení oken, ikona)
    #[cfg(not(windows))]
    let viewport = viewport.with_app_id("wsl-container-desktop");
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let r = eframe::run_native(
        "WSL Container Desktop",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    );
    tray::cleanup_on_exit(config::Config::load().shutdown_wsl_on_quit);
    r
}
