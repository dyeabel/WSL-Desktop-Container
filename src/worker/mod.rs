//! Práce na pozadí: periodické obnovování stavu a provádění akcí,
//! aby UI vlákno nikdy neblokovalo.

mod containers;
mod distro;
mod engine_ops;
mod fs_ops;
mod spec;
mod tracking;

pub use spec::{AddDistroSpec, RunSpec};

use crate::config::Config;
use crate::engine::Engine;
use crate::docker::{Docker, DfSummary, Snapshot, StatRow};
use crate::wsl::{self, Distro};
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum WslCmd {
    Start(String),
    Terminate(String),
    Shutdown,
    SetDefault(String),
}

#[derive(Debug, Clone)]
pub enum Term {
    Container { id: String, name: String },
    Logs { id: String, name: String },
    Distro(String),
    Dir(String),
}


#[derive(Debug, Clone)]
pub enum FsOp {
    List(String),
    Mkdir(String),
    Touch(String),
    Rename(String, String),
    Delete(Vec<String>),
    Chmod(String, String),
    /// Windows soubory/složky -> adresář v distribuci
    Upload(Vec<String>, String),
    /// položky v distribuci -> Windows složka
    Download(Vec<String>, String),
    Read(String),
    Write(String, String),
}

#[derive(Debug, Clone)]
pub enum Cmd {
    Refresh,
    Fs { root: bool, op: FsOp },
    Container(String, &'static str),
    PruneContainers,
    Run(RunSpec),
    Pull(String),
    /// hledání na Docker Hubu
    HubSearch(String),
    /// tagy image z Docker Hubu
    HubTags(String),
    RemoveImage(String),
    PruneImages { all: bool },
    CreateVolume(String),
    RemoveVolume(String),
    PruneVolumes,
    CreateNetwork(String, String),
    RemoveNetwork(String),
    PruneNetworks,
    /// docker compose v pracovním adresáři projektu
    Compose { dir: String, project: Option<String>, args: Vec<String> },
    /// docker compose up -d pro složku (Windows nebo Linux cesta)
    ComposeUpPath(String),
    Wsl(WslCmd),
    EngineStart,
    EngineStop,
    EngineRestart,
    SetupEngine,
    /// nainstalovat engine (apt), případně zapnout systemd, a nastavit API
    InstallEngine(Engine),
    /// seznam distribucí k instalaci (wsl --list --online)
    ListOnline,
    /// nainstalovat a připravit novou distribuci
    AddDistro(AddDistroSpec),
    /// odinstalovat engine (a odebrat nastavení API); purge = smazat i data
    UninstallEngine { engine: Engine, purge: bool },
    Sleep,
    SetKeepAlive(bool),
    PauseAll,
    UnpauseAll,
    LoadDaemonJson,
    SaveDaemonJson(String),
    Terminal(Term),
    Notepad(String),
    Explorer(String),
}

pub enum Msg {
    Snapshot(Box<Snapshot>),
    Distros(Vec<Distro>),
    Stats(Vec<StatRow>, Option<DfSummary>),
    Toast(bool, String),
    Output(String, String),
    DaemonJson(String),
    EngineInfo { tcp_configured: bool },
    FsListing(String, Vec<crate::fs::Entry>),
    FsListError(String, String),
    FsFile(String, String),
    FsChanged,
    HubResults(String, Result<Vec<crate::docker::HubRepo>, String>),
    HubTags(String, Result<Vec<crate::docker::HubTag>, String>),
    OnlineDistros(Result<Vec<(String, String)>, String>),
    /// worker změnil konfiguraci (např. přepnul distribuci) – UI si má obnovit koncept nastavení
    ConfigChanged,
}

/// Souhrnný stav pro tray (zapisuje worker, čte vlákno UI/traye).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineStatus {
    pub connected: bool,
    pub distro_running: bool,
    pub distro: String,
    /// název enginu (Docker / Podman)
    pub engine: String,
    pub running: i64,
    pub total: i64,
}

