//! Dialog „Nový stack…“: šablona, název projektu, složka, úprava compose.yaml a .env, vytvoření.

use super::*;
use crate::app::App;
use crate::compose_templates::{self as tpl, TEMPLATES};
use crate::worker::{is_windows_path, Cmd, NewStackSpec, StackOp};

/// ikona plus (Phosphor PLUS)
pub const PLUS: &str = "\u{E3D4}";
/// ikona klíče (Phosphor KEY)
const KEY: &str = "\u{E2D6}";

pub struct NewStack {
    pub template: usize,
    pub project: String,
    pub dir: String,
    /// složka se odvozuje z názvu projektu (dokud ji uživatel nezmění)
    dir_auto: bool,
    pub compose: String,
    pub env: String,
    /// vygenerovaný obsah (pro zjištění, jestli uživatel něco upravil)
    generated: (String, String),
    tab_env: bool,
    pub busy: bool,
    pub error: Option<String>,
}

impl NewStack {
    pub fn new(template: usize) -> Self {
        let mut s = Self {
            template: 0,
            project: String::new(),
            dir: String::new(),
            dir_auto: true,
            compose: String::new(),
            env: String::new(),
            generated: (String::new(), String::new()),
            tab_env: false,
            busy: false,
            error: None,
        };
        s.apply_template(template.min(TEMPLATES.len() - 1));
        s
    }

    /// Otevře dialog se šablonou podle id (WLS_DEBUG_COMPOSE_NEW).
    pub fn from_id(id: &str) -> Self {
        Self::new(TEMPLATES.iter().position(|t| t.id == id.trim()).unwrap_or(0))
    }

    fn apply_template(&mut self, idx: usize) {
        let t = &TEMPLATES[idx];
        self.template = idx;
        self.project = t.id.to_string();
        if self.dir_auto {
            self.dir = default_dir(&self.project);
        }
        self.compose = t.compose.to_string();
        self.env = t.render_env(&self.project);
        self.generated = (self.compose.clone(), self.env.clone());
        self.error = None;
    }

    fn edited(&self) -> bool {
        self.compose != self.generated.0 || self.env != self.generated.1
    }

    fn project_changed(&mut self) {
        if self.dir_auto {
            self.dir = default_dir(self.project.trim());
        }
        let p = self.project.trim().to_string();
        self.env = set_env_line(&self.env, "COMPOSE_PROJECT_NAME", &p);
        self.generated.1 = set_env_line(&self.generated.1, "COMPOSE_PROJECT_NAME", &p);
    }

    fn spec(&self, start: bool) -> NewStackSpec {
        NewStackSpec { project: self.project.trim().to_string(), dir: self.dir.trim().to_string(), compose: self.compose.clone(), env: self.env.clone(), start }
    }
}

fn default_dir(project: &str) -> String {
    format!("~/stacks/{project}")
}

