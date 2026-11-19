//! Detail kontejneru: přehled (inspect), statistiky, logy, akce a „Znovu vytvořit se změnami“.

use super::*;
use crate::app::{App, LogView};
use crate::docker::{short_id, Container};
use crate::inspect::{self, Detail, NetEdit};
use crate::worker::{Cmd, NsFeed, RunSpec, StatSample, StatsFeed, Term, STATS_WINDOW};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Stats,
    Logs,
    Recreate,
    Files,
    Network,
    Limits,
}

pub struct DetailView {
    pub id: String,
    pub name: String,
    pub tab: Tab,
    info: Option<Result<Box<Detail>, String>>,
    /// stav kontejneru ze snapshotu při posledním inspect (změna -> nový inspect)
    seen: Option<String>,
    /// zobrazit hodnoty tajných proměnných
    reveal: bool,
    stats: StatsFeed,
    logs: Option<LogView>,
    tail: u32,
    /// formulář „Znovu vytvořit“ (předvyplní se z inspect)
    form: Option<RunSpec>,
    /// sítě, které si nový kontejner ponechá (vedle hlavní z formuláře)
    nets: Option<NetEdit>,
    /// engine je Podman (nehlásí pevné IP – výchozí je ponechat současné adresy)
    podman: bool,
    /// název kontejneru, který vznikne znovuvytvořením (nové ID – dohledá se podle názvu)
    recreating: Option<String>,
    /// kontejner už neexistuje
    gone: bool,
    /// záložka Soubory (vytvoří se při prvním otevření)
    pub cfiles: Option<super::container_files::CfState>,
    /// záložka Síť: sběr portů dovnitř / ven (běží jen při otevřené záložce)
    net: Option<NsFeed>,
    /// formulář limitů a limity, ze kterých vznikl (po změně inspectu se obnoví, když není upravený)
    lform: Option<crate::limits::LimitForm>,
    lbase: crate::limits::Limits,
    /// počet CPU enginu (0 = neznámý)
    ncpu: i64,
    cfg: std::sync::Arc<std::sync::RwLock<crate::config::Config>>,
}

/// Akce z okna – provedou se až po vykreslení (okno si půjčuje `app.detail`).
enum Action {
    Send(Cmd),
    Ask(String, String, Cmd),
}

static DEBUG_DONE: AtomicBool = AtomicBool::new(false);

/// Otevře detail kontejneru (nahradí případný otevřený).
pub fn open(app: &mut App, ctx: &egui::Context, id: String, name: String) {
    let tab = app.detail.as_ref().map(|d| d.tab).unwrap_or(Tab::Overview);
    app.detail = Some(new_view(app, ctx, id, name, tab));
}

fn new_view(app: &App, ctx: &egui::Context, id: String, name: String, tab: Tab) -> DetailView {
    let (port, podman) = app.cfg.read().map(|c| (c.port, c.engine == crate::engine::Engine::Podman)).unwrap_or((2375, true));
    app.send(Cmd::Inspect(id.clone()));
    DetailView {
        stats: crate::worker::start_stats(ctx.clone(), port, id.clone()),
        id,
        name,
        tab,
        info: None,
        seen: None,
        reveal: false,
        logs: None,
        tail: 500,
        form: None,
        nets: None,
        podman,
        recreating: None,
        gone: false,
        cfiles: None,
        net: None,
        lform: None,
        lbase: Default::default(),
        ncpu: app.snap.info.as_ref().map(|i| i.ncpu).unwrap_or(0),
        cfg: app.cfg.clone(),
    }
}

/// Výsledek inspect od workeru.
pub fn on_inspect(app: &mut App, id: String, r: Result<Box<Detail>, String>) {
    let Some(dv) = app.detail.as_mut() else { return };
    if dv.id != id {
        return;
    }
    if let Ok(d) = &r {
        if dv.form.is_none() {
            dv.form = Some(d.to_run_spec());
            dv.nets = Some(d.net_edit(dv.podman));
        }
        let pristine = dv.lform.as_ref().is_none_or(|f| *f == crate::limits::LimitForm::from_limits(&dv.lbase));
        if pristine {
            dv.lform = Some(crate::limits::LimitForm::from_limits(&d.limits));
            dv.lbase = d.limits.clone();
        }
    }
    dv.info = Some(r);
}

/// Klíč stavu kontejneru ze snapshotu – při změně se znovu načte inspect.
fn state_key(c: &Container) -> String {
    let h = ["(healthy)", "(unhealthy)", "health: starting"].iter().find(|h| c.status.contains(*h)).copied().unwrap_or("");
    format!("{}|{}|{}", c.state, h, c.name())
}

/// Pro ladění/screenshoty: `WLS_DEBUG_DETAIL=<název>[:overview|stats|logs|recreate|files|network|limits]`.
fn debug_open(app: &mut App, ctx: &egui::Context) {
    if DEBUG_DONE.load(Ordering::Relaxed) {
        return;
    }
    let Ok(v) = std::env::var("WLS_DEBUG_DETAIL") else {
        DEBUG_DONE.store(true, Ordering::Relaxed);
        return;
    };
    let (name, tab) = v.split_once(':').unwrap_or((v.as_str(), ""));
    let Some(c) = app.snap.containers.iter().find(|c| c.name() == name) else { return };
    DEBUG_DONE.store(true, Ordering::Relaxed);
    let tab = match tab {
        "stats" => Tab::Stats,
        "logs" => Tab::Logs,
        "recreate" => Tab::Recreate,
        "files" => Tab::Files,
        "network" => Tab::Network,
        "limits" => Tab::Limits,
        _ => Tab::Overview,
    };
    let (id, name) = (c.id.clone(), c.name());
    app.detail = Some(new_view(app, ctx, id, name, tab));
}

