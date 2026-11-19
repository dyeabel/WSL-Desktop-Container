//! Limity prostředků kontejneru (paměť, CPU, PIDs): čtení z `HostConfig`, formulář,
//! převod velikostí a tělo `POST /containers/{id}/update` pro Docker i Podman.
//!
//! Ověřeno živě na Podmanu 5.4 (rootful, cgroup v2), kompatibilní endpoint `/containers/{id}/update`:
//! - `Memory`, `MemoryReservation`, `MemorySwap`, `CpuQuota` + `CpuPeriod`, `CpuShares`,
//!   `CpusetCpus`, `PidsLimit` se projeví hned (cgroup `memory.max`, `memory.low`,
//!   `memory.swap.max`, `cpu.max`, `cpu.weight`, `cpuset.cpus`, `pids.max`);
//! - `NanoCpus` Podman **tiše ignoruje** (200 OK, beze změny) – CPU se proto posílá jako kvóta;
//! - hodnota 0 znamená „neměnit“ (u všech polí); odebrání limitu je -1 u `Memory`, `MemorySwap`,
//!   `CpuQuota` a `PidsLimit`; `MemoryReservation: -1` nastaví `memory.low=max` (opak odebrání),
//!   rezervaci tedy za běhu odebrat nejde; inspect pak hlásí -1 (a `NanoCpus` záporné) = bez limitu.
//!
//! Docker (podle zdrojů moby, živě neověřeno): 0 = neměnit; `Memory` a `MemoryReservation` musí být
//! aspoň 6 MB, takže limit paměti za běhu odebrat nejde; `NanoCpus` a `CpuQuota` se navzájem
//! vylučují podle toho, čím byl limit nastaven; zvýšení `Memory` nad uložený `MemorySwap` bez
//! současné změny swapu Docker odmítne – posíláme ho proto vždy s pamětí.

use crate::engine::Engine;
use serde_json::{json, Map, Value};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;
const TIB: u64 = 1024 * GIB;
/// Nejmenší limit paměti, který Docker přijme (`linuxMinMemory`).
pub const MIN_MEM: u64 = 6 * MIB;
/// Výchozí perioda CFS (µs) pro limit CPU kvótou.
pub const CPU_PERIOD: i64 = 100_000;

/// Předvolby velikostí paměti (text pole, popisek tlačítka).
pub const MEM_PRESETS: [(&str, &str); 4] = [("256m", "256 MB"), ("512m", "512 MB"), ("1g", "1 GB"), ("2g", "2 GB")];
/// Předvolby počtu CPU.
pub const CPU_PRESETS: [&str; 3] = ["0.5", "1", "2"];

/// Limity z `HostConfig` tak, jak je hlásí engine (0 / -1 = bez limitu).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Limits {
    pub memory: i64,
    pub reservation: i64,
    /// paměť + swap celkem (-1 = swap bez omezení, 0 = výchozí)
    pub swap: i64,
    pub nano_cpus: i64,
    pub cpu_quota: i64,
    pub cpu_period: i64,
    pub cpu_shares: i64,
    pub cpuset: String,
    pub pids: i64,
}

fn pos(v: i64) -> Option<u64> {
    (v > 0).then_some(v as u64)
}

impl Limits {
    /// Z objektu `HostConfig` (inspect kontejneru).
    pub fn from_host_config(h: &Value) -> Self {
        let n = |k: &str| h.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        Limits {
            memory: n("Memory"),
            reservation: n("MemoryReservation"),
            swap: n("MemorySwap"),
            nano_cpus: n("NanoCpus"),
            cpu_quota: n("CpuQuota"),
            cpu_period: n("CpuPeriod"),
            cpu_shares: n("CpuShares"),
            cpuset: h.get("CpusetCpus").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            pids: n("PidsLimit"),
        }
    }

    pub fn memory(&self) -> Option<u64> {
        pos(self.memory)
    }

    pub fn reservation(&self) -> Option<u64> {
        pos(self.reservation)
    }

    /// Paměť + swap celkem (jen když je omezené).
    pub fn swap(&self) -> Option<u64> {
        pos(self.swap)
    }

