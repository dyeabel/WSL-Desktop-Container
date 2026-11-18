//! Parametry akcí zadávané z UI (spuštění kontejneru, sestavení image, přidání distribuce).

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
    pub fn args_for(&self, engine: Engine, port: u16, tls: bool) -> Vec<String> {
        self.args_with(engine.cli(port, tls), engine == Engine::Podman)
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
    /// API enginu zabezpečit pomocí TLS (jen s use_for_app a enginem)
    pub tls: bool,
}

/// Parametry sestavení image z Dockerfile (`docker build`).
///
/// Cesty mohou být windowsové i linuxové – převod přes `wsl::to_linux_path`
/// dělá až worker při spuštění (viz `worker::build`), spec drží to, co zadal uživatel.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BuildSpec {
    /// složka kontextu (povinná); build běží s ní jako pracovním adresářem
    pub context: String,
    /// cesta k Dockerfile – relativní ke kontextu, nebo absolutní (prázdné = `Dockerfile`)
    pub dockerfile: String,
    /// tagy oddělené mezerou / čárkou / novým řádkem
    pub tags: String,
    /// `KLIC=hodnota` po řádcích
    pub build_args: String,
    /// cílová fáze (`--target`)
    pub target: String,
    pub no_cache: bool,
    pub pull: bool,
}

/// Absolutní windowsová cesta (`C:\…`, `C:/…`, `\\server\…`).
pub fn is_windows_path(p: &str) -> bool {
    let b = p.trim().trim_matches('"').as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || p.trim().starts_with(r"\\")
}

impl BuildSpec {
    /// Seznam tagů (bez prázdných).
    pub fn tag_list(&self) -> Vec<String> {
        self.tags.split([',', ' ', '\n', '\t', ';']).map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
    }

    /// Dockerfile tak, jak se předá `-f` (None = výchozí `Dockerfile` v kontextu).
    /// Relativní cesta se zapisuje s `/` (uživatel mohl napsat `docker\Dockerfile`).
    pub fn dockerfile_arg(&self) -> Option<String> {
        let d = self.dockerfile.trim().trim_matches('"');
        if d.is_empty() || d == "Dockerfile" || d == "./Dockerfile" {
            return None;
        }
        if is_windows_path(d) || d.starts_with('/') {
            Some(d.to_string())
        } else {
            Some(d.replace('\\', "/"))
        }
    }

    /// Náhled příkazu (vždy jako `docker build …` s cestou ke kontextu na konci).
    pub fn args(&self) -> Vec<String> {
        let ctx = self.context.trim().trim_matches('"');
        self.args_with(vec!["docker".into()], if ctx.is_empty() { "." } else { ctx })
    }

    /// Skutečný příkaz pro daný engine. Spouští se v adresáři kontextu, proto kontext = `.`;
    /// absolutní `-f` musí být už linuxová cesta (převádí worker).
    pub fn args_for(&self, engine: Engine, port: u16, tls: bool) -> Vec<String> {
        self.args_with(engine.cli(port, tls), ".")
    }

    fn args_with(&self, cli: Vec<String>, context: &str) -> Vec<String> {
        let mut a = cli;
        a.push("build".into());
        if let Some(f) = self.dockerfile_arg() {
            a.extend(["-f".into(), f]);
        }
        for t in self.tag_list() {
            a.extend(["-t".into(), t]);
        }
        for b in self.build_args.lines().map(str::trim).filter(|s| !s.is_empty()) {
            a.extend(["--build-arg".into(), b.into()]);
        }
        if !self.target.trim().is_empty() {
            a.extend(["--target".into(), self.target.trim().into()]);
        }
        if self.no_cache {
            a.push("--no-cache".into());
        }
        if self.pull {
            a.push("--pull".into());
        }
        a.push(context.into());
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bs(dockerfile: &str, tags: &str, build_args: &str) -> BuildSpec {
        BuildSpec { context: r"C:\src\app".into(), dockerfile: dockerfile.into(), tags: tags.into(), build_args: build_args.into(), ..Default::default() }
    }

    #[test]
    fn build_spec_basic_preview() {
        let s = bs("", "app:1, app:latest  registry.local/app:1", "");
        assert_eq!(s.args().join(" "), r"docker build -t app:1 -t app:latest -t registry.local/app:1 C:\src\app");
        assert_eq!(s.tag_list(), vec!["app:1", "app:latest", "registry.local/app:1"]);
    }

    #[test]
    fn build_spec_engine_args() {
        let mut s = bs("Dockerfile", "web", "");
        s.target = " prod ".into();
        s.no_cache = true;
        s.pull = true;
        assert_eq!(
            s.args_for(Engine::Podman, 2375, false),
            ["podman", "--url", "tcp://127.0.0.1:2375", "build", "-t", "web", "--target", "prod", "--no-cache", "--pull", "."]
        );
        assert_eq!(s.args_for(Engine::Docker, 2375, false)[..2], ["docker".to_string(), "build".to_string()]);
    }

    #[test]
    fn build_spec_build_args_with_spaces() {
        let s = bs("", "", "GREETING=hello world\n\n  VERSION=1.2  \nEMPTY=");
        let a = s.args_for(Engine::Docker, 2375, false);
        assert_eq!(
            a,
            ["docker", "build", "--build-arg", "GREETING=hello world", "--build-arg", "VERSION=1.2", "--build-arg", "EMPTY=", "."]
        );
    }

    #[test]
    fn build_spec_dockerfile_relative_vs_absolute() {
        // relativní ke kontextu – zpětná lomítka na dopředná
        let a = bs(r"docker\prod.Dockerfile", "x", "").args_for(Engine::Docker, 2375, false);
        assert_eq!(a[1..4], ["build", "-f", "docker/prod.Dockerfile"]);
        // absolutní linuxová zůstává
        let a = bs("/home/u/df/Dockerfile", "x", "").args_for(Engine::Docker, 2375, false);
        assert_eq!(a[2..4], ["-f", "/home/u/df/Dockerfile"]);
        // absolutní windowsová se v spec nepřevádí (to dělá worker)
        let a = bs(r"D:\df\Dockerfile", "x", "").args_for(Engine::Docker, 2375, false);
        assert_eq!(a[2..4], ["-f", r"D:\df\Dockerfile"]);
        // výchozí název se nepředává
        assert!(!bs("./Dockerfile", "x", "").args().contains(&"-f".to_string()));
    }

    #[test]
    fn windows_path_detection() {
        assert!(is_windows_path(r"C:\x"));
        assert!(is_windows_path("d:/x"));
        assert!(is_windows_path(r"\\wsl.localhost\Debian\home"));
        assert!(!is_windows_path("/mnt/c/x"));
        assert!(!is_windows_path("Dockerfile"));
        assert!(!is_windows_path("~/app"));
    }

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
