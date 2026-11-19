//! Stránka Události: historie problémů kontejnerů (pády, OOM, healthcheck, restarty)
//! s detailem (kód ukončení a jeho význam, image, compose, doba běhu, konec logu) a sekce
//! „Oznámení o kontejnerech“ v Nastavení.

use super::*;
use crate::app::{App, Page};
use crate::config::ContainerNotify;
use crate::eventlog::{self, Entry, Kind};

/// ikony (Phosphor): BELL_RINGING, BELL_SLASH, COPY, TRASH, FILE_TEXT, ARROW_SQUARE_OUT
const BELL: &str = "\u{E5E8}";
const BELL_OFF: &str = "\u{E0D4}";
const COPY: &str = "\u{E1CA}";
const TRASH: &str = "\u{E4A6}";
const LOGS: &str = "\u{E23A}";
const OPEN: &str = "\u{E5DE}";

#[derive(Default)]
pub struct EventsUi {
    /// rozbalený záznam
    pub selected: Option<u64>,
    filter: String,
    kind: Option<Kind>,
    /// skrýt běžné situace (signál, --rm, Testcontainers)
    hide_benign: bool,
    only_toast: bool,
    confirm_clear: bool,
}

/// Kliknutí na oznámení: stránka Události, případně s rozbaleným záznamem.
pub fn navigate(app: &mut App, seq: Option<u64>) {
    app.page = Page::Events;
    if seq.is_some() {
        app.events.selected = seq;
        app.events.kind = None;
        app.events.filter.clear();
        app.events.hide_benign = false;
        app.events.only_toast = false;
    }
}

/// Počet nepřečtených problémů (odznak v postranním panelu).
pub fn unread(app: &App) -> usize {
    app.shared.event_log.lock().map(|l| l.unread).unwrap_or(0)
}

fn kind_color(k: Kind) -> egui::Color32 {
    match k {
        Kind::Crash | Kind::Oom => theme::red(),
        Kind::Health | Kind::Loop => theme::yellow(),
    }
}

/// Unixový čas jako místní `d. m. rrrr hh:mm:ss`.
pub fn fmt_local(ts: i64) -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
        let ft100 = (ts.max(0) as u64 + 11_644_473_600) * 10_000_000;
        let ft = FILETIME { dwLowDateTime: ft100 as u32, dwHighDateTime: (ft100 >> 32) as u32 };
        let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
        let mut loc: SYSTEMTIME = unsafe { std::mem::zeroed() };
        let ok = unsafe { FileTimeToSystemTime(&ft, &mut utc) != 0 && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut loc) != 0 };
        if ok {
            return format!("{}. {}. {} {:02}:{:02}:{:02}", loc.wDay, loc.wMonth, loc.wYear, loc.wHour, loc.wMinute, loc.wSecond);
        }
    }
    format!("{ts}")
}

