//! Přehled paměti WSL na stránce Přehled: kam jde RAM, kterou drží VmmemWSL.

use super::chart::{line_chart_gaps, Marker, Series};
use super::*;
use crate::app::App;
use crate::history::{Agg, Raw, AGG_STEP};
use crate::memory::{Breakdown, FreeEvent, MemPoint, HISTORY_SECS};
use crate::worker::Cmd;
use std::sync::atomic::{AtomicBool, Ordering};

/// Stav sekce v UI (zbytek je ve `Shared::memory`, zapisuje worker).
#[derive(Default)]
pub struct MemUi {
    /// běží ruční uvolnění paměti
    pub freeing: bool,
    /// poslední výsledek uvolnění (i automatického)
    pub last: Option<Result<FreeEvent, String>>,
    /// posunout přehled na tuto sekci (kliknutí na upozornění)
    pub scroll_to: bool,
    /// rozsah grafu historie (index do `RANGES`)
    pub range: usize,
    /// otevřený dialog s týdenním souhrnem
    pub week_open: bool,
}

/// Výsledek uvolnění od workeru.
pub fn on_freed(app: &mut App, auto: bool, r: Result<FreeEvent, String>) {
    if !auto {
        app.mem.freeing = false;
    }
    app.mem.last = Some(r);
}

/// Spustí ruční uvolnění paměti (tlačítko v sekci i nabídka Uklidit).
pub fn free(app: &mut App) {
    app.mem.freeing = true;
    app.send(Cmd::FreeMemory);
}

static DEBUG_DONE: AtomicBool = AtomicBool::new(false);

// barvy částí – funkce kvůli přepínání motivu
fn c_containers() -> Color32 {
    theme::accent()
}
fn c_engine() -> Color32 {
    theme::hue(0.58)
}
fn c_rest() -> Color32 {
    theme::green()
}
fn c_cache() -> Color32 {
    theme::hue(0.78)
}
fn c_unreturned() -> Color32 {
    theme::grey()
}
fn c_vmmem() -> Color32 {
    theme::red()
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let (now, points, events) = match app.shared.memory.lock() {
        Ok(m) => (m.now.clone(), m.history.points.iter().copied().collect::<Vec<_>>(), m.history.events.iter().copied().collect::<Vec<_>>()),
        Err(_) => return,
    };

    // nadpis + tlačítko vpravo
    ui.add_space(12.0);
    let head = ui
        .horizontal(|ui| {
            ui.label(RichText::new(t!("Paměť WSL", "WSL memory")).size(17.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if app.mem.freeing {
                    ui.label(muted(t!("Uvolňuji…", "Freeing…")));
                    ui.spinner();
                } else if ui
                    .button(t!("\u{EC54} Uvolnit paměť", "\u{EC54} Free memory"))
                    .on_hover_text(t!(
                        "Zahodí cache Linuxu a zhutní paměť, aby ji VmmemWSL vrátil Windows.",
                        "Drops the Linux cache and compacts memory so VmmemWSL returns it to Windows."
                    ))
                    .clicked()
                {
                    free(app);
                }
                if ui
                    .button(t!("\u{E154} Týdenní souhrn", "\u{E154} Weekly summary"))
                    .on_hover_text(t!("Špičky a průměry za 7 dní z historie uložené na disku.", "Peaks and averages over 7 days from the history kept on disk."))
                    .clicked()
                {
                    app.mem.week_open = true;
                }
            });
        })
        .response;
    ui.add_space(4.0);
    if std::env::var_os("WLS_DEBUG_MEMORY").is_some() && now.is_some() && !DEBUG_DONE.swap(true, Ordering::Relaxed) {
        head.scroll_to_me(Some(egui::Align::TOP));
    }
    if std::mem::take(&mut app.mem.scroll_to) {
        head.scroll_to_me(Some(egui::Align::TOP));
    }

    // souhrn s pruhem a graf historie vedle sebe (v úzkém okně pod sebou)
    let last = app.mem.last.clone();
    let range = &mut app.mem.range;
    super::dashboard::two_tiles(
        ui,
        |ui| {
            card(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(super::dashboard::TILE_H);
                match &now {
                    Some(n) => summary(ui, &n.b),
                    None => {
                        ui.label(muted(t!("Měřím paměť…", "Measuring memory…")));
                    }
                }
                if let Some(r) = &last {
                    ui.add_space(6.0);
                    match r {
                        Ok(e) => {
                            ui.label(muted(free_text(e, now_secs())).small());
                        }
                        Err(e) => {
                            ui.label(RichText::new(tf!("Uvolnění selhalo: {e}", "Freeing failed: {e}")).small().color(theme::red()));
                        }
                    }
                }
            });
        },
        |ui| {
            card(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_min_height(super::dashboard::TILE_H);
                history(ui, &points, &events, range);
            });
        },
    );
    if app.mem.week_open {
        week_dialog(app, ui.ctx());
    }
}

