//! Instalace `docker.exe` shimu do uživatelského PATH
//! (%LOCALAPPDATA%\WLS_Desktop\bin, bez administrátorských práv).

use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct Status {
    /// soubory jsou v cílové složce
    pub installed: bool,
    /// cílová složka je v uživatelském PATH
    pub in_path: bool,
    /// jiný docker, který má v PATH přednost (je před naší složkou; když naše složka
    /// v PATH není, každý nalezený)
    pub other_docker: Vec<OtherDocker>,
    /// jiné kopie shimu WLS Desktop (v PATH nebo zbylé po dřívější instalaci)
    pub duplicates: Vec<Dup>,
    /// shim je k dispozici vedle aplikace (zdroj pro instalaci)
    pub source_ok: bool,
    /// uživatelská proměnná DOCKER_HOST (pro nástroje, které mluví přímo s API)
    pub docker_host: Option<String>,
}

/// Jiný `docker` v PATH, který má přednost před naším.
#[derive(Debug, Clone, PartialEq)]
pub struct OtherDocker {
    /// nalezený soubor (docker.exe / .cmd …)
    pub file: String,
    /// položka PATH, ve které leží
    pub dir: String,
    /// položka je v uživatelském PATH (jde odebrat bez práv správce); jinak v systémovém
    pub user: bool,
}

/// Akce nad shimem / prostředím Windows.
#[derive(Debug, Clone)]
pub enum Action {
    Check,
    Install,
    Uninstall,
    /// odebrat jiné kopie shimu WLS Desktop z PATH / disku
    RemoveDuplicates,
    /// odebrat složku s cizím dockerem z uživatelského PATH
    RemoveFromPath(String),
    /// port a TLS (DOCKER_TLS_VERIFY + DOCKER_CERT_PATH)
    SetDockerHost(u16, bool),
    /// DOCKER_HOST=npipe:////./pipe/wls_docker_engine (relay aplikace, bez klíče na disku)
    SetDockerHostPipe,
    ClearDockerHost,
}

pub fn run(action: Action) -> Option<Result<String, String>> {
    match action {
        Action::Check => None,
        Action::Install => Some(install()),
        Action::Uninstall => Some(uninstall()),
        Action::RemoveDuplicates => Some(remove_duplicates()),
        Action::RemoveFromPath(dir) => Some(remove_from_user_path(&dir)),
        Action::SetDockerHost(port, tls) => Some(set_docker_host(Some((port, tls)))),
        Action::SetDockerHostPipe => Some(set_docker_host_pipe()),
        Action::ClearDockerHost => Some(set_docker_host(None)),
    }
}

/// Nastaví (Some) nebo odebere (None) uživatelskou proměnnou DOCKER_HOST; s TLS také
/// DOCKER_TLS_VERIFY=1 a DOCKER_CERT_PATH (složka s ca.pem, cert.pem, key.pem).
pub fn set_docker_host(target: Option<(u16, bool)>) -> Result<String, String> {
    match target {
        Some((p, tls)) => {
            let v = format!("tcp://127.0.0.1:{p}");
            // nástroje s DOCKER_CERT_PATH potřebují nešifrovaný key.pem
            crate::docker::export_plain_key(tls)?;
            let (verify, certs) = match (tls, crate::docker::tls_dir()) {
                (true, Some(d)) => (ps_quote("1"), ps_quote(&d.display().to_string())),
                _ => ("$null".to_string(), "$null".to_string()),
            };
            powershell(&format!(
                "[Environment]::SetEnvironmentVariable('DOCKER_HOST', {}, 'User');                  [Environment]::SetEnvironmentVariable('DOCKER_TLS_VERIFY', {verify}, 'User');                  [Environment]::SetEnvironmentVariable('DOCKER_CERT_PATH', {certs}, 'User')",
                ps_quote(&v)
            ))?;
            let mut msg = tf!(
                "DOCKER_HOST={v} nastaveno – nástroje je uvidí po restartu (nový terminál / VS Code)",
                "DOCKER_HOST={v} set – tools will see it after a restart (new terminal / VS Code)"
            );
            if tls {
                msg.push_str(t!(
                    ". Pozor: klientský klíč je teď i nešifrovaně v DOCKER_CERT_PATH (key.pem) – může ho číst každý uživatel ve WSL přes /mnt/c.",
                    ". Note: the client key is now also unencrypted in DOCKER_CERT_PATH (key.pem) – any WSL user can read it via /mnt/c."
                ));
            }
            Ok(msg)
        }
        None => {
            let _ = crate::docker::export_plain_key(false);
            powershell(
                "[Environment]::SetEnvironmentVariable('DOCKER_HOST', $null, 'User');                  [Environment]::SetEnvironmentVariable('DOCKER_TLS_VERIFY', $null, 'User');                  [Environment]::SetEnvironmentVariable('DOCKER_CERT_PATH', $null, 'User')",
            )?;
            Ok(t!("DOCKER_HOST odebráno", "DOCKER_HOST removed").into())
        }
    }
}

