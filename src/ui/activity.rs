//! Okno „Na pozadí“: běžící akce a příkazy ve WSL a nedávno dokončené.

use super::*;
use crate::activity::{self, Kind};
use crate::app::App;
use std::time::Duration;

/// Činnosti kratší než tohle se v indikátoru nepočítají (běžné obnovování stavu).
pub const VISIBLE_AFTER: Duration = Duration::from_millis(400);

fn dur(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{} min {} s", s / 60, s % 60)
    } else if d.as_millis() >= 10_000 {
        format!("{s} s")
    } else {
        format!("{:.1} s", d.as_secs_f32())
    }
}

fn kind_label(k: Kind) -> &'static str {
    match k {
        Kind::Action => t!("akce", "action"),
        Kind::Wsl => "WSL",
    }
}

/// Indikátor v levém panelu: počet běžících činností; klik otevře okno.
pub fn indicator(app: &mut App, ui: &mut egui::Ui) {
    let n = activity::running_longer_than(VISIBLE_AFTER);
    let text = if n > 0 { tf!("{n} na pozadí", "{n} in background") } else { t!("Na pozadí", "Background").to_string() };
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let r = ui
            .add(egui::Button::new(RichText::new(text).small().color(if n > 0 { theme::accent() } else { theme::muted() })).frame(false))
            .on_hover_text(t!("Co aplikace právě dělá: akce a příkazy spuštěné ve WSL", "What the app is doing: actions and commands run in WSL"));
        if n > 0 {
            ui.spinner();
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        }
        if r.clicked() {
            app.activity_open = !app.activity_open;
        }
    });
}

pub fn window(app: &mut App, ctx: &egui::Context) {
    if !app.activity_open {
        return;
    }
    let mut open = true;
    egui::Window::new(t!("Na pozadí", "Background activity"))
        .open(&mut open)
        .default_size([720.0, 460.0])
        .show(ctx, |ui| {
            let (running, done) = activity::snapshot();
            let running: Vec<_> = running.into_iter().filter(|r| r.started.elapsed() >= VISIBLE_AFTER || r.kind == Kind::Action).collect();
            ui.horizontal(|ui| {
                ui.label(RichText::new(tf!("Běží ({})", "Running ({})", running.len())).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(t!("Vymazat historii", "Clear history")).clicked() {
                        activity::clear_done();
                    }
                });
            });
            egui::ScrollArea::vertical().id_salt("activity").auto_shrink([false, false]).show(ui, |ui| {
                if running.is_empty() {
                    ui.label(muted(t!("Nic neběží.", "Nothing is running.")).small());
                }
                for r in &running {
                    row(ui, None, r.kind, &r.title, &r.detail, &dur(r.started.elapsed()), "");
                }
                ui.add_space(10.0);
                ui.label(RichText::new(t!("Nedávno dokončené", "Recently finished")).strong());
                ui.label(
                    muted(t!(
                        "Akce aplikace a příkazy ve WSL delší než 1 s nebo s chybou (běžné obnovování stavu se nezapisuje).",
                        "App actions and WSL commands longer than 1 s or failed (regular status refreshes are not recorded)."
                    ))
                    .small(),
                );
                if done.is_empty() {
                    ui.label(muted(t!("Zatím nic.", "Nothing yet.")).small());
                }
                for d in &done {
                    row(ui, Some(d.ok), d.kind, &d.title, &d.detail, &dur(d.took), &tf!("před {}", "{} ago", dur_ago(d.ended.elapsed())));
                }
            });
            ui.ctx().request_repaint_after(Duration::from_millis(500));
        });
    app.activity_open = open;
}

fn dur_ago(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else {
        format!("{} h", s / 3600)
    }
}

/// Jeden řádek: stav (spinner / ✓ / ✗), název, vpravo druh, doba a stáří; pod ním příkaz.
fn row(ui: &mut egui::Ui, ok: Option<bool>, kind: Kind, title: &str, detail: &str, took: &str, ago: &str) {
    ui.horizontal(|ui| {
        match ok {
            None => {
                ui.spinner();
            }
            Some(true) => {
                ui.label(RichText::new("\u{E182}").color(theme::green()));
            }
            Some(false) => {
                ui.label(RichText::new("\u{E4E0}").color(theme::red()));
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !ago.is_empty() {
                ui.add_sized([70.0, 18.0], egui::Label::new(muted(ago).small()));
            }
            ui.add_sized([70.0, 18.0], egui::Label::new(RichText::new(took).small()));
            ui.add_sized([44.0, 18.0], egui::Label::new(muted(kind_label(kind)).small()));
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(title).truncate());
            });
        });
    });
    if !detail.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            ui.add(egui::Label::new(RichText::new(detail).small().monospace().color(theme::muted())).truncate()).on_hover_text(detail);
        });
    }
    ui.add_space(4.0);
}
