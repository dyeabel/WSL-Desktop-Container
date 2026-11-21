//! Minimální HTTP/1.1 klient nad obecným spojením ([`Connect`]) s keep-alive poolem.
//!
//! Pravidla poolu:
//! - spojení se do poolu vrátí, jen když bylo tělo odpovědi dočtené do konce
//!   (`Content-Length` vyčerpaný nebo konec `chunked`) a server neposlal `Connection: close`;
//! - odpověď bez délky a bez `chunked` (tělo do zavření) se do poolu nevrací nikdy;
//! - spojení nečinné déle než [`IDLE_TTL`] se zahodí, v poolu je nejvýš [`MAX_IDLE`] spojení;
//! - selže-li spojení z poolu dřív, než přijde první bajt odpovědi, zopakuje se požadavek
//!   jednou na novém spojení – jen u GET, HEAD, DELETE, PUT, OPTIONS a TRACE bez těla
//!   (jako dřívější ureq);
//! - odpověď `101 Switching Protocols` se nevrací nikdy: tělo je surový proud až do zavření.

use super::conn::{Connect, Stream};
use crate::error::ApiError;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Jak dlouho smí spojení čekat v poolu. Musí bezpečně pokrýt interval obnovy aplikace
/// (výchozí 3 s, s událostmi až 30 s), jinak by se při každé obnově navazovalo nové spojení
/// včetně TLS handshake.
const IDLE_TTL: Duration = Duration::from_secs(60);
/// Nejvýš tolik nečinných spojení v poolu.
const MAX_IDLE: usize = 8;
/// Velikost čtecího bufferu spojení.
const BUF: usize = 16 * 1024;
/// Kolik z těla chybové odpovědi se přečte kvůli hlášce.
const ERR_BODY_MAX: u64 = 1024 * 1024;

type Conn = BufReader<Box<dyn Stream>>;

// ---------------------------------------------------------------- pool

struct Pool {
    /// nečinná spojení, nejnovější na konci
    idle: Mutex<Vec<(Conn, Instant)>>,
    /// jak dlouho smí spojení čekat (v aplikaci vždy [`IDLE_TTL`])
    ttl: Duration,
}

impl Pool {
    fn new(ttl: Duration) -> Self {
        Pool { idle: Mutex::new(Vec::new()), ttl }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(Conn, Instant)>> {
        self.idle.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Nejnovější použitelné spojení. Prošlá spojení zahodí (zavřou se až mimo zámek).
    fn take(&self) -> Option<Conn> {
        let (got, _stale) = {
            let mut g = self.lock();
            let stale = drain_stale(&mut g, self.ttl);
            (g.pop().map(|(c, _)| c), stale)
        };
        got
    }

    /// Vrátí dočtené spojení. Se zbytkem dat v bufferu (server poslal víc, než měl) se nevrací.
    fn put(&self, c: Conn) {
        if !c.buffer().is_empty() {
            return;
        }
        let _drop = {
            let mut g = self.lock();
            let mut stale = drain_stale(&mut g, self.ttl);
            if g.len() >= MAX_IDLE {
                stale.push(g.remove(0).0);
            }
            g.push((c, Instant::now()));
            stale
        };
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }
}

/// Vyjme spojení, která v poolu čekají déle než `ttl`.
fn drain_stale(v: &mut Vec<(Conn, Instant)>, ttl: Duration) -> Vec<Conn> {
    let mut stale = Vec::new();
    let mut i = 0;
    while i < v.len() {
        if v[i].1.elapsed() > ttl {
            stale.push(v.remove(i).0);
        } else {
            i += 1;
        }
    }
    stale
}

// ---------------------------------------------------------------- klient

/// HTTP/1.1 klient s keep-alive. Klonování je levné (sdílený pool).
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    conn: Arc<dyn Connect>,
    connect_timeout: Duration,
    read_timeout: Duration,
    pool: Arc<Pool>,
}

impl Client {
    pub fn new(conn: Arc<dyn Connect>, connect_timeout: Duration, read_timeout: Duration) -> Self {
        Self { inner: Arc::new(Inner { conn, connect_timeout, read_timeout, pool: Arc::new(Pool::new(IDLE_TTL)) }) }
    }

    /// Jiná doba nečinnosti v poolu (testy, aby nemusely čekat [`IDLE_TTL`]).
    /// Jen na čerstvě vytvořeném klientovi.
    #[cfg(test)]
    fn with_idle_ttl(mut self, ttl: Duration) -> Self {
        let inner = Arc::get_mut(&mut self.inner).expect("klient se ještě nesdílí");
        inner.pool = Arc::new(Pool::new(ttl));
        self
    }

    pub fn request(&self, method: &str, path: &str) -> Request<'_> {
        Request { client: self, method: method.to_string(), path: path.to_string(), headers: Vec::new(), timeout: None, bad_header: None }
    }

    /// Počet nečinných spojení v poolu.
    #[cfg(test)]
    fn idle(&self) -> usize {
        self.inner.pool.len()
    }

    fn transport(&self, e: impl std::fmt::Display) -> ApiError {
        ApiError::Transport(format!("{}: {e}", self.inner.conn.describe()))
    }

    fn fresh(&self) -> Result<Conn, ApiError> {
        let s = self.inner.conn.connect(self.inner.connect_timeout).map_err(|e| self.transport(e))?;
        Ok(BufReader::with_capacity(BUF, s))
    }
}

/// Tělo požadavku.
enum Payload<'b> {
    None,
    Bytes(&'b [u8]),
    /// posílá se jako `chunked`; nedá se zopakovat
    Stream(&'b mut dyn Read),
}

/// Jak selhala výměna na jednom spojení.
enum Fail {
    /// spojení zaniklo dřív, než přišel první bajt odpovědi – u idempotentní metody bez těla
    /// a spojení z poolu se zkusí znovu
    Early(io::Error),
    Late(ApiError),
}

/// Spojení zaniklo (server ho zavřel nebo resetoval).
fn is_dead(e: &io::Error) -> bool {
    use io::ErrorKind::*;
    matches!(e.kind(), ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof | NotConnected)
}

pub struct Request<'a> {
    client: &'a Client,
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    timeout: Option<Duration>,
    /// hlavička s CR/LF (nebo prázdným názvem) – požadavek se neodešle, `call`/`send…` vrátí chybu
    bad_header: Option<String>,
}

