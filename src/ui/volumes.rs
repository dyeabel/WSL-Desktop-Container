use super::*;
use crate::app::App;
use crate::worker::{backup_file_name, valid_volume_name, Cmd};
use std::collections::HashMap;

/// ikony (Phosphor): ARCHIVE, CLOCK_COUNTER_CLOCKWISE, COPY, FOLDER_OPEN, WARNING
const BACKUP: &str = "\u{E00C}";
const RESTORE: &str = "\u{E1A0}";
const CLONE: &str = "\u{E1CA}";
const PICK: &str = "\u{E256}";
const WARN: &str = "\u{E4E0}";
/// velikosti se obnovují nejvýš jednou za minutu (u Dockeru může běžet `du`)
const SIZES_EVERY: f64 = 60.0;

/// Dotaz před zálohou volume, který používají běžící kontejnery.
pub struct BackupAsk {
    pub volume: String,
    pub users: Vec<String>,
    pub stop: bool,
}

/// Dialog „Obnovit ze zálohy“.
pub struct RestoreDlg {
    pub file: String,
    /// true = nový volume (`name`), false = existující (`existing`)
    pub new: bool,
    pub existing: String,
    pub name: String,
    pub clear: bool,
}

impl Default for RestoreDlg {
    fn default() -> Self {
        Self { file: String::new(), new: false, existing: String::new(), name: String::new(), clear: true }
    }
}

#[derive(Default)]
pub struct VolState {
    /// velikost podle názvu volume (chybí = neznámá)
    pub sizes: HashMap<String, u64>,
    sizes_at: Option<f64>,
    /// názvy volume při posledním dotazu na velikosti (nový volume = dotaz hned)
    sizes_key: String,
    pub backup: Option<BackupAsk>,
    pub restore: Option<RestoreDlg>,
    /// klon: (zdroj, nový název)
    pub clone: Option<(String, String)>,
    /// naposledy použitá složka se zálohami
    last_dir: Option<std::path::PathBuf>,
}

impl VolState {
    /// Stav po startu; `WLS_DEBUG_RESTORE=1` otevře dialog obnovy (pro screenshoty).
    pub fn new() -> Self {
        Self { restore: std::env::var("WLS_DEBUG_RESTORE").ok().map(|_| RestoreDlg::default()), ..Default::default() }
    }
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    header(ui, "Volume", &tf!("{} volume", "{} volumes", app.snap.volumes.len()));
    if !need_engine(app, ui) {
        return;
    }
    request_sizes(app, ui.ctx());
    let mut prune = false;
    toolbar_lr(
        ui,
        240.0,
        |ui, right| if !right {
            ui.add(field(&mut app.new_volume).hint_text(t!("název nového volume", "new volume name")).desired_width(220.0));
            if accent_button(ui, t!("\u{E3D4} Vytvořit", "\u{E3D4} Create")).clicked() && !app.new_volume.trim().is_empty() {
                app.send(Cmd::CreateVolume(app.new_volume.trim().to_string()));
                app.new_volume.clear();
            }
        } else {
            more_menu(ui, |ui| {
                if ui.button(tf!("{RESTORE} Obnovit ze zálohy…", "{RESTORE} Restore from backup…")).clicked() {
                    app.vol.restore = Some(RestoreDlg::default());
                    ui.close_menu();
                }
                ui.separator();
                if ui.button(t!("\u{EC54} Odstranit nepoužívané volume…", "\u{EC54} Remove unused volumes…")).clicked() {
                    prune = true;
                    ui.close_menu();
                }
            });
            filter_field(ui, &mut app.filter);
        },
    );
    if prune {
        app.ask(
            t!("Odstranit nepoužívané volume?", "Remove unused volumes?"),
            t!("Data v nepoužívaných volume budou nenávratně smazána!", "Data in unused volumes will be permanently deleted!"),
            Cmd::PruneVolumes,
        );
    }
    ui.add_space(8.0);

