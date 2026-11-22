//! Detail kontejneru: inspect, vzorkování statistik a „Znovu vytvořit se změnami“.

use super::{Msg, RunSpec, Worker};
use crate::docker::{enc, Docker};
use crate::inspect::{self, NetEdit};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Jak dlouhou historii statistik držíme (s).
pub const STATS_WINDOW: f64 = 300.0;
/// Interval vzorkování statistik.
const STATS_EVERY: Duration = Duration::from_secs(2);

/// Interval vzorkování při skrytém okně (bez překreslení, graf bez mezer).
const HIDDEN_EVERY: Duration = Duration::from_secs(10);

/// Interval vzorkování podle viditelnosti okna.
fn stats_interval(visible: bool) -> Duration {
    if visible { STATS_EVERY } else { HIDDEN_EVERY }
}

/// Jeden vzorek statistik kontejneru.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatSample {
    /// sekundy od spuštění vzorkování
    pub t: f64,
    /// CPU v % jednoho jádra (None = první vzorek, není z čeho počítat rozdíl)
    pub cpu: Option<f64>,
    pub mem: u64,
    pub mem_limit: u64,
}

/// Běžící vzorkování statistik; zastaví se při zahození.
pub struct StatsFeed {
    pub samples: Arc<Mutex<VecDeque<StatSample>>>,
    pub started: Instant,
    stop: Arc<AtomicBool>,
}

impl Drop for StatsFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Spustí vzorkování statistik kontejneru (~každé 2 s, historie 5 min) ve vlastním vlákně.
/// Běží jen dokud existuje vrácený `StatsFeed` (tj. dokud je otevřený detail).
pub fn start_stats(ctx: eframe::egui::Context, ep: crate::http::Endpoint, id: String) -> StatsFeed {
    let samples = Arc::new(Mutex::new(VecDeque::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let (buf, st) = (samples.clone(), stop.clone());
    thread::spawn(move || {
        let d = Docker::new(&ep);
        let mut prev: Option<(u64, Instant)> = None;
        while !st.load(Ordering::SeqCst) {
            let t0 = Instant::now();
            match d.stats(&id, "") {
                Some(row) => {
                    let now = Instant::now();
                    // CPU z rozdílu kumulovaného času mezi vzorky (funguje pro Docker i Podman)
                    let cpu = prev.and_then(|(tot, at)| {
                        let wall = now.duration_since(at).as_nanos() as f64;
                        (row.cpu_total >= tot && wall > 0.0).then(|| (row.cpu_total - tot) as f64 / wall * 100.0)
                    });
                    prev = Some((row.cpu_total, now));
                    if let Ok(mut g) = buf.lock() {
                        let t = now.duration_since(started).as_secs_f64();
                        g.push_back(StatSample { t, cpu, mem: row.mem, mem_limit: row.mem_limit });
                        while g.front().is_some_and(|s| s.t < t - STATS_WINDOW) {
                            g.pop_front();
                        }
                    }
                    // skryté okno nepřekreslovat
                    if crate::tray::window_visible() {
                        ctx.request_repaint();
                    }
                }
                // kontejner neběží – zkusíme to znovu (mohl by se spustit)
                None => prev = None,
            }
            while t0.elapsed() < stats_interval(crate::tray::window_visible()) && !st.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(100));
            }
        }
    });
    StatsFeed { samples, started, stop }
}

