//! Test: každý znak v textech UI (řetězce ve zdrojácích) musí mít glyf v písmech aplikace,
//! jinak se v okně a v toastech kreslí čtvereček (např. „→“ chybí ve výchozím Ubuntu-Light).

/// Znaky mimo ASCII z řetězcových literálů (bez komentářů) ve všech `src/**/*.rs`.
fn literal_chars() -> std::collections::BTreeMap<char, String> {
    let mut found = std::collections::BTreeMap::new();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if p.extension().and_then(|x| x.to_str()) != Some("rs") || p.ends_with("glyph_check.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            for (n, line) in text.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") || t.starts_with("#[") {
                    continue;
                }
                // jen obsah uvozovek (hrubě, stačí pro texty UI); escapy \u{…} se dekódují,
                // kromě ikon Phosphor (soukromá oblast U+E000–U+F8FF – obstará ikonové písmo)
                let at = || format!("{}:{}", p.strip_prefix(env!("CARGO_MANIFEST_DIR")).unwrap().display(), n + 1);
                let mut inside = false;
                let mut prev = ' ';
                let chars: Vec<char> = line.chars().collect();
                let mut i = 0;
                while i < chars.len() {
                    let c = chars[i];
                    if c == '"' && prev != '\\' {
                        inside = !inside;
                    } else if inside && c == '\\' && chars.get(i + 1) == Some(&'u') && chars.get(i + 2) == Some(&'{') {
                        if let Some(end) = chars[i..].iter().position(|x| *x == '}') {
                            let hex: String = chars[i + 3..i + end].iter().collect();
                            if let Some(e) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                                if !e.is_ascii() && !(0xE000..=0xF8FF).contains(&(e as u32)) {
                                    found.entry(e).or_insert_with(at);
                                }
                            }
                            i += end + 1;
                            prev = '}';
                            continue;
                        }
                    } else if inside && !c.is_ascii() {
                        found.entry(c).or_insert_with(at);
                    }
                    prev = if prev == '\\' && c == '\\' { ' ' } else { c };
                    i += 1;
                }
            }
        }
    }
    found
}

#[cfg(windows)]
#[test]
fn ui_text_glyphs_exist_in_app_fonts() {
    use eframe::egui;
    let ctx = egui::Context::default();
    crate::theme::apply(&ctx, "dark");
    let _ = ctx.run(egui::RawInput::default(), |_| {});
    let font = egui::FontId::proportional(14.0);
    let chars = literal_chars();
    assert!(chars.contains_key(&'\u{2514}') && chars.contains_key(&'→'), "kontrola musí vidět i escapy (└ v mapě sítí) a přímé znaky");
    let missing: Vec<String> = chars
        .into_iter()
        // kombinující diakritika (testovací data vyhledávání) nemá vlastní glyf
        .filter(|(c, _)| !(0x300..=0x36F).contains(&(*c as u32)))
        .filter(|(c, _)| !ctx.fonts(|f| f.has_glyph(&font, *c)))
        .map(|(c, at)| format!("{c:?} U+{:04X} ({at})", c as u32))
        .collect();
    assert!(missing.is_empty(), "znaky bez glyfu v písmech aplikace:\n{}", missing.join("\n"));
}
