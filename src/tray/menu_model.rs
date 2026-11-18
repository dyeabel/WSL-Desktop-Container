//! Čistý model podnabídky „Kontejnery“ v trayi (bez objektů muda, aby šel testovat).
//!
//! Worker po každém obnovení zapíše do `Shared::tray_containers` seznam [`Item`];
//! tray z něj na hlavním vlákně postaví [`Model`] a nabídku přestaví jen tehdy,
//! když se změní [`Model::signature`] (sada kontejnerů nebo jejich stavy).

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Nejvýše tolik kontejnerů se v nabídce vypíše; zbytek shrne položka „… a N dalších“.
pub const CAP: usize = 25;

/// Prefix id položek podnabídky kontejnerů.
const PREFIX: &str = "c:";
/// Id položky „… a N dalších“ (otevře okno na stránce Kontejnery).
pub const MORE_ID: &str = "c:more";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum State {
    Running,
    Paused,
    Stopped,
}

impl State {
    pub fn from_docker(state: &str) -> State {
        match state {
            "running" => State::Running,
            "paused" => State::Paused,
            _ => State::Stopped,
        }
    }

    /// Krátká značka stavu (obyčejné Unicode znaky, které Windows nabídky vykreslí).
    pub fn marker(self) -> &'static str {
        match self {
            State::Running => "●",
            State::Paused => "‖",
            State::Stopped => "○",
        }
    }
}

/// Kontejner, jak ho vidí tray.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub state: State,
}

impl Item {
    pub fn from_container(c: &crate::docker::Container) -> Item {
        Item { id: c.id.clone(), name: c.name(), state: State::from_docker(&c.state) }
    }

    /// Text položky v nabídce: značka stavu a název (`&` by Windows bralo jako akcelerátor).
    pub fn label(&self) -> String {
        format!("{}  {}", self.state.marker(), self.name.replace('&', "&&"))
    }

    /// Akce v podnabídce kontejneru podle jeho stavu.
    pub fn actions(&self) -> Vec<Action> {
        match self.state {
            State::Running => vec![Action::Stop, Action::Restart, Action::Pause, Action::Logs, Action::Console, Action::Show],
            State::Paused => vec![Action::Stop, Action::Restart, Action::Resume, Action::Logs, Action::Show],
            State::Stopped => vec![Action::Start, Action::Restart, Action::Logs, Action::Show],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
    Restart,
    Pause,
    Resume,
    Logs,
    Console,
    Show,
}

impl Action {
    const ALL: [Action; 8] =
        [Action::Start, Action::Stop, Action::Restart, Action::Pause, Action::Resume, Action::Logs, Action::Console, Action::Show];

    fn key(self) -> &'static str {
        match self {
            Action::Start => "start",
            Action::Stop => "stop",
            Action::Restart => "restart",
            Action::Pause => "pause",
            Action::Resume => "unpause",
            Action::Logs => "logs",
            Action::Console => "console",
            Action::Show => "show",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Start => t!("Spustit", "Start"),
            Action::Stop => t!("Zastavit", "Stop"),
            Action::Restart => t!("Restartovat", "Restart"),
            Action::Pause => t!("Pozastavit", "Pause"),
            Action::Resume => t!("Obnovit", "Resume"),
            Action::Logs => t!("Logy", "Logs"),
            Action::Console => t!("Konzole", "Console"),
            Action::Show => t!("Zobrazit v aplikaci", "Show in app"),
        }
    }

    /// Akce pro `Cmd::Container` (ostatní se řeší jinak).
    pub fn container_action(self) -> Option<&'static str> {
        match self {
            Action::Start | Action::Stop | Action::Restart | Action::Pause | Action::Resume => Some(self.key()),
            Action::Logs | Action::Console | Action::Show => None,
        }
    }
}

/// Id položky nabídky: `c:<akce>:<id kontejneru>`.
pub fn encode(action: Action, id: &str) -> String {
    format!("{PREFIX}{}:{id}", action.key())
}

/// Opak [`encode`]; `None` pro cizí nebo poškozená id.
pub fn decode(menu_id: &str) -> Option<(Action, &str)> {
    let rest = menu_id.strip_prefix(PREFIX)?;
    let (key, id) = rest.split_once(':')?;
    if id.is_empty() {
        return None;
    }
    Action::ALL.iter().find(|a| a.key() == key).map(|a| (*a, id))
}

/// Co se má v podnabídce zobrazit.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Model {
    pub connected: bool,
    /// vypsané kontejnery (nejvýše [`CAP`]), běžící první, pak podle názvu
    pub entries: Vec<Item>,
    /// kolik kontejnerů se nevešlo
    pub more: usize,
    /// počet běžících (ze všech, nejen vypsaných)
    pub running: usize,
}

