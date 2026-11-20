//! Správa WSL distribucí: dialogy Odebrat, Export, Import a Disk (VHDX).

use super::*;
use crate::app::App;
use crate::worker::{Cmd, DistroDisk, DistroOp};
use crate::wsl::ExportFormat;

pub struct RemoveDlg {
    pub name: String,
    pub typed: String,
    pub backup: bool,
    pub file: String,
}

pub struct ExportDlg {
    pub name: String,
    pub format: ExportFormat,
    pub file: String,
}

#[derive(Default)]
pub struct ImportDlg {
    pub name: String,
    pub folder: String,
    pub file: String,
}

pub struct DiskDlg {
    pub name: String,
    /// None = načítá se
    pub info: Option<Result<DistroDisk, String>>,
}

/// Stav dialogů správy distribucí.
#[derive(Default)]
pub struct DistroDialogs {
    pub remove: Option<RemoveDlg>,
    pub export: Option<ExportDlg>,
    pub import: Option<ImportDlg>,
    pub disk: Option<DiskDlg>,
}

impl DistroDialogs {
    /// Pro ladění/screenshoty: WSLCD_DEBUG_REMOVE_DISTRO=<název> otevře dialog odebrání,
    /// WSLCD_DEBUG_IMPORT=1 dialog importu.
    pub fn from_env() -> Self {
        Self {
            remove: std::env::var("WSLCD_DEBUG_REMOVE_DISTRO").ok().filter(|n| !n.is_empty()).map(|n| remove_dlg(&n)),
            import: std::env::var("WSLCD_DEBUG_IMPORT").ok().map(|_| ImportDlg::default()),
            ..Default::default()
        }
    }
}

/// Výchozí soubor zálohy / exportu: %USERPROFILE%\Downloads\<name>-<suffix>.<ext>.
fn default_file(name: &str, suffix: &str, ext: &str) -> String {
    let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:".into());
    format!(r"{home}\Downloads\{name}-{suffix}.{ext}")
}

fn remove_dlg(name: &str) -> RemoveDlg {
    RemoveDlg { name: name.to_string(), typed: String::new(), backup: true, file: default_file(name, "backup", "tar") }
}

pub fn open_remove(app: &mut App, name: &str) {
    app.distro_dlg.remove = Some(remove_dlg(name));
}

pub fn open_export(app: &mut App, name: &str) {
    app.distro_dlg.export = Some(ExportDlg { name: name.to_string(), format: ExportFormat::Tar, file: default_file(name, "export", "tar") });
}

pub fn open_import(app: &mut App) {
    app.distro_dlg.import = Some(ImportDlg::default());
}

pub fn open_disk(app: &mut App, name: &str) {
    app.distro_dlg.disk = Some(DiskDlg { name: name.to_string(), info: None });
    app.send(Cmd::Distro(DistroOp::DiskInfo(name.to_string())));
}

/// Výsledek zjištění disku z workeru.
pub fn on_disk(app: &mut App, name: String, r: Result<DistroDisk, String>) {
    if let Some(d) = app.distro_dlg.disk.as_mut().filter(|d| d.name == name) {
        d.info = Some(r);
    }
}

/// Nahradí příponu souboru podle formátu exportu.
fn with_ext(file: &str, format: ExportFormat) -> String {
    let p = std::path::Path::new(file);
    match p.extension() {
        Some(_) => p.with_extension(format.ext()).display().to_string(),
        None if file.is_empty() => String::new(),
        None => format!("{file}.{}", format.ext()),
    }
}

/// Je soubor podporovaný zdroj importu?
fn import_ext_ok(file: &str) -> bool {
    let f = file.trim().to_ascii_lowercase();
    [".tar", ".tar.gz", ".tgz", ".vhdx", ".vhd"].iter().any(|e| f.ends_with(e))
}

/// Problémy zadání importu (prázdné = lze importovat).
pub fn import_problems(name: &str, existing: &[String], file: &str) -> Vec<&'static str> {
    let mut v = Vec::new();
    let name = name.trim();
    if name.is_empty() {
        v.push(t!("Zadej název nové distribuce.", "Enter a name for the new distribution."));
    } else if !crate::wsl::valid_distro_name(name) {
        v.push(t!("Název smí obsahovat jen písmena, číslice, - _ .", "The name may contain only letters, digits, - _ ."));
    } else if existing.iter().any(|e| e.eq_ignore_ascii_case(name)) {
        v.push(t!("Distribuce s tímto názvem už existuje.", "A distribution with this name already exists."));
    }
    if file.trim().is_empty() {
        v.push(t!("Vyber soubor .tar nebo .vhdx.", "Choose a .tar or .vhdx file."));
    } else if !import_ext_ok(file) {
        v.push(t!("Podporované soubory: .tar, .tar.gz, .vhdx.", "Supported files: .tar, .tar.gz, .vhdx."));
    }
    v
}

