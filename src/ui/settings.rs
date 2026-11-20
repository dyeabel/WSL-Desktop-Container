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
    // TLS je pro nové instalace výchozí
    app.add_distro = Some(AddDistroSpec { user: default_user(), engine, use_for_app: true, tls: true, ..Default::default() });
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
                    ui.label(RichText::new(format!("\u{E4E0} {e}")).color(theme::red()));
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
            ui.label("");
            ui.add_enabled_ui(spec.use_for_app && spec.engine.is_some(), |ui| {
                ui.checkbox(&mut spec.tls, t!("Zabezpečit API pomocí TLS (doporučeno)", "Secure the API with TLS (recommended)"))
                    .on_hover_text(t!(
                        "Engine jen na unix socketu, na 127.0.0.1:2376 TLS proxy, která pustí jen klienta s certifikátem aplikace.",
                        "Engine only on a unix socket; a TLS proxy on 127.0.0.1:2376 accepts only the app's client certificate."
                    ));
            });
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
            ui.label(RichText::new(format!("\u{E4E0} {p}")).color(theme::yellow()).small());
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

/// Standardní pojmenovaná roura pro nástroje (relay v aplikaci, bez DOCKER_HOST).
fn npipe_section(app: &mut App, ui: &mut egui::Ui) {
    let pipe = crate::npipe::PIPE_NAME;
    section(ui, t!("Pojmenovaná roura pro nástroje", "Named pipe for tools"));
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let mut on = app.cfg.read().map(|c| c.npipe).unwrap_or(false);
        if ui.checkbox(&mut on, tf!("Pojmenovaná roura pro nástroje {pipe} (jako Docker Desktop)", "Named pipe for tools {pipe} (like Docker Desktop)")).changed() {
            let res = match app.cfg.write() {
                Ok(mut g) => {
                    g.npipe = on;
                    g.save()
                }
                Err(e) => Err(e.to_string()),
            };
            if let Err(e) = res {
                app.toast(false, tf!("Nastavení nelze uložit: {e}", "Cannot save settings: {e}"));
            }
            app.cfg_draft.npipe = on;
            let ctx = ui.ctx().clone();
            crate::npipe::apply(on, app.cfg.clone(), move || ctx.request_repaint());
        }
        match crate::npipe::state() {
            crate::npipe::State::Running => {
                ui.label(RichText::new(t!("\u{E184} Běží – jen dokud běží WLS Desktop", "\u{E184} Running – only while WLS Desktop runs")).color(theme::green()).small());
            }
            crate::npipe::State::Failed(e) if on => {
                ui.label(RichText::new(tf!("\u{E4E0} Rouru nelze spustit: {e}", "\u{E4E0} Cannot start the pipe: {e}")).color(theme::red()).small());
            }
            _ => {}
        }
        ui.label(
            muted(tf!(
                "Aplikace přijímá spojení na {} (jen aktuální uživatel Windows, bez vzdáleného přístupu) a předává je API enginu – v režimu TLS s certifikátem aplikace, takže nástroje nepotřebují klíč na disku. \
                 Docker CLI, Testcontainers, VS Code a Docker SDK ji použijí samy, DOCKER_HOST nenastavuj. Funguje jen, dokud aplikace běží (stačí schovaná v oznamovací oblasti).",
                "The app accepts connections on {} (current Windows user only, no remote access) and relays them to the engine API – in TLS mode with the app's certificate, so tools need no key on disk. \
                 Docker CLI, Testcontainers, VS Code and Docker SDKs use it on their own; leave DOCKER_HOST unset. It works only while the app is running (hidden in the tray is enough).",
                pipe
            ))
            .small(),
        );
    });
}

