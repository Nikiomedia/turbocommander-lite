//! Einstellungen sichern und wiederherstellen (Favoriten, Darstellung).
//!
//! Das Format ist dasselbe wie in der Vollversion: eine Sicherung der Lite-Ausgabe lässt sich dort
//! wiederherstellen, und umgekehrt übernimmt die Lite-Ausgabe aus einer Sicherung der Vollversion, was sie kennt.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::lang::t;

pub const EXT: &str = "tcbackup";
const APP: &str = "TurboCommander";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BackupFile {
    pub app: String,
    pub format: u32,
    pub created: String,
    pub config: Config,
}

/// Erzeugt den Inhalt einer Sicherungsdatei.
pub fn create(cfg: &Config) -> Result<String, String> {
    let file = BackupFile {
        app: APP.to_owned(),
        format: 1,
        created: chrono::Local::now().format("%d.%m.%Y, %H:%M").to_string(),
        config: cfg.clone(),
    };
    serde_json::to_string_pretty(&file).map_err(|e| e.to_string())
}

/// Lässt aus den Einstellungen einer Sicherung weg, was es nur in der Vollversion gibt (FTP-Orte, die Galerie,
/// Medienspalten): Favoriten und Spalten, die hier unbekannt sind, und die offenen Tabs, die ohnehin nicht
/// übernommen werden.
fn keep_known(config: &mut serde_json::Value) {
    let Some(config) = config.as_object_mut() else { return };
    config.remove("panes");
    if let Some(serde_json::Value::Array(favorites)) = config.get_mut("favorites") {
        favorites.retain(|f| serde_json::from_value::<crate::config::Favorite>(f.clone()).is_ok());
    }
    if let Some(serde_json::Value::Array(columns)) = config.get_mut("columns") {
        columns.retain(|c| serde_json::from_value::<crate::config::Col>(c.clone()).is_ok());
    }
}

pub fn parse(text: &str) -> Result<BackupFile, String> {
    let not_ours = || t("Das ist keine TurboCommander-Sicherung.", "This is not a TurboCommander backup.").to_owned();
    let mut value: serde_json::Value = serde_json::from_str(text).map_err(|_| not_ours())?;
    if let Some(config) = value.get_mut("config") {
        keep_known(config);
    }
    let file: BackupFile = serde_json::from_value(value).map_err(|_| not_ours())?;
    if file.app != APP {
        return Err(not_ours());
    }
    if file.format > 1 {
        return Err(t(
            "Diese Sicherung stammt von einer neueren TurboCommander-Version.",
            "This backup was created by a newer version of TurboCommander.",
        )
        .to_owned());
    }
    Ok(file)
}

pub fn load(path: &Path) -> Result<BackupFile, String> {
    let text = std::fs::read_to_string(path).map_err(|e| crate::vfs::io_msg(&e))?;
    parse(&text)
}

/// Überträgt eine Sicherung in die laufenden Einstellungen. Offene Tabs bleiben, wie sie sind.
pub fn apply(current: &mut Config, backup: &BackupFile) {
    current.favorites = backup.config.favorites.clone();
    current.theme = backup.config.theme;
    current.stacked = backup.config.stacked;
    current.split = backup.config.split.clamp(0.15, 0.85);
    current.show_hidden = backup.config.show_hidden;
    current.col_size = backup.config.col_size;
    current.col_type = backup.config.col_type;
    current.col_date = backup.config.col_date;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Favorite;
    use crate::vfs::Location;

    #[test]
    fn roundtrip() {
        let mut cfg = Config::default();
        cfg.favorites = vec![Favorite { name: "Filme".into(), loc: Location::Local("D:\\Filme".into()) }];
        cfg.col_type = 201.0;
        let file = parse(&create(&cfg).unwrap()).unwrap();
        let mut fresh = Config::default();
        apply(&mut fresh, &file);
        assert_eq!(fresh.favorites.len(), 1);
        assert_eq!(fresh.col_type, 201.0);
    }

    #[test]
    fn a_backup_of_the_full_version_is_read() {
        // So sieht ein Ausschnitt einer Sicherung der Vollversion aus: mit FTP-Favorit, Galerie-Tab,
        // Medienspalte und verschlüsselten Passwörtern.
        let text = r#"{
            "app": "TurboCommander", "format": 1, "created": "26.11.2026, 10:00",
            "config": {
                "favorites": [
                    {"name": "Filme", "loc": {"Local": "D:\\Filme"}},
                    {"name": "Server", "loc": {"Ftp": {"site": "web", "path": "/"}}}
                ],
                "panes": [{"tabs": [{"loc": "Gallery", "sort": "Name", "desc": false}], "active": 0},
                          {"tabs": [], "active": 0}],
                "columns": ["Size", "Res", "Date"],
                "sites": [{"name": "web"}],
                "col_type": 180.0
            },
            "passwords": {"iterations": 1, "salt": "", "nonce": "", "data": ""}
        }"#;
        let file = parse(text).unwrap();
        assert_eq!(file.config.favorites.len(), 1);
        assert_eq!(file.config.favorites[0].name, "Filme");
        assert_eq!(file.config.columns, [crate::config::Col::Size, crate::config::Col::Date]);
        assert_eq!(file.config.col_type, 180.0);
    }

    #[test]
    fn foreign_files_are_rejected() {
        assert!(parse("{}").is_err());
        assert!(parse("kein json").is_err());
        assert!(parse(r#"{"app": "Anders", "format": 1, "created": "", "config": {}}"#).is_err());
    }
}
