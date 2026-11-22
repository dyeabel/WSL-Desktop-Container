//! Volume: velikosti, záloha do Windows, obnova ze zálohy a klonování.
//!
//! Záloha, obnova i klon běží v dočasném kontejneru (alpine, `--rm`). Názvy a cesty se
//! předávají jako samostatné argumenty – nikdy se nevkládají do textu shellového skriptu.

use super::{Msg, Worker};
use crate::host::{Host, HostExt, Privilege};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

/// Pomocný image pro práci s daty volume.
pub const HELPER_IMAGE: &str = "docker.io/library/alpine:3.20";

/// Smaže obsah /data včetně skrytých souborů (ale ne /data samotné).
const CLEAR: &str = "rm -rf /data/* /data/..?* /data/.[!.]* ; ";

/// Platný název volume (stejné pravidlo jako Docker: `[a-zA-Z0-9][a-zA-Z0-9_.-]*`).
pub fn valid_volume_name(n: &str) -> bool {
    let mut ch = n.chars();
    matches!(ch.next(), Some(c) if c.is_ascii_alphanumeric())
        && ch.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Výchozí název souboru zálohy: `<volume>-<YYYYMMDD-HHMM>.tar.gz`.
pub fn backup_file_name(volume: &str, (y, mo, d, h, mi): (u16, u16, u16, u16, u16)) -> String {
    format!("{volume}-{y:04}{mo:02}{d:02}-{h:02}{mi:02}.tar.gz")
}

/// Windows cesta k souboru -> (složka, název souboru).
pub fn split_target(path: &str) -> Result<(String, String), String> {
    let p = Path::new(path.trim().trim_matches('"'));
    let file = p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    let dir = p.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
    if file.is_empty() || dir.is_empty() || file.contains(['/', '\\', '\0']) {
        return Err(tf!("Neplatná cesta k souboru: {path}", "Invalid file path: {path}"));
    }
    Ok((dir, file))
}

/// Linuxová složka pro `-v <dir>:/backup` – dvojtečka by rozbila zápis přípojného bodu.
fn check_mount_dir(dir: &str) -> Result<(), String> {
    if !dir.starts_with('/') || dir.contains(':') {
        return Err(tf!("Složku {dir} nelze připojit do kontejneru", "Folder {dir} cannot be mounted into a container"));
    }
    Ok(())
}

/// Komprimovaný archiv (gzip) podle přípony.
pub fn is_gzip(file: &str) -> bool {
    let f = file.to_ascii_lowercase();
    f.ends_with(".gz") || f.ends_with(".tgz")
}

fn base_run(cli: &[String]) -> Vec<String> {
    let mut a = cli.to_vec();
    a.extend(["run".into(), "--rm".into()]);
    a
}

/// `<cli> run --rm -v <volume>:/data:ro -v <dir>:/backup alpine tar czf /backup/<file> -C /data .`
pub fn backup_args(cli: &[String], volume: &str, dir: &str, file: &str) -> Vec<String> {
    let mut a = base_run(cli);
    a.extend([
        "-v".into(),
        format!("{volume}:/data:ro"),
        "-v".into(),
        format!("{dir}:/backup"),
        HELPER_IMAGE.into(),
        "tar".into(),
        "czf".into(),
        format!("/backup/{file}"),
        "-C".into(),
        "/data".into(),
        ".".into(),
    ]);
    a
}

/// Obnova: volume rw, složka se zálohou ro; název souboru jde do skriptu jako `$1`.
pub fn restore_args(cli: &[String], volume: &str, dir: &str, file: &str, clear: bool) -> Vec<String> {
    let tar = if is_gzip(file) { r#"tar xzf "/backup/$1" -C /data"# } else { r#"tar xf "/backup/$1" -C /data"# };
    let script = format!("{}{tar}", if clear { CLEAR } else { "" });
    let mut a = base_run(cli);
    a.extend([
        "-v".into(),
        format!("{volume}:/data"),
        "-v".into(),
        format!("{dir}:/backup:ro"),
        HELPER_IMAGE.into(),
        "sh".into(),
        "-c".into(),
        script,
        "sh".into(),
        file.into(),
    ]);
    a
}

/// Klon: zdroj ro, cíl rw, `cp -a` zachová práva, vlastníky i skryté soubory.
pub fn clone_args(cli: &[String], from: &str, to: &str) -> Vec<String> {
    let mut a = base_run(cli);
    a.extend([
        "-v".into(),
        format!("{from}:/from:ro"),
        "-v".into(),
        format!("{to}:/data"),
        HELPER_IMAGE.into(),
        "cp".into(),
        "-a".into(),
        "/from/.".into(),
        "/data/".into(),
    ]);
    a
}

/// Velikosti z `GET /system/df` (`Volumes[].UsageData.Size`); záporné (= neznámé) se vynechají.
pub fn parse_df_sizes(v: &Value) -> HashMap<String, u64> {
    v.get("Volumes")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|e| {
                    let name = e.get("Name")?.as_str()?;
                    let size = e.pointer("/UsageData/Size")?.as_i64()?;
                    (size >= 0).then(|| (name.to_string(), size as u64))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Výstup `du -sb <cesty…>` (`velikost\tcesta`) -> velikosti podle názvu volume.
pub fn parse_du(out: &str, by_path: &HashMap<String, String>) -> HashMap<String, u64> {
    out.lines()
        .filter_map(|l| {
            let (size, path) = l.split_once('\t')?;
            let name = by_path.get(path.trim_end_matches('\r'))?;
            Some((name.clone(), size.trim().parse().ok()?))
        })
        .collect()
}

/// Velikosti přes `du -sb` jako root (jedno volání pro všechny cesty; `by_path`: mountpoint -> název).
pub fn du_sizes(host: &dyn Host, by_path: &HashMap<String, String>) -> HashMap<String, u64> {
    if by_path.is_empty() {
        return HashMap::new();
    }
    // cesty jako argumenty skriptu; chyby (smazaný volume apod.) se ignorují
    let mut a: Vec<String> = vec!["sh".into(), "-c".into(), r#"du -sb -- "$@" 2>/dev/null; true"#.into(), "sh".into()];
    a.extend(by_path.keys().cloned());
    host.run_owned(Privilege::Engine, None, &a).map(|out| parse_du(&out, by_path)).unwrap_or_default()
}

impl Worker {
    /// Velikosti všech volume: z /system/df, chybějící doplní `du` (jako root, jedním voláním).
    pub(super) fn volume_sizes(&self) -> Result<String, String> {
        let d = self.docker();
        let mut sizes = d.get::<Value>("/system/df").map(|v| parse_df_sizes(&v)).unwrap_or_default();
        let list: Value = d.get("/volumes")?;
        let mut by_path = HashMap::new();
        for v in list.get("Volumes").and_then(|a| a.as_array()).into_iter().flatten() {
            let (Some(name), Some(mp)) = (v.get("Name").and_then(|x| x.as_str()), v.get("Mountpoint").and_then(|x| x.as_str())) else {
                continue;
            };
            if !sizes.contains_key(name) && mp.starts_with('/') {
                by_path.insert(mp.to_string(), name.to_string());
            }
        }
        sizes.extend(du_sizes(&*self.host(), &by_path));
        self.send(Msg::VolumeSizes(sizes));
        Ok(String::new())
    }

    fn cli(&self) -> Vec<String> {
        let c = self.cfg();
        c.engine.cli_for(&self.host().engine_endpoint(&c), c.tls)
    }

    /// Běžící kontejnery, které mají volume připojený: (id, název).
    fn volume_users(&self, volume: &str) -> Vec<(String, String)> {
        self.docker()
            .snapshot()
            .containers
            .iter()
            .filter(|c| c.running() && c.mounts.iter().flatten().any(|m| m.name.as_deref() == Some(volume)))
            .map(|c| (c.id.clone(), c.name()))
            .collect()
    }

    fn volume_exists(&self, name: &str) -> bool {
        self.docker().get::<Value>(&format!("/volumes/{}", crate::docker::enc(name))).is_ok()
    }

    fn create_volume(&self, name: &str) -> Result<(), String> {
        if !valid_volume_name(name) {
            return Err(tf!("Neplatný název volume: {name}", "Invalid volume name: {name}"));
        }
        if self.volume_exists(name) {
            return Err(tf!("Volume {name} už existuje", "Volume {name} already exists"));
        }
        self.docker().post_json("/volumes/create", serde_json::json!({ "Name": name })).map(|_| ()).map_err(String::from)
    }

    fn remove_volume(&self, name: &str) {
        let _ = self.docker().delete(&format!("/volumes/{}", crate::docker::enc(name)));
    }

    pub(super) fn backup_volume(&self, volume: String, target: String, stop: bool) -> Result<String, String> {
        let host = self.host();
        let (dir, file) = split_target(&target)?;
        let ldir = host.to_guest_path(&dir)?;
        check_mount_dir(&ldir)?;
        self.send(Msg::Toast(true, tf!("Zálohuji volume {volume}…", "Backing up volume {volume}…")));
        let mut stopped = Vec::new();
        if stop {
            for (id, name) in self.volume_users(&volume) {
                self.mark_intended(&id);
                self.docker()
                    .container_action(&id, "stop")
                    .map_err(|e| tf!("Kontejner {name} nelze zastavit: {e}", "Cannot stop container {name}: {e}"))?;
                stopped.push(id);
            }
        }
        let res = host.run_owned(Privilege::User, None, &backup_args(&self.cli(), &volume, &ldir, &file));
        // zastavené kontejnery spustit znovu i při chybě zálohy
        let mut restart_err = String::new();
        for id in &stopped {
            if let Err(e) = self.docker().container_action(id, "start") {
                restart_err = tf!(" Kontejner se nepodařilo znovu spustit: {e}", " A container could not be started again: {e}");
            }
        }
        res.map_err(|e| tf!("Záloha {volume} selhala: {e}{restart_err}", "Backup of {volume} failed: {e}{restart_err}"))?;
        let size = std::fs::metadata(target.trim().trim_matches('"')).map(|m| crate::ui::fmt_bytes(m.len())).unwrap_or_else(|_| "?".into());
        let msg = tf!("Záloha {volume} uložena: {file} ({size}).", "Backup of {volume} saved: {file} ({size}).");
        if restart_err.is_empty() { Ok(msg) } else { Err(format!("{msg}{restart_err}")) }
    }

    pub(super) fn restore_volume(&self, source: String, volume: String, create: bool, clear: bool) -> Result<String, String> {
        let host = self.host();
        let (dir, file) = split_target(&source)?;
        let ldir = host.to_guest_path(&dir)?;
        check_mount_dir(&ldir)?;
        if create {
            self.create_volume(&volume)?;
        } else if !self.volume_exists(&volume) {
            return Err(tf!("Volume {volume} neexistuje", "Volume {volume} does not exist"));
        }
        self.send(Msg::Toast(true, tf!("Obnovuji volume {volume} z {file}…", "Restoring volume {volume} from {file}…")));
        match host.run_owned(Privilege::User, None, &restore_args(&self.cli(), &volume, &ldir, &file, clear)) {
            Ok(_) => {
                let _ = self.volume_sizes();
                Ok(tf!("Volume {volume} obnoven z {file}", "Volume {volume} restored from {file}"))
            }
            Err(e) => {
                if create {
                    self.remove_volume(&volume);
                }
                Err(tf!("Obnova {volume} selhala: {e}", "Restore of {volume} failed: {e}"))
            }
        }
    }

    pub(super) fn clone_volume(&self, from: String, to: String) -> Result<String, String> {
        let host = self.host();
        self.create_volume(&to)?;
        self.send(Msg::Toast(true, tf!("Klonuji volume {from} do {to}…", "Cloning volume {from} into {to}…")));
        match host.run_owned(Privilege::User, None, &clone_args(&self.cli(), &from, &to)) {
            Ok(_) => {
                let _ = self.volume_sizes();
                Ok(tf!("Volume {from} naklonován do {to}", "Volume {from} cloned into {to}"))
            }
            Err(e) => {
                self.remove_volume(&to);
                Err(tf!("Klonování {from} selhalo: {e}", "Cloning {from} failed: {e}"))
            }
        }
    }
}

/// Worker pro živé testy (bez UI).
#[cfg(test)]
pub fn test_worker(cfg: crate::config::Config) -> (Worker, std::sync::mpsc::Receiver<Msg>) {
    use std::sync::{Arc, RwLock};
    let (msg, msg_rx) = std::sync::mpsc::channel();
    let (cmd, _cmd_rx) = std::sync::mpsc::channel();
    let w = Worker { ctx: eframe::egui::Context::default(), cfg: Arc::new(RwLock::new(cfg)), msg, cmd, shared: Arc::new(super::Shared::default()) };
    (w, msg_rx)
}

/// Spuštění operací s volume pro živé testy.
#[cfg(test)]
pub fn volume_op_for_test(cfg: crate::config::Config, cmd: super::Cmd) -> Result<String, String> {
    let (w, _rx) = test_worker(cfg);
    w.exec_inner(cmd)
}

/// Velikosti volume pro živé testy.
#[cfg(test)]
pub fn volume_sizes_for_test(cfg: crate::config::Config) -> HashMap<String, u64> {
    let (w, rx) = test_worker(cfg);
    w.volume_sizes().expect("sizes");
    rx.try_iter()
        .find_map(|m| if let Msg::VolumeSizes(s) = m { Some(s) } else { None })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli() -> Vec<String> {
        vec!["podman".into(), "--url".into(), "tcp://127.0.0.1:2375".into()]
    }

    #[test]
    fn backup_args_keep_spaces_and_quotes_in_one_item() {
        let a = backup_args(&cli(), "my_vol", "/mnt/c/Users/Jan Novák/My \"Backups\"", "it's a \"b\".tar.gz");
        assert_eq!(
            a,
            [
                "podman", "--url", "tcp://127.0.0.1:2375", "run", "--rm",
                "-v", "my_vol:/data:ro",
                "-v", "/mnt/c/Users/Jan Novák/My \"Backups\":/backup",
                HELPER_IMAGE, "tar", "czf", "/backup/it's a \"b\".tar.gz", "-C", "/data", ".",
            ]
        );
    }

    #[test]
    fn restore_args_pass_file_as_argument() {
        let file = "x'; rm -rf / #.tar.gz";
        let a = restore_args(&["docker".to_string()], "vol", "/mnt/d/my dir", file, true);
        assert_eq!(&a[..7], ["docker", "run", "--rm", "-v", "vol:/data", "-v", "/mnt/d/my dir:/backup:ro"]);
        assert_eq!(a[7], HELPER_IMAGE);
        assert_eq!(&a[8..10], ["sh", "-c"]);
        // skript je konstantní, název souboru je až poslední argument ($1)
        assert_eq!(a[10], r#"rm -rf /data/* /data/..?* /data/.[!.]* ; tar xzf "/backup/$1" -C /data"#);
        assert!(!a[10].contains("rm -rf / #"));
        assert_eq!(&a[11..], ["sh", file]);
        // bez mazání a nekomprimovaný tar
        let b = restore_args(&["docker".to_string()], "vol", "/mnt/d", "a b.tar", false);
        assert_eq!(b[10], r#"tar xf "/backup/$1" -C /data"#);
        assert_eq!(b.last().unwrap(), "a b.tar");
    }

    #[test]
    fn clone_args_build() {
        let a = clone_args(&["docker".to_string()], "src", "dst");
        assert_eq!(a, ["docker", "run", "--rm", "-v", "src:/from:ro", "-v", "dst:/data", HELPER_IMAGE, "cp", "-a", "/from/.", "/data/"]);
    }

    #[test]
    #[cfg(windows)]
    fn split_and_names() {
        assert_eq!(
            split_target(r#""C:\Users\Jan Novák\zálohy\it's.tar.gz""#).unwrap(),
            (r"C:\Users\Jan Novák\zálohy".to_string(), "it's.tar.gz".to_string())
        );
        assert_eq!(split_target(r"D:\a.tar").unwrap(), (r"D:\".to_string(), "a.tar".to_string()));
        assert!(split_target("").is_err());
    }

    #[test]
    fn volume_names_and_paths() {
        assert_eq!(backup_file_name("db_data", (2026, 3, 7, 9, 5)), "db_data-20260307-0905.tar.gz");
        assert!(valid_volume_name("a.b_c-1"));
        assert!(!valid_volume_name(""));
        assert!(!valid_volume_name("-a"));
        assert!(!valid_volume_name("a b"));
        assert!(!valid_volume_name("a:b"));
        assert!(is_gzip("X.TAR.GZ") && is_gzip("x.tgz") && !is_gzip("x.tar"));
        assert!(check_mount_dir("/mnt/c/x y").is_ok());
        assert!(check_mount_dir("/mnt/c/a:b").is_err());
        assert!(check_mount_dir(r"C:\x").is_err());
    }

    #[test]
    fn parses_df_sizes() {
        let v: Value = serde_json::from_str(
            r#"{"Volumes":[
                {"Name":"a","UsageData":{"RefCount":0,"Size":100000}},
                {"Name":"b","UsageData":{"RefCount":1,"Size":-1}},
                {"Name":"c","UsageData":null},
                {"Name":"d"}
            ]}"#,
        )
        .unwrap();
        let s = parse_df_sizes(&v);
        assert_eq!(s.len(), 1);
        assert_eq!(s["a"], 100000);
        assert!(parse_df_sizes(&serde_json::json!({"Volumes": null})).is_empty());
    }

    #[test]
    fn parses_du() {
        let by_path: HashMap<String, String> = [
            ("/var/lib/docker/volumes/a/_data".to_string(), "a".to_string()),
            ("/var/lib/docker/volumes/b c/_data".to_string(), "b c".to_string()),
        ]
        .into();
        let out = "4096\t/var/lib/docker/volumes/a/_data\r\n12345\t/var/lib/docker/volumes/b c/_data\nbad line\n7\t/unknown\n";
        let s = parse_du(out, &by_path);
        assert_eq!(s.len(), 2);
        assert_eq!(s["a"], 4096);
        assert_eq!(s["b c"], 12345);
    }
}
