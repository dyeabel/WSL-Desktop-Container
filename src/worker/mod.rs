//! Práce na pozadí: periodické obnovování stavu a provádění akcí,
//! aby UI vlákno nikdy neblokovalo.

pub(crate) mod alerts;
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
pub(crate) mod k8s;
mod limits;
mod memory;
pub(crate) mod net_overview;
pub(crate) mod notify;
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
pub use k8s::{K8sMsg, K8sOp};
pub use container_fs::{CfMsg, CfOp};
pub use detail::{start_stats, StatSample, StatsFeed, STATS_WINDOW};
pub use distro_manage::{default_import_folder, DistroDisk, DistroOp};
pub use engine_ops::api_port;
pub use net_overview::{start_net_overview, start_ns_feed, NetFeed, NsFeed, NET_WINDOW};
#[cfg(test)]
pub use detail::recreate_for_test;
#[cfg(test)]
pub use compose::check_stack_for_test;
#[cfg(test)]
pub use containers::run_for_test;

use crate::config::Config;
use crate::engine::Engine;
use crate::docker::{Docker, DfSummary, Snapshot, StatRow};
use crate::host::{HostExt, Privilege};
use crate::wsl::{self, Distro};
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    /// `kubectl logs -f` poda v clusteru k3s (v kontejneru enginu)
    PodLogs { ns: String, pod: String },
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
    /// Kubernetes laboratoř (-> Msg::K8s)
    K8s(K8sOp),
    Wsl(WslCmd),
    EngineStart,
    EngineStop,
    EngineRestart,
    SetupEngine,
    /// přepnout primární distribuci (API na portu aplikace); None = engine podle paměti / instalace
    /// sleep_old = dosavadní primární distribuci uspat (wsl --terminate)
    SetPrimary { distro: String, engine: Option<Engine>, sleep_old: bool },
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
    /// uvolnit paměť WSL (cache Linuxu) – VmmemWSL ji pak vrátí Windows
    FreeMemory,
    /// smazat historii zdrojů z disku
    ClearHistory,
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
    /// živá změna limitů kontejneru (id, název pro zprávu) -> pak Msg::Inspect
    UpdateLimits { id: String, name: String, update: crate::limits::LimitUpdate },
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
                | Cmd::UpdateLimits { .. }
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
    /// Kubernetes laboratoř (Cmd::K8s)
    K8s(K8sMsg),
    /// virtuální disk distribuce (název, cesta / velikost / sparse, nebo chyba)
    DistroDisk(String, Result<DistroDisk, String>),
    /// odpověď prohlížeči souborů kontejneru (číslo pohledu)
    ContainerFs(u64, CfMsg),
    /// uvolnění paměti WSL skončilo (auto = automatické z nastavení)
    MemoryFreed { auto: bool, result: Result<crate::memory::FreeEvent, String> },
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
    /// zvýší se po každém zápisu `status` a `tray_containers` (tray přestavuje nabídku jen při změně)
    pub tray_gen: AtomicU64,
    /// poslední `/system/df` (nejvýš jednou za [`DF_EVERY`])
    last_df: Mutex<Option<Instant>>,
    /// poslední seznam distribucí z `wsl.exe -l -v` (při skrytém okně se znovu nenačítá)
    last_distros: Mutex<Option<Vec<Distro>>>,
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
    /// historie problémů kontejnerů (stránka Události)
    pub event_log: Arc<Mutex<crate::eventlog::EventLog>>,
    /// poslední automatické uvolnění paměti
    last_free_memory: Mutex<Option<Instant>>,
    /// předchozí vzorek CPU (čas, kumulované ns) pro výpočet vytížení z rozdílu
    cpu_prev: Mutex<std::collections::HashMap<String, (Instant, u64)>>,
    /// přehled paměti WSL: historie, aktuální rozpad (zapisuje worker, čte UI)
    pub memory: Mutex<crate::memory::MemState>,
    mem_probe: Mutex<crate::memory::VmmemProbe>,
    mem_running: AtomicBool,
    /// upozornění na prahy paměti a disku (zapisuje worker, aktivní čte UI pro pruh)
    pub alerts: Mutex<alerts::Alerts>,
    /// limity běžících kontejnerů pro tabulku výkonu (čas načtení, limity)
    limits: Mutex<std::collections::HashMap<String, (Instant, crate::limits::Limits)>>,
}

