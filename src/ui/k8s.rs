//! Stránka Kubernetes: lokální k3s cluster (vytvořit, spustit, zastavit), prostředí
//! (šablona FE + API + DB nebo složka s manifesty), mapa toků a pravidla (NetworkPolicy).
//! Data obnovuje `K8sOp::Snapshot`, jen dokud je stránka otevřená.

use super::*;
use crate::app::App;
use crate::k8s::cluster::ClusterState;
use crate::k8s::policy::{self, Peer, Rule, RuleSet};
use crate::worker::k8s::{K8sSnap, label_key};
use crate::worker::{Cmd, K8sMsg, K8sOp};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum K8sTab {
    #[default]
    Cluster,
    Env,
    Flows,
    Rules,
}

type ProbeKey = (Peer, String, u16);

#[derive(Default)]
pub struct K8sUi {
    pub tab: K8sTab,
    pub ns: Option<String>,
    pub snap: Option<K8sSnap>,
    pub progress: Option<String>,
    polling: bool,
    last_poll: Option<Instant>,
    new_ns: String,
    demo_rules: bool,
    image: String,
    /// rozpracovaná pravidla (None = podle clusteru)
    draft: Option<RuleSet>,
    add_from: Option<Peer>,
    add_to: String,
    add_port: String,
    probes: Vec<(ProbeKey, Option<Result<bool, String>>)>,
    show_manifest: bool,
}

pub fn on_msg(app: &mut App, m: K8sMsg) {
    let k = &mut app.k8s;
    match m {
        K8sMsg::Snapshot(s) => {
            k.polling = false;
            if k.ns.as_ref().is_none_or(|n| !s.namespaces.contains(n)) {
                k.ns = s.namespaces.first().cloned();
            }
            k.snap = Some(*s);
        }
        K8sMsg::Progress(p) => k.progress = p,
        K8sMsg::Probe { from, to, port, result } => {
            let key = (from, to, port);
            match k.probes.iter_mut().find(|(kk, _)| *kk == key) {
                Some(e) => e.1 = Some(result),
                None => k.probes.push((key, Some(result))),
            }
        }
    }
}

fn poll(app: &mut App, ctx: &egui::Context) {
    let every = Duration::from_secs(app.cfg.read().map(|c| c.refresh_secs).unwrap_or(3).clamp(2, 30));
    let k = &mut app.k8s;
    // odpověď se mohla ztratit (chyba workeru) – po 20 s zkusit znovu
    let stuck = k.polling && k.last_poll.is_some_and(|t| t.elapsed() > Duration::from_secs(20));
    if (!k.polling || stuck) && k.last_poll.is_none_or(|t| t.elapsed() >= every) {
        k.polling = true;
        k.last_poll = Some(Instant::now());
        let op = K8sOp::Snapshot { ns: k.ns.clone(), flows: k.tab == K8sTab::Flows };
        app.send(Cmd::K8s(op));
    }
    ctx.request_repaint_after(every);
}

fn refresh_now(app: &mut App) {
    app.k8s.last_poll = None;
    app.k8s.polling = false;
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(ui, "Kubernetes", t!("Lokální cluster k3s pro prostředí z více částí: spustit, vidět toky, omezit komunikaci", "Local k3s cluster for multi-part environments: run them, see the flows, restrict traffic"));
    if !need_engine(app, ui) {
        return;
    }
    poll(app, &ui.ctx().clone());
    ui.horizontal(|ui| {
        for (t, label) in [
            (K8sTab::Cluster, t!("Cluster", "Cluster")),
            (K8sTab::Env, t!("Prostředí", "Environments")),
            (K8sTab::Flows, t!("Mapa toků", "Flow map")),
            (K8sTab::Rules, t!("Pravidla", "Rules")),
        ] {
            if ui.selectable_label(app.k8s.tab == t, label).clicked() && app.k8s.tab != t {
                app.k8s.tab = t;
                refresh_now(app);
            }
        }
    });
    ui.add_space(8.0);
    if let Some(p) = app.k8s.progress.clone() {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(p);
        });
        ui.add_space(6.0);
    }
    let state = app.k8s.snap.as_ref().map(|s| s.state.clone());
    if app.k8s.tab != K8sTab::Cluster && state != Some(ClusterState::Running) {
        ui.label(muted(t!("Cluster neběží – spusť ho na záložce Cluster.", "The cluster is not running – start it on the Cluster tab.")));
        if app.k8s.tab != K8sTab::Cluster && ui.button(t!("Přejít na Cluster", "Go to Cluster")).clicked() {
            app.k8s.tab = K8sTab::Cluster;
        }
        return;
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match app.k8s.tab {
        K8sTab::Cluster => cluster_tab(app, ui),
        K8sTab::Env => env_tab(app, ui),
        K8sTab::Flows => flows_tab(app, ui),
        K8sTab::Rules => rules_tab(app, ui),
    });
}

