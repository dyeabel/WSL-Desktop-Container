use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Nastavení aplikace, ukládá se do %APPDATA%\WSL_Container_Desktop\config.json
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// WSL distribuce, ve které běží Docker Engine
    pub distro: String,
    /// kontejnerový engine v distribuci (Docker / Podman)
    pub engine: crate::engine::Engine,
    /// naposledy použitý engine v jednotlivých distribucích (pro přepnutí primární distribuce)
    pub distro_engines: std::collections::BTreeMap<String, crate::engine::Engine>,
    /// při přepnutí primární distribuce nechat tu dosavadní běžet (výchozí: uspat, wsl --terminate)
    pub keep_previous_primary: bool,
    /// TCP port Docker API (dockerd -H tcp://127.0.0.1:<port>)
    pub port: u16,
    /// Interval automatického obnovení v sekundách
    pub refresh_secs: u64,
    /// Interval obnovení a sběru dat, když je okno schované (oznamovací oblast, minimalizace)
    pub background_refresh_secs: u64,
    /// Držet distribuci naživu skrytým procesem `wsl.exe -e sleep infinity`
    pub keep_alive: bool,
    /// Po spuštění aplikace automaticky nastartovat Docker
    pub autostart_engine: bool,
    /// `docker` shim při prvním příkazu spustí aplikaci (schovanou) a engine, když neběží
    pub shim_autostart: bool,
    /// Při ukončení aplikace vypnout WSL (`wsl --shutdown`), aby nezabíralo paměť
    pub shutdown_wsl_on_quit: bool,
    /// Křížek okno jen schová do oznamovací oblasti
    pub close_to_tray: bool,
    /// Minimalizace schová okno do oznamovací oblasti
    pub minimize_to_tray: bool,
    /// Spustit rovnou schované v oznamovací oblasti
    pub start_hidden: bool,
    /// Po startu enginu znovu spustit kontejnery, které naposledy běžely
    pub restore_containers: bool,
    /// Jazyk rozhraní: "cs" nebo "en"
    pub language: String,
    /// Motiv: "dark", "light" nebo "system" (podle Windows)
    pub theme: String,
    /// Oznámení ve Windows při pádu kontejneru, selhání healthchecku, opakovaných restartech
    pub notifications: bool,
    /// Které problémy kontejnerů oznamovat a které jen zapsat do historie
    pub container_notify: ContainerNotify,
    /// Každých 10 min uvolnit cache Linuxu ve WSL (když je nad 1 GB), aby VmmemWSL vracel paměť Windows
    pub auto_free_memory: bool,
    /// Společný strop paměti pro kontejnery (jen WSL distribuce), aby engine nevyčerpal paměť celé VM
    pub container_guard: bool,
    /// Rezerva paměti pro Linux mimo kontejnery v GB (0 = automaticky, 0–64 po 0,5)
    pub guard_reserve_gb: f64,
    /// Distribuce, kde se strop paměti nemá nastavovat
    pub unguarded_distros: Vec<String>,
    /// API enginu jen přes TLS s klientským certifikátem (port 2376, certifikáty v %APPDATA%\WSL_Container_Desktop\tls)
    pub tls: bool,
    /// Upozornění „API bez ověření“ na stránce WSL engine bylo zavřeno
    pub security_hint_dismissed: bool,
    /// Standardní roura `\\.\pipe\docker_engine` pro nástroje (relay běží jen s aplikací;
    /// nástroje ji najdou bez DOCKER_HOST)
    pub npipe: bool,
    /// Publikované porty kontejnerů i na `[::1]` (relay v aplikaci), aby `localhost` odpovídal hned
    pub ipv6_localhost_relay: bool,
    /// Upozornění na prahové hodnoty paměti a disku
    pub alerts: AlertsConfig,
    /// Ukládat historii zdrojů (paměť, CPU, provoz, disk) na disk, aby přežila restart aplikace
    pub history_on_disk: bool,
    /// Kubernetes laboratoř: image k3s uzlu
    pub k3s_image: String,
    /// port API clusteru na 127.0.0.1
    pub k3s_api_port: u16,
    /// rozsah NodePort zveřejněný na 127.0.0.1 (zpřístupněné služby)
    pub k3s_nodeport_range: (u16, u16),
}

