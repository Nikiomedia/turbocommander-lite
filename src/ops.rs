//! Dateioperationen im Hintergrund: Kopieren, Verschieben, Löschen, Packen – zwischen
//! lokalen Ordnern und (lesend) ZIP-Archiven. Mit Fortschritt, Abbruch und Konfliktabfrage.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::lang::t;
use crate::tf;
use crate::util::split_ext;
use crate::vfs::{Bus, Location, Msg, io_msg};
use crate::{win, zipfs};

const CANCELLED: &str = "Abgebrochen";

#[derive(Clone, Debug)]
pub struct Source {
    /// Ordner, in dem das Element liegt
    pub dir: Location,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
}

impl Source {
    pub fn location(&self) -> Location {
        self.dir.child(&self.name)
    }
}

#[derive(Clone)]
pub enum Request {
    Transfer { sources: Vec<Source>, dest: Location, mv: bool },
    Delete { sources: Vec<Source>, permanent: bool },
    Pack { sources: Vec<PathBuf>, target: PathBuf },
    /// Holt eine Datei aus einem ZIP-Archiv in den Temp-Ordner und öffnet sie dort.
    OpenRemote { source: Source },
    /// Nimmt frühere Aufträge zurück, den letzten zuerst.
    Undo(Vec<Undo>),
}

/// Eine Datei, die ein Auftrag übertragen hat: wo sie herkam und wo sie jetzt liegt (Pfade relativ zu den
/// beiden Ordnern, mit `/`)
#[derive(Clone, Debug, PartialEq)]
pub struct Carried {
    pub from: String,
    pub to: String,
    pub size: u64,
    pub modified: Option<i64>,
}

/// Was ein Auftrag getan hat – genug, um es zurückzunehmen.
#[derive(Clone, Debug, PartialEq)]
pub enum Undo {
    /// Kopiert: diese Dateien (Pfad im Ziel, Größe) und Ordner sind dort neu entstanden.
    Copied { dest: Location, files: Vec<(String, u64)>, dirs: Vec<String>, replaced: usize },
    /// Verschoben von `from` nach `to`: einzelne Dateien, im Ziel neu angelegte Ordner und Einträge, die als
    /// Ganzes umgehängt wurden (gleicher Name an beiden Orten).
    Moved { from: Location, to: Location, files: Vec<Carried>, dirs: Vec<String>, whole: Vec<String>, replaced: usize },
    /// In den Papierkorb gelegt, nicht vor diesem Zeitpunkt (Sekunden)
    Trashed { paths: Vec<PathBuf>, after: i64 },
}

impl Undo {
    /// Wie viele Einträge das Zurücknehmen anfasst
    pub fn count(&self) -> usize {
        match self {
            Undo::Copied { files, .. } => files.len(),
            Undo::Moved { files, whole, .. } => files.len() + whole.len(),
            Undo::Trashed { paths, .. } => paths.len(),
        }
    }

    /// Sätze für die Rückfrage: was beim Zurücknehmen geschieht
    pub fn describe(&self) -> Vec<String> {
        let replaced_note = |n: usize, verb: (&str, &str)| {
            (n > 0).then(|| {
                let how = t(verb.0, verb.1);
                if n == 1 {
                    tf!(
                        "Eine Datei wurde dabei {how} – das lässt sich nicht zurücknehmen.",
                        "One file was {how} in the process – that cannot be undone."
                    )
                } else {
                    tf!(
                        "{n} Dateien wurden dabei {how} – das lässt sich nicht zurücknehmen.",
                        "{n} files were {how} in the process – that cannot be undone."
                    )
                }
            })
        };
        let mut out = Vec::new();
        match self {
            Undo::Copied { dest, files, replaced, .. } => {
                let how = if dest.is_local() { t("in den Papierkorb gelegt", "moved to the Recycle Bin") } else { t("vom Server gelöscht", "deleted from the server") };
                let (n, place) = (files.len(), dest.display());
                out.push(if n == 1 {
                    tf!("Die kopierte Datei in „{place}\" wird wieder entfernt ({how}).", "The copied file in “{place}” is removed again ({how}).")
                } else {
                    tf!("{n} kopierte Dateien in „{place}\" werden wieder entfernt ({how}).", "{n} copied files in “{place}” are removed again ({how}).")
                });
                out.extend(replaced_note(*replaced, ("überschrieben", "overwritten")));
            }
            Undo::Moved { from, files, whole, replaced, .. } => {
                let (n, place) = (files.len() + whole.len(), from.display());
                out.push(if n == 1 {
                    tf!("Der verschobene Eintrag kommt zurück nach „{place}\".", "The moved item returns to “{place}”.")
                } else {
                    tf!("{n} verschobene Einträge kommen zurück nach „{place}\".", "{n} moved items return to “{place}”.")
                });
                out.extend(replaced_note(*replaced, ("im Ziel überschrieben", "overwritten in the destination")));
            }
            Undo::Trashed { paths, .. } => out.push(if paths.len() == 1 {
                t("Der gelöschte Eintrag wird aus dem Papierkorb wiederhergestellt.", "The deleted item is restored from the Recycle Bin.").to_owned()
            } else {
                tf!(
                    "{} gelöschte Einträge werden aus dem Papierkorb wiederhergestellt.",
                    "{} deleted items are restored from the Recycle Bin.",
                    paths.len()
                )
            }),
        }
        out
    }

    /// Orte, deren Ansicht sich durch das Zurücknehmen ändert
    fn touched(&self) -> Vec<Location> {
        match self {
            Undo::Copied { dest, .. } => vec![dest.clone()],
            Undo::Moved { from, to, .. } => vec![from.clone(), to.clone()],
            Undo::Trashed { paths, .. } => paths.iter().filter_map(|p| p.parent()).map(|p| Location::Local(p.to_path_buf())).collect(),
        }
    }
}

/// Mitschrift eines laufenden Übertragungsauftrags
#[derive(Default)]
struct Trace {
    /// Übertragene Dateien; `true`: hat im Ziel eine vorhandene Datei ersetzt
    files: Vec<(Carried, bool)>,
    /// Im Ziel neu angelegte Ordner, Eltern vor Kindern
    dirs: Vec<String>,
    /// Beim Verschieben als Ganzes umgehängte Einträge
    whole: Vec<String>,
}

