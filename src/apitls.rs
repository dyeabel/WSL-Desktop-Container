//! Klientská TLS konfigurace pro API enginu (sdílí aplikace i `docker.exe` shim).
//!
//! Certifikáty jsou ve `%APPDATA%\WLS_Desktop\tls` (ca.pem, cert.pem, key.pem).

/// Složka s klientskými certifikáty (Docker konvence: ca.pem, cert.pem, key.pem) –
/// stejná se dá použít jako DOCKER_CERT_PATH.
pub fn tls_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(|p| std::path::PathBuf::from(p).join("WLS_Desktop").join("tls"))
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
    let key = PrivateKeyDer::from_pem_file(f("key.pem")).map_err(|e| format!("key.pem: {e}"))?;
    rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_client_auth_cert(certs, key)
        .map_err(|e| e.to_string())
}
