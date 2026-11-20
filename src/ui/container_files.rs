//! Prohlížeč souborů v kontejneru – samostatné okno (ikona v řádku kontejneru) i záložka
//! „Soubory“ v detailu kontejneru. Vzhled a ovládání jako stránka Soubory (files.rs).

use super::*;
use crate::app::App;
use crate::container_fs::normalize_path;
use crate::fs::{join, parent, Entry};
use crate::worker::{CfMsg, CfOp, Cmd};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static NEXT_VIEW: AtomicU64 = AtomicU64::new(1);
static DEBUG_DONE: AtomicBool = AtomicBool::new(false);

/// Akce z prohlížeče – provedou se po vykreslení (prohlížeč může být půjčený oknem detailu).
pub enum Pending {
    Send(Cmd),
    Ask(String, String, Cmd),
}

pub struct CfState {
    /// číslo pohledu – odpovědi workeru se směrují podle něj
    pub view: u64,
    pub id: String,
    pub name: String,
    pub path: String,
    pub input: String,
    pub entries: Vec<Entry>,
    pub loaded: bool,
    pub loading: bool,
    pub error: Option<String>,
    /// výpis z archivu (zastavený kontejner / bez shellu)
    pub via_archive: bool,
    pub truncated: bool,
    /// naposledy požadovaná cesta (starší odpovědi se zahodí)
    requested: String,
    pub filter: String,
    pub selected: BTreeSet<String>,
    /// název nové složky
    pub create: Option<String>,
    /// (původní cesta, nový název)
    pub rename: Option<(String, String)>,
    pub editor: Option<files::Editor>,
    pending: Vec<Pending>,
}

impl CfState {
    pub fn new(id: String, name: String, path: Option<String>) -> Self {
        let p = normalize_path(path.as_deref().unwrap_or("/"));
        Self {
            view: NEXT_VIEW.fetch_add(1, Ordering::Relaxed),
            id,
            name,
            input: p.clone(),
            path: p,
            entries: Vec::new(),
            loaded: false,
            loading: false,
            error: None,
            via_archive: false,
            truncated: false,
            requested: String::new(),
            filter: String::new(),
            selected: BTreeSet::new(),
            create: None,
            rename: None,
            editor: None,
            pending: Vec::new(),
        }
    }

    fn cmd(&self, op: CfOp) -> Cmd {
        Cmd::ContainerFs { view: self.view, id: self.id.clone(), op }
    }

    fn send(&mut self, op: CfOp) {
        let c = self.cmd(op);
        self.pending.push(Pending::Send(c));
    }

    fn navigate(&mut self, p: String) {
        self.loading = true;
        self.requested = normalize_path(&p);
        self.send(CfOp::List(self.requested.clone()));
    }

    fn reload(&mut self) {
        let p = self.path.clone();
        self.navigate(p);
    }

    pub fn take_pending(&mut self) -> Vec<Pending> {
        std::mem::take(&mut self.pending)
    }
}

/// Provede akce nasbírané při vykreslení.
pub fn flush(app: &mut App, p: Vec<Pending>) {
    for a in p {
        match a {
            Pending::Send(c) => app.send(c),
            Pending::Ask(title, text, c) => app.ask(title, text, c),
        }
    }
}

/// Otevře samostatné okno prohlížeče (nahradí otevřené).
pub fn open(app: &mut App, id: String, name: String, path: Option<String>) {
    app.cfiles = Some(CfState::new(id, name, path));
}