fn fmt_dur(s: i64) -> String {
    match s {
        i64::MIN..=0 => "0 s".into(),
        1..=119 => format!("{s} s"),
        120..=7199 => format!("{} min", s / 60),
        7200..=172_799 => format!("{} h {} min", s / 3600, s % 3600 / 60),
        _ => tf!("{} dní", "{} days", s / 86_400),
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let (entries, total) = match app.shared.event_log.lock() {
        Ok(mut l) => {
            l.unread = 0;
            (l.entries.clone(), l.entries.len())
        }
        Err(_) => (vec![], 0),
    };
    header(
        ui,
        t!("Události kontejnerů", "Container events"),
        &tf!(
            "{total} záznamů · pády, nedostatek paměti, healthcheck a restarty za posledních 30 dní",
            "{total} records · crashes, out of memory, health checks and restarts over the last 30 days"
        ),
    );

    let mut clear = false;
    let mut to_settings = false;
    toolbar_lr(ui, 240.0, |ui, right| {
        if !right {
            let cur = app.events.kind.map(|k| k.label()).unwrap_or(t!("Všechny typy", "All types"));
            egui::ComboBox::from_id_salt("ev-kind").selected_text(cur).show_ui(ui, |ui| {
                ui.selectable_value(&mut app.events.kind, None, t!("Všechny typy", "All types"));
                for k in Kind::ALL {
                    ui.selectable_value(&mut app.events.kind, Some(k), k.label());
                }
            });
            gap(ui);
            ui.checkbox(&mut app.events.hide_benign, t!("Skrýt běžné situace", "Hide routine exits"))
                .on_hover_text(t!(
                    "Ukončení signálem SIGTERM/SIGINT, dočasné kontejnery (--rm) a Testcontainers",
                    "Exits by SIGTERM/SIGINT, temporary containers (--rm) and Testcontainers"
                ));
            ui.checkbox(&mut app.events.only_toast, t!("Jen oznámené", "Notified only"));
        } else {
            more_menu(ui, |ui| {
                if ui.button(t!("\u{E270} Nastavení oznámení…", "\u{E270} Notification settings…")).clicked() {
                    to_settings = true;
                    ui.close_menu();
                }
                if ui.button(tf!("{TRASH} Smazat historii…", "{TRASH} Clear history…")).clicked() {
                    clear = true;
                    ui.close_menu();
                }
            });
            filter_field(ui, &mut app.events.filter);
        }
    });
    if clear {
        app.events.confirm_clear = true;
    }
    if to_settings {
        app.page = Page::Settings;
    }

    let f = app.events.filter.to_lowercase();
    let list: Vec<&Entry> = entries
        .iter()
        .rev()
        .filter(|e| app.events.kind.is_none_or(|k| e.kind == k))
        .filter(|e| !app.events.hide_benign || !e.benign())
        .filter(|e| !app.events.only_toast || e.toast)
        .filter(|e| {
            f.is_empty()
                || e.name.to_lowercase().contains(&f)
                || e.detail.as_ref().is_some_and(|d| d.image.to_lowercase().contains(&f) || d.project.to_lowercase().contains(&f))
        })
        .collect();

    if list.is_empty() {
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(muted(if total == 0 {
                t!(
                    "Zatím žádné problémy. Sem se zapíše každý pád kontejneru, nedostatek paměti, selhávající healthcheck i opakované restarty – i ty, které se ve Windows neoznámily.",
                    "No problems yet. Every container crash, out-of-memory kill, failing health check and restart loop is recorded here – including those not shown as a Windows notification."
                )
            } else {
                t!("Filtru nic neodpovídá.", "Nothing matches the filter.")
            }));
        });
    }

    let mut action: Option<Action> = None;
    for e in list {
        entry_row(app, ui, e, &mut action);
        ui.add_space(6.0);
    }
    if let Some(a) = action {
        apply(app, ui.ctx(), a);
    }
    clear_dialog(app, ui.ctx());
}

enum Action {
    Toggle(u64),
    Detail(String, String),
    Logs(String, String),
    Mute(String, bool),
    Remove(u64),
}