/// Pole s cestou a tlačítkem pro výběr (dialog Windows).
fn path_row(ui: &mut egui::Ui, value: &mut String, hint: &str, pick: impl FnOnce() -> Option<std::path::PathBuf>) {
    ui.horizontal(|ui| {
        ui.add(field(value).hint_text(hint).desired_width(300.0));
        if ui.button(t!("\u{E256} Vybrat…", "\u{E256} Browse…")).clicked() {
            if let Some(p) = pick() {
                *value = p.display().to_string();
            }
        }
    });
}

fn split_path(file: &str) -> (Option<std::path::PathBuf>, String) {
    let p = std::path::Path::new(file);
    let dir = p.parent().filter(|d| d.is_dir()).map(|d| d.to_path_buf());
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    (dir, name)
}

/// Dialog Windows „Uložit jako“ pro export.
fn save_dialog(title: &str, current: &str, format: ExportFormat) -> Option<std::path::PathBuf> {
    let (dir, name) = split_path(current);
    let mut d = rfd::FileDialog::new().set_title(title).set_file_name(name);
    d = match format {
        ExportFormat::Tar => d.add_filter("tar", &["tar"]),
        ExportFormat::Vhd => d.add_filter("VHDX", &["vhdx"]),
    };
    if let Some(dir) = dir {
        d = d.set_directory(dir);
    }
    d.save_file()
}

/// Všechny dialogy správy distribucí (volá se každý snímek z App::dialogs).
pub fn dialogs(app: &mut App, ctx: &egui::Context) {
    remove_dialog(app, ctx);
    export_dialog(app, ctx);
    import_dialog(app, ctx);
    disk_dialog(app, ctx);
}

fn remove_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(mut d) = app.distro_dlg.remove.take() else { return };
    let engine_distro = app.distro();
    let is_engine = d.name.eq_ignore_ascii_case(&engine_distro);
    let mut submit = false;
    let mut cancel = false;
    let (_, esc) = modal(ctx, "remove-distro", &tf!("Odebrat distribuci {}?", "Remove distribution {}?", d.name), 480.0, |ui| {
        if is_engine {
            ui.label(
                RichText::new(tf!(
                    "\u{E4E0} {} je primární distribuce aplikace. Nejdřív nastav jako primární jinou distribuci (tabulka distribucí na stránce WSL engine).",
                    "\u{E4E0} {} is the app's primary distribution. First make another distribution primary (distribution list on the WSL engine page).",
                    d.name
                ))
                .color(theme::yellow()),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(t!("\u{EB02} Otevřít WSL engine", "\u{EB02} Open WSL engine")).clicked() {
                    app.page = crate::app::Page::Engine;
                    cancel = true;
                }
            });
        } else {
            ui.label(
                RichText::new(tf!(
                    "\u{E4E0} wsl --unregister nenávratně smaže všechna data v distribuci {} (soubory, uživatele, nainstalované programy, kontejnery).",
                    "\u{E4E0} wsl --unregister permanently deletes all data in the distribution {} (files, users, installed software, containers).",
                    d.name
                ))
                .color(theme::red()),
            );
            ui.add_space(8.0);
            ui.checkbox(&mut d.backup, t!("Nejdřív exportovat zálohu (.tar)", "Export a backup first (.tar)"));
            if d.backup {
                let cur = d.file.clone();
                path_row(ui, &mut d.file, t!("soubor .tar", ".tar file"), || save_dialog(t!("Záloha distribuce", "Distribution backup"), &cur, ExportFormat::Tar));
            }
            ui.add_space(8.0);
            ui.label(tf!("Pro potvrzení napiš název distribuce {}:", "To confirm, type the distribution name {}:", d.name));
            ui.add(field(&mut d.typed).hint_text(d.name.as_str()).desired_width(300.0));
        }
        let ready = !is_engine && d.typed.trim() == d.name && (!d.backup || !d.file.trim().is_empty());
        let (ok, c) = dialog_buttons(ui, t!("\u{E4A6} Odebrat", "\u{E4A6} Remove"), true, ready);
        submit = ok && ready;
        cancel |= c;
    });
    if submit {
        let backup = d.backup.then(|| d.file.trim().to_string());
        app.send(Cmd::Distro(DistroOp::Remove { name: d.name, backup }));
    } else if !(cancel || esc) {
        app.distro_dlg.remove = Some(d);
    }
}

