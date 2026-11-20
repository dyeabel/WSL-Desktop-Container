//! Sestavení image z Dockerfile: dialog s parametry a okno s průběžným výstupem.

use super::*;
use crate::app::App;
use crate::worker::{is_windows_path, BuildSpec, Cmd};
use std::sync::{Arc, Mutex};

/// ikona kladiva (Phosphor HAMMER)
pub const HAMMER: &str = "\u{E80E}";
/// maximální délka uchovávaného výstupu (znaky)
const LOG_MAX: usize = 2_000_000;

/// Okno s výstupem sestavení.
pub struct BuildLog {
    pub title: String,
    pub text: String,
    /// None = ještě běží
    pub result: Option<Result<String, String>>,
    /// okno je zobrazené (při běhu ho lze schovat a znovu otevřít)
    pub open: bool,
}

/// Stav sestavení – parametry se pamatují po dobu běhu aplikace.
pub struct BuildState {
    pub dialog: bool,
    pub spec: BuildSpec,
    pub running: bool,
    pub log: Option<BuildLog>,
    /// výsledek kontroly existence Dockerfile na pozadí: (cesta, existuje)
    df_check: Arc<Mutex<Option<(String, bool)>>>,
    /// cesta, pro kterou kontrola běží / naposledy běžela
    df_checked: String,
}

impl Default for BuildState {
    fn default() -> Self {
        Self {
            // pro ladění/screenshoty: WSLCD_DEBUG_BUILD=1 otevře dialog po startu
            dialog: std::env::var("WSLCD_DEBUG_BUILD").is_ok(),
            spec: BuildSpec::default(),
            running: false,
            log: None,
            df_check: Arc::new(Mutex::new(None)),
            df_checked: String::new(),
        }
    }
}

/// Windowsová cesta k Dockerfile pro kontrolu existence (None = nelze ověřit z Windows, např. linuxová cesta).
fn windows_dockerfile_path(spec: &BuildSpec) -> Option<std::path::PathBuf> {
    let df = spec.dockerfile.trim().trim_matches('"');
    if is_windows_path(df) {
        return Some(df.into());
    }
    let ctx = spec.context.trim().trim_matches('"');
    if !is_windows_path(ctx) || df.starts_with('/') {
        return None;
    }
    Some(std::path::Path::new(ctx).join(if df.is_empty() { "Dockerfile" } else { df }))
}

/// Spustí (jednou pro danou cestu) kontrolu existence Dockerfile mimo UI vlákno.
/// Vrací Some(false), když soubor určitě neexistuje.
fn dockerfile_exists(st: &mut BuildState, ctx: &egui::Context) -> Option<bool> {
    let path = windows_dockerfile_path(&st.spec)?;
    let key = path.to_string_lossy().to_string();
    if st.df_checked != key {
        st.df_checked = key.clone();
        let (res, ctx) = (st.df_check.clone(), ctx.clone());
        std::thread::spawn(move || {
            let ok = path.is_file();
            if let Ok(mut g) = res.lock() {
                *g = Some((key, ok));
            }
            ctx.request_repaint();
        });
        return None;
    }
    let g = st.df_check.lock().ok()?;
    match &*g {
        Some((k, ok)) if *k == key => Some(*ok),
        _ => None,
    }
}

