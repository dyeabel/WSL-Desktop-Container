use super::*;
use crate::app::{App, LogView};

pub fn window(app: &mut App, ctx: &egui::Context) {
    let mut term = None;
    let Some(lv) = app.logs.as_mut() else { return };
    let mut open = true;
    egui::Window::new(tf!("\u{EB7A} Logy: {}", "\u{EB7A} Logs: {}", lv.name))
        .open(&mut open)
        .default_size([900.0, 520.0])
        .show(ctx, |ui| {
            term = view(ui, lv);
        });
    if let Some(t) = term {
        app.send(crate::worker::Cmd::Terminal(t));
    }
    if !open {
        app.logs = None;
    }
}

/// Obsah prohlížeče logů (lišta + text); sdílí okno logů a detail kontejneru.
/// Vrací požadavek na otevření logů v terminálu.
pub fn view(ui: &mut egui::Ui, lv: &mut LogView) -> Option<crate::worker::Term> {
    let mut term = None;
    toolbar(ui, |ui| {
        ui.add(field(&mut lv.filter).hint_text(t!("\u{E30C} Filtr řádků…", "\u{E30C} Filter lines…")).desired_width(220.0));
        ui.checkbox(&mut lv.wrap, t!("Zalamovat", "Wrap"));
        ui.checkbox(&mut lv.follow, t!("Sledovat konec", "Follow"));
        if ui.button(t!("Vymazat", "Clear")).clicked() {
            if let Ok(mut g) = lv.buf.lock() {
                g.clear();
            }
        }
        if ui.button(t!("\u{E1CA} Kopírovat", "\u{E1CA} Copy")).clicked() {
            if let Ok(g) = lv.buf.lock() {
                ui.ctx().copy_text(g.clone());
            }
        }
        if ui.button(t!("\u{EAE8} V terminálu", "\u{EAE8} In terminal")).clicked() {
            term = Some(crate::worker::Term::Logs { id: lv.id.clone(), name: lv.name.clone() });
        }
    });
    ui.separator();
    let text = lv.buf.lock().map(|g| g.clone()).unwrap_or_default();
    let f = lv.filter.to_lowercase();
    let shown: String = if f.is_empty() {
        text
    } else {
        text.lines().filter(|l| l.to_lowercase().contains(&f)).collect::<Vec<_>>().join("\n")
    };
    egui::ScrollArea::both().auto_shrink([false, false]).stick_to_bottom(lv.follow).show(ui, |ui| {
        let mode = if lv.wrap { egui::TextWrapMode::Wrap } else { egui::TextWrapMode::Extend };
        ui.add(egui::Label::new(RichText::new(shown).monospace()).wrap_mode(mode));
    });
    term
}
