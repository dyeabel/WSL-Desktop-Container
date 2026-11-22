use super::*;
use crate::app::App;
use crate::worker::Cmd;

const CARD_H: f32 = 96.0;

/// Karta se statistikou – všechny mají stejnou výšku, dlouhý text se ořízne.
fn stat_card(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    card(ui, |ui| {
        ui.vertical(|ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            ui.set_width(ui.available_width());
            ui.set_height(CARD_H);
            add(ui);
        });
    });
}

struct Stat {
    title: String,
    big: String,
    sub: String,
    dot: Option<egui::Color32>,
}

fn stat(ui: &mut egui::Ui, s: &Stat) {
    stat_card(ui, |ui| {
        ui.horizontal(|ui| {
            if let Some(c) = s.dot {
                dot(ui, c);
            }
            ui.label(muted(&s.title));
        });
        ui.label(RichText::new(&s.big).size(32.0).strong());
        ui.label(muted(&s.sub).small()).on_hover_text(&s.sub);
    });
}

/// Minimální šířka karty – při užším okně se karty přeskládají do více řádků.
const CARD_MIN_W: f32 = 190.0;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(
        ui,
        t!("Přehled", "Overview"),
        t!("Stav kontejnerového enginu ve WSL a výkon kontejnerů", "Container engine status in WSL and container performance"),
    );
    let ename = app.cfg.read().map(|c| c.engine.name()).unwrap_or("Engine");

    let distro = app.distro();
    let dstate = app.current_distro().map(|d| d.state.clone()).unwrap_or_else(|| t!("neznámý", "unknown").into());
    let ver = app.snap.info.as_ref().map(|i| format!(" · {ename} {}", i.server_version)).unwrap_or_default();
    let mut stats = vec![Stat {
        title: ename.into(),
        big: if app.snap.connected { t!("Běží", "Running") } else { t!("Zastaven", "Stopped") }.into(),
        sub: format!("{distro} ({dstate}){ver}"),
        dot: Some(if app.snap.connected { theme::green() } else { theme::grey() }),
    }];
    if let Some(i) = &app.snap.info {
        let s = |t: &str, b: String, sub: String| Stat { title: t.into(), big: b, sub, dot: None };
        stats.push(s(
            t!("Kontejnery", "Containers"),
            i.containers_running.to_string(),
            tf!("běží z {} celkem", "running of {} total", i.containers),
        ));
        stats.push(s(
            t!("Image", "Images"),
            app.snap.images.len().to_string(),
            fmt_bytes(app.snap.images.iter().map(|i| i.size.max(0) as u64).sum()),
        ));
        stats.push(s(t!("Volume", "Volumes"), app.snap.volumes.len().to_string(), String::new()));
        stats.push(s(t!("Zdroje WSL", "WSL resources"), format!("{} CPU", i.ncpu), fmt_bytes(i.mem_total.max(0) as u64) + " RAM"));
    }
    // počet sloupců podle šířky okna, karty v řádku mají stejnou šířku
    let per_row = ((ui.available_width() / CARD_MIN_W).floor() as usize).clamp(1, stats.len().max(1));
    for chunk in stats.chunks(per_row) {
        ui.columns(per_row, |cols| {
            for (col, s) in cols.iter_mut().zip(chunk) {
                stat(col, s);
            }
        });
        ui.add_space(4.0);
    }

    // aktivní upozornění na paměť a disk (i bez enginu)
    super::alerts::banner(app, ui);

    if !need_engine(app, ui) {
        // paměť WSL se měří i bez enginu
        if app.caps().vm_memory {
            super::memory::show(app, ui);
        } else {
            host_memory(app, ui);
        }
        return;
    }

    // CPU a disk jako dlaždice s grafem / pruhem
    ui.add_space(8.0);
    let mut pick = None;
    let wide = ui.available_width() >= TWO_COL_MIN;
    if wide {
        ui.columns(2, |c| {
            let (l, r) = c.split_at_mut(1);
            cpu_tile(app, &mut l[0]);
            pick = disk_tile(app, &mut r[0], true);
        });
    } else {
        cpu_tile(app, ui);
        ui.add_space(8.0);
        pick = disk_tile(app, ui, false);
    }
    cleanup_apply(app, pick);

    if app.caps().vm_memory {
        super::memory::show(app, ui);
    } else {
        host_memory(app, ui);
    }
    containers_perf(app, ui);
}

