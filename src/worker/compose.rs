//! Compose: příkazy pro stacky, nový stack ze šablony, editor souborů a jejich kontrola.

use super::{is_windows_path, Msg, Worker};
use crate::wsl;
use std::io::Write;
use std::process::Stdio;

/// Nový stack: soubory se zapíšou do složky (Windows / Linux cesta, `~` = domov uživatele distribuce).
#[derive(Debug, Clone, Default)]
pub struct NewStackSpec {
    pub project: String,
    /// prázdné = ~/stacks/<projekt>
    pub dir: String,
    pub compose: String,
    pub env: String,
    /// po vytvoření spustit `up -d`
    pub start: bool,
}

#[derive(Debug, Clone)]
pub enum StackOp {
    Create(NewStackSpec),
    /// načíst soubor do editoru; `missing_ok` = neexistující soubor otevřít prázdný (.env)
    Read { path: String, missing_ok: bool },
    Save { path: String, text: String },
    /// `compose config -q` s obsahem editoru místo souboru `path` (compose soubor nebo .env)
    Validate { dir: String, project: String, files: Vec<String>, path: String, text: String },
}

#[derive(Debug, Clone)]
pub enum StackMsg {
    /// Ok(složka stacku) / chyba (dialog zůstane otevřený)
    Created(Result<String, String>),
    /// obsah souboru a jestli existuje
    File { path: String, result: Result<(String, bool), String> },
    Saved { path: String, result: Result<(), String> },
    /// Ok(varování) / chyby
    Validated { path: String, result: Result<String, String> },
}

/// Zjistí absolutní cestu složky (rozbalí `~`) a jestli už v ní compose soubor / .env není.
/// Cesta jde jako argument, ne do textu skriptu.
const RESOLVE_SCRIPT: &str = r#"p="$1"
case "$p" in "~") p="$HOME";; "~/"*) p="$HOME/${p#\~/}";; esac
for f in compose.yaml compose.yml docker-compose.yml docker-compose.yaml .env; do
  if [ -e "$p/$f" ]; then printf 'EXISTS %s\n' "$f"; exit 0; fi
done
printf 'PATH %s\n' "$p""#;

/// Spustí příkaz (`$2…`) s obsahem .env (`$1`) v dočasném souboru jako `--env-file` před podpříkazem `config -q`.
const ENV_CHECK_SCRIPT: &str = r#"f=$(mktemp) || exit 1
printf '%s' "$1" > "$f"
shift
"$@" --env-file "$f" config -q
rc=$?
rm -f "$f"
exit $rc"#;

