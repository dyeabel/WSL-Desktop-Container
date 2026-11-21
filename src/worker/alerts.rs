//! Upozornění na prahové hodnoty: paměť WSL (VmmemWSL vůči limitu), nedostatek dostupné
//! paměti v Linuxu (MemAvailable) a místo na disku (uvnitř distribuce, disk Windows s VHDX,
//! data enginu a jeho úložiště).
//!
//! Čistá logika (pravidla, časové okno, hystereze, omezení opakování) je v `Alerts` a dá se
//! testovat bez WSL. Data sbírá worker: paměť při každém vzorku paměti (`memory_sample`),
//! disk nejvýš jednou za `DISK_EVERY`. Doručení jde přes `Sink` – ve workeru toast Windows,
//! v testech sběrač zpráv.

use super::Worker;
use crate::config::{AlertsConfig, Config};
use crate::memory::MemInfo;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// Disk se kontroluje nejvýš jednou za tuto dobu.
pub const DISK_EVERY: Duration = Duration::from_secs(300);
/// Hystereze: upozornění se znovu „nabije“ až po návratu o tolik procentních bodů pod práh.
const HYST_PCT: f64 = 5.0;
/// Hystereze u absolutních velikostí (GB): 5 % prahu.
const HYST_REL: f64 = 0.05;
const GB: f64 = 1024.0 * 1024.0 * 1024.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// VmmemWSL vůči limitu WSL
    Memory,
    /// MemAvailable v Linuxu
    Pressure,
    /// volné místo uvnitř distribuce (`df /`)
    DistroDisk,
    /// volné místo na disku Windows, kde leží VHDX
    WinDrive,
    /// volné místo pro data enginu (`df /var/lib/containers`)
    EngineFree,
    /// velikost image, kontejnerů, volume a build cache
    EngineSize,
}

impl Kind {
    pub const ALL: [Kind; 6] = [Kind::Memory, Kind::Pressure, Kind::DistroDisk, Kind::WinDrive, Kind::EngineFree, Kind::EngineSize];

    fn idx(self) -> usize {
        self as usize
    }

    /// Kam vede kliknutí na oznámení.
    pub fn target(self) -> Target {
        match self {
            Kind::Memory | Kind::Pressure => Target::Memory,
            _ => Target::Engine,
        }
    }

    pub fn enabled(self, c: &AlertsConfig) -> bool {
        match self {
            Kind::Memory => c.memory,
            Kind::Pressure => c.pressure,
            Kind::DistroDisk => c.distro_disk,
            Kind::WinDrive => c.win_drive,
            Kind::EngineFree => c.engine_free,
            Kind::EngineSize => c.engine_size,
        }
    }
}

/// Cíl navigace po kliknutí na oznámení.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Přehled, sekce Paměť WSL
    Memory,
    /// stránka WSL engine (disk distribuce)
    Engine,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub kind: Kind,
    pub title: String,
    pub body: String,
}

// ---------------------------------------------------------------- vstupy

/// Data pro pravidla paměti (bajty).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MemInput {
    /// pracovní sada VmmemWSL (None = nezjištěno / VM neběží)
    pub vmmem: Option<u64>,
    /// limit WSL (0 = neznámý)
    pub limit: u64,
    /// MemTotal v Linuxu (0 = neznámé)
    pub total: u64,
    /// MemAvailable v Linuxu (0 = chybí)
    pub available: u64,
    /// souborová cache v Linuxu
    pub cache: u64,
}

/// Vstup pro pravidla paměti z už nasbíraného vzorku; limit z .wslconfig (nebo 50 % RAM).
pub fn mem_input(mi: &MemInfo, vmmem: Option<u64>) -> MemInput {
    let ram = crate::memory::total_ram();
    let limit = if ram > 0 { crate::memory::limit_from(&crate::wslconfig::read(), ram).0 } else { 0 };
    MemInput { vmmem, limit, total: mi.total, available: mi.available, cache: mi.cache() }
}

/// Jeden řádek `df -P -B1` (bajty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DfRow {
    pub size: u64,
    pub used: u64,
    pub avail: u64,
    pub mount: String,
}

impl DfRow {
    /// Volné místo v % (jako `df`: vůči used + avail, bez rezervy pro roota).
    pub fn free_pct(&self) -> f64 {
        let base = self.used + self.avail;
        if base == 0 { 100.0 } else { self.avail as f64 * 100.0 / base as f64 }
    }
}

/// Řádky `df -P -B1` (hlavička a nesmysly se přeskočí). Čísla se hledají jako trojice
/// za názvem zařízení, takže nevadí mezery v názvu ani v přípojném bodu.
pub fn parse_df(out: &str) -> Vec<DfRow> {
    out.lines()
        .filter_map(|l| {
            let t: Vec<&str> = l.split_whitespace().collect();
            (1..t.len().saturating_sub(4)).find_map(|i| {
                let (size, used, avail) = (t[i].parse().ok()?, t[i + 1].parse().ok()?, t[i + 2].parse().ok()?);
                t[i + 3].ends_with('%').then(|| DfRow { size, used, avail, mount: t[i + 4..].join(" ") })
            })
        })
        .collect()
}