impl Trace {
    /// Hält fest, was sich zurücknehmen lässt – auch nach einem Abbruch für den erledigten Teil.
    fn record(self, from: &Location, to: &Location, mv: bool, shared: &JobShared) {
        if self.files.is_empty() && self.dirs.is_empty() && self.whole.is_empty() {
            return;
        }
        let replaced = self.files.iter().filter(|(_, replaced)| *replaced).count();
        let undo = if mv {
            let files = self.files.into_iter().map(|(file, _)| file).collect();
            Undo::Moved { from: from.clone(), to: to.clone(), files, dirs: self.dirs, whole: self.whole, replaced }
        } else {
            // Ersetzte Dateien bleiben, wie sie sind: ihr alter Inhalt ist nicht mehr da.
            let files = self.files.into_iter().filter(|(_, replaced)| !replaced).map(|(file, _)| (file.to, file.size)).collect();
            Undo::Copied { dest: to.clone(), files, dirs: self.dirs, replaced }
        };
        shared.undo.lock().unwrap().push(undo);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Overwrite,
    OverwriteAll,
    Skip,
    SkipAll,
    KeepBoth,
    Cancel,
}

#[derive(Clone, Debug)]
pub struct Conflict {
    pub name: String,
    pub dest: String,
    pub src_size: u64,
    pub dst_size: u64,
}

#[derive(Default)]
pub struct JobShared {
    pub total_bytes: AtomicU64,
    pub done_bytes: AtomicU64,
    pub total_items: AtomicU64,
    pub done_items: AtomicU64,
    pub scanning: AtomicBool,
    pub finished: AtomicBool,
    pub cancel: AtomicBool,
    /// Angehalten: der Auftrag wartet an der nächsten passenden Stelle, bis es weitergeht.
    pub paused: AtomicBool,
    /// Fortsetzung eines früheren Laufs: was im Ziel schon in voller Größe liegt, wird übersprungen.
    resume: AtomicBool,
    pub current: Mutex<String>,
    pub errors: Mutex<Vec<String>>,
    /// Dateien, an denen etwas gescheitert ist, womöglich weil ein anderes Programm sie offen hält
    suspects: Mutex<Vec<PathBuf>>,
    /// Was der Auftrag getan hat, soweit es sich zurücknehmen lässt
    pub undo: Mutex<Vec<Undo>>,
    pub conflict: Mutex<Option<Conflict>>,
    answer: Mutex<Option<Answer>>,
    cv: Condvar,
}

impl JobShared {
    pub fn fraction(&self) -> f32 {
        let total = self.total_bytes.load(Relaxed);
        if total > 0 {
            (self.done_bytes.load(Relaxed) as f64 / total as f64).min(1.0) as f32
        } else {
            let items = self.total_items.load(Relaxed);
            if items > 0 { (self.done_items.load(Relaxed) as f64 / items as f64).min(1.0) as f32 } else { 0.0 }
        }
    }

    pub fn respond(&self, answer: Answer) {
        *self.answer.lock().unwrap() = Some(answer);
        self.cv.notify_all();
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Relaxed)
    }

    /// Wartet, solange der Auftrag angehalten ist. Liefert `false`, wenn er abgebrochen wurde.
    fn keep_going(&self) -> bool {
        while self.paused.load(Relaxed) && !self.cancelled() {
            std::thread::sleep(Duration::from_millis(80));
        }
        !self.cancelled()
    }

    fn set_current(&self, text: &str) {
        *self.current.lock().unwrap() = text.to_owned();
    }

    fn error(&self, text: String) {
        self.errors.lock().unwrap().push(text);
    }

    /// Merkt sich eine Datei, an der etwas gescheitert ist. Am Ende des Auftrags wird nachgesehen, ob ein anderes
    /// Programm sie offen hält.
    fn suspect(&self, path: &Path) {
        let mut list = self.suspects.lock().unwrap();
        if list.len() < 64 && !list.iter().any(|known| known == path) {
            list.push(path.to_owned());
        }
    }

    /// Wie `suspect`, aber nur bei den Fehlern, die Windows für eine offene Datei meldet: „wird verwendet", „gesperrt"
    /// – und „Zugriff verweigert", wie es bei einem laufenden Programm für seine eigene Datei heißt.
    fn in_use(&self, path: &Path, e: &io::Error) {
        if matches!(e.raw_os_error(), Some(5 | 32 | 33)) {
            self.suspect(path);
        }
    }

    fn is_suspect(&self, path: &Path) -> bool {
        self.suspects.lock().unwrap().iter().any(|known| known == path)
    }

    fn ask(&self, conflict: Conflict, bus: &Bus) -> Answer {
        *self.conflict.lock().unwrap() = Some(conflict);
        bus.repaint();
        let answer = {
            let mut guard = self.answer.lock().unwrap();
            loop {
                if let Some(a) = guard.take() {
                    break a;
                }
                if self.cancelled() {
                    break Answer::Cancel;
                }
                guard = self.cv.wait_timeout(guard, Duration::from_millis(200)).unwrap().0;
            }
        };
        *self.conflict.lock().unwrap() = None;
        bus.repaint();
        answer
    }
}

pub struct Job {
    pub id: u64,
    pub title: String,
    pub shared: Arc<JobShared>,
    pub started: Instant,
    /// Orte, deren Ansicht nach Abschluss aktualisiert werden soll
    pub touched: Vec<Location>,
    /// Still im Hintergrund (kein Abschluss-Hinweis), z. B. „Datei zum Öffnen holen"
    pub quiet: bool,
    /// Wartet in der Schlange, bis eine andere Übertragung fertig ist
    pub waiting: bool,
    /// Der Auftrag selbst: für wartende Aufträge und um nach einem Abbruch fortzusetzen
    request: Request,
}

impl Job {
    /// Kopieren, Verschieben oder Abgleichen – das, was sich fortsetzen lässt
    pub fn is_transfer(&self) -> bool {
        self.request.is_transfer()
    }
}

/// Laufwerk oder Freigabe eines Pfads, klein geschrieben („c:", „\\server\freigabe")
fn volume(path: &Path) -> String {
    path.components().next().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).unwrap_or_default()
}

impl Request {
    pub fn is_transfer(&self) -> bool {
        matches!(self, Request::Transfer { .. })
    }

    /// Laufwerke und Server, die der Auftrag beansprucht: zwei Übertragungen auf demselben laufen besser nacheinander.
    fn resources(&self) -> Vec<String> {
        let of = |loc: &Location| match loc {
            Location::Local(path) => volume(path),
            Location::Zip { archive, .. } => volume(archive),
            _ => String::new(),
        };
        let mut out: Vec<String> = match self {
            Request::Transfer { sources, dest, .. } => sources.iter().map(|s| of(&s.dir)).chain([of(dest)]).collect(),
            _ => Vec::new(),
        };
        out.retain(|r| !r.is_empty());
        out.sort();
        out.dedup();
        out
    }

    /// Verschieben innerhalb eines Laufwerks ist nur ein Umhängen und braucht nicht zu warten.
    fn instant(&self) -> bool {
        match self {
            Request::Transfer { sources, dest: Location::Local(to), mv: true } => {
                sources.iter().all(|s| matches!(&s.dir, Location::Local(from) if volume(from) == volume(to)))
            }
            _ => false,
        }
    }
}

pub struct Jobs {
    /// Laufende und wartende Aufträge in der Reihenfolge, in der sie an die Reihe kommen
    pub list: Vec<Job>,
    next: u64,
    /// Übertragungen auf demselben Laufwerk nacheinander statt gleichzeitig
    pub serial: bool,
}

impl Default for Jobs {
    fn default() -> Self {
        Self { list: Vec::new(), next: 0, serial: true }
    }
}

impl Jobs {
    pub fn start(&mut self, req: Request, bus: &Bus) -> u64 {
        self.enqueue(req, false, bus)
    }