/// `localhost` pro publikované porty i přes IPv6 (relay `[::1]` -> `127.0.0.1` v aplikaci).
fn ipv6_relay_section(app: &mut App, ui: &mut egui::Ui) {
    use crate::portrelay::Mode;
    section(ui, t!("Publikované porty na localhost", "Published ports on localhost"));
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let mut on = app.cfg.read().map(|c| c.ipv6_localhost_relay).unwrap_or(true);
        if ui.checkbox(&mut on, t!("Publikované porty i na localhost IPv6 (::1)", "Published ports also on localhost IPv6 (::1)")).changed() {
            let res = match app.cfg.write() {
                Ok(mut g) => {
                    g.ipv6_localhost_relay = on;
                    g.save()
                }
                Err(e) => Err(e.to_string()),
            };
            if let Err(e) = res {
                app.toast(false, tf!("Nastavení nelze uložit: {e}", "Cannot save settings: {e}"));
            }
            app.cfg_draft.ipv6_localhost_relay = on;
            // relaye srovná obnovení stavu
            app.send(Cmd::Refresh);
        }
        let st = crate::portrelay::app().status();
        let list = |v: &[u16]| v.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(", ");
        match st.mode {
            Mode::Mirrored => {
                ui.label(RichText::new(t!("\u{E184} WSL je v režimu mirrored – localhost (::1) funguje bez relaye", "\u{E184} WSL is in mirrored mode – localhost (::1) works without a relay")).color(theme::green()).small());
            }
            Mode::Active if !st.relayed.is_empty() => {
                ui.label(RichText::new(tf!("\u{E184} Přeposílám [::1] → 127.0.0.1 na portech: {}", "\u{E184} Relaying [::1] → 127.0.0.1 on ports: {}", list(&st.relayed))).color(theme::green()).small());
            }
            Mode::Active if st.conflicts.is_empty() => {
                ui.label(muted(t!("Žádné publikované porty běžících kontejnerů.", "No published ports of running containers.")).small());
            }
            _ => {}
        }
        if st.mode == Mode::Active {
            for (p, e) in &st.conflicts {
                ui.label(RichText::new(tf!("\u{E4E0} [::1]:{p} je obsazený jiným programem ({e}) – použij 127.0.0.1:{p}", "\u{E4E0} [::1]:{p} is taken by another program ({e}) – use 127.0.0.1:{p}")).color(theme::yellow()).small());
            }
        }
        ui.label(
            muted(t!(
                "WSL v režimu NAT předává publikované porty jen na 127.0.0.1. Windows ale localhost zkouší nejdřív přes IPv6 (::1), kde spojení visí až do vypršení limitu. \
                 Aplikace proto (dokud běží) poslouchá na [::1] na stejných portech a přeposílá na 127.0.0.1 – jen lokálně, bez přístupu z jiných počítačů. Jen TCP.",
                "In NAT mode WSL forwards published ports only on 127.0.0.1. Windows tries localhost over IPv6 (::1) first, where the connection hangs until a timeout. \
                 While running, the app therefore listens on [::1] on the same ports and relays to 127.0.0.1 – local only, no access from other computers. TCP only."
            ))
            .small(),
        );
    });
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(ui, t!("Nastavení", "Settings"), "");

    // jazyk se mění okamžitě (bez tlačítka Uložit)
    section(ui, t!("Jazyk a vzhled", "Language and appearance"));
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
        // motiv – mění se okamžitě
        let cur_theme = app.cfg.read().map(|c| c.theme.clone()).unwrap_or_default();
        let mut th = cur_theme.clone();
        let theme_name = |m: &str| match m {
            "light" => t!("Světlý", "Light"),
            "system" => t!("Podle Windows", "Follow Windows"),
            _ => t!("Tmavý", "Dark"),
        };
        ui.horizontal(|ui| {
            ui.label(t!("Motiv", "Theme"));
            egui::ComboBox::from_id_salt("theme").selected_text(theme_name(&th)).show_ui(ui, |ui| {
                for m in ["dark", "light", "system"] {
                    ui.selectable_value(&mut th, m.to_string(), theme_name(m));
                }
            });
        });
        if th != cur_theme {
            crate::theme::set_mode(ui.ctx(), &th);
            if let Ok(mut g) = app.cfg.write() {
                g.theme = th.clone();
                let _ = g.save();
            }
            app.cfg_draft.theme = th;
        }
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

    section(ui, t!("Aplikace", "Application"));
    let mut open_engine = false;
    let mut add_distro_requested = false;
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let d = &mut app.cfg_draft;
        egui::Grid::new("appcfg").num_columns(2).spacing([16.0, 10.0]).show(ui, |ui| {
            ui.label(t!("Primární distribuce", "Primary distribution"));
            let mut add = false;
            ui.horizontal(|ui| {
                // přepíná se na stránce WSL engine / v levém panelu (přesune i API)
                ui.label(RichText::new(format!("{} · {}", d.distro, d.engine.name())).strong());
                if ui
                    .button(t!("\u{EB02} Změnit…", "\u{EB02} Change…"))
                    .on_hover_text(t!("Přepnutí na stránce WSL engine (nebo klikem na distribuci v levém panelu)", "Switch on the WSL engine page (or click the distribution in the left panel)"))
                    .clicked()
                {
                    open_engine = true;
                }
                if ui.button(t!("\u{E3D4} Přidat distribuci…", "\u{E3D4} Add distribution…")).clicked() {
                    add = true;
                }
            });
            if add {
                add_distro_requested = true;
            }
            ui.end_row();
            ui.label(t!("Port API enginu", "Engine API port"));
            ui.add(egui::DragValue::new(&mut d.port).range(1024..=65535));
            ui.end_row();
            ui.label(t!("Obnovovat každých", "Refresh every"));
            ui.add(egui::DragValue::new(&mut d.refresh_secs).range(1..=120).suffix(" s"))
                .on_hover_text(t!("Když je okno vidět: stav, statistiky kontejnerů a grafy na Přehledu.", "While the window is shown: status, container statistics and Overview charts."));
            ui.end_row();
            ui.label(t!("Na pozadí každých", "In the background every"));
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut d.background_refresh_secs).range(3..=600).suffix(" s"));
                ui.label(muted(t!("okno schované v oznamovací oblasti nebo minimalizované", "window hidden in the tray or minimized")).small());
            })
            .response
            .on_hover_text(t!(
                "Sběr dat pokračuje i se schovaným oknem (historie, upozornění), jen řidčeji – šetří procesor. Po zobrazení okna se data hned obnoví.",
                "Data collection continues with the window hidden (history, alerts), just less often – saves CPU. Showing the window refreshes the data right away."
            ));
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut d.keep_alive, t!("Udržovat distribuci naživu (doporučeno)", "Keep distribution alive (recommended)"));
            ui.end_row();
            ui.label("");
            ui.checkbox(&mut d.autostart_engine, t!("Při spuštění aplikace nastartovat engine", "Start the engine when the app starts"));
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
                if accent_button(ui, t!("\u{E248} Uložit", "\u{E248} Save")).clicked() {
                    let port_changed = cur.port != app.cfg_draft.port;
                    let c = app.cfg_draft.clone();
                    app.save_config(c);
                    if port_changed {
                        app.toast(true, t!("Port změněn – na stránce WSL engine klikni „Nastavit jen API“", "Port changed – click “Set up API only” on the WSL engine page"));
                    }
                }
                if ui.button(t!("Zahodit změny", "Discard changes")).clicked() {
                    app.cfg_draft = cur.clone();
                }
            });
        });
        ui.label(muted(t!("Při změně distribuce nebo portu je potřeba v nové distribuci jednou spustit „Nastavit jen API“ (stránka WSL engine).", "After changing the distribution or port, run “Set up API only” once in the new distribution (WSL engine page).")).small());
    });

    if add_distro_requested {
        open_add_distro(app);
    }
    if open_engine {
        app.page = crate::app::Page::Engine;
    }

    section(ui, t!("Na pozadí a oznámení", "Background and notifications"));
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let mut auto = *app.autostart.get_or_insert_with(crate::autostart::is_enabled);
        if ui
            .checkbox(&mut auto, t!("Spouštět s Windows (po přihlášení, schované v oznamovací oblasti)", "Start with Windows (after sign-in, hidden in the tray)"))
            .changed()
        {
            match crate::autostart::set(auto) {
                Ok(()) => app.autostart = Some(auto),
                Err(e) => app.toast(false, tf!("Spouštění s Windows nelze nastavit: {e}", "Cannot set start with Windows: {e}")),
            }
        }
        let mut c = app.cfg.read().map(|c| c.clone()).unwrap_or_default();
        let before = (c.close_to_tray, c.minimize_to_tray, c.start_hidden, c.auto_free_memory);
        ui.checkbox(
            &mut c.auto_free_memory,
            t!(
                "Automaticky uvolňovat paměť WSL (cache Linuxu nad 1 GB, nejvýš jednou za 10 minut)",
                "Free WSL memory automatically (Linux cache above 1 GB, at most every 10 minutes)"
            ),
        )
        .on_hover_text(t!(
            "VmmemWSL drží paměť, kterou Linux použil jako cache souborů (image, buildy), i když ji nepotřebuje. Uvolnění ji vrátí Windows; další čtení souborů může být chvíli pomalejší.",
            "VmmemWSL keeps memory Linux used as file cache (images, builds) even when it is not needed. Freeing returns it to Windows; the next file reads may be slightly slower."
        ));
        ui.checkbox(&mut c.close_to_tray, t!("Zavření okna (křížek) aplikaci jen schová do oznamovací oblasti", "Closing the window (×) only hides the app to the tray"));
        ui.checkbox(&mut c.minimize_to_tray, t!("Minimalizace schová okno do oznamovací oblasti", "Minimizing hides the window to the tray"));
        ui.checkbox(&mut c.start_hidden, t!("Spouštět rovnou schované v oznamovací oblasti", "Start hidden in the tray"));
        if (c.close_to_tray, c.minimize_to_tray, c.start_hidden, c.auto_free_memory) != before {
            let _ = c.save();
            crate::tray::sync_checks(c.close_to_tray, c.minimize_to_tray);
            app.cfg_draft.close_to_tray = c.close_to_tray;
            app.cfg_draft.minimize_to_tray = c.minimize_to_tray;
            app.cfg_draft.start_hidden = c.start_hidden;
            app.cfg_draft.auto_free_memory = c.auto_free_memory;
            if let Ok(mut g) = app.cfg.write() {
                *g = c;
            }
        }
        ui.label(muted(t!("Aplikaci úplně ukončíš přes pravý klik na ikonu v oznamovací oblasti \u{E06C} Ukončit. Levý klik okno zobrazí nebo schová.", "To quit the app completely, right-click the tray icon \u{E06C} Quit. Left-click shows or hides the window.")).small());
    });

    super::events::settings_section(app, ui);
    super::alerts::settings_section(app, ui);
    super::memory::history_settings(app, ui);

    section(ui, t!("Příkaz docker ve Windows", "The docker command in Windows"));
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(t!(
            "Malý docker.exe, který příkazy předává Dockeru ve WSL (docker ps, docker compose up, docker run -it …). \
             Windows cesty v argumentech (C:\\…, .\\data) se přeloží automaticky a příkaz běží v aktuální složce. \
             Nejde o Docker Desktop ani o Docker CLI pro Windows.",
            "A small docker.exe that forwards commands to Docker in WSL (docker ps, docker compose up, docker run -it …). \
             Windows paths in arguments (C:\\…, .\\data) are translated automatically and the command runs in the current folder. \
             This is neither Docker Desktop nor the Docker CLI for Windows."
        ));
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
                    ui.label(muted(t!("Zjišťuji stav…", "Checking status…")));
                });
            }
            Some(s) => {
                let dir = crate::shim_install::bin_dir().display().to_string();
                if s.installed && s.in_path {
                    ui.label(RichText::new(tf!("\u{E184} Nainstalováno v {dir} a přidáno do PATH", "\u{E184} Installed in {dir} and added to PATH")).color(theme::green()));
                } else if s.installed {
                    ui.label(RichText::new(tf!("\u{E4E0} Soubory jsou v {dir}, ale složka chybí v PATH", "\u{E4E0} The files are in {dir}, but the folder is missing from PATH")).color(theme::yellow()));
                } else {
                    ui.label(muted(t!("Není nainstalováno.", "Not installed.")));
                }
                other_docker_ui(app, ui, s, checking);
                duplicates_ui(app, ui, s, checking);
                if !s.source_ok {
                    ui.label(RichText::new(t!("\u{E4E0} docker.exe nebyl nalezen vedle aplikace (sestav projekt přes cargo build --release).", "\u{E4E0} docker.exe was not found next to the app (build the project with cargo build --release).")).color(theme::red()).small());
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!checking && s.source_ok, |ui| {
                        let label = if s.installed { t!("\u{E094} Aktualizovat", "\u{E094} Update") } else { t!("\u{E20C} Nainstalovat do PATH", "\u{E20C} Install to PATH") };
                        if accent_button(ui, label).clicked() {
                            app.shim_action(ui.ctx(), crate::shim_install::Action::Install);
                        }
                    });
                    ui.add_enabled_ui(!checking && (s.installed || s.in_path), |ui| {
                        if ui.button(t!("\u{E4A6} Odinstalovat", "\u{E4A6} Uninstall")).clicked() {
                            app.shim_action(ui.ctx(), crate::shim_install::Action::Uninstall);
                        }
                    });
                    if checking {
                        ui.spinner();
                    }
                });
                ui.label(muted(t!("Po instalaci otevři nový terminál. Ladění: proměnná WLS_SHIM_DEBUG=1 vypíše, co se spouští ve WSL.", "Open a new terminal after installing. Debugging: WLS_SHIM_DEBUG=1 prints what runs in WSL.")).small());
            }
        }
    });

    npipe_section(app, ui);
    ipv6_relay_section(app, ui);

    section(ui, t!("WSL – globální nastavení (%USERPROFILE%\\.wslconfig)", "WSL – global settings (%USERPROFILE%\\.wslconfig)"));
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let w = &mut app.wsl_draft;
        egui::Grid::new("wslcfg").num_columns(3).spacing([16.0, 10.0]).show(ui, |ui| {
            ui.label(t!("Paměť (memory)", "Memory (memory)"));
            ui.add(field(&mut w.memory).hint_text(t!("např. 8GB", "e.g. 8GB")).desired_width(140.0));
            ui.label(muted(t!("prázdné = výchozí (50 % RAM)", "empty = default (50% of RAM)")).small());
            ui.end_row();
            ui.label(t!("Procesory (processors)", "Processors (processors)"));
            ui.add(field(&mut w.processors).hint_text(t!("např. 4", "e.g. 4")).desired_width(140.0));
            ui.label(muted(t!("prázdné = všechny", "empty = all")).small());
            ui.end_row();
            ui.label("Swap");
            ui.add(field(&mut w.swap).hint_text(t!("např. 2GB nebo 0", "e.g. 2GB or 0")).desired_width(140.0));
            ui.label(muted("").small());
            ui.end_row();
            ui.label(t!("Uspání VM po nečinnosti", "VM idle timeout"));
            ui.add(field(&mut w.idle_timeout).hint_text(t!("ms, např. 60000", "ms, e.g. 60000")).desired_width(140.0));
            ui.label(muted(t!("vmIdleTimeout – po kolika ms bez distribucí se vypne VM", "vmIdleTimeout – ms without distributions before the VM shuts down")).small());
            ui.end_row();
            ui.label(t!("Síťový režim", "Networking mode"));
            egui::ComboBox::from_id_salt("netmode")
                .selected_text(if w.networking.is_empty() { t!("výchozí (NAT)", "default (NAT)") } else { w.networking.as_str() })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut w.networking, String::new(), t!("výchozí (NAT)", "default (NAT)"));
                    ui.selectable_value(&mut w.networking, "mirrored".to_string(), "mirrored");
                    ui.selectable_value(&mut w.networking, "nat".to_string(), "nat");
                });
            ui.label(muted(t!("mirrored = sdílí síť s Windows", "mirrored = shares the network with Windows")).small());
            ui.end_row();
            ui.label(t!("Vracení paměti", "Memory reclaim"));
            let reclaim_name = |m: &str| match m {
                "dropCache" => t!("dropCache – hned po nečinnosti", "dropCache – right after idle"),
                "gradual" => t!("gradual – postupně", "gradual – gradually"),
                "disabled" => t!("vypnuto", "disabled"),
                _ => t!("výchozí (dropCache)", "default (dropCache)"),
            };
            egui::ComboBox::from_id_salt("memreclaim").selected_text(reclaim_name(&w.memory_reclaim)).show_ui(ui, |ui| {
                for m in ["", "dropCache", "gradual", "disabled"] {
                    ui.selectable_value(&mut w.memory_reclaim, m.to_string(), reclaim_name(m));
                }
            });
            ui.label(muted(t!("autoMemoryReclaim – jak WSL vrací Windows paměť z cache", "autoMemoryReclaim – how WSL returns cached memory to Windows")).small());
            ui.end_row();
        });
        ui.add_space(6.0);
        let changed = app.wsl_draft != app.wsl_saved;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(changed, |ui| {
                if accent_button(ui, t!("\u{E248} Uložit .wslconfig", "\u{E248} Save .wslconfig")).clicked() {
                    match app.wsl_draft.save() {
                        Ok(()) => {
                            app.wsl_saved = app.wsl_draft.clone();
                            app.toast(true, t!(".wslconfig uložen – projeví se po restartu WSL", ".wslconfig saved – takes effect after a WSL restart"));
                        }
                        Err(e) => app.toast(false, tf!(".wslconfig nelze uložit: {e}", "Cannot save .wslconfig: {e}")),
                    }
                }
            });
            if ui.button(t!("\u{E3DA} Restartovat WSL (wsl --shutdown)", "\u{E3DA} Restart WSL (wsl --shutdown)")).clicked() {
                app.ask(t!("Restartovat WSL?", "Restart WSL?"), t!("Všechny distribuce se ukončí. Docker pak spustíš tlačítkem Spustit engine.", "All distributions will be terminated. Start Docker again with the Start engine button."), Cmd::Wsl(WslCmd::Shutdown));
            }
        });
    });

    section(ui, t!("O aplikaci", "About"));
    ui.label(muted(tf!(
        "WLS Desktop {} · Rust + egui · Docker API přes tcp://127.0.0.1 · konfigurace v %APPDATA%\\WLS_Desktop",
        "WLS Desktop {} · Rust + egui · Docker API via tcp://127.0.0.1 · configuration in %APPDATA%\\WLS_Desktop",
        env!("CARGO_PKG_VERSION")
    )));
}

