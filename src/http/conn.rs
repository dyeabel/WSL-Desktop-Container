//! Spojení, po kterém jde HTTP: zatím TCP na 127.0.0.1 (volitelně TLS). Další druhy
//! (unix socket, stream agenta v hostu) přibudou jako další implementace [`Connect`].

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::PathBuf;
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

/// Unix socket enginu (Docker/Podman na Linuxu). Na Windows se nepodporuje.
pub struct UnixConnect {
    pub path: PathBuf,
}

#[cfg(unix)]
impl Stream for std::os::unix::net::UnixStream {
    fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()> {
        std::os::unix::net::UnixStream::set_read_timeout(self, t)
    }

    fn shutdown_handle(&self) -> io::Result<Box<dyn Fn() + Send + Sync>> {
        let s = self.try_clone()?;
        Ok(Box::new(move || {
            let _ = s.shutdown(Shutdown::Both);
        }))
    }
}

impl Connect for UnixConnect {
    #[cfg(unix)]
    fn connect(&self, _timeout: Duration) -> io::Result<Box<dyn Stream>> {
        // Unix socket odpoví hned, časový limit připojení se nepoužije.
        match std::os::unix::net::UnixStream::connect(&self.path) {
            Ok(s) => Ok(Box::new(s)),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                let path = self.path.display();
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    crate::tf!(
                        "Nemáte přístup k {path}: přidejte se do skupiny docker (pak se odhlaste a přihlaste), nebo použijte rootless Podman: systemctl --user enable --now podman.socket",
                        "No access to {path}: join the docker group (then log out and back in), or use rootless Podman: systemctl --user enable --now podman.socket",
                    ),
                ))
            }
            Err(e) => Err(e),
        }
    }

    #[cfg(not(unix))]
    fn connect(&self, _timeout: Duration) -> io::Result<Box<dyn Stream>> {
        Err(io::Error::other(crate::t!("Unix socket není na Windows podporovaný", "Unix sockets are not supported on Windows")))
    }

    fn describe(&self) -> String {
        format!("unix://{}", self.path.display())
    }

    fn host_header(&self) -> String {
        "localhost".into()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wslcd-unix-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn unix_describe() {
        let c = UnixConnect { path: "/run/docker.sock".into() };
        assert_eq!(c.describe(), "unix:///run/docker.sock");
        assert_eq!(c.host_header(), "localhost");
    }

    #[test]
    fn unix_permission_denied_has_hint() {
        let d = dir("perm");
        let p = d.join("s.sock");
        let _l = UnixListener::bind(&p).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
        // root práva ignoruje: connect by uspěl
        let c = UnixConnect { path: p.clone() };
        match c.connect(Duration::from_millis(500)) {
            Ok(_) => eprintln!("přeskočeno: běží jako root, socket s právy 000 jde otevřít"),
            Err(e) => {
                assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
                let m = e.to_string();
                assert!(m.contains("skupiny docker") && m.contains("podman.socket"), "{m}");
                assert!(m.contains(p.to_str().unwrap()), "{m}");
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unix_stream_timeout_and_shutdown() {
        let d = dir("shut");
        let p = d.join("s.sock");
        let l = UnixListener::bind(&p).unwrap();
        let mut st = UnixConnect { path: p }.connect(Duration::from_millis(500)).unwrap();
        let _srv = l.accept().unwrap();
        st.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
        let mut b = [0u8; 1];
        assert!(st.read(&mut b).is_err());
        st.set_read_timeout(None).unwrap();
        let h = st.shutdown_handle().unwrap();
        h();
        assert_eq!(st.read(&mut b).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }
}
