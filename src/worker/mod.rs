//! Práce na pozadí: periodické obnovování stavu a provádění akcí,
//! aby UI vlákno nikdy neblokovalo.

pub(crate) mod build;
pub(crate) mod compose;
mod container_fs;
mod containers;
mod detail;
mod distro;
mod distro_manage;
mod engine_ops;
mod events;
mod fs_ops;
mod notify;
mod recreate_body;
mod spec;
#[cfg(test)]
pub mod testkit;
mod tracking;
mod volumes;

pub use volumes::{backup_file_name, valid_volume_name};
#[cfg(test)]
pub use volumes::{du_sizes, volume_op_for_test, volume_sizes_for_test, HELPER_IMAGE};
pub use build::cancel_build;
pub use spec::{is_windows_path, AddDistroSpec, BuildSpec, RunSpec};
pub use compose::{NewStackSpec, StackMsg, StackOp};
pub use container_fs::{CfMsg, CfOp};
pub use detail::{start_stats, StatSample, StatsFeed, STATS_WINDOW};
pub use distro_manage::{default_import_folder, DistroDisk, DistroOp};
pub use engine_ops::api_port;
#[cfg(test)]
pub use detail::recreate_for_test;
#[cfg(test)]
pub use compose::check_stack_for_test;
#[cfg(test)]
pub use containers::run_for_test;

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
    /// sestavení image z Dockerfile (výstup průběžně jako Msg::BuildOutput)
    Build(BuildSpec),
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
    /// velikosti volume -> Msg::VolumeSizes
    VolumeSizes,
    /// záloha volume do .tar.gz ve Windows (stop = zastavit běžící kontejnery s volume)
    BackupVolume { volume: String, target: String, stop: bool },
    /// obnova volume ze zálohy (create = nový volume, clear = nejdřív vyprázdnit)
    RestoreVolume { source: String, volume: String, create: bool, clear: bool },
    /// kopie volume pod novým názvem
    CloneVolume { from: String, to: String },
    CreateNetwork(String, String),
    RemoveNetwork(String),
    PruneNetworks,
    /// docker compose v pracovním adresáři projektu
    Compose { dir: String, project: Option<String>, args: Vec<String> },
    /// docker compose up -d pro složku (Windows nebo Linux cesta)
    ComposeUpPath(String),
    /// nový stack ze šablony, editor compose souborů, kontrola (-> Msg::Stack)
    Stack(StackOp),
    Wsl(WslCmd),
    EngineStart,
    EngineStop,
    EngineRestart,
    SetupEngine,
    /// nainstalovat engine (apt), případně zapnout systemd, a nastavit API (true = s TLS)
    InstallEngine(Engine, bool),
    /// seznam distribucí k instalaci (wsl --list --online)
    ListOnline,
    /// nainstalovat a připravit novou distribuci
    AddDistro(AddDistroSpec),
    /// odebrání / export / import / disk distribuce
    Distro(DistroOp),
    /// odinstalovat engine (a odebrat nastavení API); purge = smazat i data
    UninstallEngine { engine: Engine, purge: bool },
    Sleep,
    /// zapnout / vypnout zabezpečení API (TLS s klientským certifikátem)
    SetTls(bool),
    /// přegenerovat TLS certifikáty
    RegenCerts,
    SetKeepAlive(bool),
    PauseAll,
    UnpauseAll,
    LoadDaemonJson,
    SaveDaemonJson(String),
    Terminal(Term),
    Notepad(String),
    Explorer(String),
    /// detail kontejneru (inspect) -> Msg::Inspect
    Inspect(String),
    /// znovu vytvořit kontejner (id) s novými parametry
    Recreate { id: String, spec: RunSpec, net: crate::inspect::NetEdit },
    /// soubory v kontejneru (view = číslo prohlížeče, kterému patří odpověď)
    ContainerFs { view: u64, id: String, op: CfOp },
}

impl Cmd {
    /// Krátká akce (API volání, výpis, otevření okna) – běží v poolu vláken workeru.
    pub fn is_quick(&self) -> bool {
        matches!(
            self,
            Cmd::Container(..)
                | Cmd::RemoveImage(_)
                | Cmd::CreateVolume(_)
                | Cmd::RemoveVolume(_)
                | Cmd::CreateNetwork(..)
                | Cmd::RemoveNetwork(_)
                | Cmd::HubSearch(_)
                | Cmd::HubTags(_)
                | Cmd::Inspect(_)
                | Cmd::VolumeSizes
                | Cmd::LoadDaemonJson
                | Cmd::Terminal(_)
                | Cmd::Notepad(_)
                | Cmd::Explorer(_)
                | Cmd::ListOnline
        ) || matches!(self, Cmd::ContainerFs { op, .. } if op.is_quick())
            || matches!(
            self,
            Cmd::Fs { op: FsOp::List(_) | FsOp::Read(_) | FsOp::Mkdir(_) | FsOp::Touch(_) | FsOp::Rename(..) | FsOp::Chmod(..), .. }
        )
    }
}

