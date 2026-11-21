//! Kontejnery: spuštění z image, hromadné pozastavení, terminál.

use super::{RunSpec, Term, Worker};
use crate::host::{ExecReq, HostExt, Privilege};
use crate::wsl;
use std::thread;
use std::time::Duration;

/// Uloží stdin do nového souboru čitelného jen vlastníkem a vypíše jeho cestu.
const ENV_FILE_SCRIPT: &str = r#"umask 077; f=$(mktemp "${TMPDIR:-/tmp}/wslcd-env.XXXXXX") && cat > "$f" && echo "$f""#;

impl Worker {
    pub(super) fn run_container(&self, spec: RunSpec) -> Result<String, String> {
        let cfg = self.cfg();
        let host = self.host();
        let d = self.docker();
        let image = crate::engine::short_image(spec.image.trim());
        let args = spec.args_for(cfg.engine, cfg.port, cfg.tls);
        if spec.interactive {
            // interaktivně ve Windows Terminalu – kontejner se po ukončení smaže (--rm)
            let w: Vec<&str> = args.iter().map(|a| a.as_str()).collect();
            host.open_terminal(&tf!("{image} (interaktivně)", "{image} (interactive)"), &ExecReq::new(&w))?;
            return Ok(tf!("{image} spuštěn interaktivně v terminálu", "{image} started interactively in a terminal"));
        }
        // proměnné prostředí přes dočasný soubor (0600) místo `-e K=V` v příkazové řádce,
        // kterou ve WSL přečte kdokoli přes /proc/*/cmdline (F-11)
        let env: Vec<&str> = spec.env.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        let (args, env_file) = if env.is_empty() {
            (args, None)
        } else {
            let file = host.run_stdin(Privilege::User, &["sh", "-c", ENV_FILE_SCRIPT], &format!("{}\n", env.join("\n")))?;
            let file = file.trim().to_string();
            let spec2 = RunSpec { env: String::new(), ..spec.clone() };
            let mut a = spec2.args_for(cfg.engine, cfg.port, cfg.tls);
            let at = a.iter().position(|x| x == "-d").map(|i| i + 1).unwrap_or(a.len());
            a.splice(at..at, ["--env-file".to_string(), file.clone()]);
            (a, Some(file))
        };
        let out = host.run_owned(Privilege::User, None, &args);
        if let Some(f) = &env_file {
            let _ = host.run(Privilege::User, None, &["rm", "-f", "--", f]);
        }
        let out = out?;
        // ID je poslední řádek výstupu (před ním může být průběh stahování)
        let id = out.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or("").to_string();
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
        let host = self.host();
        let shell = "command -v bash >/dev/null && exec bash || exec sh";
        let cli = cfg.engine.cli(cfg.port, cfg.tls);
        // příkaz enginu v distribuci hostitele, v novém terminálu
        let open_cli = |title: &str, rest: &[&str]| {
            let mut v: Vec<&str> = cli.iter().map(|a| a.as_str()).collect();
            v.extend_from_slice(rest);
            host.open_terminal(title, &ExecReq::new(&v))
        };
        match t {
            Term::Container { id, name } => open_cli(&name, &["exec", "-it", &id, "sh", "-c", shell])?,
            Term::Logs { id, name } => open_cli(&format!("logs {name}"), &["logs", "-f", "--tail", "300", &id])?,
            Term::PodLogs { ns, pod } => {
                let kubectl = ["exec", "-it", crate::k8s::cluster::NAME, "kubectl", "logs", "-f", "--tail", "300", "-n", &ns, &pod];
                let mut v = vec![match cfg.engine {
                    crate::engine::Engine::Docker => "docker",
                    crate::engine::Engine::Podman => "podman",
                }];
                v.extend_from_slice(&kubectl);
                host.open_terminal(&format!("logs {pod}"), &ExecReq::new(&v).privilege(Privilege::Engine))?
            }
            // shell distribuce (bez příkazu) – jen WSL
            Term::Distro(n) => wsl::open_terminal(&n, &["-d".into(), n.clone(), "--cd".into(), "~".into()])
                .map(|_| ())?,
            Term::Dir(dir) => {
                let distro = host.wsl().expect("WSL").distro().to_string();
                wsl::open_terminal(&dir, &["-d".into(), distro, "--cd".into(), dir.clone()]).map(|_| ())?
            }
        }
        Ok(String::new())
    }

}

/// Pro živé testy: spuštění kontejneru stejnou cestou jako z dialogu.
#[cfg(test)]
pub fn run_for_test(cfg: crate::config::Config, spec: RunSpec) -> Result<String, String> {
    let (msg, _m) = std::sync::mpsc::channel();
    let (cmd, _c) = std::sync::mpsc::channel();
    let w = Worker {
        ctx: eframe::egui::Context::default(),
        cfg: std::sync::Arc::new(std::sync::RwLock::new(cfg)),
        msg,
        cmd,
        shared: std::sync::Arc::new(super::Shared::default()),
    };
    w.run_container(spec)
}
