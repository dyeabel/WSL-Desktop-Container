//! Ikona v oznamovací oblasti (tray) s nabídkou a skrývání okna.
//!
//! Okno se skrývá a zobrazuje přímo přes Win32 (ShowWindow), protože eframe
//! u skrytého okna nevolá `update` – akce z traye proto nesmí záviset na UI vlákně.
//! Obslužné rutiny traye běží na hlavním vlákně (ve smyčce zpráv), takže
//! položky nabídky (které nejsou Send) držíme v thread_local.

pub mod menu_model;

use crate::config::Config;
use crate::worker::{Cmd, EngineStatus, Shared, Term};
use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};
use menu_model::{Action, Model};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// HWND hlavního okna (0 = neznámé).
static HWND: AtomicIsize = AtomicIsize::new(0);

/// Požadavek z traye na zobrazení stránky Kontejnery (Some = i detail kontejneru: id, název).
/// Vyzvedne ho `App::handle_tray` v dalším snímku.
static SHOW_REQUEST: Mutex<Option<Option<(String, String)>>> = Mutex::new(None);

/// Vyzvedne požadavek „Zobrazit v aplikaci“ z traye.
pub fn take_show_request() -> Option<Option<(String, String)>> {
    SHOW_REQUEST.lock().ok().and_then(|mut g| g.take())
}

struct Handles {
    tray: TrayIcon,
    status: MenuItem,
    /// podnabídka se seznamem kontejnerů
    containers: Submenu,
    /// model, podle kterého je podnabídka postavená (None = přestavět)
    built: Option<Model>,
    start: MenuItem,
    stop: MenuItem,
    restart: MenuItem,
    sleep: MenuItem,
    free_mem: MenuItem,
    pause: MenuItem,
    unpause: MenuItem,
    close_tray: CheckMenuItem,
    min_tray: CheckMenuItem,
    open: MenuItem,
    term: MenuItem,
    explorer: MenuItem,
    quit: MenuItem,
    icon_on: Icon,
    icon_off: Icon,
    last: Option<EngineStatus>,
    shared: Arc<Shared>,
}

thread_local! {
    static TRAY: RefCell<Option<Handles>> = const { RefCell::new(None) };
}

fn tray_icon(color: [u8; 3]) -> Option<Icon> {
    let n = 32;
    Icon::from_rgba(crate::icon_art::render(n, Some(color)), n, n).ok()
}

// ---------------------------------------------------------------- okno (Win32)

#[cfg(windows)]
mod win {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindow, SW_HIDE, SW_RESTORE, SW_SHOW,
    };

    fn h(hwnd: isize) -> windows_sys::Win32::Foundation::HWND {
        hwnd as _
    }

    pub fn show(hwnd: isize) {
        unsafe {
            if IsIconic(h(hwnd)) != 0 {
                ShowWindow(h(hwnd), SW_RESTORE);
            }
            ShowWindow(h(hwnd), SW_SHOW);
            SetForegroundWindow(h(hwnd));
        }
    }

    pub fn hide(hwnd: isize) {
        unsafe {
            ShowWindow(h(hwnd), SW_HIDE);
        }
    }

    pub fn visible(hwnd: isize) -> bool {
        unsafe { IsWindowVisible(h(hwnd)) != 0 && IsIconic(h(hwnd)) == 0 }
    }

    /// Najde okno již běžící instance podle titulku.
    pub fn find(title: &str) -> Option<isize> {
        let w: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        let hwnd = unsafe { FindWindowW(std::ptr::null(), w.as_ptr()) };
        if hwnd.is_null() { None } else { Some(hwnd as isize) }
    }
}

#[cfg(not(windows))]
mod win {
    pub fn show(_: isize) {}
    pub fn hide(_: isize) {}
    pub fn visible(_: isize) -> bool {
        true
    }
    pub fn find(_: &str) -> Option<isize> {
        None
    }
}

pub fn set_hwnd(hwnd: isize) {
    HWND.store(hwnd, Ordering::SeqCst);
}

fn hwnd() -> isize {
    HWND.load(Ordering::SeqCst)
}

