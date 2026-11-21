//! Ovládání WSL přes wsl.exe (bez Docker CLI na Windows).

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

pub use crate::host::decode;
pub use crate::error::WslError;

fn finish(out: std::process::Output) -> Result<String, WslError> {
    crate::host::finish(crate::host::ExecOut {
        code: out.status.code().unwrap_or(-1),
        stdout: out.stdout,
        stderr: out.stderr,
    })
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

/// Záznam do přehledu „Na pozadí“ (distribuce, uživatel, zkrácený příkaz).
fn track(distro: &str, user: Option<&str>, args: &[&str]) -> crate::activity::Guard {
    let title = match user {
        Some(u) => format!("{distro} ({u})"),
        None => distro.to_string(),
    };
    crate::activity::start(crate::activity::Kind::Wsl, title, crate::activity::describe_cmd(args, 160))
}

/// Spustí program v distribuci a vrátí stdout (+stderr).
pub fn run(distro: &str, user: Option<&str>, cwd: Option<&str>, args: &[&str]) -> Result<String, WslError> {
    let mut g = track(distro, user, args);
    let out = base(distro, user, cwd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string());
    let r = out.map_err(WslError::from).and_then(finish);
    g.set_ok(r.is_ok());
    r
}

#[cfg(test)]
pub fn run_owned(distro: &str, user: Option<&str>, cwd: Option<&str>, args: &[String]) -> Result<String, WslError> {
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run(distro, user, cwd, &a)
}

/// Společná logika streamování: spustí `cmd` (stdin null), předává řádky a vrací kód.
/// Bez záznamu do přehledu „Na pozadí“ – ten řeší volající.
pub(crate) fn stream_command(
    cmd: &mut Command,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    on_line: &mut dyn FnMut(&str),
) -> Result<i32, String> {
    use std::io::{BufRead, BufReader, Read};
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    fn pump(r: impl Read + Send + 'static, tx: std::sync::mpsc::Sender<String>) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let mut r = BufReader::new(r);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match r.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let s = String::from_utf8_lossy(&buf);
                        let s = s.trim_end_matches(['\n', '\r']);
                        let s = s.rsplit('\r').next().unwrap_or("");
                        if tx.send(s.to_string()).is_err() {
                            break;
                        }
                    }
                }
            }
        })
    }
    let mut readers = Vec::new();
    if let Some(o) = child.stdout.take() {
        readers.push(pump(o, tx.clone()));
    }
    if let Some(e) = child.stderr.take() {
        readers.push(pump(e, tx.clone()));
    }
    drop(tx);
    // končí, až oba proudy dojdou (všechny odesílatele zahodí čtecí vlákna)
    let mut killed = false;
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(200)) {
            Ok(line) => on_line(&line),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if !killed && cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::SeqCst)) {
                    killed = true;
                    let _ = child.kill();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    for r in readers {
        let _ = r.join();
    }
    let st = child.wait().map_err(|e| e.to_string())?;
    let code = st.code().unwrap_or(-1);
    Ok(code)
}

fn wsl_simple(args: &[&str]) -> Result<String, WslError> {
    finish(hidden("wsl.exe").args(args).output().map_err(|e| e.to_string())?)
}

pub fn start(distro: &str) -> Result<String, WslError> {
    run(distro, None, None, &["true"])
}

pub fn terminate(distro: &str) -> Result<String, WslError> {
    wsl_simple(&["--terminate", distro])
}

