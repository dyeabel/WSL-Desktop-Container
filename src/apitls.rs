//! Klientská TLS konfigurace pro API enginu (sdílí aplikace i `docker.exe` shim).
//!
//! Certifikáty jsou ve `%APPDATA%\WSL_Container_Desktop\tls` (ca.pem, cert.pem; klíč key.pem.dpapi šifrovaný DPAPI).

/// Složka s klientskými certifikáty (Docker konvence: ca.pem, cert.pem, key.pem) –
/// stejná se dá použít jako DOCKER_CERT_PATH.
pub fn tls_dir() -> Option<std::path::PathBuf> {
    crate::config::data_dir().map(|d| d.join("tls"))
}

/// Sestaví TLS klienta z PEM souborů ve složce (ověřuje server proti CA, posílá klientský certifikát).
pub fn tls_config(dir: &std::path::Path) -> Result<rustls::ClientConfig, String> {
    use rustls_pki_types::pem::PemObject;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer};
    let f = |n: &str| dir.join(n);
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_file_iter(f("ca.pem")).map_err(|e| format!("ca.pem: {e}"))? {
        roots.add(c.map_err(|e| format!("ca.pem: {e}"))?).map_err(|e| format!("ca.pem: {e}"))?;
    }
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(f("cert.pem"))
        .map_err(|e| format!("cert.pem: {e}"))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("cert.pem: {e}"))?;
    // klíč je uložený šifrovaně (DPAPI); key.pem jen když si ho uživatel vyexportoval pro nástroje
    let key = match std::fs::read(f("key.pem.dpapi")) {
        Ok(enc) => {
            let pem = crate::dpapi::unprotect(&enc)?;
            PrivateKeyDer::from_pem_slice(&pem).map_err(|e| format!("key.pem.dpapi: {e}"))?
        }
        Err(_) => PrivateKeyDer::from_pem_file(f("key.pem")).map_err(|e| format!("key.pem: {e}"))?,
    };
    let mut cfg = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_client_auth_cert(certs, key)
        .map_err(|e| e.to_string())?;
    // Bez obnovy relací: socat (OpenSSL s verify=1 bez session id context) na pokus o obnovu
    // odpoví alertem internal_error – každé další nové spojení se stejnou konfigurací by selhalo.
    cfg.resumption = rustls::client::Resumption::disabled();
    Ok(cfg)
}