/// Sladí detail se snapshotem: změna stavu -> inspect, nové ID po znovuvytvoření -> přepnout.
fn sync(app: &mut App, ctx: &egui::Context) {
    if !app.snap.connected {
        return;
    }
    let Some(dv) = app.detail.as_mut() else { return };
    if let Some(c) = app.snap.containers.iter().find(|c| c.id == dv.id) {
        dv.gone = false;
        let key = state_key(c);
        if dv.seen.as_ref() != Some(&key) {
            let reload = dv.seen.is_some();
            dv.seen = Some(key);
            if reload {
                let id = dv.id.clone();
                app.send(Cmd::Inspect(id));
            }
        }
        return;
    }
    // znovu vytvořený kontejner má nové ID – dohledat podle názvu
    let names = [dv.recreating.clone(), Some(dv.name.clone())];
    let found = names.iter().flatten().find_map(|n| app.snap.containers.iter().find(|c| &c.name() == n));
    match found {
        Some(c) => {
            let (id, name, tab) = (c.id.clone(), c.name(), dv.tab);
            app.detail = Some(new_view(app, ctx, id, name, tab));
        }
        None => dv.gone = true,
    }
}

pub fn window(app: &mut App, ctx: &egui::Context) {
    debug_open(app, ctx);
    if app.detail.is_none() {
        return;
    }
    sync(app, ctx);
    // logy se spouští až při otevření záložky (a znovu po změně počtu řádků)
    if let Some(dv) = app.detail.as_ref() {
        if dv.tab == Tab::Logs && dv.logs.is_none() {
            let lv = app.spawn_logs(dv.id.clone(), dv.name.clone(), dv.tail);
            if let Some(dv) = app.detail.as_mut() {
                dv.logs = Some(lv);
            }
        }
    }
    let nets: Vec<String> = app.snap.networks.iter().map(|n| n.name.clone()).collect();
    let snap_c = app.detail.as_ref().and_then(|dv| app.snap.containers.iter().find(|c| c.id == dv.id).cloned());
    let Some(dv) = app.detail.as_mut() else { return };
    let mut actions: Vec<Action> = Vec::new();
    let mut open = true;
    let screen = ctx.screen_rect();
    egui::Window::new(tf!("\u{E2CE} Kontejner: {}", "\u{E2CE} Container: {}", dv.name))
        .id(egui::Id::new("container-detail"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([(screen.width() - 80.0).min(880.0), (screen.height() - 90.0).min(620.0)])
        .min_size([420.0, 300.0])
        .max_size(screen.size() - egui::vec2(16.0, 40.0))
        .show(ctx, |ui| body(ui, dv, snap_c.as_ref(), &nets, &mut actions));
    for a in actions {
        match a {
            Action::Send(c) => app.send(c),
            Action::Ask(title, text, c) => app.ask(title, text, c),
        }
    }
    let files = app.detail.as_mut().and_then(|d| d.cfiles.as_mut()).map(|c| c.take_pending()).unwrap_or_default();
    super::container_files::flush(app, files);
    if !open {
        app.detail = None;
    }
}

fn body(ui: &mut egui::Ui, dv: &mut DetailView, c: Option<&Container>, nets: &[String], actions: &mut Vec<Action>) {
    let detail = dv.info.as_ref().and_then(|r| r.as_ref().ok());
    // záhlaví: stav, image a akce
    let (state, status) = match (c, detail) {
        (Some(c), _) => (c.state.clone(), c.status.clone()),
        (None, Some(d)) => (d.status.clone(), String::new()),
        _ => (String::new(), String::new()),
    };
    let running = state == "running";
    let paused = state == "paused";
    toolbar(ui, |ui| {
        if dv.gone {
            badge(ui, t!("Neexistuje", "Gone"), theme::grey());
        } else if !state.is_empty() {
            let st = containers::state_info(&state, &status);
            badge(ui, &st.label, st.color);
            if let Some((h, hc)) = &st.health {
                badge(ui, h, *hc);
            }
            if !st.detail.is_empty() {
                ui.label(muted(&st.detail).small());
            }
        }
        if let Some(d) = detail {
            ui.label(muted(format!("{} · {}", crate::engine::short_image(&d.image), short_id(&d.id))).small());
        }
    });
    if !dv.gone {
        let id = dv.id.clone();
        let name = dv.name.clone();
        toolbar(ui, |ui| {
            if paused {
                if ui.button(t!("\u{E3D2} Obnovit", "\u{E3D2} Resume")).clicked() {
                    actions.push(Action::Send(Cmd::Container(id.clone(), "unpause")));
                }
            } else if running {
                if ui.button(t!("\u{E46C} Zastavit", "\u{E46C} Stop")).clicked() {
                    actions.push(Action::Send(Cmd::Container(id.clone(), "stop")));
                }
                if ui.button(t!("\u{E094} Restartovat", "\u{E094} Restart")).clicked() {
                    actions.push(Action::Send(Cmd::Container(id.clone(), "restart")));
                }
            } else if ui.button(t!("\u{E3D0} Spustit", "\u{E3D0} Start")).clicked() {
                actions.push(Action::Send(Cmd::Container(id.clone(), "start")));
            }
            if running && ui.button(t!("\u{EAE8} Konzole", "\u{EAE8} Console")).on_hover_text(t!("Shell v kontejneru (Windows Terminal)", "Shell in the container (Windows Terminal)")).clicked() {
                actions.push(Action::Send(Cmd::Terminal(Term::Container { id: id.clone(), name: name.clone() })));
            }
            if ui.button(t!("\u{E4A6} Odstranit", "\u{E4A6} Remove")).clicked() {
                actions.push(Action::Ask(
                    tf!("Odstranit {name}?", "Remove {name}?"),
                    t!(
                        "Kontejner bude zastaven a smazán (pojmenované volume zůstanou).",
                        "The container will be stopped and deleted (named volumes are kept)."
                    )
                    .into(),
                    Cmd::Container(id.clone(), "remove"),
                ));
            }
            if ui.button(t!("\u{E094} Načíst znovu", "\u{E094} Reload")).on_hover_text(t!("Znovu načíst detail", "Reload the details")).clicked() {
                actions.push(Action::Send(Cmd::Inspect(id.clone())));
            }
        });
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        for (t, label) in [
            (Tab::Overview, t!("\u{E2CE} Přehled", "\u{E2CE} Overview")),
            (Tab::Stats, t!("\u{E154} Statistiky", "\u{E154} Stats")),
            (Tab::Logs, t!("\u{EB7A} Logy", "\u{EB7A} Logs")),
            (Tab::Recreate, t!("\u{E3B4} Znovu vytvořit", "\u{E3B4} Recreate")),
            (Tab::Files, t!("\u{E260} Soubory", "\u{E260} Files")),
            (Tab::Network, t!("\u{EB56} Síť", "\u{EB56} Network")),
            (Tab::Limits, t!("\u{E628} Limity", "\u{E628} Limits")),
        ] {
            ui.selectable_value(&mut dv.tab, t, label);
        }
    });
    ui.separator();
    if dv.tab != Tab::Network {
        dv.net = None; // sběr portů jen při otevřené záložce
    }
    match dv.tab {
        Tab::Overview => match &dv.info {
            None => loading(ui),
            Some(Err(e)) => error(ui, e),
            Some(Ok(d)) => {
                egui::ScrollArea::vertical().id_salt("detail-overview").auto_shrink([false, false]).show(ui, |ui| overview(ui, d, &mut dv.reveal));
            }
        },
        Tab::Stats => stats(ui, &dv.stats, running),
        Tab::Network => {
            let feed = dv.net.get_or_insert_with(|| crate::worker::start_ns_feed(ui.ctx().clone(), dv.cfg.clone(), Some(dv.id.clone())));
            super::net_overview::container_tab(ui, feed, &dv.id, c, running && !dv.gone);
        }
        Tab::Logs => {
            ui.horizontal(|ui| {
                ui.label(t!("Posledních řádků", "Last lines"));
                let before = dv.tail;
                egui::ComboBox::from_id_salt("detail-tail").width(80.0).selected_text(dv.tail.to_string()).show_ui(ui, |ui| {
                    for n in [100u32, 500, 1000, 5000] {
                        ui.selectable_value(&mut dv.tail, n, n.to_string());
                    }
                });
                if dv.tail != before {
                    dv.logs = None; // spustí se znovu s novým počtem
                }
            });
            if let Some(lv) = dv.logs.as_mut() {
                if let Some(t) = logs::view(ui, lv) {
                    actions.push(Action::Send(Cmd::Terminal(t)));
                }
            } else {
                loading(ui);
            }
        }
        Tab::Files => {
            let cf = dv.cfiles.get_or_insert_with(|| super::container_files::CfState::new(dv.id.clone(), dv.name.clone(), None));
            super::container_files::view(ui, cf, running && !dv.gone);
        }
        Tab::Limits => match &dv.info {
            None => loading(ui),
            Some(Err(e)) => error(ui, e),
            Some(Ok(d)) => {
                let d = d.clone();
                egui::ScrollArea::vertical().id_salt("detail-limits").auto_shrink([false, false]).show(ui, |ui| limits_tab(ui, dv, &d, running, actions));
            }
        },
        Tab::Recreate => match &dv.info {
            None => loading(ui),
            Some(Err(e)) => error(ui, e),
            Some(Ok(d)) => {
                let d = d.clone();
                egui::ScrollArea::vertical().id_salt("detail-recreate").auto_shrink([false, false]).show(ui, |ui| recreate(ui, dv, &d, nets, actions));
            }
        },
    }
}

fn loading(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(muted(t!("Načítám…", "Loading…")));
    });
}

