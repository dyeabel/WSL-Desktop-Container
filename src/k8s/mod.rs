//! Kubernetes laboratoř: lokální k3s v kontejneru enginu, prostředí, mapa toků a pravidla
//! (NetworkPolicy). Čistá logika je v podmodulech bez I/O (`model`, `flows`, `policy`,
//! `kubeconfig`, `template`); práci s enginem a WSL dělá `cluster` a `worker::k8s`.
//! Návrh: `docs/superpowers/specs/2026-11-21-kubernetes-lab-design.md`.

pub mod cluster;
pub mod flows;
pub mod kubeconfig;
#[cfg(test)]
mod live_k8s;
pub mod model;
pub mod policy;
pub mod template;

/// Label namespace, který aplikace vytvořila jako prostředí.
pub const LAB_LABEL: &str = "wslcd.lab";
/// Label politik, které spravuje aplikace.
pub const MANAGED_LABEL: &str = "wslcd.managed";
