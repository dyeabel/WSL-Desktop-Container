#!/bin/sh
# Spouští se UVNITR WSLCD-Lite (jako root) ze scripts/linux-test.ps1.
# Použití: linux-test.sh <check|test|live> <cesta k repu> [argumenty cargo...]
set -eu

mode="${1:-}"
repo="${2:-}"
[ -n "$mode" ] && [ -n "$repo" ] || { echo "použití: $0 <check|test|live> <repo> [cargo args]" >&2; exit 2; }
shift 2

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
        -w /src \
        "$@"
}

case "$mode" in
check)
    run "$image" cargo check --locked --bins --tests "$@"
    ;;
test)
    run "$image" cargo test --locked "$@"
    ;;
live)
    sockdir=/run/wslcd-dev
    sock=$sockdir/podman.sock
    mkdir -p "$sockdir"
    rm -f "$sock"
    podman system service --time=0 "unix://$sock" &
    svc=$!
    trap 'kill "$svc" 2>/dev/null || true; wait "$svc" 2>/dev/null || true; rm -f "$sock"' EXIT INT TERM
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