impl Request<'_> {
    /// Přidá hlavičku. Název nebo hodnota s CR/LF (vložení dalších hlaviček) se odmítne:
    /// požadavek se pak vůbec neodešle a odesílací metoda vrátí `ApiError::Decode`.
    pub fn set(mut self, name: &str, value: &str) -> Self {
        let bad = |s: &str| s.contains(['\r', '\n']);
        if name.trim().is_empty() || name.contains(':') || bad(name) || bad(value) {
            self.bad_header.get_or_insert_with(|| name.escape_debug().to_string());
            return self;
        }
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    /// Časový limit čtení pro tento požadavek (místo výchozího z [`Client::new`]).
    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = Some(t);
        self
    }

    pub fn call(self) -> Result<Response, ApiError> {
        self.execute(Payload::None)
    }

    pub fn send_json(mut self, v: serde_json::Value) -> Result<Response, ApiError> {
        let body = serde_json::to_vec(&v).map_err(|e| ApiError::Decode(e.to_string()))?;
        if !self.has_header("content-type") {
            self = self.set("Content-Type", "application/json");
        }
        self.execute(Payload::Bytes(&body))
    }

    // aplikace posílá jen JSON (`send_json`) a proudy (`send`); syrové bajty zatím jen testy
    #[cfg(test)]
    pub fn send_bytes(self, body: &[u8]) -> Result<Response, ApiError> {
        self.execute(Payload::Bytes(body))
    }

    /// Tělo z čtečky. S hlavičkou `Content-Length` (nastavenou přes [`Request::set`]) se pošle
    /// přesně tolik bajtů bez kódování, jinak jako `Transfer-Encoding: chunked`.
    pub fn send(self, mut body: impl Read) -> Result<Response, ApiError> {
        self.execute(Payload::Stream(&mut body))
    }

    fn has_header(&self, name: &str) -> bool {
        self.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
    }

    /// Délka těla z hlavičky `Content-Length` nastavené volajícím.
    fn content_length(&self) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).map(|(_, v)| v.trim())
    }

    fn is_head(&self) -> bool {
        self.method.eq_ignore_ascii_case("HEAD")
    }

    /// Stavový řádek a hlavičky požadavku.
    fn head(&self, payload: &Payload) -> Vec<u8> {
        let mut h = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: wsl-container-desktop\r\nConnection: keep-alive\r\n",
            self.method,
            self.path,
            self.client.inner.conn.host_header()
        );
        for (k, v) in &self.headers {
            h.push_str(&format!("{k}: {v}\r\n"));
        }
        match payload {
            Payload::None if self.method.eq_ignore_ascii_case("GET") || self.is_head() => {}
            Payload::None => h.push_str("Content-Length: 0\r\n"),
            Payload::Bytes(b) => h.push_str(&format!("Content-Length: {}\r\n", b.len())),
            Payload::Stream(_) if self.content_length().is_some() => {}
            Payload::Stream(_) => h.push_str("Transfer-Encoding: chunked\r\n"),
        }
        h.push_str("\r\n");
        h.into_bytes()
    }

    fn execute(self, mut payload: Payload) -> Result<Response, ApiError> {
        let client = self.client;
        // metoda a cesta jdou do stavového řádku: mezera nebo CR/LF by ho rozbily
        let bad = |s: &str| s.is_empty() || s.contains(['\r', '\n', ' ']);
        if bad(&self.method) {
            return Err(ApiError::Decode(format!("invalid HTTP method {:?}", self.method)));
        }
        if bad(&self.path) {
            return Err(ApiError::Decode(format!("invalid HTTP path {:?}", self.path)));
        }
        if let Some(name) = &self.bad_header {
            return Err(ApiError::Decode(format!("invalid HTTP header {name:?}")));
        }
        // proud s výslovnou délkou: tolik bajtů bez chunked
        let raw_len = match (&payload, self.content_length()) {
            (Payload::Stream(_), Some(v)) => {
                Some(v.parse::<u64>().map_err(|_| ApiError::Decode(format!("invalid Content-Length {v:?}")))?)
            }
            _ => None,
        };
        let head = self.head(&payload);
        // stejně jako ureq: idempotentní metody a jen bez těla
        let idempotent = ["GET", "HEAD", "DELETE", "PUT", "OPTIONS", "TRACE"].iter().any(|m| self.method.eq_ignore_ascii_case(m));
        let no_body = match &payload {
            Payload::None => true,
            Payload::Bytes(b) => b.is_empty(),
            Payload::Stream(_) => false,
        };
        let retryable = idempotent && no_body;
        let read_timeout = self.timeout.unwrap_or(client.inner.read_timeout);
        let mut use_pool = true;
        loop {
            let pooled = if use_pool { client.inner.pool.take() } else { None };
            let from_pool = pooled.is_some();
            let conn = match pooled {
                Some(c) => c,
                None => client.fresh()?,
            };
            match self.exchange(conn, &head, &mut payload, raw_len, read_timeout) {
                Ok((conn, h)) => return self.finish(conn, h),
                // jen jednou a jen na novém spojení
                Err(Fail::Early(_)) if from_pool && retryable => use_pool = false,
                Err(Fail::Early(e)) => return Err(client.transport(e)),
                Err(Fail::Late(e)) => return Err(e),
            }
        }
    }

    /// Pošle požadavek a přečte hlavičky odpovědi.
    fn exchange(&self, mut c: Conn, head: &[u8], payload: &mut Payload, raw_len: Option<u64>, read_timeout: Duration) -> Result<(Conn, Head), Fail> {
        c.get_ref().set_read_timeout(Some(read_timeout)).map_err(Fail::Early)?;
        let w = c.get_mut();
        match payload {
            Payload::None => w.write_all(head).map_err(Fail::Early)?,
            Payload::Bytes(b) if b.len() <= 64 * 1024 => {
                // malé tělo v jednom zápisu s hlavičkami
                let mut all = Vec::with_capacity(head.len() + b.len());
                all.extend_from_slice(head);
                all.extend_from_slice(b);
                w.write_all(&all).map_err(Fail::Early)?;
            }
            Payload::Bytes(b) => w.write_all(head).and_then(|_| w.write_all(b)).map_err(Fail::Early)?,
            Payload::Stream(r) if raw_len.is_some() => {
                let len = raw_len.unwrap_or(0);
                w.write_all(head).map_err(Fail::Early)?;
                let mut buf = vec![0u8; 64 * 1024];
                let mut left = len;
                while left > 0 {
                    let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
                    let n = match r.read(&mut buf[..want]) {
                        Ok(0) => {
                            let e = format!("request body ended {left} B before Content-Length {len}");
                            return Err(Fail::Late(self.client.transport(e)));
                        }
                        Ok(n) => n,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(Fail::Late(self.client.transport(e))),
                    };
                    w.write_all(&buf[..n]).map_err(Fail::Early)?;
                    left -= n as u64;
                }
            }
            Payload::Stream(r) => {
                w.write_all(head).map_err(Fail::Early)?;
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    let n = match r.read(&mut buf) {
                        Ok(n) => n,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(Fail::Late(self.client.transport(e))),
                    };
                    let mut chunk = Vec::with_capacity(n + 16);
                    chunk.extend_from_slice(format!("{n:x}\r\n").as_bytes());
                    chunk.extend_from_slice(&buf[..n]);
                    chunk.extend_from_slice(b"\r\n");
                    w.write_all(&chunk).map_err(Fail::Early)?;
                    if n == 0 {
                        break;
                    }
                }
            }
        }
        w.flush().map_err(Fail::Early)?;
        // první bajt odpovědi: EOF / reset tady znamená mrtvé spojení z poolu
        match c.fill_buf() {
            Ok([]) => return Err(Fail::Early(io::ErrorKind::UnexpectedEof.into())),
            Ok(_) => {}
            Err(e) if is_dead(&e) => return Err(Fail::Early(e)),
            Err(e) => return Err(Fail::Late(self.client.transport(e))),
        }
        let h = read_head(&mut c).map_err(|e| Fail::Late(self.client.transport(e)))?;
        Ok((c, h))
    }

    fn finish(&self, c: Conn, h: Head) -> Result<Response, ApiError> {
        let framing = h.framing(self.is_head());
        // 101: spojení teď patří jinému protokolu – do poolu nikdy (framing je UntilClose)
        let reusable = h.status != 101 && h.keep_alive() && !matches!(framing, Framing::UntilClose);
        let pool = reusable.then(|| self.client.inner.pool.clone());
        let resp = Response { status: h.status, headers: h.headers, body: Body::new(c, framing, pool) };
        if resp.status >= 400 {
            return Err(resp.into_status_error());
        }
        Ok(resp)
    }
}