impl EngineStatus {
    pub fn text(&self) -> String {
        if self.connected {
            tf!("{} běží ({}) · {}/{} kontejnerů", "{} running ({}) · {}/{} containers", self.engine, self.distro, self.running, self.total)
        } else if self.distro_running {
            tf!("{} běží, Docker API nedostupné", "{} running, Docker API unavailable", self.distro)
        } else {
            tf!("{} zastavena", "{} stopped", self.distro)
        }
    }
}

#[derive(Default)]
pub struct Shared {
    pub status: Mutex<EngineStatus>,
    pub busy: AtomicUsize,
    pub want_stats: AtomicBool,
    pub tcp_checked: AtomicBool,
    stats_running: AtomicBool,
    keepalive: Mutex<Option<(Child, Instant)>>,
    /// engine byl při minulém obnovení připojený (pro detekci nového připojení)
    was_connected: AtomicBool,
    /// neukládat seznam běžících kontejnerů (aplikace právě vypíná / uspává engine)
    suppress_track: AtomicBool,
    /// naposledy uložený seznam (abychom zbytečně nezapisovali soubor)
    last_tracked: Mutex<Vec<String>>,
}

#[derive(Clone)]
struct Worker {
    ctx: eframe::egui::Context,
    cfg: Arc<RwLock<Config>>,
    msg: Sender<Msg>,
    cmd: Sender<Cmd>,
    shared: Arc<Shared>,
}

pub fn spawn(ctx: eframe::egui::Context, cfg: Arc<RwLock<Config>>) -> (Sender<Cmd>, Receiver<Msg>, Arc<Shared>) {
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    let (msg_tx, msg_rx) = channel::<Msg>();
    let shared = Arc::new(Shared::default());
    let w = Worker { ctx, cfg, msg: msg_tx, cmd: cmd_tx.clone(), shared: shared.clone() };
    thread::spawn(move || w.run(cmd_rx));
    (cmd_tx, msg_rx, shared)
}

impl Worker {
    fn send(&self, m: Msg) {
        let _ = self.msg.send(m);
        self.ctx.request_repaint();
    }
    fn cfg(&self) -> Config {
        self.cfg.read().map(|c| c.clone()).unwrap_or_default()
    }
    fn docker(&self) -> Docker {
        Docker::new(self.cfg().port)
    }

