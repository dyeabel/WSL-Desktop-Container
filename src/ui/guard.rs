//! Strop paměti kontejnerů: sekce v Nastavení a řádky v přehledu Paměti.

use super::*;
use crate::app::App;
use crate::memory::GuardNow;
use crate::worker::Cmd;

/// Bajty jako GB na 1 desetinné místo.
fn gb(v: u64) -> String {
    format!("{:.1}", v as f64 / 1_073_741_824.0)
}

fn cap_text(g: Option<&GuardNow>) -> String {
    match g.and_then(|g| g.max.map(|m| (m, g.reserve.unwrap_or(0)))) {
        Some((m, r)) => {
            let (m, r) = (gb(m), gb(r));
            tf!("Strop: {m} GB (rezerva {r} GB)", "Cap: {m} GB (reserve {r} GB)")
        }
        None => t!("Strop zatím nenastaven", "Cap not set yet").into(),
    }
}

fn usage_text(g: &GuardNow) -> Option<String> {
    let (c, m) = (gb(g.current), gb(g.max?));
    Some(tf!("Kontejnery: {c} / {m} GB (strop)", "Containers: {c} / {m} GB (cap)"))
}

fn outside_text(n: u32, en: bool) -> String {
    const HINT: &str = "(např. compose up --force-recreate)";
    match (en, n) {
        (false, 1) => format!("1 kontejner zatím mimo strop – vytvoř ho znovu {HINT}"),
        (false, _) => {
            let w = if (2..=4).contains(&n) { "kontejnery" } else { "kontejnerů" };
            format!("{n} {w} zatím mimo strop – vytvoř je znovu {HINT}")
        }
        (true, 1) => "1 container is not under the cap yet – recreate it (e.g. compose up --force-recreate)".into(),
        (true, _) => format!("{n} containers are not under the cap yet – recreate them (e.g. compose up --force-recreate)"),
    }
}

/// Řádky stropu v souhrnu paměti.
pub fn overview_rows(ui: &mut egui::Ui, g: Option<&GuardNow>, docker: bool, unguarded: &[String]) {
    if let Some(u) = g.and_then(usage_text) {
        ui.add_space(6.0);
        ui.label(u);
    }
    if let Some(g) = g.filter(|g| docker && g.outside > 0) {
        ui.label(muted(outside_text(g.outside, crate::i18n::is_en())).small());
    }
    for name in unguarded {
        ui.label(muted(tf!("Engine v distribuci {name} běží bez stropu paměti.", "The engine in distribution {name} runs without a memory cap.")).small());
    }
}

/// Karta „Paměť kontejnerů“ v Nastavení – mění se okamžitě.
pub fn settings_section(app: &mut App, ui: &mut egui::Ui) {
    section(ui, t!("Paměť kontejnerů", "Container memory"));
    let (mut on, reserve, docker) = app.cfg.read().map(|c| (c.container_guard, c.guard_reserve_gb, c.engine == crate::engine::Engine::Docker)).unwrap_or_default();
    let changed = egui::Id::new("guard-changed");
    let mut apply = false;
    card(ui, |ui| {
        ui.set_width(ui.available_width());
        let label = t!("Strop paměti pro kontejnery (rezerva pro systém)", "Memory cap for containers (reserve for the system)");
        let tip = t!("Když kontejnery vyčerpají paměť, ukončí se kontejner, ne engine ani WSL.", "When containers run out of memory, a container is killed, not the engine or WSL.");
        if ui.checkbox(&mut on, label).on_hover_text(tip).changed() {
            save(app, |c| c.container_guard = on);
            apply = true;
        }
        let mut r = reserve;
        ui.horizontal(|ui| {
            ui.label(t!("Rezerva (GB, 0 = automaticky)", "Reserve (GB, 0 = automatic)"));
            let resp = ui.add_enabled(on, egui::DragValue::new(&mut r).range(0.0..=64.0).speed(0.5).max_decimals(1));
            if super::alerts::save_now(&resp) {
                let v = crate::guard::snap_reserve(r);
                save(app, |c| c.guard_reserve_gb = v);
                apply = true;
            }
        });
        if on {
            let g = app.shared.memory.lock().ok().and_then(|m| m.guard);
            ui.label(muted(cap_text(g.as_ref())).small());
        }
        if apply {
            ui.ctx().data_mut(|d| d.insert_temp(changed, true));
        }
        if docker && ui.ctx().data(|d| d.get_temp::<bool>(changed)).unwrap_or(false) {
            ui.label(muted(t!("Projeví se po novém nastavení API enginu a jen pro nově vytvořené kontejnery.", "Takes effect after the engine API is set up again, and only for newly created containers.")).small());
        }
    });
    if apply {
        app.send(Cmd::ApplyGuard);
    }
}

fn save(app: &mut App, f: impl FnOnce(&mut crate::config::Config)) {
    let res = match app.cfg.write() {
        Ok(mut g) => {
            f(&mut g);
            g.save()
        }
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = res {
        app.toast(false, tf!("Nastavení nelze uložit: {e}", "Cannot save settings: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gb_one_decimal() {
        assert_eq!(gb(13_207_024_435), "12.3");
        assert_eq!(gb(0), "0.0");
    }

    #[test]
    fn cap_line() {
        let g = GuardNow { max: Some(13_207_024_435), reserve: Some(2_362_232_012), ..Default::default() };
        assert_eq!(cap_text(Some(&g)), "Strop: 12.3 GB (rezerva 2.2 GB)");
        assert_eq!(cap_text(None), "Strop zatím nenastaven");
        assert_eq!(cap_text(Some(&GuardNow::default())), "Strop zatím nenastaven");
    }

    #[test]
    fn usage_line() {
        let g = GuardNow { max: Some(13_207_024_435), current: 3_328_599_654, ..Default::default() };
        assert_eq!(usage_text(&g).as_deref(), Some("Kontejnery: 3.1 / 12.3 GB (strop)"));
        assert_eq!(usage_text(&GuardNow::default()), None);
    }

    #[test]
    fn outside_plural() {
        assert_eq!(outside_text(1, false), "1 kontejner zatím mimo strop – vytvoř ho znovu (např. compose up --force-recreate)");
        assert!(outside_text(3, false).starts_with("3 kontejnery zatím mimo strop – vytvoř je znovu"));
        assert!(outside_text(5, false).starts_with("5 kontejnerů zatím"));
        assert_eq!(outside_text(1, true), "1 container is not under the cap yet – recreate it (e.g. compose up --force-recreate)");
        assert_eq!(outside_text(3, true), "3 containers are not under the cap yet – recreate them (e.g. compose up --force-recreate)");
    }
}