    /// Počet CPU (z `NanoCpus`, jinak z kvóty a periody).
    pub fn cpus(&self) -> Option<f64> {
        if self.nano_cpus > 0 {
            return Some(self.nano_cpus as f64 / 1e9);
        }
        if self.cpu_quota > 0 {
            let period = if self.cpu_period > 0 { self.cpu_period } else { CPU_PERIOD };
            return Some(self.cpu_quota as f64 / period as f64);
        }
        None
    }

    pub fn pids(&self) -> Option<u64> {
        pos(self.pids)
    }

    pub fn shares(&self) -> Option<u64> {
        pos(self.cpu_shares)
    }

    /// CPU limit menší než počet jader (`ncpu` 0 = neznámý). Docker „odebírá“ limit nastavením
    /// na všechna jádra – takový limit nic neomezuje.
    pub fn effective_cpus(&self, ncpu: i64) -> Option<f64> {
        self.cpus().filter(|c| ncpu <= 0 || *c < ncpu as f64 - 0.005)
    }
}

/// Velikost jako `512m`, `1g`, `1.5g`, `256 MB`, `2GiB`, `1,5g` (binární jednotky jako
/// `docker run --memory`; číslo bez jednotky = bajty).
pub fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim().to_ascii_lowercase().replace(',', ".");
    let err = || tf!("Neplatná velikost: {}. Zadej třeba 512m nebo 1.5g.", "Invalid size: {}. Use e.g. 512m or 1.5g.", s.trim());
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let mult = match unit.trim() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => KIB,
        "m" | "mb" | "mib" => MIB,
        "g" | "gb" | "gib" => GIB,
        "t" | "tb" | "tib" => TIB,
        _ => return Err(err()),
    };
    let v: f64 = num.parse().map_err(|_| err())?;
    if !v.is_finite() || v < 0.0 || (mult == 1 && num.contains('.')) {
        return Err(err());
    }
    let b = (v * mult as f64).round();
    if b > i64::MAX as f64 {
        return Err(err());
    }
    Ok(b as u64)
}

/// Zpětně parsovatelný zápis velikosti: `512m`, `1g`, `1.5g`, `1536k`, `100b`.
pub fn fmt_size(b: u64) -> String {
    if b >= GIB && (b as u128 * 100).is_multiple_of(GIB as u128) {
        return format!("{}g", trim_num(b as f64 / GIB as f64));
    }
    for (u, suf) in [(MIB, "m"), (KIB, "k")] {
        if b >= u && b.is_multiple_of(u) {
            return format!("{}{suf}", b / u);
        }
    }
    format!("{b}b")
}

/// Číslo bez zbytečných desetinných nul (`2`, `0.5`, `1.25`).
fn trim_num(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

/// Počet CPU jako `0.5`, `2`, `1,5`, `1.5 CPU`.
pub fn parse_cpus(s: &str) -> Result<f64, String> {
    let t = s.trim().to_ascii_lowercase().replace(',', ".");
    let t = t.trim_end_matches("cpus").trim_end_matches("cpu").trim();
    let v: f64 = t.parse().map_err(|_| tf!("Neplatný počet CPU: {}. Zadej třeba 0.5 nebo 2.", "Invalid CPU count: {}. Use e.g. 0.5 or 2.", s.trim()))?;
    if !v.is_finite() || v < 0.01 {
        return Err(t!("Počet CPU musí být aspoň 0.01.", "The CPU count must be at least 0.01.").into());
    }
    Ok((v * 100.0).round() / 100.0)
}

/// `0.5`, `2`, `1.25`.
pub fn fmt_cpus(c: f64) -> String {
    trim_num(c)
}

/// Změna jednoho limitu.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change<T> {
    Keep,
    Set(T),
    Remove,
}

impl<T> Change<T> {
    pub fn is_keep(&self) -> bool {
        matches!(self, Change::Keep)
    }
}

/// Požadované změny limitů (jen to, co se liší od současného stavu).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LimitUpdate {
    pub memory: Change<u64>,
    pub reservation: Change<u64>,
    pub swap: Change<u64>,
    pub cpus: Change<f64>,
    pub pids: Change<u64>,
}

impl LimitUpdate {
    pub fn is_empty(&self) -> bool {
        self.memory.is_keep() && self.reservation.is_keep() && self.swap.is_keep() && self.cpus.is_keep() && self.pids.is_keep()
    }
}

