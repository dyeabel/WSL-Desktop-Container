//! Sestavení image z Dockerfile přes CLI enginu ve WSL s průběžným výstupem.

use super::spec::{is_windows_path, BuildSpec};
use super::{Msg, Worker};
use crate::engine::Engine;
use crate::wsl;

/// Ukončí právě prováděné RUN kroky buildů Podmanu: potomky služby wls-podman-api
/// s názvem `exe` (izolace buildah) / `buildah-oci-runtime` – kontejnery mají rodiče conmon, ne službu.
const KILL_PODMAN_BUILD_STEPS: &str = r#"kt() { for c in $(pgrep -P "$1"); do kt "$c"; done; kill -KILL "$1" 2>/dev/null; }
M=$(systemctl show -p MainPID --value wls-podman-api 2>/dev/null)
if [ -n "$M" ] && [ "$M" != 0 ]; then
  for c in $(pgrep -P "$M"); do
    case "$(cat /proc/$c/comm 2>/dev/null)" in exe|buildah-oci-run*) kt "$c" ;; esac
  done
fi
true"#;

/// Požadavek na zrušení běžícího sestavení (nastaví UI, čte `run_stream`).
static CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Zruší běžící sestavení: ukončí CLI a uvnitř distribuce i proces `build`.
pub fn cancel_build() {
    CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
}

impl Worker {
    pub(super) fn build_image(&self, spec: BuildSpec) -> Result<String, String> {
        CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
        let cfg = self.cfg();
        let res = run_build(&cfg.distro, cfg.engine, cfg.port, cfg.tls, &spec, |line| self.send(Msg::BuildOutput(line.to_string())));
        self.send(Msg::BuildDone(res.clone()));
        res
    }
}

/// Sestaví image: převede cesty na linuxové, ověří kontext a Dockerfile a spustí
/// `<cli> build … .` v adresáři kontextu. Každý řádek výstupu předá `on_line`.
/// Vrací text pro oznámení (úspěch) nebo chybu.
pub fn run_build(distro: &str, engine: Engine, port: u16, tls: bool, spec: &BuildSpec, mut on_line: impl FnMut(&str)) -> Result<String, String> {
    if spec.context.trim().is_empty() {
        return Err(t!("Není zadaná složka kontextu", "No build context folder given").into());
    }
    let ctx = wsl::to_linux_path(distro, &spec.context)?;
    let mut spec = spec.clone();
    if is_windows_path(&spec.dockerfile) {
        spec.dockerfile = wsl::to_linux_path(distro, &spec.dockerfile)?;
    }
    if wsl::run(distro, None, None, &["test", "-d", &ctx]).is_err() {
        let e = tf!("Složka kontextu {ctx} neexistuje", "Build context folder {ctx} does not exist");
        on_line(&e);
        return Err(e);
    }
    let df = spec.dockerfile_arg().unwrap_or_else(|| "Dockerfile".into());
    if wsl::run(distro, None, Some(&ctx), &["test", "-f", &df]).is_err() {
        let e = tf!("Dockerfile {df} v {ctx} neexistuje", "Dockerfile {df} not found in {ctx}");
        on_line(&e);
        return Err(e);
    }
    let args = spec.args_for(engine, port, tls);
    on_line(&format!("$ cd {ctx}"));
    // hodnoty build argumentů s „tajnými“ názvy ve výpisu zamaskovat (F-11)
    let shown: Vec<String> = args
        .iter()
        .map(|a| match a.split_once('=') {
            Some((k, _)) if crate::inspect::is_secret(k) => format!("{k}=***"),
            _ => a.clone(),
        })
        .collect();
    on_line(&format!("$ {}", shown.join(" ")));
    let code = wsl::run_stream(distro, None, Some(&ctx), &args, Some(&CANCEL), &mut on_line)?;
    if CANCEL.load(std::sync::atomic::Ordering::SeqCst) {
        // zabitý wsl.exe nemusí ukončit CLI uvnitř distribuce – dorazit ho tam (jen náš build s tímto kontextem)
        let end = args.iter().position(|a| a == "build").map(|i| i + 1).unwrap_or(args.len());
        let pattern = args[..end].join(" ");
        let _ = wsl::run(distro, None, None, &["pkill", "-INT", "-f", &pattern]);
        if engine == Engine::Podman {
            // Podman (vzdálený build) provádí RUN dál i po odpojení klienta – ukončit kroky,
            // které běží pod buildah-oci-runtime (procesy buildů služby Podman API)
            let _ = wsl::run(distro, Some("root"), None, &["sh", "-c", KILL_PODMAN_BUILD_STEPS]);
        }
        let e = t!("Sestavení zrušeno", "Build cancelled").to_string();
        on_line(&e);
        return Err(e);
    }
    let tags = spec.tag_list();
    if code == 0 {
        Ok(if tags.is_empty() {
            t!("Image sestaven (bez tagu)", "Image built (untagged)").into()
        } else {
            tf!("Image {} sestaven", "Image {} built", tags.join(", "))
        })
    } else {
        Err(tf!("Sestavení selhalo (kód {code})", "Build failed (code {code})"))
    }
}
