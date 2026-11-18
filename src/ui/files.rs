use super::*;
use crate::app::App;
use crate::fs::{join, parent, Entry};
use crate::worker::{Cmd, FsOp, Term};
use std::collections::BTreeSet;

pub struct Editor {
    pub path: String,
    pub text: String,
    pub orig: String,
}

pub struct FilesState {
    pub path: String,
    pub input: String,
    pub entries: Vec<Entry>,
    pub loaded: bool,
    pub loading: bool,
    pub error: Option<String>,
    pub root: bool,
    pub filter: String,
    pub selected: BTreeSet<String>,
    /// (je složka, název)
    pub create: Option<(bool, String)>,
    /// (původní cesta, nový název)
    pub rename: Option<(String, String)>,
    /// (cesta, práva)
    pub chmod: Option<(String, String)>,
    pub editor: Option<Editor>,
}

impl Default for FilesState {
    fn default() -> Self {
        Self {
            path: "~".into(),
            input: "~".into(),
            entries: Vec::new(),
            loaded: false,
            loading: false,
            error: None,
            root: false,
            filter: String::new(),
            selected: BTreeSet::new(),
            create: None,
            rename: None,
            chmod: None,
            editor: None,
        }
    }
}

fn fs(app: &App, op: FsOp) {
    app.send(Cmd::Fs { root: app.files.root, op });
}

pub fn reload(app: &mut App) {
    let p = app.files.path.clone();
    navigate(app, p);
}