fn state_badge(ui: &mut egui::Ui, s: Option<&ClusterState>) {
    let (text, c) = match s {
        None => (t!("zjišťuji…", "checking…").to_string(), theme::grey()),
        Some(ClusterState::Missing) => (t!("nevytvořen", "not created").into(), theme::grey()),
        Some(ClusterState::Stopped) => (t!("zastaven", "stopped").into(), theme::grey()),
        Some(ClusterState::Starting) => (t!("startuje", "starting").into(), theme::yellow()),
        Some(ClusterState::Running) => (t!("běží", "running").into(), theme::green()),
        Some(ClusterState::Error(_)) => (t!("chyba", "error").into(), theme::red()),
    };
    badge(ui, &text, c);
}

fn cluster_tab(app: &mut App, ui: &mut egui::Ui) {
    let state = app.k8s.snap.as_ref().map(|s| s.state.clone());
    let busy = app.k8s.progress.is_some();
    let (image, api, range, distro, engine) = app
        .cfg
        .read()
        .map(|c| (c.k3s_image.clone(), c.k3s_api_port, c.k3s_nodeport_range, c.distro.clone(), c.engine.name()))
        .unwrap_or_default();
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(t!("Lokální cluster", "Local cluster")).size(17.0).strong());
            state_badge(ui, state.as_ref());
        });
        ui.label(muted(tf!(
            "k3s s jedním uzlem jako kontejner {} v {engine} ({distro}) · API 127.0.0.1:{api} · zpřístupněné služby na localhost:{}–{}",
            "single-node k3s as container {} in {engine} ({distro}) · API 127.0.0.1:{api} · exposed services on localhost:{}–{}",
            crate::k8s::cluster::NAME,
            range.0,
            range.1
        )));
        ui.label(muted(tf!("Image {image}", "Image {image}")));
        if !distro.starts_with(crate::lite::DEFAULT_NAME) {
            ui.label(muted(t!(
                "Tip: nejrychlejší start má lehká distribuce WSLCD Lite (Nastavení → Přidat distribuci) – API do ~2 s, bez systemd.",
                "Tip: the light WSLCD Lite distribution starts fastest (Settings → Add distribution) – API within ~2 s, no systemd."
            )));
        }
        if let Some(ClusterState::Error(e)) = &state {
            ui.label(RichText::new(e).color(theme::red()));
        }
        ui.add_space(8.0);
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal(|ui| match state {
                Some(ClusterState::Missing) => {
                    if accent_button(ui, t!("\u{E3D0} Vytvořit a spustit cluster", "\u{E3D0} Create and start the cluster")).clicked() {
                        app.send(Cmd::K8s(K8sOp::Create));
                    }
                    ui.label(muted(t!("první start stáhne ~300 MB a trvá zhruba minutu", "the first start downloads ~300 MB and takes about a minute")));
                }
                Some(ClusterState::Stopped) => {
                    if accent_button(ui, t!("\u{E3D0} Spustit", "\u{E3D0} Start")).clicked() {
                        app.send(Cmd::K8s(K8sOp::Start));
                    }
                    delete_button(app, ui);
                }
                Some(ClusterState::Running) | Some(ClusterState::Starting) => {
                    if ui.button(t!("\u{E46C} Zastavit", "\u{E46C} Stop")).clicked() {
                        app.send(Cmd::K8s(K8sOp::Stop));
                    }
                    if ui.button(t!("\u{E2F2} Zapsat kubeconfig", "\u{E2F2} Write kubeconfig")).clicked() {
                        app.send(Cmd::K8s(K8sOp::Kubeconfig));
                    }
                    delete_button(app, ui);
                }
                _ => {}
            });
        });
    });
    if state != Some(ClusterState::Running) {
        return;
    }
    section(ui, t!("Přístup z Windows", "Access from Windows"));
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let path = crate::k8s::kubeconfig::path().map(|p| p.display().to_string()).unwrap_or_default();
        if crate::config::portable_dir().is_some() {
            ui.label(tf!(
                "Kubeconfig je ve složce aplikace (portable, nic se nepíše do ~/.kube): {path}",
                "The kubeconfig lives in the app folder (portable, nothing is written to ~/.kube): {path}"
            ));
        } else {
            ui.label(tf!(
                "Kubeconfig je v datové složce aplikace (nic se nepíše do ~/.kube): {path}",
                "The kubeconfig lives in the app's data folder (nothing is written to ~/.kube): {path}"
            ));
            ui.label(RichText::new(t!(
                "Aplikace neběží v portable režimu (vedle EXE chybí PORTABLE.txt), proto jsou data v %APPDATA%.",
                "The app is not running in portable mode (no PORTABLE.txt next to the EXE), so the data is in %APPDATA%."
            )).small().color(theme::muted()));
        }
        let cmd = format!("$env:KUBECONFIG = \"{path}\"; kubectl get pods -A");
        ui.horizontal(|ui| {
            ui.label(RichText::new(&cmd).monospace());
            if ui.small_button("\u{E1CA}").on_hover_text(t!("Kopírovat", "Copy")).clicked() {
                ui.ctx().copy_text(cmd.clone());
            }
        });
        ui.label(muted(t!(
            "kubectl ve Windows není potřeba – aplikace používá kubectl uvnitř uzlu. Pro vlastní práci: winget install Kubernetes.kubectl",
            "kubectl on Windows is not required – the app uses kubectl inside the node. For your own work: winget install Kubernetes.kubectl"
        )));
    });
    section(ui, t!("Vlastní image do clusteru", "Your own image into the cluster"));
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(muted(t!(
            "Image sestavený v enginu se do clusteru načte bez registry. V manifestu pak stačí imagePullPolicy: IfNotPresent.",
            "An image built in the engine is loaded into the cluster without a registry. Use imagePullPolicy: IfNotPresent in the manifest."
        )));
        let tags: Vec<String> = app.snap.images.iter().flat_map(|i| i.tags()).filter(|t| !t.contains("<none>")).collect();
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("k8s-img")
                .width(320.0)
                .selected_text(if app.k8s.image.is_empty() { t!("vyber image…", "pick an image…") } else { app.k8s.image.as_str() })
                .show_ui(ui, |ui| {
                    for t in &tags {
                        ui.selectable_value(&mut app.k8s.image, t.clone(), t);
                    }
                });
            if ui.add_enabled(!app.k8s.image.is_empty() && !busy, egui::Button::new(t!("\u{E20C} Načíst do clusteru", "\u{E20C} Load into cluster"))).clicked() {
                app.send(Cmd::K8s(K8sOp::LoadImage(app.k8s.image.clone())));
            }
        });
    });
}

