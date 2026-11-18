use super::*;
use crate::app::App;
use crate::worker::{AddDistroSpec, Cmd, WslCmd};

/// Výchozí jméno uživatele pro novou distribuci: Windows uživatel, malými písmeny, jen [a-z0-9_-].
fn default_user() -> String {
    let w = std::env::var("USERNAME").unwrap_or_default().to_lowercase();
    let s: String = w.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    if s.chars().next().map(|c| c.is_ascii_lowercase()).unwrap_or(false) { s } else { "user".into() }
}

/// Otevře dialog „Přidat distribuci“ (a načte seznam dostupných distribucí).
pub fn open_add_distro(app: &mut App) {
    let engine = app.cfg.read().map(|c| c.engine).ok();
    app.add_distro = Some(AddDistroSpec { user: default_user(), engine, use_for_app: true, ..Default::default() });
    if !matches!(app.online, Some(Ok(_))) {
        app.online = None;
        app.send(Cmd::ListOnline);
    }
}

fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn valid_user(s: &str) -> bool {
    s.is_empty()
        || (s.chars().next().map(|c| c.is_ascii_lowercase() || c == '_').unwrap_or(false)
            && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'))
}

/// Dialog „Přidat distribuci“.
pub fn add_distro_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(mut spec) = app.add_distro.take() else { return };
    let existing: Vec<String> = app.distros.iter().map(|d| d.name.to_lowercase()).collect();
    let online = app.online.clone();
    let mut submit = false;
    let mut cancel = false;
    let (_, esc) = modal(ctx, "add-distro", t!("Přidat distribuci", "Add distribution"), 520.0, |ui| {
        egui::Grid::new("add-distro-grid").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
            ui.label(t!("Distribuce", "Distribution"));
            match &online {
                None => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(muted(t!("načítám seznam…", "loading list…")));
                    });
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(format!("\u{E4E0} {e}")).color(theme::RED));
                }
                Some(Ok(list)) => {
                    let shown = list.iter().find(|(n, _)| *n == spec.distro).map(|(_, f)| f.clone()).unwrap_or_else(|| t!("vyber…", "choose…").to_string());
                    egui::ComboBox::from_id_salt("online-distro").width(300.0).selected_text(shown).show_ui(ui, |ui| {
                        for (n, f) in list {
                            if ui.selectable_label(spec.distro == *n, format!("{f}  ({n})")).clicked() {
                                spec.distro = n.clone();
                            }
                        }
                    });
                }
            }
            ui.end_row();
            ui.label(t!("Název instance", "Instance name"));
            ui.add(field(&mut spec.name).hint_text(if spec.distro.is_empty() { "Debian-dev" } else { spec.distro.as_str() }).desired_width(300.0));
            ui.end_row();
            ui.label(t!("Uživatel", "User"));
            ui.add(field(&mut spec.user).hint_text(t!("prázdné = jen root", "empty = root only")).desired_width(300.0));
            ui.end_row();
            ui.label(t!("Heslo", "Password"));
            ui.add(field(&mut spec.password).password(true).hint_text(t!("volitelné (sudo funguje i bez něj)", "optional (sudo works without it)")).desired_width(300.0));
            ui.end_row();
            ui.label(t!("Engine", "Engine"));
            ui.horizontal(|ui| {
                ui.radio_value(&mut spec.engine, None, t!("žádný", "none"));
                for e in crate::engine::Engine::ALL {
                    ui.radio_value(&mut spec.engine, Some(e), e.name());
                }
            });
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut spec.use_for_app, t!("Použít pro aplikaci (přepnout a nastavit API enginu)", "Use for the app (switch and set up the engine API)"));
            ui.end_row();
        });
        ui.add_space(8.0);
        let name = if spec.name.trim().is_empty() { spec.distro.clone() } else { spec.name.trim().to_string() };
        let mut problems: Vec<&str> = Vec::new();
        if spec.distro.is_empty() {
            problems.push(t!("Vyber distribuci.", "Choose a distribution."));
        }
        if !name.is_empty() && !valid_name(&name) {
            problems.push(t!("Název smí obsahovat jen písmena, číslice, - _ .", "The name may contain only letters, digits, - _ ."));
        }
        if existing.contains(&name.to_lowercase()) {
            problems.push(t!("Distribuce s tímto názvem už existuje – zadej jiný název instance.", "A distribution with this name already exists – enter another instance name."));
        }
        if !valid_user(spec.user.trim()) {
            problems.push(t!("Uživatel: malá písmena, číslice, - a _, začíná písmenem.", "User: lowercase letters, digits, - and _, starting with a letter."));
        }
        for p in &problems {
            ui.label(RichText::new(format!("\u{E4E0} {p}")).color(theme::YELLOW).small());
        }
        ui.label(
            muted(t!(
                "Na pozadí: wsl --install --no-launch, zapnutí systemd, vytvoření uživatele (sudo bez hesla), restart distribuce a podle volby instalace enginu. Stahování může trvat několik minut. Bez „Použít pro aplikaci“ se engine jen nainstaluje – API může mít vždy jen jedna distribuce (sdílí síť).",
                "In the background: wsl --install --no-launch, enabling systemd, creating the user (passwordless sudo), restarting the distribution and optionally installing the engine. Downloading may take several minutes. Without \"Use for the app\" the engine is only installed – only one distribution can expose the API (they share the network)."
            ))
            .small(),
        );
        let (ok, c) = dialog_buttons(ui, t!("\u{E20C} Nainstalovat", "\u{E20C} Install"), false, problems.is_empty());
        submit = ok;
        cancel = c;
    });
    if submit {
        app.send(Cmd::AddDistro(spec));
    } else if !(cancel || esc) {
        app.add_distro = Some(spec);
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(ui, "Nastavení", "");

    // jazyk se mění okamžitě (bez tlačítka Uložit)
    section(ui, "Jazyk / Language");
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let cur_lang = app.cfg.read().map(|c| c.language.clone()).unwrap_or_default();
        let mut lang = cur_lang.clone();
        ui.horizontal(|ui| {
            ui.label(t!("Jazyk rozhraní", "Interface language"));
            egui::ComboBox::from_id_salt("lang")
                .selected_text(crate::i18n::LANGUAGES.iter().find(|(c, _)| *c == lang).map(|(_, n)| *n).unwrap_or("Čeština"))
                .show_ui(ui, |ui| {
                    for (code, name) in crate::i18n::LANGUAGES {
                        ui.selectable_value(&mut lang, code.to_string(), name);
                    }
                });
        });
        if lang != cur_lang {
            crate::i18n::set_language(&lang);
            if let Ok(mut g) = app.cfg.write() {
                g.language = lang.clone();
                let _ = g.save();
            }
            app.cfg_draft.language = lang;
            crate::tray::retranslate();
        }
    });

    section(ui, "Aplikace");
    let names: Vec<String> = app.distros.iter().map(|d| d.name.clone()).collect();
    let mut add_distro_requested = false;
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let d = &mut app.cfg_draft;
        egui::Grid::new("appcfg").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
            ui.label("Distribuce s Dockerem");
            let mut add = false;
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("distro").selected_text(d.distro.as_str()).show_ui(ui, |ui| {
                    for n in &names {
                        ui.selectable_value(&mut d.distro, n.clone(), n);
                    }
                });
                if ui.button(t!("\u{E3D4} Přidat distribuci…", "\u{E3D4} Add distribution…")).clicked() {
                    add = true;
                }
            });
            if add {
                add_distro_requested = true;
            }
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
            ui.label("");
            ui.checkbox(
                &mut d.restore_containers,
                t!(
                    "Po startu enginu znovu spustit kontejnery, které naposledy běžely",
                    "After the engine starts, restart the containers that were running last time"
                ),
            )
            .on_hover_text(t!(
                "Aplikace si průběžně pamatuje běžící kontejnery (vypnutí z aplikace se nepočítá) a po připojení k enginu je znovu nahodí. Alternativa: restart policy „unless-stopped“ u kontejneru.",
                "The app keeps track of running containers (stopping the engine from the app does not count) and starts them again once the engine is up. Alternative: an \"unless-stopped\" restart policy on the container."
            ));
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

    if add_distro_requested {
        open_add_distro(app);
    }

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