    let f = app.filter.to_lowercase();
    let mut volumes = app.snap.volumes.clone();
    let containers = app.snap.containers.clone();
    let sizes = app.vol.sizes.clone();
    sort_items(ui.ctx(), "volumes", &mut volumes, |v, col| match col {
        1 => SortKey::text(&v.driver),
        2 => SortKey::num(sizes.get(&v.name).map(|s| *s as f64).unwrap_or(-1.0)),
        3 => SortKey::num(
            containers.iter().filter(|c| c.mounts.as_ref().is_some_and(|m| m.iter().any(|m| m.name.as_deref() == Some(v.name.as_str())))).count() as f64,
        ),
        4 => SortKey::text(&v.mountpoint),
        _ => SortKey::text(&v.name),
    });
    let cols = [
        (t!("NÁZEV", "NAME"), W::Fr(2.5)),
        ("DRIVER", W::PxOpt(80.0)),
        (t!("VELIKOST", "SIZE"), W::PxOpt(80.0)),
        (t!("POUŽÍVÁ", "USED BY"), W::Fr(1.5)),
        ("MOUNTPOINT", W::FrOpt(3.0)),
        (t!("AKCE", "ACTIONS"), icons_w(4)),
    ];
    let mut browse = None;
    let mut run_with: Option<String> = None;
    let mut backup: Option<(String, Vec<String>)> = None;
    table_sort(ui, "volumes", &cols, Sort { key: "volumes", cols: &[0, 1, 2, 3, 4] }, |mut body| {
        for v in volumes.iter().filter(|v| f.is_empty() || v.name.to_lowercase().contains(&f)) {
            let users: Vec<&crate::docker::Container> = containers
                .iter()
                .filter(|c| c.mounts.iter().flatten().any(|m| m.name.as_deref() == Some(v.name.as_str())))
                .collect();
            let used: Vec<String> = users.iter().map(|c| c.name()).collect();
            let running: Vec<String> = users.iter().filter(|c| c.running()).map(|c| c.name()).collect();
            let project = v.labels.as_ref().and_then(|l| l.get("com.docker.compose.project")).cloned();
            let h = if project.is_some() { 44.0 } else { 34.0 };
            body.row(h, |mut r| {
                r.col(|ui| match &project {
                    Some(p) => two_line(ui, RichText::new(&v.name), muted(format!("\u{E466} {p}")).small()),
                    None => {
                        ui.label(&v.name);
                    }
                });
                r.col(|ui| {
                    ui.label(&v.driver);
                });
                r.col(|ui| match app.vol.sizes.get(&v.name) {
                    Some(b) => {
                        ui.label(fmt_bytes(*b));
                    }
                    None => {
                        ui.label(muted("—"));
                    }
                });
                r.col(|ui| {
                    if used.is_empty() {
                        ui.label(muted(t!("nepoužívá se", "unused")));
                    } else {
                        ui.label(used.join(", "));
                    }
                });
                r.col(|ui| {
                    ui.label(RichText::new(&v.mountpoint).monospace().small());
                });
                r.col(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if icon(ui, "\u{E3D0}", t!("Spustit kontejner s tímto volume (lze připojit do více kontejnerů najednou)", "Run a container with this volume (can be attached to several containers at once)")) {
                        run_with = Some(v.name.clone());
                    }
                    if icon(ui, "\u{E24A}", t!("Procházet obsah (stránka Soubory, jako root)", "Browse contents (Files page, as root)")) {
                        browse = Some(v.mountpoint.clone());
                    }
                    if icon(ui, BACKUP, t!("Zálohovat do souboru .tar.gz", "Back up to a .tar.gz file")) {
                        backup = Some((v.name.clone(), running.clone()));
                    }
                    row_menu(ui, |ui| {
                        if ui.button(tf!("{BACKUP} Zálohovat…", "{BACKUP} Back up…")).clicked() {
                            backup = Some((v.name.clone(), running.clone()));
                            ui.close_menu();
                        }
                        if ui.button(tf!("{RESTORE} Obnovit ze zálohy…", "{RESTORE} Restore from backup…")).clicked() {
                            app.vol.restore = Some(RestoreDlg { existing: v.name.clone(), ..Default::default() });
                            ui.close_menu();
                        }
                        if ui.button(tf!("{CLONE} Klonovat…", "{CLONE} Clone…")).clicked() {
                            app.vol.clone = Some((v.name.clone(), format!("{}-copy", v.name)));
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button(RichText::new(t!("\u{E4A6} Odstranit…", "\u{E4A6} Remove…")).color(theme::red())).clicked() {
                            app.ask(tf!("Odstranit volume {}?", "Remove volume {}?", v.name), t!("Data ve volume budou nenávratně smazána!", "Data in the volume will be permanently deleted!"), Cmd::RemoveVolume(v.name.clone()));
                            ui.close_menu();
                        }
                    });
                });
            });
        }
    });
    if let Some(name) = run_with {
        // předvyplnit dialog: volume připojený na /data, image doplní uživatel
        app.run = crate::worker::RunSpec { restart: "no".into(), volumes: format!("{name}:/data"), ..Default::default() };
        app.run_vol = (String::new(), "/data".into(), false);
        app.run_open = true;
    }
    if let Some(p) = browse {
        app.files.root = true;
        super::files::navigate(app, p);
        app.page = crate::app::Page::Files;
    }
    if let Some((volume, users)) = backup {
        if users.is_empty() {
            pick_and_backup(app, volume, false);
        } else {
            app.vol.backup = Some(BackupAsk { volume, users, stop: false });
        }
    }
}

