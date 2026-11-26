//! Orte (lokal, ZIP, „Dieser PC"), Einträge und das Auflisten von Ordnern.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Sender;

use serde::{Deserialize, Serialize};

use crate::lang::t;
use crate::tf;
use crate::util::{fold_key, natural_cmp, split_ext, system_time_secs};
use crate::win;

// ---------------------------------------------------------------- Orte

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Location {
    /// Übersicht aller Laufwerke.
    Drives,
    Local(PathBuf),
    /// `inner` mit `/` getrennt, ohne führenden oder abschließenden Schrägstrich.
    Zip { archive: PathBuf, inner: String },
    /// Netzwerkumgebung: ohne `server` die Liste der Computer, mit `server` dessen Freigaben.
    Network { server: Option<String> },
    /// Der Papierkorb aller Laufwerke (siehe `bin`).
    RecycleBin,
    /// Ein per Kabel angeschlossenes Gerät (Telefon, Kamera), wie es der Explorer unter „Dieser PC" zeigt (siehe
    /// `device`). `id`: Pfad des Geräts in der Shell; `path` mit `/` getrennt, „/" ist das Gerät selbst.
    Device { id: String, name: String, path: String },
}

/// Servername eines Netzwerkpfads (`\\NAS01\freigabe\…` → `NAS01`).
pub fn unc_server(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    let rest = text.strip_prefix("\\\\")?;
    let server = rest.split('\\').next()?;
    (!server.is_empty() && server != "?" && server != ".").then(|| server.to_owned())
}

