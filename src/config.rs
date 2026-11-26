//! Einstellungen, gespeichert als JSON unter %APPDATA%\TurboCommander Lite\config.json – getrennt von der
//! Vollversion, damit sich beide auf demselben PC nicht gegenseitig Einstellungen wegschreiben.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::theme::ThemeMode;
use crate::vfs::{Location, SortKey};
use crate::win;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Favorite {
    pub name: String,
    pub loc: Location,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TabState {
    pub loc: Location,
    pub sort: SortKey,
    pub desc: bool,
    /// Rasteransicht mit Miniaturen statt der Liste
    #[serde(default)]
    pub grid: bool,
}

/// Spalten der Dateiliste neben dem Namen
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Col {
    Size,
    Kind,
    Date,
}

impl Col {
    /// Alle Spalten in der Reihenfolge, in der eine neu eingeschaltete ihren Platz findet
    pub const ALL: [Col; 3] = [Col::Size, Col::Kind, Col::Date];
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneState {
    pub tabs: Vec<TabState>,
    pub active: usize,
}

/// Die Rückfragen, die sich einzeln abschalten lassen
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    Copy,
    Move,
    Trash,
    /// Endgültig löschen: Umschalt+Entf, Laufwerke ohne Papierkorb
    Delete,
}

impl Ask {
    pub const ALL: [Ask; 4] = [Ask::Copy, Ask::Move, Ask::Trash, Ask::Delete];
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `None` = beim ersten Start dem Windows-Design folgen.
    pub theme: Option<ThemeMode>,
    /// Fenster übereinander (true) oder nebeneinander (false).
    pub stacked: bool,
    pub split: f32,
    pub show_hidden: bool,
    pub favorites: Vec<Favorite>,
    pub panes: [PaneState; 2],
    pub active_pane: usize,
    pub col_size: f32,
    pub col_type: f32,
    pub col_date: f32,
    /// Gezeigte Spalten in ihrer Reihenfolge von links nach rechts; gilt für beide Fenster
    pub columns: Vec<Col>,
    /// Übertragungen auf demselben Laufwerk nacheinander ausführen statt gleichzeitig
    pub transfers_serial: bool,
    /// Zuletzt im Netzwerk gefundene Computer – damit „Netzwerk" sofort etwas zeigt, während die Suche läuft.
    pub known_computers: Vec<String>,
    /// Sprache der Oberfläche; `None` = wie Windows
    pub lang: Option<crate::lang::Lang>,
    /// Schnellansicht je Fenster ein- oder ausgeschaltet
    pub quick_view: [bool; 2],
    /// Anteil der Schnellansicht an der Fläche des Fensters
    pub preview_split: f32,
    /// Kantenlänge der Miniaturen in der Rasteransicht; gilt für beide Fenster
    pub thumb_size: f32,
    /// Vor dem Kopieren und vor dem Verschieben ins andere Fenster nachfragen (F5, F6)
    pub ask_before_copy: bool,
    pub ask_before_move: bool,
    /// Vor dem Löschen in den Papierkorb nachfragen
    pub ask_before_trash: bool,
    /// Vor dem endgültigen Löschen nachfragen (Umschalt+Entf, Laufwerke ohne Papierkorb)
    pub ask_before_delete: bool,
}

impl Default for Config {
    fn default() -> Self {
        let folders = win::user_folders();
        let find = |name: &str| folders.iter().find(|(n, _)| *n == name).map(|(_, p)| p.clone());
        let home = std::env::var("USERPROFILE").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("C:\\"));
        let favorites = ["Desktop", "Downloads", "Dokumente", "Bilder"]
            .iter()
            .filter_map(|n| find(n).map(|p| Favorite { name: win::folder_label(n).to_owned(), loc: Location::Local(p) }))
            .collect();
        let tab = |loc: Location| PaneState {
            tabs: vec![TabState { loc, sort: SortKey::Name, desc: false, grid: false }],
            active: 0,
        };
        Self {
            theme: None,
            stacked: true,
            split: 0.5,
            show_hidden: false,
            favorites,
            panes: [
                tab(Location::Local(home.clone())),
                tab(Location::Local(find("Downloads").unwrap_or(home))),
            ],
            active_pane: 0,
            col_size: 92.0,
            col_type: 150.0,
            col_date: 138.0,
            columns: vec![Col::Size, Col::Kind, Col::Date],
            transfers_serial: true,
            known_computers: Vec::new(),
            thumb_size: crate::pane::THUMB_DEFAULT,
            ask_before_copy: true,
            ask_before_move: true,
            ask_before_trash: true,
            ask_before_delete: true,
            lang: None,
            quick_view: [false; 2],
            preview_split: 0.4,
        }
    }
}

