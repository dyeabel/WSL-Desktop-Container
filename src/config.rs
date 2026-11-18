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
    /// Oznámení ve Windows při pádu kontejneru, selhání healthchecku, opakovaných restartech
    pub notifications: bool,
    /// API enginu jen přes TLS s klientským certifikátem (port 2376, certifikáty v %APPDATA%\WLS_Desktop\tls)
    pub tls: bool,
    /// Upozornění „API bez ověření“ na stránce WSL engine bylo zavřeno
    pub security_hint_dismissed: bool,
    /// Pojmenovaná roura `\\.\pipe\wls_docker_engine` pro nástroje (relay běží jen s aplikací)
    pub npipe: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            distro: "Ubuntu".into(),
            engine: crate::engine::Engine::Docker,
            port: 2375,
            refresh_secs: 3,
            keep_alive: true,
            autostart_engine: false,
            close_to_tray: true,
            minimize_to_tray: true,
            start_hidden: false,
            restore_containers: false,
            language: "cs".into(),
            notifications: true,
            tls: false,
            security_hint_dismissed: false,
            npipe: false,
        }
    }
}

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("WLS_Desktop").join("config.json"))
}

impl Config {
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
