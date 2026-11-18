//! Kontejnerový engine v distribuci: Docker nebo Podman.
//!
//! Aplikace v obou případech mluví Docker-kompatibilním REST API na
//! `tcp://127.0.0.1:<port>`. Liší se jen tím, jak se API zpřístupní (systemd),
//! jak se engine spouští/zastavuje a jakým příkazem se volá CLI.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    #[default]
    Docker,
    Podman,
}

/// systemd jednotka Podman API vytvořená aplikací
pub const PODMAN_UNIT: &str = "wls-podman-api";
pub const PODMAN_UNIT_PATH: &str = "/etc/systemd/system/wls-podman-api.service";
pub const DOCKER_OVERRIDE_PATH: &str = "/etc/systemd/system/docker.service.d/10-wls-desktop-tcp.conf";

impl Engine {
    pub const ALL: [Engine; 2] = [Engine::Docker, Engine::Podman];

    pub fn name(self) -> &'static str {
        match self {
            Engine::Docker => "Docker",
            Engine::Podman => "Podman",
        }
    }

    /// systemd služby, které engine (a jeho API) obsluhují
    pub fn services(self) -> &'static [&'static str] {
        match self {
            Engine::Docker => &["docker"],
            Engine::Podman => &[PODMAN_UNIT],
        }
    }

    /// služby, které je potřeba zastavit při úplném vypnutí
    pub fn stop_units(self) -> &'static [&'static str] {
        match self {
            Engine::Docker => &["docker.socket", "docker"],
            Engine::Podman => &[PODMAN_UNIT],
        }
    }

    /// soubor, jehož existence znamená, že je TCP API nastavené
    pub fn api_marker(self) -> &'static str {
        match self {
            Engine::Docker => DOCKER_OVERRIDE_PATH,
            Engine::Podman => PODMAN_UNIT_PATH,
        }
    }

    /// Příkaz CLI ve WSL. Podman jde přes `--url` na stejné API jako aplikace,
    /// takže CLI i aplikace vidí tytéž kontejnery (rootful úložiště).
    pub fn cli(self, port: u16) -> Vec<String> {
        match self {
            Engine::Docker => vec!["docker".into()],
            Engine::Podman => vec!["podman".into(), "--url".into(), format!("tcp://127.0.0.1:{port}")],
        }
    }

    /// Skript (běží jako root, proměnná PORT), který zpřístupní API na 127.0.0.1:$PORT.
    pub fn setup_script(self) -> &'static str {
        match self {
            Engine::Docker => DOCKER_SETUP,
            Engine::Podman => PODMAN_SETUP,
        }
    }

    /// Instalace přes apt / dnf / zypper / pacman.
    pub fn install_script(self) -> String {
        let body = match self {
            Engine::Docker => DOCKER_INSTALL_BODY,
            Engine::Podman => PODMAN_INSTALL_BODY,
        };
        format!("{PKG}{body}")
    }

    /// Odinstalace (běží jako root; PURGE=1 smaže i data).
    /// Funguje pro apt / dnf / zypper / pacman.
    pub fn uninstall_script(self) -> String {
        let body = match self {
            Engine::Docker => DOCKER_UNINSTALL,
            Engine::Podman => PODMAN_UNINSTALL,
        };
        format!("{PKG}{body}")
    }

    /// Kde engine drží data (pro text v dialogu).
    pub fn data_dirs(self) -> &'static str {
        match self {
            Engine::Docker => "/var/lib/docker, /var/lib/containerd, /etc/docker",
            Engine::Podman => "/var/lib/containers",
        }
    }
}

