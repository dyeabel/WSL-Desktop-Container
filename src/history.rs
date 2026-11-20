//! Historie zdrojů na disku: paměť (VmmemWSL, Linux použito / cache), CPU kontejnerů,
//! provoz VM (eth0), úložiště enginu a disk distribuce a uvolnění paměti – aby trendy
//! přežily restart aplikace. Týdenní souhrn.
//!
//! Dvě rozlišení: minutové průměry (`raw.bin`, 48 h) a 15minutové agregace min/průměr/max
//! (`agg.bin`, 35 dní); uvolnění paměti v `events.bin`. Záznamy mají pevnou délku
//! (little-endian) a soubory hlavičku s verzí. Zapisuje se dávkově (jednou za 5 min a při
//! ukončení) přidáním na konec; staré záznamy se občas zahodí přepsáním souboru
//! přes dočasný soubor a přejmenování. Useknutý poslední záznam se při načtení ignoruje.

use crate::memory::{FreeEvent, MemPoint};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Krok minutových záznamů (s).
pub const RAW_STEP: u32 = 60;
/// Jak dlouho držet minutové záznamy (s).
pub const RAW_KEEP: u32 = 48 * 3600;
/// Krok agregací (s).
pub const AGG_STEP: u32 = 900;
/// Jak dlouho držet agregace a uvolnění paměti (s).
pub const AGG_KEEP: u32 = 35 * 86_400;
/// Zápis na disk jednou za tolik.
pub const FLUSH_EVERY: Duration = Duration::from_secs(300);
/// Rezerva nad dobou uchování, než se soubor přepíše (aby se nepřepisoval při každém zápisu).
const RAW_SLACK: u32 = 6 * 3600;
const AGG_SLACK: u32 = 86_400;
/// Souhrn se ukáže, až data pokrývají aspoň tolik (s).
pub const SUMMARY_MIN_SPAN: f64 = 20.0 * 3600.0;
const WEEK: f64 = 7.0 * 86_400.0;

const MAGIC: &[u8; 4] = b"WLSH";
const VERSION: u8 = 1;
const HEADER: usize = 8;
pub const RAW_SIZE: usize = 48;
pub const AGG_SIZE: usize = 80;
pub const EV_SIZE: usize = 24;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * 1024;

// příznaky přítomných hodnot
const F_VMMEM: u16 = 1;
const F_USED: u16 = 2;
const F_CACHE: u16 = 4;
const F_CPU: u16 = 8;
const F_NET: u16 = 16;
const F_ENGINE: u16 = 32;
const F_DISK: u16 = 64;

// ---------------------------------------------------------------- záznamy

/// Minutový záznam (průměr vzorků v minutě; čítače a disk = poslední hodnota).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Raw {
    /// unixový čas začátku minuty
    pub t: u32,
    pub vmmem: Option<u64>,
    pub used: Option<u64>,
    pub cache: Option<u64>,
    /// CPU kontejnerů v % všech CPU WSL
    pub cpu: Option<f64>,
    /// kumulované bajty eth0 VM (rx, tx) od startu VM
    pub net: Option<(u64, u64)>,
    /// úložiště enginu (image + kontejnery + volume + build cache)
    pub engine: Option<u64>,
    /// disk distribuce (použito, volné)
    pub disk: Option<(u64, u64)>,
}

/// Minimum, průměr a maximum.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Mma {
    pub min: f64,
    pub avg: f64,
    pub max: f64,
}

/// 15minutová agregace.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Agg {
    /// unixový čas začátku bucketu
    pub t: u32,
    /// počet minutových záznamů
    pub n: u16,
    pub vmmem: Option<Mma>,
    pub used: Option<Mma>,
    pub cache: Option<Mma>,
    pub cpu: Option<Mma>,
    /// přeneseno přes eth0 v bucketu (rx, tx)
    pub net: Option<(u64, u64)>,
    /// poslední hodnota v bucketu
    pub engine: Option<u64>,
    pub disk: Option<(u64, u64)>,
}

/// Jeden vzorek do historie (chybějící hodnoty = None).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sample {
    pub vmmem: Option<u64>,
    pub used: Option<u64>,
    pub cache: Option<u64>,
    pub cpu: Option<f64>,
    pub net: Option<(u64, u64)>,
    pub engine: Option<u64>,
    pub disk: Option<(u64, u64)>,
}

// ---------------------------------------------------------------- kódování

fn kib(v: u64) -> u32 {
    (v / KIB).min(u32::MAX as u64) as u32
}
fn mib(v: u64) -> u32 {
    (v / MIB).min(u32::MAX as u64) as u32
}
fn centi(v: f64) -> u16 {
    (v * 100.0).round().clamp(0.0, u16::MAX as f64) as u16
}

struct W(Vec<u8>);
impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
}

struct R<'a>(&'a [u8], usize);
impl R<'_> {
    fn u8(&mut self) -> u8 {
        self.1 += 1;
        self.0[self.1 - 1]
    }
    fn u16(&mut self) -> u16 {
        self.1 += 2;
        u16::from_le_bytes([self.0[self.1 - 2], self.0[self.1 - 1]])
    }
    fn u32(&mut self) -> u32 {
        self.1 += 4;
        u32::from_le_bytes(self.0[self.1 - 4..self.1].try_into().unwrap_or_default())
    }
    fn u64(&mut self) -> u64 {
        self.1 += 8;
        u64::from_le_bytes(self.0[self.1 - 8..self.1].try_into().unwrap_or_default())
    }
}

fn flag<T>(f: u16, bit: u16, v: T) -> Option<T> {
    (f & bit != 0).then_some(v)
}

impl Raw {
    pub fn encode(&self, out: &mut Vec<u8>) {
        let mut w = W(Vec::with_capacity(RAW_SIZE));
        let mut f = 0;
        for (bit, on) in [
            (F_VMMEM, self.vmmem.is_some()),
            (F_USED, self.used.is_some()),
            (F_CACHE, self.cache.is_some()),
            (F_CPU, self.cpu.is_some()),
            (F_NET, self.net.is_some()),
            (F_ENGINE, self.engine.is_some()),
            (F_DISK, self.disk.is_some()),
        ] {
            if on {
                f |= bit;
            }
        }
        w.u32(self.t);
        w.u16(f);
        w.u16(centi(self.cpu.unwrap_or(0.0)));
        w.u32(kib(self.vmmem.unwrap_or(0)));
        w.u32(kib(self.used.unwrap_or(0)));
        w.u32(kib(self.cache.unwrap_or(0)));
        let (rx, tx) = self.net.unwrap_or((0, 0));
        w.u64(rx);
        w.u64(tx);
        w.u32(mib(self.engine.unwrap_or(0)));
        let (du, df) = self.disk.unwrap_or((0, 0));
        w.u32(mib(du));
        w.u32(mib(df));
        debug_assert_eq!(w.0.len(), RAW_SIZE);
        out.extend_from_slice(&w.0);
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < RAW_SIZE {
            return None;
        }
        let mut r = R(b, 0);
        let t = r.u32();
        let f = r.u16();
        let cpu = r.u16() as f64 / 100.0;
        let (vmmem, used, cache) = (r.u32() as u64 * KIB, r.u32() as u64 * KIB, r.u32() as u64 * KIB);
        let (rx, tx) = (r.u64(), r.u64());
        let engine = r.u32() as u64 * MIB;
        let (du, df) = (r.u32() as u64 * MIB, r.u32() as u64 * MIB);
        Some(Raw {
            t,
            vmmem: flag(f, F_VMMEM, vmmem),
            used: flag(f, F_USED, used),
            cache: flag(f, F_CACHE, cache),
            cpu: flag(f, F_CPU, cpu),
            net: flag(f, F_NET, (rx, tx)),
            engine: flag(f, F_ENGINE, engine),
            disk: flag(f, F_DISK, (du, df)),
        })
    }
}