pub fn shutdown() -> Result<String, WslError> {
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
pub fn install_distro(distro: &str, name: &str) -> Result<String, WslError> {
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
    fn wt_escapes_semicolons() {
        assert_eq!(super::wt_escape("a; calc.exe"), r"a\; calc.exe");
    }

    #[test]
    fn parse() {
        let t = "Následuje seznam…\n\nNAME                            FRIENDLY NAME\nUbuntu                          Ubuntu\nDebian                          Debian GNU/Linux\nFedoraLinux-44                  Fedora Linux 44\n";
        let v = super::parse_online(t);
        assert_eq!(v.len(), 3);
        assert_eq!(v[1], ("Debian".to_string(), "Debian GNU/Linux".to_string()));
        assert_eq!(v[2].1, "Fedora Linux 44");
    }
}

pub fn set_default(distro: &str) -> Result<String, WslError> {
    wsl_simple(&["--set-default", distro])
}

/// Skrytý proces, který drží distribuci (a tím i dockerd) naživu.
/// Díky flock běží v distribuci vždy nejvýš jeden – další instance hned skončí.
const KEEPALIVE_SCRIPT: &str = "if command -v flock >/dev/null 2>&1; then exec flock -n /tmp/.wsl-container-desktop-keepalive.lock sleep infinity; else exec sleep infinity; fi";

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
    // wt.exe bere `;` jako oddělovač vlastních příkazů – v argumentech (název kontejneru,
    // složka z labelu compose…) ho musíme escapovat, jinak by šlo spustit cokoli ve Windows
    let mut wt = Command::new("wt.exe");
    wt.args(["-w", "0", "nt", "--title", &wt_escape(title), "wsl.exe"]).args(wsl_args.iter().map(|a| wt_escape(a)));
    if wt.spawn().is_ok() {
        return Ok(String::new());
    }
    // fallback bez cmd.exe (to by vykládalo & | > …): wsl.exe přímo v novém okně konzole
    let mut c = Command::new("wsl.exe");
    c.args(wsl_args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0000_0010); // CREATE_NEW_CONSOLE
    }
    c.spawn().map(|_| String::new()).map_err(|e| e.to_string())
}

/// Escapuje `;` pro příkazovou řádku Windows Terminalu.
fn wt_escape(s: &str) -> String {
    s.replace(';', "\\;")
}

/// Linuxová cesta -> UNC cesta pro Windows (\\wsl.localhost\Distro\...).
pub fn unc(distro: &str, linux_path: &str) -> String {
    format!(r"\\wsl.localhost\{}{}", distro, linux_path.replace('/', r"\"))
}

/// Dá se linuxová cesta převést na UNC beze změny významu? Win32 u každé části odřízne tečky
/// a mezery na konci, `\` bere jako oddělovač a `:` jako alternativní datový proud – `x.hta.`
/// by v Linuxu byla složka, ve Windows ale soubor `x.hta` (N-02).
pub fn unc_safe(linux_path: &str) -> bool {
    linux_path.starts_with('/')
        && linux_path.split('/').filter(|c| !c.is_empty()).all(|c| {
            c != "."
                && c != ".."
                && !c.ends_with('.')
                && !c.ends_with(' ')
                && !c.chars().any(|ch| ch < ' ' || "\\:<>\"|?*".contains(ch))
        })
}

/// UNC cesta ke složce v distribuci pro explorer.exe – jen když je cesta jednoznačná a ve Windows
/// je to opravdu složka (soubor by explorer.exe otevřel přidruženým programem).
pub fn explorer_dir(distro: &str, linux_path: &str) -> Result<String, String> {
    if !unc_safe(linux_path) {
        return Err(tf!("{linux_path}: název složky nejde bezpečně otevřít ve Windows", "{linux_path}: the folder name cannot be opened safely in Windows"));
    }
    let p = unc(distro, linux_path);
    if !std::path::Path::new(&p).is_dir() {
        return Err(tf!("{linux_path} není složka v {distro}", "{linux_path} is not a folder in {distro}"));
    }
    Ok(p)
}

/// Převede Windows cestu na linuxovou (wslpath), linuxovou nechá být.
pub fn to_linux_path(distro: &str, path: &str) -> Result<String, String> {
    let p = path.trim().trim_matches('"');
    if p.starts_with('/') || p.starts_with('~') {
        return Ok(p.to_string());
    }
    Ok(run(distro, None, None, &["wslpath", "-a", p])?.trim().to_string())
}

// --- správa distribucí: odebrání, export, import, disk ---

/// Platný název nové distribuce pro `wsl --import`: neprázdný, jen `[A-Za-z0-9._-]`, max. 64 znaků.
pub fn valid_distro_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Formát exportu distribuce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportFormat {
    #[default]
    Tar,
    Vhd,
}

impl ExportFormat {
    pub fn ext(self) -> &'static str {
        match self {
            ExportFormat::Tar => "tar",
            ExportFormat::Vhd => "vhdx",
        }
    }

    /// Formát podle přípony souboru (.vhd/.vhdx = Vhd, jinak tar).
    pub fn from_path(path: &str) -> Self {
        let p = path.trim().to_ascii_lowercase();
        if p.ends_with(".vhdx") || p.ends_with(".vhd") { ExportFormat::Vhd } else { ExportFormat::Tar }
    }
}

/// Text nápovědy `wsl --help` (načte se jednou) – podle něj se poznají podporované přepínače.
fn help_text() -> &'static str {
    static HELP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HELP.get_or_init(|| {
        hidden("wsl.exe")
            .arg("--help")
            .output()
            .map(|o| format!("{}{}", decode(&o.stdout), decode(&o.stderr)))
            .unwrap_or_default()
    })
}

