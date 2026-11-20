//! Standardní pojmenovaná roura Dockeru `\\.\pipe\docker_engine` pro nástroje: Docker CLI,
//! Testcontainers (Java i .NET), VS Code a SDK ji použijí samy, když DOCKER_HOST není nastavená.
//!
//! Aplikace (dokud běží) přijímá spojení na rouře a každé přepošle na API enginu:
//! v režimu TLS přes rustls s klientským certifikátem aplikace na 127.0.0.1:<port>,
//! jinak obyčejným TCP. Nástroje tak nepotřebují nešifrovaný key.pem a roura je
//! dostupná jen aktuálnímu uživateli (DACL: uživatel + SYSTEM, bez vzdálených klientů) –
//! obdoba `docker_engine` roury Docker Desktopu (nálezy F-01, F-02).
//!
//! Každé spojení má vlastní vlákno a druhé vlákno pro opačný směr; žádné async.

use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Standardní roura Dockeru ve Windows (jako Docker Desktop) – DOCKER_HOST pak není potřeba.
/// Když ji drží Docker Desktop, relay se nespustí (FILE_FLAG_FIRST_PIPE_INSTANCE).
pub const PIPE_NAME: &str = r"\\.\pipe\docker_engine";

/// Velikost bloku pro přeposílání dat.
const CHUNK: usize = 64 * 1024;

/// Kam přeposílat jedno spojení.
#[derive(Clone)]
pub enum Target {
    /// obyčejné TCP na 127.0.0.1:<port>
    Plain(u16),
    /// TLS s klientským certifikátem na 127.0.0.1:<port>
    Tls(u16, Arc<rustls::ClientConfig>),
}

/// Zjistí cíl pro nové spojení (čte se při každém spojení – změna režimu se projeví hned).
pub type TargetFn = Arc<dyn Fn() -> Result<Target, String> + Send + Sync>;

/// `npipe:////./pipe/x` (i `npipe://./pipe/x`) -> `\\.\pipe\x`; None, pokud to není místní roura.
pub fn pipe_from_docker_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("npipe:")?.trim_start_matches('/');
    let name = rest.strip_prefix("./pipe/")?;
    (!name.is_empty() && !name.contains(['/', '\\'])).then(|| format!(r"\\.\pipe\{name}"))
}

/// SDDL bezpečnostního popisovače roury: chráněná DACL (bez dědění), plný přístup jen pro
/// daného uživatele (SID) a SYSTEM. Ostatní uživatelé (i Administrators) nemají přístup.
pub fn sddl_for_sid(sid: &str) -> Result<String, String> {
    let ok = sid.len() > 4
        && sid.starts_with("S-1-")
        && sid[4..].split('-').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if !ok {
        return Err(tf!("neplatný SID: {sid}", "invalid SID: {sid}"));
    }
    Ok(format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)"))
}

