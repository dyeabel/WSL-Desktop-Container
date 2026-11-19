//! Jednoduchý čárový graf kreslený painterem (detail kontejneru, přehled paměti).

use super::*;

/// Jedna křivka: body (čas v s, hodnota).
pub struct Series<'a> {
    /// název do tooltipu (prázdný = jen hodnota)
    pub name: &'a str,
    pub color: Color32,
    pub pts: &'a [(f64, f64)],
}

/// Svislá značka v čase `t` s popiskem (zobrazí se po najetí myší).
pub struct Marker {
    pub t: f64,
    pub text: String,
}

/// Čárový graf za posledních `window` sekund před `now` (čas ve stejných jednotkách jako body).
pub fn line_chart(
    ui: &mut egui::Ui,
    series: &[Series<'_>],
    now: f64,
    window: f64,
    ymax: f64,
    fmt: impl Fn(f64) -> String,
    markers: &[Marker],
) {
    line_chart_gaps(ui, series, now, window, ymax, fmt, markers, f64::INFINITY);
}

/// Úseky křivky: body dál od sebe než `gap` se nespojí (aplikace neběžela, chybí vzorky).
pub fn segments(pts: &[(f64, f64)], gap: f64) -> Vec<&[(f64, f64)]> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 1..pts.len() {
        if pts[i].0 - pts[i - 1].0 > gap {
            out.push(&pts[start..i]);
            start = i;
        }
    }
    if start < pts.len() {
        out.push(&pts[start..]);
    }
    out
}

/// Popisek stáří: s, min, h nebo dny podle velikosti.
pub fn fmt_age(secs: f64) -> String {
    let s = secs.max(0.0).round();
    if s >= 2.0 * 86_400.0 {
        format!("{:.0} d", (s / 86_400.0).round())
    } else if s >= 2.0 * 3600.0 {
        format!("{:.0} h", (s / 3600.0).round())
    } else if s >= 120.0 {
        format!("{:.0} min", (s / 60.0).round())
    } else {
        format!("{s:.0} s")
    }
}