/// Prahy upozornění (paměť WSL, místo na disku). Chybějící klíče doplní výchozí hodnoty.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AlertsConfig {
    /// VmmemWSL nad `memory_pct` % limitu WSL aspoň `memory_minutes` minut
    pub memory: bool,
    pub memory_pct: f64,
    pub memory_minutes: u32,
    /// MemAvailable v Linuxu pod `pressure_pct` % MemTotal
    pub pressure: bool,
    pub pressure_pct: f64,
    /// volné místo uvnitř distribuce (`df /`) pod `distro_free_pct` %
    pub distro_disk: bool,
    pub distro_free_pct: f64,
    /// volné místo na disku Windows s VHDX pod `drive_free_pct` % nebo pod `drive_free_gb` GB
    pub win_drive: bool,
    pub drive_free_pct: f64,
    pub drive_free_gb: f64,
    /// volné místo pro data enginu (/var/lib/containers, /var/lib/docker) pod `engine_free_pct` %
    pub engine_free: bool,
    pub engine_free_pct: f64,
    /// image + kontejnery + volume + build cache nad `engine_size_gb` GB (výchozí vypnuto)
    pub engine_size: bool,
    pub engine_size_gb: f64,
    /// zobrazovat i jako oznámení ve Windows (jinak jen pruh na Přehledu)
    pub toast: bool,
    /// stejné upozornění nejvýš jednou za tolik minut
    pub repeat_minutes: u32,
}

impl Default for AlertsConfig {
    fn default() -> Self {
        Self {
            memory: true,
            memory_pct: 85.0,
            memory_minutes: 2,
            pressure: true,
            pressure_pct: 10.0,
            distro_disk: true,
            distro_free_pct: 10.0,
            win_drive: true,
            drive_free_pct: 10.0,
            drive_free_gb: 5.0,
            engine_free: true,
            engine_free_pct: 10.0,
            engine_size: false,
            engine_size_gb: 50.0,
            toast: true,
            repeat_minutes: 60,
        }
    }
}

/// Oznámení o problémech kontejnerů. Všechno se zapisuje do historie (stránka Události),
/// tady se volí, co se navíc ohlásí ve Windows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContainerNotify {
    pub crash: bool,
    pub oom: bool,
    pub health: bool,
    pub restart_loop: bool,
    /// oznamovat i ukončení signálem SIGTERM / SIGINT / SIGHUP (kód 143 / 130 / 129)
    pub signal_exits: bool,
    /// oznamovat i dočasné kontejnery (`--rm`) a Testcontainers
    pub ephemeral: bool,
    /// stejný problém stejného kontejneru nejvýš jednou za tolik minut
    pub repeat_minutes: u32,
    /// ztlumené kontejnery (podle názvu) – jen historie
    pub muted: Vec<String>,
}

impl Default for ContainerNotify {
    fn default() -> Self {
        Self { crash: true, oom: true, health: true, restart_loop: true, signal_exits: false, ephemeral: false, repeat_minutes: 30, muted: vec![] }
    }
}

impl ContainerNotify {
    pub fn is_muted(&self, name: &str) -> bool {
        self.muted.iter().any(|m| m == name)
    }

    pub fn repeat_secs(&self) -> i64 {
        self.repeat_minutes.max(1) as i64 * 60
    }
}

