use super::*;
use crate::app::App;
use crate::worker::{Cmd, WslCmd};

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(ui, "Nastavení", "");

    section(ui, "Aplikace");
    let names: Vec<String> = app.distros.iter().map(|d| d.name.clone()).collect();
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let d = &mut app.cfg_draft;
        egui::Grid::new("appcfg").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
            ui.label("Distribuce s Dockerem");
            egui::ComboBox::from_id_salt("distro").selected_text(d.distro.as_str()).show_ui(ui, |ui| {
                for n in &names {
                    ui.selectable_value(&mut d.distro, n.clone(), n);
                }
            });
            ui.end_row();
            ui.label("Port Docker API");
            ui.add(egui::DragValue::new(&mut d.port).range(1024..=65535));
            ui.end_row();
            ui.label("Obnovovat každých");
            ui.add(egui::DragValue::new(&mut d.refresh_secs).range(1..=120).suffix(" s"));
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut d.keep_alive, "Udržovat distribuci naživu (doporučeno)");
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut d.autostart_engine, "Při spuštění aplikace nastartovat Docker");
            ui.end_row();
        });
        ui.add_space(6.0);
        let cur = app.cfg.read().map(|c| c.clone()).unwrap_or_default();
        let changed = cur != app.cfg_draft;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(changed, |ui| {
                if accent_button(ui, "\u{E248} Uložit").clicked() {
                    let port_changed = cur.port != app.cfg_draft.port;
                    let c = app.cfg_draft.clone();
                    app.save_config(c);
                    if port_changed {
                        app.toast(true, "Port změněn – na stránce WSL engine klikni „Nastavit TCP API“");
                    }
                }
                if ui.button("Zahodit změny").clicked() {
                    app.cfg_draft = cur.clone();
                }
            });
        });
        ui.label(muted("Při změně distribuce nebo portu je potřeba v nové distribuci jednou spustit „Nastavit TCP API“ (stránka WSL engine).").small());
    });

    section(ui, "Oznamovací oblast (tray)");
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let mut c = app.cfg.read().map(|c| c.clone()).unwrap_or_default();
        let before = (c.close_to_tray, c.minimize_to_tray, c.start_hidden);
        ui.checkbox(&mut c.close_to_tray, "Zavření okna (křížek) aplikaci jen schová do oznamovací oblasti");
        ui.checkbox(&mut c.minimize_to_tray, "Minimalizace schová okno do oznamovací oblasti");
        ui.checkbox(&mut c.start_hidden, "Spouštět rovnou schované v oznamovací oblasti");
        if (c.close_to_tray, c.minimize_to_tray, c.start_hidden) != before {
            let _ = c.save();
            crate::tray::sync_checks(c.close_to_tray, c.minimize_to_tray);
            app.cfg_draft.close_to_tray = c.close_to_tray;
            app.cfg_draft.minimize_to_tray = c.minimize_to_tray;
            app.cfg_draft.start_hidden = c.start_hidden;
            if let Ok(mut g) = app.cfg.write() {
                *g = c;
            }
        }
        ui.label(muted("Aplikaci úplně ukončíš přes pravý klik na ikonu v oznamovací oblasti \u{E06C} Ukončit. Levý klik okno zobrazí nebo schová.").small());
    });

    section(ui, "Příkaz docker ve Windows");
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(
            "Malý docker.exe, který příkazy předává Dockeru ve WSL (docker ps, docker compose up, docker run -it …). \
             Windows cesty v argumentech (C:\\…, .\\data) se přeloží automaticky a příkaz běží v aktuální složce. \
             Nejde o Docker Desktop ani o Docker CLI pro Windows.",
        );
        ui.add_space(6.0);
        let checking = app.shim_checking.load(std::sync::atomic::Ordering::Relaxed);
        let st = app.shim.lock().ok().and_then(|g| g.clone());
        if st.is_none() && !checking {
            app.shim_action(ui.ctx(), crate::shim_install::Action::Check);
        }
        match &st {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(muted("Zjišťuji stav…"));
                });
            }
            Some(s) => {
                let dir = crate::shim_install::bin_dir().display().to_string();
                if s.installed && s.in_path {
                    ui.label(RichText::new(format!("\u{E184} Nainstalováno v {dir} a přidáno do PATH")).color(theme::GREEN));
                } else if s.installed {
                    ui.label(RichText::new(format!("\u{E4E0} Soubory jsou v {dir}, ale složka chybí v PATH")).color(theme::YELLOW));
                } else {
                    ui.label(muted("Není nainstalováno."));
                }
                for o in &s.other_docker {
                    ui.label(RichText::new(format!("\u{E4E0} V PATH je i jiný docker: {o} – může mít přednost")).color(theme::YELLOW).small());
                }
                if !s.source_ok {
                    ui.label(RichText::new("\u{E4E0} docker.exe nebyl nalezen vedle aplikace (sestav projekt přes cargo build --release).").color(theme::RED).small());
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!checking && s.source_ok, |ui| {
                        let label = if s.installed { "\u{E094} Aktualizovat" } else { "\u{E20C} Nainstalovat do PATH" };
                        if accent_button(ui, label).clicked() {
                            app.shim_action(ui.ctx(), crate::shim_install::Action::Install);
                        }
                    });
                    ui.add_enabled_ui(!checking && (s.installed || s.in_path), |ui| {
                        if ui.button("\u{E4A6} Odinstalovat").clicked() {
                            app.shim_action(ui.ctx(), crate::shim_install::Action::Uninstall);
                        }
                    });
                    if checking {
                        ui.spinner();
                    }
                });
                ui.label(muted("Po instalaci otevři nový terminál. Ladění: proměnná WLS_SHIM_DEBUG=1 vypíše, co se spouští ve WSL.").small());
            }
        }
    });

    section(ui, "WSL – globální nastavení (%USERPROFILE%\\.wslconfig)");
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let w = &mut app.wsl_draft;
        egui::Grid::new("wslcfg").num_columns(3).spacing([16.0, 10.0]).show(ui, |ui| {
            ui.label("Paměť (memory)");
            ui.add(field(&mut w.memory).hint_text("např. 8GB").desired_width(140.0));
            ui.label(muted("prázdné = výchozí (50 % RAM)").small());
            ui.end_row();
            ui.label("Procesory (processors)");
            ui.add(field(&mut w.processors).hint_text("např. 4").desired_width(140.0));
            ui.label(muted("prázdné = všechny").small());
            ui.end_row();
            ui.label("Swap");
            ui.add(field(&mut w.swap).hint_text("např. 2GB nebo 0").desired_width(140.0));
            ui.label(muted("").small());
            ui.end_row();
            ui.label("Uspání VM po nečinnosti");
            ui.add(field(&mut w.idle_timeout).hint_text("ms, např. 60000").desired_width(140.0));
            ui.label(muted("vmIdleTimeout – po kolika ms bez distribucí se vypne VM").small());
            ui.end_row();
            ui.label("Síťový režim");
            egui::ComboBox::from_id_salt("netmode")
                .selected_text(if w.networking.is_empty() { "výchozí (NAT)" } else { w.networking.as_str() })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut w.networking, String::new(), "výchozí (NAT)");
                    ui.selectable_value(&mut w.networking, "mirrored".to_string(), "mirrored");
                    ui.selectable_value(&mut w.networking, "nat".to_string(), "nat");
                });
            ui.label(muted("mirrored = sdílí síť s Windows").small());
            ui.end_row();
        });
        ui.add_space(6.0);
        let changed = app.wsl_draft != app.wsl_saved;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(changed, |ui| {
                if accent_button(ui, "\u{E248} Uložit .wslconfig").clicked() {
                    match app.wsl_draft.save() {
                        Ok(()) => {
                            app.wsl_saved = app.wsl_draft.clone();
                            app.toast(true, ".wslconfig uložen – projeví se po restartu WSL");
                        }
                        Err(e) => app.toast(false, format!(".wslconfig nelze uložit: {e}")),
                    }
                }
            });
            if ui.button("\u{E3DA} Restartovat WSL (wsl --shutdown)").clicked() {
                app.ask("Restartovat WSL?", "Všechny distribuce se ukončí. Docker pak spustíš tlačítkem Spustit engine.", Cmd::Wsl(WslCmd::Shutdown));
            }
        });
    });

    section(ui, "O aplikaci");
    ui.label(muted(format!(
        "WLS Desktop {} · Rust + egui · Docker API přes tcp://127.0.0.1 · konfigurace v %APPDATA%\\WLS_Desktop",
        env!("CARGO_PKG_VERSION")
    )));
}
