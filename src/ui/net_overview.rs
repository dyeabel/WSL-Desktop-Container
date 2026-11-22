//! Síťový přehled: záložky stránky Sítě (provoz, porty, mapa sítí), záložka Síť v detailu
//! kontejneru (porty dovnitř / ven) a kompaktní ukazatel v seznamu kontejnerů.
//! Data sbírá `worker::net_overview` jen dokud je daný pohled vidět.

use super::chart::{line_chart, Series};
use super::*;
use crate::app::App;
use crate::docker::Container;
use crate::netinfo::{self, fmt_rate, Conn, HostBinding, ListenKind, PortCheck, Sock, WinState};
use crate::worker::net_overview::{open_browser, NetData};
use crate::worker::{NetFeed, NsFeed, NET_WINDOW};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetTab {
    Networks,
    Traffic,
    Ports,
}

/// Stav síťového přehledu (pole `App::net`).
pub struct NetState {
    pub tab: NetTab,
    /// sběr pro záložky Provoz a Porty
    feed: Option<NetFeed>,
    /// sběr portů všech běžících kontejnerů pro ukazatel v seznamu kontejnerů
    list: Option<NsFeed>,
    /// zobrazit v distribuci i přesměrování portů kontejnerů
    all_listeners: bool,
    /// stav shimu jsme si už jednou vyžádali
    shim_asked: bool,
}

impl Default for NetState {
    fn default() -> Self {
        // pro ladění/screenshoty: WSLCD_DEBUG_NETWORK=1 (provoz) | ports | map
        let tab = match std::env::var("WSLCD_DEBUG_NETWORK").as_deref() {
            Ok("ports") => NetTab::Ports,
            Ok("map") | Ok("networks") => NetTab::Networks,
            Ok(_) => NetTab::Traffic,
            Err(_) => NetTab::Networks,
        };
        Self { tab, feed: None, list: None, all_listeners: false, shim_asked: false }
    }
}

/// Přepínač záložek stránky Sítě. Vrací true, když se má kreslit původní záložka Sítě
/// (jinak už je obsah vykreslený).
pub fn tabs(app: &mut App, ui: &mut egui::Ui) -> bool {
    ui.horizontal(|ui| {
        for (t, label) in [
            (NetTab::Networks, t!("\u{E408} Sítě", "\u{E408} Networks")),
            (NetTab::Traffic, t!("\u{E154} Provoz", "\u{E154} Traffic")),
            (NetTab::Ports, t!("\u{EB56} Porty", "\u{EB56} Ports")),
        ] {
            ui.selectable_value(&mut app.net.tab, t, label);
        }
    });
    ui.add_space(8.0);
    match app.net.tab {
        NetTab::Networks => true,
        NetTab::Traffic => {
            traffic(app, ui);
            false
        }
        NetTab::Ports => {
            ports(app, ui);
            false
        }
    }
}

/// Spustí (nebo jen „povzbudí“) sběr přehledu a vrátí kopii dat a aktuální čas feedu.
fn feed_data(app: &mut App, ctx: &egui::Context, traffic: bool) -> (std::sync::Arc<NetData>, f64) {
    let cfg = app.cfg.clone();
    let f = app.net.feed.get_or_insert_with(|| crate::worker::start_net_overview(ctx.clone(), cfg));
    f.touch(traffic);
    let d = f.data.lock().map(|g| g.clone()).unwrap_or_default();
    (d, f.started.elapsed().as_secs_f64())
}

fn loading(ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(muted(t!("Načítám…", "Loading…")));
    });
}

fn mono(s: impl Into<String>) -> RichText {
    RichText::new(s.into()).monospace()
}

fn sel(ui: &mut egui::Ui, text: RichText) -> egui::Response {
    ui.add(egui::Label::new(text).selectable(true))
}

fn mode_name(mode: &str) -> String {
    if mode.trim().is_empty() { t!("NAT (výchozí)", "NAT (default)").into() } else { mode.trim().to_string() }
}

fn mirrored(mode: &str) -> bool {
    mode.trim().eq_ignore_ascii_case("mirrored")
}

/// Text o dostupnosti publikovaných portů z Windows podle režimu sítě WSL.
fn reach_hint(mode: &str) -> &'static str {
    if mirrored(mode) {
        t!(
            "Režim mirrored: publikované porty jsou z Windows dostupné na localhost (IPv4 i IPv6) i na adresách Windows.",
            "Mirrored mode: published ports are reachable from Windows on localhost (IPv4 and IPv6) and on the Windows addresses."
        )
    } else {
        t!(
            "Režim NAT: publikované porty přeposílá WSL do Windows jen na 127.0.0.1 (IPv4); z jiných počítačů dostupné nejsou.",
            "NAT mode: WSL forwards published ports to Windows on 127.0.0.1 only (IPv4); they are not reachable from other computers."
        )
    }
}

// ---------------------------------------------------------------- záložka Provoz

/// Součty stacku: rx/s, tx/s, rx celkem, tx celkem, počet kontejnerů.
type StackSum = (f64, f64, u64, u64, usize);