/// Odinstalace Dockeru – balíčky všech podporovaných variant (docker-ce z get.docker.com,
/// docker.io z Debianu/Ubuntu, moby-engine z Fedory, docker z openSUSE/Arch).
const DOCKER_UNINSTALL: &str = r#"echo "Zastavuji Docker…"
systemctl disable --now docker.socket docker containerd >/dev/null 2>&1 || true
rm -f /etc/systemd/system/docker.service.d/10-wls-desktop-tcp.conf
rmdir /etc/systemd/system/docker.service.d 2>/dev/null || true
systemctl daemon-reload 2>/dev/null || true
PKGS=$(pkg_installed docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin \
  docker-ce-rootless-extras docker.io docker-compose moby-engine moby-cli moby-buildx moby-compose \
  docker docker-buildx docker-compose-switch)
if [ -n "$PKGS" ]; then echo "Odebírám balíčky:$PKGS"; pkg_remove $PKGS; else echo "Žádné balíčky Dockeru nenalezeny"; fi
if [ "$PURGE" = "1" ]; then echo "Mažu data Dockeru…"; rm -rf /var/lib/docker /var/lib/containerd /etc/docker; fi
getent group docker >/dev/null && [ "$PURGE" = "1" ] && groupdel docker 2>/dev/null || true
echo "Docker odinstalován"
"#;

const PODMAN_UNINSTALL: &str = r#"echo "Zastavuji Podman API…"
systemctl disable --now wls-podman-api podman-restart >/dev/null 2>&1 || true
rm -f /etc/systemd/system/wls-podman-api.service
systemctl daemon-reload 2>/dev/null || true
if [ "$PURGE" = "1" ]; then podman system reset -f >/dev/null 2>&1 || true; fi
PKGS=$(pkg_installed podman podman-docker)
# docker-compose (provider pro podman compose) odebrat jen když není nainstalovaný Docker
if ! command -v dockerd >/dev/null; then PKGS="$PKGS$(pkg_installed docker-compose)"; fi
if [ -n "$PKGS" ]; then echo "Odebírám balíčky:$PKGS"; pkg_remove $PKGS; else echo "Balíček podman nenalezen"; fi
if [ "$PURGE" = "1" ]; then echo "Mažu data Podmanu…"; rm -rf /var/lib/containers; fi
echo "Podman odinstalován"
"#;

const DOCKER_SETUP: &str = concat!(
    "set -e\n",
    r#"if [ ! -d /run/systemd/system ]; then echo "Distribuce nebezi se systemd. Pridej do /etc/wsl.conf: [boot] systemd=true a restartuj WSL (wsl --terminate <distro>)." >&2; exit 3; fi
D=$(command -v dockerd || true)
if [ -z "$D" ]; then echo "dockerd nenalezen - pouzij Nainstalovat engine (nebo: curl -fsSL https://get.docker.com | sh)." >&2; exit 2; fi
# Podman API by blokovalo port
systemctl disable --now wls-podman-api >/dev/null 2>&1 || true
EXTRA=""
if systemctl cat containerd.service >/dev/null 2>&1; then EXTRA="--containerd=/run/containerd/containerd.sock"; fi
mkdir -p /etc/systemd/system/docker.service.d
printf '[Service]\nExecStart=\nExecStart=%s -H fd:// -H tcp://127.0.0.1:%s %s\n' "$D" "$PORT" "$EXTRA" > /etc/systemd/system/docker.service.d/10-wls-desktop-tcp.conf
if [ -f /etc/docker/daemon.json ] && grep -q '"hosts"' /etc/docker/daemon.json; then echo "VAROVANI: /etc/docker/daemon.json obsahuje 'hosts' - koliduje s -H, odstran ho." >&2; fi
systemctl daemon-reload
systemctl enable docker >/dev/null 2>&1 || true
systemctl restart docker
echo "Docker API nastaveno na tcp://127.0.0.1:$PORT"
"#
);