fn error(ui: &mut egui::Ui, e: &str) {
    ui.label(RichText::new(e).color(theme::red()));
}

/// Řádek mřížky klíč – hodnota (hodnota jde označit a kopírovat).
fn kv(ui: &mut egui::Ui, k: &str, v: impl Into<String>, mono: bool) {
    ui.label(muted(k));
    let v = v.into();
    let text = if v.is_empty() { RichText::new("—").color(theme::muted()) } else if mono { RichText::new(v).monospace() } else { RichText::new(v) };
    ui.add(egui::Label::new(text).selectable(true).wrap());
    ui.end_row();
}

fn grid(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Grid::new(id).num_columns(2).spacing([16.0, 6.0]).min_col_width(110.0).striped(true).show(ui, add);
}

fn when(ts: &str) -> String {
    let f = inspect::fmt_time(ts);
    match inspect::parse_time(ts) {
        Some(t) if !f.is_empty() => format!("{f} ({})", fmt_ago(t)),
        _ => f,
    }
}

fn overview(ui: &mut egui::Ui, d: &Detail, reveal: &mut bool) {
    let quoted = |v: &[String]| v.iter().map(|a| inspect::quote_arg(a)).collect::<Vec<_>>().join(" ");
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(t!("\u{E1CA} Kopírovat JSON", "\u{E1CA} Copy JSON")).on_hover_text(t!("Celý výstup inspect do schránky", "Full inspect output to the clipboard")).clicked() {
                ui.ctx().copy_text(d.raw.clone());
            }
        });
    });
    grid(ui, "detail-general", |ui| {
        kv(ui, t!("Název", "Name"), d.name.clone(), false);
        kv(ui, "ID", d.id.clone(), true);
        kv(ui, "Image", d.image.clone(), false);
        kv(ui, t!("ID image", "Image ID"), d.image_id.clone(), true);
        let mut st = d.status.clone();
        if !d.running && d.status == "exited" {
            st = tf!("{st} (kód {})", "{st} (code {})", d.exit_code);
        }
        if !d.error.is_empty() {
            st = format!("{st} – {}", d.error);
        }
        kv(ui, t!("Stav", "State"), st, false);
        if let Some(h) = &d.health {
            kv(ui, t!("Zdraví", "Health"), h.clone(), false);
        }
        kv(ui, t!("Vytvořen", "Created"), when(&d.created), false);
        kv(ui, t!("Spuštěn", "Started"), when(&d.started), false);
        if !d.running {
            kv(ui, t!("Ukončen", "Finished"), when(&d.finished), false);
        }
        kv(ui, t!("Příkaz", "Command"), quoted(&d.cmd), true);
        kv(ui, "Entrypoint", quoted(&d.entrypoint), true);
        kv(ui, t!("Pracovní adresář", "Working dir"), d.working_dir.clone(), true);
        kv(ui, t!("Uživatel", "User"), d.user.clone(), false);
        kv(ui, "Hostname", d.hostname.clone(), true);
        kv(ui, t!("Restart", "Restart policy"), d.restart.clone(), false);
        kv(ui, t!("Režim sítě", "Network mode"), d.network_mode.clone(), false);
    });

    section(ui, t!("Porty", "Ports"));
    if d.ports.is_empty() {
        ui.label(muted(t!("Žádné porty.", "No ports.")));
    }
    for p in &d.ports {
        let t = if p.host_port.is_empty() { tf!("{} (nepublikováno)", "{} (not published)", p.text()) } else { p.text() };
        // localhost přes ::1 (relay / mirrored) – jen publikované TCP porty na 127.0.0.1
        let hint = p
            .host_port
            .parse::<u16>()
            .ok()
            .filter(|_| p.container.ends_with("/tcp") && crate::portrelay::relayable_ip(&p.host_ip))
            .and_then(|hp| containers::localhost_hint(&[hp]));
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(RichText::new(t).monospace()).selectable(true));
            if let Some((tip, warn)) = hint {
                let (glyph, color) = if warn { ("\u{E4E0}", theme::yellow()) } else { ("\u{E184}", theme::muted()) };
                ui.label(RichText::new(glyph).small().color(color)).on_hover_text(tip);
            }
        });
    }

    section(ui, t!("Připojení (mounty)", "Mounts"));
    if d.mounts.is_empty() {
        ui.label(muted(t!("Žádná připojení.", "No mounts.")));
    } else {
        egui::Grid::new("detail-mounts").num_columns(4).spacing([16.0, 6.0]).striped(true).show(ui, |ui| {
            for m in &d.mounts {
                ui.label(muted(&m.typ));
                let src = if m.typ == "volume" && !m.name.is_empty() { m.name.clone() } else { m.source.clone() };
                ui.add(egui::Label::new(RichText::new(src).monospace()).selectable(true).wrap()).on_hover_text(&m.source);
                ui.add(egui::Label::new(RichText::new(&m.destination).monospace()).selectable(true).wrap());
                ui.label(if m.rw { muted("rw") } else { RichText::new(t!("jen čtení", "read-only")).color(theme::yellow()) });
                ui.end_row();
            }
        });
    }

    section(ui, t!("Sítě", "Networks"));
    if d.networks.is_empty() {
        ui.label(muted(t!("Žádné sítě.", "No networks.")));
    }
    for n in &d.networks {
        ui.label(RichText::new(&n.name).strong());
        grid(ui, &format!("detail-net-{}", n.name), |ui| {
            let ip = if n.ip.is_empty() { String::new() } else { format!("{}/{}", n.ip, n.prefix) };
            kv(ui, t!("IP adresa", "IP address"), ip, true);
            if !n.ip6.is_empty() {
                kv(ui, "IPv6", n.ip6.clone(), true);
            }
            kv(ui, t!("Brána", "Gateway"), n.gateway.clone(), true);
            kv(ui, "MAC", n.mac.clone(), true);
            kv(ui, t!("Aliasy", "Aliases"), n.aliases.join(", "), false);
        });
        ui.add_space(4.0);
    }

    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(t!("Proměnné prostředí", "Environment")).size(17.0).strong());
        let secrets = d.env.iter().any(|(k, _)| inspect::is_secret(k));
        if secrets {
            let (glyph, tip) = if *reveal {
                ("\u{E224}", t!("Skrýt citlivé hodnoty", "Hide sensitive values"))
            } else {
                ("\u{E220}", t!("Zobrazit citlivé hodnoty", "Show sensitive values"))
            };
            if icon(ui, glyph, tip) {
                *reveal = !*reveal;
            }
        }
    });
    ui.add_space(4.0);
    if d.env.is_empty() {
        ui.label(muted(t!("Žádné proměnné.", "No variables.")));
    } else {
        grid(ui, "detail-env", |ui| {
            for (k, v) in &d.env {
                let masked = inspect::is_secret(k) && !*reveal && !v.is_empty();
                let val = if masked { "\u{2022}".repeat(8) } else { v.clone() };
                kv(ui, k, val, true);
            }
        });
    }

    section(ui, t!("Labely", "Labels"));
    if d.labels.is_empty() {
        ui.label(muted(t!("Žádné labely.", "No labels.")));
    } else {
        grid(ui, "detail-labels", |ui| {
            for (k, v) in &d.labels {
                kv(ui, k, v.clone(), false);
            }
        });
    }
    ui.add_space(8.0);
}