fn traffic(app: &mut App, ui: &mut egui::Ui) {
    let (d, now) = feed_data(app, ui.ctx(), true);
    if d.updated.is_none() {
        loading(ui);
        return;
    }
    if let Some(e) = &d.error {
        ui.label(RichText::new(e).color(theme::red()));
    }
    // VM
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(t!("Virtuální stroj WSL (eth0)", "WSL virtual machine (eth0)")).size(17.0).strong());
            if let Some((rx, tx)) = d.vm_rate {
                ui.label(RichText::new(format!("\u{E040} {}", fmt_rate(rx))).color(theme::green()).strong())
                    .on_hover_text(t!("Přijímáno (do VM)", "Received (into the VM)"));
                ui.label(RichText::new(format!("\u{E092} {}", fmt_rate(tx))).color(theme::accent()).strong())
                    .on_hover_text(t!("Odesíláno (z VM)", "Sent (from the VM)"));
            }
        });
        if let Some((rx, tx)) = d.vm_total {
            ui.label(muted(tf!(
                "Celkem od startu WSL: přijato {}, odesláno {}",
                "Total since WSL started: received {}, sent {}",
                fmt_bytes(rx),
                fmt_bytes(tx)
            )).small());
        }
        ui.add_space(6.0);
        let rx: Vec<(f64, f64)> = d.history.iter().map(|s| (s.t, s.rx)).collect();
        let tx: Vec<(f64, f64)> = d.history.iter().map(|s| (s.t, s.tx)).collect();
        let max = rx.iter().chain(&tx).map(|p| p.1).fold(0.0, f64::max);
        let ymax = (max * 1.25).max(10.0 * 1024.0);
        line_chart(
            ui,
            &[
                Series { pts: &rx, color: theme::green(), name: t!("přijato", "received") },
                Series { pts: &tx, color: theme::accent(), name: t!("odesláno", "sent") },
            ],
            now,
            NET_WINDOW,
            ymax,
            fmt_rate,
            &[],
        );
        ui.label(muted(t!("Vzorek každé ~2 s, dokud je stránka otevřená; posledních 15 minut.", "Sampled every ~2 s while this page is open; last 15 minutes.")).small());
    });

    section(ui, t!("WSL a koncové body aplikace", "WSL and app endpoints"));
    endpoints(app, ui, &d);

    // kontejnery a stacky
    section(ui, t!("Provoz kontejnerů", "Container traffic"));
    if d.containers.is_empty() {
        ui.label(muted(t!("Žádný běžící kontejner.", "No running container.")));
        return;
    }
    let mut stacks: BTreeMap<String, StackSum> = BTreeMap::new();
    for c in d.containers.iter().filter(|c| !c.stack.is_empty()) {
        let e = stacks.entry(c.stack.clone()).or_default();
        let (rx, tx) = c.rate.unwrap_or((0.0, 0.0));
        *e = (e.0 + rx, e.1 + tx, e.2 + c.rx_total, e.3 + c.tx_total, e.4 + 1);
    }
    let mut list = d.containers.clone();
    sort_items(ui.ctx(), "nettraffic", &mut list, |c, col| {
        let (rx, tx) = c.rate.unwrap_or((0.0, 0.0));
        match col {
            1 => SortKey::text(&c.stack),
            2 => SortKey::num(rx),
            3 => SortKey::num(tx),
            4 => SortKey::num(c.rx_total as f64),
            5 => SortKey::num(c.tx_total as f64),
            _ => SortKey::text(&c.name),
        }
    });
    let rate_cols = |name: &'static str| {
        [
            (name, W::Fr(2.0)),
            ("STACK", W::FrOpt(1.4)),
            ("\u{E040} /s", W::Px(96.0)),
            ("\u{E092} /s", W::Px(96.0)),
            (t!("\u{E040} CELKEM", "\u{E040} TOTAL"), W::PxOpt(96.0)),
            (t!("\u{E092} CELKEM", "\u{E092} TOTAL"), W::PxOpt(96.0)),
        ]
    };
    let rate_cell = |ui: &mut egui::Ui, v: Option<f64>| {
        match v {
            Some(v) if v >= 1.0 => ui.label(mono(fmt_rate(v))),
            Some(v) => ui.label(mono(fmt_rate(v)).color(theme::muted())),
            None => ui.label(muted("…")),
        };
    };
    table_sort(ui, "nettraffic", &rate_cols(t!("KONTEJNER", "CONTAINER")), Sort { key: "nettraffic", cols: &[0, 1, 2, 3, 4, 5] }, |mut body| {
        for c in &list {
            body.row(30.0, |mut r| {
                r.col(|ui| {
                    ui.label(RichText::new(&c.name).strong());
                });
                r.col(|ui| {
                    ui.label(if c.stack.is_empty() { muted("—") } else { RichText::new(&c.stack) });
                });
                r.col(|ui| rate_cell(ui, c.rate.map(|r| r.0)));
                r.col(|ui| rate_cell(ui, c.rate.map(|r| r.1)));
                r.col(|ui| {
                    ui.label(muted(fmt_bytes(c.rx_total)));
                });
                r.col(|ui| {
                    ui.label(muted(fmt_bytes(c.tx_total)));
                });
            });
        }
    });
    if !stacks.is_empty() {
        ui.add_space(8.0);
        let mut sl: Vec<(String, StackSum)> = stacks.into_iter().collect();
        sort_items(ui.ctx(), "netstacks", &mut sl, |(n, s), col| match col {
            1 => SortKey::num(s.4 as f64),
            2 => SortKey::num(s.0),
            3 => SortKey::num(s.1),
            4 => SortKey::num(s.2 as f64),
            5 => SortKey::num(s.3 as f64),
            _ => SortKey::text(n),
        });
        let mut cols = rate_cols(t!("COMPOSE STACK", "COMPOSE STACK"));
        cols[1] = (t!("KONTEJNERŮ", "CONTAINERS"), W::FrOpt(1.0));
        table_sort(ui, "netstacks", &cols, Sort { key: "netstacks", cols: &[0, 1, 2, 3, 4, 5] }, |mut body| {
            for (n, s) in &sl {
                body.row(30.0, |mut r| {
                    r.col(|ui| {
                        ui.label(RichText::new(format!("\u{E466} {n}")).strong());
                    });
                    r.col(|ui| {
                        ui.label(muted(s.4.to_string()));
                    });
                    r.col(|ui| rate_cell(ui, Some(s.0)));
                    r.col(|ui| rate_cell(ui, Some(s.1)));
                    r.col(|ui| {
                        ui.label(muted(fmt_bytes(s.2)));
                    });
                    r.col(|ui| {
                        ui.label(muted(fmt_bytes(s.3)));
                    });
                });
            }
        });
    }
    ui.label(muted(t!("Rychlost z rozdílu statistik kontejnerů mezi vzorky.", "Rate from the difference of container stats between samples.")).small());
}