fn entry_row(app: &App, ui: &mut egui::Ui, e: &Entry, action: &mut Option<Action>) {
    let open = app.events.selected == Some(e.seq);
    let exists = app.snap.containers.iter().any(|c| c.id == e.id);
    let muted_now = app.cfg.read().map(|c| c.container_notify.is_muted(&e.name)).unwrap_or(false);
    let fill = if open { theme::row_hover() } else { theme::card() };
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0_f32, theme::card_stroke()))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let head = ui.horizontal(|ui| {
                // popisky nesmí brát myš (označování textu) – celé záhlaví je jedno tlačítko
                ui.style_mut().interaction.selectable_labels = false;
                ui.label(muted(if open { "\u{E136}" } else { "\u{E13A}" }));
                ui.label(muted(fmt_ago(e.last.max(e.ts))));
                let c = if e.benign() { theme::grey() } else { kind_color(e.kind) };
                badge(ui, e.kind.label(), c);
                ui.label(RichText::new(&e.name).strong());
                ui.label(muted(e.summary()));
                if e.count > 1 {
                    badge(ui, &format!("{}×", e.count), theme::grey());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if e.toast {
                        ui.label(RichText::new(BELL).color(theme::accent()));
                    } else {
                        ui.label(muted(BELL_OFF));
                    }
                });
            });
            // kliknutí do záhlaví řádku rozbalí / sbalí detail
            let when = if e.count > 1 {
                tf!("{}× · poprvé {} · naposledy {}", "{}× · first {} · last {}", e.count, fmt_local(e.ts), fmt_local(e.last))
            } else {
                fmt_local(e.ts)
            };
            let notified = if e.toast {
                t!("Oznámeno ve Windows", "Shown as a Windows notification").to_string()
            } else {
                let why = e.quiet.map(|q| q.label()).unwrap_or("");
                tf!("Neoznámeno: {why}", "Not notified: {why}")
            };
            // celý pruh záhlaví včetně okrajů karty (inner_margin 12 × 8)
            let band = head.response.rect.expand2(egui::vec2(12.0, 8.0));
            let resp = ui.interact(band, ui.id().with(("ev-head", e.seq)), egui::Sense::click()).on_hover_text(format!("{when}\n{notified}"));
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                *action = Some(Action::Toggle(e.seq));
            }
            if !open {
                return;
            }
            ui.add_space(6.0);
            ui.separator();
            detail(ui, e);
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(exists, |ui| {
                    if ui.button(tf!("{OPEN} Detail kontejneru", "{OPEN} Container details")).clicked() {
                        *action = Some(Action::Detail(e.id.clone(), e.name.clone()));
                    }
                    if ui.button(tf!("{LOGS} Celý log", "{LOGS} Full log")).clicked() {
                        *action = Some(Action::Logs(e.id.clone(), e.name.clone()));
                    }
                })
                .response
                .on_disabled_hover_text(t!("Kontejner už neexistuje", "The container no longer exists"));
                let (txt, on) = if muted_now {
                    (tf!("{BELL} Znovu oznamovat {}", "{BELL} Notify again for {}", e.name), false)
                } else {
                    (tf!("{BELL_OFF} Neoznamovat {}", "{BELL_OFF} Mute {}", e.name), true)
                };
                if ui
                    .button(txt)
                    .on_hover_text(t!("Problémy kontejneru se dál zapisují sem, jen bez oznámení ve Windows.", "Problems are still recorded here, just without a Windows notification."))
                    .clicked()
                {
                    *action = Some(Action::Mute(e.name.clone(), on));
                }
                if ui.button(tf!("{TRASH} Smazat záznam", "{TRASH} Delete record")).clicked() {
                    *action = Some(Action::Remove(e.seq));
                }
            });
        });
}

