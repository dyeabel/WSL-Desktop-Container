pub mod build;
pub mod compose;
pub mod compose_new;
pub mod containers;
pub mod dashboard;
pub mod detail;
pub mod distros;
pub mod engine;
pub mod files;
pub mod images;
pub mod logs;
pub mod networks;
pub mod settings;
pub mod volumes;

use crate::theme;
use eframe::egui::{self, Color32, RichText};

pub fn header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.label(RichText::new(title).size(28.0).strong());
    if !subtitle.is_empty() {
        ui.label(RichText::new(subtitle).color(theme::MUTED));
    }
    ui.add_space(10.0);
}

pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(RichText::new(title).size(17.0).strong());
    ui.add_space(4.0);
}

pub fn accent_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(theme::ACCENT_TEXT)).fill(theme::ACCENT))
}

pub fn danger_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(Color32::WHITE)).fill(theme::RED.linear_multiply(0.85)))
}

/// Ikonové tlačítko bez rámečku s tooltipem (šířka 26 px).
pub fn icon(ui: &mut egui::Ui, glyph: &str, tip: &str) -> bool {
    // bez ui.scope(), aby se ikona v zalamovaném řádku normálně zalomila
    let old = ui.spacing().button_padding;
    ui.spacing_mut().button_padding = egui::vec2(4.0, 3.0);
    let r = ui.add(egui::Button::new(RichText::new(glyph).size(16.0)).frame(false).min_size(egui::vec2(26.0, 24.0)));
    ui.spacing_mut().button_padding = old;
    r.on_hover_text(tip).clicked()
}

/// Šířka sloupce pro `n` ikon v řádku (ikona 26 px + mezera 2 px).
pub fn icons_w(n: usize) -> W {
    W::Px(n as f32 * 26.0 + n.saturating_sub(1) as f32 * 2.0 + 8.0)
}

pub fn dot(ui: &mut egui::Ui, c: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 5.0, c);
}

pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(theme::CARD)
        .stroke(egui::Stroke::new(1.0_f32, theme::CARD_STROKE))
        .corner_radius(8)
        .inner_margin(egui::Margin::same(14))
        .show(ui, add)
        .inner
}

/// Šířka sloupce: pevná v px, nebo podíl ze zbývající šířky.
/// Varianty `*Opt` jsou méně důležité sloupce, které se v úzkém okně skryjí.
#[derive(Clone, Copy)]
pub enum W {
    Px(f32),
    Fr(f32),
    PxOpt(f32),
    FrOpt(f32),
}

impl W {
    fn optional(self) -> bool {
        matches!(self, W::PxOpt(_) | W::FrOpt(_))
    }
}

/// Tělo tabulky – přidává řádky.
pub struct TableBody<'a> {
    ui: &'a mut egui::Ui,
    /// šířky sloupců, 0 = skrytý sloupec
    widths: Vec<f32>,
    gap: f32,
    n: usize,
}

/// Jeden řádek tabulky – buňky se přidávají zleva, každá má pevnou šířku sloupce.
pub struct TableRow<'a> {
    ui: &'a mut egui::Ui,
    widths: &'a [f32],
    gap: f32,
    rect: egui::Rect,
    x: f32,
    idx: usize,
}

const CELL_PAD: f32 = 8.0;
/// minimální šířka každého podílového sloupce
const FR_MIN: f32 = 70.0;
const GAP: f32 = 12.0;

fn total_width(widths: &[f32], gap: f32) -> f32 {
    let vis: Vec<f32> = widths.iter().copied().filter(|w| *w > 0.0).collect();
    vis.iter().sum::<f32>() + gap * vis.len().saturating_sub(1) as f32 + 2.0 * CELL_PAD
}