/// Režim sítě WSL, IP distribuce a koncové body aplikace (API, TLS proxy, roura, shim).
fn endpoints(app: &mut App, ui: &mut egui::Ui, d: &NetData) {
    let (port, tls, npipe_on) = app.cfg.read().map(|c| (c.port, c.tls, c.npipe)).unwrap_or((2375, false, false));
    if !app.net.shim_asked {
        app.net.shim_asked = true;
        if app.shim.lock().map(|s| s.is_none()).unwrap_or(false) {
            app.shim_action(ui.ctx(), crate::shim_install::Action::Check);
        }
    }
    let shim = app.shim.lock().ok().and_then(|g| g.clone());
    let proxy = d.distro_listen.iter().find(|s| netinfo::listen_kind(s, port, &[]) == ListenKind::TlsProxy);
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::Grid::new("net-endpoints").num_columns(2).spacing([16.0, 6.0]).min_col_width(140.0).striped(true).show(ui, |ui| {
            ui.label(muted(t!("Síť WSL", "WSL networking")));
            ui.horizontal_wrapped(|ui| {
                ui.label(mode_name(&d.wsl_mode));
                ui.label(muted(t!("(networkingMode v .wslconfig)", "(networkingMode in .wslconfig)")).small());
            });
            ui.end_row();
            ui.label(muted(t!("IP distribuce", "Distribution IP")));
            sel(ui, mono(d.wsl_ip.clone().unwrap_or_else(|| "—".into())));
            ui.end_row();
            ui.label(muted(t!("Dostupnost z Windows", "Reachable from Windows")));
            ui.add(egui::Label::new(muted(reach_hint(&d.wsl_mode)).small()).wrap());
            ui.end_row();
            ui.label(muted(t!("API enginu", "Engine API")));
            ui.horizontal_wrapped(|ui| {
                sel(ui, mono(format!("tcp://127.0.0.1:{port}")));
                if tls {
                    badge(ui, t!("TLS s certifikátem", "TLS with certificate"), theme::green());
                } else {
                    badge(ui, t!("bez ověření", "no authentication"), theme::yellow());
                }
            });
            ui.end_row();
            if tls {
                ui.label(muted("TLS proxy"));
                match proxy {
                    Some(s) => sel(ui, mono(format!("wslcd-tls-proxy · {} · {}", s.local(), s.process()))),
                    None => ui.label(RichText::new(t!("nenaslouchá", "not listening")).color(theme::red())),
                };
                ui.end_row();
            }
            ui.label(muted(t!("Pojmenovaná roura", "Named pipe")));
            ui.horizontal_wrapped(|ui| {
                sel(ui, mono(crate::npipe::PIPE_NAME));
                match crate::npipe::state() {
                    crate::npipe::State::Running => badge(ui, t!("běží", "running"), theme::green()),
                    crate::npipe::State::Failed(e) => badge(ui, t!("chyba", "error"), theme::red()).on_hover_text(e),
                    crate::npipe::State::Off if npipe_on => badge(ui, t!("spouští se", "starting"), theme::yellow()),
                    crate::npipe::State::Off => badge(ui, t!("vypnuto", "off"), theme::grey()),
                };
            });
            ui.end_row();
            ui.label(muted(t!("Příkaz docker (shim)", "docker command (shim)")));
            ui.horizontal_wrapped(|ui| match &shim {
                None => {
                    ui.spinner();
                }
                Some(s) => {
                    if s.installed && s.in_path {
                        badge(ui, t!("v PATH", "in PATH"), theme::green());
                    } else if s.installed {
                        badge(ui, t!("nainstalován, mimo PATH", "installed, not in PATH"), theme::yellow());
                    } else {
                        badge(ui, t!("nenainstalován", "not installed"), theme::grey());
                    }
                    if let Some(h) = &s.docker_host {
                        sel(ui, mono(format!("DOCKER_HOST={h}")).small());
                    }
                }
            });
            ui.end_row();
        });
    });
}

// ---------------------------------------------------------------- záložka Porty

/// Publikované porty všech kontejnerů (IPv6 duplicita stejné publikace se vynechá).
fn bindings(containers: &[Container]) -> Vec<HostBinding> {
    let mut out: Vec<HostBinding> = Vec::new();
    for c in containers {
        let pubs = netinfo::published_of(c.ports.as_deref().unwrap_or(&[]));
        for p in &pubs {
            let v6_dup = p.host_ip.contains(':')
                && pubs.iter().any(|q| !q.host_ip.contains(':') && q.host_port == p.host_port && q.proto == p.proto && q.private == p.private);
            if v6_dup {
                continue;
            }
            out.push(HostBinding {
                container_id: c.id.clone(),
                container: c.name(),
                stack: c.project().unwrap_or("").to_string(),
                running: c.running() || c.paused(),
                p: p.clone(),
            });
        }
    }
    out
}

