//! Instalace `docker.exe` shimu do uživatelského PATH
//! (%LOCALAPPDATA%\WLS_Desktop\bin, bez administrátorských práv).

use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct Status {
    /// soubory jsou v cílové složce
    pub installed: bool,
    /// cílová složka je v uživatelském PATH
    pub in_path: bool,
    /// jiný docker.exe, který by mohl mít v PATH přednost
    pub other_docker: Vec<String>,
    /// shim je k dispozici vedle aplikace (zdroj pro instalaci)
    pub source_ok: bool,
    /// uživatelská proměnná DOCKER_HOST (pro nástroje, které mluví přímo s API)
    pub docker_host: Option<String>,
}

/// Akce nad shimem / prostředím Windows.
#[derive(Debug, Clone, Copy)]
pub enum Action {
    Check,
    Install,
    Uninstall,
    /// port a TLS (DOCKER_TLS_VERIFY + DOCKER_CERT_PATH)
    SetDockerHost(u16, bool),
    ClearDockerHost,
}

pub fn run(action: Action) -> Option<Result<String, String>> {
    match action {
        Action::Check => None,
        Action::Install => Some(install()),
        Action::Uninstall => Some(uninstall()),
        Action::SetDockerHost(port, tls) => Some(set_docker_host(Some((port, tls)))),
        Action::ClearDockerHost => Some(set_docker_host(None)),
    }
}

/// Nastaví (Some) nebo odebere (None) uživatelskou proměnnou DOCKER_HOST; s TLS také
/// DOCKER_TLS_VERIFY=1 a DOCKER_CERT_PATH (složka s ca.pem, cert.pem, key.pem).
pub fn set_docker_host(target: Option<(u16, bool)>) -> Result<String, String> {
    match target {
        Some((p, tls)) => {
            let v = format!("tcp://127.0.0.1:{p}");
            let (verify, certs) = match (tls, crate::docker::tls_dir()) {
                (true, Some(d)) => (ps_quote("1"), ps_quote(&d.display().to_string())),
                _ => ("$null".to_string(), "$null".to_string()),
            };
            powershell(&format!(
                "[Environment]::SetEnvironmentVariable('DOCKER_HOST', {}, 'User');                  [Environment]::SetEnvironmentVariable('DOCKER_TLS_VERIFY', {verify}, 'User');                  [Environment]::SetEnvironmentVariable('DOCKER_CERT_PATH', {certs}, 'User')",
                ps_quote(&v)
            ))?;
            Ok(tf!(
                "DOCKER_HOST={v} nastaveno – nástroje je uvidí po restartu (nový terminál / VS Code)",
                "DOCKER_HOST={v} set – tools will see it after a restart (new terminal / VS Code)"
            ))
        }
        None => {
            powershell(
                "[Environment]::SetEnvironmentVariable('DOCKER_HOST', $null, 'User');                  [Environment]::SetEnvironmentVariable('DOCKER_TLS_VERIFY', $null, 'User');                  [Environment]::SetEnvironmentVariable('DOCKER_CERT_PATH', $null, 'User')",
            )?;
            Ok(t!("DOCKER_HOST odebráno", "DOCKER_HOST removed").into())
        }
    }
}

pub fn bin_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("WLS_Desktop")
        .join("bin")
}

fn source() -> Option<PathBuf> {
    let p = std::env::current_exe().ok()?.parent()?.join("docker.exe");
    p.exists().then_some(p)
}

fn powershell(script: &str) -> Result<String, String> {
    let out = crate::wsl::hidden("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub fn status() -> Status {
    let dir = bin_dir();
    let installed = dir.join("docker.exe").exists();
    let user_path = powershell("[Environment]::GetEnvironmentVariable('Path','User')").unwrap_or_default();
    let d = dir.display().to_string().to_ascii_lowercase();
    let in_path = user_path.split(';').any(|p| p.trim().trim_end_matches('\\').to_ascii_lowercase() == d);
    let other_docker = crate::wsl::hidden("where.exe")
        .arg("docker")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(|l| l.trim().to_string()).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|l| !l.is_empty() && !l.to_ascii_lowercase().starts_with(&d))
        .collect();
    let docker_host = powershell("[Environment]::GetEnvironmentVariable('DOCKER_HOST','User')")
        .ok()
        .filter(|s| !s.trim().is_empty());
    Status { installed, in_path, other_docker, source_ok: source().is_some(), docker_host }
}

pub fn install() -> Result<String, String> {
    let src = source().ok_or(t!(
        "docker.exe (shim) nebyl nalezen vedle aplikace – sestav projekt (cargo build --release)",
        "docker.exe (shim) was not found next to the application – build the project (cargo build --release)"
    ))?;
    let dir = bin_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for name in ["docker.exe", "docker-compose.exe"] {
        std::fs::copy(&src, dir.join(name))
            .map_err(|e| tf!("{name}: {e} (neběží zrovna nějaký docker příkaz?)", "{name}: {e} (is a docker command currently running?)"))?;
    }
    let d = ps_quote(&dir.display().to_string());
    powershell(&format!(
        "$d={d}; $p=[Environment]::GetEnvironmentVariable('Path','User'); if(-not $p){{$p=''}}; \
         if(($p -split ';' | ForEach-Object {{ $_.TrimEnd('\\') }}) -notcontains $d){{ \
           [Environment]::SetEnvironmentVariable('Path', (($p.TrimEnd(';') + ';' + $d).TrimStart(';')), 'User') }}"
    ))?;
    Ok(tf!(
        "Příkaz docker nainstalován do {} – otevři nový terminál",
        "The docker command was installed to {} – open a new terminal",
        dir.display()
    ))
}

pub fn uninstall() -> Result<String, String> {
    let dir = bin_dir();
    let d = ps_quote(&dir.display().to_string());
    powershell(&format!(
        "$d={d}; $p=[Environment]::GetEnvironmentVariable('Path','User'); if($p){{ \
           $n = ($p -split ';' | Where-Object {{ $_ -and ($_.TrimEnd('\\') -ne $d) }}) -join ';'; \
           [Environment]::SetEnvironmentVariable('Path', $n, 'User') }}"
    ))?;
    for name in ["docker.exe", "docker-compose.exe"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    Ok(t!("Příkaz docker odebrán z PATH", "The docker command was removed from PATH").into())
}
