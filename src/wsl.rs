//! Ovládání WSL přes wsl.exe (bez Docker CLI na Windows).

use std::io::Write;
use std::process::{Child, Command, Stdio};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Default)]
pub struct Distro {
    pub name: String,
    pub state: String,
    pub version: String,
    pub default: bool,
}

impl Distro {
    pub fn running(&self) -> bool {
        self.state.eq_ignore_ascii_case("running")
    }
}

/// Příkaz bez viditelného konzolového okna.
pub fn hidden(program: &str) -> Command {
    let mut c = Command::new(program);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    c.env("WSL_UTF8", "1");
    c
}

/// wsl.exe vrací podle verze buď UTF-8, nebo UTF-16LE.
pub fn decode(bytes: &[u8]) -> String {
    let zeros = bytes.iter().filter(|b| **b == 0).count();
    if bytes.len() >= 2 && zeros * 4 > bytes.len() {
        let u16s: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u16s).replace('\u{feff}', "")
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn finish(out: std::process::Output) -> Result<String, String> {
    let stdout = decode(&out.stdout);
    let stderr = decode(&out.stderr);
    if out.status.success() {
        let mut s = stdout;
        if !stderr.trim().is_empty() {
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            s.push_str(&stderr);
        }
        Ok(s)
    } else {
        let msg = if stderr.trim().is_empty() { stdout } else { stderr };
        Err(format!("{} (kód {})", msg.trim(), out.status.code().unwrap_or(-1)))
    }
}

pub fn list() -> Result<Vec<Distro>, String> {
    let out = hidden("wsl.exe")
        .args(["-l", "-v"])
        .output()
        .map_err(|e| e.to_string())?;
    let text = decode(&out.stdout);
    let mut v = Vec::new();
    for line in text.lines().skip(1) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let default = line.starts_with('*');
        let line = line.trim_start_matches('*').trim();
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let n = parts.len();
        v.push(Distro {
            name: parts[..n - 2].join(" "),
            state: parts[n - 2].to_string(),
            version: parts[n - 1].to_string(),
            default,
        });
    }
    Ok(v)
}

pub(crate) fn base(distro: &str, user: Option<&str>, cwd: Option<&str>) -> Command {
    let mut c = hidden("wsl.exe");
    c.args(["-d", distro]);
    if let Some(u) = user {
        c.args(["-u", u]);
    }
    if let Some(d) = cwd {
        c.args(["--cd", d]);
    }
    c.arg("-e");
    c
}

/// Spustí program v distribuci a vrátí stdout (+stderr).
pub fn run(distro: &str, user: Option<&str>, cwd: Option<&str>, args: &[&str]) -> Result<String, String> {
    let out = base(distro, user, cwd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    finish(out)
}

pub fn run_owned(distro: &str, user: Option<&str>, cwd: Option<&str>, args: &[String]) -> Result<String, String> {
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run(distro, user, cwd, &a)
}

/// Spustí program a pošle mu `input` na stdin.
pub fn run_stdin(distro: &str, user: Option<&str>, args: &[&str], input: &str) -> Result<String, String> {
    let mut child = base(distro, user, None)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut si) = child.stdin.take() {
        si.write_all(input.as_bytes()).map_err(|e| e.to_string())?;
    }
    finish(child.wait_with_output().map_err(|e| e.to_string())?)
}

fn wsl_simple(args: &[&str]) -> Result<String, String> {
    finish(hidden("wsl.exe").args(args).output().map_err(|e| e.to_string())?)
}

pub fn start(distro: &str) -> Result<String, String> {
    run(distro, None, None, &["true"])
}

pub fn terminate(distro: &str) -> Result<String, String> {
    wsl_simple(&["--terminate", distro])
}

pub fn shutdown() -> Result<String, String> {
    wsl_simple(&["--shutdown"])
}