fn delete_button(app: &mut App, ui: &mut egui::Ui) {
    if ui.button(t!("\u{EC54} Smazat cluster", "\u{EC54} Delete cluster")).clicked() {
        app.ask(
            t!("Smazat cluster?", "Delete the cluster?"),
            t!(
                "Odstraní se kontejner uzlu, svazek s daty clusteru (všechna prostředí, pody, data databází) i síť. Nejde vrátit.",
                "Removes the node container, the cluster data volume (all environments, pods, database data) and the network. Cannot be undone."
            ),
            Cmd::K8s(K8sOp::Delete),
        );
    }
}

fn ns_picker(app: &mut App, ui: &mut egui::Ui) -> bool {
    let names = app.k8s.snap.as_ref().map(|s| s.namespaces.clone()).unwrap_or_default();
    let mut changed = false;
    ui.label(t!("Prostředí:", "Environment:"));
    let cur = app.k8s.ns.clone().unwrap_or_else(|| "—".into());
    egui::ComboBox::from_id_salt("k8s-ns").selected_text(cur).show_ui(ui, |ui| {
        for n in &names {
            if ui.selectable_label(app.k8s.ns.as_deref() == Some(n), n).clicked() {
                app.k8s.ns = Some(n.clone());
                app.k8s.draft = None;
                changed = true;
            }
        }
    });
    if changed {
        refresh_now(app);
    }
    !names.is_empty()
}