/// Existuje kontejner s přesně tímto názvem? (Chyba spojení se vrací jako Err.)
fn find_by_name(d: &Docker, name: &str) -> Result<Option<String>, String> {
    match d.get::<Value>(&format!("/containers/{}/json", enc(name))) {
        // dotaz podle názvu může trefit i prefix ID jiného kontejneru – ověřit název
        Ok(v) if v.get("Name").and_then(|n| n.as_str()).map(|n| n.trim_start_matches('/')) == Some(name) => {
            Ok(v.get("Id").and_then(|i| i.as_str()).map(String::from))
        }
        Ok(_) => Ok(None),
        Err(e) if e.is_not_found() => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn rename(d: &Docker, id: &str, name: &str) -> Result<String, String> {
    Ok(d.post(&format!("/containers/{id}/rename?name={}", enc(name)))?)
}

/// Pro živé testy: znovuvytvoření stejnou cestou jako z UI (bez běžícího workeru).
#[cfg(test)]
pub fn recreate_for_test(cfg: crate::config::Config, id: &str, spec: RunSpec, net: NetEdit) -> Result<String, String> {
    let (msg, _msg_rx) = std::sync::mpsc::channel();
    let (cmd, _cmd_rx) = std::sync::mpsc::channel();
    let w = Worker {
        ctx: eframe::egui::Context::default(),
        cfg: Arc::new(std::sync::RwLock::new(cfg)),
        msg,
        cmd,
        shared: Arc::new(super::Shared::default()),
    };
    w.recreate(id.to_string(), spec, net)
}

impl Worker {
    /// Načte inspect kontejneru (a výchozí hodnoty jeho image) a pošle `Msg::Inspect`.
    pub(super) fn inspect_detail(&self, id: String) -> Result<String, String> {
        let d = self.docker();
        let r = d.get::<Value>(&format!("/containers/{}/json", enc(&id))).map(|v| {
            let mut det = inspect::parse(&v);
            if let Ok(img) = d.get::<Value>(&format!("/images/{}/json", enc(&det.image_id))) {
                inspect::set_image_defaults(&mut det, &img);
            }
            Box::new(det)
        });
        self.send(Msg::Inspect(id, r.map_err(String::from)));
        Ok(String::new())
    }

    /// Vytvoří a spustí kontejner přes API z původního inspectu se změnami z formuláře
    /// (zachová bezpečnostní nastavení, limity, labely, entrypoint…).
    fn create_from(&self, d: &Docker, orig: &Value, spec: &RunSpec, net: &NetEdit, name: &str) -> Result<String, String> {
        use super::recreate_body::{create_body, extra_networks, flatten};
        let cfg = self.cfg();
        let image = if cfg.engine == crate::engine::Engine::Podman {
            crate::engine::qualify_image(&spec.image)
        } else {
            spec.image.trim().to_string()
        };
        let body = flatten(create_body(orig, spec, &image, net)?);
        let url = format!("/containers/create?name={}", enc(name));
        let resp = match d.post_json(&url, body.clone()) {
            Ok(r) => r,
            // image (třeba nový tag) ještě není stažený
            Err(e) if e.is_not_found() || e.to_string().contains("No such image") || e.to_string().contains("image not known") => {
                d.pull(&spec.image)?;
                d.post_json(&url, body)?
            }
            Err(e) => return Err(e.into()),
        };
        let nid = serde_json::from_str::<Value>(&resp)
            .ok()
            .and_then(|v| v.get("Id").and_then(|i| i.as_str()).map(String::from))
            .ok_or_else(|| format!("create: {resp}"))?;
        // další sítě připojit ještě před startem (aliasy, pevné IP a MAC)
        for (n, ep) in extra_networks(orig, spec, net) {
            d.post_json(&format!("/networks/{}/connect", enc(&n)), json!({"Container": nid, "EndpointConfig": ep}))
                .map_err(|e| tf!("Připojení k síti {n} selhalo: {e}", "Connecting to network {n} failed: {e}"))?;
        }
        d.container_action(&nid, "start")?;
        // chvíli počkat – skončil hned po startu?
        thread::sleep(Duration::from_millis(1800));
        let info: Option<Value> = d.get(&format!("/containers/{nid}/json")).ok();
        let running = info.as_ref().and_then(|v| v.pointer("/State/Running").and_then(|r| r.as_bool())).unwrap_or(true);
        if running {
            Ok(tf!("Spuštěn kontejner {name} z {}", "Started container {name} from {}", spec.image.trim()))
        } else {
            let code = info.as_ref().and_then(|v| v.pointer("/State/ExitCode").and_then(|c| c.as_i64())).unwrap_or(0);
            Err(tf!("Nový kontejner {name} se hned ukončil (kód {code}).", "The new container {name} exited immediately (code {code})."))
        }
    }

    /// Znovu vytvoří kontejner s novými parametry. Starý se zastaví a přejmenuje na
    /// `<název>-wslcd-old`; když nový naběhne, starý se smaže, jinak se nový odstraní
    /// a starý se vrátí (přejmenuje zpět a případně znovu spustí).
    pub(super) fn recreate(&self, id: String, mut spec: RunSpec, net: NetEdit) -> Result<String, String> {
        let d = self.docker();
        let v: Value = d.get(&format!("/containers/{}/json", enc(&id)))?;
        let old = inspect::parse(&v);
        let (id, old_name) = (old.id.clone(), old.name.clone());
        spec.interactive = false;
        if spec.name.trim().is_empty() {
            spec.name = old_name.clone();
        }
        let new_name = spec.name.trim().to_string();
        let backup = format!("{old_name}-wslcd-old");
        if find_by_name(&d, &backup)?.is_some() {
            return Err(tf!(
                "Kontejner {backup} už existuje (zbytek po předchozím pokusu?) – odstraň ho nebo přejmenuj.",
                "Container {backup} already exists (left over from a previous attempt?) – remove or rename it."
            ));
        }
        if new_name != old_name && find_by_name(&d, &new_name)?.is_some() {
            return Err(tf!("Název {new_name} už používá jiný kontejner.", "The name {new_name} is already used by another container."));
        }
        self.send(Msg::Toast(true, tf!("Znovu vytvářím {old_name}…", "Recreating {old_name}…")));
        let was_running = old.running || old.paused;
        if old.paused {
            d.container_action(&id, "unpause")?;
        }
        if was_running {
            d.container_action(&id, "stop")?;
        }
        let restore = |why: String| -> String {
            let mut notes = Vec::new();
            if let Err(e) = rename(&d, &id, &old_name) {
                notes.push(tf!("přejmenování zpět selhalo: {e}", "renaming back failed: {e}"));
            }
            if was_running {
                if let Err(e) = d.container_action(&id, "start") {
                    notes.push(tf!("spuštění selhalo: {e}", "start failed: {e}"));
                }
            }
            if notes.is_empty() {
                tf!(
                    "Znovuvytvoření {old_name} selhalo: {why}\nPůvodní kontejner byl obnoven.",
                    "Recreating {old_name} failed: {why}\nThe original container was restored."
                )
            } else {
                tf!(
                    "Znovuvytvoření {old_name} selhalo: {why}\nObnova původního kontejneru ({backup}): {}",
                    "Recreating {old_name} failed: {why}\nRestoring the original container ({backup}): {}",
                    notes.join("; ")
                )
            }
        };
        if let Err(e) = rename(&d, &id, &backup) {
            return Err(restore(e));
        }
        match self.create_from(&d, &v, &spec, &net, &new_name) {
            // bez `v=1` – anonymní volume (teď připojená k novému kontejneru) zůstanou
            Ok(msg) => match d.container_action(&id, "remove") {
                Ok(_) => Ok(tf!("Kontejner {old_name} znovu vytvořen. {msg}", "Container {old_name} recreated. {msg}")),
                Err(e) => Err(tf!(
                    "{msg}, ale původní kontejner {backup} nejde odstranit: {e}",
                    "{msg}, but the original container {backup} cannot be removed: {e}"
                )),
            },
            Err(e) => {
                // napůl vytvořený nový kontejner (název jsme předem ověřili, že je volný)
                if let Ok(Some(nid)) = find_by_name(&d, &new_name) {
                    if nid != id {
                        let _ = d.container_action(&nid, "remove");
                    }
                }
                // nový kontejner už neexistuje – rada „viz log kontejneru“ by mátla
                let e = e
                    .replace(" Výstup najdeš v logu kontejneru.", " Nový kontejner byl odstraněn.")
                    .replace(" See the container log for output.", " The new container was removed.");
                Err(restore(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_samples_slower_visible_fast() {
        assert_eq!(stats_interval(true), Duration::from_secs(2));
        assert_eq!(stats_interval(false), Duration::from_secs(10));
        assert!(stats_interval(false) > stats_interval(true));
    }
}
