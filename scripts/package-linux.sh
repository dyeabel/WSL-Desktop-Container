#!/bin/sh
# Balíček pro Linux: dist/WSL-Container-Desktop-<verze>-linux-x64.tar.gz (+ .sha256).
# Použití: package-linux.sh <verze>
# Běží v kontejneru wslcd-rust-dev přes `scripts/linux-test.ps1 package <verze>`, nebo v CI.
#   WSLCD_DIST  výstupní složka (výchozí <repo>/dist; v kontejneru je repo jen pro čtení)
# Cílový adresář cargo řídí CARGO_TARGET_DIR (kontejner ho má mimo repo).
set -eu

ver="${1:-}"
[ -n "$ver" ] || { echo "použití: $0 <verze>" >&2; exit 2; }
case "$ver" in
    *[!A-Za-z0-9._+-]*) echo "neplatná verze: $ver" >&2; exit 2 ;;
esac

root=$(cd "$(dirname "$0")/.." && pwd)
dist="${WSLCD_DIST:-$root/dist}"
name="WSL-Container-Desktop-$ver-linux-x64"
bin=wsl-container-desktop

cd "$root"
cargo build --release --locked --bin "$bin"
exe="${CARGO_TARGET_DIR:-$root/target}/release/$bin"
[ -x "$exe" ] || { echo "chybí $exe" >&2; exit 1; }

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
pkg="$stage/$name"
mkdir -p "$pkg" "$dist"

install -m 755 "$exe" "$pkg/$bin"
install -m 644 assets/linux/wsl-container-desktop.desktop "$pkg/wsl-container-desktop.desktop"
install -m 644 assets/icon.png "$pkg/wsl-container-desktop.png"

cat > "$pkg/README-linux.txt" <<TXT
WSL Container Desktop $ver (Linux, x86_64)

Ovládací panel pro Docker a Podman: kontejnery, obrazy, svazky, sítě a compose stacky.
Na Linuxu aplikace nic neinstaluje ani nenastavuje; jen se připojí k existujícímu enginu.

Požadavky
  - GTK 3 a knihovny pro oznamovací oblast (Debian/Ubuntu: libgtk-3-0, libxdo3,
    libayatana-appindicator3-1); volitelně notify-send (libnotify-bin) pro oznámení.
  - Přístup k socketu enginu:
      Docker          /var/run/docker.sock – uživatel musí být ve skupině "docker"
                      (sudo usermod -aG docker \$USER, pak se znovu přihlásit)
      Rootless Podman \$XDG_RUNTIME_DIR/podman/podman.sock
                      (systemctl --user enable --now podman.socket)

Spuštění
  ./wsl-container-desktop

Volitelně do nabídky aplikací (jen pro aktuálního uživatele):
  install -Dm755 wsl-container-desktop ~/.local/bin/wsl-container-desktop
  install -Dm644 wsl-container-desktop.png ~/.local/share/icons/hicolor/512x512/apps/wsl-container-desktop.png
  install -Dm644 wsl-container-desktop.desktop ~/.local/share/applications/wsl-container-desktop.desktop

Funkce pro WSL (distribuce, instalace enginu, paměť VM, disky, docker shim) jsou jen ve Windows.
Konfigurace: \$XDG_CONFIG_HOME/wsl-container-desktop (výchozí ~/.config/...),
data: \$XDG_DATA_HOME/wsl-container-desktop (výchozí ~/.local/share/...).

English: GTK 3 and a reachable Docker socket (user in the "docker" group) or rootless Podman
socket are required. The app never installs or configures anything on Linux.
TXT

archive="$dist/$name.tar.gz"
tar -C "$stage" --sort=name --owner=0 --group=0 --numeric-owner -czf "$archive" "$name"
(cd "$dist" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "hotovo: $archive"
cat "$archive.sha256"
