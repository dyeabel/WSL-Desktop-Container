//! Spouštění s Windows: hodnota v HKCU\Software\Microsoft\Windows\CurrentVersion\Run
//! (jen pro aktuálního uživatele, bez práv správce). Aplikace se spouští s `--autostart`,
//! takže se rovnou schová do oznamovací oblasti.
//!
//! Linux: soubor `$XDG_CONFIG_HOME/autostart/wsl-container-desktop.desktop`
//! (výchozí `~/.config/autostart/...`) s `Exec=<exe> --autostart`.

pub const ARG: &str = "--autostart";

/// Byla aplikace spuštěna automaticky po přihlášení?
pub fn launched_at_login() -> bool {
    std::env::args().any(|a| a == ARG)
}

#[cfg(windows)]
fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" {ARG}", exe.display()))
}

// ---------------------------------------------------------------- .desktop soubor (Linux)
// Čisté funkce bez vstupu/výstupu jsou mimo cfg, aby se testovaly i na Windows.

/// Název souboru v `autostart/`.
#[cfg_attr(windows, allow(dead_code))]
const DESKTOP_FILE: &str = "wsl-container-desktop.desktop";

/// Hodnota `Exec`: cesta (podle Desktop Entry Specification v uvozovkách, když obsahuje
/// mezeru nebo speciální znak) a `--autostart`. `%` se zdvojuje (jinak je to zástupný kód).
#[cfg_attr(windows, allow(dead_code))]
fn exec_line(exe: &std::path::Path) -> String {
    let p = exe.display().to_string().replace('%', "%%");
    let special = p.chars().any(|c| " \t\n\"'\\><~|&;$*?#()`".contains(c));
    if !special {
        return format!("{p} {ARG}");
    }
    let mut q = String::from("\"");
    for c in p.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            q.push('\\');
        }
        q.push(c);
    }
    q.push('"');
    format!("{q} {ARG}")
}

/// Obsah `.desktop` souboru pro autostart.
#[cfg_attr(windows, allow(dead_code))]
fn desktop_file_content(exe: &std::path::Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=WSL Container Desktop\nComment=Docker and Podman containers\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exec_line(exe)
    )
}

/// Hodnota `Exec` z obsahu souboru.
#[cfg_attr(windows, allow(dead_code))]
fn exec_of(content: &str) -> Option<String> {
    content.lines().find_map(|l| l.strip_prefix("Exec=")).map(str::to_string)
}

/// Obsah s nahrazeným `Exec`; None, když `Exec` chybí nebo už má požadovanou hodnotu.
#[cfg_attr(windows, allow(dead_code))]
fn with_exec(content: &str, exec: &str) -> Option<String> {
    if exec_of(content).as_deref().is_none_or(|cur| cur == exec) {
        return None;
    }
    let mut out = String::with_capacity(content.len());
    let mut done = false;
    for l in content.split_inclusive('\n') {
        if !done && l.starts_with("Exec=") {
            out.push_str(&format!("Exec={exec}\n"));
            done = true;
        } else {
            out.push_str(l);
        }
    }
    Some(out)
}

#[cfg_attr(windows, allow(dead_code))]
fn file_enabled(f: &std::path::Path) -> bool {
    f.is_file()
}

/// Zapíše (`Some(exe)`) nebo smaže (`None`) autostart soubor; smazání neexistujícího je v pořádku.
#[cfg_attr(windows, allow(dead_code))]
fn file_set(f: &std::path::Path, exe: Option<&std::path::Path>) -> Result<(), String> {
    match exe {
        Some(exe) => {
            if let Some(d) = f.parent() {
                std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
            }
            std::fs::write(f, desktop_file_content(exe)).map_err(|e| e.to_string())
        }
        None => match std::fs::remove_file(f) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        },
    }
}

/// Aktualizuje `Exec` jen v existujícím souboru (nic nevytváří).
#[cfg_attr(windows, allow(dead_code))]
fn file_fix(f: &std::path::Path, exe: &std::path::Path) {
    let Ok(cur) = std::fs::read_to_string(f) else { return };
    if let Some(new) = with_exec(&cur, &exec_line(exe)) {
        let _ = std::fs::write(f, new);
    }
}

#[cfg(not(windows))]
fn desktop_path() -> Option<std::path::PathBuf> {
    crate::config::xdg_config_home(|k| std::env::var_os(k)).map(|c| c.join("autostart").join(DESKTOP_FILE))
}

#[cfg(windows)]
mod reg {
    use windows_sys::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
    };

    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn get(name: &str) -> Option<String> {
        let (k, n) = (w(KEY), w(name));
        let mut buf = vec![0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        let r = unsafe {
            RegGetValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut len)
        };
        if r != 0 {
            return None;
        }
        let chars = (len as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..chars.min(buf.len())]))
    }

    pub fn set(name: &str, value: &str) -> Result<(), String> {
        let (k, n, v) = (w(KEY), w(name), w(value));
        let r = unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), REG_SZ, v.as_ptr().cast(), (v.len() * 2) as u32)
        };
        if r == 0 { Ok(()) } else { Err(format!("RegSetKeyValueW: {r}")) }
    }

    pub fn delete(name: &str) -> Result<(), String> {
        let (k, n) = (w(KEY), w(name));
        let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr()) };
        // 2 = ERROR_FILE_NOT_FOUND (už není)
        if r == 0 || r == 2 { Ok(()) } else { Err(format!("RegDeleteKeyValueW: {r}")) }
    }
}

