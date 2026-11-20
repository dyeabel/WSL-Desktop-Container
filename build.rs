//! Vygeneruje ikonu aplikace (.ico) a vloží ji do EXE spolu s informacemi o verzi.

#[path = "src/icon_art.rs"]
mod icon_art;

fn main() {
    println!("cargo:rerun-if-changed=src/icon_art.rs");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/icon");

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