fn ports(app: &mut App, ui: &mut egui::Ui) {
    let (d, _) = feed_data(app, ui.ctx(), false);
    if let Some(f) = &app.net.feed {
        f.touch_ports();
    }
    let api_port = app.cfg.read().map(|c| c.port).unwrap_or(2375);
    let mirror = mirrored(&d.wsl_mode);
    ui.label(muted(reach_hint(&d.wsl_mode)).small());
    ui.add_space(6.0);

    // publikované porty
    let distros_cap = app.caps().distros;
    let list = bindings(&app.snap.containers);
    let conflicts = netinfo::container_conflicts(&list);
    let win_ok = d.updated.is_some() && d.win_error.is_none();
    let own = std::process::id();
    let mut rows: Vec<PortRow> = list
        .into_iter()
        .zip(conflicts)
        .map(|(b, c)| {
            let w = (win_ok && !mirror).then(|| netinfo::windows_state(&b.p, &d.win_listen, own));
            let dc = if distros_cap { netinfo::distro_conflicts(&b.p, &d.distros.engine_netns, &d.distros.others) } else { Vec::new() };
            (b, c, w, dc)
        })
        .collect();
    sort_items(ui.ctx(), "ports", &mut rows, |(b, c, w, dc), col| match col {
        1 => SortKey::num(b.p.private),
        2 => SortKey::text(&b.container),
        3 => SortKey::text(&b.stack),
        4 => SortKey::num(if !c.is_empty() || !dc.is_empty() || matches!(w, Some(WinState::Conflict(..))) { 0 } else { 1 }),
        _ => SortKey::num(b.p.host_port),
    });
    let mut open: Option<u16> = None;
    let mut copy: Option<String> = None;
    let cols = [
        (t!("HOSTITEL", "HOST"), W::Fr(1.6)),
        (t!("\u{2192} KONTEJNER", "\u{2192} CONTAINER"), W::Px(120.0)),
        (t!("NÁZEV", "NAME"), W::Fr(1.8)),
        ("STACK", W::FrOpt(1.2)),
        ("WINDOWS", W::Fr(1.6)),
        (t!("AKCE", "ACTIONS"), icons_w(2)),
    ];
    section(ui, t!("Publikované porty", "Published ports"));
    if rows.is_empty() {
        ui.label(muted(t!("Žádný kontejner nepublikuje porty.", "No container publishes ports.")));
    } else {
        table_sort(ui, "ports", &cols, Sort { key: "ports", cols: &[0, 1, 2, 3, 4] }, |mut body| {
            for (b, others, w, dc) in &rows {
                body.row(32.0, |mut r| {
                    let ip = if b.p.host_ip.is_empty() { "0.0.0.0" } else { b.p.host_ip.as_str() };
                    r.col(|ui| {
                        let t = mono(netinfo::join_addr(ip, Some(b.p.host_port)));
                        sel(ui, if b.running { t } else { t.color(theme::muted()) });
                        if !others.is_empty() {
                            badge(ui, t!("\u{E4E0} konflikt", "\u{E4E0} conflict"), theme::red()).on_hover_text(tf!(
                                "Stejný port publikuje i {} – současně poběží jen jeden.",
                                "The same port is also published by {} – only one can run at a time.",
                                others.join(", ")
                            ));
                        }
                    });
                    r.col(|ui| {
                        ui.label(mono(format!("{}/{}", b.p.private, b.p.proto)));
                    });
                    r.col(|ui| {
                        ui.label(if b.running { RichText::new(&b.container).strong() } else { muted(&b.container) });
                        if !b.running {
                            ui.label(muted(t!("(neběží)", "(stopped)")).small());
                        }
                    });
                    r.col(|ui| {
                        ui.label(if b.stack.is_empty() { muted("—") } else { RichText::new(&b.stack) });
                    });
                    r.col(|ui| {
                        if dc.is_empty() {
                            win_cell(ui, w.as_ref(), b, mirror);
                        } else {
                            distro_conflict_badge(ui, dc, b);
                        }
                    });
                    r.col(|ui| {
                        if b.p.proto == "tcp" && b.running && icon(ui, "\u{E5DE}", &tf!("Otevřít http://127.0.0.1:{} v prohlížeči", "Open http://127.0.0.1:{} in the browser", b.p.host_port)) {
                            open = Some(b.p.host_port);
                        }
                        if icon(ui, "\u{E1CA}", t!("Kopírovat adresu", "Copy address")) {
                            copy = Some(format!("127.0.0.1:{}", b.p.host_port));
                        }
                    });
                });
            }
        });
    }
    if let Some(p) = open {
        if let Err(e) = open_browser(p) {
            app.toast(false, e);
        }
    }
    if let Some(c) = copy {
        ui.ctx().copy_text(c);
    }
    if let Some(e) = &d.win_error {
        ui.label(RichText::new(tf!("Porty Windows nelze zjistit: {e}", "Cannot read Windows ports: {e}")).color(theme::yellow()).small());
    }
    if distros_cap {
        distro_scan_note(ui, &d.distros);
    }

    // naslouchající sockety v distribuci
    section(ui, t!("Naslouchající sockety v distribuci", "Listening sockets in the distribution"));
    if d.updated.is_none() {
        loading(ui);
        return;
    }
    let published: Vec<u16> = rows.iter().map(|r| r.0.p.host_port).collect();
    let mut socks: Vec<(Sock, ListenKind)> = d.distro_listen.iter().map(|s| (s.clone(), netinfo::listen_kind(s, api_port, &published))).collect();
    let hidden = socks.iter().filter(|s| s.1 == ListenKind::ContainerProxy).count();
    if !app.net.all_listeners {
        socks.retain(|s| s.1 != ListenKind::ContainerProxy);
    }
    ui.horizontal_wrapped(|ui| {
        if app.caps().root_sampling {
            ui.label(muted(t!("ss -ltnup jako root (kořenový síťový prostor, bez procesů v kontejnerech).", "ss -ltnup as root (root network namespace, without processes inside containers).")).small());
        }
        ui.checkbox(&mut app.net.all_listeners, tf!("i porty kontejnerů ({hidden})", "container ports too ({hidden})"));
    });
    // bez root_sampling (ss -p jako root) není sloupec PROCES; indexy dalších sloupců se posunou
    let rs = app.caps().root_sampling;
    sort_items(ui.ctx(), "netlisten", &mut socks, |(s, k), col| match if rs || col < 2 { col } else { col + 1 } {
        1 => SortKey::text(&s.proto),
        2 => SortKey::text(s.procs.join(",")),
        3 => SortKey::num(*k as u8),
        _ => SortKey::num(s.local_port),
    });
    let cols = [
        (t!("ADRESA", "ADDRESS"), W::Fr(1.6)),
        ("PROTO", W::Px(60.0)),
        (t!("PROCES", "PROCESS"), W::Fr(1.6)),
        (t!("CO TO JE", "WHAT IT IS"), W::Fr(2.0)),
    ];
    let cols: Vec<_> = cols.into_iter().enumerate().filter(|(i, _)| rs || *i != 2).map(|(_, c)| c).collect();
    table_sort(ui, "netlisten", &cols, Sort { key: "netlisten", cols: if rs { &[0, 1, 2, 3] } else { &[0, 1, 2] } }, |mut body| {
        for (s, k) in &socks {
            body.row(30.0, |mut r| {
                r.col(|ui| {
                    sel(ui, mono(s.local()));
                });
                r.col(|ui| {
                    ui.label(muted(&s.proto));
                });
                if rs {
                r.col(|ui| {
                    ui.label(if s.procs.is_empty() { muted("—") } else { RichText::new(s.process()) });
                });
                }
                r.col(|ui| {
                    match k {
                        ListenKind::EngineApi => badge(ui, t!("API enginu", "engine API"), theme::accent()),
                        ListenKind::TlsProxy => badge(ui, "TLS proxy (wslcd-tls-proxy)", theme::green()),
                        ListenKind::Dns => badge(ui, "DNS", theme::grey()),
                        ListenKind::ContainerProxy => badge(ui, t!("port kontejneru", "container port"), theme::grey()),
                        ListenKind::Other => ui.label(""),
                    };
                    if s.loopback_only() {
                        ui.label(muted(t!("jen localhost", "localhost only")).small());
                    }
                });
            });
        }
    });
}

/// Řádek tabulky publikovaných portů: publikace, konflikty s kontejnery, stav ve Windows,
/// distribuce WSL, které drží stejný port.
type PortRow = (HostBinding, Vec<String>, Option<WinState>, Vec<String>);

/// Port drží jiná distribuce WSL – do Windows se přeposílá její socket, ne kontejner.
fn distro_conflict_badge(ui: &mut egui::Ui, distros: &[String], b: &HostBinding) {
    let names = distros.join(", ");
    badge(ui, &tf!("\u{E4E0} distribuce {names}", "\u{E4E0} distribution {names}"), theme::red()).on_hover_text(tf!(
        "Konflikt s distribucí {names}: na portu {} v ní naslouchá jiný proces. Distribuce WSL sdílejí síť, takže wslrelay do Windows přeposílá její socket, ne kontejner.",
        "Conflict with distribution {names}: another process listens on port {} there. WSL distributions share the network, so wslrelay forwards its socket to Windows, not the container.",
        b.p.host_port
    ));
}

/// Stav kontroly ostatních distribucí WSL pod tabulkou publikovaných portů.
fn distro_scan_note(ui: &mut egui::Ui, s: &crate::worker::net_overview::DistroScan) {
    let Some(at) = s.at else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(muted(t!("Kontroluji porty ostatních distribucí WSL…", "Checking ports of other WSL distributions…")).small());
        });
        return;
    };
    let ago = at.elapsed().as_secs();
    let mut text = if s.others.is_empty() {
        tf!("Ostatní distribuce WSL: žádná další neběží (před {ago} s).", "Other WSL distributions: no other one is running ({ago} s ago).")
    } else {
        let names: Vec<&str> = s.others.iter().map(|d| d.distro.as_str()).collect();
        tf!(
            "Ostatní distribuce WSL zkontrolované na konflikty portů: {} (před {ago} s, nejvýš po 10 s).",
            "Other WSL distributions checked for port conflicts: {} ({ago} s ago, at most every 10 s).",
            names.join(", ")
        )
    };
    if !s.skipped.is_empty() {
        text.push(' ');
        text.push_str(&tf!("Nekontrolované (zastavené se nespouštějí, WSL 1): {}.", "Not checked (stopped ones are not started, WSL 1): {}.", s.skipped.join(", ")));
    }
    ui.add(egui::Label::new(muted(text).small()).wrap());
    for e in s.error.iter().chain(s.others.iter().filter_map(|d| d.error.as_ref())) {
        ui.label(RichText::new(e).color(theme::yellow()).small());
    }
}

