use crate::config::Config;
use crate::docker::{DfSummary, Snapshot, StatRow};
use crate::theme;
use crate::ui;
use crate::worker::{self, Cmd, Msg, RunSpec, Shared};
use crate::wsl::Distro;
use crate::wslconfig;
use eframe::egui::{self, RichText};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Dashboard,
    Containers,
    Images,
    Volumes,
    Networks,
    Compose,
    Kubernetes,
    Files,
    Events,
    Engine,
    Settings,
}

impl Page {
    pub const MAIN: [Page; 9] = [Page::Dashboard, Page::Containers, Page::Images, Page::Volumes, Page::Networks, Page::Compose, Page::Kubernetes, Page::Files, Page::Events];
    pub const BOTTOM: [Page; 2] = [Page::Engine, Page::Settings];
    pub fn label(self) -> &'static str {
        match self {
            Page::Dashboard => t!("\u{E2C2}  Přehled", "\u{E2C2}  Overview"),
            Page::Containers => t!("\u{E1DA}  Kontejnery", "\u{E1DA}  Containers"),
            Page::Images => t!("\u{E2CA}  Image", "\u{E2CA}  Images"),
            Page::Volumes => t!("\u{E2A0}  Volume", "\u{E2A0}  Volumes"),
            Page::Networks => t!("\u{E408}  Sítě", "\u{E408}  Networks"),
            Page::Compose => "\u{E466}  Compose",
            Page::Kubernetes => "\u{E1B6}  Kubernetes",
            Page::Files => t!("\u{E260}  Soubory", "\u{E260}  Files"),
            Page::Events => t!("\u{E0CE}  Události", "\u{E0CE}  Events"),
            Page::Engine => "\u{EB02}  WSL engine",
            Page::Settings => t!("\u{E270}  Nastavení", "\u{E270}  Settings"),
        }
    }
}

pub struct Toast {
    pub ok: bool,
    pub text: String,
    pub until: f64,
}

pub struct Confirm {
    pub title: String,
    pub text: String,
    pub cmd: Cmd,
}

pub use crate::ui::logs::LogView;

/// Výška tlačítka navigace a mezera mezi nimi v postranním panelu (hustší seznam jako v Kiri).
const NAV_H: f32 = 34.0;
const NAV_GAP: f32 = 2.0;