/// Paměť hostitele bez VM (Linux): /proc/meminfo a součet kontejnerů, bez VmmemWSL.
fn host_memory(app: &App, ui: &mut egui::Ui) {
    let (glance, now) = app.shared.memory.lock().map(|m| (m.glance, m.now.clone())).unwrap_or_default();
    section(ui, t!("Paměť", "Memory"));
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let Some(g) = glance else {
            ui.label(muted(t!("načítám…", "loading…")));
            return;
        };
        let pct = |v: u64| v as f64 * 100.0 / g.total.max(1) as f64;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(tf!("Použito {} z {}", "Used {} of {}", fmt_bytes(g.used), fmt_bytes(g.total))).strong());
            ui.label(muted(format!("({:.0} %)", pct(g.used))));
            ui.label(muted("·"));
            ui.label(muted(tf!("cache {}", "cache {}", fmt_bytes(g.cache))));
            if let Some(n) = &now {
                ui.label(muted("·"));
                ui.label(muted(tf!("kontejnery {}", "containers {}", fmt_bytes(n.b.containers))));
                ui.label(muted("·"));
                ui.label(muted(tf!("engine {}", "engine {}", fmt_bytes(n.b.engine))));
            }
        });
        let bar = egui::ProgressBar::new((pct(g.used) / 100.0).clamp(0.0, 1.0) as f32).desired_height(8.0);
        ui.add(bar);
        ui.label(muted(t!("Paměť tohoto počítače (/proc/meminfo).", "Memory of this computer (/proc/meminfo).")).small());
    });
}

/// Tlačítko s nabídkou „Uklidit“ (přehled disku a pruh upozornění); vrací vybranou položku.
pub fn cleanup_menu(ui: &mut egui::Ui, vm_memory: bool) -> Option<u8> {
    let mut pick = None;
    ui.menu_button(t!("\u{EC54} Uklidit \u{E136}", "\u{EC54} Clean up \u{E136}"), |ui| {
        ui.set_min_width(260.0);
        for (i, label) in [
            t!("Odstranit zastavené kontejnery…", "Remove stopped containers…"),
            t!("Odstranit visící image", "Remove dangling images"),
            t!("Odstranit nepoužívané image…", "Remove unused images…"),
            t!("Odstranit nepoužívané volume…", "Remove unused volumes…"),
            t!("Odstranit nepoužívané sítě", "Remove unused networks"),
            t!("Uvolnit paměť WSL (cache)", "Free WSL memory (cache)"),
        ]
        .iter()
        .enumerate()
        {
            // položka 5 (uvolnění paměti WSL) jen tam, kde hostitel umí vm_memory
            if (i != 5 || vm_memory) && ui.button(*label).clicked() {
                pick = Some(i as u8);
                ui.close_menu();
            }
        }
    });
    pick
}

/// Provede položku z `cleanup_menu` (mazání s potvrzením).
pub fn cleanup_apply(app: &mut App, pick: Option<u8>) {
    match pick {
        Some(0) => app.ask(
            t!("Odstranit zastavené kontejnery?", "Remove stopped containers?"),
            t!("Všechny zastavené kontejnery budou smazány.", "All stopped containers will be deleted."),
            Cmd::PruneContainers,
        ),
        Some(1) => app.send(Cmd::PruneImages { all: false }),
        Some(5) => super::memory::free(app),
        Some(2) => app.ask(
            t!("Odstranit nepoužívané image?", "Remove unused images?"),
            t!("Smažou se všechny image, které nepoužívá žádný kontejner.", "All images not used by any container will be deleted."),
            Cmd::PruneImages { all: true },
        ),
        Some(3) => app.ask(
            t!("Odstranit nepoužívané volume?", "Remove unused volumes?"),
            t!("Data v nepoužívaných volume budou nenávratně smazána!", "Data in unused volumes will be permanently deleted!"),
            Cmd::PruneVolumes,
        ),
        Some(4) => app.send(Cmd::PruneNetworks),
        _ => {}
    }
}

