//! Šifrování dat pro aktuálního uživatele Windows (DPAPI, CryptProtectData).
//! Používá se pro klientský TLS klíč – soubor na /mnt/c je jinak čitelný pro každého
//! uživatele v každé WSL distribuci (bezpečnostní nález F-06).

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
    }

    unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as _);
        v
    }

    pub fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
        let input = blob(data);
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = unsafe {
            CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        if ok == 0 {
            return Err(format!("CryptProtectData: {}", std::io::Error::last_os_error()));
        }
        Ok(unsafe { take(out) })
    }

    pub fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
        let input = blob(data);
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        let ok = unsafe {
            CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        if ok == 0 {
            return Err(format!("CryptUnprotectData: {}", std::io::Error::last_os_error()));
        }
        Ok(unsafe { take(out) })
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn protect(d: &[u8]) -> Result<Vec<u8>, String> {
        Ok(d.to_vec())
    }
    pub fn unprotect(d: &[u8]) -> Result<Vec<u8>, String> {
        Ok(d.to_vec())
    }
}

pub use imp::{protect, unprotect};

#[cfg(test)]
mod tests {
    #[test]
    fn roundtrip() {
        let secret = b"-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----\n";
        let enc = super::protect(secret).unwrap();
        assert_ne!(enc.as_slice(), secret.as_slice());
        assert_eq!(super::unprotect(&enc).unwrap(), secret);
    }
}