/// Odpověď 502 pro klienta, když se nepodaří spojit s API enginu (aby `docker` ukázal důvod
/// místo uzavřeného spojení).
pub fn error_response(msg: &str) -> Vec<u8> {
    let body = format!("{{\"message\":{}}}\n", serde_json::Value::String(format!("WLS Desktop: {msg}")));
    format!(
        "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// Přeposílá data z `r` do `w`, dokud `r` nevrátí konec. Vrací počet bajtů.
pub fn pump<R: Read, W: Write>(mut r: R, mut w: W) -> io::Result<u64> {
    let mut buf = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => return Ok(total),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        w.write_all(&buf[..n])?;
        w.flush()?;
        total += n as u64;
    }
}

// ---------------------------------------------------------------- Win32

#[cfg(windows)]
mod win {
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, LocalFree, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, FALSE, HANDLE,
        INVALID_HANDLE_VALUE, TRUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER};
    use windows_sys::Win32::Storage::FileSystem::{
        FlushFileBuffers, ReadFile, WriteFile, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::{CreateEventW, GetCurrentProcess, OpenProcessToken, WaitForSingleObject};
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

    /// Jak často čekající operace kontroluje příznak ukončení (ms).
    const POLL_MS: u32 = 200;

    /// Vlastněný kernel handle – zavře se při drop.
    struct Owned(HANDLE);
    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
                // SAFETY: handle vlastníme a nikdo jiný ho nezavírá.
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    pub fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// SID aktuálního uživatele jako text (S-1-5-21-…).
    pub fn current_user_sid() -> Result<String, String> {
        let mut tok: HANDLE = std::ptr::null_mut();
        // SAFETY: GetCurrentProcess vrací pseudo-handle (nezavírá se); tok je výstupní parametr.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) } == 0 {
            return Err(format!("OpenProcessToken: {}", io::Error::last_os_error()));
        }
        let tok = Owned(tok);
        let mut len = 0u32;
        // SAFETY: první volání jen zjistí potřebnou velikost (selže s ERROR_INSUFFICIENT_BUFFER).
        unsafe { GetTokenInformation(tok.0, TokenUser, std::ptr::null_mut(), 0, &mut len) };
        if len == 0 {
            return Err(format!("GetTokenInformation: {}", io::Error::last_os_error()));
        }
        // Vec<u64> kvůli zarovnání struktury TOKEN_USER (obsahuje ukazatel)
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        // SAFETY: buffer má alespoň `len` bajtů a je zarovnaný na 8.
        if unsafe { GetTokenInformation(tok.0, TokenUser, buf.as_mut_ptr().cast(), len, &mut len) } == 0 {
            return Err(format!("GetTokenInformation: {}", io::Error::last_os_error()));
        }
        // SAFETY: GetTokenInformation(TokenUser) zapsal na začátek bufferu TOKEN_USER; SID ukazuje
        // dovnitř stejného bufferu, který žije až do konce funkce.
        let sid = unsafe { (*(buf.as_ptr() as *const TOKEN_USER)).User.Sid };
        let mut s: *mut u16 = std::ptr::null_mut();
        // SAFETY: sid je platný; výsledný řetězec alokuje systém (LocalAlloc) a uvolníme ho LocalFree.
        if unsafe { ConvertSidToStringSidW(sid, &mut s) } == 0 {
            return Err(format!("ConvertSidToStringSidW: {}", io::Error::last_os_error()));
        }
        // SAFETY: s je řetězec ukončený nulou, platný do LocalFree.
        let out = unsafe {
            let mut n = 0;
            while *s.add(n) != 0 {
                n += 1;
            }
            let v = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
            LocalFree(s.cast());
            v
        };
        Ok(out)
    }

    /// Bezpečnostní popisovač ze SDDL (uvolní se LocalFree při drop).
    pub struct SecDesc(PSECURITY_DESCRIPTOR);
    // SAFETY: popisovač je neměnná paměť alokovaná systémem; jen ho předáváme CreateNamedPipeW.
    unsafe impl Send for SecDesc {}
    unsafe impl Sync for SecDesc {}
    impl Drop for SecDesc {
        fn drop(&mut self) {
            // SAFETY: paměť alokovala ConvertStringSecurityDescriptorToSecurityDescriptorW (LocalAlloc).
            unsafe { LocalFree(self.0) };
        }
    }

    pub fn security_descriptor(sddl: &str) -> Result<SecDesc, String> {
        let w = wide(sddl);
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: w je řetězec ukončený nulou; sd je výstupní parametr.
        let ok = unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) };
        if ok == 0 {
            return Err(format!("ConvertStringSecurityDescriptorToSecurityDescriptorW: {}", io::Error::last_os_error()));
        }
        Ok(SecDesc(sd))
    }

    /// Jedna instance roury (strana serveru, overlapped I/O). Čtení a zápis mohou běžet
    /// současně ze dvou vláken – každá operace má vlastní OVERLAPPED a událost. (U synchronního
    /// handle by se čtení a zápis na stejném objektu serializovaly a obousměrný přenos by se zasekl.)
    pub struct Pipe {
        h: Owned,
        /// spojení se ukončuje – čekající operace se zruší
        closing: AtomicBool,
        /// relay se vypíná
        stop: Arc<AtomicBool>,
    }

    // SAFETY: kernel handle lze používat z libovolného vlákna; stav je jen v atomických proměnných.
    unsafe impl Send for Pipe {}
    unsafe impl Sync for Pipe {}

    impl Pipe {
        /// Vytvoří instanci roury. `first` = FILE_FLAG_FIRST_PIPE_INSTANCE: když rouru se stejným
        /// názvem už vytvořil jiný proces (squatting), vytvoření selže.
        pub fn create(name: &[u16], first: bool, sd: &SecDesc, stop: Arc<AtomicBool>) -> io::Result<Pipe> {
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: FALSE,
            };
            let mut open = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
            if first {
                open |= FILE_FLAG_FIRST_PIPE_INSTANCE;
            }
            // SAFETY: name je ukončený nulou, sa i popisovač žijí po dobu volání.
            let h = unsafe {
                CreateNamedPipeW(
                    name.as_ptr(),
                    open,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                    PIPE_UNLIMITED_INSTANCES,
                    super::CHUNK as u32,
                    super::CHUNK as u32,
                    0,
                    &sa,
                )
            };
            if h == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            Ok(Pipe { h: Owned(h), closing: AtomicBool::new(false), stop })
        }

        fn cancelled(&self) -> bool {
            self.closing.load(Ordering::SeqCst) || self.stop.load(Ordering::SeqCst)
        }

        /// Ukončí spojení: čekající i další čtení/zápisy skončí (do POLL_MS).
        pub fn abort(&self) {
            self.closing.store(true, Ordering::SeqCst);
        }

        /// Spustí overlapped operaci `start` a počká na její dokončení; při zrušení
        /// (abort / stop relay) ji zruší CancelIoEx a počká, než systém přestane OVERLAPPED používat.
        /// `done_code` = chybový kód, který znamená „hotovo bez čekání“ (ERROR_PIPE_CONNECTED).
        fn overlapped(&self, done_code: u32, start: impl FnOnce(*mut OVERLAPPED) -> i32) -> io::Result<u32> {
            if self.cancelled() {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "zrušeno"));
            }
            // SAFETY: ruční reset, nesignalizovaná, bez názvu.
            let ev = unsafe { CreateEventW(std::ptr::null(), TRUE, FALSE, std::ptr::null()) };
            if ev.is_null() {
                return Err(io::Error::last_os_error());
            }
            let ev = Owned(ev);
            // SAFETY: OVERLAPPED je POD, nulová hodnota je platná.
            let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
            ov.hEvent = ev.0;
            // `ov` i `ev` žijí na zásobníku až do konce funkce a funkce se nevrátí, dokud operace
            // běží (viz čekání níže) – systém tedy nikdy nepíše do uvolněné paměti.
            if start(&mut ov) == 0 {
                // SAFETY: bez parametrů.
                let e = unsafe { GetLastError() };
                if e == done_code {
                    return Ok(0);
                }
                if e != ERROR_IO_PENDING {
                    return Err(io::Error::from_raw_os_error(e as i32));
                }
            }
            loop {
                // SAFETY: ev je platná událost.
                match unsafe { WaitForSingleObject(ev.0, POLL_MS) } {
                    WAIT_OBJECT_0 => break,
                    WAIT_TIMEOUT if !self.cancelled() => continue,
                    w => {
                        // zrušit a počkat na dokončení (bWait=TRUE), teprve pak smí `ov` zaniknout
                        let mut n = 0u32;
                        // SAFETY: h a ov jsou platné; GetOverlappedResult s TRUE čeká na dokončení zrušení.
                        unsafe {
                            CancelIoEx(self.h.0, &ov);
                            GetOverlappedResult(self.h.0, &ov, &mut n, TRUE);
                        }
                        return Err(if w == WAIT_TIMEOUT {
                            io::Error::new(io::ErrorKind::ConnectionAborted, "zrušeno")
                        } else {
                            io::Error::last_os_error()
                        });
                    }
                }
            }
            let mut n = 0u32;
            // SAFETY: operace je dokončená (událost signalizovaná).
            if unsafe { GetOverlappedResult(self.h.0, &ov, &mut n, FALSE) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(n)
        }

        /// Počká na klienta. Ok(false) = relay se vypíná.
        pub fn connect(&self) -> io::Result<bool> {
            // SAFETY: h je platná instance roury, ov předává `overlapped`.
            match self.overlapped(ERROR_PIPE_CONNECTED, |ov| unsafe { ConnectNamedPipe(self.h.0, ov) }) {
                Ok(_) => Ok(true),
                Err(_) if self.stop.load(Ordering::SeqCst) => Ok(false),
                Err(e) => Err(e),
            }
        }

        /// Čtení od klienta; 0 = klient zavřel rouru nebo se spojení ukončuje.
        pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
            let len = buf.len().min(u32::MAX as usize) as u32;
            let p = buf.as_mut_ptr();
            // SAFETY: buf žije déle než operace (overlapped čeká na její dokončení).
            match self.overlapped(u32::MAX, |ov| unsafe { ReadFile(self.h.0, p, len, std::ptr::null_mut(), ov) }) {
                Ok(n) => Ok(n as usize),
                Err(e) if e.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) || e.kind() == io::ErrorKind::ConnectionAborted => Ok(0),
                Err(e) => Err(e),
            }
        }

        pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
            let len = buf.len().min(u32::MAX as usize) as u32;
            let p = buf.as_ptr();
            // SAFETY: buf žije déle než operace (overlapped čeká na její dokončení).
            self.overlapped(u32::MAX, |ov| unsafe { WriteFile(self.h.0, p, len, std::ptr::null_mut(), ov) }).map(|n| n as usize)
        }

        /// Počká, než klient přečte vše zapsané (jinak by se data při zavření ztratila).
        pub fn flush(&self) {
            // SAFETY: h je platný handle roury.
            unsafe { FlushFileBuffers(self.h.0) };
        }
    }
}

