use super::*;
use crate::app::App;
use crate::docker::Container;
use crate::worker::{Cmd, StackMsg, StackOp, Term};
use std::collections::BTreeMap;

pub use super::compose_new::NewStack;

/// ikona tužky (Phosphor PENCIL_SIMPLE)
const PENCIL: &str = "\u{E3B4}";
/// ikona Notepadu (Phosphor NOTEPAD)
const NOTEPAD: &str = "\u{E63E}";
/// ikona kontroly (Phosphor SEAL_CHECK)
const CHECK: &str = "\u{E606}";
/// ikona škálování (Phosphor ARROWS_OUT_LINE_VERTICAL)
const SCALE: &str = "\u{E536}";

/// Stav stránky Compose: dialog nového stacku, editor souboru, škálování.
pub struct ComposeState {
    pub new: Option<NewStack>,
    pub editor: Option<FileEditor>,
    pub scale: Option<ScaleDialog>,
}

impl Default for ComposeState {
    fn default() -> Self {
        Self {
            // pro ladění/screenshoty: WLS_DEBUG_COMPOSE_NEW=<id šablony> otevře dialog nového stacku
            new: std::env::var("WLS_DEBUG_COMPOSE_NEW").ok().map(|id| NewStack::from_id(&id)),
            editor: None,
            scale: None,
        }
    }
}

/// Vestavěný editor compose souboru / .env stacku.
pub struct FileEditor {
    pub path: String,
    pub dir: String,
    pub project: String,
    /// compose soubory stacku (pro kontrolu `config -q`)
    pub files: Vec<String>,
    pub is_env: bool,
    pub text: String,
    pub orig: String,
    pub loading: bool,
    /// soubor existuje (neexistující .env se vytvoří uložením)
    pub exists: bool,
    /// text, který se právě ukládá
    saving: Option<String>,
    validating: bool,
    /// výsledek kontroly: Ok(varování) / Err(chyby)
    check: Option<Result<String, String>>,
    error: Option<String>,
}

pub struct ScaleDialog {
    pub project: String,
    pub dir: String,
    pub files: Vec<String>,
    pub service: String,
    pub replicas: u32,
}

struct Stack {
    name: String,
    dir: String,
    files: Vec<String>,
    /// soubor .env (z labelu, jinak <složka>/.env)
    env_file: String,
    containers: Vec<Container>,
}

fn open_editor(app: &mut App, s: &Stack, path: String, is_env: bool) {
    app.send(Cmd::Stack(StackOp::Read { path: path.clone(), missing_ok: is_env }));
    app.compose.editor = Some(FileEditor {
        path,
        dir: s.dir.clone(),
        project: s.name.clone(),
        files: s.files.clone(),
        is_env,
        text: String::new(),
        orig: String::new(),
        loading: true,
        exists: true,
        saving: None,
        validating: false,
        check: None,
        error: None,
    });
}

/// Argumenty `compose up -d --scale svc=N --no-recreate svc` (s -f soubory stacku, jsou-li známé).
pub fn scale_args(files: &[String], service: &str, replicas: u32) -> Vec<String> {
    let mut a = Vec::new();
    for f in files {
        a.extend(["-f".to_string(), f.clone()]);
    }
    a.extend(["up".into(), "-d".into(), "--scale".into(), format!("{service}={replicas}"), "--no-recreate".into(), service.to_string()]);
    a
}

/// Zprávy z workeru (Cmd::Stack).
pub fn on_msg(app: &mut App, m: StackMsg) {
    match m {
        StackMsg::Created(r) => super::compose_new::on_created(app, r),
        StackMsg::File { path, result } => {
            let Some(ed) = app.compose.editor.as_mut().filter(|e| e.path == path) else { return };
            ed.loading = false;
            match result {
                Ok((text, exists)) => {
                    ed.orig = text.clone();
                    ed.text = text;
                    ed.exists = exists;
                }
                Err(e) => ed.error = Some(e),
            }
        }
        StackMsg::Saved { path, result } => {
            let Some(ed) = app.compose.editor.as_mut().filter(|e| e.path == path) else { return };
            let saved = ed.saving.take();
            match result {
                Ok(()) => {
                    if let Some(t) = saved {
                        ed.orig = t;
                    }
                    ed.exists = true;
                    ed.error = None;
                }
                Err(e) => ed.error = Some(e),
            }
        }
        StackMsg::Validated { path, result } => {
            let Some(ed) = app.compose.editor.as_mut().filter(|e| e.path == path) else { return };
            ed.validating = false;
            ed.check = Some(result);
        }
    }
}

/// Dialogy stránky Compose (volá se každý snímek).
pub fn dialogs(app: &mut App, ctx: &egui::Context) {
    super::compose_new::dialog(app, ctx);
    editor_window(app, ctx);
    scale_dialog(app, ctx);
}