    /// `resume`: Fortsetzung eines abgebrochenen Auftrags – was im Ziel schon vollständig liegt, wird übersprungen.
    fn enqueue(&mut self, req: Request, resume: bool, bus: &Bus) -> u64 {
        self.next += 1;
        let id = self.next;
        let shared = Arc::new(JobShared::default());
        shared.scanning.store(true, Relaxed);
        shared.resume.store(resume, Relaxed);
        let count = |n: usize| if n == 1 { t("1 Element", "1 item").to_owned() } else { tf!("{n} Elemente", "{n} items") };
        let (title, touched, quiet) = match &req {
            Request::Transfer { sources, dest, mv } => {
                let mut touched = vec![dest.clone()];
                touched.extend(sources.iter().map(|s| s.dir.clone()));
                let verb = if *mv { t("Verschiebe", "Moving") } else { t("Kopiere", "Copying") };
                // Ein einzelner Eintrag wird beim Namen genannt: so lassen sich Aufträge in der Liste auseinanderhalten.
                let what = match sources.as_slice() {
                    [only] => tf!("„{}\"", "“{}”", only.name),
                    many => count(many.len()),
                };
                (tf!("{verb} {what} nach „{}\"", "{verb} {what} to “{}”", dest.leaf()), touched, false)
            }
            Request::Delete { sources, .. } => {
                (tf!("Lösche {}", "Deleting {}", count(sources.len())), sources.iter().map(|s| s.dir.clone()).collect(), false)
            }
            Request::Pack { sources, target } => {
                let touched = target.parent().map(|p| vec![Location::Local(p.to_path_buf())]).unwrap_or_default();
                (tf!("Packe {}", "Packing {}", count(sources.len())), touched, false)
            }
            Request::OpenRemote { source } => (tf!("Hole „{}\"", "Fetching “{}”", source.name), Vec::new(), true),
            Request::Undo(records) => {
                let total = records.iter().map(Undo::count).sum();
                let touched = records.iter().flat_map(Undo::touched).collect();
                (tf!("Nehme zurück: {}", "Undoing: {}", count(total)), touched, false)
            }
        };
        let mut touched = touched;
        touched.dedup();
        let job = Job { id, title, shared, started: Instant::now(), touched, quiet, waiting: true, request: req };
        // Warten muss nur, was einer unfertigen Übertragung in die Quere käme.
        let must_wait = self.serial && job.is_transfer() && !job.request.instant() && self.blocked(&job);
        self.list.push(job);
        if !must_wait {
            self.launch(id, bus);
        }
        id
    }

    /// Teilt sich der Auftrag ein Laufwerk oder einen Server mit einer Übertragung, die vor ihm an der Reihe ist?
    fn blocked(&self, job: &Job) -> bool {
        let wanted = job.request.resources();
        self.list
            .iter()
            .take_while(|other| other.id != job.id)
            .filter(|other| other.is_transfer() && !other.shared.finished.load(Relaxed))
            .any(|other| other.request.resources().iter().any(|r| wanted.contains(r)))
    }

    fn launch(&mut self, id: u64, bus: &Bus) {
        let Some(job) = self.list.iter_mut().find(|j| j.id == id && j.waiting) else { return };
        job.waiting = false;
        job.started = Instant::now();
        let (req, shared) = (job.request.clone(), job.shared.clone());
        let bus = bus.clone();
        std::thread::spawn(move || {
            let result = match req {
                Request::Transfer { sources, dest, mv } => transfer(sources, dest, mv, &shared, &bus),
                Request::Delete { sources, permanent } => delete(sources, permanent, &shared),
                Request::Pack { sources, target } => pack(sources, target, &shared),
                Request::OpenRemote { source } => open_remote(source, &shared, &bus),
                Request::Undo(records) => undo(records, &shared),
            };
            if let Err(e) = result {
                if e != CANCELLED {
                    shared.error(e);
                }
            }
            shared.scanning.store(false, Relaxed);
            shared.finished.store(true, Relaxed);
            bus.send(Msg::JobFinished { id });
        });
    }

    /// Startet wartende Aufträge, denen nichts mehr im Weg steht (nach dem Ende eines Auftrags aufrufen).
    pub fn pump(&mut self, bus: &Bus) {
        loop {
            let ready = self.list.iter().find(|job| job.waiting && !(self.serial && self.blocked(job))).map(|job| job.id);
            match ready {
                Some(id) => self.launch(id, bus),
                None => break,
            }
        }
    }

    pub fn running(&self) -> impl Iterator<Item = &Job> {
        self.list.iter().filter(|j| !j.shared.finished.load(Relaxed))
    }

    pub fn pending_conflict(&self) -> Option<(u64, Conflict)> {
        self.list.iter().find_map(|j| j.shared.conflict.lock().unwrap().clone().map(|c| (j.id, c)))
    }

    pub fn get(&self, id: u64) -> Option<&Job> {
        self.list.iter().find(|j| j.id == id)
    }

    pub fn remove(&mut self, id: u64) -> Option<Job> {
        let pos = self.list.iter().position(|j| j.id == id)?;
        Some(self.list.remove(pos))
    }
}
// ---------------------------------------------------------------- Fortschritt beim Lesen

struct ProgressReader<'a> {
    inner: &'a mut dyn Read,
    shared: &'a JobShared,
}

impl Read for ProgressReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.shared.keep_going() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, CANCELLED));
        }
        let n = self.inner.read(buf)?;
        self.shared.done_bytes.fetch_add(n as u64, Relaxed);
        Ok(n)
    }
}

/// `io::copy` bricht bei `Interrupted` nicht ab, sondern versucht es erneut – daher eigene Schleife.
fn pump(reader: &mut dyn Read, writer: &mut dyn Write) -> io::Result<u64> {
    let mut buf = vec![0u8; 256 * 1024];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            return Ok(total);
        }
        writer.write_all(&buf[..n])?;
        total += n as u64;
    }
}

fn err_text(e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::Interrupted { CANCELLED.to_owned() } else { io_msg(e) }
}

// ---------------------------------------------------------------- Quelle

enum ItemSrc {
    Local(PathBuf),
    Zip(usize),
}

struct Item {
    src: ItemSrc,
    /// Zielpfad relativ zum Zielordner, mit `/`
    rel: String,
    is_dir: bool,
    size: u64,
    modified: Option<i64>,
}

enum SrcFs {
    Local,
    Zip { archive: zipfs::Archive, items: Arc<Vec<zipfs::ZipItem>> },
}

impl SrcFs {
    fn open(dir: &Location) -> Result<Self, String> {
        match dir {
            Location::Local(_) => Ok(SrcFs::Local),
            Location::Zip { archive, .. } => {
                let mut a = zipfs::open(archive)?;
                let items = zipfs::index(&mut a);
                Ok(SrcFs::Zip { archive: a, items: Arc::new(items) })
            }
            Location::Drives
            | Location::Network { .. }
            | Location::RecycleBin
            | Location::Device { .. } => Err(t(
                "Einträge dieser Übersicht lassen sich nicht kopieren.",
                "Items in this overview cannot be copied.",
            )
            .to_owned()),
        }
    }

