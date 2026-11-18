use super::*;
use crate::app::App;
use crate::worker::{Cmd, Term, WslCmd};

/// Dialog odinstalace enginu (s volbou smazat i data).
pub fn uninstall_dialog(app: &mut App, ctx: &egui::Context) {
    let Some((engine, mut purge)) = app.uninstall else { return };
    let distro = app.distro();
    let ename = engine.name();
    let mut run = false;
    let mut close = false;
    let (_, esc) = modal(ctx, "uninstall-engine", &format!("Odinstalovat {ename} z {distro}?"), 460.0, |ui| {
        ui.label(format!(
            "Zastaví a vypne služby {ename}, odebere nastavení TCP API a přes apt odstraní balíčky. Běžící kontejnery se zastaví."
        ));
        ui.add_space(8.0);
        ui.checkbox(&mut purge, "Smazat i data (image, kontejnery, volume)");
        if purge {
            ui.label(RichText::new(format!("\u{E4E0} Nenávratně se smaže: {}", engine.data_dirs())).color(theme::RED).small());
        } else {
            ui.label(muted(format!("Data zůstanou v {} – při nové instalaci se použijí znovu.", engine.data_dirs())).small());
        }
        let (ok, cancel) = dialog_buttons(ui, if purge { "Odinstalovat i s daty" } else { "Odinstalovat" }, true, true);
        run = ok;
        close = ok || cancel;
    });
    if run {
        app.send(Cmd::UninstallEngine { engine, purge });
    }
    app.uninstall = if close || esc { None } else { Some((engine, purge)) };
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let distro = app.distro();
    let engine = app.cfg.read().map(|c| c.engine).unwrap_or_default();
    let ename = engine.name();
    header(ui, "WSL engine", "Řízení WSL distribucí a kontejnerového enginu (Docker / Podman)");

    // --- kontejnerový engine ---
    let dstate = app.current_distro().map(|d| d.state.clone()).unwrap_or_else(|| "nenalezena".into());
    let running = app.snap.connected;
    let port = app.cfg.read().map(|c| c.port).unwrap_or(2375);
    card(ui, |ui| {
        ui.set_width(ui.available_width());

        // hlavička: stav
        ui.horizontal(|ui| {
            dot(ui, if running { theme::GREEN } else { theme::GREY });
            ui.label(RichText::new(ename).size(20.0).strong());
            badge(ui, if running { "běží" } else { "zastaven" }, if running { theme::GREEN } else { theme::GREY });
            ui.label(muted(format!("v distribuci {distro} ({dstate})")));
        });
        let mut info: Vec<String> = Vec::new();
        if let Some(i) = &app.snap.info {
            info.push(format!("verze {}", i.server_version));
            info.push(i.operating_system.clone());
            info.push(format!("{} CPU · {}", i.ncpu, fmt_bytes(i.mem_total.max(0) as u64)));
        }
        ui.horizontal_wrapped(|ui| {
            if !info.is_empty() {
                ui.label(muted(info.join(" · ")));
                ui.label(muted("·"));
            }
            match app.tcp_configured {
                Some(true) => ui.label(RichText::new(format!("\u{E184} API na 127.0.0.1:{port}")).color(theme::GREEN)),
                Some(false) => ui.label(RichText::new("\u{E4E0} API není nastavené").color(theme::YELLOW)),
                None => ui.label(muted("kontroluji API…")),
            };
        });

        // denní provoz
        ui.add_space(8.0);
        toolbar(ui, |ui| {
            if running {
                if ui.button(format!("\u{E46C} Zastavit")).clicked() {
                    app.send(Cmd::EngineStop);
                }
                if ui.button("\u{E094} Restartovat").clicked() {
                    app.send(Cmd::EngineRestart);
                }
            } else if accent_button(ui, &format!("\u{E3D0} Spustit {ename}")).clicked() {
                app.send(Cmd::EngineStart);
            }
            if ui.button("\u{E330} Uspat distribuci").on_hover_text("Ukončí distribuci a uvolní RAM").clicked() {
                app.ask(
                    format!("Uspat {distro}?"),
                    "Distribuce bude ukončena, všechny kontejnery se zastaví. Kontejnery s restart policy se po probuzení spustí znovu.",
                    Cmd::Sleep,
                );
            }
            if ui.button("\u{EAE8} Terminál").on_hover_text(format!("Shell v distribuci {distro}")).clicked() {
                app.send(Cmd::Terminal(Term::Distro(distro.clone())));
            }
        });

        // instalace a nastavení (sbalené, pokud je vše v pořádku)
        ui.add_space(6.0);
        ui.separator();
        // pro ladění/screenshoty: WLS_DEBUG_EXPAND=1
        let open_default = app.tcp_configured == Some(false) || std::env::var("WLS_DEBUG_EXPAND").is_ok();
        egui::CollapsingHeader::new(RichText::new("Instalace a nastavení enginu").strong())
            .id_salt(("engine-setup", app.tcp_configured == Some(false)))
            .default_open(open_default)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(muted("Engine:"));
                    // přepínač Docker | Podman
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for e in crate::engine::Engine::ALL {
                        let sel = engine == e;
                        let text = RichText::new(e.name()).color(if sel { theme::ACCENT_TEXT } else { ui.visuals().text_color() });
                        let b = egui::Button::new(text)
                            .fill(if sel { theme::ACCENT } else { ui.visuals().widgets.inactive.weak_bg_fill })
                            .min_size(egui::vec2(84.0, 26.0));
                        if ui.add(b).clicked() && !sel {
                            let mut c = app.cfg.read().map(|c| c.clone()).unwrap_or_default();
                            c.engine = e;
                            app.save_config(c);
                            app.tcp_configured = None;
                            app.shared.tcp_checked.store(false, std::sync::atomic::Ordering::SeqCst);
                            app.toast(true, format!("Engine přepnut na {} – pokud ještě není nainstalovaný, klikni „Nainstalovat a nastavit“", e.name()));
                        }
                    }
                });
                ui.add_space(6.0);
                toolbar(ui, |ui| {
                    if ui
                        .button(format!("\u{E20C} Nainstalovat a nastavit"))
                        .on_hover_text("apt install (Debian/Ubuntu), zapnutí systemd (s restartem distribuce) a nastavení API")
                        .clicked()
                    {
                        app.ask(
                            format!("Nainstalovat {ename} do {distro}?"),
                            format!(
                                "Do distribuce {distro} se přes apt nainstaluje {ename}. Pokud neběží systemd, zapne se v /etc/wsl.conf a distribuce se restartuje (kontejnery se zastaví). \
                                 Nakonec se nastaví API na 127.0.0.1:{port} – {} se přitom vypne, aby nebojoval o port.",
                                if engine == crate::engine::Engine::Docker { "Podman API" } else { "Docker" }
                            ),
                            Cmd::InstallEngine(engine),
                        );
                    }
                    if ui.button("\u{E5D4} Nastavit jen API").on_hover_text("Engine už je nainstalovaný, jen zpřístupnit API na TCP").clicked() {
                        app.ask(
                            format!("Nastavit {ename} API?"),
                            format!(
                                "V distribuci {distro} se vytvoří {} a {ename} se restartuje. API bude dostupné bez autentizace na 127.0.0.1:{port} (jen lokálně).",
                                engine.api_marker()
                            ),
                            Cmd::SetupEngine,
                        );
                    }
                    if ui.button(RichText::new(format!("\u{E4A6} Odinstalovat…")).color(theme::RED)).clicked() {
                        app.uninstall = Some((engine, false));
                    }
                });
                if engine == crate::engine::Engine::Podman {
                    ui.label(muted("Podman běží jako služba wls-podman-api; CLI v aplikaci i docker shim volají podman --url na stejné API. Pro compose se při instalaci přidá docker-compose jako provider.").small());
                }

                // příkaz docker ve Windows (shim v PATH)
                ui.add_space(8.0);
                ui.label(RichText::new("Příkaz docker ve Windows").strong());
                let checking = app.shim_checking.load(std::sync::atomic::Ordering::Relaxed);
                let st = app.shim.lock().ok().and_then(|g| g.clone());
                if st.is_none() && !checking {
                    app.shim_action(ui.ctx(), crate::shim_install::Action::Check);
                }
                toolbar(ui, |ui| {
                    match &st {
                        None => {
                            ui.spinner();
                            ui.label(muted("zjišťuji…"));
                        }
                        Some(s) => {
                            if s.installed && s.in_path {
                                badge(ui, "v PATH", theme::GREEN);
                            } else {
                                badge(ui, "není v PATH", theme::GREY);
                            }
                            ui.add_enabled_ui(!checking && s.source_ok, |ui| {
                                let label = if s.installed && s.in_path { "\u{E094} Aktualizovat" } else { "\u{E3D4} Přidat do PATH" };
                                if ui.button(label).on_hover_text(format!("docker.exe a docker-compose.exe do {}", crate::shim_install::bin_dir().display())).clicked() {
                                    app.shim_action(ui.ctx(), crate::shim_install::Action::Install);
                                }
                            });
                            ui.add_enabled_ui(!checking && (s.installed || s.in_path), |ui| {
                                if ui.button("\u{E4A6} Odebrat z PATH").clicked() {
                                    app.shim_action(ui.ctx(), crate::shim_install::Action::Uninstall);
                                }
                            });
                            if checking {
                                ui.spinner();
                            }
                        }
                    }
                });
                ui.label(muted(format!("Příkaz docker … ve Windows se provede přes {ename} v {distro}. Po změně otevři nový terminál.")).small());
                if let Some(s) = &st {
                    for o in &s.other_docker {
                        ui.label(RichText::new(format!("\u{E4E0} V PATH je i jiný docker: {o}")).color(theme::YELLOW).small());
                    }
                }

                // DOCKER_HOST pro nástroje, které mluví přímo s API
                ui.add_space(8.0);
                ui.label(RichText::new("DOCKER_HOST pro nástroje").strong());
                let want = format!("tcp://127.0.0.1:{port}");
                toolbar(ui, |ui| match &st {
                    None => {
                        ui.spinner();
                    }
                    Some(s) => {
                        match s.docker_host.as_deref() {
                            Some(v) if v == want => {
                                badge(ui, &format!("DOCKER_HOST={v}"), theme::GREEN);
                            }
                            Some(v) => {
                                badge(ui, &format!("DOCKER_HOST={v}"), theme::YELLOW).on_hover_text(format!("Neodpovídá API aplikace ({want})"));
                            }
                            None => {
                                badge(ui, "nenastaveno", theme::GREY);
                            }
                        }
                        ui.add_enabled_ui(!checking && s.docker_host.as_deref() != Some(want.as_str()), |ui| {
                            if ui.button(format!("\u{E3D4} Nastavit na {want}")).clicked() {
                                app.shim_action(ui.ctx(), crate::shim_install::Action::SetDockerHost(port));
                            }
                        });
                        ui.add_enabled_ui(!checking && s.docker_host.is_some(), |ui| {
                            if ui.button("\u{E4A6} Odebrat").clicked() {
                                app.shim_action(ui.ctx(), crate::shim_install::Action::ClearDockerHost);
                            }
                        });
                    }
                });
                ui.label(muted("Pro VS Code Dev Containers, Testcontainers, Docker SDK a další nástroje, které nevolají příkaz docker, ale mluví přímo s API. Projeví se v nově spuštěných programech.").small());
            });
    });

    // --- WSL distribuce ---
    section(ui, "WSL distribuce");
    ui.horizontal(|ui| {
        if ui.button("\u{E094} Obnovit").clicked() {
            app.send(Cmd::Refresh);
        }
        if super::danger_button(ui, "\u{E3DA} Vypnout celé WSL").clicked() {
            app.ask("Vypnout WSL?", "wsl --shutdown ukončí všechny distribuce včetně Dockeru.", Cmd::Wsl(WslCmd::Shutdown));
        }
    });
    ui.add_space(6.0);
    let distros = app.distros.clone();
    let cols = [
        ("", W::Px(14.0)),
        ("DISTRIBUCE", W::Fr(2.0)),
        ("STAV", W::Fr(1.0)),
        ("WSL", W::Px(40.0)),
        ("AKCE", W::Px(250.0)),
    ];
    table(ui, "distros", &cols, |mut body| {
        for d in &distros {
          body.row(34.0, |mut r| {
            r.col(|ui| dot(ui, if d.running() { theme::GREEN } else { theme::GREY }));
            r.col(|ui| {
                ui.label(RichText::new(&d.name).strong());
                if d.default {
                    ui.label(muted("výchozí").small());
                }
                if d.name == distro {
                    // štítek podle zvoleného enginu (Docker / Podman)
                    ui.label(RichText::new(ename).small().color(theme::ACCENT));
                }
            });
            r.col(|ui| {
                ui.label(&d.state);
            });
            r.col(|ui| {
                ui.label(&d.version);
            });
            r.col(|ui| {
                if d.running() {
                    if icon(ui, "\u{E46C}", "Ukončit (wsl --terminate)") {
                        app.ask(format!("Ukončit {}?", d.name), "Všechny procesy v distribuci budou ukončeny.", Cmd::Wsl(WslCmd::Terminate(d.name.clone())));
                    }
                } else if icon(ui, "\u{E3D0}", "Spustit") {
                    app.send(Cmd::Wsl(WslCmd::Start(d.name.clone())));
                }
                if icon(ui, "\u{EAE8}", "Terminál") {
                    app.send(Cmd::Terminal(Term::Distro(d.name.clone())));
                }
                if !d.default && icon(ui, "\u{E46A}", "Nastavit jako výchozí") {
                    app.send(Cmd::Wsl(WslCmd::SetDefault(d.name.clone())));
                }
                if d.name != distro && ui.small_button("Použít pro Docker").clicked() {
                    let mut c = app.cfg.read().map(|c| c.clone()).unwrap_or_default();
                    c.distro = d.name.clone();
                    app.cfg_draft.distro = d.name.clone();
                    app.save_config(c);
                }
            });
          });
        }
    });

    // --- daemon.json (jen Docker) ---
    if engine != crate::engine::Engine::Docker {
        return;
    }
    section(ui, "Konfigurace Docker démona (/etc/docker/daemon.json)");
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        if !app.daemon_loaded {
            ui.horizontal(|ui| {
                if ui.button("\u{E23A} Načíst daemon.json").on_hover_text("Pokud distribuce neběží, spustí ji").clicked() {
                    app.send(Cmd::LoadDaemonJson);
                }
                if app.shared.busy.load(std::sync::atomic::Ordering::Relaxed) > 0 {
                    ui.spinner();
                }
            });
            return;
        }
        ui.add(
            area(&mut app.daemon_json)
                .code_editor()
                .desired_rows(10)
                .desired_width(f32::INFINITY),
        );
        let valid = serde_json::from_str::<serde_json::Value>(&app.daemon_json);
        ui.horizontal(|ui| {
            ui.add_enabled_ui(valid.is_ok(), |ui| {
                if accent_button(ui, "\u{E248} Uložit a restartovat Docker").clicked() {
                    app.ask("Uložit daemon.json?", "Soubor se přepíše a Docker se restartuje (běžící kontejnery bez restart policy se zastaví).", Cmd::SaveDaemonJson(app.daemon_json.clone()));
                }
            });
            if ui.button("\u{E038} Načíst znovu").clicked() {
                app.daemon_loaded = false;
                app.send(Cmd::LoadDaemonJson);
            }
            match valid {
                Ok(_) => ui.label(RichText::new("\u{E182} platný JSON").small().color(theme::GREEN)),
                Err(e) => ui.label(RichText::new(format!("\u{E4E0} {e}")).small().color(theme::RED)),
            };
        });
        ui.label(muted("Příklady klíčů: \"log-driver\", \"log-opts\", \"dns\", \"registry-mirrors\", \"insecure-registries\", \"data-root\", \"default-address-pools\". Klíč \"hosts\" nepoužívej.").small());
    });
}
