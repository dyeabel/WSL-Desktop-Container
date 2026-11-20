//! Prohlížeč logů: okno logů (jeden kontejner nebo sloučené logy compose stacku) a sdílený
//! obsah `view()`, který používá i záložka Logy v detailu kontejneru.
//!
//! Řádky jsou v `logbuf::LogBuf` (nejvýš `MAX_LINES`), vykreslují se jen viditelné
//! (`ScrollArea::show_rows`, pevná výška řádku, neproporcionální písmo) a shody hledání
//! se počítají přírůstkově v `logbuf::ViewCache`.

use super::*;
use crate::app::App;
use crate::logbuf::{self, LogBuf, LogLine, Streams, ViewCache, MAX_LINES, REORDER};
use egui::text::{LayoutJob, TextFormat};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// ikony Phosphor
const CARET_UP: &str = "\u{E13C}";
const CARET_DOWN: &str = "\u{E136}";
const TEXT_AA: &str = "\u{E6EE}";
const WARNING: &str = "\u{E4E0}";

/// Nejvýš vykreslených znaků řádku (uložení a kopie mají celý text).
const MAX_SHOW: usize = 4000;
/// Počet posledních řádků na kontejner u sloučených logů stacku.
const STACK_TAIL: u32 = 500;

pub struct LogView {
    /// ID kontejneru (prázdné u stacku)
    pub id: String,
    /// název kontejneru / projektu
    pub name: String,
    /// Some(projekt) = sloučené logy compose stacku
    pub stack: Option<String>,
    /// názvy zdrojů (služeb) podle indexu zdroje
    pub sources: Vec<String>,
    ids: Vec<String>,
    port: u16,
    tail: u32,
    buf: Arc<Mutex<LogBuf>>,
    streams: Streams,
    /// zobrazit jen řádky obsahující text
    pub filter: String,
    /// hledání se zvýrazněním
    pub search: String,
    pub case: bool,
    pub show_ts: bool,
    /// posouvat na konec při nových řádcích
    pub follow: bool,
    /// skryté služby (podle indexu zdroje)
    hidden: Vec<bool>,
    cache: ViewCache,
    /// aktuální shoda: (abs. řádek, index shody v řádku)
    cur: Option<(u64, usize)>,
    /// posunout na (zobrazený řádek, znak) v příštím snímku
    scroll_to: Option<(usize, usize)>,
    focus_search: bool,
    /// požadavek na znovupřipojení stacku (vyřídí `window`, potřebuje snapshot)
    reload: bool,
    view_size: egui::Vec2,
    scroll_x: f32,
}

impl LogView {
    /// Logy jednoho kontejneru (posledních `tail` řádků a dál živě).
    pub fn single(port: u16, id: String, name: String, tail: u32) -> Self {
        Self::new(port, id.clone(), name.clone(), None, vec![(id, name)], tail)
    }

    /// Sloučené logy kontejnerů compose projektu; `targets` = (id, služba).
    pub fn stack(port: u16, project: String, targets: Vec<(String, String)>) -> Self {
        Self::new(port, String::new(), project.clone(), Some(project), targets, STACK_TAIL)
    }

    fn new(port: u16, id: String, name: String, stack: Option<String>, targets: Vec<(String, String)>, tail: u32) -> Self {
        let reorder = stack.is_some().then_some(REORDER);
        let buf = Arc::new(Mutex::new(LogBuf::new(MAX_LINES, reorder)));
        let (ids, sources): (Vec<String>, Vec<String>) = targets.into_iter().unzip();
        let streams = Streams::start(port, buf.clone(), &ids, tail);
        Self {
            id,
            name,
            stack,
            hidden: vec![false; sources.len()],
            sources,
            ids,
            port,
            tail,
            buf,
            streams,
            filter: String::new(),
            search: String::new(),
            case: false,
            show_ts: false,
            follow: true,
            cache: ViewCache::default(),
            cur: None,
            scroll_to: None,
            focus_search: false,
            reload: false,
            view_size: egui::vec2(600.0, 400.0),
            scroll_x: 0.0,
        }
    }