/// DOCKER_HOST na pojmenovanou rouru aplikace. DOCKER_TLS_VERIFY / DOCKER_CERT_PATH se odeberou
/// a nešifrovaný key.pem smaže – TLS k enginu řeší relay v aplikaci.
pub fn set_docker_host_pipe() -> Result<String, String> {
    let v = crate::npipe::docker_host_url(crate::npipe::PIPE_NAME);
    let _ = crate::docker::export_plain_key(false);
    powershell(&format!(
        "[Environment]::SetEnvironmentVariable('DOCKER_HOST', {}, 'User');          [Environment]::SetEnvironmentVariable('DOCKER_TLS_VERIFY', $null, 'User');          [Environment]::SetEnvironmentVariable('DOCKER_CERT_PATH', $null, 'User')",
        ps_quote(&v)
    ))?;
    Ok(tf!(
        "DOCKER_HOST={v} nastaveno – nástroje je uvidí po restartu (nový terminál / VS Code). Roura funguje, jen když běží WLS Desktop.",
        "DOCKER_HOST={v} set – tools will see it after a restart (new terminal / VS Code). The pipe works only while WLS Desktop is running."
    ))
}

/// Kam patří docker.exe v PATH: v portable režimu přímo složka aplikace (nic se nekopíruje),
/// jinak %LOCALAPPDATA%\WLS_Desktop\bin.
pub fn bin_dir() -> PathBuf {
    if let Some(d) = crate::config::portable_dir() {
        return d;
    }
    legacy_dir()
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

/// Položky PATH v pořadí, v jakém je uvidí nově otevřený terminál: systémová část, pak
/// uživatelská – z registru, ne z prostředí aplikace (to je kopie z chvíle jejího spuštění
/// a může obsahovat složky, které už v PATH nejsou). Vrací (všechny položky, uživatelská část).
fn registry_path() -> (Vec<String>, String) {
    let out = powershell("[Environment]::GetEnvironmentVariable('Path','Machine'); [Environment]::GetEnvironmentVariable('Path','User')")
        .unwrap_or_default();
    let mut lines = out.lines();
    let machine = lines.next().unwrap_or("").to_string();
    let user = lines.next().unwrap_or("").to_string();
    let all = machine.split(';').chain(user.split(';')).map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
    (all, user)
}

fn norm(p: &str) -> String {
    p.trim().trim_matches('"').trim_end_matches(['\\', '/']).to_ascii_lowercase()
}

/// Jiné `docker` (podle PATHEXT), které příkazový řádek najde dřív než naši složku `ours`.
/// Hledání končí u naší složky – co je za ní, přednost nemá. Když naše složka v PATH není,
/// vrátí všechny nalezené.
pub fn shadowing(entries: &[String], ours: &str, exts: &[String], exists: impl Fn(&std::path::Path) -> bool) -> Vec<(String, String)> {
    let ours = norm(ours);
    let mut found = Vec::new();
    for e in entries {
        if norm(e) == ours {
            break;
        }
        let dir = std::path::Path::new(e.trim().trim_matches('"'));
        if let Some(p) = exts.iter().map(|x| dir.join(format!("docker{x}"))).find(|p| exists(p)) {
            found.push((p.display().to_string(), e.clone()));
        }
    }
    found
}

pub fn status() -> Status {
    let dir = bin_dir();
    let installed = dir.join("docker.exe").exists();
    let (entries, user_path) = registry_path();
    let d = dir.display().to_string();
    let in_path = user_path.split(';').any(|p| norm(p) == norm(&d));
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .map(|x| x.trim().to_ascii_lowercase())
        .filter(|x| !x.is_empty())
        .collect();
    // kopie shimu WLS Desktop patří mezi duplicity, ne mezi „jiné dockery“
    let user_entries: Vec<String> = user_path.split(';').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
    let other_docker = shadowing(&entries, &d, &exts, |p| p.is_file() && !is_wls_shim(p))
        .into_iter()
        .map(|(file, dir)| OtherDocker { user: user_entries.iter().any(|u| norm(u) == norm(&dir)), file, dir })
        .collect();
    let duplicates = duplicates(&user_entries, &d, &legacy_dir().display().to_string(), legacy_files(), wls_dir);
    let docker_host = powershell("[Environment]::GetEnvironmentVariable('DOCKER_HOST','User')")
        .ok()
        .filter(|s| !s.trim().is_empty());
    Status { installed, in_path, other_docker, duplicates, source_ok: source().is_some(), docker_host }
}

/// Značka v binárce shimu (User-Agent rychlé cesty) – pozná kopii shimu WLS Desktop.
const SHIM_MARK: &[u8] = b"wls-docker-shim";

fn is_wls_shim(p: &std::path::Path) -> bool {
    std::fs::read(p).is_ok_and(|b| b.windows(SHIM_MARK.len()).any(|w| w == SHIM_MARK))
}

/// Složka, která podle jména patřila WLS Desktop, ale `docker.exe` v ní už není (přesunutý
/// nebo smazaný balíček) – položka PATH je zbytečná.
fn stale_wls_dir(dir: &std::path::Path) -> bool {
    let n = dir.display().to_string().to_ascii_lowercase();
    ["wls-desktop", "wls_desktop", "wlsdesktop"].iter().any(|k| n.contains(k)) && !dir.join("docker.exe").is_file()
}

/// Složka s kopií shimu WLS Desktop, nebo po ní zbylá položka PATH.
fn wls_dir(dir: &std::path::Path) -> bool {
    is_wls_shim(&dir.join("docker.exe")) || stale_wls_dir(dir)
}

/// Jsou ve výchozí instalační složce soubory shimu?
fn legacy_files() -> bool {
    let d = legacy_dir();
    is_wls_shim(&d.join("docker.exe")) || d.join("docker-compose.exe").is_file()
}

/// Výchozí instalační složka (mimo portable režim): %LOCALAPPDATA%\WLS_Desktop\bin.
fn legacy_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("WLS_Desktop").join("bin")
}