fn containers_perf(app: &mut App, ui: &mut egui::Ui) {
    section(ui, t!("Výkon běžících kontejnerů", "Running container performance"));
    if !app.snap.containers.iter().any(|c| c.running()) {
        ui.label(muted(t!("Žádný kontejner neběží.", "No container is running.")));
        return;
    }
    if app.stats.is_empty() {
        ui.label(muted(t!(
            "Načítám statistiky… (běžící kontejnery se měří cca 1–2 s)",
            "Loading statistics… (measuring running containers takes about 1–2 s)"
        )));
        return;
    }
    let snap = app.snap.clone();
    let mut rows: Vec<&crate::docker::StatRow> = app.stats.iter().collect();
    rows.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal));
    sort_items(ui.ctx(), "stats", &mut rows, |r, col| match col {
        1 => SortKey::num(r.cpu),
        2 => SortKey::num(r.mem as f64),
        3 => SortKey::num((r.net_rx + r.net_tx) as f64),
        4 => SortKey::num((r.blk_read + r.blk_write) as f64),
        _ => SortKey::text(&r.name),
    });
    let ncpu = app.snap.info.as_ref().map(|i| i.ncpu.max(1)).unwrap_or(1) as f64;
    ui.add_space(8.0);
    let cols = [
        (t!("NÁZEV", "NAME"), W::Fr(2.0)),
        ("CPU", W::Fr(1.2)),
        (t!("PAMĚŤ", "MEMORY"), W::Fr(1.6)),
        (t!("SÍŤ (rx / tx)", "NETWORK (rx / tx)"), W::FrOpt(1.4)),
        (t!("DISK (čtení / zápis)", "DISK (read / write)"), W::FrOpt(1.4)),
    ];
    table_sort(ui, "stats", &cols, Sort { key: "stats", cols: &[0, 1, 2, 3, 4] }, |mut body| {
        for r in &rows {
            body.row(42.0, |mut row| {
                row.col(|ui| {
                    // název + image, ze kterého kontejner běží (a compose stack, pokud patří do něj)
                    let sub = snap
                        .containers
                        .iter()
                        .find(|c| c.id == r.id)
                        .map(|c| {
                            let img = crate::engine::short_image(&c.image);
                            match c.project() {
                                Some(p) => format!("{img} · \u{E466} {p}"),
                                None => img,
                            }
                        })
                        .unwrap_or_default();
                    two_line(ui, RichText::new(&r.name).strong(), muted(&sub).small());
                });
                // limity kontejneru (CPU limit na všechna jádra nic neomezuje)
                let lim_cpus = Some(r.lim_cpus).filter(|c| *c > 0.0 && *c < ncpu - 0.005);
                row.col(|ui| {
                    stack_v(ui, |ui| {
                        let pct = lim_cpus.and_then(|c| crate::limits::pct_of(r.cpu, c * 100.0));
                        ui.horizontal(|ui| {
                            match (pct, lim_cpus) {
                                (Some(p), Some(c)) => {
                                    ui.label(tf!("{:.0} % z {} CPU", "{:.0} % of {} CPU", p, crate::limits::fmt_cpus(c)))
                                        .on_hover_text(tf!("{:.2} % jednoho jádra", "{:.2} % of one core", r.cpu));
                                }
                                _ => {
                                    ui.label(format!("{:.2} %", r.cpu));
                                }
                            }
                            near_limit(ui, pct);
                        });
                        let f = match lim_cpus {
                            Some(c) => r.cpu / (c * 100.0),
                            None => r.cpu / (ncpu * 100.0),
                        };
                        ui.add(egui::ProgressBar::new(f as f32).desired_width(ui.available_width() - 12.0).desired_height(4.0).fill(theme::accent()));
                    });
                });
                row.col(|ui| {
                    stack_v(ui, |ui| {
                        let limit = if r.lim_mem > 0 { r.lim_mem } else { r.mem_limit };
                        let pct = (r.lim_mem > 0).then(|| crate::limits::pct_of(r.mem as f64, r.lim_mem as f64)).flatten();
                        ui.horizontal(|ui| {
                            if r.lim_mem > 0 {
                                ui.label(format!("{} / {}", fmt_bytes(r.mem), fmt_bytes(r.lim_mem))).on_hover_text(t!("Limit kontejneru", "Container limit"));
                            } else {
                                ui.label(tf!("{} / {} (WSL)", "{} / {} (WSL)", fmt_bytes(r.mem), fmt_bytes(r.mem_limit)))
                                    .on_hover_text(t!("Kontejner nemá limit – platí limit WSL", "No container limit – the WSL limit applies"));
                            }
                            near_limit(ui, pct);
                        });
                        let f = if limit > 0 { r.mem as f32 / limit as f32 } else { 0.0 };
                        ui.add(egui::ProgressBar::new(f).desired_width(ui.available_width() - 12.0).desired_height(4.0).fill(theme::accent()));
                    });
                });
                row.col(|ui| {
                    ui.label(RichText::new(format!("{} / {}", fmt_bytes(r.net_rx), fmt_bytes(r.net_tx))).monospace());
                });
                row.col(|ui| {
                    ui.label(RichText::new(format!("{} / {}", fmt_bytes(r.blk_read), fmt_bytes(r.blk_write))).monospace());
                });
            });
        }
    });
}