    /// Sloučené logy stacku: připojí kontejnery projektu, které ještě nejsou připojené
    /// (spuštěné později, nové repliky, znovu vytvořené). Vrací počet nově připojených.
    fn attach_new(&mut self, targets: Vec<(String, String)>) -> usize {
        let mut n = 0;
        for (id, label) in targets {
            if self.ids.contains(&id) {
                continue;
            }
            let src = self.ids.len() as u16;
            // stejná služba jako dřív skrytá -> nový kontejner taky skrytý
            let hidden = self.sources.iter().position(|s| *s == label).and_then(|i| self.hidden.get(i).copied()).unwrap_or(false);
            if let Ok(mut g) = self.buf.lock() {
                g.push_note(src, &tf!("[připojen kontejner {}]", "[attached container {}]", crate::docker::short_id(&id)), std::time::Instant::now());
            }
            self.ids.push(id.clone());
            self.sources.push(label);
            self.hidden.push(hidden);
            // nový kontejner: celý jeho log (je krátký), u ostatních STACK_TAIL
            self.streams.add(self.port, self.buf.clone(), &id, src, self.tail);
            n += 1;
        }
        if n > 0 {
            self.cache = ViewCache::default();
        }
        n
    }

    /// Znovu připojí streamy do prázdného bufferu (u stacku s novým seznamem kontejnerů).
    fn restart(&mut self, targets: Option<Vec<(String, String)>>) {
        self.streams.stop();
        if let Some(t) = targets {
            let (ids, sources): (Vec<String>, Vec<String>) = t.into_iter().unzip();
            // skryté služby podle názvu zůstanou skryté
            self.hidden = sources.iter().map(|s| self.sources.iter().position(|o| o == s).and_then(|i| self.hidden.get(i).copied()).unwrap_or(false)).collect();
            self.ids = ids;
            self.sources = sources;
        }
        let reorder = self.stack.is_some().then_some(REORDER);
        // nový buffer: vlákna starých streamů, která ještě dobíhají, zapisují do starého
        self.buf = Arc::new(Mutex::new(LogBuf::new(MAX_LINES, reorder)));
        self.streams = Streams::start(self.port, self.buf.clone(), &self.ids, self.tail);
        self.cache = ViewCache::default();
        self.cur = None;
        self.reload = false;
    }
}

/// Výsledky ukládání (z vlákna dialogu) – vypíše je `window` jako toasty.
static TOASTS: Mutex<Vec<(bool, String)>> = Mutex::new(Vec::new());
/// Právě je otevřený dialog uložení.
static SAVING: AtomicBool = AtomicBool::new(false);
static DEBUG_DONE: AtomicBool = AtomicBool::new(false);