impl AlertsConfig {
    /// Je zapnuté aspoň jedno upozornění na disk?
    pub fn any_disk(&self) -> bool {
        self.distro_disk || self.win_drive || self.engine_free || self.engine_size
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            distro: "Ubuntu".into(),
            engine: crate::engine::Engine::Docker,
            distro_engines: Default::default(),
            keep_previous_primary: false,
            port: 2375,
            refresh_secs: 3,
            background_refresh_secs: 15,
            keep_alive: true,
            autostart_engine: false,
            shim_autostart: true,
            shutdown_wsl_on_quit: false,
            close_to_tray: true,
            minimize_to_tray: true,
            start_hidden: false,
            restore_containers: false,
            language: "cs".into(),
            theme: "dark".into(),
            notifications: true,
            container_notify: ContainerNotify::default(),
            auto_free_memory: true,
            container_guard: true,
            guard_reserve_gb: 0.0,
            unguarded_distros: Vec::new(),
            tls: false,
            security_hint_dismissed: false,
            npipe: false,
            ipv6_localhost_relay: true,
            alerts: AlertsConfig::default(),
            history_on_disk: true,
            k3s_image: DEFAULT_K3S_IMAGE.into(),
            k3s_api_port: 6443,
            k3s_nodeport_range: (30080, 30099),
        }
    }
}

/// Název souboru, jehož přítomnost vedle EXE zapne portable režim.
pub const PORTABLE_MARKER: &str = "PORTABLE.txt";

/// Složka portable balíčku (tam, kde leží EXE vedle `PORTABLE.txt`), jinak None.
pub fn portable_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    dir.join(PORTABLE_MARKER).exists().then(|| dir.to_path_buf())
}

/// Složka s daty aplikace (konfigurace, certifikáty, seznam kontejnerů…):
/// v portable režimu `<složka aplikace>\data`, jinak `%APPDATA%\WSL_Container_Desktop`
/// (dřívější `%APPDATA%\WLS_Desktop` se při prvním použití přejmenuje).
pub fn data_dir() -> Option<PathBuf> {
    // WSLCD_DATA_DIR: oddělená datová složka (testy, zkušební instance) – nic se nezapíše
    // do skutečné konfigurace a uživatelské proměnné prostředí (DOCKER_HOST) se nemění
    if let Some(d) = std::env::var_os("WSLCD_DATA_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    // testy nikdy nečtou ani nepíšou %APPDATA% ani skutečná data portable instalace
    #[cfg(test)]
    return Some(test_data_dir().clone());
    #[cfg(not(test))]
    if let Some(d) = portable_dir() {
        return Some(d.join("data"));
    }
    #[cfg(all(not(test), windows))]
    {
        let base = PathBuf::from(std::env::var_os("APPDATA")?);
        Some(migrate_dir(&base.join(LEGACY_DATA_DIR), &base.join(DATA_DIR)))
    }
    // Linux: $XDG_DATA_HOME/wsl-container-desktop (výchozí ~/.local/share/...)
    #[cfg(all(not(test), not(windows)))]
    {
        xdg_data_dir(|k| std::env::var_os(k))
    }
}

/// Název složky aplikace v XDG adresářích (Linux).
pub const XDG_DIR: &str = "wsl-container-desktop";

/// Absolutní, neprázdná hodnota proměnné prostředí (XDG specifikace ignoruje prázdné a relativní).
fn abs_env(env: &impl Fn(&str) -> Option<std::ffi::OsString>, key: &str) -> Option<PathBuf> {
    env(key).filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.has_root())
}

/// Základ pro konfiguraci: `$XDG_CONFIG_HOME`, jinak `$HOME/.config`.
#[cfg_attr(windows, allow(dead_code))] // jen Linux; čistá logika se testuje i na Windows
pub fn xdg_config_home(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    abs_env(&env, "XDG_CONFIG_HOME").or_else(|| abs_env(&env, "HOME").map(|h| h.join(".config")))
}

/// Základ pro data: `$XDG_DATA_HOME`, jinak `$HOME/.local/share`.
#[cfg_attr(windows, allow(dead_code))]
fn xdg_data_home(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    abs_env(&env, "XDG_DATA_HOME").or_else(|| abs_env(&env, "HOME").map(|h| h.join(".local").join("share")))
}

/// Datová složka aplikace: jen `$XDG_DATA_HOME`/`$HOME` (konfigurace se k jejímu určení nepoužije).
#[cfg_attr(windows, allow(dead_code))]
pub fn xdg_data_dir(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    Some(xdg_data_home(&env)?.join(XDG_DIR))
}