pub struct App {
    /// historie celkového CPU kontejnerů (čas s, % všech CPU WSL) – dlaždice CPU
    pub cpu_hist: std::collections::VecDeque<(f64, f64)>,
    /// historie vytížení celé WSL VM (čas s, % všech CPU – /proc/stat, stejně jako karta enginu)
    pub vm_cpu_hist: std::collections::VecDeque<(f64, f64)>,
    pub cfg: Arc<RwLock<Config>>,
    tx: Sender<Cmd>,
    rx: Receiver<Msg>,
    pub shared: Arc<Shared>,
    pub snap: Snapshot,
    pub distros: Vec<Distro>,
    pub stats: Vec<StatRow>,
    pub df: Option<DfSummary>,
    /// sekce Paměť WSL na přehledu (průběh a výsledek uvolnění)
    pub mem: ui::memory::MemUi,
    /// stránka Události (filtr, rozbalený záznam)
    pub events: ui::events::EventsUi,
    pub tcp_configured: Option<bool>,
    pub page: Page,
    toasts: Vec<Toast>,
    pub confirm: Option<Confirm>,
    pub output: Option<(String, String)>,
    pub logs: Option<LogView>,
    /// otevřený detail kontejneru
    pub detail: Option<ui::detail::DetailView>,
    /// samostatné okno prohlížeče souborů v kontejneru
    pub cfiles: Option<ui::container_files::CfState>,
    pub run_open: bool,
    pub run: RunSpec,
    /// výběr existujícího volume v dialogu Spustit kontejner: (název, cesta v kontejneru, jen pro čtení)
    pub run_vol: (String, String, bool),
    // stav jednotlivých stránek
    pub filter: String,
    pub show_all: bool,
    pub pull_input: String,
    pub new_volume: String,
    pub new_net: String,
    pub new_net_driver: String,
    /// síťový přehled (záložky stránky Sítě, porty kontejnerů v seznamu)
    pub net: ui::net_overview::NetState,
    pub compose_dir: String,
    /// stránka Compose: nový stack, editor souborů, škálování
    pub compose: ui::compose::ComposeState,
    /// stránka Kubernetes (cluster, prostředí, mapa toků, pravidla)
    pub k8s: ui::k8s::K8sUi,
    /// výška spodní části postranního panelu s metrikami na kartě enginu (z posledního snímku)
    nav_bottom_full: f32,
    pub daemon_json: String,
    pub daemon_loaded: bool,
    pub cfg_draft: Config,
    pub wsl_draft: wslconfig::Draft,
    pub wsl_saved: wslconfig::Draft,
    pub files: ui::files::FilesState,
    pub hub: ui::images::HubState,
    /// sestavení image z Dockerfile (dialog, průběh)
    pub build: ui::build::BuildState,
    /// volume: velikosti, dialogy zálohy / obnovy / klonu
    pub vol: ui::volumes::VolState,
    /// otevřený dialog „Přidat distribuci“
    pub add_distro: Option<crate::worker::AddDistroSpec>,
    /// distribuce k instalaci (wsl --list --online); None = načítá se
    pub online: Option<Result<Vec<(String, String)>, String>>,
    /// otevřený dialog odinstalace: (engine, smazat data)
    pub uninstall: Option<(crate::engine::Engine, bool)>,
    /// otevřený dialog instalace: (engine, zabezpečit API pomocí TLS)
    pub install: Option<(crate::engine::Engine, bool)>,
    /// dialogy správy distribucí (odebrat, export, import, disk)
    pub distro_dlg: ui::distros::DistroDialogs,
    /// stav instalace docker shimu (zjišťuje se na pozadí)
    pub shim: Arc<Mutex<Option<crate::shim_install::Status>>>,
    pub shim_checking: Arc<AtomicBool>,
    shim_msg: Arc<Mutex<Option<Result<String, String>>>>,
    /// první snímek (kvůli „spustit schované“)
    first_frame: bool,
    /// spouštění s Windows (načte se při prvním zobrazení Nastavení)
    pub autostart: Option<bool>,
    /// vybraná záložka stránky Nastavení
    pub settings_tab: ui::settings::Tab,
    /// okno „Na pozadí“ (běžící a dokončené akce a příkazy ve WSL)
    pub activity_open: bool,
    theme_checked: std::time::Instant,
    was_minimized: bool,
    /// okno bylo v minulém snímku schované (po zobrazení se hned obnoví data)
    was_hidden: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut cfg = Config::load();
        if !Config::exists() {
            // první spuštění (i nový portable balíček): výchozí distribuce WSL místo „Ubuntu“
            if let Some(d) = crate::wsl::list().ok().and_then(|l| l.into_iter().find(|d| d.default)) {
                cfg.distro = d.name;
            }
            let _ = cfg.save();
        }
        // WSLCD_THEME=dark|light|system přepíše motiv (ladění, screenshoty)
        theme::apply(&cc.egui_ctx, &std::env::var("WSLCD_THEME").unwrap_or_else(|_| cfg.theme.clone()));
        crate::i18n::set_language(&cfg.language);
        // pro ladění/screenshoty: WSLCD_LANG=en|cs přepíše jazyk (bez uložení do konfigurace)
        if let Ok(l) = std::env::var("WSLCD_LANG") {
            crate::i18n::set_language(&l);
        }
        if cfg.tls {
            // bez certifikátů se nepřipojí – chyba se ukáže jako nedostupné API
            if let Err(e) = crate::docker::configure_tls(true) {
                eprintln!("TLS: {e}");
            }
        }
        let cfg_draft = cfg.clone();
        let npipe = cfg.npipe;
        let cfg = Arc::new(RwLock::new(cfg));
        if npipe {
            // pojmenovaná roura pro nástroje – běží, dokud běží aplikace
            let ctx = cc.egui_ctx.clone();
            crate::npipe::apply(true, cfg.clone(), move || ctx.request_repaint());
        }
        let (tx, rx, shared) = worker::spawn(cc.egui_ctx.clone(), cfg.clone());
        let wsl_draft = wslconfig::Draft::load();

