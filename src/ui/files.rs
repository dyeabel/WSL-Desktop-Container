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
    header(ui, "Soubory", &format!("Souborový systém distribuce {distro}"));
    if !app.files.loaded && !app.files.loading {
        reload(app);
    }

    // --- navigace ---
    // --- navigace ---
    ui.horizontal(|ui| {
        let cur = app.files.path.clone();
        if icon(ui, "\u{E08E}", "O úroveň výš") {
            navigate(app, parent(&cur));
        }
        if icon(ui, "\u{E2C2}", "Domovská složka") {
            navigate(app, "~".into());
        }
        if app.files.loading {
            // při načítání místo ikony obnovení kolečko (stejná šířka)
            ui.add_sized([26.0, 24.0], egui::Spinner::new());
        } else if icon(ui, "\u{E094}", "Obnovit") {
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
            ("\u{E2C2} Domů".into(), "~".into()),
            ("/ Kořen".into(), "/".into()),
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
        egui::ComboBox::from_id_salt("marks").selected_text("\u{E46A} Záložky").width(150.0).show_ui(ui, |ui| {
            for (l, p) in &marks {
                if ui.selectable_label(false, l).clicked() {
                    go = Some(p.clone());
                }
            }
        });
        if let Some(p) = go {
            if p.starts_with("/var/lib/docker") && !app.files.root {
                app.files.root = true;
                app.toast(true, "Docker data patří rootovi – přepnuto na režim root");
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
            ui.menu_button("\u{E3D4} Nový \u{E136}", |ui| {
                if ui.button("\u{E258} Složka…").clicked() {
                    app.files.create = Some((true, String::new()));
                    ui.close_menu();
                }
                if ui.button("\u{E236} Soubor…").clicked() {
                    app.files.create = Some((false, String::new()));
                    ui.close_menu();
                }
            });
            ui.menu_button("\u{E4C0} Nahrát z Windows \u{E136}", |ui| {
                if ui.button("Soubory…").on_hover_text("Zkopírovat soubory z Windows do této složky").clicked() {
                    upload_files = true;
                    ui.close_menu();
                }
                if ui.button("Složku…").clicked() {
                    upload_dir = true;
                    ui.close_menu();
                }
            });
        } else {
            more_menu(ui, |ui| {
                if ui.button("\u{E256} Otevřít v Průzkumníku").clicked() {
                    app.send(Cmd::Explorer(cur.clone()));
                    ui.close_menu();
                }
                if ui.button("\u{EAE8} Terminál v této složce").clicked() {
                    app.send(Cmd::Terminal(Term::Dir(cur.clone())));
                    ui.close_menu();
                }
                if ui.button("\u{E1CA} Kopírovat cestu").clicked() {
                    ui.ctx().copy_text(cur.clone());
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("\u{E184} Vybrat vše").clicked() {
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
                if ui.checkbox(&mut app.files.root, "Pracovat jako root").on_hover_text("Operace poběží pod uživatelem root (wsl -u root)").changed() {
                    reload(app);
                    ui.close_menu();
                }
            });
            filter_field(ui, &mut app.files.filter);
            if app.files.root && badge(ui, "root", theme::YELLOW).on_hover_text("Pracuješ jako root – kliknutím vypneš").interact(egui::Sense::click()).clicked() {
                app.files.root = false;
                reload(app);
            }
        }
    });
    if upload_files {
        if let Some(files) = rfd::FileDialog::new().set_title("Nahrát do WSL").pick_files() {
            let v = files.into_iter().map(|p| p.display().to_string()).collect();
            fs(app, FsOp::Upload(v, cur_path.clone()));
        }
    }
    if upload_dir {
        if let Some(dir) = rfd::FileDialog::new().set_title("Nahrát složku do WSL").pick_folder() {
            fs(app, FsOp::Upload(vec![dir.display().to_string()], cur_path.clone()));
        }
    }

    // --- výběr (lišta se ukáže jen když je něco vybráno) ---
    let cur = app.files.path.clone();
    let f = app.files.filter.to_lowercase();
    let entries: Vec<Entry> = app
        .files
        .entries
        .iter()
        .filter(|e| f.is_empty() || e.name.to_lowercase().contains(&f))
        .cloned()
        .collect();
    let nsel = app.files.selected.len();
    if nsel > 0 {
        ui.add_space(4.0);
        egui::Frame::new()
            .fill(theme::ACCENT.gamma_multiply(0.12))
            .stroke(egui::Stroke::new(1.0_f32, theme::ACCENT.gamma_multiply(0.4)))
            .corner_radius(6)
            .inner_margin(egui::Margin::symmetric(10, 4))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                toolbar(ui, |ui| {
                    ui.label(RichText::new(format!("Vybráno: {nsel}")).strong());
                    gap(ui);
                    if ui.button("\u{E20C} Stáhnout do Windows…").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().set_title("Uložit do…").pick_folder() {
                            let items: Vec<String> = app.files.selected.iter().map(|n| join(&cur, n)).collect();
                            fs(app, FsOp::Download(items, dir.display().to_string()));
                        }
                    }
                    if ui.button(RichText::new("\u{E4A6} Smazat…").color(theme::RED)).clicked() {
                        let items: Vec<String> = app.files.selected.iter().map(|n| join(&cur, n)).collect();
                        let list = app.files.selected.iter().take(8).cloned().collect::<Vec<_>>().join(", ");
                        app.ask(
                            format!("Smazat {nsel} položek?"),
                            format!("Nenávratně se smaže (rm -rf): {list}{}", if nsel > 8 { ", …" } else { "" }),
                            Cmd::Fs { root: app.files.root, op: FsOp::Delete(items) },
                        );
                    }
                    if ui.button("Zrušit výběr").clicked() {
                        app.files.selected.clear();
                    }
                });
            });
    }
    ui.add_space(6.0);

    if let Some(e) = app.files.error.clone() {
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("\u{E4E0} {e}")).color(theme::RED));
            if !app.files.root {
                ui.label(muted("Nedostatečná práva? Zapni „Jako root“."));
            }
        });
        ui.add_space(6.0);
    }

    // --- výpis ---
    let mut go = None;
    let cols = [
        ("", W::Px(22.0)),
        ("NÁZEV", W::Fr(1.0)),
        ("VELIKOST", W::Px(76.0)),
        ("ZMĚNĚNO", W::PxOpt(96.0)),
        ("VLASTNÍK", W::PxOpt(78.0)),
        ("PRÁVA", W::PxOpt(48.0)),
        ("AKCE", icons_w(2)),
    ];
    table(ui, "files", &cols, |mut body| {
        if cur != "/" {
            body.row(30.0, |mut r| {
                r.col(|_| {});
                r.col(|ui| {
                    let l = ui.add(egui::Label::new(RichText::new("\u{E24A} ..").strong()).selectable(false).sense(egui::Sense::click()));
                    if l.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("O úroveň výš").clicked() {
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
                        if l.on_hover_text("Dvojklik = upravit").double_clicked() {
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
                    } else if icon(ui, "\u{E34C}", "Upravit ve vestavěném editoru") {
                        fs(app, FsOp::Read(path.clone()));
                    }
                    // ostatní akce v nabídce řádku
                    let mut download = false;
                    ui.menu_button(RichText::new("\u{E1FE}").size(15.0), |ui| {
                        ui.set_min_width(200.0);
                        if !e.is_dir() && ui.button("\u{E23A} Otevřít v Notepadu").clicked() {
                            app.send(Cmd::Notepad(path.clone()));
                            ui.close_menu();
                        }
                        if ui.button("\u{E3B4} Přejmenovat / přesunout…").clicked() {
                            app.files.rename = Some((path.clone(), e.name.clone()));
                            ui.close_menu();
                        }
                        if ui.button("\u{E2D6} Změnit práva…").clicked() {
                            app.files.chmod = Some((path.clone(), e.mode.clone()));
                            ui.close_menu();
                        }
                        if ui.button("\u{E20C} Stáhnout do Windows…").clicked() {
                            download = true;
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button(RichText::new("\u{E4A6} Smazat…").color(theme::RED)).clicked() {
                            app.ask(
                                format!("Smazat {}?", e.name),
                                format!("{} bude nenávratně smazán{}.", path, if e.is_dir() { "a včetně obsahu" } else { "" }),
                                Cmd::Fs { root: app.files.root, op: FsOp::Delete(vec![path.clone()]) },
                            );
                            ui.close_menu();
                        }
                    });
                    if download {
                        if let Some(dir) = rfd::FileDialog::new().set_title("Uložit do…").pick_folder() {
                            fs(app, FsOp::Download(vec![path.clone()], dir.display().to_string()));
                        }
                    }
                });
            });
        }
    });
    if entries.is_empty() && app.files.loaded && app.files.error.is_none() {
        ui.add_space(6.0);
        ui.label(muted("Prázdná složka"));
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
        let title = if *is_dir { "Nová složka" } else { "Nový soubor" };
        let (_, esc) = modal(ctx, "fs-create", title, 420.0, |ui| {
            ui.label(muted("Ve složce"));
            ui.label(RichText::new(&cur).monospace());
            ui.add_space(8.0);
            let r = ui.add(field(name).hint_text("název").desired_width(f32::INFINITY));
            if !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let valid = !name.trim().is_empty() && !name.contains('/');
            let (ok, cancel) = dialog_buttons(ui, "Vytvořit", false, valid);
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
        let (_, esc) = modal(ctx, "fs-rename", "Přejmenovat / přesunout", 460.0, |ui| {
            ui.label(RichText::new(from.as_str()).monospace());
            ui.add_space(8.0);
            let r = ui.add(field(name).desired_width(f32::INFINITY));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.label(muted("Nový název, nebo celá cesta začínající / pro přesun.").small());
            let valid = !name.trim().is_empty();
            let (ok, cancel) = dialog_buttons(ui, "Přejmenovat", false, valid);
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
        let (_, esc) = modal(ctx, "fs-chmod", "Změnit práva", 420.0, |ui| {
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
            let (ok, cancel) = dialog_buttons(ui, "Nastavit", false, valid);
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
                        if accent_button(ui, "\u{E248} Uložit (Ctrl+S)").clicked() || (ctrl_s && dirty) {
                            save = Some(FsOp::Write(ed.path.clone(), ed.text.clone()));
                            ed.orig = ed.text.clone();
                        }
                    });
                    if dirty && ui.button("\u{E038} Zahodit změny").clicked() {
                        ed.text = ed.orig.clone();
                    }
                    ui.label(muted(format!("{} řádků{}", ed.text.lines().count(), if root { " · root" } else { "" })).small());
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