fn now_secs() -> f64 {
    crate::memory::now_secs()
}

fn ago(t: f64, now: f64) -> String {
    let s = (now - t).max(0.0);
    if s < 60.0 {
        t!("právě teď", "just now").into()
    } else {
        tf!("před {:.0} min", "{:.0} min ago", (s / 60.0).floor())
    }
}

fn opt_bytes(v: Option<u64>) -> String {
    v.map(fmt_bytes).unwrap_or_else(|| "?".into())
}

/// „Ručně před 3 min: VmmemWSL 6.1 GB → 2.0 GB, cache 3.9 GB → 0.2 GB“
fn free_text(e: &FreeEvent, now: f64) -> String {
    let who = if e.auto { t!("Automaticky", "Automatically") } else { t!("Ručně", "Manually") };
    tf!(
        "\u{EC54} Poslední uvolnění – {who} {}: VmmemWSL {} → {}, cache {} → {}",
        "\u{EC54} Last freeing – {who} {}: VmmemWSL {} → {}, cache {} → {}",
        ago(e.t, now),
        opt_bytes(e.vmmem_before),
        opt_bytes(e.vmmem_after),
        fmt_bytes(e.cache_before),
        fmt_bytes(e.cache_after)
    )
}

/// Souhrn: velká čísla, pruh a legenda.
fn summary(ui: &mut egui::Ui, b: &Breakdown) {
    ui.horizontal_wrapped(|ui| {
        ui.label(muted("VmmemWSL"));
        ui.label(RichText::new(opt_bytes(b.vmmem)).size(22.0).strong());
        ui.add_space(8.0);
        let src = if b.limit_set { t!(".wslconfig", ".wslconfig") } else { t!("50 % RAM", "50 % of RAM") };
        ui.label(muted(tf!("limit {} ({src}) · RAM Windows {}", "limit {} ({src}) · Windows RAM {}", fmt_bytes(b.limit), fmt_bytes(b.ram))));
    });
    ui.add_space(6.0);
    bar(ui, b);
    ui.add_space(8.0);
    let items: [(Option<Color32>, &str, u64, &str); 6] = [
        (Some(c_containers()), t!("Kontejnery", "Containers"), b.containers, t!("Součet statistik kontejnerů (bez neaktivní cache).", "Sum of container stats (without inactive cache).")),
        (Some(c_engine()), t!("Engine", "Engine"), b.engine, t!("RSS procesů enginu (podman / dockerd, conmon, TLS proxy).", "RSS of engine processes (podman / dockerd, conmon, TLS proxy).")),
        (Some(c_rest()), t!("Zbytek Linuxu", "Rest of Linux"), b.rest, t!("Ostatní procesy distribuce a jádro.", "Other processes of the distribution and the kernel.")),
        (Some(c_cache()), t!("Cache souborů", "File cache"), b.cache, t!("Cached + Buffers + SReclaimable – uvolnitelná.", "Cached + Buffers + SReclaimable – can be freed.")),
        (Some(c_unreturned()), t!("Nevráceno Windows", "Not returned to Windows"), b.unreturned, t!("VmmemWSL − (použito + cache): Linux ji už nepotřebuje, ale VM ji zatím drží.", "VmmemWSL − (used + cache): Linux no longer needs it, but the VM still holds it.")),
        (None, t!("Volná v Linuxu", "Free in Linux"), b.free, t!("MemFree v /proc/meminfo.", "MemFree in /proc/meminfo.")),
    ];
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for (c, name, v, tip) in items {
            legend_item(ui, c, name, &fmt_bytes(v)).on_hover_text(tip);
        }
    });
}