/// Je tray aktivní (a okno jde schovat)?
pub fn active() -> bool {
    hwnd() != 0 && TRAY.with(|t| t.borrow().is_some())
}

pub fn show_window() {
    if hwnd() != 0 {
        win::show(hwnd());
    }
}

pub fn hide_window() {
    if hwnd() != 0 {
        win::hide(hwnd());
    }
}

/// Pokud už aplikace běží, zobrazí její okno a vrátí true.
pub fn activate_existing(title: &str) -> bool {
    match win::find(title) {
        Some(h) => {
            win::show(h);
            true
        }
        None => false,
    }
}

// ---------------------------------------------------------------- tray

/// Vytvoří ikonu v tray. Volat na hlavním vlákně (z `App::new`).
pub fn init(
    ctx: eframe::egui::Context,
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    cfg: Arc<RwLock<Config>>,
) -> Result<(), String> {
    let c = cfg.read().map(|c| c.clone()).unwrap_or_default();
    let status = MenuItem::with_id("status", t!("Zjišťuji stav…", "Checking status…"), false, None);
    let containers = Submenu::with_id("containers", t!("Kontejnery", "Containers"), true);
    let open = MenuItem::with_id("open", t!("Otevřít WLS Desktop", "Open WLS Desktop"), true, None);
    let start = MenuItem::with_id("start", t!("Spustit / probudit engine", "Start / wake engine"), true, None);
    let stop = MenuItem::with_id("stop", t!("Zastavit Docker", "Stop Docker"), true, None);
    let restart = MenuItem::with_id("restart", t!("Restartovat Docker", "Restart Docker"), true, None);
    let sleep = MenuItem::with_id("sleep", t!("Uspat distribuci (uvolnit RAM)", "Put distribution to sleep (free RAM)"), true, None);
    let free_mem = MenuItem::with_id("free_mem", t!("Uvolnit paměť WSL (cache)", "Free WSL memory (cache)"), true, None);
    let pause = MenuItem::with_id("pause", t!("Pozastavit všechny kontejnery", "Pause all containers"), true, None);
    let unpause = MenuItem::with_id("unpause", t!("Obnovit pozastavené kontejnery", "Resume paused containers"), true, None);
    let term = MenuItem::with_id("term", t!("Terminál distribuce", "Distribution terminal"), true, None);
    let explorer = MenuItem::with_id("explorer", t!("Soubory distribuce v Průzkumníku", "Distribution files in Explorer"), true, None);
    let close_tray = CheckMenuItem::with_id("close_tray", t!("Zavírat do oznamovací oblasti", "Close to tray"), true, c.close_to_tray, None);
    let min_tray = CheckMenuItem::with_id("min_tray", t!("Minimalizovat do oznamovací oblasti", "Minimize to tray"), true, c.minimize_to_tray, None);
    let quit = MenuItem::with_id("quit", t!("Ukončit", "Quit"), true, None);

    let menu = Menu::new();
    menu.append_items(&[
        &status,
        &containers,
        &PredefinedMenuItem::separator(),
        &open,
        &PredefinedMenuItem::separator(),
        &start,
        &stop,
        &restart,
        &sleep,
        &free_mem,
        &PredefinedMenuItem::separator(),
        &pause,
        &unpause,
        &PredefinedMenuItem::separator(),
        &term,
        &explorer,
        &PredefinedMenuItem::separator(),
        &close_tray,
        &min_tray,
        &PredefinedMenuItem::separator(),
        &quit,
    ])
    .map_err(|e| e.to_string())?;

    let icon_off = tray_icon([0x80, 0x80, 0x80]).ok_or(t!("ikona", "icon"))?;
    let icon_on = tray_icon([0x3F, 0xC2, 0x5A]).ok_or(t!("ikona", "icon"))?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("WLS Desktop")
        .with_icon(icon_off.clone())
        .build()
        .map_err(|e| e.to_string())?;

    TRAY.with(|t| {
        *t.borrow_mut() = Some(Handles {
            tray,
            status,
            containers,
            built: None,
            start,
            stop,
            restart,
            sleep,
            free_mem,
            pause,
            unpause,
            close_tray,
            min_tray,
            open,
            term,
            explorer,
            quit,
            icon_on,
            icon_off,
            last: None,
            shared,
        })
    });

    // klik na ikonu: levý = zobrazit/schovat okno; pohyb myši = aktualizovat stav
    let ctx2 = ctx.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| match e {
        TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } => {
            if hwnd() != 0 && win::visible(hwnd()) {
                hide_window();
            } else {
                show_window();
                ctx2.request_repaint();
            }
        }
        TrayIconEvent::DoubleClick { .. } => {
            show_window();
            ctx2.request_repaint();
        }
        _ => refresh(),
    }));

    // položky nabídky
    let tx = Mutex::new(tx);
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let send = |c: Cmd| {
            if let Ok(t) = tx.lock() {
                let _ = t.send(c);
            }
        };
        let distro = cfg.read().map(|c| c.distro.clone()).unwrap_or_default();
        if let Some(cmd) = container_cmd(e.id.0.as_str()) {
            match cmd {
                Some(c) => send(c),
                None => {
                    show_window();
                    ctx.request_repaint();
                }
            }
            return;
        }
        match e.id.0.as_str() {
            "open" => {
                show_window();
                ctx.request_repaint();
            }
            "start" => send(Cmd::EngineStart),
            "stop" => send(Cmd::EngineStop),
            "restart" => send(Cmd::EngineRestart),
            "sleep" => send(Cmd::Sleep),
            "free_mem" => send(Cmd::FreeMemory),
            "pause" => send(Cmd::PauseAll),
            "unpause" => send(Cmd::UnpauseAll),
            "term" => send(Cmd::Terminal(Term::Distro(distro))),
            "explorer" => send(Cmd::Explorer("/".into())),
            "close_tray" | "min_tray" => {
                let (close, min) = TRAY.with(|t| {
                    t.borrow().as_ref().map(|h| (h.close_tray.is_checked(), h.min_tray.is_checked())).unwrap_or((true, true))
                });
                if let Ok(mut c) = cfg.write() {
                    c.close_to_tray = close;
                    c.minimize_to_tray = min;
                    let _ = c.save();
                }
                ctx.request_repaint();
            }
            "quit" => {
                crate::portrelay::app().stop_all();
                crate::history::flush_on_exit();
                std::process::exit(0)
            }
            _ => {}
        }
    }));
    retranslate();
    Ok(())
}

