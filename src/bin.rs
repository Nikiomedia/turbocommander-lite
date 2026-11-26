//! Der Papierkorb als Ort: ansehen, wiederherstellen, endgültig löschen, leeren.
//!
//! Gefragt wird Windows über dieselbe Bibliothek, die auch in den Papierkorb löscht. Die Aufrufe gehören in einen
//! Hintergrund-Thread: bei einem vollen Papierkorb dauert schon das Auflisten spürbar.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use crate::lang::t;
use crate::tf;
use crate::vfs::{Entry, Listing, Location};

/// Ein Eintrag je gelöschtem Element: der ursprüngliche Name, in `note` der Ordner, aus dem es stammt, als Datum
/// der Zeitpunkt des Löschens. `target` führt zu der Datei, wie sie im Papierkorb liegt – daran wird sie auch
/// wiedererkannt.
pub fn list() -> Result<Listing, String> {
    let items = trash::os_limited::list().map_err(|e| tf!("Papierkorb: {e}", "Recycle Bin: {e}"))?;
    let entries = items
        .into_iter()
        .map(|item| {
            let stored = PathBuf::from(&item.id);
            // Größe und Art stehen an der Datei im Papierkorb; Ordner bleiben ohne Größe.
            let meta = std::fs::metadata(&stored).ok();
            let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
            let size = meta.filter(|_| !is_dir).map_or(0, |m| m.len());
            let mut e = Entry::new(item.name.to_string_lossy().into_owned(), is_dir, size, Some(item.time_deleted));
            e.note = Some(Arc::from(item.original_parent.to_string_lossy().as_ref()));
            e.target = Some(Location::Local(stored));
            e
        })
        .collect();
    Ok(Listing { entries, space: None, empty_dirs: None, pending: false, dir_modified: None })
}

/// Woran ein Eintrag der Liste im Papierkorb wiedererkannt wird.
pub fn id_of(e: &Entry) -> Option<OsString> {
    e.target.as_ref().and_then(|target| target.local_path()).map(|path| path.as_os_str().to_owned())
}

/// Die gelöschten Elemente mit diesen Kennungen, wie Windows sie jetzt führt.
fn chosen(ids: &[OsString]) -> Result<Vec<trash::TrashItem>, String> {
    let listed = trash::os_limited::list().map_err(|e| tf!("Papierkorb: {e}", "Recycle Bin: {e}"))?;
    Ok(listed.into_iter().filter(|item| ids.contains(&item.id)).collect())
}

/// Legt die Elemente an ihren ursprünglichen Ort zurück. Liefert, wie viele es waren.
pub fn restore(ids: &[OsString]) -> Result<usize, String> {
    let items = chosen(ids)?;
    let count = items.len();
    match trash::os_limited::restore_all(items) {
        Ok(()) => Ok(count),
        Err(trash::Error::RestoreCollision { path, .. }) => Err(tf!(
            "„{}\" gibt es am ursprünglichen Ort schon – nichts wurde zurückgelegt.",
            "“{}” already exists in its original place – nothing was restored.",
            path.display()
        )),
        Err(trash::Error::RestoreTwins { path, .. }) => Err(tf!(
            "Mehrere der gewählten Elemente hießen „{}\" – bitte einzeln wiederherstellen.",
            "Several of the chosen items were called “{}” – please restore them one at a time.",
            path.display()
        )),
        Err(e) => Err(tf!("Wiederherstellen: {e}", "Restoring: {e}")),
    }
}

/// Löscht die Elemente endgültig aus dem Papierkorb.
pub fn purge(ids: &[OsString]) -> Result<usize, String> {
    let items = chosen(ids)?;
    let count = items.len();
    trash::os_limited::purge_all(items).map_err(|e| tf!("Endgültig löschen: {e}", "Deleting permanently: {e}"))?;
    Ok(count)
}

/// Leert den Papierkorb aller Laufwerke.
pub fn empty() -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::{SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW};
    let flags = SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND;
    let result = unsafe { SHEmptyRecycleBinW(std::ptr::null_mut(), std::ptr::null(), flags) };
    // Ein schon leerer Papierkorb meldet einen Fehler, der keiner ist.
    if result == 0 || trash::os_limited::is_empty().unwrap_or(false) {
        Ok(())
    } else {
        Err(t("Der Papierkorb ließ sich nicht leeren.", "The Recycle Bin could not be emptied.").to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Zum Aufräumen nach Prüfläufen von Hand: entfernt aus dem Papierkorb, was aus Ordnern unterhalb von
    /// `TC_BELOW` gelöscht wurde. `TC_BELOW=C:\…\scratch cargo test purge_what_came_from -- --ignored`
    #[test]
    #[ignore]
    fn purge_what_came_from() {
        let Ok(below) = std::env::var("TC_BELOW") else { return };
        let below = below.to_lowercase();
        let ids: Vec<OsString> = list()
            .unwrap()
            .entries
            .iter()
            .filter(|e| e.note.as_deref().is_some_and(|from| from.to_lowercase().starts_with(&below)))
            .filter_map(id_of)
            .collect();
        println!("{} entfernt", purge(&ids).unwrap());
    }

    /// Löscht eine Datei in den Papierkorb, findet sie dort, holt sie zurück, löscht sie wieder und entfernt sie
    /// dann endgültig – der Papierkorb des Nutzers bleibt sonst unberührt.
    #[test]
    fn a_deleted_file_is_listed_restored_and_purged() {
        let dir = std::env::temp_dir().join(format!("tc-bin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("papierkorb-probe.txt");
        std::fs::write(&file, b"probe").unwrap();
        if trash::delete(&file).is_err() {
            // Laufwerk ohne Papierkorb: hier gibt es nichts zu prüfen.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let here = dir.to_string_lossy().into_owned();
        let ours = |listing: Listing| -> Vec<Entry> {
            listing.entries.into_iter().filter(|e| e.note.as_deref() == Some(here.as_str())).collect()
        };
        let found = ours(list().unwrap());
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].name.as_str(), found[0].size, found[0].is_dir), ("papierkorb-probe.txt", 5, false));
        assert!(found[0].modified.is_some());
        let ids: Vec<OsString> = found.iter().filter_map(id_of).collect();
        assert_eq!(restore(&ids), Ok(1));
        assert_eq!(std::fs::read(&file).unwrap(), b"probe");
        assert!(ours(list().unwrap()).is_empty());

        trash::delete(&file).unwrap();
        let ids: Vec<OsString> = ours(list().unwrap()).iter().filter_map(id_of).collect();
        assert_eq!(purge(&ids), Ok(1));
        assert!(!file.exists() && ours(list().unwrap()).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
