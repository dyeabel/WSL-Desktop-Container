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
/// unix socket Podman API v režimu TLS (skupina `wls-api`)
pub const PODMAN_TLS_SOCK: &str = "/run/wls-podman/podman.sock";
/// certifikáty v distribuci (CA, server, klient)
pub const TLS_DIR: &str = "/etc/wls-desktop/tls";
pub const DOCKER_OVERRIDE_PATH: &str = "/etc/systemd/system/docker.service.d/10-wls-desktop-tcp.conf";

/// Uvolní TCP port API v distribuci, která přestává být primární: vypne TLS proxy,
/// Podman API na TCP a `-H tcp://` u dockerd. Engine i kontejnery běží dál (unix socket);
/// po návratu distribuce na místo primární vše znovu nastaví `setup_script`.
pub const API_RELEASE: &str = r#"if [ ! -d /run/systemd/system ]; then exit 0; fi
if [ -f /etc/systemd/system/wls-tls-proxy.service ]; then
  systemctl disable --now wls-tls-proxy >/dev/null 2>&1 || true
  systemctl reset-failed wls-tls-proxy >/dev/null 2>&1 || true
  echo "TLS proxy vypnuta"
fi
F=/etc/systemd/system/wls-podman-api.service
if [ -f $F ] && grep -q 'tcp://' $F; then
  systemctl disable --now wls-podman-api >/dev/null 2>&1 || true
  echo "Podman API na TCP vypnuto"
fi
O=/etc/systemd/system/docker.service.d/10-wls-desktop-tcp.conf
if [ -f $O ] && grep -q -- '-H tcp://' $O; then
  sed -i 's# -H tcp://[^ ]*##' $O
  systemctl daemon-reload
  if systemctl is-active -q docker; then systemctl restart docker; fi
  echo "Docker API na TCP vypnuto"
fi
exit 0
"#;

/// Vypíše nainstalované enginy (`docker`, `podman`), každý na řádku.
pub const DETECT_ENGINES: &str = "for b in dockerd podman; do command -v $b >/dev/null 2>&1 && echo ${b%d}; done; true";