fn env_tab(app: &mut App, ui: &mut egui::Ui) {
    let busy = app.k8s.progress.is_some();
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(t!("Nové prostředí", "New environment")).strong());
        ui.horizontal(|ui| {
            if app.k8s.new_ns.is_empty() && app.k8s.snap.as_ref().is_some_and(|s| s.namespaces.is_empty()) {
                app.k8s.new_ns = "demo".into();
            }
            ui.add(field(&mut app.k8s.new_ns).hint_text(t!("název (např. demo)", "name (e.g. demo)")).desired_width(160.0));
            ui.add_enabled_ui(!busy && !app.k8s.new_ns.trim().is_empty(), |ui| {
                if accent_button(ui, t!("\u{E3D4} Šablona FE + API + DB", "\u{E3D4} FE + API + DB template"))
                    .on_hover_text(t!("nginx → PostgREST → PostgreSQL, fe zpřístupněné na localhost", "nginx → PostgREST → PostgreSQL, fe exposed on localhost"))
                    .clicked()
                {
                    let ns = app.k8s.new_ns.trim().to_string();
                    app.send(Cmd::K8s(K8sOp::DeployDemo { ns: ns.clone(), with_rules: app.k8s.demo_rules }));
                    app.k8s.ns = Some(ns);
                    app.k8s.draft = None;
                }
                if ui.button(t!("\u{E256} Nasadit složku s YAML…", "\u{E256} Deploy a folder of YAML…")).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        let ns = app.k8s.new_ns.trim().to_string();
                        app.send(Cmd::K8s(K8sOp::Deploy { ns: ns.clone(), folder: dir.display().to_string() }));
                        app.k8s.ns = Some(ns);
                    }
                }
            });
            ui.checkbox(&mut app.k8s.demo_rules, t!("se vzorovými pravidly", "with sample rules"));
        });
        ui.label(muted(t!(
            "Složka: nasadí se všechny *.yaml / *.yml přímo ve složce (kubectl apply) do zadaného prostředí.",
            "Folder: all *.yaml / *.yml directly in the folder are applied (kubectl apply) to the given environment."
        )));
    });
    ui.add_space(8.0);
    let mut delete = None;
    ui.horizontal(|ui| {
        if ns_picker(app, ui) {
            if let Some(ns) = app.k8s.ns.clone() {
                if ui.button(t!("\u{EC54} Smazat prostředí", "\u{EC54} Delete environment")).clicked() {
                    delete = Some(ns);
                }
            }
        }
    });
    if let Some(ns) = delete {
        app.ask(tf!("Smazat prostředí {ns}?", "Delete environment {ns}?"), t!("Smaže se namespace se vším, co v něm běží.", "The namespace and everything in it is deleted."), Cmd::K8s(K8sOp::DeleteEnv(ns)));
    }
    let Some(snap) = app.k8s.snap.clone() else { return };
    if let Some(e) = &snap.error {
        ui.label(RichText::new(e).color(theme::red()));
    }
    let Some(env) = snap.env else {
        if snap.namespaces.is_empty() {
            ui.add_space(8.0);
            ui.label(muted(t!("Zatím žádné prostředí. Začni šablonou FE + API + DB.", "No environment yet. Start with the FE + API + DB template.")));
        }
        return;
    };
    section(ui, tf!("Pody ({})", "Pods ({})", env.pods.len()).as_str());
    let (distro, engine) = app.cfg.read().map(|c| (c.distro.clone(), c.engine)).unwrap_or_default();
    let cols = [
        (t!("POD", "POD"), W::Fr(2.0)),
        (t!("KOMPONENTA", "COMPONENT"), W::Fr(1.0)),
        (t!("STAV", "STATE"), W::Fr(1.2)),
        (t!("PŘIPRAVENO", "READY"), W::Px(90.0)),
        (t!("RESTARTY", "RESTARTS"), W::Px(80.0)),
        ("IP", W::Px(110.0)),
        ("IMAGE", W::FrOpt(2.0)),
        ("", icons_w(1)),
    ];
    table(ui, "k8s-pods", &cols, |mut body| {
        for p in &env.pods {
            body.row(32.0, |mut r| {
                r.col(|ui| {
                    ui.label(RichText::new(&p.name).strong());
                });
                r.col(|ui| {
                    ui.label(&p.component);
                });
                r.col(|ui| {
                    let ok = p.phase == "Running" && p.ready.0 == p.ready.1;
                    let text = p.waiting.clone().unwrap_or_else(|| p.phase.clone());
                    badge(ui, &text, if ok { theme::green() } else if p.waiting.is_some() { theme::yellow() } else { theme::grey() });
                });
                r.col(|ui| {
                    ui.label(format!("{}/{}", p.ready.0, p.ready.1));
                });
                r.col(|ui| {
                    ui.label(p.restarts.to_string());
                });
                r.col(|ui| {
                    ui.label(RichText::new(p.ip.clone().unwrap_or_default()).monospace());
                });
                r.col(|ui| {
                    ui.label(muted(p.images.join(", ")));
                });
                r.col(|ui| {
                    if icon(ui, "\u{E2F0}", t!("Logy ve Windows Terminalu", "Logs in Windows Terminal")) {
                        let cli = match engine {
                            crate::engine::Engine::Docker => "docker",
                            crate::engine::Engine::Podman => "podman",
                        };
                        let args: Vec<String> = ["-d", &distro, "-u", "root", "--", cli, "exec", "-it", crate::k8s::cluster::NAME, "kubectl", "logs", "-f", "--tail", "300", "-n", &env.ns, &p.name]
                            .iter()
                            .map(|s| s.to_string())
                            .collect();
                        if let Err(e) = crate::wsl::open_terminal(&format!("logs {}", p.name), &args) {
                            app.toast(false, e);
                        }
                    }
                });
            });
        }
    });
    section(ui, tf!("Služby ({})", "Services ({})", env.svcs.len()).as_str());
    let range = app.cfg.read().map(|c| c.k3s_nodeport_range).unwrap_or((30080, 30099));
    let cols = [
        (t!("SLUŽBA", "SERVICE"), W::Fr(1.5)),
        (t!("TYP", "TYPE"), W::Px(100.0)),
        ("CLUSTER IP", W::Px(120.0)),
        (t!("PORTY", "PORTS"), W::Fr(1.0)),
        (t!("Z WINDOWS", "FROM WINDOWS"), W::Fr(1.5)),
    ];
    let mut expose = None;
    table(ui, "k8s-svcs", &cols, |mut body| {
        for s in &env.svcs {
            body.row(32.0, |mut r| {
                r.col(|ui| {
                    ui.label(RichText::new(&s.name).strong());
                });
                r.col(|ui| {
                    ui.label(&s.kind);
                });
                r.col(|ui| {
                    ui.label(RichText::new(s.cluster_ip.clone().unwrap_or_default()).monospace());
                });
                r.col(|ui| {
                    ui.label(s.ports.iter().map(|p| format!("{}/{}", p.port, p.proto)).collect::<Vec<_>>().join(", "));
                });
                r.col(|ui| match s.ports.iter().find_map(|p| p.node_port.filter(|n| (range.0..=range.1).contains(n))) {
                    Some(np) => {
                        ui.hyperlink_to(format!("http://localhost:{np}"), format!("http://localhost:{np}"));
                    }
                    None => {
                        if ui.small_button(t!("Zpřístupnit na localhost", "Expose on localhost")).clicked() {
                            expose = Some(s.name.clone());
                        }
                    }
                });
            });
        }
    });
    if let Some(svc) = expose {
        app.send(Cmd::K8s(K8sOp::Expose { ns: env.ns.clone(), svc }));
        refresh_now(app);
    }
}

