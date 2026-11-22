//! Oznámení. Windows: toast; aplikace se registruje jako odesílatel přes AppUserModelId v HKCU
//! (bez instalace a zástupce), aby oznámení nesla její název a ikonu.
//! Linux: `notify-send` (bez něj se oznámení nezobrazí, záznam v historii zůstává).

#[cfg(windows)]
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

/// Text těla pro servery oznámení, které ho čtou jako Pango markup (GNOME aj.): `&`, `<`, `>`
/// se nahradí entitami, jinak by např. název kontejneru `a<b` nebo `&amp;` zmizel či se zkomolil.
/// Titulek je podle specifikace prostý text, ten se neescapuje.
#[cfg_attr(windows, allow(dead_code))] // volá jen Linux; čistá logika se testuje i na Windows
fn escape_markup(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Argumenty `notify-send`. `--` brání tomu, aby titulek začínající pomlčkou byl přepínač.
#[cfg_attr(windows, allow(dead_code))] // volá jen Linux; čistá logika se testuje i na Windows
pub fn notify_send_args(title: &str, body: &str) -> Vec<String> {
    vec!["--app-name=WSL Container Desktop".into(), "--".into(), title.into(), escape_markup(body)]
}

/// Linux: `notify-send`. Když chybí (nebo selže), nic se nestane – záznam v historii zůstává.
/// `on_click` se na Linuxu nevolá (`notify-send` kliknutí nehlásí).
#[cfg(not(windows))]
pub fn show_with(title: &str, body: &str, _on_click: impl Fn() + Send + 'static) {
    use std::process::{Command, Stdio};
    let child = Command::new("notify-send")
        .args(notify_send_args(title, body))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    // proces se musí převzít, jinak zůstane zombie
    if let Ok(mut c) = child {
        std::thread::spawn(move || {
            let _ = c.wait();
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn notify_send_args() {
        let a = super::notify_send_args("Kontejner spadl", "web\nkód 1");
        assert_eq!(a, ["--app-name=WSL Container Desktop", "--", "Kontejner spadl", "web\nkód 1"]);
        // titulek začínající pomlčkou se nesmí vyložit jako přepínač
        let a = super::notify_send_args("-h", "x");
        assert_eq!(a[1], "--");
        assert_eq!(a[2], "-h");
    }

    #[test]
    fn body_markup_is_escaped() {
        let a = super::notify_send_args("a & <b>", "<b>x</b> & &amp; y");
        // titulek je prostý text
        assert_eq!(a[2], "a & <b>");
        // `&` se escapuje první, aby se entity nezdvojily
        assert_eq!(a[3], "&lt;b&gt;x&lt;/b&gt; &amp; &amp;amp; y");
    }
}
