//! HTTP klient k enginu: adresa endpointu, spojení ([`conn`]) a HTTP/1.1 klient ([`client`]).

pub mod client;
pub mod conn;
mod endpoint;

pub use endpoint::Endpoint;