    fn run(self, rx: Receiver<Cmd>) {
        if self.cfg().autostart_engine {
            let w = self.clone();
            thread::spawn(move || w.exec(Cmd::EngineStart));
        }
        self.refresh();
        loop {
            let secs = self.cfg().refresh_secs.clamp(1, 600);
            match rx.recv_timeout(Duration::from_secs(secs)) {
                Ok(Cmd::Refresh) => {
                    // sloučit nahromaděné požadavky na obnovení
                    while let Ok(Cmd::Refresh) = rx.try_recv() {}
                    self.refresh()
                }
                Err(RecvTimeoutError::Timeout) => self.refresh(),
                Ok(cmd) => {
                    let w = self.clone();
                    thread::spawn(move || w.exec(cmd));
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    fn refresh(&self) {
        let snap = self.docker().snapshot();
        let running: Vec<(String, String)> =
            snap.containers.iter().filter(|c| c.running()).map(|c| (c.id.clone(), c.name())).collect();
        let connected = snap.connected;
        let (n_run, n_total) = snap.info.as_ref().map(|i| (i.containers_running, i.containers)).unwrap_or((0, 0));
        self.track_running(&snap);
        self.send(Msg::Snapshot(Box::new(snap)));
        if let Ok(list) = wsl::list() {
            let distro = self.cfg().distro;
            let running = list.iter().any(|d| d.name == distro && d.running());
            if let Ok(mut st) = self.shared.status.lock() {
                *st = EngineStatus {
                    connected,
                    distro_running: running,
                    distro: distro.clone(),
                    engine: self.cfg().engine.name().to_string(),
                    running: n_run,
                    total: n_total,
                };
            }
            self.send(Msg::Distros(list));
            // běžící distribuci udržíme naživu, jinak ji WSL po nečinnosti zastaví i s Dockerem
            if running && self.cfg().keep_alive {
                let _ = self.ensure_keepalive(&distro);
            }
            // kontrolu TCP API děláme jen v běžící distribuci, ať ji zbytečně neprobouzíme
            if running && !self.shared.tcp_checked.swap(true, Ordering::SeqCst) {
                let w = self.clone();
                thread::spawn(move || w.check_engine());
            }
        }
        if connected
            && self.shared.want_stats.load(Ordering::Relaxed)
            && !self.shared.stats_running.swap(true, Ordering::SeqCst)
        {
            let w = self.clone();
            thread::spawn(move || {
                let d = w.docker();
                let handles: Vec<_> = running
                    .into_iter()
                    .map(|(id, name)| {
                        let d = d.clone();
                        thread::spawn(move || d.stats(&id, &name))
                    })
                    .collect();
                let rows: Vec<StatRow> = handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect();
                let df = d.df();
                w.send(Msg::Stats(rows, df));
                w.shared.stats_running.store(false, Ordering::SeqCst);
            });
        }
    }

    fn check_engine(&self) {
        let c = self.cfg();
        let ok = wsl::run(&c.distro, Some("root"), None, &["test", "-f", c.engine.api_marker()]).is_ok();
        self.send(Msg::EngineInfo { tcp_configured: ok });
    }

    fn wait_ping(&self, secs: u64) -> bool {
        let d = self.docker();
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(secs) {
            if d.ping() {
                return true;
            }
            thread::sleep(Duration::from_millis(500));
        }
        false
    }

    fn exec(&self, cmd: Cmd) {
        self.shared.busy.fetch_add(1, Ordering::SeqCst);
        self.ctx.request_repaint();
        let is_fs = matches!(cmd, Cmd::Fs { .. } | Cmd::HubSearch(_) | Cmd::HubTags(_) | Cmd::ListOnline);
        let res = self.exec_inner(cmd);
        self.shared.busy.fetch_sub(1, Ordering::SeqCst);
        match res {
            Ok(s) if !s.trim().is_empty() => self.send(Msg::Toast(true, s.trim().to_string())),
            Ok(_) => {}
            Err(e) => self.send(Msg::Toast(false, e.trim().to_string())),
        }
        if !is_fs {
            let _ = self.cmd.send(Cmd::Refresh);
        }
    }

    fn exec_inner(&self, cmd: Cmd) -> Result<String, String> {
        let cfg = self.cfg();
        let distro = cfg.distro.clone();
        let d = self.docker();
        match cmd {
            Cmd::Refresh => Ok(String::new()),
            Cmd::Fs { root, op } => self.fs_op(root, op),
            Cmd::Container(id, action) => d.container_action(&id, action).map(|_| String::new()),
            Cmd::PruneContainers => d.prune("containers", None),
            Cmd::Run(spec) => self.run_container(spec),
            Cmd::Pull(r) => {
                self.send(Msg::Toast(true, tf!("Stahuji {r}…", "Pulling {r}…")));
                d.pull(&r)
            }
            Cmd::HubSearch(term) => {
                let r = d.search(&term, cfg.engine);
                self.send(Msg::HubResults(term, r));
                Ok(String::new())
            }
            Cmd::HubTags(repo) => {
                let r = crate::docker::hub_tags(&repo);
                self.send(Msg::HubTags(repo, r));
                Ok(String::new())
            }
            Cmd::RemoveImage(id) => d.delete(&format!("/images/{}", crate::docker::enc(&id))).map(|_| t!("Image odstraněn", "Image removed").into()),
            Cmd::PruneImages { all } => d.prune("images", Some(if all { r#"{"dangling":["false"]}"# } else { r#"{"dangling":["true"]}"# })),
            Cmd::CreateVolume(n) => d.post_json("/volumes/create", serde_json::json!({ "Name": n })).map(|_| tf!("Volume {n} vytvořen", "Volume {n} created")),
            Cmd::RemoveVolume(n) => d.delete(&format!("/volumes/{}", crate::docker::enc(&n))).map(|_| tf!("Volume {n} odstraněn", "Volume {n} removed")),
            Cmd::PruneVolumes => d.prune("volumes", Some(r#"{"all":["true"]}"#)),
            Cmd::CreateNetwork(n, drv) => d
                .post_json("/networks/create", serde_json::json!({ "Name": n, "Driver": drv }))
                .map(|_| tf!("Síť {n} vytvořena", "Network {n} created")),
            Cmd::RemoveNetwork(id) => d.delete(&format!("/networks/{id}")).map(|_| t!("Síť odstraněna", "Network removed").into()),
            Cmd::PruneNetworks => d.prune("networks", None),
            Cmd::Compose { dir, project, args } => self.compose(dir, project, args),
            Cmd::ComposeUpPath(path) => {
                let dir = wsl::to_linux_path(&distro, &path)?;
                self.exec_inner(Cmd::Compose { dir, project: None, args: vec!["up".into(), "-d".into()] })
            }
            Cmd::Wsl(w) => match w {
                WslCmd::Start(n) => {
                    if n == distro && cfg.keep_alive {
                        self.ensure_keepalive(&n)?;
                    } else {
                        wsl::start(&n)?;
                    }
                    Ok(tf!("Distribuce {n} spuštěna", "Distribution {n} started"))
                }
                WslCmd::Terminate(n) => {
                    if n == distro {
                        self.suppress_tracking();
                        self.kill_keepalive();
                    }
                    wsl::terminate(&n).map(|_| tf!("Distribuce {n} ukončena", "Distribution {n} terminated"))
                }
                WslCmd::Shutdown => {
                    self.suppress_tracking();
                    self.kill_keepalive();
                    wsl::shutdown().map(|_| t!("WSL vypnuto", "WSL shut down").into())
                }
                WslCmd::SetDefault(n) => wsl::set_default(&n).map(|_| tf!("{n} je nyní výchozí distribuce", "{n} is now the default distribution")),
            },
            Cmd::EngineStart => self.engine_start(),
            Cmd::EngineStop => self.engine_stop(),
            Cmd::EngineRestart => self.engine_restart(),
            Cmd::SetupEngine => self.setup_api(&cfg).map(|(out, _)| out),
            Cmd::UninstallEngine { engine, purge } => self.uninstall_engine(engine, purge),
            Cmd::ListOnline => {
                self.send(Msg::OnlineDistros(wsl::list_online()));
                Ok(String::new())
            }
            Cmd::AddDistro(spec) => {
                let mut log = String::new();
                let title = tf!("Nová distribuce: {}", "New distribution: {}", if spec.name.trim().is_empty() { &spec.distro } else { spec.name.trim() });
                let res = self.add_distro(spec, &mut log);
                self.send(Msg::Output(title, log));
                res
            }
            Cmd::InstallEngine(engine) => {
                let mut log = String::new();
                let res = self.install_engine(engine, &mut log);
                self.send(Msg::Output(tf!("Instalace: {}", "Installation: {}", engine.name()), log));
                res
            }
            Cmd::Sleep => self.sleep_distro(),
            Cmd::SetKeepAlive(on) => self.set_keep_alive(on),
            Cmd::PauseAll => self.set_paused_all(true),
            Cmd::UnpauseAll => self.set_paused_all(false),
            Cmd::LoadDaemonJson => self.load_daemon_json(),
            Cmd::SaveDaemonJson(text) => self.save_daemon_json(text),
            Cmd::Terminal(t) => self.terminal(t),
            Cmd::Notepad(p) => wsl::hidden("notepad.exe")
                .arg(wsl::unc(&distro, &p))
                .spawn()
                .map(|_| String::new())
                .map_err(|e| e.to_string()),
            Cmd::Explorer(p) => std::process::Command::new("explorer.exe")
                .arg(wsl::unc(&distro, &p))
                .spawn()
                .map(|_| String::new())
                .map_err(|e| e.to_string()),
        }
    }
}