const PODMAN_SETUP: &str = concat!(
    "set -e\n",
    r#"if [ ! -d /run/systemd/system ]; then echo "Distribuce nebezi se systemd. Pridej do /etc/wsl.conf: [boot] systemd=true a restartuj WSL (wsl --terminate <distro>)." >&2; exit 3; fi
P=$(command -v podman || true)
if [ -z "$P" ]; then echo "podman nenalezen - pouzij Nainstalovat engine (nebo: apt install podman)." >&2; exit 2; fi
# Docker by blokoval port
systemctl disable --now docker.socket docker >/dev/null 2>&1 || true
cat > /etc/systemd/system/wls-podman-api.service <<EOF
[Unit]
Description=Podman API pro WLS Desktop (tcp://127.0.0.1:$PORT)
After=network-online.target

[Service]
ExecStart=$P system service --time=0 tcp://127.0.0.1:$PORT
Restart=on-failure

[Install]
WantedBy=multi-user.target
EOF
# kontejnery s restart policy po startu (podman nemá stále běžícího démona)
systemctl enable podman-restart.service >/dev/null 2>&1 || true
systemctl daemon-reload
systemctl enable wls-podman-api >/dev/null 2>&1 || true
systemctl restart wls-podman-api
echo "Podman API nastaveno na tcp://127.0.0.1:$PORT"
"#
);

/// Společné shell funkce pro apt / dnf / zypper / pacman:
/// `pkg_install balíček…`, `pkg_remove balíček…` a `pkg_installed balíček…`
/// (vypíše ty z uvedených, které jsou nainstalované, oddělené mezerou).
const PKG: &str = r#"pkg_install() {
  if command -v apt-get >/dev/null; then export DEBIAN_FRONTEND=noninteractive; apt-get update -qq && apt-get install -y -qq "$@";
  elif command -v dnf >/dev/null; then dnf install -y -q "$@";
  elif command -v zypper >/dev/null; then zypper --non-interactive install -y "$@";
  elif command -v pacman >/dev/null; then pacman -Sy --noconfirm --needed "$@";
  else echo "Nepodporovaný správce balíčků (umím apt, dnf, zypper, pacman)." >&2; return 2; fi
}
pkg_remove() {
  if command -v apt-get >/dev/null; then export DEBIAN_FRONTEND=noninteractive; apt-get purge -y -qq "$@" && apt-get autoremove -y -qq;
  elif command -v dnf >/dev/null; then dnf remove -y -q "$@";
  elif command -v zypper >/dev/null; then zypper --non-interactive remove "$@";
  elif command -v pacman >/dev/null; then pacman -Rns --noconfirm "$@";
  else echo "Nepodporovaný správce balíčků (umím apt, dnf, zypper, pacman)." >&2; return 2; fi
}
pkg_installed() {
  for p in "$@"; do
    if command -v dpkg-query >/dev/null; then
      dpkg-query -W -f='${Status}' "$p" 2>/dev/null | grep -q 'install ok installed' && printf ' %s' "$p"
    elif command -v rpm >/dev/null; then
      rpm -q "$p" >/dev/null 2>&1 && printf ' %s' "$p"
    elif command -v pacman >/dev/null; then
      pacman -Q "$p" >/dev/null 2>&1 && printf ' %s' "$p"
    fi
  done
  return 0
}
"#;

const DOCKER_INSTALL_BODY: &str = r#"set -e
if command -v pacman >/dev/null; then
  pkg_install docker docker-compose docker-buildx
elif command -v zypper >/dev/null; then
  pkg_install docker docker-compose
else
  # apt / dnf: oficiální instalační skript Dockeru (Debian, Ubuntu, Fedora, CentOS/RHEL…)
  pkg_install curl ca-certificates
  curl -fsSL https://get.docker.com | sh
fi
U=$(getent passwd 1000 | cut -d: -f1)
[ -n "$U" ] && usermod -aG docker "$U" || true
docker --version
"#;

const PODMAN_INSTALL_BODY: &str = r#"set -e
pkg_install podman
# compose provider pro `podman compose` (mluví se stejným API přes DOCKER_HOST z podman --url)
pkg_install docker-compose || echo "docker-compose se nepodařilo nainstalovat – podman compose nebude fungovat" >&2
podman --version
docker-compose version 2>/dev/null || true
"#;

