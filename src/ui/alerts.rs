//! Upozornění na prahy paměti a disku: pruh na Přehledu, sekce v Nastavení a přechod
//! na stránku po kliknutí na oznámení.

use super::*;
use crate::app::{App, Page};
use crate::config::AlertsConfig;
use crate::worker::alerts::{Kind, Target};

/// Kliknutí na oznámení: Přehled se sekcí Paměť WSL, nebo stránka WSL engine (disk).
pub fn navigate(app: &mut App, t: Target) {
    match t {
        Target::Memory => {
            app.page = Page::Dashboard;
            app.mem.scroll_to = true;
        }
        Target::Engine => app.page = Page::Engine,
    }
}

/// Pruh s aktivními upozorněními (Přehled).
pub fn banner(app: &mut App, ui: &mut egui::Ui) {
    let cfg = app.cfg.read().map(|c| c.alerts.clone()).unwrap_or_default();
    let list = app.shared.alerts.lock().map(|a| a.active(&cfg)).unwrap_or_default();
    for a in list {
        let mut nav = None;
        let mut cleanup = None;
        let mut free = false;
        egui::Frame::new()
            .fill(theme::yellow().gamma_multiply(0.12))
            .stroke(egui::Stroke::new(1.0_f32, theme::yellow().gamma_multiply(0.5)))
            .corner_radius(8)
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(format!("\u{E4E0} {}", a.title)).strong().color(theme::yellow()));
                ui.label(&a.body);
                ui.add_space(4.0);
                // každá položka je jeden widget – řádek se zalomí
                // odkazy jen do sekcí, které hostitel ukazuje (Linux: bez paměti VM a disku distribuce)
                let caps = app.caps();
                ui.horizontal_wrapped(|ui| match a.kind {
                    Kind::Memory => {
                        if caps.vm_memory {
                            ui.add_enabled_ui(!app.mem.freeing, |ui| {
                                if accent_button(ui, t!("\u{EC54} Uvolnit paměť", "\u{EC54} Free memory")).clicked() {
                                    free = true;
                                }
                            });
                            if ui.button(t!("Zobrazit paměť", "Show memory")).clicked() {
                                nav = Some(Target::Memory);
                            }
                        }
                    }
                    Kind::Pressure => {
                        if caps.vm_memory && ui.button(t!("Zobrazit paměť", "Show memory")).clicked() {
                            nav = Some(Target::Memory);
                        }
                    }
                    _ => {
                        ui.add_enabled_ui(app.snap.connected, |ui| cleanup = super::dashboard::cleanup_menu(ui, caps.vm_memory));
                        if caps.vm_disk && ui.button(t!("\u{EB02} Disk distribuce…", "\u{EB02} Distribution disk…")).clicked() {
                            nav = Some(Target::Engine);
                        }
                    }
                });
            });
        ui.add_space(6.0);
        if free {
            super::memory::free(app);
        }
        super::dashboard::cleanup_apply(app, cleanup);
        if let Some(t) = nav {
            navigate(app, t);
        }
    }
}

/// Uložit na disk hned? (změna zápisem, nebo konec tažení – ne v každém snímku tažení)
fn save_now(r: &egui::Response) -> bool {
    (r.changed() && !r.dragged()) || r.drag_stopped()
}

/// Hodnota prahu; vrací, zda konfiguraci uložit na disk.
fn num(ui: &mut egui::Ui, v: &mut f64, range: std::ops::RangeInclusive<f64>, suffix: &str, speed: f64) -> bool {
    save_now(&ui.add(egui::DragValue::new(v).range(range).suffix(suffix).speed(speed).max_decimals(1)))
}

fn num_u(ui: &mut egui::Ui, v: &mut u32, range: std::ops::RangeInclusive<u32>, suffix: &str) -> bool {
    save_now(&ui.add(egui::DragValue::new(v).range(range).suffix(suffix).speed(0.2)))
}