fn pick_button(ui: &mut egui::Ui, glyph: &str, tip: &str) -> bool {
    ui.add(egui::Button::new(RichText::new(glyph).size(16.0)).min_size(egui::vec2(theme::CONTROL_H, theme::CONTROL_H)))
        .on_hover_text(tip)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// Tlačítko „Sestavit…“ do lišty stránky Image.
pub fn toolbar_button(app: &mut App, ui: &mut egui::Ui) {
    let running = app.build.running;
    let r = ui
        .add_enabled(!running, egui::Button::new(tf!("{HAMMER} Sestavit…", "{HAMMER} Build…")))
        .on_hover_text(t!("Sestavit image z Dockerfile", "Build an image from a Dockerfile"))
        .on_disabled_hover_text(t!("Sestavení už běží", "A build is already running"));
    if r.clicked() {
        app.build.dialog = true;
    }
    if running {
        ui.spinner();
        if app.build.log.as_ref().is_some_and(|l| !l.open) && ui.button(t!("Zobrazit průběh", "Show progress")).clicked() {
            if let Some(l) = app.build.log.as_mut() {
                l.open = true;
            }
        }
    }
}

/// Řádek výstupu z workeru.
pub fn on_output(app: &mut App, line: String) {
    if let Some(l) = app.build.log.as_mut() {
        l.text.push_str(&line);
        l.text.push('\n');
        if l.text.len() > LOG_MAX {
            let cut = l.text.len() - LOG_MAX * 3 / 4;
            let cut = (cut..l.text.len()).find(|i| l.text.is_char_boundary(*i)).unwrap_or(0);
            l.text.drain(..cut);
        }
    }
}

/// Konec sestavení (oznámení a obnovení seznamu dělá worker).
pub fn on_done(app: &mut App, res: Result<String, String>) {
    app.build.running = false;
    if let Some(l) = app.build.log.as_mut() {
        if let Err(e) = &res {
            l.text.push_str(&format!("\n{e}\n"));
        }
        l.result = Some(res);
    }
}

fn submit(app: &mut App) {
    let spec = app.build.spec.clone();
    let tags = spec.tag_list();
    let title = if tags.is_empty() { spec.context.trim().to_string() } else { tags.join(", ") };
    app.build.running = true;
    app.build.log = Some(BuildLog { title, text: String::new(), result: None, open: true });
    app.send(Cmd::Build(spec));
}

/// Dialog a okno výstupu (volá se každý snímek).
pub fn dialogs(app: &mut App, ctx: &egui::Context) {
    dialog(app, ctx);
    log_window(app, ctx);
}

fn dialog(app: &mut App, ctx: &egui::Context) {
    if !app.build.dialog {
        return;
    }
    let df_exists = dockerfile_exists(&mut app.build, ctx);
    let running = app.build.running;
    let mut ok = false;
    let mut cancel = false;
    let (_, esc) = modal(ctx, "build-dialog", &tf!("{HAMMER} Sestavit image", "{HAMMER} Build image"), 560.0, |ui| {
        let s = &mut app.build.spec;
        let w = 400.0;
        egui::Grid::new("buildform").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
            ui.label(t!("Kontext *", "Context *"));
            ui.horizontal(|ui| {
                ui.add(field(&mut s.context).hint_text(t!(r"C:\projekty\app nebo /home/user/app", r"C:\projects\app or /home/user/app")).desired_width(w - 38.0));
                if pick_button(ui, "\u{E256}", t!("Vybrat složku…", "Pick a folder…")) {
                    let mut d = rfd::FileDialog::new().set_title(t!("Složka kontextu sestavení", "Build context folder"));
                    if is_windows_path(&s.context) {
                        d = d.set_directory(s.context.trim());
                    }
                    if let Some(p) = d.pick_folder() {
                        s.context = p.to_string_lossy().to_string();
                    }
                }
            });
            ui.end_row();

            ui.label("Dockerfile");
            ui.horizontal(|ui| {
                ui.add(
                    field(&mut s.dockerfile)
                        .hint_text(t!("Dockerfile (relativně ke kontextu)", "Dockerfile (relative to context)"))
                        .desired_width(w - 38.0),
                );
                if pick_button(ui, "\u{E23A}", t!("Vybrat soubor…", "Pick a file…")) {
                    let mut d = rfd::FileDialog::new().set_title("Dockerfile");
                    let ctx_dir = s.context.trim().trim_matches('"').to_string();
                    if is_windows_path(&ctx_dir) {
                        d = d.set_directory(&ctx_dir);
                    }
                    if let Some(p) = d.pick_file() {
                        // uvnitř kontextu zapsat relativně
                        s.dockerfile = match p.strip_prefix(&ctx_dir) {
                            Ok(rel) if is_windows_path(&ctx_dir) => rel.to_string_lossy().replace('\\', "/"),
                            _ => p.to_string_lossy().to_string(),
                        };
                    }
                }
            });
            ui.end_row();

            ui.label(t!("Tagy", "Tags"));
            ui.add(field(&mut s.tags).hint_text(t!("např. mojeapp:latest, mojeapp:1.0", "e.g. myapp:latest, myapp:1.0")).desired_width(w));
            ui.end_row();

            ui.label(t!("Build argumenty", "Build args"));
            ui.add(area(&mut s.build_args).hint_text(t!("KLIC=hodnota (řádek = 1)", "KEY=value (one per line)")).desired_rows(3).desired_width(w));
            ui.end_row();

            ui.label(t!("Cílová fáze", "Target stage"));
            ui.add(field(&mut s.target).hint_text(t!("volitelné (--target)", "optional (--target)")).desired_width(w));
            ui.end_row();

            ui.label("");
            ui.vertical(|ui| {
                ui.checkbox(&mut s.no_cache, t!("Bez cache (--no-cache)", "No cache (--no-cache)"));
                ui.checkbox(&mut s.pull, t!("Vždy stáhnout základní image (--pull)", "Always pull base images (--pull)"));
            });
            ui.end_row();
        });

        // upozornění
        if df_exists == Some(false) {
            ui.add_space(6.0);
            ui.label(
                RichText::new(tf!(
                    "\u{E4E0} Dockerfile {} neexistuje.",
                    "\u{E4E0} Dockerfile {} does not exist.",
                    windows_dockerfile_path(s).map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
                ))
                .color(theme::yellow())
                .small(),
            );
        }
        if s.tag_list().is_empty() {
            ui.add_space(4.0);
            ui.label(muted(t!("Bez tagu bude image jen pod svým ID.", "Without a tag the image is only listed by its ID.")).small());
        }
        if running {
            ui.add_space(4.0);
            ui.label(RichText::new(t!("Sestavení už běží – počkej na jeho dokončení.", "A build is already running – wait for it to finish.")).color(theme::yellow()).small());
        }

        // náhled příkazu
        ui.add_space(10.0);
        egui::Frame::new()
            .fill(theme::bg())
            .corner_radius(6)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(RichText::new(s.args().join(" ")).monospace().small().color(theme::muted())).wrap());
            });
        let valid = !s.context.trim().is_empty() && !running;
        (ok, cancel) = dialog_buttons(ui, &tf!("{HAMMER} Sestavit", "{HAMMER} Build"), false, valid);
    });
    if ok {
        submit(app);
    }
    if ok || cancel || esc {
        app.build.dialog = false;
    }
}

