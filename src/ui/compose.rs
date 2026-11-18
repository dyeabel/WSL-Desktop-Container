use super::*;
use crate::app::App;
use crate::docker::Container;
use crate::worker::{Cmd, Term};
use std::collections::BTreeMap;

struct Stack {
    name: String,
    dir: String,
    files: Vec<String>,
    containers: Vec<Container>,
}

fn stacks(cs: &[Container]) -> Vec<Stack> {
    let mut m: BTreeMap<String, Stack> = BTreeMap::new();
    for c in cs {
        // docker compose i podman-compose nastavují com.docker.compose.*; podman-compose navíc io.podman.compose.*
        let Some(p) = c.project().or_else(|| c.label("io.podman.compose.project")) else { continue };
        let s = m.entry(p.to_string()).or_insert_with(|| Stack {
            name: p.to_string(),
            dir: c.label("com.docker.compose.project.working_dir").unwrap_or("").to_string(),
            files: c
                .label("com.docker.compose.project.config_files")
                .map(|f| f.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default(),
            containers: Vec::new(),
        });
        s.containers.push(c.clone());
    }
    m.into_values().collect()
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let list = stacks(&app.snap.containers);
    header(ui, "Compose", &tf!("{} stacků (rozpoznáno podle labelů kontejnerů)", "{} stacks (detected from container labels)", list.len()));
    if !need_engine(app, ui) {
        return;
    }

    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(t!("Spustit stack ze složky", "Start stack from folder")).strong());
        toolbar(ui, |ui| {
            ui.add(
                field(&mut app.compose_dir)
                    .hint_text(t!(r"D:\projekty\app  nebo  /home/user/app", r"D:\projects\app  or  /home/user/app"))
                    .desired_width(380.0),
            );
            if accent_button(ui, "\u{E3D0} docker compose up -d").clicked() && !app.compose_dir.trim().is_empty() {
                app.send(Cmd::ComposeUpPath(app.compose_dir.trim().to_string()));
            }
        });
        ui.label(muted(t!("Složka musí obsahovat compose.yaml / docker-compose.yml. Windows cesty se převedou přes wslpath.", "The folder must contain compose.yaml / docker-compose.yml. Windows paths are converted via wslpath.")).small());
    });

    if list.is_empty() {
        ui.add_space(8.0);
        ui.label(muted(t!("Žádné compose stacky.", "No Compose stacks.")));
        return;
    }

    for s in list {
        ui.add_space(10.0);
        let running = s.containers.iter().filter(|c| c.running()).count();
        let total = s.containers.len();
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                dot(
                    ui,
                    if running == total { theme::GREEN } else if running > 0 { theme::YELLOW } else { theme::GREY },
                );
                ui.label(RichText::new(&s.name).size(18.0).strong());
                ui.label(muted(tf!("{running}/{total} běží", "{running}/{total} running")));
            });
            ui.label(RichText::new(&s.dir).monospace().small().color(theme::MUTED));
            ui.add_space(4.0);
            let dir = s.dir.clone();
            let proj = Some(s.name.clone());
            let c = |args: &[&str]| Cmd::Compose { dir: dir.clone(), project: proj.clone(), args: args.iter().map(|a| a.to_string()).collect() };
            toolbar(ui, |ui| {
                if accent_button(ui, "\u{E3D0} Up").on_hover_text("docker compose up -d").clicked() {
                    app.send(c(&["up", "-d"]));
                }
                if ui.button("\u{E46C} Stop").clicked() {
                    app.send(c(&["stop"]));
                }
                if ui.button("\u{E3D0} Start").clicked() {
                    app.send(c(&["start"]));
                }
                if ui.button("\u{E094} Restart").clicked() {
                    app.send(c(&["restart"]));
                }
                if ui.button("\u{E20C} Pull").clicked() {
                    app.send(c(&["pull"]));
                }
                if ui.button("\u{E80E} Build & up").on_hover_text("docker compose up -d --build").clicked() {
                    app.send(c(&["up", "-d", "--build"]));
                }
                if ui.button("\u{E2F2} ps").clicked() {
                    app.send(c(&["ps", "-a"]));
                }
                if ui.button(t!("\u{EB7A} Logy", "\u{EB7A} Logs")).on_hover_text("docker compose logs --tail 300").clicked() {
                    app.send(c(&["logs", "--no-color", "--tail", "300"]));
                }
                if ui.button("\u{E4A6} Down").clicked() {
                    app.ask(
                        format!("docker compose down – {}?", s.name),
                        t!("Kontejnery a sítě stacku budou odstraněny (volume zůstanou).", "The stack's containers and networks will be removed (volumes are kept)."),
                        c(&["down"]),
                    );
                }
                gap(ui);
                for f in &s.files {
                    let fname = f.rsplit('/').next().unwrap_or(f);
                    if ui.button(format!("\u{E34C} {fname}")).on_hover_text(tf!("Upravit v Notepadu: {f}", "Edit in Notepad: {f}")).clicked() {
                        app.send(Cmd::Notepad(f.clone()));
                    }
                }
                if icon(ui, "\u{E256}", t!("Otevřít složku v Průzkumníku", "Open folder in Explorer")) {
                    app.send(Cmd::Explorer(s.dir.clone()));
                }
                if icon(ui, "\u{EAE8}", t!("Terminál ve složce stacku", "Terminal in stack folder")) {
                    app.send(Cmd::Terminal(Term::Dir(s.dir.clone())));
                }
            });
            ui.add_space(4.0);
            let cols = [
                ("", W::Px(14.0)),
                (t!("SLUŽBA", "SERVICE"), W::Fr(1.5)),
                ("IMAGE", W::Fr(2.0)),
                (t!("PORTY", "PORTS"), W::FrOpt(2.0)),
                (t!("AKCE", "ACTIONS"), icons_w(3)),
            ];
            table_in(ui, &format!("svc-{}", s.name), &cols, |mut body| {
                for ct in &s.containers {
                  body.row(32.0, |mut r| {
                    r.col(|ui| dot(ui, if ct.running() { theme::GREEN } else if ct.paused() { theme::YELLOW } else { theme::GREY }));
                    r.col(|ui| {
                        ui.label(RichText::new(ct.label("com.docker.compose.service").unwrap_or("?")).strong());
                    });
                    r.col(|ui| {
                        ui.label(crate::engine::short_image(&ct.image)).on_hover_text(&ct.image);
                    });
                    r.col(|ui| {
                        ui.label(ct.ports_text());
                    });
                    r.col(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ct.running() {
                            if icon(ui, "\u{E46C}", t!("Zastavit", "Stop")) {
                                app.send(Cmd::Container(ct.id.clone(), "stop"));
                            }
                            if icon(ui, "\u{EAE8}", t!("Konzole", "Console")) {
                                app.send(Cmd::Terminal(Term::Container { id: ct.id.clone(), name: ct.name() }));
                            }
                        } else if icon(ui, "\u{E3D0}", t!("Spustit", "Start")) {
                            app.send(Cmd::Container(ct.id.clone(), "start"));
                        }
                        if icon(ui, "\u{EB7A}", t!("Logy", "Logs")) {
                            app.open_logs(ct.id.clone(), ct.name());
                        }
                    });
                  });
                }
            });
        });
    }
}