/// Umí `wsl --export` přepínač `--format` (novější WSL), nebo jen starší `--vhd`?
pub fn export_has_format_flag() -> bool {
    let h = help_text();
    h.is_empty() || h.contains("--format")
}

/// Nabízí WSL `wsl --manage … --set-sparse` / `--compact`?
pub fn manage_support() -> (bool, bool) {
    let h = help_text();
    (h.contains("--set-sparse"), h.contains("--compact"))
}

/// Argumenty `wsl --export` (každý jako samostatná položka argv).
pub fn export_args(distro: &str, file: &str, format: ExportFormat, format_flag: bool) -> Vec<String> {
    let mut v = vec!["--export".to_string(), distro.to_string(), file.to_string()];
    if format == ExportFormat::Vhd {
        if format_flag {
            v.extend(["--format".to_string(), "vhd".to_string()]);
        } else {
            v.push("--vhd".to_string());
        }
    }
    v
}

/// Argumenty `wsl --import` (formát se pozná podle přípony souboru).
pub fn import_args(distro: &str, folder: &str, file: &str) -> Vec<String> {
    let mut v = vec!["--import".to_string(), distro.to_string(), folder.to_string(), file.to_string()];
    if ExportFormat::from_path(file) == ExportFormat::Vhd {
        v.push("--vhd".to_string());
    }
    v
}

/// Argumenty `wsl --manage <distro> …` pro kompaktaci / zhuštěný disk.
pub fn manage_args(distro: &str, compact: bool) -> Vec<String> {
    let mut v = vec!["--manage".to_string(), distro.to_string()];
    if compact {
        v.push("--compact".to_string());
    } else {
        v.extend(["--set-sparse".to_string(), "true".to_string()]);
    }
    v
}

fn wsl_owned(args: &[String]) -> Result<String, WslError> {
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    wsl_simple(&a)
}

/// Zruší registraci distribuce a smaže její disk (nevratné).
pub fn unregister(distro: &str) -> Result<String, WslError> {
    wsl_simple(&["--unregister", distro])
}

/// Exportuje distribuci do souboru (může trvat minuty).
pub fn export(distro: &str, file: &str, format: ExportFormat) -> Result<String, WslError> {
    wsl_owned(&export_args(distro, file, format, export_has_format_flag()))
}

/// Importuje distribuci ze souboru .tar / .tar.gz / .vhdx do složky.
pub fn import(distro: &str, folder: &str, file: &str) -> Result<String, WslError> {
    wsl_owned(&import_args(distro, folder, file))
}