/// Jako `line_chart`, ale body dál od sebe než `gap` sekund se nespojí a mezera se
/// jemně podbarví (např. doba, kdy aplikace neběžela).
#[allow(clippy::too_many_arguments)]
pub fn line_chart_gaps(
    ui: &mut egui::Ui,
    series: &[Series<'_>],
    now: f64,
    window: f64,
    ymax: f64,
    fmt: impl Fn(f64) -> String,
    markers: &[Marker],
    gap: f64,
) {
    let w = ui.available_width().max(200.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 150.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 6.0, theme::bg());
    p.rect_stroke(rect, 6.0, egui::Stroke::new(1.0_f32, theme::card_stroke()), egui::StrokeKind::Inside);
    let plot = egui::Rect::from_min_max(rect.min + egui::vec2(8.0, 18.0), rect.max - egui::vec2(8.0, 18.0));
    let x0 = now - window;
    let ymax = ymax.max(f64::MIN_POSITIVE);
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
    p.text(egui::pos2(plot.left(), plot.top() - 2.0), egui::Align2::LEFT_BOTTOM, fmt(ymax), small.clone(), theme::muted());
    p.text(egui::pos2(plot.left(), plot.bottom() + 2.0), egui::Align2::LEFT_TOP, format!("−{}", fmt_age(window)), small.clone(), theme::muted());
    p.text(egui::pos2(plot.right(), plot.bottom() + 2.0), egui::Align2::RIGHT_TOP, t!("teď", "now"), small.clone(), theme::muted());

    // značky (pod křivkami)
    let mut hover_marker: Option<&Marker> = None;
    for m in markers.iter().filter(|m| m.t >= x0 && m.t <= now) {
        let x = to_screen(m.t, 0.0).x;
        let near = resp.hover_pos().is_some_and(|h| (h.x - x).abs() <= 5.0);
        let c = if near { theme::accent() } else { theme::muted() };
        // přerušovaná čára
        let mut y = plot.top();
        while y < plot.bottom() {
            let y2 = (y + 4.0).min(plot.bottom());
            p.line_segment([egui::pos2(x, y), egui::pos2(x, y2)], egui::Stroke::new(1.2_f32, c));
            y += 7.0;
        }
        p.text(egui::pos2(x, plot.top() - 2.0), egui::Align2::CENTER_BOTTOM, "\u{EC54}", small.clone(), c);
        if near {
            hover_marker = Some(m);
        }
    }

    if series.iter().all(|s| s.pts.is_empty()) {
        p.text(plot.center(), egui::Align2::CENTER_CENTER, t!("Zatím žádná data…", "No data yet…"), small, theme::muted());
        return;
    }
    // mezery (aplikace neběžela) podle křivky s nejvíce body
    if let Some(s) = series.iter().max_by_key(|s| s.pts.len()) {
        let k = s.pts.partition_point(|(t, _)| *t < x0);
        for w in s.pts[k..].windows(2).filter(|w| w[1].0 - w[0].0 > gap) {
            let (a, b) = (to_screen(w[0].0, 0.0).x, to_screen(w[1].0, 0.0).x);
            if b - a >= 3.0 {
                let r = egui::Rect::from_x_y_ranges(a + 1.0..=b - 1.0, plot.y_range());
                p.rect_filled(r, 0.0, theme::card_stroke().gamma_multiply(0.35));
            }
        }
    }
    for s in series {
        let k = s.pts.partition_point(|(t, _)| *t < x0);
        for seg in segments(&s.pts[k..], gap) {
            let line: Vec<egui::Pos2> = seg.iter().map(|(t, v)| to_screen(*t, *v)).collect();
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
    }

    // hodnoty pod kurzorem
    let Some(pos) = resp.hover_pos() else { return };
    if let Some(m) = hover_marker {
        let text = m.text.clone();
        resp.on_hover_ui_at_pointer(|ui| {
            ui.label(text);
        });
        return;
    }
    let t = x0 + ((pos.x - plot.left()) / plot.width()) as f64 * window;
    let mut lines = Vec::new();
    let mut at_t = None;
    for s in series {
        let near = s.pts.iter().filter(|(pt, _)| *pt >= x0).min_by(|a, b| (a.0 - t).abs().total_cmp(&(b.0 - t).abs()));
        // v mezeře bez dat nic neukazovat (nejbližší bod je daleko)
        if let Some((st, sv)) = near.filter(|(pt, _)| (pt - t).abs() <= gap / 2.0) {
            let at = to_screen(*st, *sv);
            at_t.get_or_insert(at.x);
            p.circle_filled(at, 3.5, s.color);
            lines.push(if s.name.is_empty() { fmt(*sv) } else { format!("{}: {}", s.name, fmt(*sv)) });
            if lines.len() == 1 {
                // čas podle první křivky
                let age = fmt_age(now - st);
                let when = tf!("před {age}", "{age} ago");
                lines.insert(0, when);
            }
        }
    }
    if let Some(x) = at_t {
        p.line_segment([egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())], egui::Stroke::new(1.0_f32, theme::muted()));
    }
    if lines.is_empty() {
        if gap.is_finite() && series.iter().any(|s| s.pts.iter().any(|(pt, _)| *pt >= x0 && *pt <= t)) {
            resp.on_hover_text_at_pointer(t!("Bez dat – aplikace neběžela nebo se neměřilo", "No data – the app was not running or nothing was measured"));
        }
        return;
    }
    // jedna křivka bez názvu: „hodnota (před X s)“ jako dřív
    let text = if series.len() == 1 && series[0].name.is_empty() && lines.len() == 2 {
        format!("{} ({})", lines[1], lines[0])
    } else {
        lines.join("\n")
    };
    resp.on_hover_ui_at_pointer(|ui| {
        ui.label(text);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_split_on_gaps() {
        let pts = [(0.0, 1.0), (60.0, 1.0), (120.0, 1.0), (1000.0, 1.0), (1060.0, 1.0), (5000.0, 1.0)];
        let s = segments(&pts, 180.0);
        assert_eq!(s.iter().map(|x| x.len()).collect::<Vec<_>>(), vec![3, 2, 1]);
        assert_eq!(segments(&pts, f64::INFINITY).len(), 1);
        assert!(segments(&[], 10.0).is_empty());
        assert_eq!((fmt_age(30.0), fmt_age(900.0), fmt_age(86_400.0), fmt_age(30.0 * 86_400.0)), ("30 s".into(), "15 min".into(), "24 h".into(), "30 d".into()));
    }
}