/// Položka legendy jako JEDEN prvek (čtvereček + název + hodnota), aby se v zalamovaném
/// řádku zalomila celá – vnořené `ui.horizontal` egui nezalamuje a řádek by přetekl.
pub(super) fn legend_item(ui: &mut egui::Ui, c: Option<Color32>, name: &str, value: &str) -> egui::Response {
    use egui::text::{LayoutJob, TextFormat};
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut job = LayoutJob::default();
    job.append(name, 0.0, TextFormat { font_id: font.clone(), color: theme::muted(), ..Default::default() });
    job.append(value, 6.0, TextFormat { font_id: font, color: ui.visuals().strong_text_color(), ..Default::default() });
    let galley = ui.fonts(|f| f.layout_job(job));
    let sw = 11.0;
    let size = egui::vec2(sw + 6.0 + galley.size().x, galley.size().y.max(sw));
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let sq = egui::Rect::from_center_size(egui::pos2(rect.left() + sw / 2.0, rect.center().y), egui::vec2(sw, sw));
    match c {
        Some(c) => {
            ui.painter().rect_filled(sq, 2.0, c);
        }
        None => {
            ui.painter().rect_stroke(sq, 2.0, egui::Stroke::new(1.0_f32, theme::muted()), egui::StrokeKind::Inside);
        }
    }
    let pos = egui::pos2(rect.left() + sw + 6.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, theme::muted());
    resp
}

fn swatch(ui: &mut egui::Ui, c: Option<Color32>) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(11.0, 11.0), egui::Sense::hover());
    match c {
        Some(c) => {
            ui.painter().rect_filled(rect, 2.0, c);
        }
        None => {
            ui.painter().rect_stroke(rect, 2.0, egui::Stroke::new(1.0_f32, theme::muted()), egui::StrokeKind::Inside);
        }
    }
}

/// Skládaný pruh: kontejnery / engine / zbytek / cache / nevráceno; šířka = limit WSL
/// (nebo víc, když ho VmmemWSL přesahuje), svislá čárka = limit.
fn bar(ui: &mut egui::Ui, b: &Breakdown) {
    let w = ui.available_width().max(100.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 22.0), egui::Sense::hover());
    let p = ui.painter_at(rect.expand(2.0));
    p.rect_filled(rect, 4.0, theme::bg());
    let scale = b.scale() as f64;
    let parts = [
        (c_containers(), b.containers, t!("Kontejnery", "Containers")),
        (c_engine(), b.engine, t!("Engine", "Engine")),
        (c_rest(), b.rest, t!("Zbytek Linuxu", "Rest of Linux")),
        (c_cache(), b.cache, t!("Cache souborů", "File cache")),
        (c_unreturned(), b.unreturned, t!("Nevráceno Windows", "Not returned to Windows")),
    ];
    let mut x = rect.left();
    let mut hover: Option<(&str, u64)> = None;
    for (c, v, name) in parts {
        let pw = (v as f64 / scale) as f32 * rect.width();
        if pw <= 0.0 {
            continue;
        }
        let r = egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2((x + pw).min(rect.right()), rect.bottom()));
        p.rect_filled(r, 0.0, c);
        if resp.hover_pos().is_some_and(|h| r.contains(h)) {
            hover = Some((name, v));
        }
        x += pw;
    }
    p.rect_stroke(rect, 4.0, egui::Stroke::new(1.0_f32, theme::card_stroke()), egui::StrokeKind::Inside);
    // limit WSL, když je pruh širší (VmmemWSL nad limitem)
    if (b.limit as f64) < scale && b.limit > 0 {
        let lx = rect.left() + (b.limit as f64 / scale) as f32 * rect.width();
        p.line_segment([egui::pos2(lx, rect.top() - 2.0), egui::pos2(lx, rect.bottom() + 2.0)], egui::Stroke::new(2.0_f32, theme::strong_text()));
    }
    let tip = match hover {
        Some((name, v)) => format!("{name}: {} ({:.0} %)", fmt_bytes(v), v as f64 / b.limit.max(1) as f64 * 100.0),
        None => tf!("Celý pruh = limit WSL {}", "Whole bar = WSL limit {}", fmt_bytes(b.limit)),
    };
    resp.on_hover_text(tip);
}

/// Rozsahy grafu historie: délka okna (s) a popisek přepínače.
const RANGES: [(f64, &str); 4] = [(HISTORY_SECS, "1 h"), (86_400.0, "24 h"), (7.0 * 86_400.0, "7 d"), (30.0 * 86_400.0, "30 d")];