#[cfg(windows)]
use win::Pipe;

#[cfg(windows)]
impl Read for &Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Pipe::read(self, buf)
    }
}

#[cfg(windows)]
impl Write for &Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Pipe::write(self, buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Běžící relay. Drop ho zastaví (přijímací vlákno i otevřená spojení skončí do ~200 ms).
pub struct Relay {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Relay {
    /// Vytvoří rouru `name` (první instanci synchronně, aby se chyba – např. obsazený název –
    /// vrátila hned) a spustí přijímací vlákno.
    #[cfg(windows)]
    pub fn start(name: &str, target: TargetFn) -> Result<Relay, String> {
        let sd = win::security_descriptor(&sddl_for_sid(&win::current_user_sid()?)?)?;
        let wname = win::wide(name);
        let stop = Arc::new(AtomicBool::new(false));
        let first = Pipe::create(&wname, true, &sd, stop.clone()).map_err(|e| format!("{name}: {e}"))?;
        let st = stop.clone();
        let thread = thread::Builder::new()
            .name("npipe-accept".into())
            .spawn(move || accept_loop(wname, sd, first, st, target))
            .map_err(|e| e.to_string())?;
        Ok(Relay { stop, thread: Some(thread) })
    }

    #[cfg(not(windows))]
    pub fn start(_name: &str, _target: TargetFn) -> Result<Relay, String> {
        Err("named pipes are Windows only".into())
    }
}

#[cfg(windows)]
fn accept_loop(wname: Vec<u16>, sd: win::SecDesc, first: Pipe, stop: Arc<AtomicBool>, target: TargetFn) {
    let mut next = Some(first);
    while !stop.load(Ordering::SeqCst) {
        let pipe = match next.take() {
            Some(p) => p,
            None => match Pipe::create(&wname, false, &sd, stop.clone()) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("npipe: {e}");
                    thread::sleep(Duration::from_millis(500));
                    continue;
                }
            },
        };
        match pipe.connect() {
            Ok(true) => {}
            Ok(false) => return,
            // klient se odpojil hned po připojení apod. – instanci zahodit a vytvořit novou
            Err(_) => continue,
        }
        // další instanci připravit hned, ať klienti nečekají
        next = Pipe::create(&wname, false, &sd, stop.clone()).ok();
        let t = target.clone();
        let _ = thread::Builder::new().name("npipe-conn".into()).spawn(move || serve(Arc::new(pipe), t));
    }
}