/// Nejkratší odstup mezi voláními `/system/df` na přehledu.
const DF_EVERY: Duration = Duration::from_secs(30);

/// Je čas znovu zavolat `/system/df`? (nikdy nevolané = ano)
fn df_due(last: Option<Instant>, now: Instant, every: Duration) -> bool {
    last.map_or(true, |t| now.saturating_duration_since(t) >= every)
}

/// Je potřeba spustit `wsl.exe -l -v`? Ne, když je okno skryté, API enginu odpovídá a existuje
/// poslední seznam, který jde použít.
fn need_wsl_list(visible: bool, connected: bool, have_cached: bool) -> bool {
    visible || !connected || !have_cached
}

/// Výsledek [`Worker::distro_list`].
struct DistroList {
    items: Vec<Distro>,
    /// čerstvě načteno z `wsl.exe` (jinak z mezipaměti)
    fresh: bool,
    /// liší se od naposledy odeslaného do UI
    changed: bool,
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
    if let Ok(mut l) = shared.event_log.lock() {
        *l = crate::eventlog::EventLog::load();
    }
    crate::history::start(shared.clone(), cfg.read().map(|c| c.history_on_disk).unwrap_or(true));
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
    fn host(&self) -> std::sync::Arc<dyn crate::host::Host> {
        crate::host::current(&self.cfg())
    }
    /// WSL hostitel pro akce, které existují jen ve WSL (jinde chyba místo paniky).
    fn wsl_host(&self) -> Result<crate::host::wsl::WslHost, String> {
        self.host().wsl().map(|w| w.for_distro(w.distro())).ok_or_else(crate::host::unavailable)
    }
    fn docker(&self) -> Docker {
        Docker::new(&crate::host::endpoint(&self.cfg()))
    }