fn stats(ui: &mut egui::Ui, feed: &StatsFeed, running: bool) {
    let samples: Vec<StatSample> = feed.samples.lock().map(|g| g.iter().copied().collect()).unwrap_or_default();
    let now = feed.started.elapsed().as_secs_f64();
    if !running {
        ui.label(muted(t!(
            "Kontejner neběží – statistiky se sbírají jen u běžícího kontejneru.",
            "The container is not running – stats are collected only while it runs."
        )));
    }
    let cpu: Vec<(f64, f64)> = samples.iter().filter_map(|s| s.cpu.map(|c| (s.t, c))).collect();
    let mem: Vec<(f64, f64)> = samples.iter().map(|s| (s.t, s.mem as f64)).collect();
    egui::ScrollArea::vertical().id_salt("detail-stats").auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("CPU").size(17.0).strong());
            if let Some((_, c)) = cpu.last() {
                ui.label(muted(tf!("{:.1} %", "{:.1} %", c)));
            }
            ui.label(muted(t!("(100 % = jedno jádro)", "(100 % = one core)")).small());
        });
        let cmax = cpu.iter().map(|p| p.1).fold(0.0, f64::max);
        let ymax = if cmax <= 100.0 { 100.0 } else { (cmax * 1.15 / 50.0).ceil() * 50.0 };
        chart(ui, &cpu, now, ymax, theme::accent(), |v| format!("{v:.1} %"));

        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(t!("Paměť", "Memory")).size(17.0).strong());
            if let Some(s) = samples.last() {
                let limit = if s.mem_limit > 0 { tf!(" / limit {}", " / limit {}", fmt_bytes(s.mem_limit)) } else { String::new() };
                ui.label(muted(format!("{}{limit}", fmt_bytes(s.mem))));
            }
        });
        let mmax = mem.iter().map(|p| p.1).fold(0.0, f64::max);
        let ymax = (mmax * 1.25).max(16.0 * 1024.0 * 1024.0);
        chart(ui, &mem, now, ymax, theme::green(), |v| fmt_bytes(v as u64));
        ui.add_space(4.0);
        ui.label(muted(t!("Vzorek každé ~2 s po dobu otevření okna, posledních 5 minut.", "Sampled every ~2 s while this window is open, last 5 minutes.")).small());
    });
}

