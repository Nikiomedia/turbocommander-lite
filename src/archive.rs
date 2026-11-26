//! Archive als Ordner lesen. Die Lite-Ausgabe kennt nur ZIP; 7z, RAR und TAR gibt es in der Vollversion.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::lang::t;
use crate::vfs::{Listing, io_msg};
pub use crate::zipfs::ZipItem as Item;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Zip,
}

/// Erkennt ein Archiv an seiner Endung.
pub fn kind(name: &str) -> Option<Kind> {
    name.to_lowercase().ends_with(".zip").then_some(Kind::Zip)
}

/// Kurzname des Formats für Anzeigen
pub fn label(_path: &Path) -> &'static str {
    "ZIP"
}

fn read_index(path: &Path) -> Result<Vec<Item>, String> {
    let mut archive = crate::zipfs::open(path)?;
    Ok(crate::zipfs::index(&mut archive))
}

type Cached = (u64, Option<SystemTime>, Arc<Vec<Item>>);

/// Das Inhaltsverzeichnis eines Archivs. Es wird gemerkt, solange sich Größe und Datum der Datei nicht ändern.
pub fn index(path: &Path) -> Result<Arc<Vec<Item>>, String> {
    static CACHE: Mutex<Vec<(PathBuf, Cached)>> = Mutex::new(Vec::new());
    kind(&path.to_string_lossy()).ok_or_else(|| t("Kein bekanntes Archivformat.", "Not a known archive format.").to_owned())?;
    let meta = std::fs::metadata(path).map_err(|e| io_msg(&e))?;
    let stamp = (meta.len(), meta.modified().ok());
    {
        let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, (len, modified, items))) = cache.iter().find(|(known, _)| known == path) {
            if (*len, *modified) == stamp {
                return Ok(items.clone());
            }
        }
    }
    let items = Arc::new(read_index(path)?);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(known, _)| known != path);
    cache.push((path.to_path_buf(), (stamp.0, stamp.1, items.clone())));
    // Ein paar zuletzt geöffnete Archive genügen.
    if cache.len() > 6 {
        cache.remove(0);
    }
    Ok(items)
}

/// Die Einträge eines Ordners im Archiv
pub fn list(path: &Path, inner: &str) -> Result<Listing, String> {
    crate::zipfs::list_items(&index(path)?, inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_zip_is_an_archive() {
        assert_eq!(kind("Bilder.ZIP"), Some(Kind::Zip));
        assert_eq!(kind("quellen.tar.gz"), None);
        assert_eq!(kind("a.7z"), None);
        assert_eq!(kind("a.part1.rar"), None);
        assert_eq!(label(Path::new("x.zip")), "ZIP");
    }
}