/// Volné místo na disku Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveSpace {
    /// popisek (např. `C:`)
    pub name: String,
    /// volné pro aktuálního uživatele
    pub free: u64,
    pub total: u64,
}

impl DriveSpace {
    pub fn free_pct(&self) -> f64 {
        if self.total == 0 { 100.0 } else { self.free as f64 * 100.0 / self.total as f64 }
    }
}

/// Volné a celkové místo na disku, kde leží `path` (GetDiskFreeSpaceExW).
#[cfg(windows)]
pub fn drive_space(path: &std::path::Path) -> Option<DriveSpace> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let w: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: řetězec je ukončený nulou, výstupní ukazatele míří na platné u64.
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) } != 0;
    let s = path.to_string_lossy();
    let name = s.strip_prefix(r"\\?\").unwrap_or(&s).chars().take(2).collect::<String>();
    (ok && total > 0).then_some(DriveSpace { name, free: avail, total })
}

#[cfg(not(windows))]
pub fn drive_space(_: &std::path::Path) -> Option<DriveSpace> {
    None
}

/// Data pro pravidla disku.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiskInput {
    pub distro: String,
    /// cesta a velikost souboru VHDX
    pub vhdx: Option<(String, u64)>,
    /// `df /` v distribuci
    pub inside: Option<DfRow>,
    /// disk Windows, na kterém leží VHDX
    pub drive: Option<DriveSpace>,
    /// složka s daty enginu (/var/lib/containers, /var/lib/docker)
    pub engine_path: String,
    /// `df <engine_path>` (None = složka neexistuje)
    pub engine_free: Option<DfRow>,
    /// image + kontejnery + volume + build cache (`/system/df`)
    pub engine_used: Option<u64>,
}

/// Složka s daty enginu v distribuci.
pub fn engine_dir(e: crate::engine::Engine) -> &'static str {
    match e {
        crate::engine::Engine::Docker => "/var/lib/docker",
        crate::engine::Engine::Podman => "/var/lib/containers",
    }
}

/// Příkaz pro `sh -c`: `df /` a `df <dir>` oddělené `@@`.
fn df_script(dir: &str) -> String {
    format!("df -P -B1 / 2>/dev/null; echo @@; df -P -B1 '{dir}' 2>/dev/null; true")
}

/// Rozdělí výstup `df_script` na (`/`, složka enginu).
pub fn parse_df_pair(out: &str) -> (Option<DfRow>, Option<DfRow>) {
    let (a, b) = out.split_once("@@").unwrap_or((out, ""));
    (parse_df(a).into_iter().next(), parse_df(b).into_iter().next())
}

/// Nasbírá data pro pravidla disku (jen čtení). `docker` = připojený engine (pro `/system/df`,
/// volá se jen se zapnutým upozorněním na velikost úložiště).
pub fn collect_disk(cfg: &Config, docker: Option<&crate::docker::Docker>) -> DiskInput {
    let distro = cfg.distro.clone();
    let engine_path = engine_dir(cfg.engine).to_string();
    let vhd = crate::wsl::vhd_path(&distro);
    let vhdx = vhd.as_ref().and_then(|p| std::fs::metadata(p).ok().map(|m| (p.display().to_string(), m.len())));
    let drive = vhd.as_ref().and_then(|p| p.parent()).and_then(drive_space);
    let (inside, engine_free) = crate::host::HostExt::run(&*crate::host::current(cfg), crate::host::Privilege::User, None, &["sh", "-c", &df_script(&engine_path)])
        .map(|o| parse_df_pair(&o))
        .unwrap_or((None, None));
    let engine_used = if cfg.alerts.engine_size {
        docker.and_then(|d| d.df()).map(|s| [s.images, s.containers, s.volumes, s.build_cache].iter().map(|v| (*v).max(0) as u64).sum())
    } else {
        None
    };
    DiskInput { distro, vhdx, inside, drive, engine_path, engine_free, engine_used }
}

// ---------------------------------------------------------------- pravidla

/// Výsledek vyhodnocení jednoho pravidla.
#[derive(Debug, Clone, PartialEq)]
enum Eval {
    /// chybí data – nic neoznamovat, pruh schovat
    Missing,
    /// `over` = práh překročen; `clear` = hodnota se vrátila za hysterezi (znovu nabít)
    Value { over: bool, clear: bool, title: String, body: String },
}

fn pct(v: f64) -> String {
    format!("{v:.0} %")
}

fn b(v: u64) -> String {
    crate::ui::fmt_bytes(v)
}

fn eval_memory(c: &AlertsConfig, m: &MemInput) -> Eval {
    let Some(vmmem) = m.vmmem.filter(|_| m.limit > 0) else { return Eval::Missing };
    let p = vmmem as f64 * 100.0 / m.limit as f64;
    Eval::Value {
        over: p >= c.memory_pct,
        clear: p < c.memory_pct - HYST_PCT,
        title: tf!("WSL používá {} limitu paměti", "WSL is using {} of its memory limit", pct(p)),
        body: tf!(
            "VmmemWSL drží {} z {}. Souborová cache Linuxu z toho je {} – „Uvolnit paměť“ ji vrátí Windows.",
            "VmmemWSL holds {} of {}. {} of it is Linux file cache – “Free memory” returns it to Windows.",
            b(vmmem),
            b(m.limit),
            b(m.cache)
        ),
    }
}