/// Čárový graf za posledních `STATS_WINDOW` sekund (sdílený `ui::chart`).
fn chart(ui: &mut egui::Ui, pts: &[(f64, f64)], now: f64, ymax: f64, color: egui::Color32, fmt: impl Fn(f64) -> String) {
    super::chart::line_chart(ui, &[super::chart::Series { name: "", color, pts }], now, STATS_WINDOW, ymax, fmt, &[]);
}

fn recreate(ui: &mut egui::Ui, dv: &mut DetailView, d: &Detail, nets: &[String], actions: &mut Vec<Action>) {
    let name = d.name.clone();
    ui.label(muted(tf!(
        "Kontejner se zastaví a přejmenuje na {name}-wls-old a spustí se nový s nastavením níže. \
         Když nový naběhne, původní se smaže; když ne, nový se odstraní a původní se vrátí (a znovu spustí, pokud běžel).",
        "The container is stopped and renamed to {name}-wls-old, then a new one is started with the settings below. \
         If the new one starts, the original is deleted; otherwise the new one is removed and the original restored (and restarted if it was running)."
    )));
    let warn = |ui: &mut egui::Ui, text: String| {
        ui.label(RichText::new(format!("\u{E4E0} {text}")).color(theme::yellow()).small());
    };
    ui.add_space(4.0);
    if let Some(p) = d.compose_project() {
        warn(ui, tf!(
            "Kontejner patří do compose projektu {p}. Nový kontejner už compose spravovat nebude – raději uprav compose soubor.",
            "The container belongs to compose project {p}. The new container will no longer be managed by compose – consider editing the compose file instead."
        ));
    }
    if d.entrypoint_differs() {
        warn(ui, tf!(
            "Vlastní entrypoint ({}) se nepřenese – použije se entrypoint z image.",
            "The custom entrypoint ({}) is not carried over – the image entrypoint is used.",
            d.entrypoint.join(" ")
        ));
    }
    ui.label(muted(t!(
        "Mění se jen pole níže; ostatní nastavení (labely, limity, capabilities, uživatel…) se převezme z původního kontejneru.",
        "Only the fields below change; other settings (labels, limits, capabilities, user…) are taken over from the original container."
    )).small());
    ui.add_space(8.0);

    let form = dv.form.get_or_insert_with(|| d.to_run_spec());
    let podman = dv.podman;
    let nets_edit = dv.nets.get_or_insert_with(|| d.net_edit(podman));
    let fw = (ui.available_width() - 140.0).clamp(220.0, 560.0);
    egui::Grid::new("recreate-form").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
        ui.label("Image *");
        ui.add(field(&mut form.image).hint_text("nginx:alpine").desired_width(fw));
        ui.end_row();
        ui.label(t!("Název", "Name"));
        ui.add(field(&mut form.name).hint_text(&name).desired_width(fw));
        ui.end_row();
        ui.label(t!("Porty", "Ports"));
        ui.add(field(&mut form.ports).hint_text("8080:80, 127.0.0.1:5432:5432").desired_width(fw));
        ui.end_row();
        ui.label(t!("Proměnné", "Variables"));
        ui.add(area(&mut form.env).hint_text(t!("KLIC=hodnota (řádek = 1)", "KEY=value (one per line)")).desired_rows(3).desired_width(fw));
        ui.end_row();
        ui.label("Volume");
        ui.add(area(&mut form.volumes).hint_text(t!("data:/var/lib/data (řádek = 1)", "data:/var/lib/data (one per line)")).desired_rows(2).desired_width(fw));
        ui.end_row();
        let anon = d.anonymous_volumes(&form.volumes);
        if !anon.is_empty() {
            let list = anon.iter().map(|m| format!("{} \u{2192} {}", &m.name[..12], m.destination)).collect::<Vec<_>>().join(", ");
            ui.label("");
            ui.add(egui::Label::new(muted(tf!("\u{E2CE} Anonymní volume zůstanou připojená: {list}", "\u{E2CE} Anonymous volumes stay attached: {list}")).small()).wrap())
                .on_hover_text(t!(
                    "Připojí se znovu podle názvu, takže jejich data přežijí. Cíl uvedený v poli Volume má přednost.",
                    "They are re-attached by name, so their data survives. A destination listed in the Volume field takes precedence."
                ));
            ui.end_row();
        }
        ui.label(t!("Síť", "Network"));
        let before = form.network.clone();
        egui::ComboBox::from_id_salt("recreate-net")
            .width(fw.min(300.0))
            .selected_text(if form.network.is_empty() { t!("výchozí (bridge)", "default (bridge)") } else { form.network.as_str() })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut form.network, String::new(), t!("výchozí (bridge)", "default (bridge)"));
                for n in nets.iter().filter(|n| !matches!(n.as_str(), "bridge" | "podman")) {
                    ui.selectable_value(&mut form.network, n.clone(), n);
                }
                for n in ["host", "none"] {
                    ui.selectable_value(&mut form.network, n.to_string(), n);
                }
            })
            .response
            .on_hover_text(t!("Hlavní síť (--network)", "Primary network (--network)"));
        ui.end_row();
        if form.network != before {
            // nová hlavní síť nahradí původní; ostatní zůstanou
            let p = form.network.trim().to_string();
            nets_edit.keep.retain(|n| *n != p);
        }
        if !d.networks.is_empty() && !d.special_network_mode() {
            ui.label(t!("Sítě", "Networks"));
            ui.vertical(|ui| networks(ui, d, form.network.trim(), nets_edit));
            ui.end_row();
        }
        ui.label("Restart");
        egui::ComboBox::from_id_salt("recreate-restart").width(fw.min(300.0)).selected_text(form.restart.as_str()).show_ui(ui, |ui| {
            let mut opts = vec!["no".to_string(), "always".into(), "unless-stopped".into(), "on-failure".into()];
            if !opts.contains(&form.restart) {
                opts.push(form.restart.clone());
            }
            for p in opts {
                ui.selectable_value(&mut form.restart, p.clone(), p);
            }
        });
        ui.end_row();
        ui.label(t!("Příkaz", "Command"));
        ui.add(field(&mut form.command).hint_text(t!("výchozí z image", "image default")).desired_width(fw))
            .on_hover_text(t!("Argumenty s mezerami dej do uvozovek: sh -c 'echo a; sleep 1'", "Quote arguments with spaces: sh -c 'echo a; sleep 1'"));
        ui.end_row();
    });
    ui.add_space(10.0);
    egui::Frame::new().fill(theme::bg()).corner_radius(6).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(form.args().join(" ")).monospace().small().color(theme::muted())).wrap());
    });
    ui.add_space(12.0);
    let mut reset = false;
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let ok = !form.image.trim().is_empty();
            let clicked = ui.add_enabled_ui(ok, |ui| accent_button(ui, t!("\u{E094} Znovu vytvořit…", "\u{E094} Recreate…")).clicked()).inner;
            if clicked {
                let new_name = if form.name.trim().is_empty() { name.clone() } else { form.name.trim().to_string() };
                dv.recreating = Some(new_name.clone());
                actions.push(Action::Ask(
                    tf!("Znovu vytvořit {name}?", "Recreate {name}?"),
                    tf!(
                        "Kontejner {name} se zastaví a nahradí novým kontejnerem {new_name} z {}. Data mimo volume (v zapisovatelné vrstvě kontejneru) se ztratí.",
                        "Container {name} will be stopped and replaced by a new container {new_name} from {}. Data outside volumes (in the container's writable layer) will be lost.",
                        form.image.trim()
                    ),
                    Cmd::Recreate { id: d.id.clone(), spec: form.clone(), net: nets_edit.clone() },
                ));
            }
            if ui.button(t!("Obnovit z kontejneru", "Reset from container")).on_hover_text(t!("Zahodit úpravy formuláře", "Discard form changes")).clicked() {
                reset = true;
            }
        });
    });
    if reset {
        dv.form = Some(d.to_run_spec());
        dv.nets = Some(d.net_edit(dv.podman));
    }
}