        // HWND okna pro skrývání do tray + ikona v oznamovací oblasti
        {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(h) = cc.window_handle() {
                if let RawWindowHandle::Win32(w) = h.as_raw() {
                    crate::tray::set_hwnd(w.hwnd.get());
                }
            }
        }
        let mut startup_toasts = Vec::new();
        if let Err(e) = crate::tray::init(cc.egui_ctx.clone(), tx.clone(), shared.clone(), cfg.clone()) {
            startup_toasts.push(Toast { ok: false, text: tf!("Ikonu v oznamovací oblasti nelze vytvořit: {e}", "Cannot create the tray icon: {e}"), until: 0.0 });
        }
        // portable složka (např. C:\WSLContainerDesktop) nesmí být zapisovatelná pro ostatní
        // uživatele – EXE se spouští po přihlášení a docker.exe je v PATH (N-03)
        match crate::portable_acl::protect_portable_dir() {
            Ok(true) => startup_toasts.push(Toast {
                ok: true,
                text: t!(
                    "Složka aplikace je teď přístupná jen vám (ostatní uživatelé počítače ji nemohou měnit).",
                    "The app folder is now accessible only to you (other users of this PC cannot modify it)."
                )
                .into(),
                until: 0.0,
            }),
            Ok(false) => {}
            Err(e) => startup_toasts.push(Toast {
                ok: false,
                text: tf!(
                    "Oprávnění složky aplikace nejde omezit ({e}) – ostatní uživatelé počítače ji možná mohou měnit. Přesuňte aplikaci do svého profilu.",
                    "Cannot restrict the app folder permissions ({e}) – other users of this PC may be able to modify it. Move the app into your profile."
                ),
                until: 0.0,
            }),
        }
        // pro ladění/screenshoty: WSLCD_START_PAGE=files|containers|…
        let page = match std::env::var("WSLCD_START_PAGE").unwrap_or_default().as_str() {
            "containers" => Page::Containers,
            "images" => Page::Images,
            "volumes" => Page::Volumes,
            "networks" => Page::Networks,
            "compose" => Page::Compose,
            "files" => Page::Files,
            "events" => Page::Events,
            "engine" => Page::Engine,
            "settings" => Page::Settings,
            _ => Page::Dashboard,
        };
        // pro screenshoty: WSLCD_DEBUG_MEMORY=1 otevře přehled a posune na sekci Paměť WSL
        let page = if std::env::var_os("WSLCD_DEBUG_MEMORY").is_some() { Page::Dashboard } else { page };
        // pro ladění/screenshoty: WSLCD_DEBUG_NETWORK=1|ports|map otevře síťový přehled
        let page = if std::env::var_os("WSLCD_DEBUG_NETWORK").is_some() { Page::Networks } else { page };
        Self {
            cpu_hist: Default::default(),
            vm_cpu_hist: Default::default(),
            cfg,
            tx,
            rx,
            shared,
            snap: Snapshot::default(),
            distros: Vec::new(),
            stats: Vec::new(),
            df: None,
            mem: Default::default(),
            events: Default::default(),
            tcp_configured: None,
            page,
            toasts: Vec::new(),
            // pro ladění vzhledu dialogu: WSLCD_DEBUG_DIALOG=1
            confirm: std::env::var("WSLCD_DEBUG_DIALOG").ok().map(|_| Confirm {
                title: t!("Odstranit nginx:alpine?", "Remove nginx:alpine?").into(),
                text: t!("Image bude odstraněn (pokud ho nepoužívá žádný kontejner).", "The image will be removed (if no container uses it).").into(),
                cmd: Cmd::Refresh,
            }),
            output: None,
            logs: None,
            detail: None,
            cfiles: None,
            run_open: false,
            run: RunSpec { restart: "no".into(), ..Default::default() },
            run_vol: (String::new(), "/data".into(), false),
            filter: String::new(),
            show_all: true,
            pull_input: String::new(),
            new_volume: String::new(),
            new_net: String::new(),
            new_net_driver: "bridge".into(),
            net: Default::default(),
            compose_dir: String::new(),
            compose: ui::compose::ComposeState::default(),
            k8s: ui::k8s::K8sUi::default(),
            nav_bottom_full: 0.0,
            daemon_json: String::new(),
            daemon_loaded: false,
            cfg_draft,
            wsl_saved: wsl_draft.clone(),
            wsl_draft,
            files: ui::files::FilesState::default(),
            hub: ui::images::HubState::default(),
            build: ui::build::BuildState::default(),
            vol: ui::volumes::VolState::new(),
            uninstall: None,
            install: None,
            distro_dlg: ui::distros::DistroDialogs::from_env(),
            add_distro: None,
            online: None,
            shim: Arc::new(Mutex::new(None)),
            shim_checking: Arc::new(AtomicBool::new(false)),
            shim_msg: Arc::new(Mutex::new(None)),
            first_frame: true,
            autostart: None,
            settings_tab: Default::default(),
            // pro screenshoty: WSLCD_DEBUG_ACTIVITY=1 otevře okno „Na pozadí“
            activity_open: std::env::var_os("WSLCD_DEBUG_ACTIVITY").is_some(),
            theme_checked: std::time::Instant::now(),
            was_minimized: false,
            was_hidden: false,
        }
        .with_toasts(startup_toasts)
    }

    fn with_toasts(mut self, t: Vec<Toast>) -> Self {
        self.toasts.extend(t);
        // pro ladění: WSLCD_DEBUG_HUB=nginx předvyplní hledání a vybere image
        if let Ok(q) = std::env::var("WSLCD_DEBUG_HUB") {
            self.hub.query = q.clone();
            self.hub.open = true;
            ui::images::select_repo(&mut self, &q, !q.contains('/'));
        }
        self
    }

    /// Zavření / minimalizace do tray a synchronizace stavu traye.
    /// Motiv „podle Windows“: jednou za pár sekund zkontrolovat, jestli se nezměnil.
    fn follow_system_theme(&mut self, ctx: &egui::Context) {
        if self.cfg.read().map(|c| c.theme != "system").unwrap_or(true) || std::env::var("WSLCD_THEME").is_ok() {
            return;
        }
        if self.theme_checked.elapsed() < std::time::Duration::from_secs(3) {
            return;
        }
        self.theme_checked = std::time::Instant::now();
        if theme::windows_uses_light() != theme::is_light() {
            theme::set_mode(ctx, "system");
        }
    }

    fn handle_tray(&mut self, ctx: &egui::Context) {
        crate::tray::refresh();
        // „Zobrazit v aplikaci“ / „… a N dalších“ z podnabídky kontejnerů v trayi
        if let Some(req) = crate::tray::take_show_request() {
            self.page = Page::Containers;
            if let Some((id, name)) = req {
                ui::detail::open(self, ctx, id, name);
            }
        }
        // kliknutí na oznámení o prahu paměti / disku
        if let Some(t) = crate::worker::alerts::take_nav() {
            ui::alerts::navigate(self, t);
        }
        // kliknutí na oznámení o problému kontejneru
        if let Some(seq) = crate::worker::notify::take_nav() {
            ui::events::navigate(self, seq);
        }
        let (close_to_tray, min_to_tray, start_hidden) = self
            .cfg
            .read()
            .map(|c| (c.close_to_tray, c.minimize_to_tray, c.start_hidden))
            .unwrap_or((false, false, false));
        // tray mohl změnit konfiguraci – promítnout do konceptu Nastavení
        self.cfg_draft.close_to_tray = close_to_tray;
        self.cfg_draft.minimize_to_tray = min_to_tray;
        if !crate::tray::active() {
            return;
        }
        if self.first_frame {
            self.first_frame = false;
            if start_hidden || crate::autostart::launched_at_login() || crate::wake::launched_from_shim() {
                crate::tray::hide_window();
            }
        }
        let (close, minimized) = ctx.input(|i| (i.viewport().close_requested(), i.viewport().minimized.unwrap_or(false)));
        if close && close_to_tray {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            crate::tray::hide_window();
        }
        if minimized && !self.was_minimized && min_to_tray {
            crate::tray::hide_window();
        }
        self.was_minimized = minimized;
    }

    /// Spustí na pozadí install/uninstall/kontrolu docker shimu.
    pub fn shim_action(&self, ctx: &egui::Context, action: crate::shim_install::Action) {
        if self.shim_checking.swap(true, Ordering::SeqCst) {
            return;
        }
        let (st, busy, ctx) = (self.shim.clone(), self.shim_checking.clone(), ctx.clone());
        let toasts = self.shim_msg.clone();
        std::thread::spawn(move || {
            let res = crate::shim_install::run(action);
            let s = crate::shim_install::status();
            if let Ok(mut g) = st.lock() {
                *g = Some(s);
            }
            if let (Some(r), Ok(mut t)) = (res, toasts.lock()) {
                *t = Some(r);
            }
            busy.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    pub fn ask(&mut self, title: impl Into<String>, text: impl Into<String>, cmd: Cmd) {
        self.confirm = Some(Confirm { title: title.into(), text: text.into(), cmd });
    }

    pub fn toast(&mut self, ok: bool, text: impl Into<String>) {
        self.toasts.push(Toast { ok, text: text.into(), until: 0.0 });
    }

    pub fn distro(&self) -> String {
        self.cfg.read().map(|c| c.distro.clone()).unwrap_or_default()
    }

    pub fn current_distro(&self) -> Option<&Distro> {
        let d = self.distro();
        self.distros.iter().find(|x| x.name == d)
    }

    pub fn save_config(&mut self, c: Config) {
        let changed_distro = self.distro() != c.distro;
        let old_keep = self.cfg.read().map(|g| g.keep_alive).unwrap_or(c.keep_alive);
        if old_keep != c.keep_alive {
            self.send(Cmd::SetKeepAlive(c.keep_alive));
        }
        match c.save() {
            Ok(()) => self.toast(true, t!("Nastavení uloženo", "Settings saved")),
            Err(e) => self.toast(false, tf!("Nastavení nelze uložit: {e}", "Cannot save settings: {e}")),
        }
        if let Ok(mut g) = self.cfg.write() {
            *g = c;
        }
        if changed_distro {
            self.tcp_configured = None;
            self.shared.tcp_checked.store(false, Ordering::SeqCst);
            self.files.path = "~".into();
            self.files.loaded = false;
            self.daemon_loaded = false;
        }
        self.send(Cmd::Refresh);
    }

    pub fn open_logs(&mut self, id: String, name: String) {
        self.logs = Some(self.spawn_logs(id, name, 1000));
    }

    /// Endpoint API enginu podle aktuální konfigurace (bez zámku výchozí port 2375 jako dřív).
    pub fn engine_endpoint(&self) -> crate::http::Endpoint {
        self.cfg.read().map(|c| crate::host::endpoint(&c)).unwrap_or(crate::http::Endpoint::Tcp { port: 2375 })
    }

    /// Spustí streamování posledních `tail` řádků logu (a dalších) do nového `LogView`.
    pub fn spawn_logs(&self, id: String, name: String, tail: u32) -> LogView {
        LogView::single(self.engine_endpoint(), id, name, tail)
    }

    /// Okno se sloučenými živými logy všech kontejnerů compose projektu.
    pub fn open_stack_logs(&mut self, project: &str) {
        let ep = self.engine_endpoint();
        let targets = crate::logbuf::stack_targets(&self.snap.containers, project);
        self.logs = Some(LogView::stack(ep, project.to_string(), targets));
    }

    fn drain(&mut self) {
        let shim_res = self.shim_msg.lock().ok().and_then(|mut g| g.take());
        if let Some(r) = shim_res {
            match r {
                Ok(t) => self.toast(true, t),
                Err(e) => self.toast(false, e),
            }
        }
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Snapshot(s) => self.snap = *s,
                Msg::Distros(d) => self.distros = d,
                Msg::Stats(rows, df) => {
                    let ncpu = self.snap.info.as_ref().map(|i| i.ncpu.max(1)).unwrap_or(1) as f64;
                    // fold od 0.0: `sum()` prázdného seznamu dává -0.0 („-0.0 %“ bez kontejnerů)
                    let total = rows.iter().fold(0.0, |a, r| a + r.cpu);
                    ui::dashboard::push_cpu(&mut self.cpu_hist, total / ncpu);
                    // celá VM ze stejného vzorku jako karta enginu (ve stejném okamžiku jako kontejnery)
                    if let Some(vm) = self.shared.memory.lock().ok().and_then(|m| m.glance).and_then(|g| g.cpu) {
                        ui::dashboard::push_cpu(&mut self.vm_cpu_hist, vm);
                    }
                    self.stats = rows;
                    if df.is_some() {
                        self.df = df;
                    }
                }
                Msg::Toast(ok, t) => self.toast(ok, t),
                Msg::Output(title, text) => self.output = Some((title, text)),
                Msg::DaemonJson(t) => {
                    self.daemon_json = t;
                    self.daemon_loaded = true;
                }
                Msg::EngineInfo { tcp_configured } => self.tcp_configured = Some(tcp_configured),
                Msg::FsListing(cwd, entries) => {
                    let f = &mut self.files;
                    f.loading = false;
                    f.loaded = true;
                    f.error = None;
                    if f.path != cwd {
                        f.selected.clear();
                    }
                    f.path = cwd.clone();
                    f.input = cwd;
                    f.entries = entries;
                }
                Msg::FsListError(p, e) => {
                    let f = &mut self.files;
                    f.loading = false;
                    f.loaded = true;
                    f.input = f.path.clone();
                    f.error = Some(format!("{p}: {e}"));
                }
                Msg::FsFile(path, text) => {
                    self.files.editor = Some(ui::files::Editor { path, orig: text.clone(), text });
                }
                Msg::FsChanged => ui::files::reload(self),
                Msg::OnlineDistros(r) => self.online = Some(r),
                Msg::BuildOutput(line) => ui::build::on_output(self, line),
                Msg::BuildDone(r) => ui::build::on_done(self, r),
                Msg::Inspect(id, r) => ui::detail::on_inspect(self, id, r),
                Msg::VolumeSizes(s) => self.vol.sizes = s,
                Msg::Stack(m) => ui::compose::on_msg(self, m),
                Msg::K8s(m) => ui::k8s::on_msg(self, m),
                Msg::DistroDisk(name, r) => ui::distros::on_disk(self, name, r),
                Msg::ContainerFs(view, m) => ui::container_files::on_msg(self, view, m),
                Msg::MemoryFreed { auto, result } => ui::memory::on_freed(self, auto, result),
                Msg::ConfigChanged => {
                    if let Ok(c) = self.cfg.read() {
                        self.cfg_draft = c.clone();
                    }
                    self.tcp_configured = None;
                    self.daemon_loaded = false;
                    self.files.path = "~".into();
                    self.files.loaded = false;
                }
                Msg::HubResults(term, r) => {
                    let h = &mut self.hub;
                    // starší dotaz mohl doběhnout později – zajímá nás jen poslední
                    if term == h.searched {
                        h.searching = false;
                        match r {
                            Ok(v) => {
                                h.results = v;
                                h.error = None;
                            }
                            Err(e) => {
                                h.results.clear();
                                h.error = Some(e);
                            }
                        }
                    }
                }
                Msg::HubTags(repo, r) => {
                    let h = &mut self.hub;
                    if h.selected.as_deref() == Some(repo.as_str()) {
                        h.tags_loading = false;
                        match r {
                            Ok(v) => {
                                h.tag = v.iter().find(|t| t.name == "latest").or(v.first()).map(|t| t.name.clone()).unwrap_or_else(|| "latest".into());
                                h.tags = v;
                                h.tags_error = None;
                            }
                            Err(e) => {
                                h.tags.clear();
                                h.tag = "latest".into();
                                h.tags_error = Some(e);
                            }
                        }
                    }
                }
            }
        }
    }

    fn engine_status(&self) -> (egui::Color32, String) {
        let d = self.distro();
        let e = self.cfg.read().map(|c| c.engine.name()).unwrap_or("Engine");
        if self.snap.connected {
            let (r, t) = self
                .snap
                .info
                .as_ref()
                .map(|i| (i.containers_running, i.containers))
                .unwrap_or((0, 0));
            (theme::green(), tf!("{e} běží ({d}) · {r}/{t} kontejnerů", "{e} running ({d}) · {r}/{t} containers"))
        } else if self.current_distro().map(|x| x.running()).unwrap_or(false) {
            (theme::yellow(), tf!("{d} běží, {e} API nedostupné", "{d} running, {e} API unavailable"))
        } else {
            (theme::grey(), tf!("{d} zastavena", "{d} stopped"))
        }
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("nav")
            .exact_width(220.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(theme::sidebar()).inner_margin(egui::Margin::same(10)))
            .show(ctx, |ui| {
                // metriky na kartě enginu jen tehdy, když se nad kartu vejde celá hlavní navigace
                // (výška spodní části s metrikami je z posledního snímku, napoprvé odhad)
                let nav_h = Page::MAIN.len() as f32 * (NAV_H + NAV_GAP) + 8.0;
                let bottom_full = if self.nav_bottom_full > 0.0 { self.nav_bottom_full } else { 360.0 };
                let roomy = ui.available_height() >= nav_h + bottom_full;
                let mut drew_metrics = false;
                // spodní část panelu: stav enginu + WSL engine / Nastavení – drží se spodního okraje
                let bottom = egui::TopBottomPanel::bottom("nav-bottom")
                    .frame(egui::Frame::new())
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        let (c, _) = self.engine_status();
                        let ename = self.cfg.read().map(|c| c.engine.name()).unwrap_or("Engine");
                        let port = self.cfg.read().map(|c| c.port).unwrap_or(0);
                        let distro = self.distro();
                        let connected = self.snap.connected;
                        let distro_running = self.current_distro().map(|d| d.running()).unwrap_or(false);
                        let (state, detail) = if connected {
                            let (r, t) = self.snap.info.as_ref().map(|i| (i.containers_running, i.containers)).unwrap_or((0, 0));
                            (t!("běží", "running"), tf!("{distro} · {r}/{t} kontejnerů", "{distro} · {r}/{t} containers"))
                        } else if distro_running {
                            (t!("API nedostupné", "API unavailable"), tf!("{distro} běží", "{distro} running"))
                        } else {
                            (t!("zastaven", "stopped"), tf!("{distro} zastavena", "{distro} stopped"))
                        };
                        egui::Frame::new()
                            .fill(theme::card())
                            .stroke(egui::Stroke::new(1.0_f32, theme::card_stroke()))
                            .corner_radius(8)
                            .inner_margin(egui::Margin::same(8))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.spacing_mut().item_spacing.y = 4.0;
                                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                                // řádek 1: tečka + distribuce, vpravo štítek stavu; engine je v řádku
                                // kontejnerů a v bublině (přepnutí distribuce je na stránce WSL engine)
                                ui.horizontal(|ui| {
                                    ui::dot(ui, c);
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui::badge(ui, state, c).on_hover_text(ename);
                                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                            ui.label(RichText::new(&distro).strong()).on_hover_text(tf!(
                                                "{ename} · {detail}\nAPI: tcp://127.0.0.1:{port}",
                                                "{ename} · {detail}\nAPI: tcp://127.0.0.1:{port}"
                                            ));
                                        });
                                    });
                                });
                                // chyba API při běžícím enginu (dřív ve stavové liště)
                                if let Some(e) = self.snap.error.as_ref().filter(|_| connected) {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(format!("\u{E4E0} {e}")).small().color(theme::red())).on_hover_text(e);
                                    });
                                }
                                // CPU a RAM celé WSL VM (poslední vzorek paměti); klik = Přehled
                                // v nízkém okně jen stav a tlačítka (navigace nad kartou musí zůstat vidět)
                                if distro_running && roomy {
                                    let g = self.shared.memory.lock().ok().and_then(|m| m.glance);
                                    if let Some(g) = g {
                                        let info = self.snap.info.as_ref().filter(|_| connected);
                                        let x = ui::GlanceCtx {
                                            engine: ename,
                                            containers: info.map(|i| (i.containers_running, i.containers)),
                                            ncpu: info.map(|i| i.ncpu),
                                            k8s: self.snap.containers.iter().find(|c| c.name() == crate::k8s::cluster::NAME).map(|c| c.state == "running"),
                                        };
                                        ui.add_space(2.0);
                                        drew_metrics = true;
                                        if ui::glance(ui, &g, &x).clicked() {
                                            self.page = Page::Dashboard;
                                        }
                                    }
                                }
                                ui.add_space(4.0);
                                // stejně široká tlačítka
                                let w = ui.available_width();
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    if connected {
                                        let half = egui::vec2((w - 6.0) / 2.0, 26.0);
                                        if ui.add(egui::Button::new("\u{E46C} Stop").min_size(half)).clicked() {
                                            self.send(Cmd::EngineStop);
                                        }
                                        if ui
                                            .add(egui::Button::new(t!("\u{E330} Uspat", "\u{E330} Sleep")).min_size(half))
                                            .on_hover_text(t!("Ukončí distribuci a uvolní paměť", "Terminates the distribution and frees memory"))
                                            .clicked()
                                        {
                                            self.send(Cmd::Sleep);
                                        }
                                    } else if ui
                                        .add(
                                            egui::Button::new(RichText::new(tf!("\u{E3D0} Spustit {ename}", "\u{E3D0} Start {ename}")).color(theme::accent_text()))
                                                .fill(theme::accent())
                                                .min_size(egui::vec2(w, 26.0)),
                                        )
                                        .clicked()
                                    {
                                        self.send(Cmd::EngineStart);
                                    }
                                });
                            });
                        ui.add_space(6.0);
                        ui.spacing_mut().item_spacing.y = NAV_GAP;
                        for p in Page::BOTTOM {
                            self.nav_button(ui, p);
                        }
                        // verze aplikace (dřív ve stavové liště) a přehled činností na pozadí
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add_space(6.0);
                            // testovací build z jiné větve než main: verze + commit (build.rs)
                            let build = env!("WSLCD_BUILD");
                            let (label, hover) = match build.split_once('@') {
                                Some((branch, commit)) => (
                                    format!("v{} · {commit}", env!("CARGO_PKG_VERSION")),
                                    format!("WSL Container Desktop {} – {branch} @ {commit}", env!("CARGO_PKG_VERSION")),
                                ),
                                None => (format!("v{}", env!("CARGO_PKG_VERSION")), format!("WSL Container Desktop {}", env!("CARGO_PKG_VERSION"))),
                            };
                            ui.label(RichText::new(label).small().color(theme::muted())).on_hover_text(hover);
                            ui::activity::indicator(self, ui);
                        });
                    });
                if drew_metrics {
                    self.nav_bottom_full = bottom.response.rect.height();
                }
                // název a ikona aplikace jsou v záhlaví okna; panel začíná rovnou navigací
                ui.add_space(4.0);
                // v nízkém okně se hlavní navigace posouvá místo překrytí kartou enginu
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = NAV_GAP;
                    for p in Page::MAIN {
                        self.nav_button(ui, p);
                    }
                });
            });
    }

    fn nav_button(&mut self, ui: &mut egui::Ui, p: Page) {
        let sel = self.page == p;
        let text = RichText::new(p.label()).size(15.0);
        let text = if sel { text.color(theme::strong_text()) } else { text };
        let r = ui.add(
            egui::Button::new(text)
                .selected(sel)
                .frame(sel)
                .min_size(egui::vec2(ui.available_width(), NAV_H)),
        );
        // odznak s počtem nových problémů kontejnerů
        let unread = if p == Page::Events && !sel { ui::events::unread(self) } else { 0 };
        if unread > 0 {
            let txt = if unread > 99 { "99+".to_string() } else { unread.to_string() };
            let galley = ui.painter().layout_no_wrap(txt, egui::FontId::proportional(11.0), egui::Color32::WHITE);
            let w = (galley.size().x + 10.0).max(18.0);
            let rect = egui::Rect::from_center_size(egui::pos2(r.rect.right() - 8.0 - w / 2.0, r.rect.center().y), egui::vec2(w, 18.0));
            ui.painter().rect_filled(rect, 9.0, theme::red());
            ui.painter().galley(rect.center() - galley.size() / 2.0, galley, egui::Color32::WHITE);
        }
        if sel {
            let rect = r.rect;
            ui.painter().rect_filled(
                egui::Rect::from_min_size(rect.left_top() + egui::vec2(0.0, 9.0), egui::vec2(3.0, 18.0)),
                2.0,
                theme::accent(),
            );
        }
        if r.clicked() {
            self.page = p;
            let running = self.current_distro().map(|d| d.running()).unwrap_or(false);
            if p == Page::Engine && !self.daemon_loaded && running {
                self.send(Cmd::LoadDaemonJson);
            }
            if p == Page::Files && !self.files.loaded {
                ui::files::reload(self);
            }
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        // potvrzení
        let mut close = false;
        let mut run_cmd = None;
        let distro = self.distro();
        if let Some(c) = &mut self.confirm {
            let (_, esc) = ui::modal(ctx, "confirm", &c.title, 440.0, |ui| {
                ui.label(RichText::new(&c.text).color(theme::body_text()));
                // přepnutí primární distribuce: volba uspat tu dosavadní
                if let Cmd::SetPrimary { sleep_old, .. } = &mut c.cmd {
                    ui.add_space(6.0);
                    ui.checkbox(sleep_old, tf!("Po přepnutí uspat {distro} (wsl --terminate)", "Put {distro} to sleep after switching (wsl --terminate)"))
                        .on_hover_text(t!(
                            "Uvolní paměť a CPU. Kontejnery v ní se zastaví; ty s restart policy se spustí při dalším startu distribuce.",
                            "Frees memory and CPU. Its containers stop; those with a restart policy start again when the distribution next starts."
                        ));
                }
                let (ok, cancel) = ui::dialog_buttons(ui, t!("Potvrdit", "Confirm"), true, true);
                if ok {
                    run_cmd = Some(c.cmd.clone());
                }
                close = ok || cancel;
            });
            close |= esc;
        }
        if let Some(cmd) = run_cmd {
            self.send(cmd);
        }
        if close {
            self.confirm = None;
        }

        // výstup příkazu
        let mut open = self.output.is_some();
        if let Some((title, text)) = &self.output {
            egui::Window::new(tf!("Výstup: {title}", "Output: {title}"))
                .open(&mut open)
                .default_size([720.0, 400.0])
                .show(ctx, |ui| {
                    egui::ScrollArea::both().stick_to_bottom(true).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(text).monospace()).wrap_mode(egui::TextWrapMode::Extend));
                    });
                });
        }
        if !open {
            self.output = None;
        }

        ui::logs::window(self, ctx);
        ui::files::dialogs(self, ctx);
        ui::images::hub_dialog(self, ctx);
        ui::build::dialogs(self, ctx);
        ui::engine::uninstall_dialog(self, ctx);
        ui::engine::install_dialog(self, ctx);
        ui::settings::add_distro_dialog(self, ctx);
        ui::distros::dialogs(self, ctx);
        ui::containers::run_dialog(self, ctx);
        ui::detail::window(self, ctx);
        ui::container_files::window(self, ctx);
        ui::volumes::dialogs(self, ctx);
        ui::compose::dialogs(self, ctx);
    }

    fn toasts(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        for t in &mut self.toasts {
            if t.until == 0.0 {
                t.until = now + if t.ok { 4.0 } else { 9.0 };
            }
        }
        self.toasts.retain(|t| t.until > now);
        if self.toasts.len() > 5 {
            let n = self.toasts.len() - 5;
            self.toasts.drain(..n);
        }
        if self.toasts.is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-16.0, -40.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let mut remove = None;
                for (i, t) in self.toasts.iter().enumerate() {
                    egui::Frame::new()
                        .fill(theme::card())
                        .stroke(egui::Stroke::new(1.0_f32, if t.ok { theme::green() } else { theme::red() }))
                        .corner_radius(6)
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_max_width(420.0);
                            ui.horizontal_top(|ui| {
                                ui.label(RichText::new(if t.ok { "\u{E182}" } else { "\u{E4E0}" }).color(if t.ok { theme::green() } else { theme::red() }));
                                // dlouhý text se zalomí (místo přetečení přes okno)
                                // text v pevně širokém sloupci se zalomí, křížek vpravo
                                ui.vertical(|ui| {
                                    ui.set_width(360.0);
                                    ui.add(egui::Label::new(&t.text).wrap_mode(egui::TextWrapMode::Wrap));
                                });
                                if ui.small_button("\u{E4F6}").clicked() {
                                    remove = Some(i);
                                }
                            });
                        });
                    ui.add_space(4.0);
                }
                if let Some(i) = remove {
                    self.toasts.remove(i);
                }
            });
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
    }
}

