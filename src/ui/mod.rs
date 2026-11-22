pub mod activity;
pub mod alerts;
pub mod events;
pub mod build;
pub mod chart;
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
pub mod k8s;
pub mod k8s_map;
pub mod logs;
pub mod memory;
pub mod net_overview;
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
    /// klikací řádek: text v buňkách nejde označovat (označitelný popisek by si klik vzal)
    clickable: bool,
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
        let _ = self.row_sense(height, egui::Sense::hover(), add);
    }

    /// Klikací řádek (klik mimo tlačítka v buňkách) – vrací true po kliknutí.
    pub fn row_click(&mut self, height: f32, add: impl FnOnce(TableRow<'_>)) -> bool {
        let resp = self.row_sense(height, egui::Sense::click(), add);
        if resp.hovered() {
            self.ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp.clicked()
    }

    fn row_sense(&mut self, height: f32, sense: egui::Sense, add: impl FnOnce(TableRow<'_>)) -> egui::Response {
        let total = total_width(&self.widths, self.gap);
        let (rect, resp) = self.ui.allocate_exact_size(egui::vec2(total, height), sense);
        if self.ui.is_rect_visible(rect) {
            // zvýraznit podle polohy ukazatele v celém řádku – `hovered()` je false nad popisky
            // s bublinou (image, porty, stav), které leží nad řádkem
            let fill = if resp.contains_pointer() {
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
        let clickable = sense.senses_click();
        add(TableRow { ui: self.ui, widths: &self.widths, gap: self.gap, rect, x: rect.left() + CELL_PAD, idx: 0, clickable });
        resp
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
        if self.clickable {
            child.style_mut().interaction.selectable_labels = false;
        }
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
    egui::Id::new(("wslcd-sort", key))
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
    let mut h = TableRow { ui, widths: &widths, gap: GAP, rect, x: rect.left() + CELL_PAD, idx: 0, clickable: false };
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

/// Barva vytížení: běžně akcent, od 75 % žlutá, od 90 % červená.
fn load_color(frac: f32) -> Color32 {
    if frac >= 0.9 {
        theme::red()
    } else if frac >= 0.75 {
        theme::yellow()
    } else {
        theme::accent()
    }
}

/// Co karta enginu ukazuje navíc k vzorku paměti (ze snímku enginu).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct GlanceCtx {
    /// název enginu (popisek řádku kontejnerů)
    pub engine: &'static str,
    /// běžící / všechny kontejnery (jen když engine odpovídá)
    pub containers: Option<(i64, i64)>,
    /// počet CPU VM (NCPU z /info)
    pub ncpu: Option<i64>,
    /// uzel Kubernetes laboratoře: Some(běží) – None = cluster není
    pub k8s: Option<bool>,
}

/// Řádek karty enginu ve stylu Kiri: popisek vlevo, hodnota vpravo, volitelně tenký pruh
/// pod řádkem nebo barevná tečka před hodnotou.
#[derive(Debug, Clone, PartialEq)]
pub struct GlanceRow {
    pub label: String,
    pub value: String,
    pub bar: Option<f32>,
    pub dot: Option<bool>,
    /// část pruhu zvýrazněná barvou akcentu (VmmemWSL v obsazené RAM Windows); s ní se celý
    /// pruh kreslí tlumeně
    pub part: Option<f32>,
    /// hodnota před `value` v barvě zvýrazněné části („1.7 · …“)
    pub lead: Option<String>,
}

/// Nahoře stavové řádky (kontejnery, Kubernetes), pod nimi metriky s pruhem (CPU, paměť VM,
/// paměť Windows) – všechny metriky mají pruh, takže rozteč je pravidelná.
pub fn glance_rows(g: &crate::memory::Glance, x: &GlanceCtx) -> Vec<GlanceRow> {
    let frac = |a: u64, b: u64| (b > 0).then(|| (a as f64 / b as f64) as f32);
    let row = |label: String, value: String| GlanceRow { label, value, bar: None, dot: None, part: None, lead: None };
    let mut rows = vec![];
    if let Some((r, t)) = x.containers {
        let label = if x.engine.is_empty() { t!("Kontejnery", "Containers").to_string() } else { tf!("Kontejnery · {}", "Containers · {}", x.engine) };
        rows.push(row(label, format!("{r} / {t}")));
    }
    if let Some(on) = x.k8s {
        rows.push(GlanceRow { dot: Some(on), ..row("Kubernetes".into(), if on { t!("Běží", "Running") } else { t!("Zastaven", "Stopped") }.into()) });
    }
    let cpu = g.cpu.map(|c| format!("{c:.0} %")).unwrap_or_else(|| "…".into());
    let cpu = match x.ncpu.filter(|n| *n > 0) {
        Some(n) => format!("{cpu} · {n}"),
        None => cpu,
    };
    rows.push(GlanceRow { bar: Some(g.cpu.map(|c| (c / 100.0) as f32).unwrap_or(0.0)), ..row("CPU".into(), cpu) });
    rows.push(GlanceRow { bar: Some(frac(g.used, g.total).unwrap_or(0.0)), ..row(t!("Paměť", "Memory").into(), format!("{} / {}", fmt_gb(g.used), fmt_gb(g.total))) });
    // Windows: pruh = obsazená RAM Windows celkem, v ní zvýrazněný podíl VmmemWSL
    if g.ram > 0 && g.win_used > 0 {
        let gb = |b: u64| format!("{:.1}", b as f64 / (1024.0 * 1024.0 * 1024.0));
        rows.push(GlanceRow {
            bar: frac(g.win_used, g.ram),
            part: g.vmmem.and_then(|v| frac(v, g.ram)),
            lead: g.vmmem.map(gb),
            ..row("Windows".into(), format!("{} / {}", gb(g.win_used), fmt_gb(g.ram)))
        });
    } else if let Some(v) = g.vmmem {
        rows.push(GlanceRow { bar: Some(frac(v, g.ram).unwrap_or(0.0)), ..row("Windows".into(), fmt_gb(v)) });
    }
    rows
}

/// Výška řádku karty enginu: text, u metriky navíc pruh pod ním.
/// Pruh má vlastní místo pod textem (mezera, pruh, mezera k dalšímu řádku) – nepřetíná písmena.
const GLANCE_TEXT: f32 = 16.0;
const GLANCE_BAR_GAP: f32 = 3.0;
const GLANCE_BAR: f32 = 3.0;
const GLANCE_BAR_AFTER: f32 = 6.0;

fn glance_row_h(r: &GlanceRow) -> f32 {
    GLANCE_TEXT + if r.bar.is_some() { GLANCE_BAR_GAP + GLANCE_BAR + GLANCE_BAR_AFTER } else { 0.0 }
}

/// Karta enginu v bočním panelu (styl Kiri): kontejnery, Kubernetes, CPU, paměť ve VM a ve
/// Windows (VmmemWSL). Celý blok je klikací, detail v bublině.
pub fn glance(ui: &mut egui::Ui, g: &crate::memory::Glance, x: &GlanceCtx) -> egui::Response {
    let rows = glance_rows(g, x);
    let w = ui.available_width();
    let h: f32 = rows.iter().map(glance_row_h).sum::<f32>() - GLANCE_BAR_AFTER;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    let p = ui.painter_at(rect);
    // popisek i hodnota stejným písmem → společná účaří
    let font = egui::TextStyle::Small.resolve(ui.style());
    let (x0, x1) = (rect.left(), rect.right());
    let mut top = rect.top();
    for r in &rows {
        let y = top + GLANCE_TEXT / 2.0;
        p.text(egui::pos2(x0, y), egui::Align2::LEFT_CENTER, &r.label, font.clone(), theme::muted());
        let mut vr = p.text(egui::pos2(x1, y), egui::Align2::RIGHT_CENTER, &r.value, font.clone(), theme::strong_text());
        if let Some(lead) = &r.lead {
            vr = p.text(egui::pos2(vr.left(), y), egui::Align2::RIGHT_CENTER, format!("{lead} · "), font.clone(), theme::accent());
        }
        if let Some(on) = r.dot {
            p.circle_filled(egui::pos2(vr.left() - 7.0, y), 3.0, if on { theme::green() } else { theme::muted() });
        }
        if let Some(f) = r.bar {
            let by = top + GLANCE_TEXT + GLANCE_BAR_GAP;
            let bar = egui::Rect::from_min_max(egui::pos2(x0, by), egui::pos2(x1.max(x0 + 10.0), by + GLANCE_BAR));
            p.rect_filled(bar, 2.0, theme::card_stroke());
            let seg = |f: f32, color: Color32| {
                let f = f.clamp(0.0, 1.0);
                if f > 0.0 {
                    p.rect_filled(egui::Rect::from_min_max(bar.min, egui::pos2(bar.left() + (bar.width() * f).max(3.0), bar.bottom())), 2.0, color);
                }
            };
            match r.part {
                // celkové obsazení tlumeně (od 75 % žlutě, od 90 % červeně), přes něj podíl WSL
                Some(part) => {
                    let total = if f >= 0.75 { load_color(f) } else { theme::muted() };
                    seg(f, total.gamma_multiply(0.55));
                    seg(part, theme::accent());
                }
                None => seg(f, load_color(f)),
            }
        }
        top += glance_row_h(r);
    }
    let vmmem = g.vmmem.map(fmt_bytes).unwrap_or_else(|| "?".into());
    let cpu = g.cpu.map(|c| format!("{c:.1} %")).unwrap_or_else(|| "…".into());
    let (used, total, cache, ram, win) = (fmt_bytes(g.used), fmt_bytes(g.total), fmt_bytes(g.cache), fmt_bytes(g.ram), fmt_bytes(g.win_used));
    resp.on_hover_text(tf!(
        "Celá WSL VM (všechny distribuce)\nCPU: {cpu} ze všech procesorů VM (· jejich počet)\nPaměť – použitá procesy v Linuxu: {used} z {total}\nCache souborů: {cache}\nWindows – obsazená RAM Windows: {win} z {ram}\n  z toho WSL VM (VmmemWSL, barevně): {vmmem}\n  (obsahuje i cache a paměť, kterou Linux ještě nevrátil)\nKlikni pro Přehled",
        "Whole WSL VM (all distributions)\nCPU: {cpu} of all VM processors (· their count)\nMemory – used by Linux processes: {used} of {total}\nFile cache: {cache}\nWindows – Windows RAM in use: {win} of {ram}\n  of which the WSL VM (VmmemWSL, coloured): {vmmem}\n  (includes cache and memory Linux has not returned yet)\nClick for the Overview"
    ))
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// GB s jedním desetinným místem (kompaktní pro kartu enginu).
fn fmt_gb(b: u64) -> String {
    format!("{:.1} GB", b as f64 / (1024.0 * 1024.0 * 1024.0))
}

/// Dvouřádková buňka: hlavní text + šedý popisek.
pub fn two_line(ui: &mut egui::Ui, main: RichText, sub: RichText) {
    stack_v(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.label(main);
        ui.label(sub);
    });
}

/// Svislý blok (víc řádků) svisle na střed buňky / řádku.
/// `ui.vertical` uvnitř vodorovného rozložení egui umístí nahoru (výšku obsahu ještě nezná),
/// takže dvouřádkové buňky neseděly s jednořádkovými. Výška obsahu se pamatuje z minulého
/// snímku a blok se odsadí o polovinu zbývajícího místa.
pub fn stack_v<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let id = ui.next_auto_id().with("stack_v");
    let avail = ui.available_height();
    let prev: Option<f32> = ui.ctx().data(|d| d.get_temp(id));
    let top = prev.map(|h| ((avail - h) / 2.0).max(0.0)).unwrap_or(0.0);
    let r = ui.vertical(|ui| {
        ui.add_space(top);
        ui.scope(add)
    });
    let h = r.inner.response.rect.height();
    if prev.is_none_or(|p| (p - h).abs() > 0.5) {
        ui.ctx().data_mut(|d| d.insert_temp(id, h));
        ui.ctx().request_repaint();
    }
    r.inner.inner
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
            // název stránky podle hostitele (Linux: „Engine“)
            let page = if app.kind() == crate::host::HostKind::Local { "\u{EB02} Engine" } else { "\u{EB02} WSL engine" };
            if ui.button(page).clicked() {
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

    /// Klik kamkoli do řádku (i na popisek s bublinou) je klik na řádek; tlačítko v buňce
    /// si klik nechá.
    #[test]
    fn table_row_click_outside_buttons() {
        use super::{egui, W};
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let frame = |events: Vec<egui::Event>| {
            let (mut row_clicked, mut btn_clicked) = (false, false);
            let (mut label, mut btn) = (egui::Rect::NOTHING, egui::Rect::NOTHING);
            let _ = ctx.run(egui::RawInput { screen_rect: Some(screen), events, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    super::table(ui, "t", &[("A", W::Px(200.0)), ("B", W::Px(100.0))], |mut body| {
                        row_clicked = body.row_click(40.0, |mut r| {
                            r.col(|ui| label = ui.label("wslcd-k3s").on_hover_text("bublina").rect);
                            r.col(|ui| {
                                let b = ui.button("stop");
                                btn = b.rect;
                                btn_clicked = b.clicked();
                            });
                        });
                    });
                });
            });
            (row_clicked, btn_clicked, label, btn)
        };
        let btn_ev = |p: egui::Pos2, pressed: bool| egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        // klik jako od uživatele: najetí, stisk a uvolnění v samostatných snímcích
        let click = |p: egui::Pos2| {
            let _ = frame(vec![egui::Event::PointerMoved(p)]);
            let _ = frame(vec![btn_ev(p, true)]);
            let (row, b, ..) = frame(vec![btn_ev(p, false)]);
            (row, b)
        };
        let (_, _, label, btn) = frame(vec![]);
        let empty = egui::pos2(label.right() + 40.0, label.center().y);
        assert_eq!(click(empty), (true, false), "klik do prázdného místa řádku");
        assert_eq!(click(label.center()), (true, false), "klik na popisek s bublinou otevře řádek");
        assert_eq!(click(btn.center()), (false, true), "klik na tlačítko nepatří řádku");
    }

    #[test]
    fn glance_rows_like_kiri() {
        use super::{GlanceCtx, GlanceRow};
        const G: u64 = 1024 * 1024 * 1024;
        let g = crate::memory::Glance { used: 2 * G, total: 16 * G, cache: G, vmmem: Some(6 * G), ram: 32 * G, win_used: 24 * G, cpu: Some(12.0) };
        let x = GlanceCtx { engine: "Podman", containers: Some((3, 5)), ncpu: Some(8), k8s: Some(true) };
        let rows = super::glance_rows(&g, &x);
        assert_eq!(rows.len(), 5, "kontejnery, Kubernetes, CPU, paměť, Windows");
        assert!(rows[0].label.ends_with("Podman") && rows[0].value == "3 / 5" && rows[0].bar.is_none());
        assert_eq!((rows[1].label.as_str(), rows[1].dot, rows[1].bar), ("Kubernetes", Some(true), None));
        assert_eq!((rows[2].label.as_str(), rows[2].value.as_str(), rows[2].bar), ("CPU", "12 % · 8", Some(0.12)));
        assert_eq!((rows[3].value.as_str(), rows[3].bar), ("2.0 GB / 16.0 GB", Some(2.0 / 16.0)));
        // Windows: obsazeno celkem 24 z 32 GB, z toho VmmemWSL 6 GB (zvýrazněná část a číslo vpředu)
        assert_eq!((rows[4].label.as_str(), rows[4].value.as_str(), rows[4].bar), ("Windows", "24.0 / 32.0 GB", Some(0.75)));
        assert_eq!((rows[4].part, rows[4].lead.as_deref()), (Some(6.0 / 32.0), Some("6.0")));
        // bez enginu, VmmemWSL a clusteru jen metriky CPU a paměť; neznámá RAM Windows → prázdný pruh
        let min = super::glance_rows(&crate::memory::Glance { vmmem: None, win_used: 0, ..g }, &GlanceCtx::default());
        assert_eq!((min.len(), min[0].label.as_str(), min[0].value.as_str()), (2, "CPU", "12 %"));
        let r: Vec<GlanceRow> = super::glance_rows(&crate::memory::Glance { ram: 0, ..g }, &GlanceCtx::default());
        assert_eq!((r[2].bar, r[2].value.as_str(), r[2].part), (Some(0.0), "6.0 GB", None));
        // VmmemWSL nezměřený: pruh jen celkového obsazení Windows
        let r = super::glance_rows(&crate::memory::Glance { vmmem: None, ..g }, &GlanceCtx::default());
        assert_eq!((r[2].value.as_str(), r[2].part, r[2].lead.as_deref()), ("24.0 / 32.0 GB", None, None));

        // vykreslení bez okna: 5 řádků textu, pod třemi metrikami pruh – nic se nepřekrývá
        use super::egui;
        let ctx = egui::Context::default();
        let mut h = 0.0;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| h = super::glance(ui, &g, &x).rect.height());
        });
        assert_eq!(h, 5.0 * 16.0 + 3.0 * 12.0 - 6.0, "pruh pod textem, ne přes něj");
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