fn editor_window(app: &mut App, ctx: &egui::Context) {
    let Some(ed) = app.compose.editor.as_mut() else { return };
    let mut open = true;
    let mut cmd = None;
    let dirty = ed.text != ed.orig;
    egui::Window::new(format!("{PENCIL} {}{}", ed.path, if dirty { " •" } else { "" }))
        .id(egui::Id::new("compose-editor"))
        .open(&mut open)
        .default_size([820.0, 580.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let ctrl_s = ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::S));
                let can_save = (dirty || !ed.exists) && !ed.loading && ed.saving.is_none();
                ui.add_enabled_ui(can_save, |ui| {
                    if accent_button(ui, t!("\u{E248} Uložit (Ctrl+S)", "\u{E248} Save (Ctrl+S)")).clicked() || (ctrl_s && can_save) {
                        ed.saving = Some(ed.text.clone());
                        cmd = Some(StackOp::Save { path: ed.path.clone(), text: ed.text.clone() });
                    }
                });
                if dirty && ui.button(t!("\u{E038} Zahodit změny", "\u{E038} Discard changes")).clicked() {
                    ed.text = ed.orig.clone();
                }
                let r = ui
                    .add_enabled(!ed.loading && !ed.validating, egui::Button::new(tf!("{CHECK} Zkontrolovat", "{CHECK} Validate")))
                    .on_hover_text(t!("compose config -q s obsahem editoru (bez uložení)", "compose config -q with the editor content (without saving)"));
                if r.clicked() {
                    ed.validating = true;
                    ed.check = None;
                    cmd = Some(StackOp::Validate {
                        dir: ed.dir.clone(),
                        project: ed.project.clone(),
                        files: ed.files.clone(),
                        path: ed.path.clone(),
                        text: ed.text.clone(),
                    });
                }
                if ed.validating || ed.saving.is_some() || ed.loading {
                    ui.spinner();
                }
                ui.label(muted(tf!("{} řádků · projekt {}", "{} lines · project {}", ed.text.lines().count(), ed.project)).small());
            });
            if ed.is_env && !ed.exists && !ed.loading {
                ui.label(muted(t!("Soubor zatím neexistuje – vytvoří se uložením.", "The file does not exist yet – saving creates it.")).small());
            }
            if let Some(e) = &ed.error {
                ui.label(RichText::new(format!("\u{E4F8} {e}")).color(theme::red()).small());
            }
            match &ed.check {
                Some(Ok(w)) => {
                    ui.label(RichText::new(t!("\u{E184} Konfigurace je v pořádku", "\u{E184} The configuration is valid")).color(theme::green()).small());
                    if !w.trim().is_empty() {
                        ui.label(RichText::new(w.trim()).color(theme::yellow()).monospace().small());
                    }
                }
                Some(Err(e)) => {
                    egui::ScrollArea::vertical().id_salt("compose-check").max_height(140.0).show(ui, |ui| {
                        ui.label(RichText::new(format!("\u{E4F8} {e}")).color(theme::red()).monospace().small());
                    });
                }
                None => {}
            }
            ui.separator();
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_enabled(!ed.loading, area(&mut ed.text).code_editor().desired_width(f32::INFINITY).desired_rows(28));
            });
        });
    if let Some(op) = cmd {
        app.send(Cmd::Stack(op));
    }
    if !open {
        app.compose.editor = None;
    }
}

fn scale_dialog(app: &mut App, ctx: &egui::Context) {
    let Some(sd) = app.compose.scale.as_mut() else { return };
    let mut ok = false;
    let mut cancel = false;
    let (_, esc) = modal(ctx, "compose-scale", &tf!("{SCALE} Škálovat službu {}", "{SCALE} Scale service {}", sd.service), 440.0, |ui| {
        ui.horizontal(|ui| {
            ui.label(t!("Počet instancí", "Replicas"));
            ui.add(egui::DragValue::new(&mut sd.replicas).range(0..=50));
        });
        ui.add_space(6.0);
        ui.label(
            muted(t!(
                "Služba s pevným portem hostitele nebo container_name poběží jen jednou.",
                "A service with a fixed host port or container_name can only run once."
            ))
            .small(),
        );
        ui.add_space(4.0);
        ui.label(RichText::new(format!("compose up -d --scale {}={} --no-recreate", sd.service, sd.replicas)).monospace().small().color(theme::muted()));
        (ok, cancel) = dialog_buttons(ui, &tf!("{SCALE} Škálovat", "{SCALE} Scale"), false, true);
    });
    if ok {
        let args = scale_args(&sd.files, &sd.service, sd.replicas);
        let c = Cmd::Compose { dir: sd.dir.clone(), project: Some(sd.project.clone()), args };
        app.send(c);
    }
    if ok || cancel || esc {
        app.compose.scale = None;
    }
}