    /// Erweitert eine Quelle zu allen enthaltenen Ordnern und Dateien (Ordner vor Inhalt).
    fn plan(&mut self, source: &Source, out: &mut Vec<Item>, shared: &JobShared) -> Result<(), String> {
        let top = source.name.clone();
        let result = match (&mut *self, &source.dir) {
            (SrcFs::Local, Location::Local(dir)) => {
                let path = dir.join(&source.name);
                if !source.is_dir {
                    let meta = fs::metadata(&path).ok();
                    out.push(Item {
                        src: ItemSrc::Local(path),
                        rel: top,
                        is_dir: false,
                        size: meta.as_ref().map_or(source.size, |m| m.len()),
                        modified: meta.as_ref().and_then(|m| m.modified().ok()).map(crate::util::system_time_secs),
                    });
                    return Ok(());
                }
                out.push(Item { src: ItemSrc::Local(path.clone()), rel: top.clone(), is_dir: true, size: 0, modified: None });
                let mut stack = vec![(path, top)];
                while let Some((dir_path, rel)) = stack.pop() {
                    if shared.cancelled() {
                        return Err(CANCELLED.to_owned());
                    }
                    let Ok(rd) = fs::read_dir(&dir_path) else {
                        shared.error(tf!("{}: Ordner nicht lesbar", "{}: cannot read folder", dir_path.display()));
                        continue;
                    };
                    for e in rd.flatten() {
                        let Ok(meta) = e.metadata() else { continue };
                        let name = e.file_name().to_string_lossy().into_owned();
                        let child_rel = format!("{rel}/{name}");
                        use std::os::windows::fs::MetadataExt;
                        let attrs = meta.file_attributes();
                        let is_dir = attrs & 0x10 != 0;
                        let reparse = attrs & 0x400 != 0;
                        out.push(Item {
                            src: ItemSrc::Local(e.path()),
                            rel: child_rel.clone(),
                            is_dir,
                            size: if is_dir { 0 } else { meta.len() },
                            modified: meta.modified().ok().filter(|_| !is_dir).map(crate::util::system_time_secs),
                        });
                        // Verknüpfte Ordner (Junctions) nicht verfolgen: Gefahr von Endlosschleifen.
                        if is_dir && !reparse {
                            stack.push((e.path(), child_rel));
                        }
                    }
                }
                Ok(())
            }
            (SrcFs::Zip { items, .. }, Location::Zip { inner, .. }) => {
                let base = if inner.is_empty() { source.name.clone() } else { format!("{inner}/{}", source.name) };
                let strip = if inner.is_empty() { 0 } else { inner.len() + 1 };
                let prefix = format!("{base}/");
                let mut dirs: HashSet<String> = HashSet::new();
                let add_dir = |rel: &str, out: &mut Vec<Item>, dirs: &mut HashSet<String>| {
                    // Alle Zwischenordner anlegen, auch wenn das Archiv sie nicht eigens führt.
                    let mut acc = String::new();
                    for part in rel.split('/') {
                        if !acc.is_empty() {
                            acc.push('/');
                        }
                        acc.push_str(part);
                        if dirs.insert(acc.clone()) {
                            out.push(Item { src: ItemSrc::Zip(usize::MAX), rel: acc.clone(), is_dir: true, size: 0, modified: None });
                        }
                    }
                };
                if source.is_dir {
                    add_dir(&top, out, &mut dirs);
                }
                for it in items.iter() {
                    let matches = it.path == base || it.path.starts_with(&prefix);
                    if !matches {
                        continue;
                    }
                    let rel = it.path[strip..].to_owned();
                    if it.is_dir {
                        add_dir(&rel, out, &mut dirs);
                    } else {
                        if let Some(pos) = rel.rfind('/') {
                            add_dir(&rel[..pos], out, &mut dirs);
                        }
                        out.push(Item { src: ItemSrc::Zip(it.index), rel, is_dir: false, size: it.size, modified: it.modified });
                    }
                }
                Ok(())
            }
            _ => Err(t("Quelle und Quellordner passen nicht zusammen.", "Source and source folder do not match.").to_owned()),
        };
        result
    }

    fn read(&mut self, item: &Item, f: &mut dyn FnMut(&mut dyn Read) -> Result<(), String>) -> Result<(), String> {
        match (self, &item.src) {
            (SrcFs::Local, ItemSrc::Local(path)) => {
                let mut file = File::open(path).map_err(|e| io_msg(&e))?;
                f(&mut file)
            }
            (SrcFs::Zip { archive, .. }, ItemSrc::Zip(index)) => {
                let mut file = archive
                    .by_index(*index)
                    .map_err(|e| tf!("Entpacken fehlgeschlagen: {e}", "Extraction failed: {e}"))?;
                f(&mut file)
            }
            _ => Err(t("Interner Fehler: Quelle passt nicht.", "Internal error: source does not match.").to_owned()),
        }
    }

    fn remove_file(&mut self, item: &Item, shared: &JobShared) -> Result<(), String> {
        match (self, &item.src) {
            (SrcFs::Local, ItemSrc::Local(path)) => fs::remove_file(path).map_err(|e| {
                shared.in_use(path, &e);
                io_msg(&e)
            }),
            _ => Err(t(
                "Aus ZIP-Archiven kann nicht verschoben werden.",
                "Items cannot be moved out of ZIP archives.",
            )
            .to_owned()),
        }
    }

    fn remove_dir(&mut self, item: &Item, shared: &JobShared) -> Result<(), String> {
        match (self, &item.src) {
            (SrcFs::Local, ItemSrc::Local(path)) => fs::remove_dir(path).map_err(|e| {
                shared.in_use(path, &e);
                io_msg(&e)
            }),
            _ => Ok(()),
        }
    }
}

// ---------------------------------------------------------------- Ziel

enum DestFs {
    Local(PathBuf),
}

fn rel_parent(rel: &str) -> (&str, &str) {
    match rel.rfind('/') {
        Some(pos) => (&rel[..pos], &rel[pos + 1..]),
        None => ("", rel),
    }
}

impl DestFs {
    fn open(dest: &Location) -> Result<Self, String> {
        match dest {
            Location::Local(p) => Ok(DestFs::Local(p.clone())),
            Location::Zip { .. } => Err(t(
                "In ein ZIP-Archiv kann nicht geschrieben werden. Nutze „Packen\", um ein neues Archiv zu erstellen.",
                "Cannot write to a ZIP archive. Use “Pack” to create a new archive.",
            )
            .to_owned()),
            Location::Drives
            | Location::Network { .. }
            | Location::RecycleBin
            | Location::Device { .. } => Err(t(
                "Bitte zuerst ein Laufwerk, eine Freigabe oder einen Ordner als Ziel öffnen.",
                "Please open a drive, a share or a folder as the destination first.",
            )
            .to_owned()),
        }
    }

    fn local_target(&self, rel: &str) -> Option<PathBuf> {
        let DestFs::Local(base) = self;
        Some(base.join(rel.replace('/', "\\")))
    }


    /// Größe einer bereits vorhandenen Datei gleichen Namens.
    fn existing(&mut self, rel: &str) -> Result<Option<u64>, String> {
        match self {
            DestFs::Local(base) => {
                let p = base.join(rel.replace('/', "\\"));
                Ok(fs::metadata(&p).ok().map(|m| m.len()))
            }
        }
    }

    /// Gibt es den Ordner im Ziel schon? Im Zweifel ja – dann gilt er später nicht als neu angelegt.
    fn has_dir(&mut self, rel: &str) -> bool {
        match self {
            DestFs::Local(base) => base.join(rel.replace('/', "\\")).is_dir(),
        }
    }