// ---------------------------------------------------------------- odpověď

pub struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: Body,
}

impl Response {
    // chybové stavy (>= 400) vrací `call`/`send…` jako `ApiError::Status`; stav úspěšné
    // odpovědi aplikace zatím nepotřebuje
    #[cfg(test)]
    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }

    /// Průběžná čtečka těla; po dočtení do konce vrátí spojení do poolu.
    pub fn into_reader(self) -> Box<dyn Read + Send> {
        Box::new(self.body)
    }

    pub fn into_string(mut self) -> Result<String, ApiError> {
        let mut s = String::new();
        self.body.read_to_string(&mut s)?;
        Ok(s)
    }

    pub fn into_json<T: serde::de::DeserializeOwned>(mut self) -> Result<T, ApiError> {
        let mut v = Vec::new();
        self.body.read_to_end(&mut v)?;
        serde_json::from_slice(&v).map_err(|e| ApiError::Decode(e.to_string()))
    }

    /// Chybový stav: `message` z JSON těla, jinak oříznuté tělo (stejně jako dřívější klient).
    fn into_status_error(self) -> ApiError {
        let code = self.status;
        let mut raw = Vec::new();
        let _ = self.body.take(ERR_BODY_MAX).read_to_end(&mut raw);
        let body = String::from_utf8_lossy(&raw).into_owned();
        let message = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
            .unwrap_or(body);
        ApiError::Status { code, message: message.trim().to_string() }
    }
}

fn header<'h>(headers: &'h [(String, String)], name: &str) -> Option<&'h str> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

/// Zdroj těla podle způsobu ohraničení.
enum Src {
    Len(Conn, u64),
    Chunked(Chunked<Conn>),
    UntilClose(Conn),
    /// dočteno (spojení vráceno do poolu, nebo zavřeno)
    Done,
}

/// Tělo odpovědi. Jakmile je dočtené do konce, vrátí spojení do poolu (je-li `pool`);
/// zahozené dřív spojení zavře.
struct Body {
    src: Src,
    pool: Option<Arc<Pool>>,
}

impl Body {
    fn new(c: Conn, framing: Framing, pool: Option<Arc<Pool>>) -> Self {
        let src = match framing {
            Framing::Len(n) => Src::Len(c, n),
            Framing::Chunked => Src::Chunked(Chunked::new(c)),
            Framing::UntilClose => Src::UntilClose(c),
        };
        let mut b = Body { src, pool };
        // prázdné tělo (Content-Length: 0, 204, HEAD) – spojení hned zpět
        b.release_if_done();
        b
    }

