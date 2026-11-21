//! Vygeneruje ikonu aplikace (.ico) a vloží ji do EXE spolu s informacemi o verzi.

#[path = "src/icon_art.rs"]
mod icon_art;

fn main() {
    println!("cargo:rerun-if-changed=src/icon_art.rs");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon");
    build_label();

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico_path = out.join("wsl-container-desktop.ico");
    std::fs::write(&ico_path, icon_art::ico(&[16, 20, 24, 32, 40, 48, 64, 128, 256])).expect("zápis .ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(ico_path.to_str().unwrap());
        res.set("ProductName", "WSL Container Desktop");
        res.set("FileDescription", "WSL Container Desktop – správa Dockeru ve WSL");
        res.set("CompanyName", "WSL Container Desktop");
        res.set("LegalCopyright", "");
        res.compile().expect("kompilace Windows resource (rc.exe)");
    }
}

/// `WSLCD_BUILD` = „větev@commit“ u buildu z jiné větve než main/master, jinak prázdné
/// (aplikace pak ukazuje jen verzi). Bez gitu (zdrojový balíček) je prázdné.
fn build_label() {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let label = match (git(&["rev-parse", "--abbrev-ref", "HEAD"]), git(&["rev-parse", "--short", "HEAD"])) {
        (Some(branch), Some(commit)) if !matches!(branch.as_str(), "main" | "master" | "HEAD") => {
            if let Some(dir) = git(&["rev-parse", "--git-dir"]) {
                println!("cargo:rerun-if-changed={dir}/HEAD");
                println!("cargo:rerun-if-changed={dir}/refs/heads/{branch}");
            }
            format!("{branch}@{commit}")
        }
        _ => String::new(),
    };
    println!("cargo:rustc-env=WSLCD_BUILD={label}");
}
