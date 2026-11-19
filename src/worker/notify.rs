//! Problémy kontejnerů z proudu událostí: pád (nenulový kód), nedostatek paměti,
//! selhávající healthcheck a opakované restarty. Každý problém se zapíše do historie
//! (`eventlog`, stránka Události) s detailem (inspect + konec logu); ve Windows se oznámí
//! jen ty, které nejsou běžnou situací a projdou nastavením (typ, ztlumení, interval).
//!
//! Zastavení, která vyvolal uživatel (aplikace, CLI, compose down), se nezapisují vůbec.
//! Ukončení signálem SIGTERM/SIGINT/SIGHUP, dočasné kontejnery (`--rm`) a Testcontainers se
//! zapíšou, ale neoznamují. Oznámení, která přijdou těsně po sobě (vypnutí WSL, compose),
//! se sloučí do jednoho.

use super::Worker;
use crate::config::ContainerNotify;
use crate::docker::{Docker, Event, LogOpts};
use crate::eventlog::{self, Detail, Kind, Quiet, Record};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

/// Jak dlouho po stop/kill se konec kontejneru nepovažuje za pád.
const INTENDED: Duration = Duration::from_secs(30);
/// Pád chvíli podržet: přijde-li mezitím stop/kill/remove, nejde o pád (Podman posílá
/// `remove` až po `die`), a smazání hned po skončení prozradí dočasný kontejner (`--rm`).
const HOLD: Duration = Duration::from_millis(2500);
const LOOP_WINDOW: Duration = Duration::from_secs(60);
const LOOP_STARTS: usize = 3;
/// Oznámení během tohoto okna se sloučí do jednoho.
const COALESCE: Duration = Duration::from_millis(2000);
/// Štítek kontejnerů Testcontainers.
const TC_LABEL: &str = "org.testcontainers";

/// Nastavení pro rozhodování (kopie z konfigurace).
#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// oznámení ve Windows zapnutá
    pub enabled: bool,
    pub cn: ContainerNotify,
}

impl Policy {
    fn from_cfg(c: &crate::config::Config) -> Self {
        Self { enabled: c.notifications, cn: c.container_notify.clone() }
    }
}

#[derive(Debug, PartialEq)]
pub enum Out {
    /// zapsat do historie; `quiet: None` = oznámit ve Windows
    Record(Record),
    /// kontejner skončil chybou – načíst detail hned, než ho engine případně smaže
    Prefetch(String),
    /// Podman neposílá výsledek healthchecku v události – je potřeba inspect
    CheckHealth { id: String, name: String },
    /// něco čeká na `tick` (podržený pád)
    TickLater,
    /// změnil se stav zdraví – obnovit seznam
    Refresh,
}

struct Pending {
    due: Instant,
    rec: Record,
}

#[derive(Default)]
pub struct Notifier {
    quiet_until: Option<Instant>,
    intended: HashMap<String, Instant>,
    removed: HashMap<String, Instant>,
    starts: HashMap<String, Vec<Instant>>,
    /// naposledy oznámeno ve Windows
    sent: HashMap<(String, Kind), Instant>,
    /// naposledy zaznamenáno (i bez oznámení)
    seen: HashMap<(String, Kind), Instant>,
    health: HashMap<String, String>,
    /// kontejnery se štítkem Testcontainers
    testcontainers: HashSet<String>,
    pending: Vec<Pending>,
}