/// Buňka „Windows“: jak je publikovaný port vidět z Windows.
fn win_cell(ui: &mut egui::Ui, w: Option<&WinState>, b: &HostBinding, mirror: bool) {
    if mirror {
        ui.label(muted("mirrored"));
        return;
    }
    match w {
        None => {
            ui.label(muted("…"));
        }
        Some(WinState::Forwarded) => {
            ui.label(RichText::new(format!("\u{E182} 127.0.0.1:{}", b.p.host_port)).color(theme::green()))
                .on_hover_text(t!("WSL port přeposílá do Windows (wslrelay).", "WSL forwards the port to Windows (wslrelay)."));
        }
        Some(WinState::Conflict(p, pid)) => {
            badge(ui, &format!("\u{E4E0} {p}"), theme::red()).on_hover_text(tf!(
                "Port ve Windows už drží {p} (PID {pid}) – kontejner z Windows na tomto portu nedostupný.",
                "The port is already taken in Windows by {p} (PID {pid}) – the container is not reachable from Windows on it."
            ));
        }
        Some(WinState::NotForwarded) if b.running => {
            ui.label(RichText::new(t!("nepřeposláno", "not forwarded")).color(theme::yellow())).on_hover_text(t!(
                "Ve Windows na portu nikdo nenaslouchá – WSL ho (zatím) nepřeposlalo. Zkontroluj localhostForwarding nebo počkej pár sekund.",
                "Nothing listens on the port in Windows – WSL has not forwarded it (yet). Check localhostForwarding or wait a few seconds."
            ));
        }
        Some(WinState::NotForwarded) => {
            ui.label(muted("—"));
        }
    }
}

// ---------------------------------------------------------------- mapa sítí

/// Gateway sítě z IPAM (prázdné = neznámá).
fn gateway(n: &crate::docker::Network) -> String {
    n.ipam
        .as_ref()
        .and_then(|i| i.config.as_ref())
        .map(|c| c.iter().filter_map(|c| c.gateway.clone()).filter(|g| !g.is_empty()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default()
}

/// Jednoduchá mapa: sítě s podsítí a branou a pod nimi připojené kontejnery (IP, aliasy).
pub fn network_map(app: &mut App, ui: &mut egui::Ui) {
    section(ui, t!("Mapa sítí", "Network map"));
    ui.label(muted(t!("Sítě s podsítí a bránou a připojené kontejnery s adresami a aliasy.", "Networks with subnet and gateway, and the attached containers with addresses and aliases.")).small());
    ui.add_space(4.0);
    let f = app.filter.to_lowercase();
    let mut nets: Vec<&crate::docker::Network> = app.snap.networks.iter().filter(|n| f.is_empty() || n.name.to_lowercase().contains(&f)).collect();
    nets.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    for n in nets {
        let members: Vec<(&Container, &serde_json::Value)> = app
            .snap
            .containers
            .iter()
            .filter_map(|c| c.network_settings.as_ref()?.networks.as_ref()?.get(&n.name).map(|v| (c, v)))
            .collect();
        if members.is_empty() && matches!(n.name.as_str(), "host" | "none") {
            continue;
        }
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("\u{E408} {}", n.name)).strong());
                ui.label(muted(&n.driver));
                let subnet = n.subnet();
                if !subnet.is_empty() {
                    sel(ui, mono(subnet).small());
                }
                let gw = gateway(n);
                if !gw.is_empty() {
                    ui.label(muted(tf!("brána {gw}", "gateway {gw}")).small());
                }
                if n.internal {
                    badge(ui, t!("interní", "internal"), theme::yellow());
                }
            });
            if members.is_empty() {
                ui.label(muted(t!("  bez kontejnerů", "  no containers")).small());
                return;
            }
            egui::Grid::new(("netmap", &n.id)).num_columns(3).spacing([18.0, 4.0]).show(ui, |ui| {
                for (c, v) in &members {
                    let color = if c.running() { theme::green() } else { theme::grey() };
                    ui.horizontal(|ui| {
                        ui.add_space(14.0);
                        ui.label(RichText::new("\u{2514}").color(theme::muted()));
                        dot(ui, color);
                        ui.label(RichText::new(c.name()).strong());
                    });
                    let ip = v.get("IPAddress").and_then(|x| x.as_str()).unwrap_or("");
                    let ip6 = v.get("GlobalIPv6Address").and_then(|x| x.as_str()).unwrap_or("");
                    let addr = [ip, ip6].into_iter().filter(|a| !a.is_empty()).collect::<Vec<_>>().join(", ");
                    sel(ui, if addr.is_empty() { muted("—") } else { mono(addr) });
                    let mut aliases: Vec<String> = Vec::new();
                    for key in ["Aliases", "DNSNames"] {
                        for a in v.get(key).and_then(|x| x.as_array()).into_iter().flatten().filter_map(|x| x.as_str()) {
                            // krátké ID kontejneru jako alias nic neříká
                            if !(a.len() >= 12 && c.id.starts_with(a)) && a != c.name() && !aliases.iter().any(|x| x == a) {
                                aliases.push(a.to_string());
                            }
                        }
                    }
                    ui.label(if aliases.is_empty() { muted("") } else { muted(tf!("aliasy: {}", "aliases: {}", aliases.join(", "))).small() });
                    ui.end_row();
                }
            });
        });
        ui.add_space(6.0);
    }
}

// ---------------------------------------------------------------- detail kontejneru: Síť