/// Jiná kopie shimu WLS Desktop než ta aktuální.
#[derive(Debug, Clone, PartialEq)]
pub struct Dup {
    pub path: String,
    /// složka je v uživatelském PATH (odebere se z něj)
    pub in_path: bool,
    /// soubory shimu se smažou (jen výchozí instalační složka – portable balíčky se nemažou)
    pub delete_files: bool,
}

/// Duplicity v uživatelském PATH: opakovaná položka naší složky a jiné složky s kopií shimu
/// (starší portable verze, výchozí instalace); k tomu výchozí instalační složka se zbylými
/// soubory (`legacy_files`), i když v PATH už není. `has_shim(složka)` = kopie shimu nebo
/// zbylá položka po přesunutém balíčku.
pub fn duplicates(user_entries: &[String], ours: &str, legacy: &str, legacy_files: bool, has_shim: impl Fn(&std::path::Path) -> bool) -> Vec<Dup> {
    let (ours, legacy_n) = (norm(ours), norm(legacy));
    let mut seen_ours = false;
    let mut out: Vec<Dup> = Vec::new();
    for e in user_entries {
        let n = norm(e);
        if n == ours {
            if seen_ours {
                out.push(Dup { path: e.clone(), in_path: true, delete_files: false });
            }
            seen_ours = true;
        } else if out.iter().any(|d| norm(&d.path) == n) {
            // stejná cizí kopie dvakrát – odebere se celá, stačí jeden záznam
        } else if has_shim(std::path::Path::new(e.trim().trim_matches('"'))) {
            out.push(Dup { path: e.clone(), in_path: true, delete_files: n == legacy_n });
        }
    }
    if legacy_n != ours && !out.iter().any(|d| norm(&d.path) == legacy_n) && legacy_files {
        out.push(Dup { path: legacy.to_string(), in_path: false, delete_files: true });
    }
    out
}

/// Odebere duplicity z uživatelského PATH (naše složka zůstane jednou) a smaže zbylé kopie
/// shimu ve výchozí instalační složce.
pub fn remove_duplicates() -> Result<String, String> {
    remove_duplicates_for(&bin_dir().display().to_string())
}