/// Jiné kopie shimu WLS Desktop (starší verze v PATH, zbylá výchozí instalace) a tlačítko úklidu.
pub fn duplicates_ui(app: &App, ui: &mut egui::Ui, s: &crate::shim_install::Status, checking: bool) {
    if s.duplicates.is_empty() {
        return;
    }
    ui.label(RichText::new(t!("\u{E4E0} Další kopie příkazu docker z WLS Desktop:", "\u{E4E0} Other copies of the WLS Desktop docker command:")).color(theme::yellow()).small());
    for d in &s.duplicates {
        let what = match (d.in_path, d.delete_files) {
            (true, true) => t!("v PATH, soubory se smažou", "in PATH, files will be deleted"),
            (true, false) => t!("v PATH, odebere se z něj", "in PATH, will be removed from it"),
            (false, _) => t!("mimo PATH, zbylé soubory se smažou", "not in PATH, leftover files will be deleted"),
        };
        ui.label(muted(format!("    {} – {what}", d.path)).small());
    }
    ui.add_enabled_ui(!checking, |ui| {
        if ui
            .button(t!("\u{E4A6} Odstranit duplicity", "\u{E4A6} Remove duplicates"))
            .on_hover_text(t!(
                "Aktuální složka v PATH zůstane jednou. Složky jiných verzí se jen odeberou z PATH (portable balíčky se nemažou), kopie ve výchozí instalační složce se smažou.",
                "The current folder stays in PATH once. Folders of other versions are only removed from PATH (portable packages are not deleted); copies in the default install folder are deleted."
            ))
            .clicked()
        {
            app.shim_action(ui.ctx(), crate::shim_install::Action::RemoveDuplicates);
        }
    });
}