/// Odpověď workeru – patří oknu, nebo záložce v detailu.
pub fn on_msg(app: &mut App, view: u64, m: CfMsg) {
    let st = match app.cfiles.as_mut() {
        Some(s) if s.view == view => Some(s),
        _ => app.detail.as_mut().and_then(|d| d.cfiles.as_mut()).filter(|s| s.view == view),
    };
    let Some(st) = st else { return };
    if let CfMsg::Listing(p, _) = &m {
        if *p != st.requested {
            return; // mezitím se navigovalo jinam
        }
    }
    match m {
        CfMsg::Listing(_, Ok(l)) => {
            st.loading = false;
            st.loaded = true;
            st.error = None;
            if st.path != l.cwd {
                st.selected.clear();
            }
            st.path = l.cwd.clone();
            st.input = l.cwd;
            st.entries = l.entries;
            st.via_archive = l.via_archive;
            st.truncated = l.truncated;
            let names: BTreeSet<String> = st.entries.iter().map(|e| e.name.clone()).collect();
            st.selected.retain(|n| names.contains(n));
        }
        CfMsg::Listing(_, Err(e)) => {
            st.loading = false;
            st.loaded = true;
            st.input = st.path.clone();
            st.error = Some(e);
        }
        CfMsg::File(path, text) => st.editor = Some(files::Editor { path, orig: text.clone(), text }),
        CfMsg::Changed => st.reload(),
    }
}

/// Pro ladění/screenshoty: `WSLCD_DEBUG_CFILES=<název kontejneru>[:<cesta>]`.
fn debug_open(app: &mut App) {
    if DEBUG_DONE.load(Ordering::Relaxed) {
        return;
    }
    let Ok(v) = std::env::var("WSLCD_DEBUG_CFILES") else {
        DEBUG_DONE.store(true, Ordering::Relaxed);
        return;
    };
    let (name, path) = match v.split_once(':') {
        Some((n, p)) => (n, Some(p.to_string())),
        None => (v.as_str(), None),
    };
    let Some(c) = app.snap.containers.iter().find(|c| c.name() == name) else { return };
    DEBUG_DONE.store(true, Ordering::Relaxed);
    let (id, name) = (c.id.clone(), c.name());
    open(app, id, name, path);
}

/// Samostatné okno prohlížeče.
pub fn window(app: &mut App, ctx: &egui::Context) {
    debug_open(app);
    let Some(id) = app.cfiles.as_ref().map(|s| s.id.clone()) else { return };
    let c = app.snap.containers.iter().find(|c| c.id == id);
    let running = c.map(|c| c.running()).unwrap_or(false);
    let gone = app.snap.connected && c.is_none();
    let Some(st) = app.cfiles.as_mut() else { return };
    let mut open = true;
    let screen = ctx.screen_rect();
    egui::Window::new(tf!("\u{E260} Soubory v kontejneru {}", "\u{E260} Files in container {}", st.name))
        .id(egui::Id::new(("cfiles-window", st.view)))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([(screen.width() - 80.0).min(900.0), (screen.height() - 90.0).min(620.0)])
        .min_size([420.0, 300.0])
        .max_size(screen.size() - egui::vec2(16.0, 40.0))
        .show(ctx, |ui| {
            if gone {
                ui.label(RichText::new(t!("Kontejner už neexistuje.", "The container no longer exists.")).color(theme::red()));
            } else {
                view(ui, st, running);
            }
        });
    let p = st.take_pending();
    if !open {
        app.cfiles = None;
    }
    flush(app, p);
}

fn kind_icon(e: &Entry) -> &'static str {
    if e.is_dir() {
        if e.is_link() { "\u{E256}" } else { "\u{E24A}" }
    } else if e.is_link() {
        "\u{E2E2}"
    } else {
        "\u{E230}"
    }
}