pub fn window(app: &mut App, ctx: &egui::Context) {
    let toasts = TOASTS.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default();
    for (ok, t) in toasts {
        app.toast(ok, t);
    }
    debug_open(app);
    if let Some(project) = app.logs.as_ref().filter(|l| l.reload).and_then(|l| l.stack.clone()) {
        let targets = logbuf::stack_targets(&app.snap.containers, &project);
        if let Some(lv) = app.logs.as_mut() {
            lv.restart(Some(targets));
        }
    }
    // stack: kontejnery, které naběhly později (snapshot se obnovuje z událostí enginu)
    if let Some(project) = app.logs.as_ref().and_then(|l| l.stack.clone()) {
        let targets = logbuf::stack_targets(&app.snap.containers, &project);
        if let Some(lv) = app.logs.as_mut() {
            lv.attach_new(targets);
        }
    }
    let mut term = None;
    let Some(lv) = app.logs.as_mut() else { return };
    let mut open = true;
    let title = match &lv.stack {
        Some(p) => tf!("\u{EB7A} Logy stacku: {}", "\u{EB7A} Stack logs: {}", p),
        None => tf!("\u{EB7A} Logy: {}", "\u{EB7A} Logs: {}", lv.name),
    };
    egui::Window::new(title)
        .id(egui::Id::new("logs-window"))
        .open(&mut open)
        .collapsible(false)
        .default_size([960.0, 560.0])
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

/// Pro ladění/screenshoty: `WSLCD_DEBUG_LOGS=<kontejner>[:search=<text>]`,
/// `WSLCD_DEBUG_STACK_LOGS=<projekt>[:search=<text>]`.
fn debug_open(app: &mut App) {
    if DEBUG_DONE.load(Ordering::Relaxed) {
        return;
    }
    let (single, stack) = (std::env::var("WSLCD_DEBUG_LOGS").ok(), std::env::var("WSLCD_DEBUG_STACK_LOGS").ok());
    let Some(v) = single.clone().or(stack.clone()) else {
        DEBUG_DONE.store(true, Ordering::Relaxed);
        return;
    };
    let (name, opt) = v.split_once(':').unwrap_or((v.as_str(), ""));
    if single.is_some() {
        let Some(c) = app.snap.containers.iter().find(|c| c.name() == name) else { return };
        let (id, n) = (c.id.clone(), c.name());
        app.open_logs(id, n);
    } else {
        if logbuf::stack_targets(&app.snap.containers, name).is_empty() {
            return;
        }
        app.open_stack_logs(name);
    }
    DEBUG_DONE.store(true, Ordering::Relaxed);
    if let (Some(q), Some(lv)) = (opt.strip_prefix("search="), app.logs.as_mut()) {
        lv.search = q.to_string();
        lv.show_ts = true;
    }
}

/// Stabilní barva služby podle názvu (FNV-1a -> odstín).
fn svc_color(name: &str) -> Color32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    theme::hue((h % 3600) as f32 / 3600.0)
}

