//! Tests der Dateioperationen gegen echte temporäre Ordner.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::ops::{Answer, Jobs, Request, Source};
use crate::vfs::{Bus, Location, Msg};

pub fn test_bus() -> (Bus, mpsc::Receiver<Msg>) {
    let (tx, rx) = mpsc::channel();
    (Bus::new(tx, eframe::egui::Context::default()), rx)
}

pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tc-test-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Führt eine Aufgabe bis zum Ende aus und beantwortet Konflikte mit `answer`.
pub fn run(req: Request, answer: Option<Answer>) -> Vec<String> {
    run_recorded(req, answer).0
}

/// Wie `run`; liefert zusätzlich, was die Aufgabe zum Zurücknehmen festgehalten hat.
pub fn run_recorded(req: Request, answer: Option<Answer>) -> (Vec<String>, Vec<crate::ops::Undo>) {
    let (bus, _rx) = test_bus();
    let mut jobs = Jobs::default();
    let id = jobs.start(req, &bus);
    let start = Instant::now();
    loop {
        let job = jobs.get(id).unwrap();
        if job.shared.finished.load(Relaxed) {
            return (job.shared.errors.lock().unwrap().clone(), job.shared.undo.lock().unwrap().clone());
        }
        if jobs.pending_conflict().is_some() {
            job.shared.respond(answer.expect("unerwarteter Konflikt"));
        }
        assert!(start.elapsed() < Duration::from_secs(60), "Aufgabe hängt");
        std::thread::sleep(Duration::from_millis(3));
    }
}

pub fn source(dir: &Path, name: &str) -> Source {
    let meta = fs::metadata(dir.join(name)).unwrap();
    Source { dir: Location::Local(dir.to_path_buf()), name: name.to_owned(), is_dir: meta.is_dir(), size: meta.len() }
}