/// Požádá worker o velikosti (při změně seznamu volume, jinak nejvýš jednou za minutu).
fn request_sizes(app: &mut App, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    let key = app.snap.volumes.iter().map(|v| v.name.as_str()).collect::<Vec<_>>().join("\n");
    if app.vol.sizes_key != key || app.vol.sizes_at.is_none_or(|t| now - t >= SIZES_EVERY) {
        app.vol.sizes_key = key;
        app.vol.sizes_at = Some(now);
        app.send(Cmd::VolumeSizes);
    }
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(SIZES_EVERY));
}

/// Místní čas (rok, měsíc, den, hodina, minuta) pro název zálohy.
fn local_now() -> (u16, u16, u16, u16, u16) {
    #[repr(C)]
    #[derive(Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        millis: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(st: *mut SystemTime);
    }
    let mut st = SystemTime::default();
    // SAFETY: GetLocalTime jen vyplní předanou strukturu SYSTEMTIME
    unsafe { GetLocalTime(&mut st) };
    (st.year, st.month, st.day, st.hour, st.minute)
}

/// Doplní příponu .tar.gz, pokud ji uživatel v dialogu nezadal.
fn ensure_tar_gz(path: &str) -> String {
    let l = path.to_ascii_lowercase();
    if l.ends_with(".tar.gz") || l.ends_with(".tgz") {
        path.to_string()
    } else if l.ends_with(".tar") {
        format!("{path}.gz")
    } else {
        format!("{path}.tar.gz")
    }
}

/// Dialog pro uložení zálohy a spuštění zálohy na pozadí.
fn pick_and_backup(app: &mut App, volume: String, stop: bool) {
    let mut d = rfd::FileDialog::new()
        .set_title(tf!("Zálohovat volume {volume}", "Back up volume {volume}"))
        .set_file_name(backup_file_name(&volume, local_now()))
        .add_filter("tar.gz", &["gz", "tgz"]);
    if let Some(dir) = &app.vol.last_dir {
        d = d.set_directory(dir);
    }
    if let Some(p) = d.save_file() {
        app.vol.last_dir = p.parent().map(|d| d.to_path_buf());
        let target = ensure_tar_gz(&p.to_string_lossy());
        app.send(Cmd::BackupVolume { volume, target, stop });
    }
}

fn pick_button(ui: &mut egui::Ui, glyph: &str, tip: &str) -> bool {
    ui.add(egui::Button::new(RichText::new(glyph).size(16.0)).min_size(egui::vec2(theme::CONTROL_H, theme::CONTROL_H)))
        .on_hover_text(tip)
        .clicked()
}

fn warn(ui: &mut egui::Ui, text: String) {
    ui.add(egui::Label::new(RichText::new(format!("{WARN} {text}")).color(theme::yellow()).small()).wrap());
}

/// Dialogy stránky (volá se každý snímek).
pub fn dialogs(app: &mut App, ctx: &egui::Context) {
    backup_dialog(app, ctx);
    restore_dialog(app, ctx);
    clone_dialog(app, ctx);
}

