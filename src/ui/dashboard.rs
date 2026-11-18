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

    if !need_engine(app, ui) {
        return;
    }

    if let Some(df) = app.df.clone() {
        section(ui, t!("Využití disku", "Disk usage"));
        let mut pick: Option<u8> = None;
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            toolbar_lr(
                ui,
                130.0,
                |ui, right| if !right {
                    for (k, v) in [
                        (t!("Image", "Images"), df.images),
                        (t!("Kontejnery", "Containers"), df.containers),
                        (t!("Volume", "Volumes"), df.volumes),
                        ("Build cache", df.build_cache),
                    ] {
                        ui.label(muted(k));
                        ui.label(RichText::new(fmt_bytes(v.max(0) as u64)).strong());
                        ui.add_space(14.0);
                    }
                } else {
                    ui.menu_button(t!("\u{EC54} Uklidit \u{E136}", "\u{EC54} Clean up \u{E136}"), |ui| {
                        ui.set_min_width(260.0);
                        for (i, label) in [
                            t!("Odstranit zastavené kontejnery…", "Remove stopped containers…"),
                            t!("Odstranit visící image", "Remove dangling images"),
                            t!("Odstranit nepoužívané image…", "Remove unused images…"),
                            t!("Odstranit nepoužívané volume…", "Remove unused volumes…"),
                            t!("Odstranit nepoužívané sítě", "Remove unused networks"),
                        ]
                        .iter()
                        .enumerate()
                        {
                            if ui.button(*label).clicked() {
                                pick = Some(i as u8);
                                ui.close_menu();
                            }
                        }
                    });
                },
            );
        });
        match pick {
            Some(0) => app.ask(
                t!("Odstranit zastavené kontejnery?", "Remove stopped containers?"),
                t!("Všechny zastavené kontejnery budou smazány.", "All stopped containers will be deleted."),
                Cmd::PruneContainers,
            ),
            Some(1) => app.send(Cmd::PruneImages { all: false }),
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
    let mut rows = app.stats.clone();
    rows.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal));
    sort_items(ui.ctx(), "stats", &mut rows, |r, col| match col {
        1 => SortKey::num(r.cpu),
        2 => SortKey::num(r.mem as f64),
        3 => SortKey::num((r.net_rx + r.net_tx) as f64),
        4 => SortKey::num((r.blk_read + r.blk_write) as f64),
        _ => SortKey::text(&r.name),
    });
    let total_cpu: f64 = rows.iter().map(|r| r.cpu).sum();
    let ncpu = app.snap.info.as_ref().map(|i| i.ncpu.max(1)).unwrap_or(1) as f64;
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(t!("Celkové CPU", "Total CPU")).strong());
            ui.label(format!("{total_cpu:.1} %"));
        });
        ui.add(egui::ProgressBar::new((total_cpu / (ncpu * 100.0)) as f32).desired_height(6.0).fill(theme::accent()));
    });
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
                    // název + image, ze kterého kontejner běží
                    let image = app
                        .snap
                        .containers
                        .iter()
                        .find(|c| c.id == r.id)
                        .map(|c| crate::engine::short_image(&c.image))
                        .unwrap_or_default();
                    two_line(ui, RichText::new(&r.name).strong(), muted(&image).small());
                });
                row.col(|ui| {
                    ui.vertical(|ui| {
                        ui.label(format!("{:.2} %", r.cpu));
                        ui.add(egui::ProgressBar::new((r.cpu / (ncpu * 100.0)) as f32).desired_width(ui.available_width() - 12.0).desired_height(4.0).fill(theme::accent()));
                    });
                });
                row.col(|ui| {
                    ui.vertical(|ui| {
                        ui.label(format!("{} / {}", fmt_bytes(r.mem), fmt_bytes(r.mem_limit)));
                        let f = if r.mem_limit > 0 { r.mem as f32 / r.mem_limit as f32 } else { 0.0 };
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
