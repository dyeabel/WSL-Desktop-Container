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
    Files,
    Engine,
    Settings,
}

impl Page {
    pub const MAIN: [Page; 7] = [Page::Dashboard, Page::Containers, Page::Images, Page::Volumes, Page::Networks, Page::Compose, Page::Files];
    pub const BOTTOM: [Page; 2] = [Page::Engine, Page::Settings];
    pub fn label(self) -> &'static str {
        match self {
            Page::Dashboard => t!("\u{E2C2}  Přehled", "\u{E2C2}  Overview"),
            Page::Containers => t!("\u{E1DA}  Kontejnery", "\u{E1DA}  Containers"),
            Page::Images => t!("\u{E2CA}  Image", "\u{E2CA}  Images"),
            Page::Volumes => t!("\u{E2A0}  Volume", "\u{E2A0}  Volumes"),
            Page::Networks => t!("\u{E408}  Sítě", "\u{E408}  Networks"),
            Page::Compose => "\u{E466}  Compose",
            Page::Files => t!("\u{E260}  Soubory", "\u{E260}  Files"),
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

pub struct LogView {
    pub id: String,
    pub name: String,
    pub buf: Arc<Mutex<String>>,
    pub stop: Arc<AtomicBool>,
    pub filter: String,
    pub wrap: bool,
}

impl Drop for LogView {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

pub struct App {
    pub cfg: Arc<RwLock<Config>>,
    tx: Sender<Cmd>,
    rx: Receiver<Msg>,
    pub shared: Arc<Shared>,
    pub snap: Snapshot,
    pub distros: Vec<Distro>,
    pub stats: Vec<StatRow>,
    pub df: Option<DfSummary>,
    pub tcp_configured: Option<bool>,
    pub page: Page,
    toasts: Vec<Toast>,
    pub confirm: Option<Confirm>,
    pub output: Option<(String, String)>,
    pub logs: Option<LogView>,
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
    pub compose_dir: String,
    pub daemon_json: String,
    pub daemon_loaded: bool,
    pub cfg_draft: Config,
    pub wsl_draft: wslconfig::Draft,
    pub wsl_saved: wslconfig::Draft,
    pub files: ui::files::FilesState,
    pub hub: ui::images::HubState,
    /// otevřený dialog „Přidat distribuci“
    pub add_distro: Option<crate::worker::AddDistroSpec>,
    /// distribuce k instalaci (wsl --list --online); None = načítá se
    pub online: Option<Result<Vec<(String, String)>, String>>,
    /// otevřený dialog odinstalace: (engine, smazat data)
    pub uninstall: Option<(crate::engine::Engine, bool)>,
    /// stav instalace docker shimu (zjišťuje se na pozadí)
    pub shim: Arc<Mutex<Option<crate::shim_install::Status>>>,
    pub shim_checking: Arc<AtomicBool>,
    shim_msg: Arc<Mutex<Option<Result<String, String>>>>,
    logo: egui::TextureHandle,
    /// první snímek (kvůli „spustit schované“)
    first_frame: bool,
    /// spouštění s Windows (načte se při prvním zobrazení Nastavení)
    pub autostart: Option<bool>,
    was_minimized: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let cfg = Config::load();
        crate::i18n::set_language(&cfg.language);
        // pro ladění/screenshoty: WLS_LANG=en|cs přepíše jazyk (bez uložení do konfigurace)
        if let Ok(l) = std::env::var("WLS_LANG") {
            crate::i18n::set_language(&l);
        }
        let cfg_draft = cfg.clone();
        let cfg = Arc::new(RwLock::new(cfg));
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
        let logo = cc.egui_ctx.load_texture(
            "logo",
            egui::ColorImage::from_rgba_unmultiplied([64, 64], &crate::icon_art::render(64, None)),
            egui::TextureOptions::LINEAR,
        );
        // pro ladění/screenshoty: WLS_START_PAGE=files|containers|…
        let page = match std::env::var("WLS_START_PAGE").unwrap_or_default().as_str() {
            "containers" => Page::Containers,
            "images" => Page::Images,
            "volumes" => Page::Volumes,
            "networks" => Page::Networks,
            "compose" => Page::Compose,
            "files" => Page::Files,
            "engine" => Page::Engine,
            "settings" => Page::Settings,
            _ => Page::Dashboard,
        };
        Self {
            cfg,
            tx,
            rx,
            shared,
            snap: Snapshot::default(),
            distros: Vec::new(),
            stats: Vec::new(),
            df: None,
            tcp_configured: None,
            page,
            toasts: Vec::new(),
            // pro ladění vzhledu dialogu: WLS_DEBUG_DIALOG=1
            confirm: std::env::var("WLS_DEBUG_DIALOG").ok().map(|_| Confirm {
                title: t!("Odstranit nginx:alpine?", "Remove nginx:alpine?").into(),
                text: t!("Image bude odstraněn (pokud ho nepoužívá žádný kontejner).", "The image will be removed (if no container uses it).").into(),
                cmd: Cmd::Refresh,
            }),
            output: None,
            logs: None,
            run_open: false,
            run: RunSpec { restart: "no".into(), ..Default::default() },
            run_vol: (String::new(), "/data".into(), false),
            filter: String::new(),
            show_all: true,
            pull_input: String::new(),
            new_volume: String::new(),
            new_net: String::new(),
            new_net_driver: "bridge".into(),
            compose_dir: String::new(),
            daemon_json: String::new(),
            daemon_loaded: false,
            cfg_draft,
            wsl_saved: wsl_draft.clone(),
            wsl_draft,
            files: ui::files::FilesState::default(),
            hub: ui::images::HubState::default(),
            uninstall: None,
            add_distro: None,
            online: None,
            shim: Arc::new(Mutex::new(None)),
            shim_checking: Arc::new(AtomicBool::new(false)),
            shim_msg: Arc::new(Mutex::new(None)),
            logo,
            first_frame: true,
            autostart: None,
            was_minimized: false,
        }
        .with_toasts(startup_toasts)
    }

    fn with_toasts(mut self, t: Vec<Toast>) -> Self {
        self.toasts.extend(t);
        // pro ladění: WLS_DEBUG_HUB=nginx předvyplní hledání a vybere image
        if let Ok(q) = std::env::var("WLS_DEBUG_HUB") {
            self.hub.query = q.clone();
            self.hub.open = true;
            ui::images::select_repo(&mut self, &q, !q.contains('/'));
        }
        self
    }

    /// Zavření / minimalizace do tray a synchronizace stavu traye.
    fn handle_tray(&mut self, ctx: &egui::Context) {
        crate::tray::refresh();
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
            if start_hidden || crate::autostart::launched_at_login() {
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
        let buf = Arc::new(Mutex::new(String::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let port = self.cfg.read().map(|c| c.port).unwrap_or(2375);
        let (b, s) = (buf.clone(), stop.clone());
        let view_id = id.clone();
        std::thread::spawn(move || {
            let d = crate::docker::Docker::new(port);
            let res = d.stream_logs(&id, 1000, |chunk| {
                if s.load(Ordering::Relaxed) {
                    return false;
                }
                if let Ok(mut g) = b.lock() {
                    g.push_str(chunk);
                    if g.len() > 4_000_000 {
                        let cut = g.len() - 3_000_000;
                        let cut = (cut..g.len()).find(|i| g.is_char_boundary(*i)).unwrap_or(0);
                        g.drain(..cut);
                    }
                }
                true
            });
            if let Err(e) = res {
                if let Ok(mut g) = b.lock() {
                    g.push_str(&tf!("\n[chyba streamu logů: {e}]\n", "\n[log stream error: {e}]\n"));
                }
            }
        });
        self.logs = Some(LogView { id: view_id, name, buf, stop, filter: String::new(), wrap: true });
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
            (theme::GREEN, tf!("{e} běží ({d}) · {r}/{t} kontejnerů", "{e} running ({d}) · {r}/{t} containers"))
        } else if self.current_distro().map(|x| x.running()).unwrap_or(false) {
            (theme::YELLOW, tf!("{d} běží, {e} API nedostupné", "{d} running, {e} API unavailable"))
        } else {
            (theme::GREY, tf!("{d} zastavena", "{d} stopped"))
        }
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("nav")
            .exact_width(220.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(theme::SIDEBAR).inner_margin(egui::Margin::same(10)))
            .show(ctx, |ui| {
                // spodní část panelu: stav enginu + WSL engine / Nastavení – drží se spodního okraje
                egui::TopBottomPanel::bottom("nav-bottom")
                    .frame(egui::Frame::new())
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        let (c, _) = self.engine_status();
                        let ename = self.cfg.read().map(|c| c.engine.name()).unwrap_or("Engine");
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
                            .fill(theme::CARD)
                            .stroke(egui::Stroke::new(1.0_f32, theme::CARD_STROKE))
                            .corner_radius(8)
                            .inner_margin(egui::Margin::same(12))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                                // řádek 1: tečka + engine, vpravo štítek stavu
                                ui.horizontal(|ui| {
                                    ui::dot(ui, c);
                                    ui.label(RichText::new(ename).strong());
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui::badge(ui, state, c);
                                    });
                                });
                                // řádek 2: distribuce a kontejnery
                                ui.horizontal(|ui| {
                                    ui.add_space(20.0);
                                    ui.label(RichText::new(&detail).small().color(theme::MUTED)).on_hover_text(&detail);
                                });
                                ui.add_space(6.0);
                                // řádek 3: stejně široká tlačítka
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
                                            egui::Button::new(RichText::new(tf!("\u{E3D0} Spustit {ename}", "\u{E3D0} Start {ename}")).color(theme::ACCENT_TEXT))
                                                .fill(theme::ACCENT)
                                                .min_size(egui::vec2(w, 26.0)),
                                        )
                                        .clicked()
                                    {
                                        self.send(Cmd::EngineStart);
                                    }
                                });
                            });
                        ui.add_space(8.0);
                        for p in Page::BOTTOM {
                            self.nav_button(ui, p);
                        }
                    });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Image::new(&self.logo).fit_to_exact_size(egui::vec2(26.0, 26.0)));
                    ui.label(RichText::new("WLS Desktop").size(18.0).strong());
                });
                ui.add_space(14.0);
                for p in Page::MAIN {
                    self.nav_button(ui, p);
                }
            });
    }

    fn nav_button(&mut self, ui: &mut egui::Ui, p: Page) {
        let sel = self.page == p;
        let text = RichText::new(p.label()).size(15.0);
        let text = if sel { text.color(egui::Color32::WHITE) } else { text };
        let r = ui.add(
            egui::Button::new(text)
                .selected(sel)
                .frame(sel)
                .min_size(egui::vec2(ui.available_width(), 36.0)),
        );
        if sel {
            let rect = r.rect;
            ui.painter().rect_filled(
                egui::Rect::from_min_size(rect.left_top() + egui::vec2(0.0, 9.0), egui::vec2(3.0, 18.0)),
                2.0,
                theme::ACCENT,
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

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::new().fill(theme::SIDEBAR).inner_margin(egui::Margin::symmetric(12, 5)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let (c, t) = self.engine_status();
                    ui::dot(ui, c);
                    ui.label(RichText::new(t).small());
                    if let Some(e) = &self.snap.error {
                        if self.snap.connected {
                            ui.label(RichText::new(format!("· {e}")).small().color(theme::RED));
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let port = self.cfg.read().map(|c| c.port).unwrap_or(0);
                        ui.label(RichText::new(format!("tcp://127.0.0.1:{port}")).small().color(theme::MUTED));
                        if self.shared.busy.load(Ordering::Relaxed) > 0 {
                            ui.spinner();
                            ui.label(RichText::new(t!("Pracuji…", "Working…")).small());
                        }
                    });
                });
            });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        // potvrzení
        let mut close = false;
        let mut run_cmd = None;
        if let Some(c) = &self.confirm {
            let (_, esc) = ui::modal(ctx, "confirm", &c.title, 440.0, |ui| {
                ui.label(RichText::new(&c.text).color(egui::Color32::from_gray(210)));
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
        ui::engine::uninstall_dialog(self, ctx);
        ui::settings::add_distro_dialog(self, ctx);
        ui::containers::run_dialog(self, ctx);
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
                        .fill(theme::CARD)
                        .stroke(egui::Stroke::new(1.0_f32, if t.ok { theme::GREEN } else { theme::RED }))
                        .corner_radius(6)
                        .inner_margin(egui::Margin::same(10))
                        .show(ui, |ui| {
                            ui.set_max_width(420.0);
                            ui.horizontal_top(|ui| {
                                ui.label(RichText::new(if t.ok { "\u{E182}" } else { "\u{E4E0}" }).color(if t.ok { theme::GREEN } else { theme::RED }));
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();
        self.handle_tray(ctx);
        self.shared.want_stats.store(self.page == Page::Dashboard, Ordering::Relaxed);
        self.sidebar(ctx);
        self.status_bar(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin { left: 28, right: 28, top: 20, bottom: 12 }))
            .show(ctx, |ui| {
                let viewport_w = ui.available_width();
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    // obsah vždy přesně na šířku okna – jinak si scroll oblast „pamatuje“ širší obsah
                    ui.set_max_width(viewport_w - 14.0);
                    match self.page {
                    Page::Dashboard => ui::dashboard::show(self, ui),
                    Page::Containers => ui::containers::show(self, ui),
                    Page::Images => ui::images::show(self, ui),
                    Page::Volumes => ui::volumes::show(self, ui),
                    Page::Networks => ui::networks::show(self, ui),
                    Page::Compose => ui::compose::show(self, ui),
                    Page::Files => ui::files::show(self, ui),
                    Page::Engine => ui::engine::show(self, ui),
                    Page::Settings => ui::settings::show(self, ui),
                    }
                });
            });
        self.dialogs(ctx);
        self.toasts(ctx);
        if self.logs.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(300));
        }
    }
}
