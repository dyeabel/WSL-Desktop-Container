#!/bin/sh
# WSL Container Desktop – minimální náhrada systemctl pro lehkou distribuci bez systemd.
# Umí jen to, co používá aplikace: jednotky /etc/systemd/system/*.service (+ drop-iny
# *.service.d/*.conf), ExecStartPre / ExecStart / ExecStartPost, Restart=, enable/disable.
# Procesy hlídá jednoduchý supervisor (restart po pádu), logy jsou v /var/log/wslcd/.
# Při startu distribuce ho spouští /etc/wsl.conf: [boot] command = systemctl --boot
set -u
# služby jako v systemd: pracovní adresář / a čisté prostředí – jinak by zdědily adresář
# a proměnné volajícího (wsl.exe z Windows: PWD=/mnt/c/…, WSLg), Podman pak zakládal
# volume relativně k tomu adresáři; [boot] command má navíc minimální PATH (docker-proxy)
if [ -z "${WSLCD_SYSTEMCTL_CLEAN:-}" ]; then
  cd / && exec env -i WSLCD_SYSTEMCTL_CLEAN=1 HOME=/root LANG=C.UTF-8 \
    PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin /bin/sh "$0" "$@"
fi
unset WSLCD_SYSTEMCTL_CLEAN
UNITS=/etc/systemd/system
STATE=/etc/wslcd-init/enabled
RUN=/run/wslcd-init
LOGS=/var/log/wslcd
mkdir -p "$STATE" "$RUN" "$LOGS"

unit_name() {
  case "$1" in *.service|*.socket|*.target) echo "$1" ;; *) echo "$1.service" ;; esac
}

