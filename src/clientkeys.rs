//! Úklid kopií klientského TLS klíče v úložišti klíčů Windows (CNG, „Microsoft Software Key
//! Storage Provider“, `%APPDATA%\Microsoft\Crypto\Keys`).
//!
//! Aplikace sama klíče do Windows neukládá (TLS klient je rustls). Nástroje s `DOCKER_CERT_PATH`
//! ale ano: např. Testcontainers / Docker.DotNet na Windows při každém spuštění naimportují
//! `cert.pem` + `key.pem` přes PFX a trvalý klíč po sobě nesmažou (SChannel ephemeral klíče
//! neumí). Klíčů tak přibývá s každým během testů.
//!
//! Aplikace si u každé distribuce pamatuje veřejný klíč jejího klientského certifikátu
//! ([`REGISTRY`], zapisuje se při stažení certifikátů). Úklid se týká jen klíčů s tímto
//! veřejným klíčem – cizích klíčů se nedotkne:
//! - distribuce existuje → zůstane nejnovější klíč, starší kopie se smažou;
//! - distribuce už neexistuje → smažou se všechny;
//! - klíč mladší než [`MIN_AGE`] zůstane vždy (může ho právě používat běžící test).

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Seznam distribuce → veřejný klíč klientského certifikátu (hex X‖Y), v `data\tls`.
pub const REGISTRY: &str = "client-keys.json";
/// Mladší klíče se nemažou (běžící testy je mají otevřené).
pub const MIN_AGE: Duration = Duration::from_secs(6 * 3600);

/// Bod veřejného klíče P-256 (X‖Y, 64 bajtů) z certifikátu v PEM.
pub fn cert_point(pem: &[u8]) -> Option<Vec<u8>> {
    use rustls_pki_types::pem::PemObject;
    let der = rustls_pki_types::CertificateDer::from_pem_slice(pem).ok()?;
    point_from_der(&der)
}

