//! Oznámení ve Windows (toast). Aplikace se registruje jako odesílatel přes
//! AppUserModelId v HKCU (bez instalace a zástupce), aby oznámení nesla její název a ikonu.

pub const AUMID: &str = "WSL.Container.Desktop";

#[cfg(windows)]
fn app_dir() -> Option<std::path::PathBuf> {
    crate::config::data_dir()
}

/// Zaregistruje AUMID (název a ikonu) – jen do HKCU, bez práv správce. Volat jednou při startu.
#[cfg(windows)]
pub fn register() {
    use std::os::windows::process::CommandExt;
    let icon = app_dir().map(|d| {
        let _ = std::fs::create_dir_all(&d);
        let p = d.join("icon.png");
        let _ = std::fs::write(&p, crate::icon_art::png(64, &crate::icon_art::render(64, None)));
        p
    });
    let key = format!(r"HKCU\Software\Classes\AppUserModelId\{AUMID}");
    let mut vals = vec![("DisplayName", "WSL Container Desktop".to_string())];
    if let Some(i) = icon {
        vals.push(("IconUri", i.display().to_string()));
    }
    for (name, data) in vals {
        let _ = std::process::Command::new("reg.exe")
            .args(["add", &key, "/v", name, "/t", "REG_SZ", "/d", &data, "/f"])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output();
    }
}

#[cfg(not(windows))]
pub fn register() {}

/// Zobrazí oznámení; kliknutí nejdřív zavolá `on_click` (např. požadavek na přechod
/// na stránku) a pak otevře okno aplikace.
#[cfg(windows)]
pub fn show_with(title: &str, body: &str, on_click: impl Fn() + Send + 'static) {
    use tauri_winrt_notification::{Duration, Sound, Toast};
    // druhý řádek (za `\n`) jako samostatný text oznámení
    let (line1, line2) = body.split_once('\n').unwrap_or((body, ""));
    let r = Toast::new(AUMID)
        .title(title)
        .text1(line1)
        .text2(line2)
        .sound(Some(Sound::Default))
        .duration(Duration::Short)
        .on_activated(move |_| {
            on_click();
            crate::tray::show_window();
            Ok(())
        })
        .show();
    if let Err(e) = r {
        eprintln!("toast: {e:?}");
    }
}

#[cfg(not(windows))]
pub fn show_with(_: &str, _: &str, _: impl Fn() + Send + 'static) {}