/// Původní sítě kontejneru: které si nový kontejner ponechá a ve kterých si zachová adresu.
fn networks(ui: &mut egui::Ui, d: &Detail, primary: &str, edit: &mut NetEdit) {
    if primary == "host" || primary == "none" || primary.starts_with("container:") {
        ui.label(muted(tf!("V režimu {primary} se další sítě nepřipojí.", "No other networks are attached in {primary} mode.")).small());
        return;
    }
    for n in &d.networks {
        let is_primary = n.name == primary || (primary.is_empty() && inspect::is_default_net(&n.name));
        ui.horizontal_wrapped(|ui| {
            let mut on = is_primary || edit.keep.contains(&n.name);
            let r = ui.add_enabled(!is_primary, egui::Checkbox::new(&mut on, RichText::new(&n.name).monospace()));
            if r.changed() {
                if on {
                    edit.keep.push(n.name.clone());
                } else {
                    edit.keep.retain(|k| *k != n.name);
                }
            }
            if is_primary {
                badge(ui, t!("hlavní", "primary"), theme::accent());
            }
            let aliases: Vec<&str> = n.aliases.iter().map(String::as_str).filter(|a| !(a.len() >= 12 && d.id.starts_with(a))).collect();
            if !aliases.is_empty() {
                ui.label(muted(tf!("aliasy: {}", "aliases: {}", aliases.join(", "))).small());
            }
            let addr = if n.has_static() {
                [n.static_ip4.as_str(), n.static_ip6.as_str()].into_iter().filter(|a| !a.is_empty()).collect::<Vec<_>>().join(", ")
            } else {
                n.ip.clone()
            };
            if !inspect::is_default_net(&n.name) && !addr.is_empty() {
                let mut pin = edit.pin.contains(&n.name);
                let tip = if n.has_static() {
                    t!("Pevná adresa původního kontejneru.", "Static address of the original container.")
                } else {
                    t!(
                        "Nový kontejner dostane stejnou IP (a MAC) adresu. Podman v inspectu neuvádí, jestli byla adresa pevná.",
                        "The new container gets the same IP (and MAC) address. Podman does not report whether the address was static."
                    )
                };
                let r = ui
                    .add_enabled(on, egui::Checkbox::new(&mut pin, RichText::new(tf!("ponechat IP {addr}", "keep IP {addr}")).small()))
                    .on_hover_text(tip);
                if r.changed() {
                    if pin {
                        edit.pin.push(n.name.clone());
                    } else {
                        edit.pin.retain(|k| *k != n.name);
                    }
                }
            }
        });
    }
}

