//! Práce na pozadí: periodické obnovování stavu a provádění akcí,
//! aby UI vlákno nikdy neblokovalo.

use crate::config::Config;
use crate::engine::{qualify_image, Engine};
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

#[derive(Debug, Clone, Default)]
pub struct RunSpec {
    pub image: String,
    pub name: String,
    pub ports: String,
    pub env: String,
    pub volumes: String,
    pub network: String,
    pub restart: String,
    pub command: String,
    /// spustit interaktivně ve Windows Terminalu (`run -it --rm`) místo na pozadí
    pub interactive: bool,
}

impl RunSpec {
    /// Náhled příkazu (vždy jako `docker run …`).
    pub fn args(&self) -> Vec<String> {
        self.args_with(vec!["docker".into()], false)
    }

    /// Skutečný příkaz pro daný engine (Podman: plně kvalifikovaný image).
    pub fn args_for(&self, engine: Engine, port: u16) -> Vec<String> {
        self.args_with(engine.cli(port), engine == Engine::Podman)
    }

    fn args_with(&self, cli: Vec<String>, qualify: bool) -> Vec<String> {
        let mut a: Vec<String> = cli;
        if self.interactive {
            a.extend(["run".into(), "-it".into(), "--rm".into()]);
        } else {
            a.extend(["run".into(), "-d".into()]);
        }
        if !self.name.trim().is_empty() {
            a.extend(["--name".into(), self.name.trim().into()]);
        }
        for p in self.ports.split([',', ' ', '\n']).map(str::trim).filter(|s| !s.is_empty()) {
            a.extend(["-p".into(), p.into()]);
        }
        for e in self.env.lines().map(str::trim).filter(|s| !s.is_empty()) {
            a.extend(["-e".into(), e.into()]);
        }
        for v in self.volumes.lines().map(str::trim).filter(|s| !s.is_empty()) {
            a.extend(["-v".into(), v.into()]);
        }
        if !self.network.trim().is_empty() {
            a.extend(["--network".into(), self.network.trim().into()]);
        }
        if !self.restart.is_empty() && self.restart != "no" {
            a.extend(["--restart".into(), self.restart.clone()]);
        }
        a.push(if qualify { qualify_image(&self.image) } else { self.image.trim().into() });
        a.extend(self.command.split_whitespace().map(String::from));
        a
    }
}