/// Obsah prohlížeče logů (lišty + řádky); sdílí okno logů a detail kontejneru.
/// Vrací požadavek na otevření logů v terminálu.
pub fn view(ui: &mut egui::Ui, lv: &mut LogView) -> Option<crate::worker::Term> {
    let mut term = None;
    let stack = lv.stack.is_some();
    lv.hidden.resize(lv.sources.len(), false);
    let buf = lv.buf.clone();
    let mut g = buf.lock().unwrap_or_else(|p| p.into_inner());
    g.flush(Instant::now());
    lv.cache.update(&g, &lv.filter, &lv.hidden, &lv.search, lv.case);
    if lv.cur.is_some_and(|c| lv.cache.hit_index(c).is_none()) {
        lv.cur = None;
    }

    // hledání a filtr
    let mut nav = 0;
    toolbar(ui, |ui| {
        let r = ui.add(field(&mut lv.search).hint_text(t!("\u{E30C} Hledat…", "\u{E30C} Search…")).desired_width(170.0));
        if std::mem::take(&mut lv.focus_search) {
            r.request_focus();
        }
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            nav = if ui.input(|i| i.modifiers.shift) { -1 } else { 1 };
            lv.focus_search = true;
        }
        if r.changed() {
            lv.cur = None;
        }
        if !lv.search.is_empty() {
            let total = lv.cache.hit_count;
            let (txt, color) = match lv.cur.and_then(|c| lv.cache.hit_index(c)) {
                Some(i) => (format!("{}/{total}", i + 1), theme::body_text()),
                None if total == 0 => (t!("žádná shoda", "no matches").to_string(), theme::red()),
                None => (total.to_string(), theme::body_text()),
            };
            ui.label(RichText::new(txt).color(color)).on_hover_text(t!("Shody hledání v zobrazených řádcích", "Search matches in the shown lines"));
        }
        if icon(ui, CARET_UP, t!("Předchozí shoda (Shift+Enter)", "Previous match (Shift+Enter)")) {
            nav = -1;
        }
        if icon(ui, CARET_DOWN, t!("Další shoda (Enter)", "Next match (Enter)")) {
            nav = 1;
        }
        let aa = RichText::new(TEXT_AA).size(16.0);
        if ui.selectable_label(lv.case, aa).on_hover_text(t!("Rozlišovat velká a malá písmena", "Match case")).clicked() {
            lv.case = !lv.case;
            lv.cur = None;
        }
        gap(ui);
        ui.add(field(&mut lv.filter).hint_text(t!("\u{E266} Filtr řádků…", "\u{E266} Filter lines…")).desired_width(170.0))
            .on_hover_text(t!("Zobrazit jen řádky obsahující text", "Show only lines containing the text"));
    });

    // volby a akce
    let prefix = stack.then_some(&lv.sources[..]);
    toolbar(ui, |ui| {
        ui.checkbox(&mut lv.show_ts, t!("Čas", "Time")).on_hover_text(t!("Zobrazit čas řádků (místní čas)", "Show line timestamps (local time)"));
        ui.checkbox(&mut lv.follow, t!("Sledovat konec", "Follow"));
        if ui.button(t!("\u{E21E} Vymazat", "\u{E21E} Clear")).clicked() {
            g.clear();
            lv.cur = None;
        }
        if ui.button(t!("\u{E1CA} Kopírovat", "\u{E1CA} Copy")).on_hover_text(t!("Zkopírovat zobrazené řádky", "Copy the shown lines")).clicked() {
            let rows = lv.cache.rows.iter().filter_map(|r| g.get_abs(r.abs));
            ui.ctx().copy_text(logbuf::format_lines(rows, prefix, lv.show_ts, logbuf::local_offset));
        }
        ui.menu_button(t!("\u{E248} Uložit…", "\u{E248} Save…"), |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            let mut text = None;
            if ui.button(tf!("Celý buffer ({} řádků)", "Whole buffer ({} lines)", g.len())).clicked() {
                text = Some((logbuf::format_lines(g.iter(), prefix, lv.show_ts, logbuf::local_offset), g.len()));
            }
            let n = lv.cache.rows.len();
            if ui.button(tf!("Jen zobrazené řádky ({n})", "Shown lines only ({n})")).clicked() {
                let rows = lv.cache.rows.iter().filter_map(|r| g.get_abs(r.abs));
                text = Some((logbuf::format_lines(rows, prefix, lv.show_ts, logbuf::local_offset), n));
            }
            if lv.show_ts {
                ui.label(muted(t!("Včetně času řádků", "Including timestamps")).small());
            }
            if let Some((text, n)) = text {
                save(ui.ctx(), &lv.name, text, n);
                ui.close_menu();
            }
        });
        let reload_tip = if stack {
            t!("Znovu připojit logy všech kontejnerů stacku (i nově spuštěných)", "Re-attach the logs of all the stack's containers (including newly started ones)")
        } else {
            t!("Znovu načíst log kontejneru", "Reload the container log")
        };
        if ui.button(t!("\u{E094} Znovu načíst", "\u{E094} Reload")).on_hover_text(reload_tip).clicked() {
            lv.reload = true;
        }
        if !stack && ui.button(t!("\u{EAE8} V terminálu", "\u{EAE8} In terminal")).clicked() {
            term = Some(crate::worker::Term::Logs { id: lv.id.clone(), name: lv.name.clone() });
        }
        gap(ui);
        ui.label(muted(tf!("{} řádků", "{} lines", g.len())));
        if g.dropped > 0 {
            ui.label(RichText::new(tf!("{WARNING} starší řádky zahozeny", "{WARNING} older lines dropped")).color(theme::yellow())).on_hover_text(tf!(
                "Uchovává se nejvýš {MAX_LINES} řádků, zahozeno {}",
                "At most {MAX_LINES} lines are kept, {} dropped",
                g.dropped
            ));
        }
    });

    // filtr služeb (stack)
    if stack {
        toolbar(ui, |ui| {
            ui.label(muted(t!("Služby:", "Services:")));
            for (i, name) in lv.sources.iter().enumerate() {
                let mut on = !lv.hidden[i];
                if ui.checkbox(&mut on, RichText::new(name).color(svc_color(name)).monospace()).changed() {
                    lv.hidden[i] = !on;
                }
            }
            if lv.hidden.iter().any(|h| *h) && ui.button(t!("Vše", "All")).clicked() {
                lv.hidden.iter_mut().for_each(|h| *h = false);
            }
            if lv.sources.is_empty() {
                ui.label(muted(t!("Stack nemá žádné kontejnery", "The stack has no containers")));
            }
        });
    }

    if lv.reload && !stack {
        drop(g);
        lv.restart(None);
        return term;
    }

    // změny z lišt (hledání, filtr, vymazání) – přepočítat ještě v tomto snímku
    lv.cache.update(&g, &lv.filter, &lv.hidden, &lv.search, lv.case);
    if nav != 0 {
        if let Some((row, abs, k)) = lv.cache.step(lv.cur, nav) {
            lv.cur = Some((abs, k));
            let start = lv.cache.rows[row].hits[k].start;
            let x = g.get_abs(abs).map(|l| l.text[..start].chars().count()).unwrap_or(0);
            lv.scroll_to = Some((row, x));
            lv.follow = false;
        }
    }

    ui.separator();
    let n = lv.cache.rows.len();
    if n == 0 {
        let msg = if g.len() == 0 { t!("Zatím žádné řádky…", "No lines yet…") } else { t!("Žádný řádek neodpovídá filtru", "No line matches the filter") };
        ui.label(muted(msg));
        return term;
    }
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let (row_h, char_w) = ui.fonts(|f| (f.row_height(&mono) + 3.0, f.glyph_width(&mono, '0')));
    let ts_w = if lv.show_ts { 13.0 * char_w } else { 0.0 };
    let src_chars = if stack { lv.sources.iter().map(|s| s.chars().count()).max().unwrap_or(0).min(24) + 2 } else { 0 };
    let src_w = src_chars as f32 * char_w;
    let text_x = ts_w + src_w;
    let content_w = text_x + (lv.cache.max_chars.min(MAX_SHOW) + 2) as f32 * char_w;

    let mut sa = egui::ScrollArea::both().id_salt("log-rows").auto_shrink([false, false]).stick_to_bottom(lv.follow);
    if let Some((row, xc)) = lv.scroll_to.take() {
        let y = (row as f32 * row_h - lv.view_size.y / 2.0).max(0.0);
        let x = text_x + xc as f32 * char_w;
        let visible = x >= lv.scroll_x + text_x && x <= lv.scroll_x + lv.view_size.x - 6.0 * char_w;
        let sx = if visible { lv.scroll_x } else { (x - lv.view_size.x / 3.0).max(0.0) };
        sa = sa.vertical_scroll_offset(y).horizontal_scroll_offset(sx);
    }
    let rows = &lv.cache.rows;
    let (cur, show_ts, sources) = (lv.cur, lv.show_ts, &lv.sources);
    let out = ui
        .scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            sa.show_rows(ui, row_h, n, |ui, range| {
                ui.set_min_width(content_w);
                let painter = ui.painter().clone();
                for i in range {
                    let Some(row) = rows.get(i) else { break };
                    let Some(line) = g.get_abs(row.abs) else { continue };
                    let w = ui.available_width().max(content_w);
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, row_h), egui::Sense::hover());
                    let y = rect.center().y;
                    if show_ts {
                        if let Some(ns) = line.ts {
                            let off = logbuf::local_offset(ns.div_euclid(1_000_000_000));
                            painter.text(egui::pos2(rect.left(), y), egui::Align2::LEFT_CENTER, logbuf::fmt_short(ns, off), mono.clone(), theme::muted());
                            let r = egui::Rect::from_min_size(rect.min, egui::vec2(ts_w - char_w, row_h));
                            ui.interact(r, ui.id().with(("log-ts", row.abs)), egui::Sense::hover()).on_hover_text(logbuf::fmt_full(ns, off));
                        }
                    }
                    if stack {
                        let name = sources.get(line.src as usize).map(String::as_str).unwrap_or("?");
                        let shown: String = name.chars().take(src_chars.saturating_sub(2)).collect();
                        painter.text(egui::pos2(rect.left() + ts_w, y), egui::Align2::LEFT_CENTER, shown, mono.clone(), svc_color(name));
                    }
                    let cur_k = cur.filter(|c| c.0 == row.abs).map(|c| c.1);
                    let galley = ui.fonts(|f| f.layout_job(highlight_job(line, &row.hits, cur_k, &mono)));
                    painter.galley(egui::pos2(rect.left() + text_x, y - galley.size().y / 2.0), galley, theme::body_text());
                }
            })
        })
        .inner;
    lv.view_size = out.inner_rect.size();
    lv.scroll_x = out.state.offset.x;
    term
}