    fn release_if_done(&mut self) {
        let done = match &self.src {
            Src::Len(_, left) => *left == 0,
            Src::Chunked(c) => c.done,
            _ => false,
        };
        if !done {
            return;
        }
        let conn = match std::mem::replace(&mut self.src, Src::Done) {
            Src::Len(c, _) => Some(c),
            Src::Chunked(c) if c.clean_end => Some(c.r),
            _ => None,
        };
        if let (Some(c), Some(p)) = (conn, self.pool.take()) {
            p.put(c);
        }
    }

    fn read_src(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.src {
            Src::Done => Ok(0),
            Src::Len(c, left) => {
                if *left == 0 {
                    return Ok(0);
                }
                let want = buf.len().min(usize::try_from(*left).unwrap_or(usize::MAX));
                let n = c.read(&mut buf[..want])?;
                if n == 0 {
                    return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "spojení skončilo uprostřed těla odpovědi"));
                }
                *left -= n as u64;
                Ok(n)
            }
            Src::Chunked(c) => c.read(buf),
            Src::UntilClose(c) => c.read(buf),
        }
    }
}

impl Read for Body {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        match self.read_src(buf) {
            Ok(n) => {
                self.release_if_done();
                Ok(n)
            }
            Err(e) => {
                // po chybě (timeout, rozbité kódování) už spojení nevracet
                self.pool = None;
                Err(e)
            }
        }
    }
}

// ---------------------------------------------------------------- parsování

/// Jak je ohraničené tělo odpovědi.
enum Framing {
    Len(u64),
    Chunked,
    /// bez délky a bez chunked – do zavření spojení
    UntilClose,
}

/// Stavový řádek a hlavičky odpovědi.
struct Head {
    status: u16,
    /// odpověď HTTP/1.0 (keep-alive jen výslovně)
    http10: bool,
    headers: Vec<(String, String)>,
}

impl Head {
    fn framing(&self, head_request: bool) -> Framing {
        // přepnutý protokol: všechno za hlavičkami jsou data nového protokolu až do zavření
        if self.status == 101 {
            return Framing::UntilClose;
        }
        if head_request || self.status == 204 || self.status == 304 || (100..200).contains(&self.status) {
            return Framing::Len(0);
        }
        if header(&self.headers, "transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
            return Framing::Chunked;
        }
        match header(&self.headers, "content-length").and_then(|v| v.parse::<u64>().ok()) {
            Some(n) => Framing::Len(n),
            None => Framing::UntilClose,
        }
    }

    /// Smí se spojení použít znovu (podle `Connection` a verze)?
    fn keep_alive(&self) -> bool {
        let conn = header(&self.headers, "connection").unwrap_or("").to_ascii_lowercase();
        let has = |t: &str| conn.split(',').any(|x| x.trim() == t);
        if has("close") {
            return false;
        }
        !self.http10 || has("keep-alive")
    }
}

/// Přečte stavový řádek a hlavičky; průběžné odpovědi 1xx (kromě 101) přeskočí.
fn read_head<R: BufRead>(r: &mut R) -> Result<Head, String> {
    loop {
        let mut line = String::new();
        r.read_line(&mut line).map_err(|e| e.to_string())?;
        let mut parts = line.split_whitespace();
        let version = parts.next().unwrap_or("");
        let http10 = version == "HTTP/1.0";
        let status: u16 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .filter(|_| version.starts_with("HTTP/"))
            .ok_or_else(|| format!("invalid HTTP response: {}", line.trim()))?;
        let mut headers = Vec::new();
        loop {
            line.clear();
            if r.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                return Err("unexpected end of HTTP headers".into());
            }
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            if let Some((k, v)) = l.split_once(':') {
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        if (100..200).contains(&status) && status != 101 {
            continue;
        }
        return Ok(Head { status, http10, headers });
    }
}

/// Přečte stavový řádek a hlavičky HTTP/1.1 odpovědi; vrací (status, čtečka těla) –
/// tělo je dekódované z `chunked`, případně omezené `Content-Length`.
pub(crate) fn http_response<R: BufRead + Send + 'static>(mut r: R) -> Result<(u16, Box<dyn Read + Send>), String> {
    let h = read_head(&mut r)?;
    let body: Box<dyn Read + Send> = match h.framing(false) {
        Framing::Chunked => Box::new(Chunked::new(r)),
        Framing::Len(n) => Box::new(r.take(n)),
        Framing::UntilClose => Box::new(r),
    };
    Ok((h.status, body))
}

/// Dekodér `Transfer-Encoding: chunked`.
struct Chunked<R> {
    r: R,
    /// zbývá bajtů v aktuálním bloku
    left: u64,
    done: bool,
    /// konec přečtený celý včetně trailerů (spojení je na hranici další odpovědi)
    clean_end: bool,
}

impl<R> Chunked<R> {
    fn new(r: R) -> Self {
        Self { r, left: 0, done: false, clean_end: false }
    }
}

