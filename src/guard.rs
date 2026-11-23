//! Společný strop paměti pro kontejnery (wslcd-guard): shell funkce pro setup enginu a výpočet argumentů.

use crate::config::Config;

#[cfg(test)]
pub const GUARD_SH: &str = include_str!("guard.sh");

/// Značka pro engine bez stropu paměti (použijí pozdější úkoly).
pub const UNGUARDED_MARK: &str = "wslcd-guard: engine bezi bez stropu pameti";

/// Shell funkce `guard_install <RESERVE_MB>`, `guard_remove`, `guard_apply`.
pub const FUNCS: &str = concat!(
    r#"GUARD_DIR=/usr/local/lib/wsl-container-desktop
guard_install() {
  mkdir -p "$GUARD_DIR"
  cat > "$GUARD_DIR/guard.sh" <<'WSLCDGUARD'
"#,
    include_str!("guard.sh"),
    r#"WSLCDGUARD
  chmod 755 "$GUARD_DIR/guard.sh"
  cat > /etc/systemd/system/wslcd-guard.service <<EOF
[Unit]
Description=Strop pameti pro kontejnery (WSL Container Desktop)
Before=docker.service wslcd-podman-api.service
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/sh $GUARD_DIR/guard.sh $1
[Install]
WantedBy=multi-user.target
EOF
  if [ -d /run/systemd/system ]; then
    printf '[Unit]\nDescription=Kontejnery pod stropem pameti (WSL Container Desktop)\nBefore=slices.target\n' > /etc/systemd/system/wslcd-containers.slice
  fi
  for u in docker containerd wslcd-podman-api wslcd-tls-proxy wslcd-dns; do
    case "$u" in
      docker|containerd)
        [ -e /etc/systemd/system/$u.service ] || [ -e /usr/lib/systemd/system/$u.service ] || [ -e /lib/systemd/system/$u.service ] || continue ;;
    esac
    mkdir -p /etc/systemd/system/$u.service.d
    printf '[Service]\nOOMScoreAdjust=-900\n' > /etc/systemd/system/$u.service.d/30-wsl-container-desktop-oom.conf
    pid=$(systemctl show -p MainPID --value $u 2>/dev/null)
    if [ -n "$pid" ] && [ "$pid" != 0 ] && [ -w /proc/$pid/oom_score_adj ]; then echo -900 > /proc/$pid/oom_score_adj 2>/dev/null; fi
  done
  systemctl daemon-reload
  systemctl enable wslcd-guard >/dev/null 2>&1 || true
  sh $GUARD_DIR/guard.sh "$1"
  return 0
}
guard_remove() {
  if [ ! -e "$GUARD_DIR/guard.sh" ] && [ ! -e /etc/systemd/system/wslcd-guard.service ]; then return 0; fi
  sh $GUARD_DIR/guard.sh 0 off
  systemctl disable wslcd-guard >/dev/null 2>&1
  rm -f /etc/systemd/system/wslcd-guard.service /etc/systemd/system/wslcd-containers.slice "$GUARD_DIR/guard.sh"
  for u in docker containerd wslcd-podman-api wslcd-tls-proxy wslcd-dns; do
    rm -f /etc/systemd/system/$u.service.d/30-wsl-container-desktop-oom.conf
    rmdir /etc/systemd/system/$u.service.d 2>/dev/null
  done
  rmdir "$GUARD_DIR" 2>/dev/null
  systemctl daemon-reload
  echo "Strop pameti pro kontejnery odstranen"
  return 0
}
guard_apply() {
  case "${GUARD:-}" in
    1) guard_install "${RESERVE_MB:-0}" ;;
    0) guard_remove ;;
  esac
  return 0
}
"#
);

/// Samostatný skript: funkce + kontrola systemd + `guard_apply`.
pub fn apply_script() -> String {
    format!(
        "{FUNCS}if [ ! -d /run/systemd/system ] && [ ! -d /run/wslcd-init ]; then echo \"Distribuce nebezi se systemd - strop pameti nelze nastavit.\" >&2; exit 3; fi\nguard_apply\n"
    )
}

/// Rezerva v GB: nekonečné/NaN → 0, jinak 0–64 po 0,5.
pub fn snap_reserve(gb: f64) -> f64 {
    if !gb.is_finite() {
        return 0.0;
    }
    (gb.clamp(0.0, 64.0) * 2.0).round() / 2.0
}

pub fn reserve_mb(gb: f64) -> u64 {
    (snap_reserve(gb) * 1024.0) as u64
}

/// Proměnné prostředí pro `guard_apply`; ochrana jen když je zapnutá a distribuce je VM.
pub fn env_args(cfg: &Config, vm_memory: bool) -> [String; 2] {
    let on = cfg.container_guard && vm_memory;
    [format!("GUARD={}", on as u8), format!("RESERVE_MB={}", reserve_mb(cfg.guard_reserve_gb))]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_is_snapped() {
        for (i, o) in [(-1.0, 0.0), (f64::NAN, 0.0), (0.5, 0.5), (1.3, 1.5), (2.2, 2.0), (2.25, 2.5), (100.0, 64.0)] {
            assert_eq!(snap_reserve(i), o, "{i}");
        }
        for (i, o) in [(0.0, 0), (2.0, 2048), (1.3, 1536), (64.0, 65536)] {
            assert_eq!(reserve_mb(i), o, "{i}");
        }
    }

    #[test]
    fn env_args_follow_config() {
        let mut c = Config::default();
        assert_eq!(env_args(&c, true), ["GUARD=1", "RESERVE_MB=0"]);
        assert_eq!(env_args(&c, false)[0], "GUARD=0");
        c.guard_reserve_gb = 2.0;
        assert_eq!(env_args(&c, true)[1], "RESERVE_MB=2048");
        c.container_guard = false;
        assert_eq!(env_args(&c, true)[0], "GUARD=0");
    }

    #[test]
    fn guard_script_shape() {
        let s = GUARD_SH;
        assert!(s.starts_with("#!/bin/sh") && s.ends_with('\n'));
        for bad in ["\r", "[[", "\nset -e\n", "WSLCDGUARD"] {
            assert!(!s.contains(bad), "{bad:?}");
        }
        for need in [
            "* 15 / 100", "1048576", "524288", "* 9 / 10", "wslcd-guard: max=$MAX high=$HIGH reserve=",
            "set-property --runtime", "wslcd-containers.slice machine.slice", "wslcd-containers libpod_parent",
            "cgroup.subtree_control", "VAROVANI",
        ] {
            assert!(s.contains(need), "{need}");
        }
    }

    #[test]
    fn guard_funcs_shape() {
        let f = FUNCS;
        for need in [
            GUARD_SH, "<<'WSLCDGUARD'", "guard_install() {", "guard_remove() {", "guard_apply() {", "Type=oneshot",
            "RemainAfterExit=yes", "Before=docker.service wslcd-podman-api.service", "WantedBy=multi-user.target",
            "ExecStart=/bin/sh $GUARD_DIR/guard.sh $1", "sh $GUARD_DIR/guard.sh \"$1\"", "OOMScoreAdjust=-900",
            "30-wsl-container-desktop-oom.conf", "docker containerd wslcd-podman-api wslcd-tls-proxy wslcd-dns",
        ] {
            assert!(f.contains(need), "{need}");
        }
        for bad in ["Environment=", "systemctl restart wslcd-guard", "\nset -e\n", "\r"] {
            assert!(!f.contains(bad), "{bad:?}");
        }
        assert!(apply_script().ends_with("guard_apply\n"));
    }
}