fn backup_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(b) = app.vol.backup.as_mut() else { return };
    let (mut ok, mut cancel) = (false, false);
    let (_, esc) = modal(ctx, "vol-backup", &tf!("{BACKUP} Zálohovat {}", "{BACKUP} Back up {}", b.volume), 440.0, |ui| {
        warn(
            ui,
            tf!(
                "Volume používají běžící kontejnery: {}. Záloha za běhu nemusí být konzistentní.",
                "Running containers use this volume: {}. A backup taken while they run may be inconsistent.",
                b.users.join(", ")
            ),
        );
        ui.add_space(8.0);
        ui.checkbox(&mut b.stop, t!("Zastavit je během zálohy", "Stop them during backup"));
        (ok, cancel) = dialog_buttons(ui, t!("Vybrat soubor…", "Choose file…"), false, true);
    });
    if ok {
        if let Some(b) = app.vol.backup.take() {
            pick_and_backup(app, b.volume, b.stop);
        }
    } else if cancel || esc {
        app.vol.backup = None;
    }
}

fn restore_dialog(app: &mut App, ctx: &egui::Context) {
    if app.vol.restore.is_none() {
        return;
    }
    let names: Vec<String> = app.snap.volumes.iter().map(|v| v.name.clone()).collect();
    let containers = app.snap.containers.clone();
    let last_dir = app.vol.last_dir.clone();
    let mut picked_dir = None;
    let Some(r) = app.vol.restore.as_mut() else { return };
    if !r.new && r.existing.is_empty() {
        match names.first() {
            Some(n) => r.existing = n.clone(),
            None => r.new = true,
        }
    }
    let (mut ok, mut cancel) = (false, false);
    let (_, esc) = modal(ctx, "vol-restore", &tf!("{RESTORE} Obnovit ze zálohy", "{RESTORE} Restore from backup"), 520.0, |ui| {
        let w = 360.0;
        egui::Grid::new("vol-restore-form").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
            ui.label(t!("Záloha", "Backup"));
            ui.horizontal(|ui| {
                ui.add(field(&mut r.file).hint_text(r"C:\zalohy\data.tar.gz").desired_width(w - 38.0));
                if pick_button(ui, PICK, t!("Vybrat soubor…", "Pick a file…")) {
                    let mut d = rfd::FileDialog::new()
                        .set_title(t!("Záloha volume", "Volume backup"))
                        .add_filter(t!("Archiv tar", "Tar archive"), &["gz", "tgz", "tar"]);
                    if let Some(dir) = &last_dir {
                        d = d.set_directory(dir);
                    }
                    if let Some(p) = d.pick_file() {
                        picked_dir = p.parent().map(|d| d.to_path_buf());
                        r.file = p.to_string_lossy().to_string();
                    }
                }
            });
            ui.end_row();

            ui.label(t!("Cílový volume", "Target volume"));
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.add_enabled_ui(!names.is_empty(), |ui| {
                        ui.radio_value(&mut r.new, false, t!("Existující", "Existing"));
                        egui::ComboBox::from_id_salt("vol-restore-existing")
                            .width(220.0)
                            .selected_text(r.existing.as_str())
                            .show_ui(ui, |ui| {
                                for n in &names {
                                    if ui.selectable_label(r.existing == *n, n).clicked() {
                                        r.existing = n.clone();
                                        r.new = false;
                                    }
                                }
                            });
                    });
                });
                ui.horizontal(|ui| {
                    ui.radio_value(&mut r.new, true, t!("Nový", "New"));
                    let resp = ui.add(field(&mut r.name).hint_text(t!("název nového volume", "new volume name")).desired_width(220.0));
                    if resp.gained_focus() {
                        r.new = true;
                    }
                });
            });
            ui.end_row();

            ui.label("");
            ui.add_enabled_ui(!r.new, |ui| {
                ui.checkbox(&mut r.clear, t!("Před obnovou volume vyprázdnit", "Clear the volume before restoring"));
            });
            ui.end_row();
        });

        ui.add_space(6.0);
        let name = r.name.trim();
        let mut valid = !r.file.trim().is_empty();
        if r.new {
            if !name.is_empty() && !valid_volume_name(name) {
                warn(ui, t!("Název smí obsahovat písmena, číslice, _ . -", "The name may contain letters, digits, _ . -").into());
            } else if names.iter().any(|n| n == name) {
                warn(ui, tf!("Volume {name} už existuje.", "Volume {name} already exists."));
            }
            valid &= valid_volume_name(name) && !names.iter().any(|n| n == name);
        } else {
            valid &= !r.existing.is_empty();
            warn(
                ui,
                if r.clear {
                    tf!("Současný obsah volume {} bude smazán a nahrazen zálohou.", "The current content of volume {} will be deleted and replaced by the backup.", r.existing)
                } else {
                    tf!("Soubory ve volume {} se stejným názvem budou přepsány.", "Files in volume {} with the same name will be overwritten.", r.existing)
                },
            );
            let running: Vec<String> = containers
                .iter()
                .filter(|c| c.running() && c.mounts.iter().flatten().any(|m| m.name.as_deref() == Some(r.existing.as_str())))
                .map(|c| c.name())
                .collect();
            if !running.is_empty() {
                warn(ui, tf!("Volume právě používají: {}.", "Currently used by: {}.", running.join(", ")));
            }
        }
        (ok, cancel) = dialog_buttons(ui, &tf!("{RESTORE} Obnovit…", "{RESTORE} Restore…"), true, valid);
    });
    if picked_dir.is_some() {
        app.vol.last_dir = picked_dir;
    }
    if ok {
        if let Some(r) = app.vol.restore.take() {
            let volume = if r.new { r.name.trim().to_string() } else { r.existing.clone() };
            let file = r.file.trim().trim_matches('"').to_string();
            let text = if r.new {
                tf!("Vytvoří se volume {volume} s obsahem zálohy {file}.", "Volume {volume} will be created with the content of backup {file}.")
            } else if r.clear {
                tf!("Obsah volume {volume} bude nenávratně smazán a nahrazen zálohou {file}!", "The content of volume {volume} will be permanently deleted and replaced by backup {file}!")
            } else {
                tf!("Soubory ze zálohy {file} přepíšou soubory ve volume {volume}!", "Files from backup {file} will overwrite files in volume {volume}!")
            };
            app.ask(
                tf!("Obnovit volume {volume}?", "Restore volume {volume}?"),
                text,
                Cmd::RestoreVolume { source: file, volume, create: r.new, clear: !r.new && r.clear },
            );
        }
    } else if cancel || esc {
        app.vol.restore = None;
    }
}