/// Výstup `DETECT_ENGINES` -> seznam enginů.
pub fn parse_engines(out: &str) -> Vec<Engine> {
    let mut v = vec![];
    for l in out.lines().map(str::trim) {
        let e = match l {
            "docker" => Engine::Docker,
            "podman" => Engine::Podman,
            _ => continue,
        };
        if !v.contains(&e) {
            v.push(e);
        }
    }
    v
}

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
    /// V režimu TLS mluví CLI s enginem přes unix socket (TCP port je jen za TLS proxy
    /// a vyžaduje klientský certifikát); přístup k socketu má skupina uživatele.
    pub fn cli(self, port: u16, tls: bool) -> Vec<String> {
        match (self, tls) {
            (Engine::Docker, _) => vec!["docker".into()],
            (Engine::Podman, false) => vec!["podman".into(), "--url".into(), format!("tcp://127.0.0.1:{port}")],
            (Engine::Podman, true) => vec!["podman".into(), "--url".into(), format!("unix://{PODMAN_TLS_SOCK}")],
        }
    }

    /// Skript (běží jako root), který zpřístupní API na 127.0.0.1:$PORT.
    /// Proměnné: PORT, TLS=1 (API jen přes TLS proxy s klientským certifikátem),
    /// U = výchozí uživatel distribuce (dostane přístup k unix socketu enginu).
    pub fn setup_script(self) -> String {
        format!(
            "{PKG}\n{TLS_FUNCS}\n{}",
            match self {
                Engine::Docker => DOCKER_SETUP,
                Engine::Podman => PODMAN_SETUP,
            }
        )
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
systemctl disable --now wls-tls-proxy >/dev/null 2>&1 || true
rm -f /etc/systemd/system/wls-tls-proxy.service
systemctl disable --now wls-dns >/dev/null 2>&1 || true
rm -f /etc/systemd/system/wls-dns.service
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
systemctl disable --now wls-tls-proxy >/dev/null 2>&1 || true
rm -f /etc/systemd/system/wls-tls-proxy.service
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
# odkaz /run/docker.sock na Podman (z jeho nastavení) by překážel docker.socket
rm -f /etc/tmpfiles.d/wls-docker-sock.conf
if [ -L /run/docker.sock ]; then rm -f /run/docker.sock; fi
EXTRA=""
if systemctl cat containerd.service >/dev/null 2>&1; then EXTRA="--containerd=/run/containerd/containerd.sock"; fi
# host.docker.internal jako v Docker Desktopu (Podman ho má sám): DNS pro kontejnery na
# 172.17.0.1 (dnsmasq) – odpoví host.docker.internal a ostatní dotazy předá DNS distribuce
if [ -f /etc/docker/daemon.json ] && grep -qE '"(dns|bip)"' /etc/docker/daemon.json; then
  echo "VAROVANI: daemon.json nastavuje dns nebo bip - host.docker.internal se nenastavi." >&2
  systemctl disable --now wls-dns >/dev/null 2>&1 || true
else
  M=$(command -v dnsmasq || true)
  if [ -z "$M" ]; then
    if command -v apt-get >/dev/null; then pkg_install dnsmasq-base >/dev/null 2>&1 || true; else pkg_install dnsmasq >/dev/null 2>&1 || true; fi
    M=$(command -v dnsmasq || true)
  fi
  if [ -n "$M" ]; then
    cat > /etc/systemd/system/wls-dns.service <<EOF
[Unit]
Description=DNS pro kontejnery Dockeru (host.docker.internal, WLS Desktop)
Before=docker.service

[Service]
ExecStart=$M --keep-in-foreground --conf-file=/dev/null --bind-dynamic --listen-address=172.17.0.1 --except-interface=lo --no-hosts --resolv-file=/etc/resolv.conf --address=/host.docker.internal/172.17.0.1 --address=/gateway.docker.internal/172.17.0.1 --cache-size=1000 --user=nobody --pid-file=
Restart=always
RestartSec=1

[Install]
WantedBy=docker.service
EOF
    systemctl daemon-reload
    systemctl enable wls-dns >/dev/null 2>&1 || true
    systemctl restart wls-dns || echo "VAROVANI: wls-dns (DNS pro kontejnery) nenabehla - viz journalctl -u wls-dns" >&2
    EXTRA="$EXTRA --dns 172.17.0.1"
    echo "host.docker.internal -> 172.17.0.1 (DNS pro kontejnery)"
  else
    echo "VAROVANI: dnsmasq se nepodarilo nainstalovat - host.docker.internal nebude fungovat." >&2
  fi
fi
mkdir -p /etc/systemd/system/docker.service.d
if [ "$TLS" = "1" ]; then
  # API jen na unix socketu, TCP obslouží TLS proxy
  printf '[Service]\nExecStart=\nExecStart=%s -H fd:// %s\n' "$D" "$EXTRA" > /etc/systemd/system/docker.service.d/10-wls-desktop-tcp.conf
  if [ -n "$U" ] && id "$U" >/dev/null 2>&1; then groupadd -f docker; usermod -aG docker "$U"; fi
else
  printf '[Service]\nExecStart=\nExecStart=%s -H fd:// -H tcp://127.0.0.1:%s %s\n' "$D" "$PORT" "$EXTRA" > /etc/systemd/system/docker.service.d/10-wls-desktop-tcp.conf
fi
if [ -f /etc/docker/daemon.json ] && grep -q '"hosts"' /etc/docker/daemon.json; then echo "VAROVANI: /etc/docker/daemon.json obsahuje 'hosts' - koliduje s -H, odstran ho." >&2; fi
systemctl daemon-reload
systemctl enable docker >/dev/null 2>&1 || true
systemctl restart docker
if [ "$TLS" = "1" ]; then
  tls_proxy /var/run/docker.sock docker.service
  echo "Docker API nastaveno na tcp://127.0.0.1:$PORT (TLS, vyzaduje klientsky certifikat)"
else
  no_tls_proxy
  echo "Docker API nastaveno na tcp://127.0.0.1:$PORT"
fi
"#
);