#[cfg(windows)]
fn serve(pipe: Arc<Pipe>, target: TargetFn) {
    let res = target().and_then(|t| {
        let port = match &t {
            Target::Plain(p) | Target::Tls(p, _) => *p,
        };
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|e| format!("127.0.0.1:{port}: {e}"))?;
        let _ = tcp.set_nodelay(true);
        Ok((t, tcp))
    });
    match res {
        Ok((Target::Plain(_), tcp)) => relay_plain(&pipe, tcp),
        Ok((Target::Tls(_, cfg), tcp)) => {
            if let Err(e) = relay_tls(&pipe, tcp, cfg) {
                let _ = (&*pipe).write_all(&error_response(&e));
            }
        }
        Err(e) => {
            let _ = (&*pipe).write_all(&error_response(&e));
        }
    }
    pipe.flush();
    pipe.abort();
}

/// Obyčejné TCP: dvě vlákna, každé jeden směr.
#[cfg(windows)]
fn relay_plain(pipe: &Arc<Pipe>, tcp: TcpStream) {
    let Ok(up_tcp) = tcp.try_clone() else { return };
    let up_pipe = pipe.clone();
    // klient -> engine; když klient zavře rouru, zavřít i TCP (tím skončí i druhý směr)
    let up = thread::spawn(move || {
        let _ = pump(&*up_pipe, &up_tcp);
        let _ = up_tcp.shutdown(std::net::Shutdown::Both);
    });
    // engine -> klient
    let _ = pump(&tcp, &**pipe);
    pipe.flush();
    pipe.abort();
    let _ = tcp.shutdown(std::net::Shutdown::Both);
    let _ = up.join();
}

