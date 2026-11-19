//! Živá změna limitů kontejneru (`POST /containers/{id}/update`, bez restartu) a mezipaměť
//! limitů běžících kontejnerů pro tabulku výkonu.

use super::Worker;
use crate::docker::{enc, Docker, StatRow};
use crate::engine::Engine;
use crate::limits::{self, LimitUpdate, Limits};
use serde_json::Value;
use std::time::{Duration, Instant};

/// Jak dlouho platí limity v mezipaměti (změny mimo aplikaci se projeví nejpozději za tuto dobu).
const CACHE_TTL: Duration = Duration::from_secs(60);

fn read_limits(d: &Docker, id: &str) -> Result<Limits, String> {
    let v: Value = d.get(&format!("/containers/{}/json", enc(id)))?;
    Ok(v.get("HostConfig").map(Limits::from_host_config).unwrap_or_default())
}

/// Nastaví limity kontejneru a ověří je novým inspectem (Podman některá pole tiše ignoruje).
/// Stejnou cestu používá UI (přes worker) i živý test.
pub fn apply(d: &Docker, id: &str, u: &LimitUpdate, engine: Engine) -> Result<Limits, String> {
    let cur = read_limits(d, id)?;
    let ncpu = if engine == Engine::Docker && u.cpus == limits::Change::Remove {
        d.get::<crate::docker::Info>("/info").map(|i| i.ncpu).unwrap_or(0)
    } else {
        0
    };
    let body = limits::update_body(u, &cur, engine, ncpu)?;
    d.post_json(&format!("/containers/{}/update", enc(id)), body)
        .map_err(|e| tf!("Změna limitů selhala: {e}", "Changing the limits failed: {e}"))?;
    let now = read_limits(d, id)?;
    let bad = limits::not_applied(u, &now, ncpu);
    if !bad.is_empty() {
        return Err(tf!(
            "Engine změnu nepřijal: {}",
            "The engine did not apply the change: {}",
            bad.join(", ")
        ));
    }
    Ok(now)
}

impl Worker {
    /// `Cmd::UpdateLimits` – změní limity a znovu načte detail.
    pub(super) fn update_limits(&self, id: String, name: String, u: LimitUpdate) -> Result<String, String> {
        let d = self.docker();
        let engine = self.cfg().engine;
        let r = apply(&d, &id, &u, engine);
        if let Ok(mut c) = self.shared.limits.lock() {
            c.remove(&id);
        }
        let _ = self.inspect_detail(id);
        r.map(|_| tf!("Limity kontejneru {name} změněny.", "Limits of container {name} changed."))
    }

    /// Doplní do řádků statistik limity paměti a CPU (z inspectu, s mezipamětí).
    pub(super) fn fill_limits(&self, d: &Docker, rows: &mut [StatRow]) {
        let Ok(mut cache) = self.shared.limits.lock() else { return };
        cache.retain(|id, _| rows.iter().any(|r| &r.id == id));
        for r in rows.iter_mut() {
            let fresh = cache.get(&r.id).is_some_and(|(t, _)| t.elapsed() < CACHE_TTL);
            if !fresh {
                if let Ok(l) = read_limits(d, &r.id) {
                    cache.insert(r.id.clone(), (Instant::now(), l));
                }
            }
            if let Some((_, l)) = cache.get(&r.id) {
                r.lim_mem = l.memory().unwrap_or(0);
                r.lim_cpus = l.cpus().unwrap_or(0.0);
            }
        }
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// `cargo test live_container_limits -- --ignored --nocapture`
    /// Pracuje jen s vlastním kontejnerem `wls-test-limits`.
    #[test]
    #[ignore]
    fn live_container_limits() {
        use crate::limits::LimitForm;
        let cfg = crate::config::Config::load();
        crate::docker::configure_tls(cfg.tls).expect("TLS");
        let d = Docker::new(cfg.port);
        assert!(d.ping(), "API na {} neodpovídá", cfg.port);
        let ver: Value = d.get("/version").unwrap();
        let engine = if ver.to_string().contains("Podman") { Engine::Podman } else { Engine::Docker };
        let name = "wls-test-limits";
        let _ = d.container_action(name, "remove");
        let body = serde_json::json!({"Image": "docker.io/library/alpine:3.20", "Cmd": ["sleep", "600"]});
        let url = format!("/containers/create?name={name}");
        if d.post_json(&url, body.clone()).is_err() {
            d.pull("docker.io/library/alpine:3.20").expect("pull");
            d.post_json(&url, body).expect("create");
        }
        d.container_action(name, "start").expect("start");
        let exec = |cmd: &str| -> String {
            let out = crate::container_fs::exec(&d, name, &["sh", "-c", cmd]).expect("exec");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let cgroup = || exec("cat /sys/fs/cgroup/memory.max /sys/fs/cgroup/cpu.max").replace('\n', " | ");
        let set = |mem: &str, cpus: &str| -> Limits {
            let cur = read_limits(&d, name).unwrap();
            let form = LimitForm { memory: mem.into(), cpus: cpus.into(), ..LimitForm::from_limits(&cur) };
            let u = form.to_update(&cur).expect("formulář");
            let r = apply(&d, name, &u, engine);
            eprintln!("{engine:?} memory={mem:?} cpus={cpus:?} -> {r:?}");
            let l = r.expect("apply");
            eprintln!("  cgroup: {}", cgroup());
            l
        };
        eprintln!("start: {:?}\n  cgroup: {}", read_limits(&d, name).unwrap(), cgroup());

        let l = set("256m", "0.5");
        assert_eq!((l.memory(), l.cpus()), (Some(256 << 20), Some(0.5)));
        assert_eq!(cgroup(), "268435456 | 50000 100000");

        let l = set("512m", "1");
        assert_eq!((l.memory(), l.cpus()), (Some(512 << 20), Some(1.0)));
        assert_eq!(cgroup(), "536870912 | 100000 100000");

        // znovuvytvoření limity zachová (nové ID, stejný název)
        let v: Value = d.get(&format!("/containers/{name}/json")).unwrap();
        let det = crate::inspect::parse(&v);
        let spec = det.to_run_spec();
        let r = crate::worker::recreate_for_test(cfg.clone(), &det.id, spec, det.net_edit(engine == Engine::Podman));
        eprintln!("recreate: {r:?}");
        assert!(r.is_ok(), "{r:?}");
        let l = read_limits(&d, name).unwrap();
        eprintln!("  po znovuvytvoření: {l:?}\n  cgroup: {}", cgroup());
        assert_eq!((l.memory(), l.cpus()), (Some(512 << 20), Some(1.0)));
        assert_eq!(cgroup(), "536870912 | 100000 100000");

        if engine == Engine::Podman {
            let l = set("", "");
            assert!(l.memory().is_none() && l.cpus().is_none() && l.swap().is_none(), "{l:?}");
            assert_eq!(cgroup(), "max | max 100000");
        } else {
            // Docker limit paměti za běhu neodebere – jen CPU
            let l = set("512m", "");
            assert!(l.effective_cpus(d.get::<crate::docker::Info>("/info").unwrap().ncpu).is_none(), "{l:?}");
        }
        // kontejner běží dál (žádný restart)
        assert!(crate::container_fs::is_running(&d, name).unwrap());
        d.container_action(name, "remove").expect("remove");
    }
}