fn remove_duplicates_for(ours: &str) -> Result<String, String> {
    let ours = ours.to_string();
    let (_, user_path) = registry_path();
    let entries: Vec<String> = user_path.split(';').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
    let dups = duplicates(&entries, &ours, &legacy_dir().display().to_string(), legacy_files(), wls_dir);
    if dups.is_empty() {
        return Ok(t!("Žádné duplicity nenalezeny", "No duplicates found").into());
    }
    let drop: Vec<String> = dups.iter().filter(|d| d.in_path && norm(&d.path) != norm(&ours)).map(|d| norm(&d.path)).collect();
    let mut kept_ours = false;
    let new_path: Vec<&str> = entries
        .iter()
        .filter(|e| {
            let n = norm(e);
            if n == norm(&ours) {
                !std::mem::replace(&mut kept_ours, true)
            } else {
                !drop.contains(&n)
            }
        })
        .map(|e| e.as_str())
        .collect();
    if new_path.len() != entries.len() {
        powershell(&format!("[Environment]::SetEnvironmentVariable('Path', {}, 'User')", ps_quote(&new_path.join(";"))))?;
    }
    let mut failed = Vec::new();
    for d in dups.iter().filter(|d| d.delete_files) {
        let dir = std::path::Path::new(&d.path);
        for name in ["docker.exe", "docker-compose.exe"] {
            let f = dir.join(name);
            if f.exists() && std::fs::remove_file(&f).is_err() {
                failed.push(f.display().to_string());
            }
        }
        // prázdnou složku bin uklidit (nadřazená WLS_Desktop může obsahovat další data)
        let _ = std::fs::remove_dir(dir);
    }
    let n = dups.len();
    if failed.is_empty() {
        Ok(tf!("Odstraněno duplicit: {n} – otevři nový terminál", "Duplicates removed: {n} – open a new terminal"))
    } else {
        Err(tf!(
            "PATH je upravený, ale nelze smazat: {} (neběží zrovna nějaký docker příkaz?)",
            "PATH was updated, but these could not be deleted: {} (is a docker command currently running?)",
            failed.join(", ")
        ))
    }
}

pub fn install() -> Result<String, String> {
    let src = source().ok_or(t!(
        "docker.exe (shim) nebyl nalezen vedle aplikace – sestav projekt (cargo build --release)",
        "docker.exe (shim) was not found next to the application – build the project (cargo build --release)"
    ))?;
    let dir = bin_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for name in ["docker.exe", "docker-compose.exe"] {
        // portable: docker.exe už ve složce je – zkopírovat jen docker-compose.exe
        if dir.join(name) == src {
            continue;
        }
        std::fs::copy(&src, dir.join(name))
            .map_err(|e| tf!("{name}: {e} (neběží zrovna nějaký docker příkaz?)", "{name}: {e} (is a docker command currently running?)"))?;
    }
    add_to_user_path(&dir.display().to_string())?;
    Ok(tf!(
        "Příkaz docker nainstalován do {} – otevři nový terminál",
        "The docker command was installed to {} – open a new terminal",
        dir.display()
    ))
}

/// Přidá složku na konec uživatelského PATH (pokud tam ještě není).
fn add_to_user_path(dir: &str) -> Result<(), String> {
    let d = ps_quote(dir);
    powershell(&format!(
        "$d={d}; $p=[Environment]::GetEnvironmentVariable('Path','User'); if(-not $p){{$p=''}}; \
         if(($p -split ';' | ForEach-Object {{ $_.TrimEnd('\\') }}) -notcontains $d){{ \
           [Environment]::SetEnvironmentVariable('Path', (($p.TrimEnd(';') + ';' + $d).TrimStart(';')), 'User') }}"
    ))?;
    Ok(())
}

/// Odebere složku (všechny její výskyty) z uživatelského PATH – pro cizí docker, který má
/// přednost. Systémový PATH se nemění (vyžaduje správce).
pub fn remove_from_user_path(dir: &str) -> Result<String, String> {
    let (_, user_path) = registry_path();
    let entries: Vec<&str> = user_path.split(';').map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
    let kept: Vec<&str> = entries.iter().copied().filter(|e| norm(e) != norm(dir)).collect();
    if kept.len() == entries.len() {
        return Err(tf!(
            "{dir} není v uživatelském PATH – je v systémovém, ten může změnit jen správce (Upravit proměnné prostředí systému)",
            "{dir} is not in the user PATH – it is in the system PATH, which only an administrator can change (Edit the system environment variables)"
        ));
    }
    powershell(&format!("[Environment]::SetEnvironmentVariable('Path', {}, 'User')", ps_quote(&kept.join(";"))))?;
    Ok(tf!("{dir} odebráno z PATH – otevři nový terminál", "{dir} removed from PATH – open a new terminal"))
}

