#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod i18n;
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
mod fs;
mod icon_art;
mod notify;
mod npipe;
mod inspect;
mod logbuf;
#[allow(dead_code)] // používá docker shim, tady jen kvůli testům
mod pathmap;
mod shim_install;
mod tray;
mod theme;
mod ui;
mod worker;
mod wsl;
mod wslconfig;
#[cfg(test)]
mod live_tests;

/// Vytvoří pojmenovaný mutex pro aktuální relaci; false = aplikace už běží.
/// Handle se záměrně nezavírá – mutex patří procesu až do jeho konce.
#[cfg(windows)]
fn single_instance() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "Local\\WLS-Desktop-single-instance".encode_utf16().chain(std::iter::once(0)).collect();
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
        tray::activate_existing("WLS Desktop");
        return Ok(());
    }
    std::thread::spawn(|| {
        notify::register();
        autostart::fix_path();
    });
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("WLS Desktop")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 560.0])
            .with_icon(theme::app_icon()),
        ..Default::default()
    };
    eframe::run_native(
        "WLS Desktop",
        options,
        Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
    )
}