impl eframe::App for App {
    /// Schované okno: surové pohyby myši zahodit, ať nebudí další snímky (egui by jinak
    /// po každém pohybu chtělo okamžitě překreslit).
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if !crate::tray::window_visible() {
            raw_input.events.retain(|e| !matches!(e, egui::Event::MouseMoved(_) | egui::Event::PointerMoved(_)));
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        self.follow_system_theme(ctx);
        self.handle_tray(ctx);
        self.shared.want_stats.store(self.page == Page::Dashboard, Ordering::Relaxed);
        // Schované okno (oznamovací oblast, minimalizace): data se sbírají dál, jen se nic
        // nekreslí. eframe totiž budí snímek při každém pohybu myši (okno si drží fokus) –
        // takový snímek pak skoro nic nestojí. Zobrazení okna vyžádá překreslení samo.
        if !crate::tray::window_visible() {
            self.was_hidden = true;
            return;
        }
        // okno se právě zobrazilo – na pozadí se sbíralo řidčeji, obnovit hned
        if std::mem::take(&mut self.was_hidden) {
            self.send(Cmd::Refresh);
        }
        self.sidebar(ctx);
        egui::CentralPanel::default()
            // vpravo bez okraje: posuvník je u okraje okna, odsazení 28 px má až obsah
            .frame(egui::Frame::new().fill(theme::bg()).inner_margin(egui::Margin { left: 28, right: 0, top: 20, bottom: 12 }))
            .show(ctx, |ui| {
                let viewport_w = ui.available_width();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    // obsah vždy přesně na šířku okna (bez pravého odsazení) – jinak si scroll
                    // oblast „pamatuje“ širší obsah; plovoucí posuvník leží v odsazení
                    ui.set_max_width(viewport_w - 28.0);
                    match self.page {
                    Page::Dashboard => ui::dashboard::show(self, ui),
                    Page::Containers => ui::containers::show(self, ui),
                    Page::Images => ui::images::show(self, ui),
                    Page::Volumes => ui::volumes::show(self, ui),
                    Page::Networks => ui::networks::show(self, ui),
                    Page::Compose => ui::compose::show(self, ui),
                    Page::Kubernetes => ui::k8s::show(self, ui),
                    Page::Files => ui::files::show(self, ui),
                    Page::Events => ui::events::show(self, ui),
                    Page::Engine => ui::engine::show(self, ui),
                    Page::Settings => ui::settings::show(self, ui),
                    }
                });
            });
        self.dialogs(ctx);
        ui::activity::window(self, ctx);
        self.toasts(ctx);
        if self.logs.is_some() || self.detail.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
    }
}
