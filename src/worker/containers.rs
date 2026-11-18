//! Kontejnery: spuštění z image, compose, hromadné pozastavení, terminál.

use super::{Msg, RunSpec, Term, Worker};
use crate::wsl;
use std::thread;
use std::time::Duration;

impl Worker {
    pub(super) fn run_container(&self, spec: RunSpec) -> Result<String, String> {
        let cfg = self.cfg();
        let distro = cfg.distro.clone();
        let d = self.docker();
        let image = crate::engine::short_image(spec.image.trim());
        let args = spec.args_for(cfg.engine, cfg.port, cfg.tls);
        if spec.interactive {
            // interaktivně ve Windows Terminalu – kontejner se po ukončení smaže (--rm)
            let mut w: Vec<String> = vec!["-d".into(), distro.clone(), "-e".into()];
            w.extend(args);
            wsl::open_terminal(&tf!("{image} (interaktivně)", "{image} (interactive)"), &w)?;
            return Ok(tf!("{image} spuštěn interaktivně v terminálu", "{image} started interactively in a terminal"));
        }
        let out = wsl::run_owned(&distro, None, None, &args)?;
        // ID je poslední řádek výstupu (před ním může být průběh stahování)
        let id = out.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("").to_string();
        // chvíli počkat a zkontrolovat, jestli kontejner neskončil hned po startu
        thread::sleep(Duration::from_millis(1800));
        let info: Option<serde_json::Value> = d.get(&format!("/containers/{id}/json")).ok();
        let name = info
            .as_ref()
            .and_then(|v| v.get("Name").and_then(|n| n.as_str()))
            .map(|n| n.trim_start_matches('/').to_string())
            .unwrap_or_else(|| crate::docker::short_id(&id).to_string());
        let running = info.as_ref().and_then(|v| v.pointer("/State/Running").and_then(|r| r.as_bool())).unwrap_or(true);
        if running {
            Ok(tf!("Spuštěn kontejner {name} z {image}", "Started container {name} from {image}"))
        } else {
            let code = info.as_ref().and_then(|v| v.pointer("/State/ExitCode").and_then(|c| c.as_i64())).unwrap_or(0);
            Err(tf!(
                "Kontejner {name} z {image} se hned ukončil (kód {code}). Image asi nemá dlouhodobě běžící proces – \
                 zadej příkaz (např. sleep infinity) nebo zaškrtni „Interaktivně v terminálu“. Výstup najdeš v logu kontejneru.",
                "Container {name} from {image} exited immediately (code {code}). The image probably has no long-running process – \
                 enter a command (e.g. sleep infinity) or check “Interactive in terminal”. See the container log for output."
            ))
        }
    }

    pub(super) fn compose(&self, dir: String, project: Option<String>, args: Vec<String>) -> Result<String, String> {
        let cfg = self.cfg();
        let distro = cfg.distro.clone();
        let mut a: Vec<String> = cfg.engine.cli(cfg.port, cfg.tls);
        a.push("compose".into());
        if let Some(p) = &project {
            a.extend(["-p".into(), p.clone()]);
        }
        a.extend(args.iter().cloned());
        let title = format!("{} compose {}", cfg.engine.name().to_lowercase(), args.join(" "));
        let r = wsl::run_owned(&distro, None, Some(&dir), &a);
        match r {
            Ok(out) => {
                self.send(Msg::Output(title, out));
                Ok(t!("Compose: hotovo", "Compose: done").into())
            }
            Err(e) => {
                self.send(Msg::Output(title, e.clone()));
                Err(t!("Compose selhal – viz výstup", "Compose failed – see output").into())
            }
        }
    }

    pub(super) fn set_paused_all(&self, pause: bool) -> Result<String, String> {
        let d = self.docker();
        let snap = d.snapshot();
        let mut n = 0;
        for c in snap.containers.iter().filter(|c| if pause { c.running() } else { c.paused() }) {
            d.container_action(&c.id, if pause { "pause" } else { "unpause" })?;
            n += 1;
        }
        Ok(if pause {
            tf!("Pozastaveno kontejnerů: {n}", "Containers paused: {n}")
        } else {
            tf!("Obnoveno kontejnerů: {n}", "Containers resumed: {n}")
        })
    }

    pub(super) fn terminal(&self, t: Term) -> Result<String, String> {
        let cfg = self.cfg();
        let distro = cfg.distro.clone();
        let shell = "command -v bash >/dev/null && exec bash || exec sh".to_string();
        let cli = cfg.engine.cli(cfg.port, cfg.tls);
        let with_cli = |rest: Vec<String>| {
            let mut v: Vec<String> = vec!["-d".into(), distro.clone(), "-e".into()];
            v.extend(cli.iter().cloned());
            v.extend(rest);
            v
        };
        let (title, args): (String, Vec<String>) = match t {
            Term::Container { id, name } => (
                name,
                with_cli(vec!["exec".into(), "-it".into(), id, "sh".into(), "-c".into(), shell]),
            ),
            Term::Logs { id, name } => (
                format!("logs {name}"),
                with_cli(vec!["logs".into(), "-f".into(), "--tail".into(), "300".into(), id]),
            ),
            Term::Distro(n) => (n.clone(), vec!["-d".into(), n, "--cd".into(), "~".into()]),
            Term::Dir(dir) => (dir.clone(), vec!["-d".into(), distro, "--cd".into(), dir]),
        };
        wsl::open_terminal(&title, &args)
    }

}