impl Agg {
    pub fn encode(&self, out: &mut Vec<u8>) {
        let mut w = W(Vec::with_capacity(AGG_SIZE));
        let mut f = 0;
        for (bit, on) in [
            (F_VMMEM, self.vmmem.is_some()),
            (F_USED, self.used.is_some()),
            (F_CACHE, self.cache.is_some()),
            (F_CPU, self.cpu.is_some()),
            (F_NET, self.net.is_some()),
            (F_ENGINE, self.engine.is_some()),
            (F_DISK, self.disk.is_some()),
        ] {
            if on {
                f |= bit;
            }
        }
        w.u32(self.t);
        w.u16(f);
        w.u16(self.n);
        for m in [self.vmmem, self.used, self.cache] {
            let m = m.unwrap_or_default();
            w.u32(kib(m.min as u64));
            w.u32(kib(m.avg as u64));
            w.u32(kib(m.max as u64));
        }
        let c = self.cpu.unwrap_or_default();
        w.u16(centi(c.min));
        w.u16(centi(c.avg));
        w.u16(centi(c.max));
        w.u16(0);
        let (rx, tx) = self.net.unwrap_or((0, 0));
        w.u64(rx);
        w.u64(tx);
        w.u32(mib(self.engine.unwrap_or(0)));
        let (du, df) = self.disk.unwrap_or((0, 0));
        w.u32(mib(du));
        w.u32(mib(df));
        debug_assert_eq!(w.0.len(), AGG_SIZE);
        out.extend_from_slice(&w.0);
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() < AGG_SIZE {
            return None;
        }
        let mut r = R(b, 0);
        let t = r.u32();
        let f = r.u16();
        let n = r.u16();
        let mut mem = || {
            let (a, b, c) = (r.u32(), r.u32(), r.u32());
            Mma { min: (a as u64 * KIB) as f64, avg: (b as u64 * KIB) as f64, max: (c as u64 * KIB) as f64 }
        };
        let (vmmem, used, cache) = (mem(), mem(), mem());
        let cpu = Mma { min: r.u16() as f64 / 100.0, avg: r.u16() as f64 / 100.0, max: r.u16() as f64 / 100.0 };
        r.u16();
        let (rx, tx) = (r.u64(), r.u64());
        let engine = r.u32() as u64 * MIB;
        let (du, df) = (r.u32() as u64 * MIB, r.u32() as u64 * MIB);
        Some(Agg {
            t,
            n,
            vmmem: flag(f, F_VMMEM, vmmem),
            used: flag(f, F_USED, used),
            cache: flag(f, F_CACHE, cache),
            cpu: flag(f, F_CPU, cpu),
            net: flag(f, F_NET, (rx, tx)),
            engine: flag(f, F_ENGINE, engine),
            disk: flag(f, F_DISK, (du, df)),
        })
    }
}

pub fn encode_event(e: &FreeEvent, out: &mut Vec<u8>) {
    let mut w = W(Vec::with_capacity(EV_SIZE));
    w.u32(e.t.max(0.0) as u32);
    w.u8(u8::from(e.auto) | if e.vmmem_before.is_some() { 2 } else { 0 } | if e.vmmem_after.is_some() { 4 } else { 0 });
    w.u8(0);
    w.u16(0);
    w.u32(kib(e.vmmem_before.unwrap_or(0)));
    w.u32(kib(e.vmmem_after.unwrap_or(0)));
    w.u32(kib(e.cache_before));
    w.u32(kib(e.cache_after));
    debug_assert_eq!(w.0.len(), EV_SIZE);
    out.extend_from_slice(&w.0);
}

pub fn decode_event(b: &[u8]) -> Option<FreeEvent> {
    if b.len() < EV_SIZE {
        return None;
    }
    let mut r = R(b, 0);
    let t = r.u32() as f64;
    let f = r.u8();
    r.u8();
    r.u16();
    let (vb, va, cb, ca) = (r.u32() as u64 * KIB, r.u32() as u64 * KIB, r.u32() as u64 * KIB, r.u32() as u64 * KIB);
    Some(FreeEvent {
        t,
        auto: f & 1 != 0,
        vmmem_before: (f & 2 != 0).then_some(vb),
        vmmem_after: (f & 4 != 0).then_some(va),
        cache_before: cb,
        cache_after: ca,
    })
}

/// Druh souboru (písmeno v hlavičce, název, délka záznamu).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Raw,
    Agg,
    Events,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::Raw, Kind::Agg, Kind::Events];
    fn tag(self) -> u8 {
        match self {
            Kind::Raw => b'R',
            Kind::Agg => b'A',
            Kind::Events => b'E',
        }
    }
    pub fn size(self) -> usize {
        match self {
            Kind::Raw => RAW_SIZE,
            Kind::Agg => AGG_SIZE,
            Kind::Events => EV_SIZE,
        }
    }
    pub fn file(self) -> &'static str {
        match self {
            Kind::Raw => "raw.bin",
            Kind::Agg => "agg.bin",
            Kind::Events => "events.bin",
        }
    }
    fn idx(self) -> usize {
        self as usize
    }
}

pub fn header(kind: Kind) -> [u8; HEADER] {
    let s = (kind.size() as u16).to_le_bytes();
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], kind.tag(), VERSION, s[0], s[1]]
}

/// Rozdělí obsah souboru na záznamy. Vrací (záznamy, soubor je v pořádku); chybná hlavička
/// = žádné záznamy, useknutý poslední záznam se vynechá (soubor se pak přepíše).
pub fn split_records(data: &[u8], kind: Kind) -> (Vec<&[u8]>, bool) {
    if data.len() < HEADER || data[..HEADER] != header(kind) {
        return (Vec::new(), data.is_empty());
    }
    let body = &data[HEADER..];
    let size = kind.size();
    let recs = body.chunks_exact(size).collect();
    (recs, body.len().is_multiple_of(size))
}

// ---------------------------------------------------------------- agregace

fn bucket(t: u32) -> u32 {
    t - t % AGG_STEP
}

/// Přírůstek čítače; když je menší než předchozí (restart VM), počítá se od nuly.
pub fn net_delta(prev: (u64, u64), cur: (u64, u64)) -> (u64, u64) {
    let d = |p: u64, c: u64| if c >= p { c - p } else { c };
    (d(prev.0, cur.0), d(prev.1, cur.1))
}

#[derive(Default)]
struct MmaAcc {
    min: f64,
    max: f64,
    sum: f64,
    n: u32,
}

impl MmaAcc {
    fn add(&mut self, v: f64) {
        if self.n == 0 {
            self.min = v;
            self.max = v;
        } else {
            self.min = self.min.min(v);
            self.max = self.max.max(v);
        }
        self.sum += v;
        self.n += 1;
    }
    fn get(&self) -> Option<Mma> {
        (self.n > 0).then(|| Mma { min: self.min, avg: self.sum / self.n as f64, max: self.max })
    }
}