fn eval_pressure(c: &AlertsConfig, m: &MemInput) -> Eval {
    if m.total == 0 || m.available == 0 {
        return Eval::Missing;
    }
    let p = m.available as f64 * 100.0 / m.total as f64;
    Eval::Value {
        over: p < c.pressure_pct,
        clear: p > c.pressure_pct + HYST_PCT,
        title: tf!("V Linuxu dochází paměť ({} dostupné)", "Linux is running out of memory ({} available)", pct(p)),
        body: tf!(
            "Dostupná paměť (MemAvailable) je {} z {}. Hrozí ukončování procesů (OOM) – zastav nepotřebné kontejnery nebo zvyš limit paměti WSL.",
            "Available memory (MemAvailable) is {} of {}. Processes may be killed (OOM) – stop unneeded containers or raise the WSL memory limit.",
            b(m.available),
            b(m.total)
        ),
    }
}

/// Volné místo pod prahem v % (hystereze +5 p. b.).
fn free_rule(free_pct: f64, threshold: f64, title: String, body: String) -> Eval {
    Eval::Value { over: free_pct < threshold, clear: free_pct > threshold + HYST_PCT, title, body }
}

fn eval_distro_disk(c: &AlertsConfig, d: &DiskInput) -> Eval {
    let Some(r) = &d.inside else { return Eval::Missing };
    let p = r.free_pct();
    let file = d.vhdx.as_ref().map(|(path, size)| tf!(" Soubor disku {path} má {}.", " The disk file {path} is {}.", b(*size))).unwrap_or_default();
    free_rule(
        p,
        c.distro_free_pct,
        tf!("V distribuci {} dochází místo", "Distribution {} is running out of space", d.distro),
        tf!(
            "Uvnitř zbývá {} z {} ({}).{file} Uklidit pomůže odstranění nepoužívaných image, kontejnerů a volume.",
            "{} of {} left inside ({}).{file} Removing unused images, containers and volumes helps.",
            b(r.avail),
            b(r.size),
            pct(p)
        ),
    )
}

fn eval_win_drive(c: &AlertsConfig, d: &DiskInput) -> Eval {
    let Some(s) = &d.drive else { return Eval::Missing };
    let p = s.free_pct();
    let min = c.drive_free_gb * GB;
    let file = d.vhdx.as_ref().map(|(_, size)| b(*size)).unwrap_or_else(|| "?".into());
    Eval::Value {
        over: p < c.drive_free_pct || (s.free as f64) < min,
        clear: p > c.drive_free_pct + HYST_PCT && (s.free as f64) > min * (1.0 + HYST_REL),
        title: tf!("Na disku {} dochází místo", "Drive {} is running out of space", s.name),
        body: tf!(
            "Volné je {} z {} ({}). Leží na něm disk distribuce {} (VHDX {}) – když se zaplní, WSL přestane zapisovat.",
            "{} of {} free ({}). It holds the disk of distribution {} (VHDX {}) – when it fills up, WSL can no longer write.",
            b(s.free),
            b(s.total),
            pct(p),
            d.distro,
            file
        ),
    }
}

fn eval_engine_free(c: &AlertsConfig, d: &DiskInput) -> Eval {
    let Some(r) = &d.engine_free else { return Eval::Missing };
    // stejný souborový systém jako `/` už hlídá upozornění na disk distribuce
    if c.distro_disk && d.inside.as_ref().is_some_and(|i| i.mount == r.mount && i.size == r.size) {
        return Eval::Missing;
    }
    let p = r.free_pct();
    free_rule(
        p,
        c.engine_free_pct,
        tf!("Pro data enginu dochází místo ({})", "The engine data is running out of space ({})", d.engine_path),
        tf!(
            "Na {} zbývá {} z {} ({}). Ukliď nepoužívané image, kontejnery a volume.",
            "On {}, {} of {} is left ({}). Clean up unused images, containers and volumes.",
            r.mount,
            b(r.avail),
            b(r.size),
            pct(p)
        ),
    )
}

fn eval_engine_size(c: &AlertsConfig, d: &DiskInput) -> Eval {
    let Some(used) = d.engine_used else { return Eval::Missing };
    let limit = c.engine_size_gb * GB;
    Eval::Value {
        over: used as f64 > limit,
        clear: (used as f64) < limit * (1.0 - HYST_REL),
        title: tf!("Úložiště enginu zabírá {}", "Engine storage takes {}", b(used)),
        body: tf!(
            "Image, kontejnery, volume a build cache jsou dohromady nad nastavenou hranicí {}. Ukliď nepoužívané položky.",
            "Images, containers, volumes and build cache together exceed the configured {}. Clean up unused items.",
            b(limit as u64)
        ),
    }
}

// ---------------------------------------------------------------- stav