impl<R: BufRead> Read for Chunked<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use io::{Error, ErrorKind};
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let mut line = String::new();
            if self.r.read_line(&mut line)? == 0 {
                return Err(Error::new(ErrorKind::UnexpectedEof, "chunked: unexpected end"));
            }
            let hex = line.trim().split(';').next().unwrap_or("").trim();
            let size = u64::from_str_radix(hex, 16).map_err(|_| Error::new(ErrorKind::InvalidData, format!("chunked: bad size {hex:?}")))?;
            if size == 0 {
                self.done = true;
                // trailery až po prázdný řádek
                loop {
                    line.clear();
                    if self.r.read_line(&mut line)? == 0 {
                        break;
                    }
                    if line.trim_end().is_empty() {
                        self.clean_end = true;
                        break;
                    }
                }
                return Ok(0);
            }
            self.left = size;
        }
        let want = buf.len().min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.r.read(&mut buf[..want])?;
        if n == 0 {
            return Err(Error::new(ErrorKind::UnexpectedEof, "chunked: unexpected end"));
        }
        self.left -= n as u64;
        if self.left == 0 {
            // CRLF za blokem
            let mut crlf = String::new();
            self.r.read_line(&mut crlf)?;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ApiError;
    use crate::http::conn::{Connect, TcpConnect};
    use std::io::{BufReader, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Strana falešného serveru pro jedno spojení.
    struct Srv {
        r: BufReader<TcpStream>,
        w: TcpStream,
        reqs: Arc<Mutex<Vec<String>>>,
        /// tělo a hlavička `X-Test` posledního požadavku
        body: Vec<u8>,
        x_test: String,
        /// poslední požadavek měl tělo jako `chunked`
        chunked: bool,
    }

    impl Srv {
        /// Přečte požadavek (hlavičky + tělo podle Content-Length); None = klient zavřel.
        fn req(&mut self) -> Option<String> {
            let mut line = String::new();
            if self.r.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let mut parts = line.split_whitespace();
            let what = format!("{} {}", parts.next()?, parts.next()?);
            let (mut len, mut chunked) = (0usize, false);
            self.x_test.clear();
            loop {
                let mut h = String::new();
                if self.r.read_line(&mut h).ok()? == 0 {
                    return None;
                }
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    if k.eq_ignore_ascii_case("content-length") {
                        len = v.trim().parse().ok()?;
                    } else if k.eq_ignore_ascii_case("transfer-encoding") {
                        chunked = v.contains("chunked");
                    } else if k.eq_ignore_ascii_case("x-test") {
                        self.x_test = v.trim().to_string();
                    }
                }
            }
            self.body.clear();
            self.chunked = chunked;
            if chunked {
                Chunked::new(&mut self.r).read_to_end(&mut self.body).ok()?;
            } else {
                self.body.resize(len, 0);
                self.r.read_exact(&mut self.body).ok()?;
            }
            self.reqs.lock().unwrap().push(what.clone());
            Some(what)
        }

        fn send(&mut self, b: &[u8]) -> bool {
            self.w.write_all(b).is_ok()
        }
    }

    struct Fake {
        port: u16,
        conns: Arc<AtomicUsize>,
        reqs: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        fn conns(&self) -> usize {
            self.conns.load(Ordering::SeqCst)
        }

        fn count(&self, what: &str) -> usize {
            self.reqs.lock().unwrap().iter().filter(|r| *r == what).count()
        }
    }

    /// Falešný server na 127.0.0.1:0; každé spojení obslouží `handler(pořadí spojení, …)`
    /// ve vlastním vlákně. Počítá přijatá spojení a přečtené požadavky.
    fn fake<F: Fn(usize, &mut Srv) + Send + Sync + 'static>(handler: F) -> Fake {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let conns = Arc::new(AtomicUsize::new(0));
        let reqs = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(handler);
        let (c, q) = (conns.clone(), reqs.clone());
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(s) = s else { break };
                let idx = c.fetch_add(1, Ordering::SeqCst);
                let (h, q) = (handler.clone(), q.clone());
                std::thread::spawn(move || {
                    let mut srv = Srv { r: BufReader::new(s.try_clone().unwrap()), w: s, reqs: q, body: Vec::new(), x_test: String::new(), chunked: false };
                    h(idx, &mut srv);
                });
            }
        });
        Fake { port, conns, reqs }
    }

    fn client(port: u16) -> Client {
        Client::new(Arc::new(TcpConnect { port, tls: None }), Duration::from_millis(500), Duration::from_secs(2))
    }

    const OK: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";

    /// Server odpovídá na každý požadavek stejně a spojení drží otevřené.
    fn serve_all(resp: &'static [u8]) -> impl Fn(usize, &mut Srv) + Send + Sync + 'static {
        move |_, s| {
            while s.req().is_some() {
                if !s.send(resp) {
                    break;
                }
            }
        }
    }

    #[test]
    fn http_response_chunked_and_errors() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: x\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n7;ext=1\r\n, world\r\n0\r\n\r\n";
        let (st, mut body) = http_response(std::io::Cursor::new(raw.to_vec())).unwrap();
        assert_eq!(st, 200);
        let mut s = String::new();
        body.read_to_string(&mut s).unwrap();
        assert_eq!(s, "hello, world");

        let raw = b"HTTP/1.1 404 Not Found\r\nContent-Length: 22\r\n\r\n{\"message\":\"no such\"}\nEXTRA";
        let (st, mut body) = http_response(std::io::Cursor::new(raw.to_vec())).unwrap();
        assert_eq!(st, 404);
        let mut s = String::new();
        body.read_to_string(&mut s).unwrap();
        assert_eq!(s, "{\"message\":\"no such\"}\n");

        assert!(http_response(std::io::Cursor::new(b"garbage\r\n".to_vec())).is_err());
    }

    #[test]
    fn keep_alive_reuses_connection() {
        let f = fake(serve_all(OK));
        let c = client(f.port);
        for _ in 0..3 {
            assert_eq!(c.request("GET", "/x").call().unwrap().into_string().unwrap(), "ok");
        }
        assert_eq!(f.conns(), 1);
        assert_eq!(f.count("GET /x"), 3);
    }

    #[test]
    fn chunked_body_and_reuse() {
        let f = fake(serve_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n7\r\n, world\r\n0\r\n\r\n"));
        let c = client(f.port);
        for _ in 0..2 {
            let r = c.request("GET", "/c").call().unwrap();
            assert_eq!(r.status(), 200);
            assert_eq!(r.header("transfer-encoding"), Some("chunked"));
            assert_eq!(r.into_string().unwrap(), "hello, world");
        }
        assert_eq!(f.conns(), 1);
    }

    #[test]
    fn status_404_maps_message() {
        let f = fake(serve_all(b"HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: 33\r\n\r\n{\"message\":\"no such container\"}\r\n"));
        let c = client(f.port);
        let e = c.request("GET", "/containers/x/json").call().err().unwrap();
        assert_eq!(e, ApiError::Status { code: 404, message: "no such container".into() });
        // tělo chyby je dočtené -> spojení se dá použít znovu
        assert!(c.request("GET", "/y").call().is_err());
        assert_eq!(f.conns(), 1);

        // tělo, které není JSON, se vrátí oříznuté
        let f = fake(serve_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 7\r\n\r\n boom \n"));
        let e = client(f.port).request("POST", "/z").call().err().unwrap();
        assert_eq!(e, ApiError::Status { code: 500, message: "boom".into() });
    }

    #[test]
    fn connection_close_not_pooled() {
        // server po `Connection: close` spojení nezavře – kdyby ho klient použil znovu,
        // zůstalo by u jednoho spojení
        let f = fake(serve_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 2\r\n\r\nok"));
        let c = client(f.port);
        assert_eq!(c.request("GET", "/a").call().unwrap().into_string().unwrap(), "ok");
        assert_eq!(c.idle(), 0);
        assert_eq!(c.request("GET", "/a").call().unwrap().into_string().unwrap(), "ok");
        assert_eq!(f.conns(), 2);
    }

    #[test]
    fn unknown_length_streams_and_is_not_pooled() {
        const MB: usize = 1 << 20;
        let (tx, rx) = mpsc::channel::<()>();
        let rx = Mutex::new(rx);
        let f = fake(move |idx, s| {
            if idx > 0 {
                return serve_all(OK)(idx, s);
            }
            s.req();
            s.send(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\r\n");
            let piece = [b'x'; 16 * 1024];
            for _ in 0..(MB / 2 / piece.len()) {
                s.send(&piece);
            }
            // druhá polovina až poté, co klient potvrdí, že už čte – kdyby klient tělo
            // celé načítal do paměti před vrácením, druhá polovina nepřijde
            if rx.lock().unwrap().recv_timeout(Duration::from_secs(2)).is_ok() {
                for _ in 0..(MB / 2 / piece.len()) {
                    s.send(&piece);
                }
            }
        });
        let c = client(f.port);
        let mut r = c.request("GET", "/big").call().unwrap().into_reader();
        let mut first = [0u8; 1000];
        r.read_exact(&mut first).unwrap();
        tx.send(()).unwrap();
        let mut rest = Vec::new();
        r.read_to_end(&mut rest).unwrap();
        assert_eq!(first.len() + rest.len(), MB);
        drop(r);
        assert_eq!(c.idle(), 0);
        assert_eq!(c.request("GET", "/x").call().unwrap().into_string().unwrap(), "ok");
        assert_eq!(f.conns(), 2);
    }

    /// Server odpoví na jeden požadavek (bez `Connection: close`) a spojení zavře.
    fn one_and_close(_: usize, s: &mut Srv) {
        if s.req().is_some() {
            s.send(OK);
        }
    }

    #[test]
    fn stale_pooled_get_is_retried_once() {
        let f = fake(one_and_close);
        let c = client(f.port);
        assert_eq!(c.request("GET", "/g").call().unwrap().into_string().unwrap(), "ok");
        assert_eq!(c.idle(), 1);
        assert_eq!(c.request("GET", "/g").call().unwrap().into_string().unwrap(), "ok");
        assert_eq!(f.conns(), 2);
        assert_eq!(f.count("GET /g"), 2);
    }

    #[test]
    fn stale_pooled_delete_without_body_is_retried_once() {
        let f = fake(one_and_close);
        let c = client(f.port);
        assert_eq!(c.request("DELETE", "/d").call().unwrap().into_string().unwrap(), "ok");
        assert_eq!(c.idle(), 1);
        assert_eq!(c.request("DELETE", "/d").call().unwrap().into_string().unwrap(), "ok");
        assert_eq!(f.conns(), 2);
        assert_eq!(f.count("DELETE /d"), 2);
        // PUT s prázdným tělem se opakuje také
        assert_eq!(c.idle(), 1);
        assert_eq!(c.request("PUT", "/e").send_bytes(b"").unwrap().into_string().unwrap(), "ok");
        assert_eq!(f.conns(), 3);
    }

    #[test]
    fn stale_pooled_put_with_body_is_not_retried() {
        let f = fake(one_and_close);
        let c = client(f.port);
        assert_eq!(c.request("PUT", "/u").send_bytes(b"x").unwrap().into_string().unwrap(), "ok");
        assert_eq!(c.idle(), 1);
        let e = c.request("PUT", "/u").send_bytes(b"x").err().unwrap();
        assert!(matches!(e, ApiError::Transport(_)), "{e:?}");
        assert_eq!(f.conns(), 1);
    }

    #[test]
    fn idle_connection_expires_after_ttl() {
        let f = fake(serve_all(OK));
        let c = client(f.port).with_idle_ttl(Duration::from_millis(300));
        assert_eq!(c.request("GET", "/x").call().unwrap().into_string().unwrap(), "ok");
        // nečinné kratší dobu než TTL: znovu použité
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(c.request("GET", "/x").call().unwrap().into_string().unwrap(), "ok");
        assert_eq!(f.conns(), 1);
        // nečinné déle než TTL: zahozené, nové spojení
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(c.request("GET", "/x").call().unwrap().into_string().unwrap(), "ok");
        assert_eq!(f.conns(), 2);
        assert_eq!(c.idle(), 1);
    }

    #[test]
    fn default_idle_ttl_covers_refresh_interval() {
        // aplikace obnovuje po 3 s (s událostmi až po 30 s) – spojení musí v poolu vydržet
        assert!(IDLE_TTL >= Duration::from_secs(30));
    }

    #[test]
    fn method_or_path_with_space_or_cr_lf_is_rejected_before_sending() {
        let f = fake(serve_all(OK));
        let c = client(f.port);
        for (m, p) in [("GET", "/a b"), ("GET", "/a\r\nX-Evil: 1"), ("GET", "/a\nb"), ("GET", ""), ("GE T", "/a"), ("GET\r\n", "/a"), ("", "/a")] {
            let e = c.request(m, p).call().err().unwrap();
            assert!(matches!(e, ApiError::Decode(_)), "{m:?} {p:?}: {e:?}");
        }
        assert_eq!(f.conns(), 0);
        assert_eq!(c.request("GET", "/a?x=%20").call().unwrap().into_string().unwrap(), "ok");
    }

    #[test]
    fn stale_pooled_post_is_not_retried() {
        let f = fake(one_and_close);
        let c = client(f.port);
        assert_eq!(c.request("POST", "/p").send_bytes(b"{}").unwrap().into_string().unwrap(), "ok");
        assert_eq!(c.idle(), 1);
        let e = c.request("POST", "/p").send_bytes(b"{}").err().unwrap();
        assert!(matches!(e, ApiError::Transport(_)), "{e:?}");
        assert_eq!(f.count("POST /p"), 1);
        assert_eq!(f.conns(), 1);
    }

    #[test]
    fn json_chunked_upload_headers_and_timeout() {
        // ozvěna: tělo zpět, hlavička X-Test jako X-Echo; na /slow neodpoví
        let f = fake(|_, s| {
            while let Some(what) = s.req() {
                if what.ends_with("/slow") {
                    continue;
                }
                let head = format!("HTTP/1.1 200 OK\r\nX-Echo: {}\r\nContent-Length: {}\r\n\r\n", s.x_test, s.body.len());
                let body = s.body.clone();
                if !(s.send(head.as_bytes()) && s.send(&body)) {
                    break;
                }
            }
        });
        let c = client(f.port);
        let r = c.request("POST", "/j").set("X-Test", "abc").send_json(serde_json::json!({"a": 1})).unwrap();
        assert_eq!(r.header("x-echo"), Some("abc"));
        assert_eq!(r.into_json::<serde_json::Value>().unwrap(), serde_json::json!({"a": 1}));

        // chunked upload většího těla, na stejném spojení
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let r = c.request("PUT", "/u").send(std::io::Cursor::new(data.clone())).unwrap();
        let mut got = Vec::new();
        r.into_reader().read_to_end(&mut got).unwrap();
        assert_eq!(got, data);
        assert_eq!(f.conns(), 1);

        // časový limit čtení pro jeden požadavek
        let t = Instant::now();
        let e = c.request("GET", "/slow").timeout(Duration::from_millis(200)).call().err().unwrap();
        assert!(matches!(e, ApiError::Transport(_)), "{e:?}");
        assert!(t.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn shutdown_handle_unblocks_reader() {
        let f = fake(|_, s| {
            // nekonečně pomalé vysílání, dokud spojení žije
            while s.send(b"x") {
                std::thread::sleep(Duration::from_millis(50));
            }
        });
        let mut st = TcpConnect { port: f.port, tls: None }.connect(Duration::from_millis(500)).unwrap();
        st.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let close = st.shutdown_handle().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            while matches!(st.read(&mut buf), Ok(n) if n > 0) {}
            let _ = tx.send(Instant::now());
        });
        std::thread::sleep(Duration::from_millis(150));
        close();
        assert!(rx.recv_timeout(Duration::from_secs(1)).is_ok(), "čtení se po shutdown neodblokovalo");
    }

    #[test]
    fn connect_refused_is_transport() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let c = Client::new(Arc::new(TcpConnect { port, tls: None }), Duration::from_millis(300), Duration::from_secs(1));
        let e = c.request("GET", "/_ping").call().err().unwrap();
        assert!(matches!(e, ApiError::Transport(_)), "{e:?}");
    }
    #[test]
    fn switching_protocols_is_never_pooled() {
        let f = fake(|idx, s| {
            if idx > 0 {
                return serve_all(OK)(idx, s);
            }
            s.req();
            s.send(b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: tcp\r\n\r\nraw-data");
            let _ = s.w.shutdown(std::net::Shutdown::Write);
        });
        let c = client(f.port);
        let r = c.request("GET", "/up").call().unwrap();
        assert_eq!(r.status(), 101);
        // hned po hlavičkách se spojení do poolu vrátit nesmí (dřív: „prázdné tělo“ 1xx)
        assert_eq!(c.idle(), 0);
        let mut s = String::new();
        r.into_reader().read_to_string(&mut s).unwrap();
        assert_eq!(s, "raw-data");
        assert_eq!(c.idle(), 0);
        assert_eq!(c.request("GET", "/x").call().unwrap().into_string().unwrap(), "ok");
        assert_eq!(f.conns(), 2);
    }

    #[test]
    fn header_with_cr_lf_is_rejected_before_sending() {
        let f = fake(serve_all(OK));
        let c = client(f.port);
        for (k, v) in [("X-A", "a\r\nX-Evil: 1"), ("X-A\r\nX-Evil", "1"), ("X-A", "a\nb"), ("X-A", "a\rb"), ("", "x"), ("X:A", "x")] {
            let e = c.request("GET", "/h").set("X-Ok", "1").set(k, v).call().err().unwrap();
            assert!(matches!(e, ApiError::Decode(_)), "{k:?}: {e:?}");
        }
        // nic se neodeslalo, ani spojení nevzniklo
        assert_eq!(f.conns(), 0);
        assert_eq!(c.request("GET", "/h").set("X-Ok", "a b: c").call().unwrap().into_string().unwrap(), "ok");
    }

    #[test]
    fn stream_with_content_length_is_sent_raw() {
        let f = fake(|_, s| {
            while s.req().is_some() {
                let head = format!("HTTP/1.1 200 OK\r\nX-Chunked: {}\r\nContent-Length: {}\r\n\r\n", s.chunked, s.body.len());
                let body = s.body.clone();
                if !(s.send(head.as_bytes()) && s.send(&body)) {
                    break;
                }
            }
        });
        let c = client(f.port);
        let data: Vec<u8> = (0..150_000u32).map(|i| (i % 253) as u8).collect();
        let r = c
            .request("PUT", "/archive")
            .set("Content-Type", "application/x-tar")
            .set("Content-Length", &data.len().to_string())
            .send(std::io::Cursor::new(data.clone()))
            .unwrap();
        assert_eq!(r.header("x-chunked"), Some("false"));
        let mut got = Vec::new();
        r.into_reader().read_to_end(&mut got).unwrap();
        assert_eq!(got, data);
        // bez délky chunked
        let r = c.request("PUT", "/archive").send(std::io::Cursor::new(vec![7u8; 10])).unwrap();
        assert_eq!(r.header("x-chunked"), Some("true"));
        assert_eq!(r.into_string().unwrap().len(), 10);
        assert_eq!(f.conns(), 1);
        // kratší tělo, než slibuje Content-Length -> chyba, spojení se zahodí
        let e = c.request("PUT", "/archive").set("Content-Length", "10").send(std::io::Cursor::new(vec![1u8; 3])).err().unwrap();
        assert!(matches!(e, ApiError::Transport(_)), "{e:?}");
        assert_eq!(c.idle(), 0);
    }

    #[test]
    fn short_timeout_does_not_stick_to_pooled_connection() {
        let f = fake(|_, s| {
            while let Some(what) = s.req() {
                if what.ends_with("/late") {
                    std::thread::sleep(Duration::from_millis(500));
                }
                if !s.send(OK) {
                    break;
                }
            }
        });
        let c = client(f.port);
        let r = c.request("GET", "/quick").timeout(Duration::from_millis(200)).call().unwrap();
        assert_eq!(r.into_string().unwrap(), "ok");
        assert_eq!(c.idle(), 1);
        // stejné spojení z poolu, výchozí limit (2 s) – odpověď za 500 ms musí projít
        assert_eq!(c.request("GET", "/late").call().unwrap().into_string().unwrap(), "ok");
        assert_eq!(f.conns(), 1);
    }

    // ------------------------------------------------------------ TLS

    use rustls_pki_types::pem::PemObject;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer};

    type TlsSrv = BufReader<rustls::StreamOwned<rustls::ServerConnection, TcpStream>>;

    fn provider() -> Arc<rustls::crypto::CryptoProvider> {
        Arc::new(rustls::crypto::ring::default_provider())
    }

    /// Klient s testovací CA (bez klientského certifikátu); `trust = false` = prázdná sada CA.
    /// Certifikát serveru (testdata, platnost do roku 2126) je vystavený na IP 127.0.0.1.
    fn tls_client(trust: bool) -> Arc<rustls::ClientConfig> {
        let mut roots = rustls::RootCertStore::empty();
        if trust {
            for c in CertificateDer::pem_slice_iter(include_bytes!("testdata/ca.pem")) {
                roots.add(c.unwrap()).unwrap();
            }
        }
        let cfg = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(cfg)
    }

    /// TLS server na 127.0.0.1:0, každé spojení ve vlastním vlákně; vrací (port, počet spojení).
    fn tls_fake<F: Fn(&mut TlsSrv) + Send + Sync + 'static>(handler: F) -> (u16, Arc<AtomicUsize>) {
        let certs: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(include_bytes!("testdata/server.pem")).collect::<Result<_, _>>().unwrap();
        let key = PrivateKeyDer::from_pem_slice(include_bytes!("testdata/server-key.pem")).unwrap();
        let cfg = Arc::new(
            rustls::ServerConfig::builder_with_provider(provider())
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .unwrap(),
        );
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let conns = Arc::new(AtomicUsize::new(0));
        let (c, handler) = (conns.clone(), Arc::new(handler));
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(s) = s else { break };
                c.fetch_add(1, Ordering::SeqCst);
                let (cfg, h) = (cfg.clone(), handler.clone());
                std::thread::spawn(move || {
                    let conn = rustls::ServerConnection::new(cfg).unwrap();
                    h(&mut BufReader::new(rustls::StreamOwned::new(conn, s)));
                });
            }
        });
        (port, conns)
    }

    /// Přečte hlavičky požadavku bez těla; false = klient zavřel.
    fn tls_req(r: &mut TlsSrv) -> bool {
        let mut seen = false;
        loop {
            let mut line = String::new();
            match r.read_line(&mut line) {
                Ok(0) | Err(_) => return false,
                Ok(_) if line.trim_end().is_empty() => return seen,
                Ok(_) => seen = true,
            }
        }
    }

    #[test]
    fn tls_keep_alive_reuses_connection() {
        let (port, conns) = tls_fake(|r| {
            while tls_req(r) {
                if r.get_mut().write_all(OK).and_then(|_| r.get_mut().flush()).is_err() {
                    break;
                }
            }
        });
        let tls_client_for = |trust| Client::new(Arc::new(TcpConnect { port, tls: Some(tls_client(trust)) }), Duration::from_millis(500), Duration::from_secs(2));
        let c = tls_client_for(true);
        for _ in 0..3 {
            assert_eq!(c.request("GET", "/t").call().unwrap().into_string().unwrap(), "ok");
        }
        assert_eq!(conns.load(Ordering::SeqCst), 1);
        // bez důvěryhodné CA spojení neprojde
        let e = tls_client_for(false).request("GET", "/t").call().err().unwrap();
        assert!(matches!(e, ApiError::Transport(_)), "{e:?}");
    }

    #[test]
    fn tls_shutdown_handle_unblocks_reader() {
        // server přečte požadavek a mlčí, dokud ho klient nezavře
        let (port, _) = tls_fake(|r| {
            if tls_req(r) {
                let mut b = [0u8; 16];
                let _ = r.read(&mut b);
            }
        });
        let mut st = TcpConnect { port, tls: Some(tls_client(true)) }.connect(Duration::from_millis(500)).unwrap();
        st.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        // zápis dokončí handshake
        st.write_all(b"GET /hold HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        st.flush().unwrap();
        let close = st.shutdown_handle().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 64];
            while matches!(st.read(&mut buf), Ok(n) if n > 0) {}
            let _ = tx.send(Instant::now());
        });
        std::thread::sleep(Duration::from_millis(200));
        assert!(rx.try_recv().is_err(), "čtení skončilo dřív než shutdown");
        close();
        assert!(rx.recv_timeout(Duration::from_secs(1)).is_ok(), "čtení přes TLS se po shutdown neodblokovalo");
    }
}