/// Text řádku se zvýrazněnými shodami (aktuální shoda plnou barvou akcentu).
fn highlight_job(line: &LogLine, hits: &[Range<usize>], cur: Option<usize>, font: &egui::FontId) -> LayoutJob {
    let text = line.text.as_str();
    let cut = text.char_indices().nth(MAX_SHOW).map(|(i, _)| i);
    let shown = &text[..cut.unwrap_or(text.len())];
    let normal = TextFormat { font_id: font.clone(), color: theme::body_text(), ..Default::default() };
    let mut job = LayoutJob::default();
    let mut pos = 0;
    for (k, h) in hits.iter().enumerate() {
        if h.start >= shown.len() {
            break;
        }
        let end = h.end.min(shown.len());
        if h.start > pos {
            job.append(&shown[pos..h.start], 0.0, normal.clone());
        }
        let (bg, fg) = if cur == Some(k) { (theme::accent(), theme::accent_text()) } else { (theme::accent().gamma_multiply(0.4), theme::strong_text()) };
        job.append(&shown[h.start..end], 0.0, TextFormat { background: bg, color: fg, ..normal.clone() });
        pos = end;
    }
    if pos < shown.len() {
        job.append(&shown[pos..], 0.0, normal.clone());
    }
    if cut.is_some() {
        job.append(" …", 0.0, TextFormat { color: theme::muted(), ..normal });
    }
    job
}

/// Dialog uložení a zápis souboru na pozadí; výsledek jako toast.
fn save(ctx: &egui::Context, name: &str, text: String, lines: usize) {
    if SAVING.swap(true, Ordering::SeqCst) {
        return;
    }
    let now = logbuf::now_ns();
    let file = logbuf::save_file_name(name, now, logbuf::local_offset(now.div_euclid(1_000_000_000)));
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let picked = rfd::FileDialog::new()
            .set_title(t!("Uložit log", "Save log"))
            .set_file_name(&file)
            .add_filter("log", &["log", "txt"])
            .save_file();
        if let Some(p) = picked {
            let res = match std::fs::write(&p, text.as_bytes()) {
                Ok(()) => (true, tf!("Log uložen ({lines} řádků): {}", "Log saved ({lines} lines): {}", p.display())),
                Err(e) => (false, tf!("Log nelze uložit: {e}", "Cannot save the log: {e}")),
            };
            if let Ok(mut g) = TOASTS.lock() {
                g.push(res);
            }
            ctx.request_repaint();
        }
        SAVING.store(false, Ordering::SeqCst);
    });
}
