#!/bin/sh
# Spouští se UVNITR WSLCD-Lite (jako root) ze scripts/linux-test.ps1.
# Použití: linux-test.sh <check|test|live|package> <cesta k repu> [argumenty cargo... | verze]
set -eu

mode="${1:-}"
repo="${2:-}"
[ -n "$mode" ] && [ -n "$repo" ] || { echo "použití: $0 <check|test|live|package> <repo> [cargo args | verze]" >&2; exit 2; }
shift 2

# live míří na skutečný engine WSLCD-Lite – bez filtru testů se odmítne.
if [ "$mode" = live ] && [ -z "${1:-}" ]; then
    echo "live vyžaduje filtr testů, např.: linux-test.ps1 live -- host::local" >&2
    exit 2
fi

here=$(cd "$(dirname "$0")" && pwd)
image=wslcd-rust-dev
# Cílový adresář cargo je pro každý worktree zvlášť (souběžné běhy); registr je sdílený.
target_vol="wslcd-rust-target-$(basename "$repo" | tr A-Z a-z | sed "s/[^a-z0-9-]/-/g")"

# Obraz se sestaví jen jednou (WSLCD_REBUILD=1 vynutí nové sestavení).
if [ "${WSLCD_REBUILD:-0}" = 1 ] || ! podman image exists "$image"; then
    podman build -t "$image" -f "$here/linux-dev.Containerfile" "$here"
fi

run() {
    podman run --rm \
        -v "$repo:/src:ro" \
        -v "$target_vol:/target" \
        -v wslcd-cargo-registry:/usr/local/cargo/registry \
        -e CARGO_TARGET_DIR=/target \
        -e "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-4}" \
        -w /src \
        "$@"
}

case "$mode" in
package)
    # Repo zůstává jen pro čtení; do repa se zapisuje jen složka dist/ (ignorovaná gitem).
    [ -n "${1:-}" ] || { echo "package vyžaduje verzi, např.: linux-test.ps1 package 0.9.0-dev" >&2; exit 2; }
    mkdir -p "$repo/dist"
    run -v "$repo/dist:/dist" -e WSLCD_DIST=/dist "$image" sh scripts/package-linux.sh "$1"
    ;;
check)
    run "$image" cargo check --locked --bins --tests "$@"
    ;;
test)
    run "$image" cargo test --locked "$@"
    ;;
live)
    sockdir=/run/wslcd-dev
    sock=$sockdir/podman.sock
    svc=
    cleanup() {
        [ -z "$svc" ] || { kill "$svc" 2>/dev/null || true; wait "$svc" 2>/dev/null || true; }
        rm -f "$sock"
    }
    trap cleanup EXIT
    trap "exit 130" INT TERM HUP
    mkdir -p "$sockdir"
    # zapomenutá služba z dřívějšího běhu (jen ta, co obsluhuje náš socket)
    for p in $(pgrep -f "podman system service.*$sock" || true); do kill "$p" 2>/dev/null || true; done
    sleep 0.3
    rm -f "$sock"
    podman system service --time=0 "unix://$sock" &
    svc=$!
    i=0
    while [ ! -S "$sock" ]; do
        i=$((i + 1))
        [ "$i" -le 50 ] || { echo "podman service se nespustil" >&2; exit 1; }
        sleep 0.2
    done
    echo "podman service běží (pid $svc)"
    run -v "$sock:/run/podman/podman.sock" \
        -e DOCKER_HOST=unix:///run/podman/podman.sock \
        "$image" cargo test --locked "$@" -- --ignored
    ;;
*)
    echo "neznámý režim: $mode" >&2
    exit 2
    ;;
esac