/// Štítek „u limitu“ při využití nad 90 % limitu kontejneru.
fn near_limit(ui: &mut egui::Ui, pct: Option<f64>) {
    if let Some(p) = pct.filter(|p| *p >= crate::limits::NEAR_LIMIT_PCT) {
        let c = if p >= 100.0 { theme::red() } else { theme::yellow() };
        badge(ui, &tf!("\u{E4E0} {:.0} %", "\u{E4E0} {:.0} %", p), c).on_hover_text(t!("Využití blízko limitu kontejneru", "Usage close to the container limit"));
    }
}

/// Výška dlaždic na přehledu (dvojice vedle sebe mají stejnou výšku).
pub const TILE_H: f32 = 236.0;
/// Od této šířky jsou dlaždice dvě vedle sebe.
pub const TWO_COL_MIN: f32 = 860.0;
/// Jak dlouhou historii CPU držet (s).
pub const CPU_WINDOW: f64 = 900.0;
/// Body grafů dál od sebe se nespojí (aplikace neběžela / přehled nebyl otevřený).
pub const CHART_GAP: f64 = 180.0;

/// Dvě dlaždice vedle sebe (v úzkém okně pod sebou).
pub fn two_tiles(ui: &mut egui::Ui, a: impl FnOnce(&mut egui::Ui), b: impl FnOnce(&mut egui::Ui)) {
    if ui.available_width() >= TWO_COL_MIN {
        ui.columns(2, |c| {
            a(&mut c[0]);
            b(&mut c[1]);
        });
    } else {
        a(ui);
        ui.add_space(8.0);
        b(ui);
    }
}

/// Přidá vzorek celkového CPU (% všech CPU WSL) a zahodí starší než CPU_WINDOW.
pub fn push_cpu(h: &mut std::collections::VecDeque<(f64, f64)>, pct: f64) {
    // -0.0 (prázdný součet) a NaN jako 0 – jinak dlaždice ukáže „-0.0 %“
    let pct = if pct > 0.0 { pct } else { 0.0 };
    let now = crate::memory::now_secs();
    h.push_back((now, pct));
    while h.front().is_some_and(|p| now - p.0 > CPU_WINDOW) {
        h.pop_front();
    }
}

/// Záhlaví dlaždice: popisek, velké číslo, šedý doplněk. Řádek s popiskem má výšku tlačítka,
/// aby seděl s dlaždicí, která má vpravo nabídku (Disk → Uklidit).
fn tile_head(ui: &mut egui::Ui, title: &str, big: &str, sub: &str) {
    ui.horizontal(|ui| {
        ui.set_min_height(theme::CONTROL_H);
        ui.label(muted(title));
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(big).size(24.0).strong());
        ui.label(muted(sub));
    });
    ui.add_space(4.0);
}