/// Obsah prohlížeče (okno i záložka detailu). `running` = kontejner běží.
pub fn view(ui: &mut egui::Ui, st: &mut CfState, running: bool) {
    if !st.loaded && !st.loading {
        st.reload();
    }
    // mazání a přejmenování potřebují exec (běžící kontejner se shellem)
    let can_exec = running && st.loaded && !st.via_archive;
    let cur = st.path.clone();

    // --- navigace ---
    ui.horizontal(|ui| {
        if icon(ui, "\u{E08E}", t!("O úroveň výš", "Up one level")) {
            st.navigate(parent(&cur));
        }
        if icon(ui, "\u{E2C2}", t!("Kořen (/)", "Root (/)")) {
            st.navigate("/".into());
        }
        if st.loading {
            ui.add_sized([26.0, 24.0], egui::Spinner::new());
        } else if icon(ui, "\u{E094}", t!("Obnovit", "Refresh")) {
            st.reload();
        }
        let w = ui.available_width().max(120.0);
        let r = ui.add(field(&mut st.input).font(egui::TextStyle::Monospace).desired_width(w));
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let p = st.input.trim().to_string();
            st.navigate(p);
        }
    });
    // drobečková navigace
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let mut go = None;
        if ui.add(egui::Button::new(RichText::new("/").monospace()).frame(false)).on_hover_text("/").clicked() {
            go = Some("/".to_string());
        }
        let mut acc = String::new();
        for (i, part) in cur.split('/').filter(|p| !p.is_empty()).enumerate() {
            acc = join(if i == 0 { "/" } else { &acc }, part);
            if i > 0 {
                ui.label(muted("/"));
            }
            if ui.add(egui::Button::new(RichText::new(part).monospace()).frame(false)).clicked() {
                go = Some(acc.clone());
            }
        }
        if let Some(p) = go {
            st.navigate(p);
        }
    });
    ui.add_space(4.0);

    // --- akce ---
    let mut upload_files = false;
    let mut upload_dir = false;
    toolbar_lr(ui, 330.0, |ui, right| {
        if !right {
            if ui.button(t!("\u{E258} Nová složka…", "\u{E258} New folder…")).clicked() {
                st.create = Some(String::new());
            }
            ui.menu_button(t!("\u{E4C0} Nahrát z Windows \u{E136}", "\u{E4C0} Upload from Windows \u{E136}"), |ui| {
                if ui.button(t!("Soubory…", "Files…")).on_hover_text(t!("Zkopírovat soubory z Windows do této složky", "Copy files from Windows into this folder")).clicked() {
                    upload_files = true;
                    ui.close_menu();
                }
                if ui.button(t!("Složku…", "Folder…")).clicked() {
                    upload_dir = true;
                    ui.close_menu();
                }
            });
        } else {
            more_menu(ui, |ui| {
                if ui.button(t!("\u{E1CA} Kopírovat cestu", "\u{E1CA} Copy path")).clicked() {
                    ui.ctx().copy_text(cur.clone());
                    ui.close_menu();
                }
                if ui.button(t!("\u{E184} Vybrat vše", "\u{E184} Select all")).clicked() {
                    let f = st.filter.to_lowercase();
                    st.selected = st.entries.iter().filter(|e| f.is_empty() || e.name.to_lowercase().contains(&f)).map(|e| e.name.clone()).collect();
                    ui.close_menu();
                }
            });
            filter_field(ui, &mut st.filter);
            if st.via_archive {
                badge(ui, t!("archiv", "archive"), theme::yellow()).on_hover_text(t!(
                    "Kontejner neběží nebo nemá shell – výpis z archivu API. Mazání a přejmenování nejsou k dispozici.",
                    "The container is not running or has no shell – listing from the archive API. Delete and rename are not available."
                ));
            }
        }
    });
    if upload_files {
        if let Some(files) = rfd::FileDialog::new().set_title(t!("Nahrát do kontejneru", "Upload to container")).pick_files() {
            let v = files.into_iter().map(|p| p.display().to_string()).collect();
            st.send(CfOp::Upload(v, cur.clone()));
        }
    }
    if upload_dir {
        if let Some(dir) = rfd::FileDialog::new().set_title(t!("Nahrát složku do kontejneru", "Upload folder to container")).pick_folder() {
            st.send(CfOp::Upload(vec![dir.display().to_string()], cur.clone()));
        }
    }

    // --- výběr ---
    let f = st.filter.to_lowercase();
    let mut entries: Vec<Entry> = st.entries.iter().filter(|e| f.is_empty() || e.name.to_lowercase().contains(&f)).cloned().collect();
    sort_items(ui.ctx(), "cfiles", &mut entries, |e, col| match col {
        2 => SortKey::num(e.size as f64),
        3 => SortKey::num(e.mtime as f64),
        4 => SortKey::text(&e.mode),
        5 => SortKey::text(&e.owner),
        _ => SortKey::text(&e.name),
    });
    entries.sort_by_key(|e| !e.is_dir());
    let nsel = st.selected.len();
    if nsel > 0 {
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(theme::accent().gamma_multiply(0.12))
            .stroke(egui::Stroke::new(1.0_f32, theme::accent().gamma_multiply(0.4)))
            .corner_radius(6)
            .inner_margin(egui::Margin::symmetric(10, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                toolbar(ui, |ui| {
                    ui.label(RichText::new(tf!("Vybráno: {nsel}", "Selected: {nsel}")).strong());
                    gap(ui);
                    if ui.button(t!("\u{E20C} Stáhnout do Windows…", "\u{E20C} Download to Windows…")).clicked() {
                        if let Some(dir) = rfd::FileDialog::new().set_title(t!("Uložit do…", "Save to…")).pick_folder() {
                            let items: Vec<String> = st.selected.iter().map(|n| join(&cur, n)).collect();
                            st.send(CfOp::Download(items, dir.display().to_string()));
                        }
                    }
                    if can_exec && ui.button(RichText::new(t!("\u{E4A6} Smazat…", "\u{E4A6} Delete…")).color(theme::red())).clicked() {
                        let items: Vec<String> = st.selected.iter().map(|n| join(&cur, n)).collect();
                        let list = st.selected.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
                        let c = st.cmd(CfOp::Delete(items));
                        st.pending.push(Pending::Ask(
                            tf!("Smazat {nsel} položek?", "Delete {nsel} items?"),
                            tf!(
                                "V kontejneru {} se nenávratně smaže (rm -rf): {list}{}",
                                "Will be permanently deleted in container {} (rm -rf): {list}{}",
                                st.name,
                                if nsel > 8 { ", …" } else { "" }
                            ),
                            c,
                        ));
                    }
                    if ui.button(t!("Zrušit výběr", "Clear selection")).clicked() {
                        st.selected.clear();
                    }
                });
            });
    }
    ui.add_space(6.0);

    if let Some(e) = st.error.clone() {
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("\u{E4E0} {e}")).color(theme::red()));
        });
        ui.add_space(6.0);
    }
    if st.truncated {
        ui.label(RichText::new(t!(
            "\u{E4E0} Složka je příliš velká na výpis z archivu – výpis není úplný. Spusť kontejner pro úplný výpis.",
            "\u{E4E0} The folder is too large to list from the archive – the listing is incomplete. Start the container for a full listing."
        ))
        .color(theme::yellow()));
        ui.add_space(4.0);
    }

    // --- výpis ---
    let mut go = None;
    let cols = [
        ("", W::Px(22.0)),
        (t!("NÁZEV", "NAME"), W::Fr(1.0)),
        (t!("VELIKOST", "SIZE"), W::Px(76.0)),
        (t!("ZMĚNĚNO", "MODIFIED"), W::PxOpt(96.0)),
        (t!("PRÁVA", "MODE"), W::PxOpt(52.0)),
        (t!("VLASTNÍK", "OWNER"), W::PxOpt(78.0)),
        (t!("AKCE", "ACTIONS"), icons_w(2)),
    ];
    egui::ScrollArea::vertical().id_salt(("cfiles-scroll", st.view)).auto_shrink([false, false]).show(ui, |ui| {
        table_sort(ui, &format!("cfiles-{}", st.view), &cols, Sort { key: "cfiles", cols: &[1, 2, 3, 4, 5] }, |mut body| {
            if cur != "/" {
                body.row(30.0, |mut r| {
                    r.col(|_| {});
                    r.col(|ui| {
                        let l = ui.add(egui::Label::new(RichText::new("\u{E24A} ..").strong()).selectable(false).sense(egui::Sense::click()));
                        if l.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(t!("O úroveň výš", "Up one level")).clicked() {
                            go = Some(parent(&cur));
                        }
                    });
                    for _ in 0..5 {
                        r.col(|_| {});
                    }
                });
            }
            for e in &entries {
                let path = join(&cur, &e.name);
                // odkaz s neznámým cílem (výpis z archivu) zkusí otevřít jako složku
                let opens = e.is_dir() || (e.is_link() && e.target_kind == '?');
                body.row(30.0, |mut r| {
                    r.col(|ui| {
                        let mut sel = st.selected.contains(&e.name);
                        if ui.checkbox(&mut sel, "").changed() {
                            if sel {
                                st.selected.insert(e.name.clone());
                            } else {
                                st.selected.remove(&e.name);
                            }
                        }
                    });
                    r.col(|ui| {
                        let label = format!("{} {}", kind_icon(e), e.name.replace('\n', "\u{21B5}"));
                        if opens {
                            let l = ui.add(egui::Label::new(RichText::new(label).strong()).selectable(false).sense(egui::Sense::click()));
                            if l.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                go = Some(path.clone());
                            }
                        } else {
                            let l = ui.add(egui::Label::new(label).selectable(false).sense(egui::Sense::click()));
                            if (e.kind == 'f' || e.is_link())
                                && l.on_hover_text(t!("Dvojklik = upravit", "Double-click = edit")).double_clicked() {
                                    st.send(CfOp::Read(path.clone()));
                                }
                        }
                    });
                    r.col(|ui| {
                        ui.label(if e.is_dir() { String::new() } else { fmt_bytes(e.size) });
                    });
                    r.col(|ui| {
                        ui.label(muted(fmt_ago(e.mtime)));
                    });
                    r.col(|ui| {
                        ui.label(RichText::new(&e.mode).monospace());
                    });
                    r.col(|ui| {
                        ui.label(&e.owner);
                    });
                    r.col(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if opens || !(e.kind == 'f' || e.is_link()) {
                            ui.add_space(26.0 + ui.spacing().item_spacing.x);
                        } else if icon(ui, "\u{E34C}", t!("Upravit ve vestavěném editoru", "Edit in built-in editor")) {
                            st.send(CfOp::Read(path.clone()));
                        }
                        let mut download = false;
                        row_menu(ui, |ui| {
                            if ui.button(t!("\u{E20C} Stáhnout do Windows…", "\u{E20C} Download to Windows…")).clicked() {
                                download = true;
                                ui.close_menu();
                            }
                            if ui.button(t!("\u{E1CA} Kopírovat cestu", "\u{E1CA} Copy path")).clicked() {
                                ui.ctx().copy_text(path.clone());
                                ui.close_menu();
                            }
                            if can_exec {
                                if ui.button(t!("\u{E3B4} Přejmenovat / přesunout…", "\u{E3B4} Rename / move…")).clicked() {
                                    st.rename = Some((path.clone(), e.name.clone()));
                                    ui.close_menu();
                                }
                                ui.separator();
                                if ui.button(RichText::new(t!("\u{E4A6} Smazat…", "\u{E4A6} Delete…")).color(theme::red())).clicked() {
                                    let c = st.cmd(CfOp::Delete(vec![path.clone()]));
                                    st.pending.push(Pending::Ask(
                                        tf!("Smazat {}?", "Delete {}?", e.name),
                                        tf!(
                                            "{} v kontejneru {} bude nenávratně smazán{}.",
                                            "{} in container {} will be permanently deleted{}.",
                                            path,
                                            st.name,
                                            if e.is_dir() { t!("a včetně obsahu", " including its contents") } else { "" }
                                        ),
                                        c,
                                    ));
                                    ui.close_menu();
                                }
                            } else {
                                ui.separator();
                                ui.label(muted(t!("Mazání a přejmenování: jen běžící kontejner se shellem", "Delete and rename: running container with a shell only")).small());
                            }
                        });
                        if download {
                            if let Some(dir) = rfd::FileDialog::new().set_title(t!("Uložit do…", "Save to…")).pick_folder() {
                                st.send(CfOp::Download(vec![path.clone()], dir.display().to_string()));
                            }
                        }
                    });
                });
            }
        });
        if entries.is_empty() && st.loaded && st.error.is_none() {
            ui.add_space(6.0);
            ui.label(muted(t!("Prázdná složka", "Empty folder")));
        }
    });
    if let Some(p) = go {
        st.navigate(p);
    }
    dialogs(ui.ctx(), st);
}

