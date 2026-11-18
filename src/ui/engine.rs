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
    let (_, esc) = modal(ctx, "uninstall-engine", &tf!("Odinstalovat {ename} z {distro}?", "Uninstall {ename} from {distro}?"), 460.0, |ui| {
        ui.label(tf!(
            "Zastaví a vypne služby {ename}, odebere nastavení TCP API a přes apt odstraní balíčky. Běžící kontejnery se zastaví.",
            "Stops and disables the {ename} services, removes the TCP API setup and uninstalls the packages via apt. Running containers will be stopped."
        ));
        ui.add_space(8.0);
        ui.checkbox(&mut purge, t!("Smazat i data (image, kontejnery, volume)", "Also delete data (images, containers, volumes)"));
        if purge {
            ui.label(RichText::new(tf!("\u{E4E0} Nenávratně se smaže: {}", "\u{E4E0} Permanently deleted: {}", engine.data_dirs())).color(theme::RED).small());
        } else {
            ui.label(muted(tf!("Data zůstanou v {} – při nové instalaci se použijí znovu.", "Data stays in {} – it will be reused on a new install.", engine.data_dirs())).small());
        }
        let (ok, cancel) = dialog_buttons(ui, if purge { t!("Odinstalovat i s daty", "Uninstall with data") } else { t!("Odinstalovat", "Uninstall") }, true, true);
        run = ok;
        close = ok || cancel;
    });
    if run {
        app.send(Cmd::UninstallEngine { engine, purge });
    }
    app.uninstall = if close || esc { None } else { Some((engine, purge)) };
}

/// Dialog „Nainstalovat a nastavit“ s volbou zabezpečení API (TLS je výchozí).
pub fn install_dialog(app: &mut App, ctx: &egui::Context) {
    let Some((engine, mut tls)) = app.install else { return };
    let distro = app.distro();
    let ename = engine.name();
    let port = app.cfg.read().map(|c| c.port).unwrap_or(2375);
    let port = crate::worker::api_port(tls, port);
    let other = if engine == crate::engine::Engine::Docker { "Podman API" } else { "Docker" };
    let mut run = false;
    let mut close = false;
    let (_, esc) = modal(ctx, "install-engine", &tf!("Nainstalovat {ename} do {distro}?", "Install {ename} into {distro}?"), 480.0, |ui| {
        ui.label(tf!(
            "Do distribuce {distro} se přes apt nainstaluje {ename}. Pokud neběží systemd, zapne se v /etc/wsl.conf a distribuce se restartuje (kontejnery se zastaví). \
             Nakonec se nastaví API na 127.0.0.1:{port} – {other} se přitom vypne, aby nebojoval o port.",
            "{ename} will be installed into the {distro} distribution via apt. If systemd is not running, it is enabled in /etc/wsl.conf and the distribution restarts (containers will stop). \
             Finally the API is set up on 127.0.0.1:{port} – {other} is disabled so they do not fight over the port."
        ));
        ui.add_space(8.0);
        ui.checkbox(&mut tls, t!("Zabezpečit API pomocí TLS (doporučeno)", "Secure the API with TLS (recommended)"));
        ui.label(
            muted(if tls {
                t!(
                    "Engine bude jen na unix socketu, na 127.0.0.1 pustí TLS proxy jen klienta s certifikátem aplikace.",
                    "The engine listens only on a unix socket; a TLS proxy on 127.0.0.1 accepts only the app's client certificate."
                )
            } else {
                t!(
                    "API bez ověření může ovládat každý proces, který se dostane na 127.0.0.1.",
                    "Without authentication any process that can reach 127.0.0.1 can control the API."
                )
            })
            .small(),
        );
        let (ok, cancel) = dialog_buttons(ui, t!("\u{E20C} Nainstalovat", "\u{E20C} Install"), false, true);
        run = ok;
        close = ok || cancel;
    });
    if run {
        app.send(Cmd::InstallEngine(engine, tls));
    }
    app.install = if close || esc { None } else { Some((engine, tls)) };
}