impl Location {
    pub fn parent(&self) -> Option<Location> {
        match self {
            Location::Drives => None,
            Location::Local(p) => match p.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => Some(Location::Local(parent.to_path_buf())),
                // Über einer Freigabe steht der Computer, über einem Laufwerk „Dieser PC".
                _ => Some(match unc_server(p) {
                    Some(server) => Location::Network { server: Some(server) },
                    None => Location::Drives,
                }),
            },
            Location::Network { server: Some(_) } => Some(Location::Network { server: None }),
            Location::Network { server: None } | Location::RecycleBin => {
                Some(Location::Drives)
            }
            Location::Device { id, name, path } => {
                let trimmed = path.trim_end_matches('/');
                if trimmed.is_empty() {
                    Some(Location::Drives)
                } else {
                    let up = trimmed.rfind('/').map_or("/", |i| if i == 0 { "/" } else { &trimmed[..i] });
                    Some(Location::Device { id: id.clone(), name: name.clone(), path: up.to_owned() })
                }
            }
            Location::Zip { archive, inner } => {
                if inner.is_empty() {
                    archive.parent().map(|p| Location::Local(p.to_path_buf()))
                } else {
                    let up = inner.rfind('/').map_or("", |i| &inner[..i]);
                    Some(Location::Zip { archive: archive.clone(), inner: up.to_owned() })
                }
            }
        }
    }

    pub fn child(&self, name: &str) -> Location {
        match self {
            Location::Drives => Location::Local(PathBuf::from(format!("{}\\", name.trim_end_matches('\\')))),
            Location::Local(p) => Location::Local(p.join(name)),
            Location::Zip { archive, inner } => Location::Zip {
                archive: archive.clone(),
                inner: if inner.is_empty() { name.to_owned() } else { format!("{inner}/{name}") },
            },
            Location::Network { server: None } => Location::Network { server: Some(name.to_owned()) },
            Location::Network { server: Some(server) } => Location::Local(PathBuf::from(format!("\\\\{server}\\{name}"))),
            // Im Papierkorb führt jeder Eintrag selbst zu seinem Ziel.
            Location::RecycleBin => Location::Local(PathBuf::from(name)),
            Location::Device { id, name: device, path } => Location::Device {
                id: id.clone(),
                name: device.clone(),
                path: if path.ends_with('/') { format!("{path}{name}") } else { format!("{path}/{name}") },
            },
        }
    }

    /// Liegt `self` unterhalb von `ancestor`, liefert dies den Namen des Zweigs direkt darunter, in dem `self` liegt
    /// (für „D:\a\b\c" und „D:\a" also „b"). So lässt sich nach dem Hochspringen der Herkunftsordner wählen.
    pub fn branch_below(&self, ancestor: &Location) -> Option<String> {
        let mut current = self.clone();
        for _ in 0..512 {
            let parent = current.parent()?;
            if parent == *ancestor {
                return Some(current.leaf());
            }
            current = parent;
        }
        None
    }

    /// Übersichten, die keine echten Ordner sind (Dieser PC, Netzwerk).
    pub fn is_virtual(&self) -> bool {
        matches!(self, Location::Drives | Location::Network { .. })
    }

    /// Name des Ordners selbst (für „nach oben" und Tab-Titel).
    pub fn leaf(&self) -> String {
        match self {
            Location::Drives => t("Dieser PC", "This PC").to_owned(),
            Location::Local(p) => match p.file_name() {
                Some(n) => n.to_string_lossy().into_owned(),
                None => {
                    // Wurzel: bei „C:\" der Laufwerksbuchstabe, bei „\\NAS\freigabe" der Name der Freigabe.
                    let text = p.to_string_lossy();
                    let trimmed = text.trim_end_matches('\\');
                    match unc_server(p) {
                        Some(_) => trimmed.rsplit('\\').next().unwrap_or(trimmed).to_owned(),
                        None => trimmed.to_owned(),
                    }
                }
            },
            Location::Network { server: None } => t("Netzwerk", "Network").to_owned(),
            Location::Network { server: Some(server) } => server.clone(),
            Location::RecycleBin => t("Papierkorb", "Recycle Bin").to_owned(),
            Location::Device { name, path, .. } => {
                let t = path.trim_end_matches('/');
                if t.is_empty() { name.clone() } else { t.rsplit('/').next().unwrap_or("").to_owned() }
            }
            Location::Zip { archive, inner } => {
                if inner.is_empty() {
                    archive.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                } else {
                    inner.rsplit('/').next().unwrap_or("").to_owned()
                }
            }
        }
    }

    /// Voller Pfad als Text, wie er in der Adresszeile steht.
    pub fn display(&self) -> String {
        match self {
            Location::Drives => t("Dieser PC", "This PC").to_owned(),
            Location::Local(p) => p.to_string_lossy().into_owned(),
            Location::Zip { archive, inner } => {
                if inner.is_empty() {
                    archive.to_string_lossy().into_owned()
                } else {
                    format!("{}\\{}", archive.to_string_lossy(), inner.replace('/', "\\"))
                }
            }
            Location::Network { server: None } => t("Netzwerk", "Network").to_owned(),
            Location::Network { server: Some(server) } => format!("\\\\{server}"),
            Location::RecycleBin => t("Papierkorb", "Recycle Bin").to_owned(),
            Location::Device { name, path, .. } => format!("{name}{}", path.trim_end_matches('/').replace('/', "\\")),
        }
    }

    /// Brotkrumen: (Beschriftung, Ort).
    pub fn crumbs(&self) -> Vec<(String, Location)> {
        let mut out = Vec::new();
        let mut cur = Some(self.clone());
        while let Some(loc) = cur {
            let label = loc.leaf();
            cur = match &loc {
                // Über „Dieser PC" gibt es nichts mehr.
                Location::Drives => None,
                _ => loc.parent(),
            };
            out.push((label, loc));
        }
        out.reverse();
        out
    }

    pub fn local_path(&self) -> Option<&Path> {
        match self {
            Location::Local(p) => Some(p),
            _ => None,
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Location::Local(_))
    }


    /// Kann hier geschrieben werden (Neuer Ordner, Einfügen, Umbenennen)?
    pub fn writable(&self) -> bool {
        matches!(self, Location::Local(_))
    }

    /// Lassen sich Einträge von hier löschen oder wegschieben?
    pub fn removable(&self) -> bool {
        self.writable()
    }

    /// Wandelt Adresszeilen-Text zurück in einen Ort.
    pub fn parse(text: &str) -> Option<Location> {
        let t = text.trim().trim_matches('"');
        if t.is_empty() {
            return None;
        }
        if t.eq_ignore_ascii_case("dieser pc") || t.eq_ignore_ascii_case("this pc") {
            return Some(Location::Drives);
        }
        if t.eq_ignore_ascii_case("netzwerk") || t.eq_ignore_ascii_case("network") {
            return Some(Location::Network { server: None });
        }
        if t.eq_ignore_ascii_case("papierkorb") || t.eq_ignore_ascii_case("recycle bin") {
            return Some(Location::RecycleBin);
        }
        // „\\NAS01" ohne Freigabe: die Freigaben dieses Computers zeigen.
        if let Some(rest) = t.strip_prefix("\\\\") {
            let rest = rest.trim_end_matches('\\');
            if !rest.is_empty() && !rest.contains('\\') {
                return Some(Location::Network { server: Some(rest.to_owned()) });
            }
        }
        let expanded = expand_env(t);
        let mut path = PathBuf::from(&expanded);
        if expanded.len() == 2 && expanded.ends_with(':') {
            path = PathBuf::from(format!("{expanded}\\"));
        }
        if path.is_dir() {
            return Some(Location::Local(path));
        }
        // Pfad in ein ZIP hinein: längsten existierenden Datei-Präfix suchen.
        let mut probe = path.clone();
        loop {
            if probe.is_file() {
                if is_zip_name(&probe.to_string_lossy()) {
                    let inner = path
                        .strip_prefix(&probe)
                        .ok()
                        .map(|p| p.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_default();
                    return Some(Location::Zip { archive: probe, inner });
                }
                // Datei angegeben: deren Ordner öffnen.
                return probe.parent().map(|p| Location::Local(p.to_path_buf()));
            }
            if !probe.pop() {
                return None;
            }
        }
    }
}

