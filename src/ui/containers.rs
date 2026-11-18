use super::*;
use crate::app::App;
use crate::docker::{short_id, Container};
use crate::worker::{Cmd, RunSpec, Term};
use std::collections::BTreeMap;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let (run, total) = (
        app.snap.containers.iter().filter(|c| c.running()).count(),
        app.snap.containers.len(),
    );
    header(ui, "Kontejnery", &format!("{run}/{total} běží"));
    if !need_engine(app, ui) {
        return;
    }
    let mut cmd: Option<Cmd> = None;
    let mut prune = false;
    toolbar_lr(
        ui,
        240.0,
        |ui, right| if !right {
            if accent_button(ui, "\u{E3D0} Spustit kontejner").clicked() {
                app.run_open = true;
            }
            if ui.button("\u{E094} Obnovit").clicked() {
                cmd = Some(Cmd::Refresh);
            }
            ui.checkbox(&mut app.show_all, "Zobrazit i zastavené");
        } else {
            more_menu(ui, |ui| {
                if ui.button("\u{E39E} Pozastavit všechny běžící").clicked() {
                    cmd = Some(Cmd::PauseAll);
                    ui.close_menu();
                }
                if ui.button("\u{E3D2} Obnovit všechny pozastavené").clicked() {
                    cmd = Some(Cmd::UnpauseAll);
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("\u{EC54} Odstranit zastavené kontejnery…").clicked() {
                    prune = true;
                    ui.close_menu();
                }
            });
            filter_field(ui, &mut app.filter);
        },
    );
    if let Some(c) = cmd {
        app.send(c);
    }
    if prune {
        app.ask("Odstranit zastavené kontejnery?", "Všechny zastavené kontejnery budou smazány.", Cmd::PruneContainers);
    }
    ui.add_space(8.0);

    let f = app.filter.to_lowercase();
    let mut groups: BTreeMap<String, Vec<Container>> = BTreeMap::new();
    for c in &app.snap.containers {
        if !app.show_all && !c.running() && !c.paused() {
            continue;
        }
        if !f.is_empty() && !c.name().to_lowercase().contains(&f) && !c.image.to_lowercase().contains(&f) {
            continue;
        }
        let key = c.project().map(|p| format!("\u{E466} {p}")).unwrap_or_else(|| "~Samostatné".into());
        groups.entry(key).or_default().push(c.clone());
    }
    if groups.is_empty() {
        ui.label(muted("Žádné kontejnery."));
    }
    for (g, list) in groups {
        let title = g.trim_start_matches('~');
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).size(17.0).strong());
            ui.label(muted(format!("{}", list.len())));
        });
        let cols = [
            ("", W::Px(14.0)),
            ("NÁZEV", W::Fr(2.2)),
            ("IMAGE", W::Fr(2.0)),
            ("PORTY", W::FrOpt(2.2)),
            ("SÍŤ", W::FrOpt(1.2)),
            ("STAV", W::Fr(1.6)),
            ("AKCE", icons_w(6)),
        ];
        table(ui, &format!("grid-{g}"), &cols, |mut body| {
            for c in &list {
                body.row(46.0, |mut r| row(app, &mut r, c));
            }
        });
    }
}