/// Body grafu z minutových záznamů (24 h).
fn raw_series(raw: &[Raw]) -> [Vec<(f64, f64)>; 3] {
    let pts = |f: &dyn Fn(&Raw) -> Option<u64>| raw.iter().filter_map(|r| f(r).map(|v| (r.t as f64, v as f64))).collect();
    [pts(&|r| r.vmmem), pts(&|r| r.used), pts(&|r| r.cache)]
}

/// Body grafu z 15minutových průměrů (7 a 30 dní), čas = střed bucketu.
fn agg_series(aggs: &[Agg]) -> [Vec<(f64, f64)>; 3] {
    let mid = AGG_STEP as f64 / 2.0;
    let pts = |f: &dyn Fn(&Agg) -> Option<f64>| aggs.iter().filter_map(|a| f(a).map(|v| (a.t as f64 + mid, v))).collect();
    [pts(&|a| a.vmmem.map(|m| m.avg)), pts(&|a| a.used.map(|m| m.avg)), pts(&|a| a.cache.map(|m| m.avg))]
}

/// Graf historie s vyznačeným uvolněním paměti; 1 h z paměti, delší rozsahy z disku.
fn history(ui: &mut egui::Ui, points: &[MemPoint], events: &[FreeEvent], range: &mut usize) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(t!("Historie", "History")).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (i, (_, label)) in RANGES.iter().enumerate().rev() {
                if ui.selectable_label(*range == i, *label).clicked() {
                    *range = i;
                }
            }
        });
    });
    ui.horizontal_wrapped(|ui| {
        for (c, name) in [
            (c_vmmem(), "VmmemWSL"),
            (c_rest(), t!("Linux použito", "Linux used")),
            (c_cache(), t!("Cache", "Cache")),
        ] {
            swatch(ui, Some(c));
            ui.label(muted(name).small());
            ui.add_space(6.0);
        }
    });
    ui.add_space(4.0);
    let now = now_secs();
    let range = (*range).min(RANGES.len() - 1);
    let window = RANGES[range].0;
    let store = crate::history::store();
    let stored_events;
    let (series_pts, events, gap) = match range {
        0 => {
            let vm: Vec<(f64, f64)> = points.iter().filter_map(|p| p.vmmem.map(|v| (p.t, v as f64))).collect();
            let used: Vec<(f64, f64)> = points.iter().map(|p| (p.t, p.used as f64)).collect();
            let cache: Vec<(f64, f64)> = points.iter().map(|p| (p.t, p.cache as f64)).collect();
            ([vm, used, cache], events, super::dashboard::CHART_GAP)
        }
        1 => {
            stored_events = store.events_since(now - window);
            (raw_series(&store.raw_since(now - window)), &stored_events[..], super::dashboard::CHART_GAP)
        }
        // značky uvolnění by u týdnů a měsíců graf zaplnily
        _ => (agg_series(&store.agg_since(now - window)), &[][..], 3.0 * AGG_STEP as f64),
    };
    let [vm, used, cache] = series_pts;
    let max = vm.iter().chain(&used).chain(&cache).map(|p| p.1).fold(0.0, f64::max);
    let ymax = (max * 1.15).max(256.0 * 1024.0 * 1024.0);
    let markers: Vec<Marker> = events
        .iter()
        .map(|e| Marker {
            t: e.t,
            text: tf!(
                "{} ({})\nVmmemWSL {} → {}\nCache {} → {}",
                "{} ({})\nVmmemWSL {} → {}\nCache {} → {}",
                if e.auto { t!("Automatické uvolnění", "Automatic freeing") } else { t!("Ruční uvolnění", "Manual freeing") },
                ago(e.t, now),
                opt_bytes(e.vmmem_before),
                opt_bytes(e.vmmem_after),
                fmt_bytes(e.cache_before),
                fmt_bytes(e.cache_after)
            ),
        })
        .collect();
    let series = [
        Series { name: "VmmemWSL", color: c_vmmem(), pts: &vm },
        Series { name: t!("Linux použito", "Linux used"), color: c_rest(), pts: &used },
        Series { name: t!("Cache", "Cache"), color: c_cache(), pts: &cache },
    ];
    line_chart_gaps(ui, &series, now, window, ymax, |v| fmt_bytes(v as u64), &markers, gap);
    ui.add_space(4.0);
    let note = match range {
        0 => t!(
            "Vzorek při každém obnovení, když je přehled otevřený, jinak jednou za minutu. \u{EC54} = uvolnění paměti.",
            "Sampled on every refresh while the overview is open, otherwise once a minute. \u{EC54} = memory freed."
        ),
        1 => t!(
            "Minutové průměry z historie na disku. Šedý pás = aplikace neběžela. \u{EC54} = uvolnění paměti.",
            "One-minute averages from the history on disk. Grey band = the app was not running. \u{EC54} = memory freed."
        ),
        _ => t!(
            "Průměry za 15 minut z historie na disku. Šedý pás = aplikace neběžela.",
            "15-minute averages from the history on disk. Grey band = the app was not running."
        ),
    };
    ui.label(muted(note).small());
    if range > 0 && !store.enabled() {
        ui.label(
            muted(t!(
                "Ukládání historie je v Nastavení vypnuté – jen data od spuštění aplikace.",
                "Keeping history on disk is turned off in Settings – only data since the app started."
            ))
            .small(),
        );
    }
}