/// Složky aplikace podle XDG: `(data, config)`. Čistá funkce, prostředí se předává zvenku.
#[cfg_attr(windows, allow(dead_code))]
pub fn xdg_dirs(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<(PathBuf, PathBuf)> {
    let data = xdg_data_home(&env)?.join(XDG_DIR);
    let config = xdg_config_home(&env)?.join(XDG_DIR);
    Some((data, config))
}

/// Portable instalace, ze které testy berou konfiguraci (WSLCD_PORTABLE_DIR ji přepíše).
#[cfg(test)]
pub const TEST_PORTABLE_DIR: &str = r"C:\WSLContainerDesktop";

/// Datová složka testů bez WSLCD_DATA_DIR: `target\test-data\<pid>` s kopií `config.json`
/// a `tls\` z portable instalace. Testy tak vidí skutečnou primární distribuci, port a
/// certifikáty, ale nic nezapíšou do `%APPDATA%` ani do dat instalace.
#[cfg(test)]
fn test_data_dir() -> &'static PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let portable = std::env::var_os("WSLCD_PORTABLE_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(TEST_PORTABLE_DIR));
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("test-data");
        // složky dřívějších běhů obsahují kopii klientského klíče TLS – staré se mažou;
        // mladší než hodina můžou patřit souběžně běžícím testům
        let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        remove_old_test_data(&root, &std::process::id().to_string(), cutoff);
        let dir = root.join(std::process::id().to_string());
        copy_test_data(&portable.join("data"), &dir);
        dir
    })
}

/// Smaže v `root` složky jiných běhů testů (název = PID, jiný než `own`) změněné před `cutoff`.
#[cfg(test)]
fn remove_old_test_data(root: &std::path::Path, own: &str, cutoff: std::time::SystemTime) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    for e in rd.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == own || name.parse::<u32>().is_err() {
            continue;
        }
        let old = e.metadata().ok().filter(|m| m.is_dir()).and_then(|m| m.modified().ok()).is_some_and(|t| t < cutoff);
        if old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// Zkopíruje z datové složky jen `config.json` a `tls\*` (ne distribuce, historii ani cache).
#[cfg(test)]
fn copy_test_data(src: &std::path::Path, dst: &std::path::Path) {
    let _ = std::fs::create_dir_all(dst.join("tls"));
    let _ = std::fs::copy(src.join("config.json"), dst.join("config.json"));
    if let Ok(rd) = std::fs::read_dir(src.join("tls")) {
        for e in rd.flatten().filter(|e| e.path().is_file()) {
            let _ = std::fs::copy(e.path(), dst.join("tls").join(e.file_name()));
        }
    }
}

/// Výchozí image uzlu Kubernetes laboratoře (ověřeno ve spiku, viz docs/superpowers/specs).
pub const DEFAULT_K3S_IMAGE: &str = "docker.io/rancher/k3s:v1.33.6-k3s1";

/// Složka dat v %APPDATA% (a %LOCALAPPDATA% pro příkaz docker).
pub const DATA_DIR: &str = "WSL_Container_Desktop";
/// Složka z doby před přejmenováním (WLS Desktop).
pub const LEGACY_DATA_DIR: &str = "WLS_Desktop";

/// Přejmenuje starou složku na novou, pokud nová ještě není; vrací složku, kterou použít
/// (když přejmenování selže – soubor drží jiný proces –, zůstane se u staré).
#[cfg_attr(not(windows), allow(dead_code))] // jen Windows (%APPDATA%); logika se testuje i na Linuxu
pub fn migrate_dir(old: &std::path::Path, new: &std::path::Path) -> PathBuf {
    if new.exists() || !old.exists() {
        return new.to_path_buf();
    }
    match std::fs::rename(old, new) {
        Ok(()) => new.to_path_buf(),
        Err(_) if new.exists() => new.to_path_buf(),
        Err(_) => old.to_path_buf(),
    }
}

/// Běží aplikace s oddělenou datovou složkou (`WSLCD_DATA_DIR`)? Pak se nemění nic mimo ni.
pub fn isolated() -> bool {
    std::env::var_os("WSLCD_DATA_DIR").is_some_and(|d| !d.is_empty())
}