/// Záložka Síť v detailu kontejneru: porty dovnitř (naslouchá × publikováno) a ven (spojení).
pub fn container_tab(ui: &mut egui::Ui, feed: &NsFeed, id: &str, c: Option<&Container>, running: bool) {
    feed.touch();
    // z cizího (sdíleného) NsData se kopíruje jen záznam tohoto kontejneru, ne celá mapa
    let d = feed
        .data
        .lock()
        .map(|g| crate::worker::net_overview::NsData {
            by_id: g.by_id.get(id).map(|x| std::collections::HashMap::from([(id.to_string(), x.clone())])).unwrap_or_default(),
            pid: g.pid.get(id).map(|p| std::collections::HashMap::from([(id.to_string(), *p)])).unwrap_or_default(),
            error: g.error.clone(),
            updated: g.updated,
            live: g.live.clone(),
            closed: g.closed.clone(),
            traffic: g.traffic.clone(),
            now: g.now,
        })
        .unwrap_or_default();
    let published = netinfo::published_of(c.and_then(|c| c.ports.as_deref()).unwrap_or(&[]));
    let dump = d.by_id.get(id).cloned().unwrap_or_default();
    egui::ScrollArea::vertical().id_salt("detail-network").auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            if !running {
                ui.label(muted(t!(
                    "Kontejner neběží – naslouchající porty a spojení jde zjistit jen u běžícího kontejneru.",
                    "The container is not running – listening ports and connections are known only while it runs."
                )));
            } else if d.updated.is_none() {
                ui.spinner();
                ui.label(muted(t!("Načítám…", "Loading…")));
            } else {
                if let Some(p) = d.pid.get(id) {
                    ui.label(muted(tf!("PID {p} · síťový prostor přes nsenter", "PID {p} · network namespace via nsenter")).small());
                }
                ui.label(muted(t!("· obnovuje se každé ~2 s", "· refreshed every ~2 s")).small());
            }
        });
        for e in [d.error.as_ref(), dump.error.as_ref()].into_iter().flatten() {
            ui.label(RichText::new(e).color(theme::red()).small());
        }

        // dovnitř
        section(ui, t!("\u{E040} Dovnitř – naslouchající porty", "\u{E040} In – listening ports"));
        let mut rows = netinfo::compare_ports(&dump.listen, &published);
        if !running {
            for r in &mut rows {
                r.listen.clear();
            }
        }
        if rows.is_empty() {
            ui.label(muted(t!("Kontejner nenaslouchá na žádném portu a nic nepublikuje.", "The container listens on no port and publishes nothing.")));
        } else {
            let mut open = None;
            sort_items(ui.ctx(), "netin", &mut rows, |r, col| match col {
                2 => SortKey::text(&r.process),
                3 => SortKey::text(r.published.join(",")),
                4 => SortKey::num(state_rank(r)),
                _ => SortKey::num(r.port),
            });
            let cols = [
                ("PORT", W::Px(90.0)),
                (t!("NASLOUCHÁ NA", "LISTENS ON"), W::Fr(1.4)),
                (t!("PROCES", "PROCESS"), W::FrOpt(1.0)),
                (t!("PUBLIKOVÁNO", "PUBLISHED"), W::Fr(1.4)),
                (t!("STAV", "STATE"), W::Fr(1.4)),
                ("", icons_w(1)),
            ];
            table_sort(ui, "netin", &cols, Sort { key: "netin", cols: &[0, 2, 3, 4] }, |mut body| {
                for r in &rows {
                    body.row(30.0, |mut row| {
                        row.col(|ui| {
                            ui.label(mono(format!("{}/{}", r.port, r.proto)).strong());
                        });
                        row.col(|ui| {
                            sel(ui, if r.listen.is_empty() { muted("—") } else { mono(r.listen.join(", ")) });
                        });
                        row.col(|ui| {
                            ui.label(if r.process.is_empty() { muted("—") } else { RichText::new(&r.process) });
                        });
                        row.col(|ui| {
                            sel(ui, if r.published.is_empty() { muted(t!("nepublikováno", "not published")) } else { mono(r.published.join(", ")) });
                        });
                        row.col(|ui| port_state(ui, r, running));
                        row.col(|ui| {
                            let host = published.iter().find(|p| p.private == r.port && p.proto == r.proto && r.proto == "tcp").map(|p| p.host_port);
                            if let Some(hp) = host.filter(|_| running) {
                                if icon(ui, "\u{E5DE}", &tf!("Otevřít http://127.0.0.1:{hp}", "Open http://127.0.0.1:{hp}")) {
                                    open = Some(hp);
                                }
                            }
                        });
                    });
                }
            });
            if let Some(p) = open {
                let _ = open_browser(p);
            }
        }

        // provoz kontejneru
        section(ui, t!("\u{E154} Provoz kontejneru", "\u{E154} Container traffic"));
        container_chart(ui, &d);

        // spojení
        section(ui, t!("\u{E092} Ven – navázaná spojení", "\u{E092} Out – established connections"));
        let fid = egui::Id::new(("detail-net-filter", id));
        let mut filter: String = ui.ctx().data_mut(|m| m.get_temp(fid)).unwrap_or_default();
        let fl = filter.to_lowercase();
        let matches = |c: &Conn| fl.is_empty() || format!("{} {} {} {}", c.sock.proto, c.sock.local(), c.sock.peer(), c.sock.procs.join(" ")).to_lowercase().contains(&fl);
        let mut conns: Vec<Conn> = d.live.iter().filter(|c| matches(c)).cloned().collect();
        // odchozí nahoře
        conns.sort_by(|a, b| a.inbound.cmp(&b.inbound).then_with(|| a.sock.peer().cmp(&b.sock.peer())));
        sort_items(ui.ctx(), "netout", &mut conns, conn_sort_key);
        ui.horizontal(|ui| {
            ui.add(field(&mut filter).hint_text(t!("\u{E30C} Filtr…", "\u{E30C} Filter…")).desired_width(190.0));
            let out_n = conns.iter().filter(|c| !c.inbound).count();
            ui.label(muted(tf!("{} odchozích, {} příchozích", "{} outgoing, {} incoming", out_n, conns.len() - out_n)).small());
            if !conns.is_empty() && ui.button(t!("\u{E1CA} Kopírovat", "\u{E1CA} Copy")).on_hover_text(t!("Seznam spojení do schránky", "Connection list to the clipboard")).clicked() {
                let text: Vec<String> = conns
                    .iter()
                    .map(|c| {
                        let s = &c.sock;
                        let b = |v: Option<u64>| v.map(|v| v.to_string()).unwrap_or_default();
                        format!("{}\t{}\t{} -> {}\t{}\t{}\t{}", if c.inbound { "in" } else { "out" }, s.proto, s.local(), s.peer(), s.process(), b(s.bytes_in), b(s.bytes_out))
                    })
                    .collect();
                ui.ctx().copy_text(text.join("\n"));
            }
        });
        ui.ctx().data_mut(|m| m.insert_temp(fid, filter.clone()));
        ui.add_space(4.0);
        if conns.is_empty() {
            ui.label(muted(if running { t!("Žádná navázaná spojení.", "No established connections.") } else { "—" }));
        } else {
            let cols = [
                (t!("SMĚR", "DIRECTION"), W::Px(90.0)),
                ("PROTO", W::PxOpt(52.0)),
                (t!("MÍSTNÍ", "LOCAL"), W::FrOpt(1.3)),
                (t!("VZDÁLENÁ ADRESA", "REMOTE ADDRESS"), W::Fr(1.6)),
                (t!("PROCES", "PROCESS"), W::FrOpt(1.1)),
                ("\u{E040} /s", W::Px(84.0)),
                ("\u{E092} /s", W::Px(84.0)),
                (t!("\u{E040} CELKEM", "\u{E040} TOTAL"), W::PxOpt(84.0)),
                (t!("\u{E092} CELKEM", "\u{E092} TOTAL"), W::PxOpt(84.0)),
            ];
            let now = d.now;
            table_sort(ui, "netout", &cols, Sort { key: "netout", cols: &[0, 1, 2, 3, 4, 5, 6, 7, 8] }, |mut body| {
                for c in &conns {
                    let s = &c.sock;
                    body.row(28.0, |mut r| {
                        r.col(|ui| direction(ui, c.inbound));
                        r.col(|ui| {
                            ui.label(muted(&s.proto));
                        });
                        r.col(|ui| {
                            sel(ui, mono(s.local()).color(theme::muted()));
                        });
                        r.col(|ui| {
                            sel(ui, mono(s.peer())).on_hover_text(tf!("Navázáno před {}", "Established {} ago", fmt_dur(now - c.first_seen)));
                        });
                        r.col(|ui| {
                            ui.label(if s.procs.is_empty() { muted("—") } else { RichText::new(s.process()) });
                        });
                        r.col(|ui| rate_cell(ui, c.rate.map(|r| r.0), s.bytes_in.is_some()));
                        r.col(|ui| rate_cell(ui, c.rate.map(|r| r.1), s.bytes_out.is_some()));
                        r.col(|ui| bytes_cell(ui, s.bytes_in));
                        r.col(|ui| bytes_cell(ui, s.bytes_out));
                    });
                }
            });
        }
        if !d.live.is_empty() && d.live.iter().all(|c| c.sock.bytes_in.is_none()) {
            ui.label(RichText::new(t!(
                "ss ani jádro tu neposkytují čítače bajtů pro spojení – rychlosti a součty nejsou k dispozici.",
                "Neither ss nor the kernel provides per-connection byte counters here – rates and totals are not available."
            )).color(theme::yellow()).small());
        }

        // historie
        let closed: Vec<Conn> = d.closed.iter().rev().filter(|c| matches(c)).cloned().collect();
        ui.add_space(6.0);
        egui::CollapsingHeader::new(tf!("Nedávno ukončená spojení ({})", "Recently closed connections ({})", closed.len()))
            .id_salt(("detail-net-closed", id))
            .default_open(false)
            .show(ui, |ui| closed_table(ui, closed, d.now));
        ui.add_space(6.0);
        ui.add(egui::Label::new(muted(t!(
            "ss -tie v síťovém prostoru kontejneru (nsenter jako root v distribuci) – funguje i u image bez nástrojů. Rychlost z čítačů TCP (bytes_received / bytes_acked) mezi vzorky ~2 s; UDP čítače nemá a conntrack se nepoužívá. Historie a graf platí, dokud je záložka otevřená (nejvýš 15 min).",
            "ss -tie in the container's network namespace (nsenter as root in the distribution) – works for images without tools too. Rate from the TCP counters (bytes_received / bytes_acked) between samples ~2 s apart; UDP has no counters and conntrack is not used. History and chart cover the time this tab is open (15 min at most)."
        )).small()).wrap());
    });
}

