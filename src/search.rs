//! Rekursive Dateisuche nach dem Namen in lokalen Ordnern, läuft im Hintergrund und liefert Treffer schubweise.
//! (Die Suche in Dateiinhalten, nach Größe und nach Änderungsdatum gibt es in der Vollversion.)

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::{Duration, Instant};

use crate::util::{NameFilter, system_time_secs};
use crate::vfs::{Bus, Entry, Msg};

const MAX_HITS: usize = 200_000;

pub struct Params {
    pub tab: u64,
    pub search: u64,
    pub root: PathBuf,
    /// Namensfilter; leer heißt: jeder Name
    pub query: String,
    pub show_hidden: bool,
}

pub fn spawn(params: Params, bus: Bus, cancel: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let (tab, search) = (params.tab, params.search);
        run(&params.root, &params.query, params.show_hidden, &cancel, &mut |hits, checked| {
            bus.send(Msg::SearchHits { tab, search, hits, checked });
        });
        if !cancel.load(Relaxed) {
            bus.send(Msg::SearchDone { tab, search });
        }
    });
}

/// Durchläuft `root` samt Unterordnern und meldet Treffer schubweise an `emit` – zusammen mit der Zahl der bisher
/// geprüften Dateien.
pub(crate) fn run(root: &Path, query: &str, show_hidden: bool, cancel: &AtomicBool, emit: &mut dyn FnMut(Vec<Entry>, u64)) {
    use std::os::windows::fs::MetadataExt;
    let filter = NameFilter::new(query);
    let mut batch: Vec<Entry> = Vec::new();
    let mut last = Instant::now();
    let total = std::cell::Cell::new(0usize);
    let (mut checked, mut reported) = (0u64, 0u64);
    let mut flush = |batch: &mut Vec<Entry>, checked: u64, force: bool| {
        let due = force || last.elapsed() > Duration::from_millis(120) || batch.len() >= 2000;
        if due && (!batch.is_empty() || checked != reported) {
            last = Instant::now();
            reported = checked;
            total.set(total.get() + batch.len());
            emit(std::mem::take(batch), checked);
        }
    };

    let mut stack: Vec<(PathBuf, Arc<str>)> = vec![(root.to_path_buf(), Arc::from(""))];
    'walk: while let Some((dir, rel)) = stack.pop() {
        if cancel.load(Relaxed) {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for item in rd.flatten() {
            let Ok(meta) = item.metadata() else { continue };
            let attrs = meta.file_attributes();
            let hidden = attrs & 0x2 != 0;
            if hidden && !show_hidden {
                continue;
            }
            let is_dir = attrs & 0x10 != 0;
            let name = item.file_name().to_string_lossy().into_owned();
            let mut e = Entry::new(name, is_dir, if is_dir { 0 } else { meta.len() }, meta.modified().ok().map(system_time_secs));
            if is_dir && attrs & 0x400 == 0 {
                let child: Arc<str> =
                    if rel.is_empty() { Arc::from(e.name.as_str()) } else { Arc::from(format!("{rel}\\{}", e.name)) };
                stack.push((item.path(), child));
            }
            if !is_dir {
                checked += 1;
            }
            if !filter.matches(&e.key) {
                continue;
            }
            e.hidden = hidden;
            e.note = Some(rel.clone());
            batch.push(e);
            if total.get() + batch.len() >= MAX_HITS {
                break 'walk;
            }
        }
        flush(&mut batch, checked, false);
    }
    if !cancel.load(Relaxed) {
        flush(&mut batch, checked, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tc-suche-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(root: &Path, query: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        run(root, query, false, &AtomicBool::new(false), &mut |hits, _| {
            out.extend(hits.into_iter().map(|e| match e.note.as_deref() {
                Some("") | None => e.name,
                Some(rel) => format!("{rel}\\{}", e.name),
            }));
        });
        out.sort();
        out
    }

    #[test]
    fn finds_names_in_subfolders() {
        let dir = temp("namen");
        std::fs::create_dir_all(dir.join("a\\b")).unwrap();
        std::fs::write(dir.join("Bericht.txt"), "x").unwrap();
        std::fs::write(dir.join("a\\b\\bericht-alt.doc"), "x").unwrap();
        std::fs::write(dir.join("a\\anderes.txt"), "x").unwrap();
        assert_eq!(names(&dir, "bericht"), ["Bericht.txt", "a\\b\\bericht-alt.doc"]);
        assert_eq!(names(&dir, "").len(), 5, "ohne Suchtext alles, auch die Ordner");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stops_when_cancelled() {
        let dir = temp("abbruch");
        for i in 0..50 {
            std::fs::write(dir.join(format!("{i}.txt")), "nadel").unwrap();
        }
        let cancel = AtomicBool::new(true);
        let mut hits = 0;
        run(&dir, "", false, &cancel, &mut |h, _| hits += h.len());
        assert_eq!(hits, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