fn detail(ui: &mut egui::Ui, e: &Entry) {
    let Some(d) = &e.detail else {
        ui.label(muted(t!("Detail se načítá…", "Loading details…")));
        return;
    };
    egui::Grid::new(("ev-detail", e.seq)).num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
        let mut row = |k: &str, v: String| {
            if !v.is_empty() {
                ui.label(muted(k));
                ui.add(egui::Label::new(v).wrap());
                ui.end_row();
            }
        };
        row(t!("Kontejner", "Container"), format!("{} ({})", e.name, crate::docker::short_id(&e.id)));
        if let (Kind::Crash | Kind::Loop, Some(c)) = (e.kind, e.code) {
            row(t!("Kód ukončení", "Exit code"), format!("{c} – {}", eventlog::exit_meaning(c)));
            row(t!("Co to znamená", "What it means"), eventlog::exit_hint(c).to_string());
        }
        row("Image", d.image.clone());
        if !d.project.is_empty() {
            row("Compose", if d.service.is_empty() { d.project.clone() } else { format!("{} / {}", d.project, d.service) });
        }
        if let Some(q) = e.quiet {
            row(t!("Neoznámeno", "Not notified"), q.label().to_string());
        }
        if e.kind == Kind::Crash {
            let ran = match (crate::inspect::parse_time(&d.started), crate::inspect::parse_time(&d.finished)) {
                (Some(a), Some(b)) if b >= a => tf!("{} (spuštěn {})", "{} (started {})", fmt_dur(b - a), fmt_local(a)),
                _ => String::new(),
            };
            row(t!("Doba běhu", "Ran for"), ran);
        }
        if d.oom_killed {
            row("OOM", t!("ano – ukončen kvůli nedostatku paměti", "yes – killed for running out of memory").into());
        } else if e.code == Some(137) && !d.gone {
            // SIGKILL se často plete s nedostatkem paměti – engine ho tady nehlásí
            row("OOM", t!("ne – engine nedostatek paměti nehlásí", "no – the engine does not report running out of memory").into());
        }
        let policy = if d.restart_policy.is_empty() || d.restart_policy == "no" { String::new() } else { d.restart_policy.clone() };
        if !policy.is_empty() || d.restart_count > 0 {
            row(t!("Restart", "Restart"), tf!("{policy} · restartů: {}", "{policy} · restarts: {}", d.restart_count));
        }
        row(t!("Chyba enginu", "Engine error"), d.error.clone());
        row("Healthcheck", d.health.clone());
        if d.gone {
            row(t!("Stav", "State"), t!("kontejner už byl smazán – detail ani log nejsou k dispozici", "the container was already removed – no details or log available").into());
        }
    });
    if !d.logs.trim().is_empty() {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tf!("Posledních {} řádků logu", "Last {} log lines", eventlog::LOG_LINES)).strong());
            if icon(ui, COPY, t!("Kopírovat log", "Copy log")) {
                ui.ctx().copy_text(d.logs.clone());
            }
        });
        egui::Frame::new().fill(theme::bg()).corner_radius(6).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::vertical().id_salt(("ev-log", e.seq)).max_height(260.0).stick_to_bottom(true).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(d.logs.trim_end()).monospace().size(12.0)).wrap());
            });
        });
    }
}

fn apply(app: &mut App, ctx: &egui::Context, a: Action) {
    match a {
        Action::Toggle(s) => app.events.selected = if app.events.selected == Some(s) { None } else { Some(s) },
        Action::Detail(id, name) => {
            app.page = Page::Containers;
            super::detail::open(app, ctx, id, name);
        }
        Action::Logs(id, name) => app.open_logs(id, name),
        Action::Mute(name, on) => update_notify(app, |c| {
            c.muted.retain(|m| m != &name);
            if on {
                c.muted.push(name.clone());
            }
        }),
        Action::Remove(s) => {
            if let Ok(mut l) = app.shared.event_log.lock() {
                l.remove(s);
            }
            eventlog::save_soon(&app.shared.event_log);
        }
    }
}

fn clear_dialog(app: &mut App, ctx: &egui::Context) {
    if !app.events.confirm_clear {
        return;
    }
    let mut close = false;
    let (_, esc) = modal(ctx, "ev-clear", t!("Smazat historii událostí?", "Clear the event history?"), 420.0, |ui| {
        ui.label(t!("Smažou se všechny záznamy o problémech kontejnerů. Nastavení oznámení zůstane.", "All container problem records will be deleted. Notification settings stay."));
        let (ok, cancel) = dialog_buttons(ui, t!("Smazat", "Clear"), true, true);
        if ok {
            if let Ok(mut l) = app.shared.event_log.lock() {
                l.clear();
            }
            eventlog::save_soon(&app.shared.event_log);
            app.events.selected = None;
        }
        close = ok || cancel;
    });
    if close || esc {
        app.events.confirm_clear = false;
    }
}