fn direction(ui: &mut egui::Ui, inbound: bool) {
    if inbound {
        ui.label(muted(t!("\u{E040} příchozí", "\u{E040} incoming")));
    } else {
        ui.label(RichText::new(t!("\u{E092} odchozí", "\u{E092} outgoing")).color(theme::accent()));
    }
}

/// Řazení tabulek spojení: směr, protokol, místní, vzdálená, proces, rychlosti, součty.
fn conn_sort_key(c: &Conn, col: usize) -> SortKey {
    let s = &c.sock;
    let (rin, rout) = c.rate.unwrap_or((-1.0, -1.0));
    let total = |v: Option<u64>| v.map(|v| v as f64).unwrap_or(-1.0);
    match col {
        1 => SortKey::text(&s.proto),
        2 => SortKey::text(s.local()),
        3 => SortKey::text(s.peer()),
        4 => SortKey::text(s.procs.join(",")),
        5 => SortKey::num(rin),
        6 => SortKey::num(rout),
        7 => SortKey::num(total(s.bytes_in)),
        8 => SortKey::num(total(s.bytes_out)),
        _ => SortKey::num(u8::from(c.inbound)),
    }
}

/// Rychlost spojení; `counters` = spojení má čítače (jinak „—“, ne „čekám“).
fn rate_cell(ui: &mut egui::Ui, v: Option<f64>, counters: bool) {
    match v {
        Some(v) if v >= 1.0 => ui.label(mono(fmt_rate(v))),
        Some(v) => ui.label(mono(fmt_rate(v)).color(theme::muted())),
        None if counters => ui.label(muted("…")),
        None => ui.label(muted("—")),
    };
}

fn bytes_cell(ui: &mut egui::Ui, v: Option<u64>) {
    ui.label(match v {
        Some(b) => muted(fmt_bytes(b)),
        None => muted("—"),
    });
}

/// Doba čitelně: `42 s`, `3 min 5 s`, `1 h 20 min`.
fn fmt_dur(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min {} s", s / 60, s % 60),
        _ => format!("{} h {} min", s / 3600, s % 3600 / 60),
    }
}

/// Graf provozu kontejneru (čítače rozhraní v jeho síťovém prostoru) za dobu otevření záložky.
fn container_chart(ui: &mut egui::Ui, d: &crate::worker::net_overview::NsData) {
    let rx: Vec<(f64, f64)> = d.traffic.iter().map(|s| (s.0, s.1)).collect();
    let tx: Vec<(f64, f64)> = d.traffic.iter().map(|s| (s.0, s.2)).collect();
    ui.horizontal_wrapped(|ui| {
        // aktuální hodnoty slouží i jako legenda (barvy křivek)
        let (r, w) = d.traffic.last().map(|s| (s.1, s.2)).unwrap_or((0.0, 0.0));
        ui.label(RichText::new(tf!("\u{E040} přijato {}", "\u{E040} received {}", fmt_rate(r))).color(theme::green()).strong());
        ui.label(RichText::new(tf!("\u{E092} odesláno {}", "\u{E092} sent {}", fmt_rate(w))).color(theme::accent()).strong());
    });
    let max = rx.iter().chain(&tx).map(|p| p.1).fold(0.0, f64::max);
    line_chart(
        ui,
        &[
            Series { pts: &rx, color: theme::green(), name: t!("přijato", "received") },
            Series { pts: &tx, color: theme::accent(), name: t!("odesláno", "sent") },
        ],
        d.now,
        NET_WINDOW,
        (max * 1.25).max(10.0 * 1024.0),
        fmt_rate,
        &[],
    );
    ui.label(muted(t!(
        "Všechna rozhraní kontejneru kromě lo (TCP i UDP), od otevření záložky.",
        "All container interfaces except lo (TCP and UDP), since this tab was opened."
    )).small());
}

