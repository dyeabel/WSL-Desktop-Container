//! WSLCD Lite: lehká distribuce jen pro kontejnery po vzoru Kiri Linux – Alpine minirootfs
//! (~4 MB), bez systemd, start za zlomek sekundy. Služby enginu (API, TLS proxy, DNS) běží
//! přes minimální náhradu `systemctl` (`systemctl.sh`), takže stávající skripty nastavení
//! enginu fungují beze změny. Distribuce leží v datové složce aplikace (portable).

use crate::host::{Host, HostExt, Privilege};
use crate::wsl;
use std::path::{Path, PathBuf};

/// Hodnota `AddDistroSpec::distro` pro lehkou distribuci.
pub const LITE_ID: &str = "wslcd-lite";
pub const DEFAULT_NAME: &str = "WSLCD-Lite";
pub const ALPINE_VERSION: &str = "3.24.4";
/// SHA-256 z `latest-releases.yaml` (ověřeno 2026-11-21); stažený soubor se proti němu ověří.
pub const ALPINE_SHA256: &str = "cdaa38cb8de3537967ee5b6c9b6cd00cc962da2b018e6eea48696bb274923012";
pub const SYSTEMCTL: &str = include_str!("systemctl.sh");

pub fn rootfs_name() -> String {
    format!("alpine-minirootfs-{ALPINE_VERSION}-x86_64.tar.gz")
}

pub fn rootfs_url() -> String {
    let minor: Vec<&str> = ALPINE_VERSION.split('.').take(2).collect();
    format!("https://dl-cdn.alpinelinux.org/alpine/v{}/releases/x86_64/{}", minor.join("."), rootfs_name())
}

/// Složka distribuce v datové složce aplikace (portable).
pub fn distro_dir(name: &str) -> Option<PathBuf> {
    crate::config::data_dir().map(|d| d.join("distros").join(name))
}

pub fn sha256_hex(data: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, data).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Stáhne rootfs Alpine do `<data>\cache` (jednou) a ověří SHA-256. Vrací cestu.
pub fn fetch_rootfs() -> Result<PathBuf, String> {
    if std::env::consts::ARCH != "x86_64" {
        return Err(t!("WSLCD Lite je zatím jen pro x86-64", "WSLCD Lite is x86-64 only for now").into());
    }
    let dir = crate::config::data_dir().ok_or("data_dir")?.join("cache");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = dir.join(rootfs_name());
    if let Ok(data) = std::fs::read(&file) {
        if sha256_hex(&data) == ALPINE_SHA256 {
            return Ok(file);
        }
    }
    let resp = ureq::get(&rootfs_url()).timeout(std::time::Duration::from_secs(120)).call().map_err(|e| format!("{}: {e}", rootfs_url()))?;
    let mut data = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(resp.into_reader(), 64 * 1024 * 1024), &mut data).map_err(|e| e.to_string())?;
    let got = sha256_hex(&data);
    if got != ALPINE_SHA256 {
        return Err(tf!("Kontrolní součet staženého Alpine nesedí ({got}) – soubor se nepoužije", "Checksum of the downloaded Alpine does not match ({got}) – the file is not used"));
    }
    std::fs::write(&file, &data).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(file)
}

/// `wsl --import <name> <data>\distros\<name> <rootfs> --version 2`.
pub fn import(name: &str, rootfs: &Path) -> Result<String, String> {
    let dir = distro_dir(name).ok_or("data_dir")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let args = wsl::import_args(name, &dir.display().to_string(), &rootfs.display().to_string());
    let out = wsl::hidden("wsl.exe").args(&args).arg("--version").arg("2").output().map_err(|e| e.to_string())?;
    let text = format!("{}{}", wsl::decode(&out.stdout), wsl::decode(&out.stderr));
    if out.status.success() { Ok(text) } else { Err(text) }
}

/// Příprava čerstvého Alpine (root): základní nástroje, které používají skripty aplikace,
/// náhrada systemctl, /etc/wsl.conf (start služeb při bootu, bez interop s Windows) a
/// nastavení Podmanu bez systemd. Skript systemctl přichází na stdin.
pub const PREPARE: &str = r#"set -e
cat > /usr/local/bin/systemctl
chmod 755 /usr/local/bin/systemctl
# jednotky píšou skripty aplikace stejně jako pro systemd
mkdir -p /etc/systemd/system /etc/tmpfiles.d
apk update -q
# skripty aplikace: groupadd/usermod (shadow), ss (síťový přehled), nsenter (sondy),
# ps -o comm,rss (paměť enginu), bash (sondy /dev/tcp), iptables (síť kontejnerů),
# find -printf (stránka Soubory) a du -b (velikosti volume) – busybox je nemá
apk add -q --no-cache shadow iproute2-ss util-linux-misc procps-ng bash iptables ip6tables ca-certificates tzdata findutils coreutils
cat > /etc/wsl.conf <<'EOF'
# WSLCD Lite: služby enginu startuje náhrada systemctl (bez systemd)
[boot]
systemd = false
command = /usr/local/bin/systemctl --boot