    fn run(self, rx: Receiver<Cmd>) {
        // Linux: typ enginu podle jeho API (na pozadí, UI nečeká)
        if self.host().kind() == crate::host::HostKind::Local {
            let w = self.clone();
            thread::spawn(move || w.detect_local_engine());
        }
        // spuštěno ze shimu = někdo právě potřebuje engine
        if self.cfg().autostart_engine || crate::wake::launched_from_shim() {
            let w = self.clone();
            // Linux: bez kliknutí nikdy pkexec (běžící engine nechat, systémovou službu přeskočit)
            if self.host().wsl().is_none() {
                thread::spawn(move || w.autostart_local());
            } else {
                thread::spawn(move || w.exec(Cmd::EngineStart));
            }
        }
        // docker shim žádá o start enginu (aplikace už běží, engine ne)
        let tx = self.cmd.clone();
        crate::wake::listen(move || {
            let _ = tx.send(Cmd::EngineStart);
        });
        self.refresh();
        self.spawn_events();
        let pool = self.spawn_pool(POOL_THREADS);
        loop {
            // s živým proudem událostí stačí občasná kontrola (stav WSL, záloha za ztracené události);
            // statistiky na přehledu se ale dál obnovují v nastaveném intervalu
            // okno schované -> vlastní (delší) interval; zobrazení okna pošle Cmd::Refresh hned
            let cfg = self.cfg();
            let mut secs = if crate::tray::window_visible() { cfg.refresh_secs } else { cfg.background_refresh_secs }.clamp(1, 600);
            if self.shared.events_live.load(Ordering::SeqCst) && !self.shared.want_stats.load(Ordering::Relaxed) {
                secs = secs.max(30);
            }
            match rx.recv_timeout(Duration::from_secs(secs)) {
                Ok(Cmd::Refresh) => {
                    // výslovné obnovení: engine hledat znovu (Linux)
                    crate::host::refresh_discovery();
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
        let ncpu = snap.info.as_ref().map(|i| i.ncpu.max(1)).unwrap_or(1) as f64;
        let (n_run, n_total) = snap.info.as_ref().map(|i| (i.containers_running, i.containers)).unwrap_or((0, 0));
        self.track_running(&snap);
        // relaye [::1] -> 127.0.0.1 pro publikované porty (odpojený engine = žádné)
        crate::portrelay::sync_app(&snap, self.cfg().ipv6_localhost_relay);
        if let Ok(mut t) = self.shared.tray_containers.lock() {
            *t = snap.containers.iter().map(crate::tray::menu_model::Item::from_container).collect();
            self.shared.tray_gen.fetch_add(1, Ordering::Release);
        }
        self.send(Msg::Snapshot(Box::new(snap)));
        let mut distro_running = false;
        if crate::host::current_kind() == crate::host::HostKind::Local {
            // Linux: hostitel běží vždy, žádné distribuce ani keepalive
            distro_running = true;
            if let Ok(mut st) = self.shared.status.lock() {
                *st = EngineStatus {
                    connected,
                    distro_running: true,
                    distro: self.host().label(),
                    engine: self.cfg().engine.name().to_string(),
                    running: n_run,
                    total: n_total,
                };
                self.shared.tray_gen.fetch_add(1, Ordering::Release);
            }
        } else if let Some(list) = self.distro_list(connected) {
            let distro = self.cfg().distro;
            // z mezipaměti (skryté okno, API odpovídá) distribuce běží
            let running = !list.fresh || list.items.iter().any(|d| d.name == distro && d.running());
            distro_running = running;
            if let Ok(mut st) = self.shared.status.lock() {
                *st = EngineStatus {
                    connected,
                    distro_running: running,
                    distro: distro.clone(),
                    engine: self.cfg().engine.name().to_string(),
                    running: n_run,
                    total: n_total,
                };
                self.shared.tray_gen.fetch_add(1, Ordering::Release);
            }
            if list.changed {
                self.send(Msg::Distros(list.items));
            }
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
        self.auto_free_memory();
        let want_stats = self.shared.want_stats.load(Ordering::Relaxed);
        self.memory_tick(distro_running, connected && want_stats);
        self.alerts_tick(distro_running, connected);
        if connected && want_stats && !self.shared.stats_running.swap(true, Ordering::SeqCst) {
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
                w.fill_limits(&d, &mut rows);
                let df = w.df_if_due(&d);
                w.history_stats(&rows, df.as_ref(), ncpu);
                if distro_running {
                    w.memory_sample(Some(&rows));
                }
                w.send(Msg::Stats(rows, df));
                w.shared.stats_running.store(false, Ordering::SeqCst);
            });
        }
    }

    /// `/system/df` nejvýš jednou za [`DF_EVERY`]; jinak `None` (UI si ponechá poslední hodnotu).
    fn df_if_due(&self, d: &Docker) -> Option<DfSummary> {
        let last = self.shared.last_df.lock().ok().and_then(|g| *g);
        if !df_due(last, Instant::now(), DF_EVERY) {
            return None;
        }
        let df = d.df();
        if df.is_some() {
            if let Ok(mut g) = self.shared.last_df.lock() {
                *g = Some(Instant::now());
            }
        }
        df
    }

    /// Seznam distribucí: při skrytém okně a odpovídajícím API se `wsl.exe` nespouští a použije
    /// se poslední seznam. `changed` = liší se od naposledy odeslaného do UI.
    fn distro_list(&self, connected: bool) -> Option<DistroList> {
        let cached = self.shared.last_distros.lock().ok().and_then(|g| g.clone());
        if !need_wsl_list(crate::tray::window_visible(), connected, cached.is_some()) {
            return cached.map(|items| DistroList { items, fresh: false, changed: false });
        }
        let items = wsl::list().ok()?;
        let changed = cached.as_ref() != Some(&items);
        if changed {
            if let Ok(mut g) = self.shared.last_distros.lock() {
                *g = Some(items.clone());
            }
        }
        Some(DistroList { items, fresh: true, changed })
    }

    /// Zjistí z `/version`, zda na socketu běží Docker, nebo Podman, a uloží to do konfigurace.
    /// Nedostupný engine = nechat nastavený typ.
    fn detect_local_engine(&self) {
        let Ok(v) = self.docker().get::<serde_json::Value>("/version") else { return };
        let Some(engine) = crate::engine::Engine::from_version(&v) else { return };
        // zámek konfigurace jen na změnu, uložení až po jeho uvolnění
        let changed = match self.cfg.write() {
            Ok(mut g) if g.engine != engine => {
                g.engine = engine;
                Some(g.clone())
            }
            _ => None,
        };
        if let Some(c) = changed {
            let _ = c.save();
            self.ctx.request_repaint();
        }
    }

    fn check_engine(&self) {
        let c = self.cfg();
        let host = crate::host::current(&c);
        // Linux: aplikace API nenastavuje (marker v /etc nemá smysl) – API je socket enginu
        if !host.caps().engine_install {
            return;
        }
        let ok = host.run(Privilege::Engine, None, &["test", "-f", c.engine.api_marker()]).is_ok();
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
        self.wait_ping_on(&crate::host::endpoint(&self.cfg()), secs)
    }

    fn wait_ping_on(&self, ep: &crate::http::Endpoint, secs: u64) -> bool {
        let d = Docker::new(ep);
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
            || matches!(cmd, Cmd::Stack(StackOp::Read { .. } | StackOp::Validate { .. }))
            || matches!(cmd, Cmd::K8s(K8sOp::Snapshot { .. } | K8sOp::Probe { .. }));
        // přehled „Na pozadí“: akce uživatele (bez obnovování a procházení)
        let is_fs = is_fs || matches!(cmd, Cmd::Terminal(Term::PodLogs { .. }));
        let mut act = (!is_fs && !matches!(cmd, Cmd::Refresh)).then(|| crate::activity::start(crate::activity::Kind::Action, activity_label(&cmd), String::new()));
        let res = self.exec_inner(cmd);
        if let Some(a) = act.as_mut() {
            a.set_ok(res.is_ok());
        }
        drop(act);
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
        let caps = crate::host::current_caps();
        // pojistka: akce jen pro WSL (instalační a nastavovací skripty, wsl.exe, Windows
        // programy) se na jiném hostiteli nikdy nespustí, i kdyby je UI poslalo
        if !supported(caps, &cmd) {
            return Err(crate::host::unavailable());
        }
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
            Cmd::K8s(op) => self.k8s_op(op),
            Cmd::ComposeUpPath(path) => {
                // Linux: cesta je rovnou místní
                let dir = if self.host().wsl().is_some() { wsl::to_linux_path(&distro, &path)? } else { path };
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
            // po startu / restartu hledat engine znovu (Linux: socket mohl vzniknout až teď)
            Cmd::EngineStart => {
                crate::host::refresh_discovery();
                let r = self.engine_start();
                crate::host::refresh_discovery();
                r
            }
            Cmd::EngineStop => {
                let r = self.engine_stop();
                crate::host::refresh_discovery();
                r
            }
            Cmd::EngineRestart => {
                let r = self.engine_restart();
                crate::host::refresh_discovery();
                r
            }
            Cmd::SetupEngine => {
                let r = self.setup_api(&cfg).map(|(out, _)| out);
                crate::host::refresh_discovery();
                r
            }
            Cmd::SetPrimary { distro, engine, sleep_old } => {
                let mut log = String::new();
                let r = self.set_primary(&distro, engine, sleep_old, &mut log);
                self.send(Msg::Output(tf!("Primární distribuce: {distro}", "Primary distribution: {distro}"), log));
                r
            }
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
            Cmd::FreeMemory => self.free_memory(false),
            Cmd::ClearHistory => crate::history::store().clear().map(|_| t!("Historie zdrojů smazána", "Resource history deleted").into()),
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
                // by soubor rovnou otevřel přidruženým programem (F-05). Kontrola v Linuxu nestačí:
                // Win32 jinak normalizuje názvy (`x.hta.` → `x.hta`, N-02) – proto ještě ve Windows
                // na výsledné UNC cestě těsně před spuštěním.
                if self.host().run(Privilege::User, None, &["test", "-d", &p]).is_err() {
                    return Err(tf!("{p} není složka v {distro}", "{p} is not a folder in {distro}"));
                }
                let unc = wsl::explorer_dir(&distro, &p)?;
                std::process::Command::new("explorer.exe")
                    .arg(unc)
                    .spawn()
                    .map(|_| String::new())
                    .map_err(|e| e.to_string())
            }
            Cmd::Inspect(id) => self.inspect_detail(id),
            Cmd::Recreate { id, spec, net } => self.recreate(id, spec, net),
            Cmd::UpdateLimits { id, name, update } => self.update_limits(id, name, update),
            Cmd::ContainerFs { view, id, op } => self.container_fs(view, id, op),
        }
    }
}

/// Umí aktuální hostitel akci? Na WSL (všechny schopnosti) vždy ano.
fn supported(caps: crate::host::Caps, c: &Cmd) -> bool {
    match c {
        Cmd::Wsl(_)
        | Cmd::ListOnline
        | Cmd::AddDistro(_)
        | Cmd::Distro(_)
        | Cmd::SetPrimary { .. }
        | Cmd::Sleep
        | Cmd::SetKeepAlive(_)
        // Windows programy nad UNC cestou distribuce
        | Cmd::Notepad(_)
        | Cmd::Explorer(_) => caps.distros,
        Cmd::InstallEngine(..) | Cmd::UninstallEngine { .. } | Cmd::SetupEngine => caps.engine_install,
        Cmd::SetTls(_) | Cmd::RegenCerts => caps.api_tls,
        Cmd::LoadDaemonJson | Cmd::SaveDaemonJson(_) => caps.daemon_json,
        Cmd::FreeMemory => caps.vm_memory,
        Cmd::Fs { root: true, .. } => caps.files_root,
        _ => true,
    }
}

/// Popis akce pro přehled „Na pozadí“.
fn activity_label(c: &Cmd) -> String {
    let short = |id: &str| id.chars().take(12).collect::<String>();
    match c {
        Cmd::Container(id, a) => tf!("Kontejner {}: {a}", "Container {}: {a}", short(id)),
        Cmd::Run(_) => t!("Spuštění kontejneru", "Running a container").into(),
        Cmd::Build(_) => t!("Sestavení image", "Building an image").into(),
        Cmd::Pull(r) => tf!("Stahování {r}", "Pulling {r}"),
        Cmd::RemoveImage(id) => tf!("Odstranění image {}", "Removing image {}", short(id)),
        Cmd::PruneContainers | Cmd::PruneImages { .. } | Cmd::PruneVolumes | Cmd::PruneNetworks => t!("Úklid (prune)", "Clean-up (prune)").into(),
        Cmd::CreateVolume(n) => tf!("Vytvoření volume {n}", "Creating volume {n}"),
        Cmd::RemoveVolume(n) => tf!("Odstranění volume {n}", "Removing volume {n}"),
        Cmd::BackupVolume { volume, .. } => tf!("Záloha volume {volume}", "Backing up volume {volume}"),
        Cmd::RestoreVolume { volume, .. } => tf!("Obnova volume {volume}", "Restoring volume {volume}"),
        Cmd::CloneVolume { from, to } => tf!("Kopie volume {from} → {to}", "Copying volume {from} → {to}"),
        Cmd::CreateNetwork(n, _) => tf!("Vytvoření sítě {n}", "Creating network {n}"),
        Cmd::RemoveNetwork(n) => tf!("Odstranění sítě {}", "Removing network {}", short(n)),
        Cmd::Compose { args, project, .. } => format!("compose {}{}", project.as_deref().map(|p| format!("-p {p} ")).unwrap_or_default(), args.join(" ")),
        Cmd::ComposeUpPath(p) => format!("compose up -d ({p})"),
        Cmd::Wsl(w) => match w {
            WslCmd::Start(n) => tf!("Spuštění distribuce {n}", "Starting distribution {n}"),
            WslCmd::Terminate(n) => tf!("Ukončení distribuce {n}", "Terminating distribution {n}"),
            WslCmd::Shutdown => t!("Vypnutí WSL", "Shutting down WSL").into(),
            WslCmd::SetDefault(n) => tf!("Výchozí distribuce {n}", "Default distribution {n}"),
        },
        Cmd::EngineStart => t!("Start enginu", "Starting the engine").into(),
        Cmd::EngineStop => t!("Zastavení enginu", "Stopping the engine").into(),
        Cmd::EngineRestart => t!("Restart enginu", "Restarting the engine").into(),
        Cmd::SetupEngine => t!("Nastavení API enginu", "Setting up the engine API").into(),
        Cmd::SetPrimary { distro, .. } => tf!("Přepnutí primární distribuce na {distro}", "Switching the primary distribution to {distro}"),
        Cmd::InstallEngine(e, _) => tf!("Instalace {}", "Installing {}", e.name()),
        Cmd::UninstallEngine { engine, .. } => tf!("Odinstalace {}", "Uninstalling {}", engine.name()),
        Cmd::AddDistro(_) => t!("Přidání distribuce", "Adding a distribution").into(),
        Cmd::Sleep => t!("Uspání distribuce", "Putting the distribution to sleep").into(),
        Cmd::SetTls(on) => if *on { t!("Zapnutí TLS", "Turning TLS on") } else { t!("Vypnutí TLS", "Turning TLS off") }.into(),
        Cmd::FreeMemory => t!("Uvolnění paměti WSL", "Freeing WSL memory").into(),
        Cmd::RegenCerts => t!("Nové certifikáty", "New certificates").into(),
        Cmd::PauseAll => t!("Pozastavení kontejnerů", "Pausing containers").into(),
        Cmd::UnpauseAll => t!("Obnovení kontejnerů", "Resuming containers").into(),
        Cmd::SaveDaemonJson(_) => t!("Uložení daemon.json", "Saving daemon.json").into(),
        Cmd::Recreate { .. } => t!("Znovuvytvoření kontejneru", "Recreating a container").into(),
        Cmd::UpdateLimits { name, .. } => tf!("Limity kontejneru {name}", "Container limits {name}"),
        // ostatní: název varianty
        other => format!("{other:?}").split(['(', ' ', '{']).next().unwrap_or("").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::Caps;

    #[test]
    fn df_due_decisions() {
        let t = Instant::now();
        let every = Duration::from_secs(30);
        assert!(df_due(None, t, every));
        assert!(!df_due(Some(t), t + Duration::from_secs(3), every));
        assert!(!df_due(Some(t), t + Duration::from_millis(29_999), every));
        assert!(df_due(Some(t), t + every, every));
        assert!(df_due(Some(t), t + Duration::from_secs(90), every));
        // čas „před“ posledním voláním nesmí způsobit panic
        assert!(!df_due(Some(t + every), t, every));
    }

    #[test]
    fn need_wsl_list_decisions() {
        // viditelné okno: vždy
        assert!(need_wsl_list(true, true, true));
        assert!(need_wsl_list(true, false, false));
        // skryté okno a API odpovídá s mezipamětí: ne
        assert!(!need_wsl_list(false, true, true));
        // skryté okno, ale odpojeno nebo bez mezipaměti: ano
        assert!(need_wsl_list(false, false, true));
        assert!(need_wsl_list(false, true, false));
    }

    /// Akce jen pro WSL (skripty z engine.rs, wsl.exe, Windows programy) mimo WSL neprojdou;
    /// na WSL projde vše.
    #[test]
    fn wsl_only_commands_refused_locally() {
        let wsl_only = [
            Cmd::SetupEngine,
            Cmd::InstallEngine(Engine::Docker, true),
            Cmd::UninstallEngine { engine: Engine::Podman, purge: false },
            Cmd::SetTls(true),
            Cmd::RegenCerts,
            Cmd::LoadDaemonJson,
            Cmd::SaveDaemonJson("{}".into()),
            Cmd::FreeMemory,
            Cmd::Sleep,
            Cmd::Wsl(WslCmd::Shutdown),
            Cmd::ListOnline,
            Cmd::SetPrimary { distro: "x".into(), engine: None, sleep_old: false },
            Cmd::SetKeepAlive(true),
            Cmd::Notepad("/x".into()),
            Cmd::Explorer("/x".into()),
            Cmd::Fs { root: true, op: FsOp::List("/".into()) },
        ];
        for c in &wsl_only {
            assert!(!supported(Caps::LOCAL, c), "{c:?}");
            assert!(supported(Caps::WSL, c), "{c:?}");
        }
        for c in [Cmd::EngineStart, Cmd::EngineStop, Cmd::EngineRestart, Cmd::Refresh, Cmd::Fs { root: false, op: FsOp::List("/".into()) }] {
            assert!(supported(Caps::LOCAL, &c), "{c:?}");
        }
    }
}
