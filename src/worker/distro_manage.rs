//! Správa WSL distribucí: odebrání (s volitelnou zálohou), export, import a disk (VHDX).

use super::tracking::last_running;
use super::{Msg, Worker};
use crate::wsl::{self, ExportFormat};

/// Akce nad distribucí zadaná z UI.
#[derive(Debug, Clone)]
pub enum DistroOp {
    /// odregistrovat distribuci; `backup` = nejdřív exportovat do souboru .tar
    Remove { name: String, backup: Option<String> },
    Export { name: String, file: String, format: ExportFormat },
    Import { name: String, folder: String, file: String },
    /// zjistit cestu a velikost virtuálního disku -> Msg::DistroDisk
    DiskInfo(String),
    /// `wsl --manage <name> --set-sparse true`
    SetSparse(String),
    /// `wsl --manage <name> --compact` (distribuce se nejdřív ukončí)
    Compact(String),
}

/// Virtuální disk distribuce.
#[derive(Debug, Clone)]
pub struct DistroDisk {
    pub path: String,
    /// velikost souboru (u zhuštěného disku logická)
    pub size: u64,
    pub sparse: bool,
}

/// Výchozí složka pro import: %LOCALAPPDATA%\wsl\<name>.
pub fn default_import_folder(name: &str) -> String {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| r"C:\WSL".into());
    format!(r"{base}\wsl\{name}")
}

/// Disk je pořád připojený k VM WSL – srozumitelnější chyba s radou.
fn in_use_hint(name: &str, e: String) -> String {
    if e.contains("SHARING_VIOLATION") {
        tf!(
            "Disk {name} je pořád připojený ve WSL – použij „Vypnout celé WSL“ (zastaví i engine) a zkus to znovu",
            "The disk of {name} is still attached in WSL – use “Shut down all of WSL” (also stops the engine) and try again"
        )
    } else {
        e
    }
}

