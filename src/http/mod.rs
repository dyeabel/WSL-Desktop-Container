//! HTTP klient k enginu: adresa endpointu, spojení ([`conn`]) a HTTP/1.1 klient ([`client`]).

pub mod client;
pub mod conn;

/// Kudy se z hostitele dostaneme k API enginu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Tcp { port: u16 },
    /// Unix socket enginu (na Windows se nepodporuje, varianta tu je kvůli úplným `match`).
    #[allow(dead_code)] // zatím ho nikdo nevytváří; použije ho hostitel Linux (další úkol)
    Unix(std::path::PathBuf),
}
