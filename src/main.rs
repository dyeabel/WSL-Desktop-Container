#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod i18n;
mod app;
mod apitls;
mod autostart;
mod config;
mod docker;
mod engine;
mod fs;
mod icon_art;
mod notify;
mod inspect;
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

fn main() -> eframe::Result<()> {
    // jen jedna instance: když už běží (třeba schovaná v tray), jen ji zobrazíme
    if tray::activate_existing("WLS Desktop") {
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