fn row(app: &mut App, r: &mut TableRow<'_>, c: &Container) {
    let st = state_info(&c.state, &c.status);
    let color = st.color;
    r.col(|ui| dot(ui, color));
    r.col(|ui| two_line(ui, RichText::new(c.name()).strong(), RichText::new(short_id(&c.id)).small().monospace().color(theme::MUTED)));
    r.col(|ui| {
        ui.label(crate::engine::short_image(&c.image)).on_hover_text(&c.image);
    });
    r.col(|ui| {
        ui.label(c.ports_text());
    });
    r.col(|ui| {
        ui.label(c.networks().join(", "));
    });
    r.col(|ui| {
        let tip = format!("{}\nvytvořen {}", c.status, fmt_ago(c.created));
        let resp = ui
            .vertical(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 2.0);
                ui.horizontal(|ui| {
                    badge(ui, &st.label, st.color);
                    if let Some((h, hc)) = &st.health {
                        badge(ui, h, *hc);
                    }
                });
                if !st.detail.is_empty() {
                    ui.label(muted(&st.detail).small());
                }
            })
            .response;
        resp.on_hover_text(tip);
    });
    r.col(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let id = c.id.clone();
        let name = c.name();
        if c.paused() {
            if icon(ui, "\u{E3D2}", "Obnovit (unpause)") {
                app.send(Cmd::Container(id.clone(), "unpause"));
            }
        } else if c.running() {
            if icon(ui, "\u{E46C}", "Zastavit") {
                app.send(Cmd::Container(id.clone(), "stop"));
            }
            if icon(ui, "\u{E094}", "Restartovat") {
                app.send(Cmd::Container(id.clone(), "restart"));
            }
            if icon(ui, "\u{E39E}", "Pozastavit") {
                app.send(Cmd::Container(id.clone(), "pause"));
            }
            if icon(ui, "\u{EAE8}", "Konzole (Windows Terminal)") {
                app.send(Cmd::Terminal(Term::Container { id: id.clone(), name: name.clone() }));
            }
        } else if icon(ui, "\u{E3D0}", "Spustit") {
            app.send(Cmd::Container(id.clone(), "start"));
        }
        if icon(ui, "\u{EB7A}", "Logy") {
            app.open_logs(id.clone(), name.clone());
        }
        if icon(ui, "\u{E4A6}", "Odstranit") {
            app.ask(
                format!("Odstranit {name}?"),
                "Kontejner bude zastaven a smazán (pojmenované volume zůstanou).",
                Cmd::Container(id.clone(), "remove"),
            );
        }
    });
}

/// Stav kontejneru připravený pro zobrazení.
pub struct StateInfo {
    pub label: String,
    pub color: egui::Color32,
    pub detail: String,
    pub health: Option<(String, egui::Color32)>,
}

/// Přeloží Dockerovu dobu („6 seconds“, „About a minute“, „2 hours“) do češtiny.
pub fn cz_duration(s: &str) -> String {
    let s = s.trim();
    match s {
        "" => return String::new(),
        "Less than a second" => return "méně než 1 s".into(),
        "About a minute" => return "asi 1 min".into(),
        "About an hour" => return "asi 1 h".into(),
        _ => {}
    }
    let mut it = s.split_whitespace();
    let (Some(n), Some(unit)) = (it.next(), it.next()) else { return s.to_string() };
    let u = match unit.trim_end_matches('s') {
        "second" => "s",
        "minute" => "min",
        "hour" => "h",
        "day" => "d",
        "week" => "týd.",
        "month" => "měs.",
        "year" => "r.",
        _ => return s.to_string(),
    };
    format!("{n} {u}")
}