impl Config {
    pub fn dir() -> PathBuf {
        let base = std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("."));
        base.join("TurboCommander Lite")
    }

    fn path() -> PathBuf {
        Self::dir().join("config.json")
    }

    pub fn load() -> Self {
        let path = Self::path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match serde_json::from_str::<Config>(&text) {
            Ok(mut cfg) => {
                cfg.split = cfg.split.clamp(0.15, 0.85);
                cfg.preview_split = cfg.preview_split.clamp(0.15, 0.85);
                cfg.active_pane = cfg.active_pane.min(1);
                cfg.thumb_size = if cfg.thumb_size.is_finite() {
                    cfg.thumb_size.clamp(crate::pane::THUMB_MIN, crate::pane::THUMB_MAX)
                } else {
                    crate::pane::THUMB_DEFAULT
                };
                // Jede Spalte höchstens einmal
                let mut seen = Vec::new();
                cfg.columns.retain(|col| !seen.contains(col) && { seen.push(*col); true });
                cfg
            }
            Err(_) => {
                // Unlesbare Datei nicht überschreiben, sondern beiseitelegen.
                let _ = std::fs::rename(&path, path.with_extension("json.defekt"));
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        let dir = Self::dir();
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let Ok(text) = serde_json::to_string_pretty(self) else { return };
        let tmp = dir.join("config.json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, Self::path());
        }
    }

    /// Wird vor diesem Vorgang nachgefragt?
    pub fn asks(&self, what: Ask) -> bool {
        match what {
            Ask::Copy => self.ask_before_copy,
            Ask::Move => self.ask_before_move,
            Ask::Trash => self.ask_before_trash,
            Ask::Delete => self.ask_before_delete,
        }
    }

    pub fn set_asks(&mut self, what: Ask, ask: bool) {
        match what {
            Ask::Copy => self.ask_before_copy = ask,
            Ask::Move => self.ask_before_move = ask,
            Ask::Trash => self.ask_before_trash = ask,
            Ask::Delete => self.ask_before_delete = ask,
        }
    }

    /// Schaltet eine Spalte ein oder aus. Eine neue Spalte kommt neben ihren nächsten Nachbarn aus der üblichen
    /// Reihenfolge: hinter den nächsten gezeigten Vorgänger, sonst vor den nächsten Nachfolger.
    pub fn toggle_column(&mut self, col: Col) {
        let shown = |columns: &[Col], wanted: &Col| columns.iter().position(|c| c == wanted);
        if let Some(at) = shown(&self.columns, &col) {
            self.columns.remove(at);
            return;
        }
        let rank = Col::ALL.iter().position(|c| *c == col).unwrap_or(0);
        let before = Col::ALL[..rank].iter().rev().find_map(|prev| shown(&self.columns, prev)).map(|at| at + 1);
        let place = before.or_else(|| Col::ALL[rank + 1..].iter().find_map(|next| shown(&self.columns, next)));
        self.columns.insert(place.unwrap_or(self.columns.len()), col);
    }

    /// Setzt eine Spalte an die Stelle einer anderen (Umsortieren durch Ziehen).
    pub fn move_column(&mut self, col: Col, onto: Col) {
        let (Some(from), Some(to)) = (self.columns.iter().position(|c| *c == col), self.columns.iter().position(|c| *c == onto)) else {
            return;
        };
        let moved = self.columns.remove(from);
        self.columns.insert(to, moved);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_can_be_chosen_and_reordered() {
        let mut cfg = Config { columns: vec![Col::Size, Col::Kind, Col::Date], ..Config::default() };
        cfg.toggle_column(Col::Kind);
        assert_eq!(cfg.columns, [Col::Size, Col::Date]);
        // Nach rechts ziehen: landet auf dem Platz der Zielspalte, die übrigen rücken auf.
        cfg.move_column(Col::Size, Col::Date);
        assert_eq!(cfg.columns, [Col::Date, Col::Size]);
        // Wieder eingeschaltet, sucht sich die Spalte ihren Platz hinter der letzten „früheren".
        cfg.toggle_column(Col::Kind);
        assert_eq!(cfg.columns, [Col::Date, Col::Size, Col::Kind]);
        cfg.columns.clear();
        cfg.toggle_column(Col::Kind);
        assert_eq!(cfg.columns, [Col::Kind]);
    }

    #[test]
    fn old_settings_ask_everywhere() {
        let old: Config = serde_json::from_str("{}").unwrap();
        assert!(Ask::ALL.iter().all(|what| old.asks(*what)), "ohne Angabe wird überall nachgefragt");
        let mut quiet: Config = serde_json::from_str(r#"{"ask_before_trash": false}"#).unwrap();
        assert!(!quiet.asks(Ask::Trash) && quiet.asks(Ask::Delete), "die Schalter sind unabhängig");
        quiet.set_asks(Ask::Move, false);
        assert_eq!(Ask::ALL.map(|what| quiet.asks(what)), [true, false, false, true]);
        let back: Config = serde_json::from_str(&serde_json::to_string(&quiet).unwrap()).unwrap();
        assert_eq!(Ask::ALL.map(|what| back.asks(what)), [true, false, false, true]);
    }

    #[test]
    fn settings_of_the_full_version_can_be_read() {
        // Eine Sicherung der Vollversion enthält Felder, die es hier nicht gibt; sie werden übergangen.
        let full: Config = serde_json::from_str(r#"{"sites": [], "workspaces": [], "columns": ["Size", "Date"]}"#).unwrap();
        assert_eq!(full.columns, [Col::Size, Col::Date]);
    }
}