fn stacks(cs: &[Container]) -> Vec<Stack> {
    let mut m: BTreeMap<String, Stack> = BTreeMap::new();
    for c in cs {
        // docker compose i podman-compose nastavují com.docker.compose.*; podman-compose navíc io.podman.compose.*
        let Some(p) = c.project().or_else(|| c.label("io.podman.compose.project")) else { continue };
        let s = m.entry(p.to_string()).or_insert_with(|| Stack {
            name: p.to_string(),
            dir: c.label("com.docker.compose.project.working_dir").unwrap_or("").to_string(),
            files: c
                .label("com.docker.compose.project.config_files")
                .map(|f| f.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default(),
            env_file: c
                .label("com.docker.compose.project.environment_file")
                .and_then(|f| f.split(',').map(str::trim).find(|s| !s.is_empty()))
                .map(String::from)
                .unwrap_or_else(|| {
                    let d = c.label("com.docker.compose.project.working_dir").unwrap_or("");
                    if d.is_empty() { String::new() } else { crate::fs::join(d, ".env") }
                }),
            containers: Vec::new(),
        });
        s.containers.push(c.clone());
    }
    m.into_values().collect()
}

pub fn show(app: &mut App, ui: &mut egui::Ui) {
    let list = stacks(&app.snap.containers);
    header(ui, "Compose", &tf!("{} stacků (rozpoznáno podle labelů kontejnerů)", "{} stacks (detected from container labels)", list.len()));
    if !need_engine(app, ui) {
        return;
    }

    toolbar(ui, |ui| {
        if accent_button(ui, &tf!("{} Nový stack…", "{} New stack…", super::compose_new::PLUS))
            .on_hover_text(t!("Nový stack ze šablony (PostgreSQL, Redis, WordPress, …)", "New stack from a template (PostgreSQL, Redis, WordPress, …)"))
            .clicked()
        {
            super::compose_new::open(app);
        }
    });
    ui.add_space(8.0);
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(t!("Spustit stack ze složky", "Start stack from folder")).strong());
        toolbar(ui, |ui| {
            ui.add(
                field(&mut app.compose_dir)
                    .hint_text(t!(r"D:\projekty\app  nebo  /home/user/app", r"D:\projects\app  or  /home/user/app"))
                    .desired_width(380.0),
            );
            if accent_button(ui, "\u{E3D0} docker compose up -d").clicked() && !app.compose_dir.trim().is_empty() {
                app.send(Cmd::ComposeUpPath(app.compose_dir.trim().to_string()));
            }
        });
        ui.label(muted(t!("Složka musí obsahovat compose.yaml / docker-compose.yml. Windows cesty se převedou přes wslpath.", "The folder must contain compose.yaml / docker-compose.yml. Windows paths are converted via wslpath.")).small());
    });

    if list.is_empty() {
        ui.add_space(8.0);
        ui.label(muted(t!("Žádné compose stacky.", "No Compose stacks.")));
        return;
    }

    for s in list {
        ui.add_space(10.0);
        let running = s.containers.iter().filter(|c| c.running()).count();
        let total = s.containers.len();
        card(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                dot(
                    ui,
                    if running == total { theme::green() } else if running > 0 { theme::yellow() } else { theme::grey() },
                );
                ui.label(RichText::new(&s.name).size(18.0).strong());
                ui.label(muted(tf!("{running}/{total} běží", "{running}/{total} running")));
            });
            ui.label(RichText::new(&s.dir).monospace().small().color(theme::muted()));
            ui.add_space(4.0);
            let dir = s.dir.clone();
            let proj = Some(s.name.clone());
            let c = |args: &[&str]| Cmd::Compose { dir: dir.clone(), project: proj.clone(), args: args.iter().map(|a| a.to_string()).collect() };
            toolbar(ui, |ui| {
                if accent_button(ui, "\u{E3D0} Up").on_hover_text("docker compose up -d").clicked() {
                    app.send(c(&["up", "-d"]));
                }
                if ui.button("\u{E46C} Stop").clicked() {
                    app.send(c(&["stop"]));
                }
                if ui.button("\u{E3D0} Start").clicked() {
                    app.send(c(&["start"]));
                }
                if ui.button("\u{E094} Restart").clicked() {
                    app.send(c(&["restart"]));
                }
                if ui.button("\u{E20C} Pull").clicked() {
                    app.send(c(&["pull"]));
                }
                if ui.button("\u{E80E} Build & up").on_hover_text("docker compose up -d --build").clicked() {
                    app.send(c(&["up", "-d", "--build"]));
                }
                if ui.button("\u{E2F2} ps").clicked() {
                    app.send(c(&["ps", "-a"]));
                }
                if ui.button(t!("\u{EB7A} Logy", "\u{EB7A} Logs")).on_hover_text("docker compose logs --tail 300").clicked() {
                    app.send(c(&["logs", "--no-color", "--tail", "300"]));
                }
                if ui.button("\u{E4A6} Down").clicked() {
                    app.ask(
                        format!("docker compose down – {}?", s.name),
                        t!("Kontejnery a sítě stacku budou odstraněny (volume zůstanou).", "The stack's containers and networks will be removed (volumes are kept)."),
                        c(&["down"]),
                    );
                }
                gap(ui);
                for f in &s.files {
                    let fname = f.rsplit('/').next().unwrap_or(f);
                    if ui.button(format!("{PENCIL} {fname}")).on_hover_text(tf!("Upravit compose soubor: {f}", "Edit compose file: {f}")).clicked() {
                        open_editor(app, &s, f.clone(), false);
                    }
                }
                if !s.env_file.is_empty() && ui.button(format!("{PENCIL} .env")).on_hover_text(tf!("Upravit .env: {}", "Edit .env: {}", s.env_file)).clicked() {
                    open_editor(app, &s, s.env_file.clone(), true);
                }
                for f in &s.files {
                    if icon(ui, NOTEPAD, &tf!("Upravit v Notepadu: {f}", "Edit in Notepad: {f}")) {
                        app.send(Cmd::Notepad(f.clone()));
                    }
                }
                if icon(ui, "\u{E256}", t!("Otevřít složku v Průzkumníku", "Open folder in Explorer")) {
                    app.send(Cmd::Explorer(s.dir.clone()));
                }
                if icon(ui, "\u{EAE8}", t!("Terminál ve složce stacku", "Terminal in stack folder")) {
                    app.send(Cmd::Terminal(Term::Dir(s.dir.clone())));
                }
            });
            ui.add_space(4.0);
            let cols = [
                ("", W::Px(14.0)),
                (t!("SLUŽBA", "SERVICE"), W::Fr(1.5)),
                ("IMAGE", W::Fr(2.0)),
                (t!("PORTY", "PORTS"), W::FrOpt(2.0)),
                (t!("AKCE", "ACTIONS"), icons_w(4)),
            ];
            table_in(ui, &format!("svc-{}", s.name), &cols, |mut body| {
                for ct in &s.containers {
                  body.row(32.0, |mut r| {
                    r.col(|ui| dot(ui, if ct.running() { theme::green() } else if ct.paused() { theme::yellow() } else { theme::grey() }));
                    r.col(|ui| {
                        ui.label(RichText::new(ct.label("com.docker.compose.service").unwrap_or("?")).strong());
                    });
                    r.col(|ui| {
                        ui.label(crate::engine::short_image(&ct.image)).on_hover_text(&ct.image);
                    });
                    r.col(|ui| {
                        ui.label(ct.ports_text());
                    });
                    r.col(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        if ct.running() {
                            if icon(ui, "\u{E46C}", t!("Zastavit", "Stop")) {
                                app.send(Cmd::Container(ct.id.clone(), "stop"));
                            }
                            if icon(ui, "\u{EAE8}", t!("Konzole", "Console")) {
                                app.send(Cmd::Terminal(Term::Container { id: ct.id.clone(), name: ct.name() }));
                            }
                        } else if icon(ui, "\u{E3D0}", t!("Spustit", "Start")) {
                            app.send(Cmd::Container(ct.id.clone(), "start"));
                        }
                        if icon(ui, "\u{EB7A}", t!("Logy", "Logs")) {
                            app.open_logs(ct.id.clone(), ct.name());
                        }
                        if let Some(svc) = ct.label("com.docker.compose.service") {
                            if icon(ui, SCALE, t!("Škálovat…", "Scale…")) {
                                let n = s.containers.iter().filter(|c| c.label("com.docker.compose.service") == Some(svc)).count();
                                app.compose.scale = Some(ScaleDialog {
                                    project: s.name.clone(),
                                    dir: s.dir.clone(),
                                    files: s.files.clone(),
                                    service: svc.to_string(),
                                    replicas: n as u32,
                                });
                            }
                        }
                    });
                  });
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn scale_args() {
        assert_eq!(
            super::scale_args(&["/s/compose.yaml".into()], "web", 3),
            ["-f", "/s/compose.yaml", "up", "-d", "--scale", "web=3", "--no-recreate", "web"]
        );
        assert_eq!(super::scale_args(&[], "db", 0), ["up", "-d", "--scale", "db=0", "--no-recreate", "db"]);
    }
}