/// Změní nastavení oznámení o kontejnerech a hned ho uloží.
fn update_notify(app: &mut App, f: impl FnOnce(&mut ContainerNotify)) {
    let res = match app.cfg.write() {
        Ok(mut g) => {
            f(&mut g.container_notify);
            app.cfg_draft.container_notify = g.container_notify.clone();
            g.save()
        }
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = res {
        app.toast(false, tf!("Nastavení nelze uložit: {e}", "Cannot save settings: {e}"));
    }
}

/// Sekce „Oznámení o kontejnerech“ v Nastavení – mění se okamžitě.
pub fn settings_section(app: &mut App, ui: &mut egui::Ui) {
    section(ui, t!("Oznámení o kontejnerech", "Container notifications"));
    let before = app.cfg.read().map(|c| (c.notifications, c.container_notify.clone())).unwrap_or_default();
    let (mut on, mut c) = before.clone();
    let mut unmute = None;
    let mut open_events = false;
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.checkbox(&mut on, t!("Oznámení ve Windows o problémech kontejnerů", "Windows notifications about container problems"))
            .on_hover_text(t!(
                "Zastavení z aplikace, z příkazu docker ani compose down se za pád nepovažuje.",
                "Stopping a container from the app, the docker command or compose down does not count as a crash."
            ));
        ui.add_enabled_ui(on, |ui| {
            ui.indent("cn-kinds", |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut c.crash, t!("pád (nenulový kód)", "crash (non-zero exit)"));
                    ui.checkbox(&mut c.oom, t!("nedostatek paměti", "out of memory"));
                    ui.checkbox(&mut c.health, t!("selhávající healthcheck", "failing health check"));
                    ui.checkbox(&mut c.restart_loop, t!("opakované restarty", "restart loop"));
                });
                ui.checkbox(&mut c.signal_exits, t!("i ukončení signálem SIGTERM / SIGINT / SIGHUP (kód 143 / 130 / 129)", "also exits by SIGTERM / SIGINT / SIGHUP (code 143 / 130 / 129)"))
                    .on_hover_text(t!(
                        "Obvykle jde o řádné vypnutí zvenku (vypnutí WSL, systemd, kill), ne o chybu aplikace.",
                        "Usually a regular shutdown from outside (WSL shutdown, systemd, kill), not an application error."
                    ));
                ui.checkbox(&mut c.ephemeral, t!("i dočasné kontejnery (--rm) a Testcontainers", "also temporary containers (--rm) and Testcontainers"))
                    .on_hover_text(t!(
                        "Jednorázové příkazy z terminálu a testy – chybu obvykle vidíš rovnou u nich.",
                        "One-off terminal commands and tests – you usually see the error right there."
                    ));
                ui.horizontal(|ui| {
                    ui.label(t!("Stejný problém stejného kontejneru nejvýš jednou za", "The same problem of the same container at most once per"));
                    ui.add(egui::DragValue::new(&mut c.repeat_minutes).range(1..=1440).suffix(" min").speed(0.3));
                });
            });
        });
        if !c.muted.is_empty() {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(t!("Ztlumené kontejnery:", "Muted containers:"));
                for m in &c.muted {
                    if ui.button(format!("{m} \u{E4F6}")).on_hover_text(t!("Znovu oznamovat", "Notify again")).clicked() {
                        unmute = Some(m.clone());
                    }
                }
            });
        }
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(
                muted(t!(
                    "Všechny problémy se zapisují do historie i s detailem, i když se neoznámí. Oznámení těsně po sobě se sloučí do jednoho.",
                    "All problems are recorded in the history with details, even when not notified. Notifications arriving together are merged into one."
                ))
                .small(),
            );
            if ui.link(t!("Otevřít Události", "Open Events")).clicked() {
                open_events = true;
            }
        });
    });
    if let Some(m) = unmute {
        c.muted.retain(|x| x != &m);
    }
    if (on, c.clone()) != before {
        app.cfg_draft.notifications = on;
        app.cfg_draft.container_notify = c.clone();
        let res = match app.cfg.write() {
            Ok(mut g) => {
                g.notifications = on;
                g.container_notify = c;
                g.save()
            }
            Err(e) => Err(e.to_string()),
        };
        if let Err(e) = res {
            app.toast(false, tf!("Nastavení nelze uložit: {e}", "Cannot save settings: {e}"));
        }
    }
    if open_events {
        app.page = Page::Events;
    }
}
