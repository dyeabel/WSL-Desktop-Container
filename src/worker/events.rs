//! Proud událostí enginu (`/events`): okamžité obnovení UI místo častého dotazování.
//! Když proud neběží (engine je vypnutý, spojení spadlo), stará se o obnovování
//! pravidelné dotazování v hlavní smyčce workeru.

use super::{Cmd, Worker};
use crate::docker::{Docker, Event};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

/// Jak dlouho počkat po události, než se obnoví – sloučí dávku událostí (např. compose up).
const DEBOUNCE: Duration = Duration::from_millis(300);

impl Worker {
    pub(super) fn spawn_events(&self) {
        let w = self.clone();
        thread::Builder::new()
            .name("events".into())
            .spawn(move || w.events_loop())
            .expect("events thread");
    }

    fn events_loop(self) {
        let mut wait = Duration::from_secs(2);
        loop {
            let port = self.cfg().port;
            let d = Docker::new(port);
            if !d.ping() {
                self.set_events_live(false);
                thread::sleep(Duration::from_secs(3));
                continue;
            }
            let r = d.stream_events(
                || {
                    self.set_events_live(true);
                    // mezitím se mohlo cokoli změnit
                    self.schedule_refresh();
                },
                |ev| {
                    self.on_event(&ev);
                    // změna portu v nastavení -> připojit se znovu
                    self.cfg().port == port
                },
            );
            let was_live = self.shared.events_live.load(Ordering::SeqCst);
            self.set_events_live(false);
            match r {
                Ok(()) => wait = Duration::from_secs(1),
                Err(_) => wait = (wait * 2).min(Duration::from_secs(30)),
            }
            if was_live {
                // spojení skončilo (engine se zastavil?) – ať UI hned ukáže skutečný stav
                self.schedule_refresh();
            }
            thread::sleep(wait);
        }
    }

    fn set_events_live(&self, on: bool) {
        self.shared.events_live.store(on, Ordering::SeqCst);
    }

    fn on_event(&self, ev: &Event) {
        if ev.triggers_refresh() {
            self.schedule_refresh();
        }
        self.notify_event(ev);
    }

    /// Obnovení se zpožděním; víc událostí během DEBOUNCE vede jen k jednomu obnovení.
    pub(super) fn schedule_refresh(&self) {
        if self.shared.refresh_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let w = self.clone();
        thread::spawn(move || {
            thread::sleep(DEBOUNCE);
            w.shared.refresh_pending.store(false, Ordering::SeqCst);
            let _ = w.cmd.send(Cmd::Refresh);
        });
    }
}