fn clone_dialog(app: &mut App, ctx: &egui::Context) {
    let names: Vec<String> = app.snap.volumes.iter().map(|v| v.name.clone()).collect();
    let Some((from, to)) = app.vol.clone.as_mut() else { return };
    let (mut ok, mut cancel) = (false, false);
    let (_, esc) = modal(ctx, "vol-clone", &tf!("{CLONE} Klonovat {}", "{CLONE} Clone {}", from), 420.0, |ui| {
        ui.label(t!("Název nového volume", "New volume name"));
        ui.add(field(to).desired_width(ui.available_width()));
        let n = to.trim();
        let exists = names.iter().any(|x| x == n);
        if !n.is_empty() && !valid_volume_name(n) {
            ui.add_space(4.0);
            warn(ui, t!("Název smí obsahovat písmena, číslice, _ . -", "The name may contain letters, digits, _ . -").into());
        } else if exists {
            ui.add_space(4.0);
            warn(ui, tf!("Volume {n} už existuje.", "Volume {n} already exists."));
        }
        (ok, cancel) = dialog_buttons(ui, &tf!("{CLONE} Klonovat", "{CLONE} Clone"), false, valid_volume_name(n) && !exists);
    });
    if ok {
        if let Some((from, to)) = app.vol.clone.take() {
            app.send(Cmd::CloneVolume { from, to: to.trim().to_string() });
        }
    } else if cancel || esc {
        app.vol.clone = None;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tar_gz_suffix() {
        assert_eq!(super::ensure_tar_gz(r"C:\x\a b.tar.gz"), r"C:\x\a b.tar.gz");
        assert_eq!(super::ensure_tar_gz(r"C:\x\a.TGZ"), r"C:\x\a.TGZ");
        assert_eq!(super::ensure_tar_gz(r"C:\x\a.tar"), r"C:\x\a.tar.gz");
        assert_eq!(super::ensure_tar_gz(r"C:\x\a"), r"C:\x\a.tar.gz");
    }

    #[test]
    fn local_time_is_sane() {
        let (y, mo, d, h, mi) = super::local_now();
        assert!(y >= 2024 && (1..=12).contains(&mo) && (1..=31).contains(&d) && h < 24 && mi < 60);
    }
}