fn flows_tab(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ns_picker(app, ui);
    });
    let Some(env) = app.k8s.snap.as_ref().and_then(|s| s.env.clone()) else {
        ui.label(muted(t!("Vyber prostředí.", "Pick an environment.")));
        return;
    };
    ui.label(muted(t!(
        "Spojení z conntracku uzlu: plná šipka = živé spojení, šedá = ukončené v posledních ~2 min, zelená přerušovaná = povoleno pravidlem, ale nic neteče. Číslo je port cíle.",
        "Connections from the node's conntrack: solid arrow = live, grey = closed within ~2 min, dashed green = allowed by a rule but idle. The number is the target port."
    )));
    ui.add_space(6.0);
    let edges = super::k8s_map::collapse(&env.edges);
    let rules = app.k8s.draft.clone().unwrap_or(env.rules.clone());
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        super::k8s_map::draw(ui, &env.pods, &edges, &rules);
    });
    section(ui, t!("Spojení", "Connections"));
    let cols = [
        (t!("ODKUD", "FROM"), W::Fr(1.0)),
        (t!("KAM", "TO"), W::Fr(1.0)),
        ("PORT", W::Px(70.0)),
        (t!("PŘES SLUŽBU", "VIA SERVICE"), W::Fr(1.0)),
        (t!("ŽIVÁ", "LIVE"), W::Px(70.0)),
        (t!("NEDÁVNÁ", "RECENT"), W::Px(80.0)),
    ];
    table(ui, "k8s-flows", &cols, |mut body| {
        for e in &edges {
            body.row(28.0, |mut r| {
                r.col(|ui| {
                    ui.label(e.from.label());
                });
                r.col(|ui| {
                    ui.label(e.to.label());
                });
                r.col(|ui| {
                    ui.label(e.port.to_string());
                });
                r.col(|ui| {
                    ui.label(muted(e.via.clone().unwrap_or_else(|| "—".into())));
                });
                r.col(|ui| {
                    ui.label(e.live.to_string());
                });
                r.col(|ui| {
                    ui.label(muted(e.recent.to_string()));
                });
            });
        }
    });
}

