//! Sprachumschaltung Deutsch/Englisch.
//!
//! Beide Fassungen stehen direkt an der Stelle, an der der Text gebraucht wird:
//!
//! ```ignore
//! t("Umbenennen", "Rename")                          // fester Text
//! tf!("{} von {} markiert", "{} of {} selected", a, b) // zusammengesetzter Text, wie format!
//! key("Strg+Umschalt+N")                             // Tastenkürzel → „Ctrl+Shift+N"
//! ```
//!
//! So bleibt jede Übersetzung bei dem Code, den sie beschreibt, und der Compiler prüft beide Fassungen.
//! Texte entstehen in der Sprache, die in diesem Moment eingestellt ist; nach dem Umschalten werden
//! deshalb die Listen neu aufgebaut (siehe `App::set_lang`).

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lang {
    De,
    En,
}

static ENGLISH: AtomicBool = AtomicBool::new(false);

pub fn lang() -> Lang {
    if english() { Lang::En } else { Lang::De }
}

pub fn english() -> bool {
    ENGLISH.load(Relaxed)
}

pub fn set_lang(lang: Lang) {
    ENGLISH.store(lang == Lang::En, Relaxed);
}

/// Fester Text in der eingestellten Sprache.
pub fn t<'a>(de: &'a str, en: &'a str) -> &'a str {
    if english() { en } else { de }
}

/// Tastenkürzel: im Quelltext deutsch („Strg+Umschalt+N"), im Englischen mit den dort üblichen Namen.
pub fn key(de: &str) -> String {
    if english() { key_en(de) } else { de.to_owned() }
}

fn key_en(de: &str) -> String {
    de.split('+')
        .map(|part| match part.trim() {
            "Strg" => "Ctrl",
            "Umschalt" => "Shift",
            "Entf" => "Del",
            "Einfg" => "Ins",
            "Leertaste" => "Space",
            "Rücktaste" => "Backspace",
            "Eingabe" => "Enter",
            "Bild auf" => "PgUp",
            "Bild ab" => "PgDn",
            "Pos1" => "Home",
            "Ende" => "End",
            "Pfeiltasten" => "Arrow keys",
            "Mausrad" => "mouse wheel",
            other => other,
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Zusammengesetzter Text in der eingestellten Sprache – beide Fassungen wie bei `format!`.
#[macro_export]
macro_rules! tf {
    ($de:literal, $en:literal $(, $($arg:tt)*)?) => {
        if $crate::lang::english() { format!($en $(, $($arg)*)?) } else { format!($de $(, $($arg)*)?) }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_are_renamed() {
        assert_eq!(key_en("Strg+Umschalt+N"), "Ctrl+Shift+N");
        assert_eq!(key_en("Umschalt+Entf"), "Shift+Del");
        assert_eq!(key_en("F5"), "F5");
    }
}