/// Doplní CPU z uložené historie před body, které už graf má (jednou po spuštění).
fn cpu_preload(h: &mut std::collections::VecDeque<(f64, f64)>) {
    let Some(pts) = crate::history::store().take_cpu_preload() else { return };
    let first = h.front().map(|p| p.0).unwrap_or(f64::INFINITY);
    for p in pts.into_iter().rev().filter(|p| p.0 < first) {
        h.push_front(p);
    }
}

fn cpu_tile(app: &mut App, ui: &mut egui::Ui) {
    cpu_preload(&mut app.cpu_hist);
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.set_min_height(TILE_H);
        let ncpu = app.snap.info.as_ref().map(|i| i.ncpu.max(1)).unwrap_or(1);
        let containers = app.cpu_hist.back().map(|p| p.1).unwrap_or(0.0);
        // velké číslo = celá WSL VM (jako karta enginu); kontejnery jsou jeho část – rozdíl je
        // engine (API, TLS proxy), Kubernetes mimo kontejnery a systém
        let vm = app.vm_cpu_hist.back().map(|p| p.1);
        let (title, big) = match vm {
            Some(v) => (t!("CPU WSL", "WSL CPU"), format!("{v:.1} %")),
            None => (t!("CPU kontejnerů", "Container CPU"), format!("{containers:.1} %")),
        };
        let sub = match vm {
            Some(_) => tf!("z {ncpu} CPU · kontejnery {containers:.1} %", "of {ncpu} CPUs · containers {containers:.1} %"),
            None => tf!("z {ncpu} CPU ve WSL", "of {ncpu} CPUs in WSL"),
        };
        tile_head(ui, title, &big, &sub);
        let pts: Vec<(f64, f64)> = app.cpu_hist.iter().copied().collect();
        let vm_pts: Vec<(f64, f64)> = app.vm_cpu_hist.iter().copied().collect();
        let max = pts.iter().chain(vm_pts.iter()).map(|p| p.1).fold(0.0, f64::max);
        let ymax = (max * 1.3).clamp(5.0, 100.0);
        let (vm_name, ct_name) = (t!("celá WSL VM", "whole WSL VM"), t!("kontejnery", "containers"));
        let mut series = vec![];
        if !vm_pts.is_empty() {
            series.push(super::chart::Series { name: vm_name, color: theme::accent(), pts: &vm_pts });
        }
        series.push(super::chart::Series { name: ct_name, color: theme::hue(0.57), pts: &pts });
        super::chart::line_chart_gaps(ui, &series, crate::memory::now_secs(), CPU_WINDOW, ymax, |v| format!("{v:.0} %"), &[], CHART_GAP);
        if !vm_pts.is_empty() {
            ui.horizontal(|ui| {
                for s in &series {
                    ui.label(RichText::new("\u{25CF}").color(s.color).small());
                    ui.label(muted(s.name).small());
                    ui.add_space(8.0);
                }
            });
        }
    });
}