/// Nastaví (nebo přidá) řádek `KEY=value` v textu .env.
pub fn set_env_line(env: &str, key: &str, val: &str) -> String {
    let prefix = format!("{key}=");
    let mut found = false;
    let mut out: Vec<String> = env
        .lines()
        .map(|l| {
            if l.trim_start().starts_with(&prefix) {
                found = true;
                format!("{key}={val}")
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        out.push(format!("{key}={val}"));
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// Vygeneruje nová hesla pro proměnné šablony typu Secret (ostatní řádky nechá).
fn regenerate_secrets(env: &str, template: usize) -> String {
    let mut s = env.to_string();
    for (k, v) in TEMPLATES[template].env {
        if *v == tpl::Val::Secret {
            s = set_env_line(&s, k, &tpl::password(24));
        }
    }
    s
}

pub fn open(app: &mut App) {
    if app.compose.new.is_none() {
        app.compose.new = Some(NewStack::new(0));
    }
}

pub fn dialog(app: &mut App, ctx: &egui::Context) {
    let Some(st) = app.compose.new.as_mut() else { return };
    let mut submit: Option<bool> = None;
    let mut cancel = false;
    let (_, close) = modal(ctx, "compose-new", &tf!("{PLUS} Nový compose stack", "{PLUS} New compose stack"), 860.0, |ui| {
        ui.horizontal_top(|ui| {
            // šablony
            ui.vertical(|ui| {
                ui.set_width(190.0);
                ui.label(muted(t!("Šablona", "Template")).small());
                ui.add_space(2.0);
                for (i, t) in TEMPLATES.iter().enumerate() {
                    let sel = st.template == i;
                    let r = ui.add_enabled(!st.busy, egui::Button::new(t.name).selected(sel).frame(sel).min_size(egui::vec2(190.0, 28.0)));
                    if r.on_hover_text(t.description()).clicked() && !sel {
                        st.apply_template(i);
                    }
                }
            });
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                ui.label(muted(TEMPLATES[st.template].description()));
                ui.add_space(6.0);
                let w = ui.available_width() - 130.0;
                egui::Grid::new("compose-new-form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label(t!("Název projektu", "Project name"));
                    if ui.add(field(&mut st.project).hint_text("app").desired_width(w)).changed() {
                        st.project_changed();
                    }
                    ui.end_row();

                    ui.label(t!("Složka", "Folder"));
                    ui.horizontal(|ui| {
                        if ui.add(field(&mut st.dir).hint_text("~/stacks/app").desired_width(w - 38.0)).changed() {
                            st.dir_auto = st.dir.trim().is_empty();
                        }
                        let r = ui
                            .add(egui::Button::new(RichText::new("\u{E256}").size(16.0)).min_size(egui::vec2(theme::CONTROL_H, theme::CONTROL_H)))
                            .on_hover_text(t!("Vybrat složku ve Windows…", "Pick a Windows folder…"));
                        if r.clicked() {
                            let mut d = rfd::FileDialog::new().set_title(t!("Složka stacku", "Stack folder"));
                            if is_windows_path(&st.dir) {
                                d = d.set_directory(st.dir.trim());
                            }
                            if let Some(p) = d.pick_folder() {
                                st.dir = p.to_string_lossy().to_string();
                                st.dir_auto = false;
                            }
                        }
                    });
                    ui.end_row();
                });
                let name_ok = tpl::valid_project(st.project.trim());
                if !name_ok {
                    ui.label(
                        RichText::new(t!(
                            "Název: malá písmena, číslice, - a _ (začíná písmenem nebo číslicí).",
                            "Name: lowercase letters, digits, - and _ (starting with a letter or digit)."
                        ))
                        .color(theme::yellow())
                        .small(),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.selectable_label(!st.tab_env, "compose.yaml").clicked() {
                        st.tab_env = false;
                    }
                    if ui.selectable_label(st.tab_env, ".env").clicked() {
                        st.tab_env = true;
                    }
                    if st.tab_env {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(!st.busy, egui::Button::new(tf!("{KEY} Nová hesla", "{KEY} New passwords")))
                                .on_hover_text(t!("Vygenerovat nová náhodná hesla", "Generate new random passwords"))
                                .clicked()
                            {
                                st.env = regenerate_secrets(&st.env, st.template);
                            }
                        });
                    }
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical().id_salt("compose-new-text").max_height(330.0).show(ui, |ui| {
                    let text = if st.tab_env { &mut st.env } else { &mut st.compose };
                    ui.add_enabled(!st.busy, area(text).code_editor().desired_width(f32::INFINITY).desired_rows(18));
                });
                if st.tab_env {
                    ui.label(muted(t!("Soubor .env bude čitelný jen pro vlastníka (chmod 600).", "The .env file will be readable by the owner only (chmod 600).")).small());
                }
                let keys = tpl::env_keys(&st.env);
                let missing: Vec<String> = tpl::vars_used(&st.compose).into_iter().filter(|v| !keys.contains(v)).collect();
                if !missing.is_empty() {
                    ui.label(
                        RichText::new(tf!("\u{E4E0} V .env chybí: {}", "\u{E4E0} Missing in .env: {}", missing.join(", ")))
                            .color(theme::yellow())
                            .small(),
                    );
                }
                let ports = tpl::host_ports(&tpl::expand(&st.compose, &st.env));
                if !ports.is_empty() {
                    ui.label(muted(tf!("Porty na hostiteli: {}", "Host ports: {}", ports.join(", "))).small());
                }
            });
        });

        if let Some(e) = &st.error {
            ui.add_space(8.0);
            egui::ScrollArea::vertical().id_salt("compose-new-err").max_height(120.0).show(ui, |ui| {
                ui.label(RichText::new(format!("\u{E4F8} {e}")).color(theme::red()).monospace().small());
            });
        }

        // tlačítka vpravo: Vytvořit a spustit (hlavní), Vytvořit, Zrušit
        ui.add_space(16.0);
        let valid = tpl::valid_project(st.project.trim()) && !st.compose.trim().is_empty() && !st.busy;
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(valid, |ui| {
                    if accent_button(ui, t!("\u{E3D0} Vytvořit a spustit", "\u{E3D0} Create and start")).on_hover_text("compose up -d").clicked() {
                        submit = Some(true);
                    }
                    if ui.button(t!("Vytvořit", "Create")).clicked() {
                        submit = Some(false);
                    }
                });
                if ui.add_enabled(!st.busy, egui::Button::new(t!("Zrušit", "Cancel"))).clicked() {
                    cancel = true;
                }
                if st.busy {
                    ui.spinner();
                    ui.label(muted(t!("Kontroluji a vytvářím…", "Checking and creating…")));
                }
            });
        });
    });
    if let Some(start) = submit {
        st.busy = true;
        st.error = None;
        let spec = st.spec(start);
        app.send(Cmd::Stack(StackOp::Create(spec)));
        return;
    }
    // Esc / klik mimo zavře jen neupravený dialog, který zrovna nic nevytváří
    if cancel || (close && !st.busy && !st.edited()) {
        app.compose.new = None;
    }
}

/// Výsledek vytvoření z workeru.
pub fn on_created(app: &mut App, res: Result<String, String>) {
    match res {
        Ok(dir) => {
            app.compose.new = None;
            app.compose_dir = dir;
        }
        Err(e) => {
            if let Some(st) = app.compose.new.as_mut() {
                st.busy = false;
                st.error = Some(e);
            } else {
                app.toast(false, e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_line_replace_and_add() {
        assert_eq!(set_env_line("# x\nA=1\nB=2\n", "A", "9"), "# x\nA=9\nB=2\n");
        assert_eq!(set_env_line("A=1", "C", "3"), "A=1\nC=3\n");
    }

    #[test]
    fn new_dialog_state() {
        let s = NewStack::from_id("redis");
        assert_eq!(TEMPLATES[s.template].id, "redis");
        assert_eq!(s.project, "redis");
        assert_eq!(s.dir, "~/stacks/redis");
        assert!(!s.edited());
        let mut s = s;
        s.project = "cache".into();
        s.project_changed();
        assert_eq!(s.dir, "~/stacks/cache");
        assert!(s.env.contains("COMPOSE_PROJECT_NAME=cache\n"));
        assert!(!s.edited());
        let old = s.env.clone();
        s.env = regenerate_secrets(&s.env, s.template);
        assert_ne!(s.env, old);
        assert!(s.env.contains("REDIS_PORT=6379\n"));
        // neznámé id -> první šablona
        assert_eq!(NewStack::from_id("nic").template, 0);
    }
}