pub fn navigate(app: &mut App, path: String) {
    app.files.loading = true;
    fs(app, FsOp::List(path));
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

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let distro = app.distro();
    header(ui, t!("Soubory", "Files"), &tf!("Souborový systém distribuce {distro}", "File system of distribution {distro}"));
    if !app.files.loaded && !app.files.loading {
        reload(app);
    }

    // --- navigace ---
    // --- navigace ---
    ui.horizontal(|ui| {
        let cur = app.files.path.clone();
        if icon(ui, "\u{E08E}", t!("O úroveň výš", "Up one level")) {
            navigate(app, parent(&cur));
        }
        if icon(ui, "\u{E2C2}", t!("Domovská složka", "Home folder")) {
            navigate(app, "~".into());
        }
        if app.files.loading {
            // při načítání místo ikony obnovení kolečko (stejná šířka)
            ui.add_sized([26.0, 24.0], egui::Spinner::new());
        } else if icon(ui, "\u{E094}", t!("Obnovit", "Refresh")) {
            reload(app);
        }
        // pole vyplní šířku až po Záložky (150) u pravého okraje
        let w = (ui.available_width() - 150.0 - ui.spacing().item_spacing.x).max(120.0);
        let r = ui.add(field(&mut app.files.input).font(egui::TextStyle::Monospace).desired_width(w));
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let p = app.files.input.trim().to_string();
            navigate(app, p);
        }
        let mut marks: Vec<(String, String)> = vec![
            (t!("\u{E2C2} Domů", "\u{E2C2} Home").into(), "~".into()),
            (t!("/ Kořen", "/ Root").into(), "/".into()),
            ("\u{E1DA} /etc/docker".into(), "/etc/docker".into()),
            ("\u{E2A0} Docker volume".into(), "/var/lib/docker/volumes".into()),
            ("\u{E270} /etc".into(), "/etc".into()),
            ("\u{E692} Disk C: (/mnt/c)".into(), "/mnt/c".into()),
        ];
        let mut dirs: BTreeSet<String> = BTreeSet::new();
        for c in &app.snap.containers {
            if let Some(d) = c.label("com.docker.compose.project.working_dir") {
                dirs.insert(d.to_string());
            }
        }
        marks.extend(dirs.into_iter().map(|d| (format!("\u{E466} {d}"), d)));
        let mut go = None;
        egui::ComboBox::from_id_salt("marks").selected_text(t!("\u{E46A} Záložky", "\u{E46A} Bookmarks")).width(150.0).show_ui(ui, |ui| {
            for (l, p) in &marks {
                if ui.selectable_label(false, l).clicked() {
                    go = Some(p.clone());
                }
            }
        });
        if let Some(p) = go {
            if p.starts_with("/var/lib/docker") && !app.files.root {
                app.files.root = true;
                app.toast(true, t!("Docker data patří rootovi – přepnuto na režim root", "Docker data belongs to root – switched to root mode"));
            }
            navigate(app, p);
        }
    });
    ui.add_space(4.0);

    // --- akce ---
    let cur_path = app.files.path.clone();
    let mut upload_files = false;
    let mut upload_dir = false;
    toolbar_lr(ui, 330.0, |ui, right| {
        let cur = cur_path.clone();
        if !right {
            ui.menu_button(t!("\u{E3D4} Nový \u{E136}", "\u{E3D4} New \u{E136}"), |ui| {
                if ui.button(t!("\u{E258} Složka…", "\u{E258} Folder…")).clicked() {
                    app.files.create = Some((true, String::new()));
                    ui.close_menu();
                }
                if ui.button(t!("\u{E236} Soubor…", "\u{E236} File…")).clicked() {
                    app.files.create = Some((false, String::new()));
                    ui.close_menu();
                }
            });
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
                if ui.button(t!("\u{E256} Otevřít v Průzkumníku", "\u{E256} Open in Explorer")).clicked() {
                    app.send(Cmd::Explorer(cur.clone()));
                    ui.close_menu();
                }
                if ui.button(t!("\u{EAE8} Terminál v této složce", "\u{EAE8} Terminal in this folder")).clicked() {
                    app.send(Cmd::Terminal(Term::Dir(cur.clone())));
                    ui.close_menu();
                }
                if ui.button(t!("\u{E1CA} Kopírovat cestu", "\u{E1CA} Copy path")).clicked() {
                    ui.ctx().copy_text(cur.clone());
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(t!("\u{E184} Vybrat vše", "\u{E184} Select all")).clicked() {
                    let f = app.files.filter.to_lowercase();
                    app.files.selected = app
                        .files
                        .entries
                        .iter()
                        .filter(|e| f.is_empty() || e.name.to_lowercase().contains(&f))
                        .map(|e| e.name.clone())
                        .collect();
                    ui.close_menu();
                }
                ui.separator();
                if ui.checkbox(&mut app.files.root, t!("Pracovat jako root", "Work as root")).on_hover_text(t!("Operace poběží pod uživatelem root (wsl -u root)", "Operations will run as the root user (wsl -u root)")).changed() {
                    reload(app);
                    ui.close_menu();
                }
            });
            filter_field(ui, &mut app.files.filter);
            if app.files.root && badge(ui, "root", theme::yellow()).on_hover_text(t!("Pracuješ jako root – kliknutím vypneš", "Working as root – click to turn off")).interact(egui::Sense::click()).clicked() {
                app.files.root = false;
                reload(app);
            }
        }
    });
    if upload_files {
        if let Some(files) = rfd::FileDialog::new().set_title(t!("Nahrát do WSL", "Upload to WSL")).pick_files() {
            let v = files.into_iter().map(|p| p.display().to_string()).collect();
            fs(app, FsOp::Upload(v, cur_path.clone()));
        }
    }
    if upload_dir {
        if let Some(dir) = rfd::FileDialog::new().set_title(t!("Nahrát složku do WSL", "Upload folder to WSL")).pick_folder() {
            fs(app, FsOp::Upload(vec![dir.display().to_string()], cur_path.clone()));
        }
    }

    // --- výběr (lišta se ukáže jen když je něco vybráno) ---
    let cur = app.files.path.clone();
    let f = app.files.filter.to_lowercase();
    let mut entries: Vec<Entry> = app
        .files
        .entries
        .iter()
        .filter(|e| f.is_empty() || e.name.to_lowercase().contains(&f))
        .cloned()
        .collect();
    sort_items(ui.ctx(), "files", &mut entries, |e, col| match col {
        2 => SortKey::num(e.size as f64),
        3 => SortKey::num(e.mtime as f64),
        4 => SortKey::text(&e.owner),
        5 => SortKey::text(&e.mode),
        _ => SortKey::text(&e.name),
    });
    // složky vždy nahoře (stabilně – pořadí uvnitř zůstane)
    entries.sort_by_key(|e| !e.is_dir());
    let nsel = app.files.selected.len();
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
                            let items: Vec<String> = app.files.selected.iter().map(|n| join(&cur, n)).collect();
                            fs(app, FsOp::Download(items, dir.display().to_string()));
                        }
                    }
                    if ui.button(RichText::new(t!("\u{E4A6} Smazat…", "\u{E4A6} Delete…")).color(theme::red())).clicked() {
                        let items: Vec<String> = app.files.selected.iter().map(|n| join(&cur, n)).collect();
                        let list = app.files.selected.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
                        app.ask(
                            tf!("Smazat {nsel} položek?", "Delete {nsel} items?"),
                            tf!("Nenávratně se smaže (rm -rf): {list}{}", "Will be permanently deleted (rm -rf): {list}{}", if nsel > 8 { ", …" } else { "" }),
                            Cmd::Fs { root: app.files.root, op: FsOp::Delete(items) },
                        );
                    }
                    if ui.button(t!("Zrušit výběr", "Clear selection")).clicked() {
                        app.files.selected.clear();
                    }
                });
            });
    }
    ui.add_space(6.0);

    if let Some(e) = app.files.error.clone() {
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("\u{E4E0} {e}")).color(theme::red()));
            if !app.files.root {
                ui.label(muted(t!("Nedostatečná práva? Zapni „Jako root“.", "Insufficient permissions? Turn on “As root”.")));
            }
        });
        ui.add_space(6.0);
    }

    // --- výpis ---
    let mut go = None;
    let cols = [
        ("", W::Px(22.0)),
        (t!("NÁZEV", "NAME"), W::Fr(1.0)),
        (t!("VELIKOST", "SIZE"), W::Px(76.0)),
        (t!("ZMĚNĚNO", "MODIFIED"), W::PxOpt(96.0)),
        (t!("VLASTNÍK", "OWNER"), W::PxOpt(78.0)),
        (t!("PRÁVA", "MODE"), W::PxOpt(48.0)),
        (t!("AKCE", "ACTIONS"), icons_w(2)),
    ];
    table_sort(ui, "files", &cols, Sort { key: "files", cols: &[1, 2, 3, 4, 5] }, |mut body| {
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
            body.row(30.0, |mut r| {
                r.col(|ui| {
                    let mut sel = app.files.selected.contains(&e.name);
                    if ui.checkbox(&mut sel, "").changed() {
                        if sel {
                            app.files.selected.insert(e.name.clone());
                        } else {
                            app.files.selected.remove(&e.name);
                        }
                    }
                });
                r.col(|ui| {
                    let label = format!("{} {}", kind_icon(e), e.name);
                    if e.is_dir() {
                        let l = ui.add(egui::Label::new(RichText::new(label).strong()).selectable(false).sense(egui::Sense::click()));
                        if l.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            go = Some(path.clone());
                        }
                    } else {
                        let l = ui.add(egui::Label::new(label).selectable(false).sense(egui::Sense::click()));
                        if l.on_hover_text(t!("Dvojklik = upravit", "Double-click = edit")).double_clicked() {
                            fs(app, FsOp::Read(path.clone()));
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
                    ui.label(&e.owner);
                });
                r.col(|ui| {
                    ui.label(RichText::new(&e.mode).monospace());
                });
                r.col(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if e.is_dir() {
                        // místo ikony Upravit – ⋯ zůstane ve stejném sloupci jako u souborů
                        ui.add_space(26.0 + ui.spacing().item_spacing.x);
                    } else if icon(ui, "\u{E34C}", t!("Upravit ve vestavěném editoru", "Edit in built-in editor")) {
                        fs(app, FsOp::Read(path.clone()));
                    }
                    // ostatní akce v nabídce řádku
                    let mut download = false;
                    row_menu(ui, |ui| {
                        if !e.is_dir() && ui.button(t!("\u{E23A} Otevřít v Notepadu", "\u{E23A} Open in Notepad")).clicked() {
                            app.send(Cmd::Notepad(path.clone()));
                            ui.close_menu();
                        }
                        if ui.button(t!("\u{E3B4} Přejmenovat / přesunout…", "\u{E3B4} Rename / move…")).clicked() {
                            app.files.rename = Some((path.clone(), e.name.clone()));
                            ui.close_menu();
                        }
                        if ui.button(t!("\u{E2D6} Změnit práva…", "\u{E2D6} Change permissions…")).clicked() {
                            app.files.chmod = Some((path.clone(), e.mode.clone()));
                            ui.close_menu();
                        }
                        if ui.button(t!("\u{E20C} Stáhnout do Windows…", "\u{E20C} Download to Windows…")).clicked() {
                            download = true;
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button(RichText::new(t!("\u{E4A6} Smazat…", "\u{E4A6} Delete…")).color(theme::red())).clicked() {
                            app.ask(
                                tf!("Smazat {}?", "Delete {}?", e.name),
                                tf!("{} bude nenávratně smazán{}.", "{} will be permanently deleted{}.", path, if e.is_dir() { t!("a včetně obsahu", " including its contents") } else { "" }),
                                Cmd::Fs { root: app.files.root, op: FsOp::Delete(vec![path.clone()]) },
                            );
                            ui.close_menu();
                        }
                    });
                    if download {
                        if let Some(dir) = rfd::FileDialog::new().set_title(t!("Uložit do…", "Save to…")).pick_folder() {
                            fs(app, FsOp::Download(vec![path.clone()], dir.display().to_string()));
                        }
                    }
                });
            });
        }
    });
    if entries.is_empty() && app.files.loaded && app.files.error.is_none() {
        ui.add_space(6.0);
        ui.label(muted(t!("Prázdná složka", "Empty folder")));
    }
    if let Some(p) = go {
        navigate(app, p);
    }
}