/// Dlaždice Disk: úložiště enginu proti disku distribuce; vrací výběr z nabídky Uklidit.
/// `side_by_side` = vedle dlaždice CPU (stejná výška); pod ní jen tak vysoká, jak je obsah.
fn disk_tile(app: &mut App, ui: &mut egui::Ui, side_by_side: bool) -> Option<u8> {
    let disk = app.shared.alerts.lock().ok().and_then(|a| a.disk.clone());
    let mut pick = None;
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        if side_by_side {
            ui.set_min_height(TILE_H);
        }
        ui.horizontal(|ui| {
            ui.label(muted(t!("Disk", "Disk")));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                pick = cleanup_menu(ui, app.caps().vm_memory);
            });
        });
        let Some(df) = app.df.clone() else {
            ui.label(muted(t!("Zjišťuji využití disku…", "Measuring disk usage…")));
            return;
        };
        let v = |x: i64| x.max(0) as u64;
        let engine = v(df.images) + v(df.containers) + v(df.volumes) + v(df.build_cache);
        let inside = disk.as_ref().and_then(|d| d.inside.clone());
        let sub = match &inside {
            Some(i) => tf!(
                "engine · disk distribuce {} z {}, volné {} %",
                "engine · distribution disk {} of {}, {} % free",
                fmt_bytes(i.used),
                fmt_bytes(i.size),
                (i.avail as f64 / i.size.max(1) as f64 * 100.0).round()
            ),
            None => t!("úložiště enginu", "engine storage").into(),
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(fmt_bytes(engine)).size(24.0).strong());
            ui.label(muted(sub));
        });
        ui.add_space(6.0);
        let other = inside.as_ref().map(|i| i.used.saturating_sub(engine)).unwrap_or(0);
        let free = inside.as_ref().map(|i| i.avail).unwrap_or(0);
        let parts: [(Option<Color32>, &str, u64); 6] = [
            (Some(theme::accent()), t!("Image", "Images"), v(df.images)),
            (Some(theme::hue(0.58)), t!("Kontejnery", "Containers"), v(df.containers)),
            (Some(theme::hue(0.78)), t!("Volume", "Volumes"), v(df.volumes)),
            (Some(theme::red()), "Build cache", v(df.build_cache)),
            (Some(theme::green()), t!("Ostatní data", "Other data"), other),
            (None, t!("Volné", "Free"), free),
        ];
        stacked_bar(ui, &parts);
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            for (c, name, val) in parts {
                if c.is_some() || inside.is_some() {
                    super::memory::legend_item(ui, c, name, &fmt_bytes(val));
                }
            }
        });
        if let Some(d) = &disk {
            ui.add_space(6.0);
            let mut extra = Vec::new();
            if let Some((_, size)) = d.vhdx.as_ref().filter(|_| app.caps().vm_disk) {
                extra.push(tf!("VHDX {}", "VHDX {}", fmt_bytes(*size)));
            }
            if let Some(w) = &d.drive {
                extra.push(tf!("disk {} volné {} z {}", "drive {} free {} of {}", w.name, fmt_bytes(w.free), fmt_bytes(w.total)));
            }
            if !extra.is_empty() {
                ui.label(muted(extra.join(" · ")).small());
            }
        }
    });
    pick
}

/// Vodorovný pruh poskládaný z částí; část bez barvy (volné místo) zůstane prázdná.
fn stacked_bar(ui: &mut egui::Ui, parts: &[(Option<Color32>, &str, u64)]) {
    let w = ui.available_width().max(100.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 22.0), egui::Sense::hover());
    let p = ui.painter_at(rect.expand(2.0));
    p.rect_filled(rect, 4.0, theme::bg());
    let total: u64 = parts.iter().map(|x| x.2).sum::<u64>().max(1);
    let mut x = rect.left();
    let mut tip = String::new();
    for (c, name, val) in parts {
        let pw = (*val as f64 / total as f64 * rect.width() as f64) as f32;
        if let Some(c) = c {
            if pw >= 0.5 {
                p.rect_filled(egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(pw, rect.height())), 0.0, *c);
            }
        }
        x += pw;
        tip.push_str(&format!("{name}: {} ({:.0} %)\n", fmt_bytes(*val), *val as f64 / total as f64 * 100.0));
    }
    p.rect_stroke(rect, 4.0, egui::Stroke::new(1.0_f32, theme::card_stroke()), egui::StrokeKind::Inside);
    resp.on_hover_text(tip.trim_end());
}

#[cfg(test)]
mod tests {
    #[test]
    fn no_negative_zero_cpu() {
        // příčina „-0.0 %“: prázdný součet f64 je v Rustu -0.0
        let empty: Vec<f64> = vec![];
        assert!(empty.iter().sum::<f64>().is_sign_negative());
        let mut h = std::collections::VecDeque::new();
        super::push_cpu(&mut h, -0.0);
        super::push_cpu(&mut h, f64::NAN);
        super::push_cpu(&mut h, 12.5);
        let v: Vec<String> = h.iter().map(|p| format!("{:.1}", p.1)).collect();
        assert_eq!(v, ["0.0", "0.0", "12.5"]);
    }
}