/// Rozebere `State` + `Status` z Docker API (např. „Exited (137) 5 minutes ago“,
/// „Up 2 hours (healthy)“) na český štítek, barvu a doplňující text.
pub fn state_info(state: &str, status: &str) -> StateInfo {
    let health = if status.contains("(healthy)") {
        Some(("zdravý".to_string(), theme::GREEN))
    } else if status.contains("(unhealthy)") {
        Some(("nezdravý".to_string(), theme::RED))
    } else if status.contains("health: starting") {
        Some(("startuje".to_string(), theme::YELLOW))
    } else {
        None
    };
    // „Up 6 minutes (healthy)“ -> „6 minutes“
    let up_for = || {
        let rest = status.strip_prefix("Up").unwrap_or("").trim();
        let rest = rest.split(" (").next().unwrap_or("");
        cz_duration(rest)
    };
    let (label, color, detail) = match state {
        "running" => ("Běží".to_string(), theme::GREEN, format!("běží {}", up_for())),
        "paused" => ("Pozastaven".to_string(), theme::YELLOW, format!("běží {}", up_for())),
        "restarting" => ("Restartuje".to_string(), theme::YELLOW, String::new()),
        "exited" => {
            // „Exited (1) 6 seconds ago“
            let code = status
                .split_once('(')
                .and_then(|(_, r)| r.split_once(')'))
                .map(|(c, _)| c.trim().to_string())
                .unwrap_or_default();
            let ago = status
                .split_once(')')
                .map(|(_, r)| r.trim().trim_end_matches(" ago"))
                .map(cz_duration)
                .unwrap_or_default();
            let detail = if ago.is_empty() { String::new() } else { format!("ukončen před {ago}") };
            if code.is_empty() || code == "0" {
                ("Zastaven".to_string(), theme::GREY, detail)
            } else {
                let why = match code.as_str() {
                    "137" => " – zabit (SIGKILL / nedostatek paměti)",
                    "143" => " – ukončen (SIGTERM)",
                    "139" => " – pád (SIGSEGV)",
                    _ => "",
                };
                (format!("Chyba ({code})"), theme::RED, format!("{detail}{why}"))
            }
        }
        "created" => ("Vytvořen".to_string(), theme::GREY, "zatím nespuštěn".to_string()),
        "dead" => ("Mrtvý".to_string(), theme::RED, String::new()),
        "removing" => ("Odstraňuje se".to_string(), theme::YELLOW, String::new()),
        other => (other.to_string(), theme::GREY, String::new()),
    };
    StateInfo { label, color, detail, health }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exited_ok() {
        let s = state_info("exited", "Exited (0) 6 seconds ago");
        assert_eq!(s.label, "Zastaven");
        assert_eq!(s.detail, "ukončen před 6 s");
    }

    #[test]
    fn exited_error_137() {
        let s = state_info("exited", "Exited (137) About a minute ago");
        assert_eq!(s.label, "Chyba (137)");
        assert!(s.detail.starts_with("ukončen před asi 1 min"));
        assert!(s.detail.contains("SIGKILL"));
    }

    #[test]
    fn running_healthy() {
        let s = state_info("running", "Up 2 hours (healthy)");
        assert_eq!(s.label, "Běží");
        assert_eq!(s.detail, "běží 2 h");
        assert_eq!(s.health.map(|h| h.0).as_deref(), Some("zdravý"));
    }

    #[test]
    fn durations() {
        assert_eq!(cz_duration("Less than a second"), "méně než 1 s");
        assert_eq!(cz_duration("3 days"), "3 d");
        assert_eq!(cz_duration("1 week"), "1 týd.");
    }
}