/// Textový formulář limitů (prázdné pole = bez limitu).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LimitForm {
    pub memory: String,
    pub reservation: String,
    pub swap: String,
    pub cpus: String,
    pub pids: String,
}

impl LimitForm {
    pub fn from_limits(l: &Limits) -> Self {
        LimitForm {
            memory: l.memory().map(fmt_size).unwrap_or_default(),
            reservation: l.reservation().map(fmt_size).unwrap_or_default(),
            swap: l.swap().map(fmt_size).unwrap_or_default(),
            cpus: l.cpus().map(fmt_cpus).unwrap_or_default(),
            pids: l.pids().map(|p| p.to_string()).unwrap_or_default(),
        }
    }

    /// Rozdíl proti současnému stavu (chyby u jednotlivých polí).
    pub fn to_update(&self, cur: &Limits) -> Result<LimitUpdate, Vec<String>> {
        let mut errs = Vec::new();
        let mut size = |s: &str, cur: Option<u64>, what: &str| -> Change<u64> {
            if s.trim().is_empty() {
                return if cur.is_some() { Change::Remove } else { Change::Keep };
            }
            match parse_size(s) {
                Ok(v) if v < MIN_MEM => {
                    errs.push(tf!("{what}: nejméně 6 MB.", "{what}: at least 6 MB.", ));
                    Change::Keep
                }
                Ok(v) if Some(v) == cur => Change::Keep,
                Ok(v) => Change::Set(v),
                Err(e) => {
                    errs.push(e);
                    Change::Keep
                }
            }
        };
        let memory = size(&self.memory, cur.memory(), t!("Limit paměti", "Memory limit"));
        let reservation = size(&self.reservation, cur.reservation(), t!("Rezervace", "Reservation"));
        let swap = size(&self.swap, cur.swap(), t!("Paměť + swap", "Memory + swap"));
        let cpus = if self.cpus.trim().is_empty() {
            if cur.cpus().is_some() { Change::Remove } else { Change::Keep }
        } else {
            match parse_cpus(&self.cpus) {
                Ok(v) if cur.cpus().is_some_and(|c| (c - v).abs() < 0.005) => Change::Keep,
                Ok(v) => Change::Set(v),
                Err(e) => {
                    errs.push(e);
                    Change::Keep
                }
            }
        };
        let pids = if self.pids.trim().is_empty() {
            if cur.pids().is_some() { Change::Remove } else { Change::Keep }
        } else {
            match self.pids.trim().parse::<u64>() {
                Ok(v) if v >= 1 && v <= i64::MAX as u64 => if Some(v) == cur.pids() { Change::Keep } else { Change::Set(v) },
                _ => {
                    errs.push(tf!("Neplatný limit procesů: {}", "Invalid process limit: {}", self.pids.trim()));
                    Change::Keep
                }
            }
        };
        if errs.is_empty() { Ok(LimitUpdate { memory, reservation, swap, cpus, pids }) } else { Err(errs) }
    }
}

fn after<T: Copy>(c: Change<T>, cur: Option<T>) -> Option<T> {
    match c {
        Change::Keep => cur,
        Change::Set(v) => Some(v),
        Change::Remove => None,
    }
}