impl TableBody<'_> {
    pub fn row(&mut self, height: f32, add: impl FnOnce(TableRow<'_>)) {
        let total = total_width(&self.widths, self.gap);
        let (rect, resp) = self.ui.allocate_exact_size(egui::vec2(total, height), egui::Sense::hover());
        if self.ui.is_rect_visible(rect) {
            let fill = if resp.hovered() {
                Some(egui::Color32::from_gray(0x3A))
            } else if self.n % 2 == 1 {
                Some(egui::Color32::from_gray(0x2F))
            } else {
                None
            };
            if let Some(f) = fill {
                self.ui.painter().rect_filled(rect, 5.0, f);
            }
        }
        self.n += 1;
        add(TableRow { ui: self.ui, widths: &self.widths, gap: self.gap, rect, x: rect.left() + CELL_PAD, idx: 0 });
    }
}

impl TableRow<'_> {
    pub fn col(&mut self, add: impl FnOnce(&mut egui::Ui)) {
        let Some(&w) = self.widths.get(self.idx) else { return };
        self.idx += 1;
        if w <= 0.0 {
            return; // skrytý sloupec
        }
        let cell = egui::Rect::from_min_size(egui::pos2(self.x, self.rect.top()), egui::vec2(w, self.rect.height()));
        let mut child = self.ui.new_child(
            egui::UiBuilder::new()
                .max_rect(cell)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        child.set_clip_rect(cell.expand2(egui::vec2(2.0, 0.0)).intersect(self.ui.clip_rect()));
        child.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        add(&mut child);
        self.x += w + self.gap;
    }
}

/// Tabulka v kartě přes celou šířku okna.
pub fn table(ui: &mut egui::Ui, id: &str, cols: &[(&str, W)], body: impl FnOnce(TableBody<'_>)) {
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        table_in(ui, id, cols, body);
    });
}

/// Spočítá šířky sloupců pro danou šířku. `hide_opt` skryje volitelné sloupce.
fn layout_widths(cols: &[(&str, W)], width: f32, hide_opt: bool) -> (Vec<f32>, f32) {
    let shown: Vec<bool> = cols.iter().map(|(_, w)| !(hide_opt && w.optional())).collect();
    let n_vis = shown.iter().filter(|s| **s).count();
    let gaps = GAP * n_vis.saturating_sub(1) as f32 + 2.0 * CELL_PAD;
    let mut px = 0.0;
    let mut fr = 0.0;
    let mut n_fr = 0.0;
    for ((_, w), s) in cols.iter().zip(&shown) {
        if !*s {
            continue;
        }
        match w {
            W::Px(p) | W::PxOpt(p) => px += p,
            W::Fr(f) | W::FrOpt(f) => {
                fr += f;
                n_fr += 1.0;
            }
        }
    }
    let min_total = px + n_fr * FR_MIN + gaps;
    // podílové sloupce: nejdřív každému minimum, zbytek rozdělit podle vah
    let extra = (width.max(min_total) - min_total).max(0.0);
    let widths = cols
        .iter()
        .zip(&shown)
        .map(|((_, w), s)| match (s, w) {
            (false, _) => 0.0,
            (true, W::Px(p) | W::PxOpt(p)) => *p,
            (true, W::Fr(f) | W::FrOpt(f)) => FR_MIN + extra * f / fr,
        })
        .collect();
    (widths, min_total)
}

