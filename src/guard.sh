#!/bin/sh
# wslcd-guard (WSL Container Desktop): spolecny strop pameti pro kontejnery.
# guard.sh <RESERVE_MB> [off]   RESERVE_MB 0 = max(1 GiB, 15 % MemTotal)
CG=${WSLCD_GUARD_CG:-/sys/fs/cgroup}
reset() {
  if [ -d /run/systemd/system ]; then
    for s in wslcd-containers.slice machine.slice; do systemctl set-property --runtime "$s" MemoryMax=infinity MemoryHigh=infinity MemorySwapMax=infinity 2>/dev/null; done
  else
    for d in wslcd-containers libpod_parent; do [ -d $CG/$d ] && { echo max > $CG/$d/memory.max; echo max > $CG/$d/memory.high; [ -e $CG/$d/memory.swap.max ] && echo max > $CG/$d/memory.swap.max; } 2>/dev/null; done
  fi
}
if [ "${2:-}" = off ]; then reset; exit 0; fi
T=${WSLCD_GUARD_MEMTOTAL_KB:-$(awk '/^MemTotal:/{print $2}' /proc/meminfo)}
R_MB=${1:-0}
if [ "$R_MB" -gt 0 ] 2>/dev/null; then R=$((R_MB * 1024)); else R=$((T * 15 / 100)); [ "$R" -lt 1048576 ] && R=1048576; fi
if [ $((T - R)) -lt 524288 ]; then echo "VAROVANI: na kontejnery by zbylo mene nez 512 MiB - strop pameti se nenastavi" >&2; reset; exit 0; fi
MAX=$(((T - R) * 1024)); HIGH=$((MAX * 9 / 10))
# strop swapu kontejneru = polovina swapu distribuce; bez swapu se nenastavuje
S=${WSLCD_GUARD_SWAPTOTAL_KB:-$(awk '/^SwapTotal:/{print $2}' /proc/meminfo)}; SWAP=$((${S:-0} * 512)); SP=; [ "$SWAP" -gt 0 ] && SP="MemorySwapMax=$SWAP"
echo "wslcd-guard: max=$MAX high=$HIGH reserve=$((R * 1024))"
[ "${WSLCD_GUARD_DRY:-0}" = 1 ] && exit 0
if [ -d /run/systemd/system ]; then
  for s in wslcd-containers.slice machine.slice; do
    systemctl start "$s" 2>/dev/null
    systemctl set-property --runtime "$s" MemoryMax="$MAX" MemoryHigh="$HIGH" $SP || echo "VAROVANI: $s - strop pameti nelze nastavit" >&2
  done
elif ! grep -qw memory $CG/cgroup.controllers 2>/dev/null; then
  echo "VAROVANI: cgroup v2 s radicem pameti neni pripojena - strop pameti se nenastavi" >&2
else
  echo +memory > $CG/cgroup.subtree_control 2>/dev/null
  for d in wslcd-containers libpod_parent; do
    mkdir -p $CG/$d 2>/dev/null; echo +memory > $CG/$d/cgroup.subtree_control 2>/dev/null
    { echo "$MAX" > $CG/$d/memory.max && echo "$HIGH" > $CG/$d/memory.high; } 2>/dev/null || echo "VAROVANI: $CG/$d - strop pameti nelze zapsat" >&2
    if [ "$SWAP" -gt 0 ]; then { echo "$SWAP" > $CG/$d/memory.swap.max; } 2>/dev/null || echo "VAROVANI: $CG/$d - strop swapu nelze zapsat" >&2; fi
  done
fi
exit 0
