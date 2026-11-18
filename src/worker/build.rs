//! Sestavení image z Dockerfile přes CLI enginu ve WSL s průběžným výstupem.

use super::spec::{is_windows_path, BuildSpec};
use super::{Msg, Worker};
use crate::engine::Engine;
use crate::wsl;

impl Worker {
    pub(super) fn build_image(&self, spec: BuildSpec) -> Result<String, String> {
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
    on_line(&format!("$ {}", args.join(" ")));
    let code = wsl::run_stream(distro, None, Some(&ctx), &args, &mut on_line)?;
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