/// Tělo `POST /containers/{id}/update`. `ncpu` = počet CPU enginu (Docker ho potřebuje
/// k „odebrání“ limitu `NanoCpus`; 0 = neznámý). Kontroluje i vazby mezi poli.
pub fn update_body(u: &LimitUpdate, cur: &Limits, engine: Engine, ncpu: i64) -> Result<Value, String> {
    if u.is_empty() {
        return Err(t!("Žádná změna.", "Nothing to change.").into());
    }
    let docker = engine == Engine::Docker;
    let mem = after(u.memory, cur.memory());
    let res = after(u.reservation, cur.reservation());
    if let (Some(r), Some(m)) = (res, mem) {
        if r > m {
            return Err(t!("Rezervace musí být menší než limit paměti.", "The reservation must be lower than the memory limit.").into());
        }
    }
    if u.reservation == Change::Remove {
        return Err(t!(
            "Rezervaci paměti nejde za běhu odebrat – nastav menší hodnotu (aspoň 6 MB).",
            "The memory reservation cannot be removed live – set a lower value (at least 6 MB)."
        )
        .into());
    }
    if docker && u.memory == Change::Remove {
        return Err(t!(
            "Docker neumí limit paměti za běhu odebrat – nastav vyšší hodnotu.",
            "Docker cannot remove a memory limit live – set a higher value instead."
        )
        .into());
    }
    let mut b = Map::new();
    // paměť + swap
    match u.memory {
        Change::Set(m) => {
            b.insert("Memory".into(), json!(m));
        }
        Change::Remove => {
            b.insert("Memory".into(), json!(-1));
        }
        Change::Keep => {}
    }
    match u.swap {
        Change::Set(s) => {
            let Some(m) = mem else {
                return Err(t!("Limit swapu vyžaduje limit paměti.", "A swap limit requires a memory limit.").into());
            };
            if s < m {
                return Err(t!(
                    "Paměť + swap musí být aspoň tolik jako limit paměti.",
                    "Memory + swap must be at least the memory limit."
                )
                .into());
            }
            b.insert("MemorySwap".into(), json!(s));
        }
        Change::Remove => {
            b.insert("MemorySwap".into(), json!(-1));
        }
        Change::Keep => match u.memory {
            // bez paměťového limitu nemá limit swapu smysl
            Change::Remove => {
                b.insert("MemorySwap".into(), json!(-1));
            }
            Change::Set(m) => {
                // výchozí swap Dockeru = 2× paměť; zachovat ho při změně paměti
                let old = cur.memory().unwrap_or(0);
                let swap = if cur.swap > 0 && cur.swap as u64 == old.saturating_mul(2) {
                    Some(m.saturating_mul(2) as i64)
                } else if cur.swap > 0 {
                    if (cur.swap as u64) < m {
                        return Err(tf!(
                            "Limit paměti je vyšší než současný limit paměť + swap ({}) – uprav i ten.",
                            "The memory limit exceeds the current memory + swap limit ({}) – change it too.",
                            fmt_size(cur.swap as u64)
                        ));
                    }
                    Some(cur.swap)
                } else if docker {
                    // Docker odmítne Memory > uložený MemorySwap (0) bez změny swapu
                    Some(if cur.swap < 0 { -1 } else { m.saturating_mul(2) as i64 })
                } else {
                    None
                };
                if let Some(s) = swap {
                    b.insert("MemorySwap".into(), json!(s));
                }
            }
            Change::Keep => {}
        },
    }
    if let Change::Set(r) = u.reservation {
        b.insert("MemoryReservation".into(), json!(r));
    }
    // CPU
    let quota_mode = !docker || (cur.nano_cpus <= 0 && cur.cpu_quota > 0);
    match u.cpus {
        Change::Set(c) if ncpu > 0 && c > ncpu as f64 + 0.005 => {
            return Err(tf!("Engine má jen {ncpu} CPU.", "The engine has only {ncpu} CPUs."));
        }
        Change::Set(c) if quota_mode => {
            let period = if docker && cur.cpu_period > 0 { cur.cpu_period } else { CPU_PERIOD };
            b.insert("CpuPeriod".into(), json!(period));
            b.insert("CpuQuota".into(), json!((c * period as f64).round() as i64));
        }
        Change::Set(c) => {
            b.insert("NanoCpus".into(), json!((c * 1e9).round() as i64));
        }
        Change::Remove if quota_mode => {
            b.insert("CpuQuota".into(), json!(-1));
        }
        Change::Remove => {
            // Docker NanoCpus nejde vynulovat (0 = neměnit) – nastavit na všechna jádra
            if ncpu <= 0 {
                return Err(t!("Neznámý počet CPU enginu.", "Unknown engine CPU count.").into());
            }
            b.insert("NanoCpus".into(), json!(ncpu * 1_000_000_000));
        }
        Change::Keep => {}
    }
    match u.pids {
        Change::Set(p) => {
            b.insert("PidsLimit".into(), json!(p));
        }
        Change::Remove => {
            b.insert("PidsLimit".into(), json!(-1));
        }
        Change::Keep => {}
    }
    Ok(Value::Object(b))
}