fn within(m: &HashMap<(String, Kind), Instant>, id: &str, k: Kind, now: Instant, d: Duration) -> bool {
    m.get(&(id.to_string(), k)).is_some_and(|t| now.saturating_duration_since(*t) < d)
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

    fn since(m: &HashMap<String, Instant>, id: &str, now: Instant) -> bool {
        m.get(id).is_some_and(|t| now.saturating_duration_since(*t) < INTENDED)
    }

    /// Doplní důvod, proč neoznamovat (nastavení, opakování), a zapamatuje si záznam.
    fn finish(&mut self, mut r: Record, now: Instant, p: &Policy) -> Option<Out> {
        if self.is_quiet(now) {
            return None;
        }
        if r.quiet.is_none() && !p.cn.ephemeral && self.testcontainers.contains(&r.id) {
            r.quiet = Some(Quiet::Testcontainers);
        }
        let repeat = Duration::from_secs(p.cn.repeat_secs() as u64);
        if r.quiet.is_none() {
            r.quiet = if !p.enabled {
                Some(Quiet::Disabled)
            } else if !r.kind.enabled(&p.cn) {
                Some(Quiet::KindOff)
            } else if p.cn.is_muted(&r.name) {
                Some(Quiet::Muted)
            } else if within(&self.sent, &r.id, r.kind, now, repeat) {
                Some(Quiet::Repeat)
            } else {
                None
            };
        }
        let key = (r.id.clone(), r.kind);
        if r.quiet.is_none() {
            self.sent.insert(key.clone(), now);
        }
        self.seen.insert(key, now);
        Some(Out::Record(r))
    }

    pub fn handle(&mut self, ev: &Event, now: Instant, p: &Policy) -> Vec<Out> {
        if ev.kind != "container" || ev.actor.id.is_empty() {
            return vec![];
        }
        let id = ev.actor.id.clone();
        let name = ev.name();
        if ev.attr(TC_LABEL).is_some() {
            self.testcontainers.insert(id.clone());
        }
        let rec = |kind, code, starts| Record { id: id.clone(), name: name.clone(), kind, code, starts, quiet: None };
        let mut out = vec![];
        match ev.action() {
            "stop" | "kill" => self.mark_intended(&id, now),
            "remove" | "destroy" => {
                self.removed.insert(id.clone(), now);
                self.starts.remove(&id);
                self.health.remove(&id);
            }
            "oom" => out.extend(self.finish(rec(Kind::Oom, None, None), now, p)),
            "start" => {
                let v = self.starts.entry(id.clone()).or_default();
                v.retain(|t| now.saturating_duration_since(*t) < LOOP_WINDOW);
                v.push(now);
                let n = v.len();
                if n >= LOOP_STARTS {
                    out.extend(self.finish(rec(Kind::Loop, None, Some(n)), now, p));
                }
            }
            "die" => {
                let code = ev.exit_code().unwrap_or(0);
                let skip = code == 0
                    || self.is_quiet(now)
                    || Self::since(&self.intended, &id, now)
                    || within(&self.seen, &id, Kind::Oom, now, Duration::from_secs(10));
                if !skip {
                    let mut r = rec(Kind::Crash, Some(code), None);
                    if eventlog::is_signal_exit(code) && !p.cn.signal_exits {
                        r.quiet = Some(Quiet::Signal);
                    }
                    self.pending.push(Pending { due: now + HOLD, rec: r });
                    out.push(Out::Prefetch(id.clone()));
                    out.push(Out::TickLater);
                }
            }
            "health_status" => match ev.action_arg() {
                Some(s) => out.extend(self.set_health(&id, &name, s, now, p)),
                None => out.push(Out::CheckHealth { id, name }),
            },
            _ => {}
        }
        out
    }

    /// Nový výsledek healthchecku; zaznamená přechod do `unhealthy`.
    pub fn set_health(&mut self, id: &str, name: &str, status: &str, now: Instant, p: &Policy) -> Vec<Out> {
        let prev = self.health.insert(id.to_string(), status.to_string());
        if prev.as_deref() == Some(status) {
            return vec![];
        }
        let mut out = vec![Out::Refresh];
        if status == "unhealthy" {
            let r = Record { id: id.into(), name: name.into(), kind: Kind::Health, code: None, starts: None, quiet: None };
            out.extend(self.finish(r, now, p));
        }
        out
    }

    /// Vyřídí podržené pády: stop/kill mezitím = nic; smazání hned po skončení = dočasný
    /// kontejner (kód 137/143 se smazáním je `rm -f` – nic); běžící restartovací smyčka =
    /// započítat do ní.
    pub fn tick(&mut self, now: Instant, p: &Policy) -> Vec<Out> {
        let (due, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.pending).into_iter().partition(|x| x.due <= now);
        self.pending = keep;
        let mut out = vec![];
        for Pending { mut rec, .. } in due {
            let id = rec.id.clone();
            if Self::since(&self.intended, &id, now) {
                continue;
            }
            if Self::since(&self.removed, &id, now) {
                if matches!(rec.code, Some(137 | 143)) {
                    continue;
                }
                if rec.quiet.is_none() && !p.cn.ephemeral {
                    rec.quiet = Some(Quiet::Ephemeral);
                }
            }
            let repeat = Duration::from_secs(p.cn.repeat_secs() as u64);
            if within(&self.seen, &id, Kind::Loop, now, repeat) {
                rec.kind = Kind::Loop;
            }
            out.extend(self.finish(rec, now, p));
        }
        out
    }
}

