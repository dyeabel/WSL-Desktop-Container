use super::*;
use crate::app::App;
use crate::docker::short_id;
use crate::docker::{HubRepo, HubTag};
use crate::worker::Cmd;

/// Stav hledání na Docker Hubu.
#[derive(Default)]
pub struct HubState {
    /// dialog je otevřený
    pub open: bool,
    pub query: String,
    /// poslední odeslaný dotaz
    pub searched: String,
    pub last_edit: f64,
    pub searching: bool,
    pub results: Vec<HubRepo>,
    pub error: Option<String>,
    /// vybraný repozitář, ke kterému se vybírá tag
    pub selected: Option<String>,
    pub selected_official: bool,
    pub tags: Vec<HubTag>,
    pub tags_loading: bool,
    pub tags_error: Option<String>,
    pub tag_filter: String,
    /// zvolený (nebo ručně zadaný) tag
    pub tag: String,
}

/// Oblíbené image nabízené, dokud uživatel nic nehledá.
const POPULAR: &[&str] = &[
    "nginx", "postgres", "redis", "mysql", "mariadb", "mongo", "node", "python", "alpine", "ubuntu",
    "debian", "rabbitmq", "traefik", "caddy", "httpd", "busybox", "adminer", "portainer/portainer-ce",
];

pub fn select_repo(app: &mut App, name: &str, official: bool) {
    let h = &mut app.hub;
    h.selected = Some(name.to_string());
    h.selected_official = official;
    h.tags.clear();
    h.tags_error = None;
    h.tag_filter.clear();
    h.tag = "latest".into();
    h.tags_loading = true;
    app.send(Cmd::HubTags(name.to_string()));
}

fn search(app: &mut App) {
    let q = app.hub.query.trim().to_string();
    if q.is_empty() {
        return;
    }
    app.hub.searched = q.clone();
    app.hub.searching = true;
    app.send(Cmd::HubSearch(q));
}

/// Panel „Stáhnout z Docker Hubu“: hledání, výsledky, výběr tagu.
/// Dialog „Stáhnout image z Docker Hubu“.
pub fn hub_dialog(app: &mut App, ctx: &egui::Context) {
    if !app.hub.open {
        return;
    }
    let screen = ctx.screen_rect();
    let width = (screen.width() - 120.0).clamp(420.0, 860.0);
    let max_h = (screen.height() - 220.0).max(240.0);
    let mut close = false;
    let (_, esc) = modal(ctx, "hub-dialog", t!("\u{E20C} Stáhnout image z Docker Hubu", "\u{E20C} Pull image from Docker Hub"), width, |ui| {
        egui::ScrollArea::vertical().id_salt("hub-dialog-scroll").max_height(max_h).show(ui, |ui| {
            ui.set_width(width - 12.0);
            hub_body(app, ui);
        });
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        // spodní lišta: tag + stažení (vždy viditelné) a Zavřít
        ui.horizontal(|ui| {
            if let Some(repo) = app.hub.selected.clone() {
                ui.label("Tag:");
                ui.add(field(&mut app.hub.tag).font(egui::TextStyle::Monospace).desired_width(160.0));
                let full = format!("{}:{}", repo, app.hub.tag.trim());
                ui.add_enabled_ui(!app.hub.tag.trim().is_empty(), |ui| {
                    if accent_button(ui, &tf!("\u{E20C} Stáhnout {full}", "\u{E20C} Pull {full}")).clicked() {
                        app.send(Cmd::Pull(full.clone()));
                    }
                });
            } else {
                ui.label(muted(t!("Vyber image ze seznamu a pak tag.", "Pick an image from the list, then a tag.")).small());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t!("Zavřít", "Close")).clicked() {
                    close = true;
                }
            });
        });
        ui.label(
            muted(t!(
                "Stahování běží na pozadí – průběh je vidět ve stavovém řádku.",
                "Pulling runs in the background – progress is shown in the status bar."
            ))
            .small(),
        );
    });
    if close || esc {
        app.hub.open = false;
    }
}