/// Pole, která se po aktualizaci neprojevila (engine změnu tiše ignoroval).
pub fn not_applied(u: &LimitUpdate, now: &Limits, ncpu: i64) -> Vec<&'static str> {
    let mut out = Vec::new();
    let check = |c: Change<u64>, v: Option<u64>| match c {
        Change::Keep => true,
        Change::Set(x) => v == Some(x),
        Change::Remove => v.is_none(),
    };
    if !check(u.memory, now.memory()) {
        out.push("Memory");
    }
    if !check(u.reservation, now.reservation()) {
        out.push("MemoryReservation");
    }
    if !check(u.swap, now.swap()) {
        out.push("MemorySwap");
    }
    let cpu_ok = match u.cpus {
        Change::Keep => true,
        Change::Set(x) => now.cpus().is_some_and(|c| (c - x).abs() < 0.005),
        Change::Remove => now.effective_cpus(ncpu).is_none(),
    };
    if !cpu_ok {
        out.push("CPU");
    }
    if !check(u.pids, now.pids()) {
        out.push("PidsLimit");
    }
    out
}

/// Využití vůči limitu v procentech (None = bez limitu).
pub fn pct_of(used: f64, limit: f64) -> Option<f64> {
    (limit > 0.0).then(|| used / limit * 100.0)
}