/// Tabulka ukončených spojení (nejnovější první).
fn closed_table(ui: &mut egui::Ui, mut closed: Vec<Conn>, now: f64) {
    if closed.is_empty() {
        ui.label(muted(t!("Od otevření záložky žádné spojení neskončilo.", "No connection has closed since this tab was opened.")));
        return;
    }
    sort_items(ui.ctx(), "netclosed", &mut closed, |c, col| match col {
        5 => SortKey::num(c.last_seen - c.first_seen),
        6 => SortKey::num(-c.last_seen),
        _ => conn_sort_key(c, col),
    });
    let cols = [
        (t!("SMĚR", "DIRECTION"), W::Px(90.0)),
        ("PROTO", W::PxOpt(52.0)),
        (t!("MÍSTNÍ", "LOCAL"), W::FrOpt(1.3)),
        (t!("VZDÁLENÁ ADRESA", "REMOTE ADDRESS"), W::Fr(1.6)),
        (t!("PROCES", "PROCESS"), W::FrOpt(1.1)),
        (t!("TRVÁNÍ", "DURATION"), W::PxOpt(96.0)),
        (t!("SKONČILO", "CLOSED"), W::Px(96.0)),
        (t!("\u{E040} CELKEM", "\u{E040} TOTAL"), W::Px(84.0)),
        (t!("\u{E092} CELKEM", "\u{E092} TOTAL"), W::Px(84.0)),
    ];
    table_sort(ui, "netclosed", &cols, Sort { key: "netclosed", cols: &[0, 1, 2, 3, 4, 5, 6, 7, 8] }, |mut body| {
        for c in &closed {
            let s = &c.sock;
            body.row(28.0, |mut r| {
                r.col(|ui| direction(ui, c.inbound));
                r.col(|ui| {
                    ui.label(muted(&s.proto));
                });
                r.col(|ui| {
                    sel(ui, mono(s.local()).color(theme::muted()));
                });
                r.col(|ui| {
                    sel(ui, mono(s.peer()));
                });
                r.col(|ui| {
                    ui.label(if s.procs.is_empty() { muted("—") } else { RichText::new(s.process()) });
                });
                r.col(|ui| {
                    // spojení žilo nejméně mezi prvním a posledním vzorkem
                    ui.label(muted(fmt_dur(c.last_seen - c.first_seen)));
                });
                r.col(|ui| {
                    ui.label(muted(tf!("před {}", "{} ago", fmt_dur(now - c.last_seen))));
                });
                r.col(|ui| bytes_cell(ui, s.bytes_in));
                r.col(|ui| bytes_cell(ui, s.bytes_out));
            });
        }
    });
}

/// Pořadí stavů pro řazení (problémy první).
fn state_rank(r: &PortCheck) -> f64 {
    if r.dead() {
        0.0
    } else if r.loopback_only() {
        1.0
    } else if r.unpublished() {
        2.0
    } else {
        3.0
    }
}

fn port_state(ui: &mut egui::Ui, r: &PortCheck, running: bool) {
    if !running {
        ui.label(muted("—"));
    } else if r.dead() {
        badge(ui, t!("\u{E4E0} nikdo nenaslouchá", "\u{E4E0} nothing listens"), theme::red()).on_hover_text(t!(
            "Port je publikovaný, ale žádný proces v kontejneru na něm nenaslouchá – spojení selžou.",
            "The port is published, but no process in the container listens on it – connections will fail."
        ));
    } else if r.loopback_only() {
        badge(ui, t!("\u{E4E0} jen loopback", "\u{E4E0} loopback only"), theme::yellow()).on_hover_text(t!(
            "Proces naslouchá jen na 127.0.0.1 uvnitř kontejneru – publikovaný port ho nezastihne.",
            "The process listens on 127.0.0.1 inside the container only – the published port cannot reach it."
        ));
    } else if r.unpublished() {
        badge(ui, t!("nepublikováno", "not published"), theme::grey()).on_hover_text(t!(
            "Naslouchá, ale není publikovaný – dostupný jen z kontejnerů ve stejné síti.",
            "Listening but not published – reachable only from containers on the same network."
        ));
    } else {
        badge(ui, t!("\u{E182} publikováno", "\u{E182} published"), theme::green());
    }
}

// ---------------------------------------------------------------- seznam kontejnerů

/// Seznam kontejnerů je vidět – sbírat porty běžících kontejnerů (~5 s).
pub fn touch_list(app: &mut App, ctx: &egui::Context) {
    if !app.snap.connected || !app.snap.containers.iter().any(|c| c.running()) {
        return;
    }
    let cfg = app.cfg.clone();
    app.net.list.get_or_insert_with(|| crate::worker::start_ns_feed(ctx.clone(), cfg, None)).touch();
}

/// Kompaktní ukazatel portů dovnitř / ven v řádku kontejneru (žlutě při problému).
pub fn port_hint(ui: &mut egui::Ui, st: &NetState, c: &Container) {
    if !c.running() {
        return;
    }
    let Some(f) = &st.list else { return };
    let Some(dump) = f.data.lock().ok().and_then(|g| g.by_id.get(&c.id).cloned()) else { return };
    let rows = netinfo::compare_ports(&dump.listen, &netinfo::published_of(c.ports.as_deref().unwrap_or(&[])));
    let n_in = rows.iter().filter(|r| r.listening()).count();
    let n_out = dump.conns.iter().filter(|s| !netinfo::is_inbound(s, &dump.listen)).count();
    let bad: Vec<&PortCheck> = rows.iter().filter(|r| r.dead() || r.loopback_only()).collect();
    let color = if bad.is_empty() { theme::muted() } else { theme::yellow() };
    let mut tip = tf!(
        "Naslouchá na {n_in} portech, {n_out} odchozích spojení (detail → Síť).",
        "Listening on {n_in} ports, {n_out} outgoing connections (detail → Network)."
    );
    for r in rows.iter().filter(|r| r.listening()) {
        tip.push_str(&format!("\n\u{E040} {}/{} {}", r.port, r.proto, if r.published.is_empty() { t!("(nepublikováno)", "(not published)").to_string() } else { format!("\u{2192} {}", r.published.join(", ")) }));
    }
    for r in &bad {
        tip.push_str(&format!(
            "\n\u{E4E0} {}/{} {}",
            r.port,
            r.proto,
            if r.dead() { t!("publikováno, nikdo nenaslouchá", "published, nothing listens") } else { t!("naslouchá jen na loopbacku", "listens on loopback only") }
        ));
    }
    let warn = if bad.is_empty() { "" } else { "\u{E4E0} " };
    ui.label(RichText::new(format!("{warn}\u{E040}{n_in} \u{E092}{n_out}")).small().color(color)).on_hover_text(tip);
}