#[derive(Debug, Clone, Default)]
struct State {
    /// aktivní upozornění (pruh na Přehledu)
    active: Option<Alert>,
    /// od kdy je práh nepřetržitě překročen (časové okno)
    since: Option<Instant>,
    /// oznámení odešlo a hodnota se zatím nevrátila za hysterezi
    fired: bool,
    last_sent: Option<Instant>,
}

/// Stav všech upozornění (sdílí worker a UI).
#[derive(Debug, Default)]
pub struct Alerts {
    states: [State; 6],
    /// poslední spuštění kontroly disku
    last_disk: Option<Instant>,
    /// poslední změřená data o disku (dlaždice Disk na přehledu)
    pub disk: Option<DiskInput>,
}

impl Alerts {
    /// Je čas na kontrolu disku? Když ano, rovnou ji zapíše jako spuštěnou.
    pub fn take_disk_due(&mut self, now: Instant) -> bool {
        if self.last_disk.is_some_and(|t| now.saturating_duration_since(t) < DISK_EVERY) {
            return false;
        }
        self.last_disk = Some(now);
        true
    }

    /// Aktivní upozornění zapnutých pravidel (pro pruh v UI).
    pub fn active(&self, c: &AlertsConfig) -> Vec<Alert> {
        Kind::ALL.iter().filter(|k| k.enabled(c)).filter_map(|k| self.states[k.idx()].active.clone()).collect()
    }

    /// Paměť; `m` = None, když distribuce neběží (žádná data).
    pub fn check_memory(&mut self, c: &AlertsConfig, m: Option<&MemInput>, now: Instant) -> Vec<Alert> {
        let hold = Duration::from_secs(c.memory_minutes as u64 * 60);
        let (e1, e2) = match m {
            Some(m) => (eval_memory(c, m), eval_pressure(c, m)),
            None => (Eval::Missing, Eval::Missing),
        };
        [self.step(Kind::Memory, c, e1, hold, now), self.step(Kind::Pressure, c, e2, Duration::ZERO, now)].into_iter().flatten().collect()
    }

    pub fn check_disk(&mut self, c: &AlertsConfig, d: &DiskInput, now: Instant) -> Vec<Alert> {
        self.disk = Some(d.clone());
        [
            (Kind::DistroDisk, eval_distro_disk(c, d)),
            (Kind::WinDrive, eval_win_drive(c, d)),
            (Kind::EngineFree, eval_engine_free(c, d)),
            (Kind::EngineSize, eval_engine_size(c, d)),
        ]
        .into_iter()
        .filter_map(|(k, e)| self.step(k, c, e, Duration::ZERO, now))
        .collect()
    }

    /// Jeden krok pravidla: časové okno `hold`, hystereze a omezení opakování.
    /// Vrací upozornění, které se má doručit.
    fn step(&mut self, kind: Kind, c: &AlertsConfig, e: Eval, hold: Duration, now: Instant) -> Option<Alert> {
        let st = &mut self.states[kind.idx()];
        if !kind.enabled(c) {
            st.active = None;
            st.since = None;
            return None;
        }
        let (over, clear, title, body) = match e {
            Eval::Missing => {
                st.active = None;
                st.since = None;
                return None;
            }
            Eval::Value { over, clear, title, body } => (over, clear, title, body),
        };
        if clear {
            *st = State { last_sent: st.last_sent, ..State::default() };
            return None;
        }
        if !over {
            // v pásmu hystereze: pruh zůstává, časové okno začne znovu
            st.since = None;
            return None;
        }
        let since = *st.since.get_or_insert(now);
        if now.saturating_duration_since(since) < hold {
            return None;
        }
        let alert = Alert { kind, title, body };
        st.active = Some(alert.clone());
        let repeat = Duration::from_secs(c.repeat_minutes as u64 * 60);
        if st.fired || st.last_sent.is_some_and(|t| now.saturating_duration_since(t) < repeat) {
            return None;
        }
        st.fired = true;
        st.last_sent = Some(now);
        Some(alert)
    }
}

// ---------------------------------------------------------------- doručení

/// Kam se doručují upozornění (toast ve Windows, v testech sběrač).
pub trait Sink {
    fn send(&self, a: &Alert);
}

/// Sběrač pro testy – nic nezobrazuje.
#[cfg(test)]
#[derive(Default)]
pub struct Collect(pub Mutex<Vec<Alert>>);

#[cfg(test)]
impl Sink for Collect {
    fn send(&self, a: &Alert) {
        if let Ok(mut v) = self.0.lock() {
            v.push(a.clone());
        }
    }
}

/// Požadavek na přechod na stránku po kliknutí na oznámení; vyzvedne `App::handle_tray`.
static NAV: Mutex<Option<Target>> = Mutex::new(None);

pub fn take_nav() -> Option<Target> {
    NAV.lock().ok().and_then(|mut g| g.take())
}

/// Toast ve Windows; kliknutí otevře okno na příslušné stránce.
struct ToastSink(eframe::egui::Context);

impl Sink for ToastSink {
    fn send(&self, a: &Alert) {
        let (title, body, target, ctx) = (a.title.clone(), a.body.clone(), a.kind.target(), self.0.clone());
        thread::spawn(move || {
            crate::notify::show_with(&title, &body, move || {
                if let Ok(mut g) = NAV.lock() {
                    *g = Some(target);
                }
                ctx.request_repaint();
            })
        });
    }
}