const PODMAN_SETUP: &str = concat!(
    "set -e\n",
    r#"if [ ! -d /run/systemd/system ]; then echo "Distribuce nebezi se systemd. Pridej do /etc/wsl.conf: [boot] systemd=true a restartuj WSL (wsl --terminate <distro>)." >&2; exit 3; fi
P=$(command -v podman || true)
if [ -z "$P" ]; then echo "podman nenalezen - pouzij Nainstalovat engine (nebo: apt install podman)." >&2; exit 2; fi
# Docker by blokoval port
systemctl disable --now docker.socket docker >/dev/null 2>&1 || true
if [ "$TLS" = "1" ]; then
  # API jen na unix socketu (skupina wls-api), TCP obslouží TLS proxy
  groupadd -f wls-api
  if [ -n "$U" ] && id "$U" >/dev/null 2>&1; then usermod -aG wls-api "$U"; fi
  SOCK=/run/wls-podman/podman.sock
  LISTEN="unix://$SOCK"
  PRE="ExecStartPre=/bin/sh -c 'mkdir -p /run/wls-podman && chgrp wls-api /run/wls-podman && chmod 750 /run/wls-podman && rm -f $SOCK'"
  POST="ExecStartPost=/bin/sh -c 'for i in \$\$(seq 100); do [ -S $SOCK ] && break; sleep 0.1; done; chgrp wls-api $SOCK; chmod 660 $SOCK'"
else
  LISTEN="tcp://127.0.0.1:$PORT"
  PRE=""
  POST=""
fi
cat > /etc/systemd/system/wls-podman-api.service <<EOF
[Unit]
Description=Podman API pro WLS Desktop ($LISTEN)
After=network-online.target

[Service]
$PRE
ExecStart=$P system service --time=0 $LISTEN
$POST
Restart=on-failure

[Install]
WantedBy=multi-user.target
EOF
# `podman compose` bez hlášky „Executing external compose provider…“ u každého příkazu
mkdir -p /etc/containers/containers.conf.d
printf '# WLS Desktop\n[engine]\ncompose_warning_logs = false\n' > /etc/containers/containers.conf.d/50-wls-desktop.conf
# krátká jména obrazů jako u Dockeru: hledat na Docker Hubu (`postgres:16-alpine`); aliasy
# z shortnames.conf, které míří jinam, přesměrovat na Docker Hub (`hello-world` = quay.io/podman/hello)
mkdir -p /etc/containers/registries.conf.d
cat > /etc/containers/registries.conf.d/zz-wls-desktop.conf <<'WLSREG'
# WLS Desktop: kratka jmena obrazu jako Docker (Docker Hub)
unqualified-search-registries = ["docker.io"]
short-name-mode = "permissive"

[aliases]
"hello-world" = "docker.io/library/hello-world"
"amazonlinux" = "docker.io/library/amazonlinux"
"centos" = "docker.io/library/centos"
"fedora" = "docker.io/library/fedora"
"rockylinux" = "docker.io/library/rockylinux"
"oraclelinux" = "docker.io/library/oraclelinux"
"opensuse/leap" = "docker.io/opensuse/leap"
"opensuse/tumbleweed" = "docker.io/opensuse/tumbleweed"
WLSREG
# /var/run/docker.sock -> API Podmanu (nástroje, `-v /var/run/docker.sock:…`, Docker-outside-of-Docker)
if [ "$TLS" = "1" ]; then
  DSOCK=/run/wls-podman/podman.sock
else
  systemctl enable --now podman.socket >/dev/null 2>&1 || true
  DSOCK=/run/podman/podman.sock
fi
printf '# WLS Desktop: Docker socket -> Podman API\nL+ /run/docker.sock - - - - %s\n' "$DSOCK" > /etc/tmpfiles.d/wls-docker-sock.conf
systemd-tmpfiles --create /etc/tmpfiles.d/wls-docker-sock.conf >/dev/null 2>&1 || ln -sfn "$DSOCK" /run/docker.sock
# kontejnery s restart policy po startu (podman nemá stále běžícího démona)
systemctl enable podman-restart.service >/dev/null 2>&1 || true
systemctl daemon-reload
systemctl enable wls-podman-api >/dev/null 2>&1 || true
systemctl restart wls-podman-api
if [ "$TLS" = "1" ]; then
  tls_proxy /run/wls-podman/podman.sock wls-podman-api.service
  echo "Podman API nastaveno na tcp://127.0.0.1:$PORT (TLS, vyzaduje klientsky certifikat)"
else
  no_tls_proxy
  echo "Podman API nastaveno na tcp://127.0.0.1:$PORT"
fi
"#
);