impl Worker {
    pub(super) fn distro_op(&self, op: DistroOp) -> Result<String, String> {
        match op {
            DistroOp::Remove { name, backup } => {
                let mut log = String::new();
                let res = self.remove_distro(&name, backup, &mut log);
                self.send(Msg::Output(tf!("Odebrání: {name}", "Removal: {name}"), log));
                res
            }
            DistroOp::Export { name, file, format } => {
                let mut log = String::new();
                let res = self.export_distro(&name, &file, format, &mut log);
                self.send(Msg::Output(tf!("Export: {name}", "Export: {name}"), log));
                res.map(|_| tf!("Distribuce {name} exportována do {file}", "Distribution {name} exported to {file}"))
            }
            DistroOp::Import { name, folder, file } => self.import_distro(&name, &folder, &file),
            DistroOp::DiskInfo(name) => {
                // naplní cache `wsl --help`, ať UI může podporu přepínačů číst bez čekání
                let _ = wsl::manage_support();
                let r = match wsl::vhd_path(&name) {
                    Some(p) => std::fs::metadata(&p)
                        .map(|m| DistroDisk { path: p.display().to_string(), size: m.len(), sparse: wsl::is_sparse(&m) })
                        .map_err(|e| format!("{}: {e}", p.display())),
                    None => Err(t!("Disk distribuce nebyl v registru nalezen", "The distribution disk was not found in the registry").into()),
                };
                self.send(Msg::DistroDisk(name, r));
                Ok(String::new())
            }
            DistroOp::SetSparse(name) => {
                self.stop_distro_for(&name);
                let res = wsl::set_sparse(&name);
                let _ = self.cmd.send(super::Cmd::Distro(DistroOp::DiskInfo(name.clone())));
                res.map_err(|e| in_use_hint(&name, e.into()))?;
                Ok(tf!("Disk distribuce {name} je zhuštěný (sparse)", "The disk of {name} is now sparse"))
            }
            DistroOp::Compact(name) => {
                self.stop_distro_for(&name);
                self.send(Msg::Toast(true, tf!("Komprimuji disk {name}…", "Compacting the disk of {name}…")));
                let before = wsl::vhd_path(&name).and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len());
                let res = wsl::compact(&name);
                let after = wsl::vhd_path(&name).and_then(|p| std::fs::metadata(p).ok()).map(|m| m.len());
                let _ = self.cmd.send(super::Cmd::Distro(DistroOp::DiskInfo(name.clone())));
                res.map_err(|e| {
                    if e.contains("VIRTUAL_DISK_LIMITATION") {
                        tf!(
                            "Disk {name} je zhuštěný (sparse) – ten komprimovat nejde, uvolněné místo vrací WSL samo",
                            "The disk of {name} is sparse – it cannot be compacted, WSL returns freed space by itself"
                        )
                    } else {
                        in_use_hint(&name, e.into())
                    }
                })?;
                Ok(match (before, after) {
                    (Some(b), Some(a)) => tf!(
                        "Disk {name} zkomprimován: {} → {}",
                        "Disk of {name} compacted: {} → {}",
                        crate::ui::fmt_bytes(b),
                        crate::ui::fmt_bytes(a)
                    ),
                    _ => tf!("Disk {name} zkomprimován", "Disk of {name} compacted"),
                })
            }
        }
    }

    /// Ukončí distribuci; u distribuce aplikace nejdřív zastaví keep-alive a sledování kontejnerů.
    fn stop_distro_for(&self, name: &str) {
        if name.eq_ignore_ascii_case(&self.cfg().distro) {
            self.suppress_tracking();
            self.kill_keepalive();
        }
        let _ = wsl::terminate(name);
    }

    fn export_distro(&self, name: &str, file: &str, format: ExportFormat, log: &mut String) -> Result<(), String> {
        let args = wsl::export_args(name, file, format, wsl::export_has_format_flag());
        log.push_str(&format!("wsl {}\n", args.join(" ")));
        self.send(Msg::Toast(
            true,
            tf!("Exportuji {name} do {file}… (může trvat několik minut)", "Exporting {name} to {file}… (may take several minutes)"),
        ));
        self.send(Msg::Output(tf!("Export: {name}", "Export: {name}"), format!("{log}{}\n", t!("Probíhá…", "Running…"))));
        if let Some(d) = std::path::Path::new(file).parent() {
            if !d.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(d);
            }
        }
        match wsl::export(name, file, format) {
            Ok(o) => {
                log.push_str(&o);
                let size = std::fs::metadata(file).map(|m| crate::ui::fmt_bytes(m.len())).unwrap_or_default();
                log.push_str(&tf!("\nHotovo: {file} ({size})\n", "\nDone: {file} ({size})\n"));
                Ok(())
            }
            Err(e) => {
                log.push_str(&e);
                Err(tf!("Export distribuce {name} selhal – viz výstup", "Export of distribution {name} failed – see output"))
            }
        }
    }

    fn remove_distro(&self, name: &str, backup: Option<String>, log: &mut String) -> Result<String, String> {
        // distribuci, kterou aplikace používá pro engine, odebrat nejde
        if name.eq_ignore_ascii_case(&self.cfg().distro) {
            return Err(tf!(
                "{name} používá aplikace pro engine – nejdřív v Nastavení přepni distribuci",
                "{name} is used by the app for the engine – switch the distribution in Settings first"
            ));
        }
        if !wsl::list().map(|l| l.iter().any(|d| d.name == name)).unwrap_or(false) {
            return Err(tf!("Distribuce {name} neexistuje", "Distribution {name} does not exist"));
        }
        if let Some(file) = backup.as_deref().filter(|f| !f.trim().is_empty()) {
            log.push_str(&format!("=== {} ===\n", t!("Záloha", "Backup")));
            self.export_distro(name, file, ExportFormat::from_path(file), log)
                .map_err(|_| tf!("Záloha {name} selhala – distribuce nebyla odebrána", "Backup of {name} failed – the distribution was not removed"))?;
        }
        log.push_str(&format!("\n=== {} ===\n", t!("Odebrání", "Removal")));
        log.push_str(&format!("wsl --terminate {name}\n"));
        let _ = wsl::terminate(name);
        log.push_str(&format!("wsl --unregister {name}\n"));
        match wsl::unregister(name) {
            Ok(o) => log.push_str(&o),
            Err(e) => {
                log.push_str(&e);
                return Err(tf!("Odebrání distribuce {name} selhalo – viz výstup", "Removing distribution {name} failed – see output"));
            }
        }
        last_running::forget_distro(name);
        Ok(match backup {
            Some(f) => tf!("Distribuce {name} odebrána (záloha v {f})", "Distribution {name} removed (backup in {f})"),
            None => tf!("Distribuce {name} odebrána", "Distribution {name} removed"),
        })
    }

    fn import_distro(&self, name: &str, folder: &str, file: &str) -> Result<String, String> {
        if !wsl::valid_distro_name(name) {
            return Err(t!("Neplatný název distribuce", "Invalid distribution name").into());
        }
        if wsl::list().map(|l| l.iter().any(|d| d.name.eq_ignore_ascii_case(name))).unwrap_or(false) {
            return Err(tf!("Distribuce {name} už existuje", "Distribution {name} already exists"));
        }
        if !std::path::Path::new(file).is_file() {
            return Err(tf!("Soubor {file} neexistuje", "File {file} does not exist"));
        }
        let folder = if folder.trim().is_empty() { default_import_folder(name) } else { folder.trim().to_string() };
        std::fs::create_dir_all(&folder).map_err(|e| format!("{folder}: {e}"))?;
        let args = wsl::import_args(name, &folder, file);
        let mut log = format!("wsl {}\n", args.join(" "));
        self.send(Msg::Toast(true, tf!("Importuji {name}…", "Importing {name}…")));
        let res = wsl::import(name, &folder, file);
        match &res {
            Ok(o) => log.push_str(o),
            Err(e) => log.push_str(e),
        }
        self.send(Msg::Output(tf!("Import: {name}", "Import: {name}"), log));
        res.map(|_| tf!("Distribuce {name} importována do {folder}", "Distribution {name} imported into {folder}"))
            .map_err(|_| tf!("Import distribuce {name} selhal – viz výstup", "Import of distribution {name} failed – see output"))
    }
}
