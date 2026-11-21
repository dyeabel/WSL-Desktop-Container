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
                // jen obsah uvozovek (hrubě, stačí pro texty UI); \u{…} escapy se nekontrolují –
                // to jsou ikony Phosphor
                let mut inside = false;
                let mut prev = ' ';
                for c in line.chars() {
                    if c == '"' && prev != '\\' {
                        inside = !inside;
                    } else if inside && !c.is_ascii() {
                        found.entry(c).or_insert_with(|| format!("{}:{}", p.strip_prefix(env!("CARGO_MANIFEST_DIR")).unwrap().display(), n + 1));
                    }
                    prev = c;
                }
            }
        }
    }
    found
}

#[test]
fn ui_text_glyphs_exist_in_app_fonts() {
    use eframe::egui;
    let ctx = egui::Context::default();
    crate::theme::apply(&ctx, "dark");
    let _ = ctx.run(egui::RawInput::default(), |_| {});
    let font = egui::FontId::proportional(14.0);
    let missing: Vec<String> = literal_chars()
        .into_iter()
        // kombinující diakritika (testovací data vyhledávání) nemá vlastní glyf
        .filter(|(c, _)| !(0x300..=0x36F).contains(&(*c as u32)))
        .filter(|(c, _)| !ctx.fonts(|f| f.has_glyph(&font, *c)))
        .map(|(c, at)| format!("{c:?} U+{:04X} ({at})", c as u32))
        .collect();
    assert!(missing.is_empty(), "znaky bez glyfu v písmech aplikace:\n{}", missing.join("\n"));
}