pub fn run_dialog(app: &mut App, ctx: &egui::Context) {
    if !app.run_open {
        return;
    }
    let mut submit = false;
    let mut cancel = false;
    let nets: Vec<String> = app.snap.networks.iter().map(|n| n.name.clone()).collect();
    let vols: Vec<String> = app.snap.volumes.iter().map(|v| v.name.clone()).collect();
    let (_, close) = modal(ctx, "run-dialog", "Spustit kontejner", 500.0, |ui| {
            let r = &mut app.run;
            egui::Grid::new("runform").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
                ui.label("Image *");
                ui.add(field(&mut r.image).hint_text("nginx:alpine").desired_width(360.0));
                ui.end_row();
                ui.label("Název");
                ui.add(field(&mut r.name).hint_text("volitelné").desired_width(360.0));
                ui.end_row();
                ui.label("Porty");
                ui.add(field(&mut r.ports).hint_text("8080:80, 127.0.0.1:5432:5432").desired_width(360.0));
                ui.end_row();
                ui.label("Proměnné");
                ui.add(area(&mut r.env).hint_text("KLIC=hodnota (řádek = 1)").desired_rows(3).desired_width(360.0));
                ui.end_row();
                ui.label("Volume");
                ui.add(area(&mut r.volumes).hint_text("data:/var/lib/data (řádek = 1)").desired_rows(2).desired_width(360.0));
                ui.end_row();
                // výběr existujícího volume (sdílení mezi kontejnery)
                ui.label("");
                ui.horizontal(|ui| {
                    let (vname, vpath, vro) = &mut app.run_vol;
                    egui::ComboBox::from_id_salt("runvol")
                        .width(130.0)
                        .selected_text(if vname.is_empty() { "existující volume…" } else { vname.as_str() })
                        .show_ui(ui, |ui| {
                            if vols.is_empty() {
                                ui.label(muted("žádné volume – napiš nový název do pole výše"));
                            }
                            for v in &vols {
                                ui.selectable_value(vname, v.clone(), v);
                            }
                        });
                    ui.add(field(vpath).hint_text("/data").desired_width(95.0)).on_hover_text("Cesta uvnitř kontejneru");
                    ui.checkbox(vro, "jen čtení");
                    let ok = !vname.is_empty() && vpath.trim().starts_with('/');
                    if ui.add_enabled(ok, egui::Button::new("\u{E3D4} Přidat")).clicked() {
                        let target = vpath.trim().to_string();
                        let line = format!("{}:{}{}", vname, target, if *vro { ":ro" } else { "" });
                        // stejná cesta v kontejneru může být připojená jen jednou – starý řádek nahradit
                        let mut lines: Vec<String> = r
                            .volumes
                            .lines()
                            .map(str::trim)
                            .filter(|l| !l.is_empty())
                            .filter(|l| {
                                let dest = l.splitn(3, ':').nth(1).unwrap_or("");
                                dest != target
                            })
                            .map(String::from)
                            .collect();
                        lines.push(line);
                        r.volumes = lines.join("\n");
                        vname.clear();
                    }
                });
                ui.end_row();
                ui.label("Síť");
                egui::ComboBox::from_id_salt("runnet")
                    .selected_text(if r.network.is_empty() { "výchozí (bridge)" } else { r.network.as_str() })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut r.network, String::new(), "výchozí (bridge)");
                        for n in &nets {
                            ui.selectable_value(&mut r.network, n.clone(), n);
                        }
                    });
                ui.end_row();
                ui.label("Restart");
                egui::ComboBox::from_id_salt("runrestart").selected_text(r.restart.as_str()).show_ui(ui, |ui| {
                    for p in ["no", "always", "unless-stopped", "on-failure"] {
                        ui.selectable_value(&mut r.restart, p.to_string(), p);
                    }
                });
                ui.end_row();
                ui.label("Příkaz");
                ui.horizontal(|ui| {
                    ui.add(field(&mut r.command).hint_text("volitelné").desired_width(250.0));
                    if ui
                        .button("sleep infinity")
                        .on_hover_text("Udrží kontejner běžící na pozadí (pro image bez vlastní služby: alpine, ubuntu, debian…)")
                        .clicked()
                    {
                        r.command = "sleep infinity".into();
                    }
                });
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut r.interactive, "Interaktivně v terminálu (-it, po ukončení se smaže)")
                    .on_hover_text("Otevře Windows Terminal se shellem kontejneru – vhodné pro alpine, ubuntu, busybox…");
                ui.end_row();
            });
            // image bez vlastní služby (alpine, ubuntu…) spuštěný na pozadí bez příkazu hned skončí
            let base = crate::engine::short_image(&r.image);
            let base = base.split(':').next().unwrap_or("").to_string();
            if !r.interactive
                && r.command.trim().is_empty()
                && ["alpine", "ubuntu", "debian", "busybox", "fedora", "centos", "rockylinux", "archlinux", "opensuse/leap", "amazonlinux"].contains(&base.as_str())
            {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "\u{E4E0} {base} nemá vlastní službu – na pozadí bez příkazu hned skončí. Zaškrtni „Interaktivně“ nebo dej příkaz (sleep infinity)."
                    ))
                    .color(theme::YELLOW)
                    .small(),
                );
            }
            ui.add_space(10.0);
            egui::Frame::new()
                .fill(theme::BG)
                .corner_radius(6)
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.add(egui::Label::new(RichText::new(r.args().join(" ")).monospace().small().color(theme::MUTED)).wrap());
                });
            let (ok, c) = dialog_buttons(ui, "\u{E3D0} Spustit", false, !r.image.trim().is_empty());
            submit = ok;
            cancel = c;
        });
    if submit {
        app.send(Cmd::Run(app.run.clone()));
        app.run = RunSpec { restart: "no".into(), ..Default::default() };
    }
    if std::env::var("WLS_DEBUG_LOG").is_ok() {
        eprintln!("[run_dialog] submit={submit} cancel={cancel} close={close}");
    }
    if submit || cancel || close {
        app.run_open = false;
    }
}