/// Odstraní z výstupu ANSI sekvence a hlášky Podmanu o externím compose providerovi.
pub fn clean_output(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            if it.peek() == Some(&'[') {
                it.next();
                for d in it.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out.lines()
        .filter(|l| !l.contains("Executing external compose provider"))
        .filter(|l| !(l.starts_with("Error: executing ") && l.contains("docker-compose")))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// Spustí program v distribuci s textem na stdin (zapisuje se z vlastního vlákna, ať se nezablokuje).
fn run_input(distro: &str, cwd: Option<&str>, args: &[String], input: &str) -> Result<String, String> {
    let mut child = wsl::base(distro, None, cwd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let writer = child.stdin.take().map(|mut si| {
        let data = input.as_bytes().to_vec();
        std::thread::spawn(move || {
            let _ = si.write_all(&data);
        })
    });
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if let Some(w) = writer {
        let _ = w.join();
    }
    let text = format!("{}\n{}", wsl::decode(&out.stdout), wsl::decode(&out.stderr));
    let text = clean_output(&text);
    if out.status.success() {
        Ok(text)
    } else if text.is_empty() {
        Err(tf!("kód {}", "code {}", out.status.code().unwrap_or(-1)))
    } else {
        Err(text)
    }
}

/// Argumenty `compose config -q`: compose soubory (`-` = stdin) a volitelně obsah .env.
/// `env_text` se předává jako argument skriptu, nikdy jako jeho text.
pub fn check_args(cli: &[String], project: &str, dir: &str, files: &[String], env_text: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    if let Some(env) = env_text {
        a.extend(["sh".into(), "-c".into(), ENV_CHECK_SCRIPT.into(), "sh".into(), env.to_string()]);
    }
    a.extend(cli.iter().cloned());
    a.push("compose".into());
    if !project.is_empty() {
        a.extend(["-p".into(), project.to_string()]);
    }
    a.extend(["--project-directory".into(), dir.to_string()]);
    for f in files {
        a.extend(["-f".into(), f.clone()]);
    }
    if env_text.is_none() {
        a.extend(["config".into(), "-q".into()]);
    }
    a
}

/// Kontrola stacku `config -q`. `compose_stdin` = obsah compose souboru označeného v `files` jako `-`.
#[allow(clippy::too_many_arguments)]
pub fn validate(
    distro: &str,
    cli: &[String],
    project: &str,
    dir: &str,
    files: &[String],
    env_text: Option<&str>,
    compose_stdin: &str,
) -> Result<String, String> {
    let args = check_args(cli, project, dir, files, env_text);
    run_input(distro, None, &args, compose_stdin)
}

impl Worker {
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
                self.send(Msg::Output(title, clean_output(&out)));
                Ok(t!("Compose: hotovo", "Compose: done").into())
            }
            Err(e) => {
                self.send(Msg::Output(title, clean_output(&e)));
                Err(t!("Compose selhal – viz výstup", "Compose failed – see output").into())
            }
        }
    }

    pub(super) fn stack_op(&self, op: StackOp) -> Result<String, String> {
        let cfg = self.cfg();
        let distro = cfg.distro.clone();
        let cli = cfg.engine.cli(cfg.port, cfg.tls);
        match op {
            StackOp::Create(spec) => {
                let project = spec.project.clone();
                match create_stack(&distro, &cli, &spec) {
                    Ok(dir) => {
                        self.send(Msg::Stack(StackMsg::Created(Ok(dir.clone()))));
                        if spec.start {
                            self.compose(dir, Some(project), vec!["up".into(), "-d".into()])
                        } else {
                            Ok(tf!("Stack {project} vytvořen v {dir}", "Stack {project} created in {dir}"))
                        }
                    }
                    Err(e) => {
                        self.send(Msg::Stack(StackMsg::Created(Err(e))));
                        Ok(String::new())
                    }
                }
            }
            StackOp::Read { path, missing_ok } => {
                let result = if missing_ok && wsl::run(&distro, None, None, &["test", "-e", &path]).is_err() {
                    Ok((String::new(), false))
                } else {
                    crate::fs::read_text(&distro, false, &path).map(|t| (t, true))
                };
                self.send(Msg::Stack(StackMsg::File { path, result }));
                Ok(String::new())
            }
            StackOp::Save { path, text } => {
                let r = crate::fs::write_text(&distro, false, &path, &text).map(|_| ());
                let ok = r.is_ok();
                self.send(Msg::Stack(StackMsg::Saved { path: path.clone(), result: r }));
                Ok(if ok { tf!("Uloženo: {path}", "Saved: {path}") } else { String::new() })
            }
            StackOp::Validate { dir, project, files, path, text } => {
                let is_compose = files.iter().any(|f| f == &path) || (files.is_empty() && !path.ends_with(".env"));
                let result = if is_compose {
                    let fl: Vec<String> = if files.is_empty() {
                        vec!["-".into()]
                    } else {
                        files.iter().map(|f| if f == &path { "-".to_string() } else { f.clone() }).collect()
                    };
                    validate(&distro, &cli, &project, &dir, &fl, None, &text)
                } else {
                    validate(&distro, &cli, &project, &dir, &files, Some(&text), "")
                };
                self.send(Msg::Stack(StackMsg::Validated { path, result }));
                Ok(String::new())
            }
        }
    }
}