/// Agreguje minutové záznamy (seřazené podle času) do 15minutových bucketů. `prev_net`
/// = poslední čítač sítě před prvním záznamem (pro přírůstek v prvním bucketu).
pub fn aggregate(raw: &[Raw], mut prev_net: Option<(u64, u64)>) -> Vec<Agg> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let b = bucket(raw[i].t);
        let (mut vm, mut us, mut ca, mut cpu) = (MmaAcc::default(), MmaAcc::default(), MmaAcc::default(), MmaAcc::default());
        let mut a = Agg { t: b, ..Default::default() };
        while i < raw.len() && bucket(raw[i].t) == b {
            let r = &raw[i];
            a.n = a.n.saturating_add(1);
            if let Some(v) = r.vmmem {
                vm.add(v as f64);
            }
            if let Some(v) = r.used {
                us.add(v as f64);
            }
            if let Some(v) = r.cache {
                ca.add(v as f64);
            }
            if let Some(v) = r.cpu {
                cpu.add(v);
            }
            if let Some(n) = r.net {
                let d = prev_net.map(|p| net_delta(p, n)).unwrap_or((0, 0));
                let s = a.net.get_or_insert((0, 0));
                s.0 += d.0;
                s.1 += d.1;
                prev_net = Some(n);
            }
            if r.engine.is_some() {
                a.engine = r.engine;
            }
            if r.disk.is_some() {
                a.disk = r.disk;
            }
            i += 1;
        }
        (a.vmmem, a.used, a.cache, a.cpu) = (vm.get(), us.get(), ca.get(), cpu.get());
        out.push(a);
    }
    out
}

/// Poslední čítač sítě v `raw[..idx]`.
fn net_before(raw: &[Raw], idx: usize) -> Option<(u64, u64)> {
    raw[..idx].iter().rev().find_map(|r| r.net)
}

// ---------------------------------------------------------------- minutový akumulátor

#[derive(Debug, Default, Clone)]
struct Acc {
    t: Option<u32>,
    vmmem: (f64, u32),
    used: (f64, u32),
    cache: (f64, u32),
    cpu: (f64, u32),
    net: Option<(u64, u64)>,
    engine: Option<u64>,
    disk: Option<(u64, u64)>,
}

fn add(a: &mut (f64, u32), v: Option<f64>) {
    if let Some(v) = v {
        a.0 += v;
        a.1 += 1;
    }
}

fn mean(a: (f64, u32)) -> Option<f64> {
    (a.1 > 0).then(|| a.0 / a.1 as f64)
}

impl Acc {
    fn add(&mut self, s: &Sample) {
        add(&mut self.vmmem, s.vmmem.map(|v| v as f64));
        add(&mut self.used, s.used.map(|v| v as f64));
        add(&mut self.cache, s.cache.map(|v| v as f64));
        add(&mut self.cpu, s.cpu);
        self.net = s.net.or(self.net);
        self.engine = s.engine.or(self.engine);
        self.disk = s.disk.or(self.disk);
    }
    fn take(&mut self) -> Option<Raw> {
        let t = self.t?;
        let a = std::mem::take(self);
        let r = Raw {
            t,
            vmmem: mean(a.vmmem).map(|v| v as u64),
            used: mean(a.used).map(|v| v as u64),
            cache: mean(a.cache).map(|v| v as u64),
            cpu: mean(a.cpu),
            net: a.net,
            engine: a.engine,
            disk: a.disk,
        };
        Some(r)
    }
}

// ---------------------------------------------------------------- úložiště

#[derive(Default)]
struct Inner {
    dir: Option<PathBuf>,
    enabled: bool,
    /// soubory už byly načtené (do té doby se nic nezapisuje, aby se nepřepsala stará data)
    loaded: bool,
    raw: Vec<Raw>,
    agg: Vec<Agg>,
    events: Vec<FreeEvent>,
    /// kolik prvních záznamů z každého vektoru už je na disku
    saved: [usize; 3],
    /// soubor se musí přepsat celý (useknutý konec, zahozená stará data, chybná hlavička)
    rewrite: [bool; 3],
    acc: Acc,
    /// CPU z disku pro dlaždici na přehledu (vyzvedne UI)
    cpu_preload: Option<Vec<(f64, f64)>>,
}

/// Načtená data jednoho úložiště.
#[derive(Debug, Default)]
pub struct Loaded {
    pub raw: Vec<Raw>,
    pub agg: Vec<Agg>,
    pub events: Vec<FreeEvent>,
    /// soubor je potřeba přepsat (useknutý konec / chybná hlavička)
    pub broken: [bool; 3],
}

/// Načte soubory ze složky (chybějící soubory = prázdné). Záznamy z budoucnosti
/// a staré nad dobu uchování se zahodí.
pub fn read_dir(dir: &Path, now: f64) -> Loaded {
    let mut l = Loaded::default();
    let now_u = now.max(0.0) as u32;
    let future = now_u.saturating_add(86_400);
    for k in Kind::ALL {
        let Ok(data) = std::fs::read(dir.join(k.file())) else { continue };
        let (recs, ok) = split_records(&data, k);
        l.broken[k.idx()] = !ok;
        match k {
            Kind::Raw => {
                let min = now_u.saturating_sub(RAW_KEEP);
                for r in recs.iter().filter_map(|b| Raw::decode(b)) {
                    if r.t >= min && r.t <= future && l.raw.last().is_none_or(|p| r.t > p.t) {
                        l.raw.push(r);
                    }
                }
            }
            Kind::Agg => {
                let min = now_u.saturating_sub(AGG_KEEP);
                for a in recs.iter().filter_map(|b| Agg::decode(b)) {
                    if a.t >= min && a.t <= future && l.agg.last().is_none_or(|p| a.t > p.t) {
                        l.agg.push(a);
                    }
                }
            }
            Kind::Events => {
                let min = now - AGG_KEEP as f64;
                for e in recs.iter().filter_map(|b| decode_event(b)) {
                    if e.t >= min && e.t <= future as f64 {
                        l.events.push(e);
                    }
                }
            }
        }
        // vynechané záznamy (staré, mimo pořadí) -> soubor přepsat
        let kept = match k {
            Kind::Raw => l.raw.len(),
            Kind::Agg => l.agg.len(),
            Kind::Events => l.events.len(),
        };
        if kept != recs.len() {
            l.broken[k.idx()] = true;
        }
    }
    l
}

/// Zapíše soubor celý: dočasný soubor + přejmenování (při pádu zůstane starý celý).
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

fn append(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
    f.write_all(bytes)
}

/// Jeden zápis souboru připravený pod zámkem, provedený bez něj.
struct Job {
    kind: Kind,
    full: bool,
    bytes: Vec<u8>,
    /// počet záznamů, které budou po zápisu na disku
    len: usize,
}

fn encode_all<T>(items: &[T], enc: impl Fn(&T, &mut Vec<u8>)) -> Vec<u8> {
    let mut v = Vec::new();
    for i in items {
        enc(i, &mut v);
    }
    v
}

/// Historie v paměti + soubory na disku.
#[derive(Default)]
pub struct Store {
    inner: Mutex<Inner>,
    /// zápisy na disk jeden po druhém (bez držení `inner`)
    io: Mutex<()>,
}

