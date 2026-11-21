//! Hostitel nad `wsl.exe`: příkazy běží v distribuci WSL2.

use std::io::Write;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;

use super::{track, ExecOut, ExecReq, Host, HostError, Privilege};
use crate::config::Config;
use crate::http::Endpoint;
use crate::wsl;

pub struct WslHost {
    distro: String,
}

impl WslHost {
    pub fn new(distro: impl Into<String>) -> Self {
        WslHost { distro: distro.into() }
    }

    pub fn distro(&self) -> &str {
        &self.distro
    }

    /// Stejný způsob spouštění, jiná distribuce.
    pub fn for_distro(&self, name: &str) -> WslHost {
        WslHost::new(name)
    }
}

/// Argumenty `wsl.exe`: `-d distro [-u root] [--cd cwd] -e args…`.
pub(crate) fn wsl_args(distro: &str, req: &ExecReq) -> Vec<String> {
    let mut v: Vec<String> = vec!["-d".into(), distro.into()];
    if req.privilege != Privilege::User {
        v.extend(["-u".to_string(), "root".to_string()]);
    }
    if let Some(d) = req.cwd {
        v.extend(["--cd".to_string(), d.to_string()]);
    }
    v.push("-e".into());
    v.extend(req.args.iter().map(|a| a.to_string()));
    v
}

impl Host for WslHost {
    fn label(&self) -> String {
        self.distro.clone()
    }

    fn exec(&self, req: &ExecReq) -> Result<ExecOut, HostError> {
        let mut g = track(&self.distro, req.privilege, req.args);
        let r = (|| {
            let mut cmd = wsl::hidden("wsl.exe");
            cmd.args(wsl_args(&self.distro, req));
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
            let out = match req.stdin {
                None => cmd.stdin(Stdio::null()).output().map_err(|e| e.to_string())?,
                Some(input) => {
                    let mut child = cmd.stdin(Stdio::piped()).spawn().map_err(|e| e.to_string())?;
                    if let Some(mut si) = child.stdin.take() {
                        si.write_all(input).map_err(|e| e.to_string())?;
                    }
                    child.wait_with_output().map_err(|e| e.to_string())?
                }
            };
            Ok::<_, HostError>(ExecOut {
                code: out.status.code().unwrap_or(-1),
                stdout: out.stdout,
                stderr: out.stderr,
            })
        })();
        g.set_ok(matches!(&r, Ok(o) if o.code == 0));
        r
    }

    fn exec_stream(
        &self,
        req: &ExecReq,
        cancel: Option<&AtomicBool>,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<i32, HostError> {
        let mut g = track(&self.distro, req.privilege, req.args);
        let mut cmd = wsl::hidden("wsl.exe");
        cmd.args(wsl_args(&self.distro, req));
        let code = wsl::stream_command(&mut cmd, cancel, on_line)?;
        g.set_ok(code == 0);
        Ok(code)
    }

    fn open_terminal(&self, title: &str, req: &ExecReq) -> Result<(), HostError> {
        wsl::open_terminal(title, &wsl_args(&self.distro, req))?;
        Ok(())
    }

    fn to_guest_path(&self, path: &str) -> Result<String, HostError> {
        Ok(wsl::to_linux_path(&self.distro, path)?)
    }

    fn engine_endpoint(&self, cfg: &Config) -> Endpoint {
        Endpoint::Tcp { port: cfg.port }
    }

    fn wsl(&self) -> Option<&WslHost> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{current, HostExt};

    #[test]
    fn args_user() {
        assert_eq!(wsl_args("Ubuntu", &ExecReq::new(&["ls", "-la"])), ["-d", "Ubuntu", "-e", "ls", "-la"]);
    }

    #[test]
    fn args_engine_and_root_use_root() {
        for p in [Privilege::Engine, Privilege::Root] {
            assert_eq!(
                wsl_args("D", &ExecReq::new(&["id"]).privilege(p).cwd(Some("/tmp"))),
                ["-d", "D", "-u", "root", "--cd", "/tmp", "-e", "id"]
            );
        }
    }

    #[test]
    fn current_follows_config() {
        let mut cfg = Config::default();
        cfg.distro = "A".into();
        assert_eq!(current(&cfg).label(), "A");
        cfg.distro = "B".into();
        assert_eq!(current(&cfg).label(), "B");
    }

    #[test]
    fn for_distro_keeps_nothing_else() {
        assert_eq!(WslHost::new("A").for_distro("C").distro(), "C");
    }

    /// Živý kontrakt proti distribuci z konfigurace (spouští ji):
    /// `cargo test --release host::wsl::tests::live -- --ignored --test-threads=1`
    mod live {
        use super::*;
        use std::sync::atomic::Ordering;
        use std::time::{Duration, Instant};

        fn host() -> WslHost {
            WslHost::new(Config::load().distro)
        }

        #[test]
        #[ignore]
        fn contract() {
            let h = host();
            // `finish` výstup neořezává (jako dosavadní wsl::run), proto s koncem řádku
            let e = h.run(Privilege::User, None, &["sh", "-c", "echo out; echo err >&2; exit 3"]).unwrap_err();
            assert_eq!(e, HostError::Failed { code: 3, output: "err
".into() });
            assert_eq!(h.run(Privilege::Root, None, &["id", "-u"]).unwrap().trim(), "0");
            assert_eq!(h.run(Privilege::User, Some("/tmp"), &["pwd"]).unwrap().trim(), "/tmp");
            assert_eq!(h.run_stdin(Privilege::User, &["cat"], "x\ny").unwrap(), "x\ny");

            let mut lines = Vec::new();
            let req = ExecReq::new(&["printf", "a\\rb\\nc\\n"]);
            let code = h.exec_stream(&req, None, &mut |l| lines.push(l.to_string())).unwrap();
            assert_eq!(code, 0);
            assert_eq!(lines, ["b", "c"]);
        }

        #[test]
        #[ignore]
        fn stream_cancel() {
            let h = host();
            let cancel = AtomicBool::new(false);
            let t = Instant::now();
            std::thread::scope(|s| {
                s.spawn(|| {
                    std::thread::sleep(Duration::from_millis(300));
                    cancel.store(true, Ordering::SeqCst);
                });
                let _ = h.exec_stream(&ExecReq::new(&["sleep", "60"]), Some(&cancel), &mut |_| {});
            });
            assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
        }
    }
}