[user]
default = root

[interop]
# nic uvnitř nepotřebuje spouštět programy Windows
enabled = false
appendWindowsPath = false
EOF
# Podman bez systemd: cgroupfs a log událostí do souboru
mkdir -p /etc/containers/containers.conf.d
printf '# WSLCD Lite (bez systemd)\n[engine]\ncgroup_manager = "cgroupfs"\nevents_logger = "file"\n' > /etc/containers/containers.conf.d/40-wslcd-lite.conf
echo "WSLCD Lite: Alpine $(cat /etc/alpine-release) připraven"
"#;

pub fn prepare(host: &dyn Host) -> Result<String, String> {
    host.run_stdin(Privilege::Root, &["sh", "-c", PREPARE], SYSTEMCTL).map_err(|e| e.to_string())
}

/// Obnoví náhradu systemctl v už existující distribuci WSLCD Lite (opravy skriptu se tak
/// dostanou i do starších instalací – volá se před nastavením API). V jiné distribuci nic nedělá.
const REFRESH: &str = r#"S=$(cat)
if [ -d /run/systemd/system ] || ! grep -qs 'WSL Container Desktop' /usr/local/bin/systemctl; then exit 0; fi
printf '%s\n' "$S" > /usr/local/bin/systemctl.new && chmod 755 /usr/local/bin/systemctl.new && mv -f /usr/local/bin/systemctl.new /usr/local/bin/systemctl
"#;

pub fn refresh_shim(host: &dyn Host) {
    let _ = host.run_stdin(Privilege::Root, &["sh", "-c", REFRESH], SYSTEMCTL);
}

/// Shell test „běží systemd, nebo náhrada WSLCD Lite“ (pro `sh -c`); `/run/wslcd-init`
/// vytváří náhrada systemctl při startu distribuce.
pub const HAS_INIT: &str = "test -d /run/systemd/system || test -d /run/wslcd-init";

#[cfg(test)]
mod live_lite;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_and_hash() {
        assert_eq!(rootfs_url(), "https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/alpine-minirootfs-3.24.4-x86_64.tar.gz");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(ALPINE_SHA256.len(), 64);
    }

    #[test]
    fn systemctl_shim_is_posix_and_complete() {
        assert!(SYSTEMCTL.starts_with("#!/bin/sh"));
        for cmd in ["--boot)", "start)", "stop)", "restart|", "enable)", "disable)", "is-active)", "cat)", "show)", "daemon-reload|"] {
            assert!(SYSTEMCTL.contains(cmd), "{cmd}");
        }
        assert!(!SYSTEMCTL.contains("[["), "jen POSIX sh (busybox ash)");
        assert!(SYSTEMCTL.contains("cd / && exec env -i"), "služby z / s čistým prostředím jako v systemd");
        assert!(SYSTEMCTL.contains("setsid sh -c"), "služby ve vlastní relaci");
        assert!(SYSTEMCTL.contains("podman start --all --filter restart-policy=always"), "vestavěný podman-restart");
        assert!(SYSTEMCTL.contains("start_unit podman-restart.service"), "kontejnery při bootu až po enginu");
        for p in ["get_last \"$u\" Type", "= oneshot", "get_last \"$u\" OOMScoreAdjust", "/proc/self/oom_score_adj"] {
            assert!(SYSTEMCTL.contains(p), "{p}");
        }
        let g = SYSTEMCTL.find("start_unit wslcd-guard.service").expect("guard při bootu");
        assert!(g < SYSTEMCTL.find("for f in \"$STATE\"/wslcd-dns.service").unwrap(), "guard první");
        assert!(SYSTEMCTL.contains("podman-restart.service|wslcd-guard.service) continue"), "oneshot se ve smyčce nespouští znovu");
        assert!(PREPARE.contains("command = /usr/local/bin/systemctl --boot"));
        assert!(!SYSTEMCTL.contains('\r') && !PREPARE.contains('\r'), "bez CRLF");
    }
}