/// Distribuce dostupné k instalaci (`wsl --list --online`): (název, popis).
pub fn list_online() -> Result<Vec<(String, String)>, String> {
    let out = hidden("wsl.exe").args(["--list", "--online"]).output().map_err(|e| e.to_string())?;
    let text = decode(&out.stdout);
    Ok(parse_online(&text))
}

/// Rozebere výstup `wsl --list --online` (řádky pod hlavičkou NAME … FRIENDLY NAME).
pub fn parse_online(text: &str) -> Vec<(String, String)> {
    let mut started = false;
    let mut v = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if !started {
            started = t.starts_with("NAME");
            continue;
        }
        if t.is_empty() {
            continue;
        }
        let mut it = t.splitn(2, char::is_whitespace);
        let name = it.next().unwrap_or("").to_string();
        let friendly = it.next().unwrap_or("").trim().to_string();
        if !name.is_empty() {
            v.push((name, friendly));
        }
    }
    v
}

/// Nainstaluje distribuci bez interaktivního prvního spuštění (může trvat minuty – stahování).
pub fn install_distro(distro: &str, name: &str) -> Result<String, String> {
    let mut c = hidden("wsl.exe");
    c.args(["--install", distro, "--no-launch"]);
    if !name.is_empty() && name != distro {
        c.args(["--name", name]);
    }
    finish(c.output().map_err(|e| e.to_string())?)
}

#[cfg(test)]
mod online_tests {
    #[test]
    fn parse() {
        let t = "Následuje seznam…\n\nNAME                            FRIENDLY NAME\nUbuntu                          Ubuntu\nDebian                          Debian GNU/Linux\nFedoraLinux-44                  Fedora Linux 44\n";
        let v = super::parse_online(t);
        assert_eq!(v.len(), 3);
        assert_eq!(v[1], ("Debian".to_string(), "Debian GNU/Linux".to_string()));
        assert_eq!(v[2].1, "Fedora Linux 44");
    }
}

pub fn set_default(distro: &str) -> Result<String, String> {
    wsl_simple(&["--set-default", distro])
}

/// Skrytý proces, který drží distribuci (a tím i dockerd) naživu.
/// Díky flock běží v distribuci vždy nejvýš jeden – další instance hned skončí.
const KEEPALIVE_SCRIPT: &str = "if command -v flock >/dev/null 2>&1; then exec flock -n /tmp/.wls-desktop-keepalive.lock sleep infinity; else exec sleep infinity; fi";

pub fn keepalive(distro: &str) -> Result<Child, String> {
    hidden("wsl.exe")
        .args(["-d", distro, "-e", "sh", "-c", KEEPALIVE_SCRIPT])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())
}

/// Otevře nový tab ve Windows Terminalu (fallback: nové okno konzole).
pub fn open_terminal(title: &str, wsl_args: &[String]) -> Result<String, String> {
    let mut wt = Command::new("wt.exe");
    wt.args(["-w", "0", "nt", "--title", title, "wsl.exe"]).args(wsl_args);
    if wt.spawn().is_ok() {
        return Ok(String::new());
    }
    let mut c = Command::new("cmd.exe");
    c.args(["/c", "start", title, "wsl.exe"]).args(wsl_args);
    c.spawn().map(|_| String::new()).map_err(|e| e.to_string())
}

/// Linuxová cesta -> UNC cesta pro Windows (\\wsl.localhost\Distro\...).
pub fn unc(distro: &str, linux_path: &str) -> String {
    format!(r"\\wsl.localhost\{}{}", distro, linux_path.replace('/', r"\"))
}

/// Převede Windows cestu na linuxovou (wslpath), linuxovou nechá být.
pub fn to_linux_path(distro: &str, path: &str) -> Result<String, String> {
    let p = path.trim().trim_matches('"');
    if p.starts_with('/') || p.starts_with('~') {
        return Ok(p.to_string());
    }
    Ok(run(distro, None, None, &["wslpath", "-a", p])?.trim().to_string())
}