impl Store {
    #[cfg(test)]
    pub fn new(dir: Option<PathBuf>, enabled: bool) -> Self {
        Self { inner: Mutex::new(Inner { dir, enabled, ..Default::default() }), io: Mutex::new(()) }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> Option<R> {
        self.inner.lock().ok().map(|mut g| f(&mut g))
    }

    pub fn configure(&self, dir: Option<PathBuf>, enabled: bool) {
        self.with(|i| {
            i.dir = dir;
            i.enabled = enabled;
        });
    }

    pub fn enabled(&self) -> bool {
        self.with(|i| i.enabled).unwrap_or(false)
    }

    /// Zapne / vypne ukládání; po zapnutí se (na pozadí) načte, co už na disku je.
    pub fn set_enabled(&'static self, on: bool) {
        let load = self.with(|i| {
            i.enabled = on;
            on && !i.loaded
        });
        if load == Some(true) {
            std::thread::spawn(move || self.load(crate::memory::now_secs()));
        }
    }

    /// Přidá vzorek; vzorky ve stejné minutě se zprůměrují.
    pub fn record(&self, t: f64, s: Sample) {
        let m = (t.max(0.0) as u32) / RAW_STEP * RAW_STEP;
        self.with(|i| {
            if i.acc.t != Some(m) {
                if let Some(r) = i.acc.take() {
                    push_raw(&mut i.raw, r);
                }
                i.acc.t = Some(m);
            }
            i.acc.add(&s);
        });
    }

    pub fn record_event(&self, e: FreeEvent) {
        self.with(|i| i.events.push(e));
    }

    /// Načte soubory a připojí je před data nasbíraná od spuštění.
    pub fn load(&self, now: f64) {
        let _io = self.io.lock();
        let Some(dir) = self.with(|i| i.dir.clone()).flatten() else {
            self.with(|i| i.loaded = true);
            return;
        };
        let l = read_dir(&dir, now);
        self.with(|i| {
            if i.loaded {
                return;
            }
            let first = i.raw.first().map(|r| r.t).unwrap_or(u32::MAX);
            let loaded_raw = l.raw.len();
            let mut raw: Vec<Raw> = l.raw.into_iter().filter(|r| r.t < first).collect();
            let n_raw = raw.len();
            raw.append(&mut i.raw);
            i.raw = raw;
            i.agg = l.agg;
            let n_ev = l.events.len();
            let mut ev = l.events;
            ev.append(&mut i.events);
            i.events = ev;
            i.saved = [n_raw, i.agg.len(), n_ev];
            i.rewrite = l.broken;
            // záznamy překryté daty z tohoto běhu -> soubor přepsat
            i.rewrite[0] |= n_raw != loaded_raw;
            i.loaded = true;
        });
    }

    /// Dokončené minuty -> minutové záznamy, hotové buckety -> agregace, staré záznamy pryč
    /// a zápis na disk. `last` = ukončení aplikace (uloží i rozpracovanou minutu).
    pub fn flush(&self, now: f64, last: bool) -> Result<(), String> {
        let _io = self.io.lock().map_err(|e| e.to_string())?;
        let now_u = now.max(0.0) as u32;
        let jobs = self
            .with(|i| {
                let cur_min = now_u / RAW_STEP * RAW_STEP;
                if last || i.acc.t.is_some_and(|t| t < cur_min) {
                    if let Some(r) = i.acc.take() {
                        push_raw(&mut i.raw, r);
                    }
                }
                // agregace dokončených bucketů
                let done = bucket(now_u);
                let from = i.agg.last().map(|a| a.t + AGG_STEP).unwrap_or(0);
                let start = i.raw.partition_point(|r| bucket(r.t) < from);
                let end = i.raw.partition_point(|r| bucket(r.t) < done);
                if start < end {
                    let prev = net_before(&i.raw, start);
                    let new = aggregate(&i.raw[start..end], prev);
                    i.agg.extend(new);
                }
                // uchování: zahodit až s rezervou, ať se soubor nepřepisuje při každém zápisu
                trim(&mut i.raw, |r| r.t, now_u.saturating_sub(RAW_KEEP), RAW_SLACK, &mut i.saved[0], &mut i.rewrite[0]);
                trim(&mut i.agg, |a| a.t, now_u.saturating_sub(AGG_KEEP), AGG_SLACK, &mut i.saved[1], &mut i.rewrite[1]);
                trim(&mut i.events, |e| e.t as u32, now_u.saturating_sub(AGG_KEEP), AGG_SLACK, &mut i.saved[2], &mut i.rewrite[2]);
                if !i.enabled || !i.loaded {
                    return None;
                }
                let dir = i.dir.clone()?;
                let mut jobs = Vec::new();
                for k in Kind::ALL {
                    let x = k.idx();
                    let full = i.rewrite[x] || !dir.join(k.file()).exists();
                    let from = if full { 0 } else { i.saved[x] };
                    let (bytes, len) = match k {
                        Kind::Raw => (encode_all(&i.raw[from..], Raw::encode), i.raw.len()),
                        Kind::Agg => (encode_all(&i.agg[from..], Agg::encode), i.agg.len()),
                        Kind::Events => (encode_all(&i.events[from..], encode_event), i.events.len()),
                    };
                    if full || !bytes.is_empty() {
                        jobs.push(Job { kind: k, full, bytes, len });
                    }
                }
                Some((dir, jobs))
            })
            .flatten();
        let Some((dir, jobs)) = jobs else { return Ok(()) };
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let mut err = None;
        for j in jobs {
            let path = dir.join(j.kind.file());
            let r = if j.full {
                let mut b = header(j.kind).to_vec();
                b.extend_from_slice(&j.bytes);
                write_atomic(&path, &b)
            } else {
                append(&path, &j.bytes)
            };
            match r {
                Ok(()) => {
                    self.with(|i| {
                        i.saved[j.kind.idx()] = j.len;
                        i.rewrite[j.kind.idx()] = false;
                    });
                }
                Err(e) => {
                    // nepovedený přídavek mohl zapsat kus záznamu -> příště celý soubor
                    self.with(|i| i.rewrite[j.kind.idx()] = true);
                    err = Some(format!("{}: {e}", path.display()));
                }
            }
        }
        err.map_or(Ok(()), Err)
    }

    /// Smaže historii z disku i z paměti.
    pub fn clear(&self) -> Result<(), String> {
        let _io = self.io.lock().map_err(|e| e.to_string())?;
        let dir = self
            .with(|i| {
                i.raw.clear();
                i.agg.clear();
                i.events.clear();
                i.acc = Acc::default();
                i.saved = [0; 3];
                i.rewrite = [false; 3];
                i.cpu_preload = None;
                i.dir.clone()
            })
            .flatten();
        if let Some(dir) = dir {
            for k in Kind::ALL {
                let p = dir.join(k.file());
                if p.exists() {
                    std::fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                }
                let _ = std::fs::remove_file(p.with_extension("tmp"));
            }
        }
        Ok(())
    }

    /// Minutové záznamy od času `since`.
    pub fn raw_since(&self, since: f64) -> Vec<Raw> {
        self.with(|i| {
            let s = since.max(0.0) as u32;
            let k = i.raw.partition_point(|r| r.t < s);
            i.raw[k..].to_vec()
        })
        .unwrap_or_default()
    }

    /// Agregace od `since`: uložené + dopočítané z minutových záznamů, které ještě nejsou
    /// agregované (včetně rozpracovaného bucketu).
    pub fn agg_since(&self, since: f64) -> Vec<Agg> {
        self.with(|i| {
            let s = since.max(0.0) as u32;
            let k = i.agg.partition_point(|a| a.t + AGG_STEP <= s);
            let mut v = i.agg[k..].to_vec();
            let from = i.agg.last().map(|a| a.t + AGG_STEP).unwrap_or(0).max(bucket(s));
            let start = i.raw.partition_point(|r| bucket(r.t) < from);
            v.extend(aggregate(&i.raw[start..], net_before(&i.raw, start)));
            v
        })
        .unwrap_or_default()
    }

    pub fn events_since(&self, since: f64) -> Vec<FreeEvent> {
        self.with(|i| i.events.iter().filter(|e| e.t >= since).copied().collect()).unwrap_or_default()
    }

    /// Body pro graf paměti (poslední hodina) a uvolnění paměti z uložených dat.
    pub fn mem_preload(&self, now: f64) -> (Vec<MemPoint>, Vec<FreeEvent>) {
        let since = now - crate::memory::HISTORY_SECS;
        let pts = self
            .raw_since(since)
            .iter()
            .filter_map(|r| Some(MemPoint::new(r.t as f64, r.vmmem, r.used?, r.cache.unwrap_or(0))))
            .collect();
        (pts, self.events_since(since))
    }

    /// Body CPU za posledních `window` sekund.
    pub fn cpu_points(&self, now: f64, window: f64) -> Vec<(f64, f64)> {
        self.raw_since(now - window).iter().filter_map(|r| Some((r.t as f64, r.cpu?))).collect()
    }

    pub fn set_cpu_preload(&self, v: Vec<(f64, f64)>) {
        self.with(|i| i.cpu_preload = Some(v));
    }

    /// CPU z disku pro dlaždici (jen jednou).
    pub fn take_cpu_preload(&self) -> Option<Vec<(f64, f64)>> {
        self.with(|i| i.cpu_preload.take()).flatten()
    }

    /// Týdenní souhrn.
    pub fn summary(&self, now: f64) -> Summary {
        let since = now - WEEK;
        summary(&self.agg_since(since), &self.events_since(since), now)
    }

    /// Počet záznamů v paměti (raw, agg, události) – pro testy.
    #[cfg(test)]
    pub fn counts(&self) -> (usize, usize, usize) {
        self.with(|i| (i.raw.len(), i.agg.len(), i.events.len())).unwrap_or_default()
    }
}

fn push_raw(raw: &mut Vec<Raw>, r: Raw) {
    // hodiny šly pozpátku -> záznam zahodit, pořadí musí zůstat rostoucí
    if raw.last().is_none_or(|p| r.t > p.t) {
        raw.push(r);
    }
}

/// Zahodí záznamy starší než `min`, ale až když nejstarší přesahuje o `slack`.
fn trim<T>(v: &mut Vec<T>, t: impl Fn(&T) -> u32, min: u32, slack: u32, saved: &mut usize, rewrite: &mut bool) {
    if v.first().is_some_and(|x| t(x) < min.saturating_sub(slack)) {
        let k = v.partition_point(|x| t(x) < min);
        v.drain(..k);
        *saved = saved.saturating_sub(k);
        *rewrite = true;
    }
}

// ---------------------------------------------------------------- globální úložiště

static GLOBAL: OnceLock<Store> = OnceLock::new();

/// Úložiště aplikace. Dokud ho `start` nenastaví, nic nezapisuje (testy, shim).
pub fn store() -> &'static Store {
    GLOBAL.get_or_init(Store::default)
}

