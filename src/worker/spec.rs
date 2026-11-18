//! Parametry akcí zadávané z UI (spuštění kontejneru, přidání distribuce).

use crate::engine::{qualify_image, Engine};

#[derive(Debug, Clone, Default)]
pub struct RunSpec {
    pub image: String,
    pub name: String,
    pub ports: String,
    pub env: String,
    pub volumes: String,
    pub network: String,
    pub restart: String,
    pub command: String,
    /// spustit interaktivně ve Windows Terminalu (`run -it --rm`) místo na pozadí
    pub interactive: bool,
}

impl RunSpec {
    /// Náhled příkazu (vždy jako `docker run …`).
    pub fn args(&self) -> Vec<String> {
        self.args_with(vec!["docker".into()], false)
    }

    /// Skutečný příkaz pro daný engine (Podman: plně kvalifikovaný image).
    pub fn args_for(&self, engine: Engine, port: u16) -> Vec<String> {
        self.args_with(engine.cli(port), engine == Engine::Podman)
    }

    fn args_with(&self, cli: Vec<String>, qualify: bool) -> Vec<String> {
        let mut a: Vec<String> = cli;
        if self.interactive {
            a.extend(["run".into(), "-it".into(), "--rm".into()]);
        } else {
            a.extend(["run".into(), "-d".into()]);
        }
        if !self.name.trim().is_empty() {
            a.extend(["--name".into(), self.name.trim().into()]);
        }
        for p in self.ports.split([',', ' ', '\n']).map(str::trim).filter(|s| !s.is_empty()) {
            a.extend(["-p".into(), p.into()]);
        }
        for e in self.env.lines().map(str::trim).filter(|s| !s.is_empty()) {
            a.extend(["-e".into(), e.into()]);
        }
        for v in self.volumes.lines().map(str::trim).filter(|s| !s.is_empty()) {
            a.extend(["-v".into(), v.into()]);
        }
        if !self.network.trim().is_empty() {
            a.extend(["--network".into(), self.network.trim().into()]);
        }
        if !self.restart.is_empty() && self.restart != "no" {
            a.extend(["--restart".into(), self.restart.clone()]);
        }
        a.push(if qualify { qualify_image(&self.image) } else { self.image.trim().into() });
        a.extend(split_args(&self.command));
        a
    }
}

/// Rozdělí příkaz na argumenty jako shell: mezery oddělují, '…' a "…" seskupují,
/// `\` escapuje (mimo jednoduché uvozovky).
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match c {
            '\'' => {
                has = true;
                for c in it.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    cur.push(c);
                }
            }
            '"' => {
                has = true;
                while let Some(c) = it.next() {
                    match c {
                        '"' => break,
                        '\\' => match it.next() {
                            Some(n @ ('"' | '\\' | '$' | '`')) => cur.push(n),
                            Some(n) => {
                                cur.push('\\');
                                cur.push(n);
                            }
                            None => cur.push('\\'),
                        },
                        _ => cur.push(c),
                    }
                }
            }
            '\\' => {
                has = true;
                if let Some(n) = it.next() {
                    cur.push(n);
                }
            }
            c if c.is_whitespace() => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                has = true;
                cur.push(c);
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

/// Parametry přidání nové distribuce.
#[derive(Debug, Clone, Default)]
pub struct AddDistroSpec {
    /// název z `wsl --list --online` (např. Debian)
    pub distro: String,
    /// název nové instance (prázdné = stejný jako distro)
    pub name: String,
    /// výchozí uživatel (sudo bez hesla)
    pub user: String,
    /// volitelné heslo uživatele
    pub password: String,
    /// engine k instalaci (None = žádný)
    pub engine: Option<Engine>,
    /// přepnout aplikaci na novou distribuci (a nastavit API enginu)
    pub use_for_app: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_spec_args() {
        let s = RunSpec {
            image: "nginx:alpine".into(),
            name: "web".into(),
            ports: "8080:80, 8443:443".into(),
            env: "A=1\nB=2".into(),
            volumes: "data:/data".into(),
            network: "".into(),
            restart: "unless-stopped".into(),
            command: "".into(),
            interactive: false,
        };
        assert_eq!(
            s.args().join(" "),
            "docker run -d --name web -p 8080:80 -p 8443:443 -e A=1 -e B=2 -v data:/data --restart unless-stopped nginx:alpine"
        );
    }

    #[test]
    fn split_command() {
        assert_eq!(split_args("sleep  infinity"), vec!["sleep", "infinity"]);
        assert_eq!(split_args(r#"sh -c 'echo "a b"; x' "c\"d" e\ f ''"#), vec!["sh", "-c", "echo \"a b\"; x", "c\"d", "e f", ""]);
        assert!(split_args("  ").is_empty());
    }
}