fn export_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(mut d) = app.distro_dlg.export.take() else { return };
    let mut submit = false;
    let mut cancel = false;
    let (_, esc) = modal(ctx, "export-distro", &tf!("Exportovat {}", "Export {}", d.name), 480.0, |ui| {
        egui::Grid::new("export-distro-grid").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
            ui.label(t!("Formát", "Format"));
            ui.horizontal(|ui| {
                let before = d.format;
                ui.radio_value(&mut d.format, ExportFormat::Tar, "tar");
                ui.radio_value(&mut d.format, ExportFormat::Vhd, "vhdx");
                if d.format != before {
                    d.file = with_ext(&d.file, d.format);
                }
            });
            ui.end_row();
            ui.label(t!("Soubor", "File"));
            let (title, cur, fmt) = (t!("Export distribuce", "Export distribution"), d.file.clone(), d.format);
            path_row(ui, &mut d.file, t!("cílový soubor", "target file"), || save_dialog(title, &cur, fmt));
            ui.end_row();
        });
        ui.add_space(6.0);
        ui.label(
            muted(t!(
                "Export běží na pozadí a může trvat několik minut. tar = archiv souborů (lze importovat kamkoli), vhdx = kopie disku.",
                "The export runs in the background and may take several minutes. tar = file archive (importable anywhere), vhdx = copy of the disk."
            ))
            .small(),
        );
        let ok_enabled = !d.file.trim().is_empty();
        let (ok, c) = dialog_buttons(ui, t!("\u{EAF0} Exportovat", "\u{EAF0} Export"), false, ok_enabled);
        submit = ok && ok_enabled;
        cancel = c;
    });
    if submit {
        app.send(Cmd::Distro(DistroOp::Export { name: d.name, file: d.file.trim().to_string(), format: d.format }));
    } else if !(cancel || esc) {
        app.distro_dlg.export = Some(d);
    }
}

fn import_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(mut d) = app.distro_dlg.import.take() else { return };
    let existing: Vec<String> = app.distros.iter().map(|d| d.name.clone()).collect();
    let mut submit = false;
    let mut cancel = false;
    let (_, esc) = modal(ctx, "import-distro", t!("Importovat distribuci", "Import distribution"), 520.0, |ui| {
        egui::Grid::new("import-distro-grid").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
            ui.label(t!("Název", "Name"));
            ui.add(field(&mut d.name).hint_text("Debian-dev").desired_width(300.0));
            ui.end_row();
            ui.label(t!("Soubor", "File"));
            path_row(ui, &mut d.file, ".tar / .vhdx", || {
                rfd::FileDialog::new()
                    .set_title(t!("Soubor distribuce", "Distribution file"))
                    .add_filter(t!("Distribuce WSL", "WSL distribution"), &["tar", "gz", "tgz", "vhdx", "vhd"])
                    .pick_file()
            });
            ui.end_row();
            ui.label(t!("Složka", "Folder"));
            path_row(ui, &mut d.folder, t!("výchozí: %LOCALAPPDATA%\\wsl\\<název>", "default: %LOCALAPPDATA%\\wsl\\<name>"), || {
                rfd::FileDialog::new().set_title(t!("Složka pro disk distribuce", "Folder for the distribution disk")).pick_folder()
            });
            ui.end_row();
        });
        ui.add_space(8.0);
        let problems = import_problems(&d.name, &existing, &d.file);
        for p in &problems {
            ui.label(RichText::new(format!("\u{E4E0} {p}")).color(theme::yellow()).small());
        }
        let folder = if d.folder.trim().is_empty() { crate::worker::default_import_folder(d.name.trim()) } else { d.folder.trim().to_string() };
        ui.label(muted(tf!("Disk distribuce bude ve složce {folder}.", "The distribution disk will be stored in {folder}.")).small());
        let (ok, c) = dialog_buttons(ui, t!("\u{E010} Importovat", "\u{E010} Import"), false, problems.is_empty());
        submit = ok && problems.is_empty();
        cancel = c;
    });
    if submit {
        app.send(Cmd::Distro(DistroOp::Import { name: d.name.trim().to_string(), folder: d.folder.trim().to_string(), file: d.file.trim().to_string() }));
    } else if !(cancel || esc) {
        app.distro_dlg.import = Some(d);
    }
}