/// Záložka Limity: současné limity z inspectu a jejich živá změna (`docker update`).
fn limits_tab(ui: &mut egui::Ui, dv: &mut DetailView, d: &Detail, running: bool, actions: &mut Vec<Action>) {
    use crate::limits::{self as lim, Change};
    let l = &d.limits;
    let last = dv.stats.samples.lock().ok().and_then(|g| g.back().copied()).filter(|_| running);
    let unlimited = || t!("bez limitu", "unlimited").to_string();
    section(ui, t!("Současné limity", "Current limits"));
    grid(ui, "detail-limits-now", |ui| {
        let mem = match (l.memory(), last) {
            (Some(m), Some(s)) => tf!("{} · využito {} ({:.0} %)", "{} · used {} ({:.0} %)", fmt_bytes(m), fmt_bytes(s.mem), s.mem as f64 / m as f64 * 100.0),
            (Some(m), None) => fmt_bytes(m),
            (None, Some(s)) if s.mem_limit > 0 => tf!("{} (platí limit WSL {})", "{} (WSL limit {} applies)", unlimited(), fmt_bytes(s.mem_limit)),
            (None, _) => unlimited(),
        };
        kv(ui, t!("Limit paměti", "Memory limit"), mem, false);
        kv(ui, t!("Rezervace (měkký limit)", "Reservation (soft)"), l.reservation().map(fmt_bytes).unwrap_or_else(unlimited), false);
        let swap = match (l.swap(), l.memory()) {
            (Some(s), Some(m)) => tf!("{} celkem (swap {})", "{} in total (swap {})", fmt_bytes(s), fmt_bytes(s.saturating_sub(m))),
            (Some(s), None) => fmt_bytes(s),
            (None, Some(_)) if l.swap == 0 => t!("výchozí", "default").into(),
            _ => unlimited(),
        };
        kv(ui, t!("Paměť + swap", "Memory + swap"), swap, false);
        let cpu = match l.effective_cpus(dv.ncpu) {
            Some(c) => {
                let used = last.and_then(|s| s.cpu).map(|u| tf!(" · využito {:.0} % limitu", " · {:.0} % of the limit used", u / c)).unwrap_or_default();
                format!("{} CPU{used}", lim::fmt_cpus(c))
            }
            None => unlimited(),
        };
        kv(ui, "CPU", cpu, false);
        kv(ui, t!("Váha CPU (shares)", "CPU weight (shares)"), l.shares().map(|s| s.to_string()).unwrap_or_else(|| t!("výchozí", "default").into()), false);
        kv(ui, t!("Jádra (cpuset)", "Cores (cpuset)"), if l.cpuset.is_empty() { t!("všechna", "all").to_string() } else { l.cpuset.clone() }, true);
        kv(ui, t!("Procesy (PIDs)", "Processes (PIDs)"), l.pids().map(|p| p.to_string()).unwrap_or_else(unlimited), false);
    });

    section(ui, t!("Změnit limity", "Change limits"));
    ui.label(muted(t!("Projeví se hned, bez restartu. Prázdné pole = bez limitu.", "Applied immediately, without a restart. Empty field = unlimited.")).small());
    ui.add_space(6.0);
    let form = dv.lform.get_or_insert_with(|| lim::LimitForm::from_limits(l));
    let fw = 110.0;
    let clear = |ui: &mut egui::Ui, s: &mut String| {
        if !s.is_empty() && icon(ui, "\u{E4F6}", t!("Odebrat limit", "Remove limit")) {
            s.clear();
        }
    };
    let unl = t!("bez limitu", "unlimited");
    egui::Grid::new("detail-limits-form").num_columns(3).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label(t!("Limit paměti", "Memory limit"));
        ui.add(field(&mut form.memory).hint_text(unl).desired_width(fw).min_size(egui::vec2(fw, 0.0))).on_hover_text(t!("Např. 512m, 1g, 1.5g", "E.g. 512m, 1g, 1.5g"));
        ui.horizontal(|ui| {
            for (v, label) in lim::MEM_PRESETS {
                if ui.small_button(label).clicked() {
                    form.memory = v.into();
                }
            }
            clear(ui, &mut form.memory);
        });
        ui.end_row();
        ui.label(t!("Rezervace", "Reservation"));
        ui.add(field(&mut form.reservation).hint_text(unl).desired_width(fw).min_size(egui::vec2(fw, 0.0)))
            .on_hover_text(t!("Měkký limit: při nedostatku paměti jádro kontejneru tolik ponechá.", "Soft limit: under memory pressure the kernel keeps this much for the container."));
        ui.label("");
        ui.end_row();
        ui.label(t!("Paměť + swap", "Memory + swap"));
        ui.add(field(&mut form.swap).hint_text(t!("výchozí", "default")).desired_width(fw).min_size(egui::vec2(fw, 0.0)))
            .on_hover_text(t!("Paměť a swap celkem (≥ limit paměti). Rovno limitu = bez swapu.", "Memory and swap in total (≥ memory limit). Equal to the limit = no swap."));
        ui.horizontal(|ui| clear(ui, &mut form.swap));
        ui.end_row();
        ui.label("CPU");
        ui.add(field(&mut form.cpus).hint_text(unl).desired_width(fw).min_size(egui::vec2(fw, 0.0))).on_hover_text(t!("Počet jader, např. 0.5 nebo 2", "Number of cores, e.g. 0.5 or 2"));
        ui.horizontal(|ui| {
            for v in lim::CPU_PRESETS {
                if ui.small_button(format!("{v} CPU")).clicked() {
                    form.cpus = v.into();
                }
            }
            clear(ui, &mut form.cpus);
        });
        ui.end_row();
        ui.label(t!("Procesy", "Processes"));
        ui.add(field(&mut form.pids).hint_text(unl).desired_width(fw).min_size(egui::vec2(fw, 0.0))).on_hover_text(t!("Nejvyšší počet procesů (PIDs)", "Maximum number of processes (PIDs)"));
        ui.horizontal(|ui| clear(ui, &mut form.pids));
        ui.end_row();
    });

    let engine = if dv.podman { crate::engine::Engine::Podman } else { crate::engine::Engine::Docker };
    let edited = *form != lim::LimitForm::from_limits(l);
    let check = form.to_update(l).map_err(|e| e.join("\n")).and_then(|u| lim::update_body(&u, l, engine, dv.ncpu).map(|_| u));
    ui.add_space(8.0);
    if let (Err(e), true) = (&check, edited) {
        ui.label(RichText::new(format!("\u{E4E0} {e}")).color(theme::red()));
    }
    let mut reset = false;
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let ok = !dv.gone && check.is_ok();
            let clicked = ui.add_enabled_ui(ok, |ui| accent_button(ui, t!("\u{E182} Použít", "\u{E182} Apply")).clicked()).inner;
            if let (true, Ok(u)) = (clicked, &check) {
                let cmd = Cmd::UpdateLimits { id: d.id.clone(), name: d.name.clone(), update: *u };
                // limit pod současným využitím -> jádro může procesy v kontejneru ukončit (OOM)
                match (u.memory, last) {
                    (Change::Set(m), Some(s)) if s.mem > m => actions.push(Action::Ask(
                        t!("Limit pod současným využitím", "Limit below current usage").into(),
                        tf!(
                            "Kontejner teď používá {} paměti, nový limit je {}. Jádro může procesy v kontejneru ukončit (OOM). Pokračovat?",
                            "The container uses {} of memory now, the new limit is {}. The kernel may kill processes in the container (OOM). Continue?",
                            fmt_bytes(s.mem),
                            fmt_bytes(m)
                        ),
                        cmd,
                    )),
                    _ => actions.push(Action::Send(cmd)),
                }
            }
            if edited && ui.button(t!("Obnovit z kontejneru", "Reset from container")).on_hover_text(t!("Zahodit úpravy", "Discard changes")).clicked() {
                reset = true;
            }
        });
    });
    if reset {
        dv.lform = Some(lim::LimitForm::from_limits(l));
        dv.lbase = l.clone();
    }
}