/// Tabulka, jejíž sloupce se počítají z dostupné šířky – drží šířku okna
/// a nemění se podle obsahu; dlouhý text se ořízne. V úzkém okně se nejdřív
/// skryjí volitelné sloupce, teprve pak se tabulka dá posouvat vodorovně.
pub fn table_in(ui: &mut egui::Ui, id: &str, cols: &[(&str, W)], body: impl FnOnce(TableBody<'_>)) {
    let avail = ui.available_width();
    let (full, full_min) = layout_widths(cols, avail, false);
    if avail >= full_min {
        build_table(ui, cols, full, body);
        return;
    }
    let (reduced, reduced_min) = layout_widths(cols, avail, true);
    if avail >= reduced_min {
        build_table(ui, cols, reduced, body);
    } else {
        egui::ScrollArea::horizontal().id_salt(format!("{id}-hscroll")).show(ui, |ui| {
            build_table(ui, cols, reduced, body);
        });
    }
}

fn build_table(ui: &mut egui::Ui, cols: &[(&str, W)], widths: Vec<f32>, body: impl FnOnce(TableBody<'_>)) {
    ui.spacing_mut().item_spacing.y = 0.0;
    let mut b = TableBody { ui, widths, gap: GAP, n: 1 };
    // hlavička
    b.row(30.0, |mut h| {
        for (t, _) in cols {
            h.col(|ui| {
                ui.label(RichText::new(*t).small().strong().color(theme::MUTED));
            });
        }
    });
    b.n = 0;
    body(b);
}

/// Barevný štítek („pilulka“).
pub fn badge(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .stroke(egui::Stroke::new(1.0_f32, color.gamma_multiply(0.45)))
        .corner_radius(9)
        .inner_margin(egui::Margin::symmetric(7, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().strong().color(color));
        })
        .response
}

/// Dvouřádková buňka: hlavní text + šedý popisek.
pub fn two_line(ui: &mut egui::Ui, main: RichText, sub: RichText) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.label(main);
        ui.label(sub);
    });
}

/// Modální dialog s okraji a nadpisem. Vrací (výsledek obsahu, zavřít – Esc / klik mimo).
pub fn modal<R>(ctx: &egui::Context, id: &str, title: &str, width: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> (R, bool) {
    let r = egui::Modal::new(egui::Id::new(id))
        .frame(
            egui::Frame::new()
                .fill(theme::DIALOG)
                .stroke(egui::Stroke::new(1.0_f32, theme::CARD_STROKE))
                .corner_radius(10)
                .inner_margin(egui::Margin::same(22)),
        )
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.label(RichText::new(title).size(18.0).strong());
            ui.add_space(10.0);
            add(ui)
        });
    let close = r.should_close();
    (r.inner, close)
}

/// Spodní lišta dialogu: tlačítka vpravo. Vrací (OK, Zrušit).
pub fn dialog_buttons(ui: &mut egui::Ui, ok: &str, danger: bool, enabled: bool) -> (bool, bool) {
    ui.add_space(16.0);
    let mut res = (false, false);
    // horizontal kolem right_to_left: řádek má jen výšku tlačítek (žádné prázdné místo pod nimi)
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                let b = if danger { danger_button(ui, ok) } else { accent_button(ui, ok) };
                res.0 = b.clicked();
            });
            res.1 = ui.button(t!("Zrušit", "Cancel")).clicked();
        });
    });
    res
}

/// Lišta nástrojů: při zúžení okna se zalamuje po celých tlačítkách
/// (text v tlačítku se nikdy neláme).
pub fn toolbar<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal_wrapped(|ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
        add(ui)
    })
    .inner
}

/// Lišta nástrojů s pravou částí napevno u pravého okraje (filtr, nabídka ⋯).
/// Levá část se při zúžení okna zalamuje, pravá zůstává na místě.
/// `content(ui, false)` kreslí levou část, `content(ui, true)` pravou
/// (jedna closure, aby obě části mohly měnit stejný stav aplikace).
pub fn toolbar_lr(ui: &mut egui::Ui, right_width: f32, mut content: impl FnMut(&mut egui::Ui, bool)) {
    ui.horizontal_top(|ui| {
        let total = ui.available_width();
        let lw = (total - right_width - 12.0).max(120.0);
        ui.allocate_ui_with_layout(
            egui::vec2(lw, 0.0),
            egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
            |ui| {
                ui.set_min_width(lw);
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 8.0);
                content(ui, false);
            },
        );
        // pravá část: výška přesně jednoho ovládacího prvku, prvky svisle na střed
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), theme::CONTROL_H),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                ui.spacing_mut().item_spacing.x = 6.0;
                content(ui, true);
            },
        );
    });
}