/// `wsl --manage …`; disk bývá chvíli po `--terminate` ještě připojený (ERROR_SHARING_VIOLATION),
/// proto se několikrát zopakuje. Na WSL 2.x/3.x zůstává disk spuštěné distribuce připojený
/// k VM často až do `wsl --shutdown` – pak chyba propadne volajícímu.
fn manage(args: &[String]) -> Result<String, WslError> {
    let mut res = wsl_owned(args);
    for _ in 0..3 {
        match &res {
            Err(e) if e.contains("SHARING_VIOLATION") => {
                std::thread::sleep(std::time::Duration::from_secs(2));
                res = wsl_owned(args);
            }
            _ => break,
        }
    }
    res
}

/// Zapne zhuštěný (sparse) disk – WSL pak samo vrací uvolněné místo (distribuce musí být ukončená).
pub fn set_sparse(distro: &str) -> Result<String, WslError> {
    manage(&manage_args(distro, false))
}

/// Zkomprimuje VHDX distribuce (distribuce musí být ukončená, disk nesmí být zhuštěný).
pub fn compact(distro: &str) -> Result<String, WslError> {
    manage(&manage_args(distro, true))
}

/// Je soubor zhuštěný (FILE_ATTRIBUTE_SPARSE_FILE)?
pub fn is_sparse(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x200 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        false
    }
}

/// Cesta k virtuálnímu disku distribuce (BasePath + VhdFileName z registru Lxss).
pub fn vhd_path(distro: &str) -> Option<std::path::PathBuf> {
    lxss::find(distro).map(|(base, file)| {
        let base = base.strip_prefix(r"\\?\").unwrap_or(&base).to_string();
        std::path::PathBuf::from(base).join(if file.is_empty() { "ext4.vhdx".to_string() } else { file })
    })
}

#[cfg(windows)]
mod lxss {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ,
    };

    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Lxss";

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn get_sz(key: HKEY, name: &str) -> Option<String> {
        let n = w(name);
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        let r = unsafe {
            RegGetValueW(key, std::ptr::null(), n.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len)
        };
        if r != 0 {
            return None;
        }
        let chars = (len as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..chars.min(buf.len())]))
    }

    /// (BasePath, VhdFileName) distribuce podle DistributionName.
    pub fn find(distro: &str) -> Option<(String, String)> {
        let k = w(KEY);
        let mut root: HKEY = std::ptr::null_mut();
        if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, k.as_ptr(), 0, KEY_READ, &mut root) } != 0 {
            return None;
        }
        let mut found = None;
        let mut i = 0u32;
        loop {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            let r = unsafe {
                RegEnumKeyExW(
                    root,
                    i,
                    name.as_mut_ptr(),
                    &mut len,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if r != 0 {
                break;
            }
            i += 1;
            let mut sub: HKEY = std::ptr::null_mut();
            if unsafe { RegOpenKeyExW(root, name.as_ptr(), 0, KEY_READ, &mut sub) } != 0 {
                continue;
            }
            if get_sz(sub, "DistributionName").map(|d| d.eq_ignore_ascii_case(distro)).unwrap_or(false) {
                found = get_sz(sub, "BasePath").map(|b| (b, get_sz(sub, "VhdFileName").unwrap_or_default()));
            }
            unsafe { RegCloseKey(sub) };
            if found.is_some() {
                break;
            }
        }
        unsafe { RegCloseKey(root) };
        found
    }
}

#[cfg(not(windows))]
mod lxss {
    pub fn find(_: &str) -> Option<(String, String)> {
        None
    }
}

#[cfg(test)]
mod manage_tests {
    use super::*;

