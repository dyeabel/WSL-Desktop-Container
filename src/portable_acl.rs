//! Oprávnění složky portable balíčku (N-03).
//!
//! Portable balíček rozbalený třeba do `C:\WSLContainerDesktop` dědí ACL kořene disku:
//! `Authenticated Users:(M)` – jiný místní uživatel by mohl podvrhnout `wsl-container-desktop.exe`
//! (spouští se po přihlášení), `docker.exe` (je v PATH) nebo `data\config.json` a certifikáty.
//! Při startu v portable režimu proto aplikace nastaví složce chráněné ACL (bez dědění):
//! plný přístup jen pro aktuálního uživatele, SYSTEM a Administrátory; podsložky a soubory
//! oprávnění zdědí. Složka v profilu uživatele už takové ACL má – nic se nemění.

/// Požadovaný DACL ve formátu SDDL.
#[cfg_attr(not(windows), allow(dead_code))] // volá jen kód pro Windows (nastavení ACL); testuje se i na Linuxu
pub fn wanted_sddl(user_sid: &str) -> String {
    format!("D:PAI(A;OICI;FA;;;{user_sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)")
}

/// Je DACL (SDDL, jen část `D:`) chráněný proti dědění a povoluje přístup jen uživateli,
/// SYSTEM a Administrátorům? Zakazující ACE nevadí.
///
/// `sid_of` převede správce z SDDL na textový SID: Windows píše dobře známé účty zkratkou
/// (`SY`, `BA`, a také `LA` = vestavěný Administrator s RID 500 – pod ním běží např.
/// runner GitHub Actions), takže uživatel nemusí být v SDDL zapsaný svým SID.
#[cfg_attr(not(windows), allow(dead_code))] // volá jen kód pro Windows (nastavení ACL); testuje se i na Linuxu
pub fn dacl_is_private(sddl: &str, user_sid: &str, sid_of: impl Fn(&str) -> String) -> bool {
    let Some(d) = sddl.find("D:").map(|i| &sddl[i + 2..]) else { return false };
    let flags = &d[..d.find('(').unwrap_or(d.len())];
    if !flags.contains('P') {
        return false;
    }
    d.split('(').skip(1).filter_map(|a| a.split(')').next()).all(|ace| {
        let f: Vec<&str> = ace.split(';').collect();
        let allow = f.first().is_some_and(|t| *t == "A" || *t == "OA");
        let sid = sid_of(f.get(5).copied().unwrap_or(""));
        !allow || sid.eq_ignore_ascii_case(user_sid) || sid == SID_SYSTEM || sid == SID_ADMINS
    })
}

#[cfg_attr(not(windows), allow(dead_code))] // volá jen kód pro Windows (nastavení ACL); testuje se i na Linuxu
const SID_SYSTEM: &str = "S-1-5-18";
#[cfg_attr(not(windows), allow(dead_code))] // volá jen kód pro Windows (nastavení ACL); testuje se i na Linuxu
const SID_ADMINS: &str = "S-1-5-32-544";

/// Zkratka nebo SID ze SDDL → textový SID (`SY` → `S-1-5-18`, `LA` → `S-1-5-21-…-500`).
/// Když převod selže, vrátí vstup beze změny.
#[cfg(windows)]
pub fn canonical_sid(s: &str) -> String {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSidToSidW};
    use windows_sys::Win32::Security::PSID;
    if s.starts_with("S-") || s.is_empty() {
        return s.to_string();
    }
    let w = crate::npipe::win::wide(s);
    let mut sid: PSID = std::ptr::null_mut();
    // SAFETY: w je řetězec ukončený nulou; sid alokuje systém (LocalFree).
    if unsafe { ConvertStringSidToSidW(w.as_ptr(), &mut sid) } == 0 {
        return s.to_string();
    }
    let mut out: *mut u16 = std::ptr::null_mut();
    // SAFETY: sid je platný; out alokuje systém (LocalFree).
    let ok = unsafe { ConvertSidToStringSidW(sid, &mut out) };
    let r = if ok != 0 {
        // SAFETY: out je řetězec ukončený nulou, platný do LocalFree.
        unsafe {
            let mut n = 0;
            while *out.add(n) != 0 {
                n += 1;
            }
            let v = String::from_utf16_lossy(std::slice::from_raw_parts(out, n));
            LocalFree(out.cast());
            v
        }
    } else {
        s.to_string()
    };
    // SAFETY: sid alokovala ConvertStringSidToSidW (LocalAlloc).
    unsafe { LocalFree(sid) };
    r
}

