//! Přehled paměti WSL na stránce Přehled: kam jde RAM, kterou drží VmmemWSL.

use super::chart::{line_chart, Marker, Series};
use super::*;
use crate::app::App;
use crate::memory::{Breakdown, FreeEvent, MemNow, MemPoint, HISTORY_SECS};
use crate::worker::Cmd;
use std::sync::atomic::{AtomicBool, Ordering};

/// Stav sekce v UI (zbytek je ve `Shared::memory`, zapisuje worker).
#[derive(Default)]
pub struct MemUi {
    /// běží ruční uvolnění paměti
    pub freeing: bool,
    /// poslední výsledek uvolnění (i automatického)
    pub last: Option<Result<FreeEvent, String>>,
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
            });
        })
        .response;
    ui.add_space(4.0);
    if std::env::var_os("WLS_DEBUG_MEMORY").is_some() && now.is_some() && !DEBUG_DONE.swap(true, Ordering::Relaxed) {
        head.scroll_to_me(Some(egui::Align::TOP));
    }

    card(ui, |ui| {
        ui.set_width(ui.available_width());
        match &now {
            Some(n) => summary(ui, &n.b),
            None => {
                ui.label(muted(t!("Měřím paměť…", "Measuring memory…")));
            }
        }
        if let Some(r) = &app.mem.last {
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

    ui.add_space(8.0);
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        history(ui, &points, &events);
    });

    if let Some(n) = &now {
        if !n.containers.is_empty() {
            ui.add_space(8.0);
            mem_table(app, ui, n);
        }
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
fn legend_item(ui: &mut egui::Ui, c: Option<Color32>, name: &str, value: &str) -> egui::Response {
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

/// Graf za poslední hodinu s vyznačeným uvolněním paměti.
fn history(ui: &mut egui::Ui, points: &[MemPoint], events: &[FreeEvent]) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(t!("Historie – poslední hodina", "History – last hour")).strong());
        ui.add_space(8.0);
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
    let vm: Vec<(f64, f64)> = points.iter().filter_map(|p| p.vmmem.map(|v| (p.t, v as f64))).collect();
    let used: Vec<(f64, f64)> = points.iter().map(|p| (p.t, p.used as f64)).collect();
    let cache: Vec<(f64, f64)> = points.iter().map(|p| (p.t, p.cache as f64)).collect();
    let max = vm.iter().chain(&used).chain(&cache).map(|p| p.1).fold(0.0, f64::max);
    let ymax = (max * 1.15).max(256.0 * 1024.0 * 1024.0);
    let now = now_secs();
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
    line_chart(ui, &series, now, HISTORY_SECS, ymax, |v| fmt_bytes(v as u64), &markers);
    ui.add_space(4.0);
    ui.label(
        muted(t!(
            "Vzorek při každém obnovení, když je přehled otevřený, jinak jednou za minutu. \u{EC54} = uvolnění paměti.",
            "Sampled on every refresh while the overview is open, otherwise once a minute. \u{EC54} = memory freed."
        ))
        .small(),
    );
}

/// Řádek tabulky: stack (součet jeho kontejnerů) nebo samostatný kontejner.
struct Row {
    name: String,
    /// stack: počet kontejnerů; kontejner: 0
    members: usize,
    sub: String,
    mem: u64,
}

/// Seskupí kontejnery podle compose projektu.
fn rows(app: &App, n: &MemNow) -> Vec<Row> {
    let mut stacks: std::collections::BTreeMap<String, (u64, Vec<(String, u64)>)> = Default::default();
    let mut out = Vec::new();
    for (id, name, mem) in &n.containers {
        let c = app.snap.containers.iter().find(|c| &c.id == id);
        match c.and_then(|c| c.project()) {
            Some(p) => {
                let e = stacks.entry(p.to_string()).or_default();
                e.0 += mem;
                e.1.push((name.clone(), *mem));
            }
            None => out.push(Row {
                name: name.clone(),
                members: 0,
                sub: c.map(|c| crate::engine::short_image(&c.image)).unwrap_or_default(),
                mem: *mem,
            }),
        }
    }
    for (p, (mem, mut members)) in stacks {
        members.sort_by_key(|m| std::cmp::Reverse(m.1));
        let sub = members.iter().map(|(n, m)| format!("{n} {}", fmt_bytes(*m))).collect::<Vec<_>>().join(" · ");
        out.push(Row { name: p, members: members.len(), sub, mem });
    }
    out
}

fn mem_table(app: &App, ui: &mut egui::Ui, n: &MemNow) {
    let mut rows = rows(app, n);
    rows.sort_by_key(|r| std::cmp::Reverse(r.mem));
    sort_items(ui.ctx(), "mem", &mut rows, |r, col| match col {
        1 => SortKey::num(r.members as f64),
        2 | 3 => SortKey::num(r.mem as f64),
        _ => SortKey::text(&r.name),
    });
    let used = n.b.used.max(1) as f64;
    let cols = [
        (t!("STACK / KONTEJNER", "STACK / CONTAINER"), W::Fr(2.4)),
        (t!("TYP", "TYPE"), W::PxOpt(110.0)),
        (t!("PAMĚŤ", "MEMORY"), W::Fr(1.4)),
        (t!("Z LINUXU", "OF LINUX"), W::Px(80.0)),
    ];
    table_sort(ui, "mem", &cols, Sort { key: "mem", cols: &[0, 1, 2, 3] }, |mut body| {
        for r in &rows {
            body.row(42.0, |mut row| {
                row.col(|ui| {
                    two_line(ui, RichText::new(&r.name).strong(), muted(&r.sub).small());
                });
                row.col(|ui| {
                    if r.members > 0 {
                        ui.label(muted(tf!("\u{E466} stack ({})", "\u{E466} stack ({})", r.members)));
                    } else {
                        ui.label(muted(t!("\u{E1DA} kontejner", "\u{E1DA} container")));
                    }
                });
                row.col(|ui| {
                    ui.vertical(|ui| {
                        ui.label(fmt_bytes(r.mem));
                        ui.add(
                            egui::ProgressBar::new((r.mem as f64 / used) as f32)
                                .desired_width(ui.available_width() - 12.0)
                                .desired_height(4.0)
                                .fill(c_containers()),
                        );
                    });
                });
                row.col(|ui| {
                    ui.label(format!("{:.1} %", r.mem as f64 / used * 100.0));
                });
            });
        }
    });
    ui.add_space(2.0);
    ui.label(
        muted(t!(
            "Paměť kontejnerů = statistiky enginu bez neaktivní cache souborů; podíl z paměti použité procesy v Linuxu.",
            "Container memory = engine stats without inactive file cache; share of memory used by processes in Linux."
        ))
        .small(),
    );
}