/// TLS: handshake, pak dvě vlákna nad sdíleným `ClientConnection` (zámek se drží jen při
/// zpracování TLS záznamů, ne při čekání na síť nebo rouru). Err = handshake selhal.
#[cfg(windows)]
fn relay_tls(pipe: &Arc<Pipe>, tcp: TcpStream, cfg: Arc<rustls::ClientConfig>) -> Result<(), String> {
    let server = rustls_pki_types::ServerName::IpAddress(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST).into());
    let mut conn = rustls::ClientConnection::new(cfg, server).map_err(|e| e.to_string())?;
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(10)));
    {
        let mut s = &tcp;
        while conn.is_handshaking() {
            conn.complete_io(&mut s).map_err(|e| format!("TLS: {e}"))?;
        }
    }
    let _ = tcp.set_read_timeout(None);
    let conn = Arc::new(Mutex::new(conn));
    let Ok(up_tcp) = tcp.try_clone() else { return Ok(()) };
    let (up_pipe, up_conn) = (pipe.clone(), conn.clone());
    // klient -> engine
    let up = thread::spawn(move || {
        let mut buf = vec![0u8; CHUNK];
        let send = |c: &mut rustls::ClientConnection| -> io::Result<()> {
            while c.wants_write() {
                c.write_tls(&mut &up_tcp)?;
            }
            Ok(())
        };
        loop {
            let n = match up_pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let Ok(mut c) = up_conn.lock() else { break };
            if c.writer().write_all(&buf[..n]).and_then(|_| send(&mut c)).is_err() {
                break;
            }
        }
        if let Ok(mut c) = up_conn.lock() {
            c.send_close_notify();
            let _ = send(&mut c);
        }
        let _ = up_tcp.shutdown(std::net::Shutdown::Both);
    });
    // engine -> klient
    let _ = tls_down(&tcp, &conn, pipe);
    pipe.flush();
    pipe.abort();
    let _ = tcp.shutdown(std::net::Shutdown::Both);
    let _ = up.join();
    Ok(())
}