// ---------------------------------------------------------------- doručení

/// Požadavek na přechod na stránku Události po kliknutí na oznámení (seq záznamu, je-li jeden).
static NAV: Mutex<Option<Option<u64>>> = Mutex::new(None);

pub fn take_nav() -> Option<Option<u64>> {
    NAV.lock().ok().and_then(|mut g| g.take())
}

/// Oznámení čekající na sloučení.
static QUEUE: Mutex<Vec<(u64, Record)>> = Mutex::new(Vec::new());

fn texts(r: &Record) -> (String, String) {
    let name = &r.name;
    match r.kind {
        Kind::Crash => {
            let code = r.code.unwrap_or(0);
            let (m, hint) = (eventlog::exit_meaning(code), eventlog::exit_hint(code));
            let body = tf!("Kód {code}: {m}.\n{hint}", "Code {code}: {m}.\n{hint}");
            (tf!("Kontejner {name} spadl", "Container {name} crashed"), body)
        }
        Kind::Oom => (
            tf!("Kontejner {name}: nedostatek paměti", "Container {name}: out of memory"),
            t!("Proces byl ukončen kvůli nedostatku paměti (OOM).", "The process was killed because it ran out of memory (OOM).").into(),
        ),
        Kind::Health => (
            tf!("Kontejner {name} není v pořádku", "Container {name} is unhealthy"),
            t!("Kontrola stavu (healthcheck) selhává.", "The health check is failing.").into(),
        ),
        Kind::Loop => (
            tf!("Kontejner {name} se opakovaně restartuje", "Container {name} keeps restarting"),
            match r.starts {
                Some(n) => tf!("{n}× spuštěn během poslední minuty.", "Started {n}× within the last minute."),
                None => t!("Opakovaně se spouští a padá.", "It keeps starting and crashing.").into(),
            },
        ),
    }
}

/// Text sloučeného oznámení pro víc problémů najednou.
fn merged_texts(items: &[(u64, Record)]) -> (String, String) {
    let n = items.len();
    let names: Vec<String> = items.iter().take(4).map(|(_, r)| format!("{} ({})", r.name, r.kind.label().to_lowercase())).collect();
    let mut body = names.join(", ");
    if n > 4 {
        body.push_str(&tf!(" a {} další", " and {} more", n - 4));
    }
    (tf!("Problémy u {n} kontejnerů", "Problems with {n} containers"), body)
}

/// Načte stav kontejneru a konec jeho logu.
fn fetch_detail(d: &Docker, id: &str) -> Detail {
    let v: Option<serde_json::Value> = d.get(&format!("/containers/{id}/json")).ok();
    let Some(v) = v else {
        return Detail { gone: true, ..Default::default() };
    };
    let s = |p: &str| v.pointer(p).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let label = |k: &str| v.pointer("/Config/Labels").and_then(|l| l.get(k)).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let health = ["/State/Health/Log", "/State/Healthcheck/Log"]
        .iter()
        .find_map(|p| v.pointer(p).and_then(|l| l.as_array()).and_then(|a| a.last()).cloned())
        .map(|l| {
            let code = l.get("ExitCode").and_then(|c| c.as_i64()).unwrap_or(0);
            let out = l.get("Output").and_then(|o| o.as_str()).unwrap_or("").trim().to_string();
            tf!("kód {code}: {out}", "code {code}: {out}")
        })
        .unwrap_or_default();
    let mut logs = String::new();
    let opts = LogOpts { tail: eventlog::LOG_LINES, follow: false, timestamps: true, closer: Default::default() };
    let _ = d.stream_logs(id, &opts, |chunk, _| {
        logs.push_str(chunk);
        logs.len() < 4 * eventlog::LOG_BYTES
    });
    Detail {
        image: s("/Config/Image"),
        project: label("com.docker.compose.project"),
        service: label("com.docker.compose.service"),
        started: s("/State/StartedAt"),
        finished: s("/State/FinishedAt"),
        restart_policy: s("/HostConfig/RestartPolicy/Name"),
        restart_count: v.pointer("/RestartCount").and_then(|x| x.as_i64()).unwrap_or(0),
        error: s("/State/Error"),
        oom_killed: v.pointer("/State/OOMKilled").and_then(|x| x.as_bool()).unwrap_or(false),
        health,
        logs: eventlog::tail_text(&logs),
        gone: false,
    }
}