/// Vytvoří složku stacku a zapíše `compose.yaml` a `.env` (po kontrole `config -q`). Vrací absolutní cestu.
pub fn create_stack(distro: &str, cli: &[String], spec: &NewStackSpec) -> Result<String, String> {
    let project = spec.project.trim();
    if !crate::compose_templates::valid_project(project) {
        return Err(t!(
            "Neplatný název projektu – jen malá písmena, číslice, - a _ (začíná písmenem nebo číslicí)",
            "Invalid project name – only lowercase letters, digits, - and _ (starting with a letter or digit)"
        )
        .into());
    }
    let raw = spec.dir.trim().trim_matches('"');
    let raw = if raw.is_empty() { format!("~/stacks/{project}") } else { raw.to_string() };
    let path = if is_windows_path(&raw) {
        wsl::to_linux_path(distro, &raw)?
    } else if raw.starts_with('/') || raw.starts_with('~') {
        raw
    } else {
        return Err(t!("Zadej absolutní cestu (Windows, /… nebo ~/…)", "Enter an absolute path (Windows, /… or ~/…)").into());
    };
    let out = wsl::run(distro, None, None, &["sh", "-c", RESOLVE_SCRIPT, "sh", &path])?;
    let line = out.lines().map(str::trim).find(|l| l.starts_with("PATH ") || l.starts_with("EXISTS ")).unwrap_or("");
    if let Some(f) = line.strip_prefix("EXISTS ") {
        return Err(tf!("Ve složce {path} už je {f} – zvol jinou složku", "{path} already contains {f} – choose another folder"));
    }
    let dir = line.strip_prefix("PATH ").filter(|p| p.starts_with('/')).ok_or_else(|| tf!("Cestu {path} nelze zpracovat", "Cannot resolve path {path}"))?.to_string();
    if dir.trim_end_matches('/').is_empty() {
        return Err(t!("Kořenová složka není povolená", "The root folder is not allowed").into());
    }
    // kontrola ještě před vytvořením složky (compose ji k tomu nepotřebuje)
    validate(distro, cli, project, &dir, &["-".into()], Some(&spec.env), &spec.compose)
        .map_err(|e| tf!("Kontrola compose souboru selhala:\n{e}", "The compose file check failed:\n{e}"))?;
    crate::fs::mkdir(distro, false, &dir)?;
    let compose = crate::fs::join(&dir, "compose.yaml");
    let env = crate::fs::join(&dir, ".env");
    crate::fs::write_text(distro, false, &env, &spec.env)?;
    // hesla – jen pro vlastníka
    let _ = crate::fs::chmod(distro, false, "600", &env);
    crate::fs::write_text(distro, false, &compose, &spec.compose)?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_podman_noise() {
        let s = "\u{1b}[4m>>>> Executing external compose provider \"/usr/libexec/docker/cli-plugins/docker-compose\". Please see podman-compose(1). <<<<\n\n\u{1b}[0myaml: line 3: bad\nError: executing /usr/libexec/docker/cli-plugins/docker-compose -p x config -q: exit status 1\n";
        assert_eq!(clean_output(s), "yaml: line 3: bad");
    }

    #[test]
    fn check_args_pass_values_as_argv() {
        let cli = vec!["docker".to_string()];
        let a = check_args(&cli, "p", "/s", &["-".into()], None);
        assert_eq!(a, ["docker", "compose", "-p", "p", "--project-directory", "/s", "-f", "-", "config", "-q"]);
        let env = "A=$(rm -rf /)\n";
        let a = check_args(&cli, "p", "/s", &[], Some(env));
        assert_eq!(&a[..3], ["sh", "-c", ENV_CHECK_SCRIPT]);
        assert_eq!(a[4], env);
        assert!(!ENV_CHECK_SCRIPT.contains("A="));
        assert_eq!(&a[5..], ["docker", "compose", "-p", "p", "--project-directory", "/s"]);
    }
}
