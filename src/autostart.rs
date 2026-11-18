//! Spouštění s Windows: hodnota v HKCU\Software\Microsoft\Windows\CurrentVersion\Run
//! (jen pro aktuálního uživatele, bez práv správce). Aplikace se spouští s `--autostart`,
//! takže se rovnou schová do oznamovací oblasti.

pub const ARG: &str = "--autostart";

/// Byla aplikace spuštěna automaticky po přihlášení?
pub fn launched_at_login() -> bool {
    std::env::args().any(|a| a == ARG)
}

fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" {ARG}", exe.display()))
}

#[cfg(windows)]
mod reg {
    use windows_sys::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
    };

    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "WLS Desktop";

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn get() -> Option<String> {
        let (k, n) = (w(KEY), w(NAME));
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

    pub fn set(value: &str) -> Result<(), String> {
        let (k, n, v) = (w(KEY), w(NAME), w(value));
        let r = unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr(), REG_SZ, v.as_ptr().cast(), (v.len() * 2) as u32)
        };
        if r == 0 { Ok(()) } else { Err(format!("RegSetKeyValueW: {r}")) }
    }

    pub fn delete() -> Result<(), String> {
        let (k, n) = (w(KEY), w(NAME));
        let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, k.as_ptr(), n.as_ptr()) };
        // 2 = ERROR_FILE_NOT_FOUND (už není)
        if r == 0 || r == 2 { Ok(()) } else { Err(format!("RegDeleteKeyValueW: {r}")) }
    }
}

#[cfg(not(windows))]
mod reg {
    pub fn get() -> Option<String> {
        None
    }
    pub fn set(_: &str) -> Result<(), String> {
        Err("jen Windows".into())
    }
    pub fn delete() -> Result<(), String> {
        Ok(())
    }
}

pub fn is_enabled() -> bool {
    reg::get().is_some()
}

pub fn set(on: bool) -> Result<(), String> {
    if on {
        reg::set(&command().ok_or("current_exe")?)
    } else {
        reg::delete()
    }
}

/// Pokud je autostart zapnutý, ale EXE se mezitím přesunulo, opraví cestu.
pub fn fix_path() {
    if let (Some(cur), Some(want)) = (reg::get(), command()) {
        if !cur.eq_ignore_ascii_case(&want) {
            let _ = reg::set(&want);
        }
    }
}
