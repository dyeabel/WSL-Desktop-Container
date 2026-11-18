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
    header(ui, "Přehled", "Stav kontejnerového enginu ve WSL a výkon kontejnerů");
    let ename = app.cfg.read().map(|c| c.engine.name()).unwrap_or("Engine");

    let distro = app.distro();
    let dstate = app.current_distro().map(|d| d.state.clone()).unwrap_or_else(|| "neznámý".into());
    let ver = app.snap.info.as_ref().map(|i| format!(" · {ename} {}", i.server_version)).unwrap_or_default();
    let mut stats = vec![Stat {
        title: ename.into(),
        big: if app.snap.connected { "Běží" } else { "Zastaven" }.into(),
        sub: format!("{distro} ({dstate}){ver}"),
        dot: Some(if app.snap.connected { theme::GREEN } else { theme::GREY }),
    }];
    if let Some(i) = &app.snap.info {
        let s = |t: &str, b: String, sub: String| Stat { title: t.into(), big: b, sub, dot: None };
        stats.push(s("Kontejnery", i.containers_running.to_string(), format!("běží z {} celkem", i.containers)));
        stats.push(s("Image", app.snap.images.len().to_string(), fmt_bytes(app.snap.images.iter().map(|i| i.size.max(0) as u64).sum())));
        stats.push(s("Volume", app.snap.volumes.len().to_string(), String::new()));
        stats.push(s("Zdroje WSL", format!("{} CPU", i.ncpu), fmt_bytes(i.mem_total.max(0) as u64) + " RAM"));
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
        section(ui, "Využití disku");
        let mut pick: Option<u8> = None;
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            toolbar_lr(
                ui,
                130.0,
                |ui, right| if !right {
                    for (k, v) in [("Image", df.images), ("Kontejnery", df.containers), ("Volume", df.volumes), ("Build cache", df.build_cache)] {
                        ui.label(muted(k));
                        ui.label(RichText::new(fmt_bytes(v.max(0) as u64)).strong());
                        ui.add_space(14.0);
                    }
                } else {
                    ui.menu_button("\u{EC54} Uklidit \u{E136}", |ui| {
                        ui.set_min_width(260.0);
                        for (i, label) in [
                            "Odstranit zastavené kontejnery…",
                            "Odstranit visící image",
                            "Odstranit nepoužívané image…",
                            "Odstranit nepoužívané volume…",
                            "Odstranit nepoužívané sítě",
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
            Some(0) => app.ask("Odstranit zastavené kontejnery?", "Všechny zastavené kontejnery budou smazány.", Cmd::PruneContainers),
            Some(1) => app.send(Cmd::PruneImages { all: false }),
            Some(2) => app.ask("Odstranit nepoužívané image?", "Smažou se všechny image, které nepoužívá žádný kontejner.", Cmd::PruneImages { all: true }),
            Some(3) => app.ask("Odstranit nepoužívané volume?", "Data v nepoužívaných volume budou nenávratně smazána!", Cmd::PruneVolumes),
            Some(4) => app.send(Cmd::PruneNetworks),
            _ => {}
        }
    }

    section(ui, "Výkon běžících kontejnerů");
    if !app.snap.containers.iter().any(|c| c.running()) {
        ui.label(muted("Žádný kontejner neběží."));
        return;
    }
    if app.stats.is_empty() {
        ui.label(muted("Načítám statistiky… (běžící kontejnery se měří cca 1–2 s)"));
        return;
    }
    let mut rows = app.stats.clone();
    rows.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal));
    let total_cpu: f64 = rows.iter().map(|r| r.cpu).sum();
    let ncpu = app.snap.info.as_ref().map(|i| i.ncpu.max(1)).unwrap_or(1) as f64;
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new("Celkové CPU").strong());
            ui.label(format!("{total_cpu:.1} %"));
        });
        ui.add(egui::ProgressBar::new((total_cpu / (ncpu * 100.0)) as f32).desired_height(6.0).fill(theme::ACCENT));
    });
    ui.add_space(8.0);
    let cols = [
        ("NÁZEV", W::Fr(2.0)),
        ("CPU", W::Fr(1.2)),
        ("PAMĚŤ", W::Fr(1.6)),
        ("SÍŤ (rx / tx)", W::FrOpt(1.4)),
        ("DISK (čtení / zápis)", W::FrOpt(1.4)),
    ];
    table(ui, "stats", &cols, |mut body| {
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
                        ui.add(egui::ProgressBar::new((r.cpu / (ncpu * 100.0)) as f32).desired_width(ui.available_width() - 12.0).desired_height(4.0).fill(theme::ACCENT));
                    });
                });
                row.col(|ui| {
                    ui.vertical(|ui| {
                        ui.label(format!("{} / {}", fmt_bytes(r.mem), fmt_bytes(r.mem_limit)));
                        let f = if r.mem_limit > 0 { r.mem as f32 / r.mem_limit as f32 } else { 0.0 };
                        ui.add(egui::ProgressBar::new(f).desired_width(ui.available_width() - 12.0).desired_height(4.0).fill(theme::ACCENT));
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