/// Místní čas: (den v týdnu 0 = pondělí, den, měsíc, hodina, minuta).
fn local(t: f64) -> (usize, i64, i64, i64, i64) {
    let s = t as i64;
    let l = s + crate::logbuf::local_offset(s) as i64;
    let days = l.div_euclid(86_400);
    let (_, m, d) = crate::logbuf::civil_from_days(days);
    let sec = l.rem_euclid(86_400);
    ((days + 3).rem_euclid(7) as usize, d, m, sec / 3600, sec / 60 % 60)
}

fn fmt_day(t: f64) -> String {
    let (wd, d, m, _, _) = local(t);
    let cs = ["po", "út", "st", "čt", "pá", "so", "ne"];
    let en = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    if crate::i18n::is_en() {
        format!("{} {m}/{d}", en[wd])
    } else {
        format!("{} {d}. {m}.", cs[wd])
    }
}

fn fmt_hm(t: f64) -> String {
    let (_, _, _, h, mi) = local(t);
    format!("{h}:{mi:02}")
}

/// Rozdíl s označením: „+1.2 GB“, „−300.0 MB“, „±0“.
fn fmt_delta(from: u64, to: u64) -> String {
    match to.cmp(&from) {
        std::cmp::Ordering::Greater => format!("+{}", fmt_bytes(to - from)),
        std::cmp::Ordering::Less => format!("−{}", fmt_bytes(from - to)),
        std::cmp::Ordering::Equal => "±0".into(),
    }
}

/// Řádky týdenního souhrnu (název, hodnota).
fn week_rows(s: &crate::history::Summary, now: f64) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let pa = |p: crate::history::PeakAvg| tf!("špička {} · průměr {}", "peak {} · average {}", fmt_bytes(p.peak as u64), fmt_bytes(p.avg as u64));
    if let Some(p) = s.vmmem {
        rows.push(("VmmemWSL".to_string(), pa(p)));
    }
    if let Some(p) = s.used {
        rows.push((t!("Linux použito", "Linux used").to_string(), pa(p)));
    }
    if let Some(p) = s.cpu {
        rows.push((t!("CPU kontejnerů", "Container CPU").to_string(), tf!("špička {:.0} % · průměr {:.1} %", "peak {:.0} % · average {:.1} %", p.peak, p.avg)));
    }
    rows.push((
        t!("Uvolnění paměti", "Memory freed").to_string(),
        tf!(
            "{}× automaticky, {}× ručně · vráceno {}",
            "{}× automatic, {}× manual · returned {}",
            s.frees_auto,
            s.frees_manual,
            fmt_bytes(s.freed)
        ),
    ));
    let span = super::chart::fmt_age(now - s.from.unwrap_or(now));
    if let Some((a, b)) = s.engine {
        rows.push((t!("Úložiště enginu", "Engine storage").to_string(), tf!("{} ({} za {span})", "{} ({} in {span})", fmt_bytes(b), fmt_delta(a, b))));
    }
    if let Some((a, b)) = s.disk_used {
        let free = s.disk_free.map(|f| tf!(" · volné {}", " · free {}", fmt_bytes(f))).unwrap_or_default();
        rows.push((t!("Disk distribuce", "Distribution disk").to_string(), tf!("použito {} ({}){free}", "used {} ({}){free}", fmt_bytes(b), fmt_delta(a, b))));
    }
    if let Some((rx, tx)) = s.net {
        rows.push((t!("Provoz VM (eth0)", "VM traffic (eth0)").to_string(), tf!("přijato {} · odesláno {}", "received {} · sent {}", fmt_bytes(rx), fmt_bytes(tx))));
    }
    if let Some((h, cpu)) = s.busiest {
        rows.push((
            t!("Nejvytíženější hodina", "Busiest hour").to_string(),
            format!("{} {}–{} · CPU {cpu:.0} %", fmt_day(h), fmt_hm(h), fmt_hm(h + 3600.0)),
        ));
    }
    rows
}