/// Pole filtru (pevná šířka) – patří do pravé části `toolbar_lr`.
pub fn filter_field(ui: &mut egui::Ui, text: &mut String) {
    ui.add(field(text).hint_text(t!("\u{E30C} Filtr…", "\u{E30C} Filter…")).desired_width(190.0));
}

/// Nabídka „⋯“ pro méně častné akce (úklid apod.).
pub fn more_menu(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.menu_button(RichText::new("\u{E1FE}").size(16.0), |ui| {
        ui.set_min_width(240.0);
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        add(ui);
    })
    .response
    .on_hover_text(t!("Další akce", "More actions"));
}

/// Menu „⋯“ v řádku tabulky: menší, bez rámečku, stejně jako ikonová tlačítka (26×24).
pub fn row_menu<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) {
    let old = ui.spacing().button_padding;
    ui.spacing_mut().button_padding = egui::vec2(4.0, 3.0);
    let button = egui::Button::new(RichText::new("\u{E1FE}").size(15.0)).frame(false).min_size(egui::vec2(26.0, 24.0));
    let r = egui::menu::menu_custom_button(ui, button, |ui| {
        ui.spacing_mut().button_padding = old;
        ui.set_min_width(220.0);
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        add(ui)
    });
    ui.spacing_mut().button_padding = old;
    r.response.on_hover_text(t!("Další akce", "More actions"));
}

/// Mezera mezi skupinami v liště.
pub fn gap(ui: &mut egui::Ui) {
    ui.add_space(10.0);
}

/// Jednořádkové vstupní pole s vnitřním okrajem.
pub fn field(s: &mut String) -> egui::TextEdit<'_> {
    // výška pole = výška tlačítka (theme::CONTROL_H)
    egui::TextEdit::singleline(s)
        .margin(egui::Margin::symmetric(8, 6))
        .min_size(egui::vec2(0.0, theme::CONTROL_H))
        .vertical_align(egui::Align::Center)
}

/// Víceřádkové vstupní pole s vnitřním okrajem.
pub fn area(s: &mut String) -> egui::TextEdit<'_> {
    egui::TextEdit::multiline(s).margin(egui::Margin::symmetric(8, 6))
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(theme::MUTED)
}


pub fn fmt_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else { format!("{v:.1} {}", U[i]) }
}

pub fn fmt_ago(ts: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let s = (now - ts).max(0);
    match s {
        0..=59 => t!("právě teď", "just now").into(),
        60..=3599 => tf!("před {} min", "{} min ago", s / 60),
        3600..=86_399 => tf!("před {} h", "{} h ago", s / 3600),
        86_400..=2_591_999 => tf!("před {} dny", "{} days ago", s / 86_400),
        _ => tf!("před {} měs.", "{} mo. ago", s / 2_592_000),
    }
}

/// Neaktivní obsah, když Docker není připojen.
pub fn need_engine(app: &mut crate::app::App, ui: &mut egui::Ui) -> bool {
    if app.snap.connected {
        return true;
    }
    let ename = app.cfg.read().map(|c| c.engine.name()).unwrap_or("Engine");
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            dot(ui, theme::GREY);
            ui.label(RichText::new(tf!("{ename} není dostupný", "{ename} is not available")).strong());
        });
        if let Some(e) = &app.snap.error {
            ui.label(muted(e));
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if accent_button(ui, t!("\u{E3D0} Spustit engine", "\u{E3D0} Start engine")).clicked() {
                app.send(crate::worker::Cmd::EngineStart);
            }
            if ui.button("\u{EB02} WSL engine").clicked() {
                app.page = crate::app::Page::Engine;
            }
        });
    });
    false
}

#[cfg(test)]
mod tests {
    #[test]
    fn bytes() {
        assert_eq!(super::fmt_bytes(512), "512 B");
        assert_eq!(super::fmt_bytes(1536), "1.5 KB");
        assert_eq!(super::fmt_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
