use super::*;
use crate::app::App;
use crate::docker::short_id;
use crate::worker::Cmd;

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(ui, t!("Sítě", "Networks"), &tf!("{} sítí", "{} networks", app.snap.networks.len()));
    if !need_engine(app, ui) {
        return;
    }
    if !super::net_overview::tabs(app, ui) {
        return;
    }
    let mut prune = false;
    toolbar_lr(
        ui,
        240.0,
        |ui, right| if !right {
            ui.add(field(&mut app.new_net).hint_text(t!("název nové sítě", "new network name")).desired_width(200.0));
            egui::ComboBox::from_id_salt("netdrv").selected_text(app.new_net_driver.as_str()).show_ui(ui, |ui| {
                for d in ["bridge", "ipvlan", "macvlan"] {
                    ui.selectable_value(&mut app.new_net_driver, d.to_string(), d);
                }
            });
            if accent_button(ui, t!("\u{E3D4} Vytvořit", "\u{E3D4} Create")).clicked() && !app.new_net.trim().is_empty() {
                app.send(Cmd::CreateNetwork(app.new_net.trim().to_string(), app.new_net_driver.clone()));
                app.new_net.clear();
            }
        } else {
            more_menu(ui, |ui| {
                if ui.button(t!("\u{EC54} Odstranit nepoužívané sítě", "\u{EC54} Remove unused networks")).clicked() {
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
    let mut nets: Vec<_> = app.snap.networks.iter().filter(|n| f.is_empty() || n.name.to_lowercase().contains(&f)).cloned().collect();
    let containers = app.snap.containers.clone();
    sort_items(ui.ctx(), "nets", &mut nets, |n, col| match col {
        1 => SortKey::text(&n.id),
        2 => SortKey::text(&n.driver),
        4 => SortKey::num(containers.iter().filter(|c| c.networks().contains(&n.name)).count() as f64),
        _ => SortKey::text(&n.name),
    });
    let cols = [
        (t!("NÁZEV", "NAME"), W::Fr(2.0)),
        ("ID", W::PxOpt(120.0)),
        ("DRIVER", W::Px(110.0)),
        ("SUBNET", W::FrOpt(1.5)),
        (t!("KONTEJNERY", "CONTAINERS"), W::Fr(2.0)),
        (t!("AKCE", "ACTIONS"), W::Px(80.0)),
    ];
    table_sort(ui, "nets", &cols, Sort { key: "nets", cols: &[0, 1, 2, 4] }, |mut body| {
        for n in &nets {
            let used: Vec<String> = containers.iter().filter(|c| c.networks().contains(&n.name)).map(|c| c.name()).collect();
            body.row(34.0, |mut r| {
                r.col(|ui| {
                    ui.label(RichText::new(&n.name).strong());
                });
                r.col(|ui| {
                    ui.label(RichText::new(short_id(&n.id)).monospace().color(theme::muted()));
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
                        ui.label(muted(t!("vestavěná", "built-in")).small());
                    } else if icon(ui, "\u{E4A6}", t!("Odstranit", "Remove")) {
                        app.ask(tf!("Odstranit síť {}?", "Remove network {}?", n.name), t!("Síť bude odstraněna.", "The network will be removed."), Cmd::RemoveNetwork(n.id.clone()));
                    }
                });
            });
        }
    });
    super::net_overview::network_map(app, ui);
}