# soubory jednotky v pořadí: hlavní, pak drop-iny podle jména
unit_files() {
  [ -f "$UNITS/$1" ] && echo "$UNITS/$1"
  [ -f "/usr/lib/systemd/system/$1" ] && echo "/usr/lib/systemd/system/$1"
  if [ -d "$UNITS/$1.d" ]; then ls "$UNITS/$1.d"/*.conf 2>/dev/null | sort; fi
  return 0
}

# poslední hodnota klíče (prázdná hodnota ruší předchozí, jako v systemd)
get_last() {
  v=""
  for f in $(unit_files "$1"); do
    while IFS= read -r line || [ -n "$line" ]; do
      case "$line" in "$2="*) v="${line#"$2="}" ;; esac
    done < "$f"
  done
  printf '%s' "$v"
}

# všechny hodnoty klíče, každá na řádek
get_all() {
  for f in $(unit_files "$1"); do
    while IFS= read -r line || [ -n "$line" ]; do
      case "$line" in "$2="?*) printf '%s\n' "${line#"$2="}" ;; esac
    done < "$f"
  done
}

# systemd: `$$` = `$`, úvodní `-` = ignorovat chybu
unescape() {
  printf '%s' "$1" | sed -e 's/\$\$/$/g' -e 's/^-//'
}

main_pid() {
  p=$(cat "$RUN/$1.pid" 2>/dev/null || true)
  if [ -n "$p" ] && kill -0 "$p" 2>/dev/null; then echo "$p"; else echo 0; fi
}

is_active() { [ "$(main_pid "$1")" != 0 ]; }

# Vestavěný podman-restart (Alpine jednotku nemá): po startu spustí kontejnery s restart
# policy always a unless-stopped. Ručně zastavený unless-stopped zůstane stát: po `podman stop`
# je FinishedAt po StartedAt, kdežto po uspání/pádu distribuce Podman FinishedAt nezapíše
# (zůstane nulový nebo starší než poslední start).
podman_restart() {
  command -v podman >/dev/null 2>&1 || return 0
  podman start --all --filter restart-policy=always
  for c in $(podman ps -a -q --filter restart-policy=unless-stopped --filter status=exited); do
    set -- $(podman inspect --format '{{.State.FinishedAt.Unix}} {{.State.StartedAt.Unix}}' "$c")
    [ "$#" = 2 ] && [ "$1" -lt "$2" ] && podman start "$c"
  done
  return 0
}

start_unit() {
  u=$1
  is_active "$u" && return 0
  cmd=$(get_last "$u" ExecStart)
  if [ -z "$cmd" ] && [ "$u" = podman-restart.service ] && [ -z "$(unit_files "$u")" ]; then
    (podman_restart >> "$LOGS/podman-restart.log" 2>&1 &)
    return 0
  fi
  if [ -z "$cmd" ]; then
    # jednotky distribucí, které tu nejsou (docker.socket, containerd, podman-restart…)
    [ -z "$(unit_files "$u")" ] && return 0
    echo "systemctl: $u nemá ExecStart" >&2
    return 1
  fi
  cmd=$(unescape "$cmd")
  # oneshot (strop paměti): synchronně, bez supervisoru; $u.stop smazat tu (po restartu by zůstal)
  if [ "$(get_last "$u" Type)" = oneshot ]; then
    rm -f "$RUN/$u.stop"
    sh -c "$cmd" >> "$LOGS/${u%.service}.log" 2>&1 || return 1
    return 0
  fi
  # socket activation systemd tu není: dockerd -H fd:// → vlastní unix socket
  case "$cmd" in *"-H fd://"*) cmd=$(printf '%s' "$cmd" | sed 's#-H fd://#-H unix:///var/run/docker.sock#') ;; esac
  restart=$(get_last "$u" Restart)
  oom=$(get_last "$u" OOMScoreAdjust)
  oomp=""
  if [ -n "$oom" ]; then oomp="echo $oom > /proc/self/oom_score_adj 2>/dev/null; "; fi
  log="$LOGS/${u%.service}.log"
  [ -f "$log" ] && [ "$(wc -c < "$log")" -gt 1048576 ] && mv -f "$log" "$log.1"
  get_all "$u" ExecStartPre | while IFS= read -r pre; do sh -c "$(unescape "$pre")" >> "$log" 2>&1 || true; done
  rm -f "$RUN/$u.stop"
  # supervisor: drží PID hlavního procesu v $RUN/$u.pid a po pádu ho znovu spustí
  (
    trap '' HUP
    while :; do
      # vlastní relace jako v systemd: konec relace wsl.exe pošle SIGHUP a Podman 5.8 na něj
      # znovu načte konfiguraci a ztratí volume_path (svazky pak vznikají s relativní cestou)
      setsid sh -c "${oomp}exec $cmd" >> "$log" 2>&1 < /dev/null &
      echo $! > "$RUN/$u.pid"
      wait $!
      code=$?
      [ -e "$RUN/$u.stop" ] && break
      case "$restart" in always) ;; on-failure) [ "$code" = 0 ] && break ;; *) break ;; esac
      sleep 1
    done
    rm -f "$RUN/$u.pid" "$RUN/$u.sup"
  ) > /dev/null 2>&1 &
  echo $! > "$RUN/$u.sup"
  i=0
  while [ "$i" -lt 20 ] && [ ! -s "$RUN/$u.pid" ]; do sleep 0.05; i=$((i + 1)); done
  get_all "$u" ExecStartPost | while IFS= read -r post; do sh -c "$(unescape "$post")" >> "$log" 2>&1 || true; done
  return 0
}

stop_unit() {
  u=$1
  touch "$RUN/$u.stop"
  sup=$(cat "$RUN/$u.sup" 2>/dev/null || true)
  pid=$(main_pid "$u")
  [ "$pid" != 0 ] && kill "$pid" 2>/dev/null
  i=0
  while [ "$i" -lt 100 ] && [ "$(main_pid "$u")" != 0 ]; do sleep 0.1; i=$((i + 1)); done
  pid=$(main_pid "$u")
  [ "$pid" != 0 ] && kill -9 "$pid" 2>/dev/null
  [ -n "$sup" ] && kill "$sup" 2>/dev/null
  rm -f "$RUN/$u.pid" "$RUN/$u.sup"
  return 0
}

# odkazy z /etc/tmpfiles.d (jen „L+ cesta - - - - cíl“, jako /run/docker.sock -> API Podmanu)
tmpfiles() {
  for f in /etc/tmpfiles.d/*.conf; do
    [ -e "$f" ] || continue
    while read -r t p _ _ _ _ target; do
      case "$t" in L|L+) [ -n "$target" ] && mkdir -p "$(dirname "$p")" && ln -sfn "$target" "$p" ;; esac
    done < "$f"
  done
  return 0
}

boot() {
  tmpfiles
  # strop paměti pro kontejnery dřív než engine (oneshot)
  [ -e "$STATE/wslcd-guard.service" ] && start_unit wslcd-guard.service
  # DNS pro kontejnery před enginem, TLS proxy až po něm
  for f in "$STATE"/wslcd-dns.service "$STATE"/*; do
    [ -e "$f" ] || continue
    u=$(basename "$f")
    case "$u" in wslcd-tls-proxy.service|podman-restart.service|wslcd-guard.service) continue ;; esac
    start_unit "$u"
  done
  [ -e "$STATE/wslcd-tls-proxy.service" ] && start_unit wslcd-tls-proxy.service
  # kontejnery až po enginu
  [ -e "$STATE/podman-restart.service" ] && start_unit podman-restart.service
  return 0
}

[ "$#" -eq 0 ] && { echo "usage: systemctl COMMAND [UNIT…]" >&2; exit 1; }
cmd=$1; shift
now=0; quiet=0; value=0; prop=""
units=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --now) now=1 ;;
    -q|--quiet) quiet=1 ;;
    --value) value=1 ;;
    -p) shift; prop=$1 ;;
    --no-pager|--no-block|-l|--full) ;;
    -n) shift ;;
    -*) ;;
    *) units="$units $(unit_name "$1")" ;;
  esac
  shift
done

rc=0
case "$cmd" in
  --boot) boot ;;
  daemon-reload|reset-failed|mask|unmask) ;;
  start) for u in $units; do start_unit "$u" || rc=1; done ;;
  stop) for u in $units; do stop_unit "$u"; done ;;
  restart|try-restart|reload-or-restart) for u in $units; do stop_unit "$u"; start_unit "$u" || rc=1; done ;;
  enable) for u in $units; do { [ -n "$(unit_files "$u")" ] || [ "$u" = podman-restart.service ]; } && touch "$STATE/$u"; [ "$now" = 1 ] && { start_unit "$u" || rc=1; }; done ;;
  disable) for u in $units; do rm -f "$STATE/$u"; [ "$now" = 1 ] && stop_unit "$u"; done ;;
  is-active) for u in $units; do if is_active "$u"; then [ "$quiet" = 1 ] || echo active; else [ "$quiet" = 1 ] || echo inactive; rc=3; fi; done ;;
  is-enabled) for u in $units; do if [ -e "$STATE/$u" ]; then echo enabled; else echo disabled; rc=1; fi; done ;;
  cat) for u in $units; do f=$(unit_files "$u"); [ -z "$f" ] && { echo "No files found for $u." >&2; rc=1; continue; }; for x in $f; do echo "# $x"; cat "$x"; done; done ;;
  show)
    for u in $units; do
      case "$prop" in
        MainPID) v=$(main_pid "$u") ;;
        ActiveState) if is_active "$u"; then v=active; else v=inactive; fi ;;
        *) v="" ;;
      esac
      if [ "$value" = 1 ]; then echo "$v"; else echo "$prop=$v"; fi
    done ;;
  status)
    for u in $units; do
      if is_active "$u"; then echo "● $u - active (running) pid $(main_pid "$u")"; else echo "○ $u - inactive"; rc=3; fi
      tail -n 15 "$LOGS/${u%.service}.log" 2>/dev/null
    done ;;
  list-units) for f in "$RUN"/*.pid; do [ -e "$f" ] && basename "$f" .pid; done ;;
  *) echo "systemctl (wslcd): příkaz $cmd není podporovaný" >&2; rc=1 ;;
esac
exit $rc