/// V portable režimu zajistí soukromé ACL složky aplikace. Ok(true) = ACL se změnilo,
/// Ok(false) = nebylo potřeba (nebo nejde o portable režim).
#[cfg(windows)]
pub fn protect_portable_dir() -> Result<bool, String> {
    match crate::config::portable_dir() {
        Some(dir) => protect_dir(&dir),
        None => Ok(false),
    }
}

#[cfg(not(windows))]
pub fn protect_portable_dir() -> Result<bool, String> {
    Ok(false)
}

#[cfg(windows)]
pub fn protect_dir(dir: &std::path::Path) -> Result<bool, String> {
    use crate::npipe::win::{current_user_sid, security_descriptor, wide};
    use windows_sys::Win32::Foundation::{LocalFree, ERROR_SUCCESS};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW, SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, ACL, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
    };

    let sid = current_user_sid()?;
    let path = wide(&dir.to_string_lossy());

    // současný DACL jako SDDL
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: path je řetězec ukončený nulou; výstupní ukazatele jsou platné; sd uvolní LocalFree.
    let r = unsafe {
        GetNamedSecurityInfoW(path.as_ptr(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, std::ptr::null_mut(), std::ptr::null_mut(), &mut dacl, std::ptr::null_mut(), &mut sd)
    };
    if r != ERROR_SUCCESS {
        return Err(format!("GetNamedSecurityInfoW: {}", std::io::Error::from_raw_os_error(r as i32)));
    }
    let mut s: *mut u16 = std::ptr::null_mut();
    // SAFETY: sd je platný popisovač z GetNamedSecurityInfoW; s alokuje systém (LocalFree).
    let ok = unsafe { ConvertSecurityDescriptorToStringSecurityDescriptorW(sd, SDDL_REVISION_1, DACL_SECURITY_INFORMATION, &mut s, std::ptr::null_mut()) };
    let current = if ok != 0 {
        // SAFETY: s je řetězec ukončený nulou, platný do LocalFree.
        unsafe {
            let mut n = 0;
            while *s.add(n) != 0 {
                n += 1;
            }
            let v = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
            LocalFree(s.cast());
            v
        }
    } else {
        String::new()
    };
    // SAFETY: sd alokovala GetNamedSecurityInfoW (LocalAlloc).
    unsafe { LocalFree(sd) };
    if dacl_is_private(&current, &sid, canonical_sid) {
        return Ok(false);
    }

    // nový chráněný DACL (dědí se do podsložek a souborů)
    let want = security_descriptor(&wanted_sddl(&sid))?;
    let (mut present, mut defaulted) = (0, 0);
    let mut new_dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: want.raw() je platný popisovač; výstupy ukazují do něj a žijí s `want`.
    if unsafe { GetSecurityDescriptorDacl(want.raw(), &mut present, &mut new_dacl, &mut defaulted) } == 0 || present == 0 {
        return Err(format!("GetSecurityDescriptorDacl: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: path ukončená nulou, new_dacl platný po dobu volání (patří `want`).
    let r = unsafe {
        SetNamedSecurityInfoW(
            path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl,
            std::ptr::null(),
        )
    };
    if r != ERROR_SUCCESS {
        return Err(format!("{}: {}", dir.display(), std::io::Error::from_raw_os_error(r as i32)));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "S-1-5-21-1-2-3-1001";

    /// převod zkratek bez Win32 (pro test čisté logiky); LA = vestavěný Administrator
    fn fake_sid(s: &str) -> String {
        match s {
            "SY" => "S-1-5-18".into(),
            "BA" => "S-1-5-32-544".into(),
            "BU" => "S-1-5-32-545".into(),
            "AU" => "S-1-5-11".into(),
            "LA" => "S-1-5-21-1-2-3-500".into(),
            s => s.into(),
        }
    }

    #[test]
    fn detects_inherited_and_shared_acl() {
        // ACL kořene disku zděděné do C:\WSLContainerDesktop (N-03)
        let c_root = "D:AI(A;ID;FA;;;BA)(A;OICIIOID;GA;;;BA)(A;OICIID;FA;;;SY)(A;OICIID;0x1200a9;;;BU)(A;ID;0x1301bf;;;AU)(A;OICIIOID;SDGXGWGR;;;AU)";
        assert!(!dacl_is_private(c_root, ME, fake_sid));
        // chráněné, ale s přístupem pro Users
        assert!(!dacl_is_private("D:P(A;OICI;FA;;;S-1-5-21-1-2-3-1001)(A;OICI;0x1200a9;;;BU)", ME, fake_sid));
        // nechráněné (dědí z rodiče), i když jen uživatel
        assert!(!dacl_is_private("D:(A;OICI;FA;;;S-1-5-21-1-2-3-1001)", ME, fake_sid));
        assert!(!dacl_is_private("", ME, fake_sid));
        // výsledek opravy
        assert!(dacl_is_private(&wanted_sddl(ME), ME, fake_sid));
        // zakazující ACE pro jiné nevadí
        assert!(dacl_is_private("D:P(D;OICI;FA;;;AU)(A;OICI;FA;;;s-1-5-21-1-2-3-1001)(A;OICI;FA;;;SY)", ME, fake_sid));
    }

    /// Uživatel = vestavěný Administrator (RID 500, runner GitHub Actions): Windows ho v SDDL
    /// zapíše jako `LA`. Bez převodu by se ACL přepisovalo při každém startu.
    #[test]
    fn builtin_admin_written_as_alias() {
        let admin = "S-1-5-21-1-2-3-500";
        assert!(dacl_is_private("D:PAI(A;OICI;FA;;;LA)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)", admin, fake_sid));
        // jiný uživatel než LA → není soukromé
        assert!(!dacl_is_private("D:PAI(A;OICI;FA;;;LA)(A;OICI;FA;;;SY)", ME, fake_sid));
    }

    #[cfg(windows)]
    #[test]
    fn canonical_sid_resolves_aliases() {
        assert_eq!(canonical_sid("SY"), "S-1-5-18");
        assert_eq!(canonical_sid("BA"), "S-1-5-32-544");
        assert_eq!(canonical_sid("S-1-5-21-1-2-3-4"), "S-1-5-21-1-2-3-4");
        let la = canonical_sid("LA");
        assert!(la.starts_with("S-1-5-21-") && la.ends_with("-500"), "{la}");
    }

    /// Na skutečné složce: zděděné ACL z %TEMP% se nahradí chráněným, druhé volání nic nemění,
    /// soubor uvnitř oprávnění zdědí.
    #[cfg(windows)]
    #[test]
    fn protects_real_folder() {
        let d = std::env::temp_dir().join(format!("wslcd-acl-test-{}", std::process::id()));
        std::fs::create_dir_all(d.join("data")).unwrap();
        std::fs::write(d.join("data").join("config.json"), "{}").unwrap();
        let first = protect_dir(&d);
        let second = protect_dir(&d);
        let readable = std::fs::read_to_string(d.join("data").join("config.json"));
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(first, Ok(true));
        assert_eq!(second, Ok(false), "po opravě je ACL soukromé");
        assert_eq!(readable.unwrap(), "{}", "vlastník má dál přístup");
    }

    /// Scénář N-03: složka v kořeni C:\ dědí `Authenticated Users:(M)`; po opravě tam nesmí být.
    /// `cargo test live_portable_acl_c_root -- --ignored --nocapture` (vytvoří a smaže C:\WSLCD-ACL-Test-<pid>)
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn live_portable_acl_c_root() {
        let d = std::path::PathBuf::from(format!(r"C:\WSLCD-ACL-Test-{}", std::process::id()));
        std::fs::create_dir_all(d.join("data")).unwrap();
        let icacls = |p: &std::path::Path| String::from_utf8_lossy(&std::process::Command::new("icacls").arg(p).output().unwrap().stdout).to_string();
        let before = icacls(&d);
        let r = protect_dir(&d);
        let after = icacls(&d);
        let after_child = icacls(&d.join("data"));
        let _ = std::fs::remove_dir_all(&d);
        eprintln!("před:\n{before}\npo:\n{after}\npodsložka:\n{after_child}");
        assert_eq!(r, Ok(true));
        // AU = S-1-5-11, BU = S-1-5-32-545 (názvy jsou lokalizované – kontrola přes počet řádků s „(“)
        assert!(after.lines().filter(|l| l.contains("(F)")).count() == 3, "{after}");
        assert!(!after.contains("(M)") && !after.contains("(RX)") && !after_child.contains("(M)"), "{after}\n{after_child}");
    }
}