impl Worker {
    pub(super) fn notify_event(&self, ev: &Event) {
        let p = Policy::from_cfg(&self.cfg());
        let outs = match self.shared.notifier.lock() {
            Ok(mut n) => n.handle(ev, Instant::now(), &p),
            Err(_) => return,
        };
        self.process(outs);
    }

    fn spawn_detail(&self, id: String) {
        let w = self.clone();
        thread::spawn(move || {
            let d = fetch_detail(&w.docker(), &id);
            if let Ok(mut log) = w.shared.event_log.lock() {
                log.detail_ready(&id, d);
            }
            eventlog::save_soon(&w.shared.event_log);
            w.ctx.request_repaint();
        });
    }

    fn record(&self, r: Record) {
        let cn = self.cfg().container_notify;
        let toast = r.quiet.is_none();
        let id = r.id.clone();
        let kind = r.kind;
        let res = self.shared.event_log.lock().map(|mut l| {
            let seq = l.add(r.clone(), toast, eventlog::now_unix(), cn.repeat_secs());
            (seq, seq.is_some() && l.awaits(&id))
        });
        let Ok((seq, need_detail)) = res else { return };
        // u pádu se detail načítá už při `die` (Prefetch); u ostatních až teď
        if need_detail && kind != Kind::Crash {
            self.spawn_detail(id);
        }
        eventlog::save_soon(&self.shared.event_log);
        self.ctx.request_repaint();
        if toast {
            self.queue_toast(seq.unwrap_or(0), r);
        }
    }