fn path() -> Option<PathBuf> {
    // Linux: konfigurace v $XDG_CONFIG_HOME/wsl-container-desktop, data jinde; s WSLCD_DATA_DIR
    // nebo v portable režimu zůstává `config.json` v datové složce (jako na Windows)
    #[cfg(all(not(test), not(windows)))]
    if !isolated() && portable_dir().is_none() {
        return xdg_dirs(|k| std::env::var_os(k)).map(|(_, config)| config.join("config.json"));
    }
    data_dir().map(|d| d.join("config.json"))
}

/// Kde aplikace drží konfiguraci a data, jako text pro „O aplikaci“. Windows beze změny
/// (`%APPDATA%\WSL_Container_Desktop`); Linux: skutečné cesty podle XDG (konfigurace a data
/// zvlášť), s `WSLCD_DATA_DIR` nebo v portable režimu jedna složka.
pub fn location_label() -> String {
    #[cfg(windows)]
    {
        format!(r"%APPDATA%\{DATA_DIR}")
    }
    #[cfg(not(windows))]
    {
        let data = data_dir();
        let cfg = path().and_then(|p| p.parent().map(PathBuf::from));
        match (cfg, data) {
            (Some(c), Some(d)) if c != d => format!("{} + {}", c.display(), d.display()),
            (Some(p), _) | (None, Some(p)) => p.display().to_string(),
            (None, None) => "?".to_string(),
        }
    }
}