/// Dialog s týdenním souhrnem.
fn week_dialog(app: &mut App, ctx: &egui::Context) {
    let now = now_secs();
    let s = crate::history::store().summary(now);
    let mut close = false;
    let (_, esc) = modal(ctx, "week-summary", t!("Týdenní souhrn", "Weekly summary"), 480.0, |ui| {
        match s.from {
            Some(from) if s.enough => {
                ui.label(muted(tf!("Posledních 7 dní · data od {} {}", "Last 7 days · data since {} {}", fmt_day(from), fmt_hm(from))));
                ui.add_space(8.0);
                egui::Grid::new("week-summary-grid").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                    for (name, value) in week_rows(&s, now) {
                        ui.label(muted(name));
                        ui.label(RichText::new(value).color(theme::strong_text()));
                        ui.end_row();
                    }
                });
            }
            _ => {
                ui.label(t!(
                    "Zatím málo dat – souhrn bude k dispozici zhruba po dni provozu aplikace.",
                    "Not enough data yet – the summary appears after about a day of running the app."
                ));
                if let Some(from) = s.from {
                    ui.label(muted(tf!("Historie od {} {}", "History since {} {}", fmt_day(from), fmt_hm(from))).small());
                }
            }
        }
        if !crate::history::store().enabled() {
            ui.add_space(6.0);
            ui.label(
                muted(t!(
                    "Ukládání historie na disk je v Nastavení vypnuté.",
                    "Keeping history on disk is turned off in Settings."
                ))
                .small(),
            );
        }
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = ui.button(t!("Zavřít", "Close")).clicked();
            });
        });
    });
    if close || esc {
        app.mem.week_open = false;
    }
}

/// Sekce Nastavení: ukládání historie zdrojů a její smazání.
pub fn history_settings(app: &mut App, ui: &mut egui::Ui) {
    section(ui, t!("Historie zdrojů", "Resource history"));
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let mut c = app.cfg.read().map(|c| c.clone()).unwrap_or_default();
        if ui
            .checkbox(&mut c.history_on_disk, t!("Ukládat historii zdrojů na disk", "Keep resource history on disk"))
            .on_hover_text(t!(
                "Paměť WSL, CPU kontejnerů, provoz VM a místo na disku – grafy a týdenní souhrn pak přežijí restart aplikace.",
                "WSL memory, container CPU, VM traffic and disk space – charts and the weekly summary then survive app restarts."
            ))
            .changed()
        {
            crate::history::store().set_enabled(c.history_on_disk);
            let _ = c.save();
            app.cfg_draft.history_on_disk = c.history_on_disk;
            if let Ok(mut g) = app.cfg.write() {
                *g = c;
            }
        }
        let dir = crate::history::dir().map(|d| d.display().to_string()).unwrap_or_default();
        ui.label(
            muted(tf!(
                "Minutové průměry 48 hodin, 15minutové průměry 35 dní (celkem pod 1 MB). Zapisuje se jednou za 5 minut a při ukončení do {dir}.",
                "One-minute averages for 48 hours, 15-minute averages for 35 days (under 1 MB in total). Written every 5 minutes and on exit to {dir}."
            ))
            .small(),
        );
        ui.add_space(6.0);
        if danger_button(ui, t!("\u{E4A6} Smazat historii…", "\u{E4A6} Delete history…")).clicked() {
            app.ask(
                t!("Smazat historii zdrojů?", "Delete resource history?"),
                t!(
                    "Uložená historie paměti, CPU, provozu a disku se smaže. Grafy delší než hodina a týdenní souhrn začnou znovu od nuly.",
                    "The stored memory, CPU, traffic and disk history will be deleted. Charts longer than an hour and the weekly summary start again from zero."
                ),
                Cmd::ClearHistory,
            );
        }
    });
}