    fn queue_toast(&self, seq: u64, r: Record) {
        let first = match QUEUE.lock() {
            Ok(mut q) => {
                q.push((seq, r));
                q.len() == 1
            }
            Err(_) => return,
        };
        if !first {
            return;
        }
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            thread::sleep(COALESCE);
            let items = QUEUE.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
            let (title, body, target) = match items.as_slice() {
                [] => return,
                [(seq, r)] => {
                    let (t, b) = texts(r);
                    (t, b, (*seq > 0).then_some(*seq))
                }
                many => {
                    let (t, b) = merged_texts(many);
                    (t, b, None)
                }
            };
            crate::notify::show_with(&title, &body, move || {
                if let Ok(mut g) = NAV.lock() {
                    *g = Some(target);
                }
                ctx.request_repaint();
            });
        });
    }

    fn process(&self, outs: Vec<Out>) {
        for o in outs {
            match o {
                Out::Record(r) => self.record(r),
                Out::Prefetch(id) => self.spawn_detail(id),
                Out::Refresh => self.schedule_refresh(),
                Out::TickLater => {
                    let w = self.clone();
                    thread::spawn(move || {
                        thread::sleep(HOLD + Duration::from_millis(200));
                        let p = Policy::from_cfg(&w.cfg());
                        let outs = w.shared.notifier.lock().map(|mut n| n.tick(Instant::now(), &p)).unwrap_or_default();
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
                            let p = Policy::from_cfg(&w.cfg());
                            let outs = w.shared.notifier.lock().map(|mut n| n.set_health(&id, &name, s, Instant::now(), &p)).unwrap_or_default();
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

    fn pol() -> Policy {
        Policy { enabled: true, cn: ContainerNotify::default() }
    }

    fn records(o: Vec<Out>) -> Vec<Record> {
        o.into_iter().filter_map(|o| if let Out::Record(r) = o { Some(r) } else { None }).collect()
    }

    /// die + uplynutí podržení
    fn die(n: &mut Notifier, id: &str, code: i64, t: Instant) -> Vec<Record> {
        let p = pol();
        n.handle(&ev("die", id, &format!(r#","exitCode":"{code}""#)), t, &p);
        records(n.tick(t + HOLD, &p))
    }

    #[test]
    fn crash_is_notified_then_repeat_is_quiet() {
        let mut n = Notifier::default();
        let t = Instant::now();
        let p = pol();
        let o = n.handle(&ev("die", "a", r#","exitCode":"3""#), t, &p);
        assert_eq!(o, vec![Out::Prefetch("a".into()), Out::TickLater]);
        let r = records(n.tick(t + HOLD, &p));
        assert_eq!(r.len(), 1);
        assert_eq!((r[0].kind, r[0].code, r[0].quiet), (Kind::Crash, Some(3), None));
        let r = die(&mut n, "a", 3, t + Duration::from_secs(10));
        assert_eq!(r[0].quiet, Some(Quiet::Repeat));
        // kód 0 se nezapisuje
        assert!(n.handle(&ev("die", "b", r#","exitCode":"0""#), t, &p).is_empty());
    }

    #[test]
    fn benign_exits_are_logged_quietly() {
        let mut n = Notifier::default();
        let t = Instant::now();
        let p = pol();
        // SIGTERM bez zásahu uživatele (např. vypnutí systému v kontejneru)
        assert_eq!(die(&mut n, "a", 143, t)[0].quiet, Some(Quiet::Signal));
        // --rm: smazán hned po pádu
        n.handle(&ev("die", "b", r#","exitCode":"1""#), t, &p);
        n.handle(&ev("remove", "b", ""), t + Duration::from_millis(200), &p);
        assert_eq!(records(n.tick(t + HOLD, &p))[0].quiet, Some(Quiet::Ephemeral));
        // Testcontainers podle štítku
        n.handle(&ev("create", "c", r#","org.testcontainers":"true""#), t, &p);
        assert_eq!(die(&mut n, "c", 1, t)[0].quiet, Some(Quiet::Testcontainers));
        // ztlumený kontejner a vypnutý typ
        let mut p2 = pol();
        p2.cn.muted = vec!["c-d".into()];
        n.handle(&ev("die", "d", r#","exitCode":"1""#), t, &p2);
        assert_eq!(records(n.tick(t + HOLD, &p2))[0].quiet, Some(Quiet::Muted));
        p2.cn.crash = false;
        n.handle(&ev("die", "e", r#","exitCode":"1""#), t, &p2);
        assert_eq!(records(n.tick(t + HOLD, &p2))[0].quiet, Some(Quiet::KindOff));
    }

    #[test]
    fn intended_stop_is_not_recorded() {
        let mut n = Notifier::default();
        let t = Instant::now();
        let p = pol();
        n.handle(&ev("kill", "a", ""), t, &p);
        assert!(n.handle(&ev("die", "a", r#","exitCode":"1""#), t, &p).is_empty());
        // Podman rm -f: die 137, remove až potom
        n.handle(&ev("die", "b", r#","exitCode":"137""#), t, &p);
        n.handle(&ev("remove", "b", ""), t + Duration::from_millis(100), &p);
        assert!(n.tick(t + HOLD, &p).is_empty());
        // 137 bez zásahu uživatele se oznámí
        assert_eq!(die(&mut n, "c", 137, t)[0].quiet, None);
        // ticho po vypnutí enginu
        n.quiet(t, Duration::from_secs(60));
        assert!(records(n.handle(&ev("die", "d", r#","exitCode":"2""#), t, &p)).is_empty());
        assert!(n.tick(t + HOLD, &p).is_empty());
    }

    #[test]
    fn restart_loop_absorbs_crashes_and_health() {
        let mut n = Notifier::default();
        let t = Instant::now();
        let p = pol();
        assert!(records(n.handle(&ev("start", "a", ""), t, &p)).is_empty());
        assert!(records(n.handle(&ev("start", "a", ""), t + Duration::from_secs(10), &p)).is_empty());
        let r = records(n.handle(&ev("start", "a", ""), t + Duration::from_secs(20), &p));
        assert_eq!((r[0].kind, r[0].starts), (Kind::Loop, Some(3)));
        // další pád se započítá do smyčky (bez nového oznámení)
        let r = die(&mut n, "a", 1, t + Duration::from_secs(25));
        assert_eq!((r[0].kind, r[0].quiet), (Kind::Loop, Some(Quiet::Repeat)));
        // Docker: výsledek v akci; opakovaný stav nic nezapisuje
        let o = n.handle(&ev("health_status: unhealthy", "h", ""), t, &p);
        assert!(o.contains(&Out::Refresh));
        assert_eq!(records(o).len(), 1);
        assert!(n.handle(&ev("health_status: unhealthy", "h", ""), t, &p).is_empty());
        // Podman: bez výsledku -> inspect
        assert_eq!(n.handle(&ev("health_status", "p", ""), t, &p), vec![Out::CheckHealth { id: "p".into(), name: "c-p".into() }]);
    }

    #[test]
    fn disabled_notifications_still_record() {
        let mut n = Notifier::default();
        let t = Instant::now();
        let p = Policy { enabled: false, cn: ContainerNotify::default() };
        n.handle(&ev("die", "a", r#","exitCode":"1""#), t, &p);
        assert_eq!(records(n.tick(t + HOLD, &p))[0].quiet, Some(Quiet::Disabled));
    }
}