    /// „Name (2).ext" – erster freier Name.
    fn unique(&mut self, rel: &str) -> Result<String, String> {
        let (dir, name) = rel_parent(rel);
        let (stem, ext) = split_ext(name);
        for n in 2..10_000 {
            let candidate = if ext.is_empty() { format!("{stem} ({n})") } else { format!("{stem} ({n}).{ext}") };
            let full = if dir.is_empty() { candidate } else { format!("{dir}/{candidate}") };
            if self.existing(&full)?.is_none() {
                return Ok(full);
            }
        }
        Err(t("Kein freier Name gefunden.", "No unused name found.").to_owned())
    }

    fn mkdir(&mut self, rel: &str) -> Result<(), String> {
        match self {
            DestFs::Local(base) => fs::create_dir_all(base.join(rel.replace('/', "\\"))).map_err(|e| io_msg(&e)),
        }
    }

    fn write(&mut self, rel: &str, reader: &mut dyn Read, modified: Option<i64>) -> Result<(), String> {
        match self {
            DestFs::Local(base) => {
                let target = base.join(rel.replace('/', "\\"));
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| io_msg(&e))?;
                }
                let file = File::create(&target).map_err(|e| io_msg(&e))?;
                let mut writer = BufWriter::with_capacity(256 * 1024, file);
                let result = pump(reader, &mut writer).and_then(|_| writer.flush());
                match result {
                    Ok(()) => {
                        if let (Some(secs), Ok(file)) = (modified, writer.into_inner()) {
                            if secs > 0 {
                                let _ = file.set_modified(UNIX_EPOCH + Duration::from_secs(secs as u64));
                            }
                        }
                        Ok(())
                    }
                    Err(e) => {
                        drop(writer);
                        let _ = fs::remove_file(&target);
                        Err(err_text(&e))
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- Kopieren / Verschieben

fn is_inside(child: &Location, parent: &Location) -> bool {
    match (child, parent) {
        (Location::Local(c), Location::Local(p)) => c.starts_with(p),
        _ => false,
    }
}

fn transfer(
    sources: Vec<Source>,
    dest: Location,
    mv: bool,
    shared: &JobShared,
    bus: &Bus,
) -> Result<(), String> {
    // Suchtreffer können aus verschiedenen Ordnern stammen: je Quellordner ein Durchgang.
    let mut groups: Vec<(Location, Vec<Source>)> = Vec::new();
    for s in sources {
        match groups.iter_mut().find(|(dir, _)| *dir == s.dir) {
            Some((_, list)) => list.push(s),
            None => groups.push((s.dir.clone(), vec![s])),
        }
    }
    for (dir, group) in groups {
        // Geräte am Kabel: die Übertragung macht Windows selbst (siehe `device`).
        if let Location::Device { id, path, .. } = &dir {
            let Location::Local(target) = &dest else {
                return Err(t(
                    "Von einem Gerät am Kabel lässt sich nur in einen Ordner auf diesem PC kopieren.",
                    "From a device on a cable, items can only be copied to a folder on this PC.",
                )
                .to_owned());
            };
            if mv {
                return Err(t(
                    "Von einem Gerät am Kabel wird nur kopiert, nicht verschoben.",
                    "Items on a device on a cable are only copied, not moved.",
                )
                .to_owned());
            }
            shared.scanning.store(false, Relaxed);
            shared.set_current(t("Windows kopiert vom Gerät …", "Windows is copying from the device …"));
            let names: Vec<String> = group.iter().map(|s| s.name.clone()).collect();
            // Zum Öffnen einer einzelnen Datei (Ziel im Zwischenordner) ohne Fenster und Rückfragen
            let quiet = target.starts_with(std::env::temp_dir().join("TurboCommanderLite"));
            crate::device::copy_out(id, path, &names, target, quiet)?;
            continue;
        }
        shared.scanning.store(true, Relaxed);
        let mut trace = Trace::default();
        let result = transfer_group(group, dest.clone(), mv, shared, bus, &mut trace);
        trace.record(&dir, &dest, mv, shared);
        result?;
    }
    Ok(())
}

fn transfer_group(
    mut sources: Vec<Source>,
    dest: Location,
    mv: bool,
    shared: &JobShared,
    bus: &Bus,
    trace: &mut Trace,
) -> Result<(), String> {
    if sources.is_empty() {
        return Ok(());
    }
    let src_dir = sources[0].dir.clone();
    let same_dir = src_dir == dest;
    if same_dir && mv {
        return Ok(());
    }
    for s in &sources {
        if s.is_dir && is_inside(&dest, &s.location()) {
            return Err(tf!("Der Zielordner liegt innerhalb von „{}\".", "The destination folder is inside “{}”.", s.name));
        }
    }
    let mut dst = DestFs::open(&dest)?;

    // Schneller Weg: Verschieben innerhalb eines Laufwerks bzw. Servers ist nur ein Umbenennen.
    if mv {
        match (&src_dir, &dest) {
            (Location::Local(from), Location::Local(to)) => {
                sources.retain(|s| {
                    let target = to.join(&s.name);
                    if target.exists() {
                        return true;
                    }
                    let failed = fs::rename(from.join(&s.name), &target).is_err();
                    if !failed {
                        trace.whole.push(s.name.clone());
                    }
                    failed
                });
            }
            _ => {}
        }
        if sources.is_empty() {
            return Ok(());
        }
    }

    let mut src = SrcFs::open(&src_dir)?;
    let mut plan = Vec::new();
    for s in &sources {
        shared.set_current(&s.name);
        src.plan(s, &mut plan, shared)?;
    }
    shared.total_bytes.fetch_add(plan.iter().map(|i| i.size).sum(), Relaxed);
    shared.total_items.fetch_add(plan.len() as u64, Relaxed);
    shared.scanning.store(false, Relaxed);

    #[derive(PartialEq)]
    enum Policy {
        Ask,
        OverwriteAll,
        SkipAll,
    }
    let mut policy = Policy::Ask;
    let resume = shared.resume.load(Relaxed);
    // Umbenannte Wurzeln (z. B. „Kopie" im selben Ordner): alter Präfix → neuer Präfix
    let mut renamed_roots: HashMap<String, String> = HashMap::new();
    let mut incomplete_dirs: HashSet<String> = HashSet::new();
    let mark_incomplete = |rel: &str, set: &mut HashSet<String>| {
        let mut acc = String::new();
        for part in rel.split('/') {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(part);
            set.insert(acc.clone());
        }
    };

    for item in &plan {
        if !shared.keep_going() {
            return Err(CANCELLED.to_owned());
        }
        shared.set_current(&item.rel);
        // Ziel-Pfad, ggf. unter einer umbenannten Wurzel
        let top = item.rel.split('/').next().unwrap_or("");
        let mut rel = match renamed_roots.get(top) {
            Some(new_top) => format!("{new_top}{}", &item.rel[top.len()..]),
            None => item.rel.clone(),
        };
        let is_top = !item.rel.contains('/');

        if item.is_dir {
            if same_dir && is_top {
                let fresh = dst.unique(&rel)?;
                renamed_roots.insert(top.to_owned(), fresh.clone());
                rel = fresh;
            }
            let existed = dst.has_dir(&rel);
            match dst.mkdir(&rel) {
                Ok(()) if !existed => trace.dirs.push(rel.clone()),
                Ok(()) => {}
                Err(e) => {
                    shared.error(format!("{}: {e}", item.rel));
                    mark_incomplete(&item.rel, &mut incomplete_dirs);
                }
            }
            shared.done_items.fetch_add(1, Relaxed);
            continue;
        }

        // Ersetzt die Datei eine vorhandene? Dann lässt sich das später nicht zurücknehmen.
        let mut replacing = false;
        match dst.existing(&rel) {
            Err(e) => {
                shared.error(format!("{}: {e}", item.rel));
                mark_incomplete(&item.rel, &mut incomplete_dirs);
                shared.done_items.fetch_add(1, Relaxed);
                shared.done_bytes.fetch_add(item.size, Relaxed);
                continue;
            }
            // Fortsetzung eines früheren Laufs: die Datei liegt schon in voller Größe im Ziel.
            Ok(Some(dst_size)) if resume && dst_size == item.size && !(same_dir && is_top) => {
                // Beim Verschieben fehlte dann nur noch das Entfernen der Quelle.
                if mv && src.remove_file(item, shared).is_err() {
                    mark_incomplete(&item.rel, &mut incomplete_dirs);
                }
                shared.done_items.fetch_add(1, Relaxed);
                shared.done_bytes.fetch_add(item.size, Relaxed);
                continue;
            }
            Ok(Some(dst_size)) => {
                let answer = if same_dir && is_top {
                    Answer::KeepBoth
                } else if resume {
                    // Eine angefangene Datei aus dem früheren Lauf: neu übertragen, ohne zu fragen.
                    Answer::Overwrite
                } else {
                    match policy {
                        Policy::OverwriteAll => Answer::Overwrite,
                        Policy::SkipAll => Answer::Skip,
                        Policy::Ask => shared.ask(
                            Conflict { name: item.rel.clone(), dest: dest.display(), src_size: item.size, dst_size },
                            bus,
                        ),
                    }
                };
                match answer {
                    Answer::Overwrite => replacing = true,
                    Answer::OverwriteAll => {
                        policy = Policy::OverwriteAll;
                        replacing = true;
                    }
                    Answer::KeepBoth => rel = dst.unique(&rel)?,
                    Answer::Cancel => {
                        shared.cancel.store(true, Relaxed);
                        return Err(CANCELLED.to_owned());
                    }
                    Answer::Skip | Answer::SkipAll => {
                        if answer == Answer::SkipAll {
                            policy = Policy::SkipAll;
                        }
                        mark_incomplete(&item.rel, &mut incomplete_dirs);
                        shared.done_items.fetch_add(1, Relaxed);
                        shared.done_bytes.fetch_add(item.size, Relaxed);
                        continue;
                    }
                }
            }
            Ok(None) => {}
        }

        let before = shared.done_bytes.load(Relaxed);
        let result = match (&item.src, dst.local_target(&rel)) {
            (ItemSrc::Local(from), Some(target)) => {
                if let Some(parent) = target.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                win::copy_file(from, &target, &mut |delta| {
                    shared.done_bytes.fetch_add(delta, Relaxed);
                    shared.keep_going()
                })
                .map_err(|e| {
                    // Offen sein kann die Quelle wie eine Datei, die im Ziel ersetzt werden soll.
                    shared.in_use(from, &e);
                    shared.in_use(&target, &e);
                    err_text(&e)
                })
            }
            _ => src.read(item, &mut |reader| {
                let mut progress = ProgressReader { inner: reader, shared };
                dst.write(&rel, &mut progress, item.modified)
            }),
        };
        match result {
            Ok(()) => {
                // Fortschritt exakt auf die Dateigröße setzen (leere Dateien, abweichende Größen)
                shared.done_bytes.store(before + item.size, Relaxed);
                shared.done_items.fetch_add(1, Relaxed);
                let mut carried = true;
                if mv {
                    if let Err(e) = src.remove_file(item, shared) {
                        shared.error(tf!(
                            "{}: Quelle konnte nicht entfernt werden – {e}",
                            "{}: source could not be removed – {e}",
                            item.rel
                        ));
                        mark_incomplete(&item.rel, &mut incomplete_dirs);
                        // Die Datei liegt nun an beiden Orten: nichts, was zurückzuholen wäre.
                        carried = false;
                    }
                }
                if carried {
                    let file = Carried { from: item.rel.clone(), to: rel.clone(), size: item.size, modified: item.modified };
                    trace.files.push((file, replacing));
                }
            }
            Err(e) if e == CANCELLED => return Err(e),
            Err(e) => {
                shared.error(format!("{}: {e}", item.rel));
                mark_incomplete(&item.rel, &mut incomplete_dirs);
                shared.done_bytes.store(before + item.size, Relaxed);
                shared.done_items.fetch_add(1, Relaxed);
            }
        }
    }

    if mv {
        // Leere Quellordner von innen nach außen entfernen – nur wenn alles darin übertragen wurde.
        for item in plan.iter().rev().filter(|i| i.is_dir) {
            if !incomplete_dirs.contains(&item.rel) {
                if let (Err(e), ItemSrc::Local(path)) = (src.remove_dir(item, shared), &item.src) {
                    // Hält ein Programm den leeren Ordner fest, soll er nicht unbemerkt zurückbleiben.
                    if shared.is_suspect(path) {
                        shared.error(tf!(
                            "{}: Ordner konnte nicht entfernt werden – {e}",
                            "{}: folder could not be removed – {e}",
                            item.rel
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- Zurücknehmen

fn undo(records: Vec<Undo>, shared: &JobShared) -> Result<(), String> {
    shared.total_items.store(records.iter().map(Undo::count).sum::<usize>() as u64, Relaxed);
    shared.scanning.store(false, Relaxed);
    // Das zuletzt Getane zuerst
    for record in records.into_iter().rev() {
        match record {
            Undo::Copied { dest, files, dirs, .. } => undo_copy(&dest, &files, &dirs, shared)?,
            Undo::Moved { from, to, files, dirs, whole, .. } => undo_move(&from, &to, &files, &dirs, &whole, shared)?,
            Undo::Trashed { paths, after } => undo_trash(&paths, after, shared)?,
        }
    }
    Ok(())
}

/// Entfernt, was ein Kopieren neu angelegt hat – aber nur Dateien, die seither nicht verändert wurden.
fn undo_copy(dest: &Location, files: &[(String, u64)], dirs: &[String], shared: &JobShared) -> Result<(), String> {
    let changed = |rel: &str| {
        shared.error(tf!(
            "{rel}: seit dem Kopieren verändert – bleibt liegen",
            "{rel}: changed since it was copied – left in place"
        ))
    };
    match dest {
        Location::Local(base) => {
            let mut gone = Vec::new();
            for (rel, size) in files {
                let path = base.join(rel.replace('/', "\\"));
                match fs::metadata(&path) {
                    Ok(meta) if meta.is_file() && meta.len() == *size => gone.push(path),
                    Ok(_) => changed(rel),
                    Err(_) => {} // schon weg
                }
                shared.done_items.fetch_add(1, Relaxed);
            }
            if !gone.is_empty() {
                // In den Papierkorb: auch das Zurücknehmen soll nichts endgültig vernichten.
                trash::delete_all(&gone).map_err(|e| tf!("Papierkorb: {e}", "Recycle Bin: {e}"))?;
            }
            // Angelegte Ordner von innen nach außen; was nicht leer ist, bleibt stehen.
            for dir in dirs.iter().rev() {
                let _ = fs::remove_dir(base.join(dir.replace('/', "\\")));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Bringt Verschobenes zurück: `now` ist der Ordner, in dem es liegt, `home` der, aus dem es kam.
fn undo_move(
    home: &Location,
    now: &Location,
    files: &[Carried],
    dirs: &[String],
    whole: &[String],
    shared: &JobShared,
) -> Result<(), String> {
    let taken = || t("liegt am alten Ort schon wieder – übersprungen", "exists at the old place again – skipped");
    // Als Ganzes umgehängte Einträge: wieder umhängen.
    match (now, home) {
        (Location::Local(now), Location::Local(home)) => {
            for name in whole {
                if home.join(name).exists() {
                    shared.error(format!("{name}: {}", taken()));
                } else if let Err(e) = fs::rename(now.join(name), home.join(name)) {
                    shared.error(format!("{name}: {}", io_msg(&e)));
                }
                shared.done_items.fetch_add(1, Relaxed);
            }
        }
        _ => {}
    }
    if files.is_empty() && dirs.is_empty() {
        return Ok(());
    }
    let mut src = SrcFs::open(now)?;
    let mut dst = DestFs::open(home)?;
    let source = |rel: &str| match now {
        Location::Local(base) => Some(ItemSrc::Local(base.join(rel.replace('/', "\\")))),
        _ => None,
    };
    let mut made: HashSet<String> = HashSet::new();
    for file in files {
        if shared.cancelled() {
            return Err(CANCELLED.to_owned());
        }
        shared.set_current(&file.from);
        let Some(src_item) = source(&file.to) else { continue };
        let item = Item { src: src_item, rel: file.from.clone(), is_dir: false, size: file.size, modified: file.modified };
        if let Err(e) = move_back(&mut src, &mut dst, &item, &mut made, shared, taken()) {
            if e == CANCELLED {
                return Err(e);
            }
            shared.error(format!("{}: {e}", file.to));
        }
        shared.done_items.fetch_add(1, Relaxed);
    }
    // Was das Verschieben im Ziel an Ordnern angelegt hat, verschwindet wieder – sofern es jetzt leer ist.
    for dir in dirs.iter().rev() {
        if let Some(src_item) = source(dir) {
            let _ = src.remove_dir(&Item { src: src_item, rel: dir.clone(), is_dir: true, size: 0, modified: None }, shared);
        }
    }
    Ok(())
}

/// Holt eine Datei an ihren alten Platz (`item.rel`), ohne dort etwas zu überschreiben.
fn move_back(
    src: &mut SrcFs,
    dst: &mut DestFs,
    item: &Item,
    made: &mut HashSet<String>,
    shared: &JobShared,
    taken: &str,
) -> Result<(), String> {
    if dst.existing(&item.rel)?.is_some() {
        return Err(taken.to_owned());
    }
    let (dir, _) = rel_parent(&item.rel);
    if !dir.is_empty() && !made.contains(dir) {
        let mut path = String::new();
        for part in dir.split('/') {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(part);
            if made.insert(path.clone()) {
                dst.mkdir(&path)?;
            }
        }
    }
    match (&item.src, dst.local_target(&item.rel)) {
        (ItemSrc::Local(now), Some(home)) => {
            // Auf demselben Laufwerk genügt ein Umhängen; sonst kopieren und die Kopie am neuen Ort entfernen.
            if fs::rename(now, &home).is_ok() {
                return Ok(());
            }
            win::copy_file(now, &home, &mut |_| !shared.cancelled()).map_err(|e| err_text(&e))?;
            fs::remove_file(now).map_err(|e| io_msg(&e))
        }
        _ => {
            src.read(item, &mut |reader| dst.write(&item.rel, reader, item.modified))?;
            src.remove_file(item, shared)
        }
    }
}

/// Stellt aus dem Papierkorb wieder her, was der Auftrag hineingelegt hat.
fn undo_trash(paths: &[PathBuf], after: i64, shared: &JobShared) -> Result<(), String> {
    shared.set_current(t("Aus dem Papierkorb …", "From the Recycle Bin …"));
    let listed = trash::os_limited::list().map_err(|e| tf!("Papierkorb: {e}", "Recycle Bin: {e}"))?;
    let mut chosen = Vec::new();
    for path in paths {
        // Je Pfad der jüngste Eintrag, der seit dem Auftrag im Papierkorb liegt
        let found = listed.iter().filter(|item| item.time_deleted >= after && item.original_path() == *path).max_by_key(|item| item.time_deleted);
        match found {
            Some(item) => chosen.push(item.clone()),
            None => shared.error(tf!(
                "{}: nicht mehr im Papierkorb",
                "{}: no longer in the Recycle Bin",
                path.file_name().unwrap_or_default().to_string_lossy()
            )),
        }
    }
    if !chosen.is_empty() {
        trash::os_limited::restore_all(chosen).map_err(|e| tf!("Wiederherstellen: {e}", "Restoring: {e}"))?;
    }
    shared.done_items.store(paths.len() as u64, Relaxed);
    Ok(())
}

// ---------------------------------------------------------------- Löschen

/// Die Meldung des Papierkorbs ohne ihre technische Verpackung. `left`: was er nicht genommen hat.
fn trash_text(e: &trash::Error, left: &[&PathBuf]) -> String {
    let reason = match e {
        // So meldet Windows eine Datei, die ein anderes Programm offen hält – das sagt TurboCommander mit eigenen
        // Worten und mit den Namen der Dateien.
        trash::Error::Os { code, .. } if matches!(*code as u32, 0x8027_0027 | 0x8007_0020) && !left.is_empty() => {
            let mut names: Vec<String> =
                left.iter().take(3).filter_map(|path| path.file_name()).map(|name| name.to_string_lossy().into_owned()).collect();
            if left.len() > 3 {
                names.push(tf!("{} weitere", "{} more", left.len() - 3));
            }
            format!("{}: {}", names.join(", "), io_msg(&io::Error::from_raw_os_error(32)))
        }
        trash::Error::Os { description, .. } => description.trim_start_matches("windows error: ").to_owned(),
        other => other.to_string(),
    };
    tf!("Papierkorb: {reason}", "Recycle Bin: {reason}")
}

fn delete(sources: Vec<Source>, permanent: bool, shared: &JobShared) -> Result<(), String> {
    if sources.is_empty() {
        return Ok(());
    }
    shared.total_items.store(sources.len() as u64, Relaxed);
    shared.scanning.store(false, Relaxed);
    match &sources[0].dir {
        Location::Local(dir) => {
            // Suchtreffer liegen in verschiedenen Ordnern: jeder Eintrag in seinem eigenen.
            let paths: Vec<PathBuf> = sources.iter().map(|s| s.dir.local_path().unwrap_or(dir).join(&s.name)).collect();
            if !permanent {
                shared.set_current(t("In den Papierkorb …", "Moving to the Recycle Bin …"));
                // Ein paar Sekunden Spielraum: die Zeit im Papierkorb ist gröber als die Uhr hier.
                let after = crate::util::system_time_secs(std::time::SystemTime::now()) - 5;
                if let Err(e) = trash::delete_all(&paths) {
                    // Was noch da ist, hat der Papierkorb nicht genommen – oft, weil ein Programm es offen hält.
                    let left: Vec<&PathBuf> = paths.iter().filter(|path| path.exists()).collect();
                    for path in &left {
                        shared.suspect(path);
                    }
                    return Err(trash_text(&e, &left));
                }
                shared.undo.lock().unwrap().push(Undo::Trashed { paths, after });
                shared.done_items.store(sources.len() as u64, Relaxed);
                return Ok(());
            }
            for (s, path) in sources.iter().zip(&paths) {
                if shared.cancelled() {
                    return Err(CANCELLED.to_owned());
                }
                shared.set_current(&s.name);
                let res = if s.is_dir { fs::remove_dir_all(path) } else { fs::remove_file(path) };
                if let Err(e) = res {
                    shared.in_use(path, &e);
                    shared.error(format!("{}: {}", s.name, io_msg(&e)));
                }
                shared.done_items.fetch_add(1, Relaxed);
            }
            Ok(())
        }
        Location::Zip { .. } => {
            Err(t("Aus ZIP-Archiven kann nicht gelöscht werden.", "Items cannot be deleted from ZIP archives.").to_owned())
        }
        Location::Drives
            | Location::Network { .. }
            | Location::RecycleBin
            | Location::Device { .. } => Err(t(
            "Einträge dieser Übersicht lassen sich hier nicht löschen.",
            "Items in this overview cannot be deleted here.",
        )
        .to_owned()),
    }
}

// ---------------------------------------------------------------- Packen

fn zip_time(path: &Path) -> Option<zip::DateTime> {
    use chrono::{Datelike, Timelike};
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let local: chrono::DateTime<chrono::Local> = modified.into();
    zip::DateTime::from_date_and_time(
        local.year().clamp(1980, 2107) as u16,
        local.month() as u8,
        local.day() as u8,
        local.hour() as u8,
        local.minute() as u8,
        local.second() as u8,
    )
    .ok()
}

fn pack(sources: Vec<PathBuf>, target: PathBuf, shared: &JobShared) -> Result<(), String> {
    use zip::write::SimpleFileOptions;
    // (Pfad auf der Platte, Name im Archiv, Ordner?)
    let mut plan: Vec<(PathBuf, String, bool, u64)> = Vec::new();
    for src in &sources {
        let Some(name) = src.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        if src.is_dir() {
            plan.push((src.clone(), name.clone(), true, 0));
            let mut stack = vec![(src.clone(), name)];
            while let Some((dir, rel)) = stack.pop() {
                if shared.cancelled() {
                    return Err(CANCELLED.to_owned());
                }
                let Ok(rd) = fs::read_dir(&dir) else { continue };
                for e in rd.flatten() {
                    let Ok(meta) = e.metadata() else { continue };
                    let child = format!("{rel}/{}", e.file_name().to_string_lossy());
                    if meta.is_dir() {
                        plan.push((e.path(), child.clone(), true, 0));
                        stack.push((e.path(), child));
                    } else {
                        plan.push((e.path(), child, false, meta.len()));
                    }
                }
            }
        } else {
            let size = fs::metadata(src).map(|m| m.len()).unwrap_or(0);
            plan.push((src.clone(), name, false, size));
        }
    }
    shared.total_bytes.store(plan.iter().map(|p| p.3).sum(), Relaxed);
    shared.total_items.store(plan.len() as u64, Relaxed);
    shared.scanning.store(false, Relaxed);

    let tmp = target.with_extension("zip.teil");
    let file = File::create(&tmp).map_err(|e| io_msg(&e))?;
    let mut writer = zip::ZipWriter::new(BufWriter::with_capacity(256 * 1024, file));
    let result = (|| -> Result<(), String> {
        for (path, name, is_dir, size) in &plan {
            if shared.cancelled() {
                return Err(CANCELLED.to_owned());
            }
            shared.set_current(name);
            let mut options = SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .large_file(*size >= 0xFFFF_0000);
            if let Some(t) = zip_time(path) {
                options = options.last_modified_time(t);
            }
            if *is_dir {
                writer.add_directory(format!("{name}/"), options).map_err(|e| e.to_string())?;
            } else {
                writer.start_file(name.as_str(), options).map_err(|e| e.to_string())?;
                let mut input = File::open(path).map_err(|e| format!("{name}: {}", io_msg(&e)))?;
                let mut progress = ProgressReader { inner: &mut input, shared };
                pump(&mut progress, &mut writer).map_err(|e| err_text(&e))?;
            }
            shared.done_items.fetch_add(1, Relaxed);
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            let inner = writer.finish().map_err(|e| e.to_string())?;
            inner.into_inner().map_err(|e| io_msg(e.error()))?.sync_all().ok();
            if target.exists() {
                let _ = fs::remove_file(&target);
            }
            fs::rename(&tmp, &target).map_err(|e| io_msg(&e))
        }
        Err(e) => {
            drop(writer);
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

// ---------------------------------------------------------------- Entfernte Datei öffnen

fn open_remote(source: Source, shared: &JobShared, bus: &Bus) -> Result<(), String> {
    let stamp = std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let dir = std::env::temp_dir().join("TurboCommanderLite").join(format!("{stamp:x}"));
    fs::create_dir_all(&dir).map_err(|e| io_msg(&e))?;
    let dest = Location::Local(dir.clone());
    let name = source.name.clone();
    transfer(vec![source], dest, false, shared, bus)?;
    if shared.errors.lock().unwrap().is_empty() {
        bus.send(Msg::OpenTemp(dir.join(name)));
    }
    Ok(())
}

// ---------------------------------------------------------------- Ordnergrößen

/// Summiert lokale Ordner nacheinander im Hintergrund und meldet Zwischenstände.
pub fn spawn_dir_sizes(tab: u64, loc: Location, names: Vec<String>, bus: Bus, cancel: Arc<AtomicBool>) {
    let Some(base) = loc.local_path().map(PathBuf::from) else { return };
    std::thread::spawn(move || {
        use std::os::windows::fs::MetadataExt;
        for name in names {
            let mut total = 0u64;
            let mut stack = vec![base.join(&name)];
            let mut last = Instant::now();
            while let Some(dir) = stack.pop() {
                if cancel.load(Relaxed) {
                    return;
                }
                let Ok(rd) = fs::read_dir(&dir) else { continue };
                for e in rd.flatten() {
                    let Ok(meta) = e.metadata() else { continue };
                    let attrs = meta.file_attributes();
                    if attrs & 0x10 != 0 {
                        // Verknüpfte Ordner nicht verfolgen
                        if attrs & 0x400 == 0 {
                            stack.push(e.path());
                        }
                    } else {
                        total += meta.len();
                    }
                }
                if last.elapsed() > Duration::from_millis(250) {
                    last = Instant::now();
                    bus.send(Msg::DirSize { tab, loc: loc.clone(), name: name.clone(), size: total, done: false });
                }
            }
            bus.send(Msg::DirSize { tab, loc: loc.clone(), name, size: total, done: true });
        }
    });
}