pub fn dialogs(app: &mut App, ctx: &egui::Context) {
    let cur = app.files.path.clone();
    let root = app.files.root;
    let mut op = None;

    // nová složka / soubor
    let mut close = false;
    if let Some((is_dir, name)) = app.files.create.as_mut() {
        let title = if *is_dir { t!("Nová složka", "New folder") } else { t!("Nový soubor", "New file") };
        let (_, esc) = modal(ctx, "fs-create", title, 420.0, |ui| {
            ui.label(muted(t!("Ve složce", "In folder")));
            ui.label(RichText::new(&cur).monospace());
            ui.add_space(8.0);
            let r = ui.add(field(name).hint_text(t!("název", "name")).desired_width(f32::INFINITY));
            if !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let valid = !name.trim().is_empty() && !name.contains('/');
            let (ok, cancel) = dialog_buttons(ui, t!("Vytvořit", "Create"), false, valid);
            if (ok || enter) && valid {
                let p = join(&cur, name.trim());
                op = Some(if *is_dir { FsOp::Mkdir(p) } else { FsOp::Touch(p) });
                close = true;
            }
            close |= cancel;
        });
        close |= esc;
    }
    if close {
        app.files.create = None;
    }

    // přejmenování
    let mut close = false;
    if let Some((from, name)) = app.files.rename.as_mut() {
        let (_, esc) = modal(ctx, "fs-rename", t!("Přejmenovat / přesunout", "Rename / move"), 460.0, |ui| {
            ui.label(RichText::new(from.as_str()).monospace());
            ui.add_space(8.0);
            let r = ui.add(field(name).desired_width(f32::INFINITY));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.label(muted(t!("Nový název, nebo celá cesta začínající / pro přesun.", "New name, or a full path starting with / to move.")).small());
            let valid = !name.trim().is_empty();
            let (ok, cancel) = dialog_buttons(ui, t!("Přejmenovat", "Rename"), false, valid);
            if (ok || enter) && valid {
                let n = name.trim();
                let to = if n.starts_with('/') { n.to_string() } else { join(&parent(from), n) };
                op = Some(FsOp::Rename(from.clone(), to));
                close = true;
            }
            close |= cancel;
        });
        close |= esc;
    }
    if close {
        app.files.rename = None;
    }

    // chmod
    let mut close = false;
    if let Some((path, mode)) = app.files.chmod.as_mut() {
        let (_, esc) = modal(ctx, "fs-chmod", t!("Změnit práva", "Change permissions"), 420.0, |ui| {
            ui.label(RichText::new(path.as_str()).monospace());
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.add(field(mode).desired_width(80.0).font(egui::TextStyle::Monospace));
                for m in ["644", "600", "755", "700", "+x"] {
                    if ui.button(m).clicked() {
                        *mode = m.to_string();
                    }
                }
            });
            let valid = !mode.trim().is_empty();
            let (ok, cancel) = dialog_buttons(ui, t!("Nastavit", "Apply"), false, valid);
            if ok && valid {
                op = Some(FsOp::Chmod(mode.trim().to_string(), path.clone()));
                close = true;
            }
            close |= cancel;
        });
        close |= esc;
    }
    if close {
        app.files.chmod = None;
    }

    // editor
    let mut open = app.files.editor.is_some();
    let mut save = None;
    if let Some(ed) = app.files.editor.as_mut() {
        let dirty = ed.text != ed.orig;
        egui::Window::new(format!("\u{E34C} {}{}", ed.path, if dirty { " •" } else { "" }))
            .id(egui::Id::new("file-editor"))
            .open(&mut open)
            .default_size([820.0, 560.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let ctrl_s = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S));
                    ui.add_enabled_ui(dirty, |ui| {
                        if accent_button(ui, t!("\u{E248} Uložit (Ctrl+S)", "\u{E248} Save (Ctrl+S)")).clicked() || (ctrl_s && dirty) {
                            save = Some(FsOp::Write(ed.path.clone(), ed.text.clone()));
                            ed.orig = ed.text.clone();
                        }
                    });
                    if dirty && ui.button(t!("\u{E038} Zahodit změny", "\u{E038} Discard changes")).clicked() {
                        ed.text = ed.orig.clone();
                    }
                    ui.label(muted(tf!("{} řádků{}", "{} lines{}", ed.text.lines().count(), if root { " · root" } else { "" })).small());
                });
                ui.separator();
                egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                    ui.add(area(&mut ed.text).code_editor().desired_width(f32::INFINITY).desired_rows(30));
                });
            });
    }
    if let Some(o) = save {
        op = Some(o);
    }
    if !open {
        app.files.editor = None;
    }
    if let Some(o) = op {
        fs(app, o);
    }
}