impl Config {
    /// Existuje uložená konfigurace? (false = první spuštění)
    pub fn exists() -> bool {
        path().is_some_and(|p| p.exists())
    }

    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        #[cfg(windows)]
        let missing = "Proměnná APPDATA není nastavená";
        #[cfg(not(windows))]
        let missing = "Proměnná HOME není nastavená";
        let p = path().ok_or(missing)?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let s = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(p, s).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod migrate_tests {
    #[test]
    fn data_dir_is_moved_once() {
        let base = std::env::temp_dir().join(format!("wslcd-migrate-{}", std::process::id()));
        let (old, new) = (base.join("WLS_Desktop"), base.join("WSL_Container_Desktop"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("config.json"), "{}").unwrap();
        assert_eq!(super::migrate_dir(&old, &new), new);
        assert!(new.join("config.json").is_file() && !old.exists());
        // další volání nic nemění, nová složka má přednost
        std::fs::create_dir_all(&old).unwrap();
        assert_eq!(super::migrate_dir(&old, &new), new);
        assert!(old.exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn tests_never_use_appdata() {
        let d = super::data_dir().unwrap();
        if let Some(app) = std::env::var_os("APPDATA") {
            assert!(!d.starts_with(app), "{}", d.display());
        }
        assert!(!d.starts_with(super::TEST_PORTABLE_DIR), "testy nesmí psát do dat instalace: {}", d.display());
    }

    #[test]
    fn test_data_copies_only_config_and_tls() {
        let base = std::env::temp_dir().join(format!("wslcd-testdata-{}", std::process::id()));
        let (src, dst) = (base.join("src"), base.join("dst"));
        std::fs::create_dir_all(src.join("tls")).unwrap();
        std::fs::create_dir_all(src.join("distros").join("X")).unwrap();
        std::fs::write(src.join("config.json"), r#"{"distro":"P"}"#).unwrap();
        std::fs::write(src.join("tls").join("cert.pem"), "c").unwrap();
        std::fs::write(src.join("distros").join("X").join("ext4.vhdx"), "v").unwrap();
        super::copy_test_data(&src, &dst);
        assert!(dst.join("config.json").is_file() && dst.join("tls").join("cert.pem").is_file());
        assert!(!dst.join("distros").exists());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn old_test_data_is_removed() {
        use std::time::{Duration, SystemTime};
        let base = std::env::temp_dir().join(format!("wslcd-testdata-clean-{}", std::process::id()));
        for d in ["111", "222", "own", "keep-me"] {
            std::fs::create_dir_all(base.join(d).join("tls")).unwrap();
        }
        std::fs::write(base.join("333"), "soubor, ne složka").unwrap();
        // všechno je mladší než hodina -> nic se nesmaže
        super::remove_old_test_data(&base, "own", SystemTime::now() - Duration::from_secs(3600));
        for d in ["111", "222", "own", "keep-me", "333"] {
            assert!(base.join(d).exists(), "{d}");
        }
        // hranice v budoucnosti: staré jsou všechny, smažou se jen cizí PID složky
        super::remove_old_test_data(&base, "222", SystemTime::now() + Duration::from_secs(60));
        assert!(!base.join("111").exists());
        for d in ["222", "own", "keep-me", "333"] {
            assert!(base.join(d).exists(), "{d}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[cfg(test)]
mod xdg_tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| OsString::from(v))
    }

    #[test]
    fn xdg_paths() {
        let app = super::XDG_DIR;
        // proměnné XDG mají přednost před HOME
        let (data, cfg) = super::xdg_dirs(env(&[("XDG_DATA_HOME", "/x/data"), ("XDG_CONFIG_HOME", "/x/cfg"), ("HOME", "/h")])).unwrap();
        assert_eq!(data, PathBuf::from("/x/data").join(app));
        assert_eq!(cfg, PathBuf::from("/x/cfg").join(app));
        // výchozí hodnoty podle HOME
        let (data, cfg) = super::xdg_dirs(env(&[("HOME", "/h")])).unwrap();
        assert_eq!(data, PathBuf::from("/h/.local/share").join(app));
        assert_eq!(cfg, PathBuf::from("/h/.config").join(app));
        // prázdné a relativní hodnoty XDG se podle specifikace ignorují
        let (data, cfg) = super::xdg_dirs(env(&[("XDG_DATA_HOME", ""), ("XDG_CONFIG_HOME", "rel/cfg"), ("HOME", "/h")])).unwrap();
        assert_eq!(data, PathBuf::from("/h/.local/share").join(app));
        assert_eq!(cfg, PathBuf::from("/h/.config").join(app));
        // bez HOME a bez XDG nic
        assert!(super::xdg_dirs(env(&[])).is_none());
        // jen XDG bez HOME stačí
        assert!(super::xdg_dirs(env(&[("XDG_DATA_HOME", "/x/data"), ("XDG_CONFIG_HOME", "/x/cfg")])).is_some());
    }

    #[test]
    fn xdg_data_dir_needs_only_data_home() {
        // XDG_DATA_HOME bez XDG_CONFIG_HOME a bez HOME stačí
        assert_eq!(super::xdg_data_dir(env(&[("XDG_DATA_HOME", "/x/data")])), Some(PathBuf::from("/x/data").join(super::XDG_DIR)));
        // jinak výchozí podle HOME; bez obojího nic
        assert_eq!(super::xdg_data_dir(env(&[("HOME", "/h")])), Some(PathBuf::from("/h/.local/share").join(super::XDG_DIR)));
        assert!(super::xdg_data_dir(env(&[("XDG_CONFIG_HOME", "/x/cfg")])).is_none());
    }

    #[test]
    fn xdg_config_home_base() {
        assert_eq!(super::xdg_config_home(env(&[("XDG_CONFIG_HOME", "/x/cfg")])), Some(PathBuf::from("/x/cfg")));
        assert_eq!(super::xdg_config_home(env(&[("HOME", "/h")])), Some(PathBuf::from("/h/.config")));
        assert_eq!(super::xdg_config_home(env(&[])), None);
    }
}

#[cfg(test)]
mod guard_tests {
    use super::*;

    #[test]
    fn config_defaults_guard_on() {
        let c: Config = serde_json::from_str("{}").unwrap();
        assert!(c.container_guard && c.guard_reserve_gb == 0.0 && c.unguarded_distros.is_empty());
        let c: Config =
            serde_json::from_str(r#"{"container_guard":false,"guard_reserve_gb":2.5,"unguarded_distros":["A"]}"#).unwrap();
        assert!(!c.container_guard && c.guard_reserve_gb == 2.5 && c.unguarded_distros == ["A"]);
    }
}