/// Sekce „Upozornění“ v Nastavení – mění se okamžitě (bez tlačítka Uložit).
pub fn settings_section(app: &mut App, ui: &mut egui::Ui) {
    section(ui, t!("Upozornění na paměť a disk", "Memory and disk alerts"));
    let before = app.cfg.read().map(|c| c.alerts.clone()).unwrap_or_default();
    let mut a: AlertsConfig = before.clone();
    let mut save = false;
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Grid::new("alerts-grid").num_columns(2).spacing([16.0, 8.0]).show(ui, |ui| {
            save |= ui.checkbox(&mut a.memory, t!("VmmemWSL nad", "VmmemWSL above")).changed();
            ui.horizontal(|ui| {
                save |= num(ui, &mut a.memory_pct, 10.0..=100.0, " %", 0.5);
                ui.label(t!("limitu paměti WSL déle než", "of the WSL memory limit for more than"));
                save |= num_u(ui, &mut a.memory_minutes, 0..=60, " min");
            });
            ui.end_row();
            save |= ui.checkbox(&mut a.pressure, t!("Dostupná paměť v Linuxu pod", "Available memory in Linux below")).changed();
            ui.horizontal(|ui| {
                save |= num(ui, &mut a.pressure_pct, 1.0..=50.0, " %", 0.5);
                ui.label(muted(t!("MemAvailable z MemTotal", "MemAvailable of MemTotal")));
            });
            ui.end_row();
            save |= ui.checkbox(&mut a.distro_disk, t!("Volné místo v distribuci pod", "Free space in the distribution below")).changed();
            ui.horizontal(|ui| {
                save |= num(ui, &mut a.distro_free_pct, 1.0..=50.0, " %", 0.5);
                ui.label(muted(t!("df / uvnitř disku VHDX", "df / inside the VHDX disk")));
            });
            ui.end_row();
            save |= ui.checkbox(&mut a.win_drive, t!("Volné místo na disku Windows s VHDX pod", "Free space on the Windows drive with the VHDX below")).changed();
            ui.horizontal(|ui| {
                save |= num(ui, &mut a.drive_free_pct, 1.0..=50.0, " %", 0.5);
                ui.label(t!("nebo pod", "or below"));
                save |= num(ui, &mut a.drive_free_gb, 0.5..=1000.0, " GB", 0.5);
            });
            ui.end_row();
            save |= ui.checkbox(&mut a.engine_free, t!("Volné místo pro data enginu pod", "Free space for engine data below")).changed();
            ui.horizontal(|ui| {
                save |= num(ui, &mut a.engine_free_pct, 1.0..=50.0, " %", 0.5);
                ui.label(muted("/var/lib/containers, /var/lib/docker"));
            });
            ui.end_row();
            save |= ui.checkbox(&mut a.engine_size, t!("Úložiště enginu větší než", "Engine storage larger than")).changed();
            ui.horizontal(|ui| {
                save |= num(ui, &mut a.engine_size_gb, 1.0..=10_000.0, " GB", 1.0);
                ui.label(muted(t!("image, kontejnery, volume, build cache", "images, containers, volumes, build cache")));
            });
            ui.end_row();
            save |= ui.checkbox(&mut a.toast, t!("Oznámení ve Windows, nejvýš jednou za", "Windows notification, at most once per")).changed();
            ui.horizontal(|ui| {
                save |= num_u(ui, &mut a.repeat_minutes, 1..=1440, " min");
                ui.label(muted(t!("pro každé upozornění", "for each alert")));
            });
            ui.end_row();
        });
        ui.add_space(4.0);
        ui.label(
            muted(t!(
                "Aktivní upozornění se ukazuje i jako pruh na Přehledu. Znovu se ohlásí, až hodnota klesne aspoň 5 % pod práh a pak ho znovu překročí. Disk se kontroluje nejvýš jednou za 5 minut.",
                "An active alert is also shown as a banner on the Overview. It fires again only after the value drops at least 5 % below the threshold and then crosses it again. Disk is checked at most every 5 minutes."
            ))
            .small(),
        );
    });
    if a != before {
        app.cfg_draft.alerts = a.clone();
        let res = match app.cfg.write() {
            Ok(mut g) => {
                g.alerts = a;
                if save { g.save() } else { Ok(()) }
            }
            Err(e) => Err(e.to_string()),
        };
        if let Err(e) = res {
            app.toast(false, tf!("Nastavení nelze uložit: {e}", "Cannot save settings: {e}"));
        }
    } else if save {
        // konec tažení: hodnota už je v paměti, uložit na disk
        let res = app.cfg.read().map(|g| g.save()).unwrap_or(Ok(()));
        if let Err(e) = res {
            app.toast(false, tf!("Nastavení nelze uložit: {e}", "Cannot save settings: {e}"));
        }
    }
}