/// Potvrzení „Zapnout TLS“ (tlačítko v nastavení i upozornění nahoře).
fn ask_tls_on(app: &mut App) {
    app.ask(
        t!("Zabezpečit API pomocí TLS?", "Secure the API with TLS?"),
        t!(
            "Engine bude poslouchat jen na unix socketu v distribuci a na 127.0.0.1:2376 bude TLS proxy (socat), která pustí jen klienta s certifikátem. \
             Vytvoří se CA a certifikáty, klientský se uloží do %APPDATA%\\WLS_Desktop\\tls. Příkaz docker ve Windows i CLI ve WSL půjdou přes socket. \
             Engine se restartuje.",
            "The engine will listen only on a unix socket in the distribution and a TLS proxy (socat) on 127.0.0.1:2376 will accept only clients with a certificate. \
             A CA and certificates are created; the client one is saved to %APPDATA%\\WLS_Desktop\\tls. The docker command in Windows and the CLI in WSL will use the socket. \
             The engine will restart."
        ),
        Cmd::SetTls(true),
    );
}

/// Jednorázové (zavíratelné) upozornění, že API běží bez ověření.
fn security_hint(app: &mut App, ui: &mut egui::Ui, tls: bool) {
    let dismissed = app.cfg.read().map(|c| c.security_hint_dismissed).unwrap_or(true);
    if tls || dismissed || app.tcp_configured != Some(true) {
        return;
    }
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(t!("\u{E4E0} API enginu běží bez ověření", "\u{E4E0} The engine API runs without authentication")).color(theme::YELLOW).strong());
        ui.label(muted(t!(
            "Každý proces, který se dostane na 127.0.0.1 (i jiné WSL distribuce, kontejnery se sítí host nebo webová stránka v prohlížeči), může engine ovládat jako root. TLS s klientským certifikátem tomu zabrání.",
            "Any process that can reach 127.0.0.1 (including other WSL distributions, host-network containers or a web page in the browser) can control the engine as root. TLS with a client certificate prevents that."
        )));
        ui.add_space(4.0);
        toolbar(ui, |ui| {
            if accent_button(ui, t!("\u{E2FA} Zapnout TLS", "\u{E2FA} Turn TLS on")).clicked() {
                ask_tls_on(app);
            }
            if ui.button(t!("Už nezobrazovat", "Don't show again")).clicked() {
                let res = match app.cfg.write() {
                    Ok(mut g) => {
                        g.security_hint_dismissed = true;
                        g.save()
                    }
                    Err(e) => Err(e.to_string()),
                };
                if let Err(e) = res {
                    app.toast(false, tf!("Nastavení nelze uložit: {e}", "Cannot save settings: {e}"));
                }
                app.cfg_draft.security_hint_dismissed = true;
            }
        });
    });
    ui.add_space(8.0);
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let distro = app.distro();
    let engine = app.cfg.read().map(|c| c.engine).unwrap_or_default();
    let ename = engine.name();
    header(ui, "WSL engine", t!("Řízení WSL distribucí a kontejnerového enginu (Docker / Podman)", "Manage WSL distributions and the container engine (Docker / Podman)"));

    // --- kontejnerový engine ---
    let dstate = app.current_distro().map(|d| d.state.clone()).unwrap_or_else(|| t!("nenalezena", "not found").into());
    let running = app.snap.connected;
    let port = app.cfg.read().map(|c| c.port).unwrap_or(2375);
    let tls = app.cfg.read().map(|c| c.tls).unwrap_or(false);
    let npipe_on = app.cfg.read().map(|c| c.npipe).unwrap_or(false);
    security_hint(app, ui, tls);
    card(ui, |ui| {
        ui.set_width(ui.available_width());

        // hlavička: stav
        ui.horizontal(|ui| {
            dot(ui, if running { theme::GREEN } else { theme::GREY });
            ui.label(RichText::new(ename).size(20.0).strong());
            badge(ui, if running { t!("běží", "running") } else { t!("zastaven", "stopped") }, if running { theme::GREEN } else { theme::GREY });
            ui.label(muted(tf!("v distribuci {distro} ({dstate})", "in distribution {distro} ({dstate})")));
        });
        let mut info: Vec<String> = Vec::new();
        if let Some(i) = &app.snap.info {
            info.push(tf!("verze {}", "version {}", i.server_version));
            info.push(i.operating_system.clone());
            info.push(format!("{} CPU · {}", i.ncpu, fmt_bytes(i.mem_total.max(0) as u64)));
        }
        ui.horizontal_wrapped(|ui| {
            if !info.is_empty() {
                ui.label(muted(info.join(" · ")));
                ui.label(muted("·"));
            }
            match app.tcp_configured {
                Some(true) => ui.label(RichText::new(tf!("\u{E184} API na 127.0.0.1:{port}", "\u{E184} API on 127.0.0.1:{port}")).color(theme::GREEN)),
                Some(false) => ui.label(RichText::new(t!("\u{E4E0} API není nastavené", "\u{E4E0} API is not configured")).color(theme::YELLOW)),
                None => ui.label(muted(t!("kontroluji API…", "checking API…"))),
            };
        });

        // denní provoz
        ui.add_space(8.0);
        toolbar(ui, |ui| {
            if running {
                if ui.button(t!("\u{E46C} Zastavit", "\u{E46C} Stop")).clicked() {
                    app.send(Cmd::EngineStop);
                }
                if ui.button(t!("\u{E094} Restartovat", "\u{E094} Restart")).clicked() {
                    app.send(Cmd::EngineRestart);
                }
            } else if accent_button(ui, &tf!("\u{E3D0} Spustit {ename}", "\u{E3D0} Start {ename}")).clicked() {
                app.send(Cmd::EngineStart);
            }
            if ui.button(t!("\u{E330} Uspat distribuci", "\u{E330} Put to sleep")).on_hover_text(t!("Ukončí distribuci a uvolní RAM", "Terminates the distribution and frees RAM")).clicked() {
                app.ask(
                    tf!("Uspat {distro}?", "Put {distro} to sleep?"),
                    t!(
                        "Distribuce bude ukončena, všechny kontejnery se zastaví. Kontejnery s restart policy se po probuzení spustí znovu.",
                        "The distribution will be terminated and all containers stopped. Containers with a restart policy start again after wake-up."
                    ),
                    Cmd::Sleep,
                );
            }
            if ui.button(t!("\u{EAE8} Terminál", "\u{EAE8} Terminal")).on_hover_text(tf!("Shell v distribuci {distro}", "Shell in distribution {distro}")).clicked() {
                app.send(Cmd::Terminal(Term::Distro(distro.clone())));
            }
        });

        // instalace a nastavení (sbalené, pokud je vše v pořádku)
        ui.add_space(6.0);
        ui.separator();
        // pro ladění/screenshoty: WLS_DEBUG_EXPAND=1
        let open_default = app.tcp_configured == Some(false) || std::env::var("WLS_DEBUG_EXPAND").is_ok();
        egui::CollapsingHeader::new(RichText::new(t!("Instalace a nastavení enginu", "Engine installation and setup")).strong())
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
                            app.toast(true, tf!("Engine přepnut na {} – pokud ještě není nainstalovaný, klikni „Nainstalovat a nastavit“", "Engine switched to {} – if it is not installed yet, click “Install and set up”", e.name()));
                        }
                    }
                });
                ui.add_space(6.0);
                toolbar(ui, |ui| {
                    if ui
                        .button(t!("\u{E20C} Nainstalovat a nastavit", "\u{E20C} Install and set up"))
                        .on_hover_text(t!(
                            "apt install (Debian/Ubuntu), zapnutí systemd (s restartem distribuce) a nastavení API",
                            "apt install (Debian/Ubuntu), enabling systemd (restarts the distribution) and API setup"
                        ))
                        .clicked()
                    {
                        // TLS je pro nové instalace výchozí
                        app.install = Some((engine, true));
                    }
                    if ui.button(t!("\u{E5D4} Nastavit jen API", "\u{E5D4} Set up API only")).on_hover_text(t!("Engine už je nainstalovaný, jen zpřístupnit API na TCP", "The engine is already installed, just expose the API over TCP")).clicked() {
                        app.ask(
                            tf!("Nastavit {ename} API?", "Set up the {ename} API?"),
                            tf!(
                                "V distribuci {distro} se vytvoří {} a {ename} se restartuje. API bude na 127.0.0.1:{port} {}.",
                                "{} will be created in the {distro} distribution and {ename} restarted. The API will be on 127.0.0.1:{port} {}.",
                                engine.api_marker(),
                                if tls { t!("jen s klientským certifikátem (TLS)", "with a client certificate only (TLS)") } else { t!("bez autentizace (jen lokálně)", "without authentication (local only)") }
                            ),
                            Cmd::SetupEngine,
                        );
                    }
                    if ui.button(RichText::new(t!("\u{E4A6} Odinstalovat…", "\u{E4A6} Uninstall…")).color(theme::RED)).clicked() {
                        app.uninstall = Some((engine, false));
                    }
                });
                if engine == crate::engine::Engine::Podman {
                    ui.label(muted(t!("Podman běží jako služba wls-podman-api; CLI v aplikaci i docker shim volají podman --url na stejné API. Pro compose se při instalaci přidá docker-compose jako provider.", "Podman runs as the wls-podman-api service; the in-app CLI and the docker shim call podman --url against the same API. For compose, docker-compose is added as the provider during installation.")).small());
                }

                // příkaz docker ve Windows (shim v PATH)
                ui.add_space(8.0);
                ui.label(RichText::new(t!("Příkaz docker ve Windows", "The docker command in Windows")).strong());
                let checking = app.shim_checking.load(std::sync::atomic::Ordering::Relaxed);
                let st = app.shim.lock().ok().and_then(|g| g.clone());
                if st.is_none() && !checking {
                    app.shim_action(ui.ctx(), crate::shim_install::Action::Check);
                }
                toolbar(ui, |ui| {
                    match &st {
                        None => {
                            ui.spinner();
                            ui.label(muted(t!("zjišťuji…", "checking…")));
                        }
                        Some(s) => {
                            if s.installed && s.in_path {
                                badge(ui, t!("v PATH", "in PATH"), theme::GREEN);
                            } else {
                                badge(ui, t!("není v PATH", "not in PATH"), theme::GREY);
                            }
                            ui.add_enabled_ui(!checking && s.source_ok, |ui| {
                                let label = if s.installed && s.in_path { t!("\u{E094} Aktualizovat", "\u{E094} Update") } else { t!("\u{E3D4} Přidat do PATH", "\u{E3D4} Add to PATH") };
                                if ui.button(label).on_hover_text(tf!("docker.exe a docker-compose.exe do {}", "docker.exe and docker-compose.exe into {}", crate::shim_install::bin_dir().display())).clicked() {
                                    app.shim_action(ui.ctx(), crate::shim_install::Action::Install);
                                }
                            });
                            ui.add_enabled_ui(!checking && (s.installed || s.in_path), |ui| {
                                if ui.button(t!("\u{E4A6} Odebrat z PATH", "\u{E4A6} Remove from PATH")).clicked() {
                                    app.shim_action(ui.ctx(), crate::shim_install::Action::Uninstall);
                                }
                            });
                            if checking {
                                ui.spinner();
                            }
                        }
                    }
                });
                ui.label(muted(tf!("Příkaz docker … ve Windows se provede přes {ename} v {distro}. Po změně otevři nový terminál.", "docker … in Windows runs via {ename} in {distro}. Open a new terminal after a change.")).small());
                if let Some(s) = &st {
                    for o in &s.other_docker {
                        ui.label(RichText::new(tf!("\u{E4E0} V PATH je i jiný docker: {o}", "\u{E4E0} Another docker is also in PATH: {o}")).color(theme::YELLOW).small());
                    }
                }

                // DOCKER_HOST pro nástroje, které mluví přímo s API
                ui.add_space(8.0);
                ui.label(RichText::new(t!("DOCKER_HOST pro nástroje", "DOCKER_HOST for tools")).strong());
                let pipe_url = crate::npipe::docker_host_url(crate::npipe::PIPE_NAME);
                let want = if npipe_on { pipe_url.clone() } else { format!("tcp://127.0.0.1:{port}") };
                let relay = crate::npipe::state();
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
                                badge(ui, &format!("DOCKER_HOST={v}"), theme::YELLOW).on_hover_text(tf!("Neodpovídá API aplikace ({want})", "Does not match the app API ({want})"));
                            }
                            None => {
                                badge(ui, t!("nenastaveno", "not set"), theme::GREY);
                            }
                        }
                        ui.add_enabled_ui(!checking && s.docker_host.as_deref() != Some(want.as_str()), |ui| {
                            if ui.button(tf!("\u{E3D4} Nastavit na {want}", "\u{E3D4} Set to {want}")).clicked() {
                                let action = if npipe_on {
                                    crate::shim_install::Action::SetDockerHostPipe
                                } else {
                                    crate::shim_install::Action::SetDockerHost(port, tls)
                                };
                                app.shim_action(ui.ctx(), action);
                            }
                        });
                        ui.add_enabled_ui(!checking && s.docker_host.is_some(), |ui| {
                            if ui.button(t!("\u{E4A6} Odebrat", "\u{E4A6} Remove")).clicked() {
                                app.shim_action(ui.ctx(), crate::shim_install::Action::ClearDockerHost);
                            }
                        });
                    }
                });
                ui.label(muted(t!("Pro VS Code Dev Containers, Testcontainers, Docker SDK a další nástroje, které nevolají příkaz docker, ale mluví přímo s API. Projeví se v nově spuštěných programech.", "For VS Code Dev Containers, Testcontainers, Docker SDK and other tools that talk to the API directly instead of calling the docker command. Takes effect in newly started programs.")).small());
                // pojmenovaná roura: stav relay (běží jen s aplikací)
                let host_is_pipe = st
                    .as_ref()
                    .and_then(|s| s.docker_host.as_deref())
                    .and_then(crate::npipe::pipe_from_docker_host)
                    .is_some_and(|p| p.eq_ignore_ascii_case(crate::npipe::PIPE_NAME));
                match (&relay, npipe_on) {
                    (crate::npipe::State::Running, _) => {
                        ui.label(
                            RichText::new(tf!(
                                "\u{E184} Roura {} běží – jen dokud běží WLS Desktop (ukončením aplikace přestane fungovat).",
                                "\u{E184} Pipe {} is running – only while WLS Desktop runs (quitting the app stops it).",
                                crate::npipe::PIPE_NAME
                            ))
                            .color(theme::GREEN)
                            .small(),
                        );
                    }
                    (crate::npipe::State::Failed(e), true) => {
                        ui.label(RichText::new(tf!("\u{E4E0} Rouru nelze spustit: {e}", "\u{E4E0} Cannot start the pipe: {e}")).color(theme::RED).small());
                    }
                    _ if host_is_pipe => {
                        ui.label(
                            RichText::new(t!(
                                "\u{E4E0} DOCKER_HOST míří na rouru, ale ta je vypnutá – zapni ji v Nastavení.",
                                "\u{E4E0} DOCKER_HOST points to the pipe, but it is turned off – enable it in Settings."
                            ))
                            .color(theme::YELLOW)
                            .small(),
                        );
                    }
                    _ => {}
                }

                // zabezpečení API (TLS s klientským certifikátem)
                ui.add_space(8.0);
                ui.label(RichText::new(t!("Zabezpečení API", "API security")).strong());
                toolbar(ui, |ui| {
                    if tls {
                        badge(ui, &tf!("\u{E40C} TLS s klientským certifikátem · port {port}", "\u{E40C} TLS with client certificate · port {port}"), theme::GREEN);
                        if ui.button(t!("\u{E306} Vypnout TLS", "\u{E306} Turn TLS off")).clicked() {
                            app.ask(
                                t!("Vypnout zabezpečení API?", "Turn API security off?"),
                                t!(
                                    "API bude znovu na tcp://127.0.0.1:2375 bez ověření – může ho ovládat každý proces, který se dostane na localhost (i jiné WSL distribuce a kontejnery).",
                                    "The API will be on tcp://127.0.0.1:2375 again without authentication – any process that can reach localhost (including other WSL distributions and containers) can control it."
                                ),
                                Cmd::SetTls(false),
                            );
                        }
                        if ui
                            .button(t!("\u{E094} Přegenerovat certifikáty", "\u{E094} Regenerate certificates"))
                            .on_hover_text(t!(
                                "Nová CA, serverový i klientský certifikát. Platí 825 dní; před vypršením se obnoví samy při nastavení API.",
                                "New CA, server and client certificates. Valid 825 days; renewed automatically on API setup before they expire."
                            ))
                            .clicked()
                        {
                            app.ask(
                                t!("Přegenerovat certifikáty?", "Regenerate certificates?"),
                                t!(
                                    "Vytvoří se nová CA a certifikáty, staré přestanou platit. Engine se restartuje.",
                                    "A new CA and certificates are created; the old ones stop working. The engine will restart."
                                ),
                                Cmd::RegenCerts,
                            );
                        }
                    } else {
                        badge(ui, &tf!("\u{E412} bez ověření · port {port}", "\u{E412} no authentication · port {port}"), theme::YELLOW);
                        if ui.button(t!("\u{E2FA} Zapnout TLS", "\u{E2FA} Turn TLS on")).clicked() {
                            ask_tls_on(app);
                        }
                    }
                });
                ui.label(
                    muted(if tls {
                        t!(
                            "Nástroje s DOCKER_HOST=tcp://… potřebují DOCKER_TLS_VERIFY=1 a DOCKER_CERT_PATH (nešifrovaný klíč) – tlačítko výše je nastaví. Bez klíče na disku: pojmenovaná roura v Nastavení.",
                            "Tools using DOCKER_HOST=tcp://… need DOCKER_TLS_VERIFY=1 and DOCKER_CERT_PATH (unencrypted key) – the button above sets them. Without a key on disk: the named pipe in Settings."
                        )
                    } else {
                        t!(
                            "Bez TLS může API ovládat každý proces, který se dostane na 127.0.0.1 – i jiné WSL distribuce a kontejnery se síťovým režimem host.",
                            "Without TLS any process that can reach 127.0.0.1 can control the API – including other WSL distributions and containers with host networking."
                        )
                    })
                    .small(),
                );
            });
    });

    // --- WSL distribuce ---
    section(ui, t!("WSL distribuce", "WSL distributions"));
    ui.horizontal(|ui| {
        if accent_button(ui, t!("\u{E3D4} Přidat distribuci…", "\u{E3D4} Add distribution…")).clicked() {
            super::settings::open_add_distro(app);
        }
        if ui.button(t!("\u{E010} Importovat…", "\u{E010} Import…")).on_hover_text(t!("wsl --import ze souboru .tar / .vhdx", "wsl --import from a .tar / .vhdx file")).clicked() {
            super::distros::open_import(app);
        }
        if ui.button(t!("\u{E094} Obnovit", "\u{E094} Refresh")).clicked() {
            app.send(Cmd::Refresh);
        }
        if super::danger_button(ui, t!("\u{E3DA} Vypnout celé WSL", "\u{E3DA} Shut down all of WSL")).clicked() {
            app.ask(t!("Vypnout WSL?", "Shut down WSL?"), t!("wsl --shutdown ukončí všechny distribuce včetně Dockeru.", "wsl --shutdown terminates all distributions, including Docker."), Cmd::Wsl(WslCmd::Shutdown));
        }
    });
    ui.add_space(6.0);
    let distros = app.distros.clone();
    let cols = [
        ("", W::Px(14.0)),
        (t!("DISTRIBUCE", "DISTRIBUTION"), W::Fr(2.0)),
        (t!("STAV", "STATE"), W::Fr(1.0)),
        ("WSL", W::Px(40.0)),
        (t!("AKCE", "ACTIONS"), W::Px(280.0)),
    ];
    table(ui, "distros", &cols, |mut body| {
        for d in &distros {
          body.row(34.0, |mut r| {
            r.col(|ui| dot(ui, if d.running() { theme::GREEN } else { theme::GREY }));
            r.col(|ui| {
                ui.label(RichText::new(&d.name).strong());
                if d.default {
                    ui.label(muted(t!("výchozí", "default")).small());
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
                    if icon(ui, "\u{E46C}", t!("Ukončit (wsl --terminate)", "Terminate (wsl --terminate)")) {
                        app.ask(tf!("Ukončit {}?", "Terminate {}?", d.name), t!("Všechny procesy v distribuci budou ukončeny.", "All processes in the distribution will be terminated."), Cmd::Wsl(WslCmd::Terminate(d.name.clone())));
                    }
                } else if icon(ui, "\u{E3D0}", t!("Spustit", "Start")) {
                    app.send(Cmd::Wsl(WslCmd::Start(d.name.clone())));
                }
                if icon(ui, "\u{EAE8}", t!("Terminál", "Terminal")) {
                    app.send(Cmd::Terminal(Term::Distro(d.name.clone())));
                }
                if !d.default && icon(ui, "\u{E46A}", t!("Nastavit jako výchozí", "Set as default")) {
                    app.send(Cmd::Wsl(WslCmd::SetDefault(d.name.clone())));
                }
                if d.name != distro && ui.small_button(t!("Použít pro Docker", "Use for Docker")).clicked() {
                    let mut c = app.cfg.read().map(|c| c.clone()).unwrap_or_default();
                    c.distro = d.name.clone();
                    app.cfg_draft.distro = d.name.clone();
                    app.save_config(c);
                }
                super::row_menu(ui, |ui| {
                    if ui.button(t!("\u{EAF0} Exportovat…", "\u{EAF0} Export…")).clicked() {
                        ui.close_menu();
                        super::distros::open_export(app, &d.name);
                    }
                    if ui.button(t!("\u{E29E} Disk a komprimace…", "\u{E29E} Disk and compacting…")).clicked() {
                        ui.close_menu();
                        super::distros::open_disk(app, &d.name);
                    }
                    ui.separator();
                    if ui.button(RichText::new(t!("\u{E4A6} Odebrat distribuci…", "\u{E4A6} Remove distribution…")).color(theme::RED)).clicked() {
                        ui.close_menu();
                        super::distros::open_remove(app, &d.name);
                    }
                });
            });
          });
        }
    });

    // --- daemon.json (jen Docker) ---
    if engine != crate::engine::Engine::Docker {
        return;
    }
    section(ui, t!("Konfigurace Docker démona (/etc/docker/daemon.json)", "Docker daemon configuration (/etc/docker/daemon.json)"));
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        if !app.daemon_loaded {
            ui.horizontal(|ui| {
                if ui.button(t!("\u{E23A} Načíst daemon.json", "\u{E23A} Load daemon.json")).on_hover_text(t!("Pokud distribuce neběží, spustí ji", "Starts the distribution if it is not running")).clicked() {
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
                if accent_button(ui, t!("\u{E248} Uložit a restartovat Docker", "\u{E248} Save and restart Docker")).clicked() {
                    app.ask(t!("Uložit daemon.json?", "Save daemon.json?"), t!("Soubor se přepíše a Docker se restartuje (běžící kontejnery bez restart policy se zastaví).", "The file will be overwritten and Docker restarted (running containers without a restart policy will stop)."), Cmd::SaveDaemonJson(app.daemon_json.clone()));
                }
            });
            if ui.button(t!("\u{E038} Načíst znovu", "\u{E038} Reload")).clicked() {
                app.daemon_loaded = false;
                app.send(Cmd::LoadDaemonJson);
            }
            match valid {
                Ok(_) => ui.label(RichText::new(t!("\u{E182} platný JSON", "\u{E182} valid JSON")).small().color(theme::GREEN)),
                Err(e) => ui.label(RichText::new(format!("\u{E4E0} {e}")).small().color(theme::RED)),
            };
        });
        ui.label(muted(t!("Příklady klíčů: \"log-driver\", \"log-opts\", \"dns\", \"registry-mirrors\", \"insecure-registries\", \"data-root\", \"default-address-pools\". Klíč \"hosts\" nepoužívej.", "Example keys: \"log-driver\", \"log-opts\", \"dns\", \"registry-mirrors\", \"insecure-registries\", \"data-root\", \"default-address-pools\". Do not use the \"hosts\" key.")).small());
    });
}
