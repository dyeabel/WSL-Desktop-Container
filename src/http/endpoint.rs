//! Adresa API enginu (samostatný soubor, aby ji mohl převzít i shim `docker.exe`).

/// Kudy se z hostitele dostaneme k API enginu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Tcp { port: u16 },
    /// Unix socket enginu (vytváří ho jen hostitel Linux; na Windows je varianta kvůli úplným `match`).
    #[cfg_attr(not(unix), allow(dead_code))]
    Unix(std::path::PathBuf),
}
