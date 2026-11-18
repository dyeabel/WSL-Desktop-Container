use super::*;
use crate::app::App;
use crate::worker::Cmd;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(ui, "Volume", &format!("{} volume", app.snap.volumes.len()));
    if !need_engine(app, ui) {
        return;
    }
    let mut prune = false;
    toolbar_lr(
        ui,
        240.0,
        |ui, right| if !right {
            ui.add(field(&mut app.new_volume).hint_text("název nového volume").desired_width(220.0));
            if accent_button(ui, "\u{E3D4} Vytvořit").clicked() && !app.new_volume.trim().is_empty() {
                app.send(Cmd::CreateVolume(app.new_volume.trim().to_string()));
                app.new_volume.clear();
            }
        } else {
            more_menu(ui, |ui| {
                if ui.button("\u{EC54} Odstranit nepoužívané volume…").clicked() {
                    prune = true;
                    ui.close_menu();
                }
            });
            filter_field(ui, &mut app.filter);
        },
    );
    if prune {
        app.ask("Odstranit nepoužívané volume?", "Data v nepoužívaných volume budou nenávratně smazána!", Cmd::PruneVolumes);
    }
    ui.add_space(8.0);

    let f = app.filter.to_lowercase();
    let volumes = app.snap.volumes.clone();
    let containers = app.snap.containers.clone();
    let cols = [
        ("NÁZEV", W::Fr(2.5)),
        ("DRIVER", W::PxOpt(80.0)),
        ("POUŽÍVÁ", W::Fr(1.5)),
        ("MOUNTPOINT", W::FrOpt(3.0)),
        ("AKCE", icons_w(3)),
    ];
    let mut browse = None;
    let mut run_with: Option<String> = None;
    table(ui, "volumes", &cols, |mut body| {
        for v in volumes.iter().filter(|v| f.is_empty() || v.name.to_lowercase().contains(&f)) {
            let used: Vec<String> = containers
                .iter()
                .filter(|c| c.mounts.iter().flatten().any(|m| m.name.as_deref() == Some(v.name.as_str())))
                .map(|c| c.name())
                .collect();
            let project = v.labels.as_ref().and_then(|l| l.get("com.docker.compose.project")).cloned();
            let h = if project.is_some() { 44.0 } else { 34.0 };
            body.row(h, |mut r| {
                r.col(|ui| match &project {
                    Some(p) => two_line(ui, RichText::new(&v.name), muted(format!("\u{E466} {p}")).small()),
                    None => {
                        ui.label(&v.name);
                    }
                });
                r.col(|ui| {
                    ui.label(&v.driver);
                });
                r.col(|ui| {
                    if used.is_empty() {
                        ui.label(muted("nepoužívá se"));
                    } else {
                        ui.label(used.join(", "));
                    }
                });
                r.col(|ui| {
                    ui.label(RichText::new(&v.mountpoint).monospace().small());
                });
                r.col(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if icon(ui, "\u{E3D0}", "Spustit kontejner s tímto volume (lze připojit do více kontejnerů najednou)") {
                        run_with = Some(v.name.clone());
                    }
                    if icon(ui, "\u{E24A}", "Procházet obsah (stránka Soubory, jako root)") {
                        browse = Some(v.mountpoint.clone());
                    }
                    if icon(ui, "\u{E4A6}", "Odstranit") {
                        app.ask(format!("Odstranit volume {}?", v.name), "Data ve volume budou nenávratně smazána!", Cmd::RemoveVolume(v.name.clone()));
                    }
                });
            });
        }
    });
    if let Some(name) = run_with {
        // předvyplnit dialog: volume připojený na /data, image doplní uživatel
        app.run = crate::worker::RunSpec { restart: "no".into(), volumes: format!("{name}:/data"), ..Default::default() };
        app.run_vol = (String::new(), "/data".into(), false);
        app.run_open = true;
    }
    if let Some(p) = browse {
        app.files.root = true;
        super::files::navigate(app, p);
        app.page = crate::app::Page::Files;
    }
}
