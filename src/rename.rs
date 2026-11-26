//! Umbenennen in einem Ordner auf diesem PC (F2 und das Zurücknehmen mit Strg+Z).
//! Das Mehrfach-Umbenennen mit Regeln und Vorschau gibt es in der Vollversion.

use std::collections::HashSet;

use crate::lang::t;
use crate::tf;
use crate::util::fold_key;
use crate::vfs::{Location, io_msg};

/// Benennt die Paare (alt, neu) in `loc` um. Liefert die erfolgten Paare und die Fehlermeldungen.
/// Eine vorhandene Datei wird nie ersetzt; tauschen zwei Einträge ihre Namen, geht es über einen Zwischennamen.
pub fn execute(loc: &Location, pairs: &[(String, String)]) -> (Vec<(String, String)>, Vec<String>) {
    let mut done = Vec::new();
    let mut errors = Vec::new();
    let Location::Local(dir) = loc else {
        return (done, vec![t("Hier kann nicht umbenannt werden.", "Renaming is not possible here.").to_owned()]);
    };
    let rename = |from: &str, to: &str| -> Result<(), String> {
        // Auch Windows ersetzt beim Umbenennen eine vorhandene Datei – das darf hier nie passieren.
        let case_only = fold_key(from) == fold_key(to);
        if !case_only && dir.join(to).exists() {
            return Err(tf!("„{to}\" existiert bereits", "“{to}” already exists"));
        }
        std::fs::rename(dir.join(from), dir.join(to)).map_err(|e| io_msg(&e))
    };

    let old_keys: HashSet<String> = pairs.iter().map(|(o, _)| fold_key(o)).collect();
    // Einträge, deren neuer Name gerade noch von einem anderen belegt ist, gehen über einen Zwischennamen.
    let mut staged: Vec<(String, String, String)> = Vec::new();
    for (i, (old, new)) in pairs.iter().enumerate() {
        let needs_temp = fold_key(old) != fold_key(new) && old_keys.contains(&fold_key(new));
        if needs_temp {
            let temp = format!("~tc{i:04}~{old}");
            match rename(old, &temp) {
                Ok(()) => staged.push((temp, old.clone(), new.clone())),
                Err(e) => errors.push(format!("{old}: {e}")),
            }
        } else {
            match rename(old, new) {
                Ok(()) => done.push((old.clone(), new.clone())),
                Err(e) => errors.push(format!("{old}: {e}")),
            }
        }
    }
    for (temp, old, new) in staged {
        match rename(&temp, &new) {
            Ok(()) => done.push((old, new)),
            Err(e) => {
                // Zurück auf den alten Namen, damit keine Zwischennamen liegen bleiben.
                let _ = rename(&temp, &old);
                errors.push(format!("{old}: {e}"));
            }
        }
    }
    (done, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_never_overwrites_existing_file() {
        let dir = std::env::temp_dir().join(format!("tc-rename-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "A").unwrap();
        std::fs::write(dir.join("b.txt"), "B").unwrap();
        let pairs = vec![("a.txt".to_owned(), "b.txt".to_owned())];
        let (done, errors) = execute(&Location::Local(dir.clone()), &pairs);
        assert!(done.is_empty());
        assert_eq!(errors.len(), 1);
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "A");
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "B");
        // Nur Groß-/Kleinschreibung ändern ist erlaubt.
        let (done, errors) = execute(&Location::Local(dir.clone()), &[("a.txt".to_owned(), "A.TXT".to_owned())]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(done.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn swap_goes_through_temp_names() {
        let dir = std::env::temp_dir().join(format!("tc-rename-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "A").unwrap();
        std::fs::write(dir.join("b.txt"), "B").unwrap();
        let pairs = vec![("a.txt".to_owned(), "b.txt".to_owned()), ("b.txt".to_owned(), "a.txt".to_owned())];
        let (done, errors) = execute(&Location::Local(dir.clone()), &pairs);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(done.len(), 2);
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "B");
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "A");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
