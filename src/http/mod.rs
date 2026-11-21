//! HTTP klient k enginu: adresa endpointu, spojení ([`conn`]) a HTTP/1.1 klient ([`client`]).

pub mod client;
pub mod conn;

/// Kudy se z hostitele dostaneme k API enginu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Tcp { port: u16 },
}