/// Zpracuje položku podnabídky kontejnerů (`c:…`).
/// `None` = cizí id; `Some(None)` = jen zobrazit okno (požadavek na stránku je uložený);
/// `Some(Some(cmd))` = poslat příkaz workeru.
fn container_cmd(menu_id: &str) -> Option<Option<Cmd>> {
    if menu_id == menu_model::MORE_ID {
        if let Ok(mut g) = SHOW_REQUEST.lock() {
            *g = Some(None);
        }
        return Some(None);
    }
    let (action, id) = menu_model::decode(menu_id)?;
    if let Some(a) = action.container_action() {
        // stop/restart/… přes Cmd::Container – worker je označí jako akci uživatele (notifikace)
        return Some(Some(Cmd::Container(id.to_string(), a)));
    }
    let name = TRAY
        .with(|t| t.borrow().as_ref().and_then(|h| h.built.as_ref()?.name_of(id).map(str::to_string)))
        .unwrap_or_else(|| crate::docker::short_id(id).to_string());
    let id = id.to_string();
    Some(match action {
        Action::Logs => Some(Cmd::Terminal(Term::Logs { id, name })),
        Action::Console => Some(Cmd::Terminal(Term::Container { id, name })),
        _ => {
            if let Ok(mut g) = SHOW_REQUEST.lock() {
                *g = Some(Some((id, name)));
            }
            None
        }
    })
}

