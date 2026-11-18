//! Vygeneruje ikonu aplikace (.ico) a vloží ji do EXE spolu s informacemi o verzi.

#[path = "src/icon_art.rs"]
mod icon_art;

fn main() {
    println!("cargo:rerun-if-changed=src/icon_art.rs");
    println!("cargo:rerun-if-changed=build.rs");

    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico_path = out.join("wls-desktop.ico");
    std::fs::write(&ico_path, icon_art::ico(&[16, 20, 24, 32, 40, 48, 64, 128, 256])).expect("zápis .ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon(ico_path.to_str().unwrap());
        res.set("ProductName", "WLS Desktop");
        res.set("FileDescription", "WLS Desktop – správa Dockeru ve WSL");
        res.set("CompanyName", "WLS Desktop");
        res.set("LegalCopyright", "");
        res.compile().expect("kompilace Windows resource (rc.exe)");
    }
}