#[cfg(windows)]
fn tls_down(tcp: &TcpStream, conn: &Mutex<rustls::ClientConnection>, pipe: &Pipe) -> io::Result<()> {
    let mut raw = vec![0u8; CHUNK];
    let mut tmp = vec![0u8; CHUNK];
    loop {
        let n = (&mut &*tcp).read(&mut raw)?;
        if n == 0 {
            return Ok(());
        }
        let mut plain = Vec::new();
        let mut eof = false;
        {
            let mut c = conn.lock().map_err(|_| io::Error::other("lock"))?;
            let mut rd = &raw[..n];
            while !rd.is_empty() {
                c.read_tls(&mut rd)?;
                c.process_new_packets().map_err(io::Error::other)?;
                // vybírat průběžně, jinak se zaplní buffer dešifrovaných dat
                loop {
                    match c.reader().read(&mut tmp) {
                        Ok(0) => {
                            eof = true;
                            break;
                        }
                        Ok(k) => plain.extend_from_slice(&tmp[..k]),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) => return Err(e),
                    }
                }
            }
            while c.wants_write() {
                c.write_tls(&mut &*tcp)?;
            }
        }
        if !plain.is_empty() {
            (&mut &*pipe).write_all(&plain)?;
        }
        if eof {
            return Ok(());
        }
    }
}

// ---------------------------------------------------------------- relay aplikace

/// Stav relay aplikace (pro UI).
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Off,
    Running,
    Failed(String),
}

/// Relay aplikace – zamyká jen vlákno z `apply` (start / zastavení může chvíli trvat).
static APP_RELAY: Mutex<Option<Relay>> = Mutex::new(None);
/// Stav pro UI – drží se jen na okamžik, takže čtení z UI vlákna neblokuje.
static APP_STATE: Mutex<State> = Mutex::new(State::Off);

pub fn state() -> State {
    APP_STATE.lock().map(|g| g.clone()).unwrap_or(State::Off)
}

/// Zapne / vypne jednu rouru; vrací nový stav.
fn switch(slot: &Mutex<Option<Relay>>, name: &str, on: bool, target: impl FnOnce() -> TargetFn) -> State {
    let Ok(mut g) = slot.lock() else { return State::Failed("lock".into()) };
    if !on {
        // Drop počká na přijímací vlákno, takže název je pak hned volný pro nové zapnutí
        *g = None;
        return State::Off;
    }
    if g.is_some() {
        return State::Running;
    }
    match Relay::start(name, target()) {
        Ok(r) => {
            *g = Some(r);
            State::Running
        }
        Err(e) => State::Failed(e),
    }
}

/// Cíl podle aktuální konfigurace aplikace (port, TLS klient aplikace).
pub fn app_target(cfg: Arc<RwLock<crate::config::Config>>) -> TargetFn {
    Arc::new(move || {
        let (port, tls) = cfg.read().map(|c| (c.port, c.tls)).map_err(|e| e.to_string())?;
        if !tls {
            return Ok(Target::Plain(port));
        }
        crate::docker::current_tls()
            .map(|c| Target::Tls(port, c))
            .ok_or_else(|| t!("TLS klient není načtený", "The TLS client is not loaded").to_string())
    })
}