impl Model {
    pub fn build(connected: bool, items: &[Item]) -> Model {
        if !connected {
            return Model { connected, entries: Vec::new(), more: 0, running: 0 };
        }
        let mut entries = items.to_vec();
        entries.sort_by(|a, b| {
            a.state
                .cmp(&b.state)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.id.cmp(&b.id))
        });
        let running = entries.iter().filter(|i| i.state == State::Running).count();
        let more = entries.len().saturating_sub(CAP);
        entries.truncate(CAP);
        Model { connected, entries, more, running }
    }

    /// Otisk modelu – nabídka se přestavuje jen při jeho změně.
    pub fn signature(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }

    pub fn title(&self) -> String {
        if self.connected {
            tf!("Kontejnery ({} běží)", "Containers ({} running)", self.running)
        } else {
            t!("Kontejnery", "Containers").to_string()
        }
    }

    pub fn more_label(&self) -> String {
        tf!("… a {} dalších", "… and {} more", self.more)
    }

    /// Text zakázané položky, když se nemá nic vypsat.
    pub fn placeholder(&self) -> Option<&'static str> {
        if !self.connected {
            Some(t!("Engine neběží", "Engine is not running"))
        } else if self.entries.is_empty() {
            Some(t!("Žádné kontejnery", "No containers"))
        } else {
            None
        }
    }

    pub fn name_of(&self, id: &str) -> Option<&str> {
        self.entries.iter().find(|i| i.id == id).map(|i| i.name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, name: &str, state: State) -> Item {
        Item { id: id.into(), name: name.into(), state }
    }

    #[test]
    fn sorts_running_first_then_by_name() {
        let items = vec![
            item("1", "zeta", State::Stopped),
            item("2", "beta", State::Running),
            item("3", "Alpha", State::Stopped),
            item("4", "gamma", State::Paused),
            item("5", "alef", State::Running),
        ];
        let m = Model::build(true, &items);
        let names: Vec<_> = m.entries.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["alef", "beta", "gamma", "Alpha", "zeta"]);
        assert_eq!(m.running, 2);
        assert_eq!(m.more, 0);
        assert_eq!(m.placeholder(), None);
    }

    #[test]
    fn caps_list_and_counts_rest() {
        let items: Vec<_> = (0..40)
            .map(|i| item(&format!("id{i}"), &format!("c{i:02}"), if i >= 30 { State::Running } else { State::Stopped }))
            .collect();
        let m = Model::build(true, &items);
        assert_eq!(m.entries.len(), CAP);
        assert_eq!(m.more, 15);
        // všech 10 běžících je nahoře a počítají se i ty mimo výpis
        assert_eq!(m.running, 10);
        assert!(m.entries[..10].iter().all(|i| i.state == State::Running));
        assert_eq!(m.entries[10].name, "c00");

        let exact: Vec<_> = (0..CAP).map(|i| item(&i.to_string(), &i.to_string(), State::Stopped)).collect();
        assert_eq!(Model::build(true, &exact).more, 0);
    }

    #[test]
    fn disconnected_is_empty_with_placeholder() {
        let m = Model::build(false, &[item("1", "a", State::Running)]);
        assert!(m.entries.is_empty());
        assert!(m.placeholder().is_some());
        assert!(Model::build(true, &[]).placeholder().is_some());
    }

    #[test]
    fn signature_changes_only_with_set_or_state() {
        let a = vec![item("1", "web", State::Running), item("2", "db", State::Stopped)];
        let base = Model::build(true, &a).signature();
        // jiné pořadí na vstupu = stejný model
        let mut b = a.clone();
        b.reverse();
        assert_eq!(Model::build(true, &b).signature(), base);
        // změna stavu
        let mut c = a.clone();
        c[1].state = State::Running;
        assert_ne!(Model::build(true, &c).signature(), base);
        // přibyl kontejner
        let mut d = a.clone();
        d.push(item("3", "cache", State::Stopped));
        assert_ne!(Model::build(true, &d).signature(), base);
        // přejmenování
        let mut e = a.clone();
        e[0].name = "web2".into();
        assert_ne!(Model::build(true, &e).signature(), base);
        // odpojení
        assert_ne!(Model::build(false, &a).signature(), base);
    }

    #[test]
    fn id_roundtrip() {
        let id = "3f1c0a9b8e7d";
        for a in Action::ALL {
            let s = encode(a, id);
            assert!(s.starts_with("c:"));
            assert_eq!(decode(&s), Some((a, id)));
        }
        assert_eq!(encode(Action::Stop, "abc"), "c:stop:abc");
        assert_eq!(decode("c:stop:"), None);
        assert_eq!(decode("c:nonsense:abc"), None);
        assert_eq!(decode(MORE_ID), None);
        assert_eq!(decode("stop"), None);
        assert_eq!(decode("quit"), None);
    }

    #[test]
    fn actions_depend_on_state() {
        let r = item("1", "a", State::Running).actions();
        assert!(r.contains(&Action::Stop) && r.contains(&Action::Pause) && r.contains(&Action::Console));
        assert!(!r.contains(&Action::Start) && !r.contains(&Action::Resume));
        let p = item("1", "a", State::Paused).actions();
        assert!(p.contains(&Action::Resume) && !p.contains(&Action::Pause) && !p.contains(&Action::Console));
        let s = item("1", "a", State::Stopped).actions();
        assert!(s.contains(&Action::Start) && !s.contains(&Action::Stop) && !s.contains(&Action::Console));
        for v in [r, p, s] {
            assert!(v.contains(&Action::Restart) && v.contains(&Action::Logs) && v.contains(&Action::Show));
        }
        assert_eq!(Action::Resume.container_action(), Some("unpause"));
        assert_eq!(Action::Logs.container_action(), None);
    }

    #[test]
    fn label_has_marker_and_escapes_ampersand() {
        assert_eq!(item("1", "a&b", State::Running).label(), "●  a&&b");
        assert!(item("1", "x", State::Stopped).label().starts_with('○'));
        assert!(item("1", "x", State::Paused).label().starts_with('‖'));
    }
}
