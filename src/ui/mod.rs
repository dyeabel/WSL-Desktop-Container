pub mod build;
pub mod compose;
pub mod compose_new;
pub mod container_files;
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
        ui.label(RichText::new(subtitle).color(theme::muted()));
    }
    ui.add_space(10.0);
}

pub fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(RichText::new(title).size(17.0).strong());
    ui.add_space(4.0);
}

pub fn accent_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(theme::accent_text())).fill(theme::accent()))
}

pub fn danger_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Button::new(RichText::new(text).color(Color32::WHITE)).fill(theme::red().linear_multiply(0.85)))
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
        .fill(theme::card())
        .stroke(egui::Stroke::new(1.0_f32, theme::card_stroke()))
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
                Some(theme::row_hover())
            } else if self.n % 2 == 1 {
                Some(theme::row_stripe())
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
    table_sort(ui, id, cols, Sort::NONE, body);
}

/// Tabulka v kartě s řaditelnými sloupci (klik na hlavičku: vzestupně / sestupně).
pub fn table_sort(ui: &mut egui::Ui, id: &str, cols: &[(&str, W)], sort: Sort<'_>, body: impl FnOnce(TableBody<'_>)) {
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        table_in_sort(ui, id, cols, sort, body);
    });
}

// ---------------------------------------------------------------- řazení

/// Které sloupce tabulky jdou řadit a pod jakým klíčem se stav pamatuje
/// (víc tabulek může sdílet jeden klíč – např. skupiny kontejnerů).
#[derive(Clone, Copy)]
pub struct Sort<'a> {
    pub key: &'a str,
    pub cols: &'a [usize],
}

impl Sort<'static> {
    pub const NONE: Sort<'static> = Sort { key: "", cols: &[] };
}

/// Hodnota pro řazení.
#[derive(Clone, Debug, PartialEq)]
pub enum SortKey {
    Text(String),
    Num(f64),
}

impl SortKey {
    pub fn text(s: impl AsRef<str>) -> Self {
        SortKey::Text(s.as_ref().to_lowercase())
    }
    pub fn num(n: impl Into<f64>) -> Self {
        SortKey::Num(n.into())
    }
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        match (self, o) {
            (SortKey::Num(a), SortKey::Num(b)) => a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal),
            (SortKey::Text(a), SortKey::Text(b)) => natural_cmp(a, b),
            (SortKey::Num(_), SortKey::Text(_)) => std::cmp::Ordering::Less,
            (SortKey::Text(_), SortKey::Num(_)) => std::cmp::Ordering::Greater,
        }
    }
}

/// Porovnání „přirozeně“: čísla v textu podle hodnoty (web2 < web10).
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n1 = String::new();
                while let Some(c) = x.peek().copied().filter(char::is_ascii_digit) {
                    n1.push(c);
                    x.next();
                }
                let mut n2 = String::new();
                while let Some(c) = y.peek().copied().filter(char::is_ascii_digit) {
                    n2.push(c);
                    y.next();
                }
                let (t1, t2) = (n1.trim_start_matches('0'), n2.trim_start_matches('0'));
                let o = t1.len().cmp(&t2.len()).then_with(|| t1.cmp(t2));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                if c != d {
                    return c.cmp(&d);
                }
                x.next();
                y.next();
            }
        }
    }
}

fn sort_id(key: &str) -> egui::Id {
    egui::Id::new(("wls-sort", key))
}

/// Aktuální řazení tabulky: (index sloupce, vzestupně).
pub fn sort_state(ctx: &egui::Context, key: &str) -> Option<(usize, bool)> {
    ctx.data_mut(|d| d.get_persisted::<(usize, bool)>(sort_id(key)))
}

/// Seřadí položky podle zvoleného sloupce (stabilně; bez volby se pořadí nemění).
/// `key(položka, sloupec)` vrací hodnotu, podle které se řadí.
pub fn sort_items<T>(ctx: &egui::Context, key: &str, items: &mut [T], k: impl Fn(&T, usize) -> SortKey) {
    if let Some((col, asc)) = sort_state(ctx, key) {
        items.sort_by(|a, b| {
            let o = k(a, col).cmp(&k(b, col));
            if asc { o } else { o.reverse() }
        });
    }
}

