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
        a.extend(self.command.split_whitespace().map(String::from));
        a
    }
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
}