pub fn uninstall() -> Result<String, String> {
    let dir = bin_dir();
    let d = ps_quote(&dir.display().to_string());
    powershell(&format!(
        "$d={d}; $p=[Environment]::GetEnvironmentVariable('Path','User'); if($p){{ \
           $n = ($p -split ';' | Where-Object {{ $_ -and ($_.TrimEnd('\\') -ne $d) }}) -join ';'; \
           [Environment]::SetEnvironmentVariable('Path', $n, 'User') }}"
    ))?;
    // portable: docker.exe je součást balíčku – smazat jen kopii docker-compose.exe
    let names: &[&str] = if crate::config::portable_dir().is_some() { &["docker-compose.exe"] } else { &["docker.exe", "docker-compose.exe"] };
    for name in names {
        let _ = std::fs::remove_file(dir.join(name));
    }
    Ok(t!("Příkaz docker odebrán z PATH", "The docker command was removed from PATH").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn only_dockers_before_ours_shadow() {
        let exts = v(&[".com", ".exe", ".bat", ".cmd"]);
        let present = |p: &std::path::Path| {
            let s = p.display().to_string().to_ascii_lowercase().replace('/', "\\");
            [r"c:\tools\docker.cmd", r"c:\old\docker.exe", r"c:\wls\docker.exe"].contains(&s.as_str())
        };
        // nástroj před naší složkou má přednost, stará kopie za ní ne (lomítko na konci, velikost písmen)
        let path = v(&[r"C:\Tools", r"C:\WLS\", r"C:\Old"]);
        assert_eq!(shadowing(&path, r"c:\wls", &exts, present), vec![(r"C:\Tools\docker.cmd".to_string(), r"C:\Tools".to_string())]);
        // naše složka v PATH není -> každý nalezený
        let path = v(&[r"C:\Tools", r"C:\Old"]);
        let files: Vec<String> = shadowing(&path, r"C:\WLS", &exts, present).into_iter().map(|(f, _)| f).collect();
        assert_eq!(files, v(&[r"C:\Tools\docker.cmd", r"C:\Old\docker.exe"]));
        // nic před námi
        let path = v(&[r"C:\WLS", r"C:\Tools", r"C:\Old"]);
        assert!(shadowing(&path, r"C:\WLS", &exts, present).is_empty());
    }

    #[test]
    fn duplicates_of_own_shim() {
        let legacy = r"C:\Users\u\AppData\Local\WLS_Desktop\bin";
        let shims = [r"c:\wls\wls-0.4.0", r"c:\users\u\appdata\local\wls_desktop\bin", r"c:\wls\wls-0.5.0"];
        let has = |p: &std::path::Path| shims.contains(&p.display().to_string().to_ascii_lowercase().as_str());
        let path = v(&[r"C:\Tools", r"C:\WLS\WLS-0.4.0", r"C:\WLS\WLS-0.5.0", r"C:\WLS\WLS-0.5.0\", legacy, r"C:\WLS\WLS-0.4.0"]);
        let d = duplicates(&path, r"C:\WLS\WLS-0.5.0", legacy, true, has);
        assert_eq!(
            d,
            vec![
                Dup { path: r"C:\WLS\WLS-0.4.0".into(), in_path: true, delete_files: false },
                Dup { path: r"C:\WLS\WLS-0.5.0\".into(), in_path: true, delete_files: false },
                Dup { path: legacy.into(), in_path: true, delete_files: true },
            ]
        );
        // výchozí instalace mimo PATH se zbylými soubory (stav po přechodu na portable)
        let d = duplicates(&v(&[r"C:\WLS\WLS-0.5.0"]), r"C:\WLS\WLS-0.5.0", legacy, true, has);
        assert_eq!(d, vec![Dup { path: legacy.into(), in_path: false, delete_files: true }]);
        // bez zbylých souborů ve výchozí složce nic
        assert!(duplicates(&v(&[r"C:\WLS\WLS-0.5.0"]), r"C:\WLS\WLS-0.5.0", legacy, false, has).is_empty());
        // výchozí instalace je ta aktuální -> nic
        assert!(duplicates(&v(&[legacy]), legacy, legacy, true, has).is_empty());
    }

    #[test]
    fn moved_package_leaves_stale_entry() {
        // balíček přesunutý z podsložky o úroveň výš: stará položka PATH už neexistuje
        let gone = std::env::temp_dir().join("wls-test-nonexistent").join("WLS-Desktop-0.5.0-win-x64");
        assert!(stale_wls_dir(&gone));
        assert!(!stale_wls_dir(&std::env::temp_dir().join("wls-test-nonexistent").join("tools")));
        assert!(!stale_wls_dir(&std::env::temp_dir()));
        // existující, ale prázdná složka balíčku (soubory přesunuté jinam)
        let empty = std::env::temp_dir().join(format!("wls-desktop-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        assert!(stale_wls_dir(&empty));
        std::fs::write(empty.join("docker.exe"), b"x").unwrap();
        assert!(!stale_wls_dir(&empty));
        let _ = std::fs::remove_dir_all(&empty);
    }
}