impl Worker {
    fn alert_sink(&self) -> Option<ToastSink> {
        self.cfg().alerts.toast.then(|| ToastSink(self.ctx.clone()))
    }

    fn deliver(&self, list: Vec<Alert>) {
        if let Some(s) = self.alert_sink() {
            for a in &list {
                s.send(a);
            }
        }
        self.ctx.request_repaint();
    }

    /// Pravidla paměti z právě nasbíraného vzorku.
    pub(super) fn alerts_memory(&self, m: MemInput) {
        let c = self.cfg().alerts;
        let out = self.shared.alerts.lock().map(|mut a| a.check_memory(&c, Some(&m), Instant::now())).unwrap_or_default();
        self.deliver(out);
    }

    /// Volá se při každém obnovení: bez běžící distribuce schová upozornění na paměť,
    /// disk zkontroluje nejvýš jednou za `DISK_EVERY` (na pozadí).
    pub(super) fn alerts_tick(&self, distro_running: bool, connected: bool) {
        let cfg = self.cfg();
        let now = Instant::now();
        if !distro_running {
            if let Ok(mut a) = self.shared.alerts.lock() {
                a.check_memory(&cfg.alerts, None, now);
            }
            return;
        }
        if !cfg.alerts.any_disk() || !self.shared.alerts.lock().map(|mut a| a.take_disk_due(now)).unwrap_or(false) {
            return;
        }
        let w = self.clone();
        thread::spawn(move || {
            let d = connected.then(|| w.docker());
            let input = collect_disk(&cfg, d.as_ref());
            w.history_disk(&input);
            let c = w.cfg().alerts;
            let out = w.shared.alerts.lock().map(|mut a| a.check_disk(&c, &input, Instant::now())).unwrap_or_default();
            w.deliver(out);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1 << 30;

    fn mem(vmmem: Option<u64>, limit: u64) -> MemInput {
        MemInput { vmmem, limit, total: 8 * G, available: 4 * G, cache: G }
    }

    fn kinds(v: &[Alert]) -> Vec<Kind> {
        v.iter().map(|a| a.kind).collect()
    }

    fn min(n: u64) -> Duration {
        Duration::from_secs(n * 60)
    }

    #[test]
    fn memory_needs_duration_window() {
        let c = AlertsConfig::default();
        let mut a = Alerts::default();
        let t = Instant::now();
        let hi = mem(Some(9 * G), 10 * G); // 90 %
        assert!(a.check_memory(&c, Some(&hi), t).is_empty());
        assert!(a.check_memory(&c, Some(&hi), t + min(1)).is_empty());
        assert!(a.active(&c).is_empty());
        let out = a.check_memory(&c, Some(&hi), t + min(2));
        assert_eq!(kinds(&out), vec![Kind::Memory]);
        assert!(out[0].body.contains("1.0 GB"), "zmíní cache: {}", out[0].body);
        assert_eq!(kinds(&a.active(&c)), vec![Kind::Memory]);
        // krátká špička nic neoznámí: okno začíná znovu
        let mut a = Alerts::default();
        a.check_memory(&c, Some(&hi), t);
        a.check_memory(&c, Some(&mem(Some(5 * G), 10 * G)), t + min(1));
        assert!(a.check_memory(&c, Some(&hi), t + min(2)).is_empty());
        assert_eq!(kinds(&a.check_memory(&c, Some(&hi), t + min(4))), vec![Kind::Memory]);
    }

    #[test]
    fn memory_threshold_boundary() {
        let c = AlertsConfig { memory_minutes: 0, ..Default::default() };
        let mut a = Alerts::default();
        let t = Instant::now();
        assert!(a.check_memory(&c, Some(&mem(Some(84 * G / 10), 10 * G)), t).is_empty());
        assert_eq!(kinds(&a.check_memory(&c, Some(&mem(Some(85 * G / 10), 10 * G)), t)), vec![Kind::Memory]);
    }

    #[test]
    fn hysteresis_and_rate_limit() {
        let c = AlertsConfig { memory_minutes: 0, ..Default::default() };
        let mut a = Alerts::default();
        let t = Instant::now();
        let hi = mem(Some(9 * G), 10 * G);
        let band = mem(Some(82 * G / 10), 10 * G); // 82 %: pod prahem, nad 80 %
        let low = mem(Some(7 * G), 10 * G);
        assert_eq!(a.check_memory(&c, Some(&hi), t).len(), 1);
        // pořád nad prahem / v pásmu hystereze: nic dalšího, pruh zůstává
        assert!(a.check_memory(&c, Some(&hi), t + min(90)).is_empty());
        assert!(a.check_memory(&c, Some(&band), t + min(91)).is_empty());
        assert_eq!(a.active(&c).len(), 1);
        assert!(a.check_memory(&c, Some(&hi), t + min(92)).is_empty(), "nenabito – v pásmu hystereze");
        // pod hysterezí: pruh zmizí, znovu nabito
        assert!(a.check_memory(&c, Some(&low), t + min(93)).is_empty());
        assert!(a.active(&c).is_empty());
        assert_eq!(a.check_memory(&c, Some(&hi), t + min(94)).len(), 1);
        // nabito, ale v rámci hodiny od posledního oznámení -> jen pruh
        a.check_memory(&c, Some(&low), t + min(95));
        assert!(a.check_memory(&c, Some(&hi), t + min(100)).is_empty());
        assert_eq!(a.active(&c).len(), 1);
        // po hodině se oznámí (stále nad prahem a nenahlášeno)
        assert_eq!(a.check_memory(&c, Some(&hi), t + min(155)).len(), 1);
    }

    #[test]
    fn disabled_and_missing_data() {
        let t = Instant::now();
        let off = AlertsConfig { memory: false, pressure: false, memory_minutes: 0, ..Default::default() };
        let mut a = Alerts::default();
        let starving = MemInput { vmmem: Some(10 * G), limit: 10 * G, total: 8 * G, available: G / 10, cache: 0 };
        assert!(a.check_memory(&off, Some(&starving), t).is_empty());
        assert!(a.active(&off).is_empty());
        let c = AlertsConfig { memory_minutes: 0, ..Default::default() };
        // bez VmmemWSL / bez limitu: jen tlak na paměť
        assert_eq!(kinds(&a.check_memory(&c, Some(&MemInput { vmmem: None, ..starving }), t)), vec![Kind::Pressure]);
        assert!(a.check_memory(&c, Some(&MemInput { limit: 0, available: 0, ..starving }), t).is_empty());
        // distribuce neběží -> pruh zmizí
        let mut a = Alerts::default();
        assert_eq!(a.check_memory(&c, Some(&starving), t).len(), 2);
        a.check_memory(&c, None, t);
        assert!(a.active(&c).is_empty());
        // vypnutí v nastavení schová pruh hned (i bez nové kontroly)
        let mut a = Alerts::default();
        a.check_memory(&c, Some(&starving), t);
        assert_eq!(a.active(&AlertsConfig { pressure: false, ..c.clone() }).len(), 1);
    }

    #[test]
    fn pressure_rule() {
        let c = AlertsConfig::default();
        let mut a = Alerts::default();
        let t = Instant::now();
        let m = |avail| MemInput { vmmem: None, limit: 0, total: 100 * G, available: avail, cache: 0 };
        assert!(a.check_memory(&c, Some(&m(11 * G)), t).is_empty());
        assert_eq!(kinds(&a.check_memory(&c, Some(&m(9 * G)), t)), vec![Kind::Pressure]);
        // 12 % je v pásmu hystereze (10–15 %)
        a.check_memory(&c, Some(&m(12 * G)), t);
        assert_eq!(a.active(&c).len(), 1);
        a.check_memory(&c, Some(&m(16 * G)), t);
        assert!(a.active(&c).is_empty());
    }

    fn row(size: u64, avail: u64, mount: &str) -> DfRow {
        DfRow { size, used: size - avail, avail, mount: mount.into() }
    }

    fn disk() -> DiskInput {
        DiskInput {
            distro: "Debian".into(),
            vhdx: Some((r"C:\wsl\ext4.vhdx".into(), 30 * G)),
            inside: Some(row(1000 * G, 500 * G, "/")),
            drive: Some(DriveSpace { name: "C:".into(), free: 200 * G, total: 1000 * G }),
            engine_path: "/var/lib/containers".into(),
            engine_free: Some(row(1000 * G, 500 * G, "/")),
            engine_used: Some(20 * G),
        }
    }

    #[test]
    fn disk_rules() {
        let c = AlertsConfig { engine_size: true, ..Default::default() };
        let t = Instant::now();
        let mut a = Alerts::default();
        assert!(a.check_disk(&c, &disk(), t).is_empty());
        // uvnitř distribuce 5 % volného; data enginu na stejném FS se nehlásí dvakrát
        let d = DiskInput { inside: Some(row(1000 * G, 50 * G, "/")), engine_free: Some(row(1000 * G, 50 * G, "/")), ..disk() };
        assert_eq!(kinds(&a.check_disk(&c, &d, t)), vec![Kind::DistroDisk]);
        // engine na vlastním disku
        let mut a = Alerts::default();
        let d = DiskInput { engine_free: Some(row(100 * G, 5 * G, "/var/lib/containers")), ..disk() };
        assert_eq!(kinds(&a.check_disk(&c, &d, t)), vec![Kind::EngineFree]);
        // disk Windows: 10 % nebo 5 GB
        let mut a = Alerts::default();
        let d = DiskInput { drive: Some(DriveSpace { name: "D:".into(), free: 4 * G, total: 20 * G }), ..disk() };
        let out = a.check_disk(&c, &d, t);
        assert_eq!(kinds(&out), vec![Kind::WinDrive]);
        assert!(out[0].title.contains("D:"));
        let mut a = Alerts::default();
        let d = DiskInput { drive: Some(DriveSpace { name: "D:".into(), free: 90 * G, total: 1000 * G }), ..disk() };
        assert_eq!(kinds(&a.check_disk(&c, &d, t)), vec![Kind::WinDrive]);
        // úložiště enginu nad 50 GB; hystereze 5 %
        let mut a = Alerts::default();
        let d = DiskInput { engine_used: Some(51 * G), ..disk() };
        assert_eq!(kinds(&a.check_disk(&c, &d, t)), vec![Kind::EngineSize]);
        a.check_disk(&c, &DiskInput { engine_used: Some(49 * G), ..disk() }, t);
        assert_eq!(a.active(&c).len(), 1, "49 GB je v pásmu hystereze");
        a.check_disk(&c, &DiskInput { engine_used: Some(47 * G), ..disk() }, t);
        assert!(a.active(&c).is_empty());
    }

    #[test]
    fn disk_defaults_and_missing() {
        let c = AlertsConfig::default();
        let t = Instant::now();
        let mut a = Alerts::default();
        // velikost úložiště je ve výchozím stavu vypnutá
        let d = DiskInput { engine_used: Some(500 * G), ..disk() };
        assert!(a.check_disk(&c, &d, t).is_empty());
        // chybí všechna data
        assert!(a.check_disk(&AlertsConfig { engine_size: true, ..c.clone() }, &DiskInput::default(), t).is_empty());
        // vypnutá pravidla
        let off = AlertsConfig { distro_disk: false, win_drive: false, engine_free: false, ..c.clone() };
        assert!(!off.any_disk());
        let full = DiskInput { inside: Some(row(10 * G, 0, "/")), drive: Some(DriveSpace { name: "C:".into(), free: 0, total: G }), ..disk() };
        assert!(a.check_disk(&off, &full, t).is_empty());
        // engine_free se hlásí, když je hlídání disku distribuce vypnuté (stejný FS)
        let only_engine = AlertsConfig { distro_disk: false, win_drive: false, ..c.clone() };
        assert_eq!(kinds(&a.check_disk(&only_engine, &DiskInput { engine_free: Some(row(10 * G, 0, "/")), ..full }, t)), vec![Kind::EngineFree]);
    }

    #[test]
    fn disk_check_cadence() {
        let mut a = Alerts::default();
        let t = Instant::now();
        assert!(a.take_disk_due(t));
        assert!(!a.take_disk_due(t + Duration::from_secs(10)));
        assert!(!a.take_disk_due(t + Duration::from_secs(299)));
        assert!(a.take_disk_due(t + DISK_EVERY));
    }

    #[test]
    fn parses_df() {
        let out = "Filesystem        1B-blocks        Used     Available Capacity Mounted on\n\
                   /dev/sdd       1081101176832 21321834496 1004787068928       3% /\n\
                   C:\\ 999 1 2 1% /mnt/c\n\
                   my disk 100 40 60 40% /mnt/with space\n\
                   garbage line\n";
        let r = parse_df(out);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0], DfRow { size: 1_081_101_176_832, used: 21_321_834_496, avail: 1_004_787_068_928, mount: "/".into() });
        assert_eq!(r[2].mount, "/mnt/with space");
        assert_eq!(r[2].free_pct(), 60.0);
        assert!(parse_df("").is_empty());
        let (root, eng) = parse_df_pair(&format!("{out}@@\n"));
        assert_eq!(root.unwrap().mount, "/");
        assert!(eng.is_none());
        let (_, eng) = parse_df_pair("@@\nFilesystem 1B-blocks Used Available Capacity Mounted on\n/dev/sde 100 90 10 90% /var/lib/containers\n");
        assert_eq!(eng.unwrap().free_pct(), 10.0);
        // prázdný FS nedělí nulou
        assert_eq!(DfRow { size: 0, used: 0, avail: 0, mount: "/".into() }.free_pct(), 100.0);
    }

    #[cfg(windows)]
    #[test]
    fn system_drive_free_space() {
        let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let s = drive_space(std::path::Path::new(&format!("{sys}\\"))).expect("GetDiskFreeSpaceExW");
        assert!(s.total > 0 && s.free <= s.total, "{s:?}");
        assert!(s.name.eq_ignore_ascii_case(&sys), "{s:?}");
        // podsložka vrací stejný disk; neexistující cesta nic
        let win = std::env::var("SystemRoot").unwrap_or_else(|_| format!("{sys}\\Windows"));
        assert_eq!(drive_space(std::path::Path::new(&win)).map(|d| d.total), Some(s.total));
        assert!(drive_space(std::path::Path::new(r"\\?\Volume{00000000-0000-0000-0000-000000000000}\")).is_none());
    }

    #[test]
    fn config_defaults_and_old_configs() {
        let c = AlertsConfig::default();
        assert!(c.memory && c.pressure && c.distro_disk && c.win_drive && c.engine_free && c.toast);
        assert!(!c.engine_size);
        assert_eq!((c.memory_pct, c.memory_minutes, c.pressure_pct, c.drive_free_gb, c.repeat_minutes), (85.0, 2, 10.0, 5.0, 60));
        // stará konfigurace bez sekce alerts
        let old: crate::config::Config = serde_json::from_str(r#"{"distro":"Debian","port":2376}"#).unwrap();
        assert_eq!(old.alerts, c);
        // částečná sekce doplní zbytek
        let part: crate::config::Config = serde_json::from_str(r#"{"alerts":{"memory_pct":70,"engine_size":true}}"#).unwrap();
        assert_eq!((part.alerts.memory_pct, part.alerts.engine_size, part.alerts.pressure_pct), (70.0, true, 10.0));
    }

    #[test]
    fn collect_sink_gathers_messages() {
        let s = Collect::default();
        s.send(&Alert { kind: Kind::Memory, title: "t".into(), body: "b".into() });
        assert_eq!(s.0.lock().unwrap().len(), 1);
        assert_eq!(Kind::Memory.target(), Target::Memory);
        assert_eq!(Kind::WinDrive.target(), Target::Engine);
    }

    /// Skutečné hodnoty (jen čtení), stejná cesta jako worker; nic se nezobrazuje:
    /// `cargo test live_alerts -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_alerts() {
        let mut cfg = crate::config::Config::load();
        crate::docker::configure_tls(cfg.tls).expect("TLS");
        let d = crate::docker::Docker::new(&crate::host::endpoint(&cfg));
        // paměť: stejně jako `memory_sample` (minimální vzorek + VmmemWSL) -> `mem_input`
        let out = crate::host::HostExt::run(&*crate::host::current(&cfg), crate::host::Privilege::User, None, &["sh", "-c", crate::memory::SAMPLE_SCRIPT_MIN]).expect("wsl");
        let (mi, _) = crate::memory::parse_sample(&out).expect("meminfo");
        let vmmem = crate::memory::VmmemProbe::default().get();
        let m = mem_input(&mi, vmmem);
        // disk: se zapnutou velikostí úložiště, ať se projde i `/system/df`
        cfg.alerts.engine_size = true;
        let t0 = Instant::now();
        let disk = collect_disk(&cfg, d.ping().then_some(&d));
        let g = |v: u64| crate::ui::fmt_bytes(v);
        println!("collect_disk: {:?}", t0.elapsed());
        println!(
            "memory: VmmemWSL {} / limit {} ({}), MemAvailable {} / {}, cache {}",
            m.vmmem.map(g).unwrap_or_else(|| "-".into()),
            g(m.limit),
            m.vmmem.map(|v| format!("{:.1} %", v as f64 * 100.0 / m.limit.max(1) as f64)).unwrap_or_default(),
            g(m.available),
            g(m.total),
            g(m.cache)
        );
        println!("vhdx: {:?}", disk.vhdx.as_ref().map(|(p, s)| format!("{p} {}", g(*s))));
        println!("inside /: {:?} free {:.1} %", disk.inside, disk.inside.as_ref().map(|r| r.free_pct()).unwrap_or(0.0));
        println!("drive: {:?} free {:.1} %", disk.drive, disk.drive.as_ref().map(|r| r.free_pct()).unwrap_or(0.0));
        println!("engine {}: {:?}, storage {:?}", disk.engine_path, disk.engine_free, disk.engine_used.map(g));
        let run = |label: &str, c: &AlertsConfig| -> Vec<Alert> {
            let mut a = Alerts::default();
            let t = Instant::now();
            // časové okno: druhý vzorek po `memory_minutes`
            let mut out = a.check_memory(c, Some(&m), t);
            out.extend(a.check_memory(c, Some(&m), t + Duration::from_secs(c.memory_minutes as u64 * 60)));
            out.extend(a.check_disk(c, &disk, t));
            let sink = Collect::default();
            for x in &out {
                sink.send(x);
            }
            let got = sink.0.into_inner().unwrap();
            println!("--- {label}: {} alert(s)", got.len());
            for x in &got {
                println!("  [{:?}] {} | {}", x.kind, x.title, x.body);
            }
            got
        };
        let defaults = AlertsConfig { engine_size: false, ..cfg.alerts.clone() };
        run("configured thresholds", &defaults);
        run("defaults", &AlertsConfig::default());
        let low = AlertsConfig {
            memory_pct: 1.0,
            pressure_pct: 99.0,
            distro_free_pct: 99.9,
            drive_free_pct: 99.9,
            drive_free_gb: 1.0e6,
            engine_free_pct: 99.9,
            engine_size: true,
            engine_size_gb: 0.000_001,
            distro_disk: false, // ať se ukáže i data enginu na stejném FS
            ..AlertsConfig::default()
        };
        let got = run("artificially low thresholds", &low);
        let low2 = AlertsConfig { distro_disk: true, ..low.clone() };
        let got2 = run("artificially low thresholds (distro disk on)", &low2);
        let k = kinds(&got);
        if m.vmmem.is_some() {
            assert!(k.contains(&Kind::Memory));
        }
        assert!(k.contains(&Kind::Pressure));
        if disk.drive.is_some() {
            assert!(k.contains(&Kind::WinDrive));
        }
        if disk.engine_free.is_some() {
            assert!(k.contains(&Kind::EngineFree));
        }
        if disk.engine_used.is_some_and(|u| u > 0) {
            assert!(k.contains(&Kind::EngineSize));
        }
        if disk.inside.is_some() {
            assert!(kinds(&got2).contains(&Kind::DistroDisk));
        }
    }
}