/// Počet vláken pro krátké akce.
const POOL_THREADS: usize = 4;

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
    /// řádek výstupu probíhajícího sestavení image
    BuildOutput(String),
    /// sestavení skončilo (text úspěchu / chyba)
    BuildDone(Result<String, String>),
    /// výsledek inspect kontejneru (id, jak bylo požadováno)
    Inspect(String, Result<Box<crate::inspect::Detail>, String>),
    /// velikosti volume v bajtech (chybí = neznámá)
    VolumeSizes(std::collections::HashMap<String, u64>),
    /// výsledky operací se stacky (Cmd::Stack)
    Stack(StackMsg),
    /// virtuální disk distribuce (název, cesta / velikost / sparse, nebo chyba)
    DistroDisk(String, Result<DistroDisk, String>),
    /// odpověď prohlížeči souborů kontejneru (číslo pohledu)
    ContainerFs(u64, CfMsg),
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
    /// kontejnery pro podnabídku traye (zapisuje worker při obnovení)
    pub tray_containers: Mutex<Vec<crate::tray::menu_model::Item>>,
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
    /// proud událostí běží – dotazování stačí zřídka
    pub events_live: AtomicBool,
    refresh_pending: AtomicBool,
    notifier: Mutex<notify::Notifier>,
    /// předchozí vzorek CPU (čas, kumulované ns) pro výpočet vytížení z rozdílu
    cpu_prev: Mutex<std::collections::HashMap<String, (Instant, u64)>>,
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
        self.spawn_events();
        let pool = self.spawn_pool(POOL_THREADS);
        loop {
            // s živým proudem událostí stačí občasná kontrola (stav WSL, záloha za ztracené události);
            // statistiky na přehledu se ale dál obnovují v nastaveném intervalu
            let mut secs = self.cfg().refresh_secs.clamp(1, 600);
            if self.shared.events_live.load(Ordering::SeqCst) && !self.shared.want_stats.load(Ordering::Relaxed) {
                secs = secs.max(30);
            }
            match rx.recv_timeout(Duration::from_secs(secs)) {
                Ok(Cmd::Refresh) => {
                    // sloučit nahromaděné požadavky na obnovení
                    while let Ok(Cmd::Refresh) = rx.try_recv() {}
                    self.refresh()
                }
                Err(RecvTimeoutError::Timeout) => self.refresh(),
                Ok(cmd) => {
                    if cmd.is_quick() {
                        // krátké akce: omezený počet vláken místo nového vlákna pro každou
                        let _ = pool.send(cmd);
                    } else {
                        // dlouhé (instalace, build, compose…) nesmí blokovat pool
                        let w = self.clone();
                        thread::spawn(move || w.exec(cmd));
                    }
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
        if let Ok(mut t) = self.shared.tray_containers.lock() {
            *t = snap.containers.iter().map(crate::tray::menu_model::Item::from_container).collect();
        }
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
                let mut rows: Vec<StatRow> = handles.into_iter().filter_map(|h| h.join().ok().flatten()).collect();
                // CPU z rozdílu kumulovaného času mezi obnoveními (100 % = jedno jádro) – funguje
                // i na Podmanu, který v jednorázových statistikách nevyplňuje precpu_stats
                if let Ok(mut prev) = w.shared.cpu_prev.lock() {
                    let now = Instant::now();
                    for r in &mut rows {
                        if let Some((t, total)) = prev.get(&r.id) {
                            let dt = now.duration_since(*t).as_nanos() as f64;
                            if dt > 0.0 && r.cpu_total >= *total {
                                r.cpu = (r.cpu_total - total) as f64 / dt * 100.0;
                            }
                        }
                    }
                    prev.clear();
                    prev.extend(rows.iter().map(|r| (r.id.clone(), (now, r.cpu_total))));
                }
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

    /// Pool vláken pro krátké akce; vrací frontu, do které se posílají.
    fn spawn_pool(&self, n: usize) -> Sender<Cmd> {
        let (tx, rx) = channel::<Cmd>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..n {
            let (w, rx) = (self.clone(), rx.clone());
            let _ = thread::Builder::new().name(format!("worker-{i}")).spawn(move || loop {
                let cmd = match rx.lock() {
                    Ok(r) => r.recv(),
                    Err(_) => return,
                };
                match cmd {
                    Ok(c) => w.exec(c),
                    Err(_) => return,
                }
            });
        }
        tx
    }

    fn wait_ping(&self, secs: u64) -> bool {
        self.wait_ping_on(self.cfg().port, secs)
    }

    fn wait_ping_on(&self, port: u16, secs: u64) -> bool {
        let d = Docker::new(port);
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
        let is_fs = matches!(cmd, Cmd::Fs { .. } | Cmd::ContainerFs { .. } | Cmd::HubSearch(_) | Cmd::HubTags(_) | Cmd::ListOnline | Cmd::Inspect(_) | Cmd::VolumeSizes)
            || matches!(cmd, Cmd::Stack(StackOp::Read { .. } | StackOp::Validate { .. }));
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
            Cmd::Container(id, action) => {
                if matches!(action, "stop" | "restart" | "kill" | "remove") {
                    self.mark_intended(&id);
                }
                d.container_action(&id, action).map(|_| String::new()).map_err(String::from)
            }
            Cmd::PruneContainers => d.prune("containers", None),
            Cmd::Run(spec) => self.run_container(spec),
            Cmd::Build(spec) => self.build_image(spec),
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
            Cmd::RemoveImage(id) => d.delete(&format!("/images/{}", crate::docker::enc(&id))).map(|_| t!("Image odstraněn", "Image removed").into()).map_err(String::from),
            Cmd::PruneImages { all } => d.prune("images", Some(if all { r#"{"dangling":["false"]}"# } else { r#"{"dangling":["true"]}"# })),
            Cmd::CreateVolume(n) => d.post_json("/volumes/create", serde_json::json!({ "Name": n })).map(|_| tf!("Volume {n} vytvořen", "Volume {n} created")).map_err(String::from),
            Cmd::RemoveVolume(n) => d.delete(&format!("/volumes/{}", crate::docker::enc(&n))).map(|_| tf!("Volume {n} odstraněn", "Volume {n} removed")).map_err(String::from),
            Cmd::PruneVolumes => d.prune("volumes", Some(r#"{"all":["true"]}"#)),
            Cmd::VolumeSizes => self.volume_sizes(),
            Cmd::BackupVolume { volume, target, stop } => self.backup_volume(volume, target, stop),
            Cmd::RestoreVolume { source, volume, create, clear } => self.restore_volume(source, volume, create, clear),
            Cmd::CloneVolume { from, to } => self.clone_volume(from, to),
            Cmd::CreateNetwork(n, drv) => d
                .post_json("/networks/create", serde_json::json!({ "Name": n, "Driver": drv }))
                .map(|_| tf!("Síť {n} vytvořena", "Network {n} created")).map_err(String::from),
            Cmd::RemoveNetwork(id) => d.delete(&format!("/networks/{id}")).map(|_| t!("Síť odstraněna", "Network removed").into()).map_err(String::from),
            Cmd::PruneNetworks => d.prune("networks", None),
            Cmd::Compose { dir, project, args } => {
                if args.iter().any(|a| matches!(a.as_str(), "down" | "stop" | "restart" | "rm" | "kill")) {
                    self.quiet_notifications(Duration::from_secs(60));
                }
                self.compose(dir, project, args)
            }
            Cmd::Stack(op) => self.stack_op(op),
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
                    wsl::terminate(&n).map(|_| tf!("Distribuce {n} ukončena", "Distribution {n} terminated")).map_err(String::from)
                }
                WslCmd::Shutdown => {
                    self.suppress_tracking();
                    self.kill_keepalive();
                    wsl::shutdown().map(|_| t!("WSL vypnuto", "WSL shut down").into()).map_err(String::from)
                }
                WslCmd::SetDefault(n) => wsl::set_default(&n).map(|_| tf!("{n} je nyní výchozí distribuce", "{n} is now the default distribution")).map_err(String::from),
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
            Cmd::Distro(op) => self.distro_op(op),
            Cmd::InstallEngine(engine, tls) => {
                let mut log = String::new();
                let res = self.install_engine(engine, tls, &mut log);
                self.send(Msg::Output(tf!("Instalace: {}", "Installation: {}", engine.name()), log));
                res
            }
            Cmd::Sleep => self.sleep_distro(),
            Cmd::SetTls(on) => self.set_tls(on),
            Cmd::RegenCerts => self.regen_certs(),
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
            Cmd::Explorer(p) => {
                // jen existující složka – cesta může pocházet z labelu kontejneru a explorer.exe
                // by soubor rovnou otevřel přidruženým programem (F-05)
                if wsl::run(&distro, None, None, &["test", "-d", &p]).is_err() {
                    return Err(tf!("{p} není složka v {distro}", "{p} is not a folder in {distro}"));
                }
                std::process::Command::new("explorer.exe")
                    .arg(wsl::unc(&distro, &p))
                    .spawn()
                    .map(|_| String::new())
                    .map_err(|e| e.to_string())
            }
            Cmd::Inspect(id) => self.inspect_detail(id),
            Cmd::Recreate { id, spec, net } => self.recreate(id, spec, net),
            Cmd::ContainerFs { view, id, op } => self.container_fs(view, id, op),
        }
    }
}