fn log_window(app: &mut App, ctx: &egui::Context) {
    let running = app.build.running;
    let Some(log) = app.build.log.as_mut() else { return };
    if !log.open {
        return;
    }
    let mut open = true;
    let mut close = false;
    egui::Window::new(tf!("{HAMMER} Sestavení: {}", "{HAMMER} Build: {}", log.title))
        .collapsible(false)
        .id(egui::Id::new("build-log"))
        .open(&mut open)
        .default_size([900.0, 520.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                match &log.result {
                    None => {
                        ui.spinner();
                        ui.label(t!("Sestavuji…", "Building…"));
                    }
                    Some(Ok(t)) => {
                        ui.label(RichText::new(format!("\u{E184} {t}")).color(theme::green()).strong());
                    }
                    Some(Err(e)) => {
                        ui.label(RichText::new(format!("\u{E4F8} {e}")).color(theme::red()).strong());
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut r = ui.button(t!("Zavřít", "Close"));
                    if running {
                        r = r.on_hover_text(t!("Sestavení poběží dál na pozadí", "The build keeps running in the background"));
                    }
                    if r.clicked() {
                        close = true;
                    }
                    if running && super::danger_button(ui, t!("\u{E4F6} Zrušit sestavení", "\u{E4F6} Cancel build")).clicked() {
                        crate::worker::cancel_build();
                    }
                    if ui.button(t!("\u{E1CA} Kopírovat", "\u{E1CA} Copy")).clicked() {
                        ui.ctx().copy_text(log.text.clone());
                    }
                });
            });
            ui.separator();
            egui::ScrollArea::both().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(&log.text).monospace()).selectable(true).wrap_mode(egui::TextWrapMode::Extend));
            });
        });
    if !open || close {
        if running {
            log.open = false;
        } else {
            app.build.log = None;
        }
    }
}
