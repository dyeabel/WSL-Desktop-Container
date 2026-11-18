use super::*;
use crate::app::App;
use crate::docker::short_id;
use crate::worker::Cmd;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(ui, "Sítě", &format!("{} sítí", app.snap.networks.len()));
    if !need_engine(app, ui) {
        return;
    }
    let mut prune = false;
    toolbar_lr(
        ui,
        240.0,
        |ui, right| if !right {
            ui.add(field(&mut app.new_net).hint_text("název nové sítě").desired_width(200.0));
            egui::ComboBox::from_id_salt("netdrv").selected_text(app.new_net_driver.as_str()).show_ui(ui, |ui| {
                for d in ["bridge", "ipvlan", "macvlan"] {
                    ui.selectable_value(&mut app.new_net_driver, d.to_string(), d);
                }
            });
            if accent_button(ui, "\u{E3D4} Vytvořit").clicked() && !app.new_net.trim().is_empty() {
                app.send(Cmd::CreateNetwork(app.new_net.trim().to_string(), app.new_net_driver.clone()));
                app.new_net.clear();
            }
        } else {
            more_menu(ui, |ui| {
                if ui.button("\u{EC54} Odstranit nepoužívané sítě").clicked() {
                    prune = true;
                    ui.close_menu();
                }
            });
            filter_field(ui, &mut app.filter);
        },
    );
    if prune {
        app.send(Cmd::PruneNetworks);
    }
    ui.add_space(8.0);

    let f = app.filter.to_lowercase();
    let nets: Vec<_> = app.snap.networks.iter().filter(|n| f.is_empty() || n.name.to_lowercase().contains(&f)).cloned().collect();
    let containers = app.snap.containers.clone();
    let cols = [
        ("NÁZEV", W::Fr(2.0)),
        ("ID", W::PxOpt(120.0)),
        ("DRIVER", W::Px(110.0)),
        ("SUBNET", W::FrOpt(1.5)),
        ("KONTEJNERY", W::Fr(2.0)),
        ("AKCE", W::Px(80.0)),
    ];
    table(ui, "nets", &cols, |mut body| {
        for n in &nets {
            let used: Vec<String> = containers.iter().filter(|c| c.networks().contains(&n.name)).map(|c| c.name()).collect();
            body.row(34.0, |mut r| {
                r.col(|ui| {
                    ui.label(RichText::new(&n.name).strong());
                });
                r.col(|ui| {
                    ui.label(RichText::new(short_id(&n.id)).monospace().color(theme::MUTED));
                });
                r.col(|ui| {
                    ui.label(format!("{} · {}", n.driver, n.scope));
                });
                r.col(|ui| {
                    ui.label(n.subnet());
                });
                r.col(|ui| {
                    if used.is_empty() {
                        ui.label(muted("—"));
                    } else {
                        ui.label(used.join(", "));
                    }
                });
                r.col(|ui| {
                    if n.builtin() {
                        ui.label(muted("vestavěná").small());
                    } else if icon(ui, "\u{E4A6}", "Odstranit") {
                        app.ask(format!("Odstranit síť {}?", n.name), "Síť bude odstraněna.", Cmd::RemoveNetwork(n.id.clone()));
                    }
                });
            });
        }
    });
}