/// Ersetzt `%NAME%` durch den Wert der Umgebungsvariablen; unbekannte bleiben stehen.
pub fn expand_env(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(val) if !name.is_empty() => out.push_str(&val),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Ist das ein Archiv, das sich wie ein Ordner öffnen lässt (ZIP, TAR, 7z, RAR)?
pub fn is_zip_name(name: &str) -> bool {
    crate::archive::kind(name).is_some()
}

// ---------------------------------------------------------------- Einträge

/// Wo der Inhalt einer Datei aus einem Cloud-Ordner liegt – abgelesen an ihren Attributen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Cloud {
    /// Keine Angabe; bei Dateien in einem Cloud-Ordner heißt das: der Inhalt liegt auf diesem PC.
    #[default]
    Unknown,
    /// Nur in der Cloud: der Inhalt wird beim Öffnen geholt.
    Online,
    /// Soll immer auf diesem PC bleiben.
    Pinned,
}

impl Cloud {
    const OFFLINE: u32 = 0x1000;
    const RECALL_ON_OPEN: u32 = 0x4_0000;
    pub const PINNED: u32 = 0x8_0000;
    pub const UNPINNED: u32 = 0x10_0000;
    const RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

    /// OneDrive kennzeichnet Dateien, die nur online liegen, mit „wird beim Zugriff geholt"; Dropbox setzt dazu
    /// „nicht behalten" – auch an Ordnern. Stehen „behalten" und „nicht behalten" zugleich da (so bei Dateien, die
    /// ein Dienst gar nicht abgleicht), sagt das nichts.
    pub fn from_attrs(attrs: u32) -> Cloud {
        let (pinned, unpinned) = (attrs & Self::PINNED != 0, attrs & Self::UNPINNED != 0);
        if attrs & (Self::OFFLINE | Self::RECALL_ON_OPEN | Self::RECALL_ON_DATA_ACCESS) != 0 || (unpinned && !pinned) {
            Cloud::Online
        } else if pinned && !unpinned {
            Cloud::Pinned
        } else {
            Cloud::Unknown
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    /// Gefalteter Schlüssel für Sortierung und Filter.
    pub key: String,
    pub is_dir: bool,
    /// „.."-Zeile
    pub is_parent: bool,
    pub size: u64,
    /// Unix-Sekunden
    pub modified: Option<i64>,
    pub hidden: bool,
    pub link: bool,
    /// Suchtreffer: Ordner relativ zum Suchstart. Laufwerke: Beschriftung.
    pub note: Option<Arc<str>>,
    /// Einträge in Übersichten (Dieser PC, Netzwerk) führen zu diesem Ort.
    pub target: Option<Location>,
    /// Eigenes Symbol in Übersichten (Telefon, Papierkorb); sonst ergibt es sich aus dem Ziel.
    pub icon: Option<crate::icons::Cat>,
    pub cloud: Cloud,
}

impl Entry {
    pub fn new(name: String, is_dir: bool, size: u64, modified: Option<i64>) -> Self {
        let key = fold_key(&name);
        Self { name, key, is_dir, is_parent: false, size, modified, hidden: false, link: false, note: None, target: None, icon: None, cloud: Cloud::Unknown }
    }

    pub fn parent_row() -> Self {
        Self {
            name: "..".to_owned(),
            key: String::new(),
            is_dir: true,
            is_parent: true,
            size: 0,
            modified: None,
            hidden: false,
            link: false,
            note: None,
            target: None,
            icon: None,
            cloud: Cloud::Unknown,
        }
    }

    /// Eintrag einer Übersicht: Name, zwei Beschreibungsspalten und das Ziel beim Öffnen.
    pub fn overview(name: impl Into<String>, description: &str, detail: &str, target: Location) -> Self {
        let mut e = Self::new(name.into(), true, 0, None);
        e.note = Some(Arc::from(format!("{description}\u{1f}{detail}")));
        e.target = Some(target);
        e
    }

    /// Endung in Kleinbuchstaben, ohne Punkt; Ordner haben keine.
    pub fn ext(&self) -> &str {
        if self.is_dir { "" } else { split_ext(&self.key).1 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortKey {
    Name,
    Ext,
    Size,
    Date,
}

/// „.." zuerst, dann Ordner, dann Dateien – jeweils nach dem gewählten Schlüssel.
pub fn sort_entries(entries: &mut [Entry], key: SortKey, desc: bool) {
    use std::cmp::Ordering;
    entries.sort_by(|a, b| {
        if a.is_parent != b.is_parent {
            return if a.is_parent { Ordering::Less } else { Ordering::Greater };
        }
        if a.is_dir != b.is_dir {
            return if a.is_dir { Ordering::Less } else { Ordering::Greater };
        }
        let primary = match key {
            SortKey::Name => Ordering::Equal,
            SortKey::Ext => a.ext().cmp(b.ext()),
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Date => a.modified.cmp(&b.modified),
        };
        let ord = primary.then_with(|| natural_cmp(&a.key, &b.key));
        if desc { ord.reverse() } else { ord }
    });
}

pub struct Listing {
    pub entries: Vec<Entry>,
    /// (frei, gesamt)
    pub space: Option<(u64, u64)>,
    /// Namen leerer Unterordner, falls beim Auflisten schon bekannt (ZIP); sonst wird nachträglich geprüft.
    pub empty_dirs: Option<Vec<String>>,
    /// Es folgt noch eine vollständigere Liste (Netzwerksuche läuft weiter).
    pub pending: bool,
    /// Änderungszeit des Ordners selbst, unmittelbar vor dem Lesen: daran lässt sich später erkennen, ob in ihm
    /// etwas hinzukam, verschwand oder umbenannt wurde, ohne ihn neu zu lesen (nur lokale Ordner).
    pub dir_modified: Option<std::time::SystemTime>,
}

/// Einträge der Netzwerkumgebung: Computer (ohne `server`) oder die Freigaben eines Computers.
pub fn network_listing(names: &[String], server: Option<&str>, pending: bool) -> Listing {
    let entries = names
        .iter()
        .map(|name| match server {
            None => Entry::overview(name.as_str(), "Computer", "", Location::Network { server: Some(name.clone()) }),
            Some(server) => {
                let path = format!("\\\\{server}\\{name}");
                Entry::overview(name.as_str(), t("Freigabe", "Share"), &path, Location::Local(PathBuf::from(&path)))
            }
        })
        .collect();
    Listing { entries, space: None, empty_dirs: None, pending, dir_modified: None }
}

pub fn list_local(path: &Path) -> Result<Listing, String> {
    use std::os::windows::fs::MetadataExt;
    // Vor dem Lesen: kommt währenddessen etwas hinzu, ist die Zeit des Ordners danach neuer als die gemerkte.
    let dir_modified = dir_modified(path);
    let rd = std::fs::read_dir(path).map_err(|e| io_msg(&e))?;
    let mut entries = Vec::with_capacity(256);
    for item in rd {
        let Ok(item) = item else { continue };
        let name = item.file_name().to_string_lossy().into_owned();
        // Unter Windows stammen die Metadaten direkt aus dem Verzeichniseintrag (kein Extra-Zugriff).
        let Ok(meta) = item.metadata() else {
            entries.push(Entry::new(name, false, 0, None));
            continue;
        };
        let attrs = meta.file_attributes();
        let is_link = attrs & 0x400 != 0; // Reparse-Punkt (Verknüpfung/Junction)
        let is_dir = attrs & 0x10 != 0;
        let modified = meta.modified().ok().map(system_time_secs);
        let mut e = Entry::new(name, is_dir, if is_dir { 0 } else { meta.len() }, modified);
        e.hidden = attrs & 0x2 != 0;
        e.link = is_link;
        e.cloud = Cloud::from_attrs(attrs);
        entries.push(e);
    }
    Ok(Listing { entries, space: win::disk_space(path), empty_dirs: None, pending: false, dir_modified })
}

/// Änderungszeit eines Ordners: Windows setzt sie neu, wenn in ihm etwas angelegt, gelöscht oder umbenannt wird.
pub fn dir_modified(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok().and_then(|meta| meta.modified().ok())
}

pub fn list_drives() -> Listing {
    let mut entries: Vec<Entry> = win::drives()
        .into_iter()
        .map(|d| {
            let label = if d.label.is_empty() { d.kind_text().to_owned() } else { format!("{} · {}", d.label, d.kind_text()) };
            let space = if d.total > 0 {
                tf!("{} frei von {}", "{} free of {}", crate::util::fmt_size(d.free), crate::util::fmt_size(d.total))
            } else {
                t("nicht bereit", "not ready").to_owned()
            };
            let mut e = Entry::overview(d.letter(), &label, &space, Location::Local(PathBuf::from(&d.root)));
            e.size = d.total;
            e
        })
        .collect();
    for (name, path) in win::cloud_folders() {
        let detail = path.to_string_lossy().into_owned();
        entries.push(Entry::overview(win::folder_label(name), t("Cloud-Ordner", "Cloud folder"), &detail, Location::Local(path)));
    }
    // Was Windows sonst noch neben „Dieser PC" einhängt: verbundene Telefone und weitere Cloud-Dienste.
    for place in win::places() {
        let detail = place.path.to_string_lossy().into_owned();
        let kind = if place.phone { t("Smartphone", "Phone") } else { t("Cloud-Ordner", "Cloud folder") };
        let mut e = Entry::overview(place.name, kind, &detail, Location::Local(place.path));
        e.icon = place.phone.then_some(crate::icons::Cat::Phone);
        entries.push(e);
    }
    // Per Kabel angeschlossene Telefone und Kameras
    for (id, name) in crate::device::devices() {
        let target = Location::Device { id, name: name.clone(), path: "/".to_owned() };
        let mut e = Entry::overview(name, t("Gerät am Kabel", "Device on a cable"), t("nur lesen", "read-only"), target);
        e.icon = Some(crate::icons::Cat::Device);
        entries.push(e);
    }
    let mut bin = Entry::overview(
        t("Papierkorb", "Recycle Bin"),
        t("Gelöschtes ansehen, wiederherstellen, leeren", "View, restore and empty deleted items"),
        "",
        Location::RecycleBin,
    );
    bin.icon = Some(crate::icons::Cat::Bin);
    entries.push(bin);
    entries.push(Entry::overview(
        t("Netzwerk", "Network"),
        t("Computer und Freigaben im Netzwerk", "Computers and shares on the network"),
        "",
        Location::Network { server: None },
    ));
    Listing { entries, space: None, empty_dirs: None, pending: false, dir_modified: None }
}

/// Prüft im Hintergrund, welche der genannten Unterordner leer sind, und meldet sie schubweise.
/// „Leer" heißt: nichts, was in der Liste zu sehen wäre – versteckte Dateien zählen nur, wenn sie angezeigt werden.
pub fn spawn_empty_check(
    tab: u64,
    loc: Location,
    dirs: Vec<String>,
    show_hidden: bool,
    bus: Bus,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) {
    use std::os::windows::fs::MetadataExt;
    use std::sync::atomic::Ordering::Relaxed;
    let Some(base) = loc.local_path().map(PathBuf::from) else { return };
    if dirs.is_empty() {
        bus.send(Msg::EmptyDirs { tab, loc, names: Vec::new(), done: true });
        return;
    }
    std::thread::spawn(move || {
        let mut batch: Vec<String> = Vec::new();
        let mut last = std::time::Instant::now();
        for name in dirs {
            if cancel.load(Relaxed) {
                return;
            }
            // Nicht lesbare Ordner gelten nicht als leer.
            let Ok(mut content) = std::fs::read_dir(base.join(&name)) else { continue };
            let has_visible = content.any(|item| match item {
                Ok(item) => show_hidden || item.metadata().map_or(true, |m| m.file_attributes() & 0x2 == 0),
                Err(_) => true,
            });
            if !has_visible {
                batch.push(name);
            }
            if !batch.is_empty() && last.elapsed() > std::time::Duration::from_millis(120) {
                last = std::time::Instant::now();
                bus.send(Msg::EmptyDirs { tab, loc: loc.clone(), names: std::mem::take(&mut batch), done: false });
            }
        }
        bus.send(Msg::EmptyDirs { tab, loc, names: batch, done: true });
    });
}

pub fn io_msg(e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => t("Der Ordner wurde nicht gefunden.", "The folder was not found.").to_owned(),
        PermissionDenied => t("Zugriff verweigert.", "Access denied.").to_owned(),
        _ => match e.raw_os_error() {
            Some(21) => t("Das Gerät ist nicht bereit.", "The device is not ready.").to_owned(),
            Some(53) | Some(67) => t("Der Netzwerkpfad wurde nicht gefunden.", "The network path was not found.").to_owned(),
            Some(32) => t("Die Datei wird von einem anderen Programm verwendet.", "The file is in use by another program.")
                .to_owned(),
            Some(112) => t("Auf dem Datenträger ist nicht genug Platz.", "There is not enough space on the disk.").to_owned(),
            Some(183) => t("Ein Element mit diesem Namen existiert bereits.", "An item with this name already exists.")
                .to_owned(),
            Some(145) => t("Der Ordner ist nicht leer.", "The folder is not empty.").to_owned(),
            _ => e.to_string(),
        },
    }
}

// ---------------------------------------------------------------- Nachrichten aus dem Hintergrund

pub enum Msg {
    Listed { tab: u64, generation: u64, loc: Location, result: Result<Listing, String> },
    DirSize { tab: u64, loc: Location, name: String, size: u64, done: bool },
    /// Leere Unterordner des angezeigten Ordners; `done` schließt die Prüfung ab.
    EmptyDirs { tab: u64, loc: Location, names: Vec<String>, done: bool },
    /// `checked`: so viele Dateien hat die Suche bisher angesehen
    SearchHits { tab: u64, search: u64, hits: Vec<Entry>, checked: u64 },
    SearchDone { tab: u64, search: u64 },
    Preview { key: crate::preview::PreviewKey, data: crate::preview::PreviewData },
    /// Ein Programmsymbol ist geladen (oder die Datei hat keines)
    Icon { key: crate::shellicon::IconKey, image: Option<eframe::egui::ColorImage> },
    FsChanged(PathBuf),
    JobFinished { id: u64 },
    Drives(Vec<win::Drive>),
    /// Per Kabel angeschlossene Geräte: (Kennung, Name)
    Devices(Vec<(String, String)>),
    /// Die Ordner aus dem Schnellzugriff des Explorers sind gelesen.
    QuickAccess(Vec<(String, PathBuf)>),
    /// Ob das Laufwerk `root` (z. B. `D:\`) einen Papierkorb hat
    RecycleBin { root: String, has: bool },
    /// Eine Miniatur für die Rasteransicht ist geladen (oder die Kachel wurde übersprungen).
    Thumb { request: crate::thumbs::Request, loaded: crate::thumbs::Loaded },
    /// Von außen im Fenster abgelegt. `pos`: Stelle im Fenster in Pixeln. `temp`: die Dateien wurden gerade erst für
    /// diese Ablage in einen Zwischenordner geschrieben (Link aus dem Browser, Anhang) und gehören dort nicht hin.
    Dropped { paths: Vec<PathBuf>, pos: Option<(f32, f32)>, shift: bool, ctrl: bool, temp: bool },
    Toast { text: String, error: bool },
    /// Eine entfernte/gepackte Datei wurde in den Temp-Ordner geholt und soll geöffnet werden.
    OpenTemp(PathBuf),
    /// Ansichten dieses Ordners neu laden und optional den Cursor auf einen Namen setzen.
    Refresh { loc: Location, focus: Option<String> },
    /// Im Öffnen-Dialog wurde eine Sicherungsdatei gewählt.
    BackupPicked(PathBuf),
    /// Ergebnis eines Umbenennens: erfolgte Paare (alt, neu) und Fehlermeldungen.
    Renamed { loc: Location, done: Vec<(String, String)>, errors: Vec<String>, undo: bool },
}

/// Kanal zurück in die Oberfläche; weckt das Fenster nach jeder Nachricht.
#[derive(Clone)]
pub struct Bus {
    tx: Sender<Msg>,
    ctx: eframe::egui::Context,
}

impl Bus {
    pub fn new(tx: Sender<Msg>, ctx: eframe::egui::Context) -> Self {
        Self { tx, ctx }
    }

    pub fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }

    pub fn error(&self, text: impl Into<String>) {
        self.send(Msg::Toast { text: text.into(), error: true });
    }

    pub fn repaint(&self) {
        self.ctx.request_repaint();
    }

    pub fn repaint_after(&self, wait: std::time::Duration) {
        self.ctx.request_repaint_after(wait);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_is_walked_like_a_folder_tree() {
        let root = Location::Device { id: "usb#1".into(), name: "Kamera".into(), path: "/".into() };
        assert_eq!(root.parent(), Some(Location::Drives));
        assert_eq!(root.leaf(), "Kamera");
        let deep = root.child("Interner Speicher").child("DCIM");
        assert_eq!(deep, Location::Device { id: "usb#1".into(), name: "Kamera".into(), path: "/Interner Speicher/DCIM".into() });
        assert_eq!(deep.leaf(), "DCIM");
        assert_eq!(deep.display(), "Kamera\\Interner Speicher\\DCIM");
        assert_eq!(deep.parent().and_then(|up| up.parent()), Some(root.clone()));
        assert_eq!(deep.branch_below(&root), Some("Interner Speicher".into()));
        assert!(!deep.writable() && !deep.removable() && !deep.is_virtual());
    }

    #[test]
    fn cloud_state_is_read_from_the_attributes() {
        // Werte, wie sie in OneDrive- und Dropbox-Ordnern vorkommen
        assert_eq!(Cloud::from_attrs(0x40_1400), Cloud::Online); // OneDrive: nur online
        assert_eq!(Cloud::from_attrs(0x50_1400), Cloud::Online); // Dropbox: nur online
        assert_eq!(Cloud::from_attrs(0x10_0410), Cloud::Online); // Dropbox: Ordner nur online
        assert_eq!(Cloud::from_attrs(0x8_0420), Cloud::Pinned); // immer behalten
        assert_eq!(Cloud::from_attrs(0x420), Cloud::Unknown); // liegt auf dem PC
        assert_eq!(Cloud::from_attrs(0x18_0020), Cloud::Unknown); // wird nicht abgeglichen
        assert_eq!(Cloud::from_attrs(0x20), Cloud::Unknown);
    }

    /// Zum Nachsehen von Hand: zählt, welchen Cloud-Zustand die Einträge eines Ordners melden – ohne Namen.
    /// `TC_DIR=C:\Users\x\OneDrive cargo test cloud_states_of_a_folder -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn cloud_states_of_a_folder() {
        let Ok(dir) = std::env::var("TC_DIR") else { return };
        let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
        let mut waiting = vec![(PathBuf::from(dir), 0)];
        while let Some((folder, depth)) = waiting.pop() {
            let Ok(listing) = list_local(&folder) else { continue };
            for e in listing.entries {
                *counts.entry(format!("{} {:?}", if e.is_dir { "Ordner" } else { "Datei " }, e.cloud)).or_default() += 1;
                if e.is_dir && depth < 2 {
                    waiting.push((folder.join(&e.name), depth + 1));
                }
            }
        }
        println!("{counts:#?}");
    }

    #[test]
    fn zip_parent() {
        let z = Location::Zip { archive: PathBuf::from("C:\\x\\a.zip"), inner: "a/b".into() };
        assert_eq!(z.parent(), Some(Location::Zip { archive: PathBuf::from("C:\\x\\a.zip"), inner: "a".into() }));
        let top = Location::Zip { archive: PathBuf::from("C:\\x\\a.zip"), inner: String::new() };
        assert_eq!(top.parent(), Some(Location::Local(PathBuf::from("C:\\x"))));
    }

    #[test]
    fn network_paths_climb_to_computer_and_network() {
        let share = Location::Local(PathBuf::from("\\\\NAS01\\filme"));
        let server = Location::Network { server: Some("NAS01".into()) };
        let network = Location::Network { server: None };
        assert_eq!(Location::Local(PathBuf::from("\\\\NAS01\\filme\\2024")).parent(), Some(share.clone()));
        assert_eq!(share.parent(), Some(server.clone()));
        assert_eq!(server.parent(), Some(network.clone()));
        assert_eq!(network.parent(), Some(Location::Drives));
        assert_eq!(share.leaf(), "filme");
        assert_eq!(server.child("filme"), share);
        assert_eq!(network.child("NAS01"), server);
        let labels: Vec<String> = share.crumbs().into_iter().map(|(label, _)| label).collect();
        assert_eq!(labels, ["Dieser PC", "Netzwerk", "NAS01", "filme"]);
        assert_eq!(Location::parse("\\\\NAS01"), Some(server));
        assert_eq!(Location::parse("Netzwerk"), Some(network));
        assert_eq!(Location::parse("Papierkorb"), Some(Location::RecycleBin));
        assert_eq!(Location::RecycleBin.parent(), Some(Location::Drives));
        assert_eq!(Location::Local(PathBuf::from("C:\\")).leaf(), "C:");
    }

    #[test]
    fn branch_below_names_the_folder_we_came_from() {
        let deep = Location::Local(PathBuf::from("D:\\Claude\\Tutorial2Short\\Done"));
        assert_eq!(deep.branch_below(&Location::Local(PathBuf::from("D:\\Claude\\Tutorial2Short"))), Some("Done".into()));
        assert_eq!(deep.branch_below(&Location::Local(PathBuf::from("D:\\Claude"))), Some("Tutorial2Short".into()));
        assert_eq!(deep.branch_below(&Location::Local(PathBuf::from("D:\\"))), Some("Claude".into()));
        assert_eq!(deep.branch_below(&Location::Drives), Some("D:".into()));
        assert_eq!(deep.branch_below(&Location::Local(PathBuf::from("C:\\Anders"))), None);
        assert_eq!(deep.branch_below(&deep), None);
    }

    #[test]
    fn local_root_parent_is_drives() {
        assert_eq!(Location::Local(PathBuf::from("C:\\")).parent(), Some(Location::Drives));
        assert_eq!(Location::Drives.child("D:"), Location::Local(PathBuf::from("D:\\")));
    }

    #[test]
    fn sorting_puts_dirs_first() {
        let mut v = vec![
            Entry::new("b.txt".into(), false, 5, None),
            Entry::new("Zeta".into(), true, 0, None),
            Entry::new("a10.txt".into(), false, 1, None),
            Entry::new("a2.txt".into(), false, 9, None),
            Entry::parent_row(),
        ];
        sort_entries(&mut v, SortKey::Name, false);
        let names: Vec<_> = v.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["..", "Zeta", "a2.txt", "a10.txt", "b.txt"]);
        sort_entries(&mut v, SortKey::Size, true);
        assert_eq!(v[2].name, "a2.txt");
    }
}
