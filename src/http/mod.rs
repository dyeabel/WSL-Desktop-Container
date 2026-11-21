//! HTTP klient k enginu. Zatím jen adresa endpointu; zbytek modulu přibude později.

/// Kudy se z hostitele dostaneme k API enginu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Tcp { port: u16 },
}