/// Parametry přidání nové distribuce.
#[derive(Debug, Clone, Default)]
pub struct AddDistroSpec {
    /// název z `wsl --list --online` (např. Debian)
    pub distro: String,
    /// název nové instance (prázdné = stejný jako distro)
    pub name: String,
    /// výchozí uživatel (sudo bez hesla)
    pub user: String,
    /// volitelné heslo uživatele
    pub password: String,
    /// engine k instalaci (None = žádný)
    pub engine: Option<Engine>,
    /// přepnout aplikaci na novou distribuci (a nastavit API enginu)
    pub use_for_app: bool,
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

/// Uložený seznam kontejnerů, které naposledy běžely – pro každou distribuci a engine zvlášť.
mod last_running {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn path() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("WLS_Desktop").join("last_running.json"))
    }

    fn load_all() -> BTreeMap<String, Vec<String>> {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn key(distro: &str, engine: &str) -> String {
        format!("{distro}/{engine}")
    }

    pub fn load(key: &str) -> Vec<String> {
        load_all().remove(key).unwrap_or_default()
    }

    pub fn save(key: &str, ids: &[String]) {
        let mut all = load_all();
        all.insert(key.to_string(), ids.to_vec());
        if let Some(p) = path() {
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(p, serde_json::to_string_pretty(&all).unwrap_or_default());
        }
    }
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

    /// Pamatuje si běžící kontejnery; po novém připojení k enginu je případně znovu spustí.
    fn track_running(&self, snap: &Snapshot) {
        let cfg = self.cfg();
        let key = last_running::key(&cfg.distro, cfg.engine.name());
        if !snap.connected {
            self.shared.was_connected.store(false, Ordering::SeqCst);
            return;
        }
        let first = !self.shared.was_connected.swap(true, Ordering::SeqCst);
        if first {
            // nové připojení: nejdřív obnovit, ukládat až při dalším obnovení
            self.shared.suppress_track.store(false, Ordering::SeqCst);
            if let Ok(mut g) = self.shared.last_tracked.lock() {
                *g = last_running::load(&key);
            }
            if cfg.restore_containers {
                let saved = last_running::load(&key);
                let to_start: Vec<String> = saved
                    .into_iter()
                    .filter(|id| snap.containers.iter().any(|c| &c.id == id && !c.running() && !c.paused()))
                    .collect();
                if !to_start.is_empty() {
                    let w = self.clone();
                    thread::spawn(move || {
                        let d = w.docker();
                        let ok = to_start.iter().filter(|id| d.container_action(id, "start").is_ok()).count();
                        w.send(Msg::Toast(true, tf!("Obnoveno kontejnerů: {ok}/{}", "Containers restored: {ok}/{}", to_start.len())));
                        let _ = w.cmd.send(Cmd::Refresh);
                    });
                }
            }
            return;
        }
        if self.shared.suppress_track.load(Ordering::SeqCst) {
            return;
        }
        let mut ids: Vec<String> = snap.containers.iter().filter(|c| c.running()).map(|c| c.id.clone()).collect();
        ids.sort();
        if let Ok(mut g) = self.shared.last_tracked.lock() {
            if *g != ids {
                last_running::save(&key, &ids);
                *g = ids;
            }
        }
    }

    /// Aplikace sama vypíná engine – seznam běžících kontejnerů nepřepisovat.
    fn suppress_tracking(&self) {
        self.shared.suppress_track.store(true, Ordering::SeqCst);
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

    fn ensure_keepalive(&self, distro: &str) -> Result<(), String> {
        let mut g = self.shared.keepalive.lock().map_err(|e| e.to_string())?;
        if let Some((c, started)) = g.as_mut() {
            if matches!(c.try_wait(), Ok(None)) {
                return Ok(());
            }
            // proces skončil (typicky zámek drží jiná instance) – nezkoušet znovu hned
            if started.elapsed() < Duration::from_secs(60) {
                return Ok(());
            }
        }
        *g = Some((wsl::keepalive(distro)?, Instant::now()));
        Ok(())
    }

    /// Ukončí udržovací proces – náš i případný z jiné instance aplikace.
    fn kill_keepalive_all(&self, distro: &str) {
        self.kill_keepalive();
        let _ = wsl::run(distro, Some("root"), None, &["sh", "-c", "pkill -f 'flock -n /tmp/.wls-desktop-keepalive.lock' || true"]);
    }

    fn kill_keepalive(&self) {
        if let Ok(mut g) = self.shared.keepalive.lock() {
            if let Some((mut c, _)) = g.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
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

    /// Spustí skript, který zpřístupní API enginu na TCP, a počká na odezvu.
    /// Vrací (výstup skriptu, API odpovídá).
    fn setup_api(&self, cfg: &Config) -> Result<(String, bool), String> {
        let port = cfg.port.to_string();
        let out = wsl::run(&cfg.distro, Some("root"), None, &["env", &format!("PORT={port}"), "sh", "-c", cfg.engine.setup_script()]);
        self.check_engine();
        let out = out?;
        if self.wait_ping(25) {
            Ok((out, true))
        } else {
            Err(tf!(
                "{out}\nAPI zatím neodpovídá z Windows – zkontroluj networkingMode / localhostForwarding.",
                "{out}\nThe API is not responding from Windows yet – check networkingMode / localhostForwarding."
            ))
        }
    }

    /// Nainstaluje engine, zapne systemd (s restartem distribuce), nastaví API.
    fn install_engine(&self, engine: Engine, log: &mut String) -> Result<String, String> {
        let mut cfg = self.cfg();
        let distro = cfg.distro.clone();
        let step = |log: &mut String, s: &str| {
            log.push_str(&format!("\n=== {s} ===\n"));
        };

        // 1) instalace balíčků
        step(log, &tf!("Instalace {}", "Installing {}", engine.name()));
        self.send(Msg::Toast(
            true,
            tf!("Instaluji {} do {distro}… (může trvat několik minut)", "Installing {} into {distro}… (may take several minutes)", engine.name()),
        ));
        match wsl::run(&distro, Some("root"), None, &["sh", "-c", &engine.install_script()]) {
            Ok(o) => log.push_str(&o),
            Err(e) => {
                log.push_str(&e);
                return Err(tf!("Instalace {} selhala – viz výstup", "Installation of {} failed – see output", engine.name()));
            }
        }

        // 2) systemd
        step(log, t!("Kontrola systemd", "Checking systemd"));
        if wsl::run(&distro, Some("root"), None, &["test", "-d", "/run/systemd/system"]).is_err() {
            log.push_str(t!(
                "systemd neběží – zapínám v /etc/wsl.conf a restartuji distribuci\n",
                "systemd is not running – enabling it in /etc/wsl.conf and restarting the distribution\n"
            ));
            let script = "grep -q '^systemd=true' /etc/wsl.conf 2>/dev/null || printf '\\n[boot]\\nsystemd=true\\n' >> /etc/wsl.conf";
            wsl::run(&distro, Some("root"), None, &["sh", "-c", script]).map_err(|e| {
                log.push_str(&e);
                t!("Nelze upravit /etc/wsl.conf", "Cannot modify /etc/wsl.conf").to_string()
            })?;
            self.kill_keepalive();
            let _ = wsl::terminate(&distro);
            thread::sleep(Duration::from_secs(3));
            if cfg.keep_alive {
                self.ensure_keepalive(&distro)?;
            } else {
                wsl::start(&distro)?;
            }
            thread::sleep(Duration::from_secs(3));
            if wsl::run(&distro, Some("root"), None, &["test", "-d", "/run/systemd/system"]).is_err() {
                log.push_str(t!(
                    "systemd stále neběží – zkus „Vypnout celé WSL“ a znovu.\n",
                    "systemd is still not running – try “Shut down all of WSL” and retry.\n"
                ));
                return Err(t!("systemd se nepodařilo zapnout", "Failed to enable systemd").into());
            }
            log.push_str(t!("systemd běží\n", "systemd is running\n"));
        } else {
            log.push_str(t!("systemd běží\n", "systemd is running\n"));
        }

        // 3) přepnout engine v konfiguraci a nastavit API
        cfg.engine = engine;
        if let Ok(mut g) = self.cfg.write() {
            g.engine = engine;
            let _ = g.save();
        }
        self.shared.tcp_checked.store(false, Ordering::SeqCst);
        step(log, t!("Nastavení TCP API", "Setting up the TCP API"));
        match self.setup_api(&cfg) {
            Ok((o, _)) => {
                log.push_str(&o);
                log.push_str(t!("\nAPI odpovídá – hotovo.\n", "\nThe API is responding – done.\n"));
                Ok(tf!("{} nainstalován a nastaven", "{} installed and configured", engine.name()))
            }
            Err(e) => {
                log.push_str(&e);
                Err(t!(
                    "Engine je nainstalovaný, ale API neodpovídá – viz výstup",
                    "The engine is installed, but the API is not responding – see output"
                )
                .into())
            }
        }
    }

    /// Nainstaluje novou WSL distribuci a připraví ji (systemd, uživatel, volitelně engine).
    fn add_distro(&self, spec: AddDistroSpec, log: &mut String) -> Result<String, String> {
        let name = if spec.name.trim().is_empty() { spec.distro.clone() } else { spec.name.trim().to_string() };
        let step = |log: &mut String, s: &str| log.push_str(&format!("\n=== {s} ===\n"));
        if wsl::list().map(|l| l.iter().any(|d| d.name.eq_ignore_ascii_case(&name))).unwrap_or(false) {
            return Err(tf!("Distribuce {name} už existuje – zvol jiný název", "Distribution {name} already exists – choose a different name"));
        }

        // 1) instalace
        step(log, &tf!("Instalace {} jako {name}", "Installing {} as {name}", spec.distro));
        self.send(Msg::Toast(
            true,
            tf!("Instaluji distribuci {name}… (stahování může trvat několik minut)", "Installing distribution {name}… (the download may take several minutes)"),
        ));
        match wsl::install_distro(&spec.distro, &name) {
            Ok(o) => log.push_str(&o),
            Err(e) => {
                log.push_str(&e);
                return Err(tf!("Instalace distribuce {name} selhala – viz výstup", "Installation of distribution {name} failed – see output"));
            }
        }

        // 2) systemd, uživatel, sudo
        step(log, t!("Příprava (systemd, uživatel, sudo)", "Preparation (systemd, user, sudo)"));
        let user = spec.user.trim().to_string();
        match wsl::run(&name, Some("root"), None, &["env", &format!("U={user}"), "sh", "-c", &crate::engine::distro_prepare_script()]) {
            Ok(o) => log.push_str(&o),
            Err(e) => {
                log.push_str(&e);
                return Err(t!("Příprava distribuce selhala – viz výstup", "Distribution preparation failed – see output").into());
            }
        }
        if !user.is_empty() && !spec.password.is_empty() {
            match wsl::run_stdin(&name, Some("root"), &["chpasswd"], &format!("{user}:{}\n", spec.password)) {
                Ok(_) => log.push_str(t!("Heslo nastaveno\n", "Password set\n")),
                Err(e) => log.push_str(&tf!("Heslo se nepodařilo nastavit: {e}\n", "Failed to set the password: {e}\n")),
            }
        }

        // 3) restart kvůli systemd a výchozímu uživateli
        step(log, t!("Restart distribuce", "Restarting the distribution"));
        let _ = wsl::terminate(&name);
        thread::sleep(Duration::from_secs(3));
        wsl::start(&name)?;
        thread::sleep(Duration::from_secs(3));
        let systemd = wsl::run(&name, Some("root"), None, &["test", "-d", "/run/systemd/system"]).is_ok();
        log.push_str(if systemd {
            t!("systemd běží\n", "systemd is running\n")
        } else {
            t!("systemd zatím neběží – může pomoci „Vypnout celé WSL“\n", "systemd is not running yet – “Shut down all of WSL” may help\n")
        });

        // 4) přepnout aplikaci a/nebo nainstalovat engine
        if spec.use_for_app {
            step(log, t!("Přepnutí aplikace na novou distribuci", "Switching the app to the new distribution"));
            self.kill_keepalive();
            if let Ok(mut g) = self.cfg.write() {
                g.distro = name.clone();
                if let Some(e) = spec.engine {
                    g.engine = e;
                }
                let _ = g.save();
            }
            self.shared.tcp_checked.store(false, Ordering::SeqCst);
            self.send(Msg::ConfigChanged);
            if let Some(e) = spec.engine {
                return self.install_engine(e, log).map(|_| {
                    tf!(
                        "Distribuce {name} je připravená s {} a aplikace ji používá",
                        "Distribution {name} is ready with {} and the app is using it",
                        e.name()
                    )
                });
            }
            return Ok(tf!(
                "Distribuce {name} je připravená a aplikace ji používá – nainstaluj engine na stránce WSL engine",
                "Distribution {name} is ready and the app is using it – install an engine on the WSL engine page"
            ));
        }
        if let Some(e) = spec.engine {
            step(log, &tf!("Instalace {} (bez nastavení API)", "Installing {} (without API setup)", e.name()));
            match wsl::run(&name, Some("root"), None, &["sh", "-c", &e.install_script()]) {
                Ok(o) => log.push_str(&o),
                Err(err) => {
                    log.push_str(&err);
                    return Err(tf!("Instalace {} selhala – viz výstup", "Installation of {} failed – see output", e.name()));
                }
            }
        }
        Ok(tf!("Distribuce {name} je nainstalovaná a připravená", "Distribution {name} is installed and ready"))
    }

    fn exec_inner(&self, cmd: Cmd) -> Result<String, String> {
        let cfg = self.cfg();
        let distro = cfg.distro.clone();
        let d = self.docker();
        match cmd {
            Cmd::Refresh => Ok(String::new()),
            Cmd::Fs { root, op } => {
                use crate::fs;
                let changed = |r: Result<String, String>, ok: String| {
                    self.send(Msg::FsChanged);
                    r.map(|_| ok)
                };
                match op {
                    FsOp::List(p) => {
                        match fs::list(&distro, root, &p) {
                            Ok((cwd, entries)) => self.send(Msg::FsListing(cwd, entries)),
                            Err(e) => self.send(Msg::FsListError(p, e)),
                        }
                        Ok(String::new())
                    }
                    FsOp::Mkdir(p) => changed(fs::mkdir(&distro, root, &p), t!("Složka vytvořena", "Folder created").into()),
                    FsOp::Touch(p) => changed(fs::touch(&distro, root, &p), t!("Soubor vytvořen", "File created").into()),
                    FsOp::Rename(a, b) => changed(fs::rename(&distro, root, &a, &b), t!("Přejmenováno", "Renamed").into()),
                    FsOp::Delete(p) => {
                        let n = p.len();
                        changed(fs::delete(&distro, root, &p), tf!("Smazáno položek: {n}", "Items deleted: {n}"))
                    }
                    FsOp::Chmod(m, p) => changed(fs::chmod(&distro, root, &m, &p), tf!("Práva nastavena na {m}", "Permissions set to {m}")),
                    FsOp::Upload(win, dir) => {
                        let mut src = Vec::new();
                        for w in &win {
                            src.push(wsl::to_linux_path(&distro, w)?);
                        }
                        let n = src.len();
                        changed(fs::copy(&distro, root, &src, &dir), tf!("Nahráno položek: {n}", "Items uploaded: {n}"))
                    }
                    FsOp::Download(items, win_dir) => {
                        let dst = wsl::to_linux_path(&distro, &win_dir)?;
                        let n = items.len();
                        fs::copy(&distro, root, &items, &dst).map(|_| tf!("Staženo {n} položek do {win_dir}", "Downloaded {n} items to {win_dir}"))
                    }
                    FsOp::Read(p) => {
                        let t = fs::read_text(&distro, root, &p)?;
                        self.send(Msg::FsFile(p, t));
                        Ok(String::new())
                    }
                    FsOp::Write(p, t) => changed(fs::write_text(&distro, root, &p, &t), tf!("Uloženo: {p}", "Saved: {p}")),
                }
            }
            Cmd::Container(id, action) => d.container_action(&id, action).map(|_| String::new()),
            Cmd::PruneContainers => d.prune("containers", None),
            Cmd::Run(spec) => {
                let image = crate::engine::short_image(spec.image.trim());
                let args = spec.args_for(cfg.engine, cfg.port);
                if spec.interactive {
                    // interaktivně ve Windows Terminalu – kontejner se po ukončení smaže (--rm)
                    let mut w: Vec<String> = vec!["-d".into(), distro.clone(), "-e".into()];
                    w.extend(args);
                    wsl::open_terminal(&tf!("{image} (interaktivně)", "{image} (interactive)"), &w)?;
                    return Ok(tf!("{image} spuštěn interaktivně v terminálu", "{image} started interactively in a terminal"));
                }
                let out = wsl::run_owned(&distro, None, None, &args)?;
                // ID je poslední řádek výstupu (před ním může být průběh stahování)
                let id = out.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("").to_string();
                // chvíli počkat a zkontrolovat, jestli kontejner neskončil hned po startu
                thread::sleep(Duration::from_millis(1800));
                let info: Option<serde_json::Value> = d.get(&format!("/containers/{id}/json")).ok();
                let name = info
                    .as_ref()
                    .and_then(|v| v.get("Name").and_then(|n| n.as_str()))
                    .map(|n| n.trim_start_matches('/').to_string())
                    .unwrap_or_else(|| crate::docker::short_id(&id).to_string());
                let running = info.as_ref().and_then(|v| v.pointer("/State/Running").and_then(|r| r.as_bool())).unwrap_or(true);
                if running {
                    Ok(tf!("Spuštěn kontejner {name} z {image}", "Started container {name} from {image}"))
                } else {
                    let code = info.as_ref().and_then(|v| v.pointer("/State/ExitCode").and_then(|c| c.as_i64())).unwrap_or(0);
                    Err(tf!(
                        "Kontejner {name} z {image} se hned ukončil (kód {code}). Image asi nemá dlouhodobě běžící proces – \
                         zadej příkaz (např. sleep infinity) nebo zaškrtni „Interaktivně v terminálu“. Výstup najdeš v logu kontejneru.",
                        "Container {name} from {image} exited immediately (code {code}). The image probably has no long-running process – \
                         enter a command (e.g. sleep infinity) or check “Interactive in terminal”. See the container log for output."
                    ))
                }
            }
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
            Cmd::Compose { dir, project, args } => {
                let mut a: Vec<String> = cfg.engine.cli(cfg.port);
                a.push("compose".into());
                if let Some(p) = &project {
                    a.extend(["-p".into(), p.clone()]);
                }
                a.extend(args.iter().cloned());
                let title = format!("{} compose {}", cfg.engine.name().to_lowercase(), args.join(" "));
                let r = wsl::run_owned(&distro, None, Some(&dir), &a);
                match r {
                    Ok(out) => {
                        self.send(Msg::Output(title, out));
                        Ok(t!("Compose: hotovo", "Compose: done").into())
                    }
                    Err(e) => {
                        self.send(Msg::Output(title, e.clone()));
                        Err(t!("Compose selhal – viz výstup", "Compose failed – see output").into())
                    }
                }
            }
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
            Cmd::EngineStart => {
                if cfg.keep_alive {
                    self.ensure_keepalive(&distro)?;
                } else {
                    wsl::start(&distro)?;
                }
                let name = cfg.engine.name();
                let mut a = vec!["systemctl", "start"];
                a.extend(cfg.engine.services());
                wsl::run(&distro, Some("root"), None, &a)?;
                if self.wait_ping(20) {
                    Ok(tf!("{name} běží", "{name} is running"))
                } else {
                    self.check_engine();
                    Err(tf!(
                        "{name} se nespustil nebo API na 127.0.0.1:{} neodpovídá. Použij „Nastavit jen API“ nebo „Nainstalovat a nastavit“ na stránce WSL engine.",
                        "{name} did not start or the API on 127.0.0.1:{} is not responding. Use “Set up API only” or “Install and set up” on the WSL engine page.",
                        cfg.port
                    ))
                }
            }
            Cmd::EngineStop => {
                self.suppress_tracking();
                let mut a = vec!["systemctl", "stop"];
                a.extend(cfg.engine.stop_units());
                wsl::run(&distro, Some("root"), None, &a)?;
                Ok(tf!("{} zastaven", "{} stopped", cfg.engine.name()))
            }
            Cmd::EngineRestart => {
                self.suppress_tracking();
                let mut a = vec!["systemctl", "restart"];
                a.extend(cfg.engine.services());
                wsl::run(&distro, Some("root"), None, &a)?;
                if self.wait_ping(20) {
                    // další obnovení bere jako nové připojení (obnova kontejnerů + zapnutí sledování)
                    self.shared.was_connected.store(false, Ordering::SeqCst);
                    Ok(tf!("{} restartován", "{} restarted", cfg.engine.name()))
                } else {
                    Err(tf!("{} po restartu neodpovídá", "{} is not responding after restart", cfg.engine.name()))
                }
            }
            Cmd::SetupEngine => self.setup_api(&cfg).map(|(out, _)| out),
            Cmd::UninstallEngine { engine, purge } => {
                self.send(Msg::Toast(true, tf!("Odinstalovávám {}…", "Uninstalling {}…", engine.name())));
                let r = wsl::run(
                    &distro,
                    Some("root"),
                    None,
                    &["env", &format!("PURGE={}", if purge { 1 } else { 0 }), "sh", "-c", engine.uninstall_script()],
                );
                self.shared.tcp_checked.store(false, Ordering::SeqCst);
                self.check_engine();
                match r {
                    Ok(o) => {
                        self.send(Msg::Output(tf!("Odinstalace: {}", "Uninstall: {}", engine.name()), o));
                        Ok(tf!(
                            "{} odinstalován{}",
                            "{} uninstalled{}",
                            engine.name(),
                            if purge { t!(" i s daty", " including data") } else { "" }
                        ))
                    }
                    Err(e) => {
                        self.send(Msg::Output(tf!("Odinstalace: {}", "Uninstall: {}", engine.name()), e));
                        Err(t!("Odinstalace selhala – viz výstup", "Uninstall failed – see output").into())
                    }
                }
            }
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
            Cmd::Sleep => {
                self.suppress_tracking();
                self.kill_keepalive();
                wsl::terminate(&distro)?;
                Ok(tf!(
                    "{distro} uspána (ukončena). Probudíš ji tlačítkem Spustit.",
                    "{distro} put to sleep (terminated). Wake it up with the Start button."
                ))
            }
            Cmd::SetKeepAlive(on) => {
                if on {
                    self.ensure_keepalive(&distro)?;
                    Ok(t!("Udržování distribuce zapnuto", "Keep-alive for the distribution enabled").into())
                } else {
                    self.kill_keepalive_all(&distro);
                    Ok(t!(
                        "Udržování distribuce vypnuto – WSL se může po nečinnosti zastavit",
                        "Keep-alive for the distribution disabled – WSL may stop after inactivity"
                    )
                    .into())
                }
            }
            Cmd::PauseAll | Cmd::UnpauseAll => {
                let pause = matches!(cmd, Cmd::PauseAll);
                let snap = d.snapshot();
                let mut n = 0;
                for c in snap.containers.iter().filter(|c| if pause { c.running() } else { c.paused() }) {
                    d.container_action(&c.id, if pause { "pause" } else { "unpause" })?;
                    n += 1;
                }
                Ok(if pause {
                    tf!("Pozastaveno kontejnerů: {n}", "Containers paused: {n}")
                } else {
                    tf!("Obnoveno kontejnerů: {n}", "Containers resumed: {n}")
                })
            }
            Cmd::LoadDaemonJson => {
                let t = wsl::run(&distro, Some("root"), None, &["sh", "-c", "cat /etc/docker/daemon.json 2>/dev/null || echo '{}'"])?;
                self.send(Msg::DaemonJson(t));
                Ok(String::new())
            }
            Cmd::SaveDaemonJson(text) => {
                let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| tf!("Neplatný JSON: {e}", "Invalid JSON: {e}"))?;
                if v.get("hosts").is_some() {
                    return Err(t!(
                        "daemon.json nesmí obsahovat \"hosts\" – koliduje s -H v systemd. Port se nastavuje v Nastavení.",
                        "daemon.json must not contain \"hosts\" – it conflicts with -H in systemd. The port is set in Settings."
                    )
                    .into());
                }
                let pretty = serde_json::to_string_pretty(&v).unwrap_or(text);
                wsl::run_stdin(&distro, Some("root"), &["sh", "-c", "mkdir -p /etc/docker && cat > /etc/docker/daemon.json"], &pretty)?;
                wsl::run(&distro, Some("root"), None, &["systemctl", "restart", "docker"])?;
                if self.wait_ping(20) {
                    Ok(t!("daemon.json uložen, Docker restartován", "daemon.json saved, Docker restarted").into())
                } else {
                    Err(t!(
                        "Uloženo, ale Docker po restartu neodpovídá – zkontroluj daemon.json",
                        "Saved, but Docker is not responding after restart – check daemon.json"
                    )
                    .into())
                }
            }
            Cmd::Terminal(t) => {
                let shell = "command -v bash >/dev/null && exec bash || exec sh".to_string();
                let cli = cfg.engine.cli(cfg.port);
                let with_cli = |rest: Vec<String>| {
                    let mut v: Vec<String> = vec!["-d".into(), distro.clone(), "-e".into()];
                    v.extend(cli.iter().cloned());
                    v.extend(rest);
                    v
                };
                let (title, args): (String, Vec<String>) = match t {
                    Term::Container { id, name } => (
                        name,
                        with_cli(vec!["exec".into(), "-it".into(), id, "sh".into(), "-c".into(), shell]),
                    ),
                    Term::Logs { id, name } => (
                        format!("logs {name}"),
                        with_cli(vec!["logs".into(), "-f".into(), "--tail".into(), "300".into(), id]),
                    ),
                    Term::Distro(n) => (n.clone(), vec!["-d".into(), n, "--cd".into(), "~".into()]),
                    Term::Dir(dir) => (dir.clone(), vec!["-d".into(), distro, "--cd".into(), dir]),
                };
                wsl::open_terminal(&title, &args)
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_spec_args() {
        let s = RunSpec {
            image: "nginx:alpine".into(),
            name: "web".into(),
            ports: "8080:80, 8443:443".into(),
            env: "A=1\nB=2".into(),
            volumes: "data:/data".into(),
            network: "".into(),
            restart: "unless-stopped".into(),
            command: "".into(),
            interactive: false,
        };
        assert_eq!(
            s.args().join(" "),
            "docker run -d --name web -p 8080:80 -p 8443:443 -e A=1 -e B=2 -v data:/data --restart unless-stopped nginx:alpine"
        );
    }
}