/// Složka historie v datové složce aplikace (respektuje portable režim a `WSLCD_DATA_DIR`).
pub fn dir() -> Option<PathBuf> {
    crate::config::data_dir().map(|d| d.join("history"))
}

/// Načte historii na pozadí, předvyplní graf paměti a CPU a pak jednou za `FLUSH_EVERY`
/// zapisuje na disk.
pub fn start(shared: std::sync::Arc<crate::worker::Shared>, enabled: bool) {
    let s = store();
    s.configure(dir(), enabled);
    std::thread::Builder::new()
        .name("history".into())
        .spawn(move || {
            let now = crate::memory::now_secs();
            if enabled {
                s.load(now);
            }
            let (pts, evs) = s.mem_preload(now);
            if let Ok(mut m) = shared.memory.lock() {
                m.history.prefill(pts, evs, now);
            }
            s.set_cpu_preload(s.cpu_points(now, crate::ui::dashboard::CPU_WINDOW));
            loop {
                std::thread::sleep(FLUSH_EVERY);
                let _ = s.flush(crate::memory::now_secs(), false);
            }
        })
        .ok();
}

/// Při ukončení aplikace: uloží i rozpracovanou minutu.
pub fn flush_on_exit() {
    let _ = store().flush(crate::memory::now_secs(), true);
}

// ---------------------------------------------------------------- souhrn

/// Špička a průměr.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PeakAvg {
    pub peak: f64,
    pub avg: f64,
}

/// Souhrn za posledních 7 dní.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Summary {
    /// čas prvních dat v okně
    pub from: Option<f64>,
    /// data pokrývají dost času (zhruba den)
    pub enough: bool,
    pub vmmem: Option<PeakAvg>,
    pub used: Option<PeakAvg>,
    pub cpu: Option<PeakAvg>,
    pub frees_auto: u32,
    pub frees_manual: u32,
    /// kolik uvolnění vrátilo (VmmemWSL před − po, jinak pokles cache)
    pub freed: u64,
    /// úložiště enginu (nejstarší v okně, teď)
    pub engine: Option<(u64, u64)>,
    /// použito na disku distribuce (nejstarší v okně, teď)
    pub disk_used: Option<(u64, u64)>,
    pub disk_free: Option<u64>,
    /// přeneseno přes eth0 VM (rx, tx)
    pub net: Option<(u64, u64)>,
    /// nejvytíženější hodina (začátek, průměrné CPU)
    pub busiest: Option<(f64, f64)>,
}

fn peak_avg(aggs: &[Agg], f: impl Fn(&Agg) -> Option<Mma>) -> Option<PeakAvg> {
    let (mut peak, mut sum, mut n) = (f64::MIN, 0.0, 0.0);
    for a in aggs {
        if let Some(m) = f(a) {
            let w = a.n.max(1) as f64;
            peak = peak.max(m.max);
            sum += m.avg * w;
            n += w;
        }
    }
    (n > 0.0).then(|| PeakAvg { peak, avg: sum / n })
}