/// Varování o jiném dockeru v PATH (`other_docker` obsahuje jen ty, které mají přednost)
/// a tlačítko, které jeho složku odebere z uživatelského PATH.
pub fn other_docker_ui(app: &App, ui: &mut egui::Ui, s: &crate::shim_install::Status, checking: bool) {
    for o in &s.other_docker {
        let file = &o.file;
        let text = if s.in_path {
            tf!("\u{E4E0} V PATH je před ním jiný docker, který má přednost: {file}", "\u{E4E0} Another docker comes earlier in PATH and takes precedence: {file}")
        } else {
            tf!("\u{E4E0} V PATH je jiný docker: {file}", "\u{E4E0} Another docker is in PATH: {file}")
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(text).color(theme::yellow()).small());
            if o.user {
                ui.add_enabled_ui(!checking, |ui| {
                    if ui
                        .small_button(t!("\u{E4A6} Odebrat z PATH", "\u{E4A6} Remove from PATH"))
                        .on_hover_text(tf!(
                            "Odebere {} z uživatelského PATH. Soubory zůstanou, program jen přestane být dostupný jako docker.",
                            "Removes {} from the user PATH. The files stay; the program just stops being available as docker.",
                            o.dir
                        ))
                        .clicked()
                    {
                        app.shim_action(ui.ctx(), crate::shim_install::Action::RemoveFromPath(o.dir.clone()));
                    }
                });
            } else {
                ui.label(muted(t!("(systémový PATH – změní jen správce)", "(system PATH – only an administrator can change it)")).small());
            }
        });
    }
}