/// Hranice „u limitu“ (štítek v tabulce výkonu).
pub const NEAR_LIMIT_PCT: f64 = 90.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_both_ways() {
        assert_eq!(parse_size("512m").unwrap(), 512 * MIB);
        assert_eq!(parse_size("1g").unwrap(), GIB);
        assert_eq!(parse_size("1.5g").unwrap(), GIB + GIB / 2);
        assert_eq!(parse_size("1,5G").unwrap(), GIB + GIB / 2);
        assert_eq!(parse_size(" 256 MB ").unwrap(), 256 * MIB);
        assert_eq!(parse_size("2GiB").unwrap(), 2 * GIB);
        assert_eq!(parse_size("64k").unwrap(), 64 * KIB);
        assert_eq!(parse_size("1048576").unwrap(), MIB);
        for bad in ["", "abc", "1.5", "12x", "-1g", "1..2g", "g"] {
            assert!(parse_size(bad).is_err(), "{bad}");
        }
        assert_eq!(fmt_size(512 * MIB), "512m");
        assert_eq!(fmt_size(GIB), "1g");
        assert_eq!(fmt_size(GIB + GIB / 2), "1.5g");
        assert_eq!(fmt_size(1536 * KIB), "1536k");
        assert_eq!(fmt_size(100), "100b");
        for b in [6 * MIB, 256 * MIB, 3 * GIB + GIB / 4, 1536 * KIB, 12345, 7 * GIB] {
            assert_eq!(parse_size(&fmt_size(b)).unwrap(), b, "{}", fmt_size(b));
        }
    }

    #[test]
    fn cpus_both_ways() {
        assert_eq!(parse_cpus("0.5").unwrap(), 0.5);
        assert_eq!(parse_cpus("2").unwrap(), 2.0);
        assert_eq!(parse_cpus("1,5").unwrap(), 1.5);
        assert_eq!(parse_cpus("1.5 CPU").unwrap(), 1.5);
        assert_eq!(parse_cpus("0.333").unwrap(), 0.33);
        assert!(parse_cpus("0").is_err() && parse_cpus("-1").is_err() && parse_cpus("x").is_err() && parse_cpus("").is_err());
        assert_eq!(fmt_cpus(0.5), "0.5");
        assert_eq!(fmt_cpus(2.0), "2");
        assert_eq!(fmt_cpus(1.25), "1.25");
        for c in ["0.5", "1", "2", "1.25", "0.01"] {
            assert_eq!(fmt_cpus(parse_cpus(c).unwrap()), c);
        }
    }

    #[test]
    fn host_config_reading() {
        // Podman po odebrání limitů: -1 a záporné NanoCpus = bez limitu
        let h = json!({"Memory": -1, "MemorySwap": -1, "NanoCpus": -10000, "CpuQuota": -1, "CpuPeriod": 100000, "PidsLimit": -1, "CpusetCpus": ""});
        let l = Limits::from_host_config(&h);
        assert!(l.memory().is_none() && l.swap().is_none() && l.cpus().is_none() && l.pids().is_none());
        // Podman s kvótou
        let l = Limits::from_host_config(&json!({"Memory": 268435456, "NanoCpus": 500000000, "CpuQuota": 50000, "CpuPeriod": 100000, "PidsLimit": 2048}));
        assert_eq!(l.memory(), Some(256 * MIB));
        assert_eq!(l.cpus(), Some(0.5));
        assert_eq!(l.pids(), Some(2048));
        // Docker --cpu-quota bez NanoCpus
        let l = Limits::from_host_config(&json!({"CpuQuota": 150000, "CpuPeriod": 0}));
        assert_eq!(l.cpus(), Some(1.5));
        // Docker „odebraný“ limit = všechna jádra
        let l = Limits::from_host_config(&json!({"NanoCpus": 8_000_000_000i64}));
        assert_eq!(l.effective_cpus(8), None);
        assert_eq!(l.effective_cpus(16), Some(8.0));
        let f = LimitForm::from_limits(&Limits { memory: (GIB + GIB / 2) as i64, nano_cpus: 1_500_000_000, ..Default::default() });
        assert_eq!((f.memory.as_str(), f.cpus.as_str(), f.swap.as_str()), ("1.5g", "1.5", ""));
    }

    fn form(mem: &str, cpus: &str) -> LimitForm {
        LimitForm { memory: mem.into(), cpus: cpus.into(), ..Default::default() }
    }

    #[test]
    fn podman_body() {
        let none = Limits::default();
        let u = form("256m", "0.5").to_update(&none).unwrap();
        assert_eq!(u.memory, Change::Set(256 * MIB));
        let b = update_body(&u, &none, Engine::Podman, 8).unwrap();
        // NanoCpus Podman ignoruje – kvóta; swap se bez limitu neposílá
        assert_eq!(b, json!({"Memory": 268435456u64, "CpuPeriod": 100000, "CpuQuota": 50000}));
        // změna na 512m / 1 CPU
        let cur = Limits { memory: (256 * MIB) as i64, nano_cpus: 500_000_000, cpu_quota: 50_000, cpu_period: 100_000, ..Default::default() };
        let b = update_body(&form("512m", "1").to_update(&cur).unwrap(), &cur, Engine::Podman, 8).unwrap();
        assert_eq!(b, json!({"Memory": 536870912u64, "CpuPeriod": 100000, "CpuQuota": 100000}));
        // odebrání: -1 (0 by Podman ignoroval)
        let u = form("", "").to_update(&cur).unwrap();
        assert_eq!((u.memory, u.cpus), (Change::Remove, Change::Remove));
        let b = update_body(&u, &cur, Engine::Podman, 8).unwrap();
        assert_eq!(b, json!({"Memory": -1, "MemorySwap": -1, "CpuQuota": -1}));
        // pids
        let cur = Limits { pids: 2048, ..Default::default() };
        let f = LimitForm { pids: "100".into(), ..Default::default() };
        assert_eq!(update_body(&f.to_update(&cur).unwrap(), &cur, Engine::Podman, 8).unwrap(), json!({"PidsLimit": 100}));
        let f = LimitForm::default();
        assert_eq!(update_body(&f.to_update(&cur).unwrap(), &cur, Engine::Podman, 8).unwrap(), json!({"PidsLimit": -1}));
    }

    #[test]
    fn docker_body() {
        let none = Limits::default();
        let b = update_body(&form("256m", "0.5").to_update(&none).unwrap(), &none, Engine::Docker, 8).unwrap();
        // Docker: NanoCpus; swap musí jít s pamětí (uložený 0 by Memory odmítl) – výchozí 2×
        assert_eq!(b, json!({"Memory": 268435456u64, "MemorySwap": 536870912u64, "NanoCpus": 500000000i64}));
        // výchozí swap (2×) se při změně paměti posune
        let cur = Limits { memory: (256 * MIB) as i64, swap: (512 * MIB) as i64, nano_cpus: 500_000_000, ..Default::default() };
        let f = LimitForm { swap: "512m".into(), ..form("1g", "1") };
        let b = update_body(&f.to_update(&cur).unwrap(), &cur, Engine::Docker, 8).unwrap();
        assert_eq!(b, json!({"Memory": GIB, "MemorySwap": 2 * GIB, "NanoCpus": 1_000_000_000i64}));
        // swap bez omezení zůstane
        let cur2 = Limits { swap: -1, ..cur.clone() };
        let b = update_body(&form("1g", "0.5").to_update(&cur2).unwrap(), &cur2, Engine::Docker, 8).unwrap();
        assert_eq!(b, json!({"Memory": GIB, "MemorySwap": -1}));
        // limit kvótou (--cpu-quota) se mění kvótou, odebrání = -1
        let q = Limits { cpu_quota: 150_000, cpu_period: 50_000, ..Default::default() };
        let b = update_body(&form("", "1").to_update(&q).unwrap(), &q, Engine::Docker, 8).unwrap();
        assert_eq!(b, json!({"CpuPeriod": 50000, "CpuQuota": 50000}));
        assert_eq!(update_body(&form("", "").to_update(&q).unwrap(), &q, Engine::Docker, 8).unwrap(), json!({"CpuQuota": -1}));
        // NanoCpus nejde vynulovat -> všechna jádra
        let n = Limits { nano_cpus: 500_000_000, ..Default::default() };
        assert_eq!(update_body(&form("", "").to_update(&n).unwrap(), &n, Engine::Docker, 8).unwrap(), json!({"NanoCpus": 8_000_000_000i64}));
        assert!(update_body(&form("", "").to_update(&n).unwrap(), &n, Engine::Docker, 0).is_err());
        // limit paměti Docker za běhu neodebere
        let m = Limits { memory: (256 * MIB) as i64, ..Default::default() };
        assert!(update_body(&form("", "").to_update(&m).unwrap(), &m, Engine::Docker, 8).is_err());
        assert!(update_body(&form("", "").to_update(&m).unwrap(), &m, Engine::Podman, 8).is_ok());
    }

    #[test]
    fn validation_errors() {
        let none = Limits::default();
        // chyby polí
        assert!(form("1m", "").to_update(&none).is_err(), "pod 6 MB");
        assert!(form("abc", "").to_update(&none).is_err());
        assert!(form("", "0").to_update(&none).is_err());
        let e = form("x", "y").to_update(&none).unwrap_err();
        assert_eq!(e.len(), 2);
        assert!(LimitForm { pids: "0".into(), ..Default::default() }.to_update(&none).is_err());
        // beze změny
        let cur = Limits { memory: (512 * MIB) as i64, ..Default::default() };
        let u = form("512m", "").to_update(&cur).unwrap();
        assert!(u.is_empty());
        assert!(update_body(&u, &cur, Engine::Podman, 8).is_err());
        // víc CPU než engine
        assert!(update_body(&form("", "16").to_update(&none).unwrap(), &none, Engine::Podman, 8).is_err());
        // vazby: rezervace > limit, swap < paměť, swap bez paměti, odebrání rezervace
        let f = LimitForm { reservation: "1g".into(), ..form("512m", "") };
        assert!(update_body(&f.to_update(&none).unwrap(), &none, Engine::Podman, 8).is_err());
        let f = LimitForm { swap: "256m".into(), ..form("512m", "") };
        assert!(update_body(&f.to_update(&none).unwrap(), &none, Engine::Podman, 8).is_err());
        let f = LimitForm { swap: "1g".into(), ..Default::default() };
        assert!(update_body(&f.to_update(&none).unwrap(), &none, Engine::Podman, 8).is_err());
        let r = Limits { reservation: (128 * MIB) as i64, ..Default::default() };
        assert!(update_body(&LimitForm::default().to_update(&r).unwrap(), &r, Engine::Podman, 8).is_err());
        // zvýšení paměti nad vlastní (ne výchozí) swap
        let s = Limits { memory: (256 * MIB) as i64, swap: (300 * MIB) as i64, ..Default::default() };
        let f = LimitForm { swap: "300m".into(), ..form("512m", "") };
        assert!(update_body(&f.to_update(&s).unwrap(), &s, Engine::Docker, 8).is_err());
    }

    #[test]
    fn detects_ignored_fields() {
        let none = Limits::default();
        let u = form("256m", "0.5").to_update(&none).unwrap();
        // Podman ignoroval NanoCpus
        let now = Limits { memory: (256 * MIB) as i64, ..Default::default() };
        assert_eq!(not_applied(&u, &now, 8), vec!["CPU"]);
        let now = Limits { memory: (256 * MIB) as i64, nano_cpus: 500_000_000, ..Default::default() };
        assert!(not_applied(&u, &now, 8).is_empty());
    }
}
