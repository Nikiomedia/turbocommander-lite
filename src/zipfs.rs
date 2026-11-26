//! ZIP-Archive als Ordner lesen.

use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use zip::ZipArchive;

use crate::lang::t;
use crate::tf;
use crate::vfs::{Entry, Listing};

#[derive(Clone, Debug, PartialEq)]
pub struct ZipItem {
    pub index: usize,
    /// Normalisiert: `/` als Trenner, kein abschließender Schrägstrich.
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<i64>,
}

pub type Archive = ZipArchive<BufReader<File>>;

pub fn open(archive: &Path) -> Result<Archive, String> {
    let file = File::open(archive).map_err(|e| crate::vfs::io_msg(&e))?;
    ZipArchive::new(BufReader::new(file))
        .map_err(|e| tf!("Das Archiv lässt sich nicht lesen: {e}", "The archive cannot be read: {e}"))
}

fn zip_time(dt: Option<zip::DateTime>) -> Option<i64> {
    let dt = dt?;
    let date = chrono::NaiveDate::from_ymd_opt(dt.year() as i32, dt.month() as u32, dt.day() as u32)?;
    let naive = date.and_hms_opt(dt.hour() as u32, dt.minute() as u32, dt.second() as u32)?;
    naive.and_local_timezone(chrono::Local).earliest().map(|t| t.timestamp())
}

pub fn index(archive: &mut Archive) -> Vec<ZipItem> {
    let mut out = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let Ok(file) = archive.by_index_raw(i) else { continue };
        let Ok(name) = file.name() else { continue };
        let raw = name.replace('\\', "/");
        let path = raw.trim_matches('/').to_owned();
        if path.is_empty() || path.split('/').any(|part| part == "..") {
            continue;
        }
        out.push(ZipItem {
            index: i,
            path,
            is_dir: file.is_dir() || raw.ends_with('/'),
            size: file.size(),
            modified: zip_time(file.last_modified()),
        });
    }
    out
}

/// Direkte Kinder von `inner` aus dem Inhaltsverzeichnis eines Archivs (gleich welchen Formats).
/// Ordner ohne eigenen Archiv-Eintrag werden ergänzt.
pub fn list_items(items: &[ZipItem], inner: &str) -> Result<Listing, String> {
    let prefix = if inner.is_empty() { String::new() } else { format!("{inner}/") };
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut entries: Vec<Entry> = Vec::new();
    // Ordner, unter denen im Archiv etwas liegt
    let mut filled: std::collections::HashSet<String> = std::collections::HashSet::new();
    for item in items {
        let Some(rest) = item.path.strip_prefix(prefix.as_str()) else { continue };
        if rest.is_empty() {
            continue;
        }
        let (name, deeper) = match rest.find('/') {
            Some(pos) => (&rest[..pos], true),
            None => (rest, false),
        };
        let is_dir = deeper || item.is_dir;
        if deeper {
            filled.insert(name.to_owned());
        }
        match seen.get(name) {
            Some(&idx) => {
                if !deeper && !item.is_dir {
                    // Datei und gleichnamiger Ordner: Datei gewinnt nicht, Ordner bleibt.
                    continue;
                }
                if !deeper && entries[idx].modified.is_none() {
                    entries[idx].modified = item.modified;
                }
            }
            None => {
                let e = Entry::new(
                    name.to_owned(),
                    is_dir,
                    if is_dir { 0 } else { item.size },
                    if deeper { None } else { item.modified },
                );
                seen.insert(name.to_owned(), entries.len());
                entries.push(e);
            }
        }
    }
    if !inner.is_empty() && entries.is_empty() && !items.iter().any(|i| i.path == inner) {
        return Err(t("Dieser Ordner existiert im Archiv nicht.", "This folder does not exist in the archive.").to_owned());
    }
    let empty_dirs = entries.iter().filter(|e| e.is_dir && !filled.contains(&e.name)).map(|e| e.name.clone()).collect();
    Ok(Listing { entries, space: None, empty_dirs: Some(empty_dirs), pending: false, dir_modified: None })
}