    #[test]
    fn unc_safe_rejects_win32_normalization() {
        assert!(unc_safe("/home/u/stacks/my app"));
        assert!(unc_safe("/srv/app.v2/x"));
        assert!(!unc_safe("srv/app"), "jen absolutní");
        assert!(!unc_safe("/srv/app/x.hta."), "tečka na konci");
        assert!(!unc_safe("/srv/app/x.hta "), "mezera na konci");
        assert!(!unc_safe("/srv/a\\..\\x.hta"), "zpětné lomítko");
        assert!(!unc_safe("/srv/x.hta:stream"));
        assert!(!unc_safe("/srv/../etc"));
        assert!(!unc_safe("/srv/a\nb"));
        // nejednoznačný název → chyba bez přístupu na disk (existenci ověřuje live_explorer_dir)
        assert!(explorer_dir("WSLCD-No-Such-Distro", "/srv/x.hta.").is_err());
    }

    /// N-02 živě: složka `x.hta.` projde `test -d` v Linuxu, ve Windows ale vede na soubor `x.hta`.
    /// `cargo test live_explorer_dir -- --ignored` (WSLCD_DISTRO, výchozí Debian)
    #[test]
    #[ignore]
    fn live_explorer_dir() {
        let d = std::env::var("WSLCD_DISTRO").unwrap_or_else(|_| "Debian".into());
        let base = format!("/tmp/wslcd-n02-{}", std::process::id());
        run(&d, None, None, &["sh", "-c", "mkdir -p \"$1/ok\" \"$1/x.hta.\" && echo x > \"$1/x.hta\"", "sh", &base]).unwrap();
        assert!(run(&d, None, None, &["test", "-d", &format!("{base}/x.hta.")]).is_ok(), "Linux: je to složka");
        let bad = explorer_dir(&d, &format!("{base}/x.hta."));
        let ok = explorer_dir(&d, &format!("{base}/ok"));
        let _ = run(&d, None, None, &["rm", "-rf", "--", &base]);
        assert!(bad.is_err(), "{bad:?}");
        assert!(ok.is_ok(), "{ok:?}");
    }

    #[test]
    fn names() {
        assert!(valid_distro_name("WSLCD-Test-Import"));
        assert!(valid_distro_name("debian_12.dev"));
        assert!(!valid_distro_name(""));
        assert!(!valid_distro_name("a b"));
        assert!(!valid_distro_name("a;calc"));
        assert!(!valid_distro_name("Ubuntu/x"));
        assert!(!valid_distro_name("č"));
        assert!(!valid_distro_name(&"a".repeat(65)));
    }

    #[test]
    fn export_argv() {
        assert_eq!(export_args("Deb", r"C:\b k\x.tar", ExportFormat::Tar, true), ["--export", "Deb", r"C:\b k\x.tar"]);
        assert_eq!(export_args("Deb", "x.vhdx", ExportFormat::Vhd, true), ["--export", "Deb", "x.vhdx", "--format", "vhd"]);
        assert_eq!(export_args("Deb", "x.vhdx", ExportFormat::Vhd, false), ["--export", "Deb", "x.vhdx", "--vhd"]);
    }

    #[test]
    fn import_argv() {
        assert_eq!(import_args("N", r"D:\wsl\N", r"D:\a.tar"), ["--import", "N", r"D:\wsl\N", r"D:\a.tar"]);
        assert_eq!(import_args("N", "f", r"D:\A.VHDX"), ["--import", "N", "f", r"D:\A.VHDX", "--vhd"]);
        assert_eq!(import_args("N", "f", "a.tar.gz").len(), 4);
    }

    #[test]
    fn manage_argv() {
        assert_eq!(manage_args("N", false), ["--manage", "N", "--set-sparse", "true"]);
        assert_eq!(manage_args("N", true), ["--manage", "N", "--compact"]);
    }

    #[test]
    fn format_from_path() {
        assert_eq!(ExportFormat::from_path("a.vhd"), ExportFormat::Vhd);
        assert_eq!(ExportFormat::from_path("a.tar"), ExportFormat::Tar);
        assert_eq!(ExportFormat::Vhd.ext(), "vhdx");
    }
}