/// Název hodnoty v registru.
#[cfg(windows)]
const NAME: &str = "WSL Container Desktop";
/// Hodnota z doby před přejmenováním (WLS Desktop) – převede se na novou.
#[cfg(windows)]
const LEGACY_NAME: &str = "WLS Desktop";

#[cfg(not(windows))]
pub fn is_enabled() -> bool {
    desktop_path().is_some_and(|f| file_enabled(&f))
}

#[cfg(not(windows))]
pub fn set(on: bool) -> Result<(), String> {
    let f = desktop_path().ok_or("HOME")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    file_set(&f, on.then_some(exe.as_path()))
}

/// Pokud je autostart zapnutý a EXE se mezitím přesunulo, opraví `Exec`; jinak nic nedělá.
#[cfg(not(windows))]
pub fn fix_path() {
    if let (Some(f), Ok(exe)) = (desktop_path(), std::env::current_exe()) {
        file_fix(&f, &exe);
    }
}

#[cfg(windows)]
pub fn is_enabled() -> bool {
    reg::get(NAME).or_else(|| reg::get(LEGACY_NAME)).is_some()
}

#[cfg(windows)]
pub fn set(on: bool) -> Result<(), String> {
    if on {
        reg::set(NAME, &command().ok_or("current_exe")?)?;
        reg::delete(LEGACY_NAME)
    } else {
        reg::delete(NAME)?;
        reg::delete(LEGACY_NAME)
    }
}

/// Pokud je autostart zapnutý, ale EXE se mezitím přesunulo (nebo jde o hodnotu z doby před
/// přejmenováním aplikace), opraví cestu a název hodnoty.
#[cfg(windows)]
pub fn fix_path() {
    let Some(want) = command() else { return };
    if reg::get(LEGACY_NAME).is_some() {
        if reg::set(NAME, &want).is_ok() {
            let _ = reg::delete(LEGACY_NAME);
        }
        return;
    }
    if let Some(cur) = reg::get(NAME) {
        if !cur.eq_ignore_ascii_case(&want) {
            let _ = reg::set(NAME, &want);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn autostart_desktop_file_content() {
        let c = super::desktop_file_content(Path::new("/opt/wslcd/wsl-container-desktop"));
        assert!(c.starts_with("[Desktop Entry]
"));
        assert!(c.contains("
Type=Application
"));
        assert!(c.contains("
Name=WSL Container Desktop
"));
        assert!(c.contains("
Exec=/opt/wslcd/wsl-container-desktop --autostart
"));
        assert!(c.contains("
Terminal=false
"));
    }

    #[test]
    fn desktop_exec_is_quoted() {
        // mezera a speciální znaky → uvozovky a escapování podle Desktop Entry Specification
        assert_eq!(super::exec_line(Path::new("/home/u/my apps/app")), "\"/home/u/my apps/app\" --autostart");
        assert_eq!(super::exec_line(Path::new("/a/$b")), "\"/a/\\$b\" --autostart");
        assert_eq!(super::exec_line(Path::new("/a/100%")), "/a/100%% --autostart");
    }

    #[test]
    fn desktop_exec_is_replaced_only_when_different() {
        let c = super::desktop_file_content(Path::new("/old/app"));
        assert_eq!(super::exec_of(&c).as_deref(), Some("/old/app --autostart"));
        let n = super::with_exec(&c, "/new/app --autostart").unwrap();
        assert!(n.contains("
Exec=/new/app --autostart
") && !n.contains("/old/app"));
        assert_eq!(n.lines().count(), c.lines().count());
        // stejná hodnota → nic k přepsání
        assert!(super::with_exec(&c, "/old/app --autostart").is_none());
        // soubor bez Exec se nepřepisuje
        assert!(super::with_exec("[Desktop Entry]
Name=x
", "/a").is_none());
    }

    #[test]
    fn desktop_file_roundtrip_on_disk() {
        let dir = std::env::temp_dir().join(format!("wslcd-autostart-{}", std::process::id()));
        let f = dir.join("autostart").join("wsl-container-desktop.desktop");
        assert!(!super::file_enabled(&f));
        super::file_set(&f, Some(Path::new("/old/app"))).unwrap();
        assert!(super::file_enabled(&f));
        // fix: soubor existuje → Exec se aktualizuje
        super::file_fix(&f, Path::new("/new/app"));
        assert!(std::fs::read_to_string(&f).unwrap().contains("Exec=/new/app --autostart"));
        super::file_set(&f, None).unwrap();
        assert!(!super::file_enabled(&f));
        // fix bez souboru ho nesmí vytvořit
        super::file_fix(&f, Path::new("/new/app"));
        assert!(!f.exists());
        // vypnutí, když už není, není chyba
        super::file_set(&f, None).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