/// SubjectPublicKeyInfo s křivkou prime256v1: OID 1.2.840.10045.3.1.7, pak BIT STRING
/// s nekomprimovaným bodem (`03 42 00 04` + X + Y).
fn point_from_der(der: &[u8]) -> Option<Vec<u8>> {
    const CURVE: [u8; 10] = [0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
    const BITS: [u8; 4] = [0x03, 0x42, 0x00, 0x04];
    let i = der.windows(CURVE.len()).position(|w| w == CURVE)? + CURVE.len();
    let rest = der.get(i..)?;
    rest.starts_with(&BITS).then(|| rest.get(BITS.len()..BITS.len() + 64).map(<[u8]>::to_vec))?
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    s.len().is_multiple_of(2).then_some(())?;
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

fn registry_path() -> Option<PathBuf> {
    crate::apitls::tls_dir().map(|d| d.join(REGISTRY))
}

fn load(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn save(path: &Path, reg: &BTreeMap<String, String>) {
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(s) = serde_json::to_string_pretty(reg) {
        let _ = std::fs::write(path, s);
    }
}

/// Zapamatuje si veřejný klíč klientského certifikátu distribuce.
pub fn remember(distro: &str, cert_pem: &[u8]) {
    let (Some(path), Some(p)) = (registry_path(), cert_point(cert_pem)) else { return };
    let mut reg = load(&path);
    if reg.get(distro).map(String::as_str) != Some(hex(&p).as_str()) {
        reg.insert(distro.to_string(), hex(&p));
        save(&path, &reg);
    }
}

/// Klíč v úložišti: název, bod veřejného klíče a stáří (None = neznámé).
#[derive(Debug, Clone, PartialEq)]
pub struct Key {
    pub name: String,
    pub point: Vec<u8>,
    pub age: Option<Duration>,
}

/// Které klíče smazat (čistá logika, viz popis modulu).
pub fn plan(keys: &[Key], active: &HashSet<Vec<u8>>, stale: &HashSet<Vec<u8>>, min_age: Duration) -> Vec<String> {
    let old = |k: &Key| k.age.is_some_and(|a| a >= min_age);
    let mut out = Vec::new();
    for p in active {
        let mut same: Vec<&Key> = keys.iter().filter(|k| &k.point == p).collect();
        // nejnovější první (neznámé stáří bere jako nejnovější)
        same.sort_by_key(|k| k.age.unwrap_or_default());
        out.extend(same.iter().skip(1).filter(|k| old(k)).map(|k| k.name.clone()));
    }
    out.extend(keys.iter().filter(|k| stale.contains(&k.point) && !active.contains(&k.point) && old(k)).map(|k| k.name.clone()));
    out
}

/// Výsledek úklidu.
#[derive(Debug, Default, PartialEq, serde::Serialize)]
pub struct Report {
    pub deleted: usize,
    pub kept: usize,
    pub errors: Vec<String>,
}

/// Uklidí kopie klíčů. `existing` = názvy distribucí, které ve WSL existují; `seed` = certifikát
/// aktuální primární distribuce (zapamatuje se, pokud ještě není v seznamu); `min_age` = mladší
/// klíče zůstanou (běžně [`MIN_AGE`]).
pub fn cleanup(existing: &[String], seed: Option<(&str, &[u8])>, min_age: Duration) -> Report {
    let Some(path) = registry_path() else { return Report::default() };
    if let Some((d, pem)) = seed {
        remember(d, pem);
    }
    let mut reg = load(&path);
    if reg.is_empty() {
        return Report::default();
    }
    let lower: HashSet<String> = existing.iter().map(|d| d.to_lowercase()).collect();
    let (mut active, mut stale) = (HashSet::new(), HashSet::new());
    for (d, h) in &reg {
        let Some(p) = unhex(h) else { continue };
        if lower.contains(&d.to_lowercase()) {
            active.insert(p);
        } else {
            stale.insert(p);
        }
    }
    let keys = match imp::list(&|p| active.contains(p) || stale.contains(p)) {
        Ok(k) => k,
        Err(e) => return Report { errors: vec![e], ..Default::default() },
    };
    let del = plan(&keys, &active, &stale, min_age);
    let mut r = Report::default();
    for n in &del {
        match imp::delete(n) {
            Ok(()) => r.deleted += 1,
            Err(e) => r.errors.push(format!("{n}: {e}")),
        }
    }
    r.kept = keys.len() - r.deleted;
    // zaniklé distribuce bez zbylých klíčů už není potřeba si pamatovat
    let left: HashSet<&Vec<u8>> = keys.iter().filter(|k| !del.contains(&k.name)).map(|k| &k.point).collect();
    let before = reg.len();
    reg.retain(|d, h| lower.contains(&d.to_lowercase()) || unhex(h).is_some_and(|p| left.contains(&p)));
    if reg.len() != before {
        save(&path, &reg);
    }
    r
}

#[cfg(windows)]
mod imp {
    use super::Key;
    use std::time::SystemTime;
    use windows_sys::Win32::Foundation::NTE_NO_MORE_ITEMS;
    use windows_sys::Win32::Security::Cryptography::{
        NCryptDeleteKey, NCryptEnumKeys, NCryptExportKey, NCryptFreeBuffer, NCryptFreeObject, NCryptGetProperty, NCryptKeyName,
        NCryptOpenKey, NCryptOpenStorageProvider, BCRYPT_ECCPUBLIC_BLOB, MS_KEY_STORAGE_PROVIDER, NCRYPT_SILENT_FLAG,
        NCRYPT_UNIQUE_NAME_PROPERTY,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe fn from_wide(p: *const u16) -> String {
        if p.is_null() {
            return String::new();
        }
        let mut n = 0;
        while *p.add(n) != 0 {
            n += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, n))
    }

    struct Handle(usize);
    impl Drop for Handle {
        fn drop(&mut self) {
            if self.0 != 0 {
                unsafe { NCryptFreeObject(self.0) };
            }
        }
    }

    fn provider() -> Result<Handle, String> {
        let mut h = 0;
        let r = unsafe { NCryptOpenStorageProvider(&mut h, MS_KEY_STORAGE_PROVIDER, 0) };
        if r != 0 {
            return Err(format!("NCryptOpenStorageProvider: 0x{r:08X}"));
        }
        Ok(Handle(h))
    }

    fn open(prov: &Handle, name: &str) -> Option<Handle> {
        let mut k = 0;
        let n = wide(name);
        (unsafe { NCryptOpenKey(prov.0, &mut k, n.as_ptr(), 0, NCRYPT_SILENT_FLAG) } == 0).then_some(Handle(k))
    }

    /// X‖Y veřejného klíče ECC (BCRYPT_ECCKEY_BLOB: magic, cbKey, X, Y).
    fn point(key: &Handle) -> Option<Vec<u8>> {
        let mut buf = [0u8; 256];
        let mut len = 0u32;
        let r = unsafe {
            NCryptExportKey(key.0, 0, BCRYPT_ECCPUBLIC_BLOB, std::ptr::null(), buf.as_mut_ptr(), buf.len() as u32, &mut len, NCRYPT_SILENT_FLAG)
        };
        if r != 0 || len < 8 {
            return None;
        }
        let cb = u32::from_le_bytes(buf[4..8].try_into().ok()?) as usize;
        buf.get(8..8 + 2 * cb).map(<[u8]>::to_vec)
    }

    /// Stáří podle souboru klíče v %APPDATA%\Microsoft\Crypto\Keys.
    fn age(key: &Handle) -> Option<std::time::Duration> {
        let mut buf = [0u16; 260];
        let mut len = 0u32;
        let r = unsafe {
            NCryptGetProperty(key.0, NCRYPT_UNIQUE_NAME_PROPERTY, buf.as_mut_ptr() as *mut u8, (buf.len() * 2) as u32, &mut len, NCRYPT_SILENT_FLAG)
        };
        if r != 0 {
            return None;
        }
        let file = unsafe { from_wide(buf.as_ptr()) };
        let dir = std::env::var_os("APPDATA")?;
        let meta = std::fs::metadata(std::path::Path::new(&dir).join("Microsoft").join("Crypto").join("Keys").join(file)).ok()?;
        SystemTime::now().duration_since(meta.modified().ok()?).ok()
    }

    /// Klíče ECC P-256, jejichž veřejný klíč vyhovuje `want`.
    pub fn list(want: &dyn Fn(&Vec<u8>) -> bool) -> Result<Vec<Key>, String> {
        let prov = provider()?;
        let mut names = Vec::new();
        let mut state: *mut core::ffi::c_void = std::ptr::null_mut();
        loop {
            let mut item: *mut NCryptKeyName = std::ptr::null_mut();
            let r = unsafe { NCryptEnumKeys(prov.0, std::ptr::null(), &mut item, &mut state, NCRYPT_SILENT_FLAG) };
            if r == NTE_NO_MORE_ITEMS || item.is_null() {
                break;
            }
            if r != 0 {
                unsafe { NCryptFreeBuffer(item as _) };
                if !state.is_null() {
                    unsafe { NCryptFreeBuffer(state) };
                }
                return Err(format!("NCryptEnumKeys: 0x{r:08X}"));
            }
            let (name, alg) = unsafe { (from_wide((*item).pszName), from_wide((*item).pszAlgid)) };
            unsafe { NCryptFreeBuffer(item as _) };
            if alg.starts_with("ECDSA_P256") || alg.starts_with("ECDH_P256") || alg == "ECDSA" || alg == "ECDH" {
                names.push(name);
            }
        }
        if !state.is_null() {
            unsafe { NCryptFreeBuffer(state) };
        }
        let mut out = Vec::new();
        for name in names {
            let Some(k) = open(&prov, &name) else { continue };
            let Some(p) = point(&k).filter(|p| p.len() == 64 && want(p)) else { continue };
            out.push(Key { name, point: p, age: age(&k) });
        }
        Ok(out)
    }

    pub fn delete(name: &str) -> Result<(), String> {
        let prov = provider()?;
        let k = open(&prov, name).ok_or("klíč nelze otevřít")?;
        // NCryptDeleteKey uvolní handle i při úspěchu
        let r = unsafe { NCryptDeleteKey(k.0, NCRYPT_SILENT_FLAG) };
        if r == 0 {
            std::mem::forget(k);
            Ok(())
        } else {
            Err(format!("NCryptDeleteKey: 0x{r:08X}"))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Key;
    pub fn list(_want: &dyn Fn(&Vec<u8>) -> bool) -> Result<Vec<Key>, String> {
        Ok(Vec::new())
    }
    pub fn delete(_name: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // veřejný klientský certifikát (bez klíče)
    const CERT: &str = "-----BEGIN CERTIFICATE-----
MIIBvzCCAWSgAwIBAgIUcp4mHvldxMQEu0kry6bZ04ugEvkwCgYIKoZIzj0EAwIw
IzEhMB8GA1UEAwwYV1NMIENvbnRhaW5lciBEZXNrdG9wIENBMB4XDTI2MTEyMjEy
NDMwMFoXDTI5MDIyNDEyNDMwMFowJzElMCMGA1UEAwwcd3NsLWNvbnRhaW5lci1k
ZXNrdG9wLWNsaWVudDBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABFTNQ1JENx7P
fdkdMULM2sykBkFPfsT3zGjUDFUz8C/rXPhsCVww7xlLusl8LOhELttH7WX9UTsy
fig9SQVfRyCjcjBwMAkGA1UdEwQCMAAwDgYDVR0PAQH/BAQDAgeAMBMGA1UdJQQM
MAoGCCsGAQUFBwMCMB0GA1UdDgQWBBTNVrdsBJGSK/wzyKmskyCEGx9XMDAfBgNV
HSMEGDAWgBRj9g2wQmrdsLojKGmKDUU2eY2FTzAKBggqhkjOPQQDAgNJADBGAiEA
2JEIPa+UpkCIuq0ih2/bF+g9dWERd58LrmcztS2NYVQCIQCPQGLIy9aks5KoRmfp
8bwZ2XjOVZkM1xnRzf1j1rkCXQ==
-----END CERTIFICATE-----";

    #[test]
    fn point_from_certificate() {
        let p = cert_point(CERT.as_bytes()).expect("bod P-256");
        assert_eq!(p.len(), 64);
        assert_eq!(&p[..4], &[0x54, 0xcd, 0x43, 0x52]);
        assert_eq!(unhex(&hex(&p)), Some(p));
        assert!(cert_point(b"neni pem").is_none());
    }

    fn key(name: &str, point: u8, hours: Option<u64>) -> Key {
        Key { name: name.into(), point: vec![point; 64], age: hours.map(|h| Duration::from_secs(h * 3600)) }
    }

    #[test]
    fn keeps_newest_active_and_young_keys() {
        let keys = [
            key("a-new", 1, Some(7)),
            key("a-old", 1, Some(30)),
            key("a-older", 1, Some(48)),
            key("a-young", 1, Some(1)),
            key("gone-old", 2, Some(10)),
            key("gone-young", 2, Some(2)),
            key("gone-unknown", 2, None),
            key("foreign", 3, Some(100)),
        ];
        let active = HashSet::from([vec![1u8; 64]]);
        let stale = HashSet::from([vec![2u8; 64]]);
        let mut del = plan(&keys, &active, &stale, MIN_AGE);
        del.sort();
        // a-young je nejnovější (zůstane), a-new má přes 6 h, ale není nejnovější → smazat
        assert_eq!(del, ["a-new", "a-old", "a-older", "gone-old"]);
    }

    #[test]
    fn unknown_age_counts_as_newest() {
        let keys = [key("x", 1, None), key("y", 1, Some(20))];
        let active = HashSet::from([vec![1u8; 64]]);
        assert_eq!(plan(&keys, &active, &HashSet::new(), MIN_AGE), ["y"]);
    }
}