fn peer_label(p: &Peer) -> String {
    match p {
        Peer::Component(c) => c.clone(),
        Peer::Localhost => t!("localhost (Windows)", "localhost (Windows)").into(),
    }
}

fn probe_ui(app: &mut App, ui: &mut egui::Ui, ns: &str, key: ProbeKey) {
    let state = app.k8s.probes.iter().find(|(k, _)| *k == key).map(|(_, r)| r.clone());
    if ui.small_button(t!("Zkusit spojení", "Test connection")).clicked() {
        match app.k8s.probes.iter_mut().find(|(k, _)| *k == key) {
            Some(e) => e.1 = None,
            None => app.k8s.probes.push((key.clone(), None)),
        }
        app.send(Cmd::K8s(K8sOp::Probe { ns: ns.to_string(), from: key.0.clone(), to: key.1.clone(), port: key.2 }));
    }
    match state {
        Some(None) => {
            ui.spinner();
        }
        Some(Some(Ok(true))) => {
            badge(ui, t!("prošlo", "open"), theme::green());
        }
        Some(Some(Ok(false))) => {
            badge(ui, t!("zablokováno / nedostupné", "blocked / unreachable"), theme::red()).on_hover_text(t!(
                "NetworkPolicy zahazuje tiše – vypadá to jako timeout",
                "NetworkPolicy drops silently – it looks like a timeout"
            ));
        }
        Some(Some(Err(e))) => {
            badge(ui, t!("chyba", "error"), theme::yellow()).on_hover_text(e);
        }
        None => {}
    }
}

