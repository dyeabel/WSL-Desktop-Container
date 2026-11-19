//! Jednoduchý čárový graf (painter) za posledních N sekund – sdílí detail kontejneru
//! (CPU, paměť) a síťový přehled (provoz VM).

use crate::theme;
use eframe::egui::{self, Color32};

/// Jedna křivka grafu: body (čas v s, hodnota), barva, popisek (pro tooltip; prázdný = bez).
pub struct Series<'a> {
    pub pts: &'a [(f64, f64)],
    pub color: Color32,
    pub label: &'a str,
}

/// Čárový graf za posledních `window` sekund (osa x končí v `now`), osa y 0..`ymax`.
pub fn line_chart(ui: &mut egui::Ui, series: &[Series<'_>], now: f64, window: f64, ymax: f64, fmt: impl Fn(f64) -> String) {
    let w = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 150.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 6.0, theme::bg());
    p.rect_stroke(rect, 6.0, egui::Stroke::new(1.0_f32, theme::card_stroke()), egui::StrokeKind::Inside);
    let plot = egui::Rect::from_min_max(rect.min + egui::vec2(8.0, 18.0), rect.max - egui::vec2(8.0, 18.0));
    let x0 = now - window;
    let ymax = if ymax > 0.0 { ymax } else { 1.0 };
    let to_screen = |t: f64, v: f64| {
        let x = plot.left() + ((t - x0) / window).clamp(0.0, 1.0) as f32 * plot.width();
        let y = plot.bottom() - (v / ymax).clamp(0.0, 1.0) as f32 * plot.height();
        egui::pos2(x, y)
    };
    let small = egui::FontId::proportional(11.0);
    for f in [0.0, 0.5, 1.0] {
        let y = plot.bottom() - f * plot.height();
        p.line_segment([egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)], egui::Stroke::new(1.0_f32, theme::card_stroke()));
    }
    let mins = (window / 60.0).round();
    p.text(egui::pos2(plot.left(), plot.top() - 2.0), egui::Align2::LEFT_BOTTOM, fmt(ymax), small.clone(), theme::muted());
    p.text(egui::pos2(plot.left(), plot.bottom() + 2.0), egui::Align2::LEFT_TOP, format!("\u{2212}{mins} min"), small.clone(), theme::muted());
    p.text(egui::pos2(plot.right(), plot.bottom() + 2.0), egui::Align2::RIGHT_TOP, t!("teď", "now"), small.clone(), theme::muted());
    if series.iter().all(|s| s.pts.is_empty()) {
        p.text(plot.center(), egui::Align2::CENTER_CENTER, t!("Zatím žádná data…", "No data yet…"), small, theme::muted());
        return;
    }
    for s in series {
        let line: Vec<egui::Pos2> = s.pts.iter().filter(|(t, _)| *t >= x0).map(|(t, v)| to_screen(*t, *v)).collect();
        match line.len() {
            0 => {}
            1 => {
                p.circle_filled(line[0], 2.5, s.color);
            }
            _ => {
                p.add(egui::Shape::line(line, egui::Stroke::new(1.8_f32, s.color)));
            }
        }
    }
    // hodnoty pod kurzorem
    if let Some(pos) = resp.hover_pos() {
        let t = x0 + ((pos.x - plot.left()) / plot.width()) as f64 * window;
        let mut lines = Vec::new();
        let mut ago = None;
        for s in series {
            if let Some((st, sv)) = s.pts.iter().min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs())) {
                let at = to_screen(*st, *sv);
                p.line_segment([egui::pos2(at.x, plot.top()), egui::pos2(at.x, plot.bottom())], egui::Stroke::new(1.0_f32, theme::muted()));
                p.circle_filled(at, 3.5, s.color);
                ago.get_or_insert((now - st).max(0.0).round());
                lines.push(if s.label.is_empty() { fmt(*sv) } else { format!("{}: {}", s.label, fmt(*sv)) });
            }
        }
        if let Some(ago) = ago {
            let text = if lines.len() == 1 {
                tf!("{} (před {ago:.0} s)", "{} ({ago:.0} s ago)", lines[0])
            } else {
                format!("{}\n{}", lines.join("\n"), tf!("před {ago:.0} s", "{ago:.0} s ago"))
            };
            resp.on_hover_ui_at_pointer(|ui| {
                ui.label(text);
            });
        }
    }
}
