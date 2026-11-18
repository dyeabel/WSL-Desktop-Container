use super::*;
use crate::app::App;

pub fn window(app: &mut App, ctx: &egui::Context) {
    let mut term = None;
    let Some(lv) = app.logs.as_mut() else { return };
    let mut open = true;
    egui::Window::new(format!("\u{EB7A} Logy: {}", lv.name))
        .open(&mut open)
        .default_size([900.0, 520.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add(field(&mut lv.filter).hint_text("\u{E30C} Filtr řádků…").desired_width(220.0));
                ui.checkbox(&mut lv.wrap, "Zalamovat");
                if ui.button("Vymazat").clicked() {
                    if let Ok(mut g) = lv.buf.lock() {
                        g.clear();
                    }
                }
                if ui.button("\u{E1CA} Kopírovat").clicked() {
                    if let Ok(g) = lv.buf.lock() {
                        ui.ctx().copy_text(g.clone());
                    }
                }
                if ui.button("\u{EAE8} V terminálu").clicked() {
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
            egui::ScrollArea::both().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                let mode = if lv.wrap { egui::TextWrapMode::Wrap } else { egui::TextWrapMode::Extend };
                ui.add(egui::Label::new(RichText::new(shown).monospace()).wrap_mode(mode));
            });
        });
    if let Some(t) = term {
        app.send(crate::worker::Cmd::Terminal(t));
    }
    if !open {
        app.logs = None;
    }
}