/// Souhrn z agregací a uvolnění paměti za 7 dní před `now`.
pub fn summary(aggs: &[Agg], events: &[FreeEvent], now: f64) -> Summary {
    let since = now - WEEK;
    let aggs: Vec<Agg> = aggs.iter().filter(|a| a.t as f64 + AGG_STEP as f64 > since && a.t as f64 <= now).copied().collect();
    let mut s = Summary { from: aggs.first().map(|a| (a.t as f64).max(since)), ..Default::default() };
    s.enough = s.from.is_some_and(|f| now - f >= SUMMARY_MIN_SPAN);
    s.vmmem = peak_avg(&aggs, |a| a.vmmem);
    s.used = peak_avg(&aggs, |a| a.used);
    s.cpu = peak_avg(&aggs, |a| a.cpu);
    for e in events.iter().filter(|e| e.t >= since && e.t <= now) {
        if e.auto {
            s.frees_auto += 1;
        } else {
            s.frees_manual += 1;
        }
        s.freed += match (e.vmmem_before, e.vmmem_after) {
            (Some(b), Some(a)) => b.saturating_sub(a),
            _ => e.cache_before.saturating_sub(e.cache_after),
        };
    }
    let first_last = |f: &dyn Fn(&Agg) -> Option<u64>| {
        let first = aggs.iter().find_map(f)?;
        let last = aggs.iter().rev().find_map(f)?;
        Some((first, last))
    };
    s.engine = first_last(&|a| a.engine);
    s.disk_used = first_last(&|a| a.disk.map(|d| d.0));
    s.disk_free = aggs.iter().rev().find_map(|a| a.disk.map(|d| d.1));
    s.net = aggs.iter().filter_map(|a| a.net).reduce(|x, y| (x.0 + y.0, x.1 + y.1));
    // nejvytíženější hodina podle průměrného CPU kontejnerů
    let mut hours: Vec<(u32, f64, f64)> = Vec::new();
    for a in &aggs {
        let Some(c) = a.cpu else { continue };
        let h = a.t - a.t % 3600;
        let w = a.n.max(1) as f64;
        match hours.last_mut() {
            Some(x) if x.0 == h => {
                x.1 += c.avg * w;
                x.2 += w;
            }
            _ => hours.push((h, c.avg * w, w)),
        }
    }
    s.busiest = hours
        .iter()
        .map(|(h, sum, w)| (*h as f64, sum / w))
        .filter(|x| x.1 > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1 << 30;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wslcd-hist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn raw(t: u32) -> Raw {
        Raw {
            t,
            vmmem: Some(3 * G),
            used: Some(G),
            cache: Some(G / 2),
            cpu: Some(12.34),
            net: Some((1_000_000_007, 42)),
            engine: Some(20 * G),
            disk: Some((40 * G, 200 * G)),
        }
    }

    #[test]
    fn raw_roundtrip_and_flags() {
        let r = raw(1_700_000_040);
        let mut b = Vec::new();
        r.encode(&mut b);
        assert_eq!(b.len(), RAW_SIZE);
        assert_eq!(Raw::decode(&b), Some(r));
        // chybějící hodnoty zůstanou None
        let r = Raw { t: 60, used: Some(5 * KIB), ..Default::default() };
        let mut b = Vec::new();
        r.encode(&mut b);
        assert_eq!(Raw::decode(&b), Some(r));
        assert!(Raw::decode(&b[..RAW_SIZE - 1]).is_none());
    }

    #[test]
    fn agg_and_event_roundtrip() {
        let m = Mma { min: G as f64, avg: 2.0 * G as f64, max: 3.0 * G as f64 };
        let a = Agg {
            t: 900,
            n: 15,
            vmmem: Some(m),
            used: None,
            cache: Some(m),
            cpu: Some(Mma { min: 0.5, avg: 1.25, max: 99.5 }),
            net: Some((u64::MAX / 2, 7)),
            engine: None,
            disk: Some((G, 2 * G)),
        };
        let mut b = Vec::new();
        a.encode(&mut b);
        assert_eq!(b.len(), AGG_SIZE);
        assert_eq!(Agg::decode(&b), Some(a));
        let e = FreeEvent { t: 1234.0, auto: true, vmmem_before: Some(6 * G), vmmem_after: None, cache_before: 2 * G, cache_after: G / 4 };
        let mut b = Vec::new();
        encode_event(&e, &mut b);
        assert_eq!(b.len(), EV_SIZE);
        assert_eq!(decode_event(&b), Some(e));
    }

    #[test]
    fn values_are_rounded_to_storage_units() {
        let r = Raw { t: 0, vmmem: Some(1500), engine: Some(3 * MIB + 5), cpu: Some(700.0), ..Default::default() };
        let mut b = Vec::new();
        r.encode(&mut b);
        let d = Raw::decode(&b).unwrap();
        assert_eq!((d.vmmem, d.engine), (Some(1024), Some(3 * MIB)));
        assert_eq!(d.cpu, Some(655.35), "u16 setiny procenta se nasytí");
    }

    #[test]
    fn truncated_tail_and_bad_header() {
        let mut data = header(Kind::Raw).to_vec();
        for t in [60, 120, 180] {
            raw(t).encode(&mut data);
        }
        let (recs, ok) = split_records(&data, Kind::Raw);
        assert_eq!((recs.len(), ok), (3, true));
        data.truncate(data.len() - 5);
        let (recs, ok) = split_records(&data, Kind::Raw);
        assert_eq!((recs.len(), ok), (2, false));
        // jiný druh / verze -> nic
        assert_eq!(split_records(&data, Kind::Agg).0.len(), 0);
        let mut v2 = data.clone();
        v2[5] = 2;
        assert!(split_records(&v2, Kind::Raw).0.is_empty());
        assert_eq!(split_records(&[], Kind::Raw), (vec![], true));
    }

    #[test]
    fn aggregate_min_avg_max_and_buckets() {
        let mut v = Vec::new();
        // bucket 0..900: 3 záznamy, bucket 900..1800: 1 záznam (hranice 900 patří do druhého)
        for (t, vm, cpu) in [(0, 1, 10.0), (60, 3, 20.0), (840, 2, 30.0), (900, 5, 0.0)] {
            v.push(Raw { t, vmmem: Some(vm * G), cpu: Some(cpu), ..Default::default() });
        }
        v[1].used = Some(G);
        let a = aggregate(&v, None);
        assert_eq!(a.len(), 2);
        assert_eq!((a[0].t, a[0].n, a[1].t, a[1].n), (0, 3, 900, 1));
        assert_eq!(a[0].vmmem, Some(Mma { min: G as f64, avg: 2.0 * G as f64, max: 3.0 * G as f64 }));
        assert_eq!(a[0].cpu, Some(Mma { min: 10.0, avg: 20.0, max: 30.0 }));
        assert_eq!(a[0].used, Some(Mma { min: G as f64, avg: G as f64, max: G as f64 }), "průměr jen ze záznamů s hodnotou");
        assert_eq!(a[1].used, None);
    }

    #[test]
    fn aggregate_net_deltas_gaps_and_reset() {
        let n = |t, rx, tx| Raw { t, net: Some((rx, tx)), ..Default::default() };
        // mezera mezi 120 a 3600 (aplikace neběžela) – bucket 900..3600 chybí úplně
        let v = [n(0, 100, 10), n(60, 150, 20), n(120, 200, 30), n(3600, 1200, 130), n(3660, 50, 5)];
        let a = aggregate(&v, Some((90, 10)));
        assert_eq!(a.iter().map(|a| a.t).collect::<Vec<_>>(), vec![0, 3600]);
        assert_eq!(a[0].net, Some((110, 20)));
        // provoz během mezery se připíše prvnímu bucketu po ní; restart VM = čítač od nuly
        assert_eq!(a[1].net, Some((1000 + 50, 100 + 5)));
        // bez předchozí hodnoty začíná přírůstek nulou
        assert_eq!(aggregate(&v[..1], None)[0].net, Some((0, 0)));
        assert_eq!(net_delta((10, 10), (5, 20)), (5, 10));
    }

    #[test]
    fn minute_accumulator_averages_and_keeps_last_counters() {
        let s = Store::new(None, false);
        s.record(60.0, Sample { vmmem: Some(2 * G), cpu: Some(10.0), net: Some((1, 1)), ..Default::default() });
        s.record(90.0, Sample { vmmem: Some(4 * G), net: Some((5, 6)), ..Default::default() });
        s.record(119.9, Sample { cpu: Some(20.0), disk: Some((G, G)), ..Default::default() });
        assert_eq!(s.counts().0, 0, "rozpracovaná minuta");
        s.record(125.0, Sample { used: Some(G), ..Default::default() });
        let r = s.raw_since(0.0);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0], Raw { t: 60, vmmem: Some(3 * G), used: None, cache: None, cpu: Some(15.0), net: Some((5, 6)), engine: None, disk: Some((G, G)) });
        // hodiny pozpátku -> starší minuta se zahodí
        s.record(30.0, Sample { used: Some(1), ..Default::default() });
        s.record(200.0, Sample { used: Some(1), ..Default::default() });
        assert_eq!(s.raw_since(0.0).iter().map(|r| r.t).collect::<Vec<_>>(), vec![60, 120]);
    }

    #[test]
    fn flush_reload_append_and_truncated_tail() {
        let dir = tmp("flush");
        let now = 2_000_000_000.0;
        let s = Store::new(Some(dir.clone()), true);
        s.load(now);
        for m in 0..40 {
            s.record(now - 3600.0 + m as f64 * 60.0, Sample { vmmem: Some(G + m * MIB), used: Some(G), cache: Some(G / 2), cpu: Some(m as f64), ..Default::default() });
        }
        s.record_event(FreeEvent { t: now - 100.0, auto: false, vmmem_before: Some(4 * G), vmmem_after: Some(2 * G), cache_before: G, cache_after: 0 });
        s.flush(now, true).unwrap();
        let (r1, a1, e1) = s.counts();
        assert_eq!((r1, e1), (40, 1));
        assert!(a1 >= 2, "dokončené buckety agregované: {a1}");
        let len = |k: Kind| std::fs::metadata(dir.join(k.file())).unwrap().len() as usize;
        assert_eq!(len(Kind::Raw), HEADER + 40 * RAW_SIZE);
        // další flush jen přidá nové záznamy (append)
        s.record(now + 60.0, Sample { used: Some(G), ..Default::default() });
        s.flush(now + 61.0, true).unwrap();
        assert_eq!(len(Kind::Raw), HEADER + 41 * RAW_SIZE);
        // useknutý konec: načte se bez posledního záznamu a soubor se při dalším zápisu přepíše
        let f = std::fs::OpenOptions::new().write(true).open(dir.join("raw.bin")).unwrap();
        f.set_len((HEADER + 41 * RAW_SIZE - 7) as u64).unwrap();
        drop(f);
        let s2 = Store::new(Some(dir.clone()), true);
        s2.load(now + 62.0);
        assert_eq!(s2.counts(), (40, a1, 1));
        s2.record(now + 180.0, Sample { used: Some(G), ..Default::default() });
        s2.flush(now + 181.0, true).unwrap();
        assert_eq!(len(Kind::Raw), HEADER + 41 * RAW_SIZE);
        let s3 = Store::new(Some(dir.clone()), true);
        s3.load(now + 182.0);
        assert_eq!(s3.counts().0, 41);
        assert_eq!(s3.raw_since(0.0)[0].vmmem, Some(G));
        // vypnuté ukládání nic nezapíše
        let off = Store::new(Some(dir.clone()), false);
        off.load(now);
        off.record(now + 600.0, Sample { used: Some(1), ..Default::default() });
        off.flush(now + 700.0, true).unwrap();
        assert_eq!(len(Kind::Raw), HEADER + 41 * RAW_SIZE);
        // smazání historie
        s3.clear().unwrap();
        assert!(!dir.join("raw.bin").exists() && !dir.join("agg.bin").exists());
        assert_eq!(s3.counts(), (0, 0, 0));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retention_compacts_old_records() {
        let dir = tmp("retention");
        let t0 = 1_900_000_000.0;
        let s = Store::new(Some(dir.clone()), true);
        s.load(t0);
        // 3 dny po 10 minutách
        let mut t = t0;
        while t < t0 + 3.0 * 86_400.0 {
            s.record(t, Sample { used: Some(G), cpu: Some(1.0), ..Default::default() });
            t += 600.0;
        }
        s.record_event(FreeEvent { t: t0, auto: true, vmmem_before: None, vmmem_after: None, cache_before: G, cache_after: 0 });
        let now = t0 + 3.0 * 86_400.0;
        s.flush(now, true).unwrap();
        let raw = s.raw_since(0.0);
        assert!(raw.first().unwrap().t as f64 >= now - RAW_KEEP as f64, "minutové záznamy jen 48 h");
        assert_eq!(s.counts().2, 1, "událost se drží 35 dní");
        let aggs = s.agg_since(0.0);
        assert!(aggs.first().unwrap().t as f64 <= t0 + 900.0, "agregace pokrývají celé 3 dny");
        // soubor po kompakci obsahuje jen platné záznamy
        let raw_len = std::fs::metadata(dir.join("raw.bin")).unwrap().len() as usize;
        assert_eq!(raw_len, HEADER + raw.len() * RAW_SIZE);
        assert!(!dir.join("raw.tmp").exists());
        // načtení po 40 dnech: nic starého
        let s2 = Store::new(Some(dir.clone()), true);
        s2.load(now + 40.0 * 86_400.0);
        assert_eq!(s2.counts(), (0, 0, 0));
        // přepsání při dalším zápisu (vynechané staré záznamy)
        s2.flush(now + 40.0 * 86_400.0, false).unwrap();
        assert_eq!(std::fs::metadata(dir.join("agg.bin")).unwrap().len() as usize, HEADER);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn size_estimate_is_small() {
        let raw = (RAW_KEEP + RAW_SLACK) as usize / RAW_STEP as usize * RAW_SIZE;
        let agg = (AGG_KEEP + AGG_SLACK) as usize / AGG_STEP as usize * AGG_SIZE;
        assert!(raw + agg < 1024 * 1024, "{raw} + {agg}");
    }

    #[test]
    fn agg_since_merges_stored_and_pending() {
        let s = Store::new(None, true);
        s.load(0.0);
        for m in 0..60 {
            s.record(m as f64 * 60.0, Sample { cpu: Some(m as f64), ..Default::default() });
        }
        s.flush(1800.0, false).unwrap();
        // uložené 0, 900; dopočítané 1800, 2700 (rozpracovaná minuta 3540 ještě ne)
        let a = s.agg_since(0.0);
        assert_eq!(a.iter().map(|a| a.t).collect::<Vec<_>>(), vec![0, 900, 1800, 2700]);
        assert_eq!(a[3].n, 14);
        assert_eq!(s.agg_since(2000.0).iter().map(|a| a.t).collect::<Vec<_>>(), vec![1800, 2700]);
    }

    #[test]
    fn preload_into_memory_history_and_cpu() {
        let s = Store::new(None, true);
        s.load(0.0);
        let now = 100_020.0; // celá minuta
        for m in 0..120 {
            let t = now - 7200.0 + m as f64 * 60.0;
            s.record(t, Sample { vmmem: Some(G), used: Some(m * MIB), cache: Some(0), cpu: Some(m as f64), ..Default::default() });
        }
        s.record_event(FreeEvent { t: now - 600.0, auto: true, vmmem_before: None, vmmem_after: None, cache_before: 1, cache_after: 0 });
        s.record_event(FreeEvent { t: now - 5000.0, auto: true, vmmem_before: None, vmmem_after: None, cache_before: 1, cache_after: 0 });
        s.flush(now, false).unwrap();
        let (pts, evs) = s.mem_preload(now);
        assert_eq!(pts.len(), 60);
        assert_eq!(evs.len(), 1);
        // doplní se před body, které už v historii jsou
        let mut h = crate::memory::History::default();
        h.push(MemPoint::new(now - 90.0, None, 7, 7));
        h.push(MemPoint::new(now - 10.0, None, 8, 8));
        h.prefill(pts, evs, now);
        assert!(h.points.iter().zip(h.points.iter().skip(1)).all(|(a, b)| a.t < b.t));
        assert_eq!(h.points.back().unwrap().used, 8);
        assert_eq!(h.events.len(), 1);
        assert!(h.points.front().unwrap().t >= now - crate::memory::HISTORY_SECS);
        let cpu = s.cpu_points(now, 900.0);
        assert_eq!(cpu.len(), 15);
        assert_eq!(cpu.last().unwrap().1, 119.0);
        s.set_cpu_preload(cpu);
        assert!(s.take_cpu_preload().is_some());
        assert!(s.take_cpu_preload().is_none());
    }

    #[test]
    fn summary_peaks_averages_deltas_and_busiest_hour() {
        let now = 1_000_000.0 * 900.0; // zarovnané na bucket
        let mut aggs = Vec::new();
        let start = (now - 8.0 * 86_400.0) as u32; // den před oknem – nezapočítá se
        let mut t = start;
        while (t as f64) < now {
            let in_week = t as f64 >= now - WEEK;
            let busy = t as f64 >= now - 2.0 * 86_400.0 && (t as f64) < now - 2.0 * 86_400.0 + 3600.0;
            let cpu = if busy { 80.0 } else { 5.0 };
            let mm = |v: f64| Some(Mma { min: v / 2.0, avg: v, max: v * 2.0 });
            aggs.push(Agg {
                t,
                n: 15,
                vmmem: mm(if in_week { 4.0 } else { 100.0 } * G as f64),
                used: mm(G as f64),
                cache: None,
                cpu: Some(Mma { min: 0.0, avg: cpu, max: cpu + 10.0 }),
                net: Some((1000, 10)),
                engine: Some(if in_week { 10 * G } else { 0 } + (t - start) as u64),
                disk: Some((30 * G + (t - start) as u64, 100 * G)),
            });
            t += AGG_STEP;
        }
        let ev = |t: f64, auto, b: Option<u64>, a: Option<u64>| FreeEvent { t, auto, vmmem_before: b, vmmem_after: a, cache_before: 3 * G, cache_after: G };
        let events = [
            ev(now - 3600.0, true, Some(6 * G), Some(4 * G)),
            ev(now - 7200.0, true, None, None),
            ev(now - 100.0, false, Some(5 * G), Some(6 * G)),
            ev(now - 9.0 * 86_400.0, false, Some(9 * G), Some(G)),
        ];
        let s = summary(&aggs, &events, now);
        assert!(s.enough);
        assert_eq!(s.from, Some(now - WEEK));
        let vm = s.vmmem.unwrap();
        assert_eq!((vm.peak, vm.avg), (8.0 * G as f64, 4.0 * G as f64), "starší data se nepočítají");
        let cpu = s.cpu.unwrap();
        assert_eq!(cpu.peak, 90.0);
        let n = 7 * 96;
        assert!((cpu.avg - (4.0 * 80.0 + (n - 4) as f64 * 5.0) / n as f64).abs() < 1e-9);
        assert_eq!((s.frees_auto, s.frees_manual), (2, 1));
        assert_eq!(s.freed, 2 * G + 2 * G, "VmmemWSL rozdíl, jinak pokles cache; nárůst = 0");
        let (e0, e1) = s.engine.unwrap();
        assert_eq!(e1 - e0, (n as u64 - 1) * 900);
        assert!(e0 >= 10 * G);
        assert_eq!(s.disk_free, Some(100 * G));
        assert_eq!(s.net, Some((n as u64 * 1000, n as u64 * 10)));
        assert_eq!(s.busiest, Some((now - 2.0 * 86_400.0, 80.0)));
    }

    #[test]
    fn summary_needs_about_a_day() {
        let now = 500_000.0 * 900.0;
        let a = |t: f64| Agg { t: t as u32, n: 1, cpu: Some(Mma::default()), ..Default::default() };
        assert!(!summary(&[], &[], now).enough);
        assert!(!summary(&[a(now - 3.0 * 3600.0)], &[], now).enough);
        assert!(summary(&[a(now - 21.0 * 3600.0)], &[], now).enough);
        // bez zátěže žádná nejvytíženější hodina
        assert_eq!(summary(&[a(now - 21.0 * 3600.0)], &[], now).busiest, None);
    }

    /// Skutečné vzorky ~2 minuty stejnou cestou jako aplikace do dočasné `WSLCD_DATA_DIR`,
    /// zápis, nové načtení a souhrn:
    /// `cargo test live_history_two_minutes -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_history_two_minutes() {
        // skutečná konfigurace jen ke čtení (distribuce, port, TLS), pak oddělená datová složka
        let real = crate::config::Config::load();
        crate::docker::configure_tls(real.tls).expect("TLS");
        let data = std::env::temp_dir().join(format!("wslcd-test-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        std::fs::create_dir_all(&data).unwrap();
        // SAFETY (env): test běží samostatně
        unsafe { std::env::set_var("WSLCD_DATA_DIR", &data) };
        let mut cfg = real.clone();
        cfg.alerts.toast = false;
        let hdir = dir().unwrap();
        assert!(hdir.starts_with(&data), "{}", hdir.display());
        let s = store();
        s.configure(Some(hdir.clone()), true);
        s.load(crate::memory::now_secs());
        let mut prev = std::collections::HashMap::new();
        let t0 = std::time::Instant::now();
        let mut n = 0;
        while t0.elapsed() < Duration::from_secs(125) {
            let t = std::time::Instant::now();
            crate::worker::testkit::history_sample(cfg.clone(), &mut prev, n % 6 == 0);
            n += 1;
            println!("vzorek {n} ({:?})", t.elapsed());
            std::thread::sleep(Duration::from_secs(10).saturating_sub(t.elapsed()));
        }
        let now = crate::memory::now_secs();
        s.flush(now, true).unwrap();
        for k in Kind::ALL {
            let len = std::fs::metadata(hdir.join(k.file())).map(|m| m.len()).unwrap_or(0);
            println!("{}: {len} B", k.file());
        }
        let s2 = Store::new(Some(hdir.clone()), true);
        s2.load(now);
        let (r, a, e) = s2.counts();
        println!("načteno: {r} minutových záznamů, {a} agregací, {e} uvolnění");
        let g = |v: u64| format!("{:.2} GB", v as f64 / (1u64 << 30) as f64);
        for x in s2.raw_since(0.0) {
            println!(
                "  t={} VmmemWSL {} used {} cache {} CPU {:?} eth0 {:?} engine {} disk {:?}",
                x.t,
                x.vmmem.map(g).unwrap_or_default(),
                x.used.map(g).unwrap_or_default(),
                x.cache.map(g).unwrap_or_default(),
                x.cpu.map(|c| (c * 100.0).round() / 100.0),
                x.net,
                x.engine.map(g).unwrap_or_default(),
                x.disk.map(|(u, f)| (g(u), g(f)))
            );
        }
        for x in s2.agg_since(0.0) {
            println!("  agg t={} n={} vmmem {:?} cpu {:?} net {:?}", x.t, x.n, x.vmmem.map(|m| g(m.max as u64)), x.cpu, x.net);
        }
        let sum = s2.summary(now);
        println!("souhrn (enough = {}): {sum:#?}", sum.enough);
        let _ = std::fs::remove_dir_all(&data);
        assert!(r >= 2, "aspoň dvě minuty dat");
        // v paměti plná přesnost, na disku KiB / MiB / setiny procenta
        let stored: Vec<Raw> = s
            .raw_since(0.0)
            .iter()
            .map(|r| {
                let mut b = Vec::new();
                r.encode(&mut b);
                Raw::decode(&b).unwrap()
            })
            .collect();
        assert_eq!(s2.raw_since(0.0), stored, "po načtení stejná data");
        let last = s2.raw_since(0.0).last().copied().unwrap();
        assert!(last.used.is_some() && last.net.is_some(), "paměť a eth0 v každé minutě: {last:?}");
        assert!(s2.raw_since(0.0).iter().any(|x| x.disk.is_some()), "disk distribuce");
        assert!(!sum.enough, "dvě minuty na souhrn nestačí");
    }
}