/// Přestaví podnabídku kontejnerů, pokud se změnil model (sada kontejnerů nebo jejich stavy).
fn refresh_containers(h: &mut Handles, connected: bool) {
    let items = h.shared.tray_containers.lock().map(|v| v.clone()).unwrap_or_default();
    let model = Model::build(connected, &items);
    if h.built.as_ref().map(Model::signature) == Some(model.signature()) {
        return;
    }
    let sub = &h.containers;
    sub.set_text(model.title());
    while sub.remove_at(0).is_some() {}
    if let Some(text) = model.placeholder() {
        let _ = sub.append(&MenuItem::with_id("c:none", text, false, None));
    }
    for item in &model.entries {
        let entry = Submenu::new(item.label(), true);
        for a in item.actions() {
            if a == Action::Logs {
                let _ = entry.append(&PredefinedMenuItem::separator());
            }
            let _ = entry.append(&MenuItem::with_id(menu_model::encode(a, &item.id), a.label(), true, None));
        }
        let _ = sub.append(&entry);
    }
    if model.more > 0 {
        let _ = sub.append(&PredefinedMenuItem::separator());
        let _ = sub.append(&MenuItem::with_id(menu_model::MORE_ID, model.more_label(), true, None));
    }
    h.built = Some(model);
}

/// Promítne aktuální stav enginu do ikony, tooltipu a nabídky (jen při změně).
pub fn refresh() {
    TRAY.with(|t| {
        let mut b = t.borrow_mut();
        let Some(h) = b.as_mut() else { return };
        let st = h.shared.status.lock().map(|s| s.clone()).unwrap_or_default();
        refresh_containers(h, st.connected);
        if h.last.as_ref() == Some(&st) {
            return;
        }
        let text = st.text();
        h.status.set_text(&text);
        let _ = h.tray.set_tooltip(Some(format!("WLS Desktop\n{text}")));
        let _ = h.tray.set_icon(Some(if st.connected { h.icon_on.clone() } else { h.icon_off.clone() }));
        h.start.set_enabled(!st.connected);
        if !st.engine.is_empty() {
            h.stop.set_text(tf!("Zastavit {}", "Stop {}", st.engine));
            h.restart.set_text(tf!("Restartovat {}", "Restart {}", st.engine));
        }
        h.stop.set_enabled(st.connected);
        h.restart.set_enabled(st.connected);
        h.sleep.set_enabled(st.distro_running);
        h.free_mem.set_enabled(st.distro_running);
        h.pause.set_enabled(st.connected && st.running > 0);
        h.unpause.set_enabled(st.connected);
        h.last = Some(st);
    });
}

/// Srovná zaškrtnutí v nabídce s konfigurací (po změně v Nastavení).
pub fn sync_checks(close: bool, min: bool) {
    TRAY.with(|t| {
        if let Some(h) = t.borrow().as_ref() {
            if h.close_tray.is_checked() != close {
                h.close_tray.set_checked(close);
            }
            if h.min_tray.is_checked() != min {
                h.min_tray.set_checked(min);
            }
        }
    });
}

/// Nastaví texty nabídky podle aktuálního jazyka (po startu a po přepnutí jazyka).
pub fn retranslate() {
    TRAY.with(|t| {
        if let Some(h) = t.borrow_mut().as_mut() {
            h.open.set_text(t!("Otevřít WLS Desktop", "Open WLS Desktop"));
            h.start.set_text(t!("Spustit / probudit engine", "Start / wake engine"));
            h.sleep.set_text(t!("Uspat distribuci (uvolnit RAM)", "Put distribution to sleep (free RAM)"));
            h.free_mem.set_text(t!("Uvolnit paměť WSL (cache)", "Free WSL memory (cache)"));
            h.pause.set_text(t!("Pozastavit všechny kontejnery", "Pause all containers"));
            h.unpause.set_text(t!("Obnovit pozastavené kontejnery", "Resume paused containers"));
            h.term.set_text(t!("Terminál distribuce", "Distribution terminal"));
            h.explorer.set_text(t!("Soubory distribuce v Průzkumníku", "Distribution files in Explorer"));
            h.close_tray.set_text(t!("Zavírat do oznamovací oblasti", "Close to tray"));
            h.min_tray.set_text(t!("Minimalizovat do oznamovací oblasti", "Minimize to tray"));
            h.quit.set_text(t!("Ukončit", "Quit"));
            // stavové texty, tooltip a podnabídka kontejnerů se přepočítají
            h.last = None;
            h.built = None;
        }
    });
    refresh();
}