fn rules_tab(app: &mut App, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ns_picker(app, ui);
    });
    let Some(env) = app.k8s.snap.as_ref().and_then(|s| s.env.clone()) else {
        ui.label(muted(t!("Vyber prostředí.", "Pick an environment.")));
        return;
    };
    let ns = env.ns.clone();
    let mut comps: Vec<String> = env.pods.iter().map(|p| p.component.clone()).collect();
    comps.sort();
    comps.dedup();
    let dirty = app.k8s.draft.as_ref().is_some_and(|d| *d != env.rules);
    let mut set = app.k8s.draft.clone().unwrap_or(env.rules.clone());
    ui.label(muted(t!(
        "Pravidla se převádějí na NetworkPolicy, které k3s vynucuje (kube-router). Komponenty = pody se stejným labelem app.",
        "Rules become NetworkPolicies enforced by k3s (kube-router). Components = pods with the same app label."
    )));
    ui.add_space(6.0);
    let mut changed = false;
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        changed |= ui
            .checkbox(&mut set.deny_all, t!("Vše ostatní v prostředí zakázat (default deny, DNS zůstane povolené)", "Deny everything else in the environment (default deny, DNS stays allowed)"))
            .changed();
        ui.add_space(6.0);
        let mut remove = None;
        for (i, r) in set.rules.clone().iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}  →  {} : {}", peer_label(&r.from), r.to, r.port)).monospace());
                if icon(ui, "\u{E4F6}", t!("Odebrat", "Remove")) {
                    remove = Some(i);
                }
                probe_ui(app, ui, &ns, (r.from.clone(), r.to.clone(), r.port));
            });
        }
        if let Some(i) = remove {
            set.rules.remove(i);
            changed = true;
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(t!("Povolit", "Allow"));
            let from_text = app.k8s.add_from.as_ref().map(peer_label).unwrap_or_else(|| t!("odkud…", "from…").into());
            egui::ComboBox::from_id_salt("k8s-from").selected_text(from_text).show_ui(ui, |ui| {
                ui.selectable_value(&mut app.k8s.add_from, Some(Peer::Localhost), peer_label(&Peer::Localhost))
                    .on_hover_text(t!("provoz z Windows přes NodePort (pravidlo povolí provoz z uzlu)", "traffic from Windows via NodePort (the rule allows traffic from the node)"));
                for c in &comps {
                    ui.selectable_value(&mut app.k8s.add_from, Some(Peer::Component(c.clone())), c);
                }
            });
            ui.label("→");
            egui::ComboBox::from_id_salt("k8s-to").selected_text(if app.k8s.add_to.is_empty() { t!("kam…", "to…") } else { app.k8s.add_to.as_str() }).show_ui(ui, |ui| {
                for c in &comps {
                    ui.selectable_value(&mut app.k8s.add_to, c.clone(), c);
                }
            });
            ui.label(":");
            ui.add(field(&mut app.k8s.add_port).hint_text("port").desired_width(70.0));
            let port: Option<u16> = app.k8s.add_port.trim().parse().ok().filter(|p| *p > 0);
            let ok = app.k8s.add_from.is_some() && !app.k8s.add_to.is_empty() && port.is_some();
            if ui.add_enabled(ok, egui::Button::new(t!("\u{E3D4} Přidat", "\u{E3D4} Add"))).clicked() {
                let r = Rule { from: app.k8s.add_from.clone().unwrap_or(Peer::Localhost), to: app.k8s.add_to.clone(), port: port.unwrap_or(0) };
                if !set.rules.contains(&r) {
                    set.rules.push(r);
                    changed = true;
                }
            }
        });
        // porty cílové komponenty jako nápověda (porty kontejnerů = porty služeb s targetPort)
        if !app.k8s.add_to.is_empty() {
            let ports: Vec<String> = env
                .svcs
                .iter()
                .filter(|s| env.pods.iter().any(|p| p.component == app.k8s.add_to && s.selects(p)))
                .flat_map(|s| s.ports.iter().map(|p| p.port.to_string()))
                .collect();
            if !ports.is_empty() {
                ui.label(muted(tf!("porty služeb komponenty: {}", "the component's service ports: {}", ports.join(", "))));
            }
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.add_enabled(dirty || changed, egui::Button::new(RichText::new(t!("\u{E3D8} Použít", "\u{E3D8} Apply")).color(theme::accent_text())).fill(theme::accent())).clicked() {
                app.send(Cmd::K8s(K8sOp::ApplyRules { ns: ns.clone(), set: set.clone() }));
                app.k8s.draft = None;
                refresh_now(app);
                changed = false;
            }
            if ui.add_enabled(dirty || changed, egui::Button::new(t!("Zahodit změny", "Discard changes"))).clicked() {
                app.k8s.draft = None;
                changed = false;
            }
            ui.toggle_value(&mut app.k8s.show_manifest, t!("Zobrazit manifest", "Show manifest"));
            if dirty || changed {
                badge(ui, t!("neuloženo", "not applied"), theme::yellow());
            }
        });
    });
    if changed {
        app.k8s.draft = Some(set.clone());
    }
    if app.k8s.show_manifest {
        let text = policy::as_list(&policy::to_manifests(&ns, &set, label_key(&env.pods), &["<brána podů uzlu>/32".into()]));
        let mut t = text;
        ui.add(area(&mut t).code_editor().desired_width(f32::INFINITY).desired_rows(14).interactive(false));
    }
    if !env.foreign.is_empty() {
        section(ui, t!("Další politiky v prostředí (jen ke čtení)", "Other policies in the environment (read-only)"));
        ui.label(env.foreign.join(", "));
    }
    section(ui, t!("Zkusit libovolné spojení", "Test any connection"));
    ui.horizontal(|ui| {
        ui.label(muted(t!("vyber zdroj, cíl a port ve formuláři výše – test proběhne bez uložení pravidla", "pick source, target and port in the form above – the test runs without saving a rule")));
        let port: Option<u16> = app.k8s.add_port.trim().parse().ok();
        if let (Some(from), false, Some(port)) = (app.k8s.add_from.clone(), app.k8s.add_to.is_empty(), port) {
            probe_ui(app, ui, &ns, (from, app.k8s.add_to.clone(), port));
        }
    });
}