/// Legt einen kleinen Baum an: a.txt, groß.bin (1,5 MB), sub/b.txt, sub/tief/c.txt, leer/
pub fn make_tree(root: &Path) {
    fs::create_dir_all(root.join("sub/tief")).unwrap();
    fs::create_dir_all(root.join("leer")).unwrap();
    fs::write(root.join("a.txt"), "Hallo Welt äöü").unwrap();
    let big: Vec<u8> = (0..1_500_000u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect();
    fs::write(root.join("groß.bin"), big).unwrap();
    fs::write(root.join("sub/b.txt"), "b").unwrap();
    fs::write(root.join("sub/tief/c.txt"), "ccc").unwrap();
}

pub fn assert_same_tree(a: &Path, b: &Path) {
    for name in ["a.txt", "groß.bin", "sub/b.txt", "sub/tief/c.txt"] {
        assert_eq!(fs::read(a.join(name)).unwrap(), fs::read(b.join(name)).unwrap(), "{name} weicht ab");
    }
    assert!(b.join("leer").is_dir(), "leerer Ordner fehlt");
}

#[test]
fn copy_tree_keeps_content_and_times() {
    let base = temp_dir("copy");
    make_tree(&base.join("quelle"));
    fs::create_dir_all(base.join("ziel")).unwrap();
    let errors = run(
        Request::Transfer { sources: vec![source(&base, "quelle")], dest: Location::Local(base.join("ziel")), mv: false },
        None,
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_same_tree(&base.join("quelle"), &base.join("ziel/quelle"));
    let t1 = fs::metadata(base.join("quelle/a.txt")).unwrap().modified().unwrap();
    let t2 = fs::metadata(base.join("ziel/quelle/a.txt")).unwrap().modified().unwrap();
    assert_eq!(t1, t2, "Änderungsdatum ging verloren");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn move_removes_source() {
    let base = temp_dir("move");
    make_tree(&base.join("quelle"));
    fs::create_dir_all(base.join("ziel")).unwrap();
    let errors = run(
        Request::Transfer { sources: vec![source(&base, "quelle")], dest: Location::Local(base.join("ziel")), mv: true },
        None,
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!base.join("quelle").exists());
    assert!(base.join("ziel/quelle/sub/tief/c.txt").is_file());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn move_onto_existing_merges_and_cleans_up() {
    // Ziel existiert schon: kein schnelles Umbenennen möglich, also Datei für Datei.
    let base = temp_dir("merge");
    make_tree(&base.join("quelle"));
    fs::create_dir_all(base.join("ziel/quelle/sub")).unwrap();
    fs::write(base.join("ziel/quelle/a.txt"), "alt").unwrap();
    fs::write(base.join("ziel/quelle/sub/nur-im-ziel.txt"), "bleibt").unwrap();
    let errors = run(
        Request::Transfer { sources: vec![source(&base, "quelle")], dest: Location::Local(base.join("ziel")), mv: true },
        Some(Answer::Overwrite),
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(fs::read_to_string(base.join("ziel/quelle/a.txt")).unwrap(), "Hallo Welt äöü");
    assert_eq!(fs::read_to_string(base.join("ziel/quelle/sub/nur-im-ziel.txt")).unwrap(), "bleibt");
    assert!(!base.join("quelle").exists(), "Quelle wurde nicht aufgeräumt");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn conflict_skip_keeps_both_sides() {
    let base = temp_dir("skip");
    fs::create_dir_all(base.join("von")).unwrap();
    fs::create_dir_all(base.join("nach")).unwrap();
    fs::write(base.join("von/x.txt"), "neu").unwrap();
    fs::write(base.join("nach/x.txt"), "alt").unwrap();
    let errors = run(
        Request::Transfer { sources: vec![source(&base.join("von"), "x.txt")], dest: Location::Local(base.join("nach")), mv: true },
        Some(Answer::Skip),
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(fs::read_to_string(base.join("nach/x.txt")).unwrap(), "alt");
    assert!(base.join("von/x.txt").exists(), "übersprungene Quelle darf beim Verschieben nicht verschwinden");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn conflict_keep_both_makes_numbered_copy() {
    let base = temp_dir("both");
    fs::create_dir_all(base.join("von")).unwrap();
    fs::create_dir_all(base.join("nach")).unwrap();
    fs::write(base.join("von/x.txt"), "neu").unwrap();
    fs::write(base.join("nach/x.txt"), "alt").unwrap();
    let errors = run(
        Request::Transfer { sources: vec![source(&base.join("von"), "x.txt")], dest: Location::Local(base.join("nach")), mv: false },
        Some(Answer::KeepBoth),
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(fs::read_to_string(base.join("nach/x.txt")).unwrap(), "alt");
    assert_eq!(fs::read_to_string(base.join("nach/x (2).txt")).unwrap(), "neu");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn copy_within_same_folder_duplicates() {
    let base = temp_dir("dup");
    make_tree(&base.join("ordner"));
    fs::write(base.join("datei.txt"), "inhalt").unwrap();
    let errors = run(
        Request::Transfer {
            sources: vec![source(&base, "ordner"), source(&base, "datei.txt")],
            dest: Location::Local(base.clone()),
            mv: false,
        },
        None,
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(fs::read_to_string(base.join("datei (2).txt")).unwrap(), "inhalt");
    assert_same_tree(&base.join("ordner"), &base.join("ordner (2)"));
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn copy_into_itself_is_refused() {
    let base = temp_dir("self");
    make_tree(&base.join("ordner"));
    let errors = run(
        Request::Transfer { sources: vec![source(&base, "ordner")], dest: Location::Local(base.join("ordner/sub")), mv: false },
        None,
    );
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("innerhalb"), "{errors:?}");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn delete_permanently() {
    let base = temp_dir("delete");
    make_tree(&base.join("ordner"));
    fs::write(base.join("datei.txt"), "x").unwrap();
    fs::write(base.join("bleibt.txt"), "x").unwrap();
    let errors = run(
        Request::Delete { sources: vec![source(&base, "ordner"), source(&base, "datei.txt")], permanent: true },
        None,
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!base.join("ordner").exists());
    assert!(!base.join("datei.txt").exists());
    assert!(base.join("bleibt.txt").exists());
    let _ = fs::remove_dir_all(&base);
}

/// Suchtreffer liegen in verschiedenen Ordnern – auch gleichnamige.
#[test]
fn delete_takes_each_item_from_its_own_folder() {
    let base = temp_dir("delete-folders");
    let (a, b) = (base.join("a"), base.join("b"));
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(a.join("eins.jpg"), "x").unwrap();
    fs::write(a.join("zwei.jpg"), "bleibt").unwrap();
    fs::write(b.join("zwei.jpg"), "weg").unwrap();
    let sources = vec![source(&a, "eins.jpg"), source(&b, "zwei.jpg")];
    let errors = run(Request::Delete { sources, permanent: true }, None);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!a.join("eins.jpg").exists() && !b.join("zwei.jpg").exists());
    assert_eq!(fs::read_to_string(a.join("zwei.jpg")).unwrap(), "bleibt");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn pack_browse_and_extract() {
    let base = temp_dir("zip");
    make_tree(&base.join("ordner"));
    fs::write(base.join("einzeln.txt"), "solo").unwrap();
    let archive = base.join("paket.zip");
    let errors = run(
        Request::Pack { sources: vec![base.join("ordner"), base.join("einzeln.txt")], target: archive.clone() },
        None,
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert!(archive.is_file());
    assert!(!base.join("paket.zip.teil").exists(), "Zwischendatei blieb liegen");

    // Als Ordner lesen
    let top = crate::archive::list(&archive, "").unwrap();
    let mut names: Vec<_> = top.entries.iter().map(|e| (e.name.clone(), e.is_dir)).collect();
    names.sort();
    assert_eq!(names, [("einzeln.txt".to_owned(), false), ("ordner".to_owned(), true)]);
    let sub = crate::archive::list(&archive, "ordner/sub").unwrap();
    let mut names: Vec<_> = sub.entries.iter().map(|e| e.name.clone()).collect();
    names.sort();
    assert_eq!(names, ["b.txt", "tief"]);
    assert!(crate::archive::list(&archive, "gibt/es/nicht").is_err());

    // Entpacken: ganzer Ordner und einzelne Datei aus einem Unterordner
    fs::create_dir_all(base.join("raus")).unwrap();
    let root = Location::Zip { archive: archive.clone(), inner: String::new() };
    let deep = Location::Zip { archive: archive.clone(), inner: "ordner/sub/tief".to_owned() };
    let errors = run(
        Request::Transfer {
            sources: vec![Source { dir: root, name: "ordner".to_owned(), is_dir: true, size: 0 }],
            dest: Location::Local(base.join("raus")),
            mv: false,
        },
        None,
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_same_tree(&base.join("ordner"), &base.join("raus/ordner"));
    let errors = run(
        Request::Transfer {
            sources: vec![Source { dir: deep, name: "c.txt".to_owned(), is_dir: false, size: 3 }],
            dest: Location::Local(base.join("raus")),
            mv: false,
        },
        None,
    );
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(fs::read_to_string(base.join("raus/c.txt")).unwrap(), "ccc");
    let _ = fs::remove_dir_all(&base);
}

/// Sammelt die Meldungen der Hintergrundprüfung, bis sie fertig ist.
fn empty_dirs_of(dir: &Path, show_hidden: bool) -> Vec<String> {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    let (bus, rx) = test_bus();
    let mut dirs: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    crate::vfs::spawn_empty_check(1, Location::Local(dir.to_path_buf()), dirs, show_hidden, bus, Arc::new(AtomicBool::new(false)));
    let mut found = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(10)).expect("Prüfung meldet sich nicht") {
            Msg::EmptyDirs { names, done, .. } => {
                found.extend(names);
                if done {
                    found.sort();
                    return found;
                }
            }
            _ => {}
        }
    }
}

#[test]
fn empty_folders_are_detected() {
    let base = temp_dir("empty");
    fs::create_dir_all(base.join("leer")).unwrap();
    fs::create_dir_all(base.join("voll")).unwrap();
    fs::write(base.join("voll/datei.txt"), "x").unwrap();
    fs::create_dir_all(base.join("nur-unterordner/innen")).unwrap();
    fs::create_dir_all(base.join("nur-versteckt")).unwrap();
    let hidden = base.join("nur-versteckt/desktop.ini");
    fs::write(&hidden, "x").unwrap();
    std::process::Command::new("attrib").arg("+h").arg(&hidden).status().unwrap();

    // Versteckte Dateien werden nicht angezeigt: der Ordner wirkt leer.
    assert_eq!(empty_dirs_of(&base, false), ["leer", "nur-versteckt"]);
    // Werden sie angezeigt, ist er es nicht.
    assert_eq!(empty_dirs_of(&base, true), ["leer"]);
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn empty_folders_inside_zip_are_known() {
    let base = temp_dir("zipempty");
    make_tree(&base.join("ordner"));
    let archive = base.join("paket.zip");
    let errors = run(Request::Pack { sources: vec![base.join("ordner")], target: archive.clone() }, None);
    assert!(errors.is_empty(), "{errors:?}");
    let inside = crate::archive::list(&archive, "ordner").unwrap();
    assert_eq!(inside.empty_dirs, Some(vec!["leer".to_owned()]));
    let top = crate::archive::list(&archive, "").unwrap();
    assert_eq!(top.empty_dirs, Some(Vec::new()));
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn writing_into_zip_is_refused_clearly() {
    let base = temp_dir("zipro");
    fs::write(base.join("a.txt"), "a").unwrap();
    let errors = run(
        Request::Pack { sources: vec![base.join("a.txt")], target: base.join("z.zip") },
        None,
    );
    assert!(errors.is_empty(), "{errors:?}");
    let errors = run(
        Request::Transfer {
            sources: vec![source(&base, "a.txt")],
            dest: Location::Zip { archive: base.join("z.zip"), inner: String::new() },
            mv: false,
        },
        None,
    );
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("ZIP"), "{errors:?}");
    let _ = fs::remove_dir_all(&base);
}

/// Räumt aus dem Papierkorb, was ein Test unterhalb von `base` hineingelegt hat.
fn purge_from_bin(base: &Path) {
    let Ok(items) = trash::os_limited::list() else { return };
    let ours: Vec<_> = items.into_iter().filter(|item| item.original_parent.starts_with(base)).collect();
    if !ours.is_empty() {
        let _ = trash::os_limited::purge_all(ours);
    }
}

#[test]
fn copy_can_be_undone_without_touching_older_files() {
    use crate::ops::Undo;
    let base = temp_dir("undo-copy");
    make_tree(&base.join("quelle"));
    // Im Ziel gibt es den Ordner schon, mit einer Datei, die das Kopieren ersetzt, und einer eigenen.
    fs::create_dir_all(base.join("ziel/quelle")).unwrap();
    fs::write(base.join("ziel/quelle/a.txt"), "alt").unwrap();
    fs::write(base.join("ziel/quelle/eigen.txt"), "bleibt").unwrap();
    let dest = Location::Local(base.join("ziel"));
    let copy = Request::Transfer { sources: vec![source(&base, "quelle")], dest: dest.clone(), mv: false };
    let (errors, records) = run_recorded(copy, Some(Answer::OverwriteAll));
    assert!(errors.is_empty(), "{errors:?}");
    let [Undo::Copied { files, dirs, replaced, .. }] = records.as_slice() else { panic!("Mitschrift: {records:?}") };
    let mut names: Vec<&str> = files.iter().map(|(rel, _)| rel.as_str()).collect();
    names.sort();
    assert_eq!(names, ["quelle/groß.bin", "quelle/sub/b.txt", "quelle/sub/tief/c.txt"]);
    assert_eq!(dirs, &["quelle/leer", "quelle/sub", "quelle/sub/tief"].map(String::from).to_vec());
    assert_eq!(*replaced, 1);
    assert!(records[0].describe().len() == 2);

    // Eine kopierte Datei wird danach bearbeitet: sie bleibt beim Zurücknehmen liegen.
    fs::write(base.join("ziel/quelle/sub/b.txt"), "inzwischen bearbeitet").unwrap();
    let errors = run(Request::Undo(records), None);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(!base.join("ziel/quelle/groß.bin").exists());
    assert!(!base.join("ziel/quelle/sub/tief").exists(), "leer gewordene, neu angelegte Ordner verschwinden");
    assert!(!base.join("ziel/quelle/leer").exists());
    assert_eq!(fs::read_to_string(base.join("ziel/quelle/sub/b.txt")).unwrap(), "inzwischen bearbeitet");
    assert_eq!(fs::read_to_string(base.join("ziel/quelle/a.txt")).unwrap(), "Hallo Welt äöü", "Ersetztes bleibt, wie es ist");
    assert_eq!(fs::read_to_string(base.join("ziel/quelle/eigen.txt")).unwrap(), "bleibt");
    assert!(base.join("quelle/groß.bin").is_file(), "die Quelle bleibt unberührt");
    purge_from_bin(&base);
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn move_can_be_undone() {
    use crate::ops::Undo;
    let base = temp_dir("undo-move");
    make_tree(&base.join("quelle"));
    fs::create_dir_all(base.join("ziel")).unwrap();
    let (here, dest) = (Location::Local(base.clone()), Location::Local(base.join("ziel")));

    // Ganzer Ordner auf demselben Laufwerk: wird umgehängt und wieder zurückgehängt.
    let (errors, records) = run_recorded(Request::Transfer { sources: vec![source(&base, "quelle")], dest: dest.clone(), mv: true }, None);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(records, [Undo::Moved { from: here.clone(), to: dest.clone(), files: vec![], dirs: vec![], whole: vec!["quelle".into()], replaced: 0 }]);
    assert!(!base.join("quelle").exists());
    let errors = run(Request::Undo(records), None);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(!base.join("ziel/quelle").exists());
    assert!(base.join("quelle/sub/tief/c.txt").is_file());

    // In einen vorhandenen Ordner hinein: Datei für Datei, eine davon ersetzt eine vorhandene.
    fs::create_dir_all(base.join("ziel/quelle/sub")).unwrap();
    fs::write(base.join("ziel/quelle/a.txt"), "alt").unwrap();
    fs::write(base.join("ziel/quelle/eigen.txt"), "bleibt").unwrap();
    let merge = Request::Transfer { sources: vec![source(&base, "quelle")], dest: dest.clone(), mv: true };
    let (errors, records) = run_recorded(merge, Some(Answer::OverwriteAll));
    assert!(errors.is_empty(), "{errors:?}");
    let [Undo::Moved { files, dirs, whole, replaced, .. }] = records.as_slice() else { panic!("Mitschrift: {records:?}") };
    assert_eq!((files.len(), whole.len(), *replaced), (4, 0, 1));
    assert_eq!(dirs, &["quelle/leer", "quelle/sub/tief"].map(String::from).to_vec());
    assert!(!base.join("quelle").exists());
    let errors = run(Request::Undo(records), None);
    assert!(errors.is_empty(), "{errors:?}");
    for name in ["a.txt", "groß.bin", "sub/b.txt", "sub/tief/c.txt"] {
        assert!(base.join("quelle").join(name).is_file(), "{name} ist nicht zurück");
        assert!(!base.join("ziel/quelle").join(name).exists(), "{name} liegt noch im Ziel");
    }
    assert_eq!(fs::read_to_string(base.join("ziel/quelle/eigen.txt")).unwrap(), "bleibt");
    assert!(base.join("ziel/quelle/sub").is_dir(), "der Ordner war schon vorher da");
    assert!(!base.join("ziel/quelle/sub/tief").exists());

    // Liegt am alten Ort inzwischen etwas Gleichnamiges, wird nichts überschrieben.
    let (_, records) = run_recorded(Request::Transfer { sources: vec![source(&base.join("quelle"), "a.txt")], dest: dest.clone(), mv: true }, None);
    fs::write(base.join("quelle/a.txt"), "neu angelegt").unwrap();
    let errors = run(Request::Undo(records), None);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(fs::read_to_string(base.join("quelle/a.txt")).unwrap(), "neu angelegt");
    assert!(base.join("ziel/a.txt").is_file());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn deleting_to_the_recycle_bin_can_be_undone() {
    use crate::ops::Undo;
    let base = temp_dir("undo-papierkorb");
    make_tree(&base.join("weg"));
    fs::write(base.join("einzeln.txt"), "einzeln").unwrap();
    let delete = Request::Delete { sources: vec![source(&base, "weg"), source(&base, "einzeln.txt")], permanent: false };
    let (errors, records) = run_recorded(delete, None);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(matches!(records.as_slice(), [Undo::Trashed { paths, .. }] if paths.len() == 2));
    assert!(!base.join("weg").exists() && !base.join("einzeln.txt").exists());
    let errors = run(Request::Undo(records.clone()), None);
    assert!(errors.is_empty(), "{errors:?}");
    assert!(base.join("weg/sub/tief/c.txt").is_file());
    assert_eq!(fs::read_to_string(base.join("einzeln.txt")).unwrap(), "einzeln");
    // Ein zweites Mal gibt es nichts mehr zu holen.
    let errors = run(Request::Undo(records), None);
    assert_eq!(errors.len(), 2, "{errors:?}");
    // Endgültiges Löschen hinterlässt nichts zum Zurücknehmen.
    let (_, records) = run_recorded(Request::Delete { sources: vec![source(&base, "einzeln.txt")], permanent: true }, None);
    assert!(records.is_empty());
    purge_from_bin(&base);
    let _ = fs::remove_dir_all(&base);
}

/// Wartet auf das Ende eines Auftrags, nimmt ihn aus der Liste und liefert seine Fehler.
fn wait_finished(jobs: &mut Jobs, id: u64) -> Vec<String> {
    let start = Instant::now();
    let mut asked: Option<Instant> = None;
    loop {
        if jobs.get(id).expect("Auftrag fehlt").shared.finished.load(Relaxed) {
            return jobs.remove(id).unwrap().shared.errors.lock().unwrap().clone();
        }
        // Eine gerade beantwortete Rückfrage verschwindet erst einen Augenblick später; bleibt sie stehen, ist es eine neue.
        if jobs.pending_conflict().is_none() {
            asked = None;
        } else if asked.get_or_insert_with(Instant::now).elapsed() > Duration::from_secs(2) {
            panic!("unerwartete Rückfrage");
        }
        assert!(start.elapsed() < Duration::from_secs(60), "Aufgabe hängt");
        std::thread::sleep(Duration::from_millis(3));
    }
}

/// Wartet, bis ein Auftrag mit einer Rückfrage stehen bleibt: dann läuft er sicher noch.
fn wait_for_question(jobs: &Jobs) {
    let start = Instant::now();
    while jobs.pending_conflict().is_none() {
        assert!(start.elapsed() < Duration::from_secs(30), "keine Rückfrage");
        std::thread::sleep(Duration::from_millis(3));
    }
}

#[test]
fn transfers_on_the_same_drive_wait_their_turn() {
    let base = temp_dir("schlange");
    for name in ["eins", "zwei", "drei", "vier", "fuenf"] {
        make_tree(&base.join(name));
    }
    fs::create_dir_all(base.join("ziel/eins")).unwrap();
    fs::create_dir_all(base.join("anderswo")).unwrap();
    fs::write(base.join("ziel/eins/a.txt"), "schon da").unwrap();
    fs::write(base.join("brief.txt"), "brief").unwrap();
    let dest = Location::Local(base.join("ziel"));
    let (bus, _rx) = test_bus();
    let mut jobs = Jobs::default();
    let copy = |name: &str| Request::Transfer { sources: vec![source(&base, name)], dest: dest.clone(), mv: false };

    // Der erste Auftrag bleibt an einer Rückfrage stehen; die nächsten müssen warten.
    let first = jobs.start(copy("eins"), &bus);
    wait_for_question(&jobs);
    let second = jobs.start(copy("zwei"), &bus);
    let third = jobs.start(copy("drei"), &bus);
    let fourth = jobs.start(copy("vier"), &bus);
    assert!(!jobs.get(first).unwrap().waiting);
    assert!([second, third, fourth].iter().all(|id| jobs.get(*id).unwrap().waiting));

    // Verschieben auf demselben Laufwerk ist nur ein Umhängen und wartet nicht.
    let hop = Request::Transfer { sources: vec![source(&base, "brief.txt")], dest: Location::Local(base.join("anderswo")), mv: true };
    let hop = jobs.start(hop, &bus);
    assert!(!jobs.get(hop).unwrap().waiting);
    let start = Instant::now();
    while !jobs.get(hop).unwrap().shared.finished.load(Relaxed) {
        assert!(start.elapsed() < Duration::from_secs(30));
        std::thread::sleep(Duration::from_millis(3));
    }
    jobs.remove(hop);
    assert!(base.join("anderswo/brief.txt").is_file());

    // Der erste geht weiter; danach kommen die anderen der Reihe nach an die Reihe.
    jobs.get(first).unwrap().shared.respond(Answer::OverwriteAll);
    assert!(wait_finished(&mut jobs, first).is_empty());
    jobs.pump(&bus);
    assert!(!jobs.get(second).unwrap().waiting && jobs.get(third).unwrap().waiting);
    assert!(wait_finished(&mut jobs, second).is_empty());
    jobs.pump(&bus);
    assert!(wait_finished(&mut jobs, third).is_empty());
    jobs.pump(&bus);
    assert!(wait_finished(&mut jobs, fourth).is_empty());
    for name in ["eins", "zwei", "drei", "vier"] {
        assert_same_tree(&base.join(name), &base.join("ziel").join(name));
    }

    // Ohne Warteschlange startet alles sofort.
    jobs.serial = false;
    fs::write(base.join("ziel/zwei/a.txt"), "x").unwrap();
    let blocker = jobs.start(copy("zwei"), &bus);
    wait_for_question(&jobs);
    let parallel = jobs.start(copy("fuenf"), &bus);
    assert!(!jobs.get(parallel).unwrap().waiting);
    assert!(wait_finished(&mut jobs, parallel).is_empty());
    jobs.get(blocker).unwrap().shared.respond(Answer::SkipAll);
    wait_finished(&mut jobs, blocker);
    let _ = fs::remove_dir_all(&base);
}