/// Klik na hlavičku: jiný sloupec -> vzestupně, stejný -> obrátit, třetí klik -> zrušit.
fn toggle_sort(ctx: &egui::Context, key: &str, col: usize) {
    let next = match sort_state(ctx, key) {
        Some((c, true)) if c == col => Some((col, false)),
        Some((c, false)) if c == col => None,
        _ => Some((col, true)),
    };
    ctx.data_mut(|d| match next {
        Some(v) => d.insert_persisted(sort_id(key), v),
        None => d.remove::<(usize, bool)>(sort_id(key)),
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
    table_in_sort(ui, id, cols, Sort::NONE, body);
}

pub fn table_in_sort(ui: &mut egui::Ui, id: &str, cols: &[(&str, W)], sort: Sort<'_>, body: impl FnOnce(TableBody<'_>)) {
    let avail = ui.available_width();
    let (full, full_min) = layout_widths(cols, avail, false);
    if avail >= full_min {
        build_table(ui, cols, full, sort, body);
        return;
    }
    let (reduced, reduced_min) = layout_widths(cols, avail, true);
    if avail >= reduced_min {
        build_table(ui, cols, reduced, sort, body);
    } else {
        egui::ScrollArea::horizontal().id_salt(format!("{id}-hscroll")).show(ui, |ui| {
            build_table(ui, cols, reduced, sort, body);
        });
    }
}

fn build_table(ui: &mut egui::Ui, cols: &[(&str, W)], widths: Vec<f32>, sort: Sort<'_>, body: impl FnOnce(TableBody<'_>)) {
    ui.spacing_mut().item_spacing.y = 0.0;
    let state = if sort.key.is_empty() { None } else { sort_state(ui.ctx(), sort.key) };
    let mut clicked = None;
    // hlavička (bez výplně), pod ní tenká čára a mezera před prvním řádkem
    let total = total_width(&widths, GAP);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(total, 28.0), egui::Sense::hover());
    let mut h = TableRow { ui, widths: &widths, gap: GAP, rect, x: rect.left() + CELL_PAD, idx: 0 };
    for (i, (t, _)) in cols.iter().enumerate() {
        let sortable = sort.cols.contains(&i) && !t.is_empty();
        h.col(|ui| {
            let arrow = match state {
                Some((c, true)) if c == i => " \u{E13C}",
                Some((c, false)) if c == i => " \u{E136}",
                _ => "",
            };
            let active = !arrow.is_empty();
            let text = RichText::new(format!("{t}{arrow}")).small().strong().color(if active { theme::accent() } else { theme::muted() });
            if sortable {
                let r = ui.add(egui::Label::new(text).selectable(false).sense(egui::Sense::click()));
                if r.clicked() {
                    clicked = Some(i);
                }
                r.on_hover_text(t!("Seřadit (klikni znovu pro opačné pořadí)", "Sort (click again to reverse)"));
            } else {
                ui.label(text);
            }
        });
    }
    let y = rect.bottom() + 1.0;
    ui.painter().hline(rect.left()..=rect.right(), y, egui::Stroke::new(1.0_f32, theme::card_stroke()));
    ui.add_space(6.0);
    if let Some(i) = clicked {
        toggle_sort(ui.ctx(), sort.key, i);
    }
    body(TableBody { ui, widths, gap: GAP, n: 0 });
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
                .fill(theme::dialog())
                .stroke(egui::Stroke::new(1.0_f32, theme::card_stroke()))
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
    RichText::new(text.into()).color(theme::muted())
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
            dot(ui, theme::grey());
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

#[cfg(test)]
mod sort_tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["web10", "web2", "Web1", "api"];
        v.sort_by(|a, b| natural_cmp(&a.to_lowercase(), &b.to_lowercase()));
        assert_eq!(v, ["api", "Web1", "web2", "web10"]);
        assert!(SortKey::num(2).cmp(&SortKey::num(10)).is_lt());
    }

    #[test]
    fn click_cycles_asc_desc_off() {
        let ctx = egui::Context::default();
        let mut items = vec![("b", 2), ("a", 3), ("c", 1)];
        toggle_sort(&ctx, "t", 1);
        assert_eq!(sort_state(&ctx, "t"), Some((1, true)));
        sort_items(&ctx, "t", &mut items, |x, _| SortKey::num(x.1));
        assert_eq!(items[0].0, "c");
        toggle_sort(&ctx, "t", 1);
        sort_items(&ctx, "t", &mut items, |x, _| SortKey::num(x.1));
        assert_eq!(items[0].0, "a");
        toggle_sort(&ctx, "t", 1);
        assert_eq!(sort_state(&ctx, "t"), None);
        toggle_sort(&ctx, "t", 1);
        toggle_sort(&ctx, "t", 0);
        assert_eq!(sort_state(&ctx, "t"), Some((0, true)));
    }
}
