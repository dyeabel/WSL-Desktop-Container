//! Jednoduchá lokalizace: čeština (výchozí) a angličtina.
//!
//! Texty se píšou přímo v kódu jako dvojice:
//! - `t!("Kontejnery", "Containers")` – pevný text (`&'static str`)
//! - `tf!("Smazáno {n}", "Deleted {n}")` – formátovaný text (`String`, jako `format!`)
//!
//! Jazyk se přepíná za běhu (`set_language`), další snímek UI už kreslí nový jazyk.

use std::sync::atomic::{AtomicBool, Ordering};

static ENGLISH: AtomicBool = AtomicBool::new(false);

/// Nastaví jazyk podle kódu ("en" = angličtina, cokoli jiného = čeština).
pub fn set_language(code: &str) {
    ENGLISH.store(code.eq_ignore_ascii_case("en"), Ordering::Relaxed);
}

pub fn is_en() -> bool {
    ENGLISH.load(Ordering::Relaxed)
}

/// Dostupné jazyky: (kód, název v daném jazyce).
pub const LANGUAGES: [(&str, &str); 2] = [("cs", "Čeština"), ("en", "English")];

/// Pevný text v aktuálním jazyce: `t!("česky", "English")`.
#[macro_export]
macro_rules! t {
    ($cs:expr, $en:expr $(,)?) => {
        if $crate::i18n::is_en() { $en } else { $cs }
    };
}

/// Formátovaný text v aktuálním jazyce: `tf!("Smazáno {n}", "Deleted {n}")`.
/// Oba řetězce musí být literály (jako u `format!`), mohou používat stejné proměnné.
#[macro_export]
macro_rules! tf {
    ($cs:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::is_en() { format!($en $(, $arg)*) } else { format!($cs $(, $arg)*) }
    };
}

#[cfg(test)]
mod tests {
    // Jazyk je globální – testy ho nepřepínají, aby se nepraly s ostatními testy (běží souběžně
    // a předpokládají češtinu). Ověřuje se jen výchozí čeština a formátování.
    #[test]
    fn default_czech_and_format() {
        let n = 3;
        assert!(!super::is_en());
        assert_eq!(crate::t!("Ano", "Yes"), "Ano");
        assert_eq!(crate::tf!("Smazáno {n}", "Deleted {n}"), "Smazáno 3");
        assert_eq!(crate::tf!("Smazáno {}", "Deleted {}", n), "Smazáno 3");
    }
}
