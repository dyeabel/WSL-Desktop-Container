//! Spojení, po kterém jde HTTP: zatím TCP na 127.0.0.1 (volitelně TLS). Další druhy
//! (unix socket, stream agenta v hostu) přibudou jako další implementace [`Connect`].

// Spojení zatím používají jen testy klienta; docker.rs na ně přejde v dalším kroku (Task 6).
#![cfg_attr(not(test), allow(dead_code))]

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::Duration;

/// Obousměrné spojení k enginu.
pub trait Stream: Read + Write + Send {
    /// Časový limit jednoho čtení (`None` = bez limitu).
    fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()>;
    /// Funkce, jejíž volání ukončí spojení v obou směrech – i z jiného vlákna, kde
    /// zrovna visí blokující čtení.
    fn shutdown_handle(&self) -> io::Result<Box<dyn Fn() + Send + Sync>>;
}

/// Jak navázat nové spojení k enginu.
pub trait Connect: Send + Sync {
    fn connect(&self, timeout: Duration) -> io::Result<Box<dyn Stream>>;
    /// Popis pro hlášky, např. `"127.0.0.1:2376"`.
    fn describe(&self) -> String;
    /// Hodnota hlavičky `Host`.
    fn host_header(&self) -> String;
}

fn tcp_shutdown(s: &TcpStream) -> io::Result<Box<dyn Fn() + Send + Sync>> {
    let s = s.try_clone()?;
    Ok(Box::new(move || {
        let _ = s.shutdown(Shutdown::Both);
    }))
}

impl Stream for TcpStream {
    fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        TcpStream::set_read_timeout(self, t)
    }

    fn shutdown_handle(&self) -> io::Result<Box<dyn Fn() + Send + Sync>> {
        tcp_shutdown(self)
    }
}

impl Stream for rustls::StreamOwned<rustls::ClientConnection, TcpStream> {
    fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        self.sock.set_read_timeout(t)
    }

    fn shutdown_handle(&self) -> io::Result<Box<dyn Fn() + Send + Sync>> {
        tcp_shutdown(&self.sock)
    }
}

/// TCP na `127.0.0.1:port`, s TLS (klientský certifikát), je-li `tls` zadané.
pub struct TcpConnect {
    pub port: u16,
    pub tls: Option<Arc<rustls::ClientConfig>>,
}

impl Connect for TcpConnect {
    fn connect(&self, timeout: Duration) -> io::Result<Box<dyn Stream>> {
        let addr = SocketAddr::from(([127, 0, 0, 1], self.port));
        let sock = TcpStream::connect_timeout(&addr, timeout)?;
        let _ = sock.set_nodelay(true);
        match &self.tls {
            None => Ok(Box::new(sock)),
            Some(cfg) => {
                let name = rustls_pki_types::ServerName::try_from("127.0.0.1").map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
                let conn = rustls::ClientConnection::new(cfg.clone(), name).map_err(io::Error::other)?;
                Ok(Box::new(rustls::StreamOwned::new(conn, sock)))
            }
        }
    }

    fn describe(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    fn host_header(&self) -> String {
        self.describe()
    }
}