fn disk_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(mut d) = app.distro_dlg.disk.take() else { return };
    let is_engine = d.name.eq_ignore_ascii_case(&app.distro());
    let mut close = false;
    let mut cmd = None;
    let (_, esc) = modal(ctx, "distro-disk", &tf!("Disk distribuce {}", "Disk of distribution {}", d.name), 500.0, |ui| {
        match &d.info {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(muted(t!("zjišťuji velikost…", "checking size…")));
                });
            }
            Some(Err(e)) => {
                ui.label(RichText::new(format!("\u{E4E0} {e}")).color(theme::red()));
            }
            Some(Ok(info)) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(fmt_bytes(info.size)).size(20.0).strong());
                    if info.sparse {
                        badge(ui, "sparse", theme::green()).on_hover_text(t!("Zhuštěný disk – uvolněné místo se vrací automaticky", "Sparse disk – freed space is returned automatically"));
                    }
                });
                ui.label(muted(info.path.as_str()).small());
            }
        }
        ui.add_space(8.0);
        // podpora přepínačů je zjištěná workerem (wsl --help) – po načtení info je v cache
        let is_sparse = matches!(&d.info, Some(Ok(i)) if i.sparse);
        let (sparse, compact) = if d.info.is_some() { crate::wsl::manage_support() } else { (false, false) };
        let (sparse, compact) = (sparse && !is_sparse, compact && !is_sparse);
        if d.info.is_some() && !is_sparse && !sparse && !compact {
            ui.label(muted(t!("Tato verze WSL neumí wsl --manage --set-sparse ani --compact.", "This WSL version supports neither wsl --manage --set-sparse nor --compact.")));
        }
        ui.label(
            muted(if is_sparse {
                t!(
                    "Disk je zhuštěný: WSL uvolněné místo vrací Windows samo, komprimace není potřeba (a zhuštěný disk ani komprimovat nejde).",
                    "The disk is sparse: WSL returns freed space to Windows by itself, compacting is not needed (and a sparse disk cannot be compacted)."
                )
            } else {
                t!(
                    "Zhuštěný disk (sparse) vrací uvolněné místo Windows automaticky. Komprimace ho zmenší jednorázově. Obojí distribuci ukončí.",
                    "A sparse disk returns freed space to Windows automatically. Compacting shrinks it once. Both terminate the distribution."
                )
            })
            .small(),
        );
        if is_engine && (sparse || compact) {
            ui.label(RichText::new(t!("\u{E4E0} Zastaví se engine i všechny kontejnery.", "\u{E4E0} The engine and all containers will stop.")).color(theme::yellow()).small());
        }
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t!("Zavřít", "Close")).clicked() {
                    close = true;
                }
                if compact && ui.button(t!("\u{E09A} Zkomprimovat", "\u{E09A} Compact")).on_hover_text(format!("wsl --manage {} --compact", d.name)).clicked() {
                    cmd = Some(DistroOp::Compact(d.name.clone()));
                }
                if sparse && ui.button(t!("\u{E29E} Zapnout sparse", "\u{E29E} Enable sparse")).on_hover_text(format!("wsl --manage {} --set-sparse true", d.name)).clicked() {
                    cmd = Some(DistroOp::SetSparse(d.name.clone()));
                }
            });
        });
    });
    if let Some(op) = cmd {
        // po dokončení pošle worker nové informace o disku
        d.info = None;
        app.send(Cmd::Distro(op));
    }
    if !(close || esc) {
        app.distro_dlg.disk = Some(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_validation() {
        let ex = vec!["Debian".to_string()];
        assert!(import_problems("WSLCD-Test-Import", &ex, r"C:\a.tar").is_empty());
        assert!(import_problems("x", &ex, r"C:\a.TAR.GZ").is_empty());
        assert!(import_problems("x", &ex, r"C:\a.vhdx").is_empty());
        assert_eq!(import_problems("debian", &ex, r"C:\a.tar").len(), 1);
        assert_eq!(import_problems("a b", &ex, r"C:\a.tar").len(), 1);
        assert_eq!(import_problems("", &ex, "").len(), 2);
        assert_eq!(import_problems("x", &ex, r"C:\a.zip").len(), 1);
    }

    #[test]
    fn export_ext() {
        assert_eq!(with_ext(r"C:\x\Deb-export.tar", ExportFormat::Vhd), r"C:\x\Deb-export.vhdx");
        assert_eq!(with_ext(r"C:\x\Deb-export.vhdx", ExportFormat::Tar), r"C:\x\Deb-export.tar");
        assert_eq!(with_ext(r"C:\x\Deb", ExportFormat::Tar), r"C:\x\Deb.tar");
        assert_eq!(with_ext("", ExportFormat::Tar), "");
    }
}