/// TLS proxy před unix socketem enginu: `tls_proxy <socket> <jednotka enginu>` vytvoří
/// (jen pokud chybí, nebo REGEN=1) CA, serverový a klientský certifikát v TLS_DIR a službu
/// `wls-tls-proxy` (socat), která na 127.0.0.1:$PORT přijme jen spojení s klientským
/// certifikátem od té CA. Proxy je svázaná s jednotkou enginu (startuje a zastavuje se s ní).
const TLS_FUNCS: &str = r#"tls_proxy() {
  command -v socat >/dev/null || pkg_install socat
  command -v openssl >/dev/null || pkg_install openssl
  # WLS_TLS_DIR / WLS_TLS_UNIT jen pro testy (jiná složka a služba)
  T=${WLS_TLS_DIR:-/etc/wls-desktop/tls}
  PU=${WLS_TLS_UNIT:-wls-tls-proxy}
  mkdir -p $T
  chmod 700 $T
  # certifikáty, které do 30 dnů vyprší, se vytvoří znovu
  for c in $T/server.pem $T/cert.pem; do
    if [ -s "$c" ] && ! openssl x509 -checkend 2592000 -noout -in "$c" >/dev/null 2>&1; then REGEN=1; fi
  done
  if [ ! -s $T/ca.pem ] || [ ! -s $T/server.pem ] || [ ! -s $T/cert.pem ] || [ "$REGEN" = "1" ]; then
    ( cd $T
      rm -f ./*.pem ./*.srl ./*.csr ./*.ext
      umask 077
      EC="-newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes"
      openssl req -x509 $EC -keyout ca-key.pem -out ca.pem -days 3650 -subj "/CN=WLS Desktop CA" \
        -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" 2>/dev/null
      openssl req $EC -keyout server-key.pem -out server.csr -subj "/CN=localhost" 2>/dev/null
      printf 'basicConstraints=CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1,DNS:localhost\n' > server.ext
      openssl x509 -req -in server.csr -CA ca.pem -CAkey ca-key.pem -CAcreateserial -out server.pem -days 825 -extfile server.ext 2>/dev/null
      openssl req $EC -keyout key.pem -out client.csr -subj "/CN=wls-desktop-client" 2>/dev/null
      printf 'basicConstraints=CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=clientAuth\n' > client.ext
      openssl x509 -req -in client.csr -CA ca.pem -CAkey ca-key.pem -CAcreateserial -out cert.pem -days 825 -extfile client.ext 2>/dev/null
      rm -f ./*.csr ./*.ext ./*.srl
      chmod 400 ca-key.pem )
    echo "Certifikaty vytvoreny v $T"
  fi
  S=$(command -v socat)
  cat > /etc/systemd/system/$PU.service <<EOF
[Unit]
Description=TLS proxy pro API enginu (WLS Desktop, tcp://127.0.0.1:$PORT)
BindsTo=$2
After=$2

[Service]
ExecStart=$S OPENSSL-LISTEN:$PORT,bind=127.0.0.1,reuseaddr,fork,backlog=128,max-children=256,cert=$T/server.pem,key=$T/server-key.pem,cafile=$T/ca.pem,verify=1 UNIX-CONNECT:$1
Restart=always
RestartSec=1

[Install]
WantedBy=$2
EOF
  systemctl daemon-reload
  systemctl enable $PU >/dev/null 2>&1 || true
  systemctl restart $PU
}
no_tls_proxy() {
  if [ -f /etc/systemd/system/wls-tls-proxy.service ]; then
    systemctl disable --now wls-tls-proxy >/dev/null 2>&1 || true
    rm -f /etc/systemd/system/wls-tls-proxy.service
    systemctl daemon-reload
  fi
}
"#;

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
elif command -v apt-get >/dev/null; then
  # balíčky distribuce (podepsané, ověřuje apt) – žádné `curl | sh` (bezpečnostní nález F-13)
  pkg_install docker.io
  pkg_install docker-compose-v2 2>/dev/null || pkg_install docker-compose || echo "docker compose se nepodařilo nainstalovat" >&2
  pkg_install docker-buildx 2>/dev/null || true
elif command -v dnf >/dev/null; then
  if dnf -q list --available moby-engine >/dev/null 2>&1 || rpm -q moby-engine >/dev/null 2>&1; then
    # Fedora: Moby z repozitářů distribuce
    pkg_install moby-engine docker-compose
  else
    # RHEL / Alma / Rocky / Oracle: oficiální repozitář Dockeru (balíčky ověřuje dnf přes GPG)
    dnf install -y -q dnf-plugins-core
    dnf config-manager --add-repo https://download.docker.com/linux/centos/docker-ce.repo 2>/dev/null \
      || dnf config-manager addrepo --from-repofile=https://download.docker.com/linux/centos/docker-ce.repo
    pkg_install docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
  fi
else
  echo "Nepodporovaný správce balíčků (umím apt, dnf, zypper, pacman)." >&2
  exit 2
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

/// Shell funkce pkg_install / pkg_remove / pkg_installed (pro testy a vlastní skripty).
#[allow(dead_code)]
pub fn pkg_helpers() -> &'static str {
    PKG
}

/// Skript přípravy nové distribuce včetně funkce pkg_install.
pub fn distro_prepare_script() -> String {
    format!("{PKG}{DISTRO_PREPARE}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_engines_from_detect_output() {
        assert_eq!(parse_engines("docker\npodman\n"), vec![Engine::Docker, Engine::Podman]);
        assert_eq!(parse_engines("podman\r\n"), vec![Engine::Podman]);
        assert!(parse_engines("").is_empty());
    }

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
        assert_eq!(Engine::Docker.cli(2375, false), vec!["docker"]);
        assert_eq!(Engine::Podman.cli(2375, false), vec!["podman", "--url", "tcp://127.0.0.1:2375"]);
        assert_eq!(Engine::Podman.cli(2376, true), vec!["podman", "--url", "unix:///run/wls-podman/podman.sock"]);
    }
}