fn hub_body(app: &mut App, ui: &mut egui::Ui) {
    {

        // --- hledání (Enter, tlačítko, nebo samo po chvíli psaní)
        let now = ui.input(|i| i.time);
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 150.0).max(160.0);
            let r = ui.add(
                field(&mut app.hub.query)
                    .hint_text(t!(
                        "\u{E30C} Hledat image… (např. postgres, redis, bitnami/kafka)",
                        "\u{E30C} Search images… (e.g. postgres, redis, bitnami/kafka)"
                    ))
                    .desired_width(w),
            );
            if r.changed() {
                app.hub.last_edit = now;
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if accent_button(ui, t!("Hledat", "Search")).clicked() || enter {
                search(app);
            }
            if app.hub.searching {
                ui.spinner();
            }
        });
        let q = app.hub.query.trim().to_string();
        if q.chars().count() >= 2 && q != app.hub.searched && !app.hub.searching {
            if now - app.hub.last_edit > 0.6 {
                search(app);
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
            }
        }
        if q.is_empty() && !app.hub.searched.is_empty() {
            app.hub.searched.clear();
            app.hub.results.clear();
        }

        if let Some(e) = &app.hub.error {
            ui.label(RichText::new(format!("\u{E4E0} {e}")).color(theme::red()));
        }

        // --- oblíbené / výsledky
        if q.is_empty() {
            ui.add_space(4.0);
            toolbar(ui, |ui| {
                ui.label(muted(t!("Oblíbené:", "Popular:")));
                for p in POPULAR {
                    let sel = app.hub.selected.as_deref() == Some(*p);
                    if ui.add(egui::Button::new(*p).selected(sel)).clicked() {
                        select_repo(app, p, !p.contains('/'));
                    }
                }
            });
        } else if !app.hub.results.is_empty() {
            ui.add_space(6.0);
            let results = app.hub.results.clone();
            let mut pick = None;
            egui::ScrollArea::vertical().id_salt("hub-results").max_height(260.0).show(ui, |ui| {
                let cols = [
                    ("IMAGE", W::Fr(1.6)),
                    (t!("POPIS", "DESCRIPTION"), W::FrOpt(3.0)),
                    (t!("HVĚZDY", "STARS"), W::Px(70.0)),
                    ("", W::Px(90.0)),
                ];
                table_in(ui, "hub-results-t", &cols, |mut body| {
                    for r in &results {
                        let sel = app.hub.selected.as_deref() == Some(r.name.as_str());
                        body.row(32.0, |mut row| {
                            row.col(|ui| {
                                let t = RichText::new(&r.name).strong();
                                let t = if sel { t.color(theme::accent()) } else { t };
                                if ui.add(egui::Label::new(t).selectable(false).sense(egui::Sense::click())).clicked() {
                                    pick = Some((r.name.clone(), r.is_official));
                                }
                                if r.is_official {
                                    ui.label(RichText::new(t!("\u{E606} oficiální", "\u{E606} official")).small().color(theme::green()));
                                }
                            });
                            row.col(|ui| {
                                ui.label(muted(&r.description)).on_hover_text(&r.description);
                            });
                            row.col(|ui| {
                                ui.label(format!("\u{E46A} {}", r.star_count));
                            });
                            row.col(|ui| {
                                if ui.add(egui::Button::new(if sel { t!("Vybráno", "Selected") } else { t!("Vybrat", "Select") }).selected(sel)).clicked() {
                                    pick = Some((r.name.clone(), r.is_official));
                                }
                            });
                        });
                    }
                });
            });
            if let Some((n, o)) = pick {
                select_repo(app, &n, o);
            }
        } else if !app.hub.searching && !app.hub.searched.is_empty() && app.hub.error.is_none() {
            ui.label(muted(tf!("Pro „{}“ nebylo nic nalezeno.", "Nothing found for “{}”.", app.hub.searched)));
        }

        // --- výběr tagu
        if let Some(repo) = app.hub.selected.clone() {
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(&repo).size(17.0).strong());
                if app.hub.selected_official {
                    ui.label(RichText::new(t!("\u{E606} oficiální", "\u{E606} official")).small().color(theme::green()));
                }
                if app.hub.tags_loading {
                    ui.spinner();
                    ui.label(muted(t!("načítám tagy…", "loading tags…")));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon(ui, "\u{E4F6}", t!("Zavřít výběr", "Close selection")) {
                        app.hub.selected = None;
                    }
                });
            });
            if app.hub.selected.is_none() {
                return;
            }
            if let Some(e) = &app.hub.tags_error {
                ui.label(muted(tf!("Tagy nelze načíst ({e}) – zadej tag ručně.", "Cannot load tags ({e}) – enter a tag manually.")).small());
            }
            if !app.hub.tags.is_empty() {
                ui.add(
                    field(&mut app.hub.tag_filter)
                        .hint_text(t!("\u{E30C} Filtrovat tagy… (např. alpine, 16)", "\u{E30C} Filter tags… (e.g. alpine, 16)"))
                        .desired_width(260.0),
                );
                ui.add_space(4.0);
                let f = app.hub.tag_filter.to_lowercase();
                let tags: Vec<HubTag> = app.hub.tags.iter().filter(|t| f.is_empty() || t.name.to_lowercase().contains(&f)).cloned().collect();
                let mut chosen = None;
                egui::ScrollArea::vertical().id_salt("hub-tags").max_height(220.0).show(ui, |ui| {
                    let cols = [
                        ("TAG", W::Fr(2.0)),
                        (t!("VELIKOST (amd64)", "SIZE (amd64)"), W::Px(120.0)),
                        (t!("AKTUALIZOVÁNO", "UPDATED"), W::PxOpt(120.0)),
                        ("", W::Px(90.0)),
                    ];
                    table_in(ui, "hub-tags-t", &cols, |mut body| {
                        for t in &tags {
                            let sel = app.hub.tag == t.name;
                            body.row(30.0, |mut row| {
                                row.col(|ui| {
                                    let txt = RichText::new(&t.name).monospace();
                                    let txt = if sel { txt.color(theme::accent()).strong() } else { txt };
                                    if ui.add(egui::Label::new(txt).selectable(false).sense(egui::Sense::click())).clicked() {
                                        chosen = Some(t.name.clone());
                                    }
                                });
                                row.col(|ui| {
                                    ui.label(if t.size > 0 { fmt_bytes(t.size) } else { "—".into() });
                                });
                                row.col(|ui| {
                                    ui.label(muted(&t.updated));
                                });
                                row.col(|ui| {
                                    if ui.add(egui::Button::new(if sel { t!("Vybráno", "Selected") } else { t!("Vybrat", "Select") }).selected(sel)).clicked() {
                                        chosen = Some(t.name.clone());
                                    }
                                });
                            });
                        }
                    });
                    if tags.is_empty() {
                        ui.label(muted(t!("Žádný tag neodpovídá filtru.", "No tag matches the filter.")));
                    }
                });
                if let Some(c) = chosen {
                    app.hub.tag = c;
                }
            }
        }
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let total: i64 = app.snap.images.iter().map(|i| i.size).sum();
    header(
        ui,
        t!("Image", "Images"),
        &tf!("{} image · {}", "{} images · {}", app.snap.images.len(), fmt_bytes(total.max(0) as u64)),
    );
    if !need_engine(app, ui) {
        return;
    }
    let mut prune_dangling = false;
    let mut prune_all = false;
    toolbar_lr(
        ui,
        240.0,
        |ui, right| if !right {
            if accent_button(ui, t!("\u{E30C} Hledat na Docker Hubu…", "\u{E30C} Search Docker Hub…")).clicked() {
                app.hub.open = true;
            }
            crate::ui::build::toolbar_button(app, ui);
            gap(ui);
            // pevná šířka: Hledat + Sestavit + pole + Stáhnout se vejdou na jeden řádek
            let w = 200.0;
            // krátká nápověda – delší by pole roztáhla (wrap_mode Extend) a tlačítko by se zalomilo
            let r = ui
                .add(field(&mut app.pull_input).hint_text(t!("nebo přesný název…", "or exact name…")).desired_width(w))
                .on_hover_text(t!("Přesný název image, např. ghcr.io/org/app:1.2", "Exact image name, e.g. ghcr.io/org/app:1.2"));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.button(t!("\u{E20C} Stáhnout", "\u{E20C} Pull")).clicked() || enter) && !app.pull_input.trim().is_empty() {
                app.send(Cmd::Pull(app.pull_input.trim().to_string()));
                app.pull_input.clear();
            }
        } else {
            more_menu(ui, |ui| {
                if ui
                    .button(t!("\u{EC54} Odstranit visící image", "\u{EC54} Remove dangling images"))
                    .on_hover_text(t!("image bez tagu (docker image prune)", "untagged images (docker image prune)"))
                    .clicked()
                {
                    prune_dangling = true;
                    ui.close_menu();
                }
                if ui.button(t!("\u{EC54} Odstranit všechny nepoužívané…", "\u{EC54} Remove all unused…")).clicked() {
                    prune_all = true;
                    ui.close_menu();
                }
            });
            filter_field(ui, &mut app.filter);
        },
    );
    if prune_dangling {
        app.send(Cmd::PruneImages { all: false });
    }
    if prune_all {
        app.ask(
            t!("Odstranit nepoužívané image?", "Remove unused images?"),
            t!("Smažou se všechny image, které nepoužívá žádný kontejner.", "All images not used by any container will be deleted."),
            Cmd::PruneImages { all: true },
        );
    }
    ui.add_space(8.0);

    let f = app.filter.to_lowercase();
    let images = app.snap.images.clone();
    let containers = app.snap.containers.clone();
    let cols = [
        (t!("REPOZITÁŘ:TAG", "REPOSITORY:TAG"), W::Fr(3.0)),
        ("ID", W::PxOpt(120.0)),
        (t!("VELIKOST", "SIZE"), W::Px(90.0)),
        (t!("VYTVOŘEN", "CREATED"), W::PxOpt(110.0)),
        (t!("POUŽÍVÁ", "USED BY"), W::FrOpt(1.5)),
        (t!("AKCE", "ACTIONS"), icons_w(3)),
    ];
    table(ui, "images", &cols, |mut body| {
        for img in images.iter().filter(|i| f.is_empty() || i.tags().join(" ").to_lowercase().contains(&f)) {
            let tags = img.tags();
            body.row(34.0, |mut r| {
            r.col(|ui| {
                let shown: Vec<String> = tags.iter().map(|t| crate::engine::short_image(t)).collect();
                ui.label(RichText::new(shown.join(", ")).strong()).on_hover_text(tags.join("\n"));
            });
            r.col(|ui| {
                ui.label(RichText::new(short_id(&img.id)).monospace().color(theme::muted()));
            });
            r.col(|ui| {
                ui.label(fmt_bytes(img.size.max(0) as u64));
            });
            r.col(|ui| {
                ui.label(fmt_ago(img.created));
            });
            let used: Vec<String> = containers.iter().filter(|c| c.image_id == img.id).map(|c| c.name()).collect();
            r.col(|ui| {
                if used.is_empty() {
                    ui.label(muted("—"));
                } else {
                    ui.label(used.join(", "));
                }
            });
            r.col(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let tag = tags.first().cloned().unwrap_or_default();
                if !tag.contains("<none>") {
                    if icon(ui, "\u{E3D0}", t!("Spustit kontejner z tohoto image", "Run a container from this image")) {
                        app.run.image = tag.clone();
                        app.run_open = true;
                    }
                    if icon(ui, "\u{E20C}", t!("Aktualizovat (pull)", "Update (pull)")) {
                        app.send(Cmd::Pull(tag.clone()));
                    }
                }
                if icon(ui, "\u{E4A6}", t!("Odstranit", "Remove")) {
                    let id = if tags.len() == 1 && !tag.contains("<none>") { tag.clone() } else { img.id.clone() };
                    app.ask(
                        tf!("Odstranit {tag}?", "Remove {tag}?"),
                        t!(
                            "Image bude odstraněn (pokud ho nepoužívá žádný kontejner).",
                            "The image will be removed (if no container uses it)."
                        ),
                        Cmd::RemoveImage(id),
                    );
                }
            });
            });
        }
    });
}