fn dialogs(ctx: &egui::Context, st: &mut CfState) {
    let cur = st.path.clone();
    let view = st.view;
    let mut op = None;

    // nová složka
    let mut close = false;
    if let Some(name) = st.create.as_mut() {
        let (_, esc) = modal(ctx, &format!("cfs-create-{view}"), t!("Nová složka", "New folder"), 420.0, |ui| {
            ui.label(muted(t!("Ve složce", "In folder")));
            ui.label(RichText::new(&cur).monospace());
            ui.add_space(8.0);
            let r = ui.add(field(name).hint_text(t!("název", "name")).desired_width(f32::INFINITY));
            if !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let n = name.trim();
            let valid = !n.is_empty() && !n.contains('/') && n != "." && n != "..";
            let (ok, cancel) = dialog_buttons(ui, t!("Vytvořit", "Create"), false, valid);
            if (ok || enter) && valid {
                op = Some(CfOp::Mkdir(join(&cur, n)));
                close = true;
            }
            close |= cancel;
        });
        close |= esc;
    }
    if close {
        st.create = None;
    }

    // přejmenování
    let mut close = false;
    if let Some((from, name)) = st.rename.as_mut() {
        let (_, esc) = modal(ctx, &format!("cfs-rename-{view}"), t!("Přejmenovat / přesunout", "Rename / move"), 460.0, |ui| {
            ui.label(RichText::new(from.as_str()).monospace());
            ui.add_space(8.0);
            let r = ui.add(field(name).desired_width(f32::INFINITY));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.label(muted(t!("Nový název, nebo celá cesta začínající / pro přesun.", "New name, or a full path starting with / to move.")).small());
            let n = name.trim();
            let valid = !n.is_empty() && n != "." && n != "..";
            let (ok, cancel) = dialog_buttons(ui, t!("Přejmenovat", "Rename"), false, valid);
            if (ok || enter) && valid {
                let to = if n.starts_with('/') { n.to_string() } else { join(&parent(from), n) };
                op = Some(CfOp::Rename(from.clone(), to));
                close = true;
            }
            close |= cancel;
        });
        close |= esc;
    }
    if close {
        st.rename = None;
    }

    // editor
    let mut open = st.editor.is_some();
    if let Some(ed) = st.editor.as_mut() {
        let dirty = ed.text != ed.orig;
        egui::Window::new(format!("\u{E34C} {}: {}{}", st.name, ed.path, if dirty { " •" } else { "" }))
            .collapsible(false)
            .id(egui::Id::new(("cfile-editor", view)))
            .open(&mut open)
            .default_size([820.0, 560.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let ctrl_s = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S));
                    ui.add_enabled_ui(dirty, |ui| {
                        if accent_button(ui, t!("\u{E248} Uložit (Ctrl+S)", "\u{E248} Save (Ctrl+S)")).clicked() || (ctrl_s && dirty) {
                            op = Some(CfOp::Write(ed.path.clone(), ed.text.clone()));
                            ed.orig = ed.text.clone();
                        }
                    });
                    if dirty && ui.button(t!("\u{E038} Zahodit změny", "\u{E038} Discard changes")).clicked() {
                        ed.text = ed.orig.clone();
                    }
                    ui.label(muted(tf!("{} řádků", "{} lines", ed.text.lines().count())).small());
                });
                ui.separator();
                egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add(area(&mut ed.text).code_editor().desired_width(f32::INFINITY).desired_rows(30));
                });
            });
    }
    if !open {
        st.editor = None;
    }
    if let Some(o) = op {
        st.send(o);
    }
}