/// Zapne / vypne rouru aplikace na pozadí (neblokuje UI); `done` se zavolá po změně stavu.
pub fn apply(on: bool, cfg: Arc<RwLock<crate::config::Config>>, done: impl FnOnce() + Send + 'static) {
    thread::spawn(move || {
        let st = match switch(&APP_RELAY, PIPE_NAME, on, || app_target(cfg.clone())) {
            State::Failed(e) => State::Failed(tf!("{e} (rouru nejspíš drží Docker Desktop)", "{e} (probably held by Docker Desktop)")),
            s => s,
        };
        if let Ok(mut s) = APP_STATE.lock() {
            *s = st;
        }
        done();
    });
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn docker_host_roundtrip() {
        assert_eq!(pipe_from_docker_host("npipe:////./pipe/docker_engine").as_deref(), Some(PIPE_NAME));
        assert_eq!(pipe_from_docker_host("npipe://./pipe/docker_engine").as_deref(), Some(PIPE_NAME));
        assert_eq!(pipe_from_docker_host("npipe:///./pipe/x").as_deref(), Some(r"\\.\pipe\x"));
        assert_eq!(pipe_from_docker_host("tcp://127.0.0.1:2375"), None);
        assert_eq!(pipe_from_docker_host("npipe:////server/pipe/x"), None);
        assert_eq!(pipe_from_docker_host("npipe:////./pipe/"), None);
    }

    #[test]
    fn sddl_only_user_and_system() {
        let s = sddl_for_sid("S-1-5-21-1004336348-1177238915-682003330-512").unwrap();
        assert_eq!(s, "D:P(A;;GA;;;S-1-5-21-1004336348-1177238915-682003330-512)(A;;GA;;;SY)");
        // žádné vkládání dalších ACE přes „SID“
        for bad in ["", "S-1-", "S-1-5-21)(A;;GA;;;WD", "WD", "S-1-5--1", "s-1-5-18"] {
            assert!(sddl_for_sid(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn error_response_is_valid_http() {
        let r = String::from_utf8(error_response("127.0.0.1:2376: \"refused\"")).unwrap();
        let (head, body) = r.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("HTTP/1.1 502 "));
        let len: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).unwrap().parse().unwrap();
        assert_eq!(len, body.len());
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert!(v["message"].as_str().unwrap().contains("\"refused\""));
    }

    #[test]
    fn pump_copies_everything() {
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let mut out = Vec::new();
        assert_eq!(pump(std::io::Cursor::new(&data), &mut out).unwrap(), data.len() as u64);
        assert_eq!(out, data);
    }

    #[cfg(windows)]
    #[test]
    fn current_user_sid_is_valid() {
        let sid = win::current_user_sid().unwrap();
        let sddl = sddl_for_sid(&sid).unwrap();
        // systém SDDL přijme
        win::security_descriptor(&sddl).unwrap();
    }

    /// Celý relay proti falešnému „engine“ na náhodném portu (bez WSL).
    #[cfg(windows)]
    #[test]
    fn relay_plain_against_local_listener() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            for s in l.incoming().take(2) {
                let mut s = s.unwrap();
                let mut buf = [0u8; 1024];
                let n = s.read(&mut buf).unwrap();
                assert!(buf[..n].starts_with(b"GET /_ping"));
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK").unwrap();
            }
        });
        let name = format!(r"\\.\pipe\wls_test_unit_{}", std::process::id());
        let relay = Relay::start(&name, Arc::new(move || Ok(Target::Plain(port)))).unwrap();
        // druhý relay se stejným názvem nesmí vzniknout (FILE_FLAG_FIRST_PIPE_INSTANCE)
        assert!(Relay::start(&name, Arc::new(move || Ok(Target::Plain(port)))).is_err());
        for _ in 0..2 {
            let r = ping_pipe(&name);
            assert!(r.starts_with("HTTP/1.1 200") && r.ends_with("OK"), "{r}");
        }
        drop(relay);
    }

    #[cfg(windows)]
    #[test]
    fn relay_reports_unreachable_engine() {
        // volný port: bind a hned zavřít
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let name = format!(r"\\.\pipe\wls_test_unreach_{}", std::process::id());
        let _relay = Relay::start(&name, Arc::new(move || Ok(Target::Plain(port)))).unwrap();
        let r = ping_pipe(&name);
        assert!(r.starts_with("HTTP/1.1 502"), "{r}");
    }

    /// Pošle `GET /_ping` přes rouru (obyčejný std soubor) a vrátí celou odpověď.
    #[cfg(windows)]
    pub(crate) fn ping_pipe(name: &str) -> String {
        get_via_pipe(name, "/_ping")
    }

    /// Pošle `GET <path>` (Connection: close) přes rouru a vrátí celou odpověď.
    #[cfg(windows)]
    pub(crate) fn get_via_pipe(name: &str, path: &str) -> String {
        let mut f = None;
        for _ in 0..50 {
            match std::fs::OpenOptions::new().read(true).write(true).open(name) {
                Ok(x) => {
                    f = Some(x);
                    break;
                }
                // ERROR_PIPE_BUSY – další instance se právě vytváří
                Err(_) => thread::sleep(Duration::from_millis(20)),
            }
        }
        let mut f = f.expect("roura se neotevřela");
        f.write_all(format!("GET {path} HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n\r\n").as_bytes()).unwrap();
        let mut out = String::new();
        let _ = f.read_to_string(&mut out);
        out
    }
}
