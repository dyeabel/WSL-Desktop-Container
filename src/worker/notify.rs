//! Oznámení o problémech kontejnerů z proudu událostí: pád (nenulový kód),
//! nedostatek paměti, selhávající healthcheck a opakované restarty.
//! Zastavení, která vyvolal uživatel (aplikace, CLI, compose down), se neoznamují.

use super::Worker;
use crate::docker::Event;
use std::collections::HashMap;
use std::thread;
use std::time::{Duration, Instant};

/// Jak dlouho po stop/kill/remove se pád nepovažuje za pád.
const INTENDED: Duration = Duration::from_secs(30);
/// Kód 137/143 (SIGKILL/SIGTERM) chvíli podržet – Podman posílá `remove` až po `die`.
const SIGNAL_GRACE: Duration = Duration::from_millis(2500);
const LOOP_WINDOW: Duration = Duration::from_secs(60);
const LOOP_STARTS: usize = 3;
/// Stejný typ oznámení pro jeden kontejner nejvýš jednou za tuto dobu.
const REPEAT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    Crashed { name: String, code: i64 },
    Oom { name: String },
    Unhealthy { name: String },
    RestartLoop { name: String, starts: usize },
}

impl Notice {
    fn kind(&self) -> &'static str {
        match self {
            Notice::Crashed { .. } => "crash",
            Notice::Oom { .. } => "oom",
            Notice::Unhealthy { .. } => "health",
            Notice::RestartLoop { .. } => "loop",
        }
    }

    pub fn texts(&self) -> (String, String) {
        match self {
            Notice::Crashed { name, code } => (
                tf!("Kontejner {name} spadl", "Container {name} crashed"),
                tf!("Ukončil se s kódem {code}.", "Exited with code {code}."),
            ),
            Notice::Oom { name } => (
                tf!("Kontejner {name}: nedostatek paměti", "Container {name}: out of memory"),
                t!("Proces byl ukončen kvůli nedostatku paměti (OOM).", "The process was killed because it ran out of memory (OOM).").into(),
            ),
            Notice::Unhealthy { name } => (
                tf!("Kontejner {name} není v pořádku", "Container {name} is unhealthy"),
                t!("Kontrola stavu (healthcheck) selhává.", "The health check is failing.").into(),
            ),
            Notice::RestartLoop { name, starts } => (
                tf!("Kontejner {name} se opakovaně restartuje", "Container {name} keeps restarting"),
                tf!("{starts}× spuštěn během poslední minuty.", "Started {starts}× within the last minute."),
            ),
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Out {
    Notice(Notice),
    /// Podman neposílá výsledek healthchecku v události – je potřeba inspect
    CheckHealth { id: String, name: String },
    /// něco čeká na `tick` (podržený pád se signálem)
    TickLater,
    /// změnil se stav zdraví – obnovit seznam
    Refresh,
}

#[derive(Default)]
pub struct Notifier {
    quiet_until: Option<Instant>,
    intended: HashMap<String, Instant>,
    starts: HashMap<String, Vec<Instant>>,
    sent: HashMap<(String, &'static str), Instant>,
    health: HashMap<String, String>,
    pending: Vec<(Instant, String, Notice)>,
}

impl Notifier {
    pub fn quiet(&mut self, now: Instant, d: Duration) {
        self.quiet_until = Some(now + d);
    }

    pub fn mark_intended(&mut self, id: &str, now: Instant) {
        self.intended.insert(id.to_string(), now);
    }

    fn is_quiet(&self, now: Instant) -> bool {
        self.quiet_until.is_some_and(|q| now < q)
    }

    fn intended(&self, id: &str, now: Instant) -> bool {
        self.intended.get(id).is_some_and(|t| now.saturating_duration_since(*t) < INTENDED)
    }

    /// Vrátí oznámení, pokud se smí poslat (ticho, opakování), a zapamatuje si ho.
    fn emit(&mut self, id: &str, n: Notice, now: Instant) -> Option<Out> {
        if self.is_quiet(now) {
            return None;
        }
        let key = (id.to_string(), n.kind());
        if self.sent.get(&key).is_some_and(|t| now.saturating_duration_since(*t) < REPEAT) {
            return None;
        }
        self.sent.insert(key, now);
        Some(Out::Notice(n))
    }

    fn recently_sent(&self, id: &str, kind: &'static str, now: Instant, within: Duration) -> bool {
        self.sent.get(&(id.to_string(), kind)).is_some_and(|t| now.saturating_duration_since(*t) < within)
    }

    pub fn handle(&mut self, ev: &Event, now: Instant) -> Vec<Out> {
        if ev.kind != "container" || ev.actor.id.is_empty() {
            return vec![];
        }
        let id = ev.actor.id.clone();
        let name = ev.name();
        let mut out = vec![];
        match ev.action() {
            "stop" | "kill" => self.mark_intended(&id, now),
            "remove" | "destroy" => {
                self.mark_intended(&id, now);
                self.starts.remove(&id);
                self.health.remove(&id);
            }
            "oom" => out.extend(self.emit(&id, Notice::Oom { name }, now)),
            "start" => {
                let v = self.starts.entry(id.clone()).or_default();
                v.retain(|t| now.saturating_duration_since(*t) < LOOP_WINDOW);
                v.push(now);
                let n = v.len();
                if n >= LOOP_STARTS {
                    out.extend(self.emit(&id, Notice::RestartLoop { name, starts: n }, now));
                }
            }
            "die" => {
                let code = ev.exit_code().unwrap_or(0);
                let skip = code == 0
                    || self.intended(&id, now)
                    || self.recently_sent(&id, "oom", now, Duration::from_secs(10))
                    || self.recently_sent(&id, "loop", now, REPEAT);
                if !skip {
                    let n = Notice::Crashed { name, code };
                    if code == 137 || code == 143 {
                        self.pending.push((now + SIGNAL_GRACE, id, n));
                        out.push(Out::TickLater);
                    } else {
                        out.extend(self.emit(&id, n, now));
                    }
                }
            }
            "health_status" => match ev.action_arg() {
                Some(s) => out.extend(self.set_health(&id, &name, s, now)),
                None => out.push(Out::CheckHealth { id, name }),
            },
            _ => {}
        }
        out
    }

    /// Nový výsledek healthchecku; oznámí přechod do `unhealthy`.
    pub fn set_health(&mut self, id: &str, name: &str, status: &str, now: Instant) -> Vec<Out> {
        let prev = self.health.insert(id.to_string(), status.to_string());
        if prev.as_deref() == Some(status) {
            return vec![];
        }
        let mut out = vec![Out::Refresh];
        if status == "unhealthy" {
            out.extend(self.emit(id, Notice::Unhealthy { name: name.to_string() }, now));
        }
        out
    }

    /// Vyřídí podržené pády: pokud mezitím přišel stop/kill/remove, nic se neoznámí.
    pub fn tick(&mut self, now: Instant) -> Vec<Out> {
        let (due, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending).into_iter().partition(|(t, _, _)| *t <= now);
        self.pending = keep;
        let mut out = vec![];
        for (_, id, n) in due {
            if !self.intended(&id, now) {
                out.extend(self.emit(&id, n, now));
            }
        }
        out
    }
}

impl Worker {
    pub(super) fn notify_event(&self, ev: &Event) {
        let outs = match self.shared.notifier.lock() {
            Ok(mut n) => n.handle(ev, Instant::now()),
            Err(_) => return,
        };
        self.process(outs);
    }

    fn process(&self, outs: Vec<Out>) {
        for o in outs {
            match o {
                Out::Notice(n) => {
                    if self.cfg().notifications {
                        let (title, body) = n.texts();
                        thread::spawn(move || crate::notify::show(&title, &body));
                    }
                }
                Out::Refresh => self.schedule_refresh(),
                Out::TickLater => {
                    let w = self.clone();
                    thread::spawn(move || {
                        thread::sleep(SIGNAL_GRACE + Duration::from_millis(200));
                        let outs = w.shared.notifier.lock().map(|mut n| n.tick(Instant::now())).unwrap_or_default();
                        w.process(outs);
                    });
                }
                Out::CheckHealth { id, name } => {
                    let w = self.clone();
                    thread::spawn(move || {
                        let v: Option<serde_json::Value> = w.docker().get(&format!("/containers/{id}/json")).ok();
                        let status = v.as_ref().and_then(|v| {
                            v.pointer("/State/Health/Status").or_else(|| v.pointer("/State/Healthcheck/Status")).and_then(|s| s.as_str())
                        });
                        if let Some(s) = status {
                            let outs = w.shared.notifier.lock().map(|mut n| n.set_health(&id, &name, s, Instant::now())).unwrap_or_default();
                            w.process(outs);
                        }
                    });
                }
            }
        }
    }

    /// Aplikace sama zastavuje engine / distribuci – chvíli nic neoznamovat.
    pub(super) fn quiet_notifications(&self, d: Duration) {
        if let Ok(mut n) = self.shared.notifier.lock() {
            n.quiet(Instant::now(), d);
        }
    }

    /// Kontejner zastavuje / maže uživatel – jeho konec není pád.
    pub(super) fn mark_intended(&self, id: &str) {
        if let Ok(mut n) = self.shared.notifier.lock() {
            n.mark_intended(id, Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(action: &str, id: &str, attrs: &str) -> Event {
        serde_json::from_str(&format!(
            r#"{{"Type":"container","Action":"{action}","Actor":{{"ID":"{id}","Attributes":{{"name":"c-{id}"{attrs}}}}}}}"#
        ))
        .unwrap()
    }

    fn notices(o: Vec<Out>) -> Vec<Notice> {
        o.into_iter().filter_map(|o| if let Out::Notice(n) = o { Some(n) } else { None }).collect()
    }

    #[test]
    fn crash_is_notified_once() {
        let mut n = Notifier::default();
        let t = Instant::now();
        assert_eq!(notices(n.handle(&ev("die", "a", r#","exitCode":"3""#), t)), vec![Notice::Crashed { name: "c-a".into(), code: 3 }]);
        assert!(notices(n.handle(&ev("die", "a", r#","exitCode":"3""#), t + Duration::from_secs(5))).is_empty());
        assert!(n.handle(&ev("die", "b", r#","exitCode":"0""#), t).is_empty());
    }

    #[test]
    fn intended_stop_is_not_a_crash() {
        let mut n = Notifier::default();
        let t = Instant::now();
        n.handle(&ev("kill", "a", ""), t);
        assert!(n.handle(&ev("die", "a", r#","exitCode":"1""#), t).is_empty());
        // Podman rm -f: die 137, remove až potom
        assert_eq!(n.handle(&ev("die", "b", r#","exitCode":"137""#), t), vec![Out::TickLater]);
        n.handle(&ev("remove", "b", ""), t + Duration::from_millis(100));
        assert!(n.tick(t + Duration::from_secs(3)).is_empty());
        // 137 bez zásahu uživatele se oznámí
        n.handle(&ev("die", "c", r#","exitCode":"137""#), t);
        assert_eq!(notices(n.tick(t + Duration::from_secs(3))).len(), 1);
        // ticho po vypnutí enginu
        n.quiet(t, Duration::from_secs(60));
        assert!(notices(n.handle(&ev("die", "d", r#","exitCode":"2""#), t)).is_empty());
    }

    #[test]
    fn restart_loop_and_health() {
        let mut n = Notifier::default();
        let t = Instant::now();
        assert!(notices(n.handle(&ev("start", "a", ""), t)).is_empty());
        assert!(notices(n.handle(&ev("start", "a", ""), t + Duration::from_secs(10))).is_empty());
        assert_eq!(notices(n.handle(&ev("start", "a", ""), t + Duration::from_secs(20))).len(), 1);
        // Docker: výsledek v akci; opakovaný stav nic neoznamuje
        let o = n.handle(&ev("health_status: unhealthy", "h", ""), t);
        assert!(o.contains(&Out::Refresh));
        assert_eq!(notices(o).len(), 1);
        assert!(n.handle(&ev("health_status: unhealthy", "h", ""), t).is_empty());
        // Podman: bez výsledku -> inspect
        assert_eq!(n.handle(&ev("health_status", "p", ""), t), vec![Out::CheckHealth { id: "p".into(), name: "c-p".into() }]);
    }
}