/// Příprava nové distribuce (běží jako root, proměnná U = uživatel):
/// systemd, výchozí uživatel se sudo bez hesla.
pub const DISTRO_PREPARE: &str = r#"set -e
pkg_install sudo >/dev/null 2>&1 || true
# systemd + výchozí uživatel v /etc/wsl.conf
touch /etc/wsl.conf
if ! grep -q '^systemd=true' /etc/wsl.conf; then printf '\n[boot]\nsystemd=true\n' >> /etc/wsl.conf; fi
if [ -n "$U" ]; then
  if ! id "$U" >/dev/null 2>&1; then
    G=""
    getent group sudo >/dev/null && G="sudo"
    getent group wheel >/dev/null && G="${G:+$G,}wheel"
    if [ -n "$G" ]; then useradd -m -s /bin/bash -G "$G" "$U"; else useradd -m -s /bin/bash "$U"; fi
    echo "Uživatel $U vytvořen"
  fi
  mkdir -p /etc/sudoers.d
  echo "$U ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/wls-$U
  chmod 440 /etc/sudoers.d/wls-$U
  if ! grep -q '^\[user\]' /etc/wsl.conf; then printf '\n[user]\ndefault=%s\n' "$U" >> /etc/wsl.conf; fi
fi
cat /etc/wsl.conf
"#;

/// Doplní krátký název image na plně kvalifikovaný (Podman krátká jména
/// bez nastaveného registru odmítá; Docker plná jména zvládá taky).
/// `nginx` -> `docker.io/library/nginx`, `bitnami/redis:7` -> `docker.io/bitnami/redis:7`.
pub fn qualify_image(r: &str) -> String {
    let r = r.trim();
    if r.is_empty() {
        return String::new();
    }
    let first = r.split('/').next().unwrap_or("");
    let has_registry = r.contains('/') && (first.contains('.') || first.contains(':') || first == "localhost");
    if has_registry {
        r.to_string()
    } else if r.contains('/') {
        format!("docker.io/{r}")
    } else {
        format!("docker.io/library/{r}")
    }
}

/// Opačný směr pro zobrazení / Docker Hub API: `docker.io/library/nginx` -> `nginx`.
pub fn short_image(r: &str) -> String {
    let r = r.strip_prefix("docker.io/").unwrap_or(r);
    r.strip_prefix("library/").unwrap_or(r).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qualify() {
        assert_eq!(qualify_image("nginx"), "docker.io/library/nginx");
        assert_eq!(qualify_image("nginx:1.27-alpine"), "docker.io/library/nginx:1.27-alpine");
        assert_eq!(qualify_image("bitnami/redis:7"), "docker.io/bitnami/redis:7");
        assert_eq!(qualify_image("ghcr.io/org/app:1"), "ghcr.io/org/app:1");
        assert_eq!(qualify_image("localhost:5000/x"), "localhost:5000/x");
        assert_eq!(qualify_image("docker.io/library/alpine"), "docker.io/library/alpine");
    }

    #[test]
    fn short() {
        assert_eq!(short_image("docker.io/library/nginx"), "nginx");
        assert_eq!(short_image("docker.io/bitnami/redis"), "bitnami/redis");
        assert_eq!(short_image("quay.io/x/y"), "quay.io/x/y");
    }

    #[test]
    fn cli() {
        assert_eq!(Engine::Docker.cli(2375), vec!["docker"]);
        assert_eq!(Engine::Podman.cli(2375), vec!["podman", "--url", "tcp://127.0.0.1:2375"]);
    }
}

/// Shell funkce pkg_install / pkg_remove / pkg_installed (pro testy a vlastní skripty).
#[allow(dead_code)]
pub fn pkg_helpers() -> &'static str {
    PKG
}

/// Skript přípravy nové distribuce včetně funkce pkg_install.
pub fn distro_prepare_script() -> String {
    format!("{PKG}{DISTRO_PREPARE}")
}
