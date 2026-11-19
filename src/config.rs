use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Nastavení aplikace, ukládá se do %APPDATA%\WLS_Desktop\config.json
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// WSL distribuce, ve které běží Docker Engine
    pub distro: String,
    /// kontejnerový engine v distribuci (Docker / Podman)
    pub engine: crate::engine::Engine,
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
    /// API enginu jen přes TLS s klientským certifikátem (port 2376, certifikáty v %APPDATA%\WLS_Desktop\tls)
    pub tls: bool,
    /// Upozornění „API bez ověření“ na stránce WSL engine bylo zavřeno
    pub security_hint_dismissed: bool,
    /// Pojmenovaná roura `\\.\pipe\wls_docker_engine` pro nástroje (relay běží jen s aplikací)
    pub npipe: bool,
    /// Publikované porty kontejnerů i na `[::1]` (relay v aplikaci), aby `localhost` odpovídal hned
    pub ipv6_localhost_relay: bool,
    /// Upozornění na prahové hodnoty paměti a disku
    pub alerts: AlertsConfig,
    /// Ukládat historii zdrojů (paměť, CPU, provoz, disk) na disk, aby přežila restart aplikace
    pub history_on_disk: bool,
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
            port: 2375,
            refresh_secs: 3,
            background_refresh_secs: 15,
            keep_alive: true,
            autostart_engine: false,
            close_to_tray: true,
            minimize_to_tray: true,
            start_hidden: false,
            restore_containers: false,
            language: "cs".into(),
            theme: "dark".into(),
            notifications: true,
            container_notify: ContainerNotify::default(),
            auto_free_memory: true,
            tls: false,
            security_hint_dismissed: false,
            npipe: false,
            ipv6_localhost_relay: true,
            alerts: AlertsConfig::default(),
            history_on_disk: true,
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
/// v portable režimu `<složka aplikace>\data`, jinak `%APPDATA%\WLS_Desktop`.
pub fn data_dir() -> Option<PathBuf> {
    // WLS_DATA_DIR: oddělená datová složka (testy, zkušební instance) – nic se nezapíše
    // do skutečné konfigurace a uživatelské proměnné prostředí (DOCKER_HOST) se nemění
    if let Some(d) = std::env::var_os("WLS_DATA_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    if let Some(d) = portable_dir() {
        return Some(d.join("data"));
    }
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("WLS_Desktop"))
}

/// Běží aplikace s oddělenou datovou složkou (`WLS_DATA_DIR`)? Pak se nemění nic mimo ni.
pub fn isolated() -> bool {
    std::env::var_os("WLS_DATA_DIR").is_some_and(|d| !d.is_empty())
}

fn path() -> Option<PathBuf> {
    data_dir().map(|d| d.join("config.json"))
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
        let p = path().ok_or("Proměnná APPDATA není nastavená")?;
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let s = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(p, s).map_err(|e| e.to_string())
    }
}
