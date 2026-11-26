//! Ein Dateifenster: Tabs, Adresszeile, Spaltenkopf, virtuelle Dateiliste und Statuszeile.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::Instant;

use eframe::egui::{
    self, Align2, CornerRadius, CursorIcon, DragAndDrop, Id, Key, Modifiers, Pos2, Rect, Sense, Stroke, StrokeKind,
    Ui, UiBuilder, Vec2,
};

use crate::config::{Col, Config, PaneState, TabState};
use crate::icons::{self, Cat};
use crate::lang::{key, t};
use crate::ops::{self, Source};
use crate::tf;
use crate::theme::{self, Palette};
use crate::util::{NameFilter, fmt_size, fmt_time, fold_key, split_ext};
use crate::vfs::{self, Bus, Entry, Listing, Location, Msg, SortKey, sort_entries};
use crate::widgets::{draw_text, icon_button, menu_begin, menu_caption, menu_item, menu_separator, progress_bar, text_width};
use crate::{search, win};

pub const TAB_H: f32 = 32.0;
pub const PATH_H: f32 = 38.0;
pub const HEAD_H: f32 = 26.0;
pub const ROW_H: f32 = 24.0;
/// Rasteransicht: die Kantenlänge der Fläche für die Miniatur ist einstellbar (Strg+Mausrad, Regler in der Statuszeile).
pub const THUMB_MIN: f32 = 56.0;
pub const THUMB_MAX: f32 = 280.0;
pub const THUMB_DEFAULT: f32 = 104.0;

/// Mindestbreite und Höhe einer Kachel für diese Miniaturgröße
fn tile_size(thumb: f32) -> (f32, f32) {
    (thumb + 22.0, thumb + 38.0)
}

/// So groß wird eine Miniatur angefordert: die nächste Stufe über dem Bedarf. Stufen statt krummer Werte, damit
/// beim Ziehen am Regler nicht für jede Zwischengröße alles neu geladen wird.
fn thumb_pixels(wanted: f32) -> u32 {
    const STEPS: [u32; 10] = [64, 80, 104, 128, 160, 208, 256, 320, 416, 512];
    STEPS.into_iter().find(|step| *step as f32 >= wanted.round()).unwrap_or(512)
}
pub const STATUS_H: f32 = 25.0;

/// Ab so vielen Zeichen sucht das Suchfeld von selbst in Unterordnern (Enter erzwingt es auch darunter).
const SEARCH_MIN_CHARS: usize = 2;
/// Tipp-Pause in Sekunden, nach der die Suche in Unterordnern startet
const SEARCH_DELAY: f64 = 0.25;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Relaxed)
}

/// Was Hintergrundarbeit braucht: Rückkanal, Einstellungen.
pub struct Env<'a> {
    pub bus: &'a Bus,
    pub cfg: &'a mut Config,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    Open,
    OpenWith,
    OpenInNewTab,
    OpenInOther,
    Reveal,
    Properties,
    Copy,
    Move,
    Delete,
    DeletePermanent,
    Rename,
    NewFolder,
    NewFile,
    ClipCopy,
    ClipCut,
    ClipPaste,
    CopyPath,
    Refresh,
    Pack,
    Extract,
    CalcSizes,
    CalcAllSizes,
    SelectAll,
    InvertSelection,
    AddFavorite,
    FavoriteSelected,
    /// Die Ordner aus dem Schnellzugriff des Explorers als Favoriten übernehmen
    ImportQuickAccess,
    /// Im Papierkorb: die gewählten Elemente an ihren ursprünglichen Ort zurücklegen
    BinRestore,
    /// Den Papierkorb leeren (fragt vorher)
    BinEmpty,
    /// In Cloud-Ordnern: die gewählten Dateien immer auf diesem PC behalten
    CloudKeep,
    /// In Cloud-Ordnern: den Platz der gewählten Dateien freigeben (sie bleiben online)
    CloudFree,
    ToggleHidden,
    /// Liste oder Rasteransicht mit Miniaturen
    ToggleGrid,
    QuickView,
    ToggleLayout,
    ToggleTheme,
    ToggleLang,
    SwapPanes,
    SameFolder,
    /// Die letzte Dateioperation oder das letzte Umbenennen zurücknehmen
    Undo,
    FocusFilter,
    /// Eine Funktion der Vollversion wurde aufgerufen (Tastenkürzel): kurzer Hinweis, wo es sie gibt
    FullOnly(&'static str),
    /// Übersicht: was die Vollversion zusätzlich kann, und wo es sie gibt
    FullVersion,
    /// Einstellen, wovor nachgefragt wird (Kopieren, Verschieben, Papierkorb, endgültig löschen)
    Asking,
    EditPath,
    NewTab,
    CloseTab,
    Back,
    Forward,
    Up,
    Shortcuts,
    BackupSave,
    BackupRestore,
    GoTo(Location),
}

pub enum Action {
    Activate(usize),
    Cmd(usize, Cmd),
    Rename { pane: usize, old: String, new: String },
    OpenRemote(Source),
    /// Eine Sicherungsdatei wurde geöffnet (Doppelklick/Enter)
    OpenBackup(PathBuf),
    Drop { pane: usize, dest: Location, payload: Arc<DragPayload>, mods: Modifiers },
    /// Ein Tab wurde außerhalb seines Fensters losgelassen (Kennung des Tabs, Stelle der Maus)
    MoveTab { from: usize, tab: u64, pos: Pos2 },
}

pub struct DragPayload {
    pub sources: Vec<Source>,
    pub from_tab: u64,
}

pub struct DirSize {
    pub size: u64,
    pub done: bool,
}

pub struct SearchState {
    pub id: u64,
    pub query: String,
    pub running: bool,
    /// So viele Dateien hat die Suche bisher angesehen
    pub checked: u64,
    pub cancel: Arc<AtomicBool>,
}

impl SearchState {
    /// Wonach gesucht wird, für die Statuszeile: „*.txt"
    pub fn what(&self) -> String {
        tf!("„{}\"", "“{}”", self.query)
    }
}

/// Laufendes Auswahlrechteck („Gummiband") in der Dateiliste.
struct Band {
    /// x in Bildschirmkoordinaten, y als Position im gesamten Listeninhalt (bleibt beim Scrollen stehen)
    anchor: Pos2,
    /// Markierungen beim Start; mit Strg/Umschalt baut das Rechteck darauf auf
    base: Vec<bool>,
    /// Strg: Einträge im Rechteck umschalten statt hinzufügen
    toggle: bool,
    last: Option<[usize; 4]>,
}

pub struct InlineRename {
    pub entry: usize,
    pub text: String,
    pub fresh: bool,
}

pub struct Tab {
    pub id: u64,
    pub loc: Location,
    pub entries: Vec<Entry>,
    /// Sichtbare Zeilen (nach Filter) als Indizes in `entries`
    pub rows: Vec<u32>,
    pub marked: Vec<bool>,
    pub marked_count: usize,
    pub cursor: usize,
    /// Nach einem Klick ins Leere ist nichts gewählt: die Zeile unter dem Cursor wird nicht hervorgehoben, und
    /// Befehle wirken auf nichts. Der Cursor bleibt stehen, damit die Pfeiltasten von dort weitergehen; sobald er
    /// bewegt oder ein Eintrag angeklickt wird, gilt die Zeile wieder als gewählt.
    pub unselected: bool,
    pub anchor: usize,
    pub sort: SortKey,
    pub desc: bool,
    pub filter: String,
    pub back: Vec<(Location, Option<String>)>,
    pub forward: Vec<(Location, Option<String>)>,
    pub loading: bool,
    pub load_started: Instant,
    pub generation: u64,
    pub error: Option<String>,
    /// Der Ort, dessen Inhalt gerade angezeigt wird – erst gesetzt, wenn er sich lesen ließ. Nur solche Ordner
    /// werden auf Änderungen überwacht; beim Wechsel in einen anderen Ordner gilt bis dahin noch der alte.
    pub listed: Option<Location>,
    /// Änderungszeit des angezeigten Ordners, wie sie beim Lesen war (siehe `Listing::dir_modified`)
    pub dir_modified: Option<std::time::SystemTime>,
    /// Der Ordner hat sich geändert, während dieser Tab im Hintergrund lag: neu laden, sobald er gezeigt wird.
    pub stale: bool,
    pub space: Option<(u64, u64)>,
    pub scroll_to: Option<f32>,
    pub scroll_y: f32,
    pub view_h: f32,
    /// Nach dem Laden den Cursor auf diesen Namen setzen
    pub focus_name: Option<String>,
    /// Frisch navigiert: Auswahl und Scrollposition nicht übernehmen
    pub fresh: bool,
    pub sizes: HashMap<String, DirSize>,
    pub size_cancel: Arc<AtomicBool>,
    /// Namen leerer Unterordner (werden ausgegraut dargestellt)
    pub empty_dirs: HashSet<String>,
    /// Ergebnis der gerade laufenden Prüfung; ersetzt `empty_dirs`, sobald sie fertig ist
    empty_next: HashSet<String>,
    empty_cancel: Arc<AtomicBool>,
    /// Sortierung und Darstellung, die vor dem Wechsel in den Papierkorb galten (Schlüssel, absteigend, Raster)
    before_own_view: Option<(SortKey, bool, bool)>,
    /// Der gezeigte Ordner liegt in einem Cloud-Ordner (OneDrive, Dropbox …)
    pub in_cloud: bool,
    /// Gruppen mit Überschrift: (erste Zeile, Beschriftung). Leer: keine Gruppen. (In der Lite-Ausgabe immer leer;
    /// die Vollversion gliedert damit die Galerie nach Monaten.)
    groups: Vec<(usize, String)>,
    /// Wo die Gruppen liegen, mit der Aufteilung der zuletzt gezeichneten Liste (siehe `plan_layout`)
    spans: Vec<Span>,
    pub search: Option<SearchState>,
    /// Zeitpunkt, zu dem die Suche in Unterordnern nach einer Tipp-Pause startet
    pub search_due: Option<f64>,
    pub rename: Option<InlineRename>,
    /// Zeile, auf deren Namen gerade gedrückt wurde, obwohl sie schon gewählt war (Kandidat fürs Umbenennen)
    rename_armed: Option<usize>,
    /// Name und Zeitpunkt: startet das Umbenennen, wenn kein Doppelklick mehr daraus wird
    rename_due: Option<(String, f64)>,
    band: Option<Band>,
    typeahead: String,
    typeahead_at: f64,
    /// Rasteransicht mit Miniaturen statt der Liste
    pub grid: bool,
    /// Wie zuletzt gezeichnet: Einträge je Zeile (in der Liste 1), Höhe einer Zeile, Breite einer Kachel
    per_line: usize,
    line_h: f32,
    tile_w: f32,
    /// Nach dem Wechsel der Darstellung: den Cursor ins Bild holen, sobald die neue Aufteilung feststeht
    reveal_pending: bool,
    /// Miniaturgröße, mit der die Rasteransicht zuletzt gezeichnet wurde (0 = Liste)
    thumb_seen: f32,
    /// Während die Miniaturgröße geändert wird: der Eintrag, der dabei oben stehen bleibt, und der Zeitpunkt der
    /// letzten Änderung
    zoom_anchor: Option<(usize, f64)>,
}

/// Höhe der Überschrift über einer Gruppe
const GROUP_H: f32 = 36.0;

/// Eine Gruppe von Zeilen unter einer Überschrift: `first` und `len` zählen in `Tab::rows`, `top` ist die Oberkante
/// der Überschrift im gesamten Inhalt.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    first: usize,
    len: usize,
    top: f32,
}

/// Eine Zeile der Liste, wie sie gezeichnet wird: eine Überschrift (`title`) oder Einträge – in der Liste einer,
/// in der Rasteransicht so viele, wie nebeneinander passen. `top`: Oberkante im gesamten Inhalt.
struct Line<'a> {
    top: f32,
    rows: std::ops::Range<usize>,
    title: Option<&'a str>,
}

/// Orte mit eigener Darstellung: (Sortierung, absteigend, Raster). Der Papierkorb zeigt eine Liste, das zuletzt
/// Gelöschte oben.
fn own_view(loc: &Location) -> Option<(SortKey, bool, bool)> {
    match loc {
        Location::RecycleBin => Some((SortKey::Date, true, false)),
        _ => None,
    }
}

impl Tab {
    /// Nummer des Eintrags an einer Stelle: `x` ab dem linken Rand der Liste, `y` im gesamten Inhalt.
    fn row_at(&self, x: f32, y: f32) -> Option<usize> {
        if y < 0.0 {
            return None;
        }
        let col = if self.per_line > 1 { (((x - 4.0) / self.tile_w).floor().max(0.0) as usize).min(self.per_line - 1) } else { 0 };
        if self.spans.is_empty() {
            let line = (y / self.line_h).floor() as usize;
            return Some(line * self.per_line + col);
        }
        // Mit Gruppen: auf einer Überschrift und hinter dem letzten Eintrag einer Gruppe liegt nichts.
        let span = self.spans.iter().rev().find(|span| span.top <= y)?;
        let inside = y - span.top - GROUP_H;
        if inside < 0.0 {
            return None;
        }
        let row = span.first + (inside / self.line_h).floor() as usize * self.per_line + col;
        (row < span.first + span.len).then_some(row)
    }

    /// Die Gruppe, in der eine Zeile liegt (ohne Gruppen: keine).
    fn span_of(&self, row: usize) -> Option<&Span> {
        self.spans.partition_point(|span| span.first <= row).checked_sub(1).map(|i| &self.spans[i])
    }

    /// Oberkante einer Zeile im gesamten Inhalt.
    fn row_top(&self, row: usize) -> f32 {
        match self.span_of(row) {
            Some(span) => span.top + GROUP_H + ((row - span.first) / self.per_line) as f32 * self.line_h,
            None => (row / self.per_line) as f32 * self.line_h,
        }
    }

    /// Spalte einer Zeile in der Rasteransicht (in der Liste immer 0).
    fn row_col(&self, row: usize) -> usize {
        match self.span_of(row) {
            Some(span) => (row - span.first) % self.per_line,
            None => row % self.per_line,
        }
    }

    /// Die oberste Zeile, die ab der Höhe `y` (im gesamten Inhalt) noch zu sehen ist.
    fn row_near(&self, y: f32) -> usize {
        if self.spans.is_empty() {
            return (y / self.line_h).floor().max(0.0) as usize * self.per_line;
        }
        let bottom = |span: &Span| span.top + GROUP_H + span.len.div_ceil(self.per_line) as f32 * self.line_h;
        match self.spans.iter().find(|span| bottom(span) > y) {
            Some(span) => {
                let line = ((y - span.top - GROUP_H) / self.line_h).floor().max(0.0) as usize;
                (span.first + line * self.per_line).min(span.first + span.len - 1)
            }
            None => self.rows.len().saturating_sub(1),
        }
    }

    /// Höhe des gesamten Inhalts.
    fn content_height(&self) -> f32 {
        match self.spans.last() {
            Some(last) => last.top + GROUP_H + last.len.div_ceil(self.per_line) as f32 * self.line_h,
            None => self.rows.len().div_ceil(self.per_line) as f32 * self.line_h,
        }
    }

    /// Die Zeile `steps` Zeilen tiefer, in derselben Spalte. Am Ende einer Gruppe geht es in die erste Zeile der
    /// nächsten; ist die letzte Zeile einer Gruppe kürzer, auf ihren letzten Eintrag.
    fn row_below(&self, row: usize, steps: usize) -> usize {
        if self.spans.is_empty() {
            return row + steps * self.per_line;
        }
        let mut row = row;
        for _ in 0..steps {
            let Some(span) = self.span_of(row) else { break };
            let (end, at) = (span.first + span.len, row - span.first);
            row = if row + self.per_line < end {
                row + self.per_line
            } else if at / self.per_line < (span.len - 1) / self.per_line {
                end - 1
            } else if end < self.rows.len() {
                end + (at % self.per_line).min(self.span_of(end).map_or(0, |next| next.len.saturating_sub(1)))
            } else {
                row
            };
        }
        row
    }

    /// Die Zeile `steps` Zeilen höher, in derselben Spalte; von der ersten Zeile einer Gruppe in die letzte der
    /// Gruppe davor.
    fn row_above(&self, row: usize, steps: usize) -> usize {
        if self.spans.is_empty() {
            return row.saturating_sub(steps * self.per_line);
        }
        let mut row = row;
        for _ in 0..steps {
            let Some(span) = self.span_of(row) else { break };
            let at = row - span.first;
            row = if at >= self.per_line {
                row - self.per_line
            } else {
                match span.first.checked_sub(1).and_then(|last| self.span_of(last)) {
                    Some(before) => {
                        let last_line = before.first + (before.len - 1) / self.per_line * self.per_line;
                        (last_line + at % self.per_line).min(before.first + before.len - 1)
                    }
                    None => row,
                }
            };
        }
        row
    }

    /// Leert die Liste der Zeilen – und mit ihr die Gruppen, die auf diese Zeilen zeigen. (Blieben sie stehen,
    /// griffe das Zeichnen ins Leere: so stürzte das Programm beim Verlassen der Galerie ab.)
    fn clear_rows(&mut self) {
        self.rows.clear();
        self.groups.clear();
        self.spans.clear();
    }

    /// Legt fest, wo die Gruppen liegen: jede beginnt unter ihrer Überschrift in einer neuen Zeile.
    fn plan_layout(&mut self) {
        self.spans.clear();
        // Gruppen, die nicht mehr zu den Zeilen passen, gelten nicht: dann ist die Liste ungegliedert.
        let fits = self.groups.first().is_none_or(|(first, _)| *first == 0)
            && self.groups.windows(2).all(|pair| pair[0].0 < pair[1].0)
            && self.groups.last().is_none_or(|(first, _)| *first < self.rows.len());
        if !fits {
            self.groups.clear();
        }
        let mut top = 0.0;
        for (i, (first, _)) in self.groups.iter().enumerate() {
            let end = self.groups.get(i + 1).map_or(self.rows.len(), |next| next.0);
            let len = end.saturating_sub(*first);
            self.spans.push(Span { first: *first, len, top });
            top += GROUP_H + len.div_ceil(self.per_line.max(1)) as f32 * self.line_h;
        }
    }

    /// Gruppen mit Überschrift gibt es in der Lite-Ausgabe nicht (die Vollversion gliedert die Galerie nach Monaten).
    fn regroup(&mut self) {
        self.groups.clear();
        self.plan_layout();
    }

    /// Die Zeilen im Ausschnitt `from..to` (Höhen im gesamten Inhalt), dazu je eine davor und danach.
    fn visible_lines(&self, from: f32, to: f32) -> Vec<Line<'_>> {
        let (per, h, total) = (self.per_line.max(1), self.line_h, self.rows.len());
        let mut out = Vec::new();
        // Nummern der Zeilen, die in einen Ausschnitt fallen, der bei `base` beginnt
        let between = |base: f32, lines: usize| {
            let first = (((from - base) / h).floor().max(0.0) as usize).min(lines);
            let last = ((((to - base) / h).ceil().max(0.0) as usize) + 1).min(lines);
            first..last.max(first)
        };
        if self.spans.is_empty() {
            for line in between(0.0, total.div_ceil(per)) {
                out.push(Line { top: line as f32 * h, rows: line * per..((line + 1) * per).min(total), title: None });
            }
            return out;
        }
        for (span, (_, title)) in self.spans.iter().zip(&self.groups) {
            if span.first + span.len > total {
                break;
            }
            let lines = span.len.div_ceil(per);
            let base = span.top + GROUP_H;
            if base + lines as f32 * h < from || span.top > to {
                continue;
            }
            if base >= from {
                out.push(Line { top: span.top, rows: span.first..span.first + span.len, title: Some(title) });
            }
            for line in between(base, lines) {
                let start = span.first + line * per;
                out.push(Line { top: base + line as f32 * h, rows: start..(start + per).min(span.first + span.len), title: None });
            }
        }
        out
    }

    pub fn new(loc: Location, sort: SortKey, desc: bool) -> Self {
        // Der Papierkorb bringt seine eigene Darstellung mit.
        let (sort, desc, grid) = own_view(&loc).unwrap_or((sort, desc, false));
        Self {
            grid,
            in_cloud: false,
            groups: Vec::new(),
            spans: Vec::new(),
            before_own_view: None,
            per_line: 1,
            line_h: ROW_H,
            tile_w: 0.0,
            reveal_pending: false,
            thumb_seen: 0.0,
            zoom_anchor: None,
            id: next_id(),
            loc,
            entries: Vec::new(),
            rows: Vec::new(),
            marked: Vec::new(),
            marked_count: 0,
            cursor: 0,
            unselected: false,
            anchor: 0,
            sort,
            desc,
            filter: String::new(),
            back: Vec::new(),
            forward: Vec::new(),
            loading: false,
            load_started: Instant::now(),
            generation: 0,
            error: None,
            listed: None,
            dir_modified: None,
            stale: false,
            space: None,
            scroll_to: None,
            scroll_y: 0.0,
            view_h: 400.0,
            focus_name: None,
            fresh: true,
            sizes: HashMap::new(),
            size_cancel: Arc::new(AtomicBool::new(false)),
            empty_dirs: HashSet::new(),
            empty_next: HashSet::new(),
            empty_cancel: Arc::new(AtomicBool::new(false)),
            search: None,
            search_due: None,
            rename: None,
            rename_armed: None,
            rename_due: None,
            band: None,
            typeahead: String::new(),
            typeahead_at: 0.0,
        }
    }

    /// Hält an, was im Hintergrund für diesen Tab läuft (Suche, Ordnergrößen, Prüfung auf leere Ordner).
    pub fn cancel_work(&self) {
        self.size_cancel.store(true, Relaxed);
        self.empty_cancel.store(true, Relaxed);
        if let Some(s) = &self.search {
            s.cancel.store(true, Relaxed);
        }
    }

    /// Wechselt den Ort. Der Papierkorb bringt seine eigene Darstellung mit (siehe `own_view`); beim
    /// Verlassen gilt wieder, was vorher eingestellt war.
    fn set_loc(&mut self, loc: Location) {
        let (was, now) = (own_view(&self.loc), own_view(&loc));
        match (was, now) {
            (None, Some(view)) => {
                self.before_own_view = Some((self.sort, self.desc, self.grid));
                (self.sort, self.desc, self.grid) = view;
            }
            (Some(_), Some(view)) => (self.sort, self.desc, self.grid) = view,
            // Ein Tab, der schon so geöffnet wurde, hat kein „vorher": dann die übliche Liste nach Namen.
            (Some(_), None) => {
                (self.sort, self.desc, self.grid) = self.before_own_view.take().unwrap_or((SortKey::Name, false, false));
            }
            (None, None) => {}
        }
        self.in_cloud = false;
        self.loc = loc;
    }

    /// Merkt sich, ob der gezeigte Ordner in einem Cloud-Ordner liegt (`roots`: deren Pfade in Kleinbuchstaben,
    /// mit abschließendem `\`). Dort zeigt die Liste an jeder Datei, ob ihr Inhalt auf dem PC liegt.
    pub fn note_cloud(&mut self, roots: &[String]) {
        self.in_cloud = self.loc.local_path().is_some_and(|path| {
            let mut folded = path.to_string_lossy().to_lowercase();
            if !folded.ends_with('\\') {
                folded.push('\\');
            }
            roots.iter().any(|root| folded.starts_with(root.as_str()))
        });
    }

    pub fn title(&self) -> String {
        match &self.search {
            Some(s) => tf!("Suche: {}", "Search: {}", s.query),
            None => self.loc.leaf(),
        }
    }

    // ------------------------------------------------------------ Laden

    pub fn request(&mut self, env: &mut Env) {
        self.generation += 1;
        self.loading = true;
        self.stale = false;
        self.load_started = Instant::now();
        let (tab, generation, loc, bus) = (self.id, self.generation, self.loc.clone(), env.bus.clone());
        // Computer, die aus Favoriten und gemerkten Tabs schon bekannt sind, erscheinen sofort.
        let known_servers: Vec<String> = env
            .cfg
            .favorites
            .iter()
            .map(|f| &f.loc)
            .chain(env.cfg.panes.iter().flat_map(|pane| pane.tabs.iter().map(|t| &t.loc)))
            .filter_map(|l| l.local_path().and_then(vfs::unc_server))
            .collect();
        let last_found = env.cfg.known_computers.clone();
        std::thread::spawn(move || {
            let result = match &loc {
                Location::Local(p) => vfs::list_local(p),
                Location::Drives => Ok(vfs::list_drives()),
                Location::Zip { archive, inner } => crate::archive::list(archive, inner),
                Location::Network { server: Some(server) } => {
                    crate::netenum::shares(server).map(|names| vfs::network_listing(&names, Some(server), false))
                }
                Location::Network { server: None } => {
                    // Die Suche im Netz dauert oft über zehn Sekunden: erst zeigen, was schon bekannt ist.
                    let remembered = crate::netenum::remembered_computers().unwrap_or(last_found);
                    let early = crate::netenum::merge_names(&known_servers, &remembered);
                    let early_result = Ok(vfs::network_listing(&early, None, true));
                    bus.send(Msg::Listed { tab, generation, loc: loc.clone(), result: early_result });
                    match crate::netenum::find_computers() {
                        Ok(found) => Ok(vfs::network_listing(&crate::netenum::merge_names(&known_servers, &found), None, false)),
                        // Die Suche schlug fehl: die bekannten Computer bleiben trotzdem nutzbar.
                        Err(_) if !early.is_empty() => Ok(vfs::network_listing(&early, None, false)),
                        Err(e) => Err(e),
                    }
                }
                Location::RecycleBin => crate::bin::list(),
                Location::Device { id, path, .. } => crate::device::list(id, path).map(|entries| Listing {
                    entries,
                    space: None,
                    empty_dirs: None,
                    pending: false,
                    dir_modified: None,
                }),
            };
            bus.send(Msg::Listed { tab, generation, loc, result });
        });
    }

    pub fn on_listed(&mut self, generation: u64, result: Result<Listing, String>, show_hidden: bool, bus: &Bus) {
        if generation != self.generation || self.search.is_some() {
            return;
        }
        self.loading = false;
        self.band = None;
        let fresh = std::mem::take(&mut self.fresh);
        let focus = self.focus_name.take();
        if focus.is_some() {
            // Der Cursor soll auf einen bestimmten Eintrag (neu angelegt, umbenannt): der ist dann auch gewählt.
            self.unselected = false;
        }
        let keep_name = focus.or_else(|| if fresh { None } else { self.at_cursor().map(|e| e.name.clone()) });
        let marks: HashSet<String> = if fresh || self.marked_count == 0 {
            HashSet::new()
        } else {
            self.entries.iter().zip(&self.marked).filter(|(_, m)| **m).map(|(e, _)| e.name.clone()).collect()
        };
        let old_cursor = self.cursor;
        let mut known_empty: Option<Vec<String>> = None;
        let mut entries = match result {
            Ok(listing) => {
                self.space = listing.space;
                self.error = None;
                self.listed = Some(self.loc.clone());
                self.dir_modified = listing.dir_modified;
                known_empty = listing.empty_dirs;
                // Bei der Netzwerksuche kommt noch eine vollständigere Liste nach.
                self.loading = listing.pending;
                listing.entries
            }
            Err(e) => {
                self.space = None;
                self.error = Some(e);
                self.listed = None;
                self.dir_modified = None;
                Vec::new()
            }
        };
        if !show_hidden {
            entries.retain(|e| !e.hidden);
        }
        if self.loc.parent().is_some() {
            entries.push(Entry::parent_row());
        }
        if self.loc == Location::Drives {
            // „Dieser PC" behält seine Reihenfolge: Laufwerke, Cloud-Ordner, Geräte, Papierkorb, Netzwerk.
        } else if self.loc.is_virtual() {
            sort_entries(&mut entries, SortKey::Name, false);
        } else {
            sort_entries(&mut entries, self.sort, self.desc);
        }
        // Leere Ordner: bei ZIP-Archiven schon bekannt, bei lokalen Ordnern im Hintergrund nachsehen.
        // Die bisherige Darstellung bleibt stehen, bis das neue Ergebnis da ist – so flackert nichts.
        self.empty_cancel.store(true, Relaxed);
        self.empty_cancel = Arc::new(AtomicBool::new(false));
        self.empty_next.clear();
        match known_empty {
            Some(names) => self.empty_dirs = names.into_iter().collect(),
            None if self.loc.is_local() => {
                let dirs = entries.iter().filter(|e| e.is_dir && !e.is_parent && !e.link).map(|e| e.name.clone()).collect();
                vfs::spawn_empty_check(self.id, self.loc.clone(), dirs, show_hidden, bus.clone(), self.empty_cancel.clone());
            }
            None => self.empty_dirs.clear(),
        }
        self.marked = entries.iter().map(|e| marks.contains(&e.name)).collect();
        self.marked_count = self.marked.iter().filter(|m| **m).count();
        self.entries = entries;
        self.rename = None;
        self.rebuild_rows();
        self.cursor = keep_name
            .and_then(|n| self.row_of_name(&n))
            .unwrap_or(if fresh { 0 } else { old_cursor })
            .min(self.rows.len().saturating_sub(1));
        self.anchor = self.cursor;
        if fresh {
            self.scroll_y = 0.0;
            self.scroll_to = Some(0.0);
        }
        self.reveal_cursor();
    }

    pub fn on_empty_dirs(&mut self, names: Vec<String>, done: bool) {
        for name in names {
            self.empty_dirs.insert(name.clone());
            self.empty_next.insert(name);
        }
        if done {
            // Jetzt auch alles entfernen, was inzwischen nicht mehr leer ist.
            self.empty_dirs = std::mem::take(&mut self.empty_next);
        }
    }

    pub fn rebuild_rows(&mut self) {
        if self.search.is_some() {
            self.rows = (0..self.entries.len() as u32).collect();
        } else {
            let filter = NameFilter::new(&self.filter);
            self.rows = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.is_parent || filter.matches(&e.key))
                .map(|(i, _)| i as u32)
                .collect();
        }
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
        self.anchor = self.anchor.min(self.rows.len().saturating_sub(1));
        self.regroup();
    }

    pub fn row_of_name(&self, name: &str) -> Option<usize> {
        self.rows.iter().position(|&i| self.entries[i as usize].name == name)
    }

    pub fn set_sort(&mut self, key: SortKey) {
        if self.sort == key {
            self.desc = !self.desc;
        } else {
            self.sort = key;
            self.desc = matches!(key, SortKey::Size | SortKey::Date);
        }
        self.resort();
    }

    fn resort(&mut self) {
        self.band = None;
        let keep = self.at_cursor().map(|e| e.name.clone());
        let marks: HashSet<String> =
            self.entries.iter().zip(&self.marked).filter(|(_, m)| **m).map(|(e, _)| e.name.clone()).collect();
        sort_entries(&mut self.entries, self.sort, self.desc);
        self.marked = self.entries.iter().map(|e| marks.contains(&e.name)).collect();
        self.rebuild_rows();
        if let Some(row) = keep.and_then(|n| self.row_of_name(&n)) {
            self.cursor = row;
            self.anchor = row;
        }
        self.reveal_cursor();
    }

    // ------------------------------------------------------------ Navigation

    fn reset_view(&mut self) {
        if let Some(s) = self.search.take() {
            s.cancel.store(true, Relaxed);
        }
        self.size_cancel.store(true, Relaxed);
        self.size_cancel = Arc::new(AtomicBool::new(false));
        self.sizes.clear();
        self.empty_cancel.store(true, Relaxed);
        self.empty_dirs.clear();
        self.empty_next.clear();
        self.filter.clear();
        self.entries.clear();
        self.clear_rows();
        self.marked.clear();
        self.marked_count = 0;
        self.cursor = 0;
        self.unselected = false;
        self.anchor = 0;
        self.error = None;
        self.rename = None;
        self.fresh = true;
        self.typeahead.clear();
        self.search_due = None;
        self.rename_armed = None;
        self.rename_due = None;
        self.band = None;
    }

    pub fn navigate(&mut self, loc: Location, env: &mut Env) {
        if loc == self.loc && self.search.is_none() {
            self.reload(env);
            return;
        }
        let name = self.at_cursor().map(|e| e.name.clone());
        self.back.push((self.loc.clone(), name));
        if self.back.len() > 100 {
            self.back.remove(0);
        }
        self.forward.clear();
        // Geht es in einen übergeordneten Ordner (Klick in der Pfadleiste, Favorit, Eingabe),
        // steht der Cursor danach auf dem Ordner, aus dem man kommt – wie bei „nach oben".
        let came_from = self.loc.branch_below(&loc);
        self.reset_view();
        self.set_loc(loc);
        self.focus_name = came_from;
        self.request(env);
    }

    pub fn go_up(&mut self, env: &mut Env) {
        if self.search.is_some() {
            self.end_search(env, false);
            return;
        }
        if let Some(parent) = self.loc.parent() {
            let leaf = match &self.loc {
                // In „Dieser PC" heißt der Eintrag „C:" und nicht „C:\".
                Location::Local(p) if p.parent().is_none() => p.to_string_lossy().trim_end_matches('\\').to_owned(),
                other => other.leaf(),
            };
            self.navigate(parent, env);
            self.focus_name = Some(leaf);
        }
    }

    pub fn go_back(&mut self, env: &mut Env) {
        if let Some((loc, name)) = self.back.pop() {
            let current = self.at_cursor().map(|e| e.name.clone());
            self.forward.push((self.loc.clone(), current));
            self.reset_view();
            self.set_loc(loc);
            self.focus_name = name;
            self.request(env);
        }
    }

    pub fn go_forward(&mut self, env: &mut Env) {
        if let Some((loc, name)) = self.forward.pop() {
            let current = self.at_cursor().map(|e| e.name.clone());
            self.back.push((self.loc.clone(), current));
            self.reset_view();
            self.set_loc(loc);
            self.focus_name = name;
            self.request(env);
        }
    }

    /// Lädt neu und behält Cursor, Markierungen und Scrollposition.
    pub fn reload(&mut self, env: &mut Env) {
        if self.search.is_some() {
            return;
        }
        self.request(env);
    }

    // ------------------------------------------------------------ Suche

    pub fn start_search(&mut self, env: &mut Env) {
        let query = self.filter.trim().to_owned();
        let Some(root) = self.loc.local_path().map(PathBuf::from) else {
            env.bus.error(t(
                "Die Suche in Unterordnern gibt es nur für lokale Ordner.",
                "Searching subfolders is only available for local folders.",
            ));
            return;
        };
        if query.is_empty() {
            return;
        }
        if let Some(s) = self.search.take() {
            s.cancel.store(true, Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let id = next_id();
        self.search = Some(SearchState {
            id,
            query: query.clone(),
            running: true,
            checked: 0,
            cancel: cancel.clone(),
        });
        self.entries.clear();
        self.marked.clear();
        self.marked_count = 0;
        self.clear_rows();
        self.cursor = 0;
        self.unselected = false;
        self.anchor = 0;
        self.error = None;
        self.loading = false;
        self.scroll_to = Some(0.0);
        let params = search::Params { tab: self.id, search: id, root, query, show_hidden: env.cfg.show_hidden };
        search::spawn(params, env.bus.clone(), cancel);
    }

    /// Beendet die Suche und zeigt wieder den Ordner. `keep_filter` lässt den Suchtext als Filter stehen.
    pub fn end_search(&mut self, env: &mut Env, keep_filter: bool) {
        self.search_due = None;
        if let Some(s) = self.search.take() {
            s.cancel.store(true, Relaxed);
            if !keep_filter {
                self.filter.clear();
            }
            self.entries.clear();
            self.clear_rows();
            self.marked.clear();
            self.marked_count = 0;
            self.fresh = true;
            self.request(env);
        }
    }

    /// Suchtext leeren; eine laufende Suche endet damit.
    pub fn clear_search(&mut self, env: &mut Env) {
        self.search_due = None;
        if self.search.is_some() {
            self.end_search(env, false);
        } else {
            self.filter.clear();
            self.rebuild_rows();
        }
    }

    pub fn on_search_hits(&mut self, search: u64, hits: Vec<Entry>, checked: u64) {
        let Some(s) = self.search.as_mut().filter(|s| s.id == search) else { return };
        s.checked = checked;
        if hits.is_empty() {
            return;
        }
        self.marked.extend(std::iter::repeat_n(false, hits.len()));
        self.entries.extend(hits);
        self.rebuild_rows();
    }

    pub fn on_search_done(&mut self, search: u64) {
        if let Some(s) = self.search.as_mut().filter(|s| s.id == search) {
            s.running = false;
            self.resort();
        }
    }

    // ------------------------------------------------------------ Auswahl

    pub fn entry_at(&self, row: usize) -> Option<&Entry> {
        self.rows.get(row).map(|&i| &self.entries[i as usize])
    }

    /// Der gewählte Eintrag: der unter dem Cursor – außer nach einem Klick ins Leere, dann ist keiner gewählt.
    pub fn current(&self) -> Option<&Entry> {
        if self.unselected { None } else { self.at_cursor() }
    }

    /// Der Eintrag, auf dem der Cursor steht, auch wenn gerade nichts gewählt ist (um die Stelle wiederzufinden).
    fn at_cursor(&self) -> Option<&Entry> {
        self.entry_at(self.cursor)
    }

    /// Ordner, in dem ein Eintrag wirklich liegt (bei Suchtreffern ein Unterordner).
    pub fn entry_dir(&self, e: &Entry) -> Location {
        match (&self.search, &e.note, &self.loc) {
            (Some(_), Some(rel), Location::Local(root)) if !rel.is_empty() => Location::Local(root.join(&**rel)),
            _ => self.loc.clone(),
        }
    }

    /// Wohin das Öffnen eines Eintrags führt (in Übersichten steht das Ziel am Eintrag).
    pub fn entry_location(&self, e: &Entry) -> Location {
        e.target.clone().unwrap_or_else(|| self.entry_dir(e).child(&e.name))
    }

    /// Indizes der Einträge, auf die eine Aktion wirkt: Markierte, sonst der Eintrag unter dem Cursor.
    pub fn selection(&self) -> Vec<usize> {
        if self.marked_count > 0 {
            self.rows.iter().map(|&i| i as usize).filter(|&i| self.marked[i]).collect()
        } else {
            match self.rows.get(self.cursor) {
                Some(&i) if !self.unselected && !self.entries[i as usize].is_parent => vec![i as usize],
                _ => Vec::new(),
            }
        }
    }

    pub fn selected_entries(&self) -> Vec<Entry> {
        self.selection().into_iter().map(|i| self.entries[i].clone()).collect()
    }

    pub fn sources(&self) -> Vec<Source> {
        self.selection()
            .into_iter()
            .map(|i| {
                let e = &self.entries[i];
                Source { dir: self.entry_dir(e), name: e.name.clone(), is_dir: e.is_dir, size: e.size }
            })
            .collect()
    }

    fn set_mark(&mut self, entry: usize, on: bool) {
        if self.entries[entry].is_parent || self.marked[entry] == on {
            return;
        }
        self.marked[entry] = on;
        if on {
            self.marked_count += 1;
        } else {
            self.marked_count -= 1;
        }
    }

    pub fn clear_marks(&mut self) {
        if self.marked_count > 0 {
            self.marked.iter_mut().for_each(|m| *m = false);
            self.marked_count = 0;
        }
    }

    pub fn mark_all(&mut self) {
        for row in 0..self.rows.len() {
            let i = self.rows[row] as usize;
            self.set_mark(i, true);
        }
    }

    pub fn invert_marks(&mut self) {
        for row in 0..self.rows.len() {
            let i = self.rows[row] as usize;
            let on = !self.marked[i];
            self.set_mark(i, on);
        }
    }

    fn mark_range(&mut self, a: usize, b: usize) {
        self.clear_marks();
        let (lo, hi) = (a.min(b), a.max(b));
        for row in lo..=hi.min(self.rows.len().saturating_sub(1)) {
            let i = self.rows[row] as usize;
            self.set_mark(i, true);
        }
    }

    fn toggle_row(&mut self, row: usize) {
        if let Some(&i) = self.rows.get(row) {
            let on = !self.marked[i as usize];
            self.set_mark(i as usize, on);
        }
    }

    pub fn reveal_cursor(&mut self) {
        let mut top = self.row_top(self.cursor);
        let bottom = top + self.line_h;
        // In der ersten Zeile einer Gruppe gehört die Überschrift mit ins Bild.
        if self.span_of(self.cursor).is_some_and(|span| self.cursor < span.first + self.per_line) {
            top -= GROUP_H;
        }
        let view = self.view_h.max(self.line_h);
        if top < self.scroll_y {
            self.scroll_y = top;
            self.scroll_to = Some(top);
        } else if bottom > self.scroll_y + view {
            self.scroll_y = bottom - view;
            self.scroll_to = Some(self.scroll_y);
        }
    }

    fn move_cursor(&mut self, to: usize, extend: bool) {
        if self.rows.is_empty() {
            return;
        }
        let to = to.min(self.rows.len() - 1);
        self.cursor = to;
        self.unselected = false;
        if extend {
            self.mark_range(self.anchor, to);
        } else {
            self.anchor = to;
        }
        self.reveal_cursor();
    }

    pub fn start_rename(&mut self) {
        if !self.loc.writable() || self.unselected {
            return;
        }
        if let Some(&i) = self.rows.get(self.cursor) {
            let e = &self.entries[i as usize];
            if !e.is_parent {
                self.rename = Some(InlineRename { entry: i as usize, text: e.name.clone(), fresh: true });
            }
        }
    }

    /// Berechnet die Größe der genannten Ordner (nur lokal).
    pub fn calc_sizes(&mut self, names: Vec<String>, env: &Env) {
        if !self.loc.is_local() || names.is_empty() {
            return;
        }
        for n in &names {
            self.sizes.insert(n.clone(), DirSize { size: 0, done: false });
        }
        ops::spawn_dir_sizes(self.id, self.loc.clone(), names, env.bus.clone(), self.size_cancel.clone());
    }

    // ------------------------------------------------------------ Tastatur

    /// Tasten für die Dateiliste. Liefert einen Befehl, den das Hauptfenster ausführen soll.
    pub fn keys(&mut self, ctx: &egui::Context, env: &Env) -> Option<Cmd> {
        let mut cmd = None;
        // In der Rasteransicht springen Auf/Ab um eine ganze Zeile, Links/Rechts um eine Kachel.
        let per = self.per_line;
        let page = ((self.view_h / self.line_h).floor() as usize).max(2) - 1;
        let last = self.rows.len().saturating_sub(1);
        let (left, right) = ctx.input(|i| (i.num_presses(Key::ArrowLeft), i.num_presses(Key::ArrowRight)));
        let (down, up, pgdn, pgup, home, end, shift, enter, back, space, insert, texts, time, plain) = ctx.input(|i| {
            let texts: Vec<String> = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Text(t) => Some(t.clone()),
                    _ => None,
                })
                .collect();
            (
                i.num_presses(Key::ArrowDown),
                i.num_presses(Key::ArrowUp),
                i.num_presses(Key::PageDown),
                i.num_presses(Key::PageUp),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.modifiers.shift,
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Backspace),
                i.key_pressed(Key::Space),
                i.key_pressed(Key::Insert),
                texts,
                i.time,
                !i.modifiers.ctrl && !i.modifiers.alt && !i.modifiers.command,
            )
        });
        if !plain {
            return None;
        }
        // Nichts gewählt (Klick ins Leere): die erste Pfeiltaste wählt wieder die Zeile, auf der der Cursor steht.
        let arrows = down + up + if per > 1 { left + right } else { 0 };
        let (down, up, left, right) = if self.unselected && arrows > 0 {
            self.move_cursor(self.cursor, shift);
            (0, 0, 0, 0)
        } else {
            (down, up, left, right)
        };
        if down > 0 {
            self.move_cursor(self.row_below(self.cursor, down), shift);
        }
        if up > 0 && (self.cursor >= per || !self.spans.is_empty()) {
            self.move_cursor(self.row_above(self.cursor, up), shift);
        }
        if per > 1 {
            if right > 0 {
                self.move_cursor(self.cursor + right, shift);
            }
            if left > 0 {
                self.move_cursor(self.cursor.saturating_sub(left), shift);
            }
        }
        if pgdn > 0 {
            self.move_cursor(self.row_below(self.cursor, page * pgdn), shift);
        }
        if pgup > 0 {
            self.move_cursor(self.row_above(self.cursor, page * pgup), shift);
        }
        if home {
            self.move_cursor(0, shift);
        }
        if end {
            self.move_cursor(last, shift);
        }
        if enter {
            cmd = Some(Cmd::Open);
        }
        if back {
            cmd = Some(Cmd::Up);
        }
        if insert || space {
            self.unselected = false;
            self.toggle_row(self.cursor);
            if space {
                if let Some(e) = self.current().filter(|e| e.is_dir && !e.is_parent) {
                    if !self.sizes.contains_key(&e.name) {
                        let name = e.name.clone();
                        self.calc_sizes(vec![name], env);
                    }
                }
            }
            if insert {
                self.move_cursor(self.cursor + 1, false);
            }
        }
        // Tippen springt zum ersten passenden Namen.
        for t in texts {
            if t.chars().all(|c| c.is_control() || c == ' ') {
                continue;
            }
            if time - self.typeahead_at > 0.9 {
                self.typeahead.clear();
            }
            self.typeahead_at = time;
            self.typeahead.push_str(&fold_key(&t));
            let needle = self.typeahead.clone();
            let start = if needle.chars().count() == 1 { self.cursor + 1 } else { self.cursor };
            let n = self.rows.len();
            let hit = (0..n).map(|k| (start + k) % n.max(1)).find(|&row| {
                let e = &self.entries[self.rows[row] as usize];
                !e.is_parent && e.key.starts_with(&needle)
            });
            if let Some(row) = hit {
                self.move_cursor(row, false);
            }
        }
        cmd
    }

    pub fn state(&self) -> TabState {
        TabState { loc: self.loc.clone(), sort: self.sort, desc: self.desc, grid: self.grid }
    }

    /// Wechselt zwischen Liste und Rasteransicht.
    pub fn toggle_grid(&mut self) {
        self.grid = !self.grid;
        self.reveal_pending = true;
    }
}

// ================================================================ Fenster

pub struct Pane {
    pub tabs: Vec<Tab>,
    pub active: usize,
    path_edit: Option<String>,
    path_edit_fresh: bool,
    pub focus_filter: bool,
    /// Tab, der gerade mit der Maus verschoben wird: seine Kennung und der Griffpunkt (Abstand zur linken Kante)
    tab_drag: Option<(u64, f32)>,
    /// Fläche der Dateiliste im zuletzt gezeichneten Bild
    list_rect: Rect,
}

/// Alles, was ein Fenster beim Zeichnen braucht.
pub struct PaneCx<'a> {
    pub p: &'a Palette,
    pub env: Env<'a>,
    pub index: usize,
    pub active: bool,
    pub interactive: bool,
    pub actions: &'a mut Vec<Action>,
    pub drives: &'a [win::Drive],
    pub folders: &'a [(&'static str, PathBuf)],
    /// Verbundene Telefone und weitere Cloud-Dienste (siehe `win::places`)
    pub places: &'a [win::Place],
    /// Per Kabel angeschlossene Geräte: (Kennung, Name)
    pub devices: &'a [(String, String)],
    pub types: &'a mut HashMap<String, String>,
    pub today: chrono::NaiveDate,
    pub can_paste: bool,
    /// Schnellansicht dieses Fensters, wenn sie eingeschaltet ist
    pub preview: Option<&'a mut crate::preview::Preview>,
    /// Echte Windows-Symbole für Programme und Verknüpfungen
    pub icons: &'a mut crate::shellicon::ShellIcons,
    /// Miniaturen für die Rasteransicht
    pub thumbs: &'a mut crate::thumbs::Thumbs,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColMode {
    Normal,
    Search,
    Drives,
}

struct Cols {
    name: (f32, f32),
    note: Option<(f32, f32)>,
    size: Option<(f32, f32)>,
    kind: Option<(f32, f32)>,
    date: Option<(f32, f32)>,
    /// Die gezeigten Spalten von links nach rechts
    order: Vec<Col>,
}

impl Cols {
    fn range(&self, col: Col) -> Option<(f32, f32)> {
        match col {
            Col::Size => self.size,
            Col::Kind => self.kind,
            Col::Date => self.date,
        }
    }
}

/// Überschrift einer Spalte
fn col_title(col: Col) -> &'static str {
    match col {
        Col::Size => t("Größe", "Size"),
        Col::Kind => t("Typ", "Type"),
        Col::Date => t("Geändert", "Modified"),
    }
}

const COL_MIN: f32 = 44.0;
const COL_MAX: f32 = 520.0;

/// Spalten mit einstellbarer Breite
#[derive(Clone, Copy, PartialEq, Eq)]
enum FitCol {
    Size,
    Kind,
    Date,
}

/// Breite des breitesten Textes. Bei sehr vielen verschiedenen Texten werden nur die längsten vermessen.
fn widest(ui: &Ui, texts: impl Iterator<Item = String>, font: &egui::FontId) -> f32 {
    let mut unique: Vec<String> = texts.collect::<HashSet<String>>().into_iter().collect();
    if unique.len() > 200 {
        unique.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
        unique.truncate(80);
    }
    unique.iter().map(|t| text_width(ui, t, font.clone())).fold(0.0, f32::max)
}

fn layout_cols(rect: Rect, cfg: &Config, mode: ColMode) -> Cols {
    let width = rect.width();
    let mut right = rect.right() - 12.0;
    let mut take = |w: f32, min_total: f32| -> Option<(f32, f32)> {
        if width < min_total {
            return None;
        }
        let col = (right - w, right);
        right -= w;
        Some(col)
    };
    let (mut note, mut size, mut kind, mut date) = (None, None, None, None);
    let mut order = Vec::new();
    match mode {
        ColMode::Drives => {
            date = take((width * 0.3).clamp(150.0, 240.0), 380.0);
            kind = take((width * 0.3).clamp(140.0, 260.0), 560.0);
        }
        _ => {
            let col_width = |col: Col| match col {
                Col::Size => cfg.col_size,
                Col::Kind => cfg.col_type,
                Col::Date => cfg.col_date,
            };
            // So breit muss der Name mindestens bleiben, damit die Spalte ihren Platz behält.
            let need = |col: Col| match col {
                Col::Kind => 230.0,
                Col::Date => 150.0,
                Col::Size => 130.0,
            };
            let note_w = if mode == ColMode::Search && width >= 500.0 { (width * 0.26).clamp(110.0, 300.0) } else { 0.0 };
            order = cfg.columns.clone();
            // Wird es eng, weicht zuerst die Spalte, die dem Namen am meisten Platz abverlangt.
            loop {
                let name_w = width - note_w - order.iter().map(|col| col_width(*col)).sum::<f32>() - 28.0;
                let victim = order.iter().copied().filter(|col| name_w < need(*col)).max_by(|a, b| need(*a).total_cmp(&need(*b)));
                match victim {
                    Some(victim) => order.retain(|col| *col != victim),
                    None => break,
                }
            }
            for col in order.iter().rev() {
                let range = take(col_width(*col), 0.0);
                match col {
                    Col::Size => size = range,
                    Col::Kind => kind = range,
                    Col::Date => date = range,
                }
            }
            if note_w > 0.0 {
                note = take(note_w, 0.0);
            }
        }
    }
    Cols { name: (rect.left() + 8.0, right - 8.0), note, size, kind, date, order }
}

/// Teilt die Fläche unter der Pfadleiste in Dateiliste und Schnellansicht. In breiten Fenstern steht die
/// Schnellansicht rechts, in hohen darunter; der Trenner dazwischen lässt sich ziehen.
fn split_for_preview(ui: &mut Ui, area: Rect, cx: &mut PaneCx) -> (Rect, Option<Rect>) {
    const BAR: f32 = 7.0;
    const DEFAULT: f32 = 0.4;
    let p = cx.p;
    let side = area.width() >= area.height() * 1.25;
    let total = if side { area.width() } else { area.height() };
    let (min_list, min_view) = if side { (240.0, 200.0) } else { (HEAD_H + 3.0 * ROW_H, 120.0) };
    if total < min_list + min_view + BAR {
        return (area, None); // zu wenig Platz: die Liste geht vor
    }
    let id = Id::new(("vorschau-trenner", cx.index));
    let view = (total * cx.env.cfg.preview_split.clamp(0.15, 0.85)).clamp(min_view, total - min_list - BAR).round();
    let (list, bar, view_r) = if side {
        let x = area.right() - view;
        (area.with_max_x(x - BAR), Rect::from_min_max(Pos2::new(x - BAR, area.top()), Pos2::new(x, area.bottom())), area.with_min_x(x))
    } else {
        let y = area.bottom() - view;
        (area.with_max_y(y - BAR), Rect::from_min_max(Pos2::new(area.left(), y - BAR), Pos2::new(area.right(), y)), area.with_min_y(y))
    };
    let resp = ui.interact(bar, id, Sense::click_and_drag());
    let painter = ui.painter();
    painter.rect_filled(bar, 0, p.surface_alt);
    let line = Stroke::new(1.0, p.border_soft);
    if side {
        painter.vline(bar.left() + 0.5, bar.y_range(), line);
        painter.vline(bar.right() - 0.5, bar.y_range(), line);
    } else {
        painter.hline(bar.x_range(), bar.top() + 0.5, line);
        painter.hline(bar.x_range(), bar.bottom() - 0.5, line);
    }
    let hot = resp.hovered() || resp.dragged();
    let grip = if side { Vec2::new(3.0, 36.0) } else { Vec2::new(36.0, 3.0) };
    painter.rect_filled(Rect::from_center_size(bar.center(), grip), CornerRadius::same(2), if hot { p.accent } else { p.border });
    if hot {
        ui.ctx().set_cursor_icon(if side { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::ResizeVertical });
    }
    if resp.dragged() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let wanted = if side { area.right() - pos.x } else { area.bottom() - pos.y } - BAR / 2.0;
            cx.env.cfg.preview_split = (wanted / total).clamp(0.15, 0.85);
        }
    }
    if resp.double_clicked() {
        cx.env.cfg.preview_split = DEFAULT;
    }
    (list, Some(view_r))
}

enum RowEvent {
    /// Zeile und ob der Druck auf dem Namenstext lag
    Press(usize, bool),
    Click(usize),
    Open(usize),
    Secondary(usize),
    /// Zeile und ob das Ziehen auf Symbol oder Name begann (dann werden die Dateien gezogen)
    DragStart(usize, bool),
    Middle(usize),
    RenameCommit,
    RenameCancel,
}

impl Pane {
    pub fn new(state: &PaneState) -> Self {
        let mut tabs: Vec<Tab> = state
            .tabs
            .iter()
            .map(|t| {
                let mut tab = Tab::new(t.loc.clone(), t.sort, t.desc);
                tab.grid = t.grid;
                tab
            })
            .collect();
        if tabs.is_empty() {
            tabs.push(Tab::new(Location::Drives, SortKey::Name, false));
        }
        let active = state.active.min(tabs.len() - 1);
        Self {
            tabs,
            active,
            path_edit: None,
            path_edit_fresh: false,
            focus_filter: false,
            tab_drag: None,
            list_rect: Rect::NOTHING,
        }
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    pub fn state(&self) -> PaneState {
        PaneState { tabs: self.tabs.iter().map(Tab::state).collect(), active: self.active }
    }

    pub fn add_tab(&mut self, loc: Location, env: &mut Env) {
        let (sort, desc) = (self.tab().sort, self.tab().desc);
        let mut tab = Tab::new(loc, sort, desc);
        tab.request(env);
        self.tabs.insert(self.active + 1, tab);
        self.active += 1;
    }

    /// Nimmt einen Tab heraus, um ihn ins andere Fenster zu setzen. Der letzte Tab bleibt, wo er ist.
    pub fn take_tab(&mut self, id: u64) -> Option<Tab> {
        let index = self.tabs.iter().position(|t| t.id == id)?;
        if self.tabs.len() <= 1 {
            return None;
        }
        let tab = self.tabs.remove(index);
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        Some(tab)
    }

    /// Setzt einen fertigen Tab hinter den aktiven und zeigt ihn an.
    pub fn insert_tab(&mut self, tab: Tab) {
        self.tabs.insert(self.active + 1, tab);
        self.active += 1;
        self.path_edit = None;
    }

    pub fn close_tab(&mut self, index: usize) {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index).cancel_work();
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    /// Wohin ein Ablegen an dieser Stelle des Fensters ginge: in den Ordner unter der Maus, sonst in den angezeigten.
    pub fn drop_location_at(&self, pos: Pos2) -> Location {
        let tab = self.tab();
        if self.list_rect.contains(pos) {
            let row = tab.row_at(pos.x - self.list_rect.left(), pos.y - self.list_rect.top() + tab.scroll_y);
            if let Some(e) = row.and_then(|row| tab.entry_at(row)).filter(|e| e.is_dir && !e.is_parent) {
                return tab.entry_dir(e).child(&e.name);
            }
        }
        tab.loc.clone()
    }

    pub fn edit_path(&mut self) {
        self.path_edit = Some(self.tab().loc.display());
        self.path_edit_fresh = true;
    }

    /// Öffnet den Eintrag unter dem Cursor: Ordner betreten, Datei starten.
    pub fn open_current(&mut self, env: &mut Env) -> Option<Action> {
        let tab = self.tab_mut();
        let entry = tab.current()?.clone();
        if entry.is_parent {
            tab.go_up(env);
            return None;
        }
        let dir = tab.entry_dir(&entry);
        let target = tab.entry_location(&entry);
        if entry.is_dir {
            tab.navigate(target, env);
            return None;
        }
        match &target {
            Location::Local(path) => {
                if entry.ext() == crate::backup::EXT {
                    return Some(Action::OpenBackup(path.clone()));
                }
                if vfs::is_zip_name(&entry.name) {
                    tab.navigate(Location::Zip { archive: path.clone(), inner: String::new() }, env);
                } else if !win::shell_open(path) {
                    env.bus.error(tf!("„{}\" lässt sich nicht öffnen.", "“{}” cannot be opened.", entry.name));
                }
                None
            }
            _ => Some(Action::OpenRemote(Source { dir, name: entry.name.clone(), is_dir: false, size: entry.size })),
        }
    }

    // ------------------------------------------------------------ Zeichnen

    pub fn ui(&mut self, ui: &mut Ui, rect: Rect, cx: &mut PaneCx) {
        let p = cx.p;
        let border = if cx.active { theme::mix(p.border, p.accent, 0.7) } else { p.border };
        ui.painter().rect(rect, CornerRadius::same(10), p.surface, Stroke::new(1.0, border), StrokeKind::Inside);

        if cx.interactive && !cx.active {
            let hit = ui.input(|i| i.pointer.any_pressed().then(|| i.pointer.interact_pos()).flatten());
            if hit.is_some_and(|pos| rect.contains(pos)) && !egui::Popup::is_any_open(ui.ctx()) {
                cx.actions.push(Action::Activate(cx.index));
            }
        }

        let inner = rect.shrink(1.0);
        let tabs_r = Rect::from_min_size(inner.min, Vec2::new(inner.width(), TAB_H));
        let path_r = Rect::from_min_size(tabs_r.left_bottom(), Vec2::new(inner.width(), PATH_H));
        let status_r = Rect::from_min_max(Pos2::new(inner.left(), inner.bottom() - STATUS_H), inner.max);
        // Zwischen Pfadleiste und Statuszeile: Dateiliste und – wenn eingeschaltet – daneben die Schnellansicht.
        let content_r = Rect::from_min_max(path_r.left_bottom(), status_r.right_top());
        let (list_area, preview_r) = if cx.preview.is_some() { split_for_preview(ui, content_r, cx) } else { (content_r, None) };
        let head_r = Rect::from_min_size(list_area.min, Vec2::new(list_area.width(), HEAD_H));
        let list_r = Rect::from_min_max(head_r.left_bottom(), list_area.max);
        self.list_rect = list_r;

        self.tabs_ui(ui, tabs_r, rect, cx);
        self.path_ui(ui, path_r, cx);
        let mode = match (&self.tab().loc, self.tab().search.is_some()) {
            (_, true) => ColMode::Search,
            // Wie bei Suchtreffern liegt im Papierkorb jeder Eintrag in einem anderen Ordner.
            (Location::RecycleBin, _) => ColMode::Search,
            (loc, _) if loc.is_virtual() => ColMode::Drives,
            _ => ColMode::Normal,
        };
        if list_r.height() > 8.0 {
            self.header_ui(ui, head_r, mode, cx);
            self.list_ui(ui, list_r, mode, cx);
        }
        if let (Some(r), Some(preview)) = (preview_r, cx.preview.as_deref_mut()) {
            preview.ui(ui, r, p);
        }
        self.status_ui(ui, status_r, cx);
    }

    fn tabs_ui(&mut self, ui: &mut Ui, rect: Rect, pane_rect: Rect, cx: &mut PaneCx) {
        let p = cx.p;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius { nw: 9, ne: 9, sw: 0, se: 0 }, p.surface_alt);
        painter.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, p.border_soft));

        let count = self.tabs.len();
        let plus_w = 34.0;
        let tab_w = ((rect.width() - plus_w - 8.0) / count as f32).clamp(40.0, 196.0);
        let first_x = rect.left() + 6.0;
        // So viele Tabs passen in die Leiste.
        let shown = (((rect.right() - plus_w + 5.0 - first_x) / tab_w).floor().max(1.0) as usize).min(count);
        let (mut activate, mut close, mut close_others, mut duplicate) = (None, None, None, None);

        // Tab, der gerade mit der Maus verschoben wird: Stelle, Griffpunkt und Mausposition
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let dragged = self.tab_drag.and_then(|(id, grab)| {
            let index = self.tabs.iter().position(|t| t.id == id)?;
            Some((index, grab, pointer?))
        });
        // Verlässt die Maus die Leiste deutlich, hängt der Tab als Kärtchen an der Maus – Ziel ist dann das andere Fenster.
        let detached = dragged.is_some_and(|(_, _, pos)| !rect.expand2(Vec2::new(40.0, 26.0)).contains(pos));
        let mut order: Vec<usize> = (0..count).collect();
        if let Some((i, ..)) = dragged {
            order.retain(|&x| x != i);
            order.push(i); // zuletzt zeichnen, damit er über den Nachbarn liegt
        }
        let (mut drag_start, mut drag_stop, mut reorder) = (None, None, None);

        for i in order {
            let tab = &self.tabs[i];
            let x = first_x + i as f32 * tab_w;
            let slot = Rect::from_min_max(Pos2::new(x, rect.top() + 5.0), Pos2::new(x + tab_w - 3.0, rect.bottom()));
            if i >= shown {
                continue;
            }
            let mine = dragged.filter(|(d, ..)| *d == i);
            let lifted = mine.filter(|_| !detached);
            let r = match lifted {
                Some((_, grab, pos)) => {
                    let fx = (pos.x - grab).clamp(first_x, first_x + (shown - 1) as f32 * tab_w);
                    slot.translate(Vec2::new(fx - slot.left(), 0.0))
                }
                None => slot,
            };
            let id = Id::new(("tab", tab.id));
            let resp = ui.interact(r, id, Sense::click_and_drag());
            let is_active = i == self.active;
            if lifted.is_some() {
                painter.rect(
                    r,
                    CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 },
                    p.surface,
                    Stroke::new(1.0, theme::mix(p.border, p.accent, 0.8)),
                    StrokeKind::Inside,
                );
            } else if is_active {
                painter.rect_filled(r, CornerRadius { nw: 7, ne: 7, sw: 0, se: 0 }, p.surface);
                let line = if cx.active { p.accent } else { p.text_faint };
                painter.rect_filled(
                    Rect::from_min_size(Pos2::new(r.left() + 9.0, r.top()), Vec2::new(r.width() - 18.0, 2.0)),
                    CornerRadius::same(1),
                    line,
                );
            } else if resp.hovered() && dragged.is_none() {
                painter.rect_filled(r.shrink2(Vec2::new(0.0, 2.0)), CornerRadius::same(6), p.control_hover);
            }
            let (glyph, color) = match (&tab.search, &tab.loc) {
                (Some(_), _) => (icons::MAGNIFYING_GLASS, p.accent),
                (_, Location::Drives) => (icons::DESKTOP, p.accent),
                (_, Location::Network { .. }) => (icons::NETWORK, p.accent),
                (_, Location::RecycleBin) => (icons::TRASH, p.text_dim),
                (_, Location::Device { .. }) => (icons::DEVICES, p.accent),
                (_, Location::Zip { .. }) => (icons::FILE_ZIP, p.archive),
                (_, Location::Local(_)) => (icons::FOLDER, p.folder),
            };
            let cy = r.center().y + 1.0;
            let icon_color = if is_active { color } else { theme::mix(p.surface_alt, color, 0.75) };
            painter.text(Pos2::new(r.left() + 15.0, cy), Align2::CENTER_CENTER, glyph, theme::font(14.0), icon_color);
            let show_close = count > 1 && r.width() > 76.0 && (resp.hovered() || is_active) && dragged.is_none();
            let text_w = r.width() - 28.0 - if show_close { 22.0 } else { 6.0 };
            let (font, color) = if is_active { (theme::semibold(12.5), p.text) } else { (theme::font(12.5), p.text_dim) };
            draw_text(&painter, r.left() + 27.0, cy, text_w, &tab.title(), font, color, false);
            if show_close {
                let close_r = Rect::from_center_size(Pos2::new(r.right() - 13.0, cy), Vec2::splat(18.0));
                let c = ui.interact(close_r, id.with("x"), Sense::click());
                if c.hovered() {
                    painter.rect_filled(close_r, CornerRadius::same(5), p.control_active);
                }
                painter.text(
                    close_r.center(),
                    Align2::CENTER_CENTER,
                    icons::X,
                    theme::font(11.0),
                    if c.hovered() { p.text } else { p.text_faint },
                );
                if c.clicked() {
                    close = Some(i);
                }
            }
            if resp.clicked() {
                activate = Some(i);
            }
            if resp.middle_clicked() {
                close = Some(i);
            }
            if resp.drag_started_by(egui::PointerButton::Primary) {
                let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(r.center());
                drag_start = Some((tab.id, (origin.x - slot.left()).clamp(0.0, tab_w)));
                activate = Some(i);
            }
            if let Some((_, _, pos)) = mine {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                if detached {
                    // Kärtchen an der Maus
                    let ghost = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("tab-kaertchen")));
                    let chip = Rect::from_min_size(pos + Vec2::new(12.0, 12.0), Vec2::new(tab_w.clamp(110.0, 190.0), 28.0));
                    ghost.rect(chip, CornerRadius::same(7), p.surface, Stroke::new(1.0, p.accent), StrokeKind::Inside);
                    ghost.text(Pos2::new(chip.left() + 15.0, chip.center().y), Align2::CENTER_CENTER, glyph, theme::font(14.0), color);
                    draw_text(&ghost, chip.left() + 27.0, chip.center().y, chip.width() - 36.0, &tab.title(), theme::semibold(12.5), p.text, false);
                } else {
                    let target = (((r.center().x - first_x) / tab_w).floor().max(0.0) as usize).min(shown - 1);
                    if target != i {
                        reorder = Some((i, target));
                    }
                }
                if resp.drag_stopped() {
                    drag_stop = Some((tab.id, pos));
                }
            }
            resp.context_menu(|ui| {
                menu_begin(ui, 210.0);
                if menu_item(ui, p, icons::COPY, t("Tab duplizieren", "Duplicate tab"), &key("Strg+T"), true) {
                    duplicate = Some(i);
                }
                if menu_item(ui, p, icons::X, t("Tab schließen", "Close tab"), &key("Strg+W"), count > 1) {
                    close = Some(i);
                }
                if menu_item(ui, p, icons::TABS, t("Andere Tabs schließen", "Close other tabs"), "", count > 1) {
                    close_others = Some(i);
                }
            });
        }

        let x = first_x + shown as f32 * tab_w;
        let plus_r = Rect::from_center_size(Pos2::new((x + 13.0).min(rect.right() - 17.0), rect.center().y + 2.0), Vec2::splat(24.0));
        let plus = icon_button(ui, plus_r, Id::new(("tab-plus", cx.index)), icons::PLUS, p, true, false);
        if plus.on_hover_text(t("Neuer Tab (Strg+T)", "New tab (Ctrl+T)")).clicked() {
            duplicate = Some(self.active);
        }

        if let Some(i) = activate {
            self.active = i;
            self.path_edit = None;
            cx.actions.push(Action::Activate(cx.index));
        }
        if let Some(i) = duplicate {
            let loc = self.tabs[i].loc.clone();
            self.active = i;
            self.add_tab(loc, &mut cx.env);
        }
        if let Some(i) = close_others {
            let keep = self.tabs.remove(i);
            for t in self.tabs.drain(..) {
                t.size_cancel.store(true, Relaxed);
            }
            self.tabs.push(keep);
            self.active = 0;
        }
        if let Some(i) = close {
            self.close_tab(i);
        }

        // Verschieben mit der Maus
        if let Some((from, to)) = reorder.filter(|&(from, to)| from < self.tabs.len() && to < self.tabs.len()) {
            let active_id = self.tabs[self.active].id;
            let tab = self.tabs.remove(from);
            self.tabs.insert(to, tab);
            self.active = self.tabs.iter().position(|t| t.id == active_id).unwrap_or(0);
        }
        if drag_start.is_some() {
            self.tab_drag = drag_start;
        }
        if let Some((id, pos)) = drag_stop {
            self.tab_drag = None;
            if !pane_rect.contains(pos) {
                cx.actions.push(Action::MoveTab { from: cx.index, tab: id, pos });
            }
        } else if self.tab_drag.is_some() && !ui.input(|i| i.pointer.primary_down()) {
            self.tab_drag = None;
        }
    }

    fn path_ui(&mut self, ui: &mut Ui, rect: Rect, cx: &mut PaneCx) {
        let p = cx.p;
        let painter = ui.painter_at(rect);
        painter.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, p.border_soft));
        let cy = rect.center().y;
        let pane = cx.index;
        let mut cmd: Option<Cmd> = None;
        let mut x = rect.left() + 6.0;
        let button = |x: f32| Rect::from_center_size(Pos2::new(x + 13.0, cy), Vec2::splat(26.0));

        let (can_back, can_fwd, can_up) = {
            let t = self.tab();
            (!t.back.is_empty(), !t.forward.is_empty(), t.loc.parent().is_some() || t.search.is_some())
        };
        if icon_button(ui, button(x), Id::new(("nav-back", pane)), icons::ARROW_LEFT, p, can_back, false)
            .on_hover_text(t("Zurück (Alt+←)", "Back (Alt+←)"))
            .clicked()
        {
            cmd = Some(Cmd::Back);
        }
        x += 27.0;
        if icon_button(ui, button(x), Id::new(("nav-fwd", pane)), icons::ARROW_RIGHT, p, can_fwd, false)
            .on_hover_text(t("Vorwärts (Alt+→)", "Forward (Alt+→)"))
            .clicked()
        {
            cmd = Some(Cmd::Forward);
        }
        x += 27.0;
        if icon_button(ui, button(x), Id::new(("nav-up", pane)), icons::ARROW_UP, p, can_up, false)
            .on_hover_text(t("Übergeordneter Ordner (Rücktaste)", "Parent folder (Backspace)"))
            .clicked()
        {
            cmd = Some(Cmd::Up);
        }
        x += 31.0;

        // Laufwerks-Schaltfläche
        let (glyph, label) = match &self.tab().loc {
            Location::Local(path) => {
                let root = path.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
                let glyph = if root.starts_with("\\\\") { icons::NETWORK } else { icons::HARD_DRIVE };
                (glyph, if root.len() > 12 { t("Netz", "Network").to_owned() } else { root })
            }
            Location::Drives => (icons::DESKTOP, "PC".to_owned()),
            Location::Network { .. } => (icons::NETWORK, t("Netz", "Network").to_owned()),
            Location::RecycleBin => (icons::TRASH, t("Papierkorb", "Recycle Bin").to_owned()),
            Location::Device { name, .. } => (icons::DEVICES, name.clone()),
            Location::Zip { archive, .. } => (icons::FILE_ZIP, crate::archive::label(archive).to_owned()),
        };
        let label_w = text_width(ui, &label, theme::semibold(12.5)).min(110.0);
        let drive_r = Rect::from_min_size(Pos2::new(x, cy - 13.0), Vec2::new(label_w + 48.0, 26.0));
        let drive = ui.interact(drive_r, Id::new(("drive", pane)), Sense::click());
        let open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&drive));
        let fill = if drive.hovered() || open { p.control_hover } else { p.control };
        painter.rect(drive_r, CornerRadius::same(7), fill, Stroke::new(1.0, p.border), StrokeKind::Inside);
        painter.text(Pos2::new(drive_r.left() + 14.0, cy), Align2::CENTER_CENTER, glyph, theme::font(15.0), p.accent);
        draw_text(&painter, drive_r.left() + 26.0, cy, label_w, &label, theme::semibold(12.5), p.text, false);
        painter.text(
            Pos2::new(drive_r.right() - 11.0, cy),
            Align2::CENTER_CENTER,
            icons::CARET_DOWN,
            theme::font(10.0),
            p.text_faint,
        );
        egui::Popup::menu(&drive).width(310.0).show(|ui| {
            if let Some(c) = self.drive_menu(ui, cx) {
                cmd = Some(c);
            }
        });
        x = drive_r.right() + 8.0;

        // Filterfeld rechts
        let filter_w = if rect.width() < 360.0 { 0.0 } else { (rect.width() * 0.27).clamp(104.0, 230.0) };
        let filter_r = Rect::from_min_max(Pos2::new(rect.right() - 8.0 - filter_w, cy - 13.0), Pos2::new(rect.right() - 8.0, cy + 13.0));
        let crumbs_right = filter_r.left() - 8.0;
        let crumbs_r = Rect::from_min_max(Pos2::new(x, cy - 13.0), Pos2::new(crumbs_right, cy + 13.0));

        if crumbs_r.width() > 30.0 {
            if let Some(c) = self.crumbs_ui(ui, crumbs_r, cx) {
                cmd = Some(c);
            }
        }
        if filter_w > 0.0 {
            self.filter_ui(ui, filter_r, cx);
        } else {
            // Ohne Suchfeld (sehr schmales Fenster) verfallen die Wünsche, statt später unvermittelt zu greifen.
            self.focus_filter = false;
        }
        if let Some(c) = cmd {
            cx.actions.push(Action::Cmd(pane, c));
        }
    }

    fn drive_menu(&self, ui: &mut Ui, cx: &PaneCx) -> Option<Cmd> {
        let p = cx.p;
        let mut cmd = None;
        menu_begin(ui, 310.0);
        menu_caption(ui, p, t("LAUFWERKE", "DRIVES"));
        for d in cx.drives {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(310.0, 40.0), Sense::click());
            let painter = ui.painter();
            if resp.hovered() {
                painter.rect_filled(rect, CornerRadius::same(6), p.control_hover);
            }
            let glyph = match d.kind {
                win::DriveKind::Removable => icons::USB,
                win::DriveKind::Network => icons::NETWORK,
                win::DriveKind::Optical => icons::DISC,
                _ => icons::HARD_DRIVE,
            };
            painter.text(Pos2::new(rect.left() + 19.0, rect.center().y), Align2::CENTER_CENTER, glyph, theme::font(18.0), p.accent);
            draw_text(painter, rect.left() + 38.0, rect.top() + 13.0, 170.0, &d.caption(), theme::font(13.0), p.text, false);
            let info = if d.total > 0 {
                tf!("{} frei von {}", "{} free of {}", fmt_size(d.free), fmt_size(d.total))
            } else {
                t("nicht bereit", "not ready").to_owned()
            };
            draw_text(painter, rect.left() + 38.0, rect.top() + 28.0, 170.0, &info, theme::font(11.0), p.text_faint, false);
            if d.total > 0 {
                let used = 1.0 - d.free as f32 / d.total as f32;
                let bar = Rect::from_min_size(Pos2::new(rect.right() - 86.0, rect.center().y - 2.5), Vec2::new(76.0, 5.0));
                progress_bar(painter, bar, used, p.control_active, if used > 0.9 { p.danger } else { p.accent });
            }
            if resp.clicked() {
                cmd = Some(Cmd::GoTo(Location::Local(PathBuf::from(&d.root))));
            }
        }
        if menu_item(ui, p, icons::DESKTOP, t("Dieser PC", "This PC"), "", true) {
            cmd = Some(Cmd::GoTo(Location::Drives));
        }
        if menu_item(ui, p, icons::NETWORK, t("Netzwerk", "Network"), "", true) {
            cmd = Some(Cmd::GoTo(Location::Network { server: None }));
        }
        // Die Computer, die bei der letzten Suche im Netzwerk gefunden wurden, ohne Umweg
        for computer in cx.env.cfg.known_computers.iter().take(8) {
            if menu_item(ui, p, icons::DESKTOP_TOWER, computer, "", true) {
                cmd = Some(Cmd::GoTo(Location::Network { server: Some(computer.clone()) }));
            }
        }
        // Verbundene Telefone und Cloud-Dienste, die Windows neben „Dieser PC" einhängt
        for place in cx.places {
            let glyph = if place.phone { icons::DEVICE_MOBILE } else { icons::CLOUD };
            if menu_item(ui, p, glyph, &place.name, "", true) {
                cmd = Some(Cmd::GoTo(Location::Local(place.path.clone())));
            }
        }
        // Per Kabel angeschlossene Telefone und Kameras
        for (id, name) in cx.devices {
            if menu_item(ui, p, icons::DEVICES, name, t("am Kabel", "on a cable"), true) {
                cmd = Some(Cmd::GoTo(Location::Device { id: id.clone(), name: name.clone(), path: "/".to_owned() }));
            }
        }
        if menu_item(ui, p, icons::TRASH, t("Papierkorb", "Recycle Bin"), "", true) {
            cmd = Some(Cmd::GoTo(Location::RecycleBin));
        }
        menu_separator(ui, p);
        menu_caption(ui, p, t("ORDNER", "FOLDERS"));
        for (name, path) in cx.folders {
            let glyph = match *name {
                "Desktop" => icons::MONITOR,
                "Downloads" => icons::DOWNLOAD_SIMPLE,
                "Bilder" => icons::IMAGES,
                "Musik" => icons::MUSIC_NOTES,
                "Videos" => icons::FILM_STRIP,
                cloud if cloud.starts_with("OneDrive") || cloud.starts_with("Dropbox") => icons::CLOUD,
                _ => icons::FOLDER,
            };
            if menu_item(ui, p, glyph, win::folder_label(name), "", true) {
                cmd = Some(Cmd::GoTo(Location::Local(path.clone())));
            }
        }
        cmd
    }

    fn crumbs_ui(&mut self, ui: &mut Ui, rect: Rect, cx: &mut PaneCx) -> Option<Cmd> {
        let p = cx.p;
        let pane = cx.index;
        let painter = ui.painter_at(rect);
        let cy = rect.center().y;
        let mut cmd = None;

        if let Some(text) = &mut self.path_edit {
            painter.rect(rect, CornerRadius::same(7), p.field, Stroke::new(1.0, p.accent), StrokeKind::Inside);
            let id = Id::new(("path-edit", pane));
            if self.path_edit_fresh {
                ui.memory_mut(|m| m.request_focus(id));
            }
            let edit = egui::TextEdit::singleline(text)
                .id(id)
                .frame(egui::Frame::NONE)
                .font(theme::font(13.0))
                .desired_width(rect.width() - 16.0);
            let resp = ui.put(rect.shrink2(Vec2::new(8.0, 2.0)), edit);
            if self.path_edit_fresh {
                self.path_edit_fresh = false;
                select_all(ui.ctx(), id, text.chars().count());
            }
            if resp.lost_focus() {
                if ui.input(|i| i.key_pressed(Key::Enter)) {
                    match Location::parse(text) {
                        Some(loc) => cmd = Some(Cmd::GoTo(loc)),
                        None => cx.env.bus.error(tf!("„{}\" wurde nicht gefunden.", "“{}” was not found.", text.trim())),
                    }
                }
                self.path_edit = None;
            }
            return cmd;
        }

        let crumbs = self.tab().loc.crumbs();
        let font = theme::font(13.0);
        let widths: Vec<f32> = crumbs.iter().map(|(label, _)| text_width(ui, label, font.clone()) + 14.0).collect();
        const SEP: f32 = 13.0;
        // Von hinten so viele Krumen nehmen, wie hineinpassen.
        let mut start = crumbs.len();
        let mut used = 0.0;
        while start > 0 {
            let w = widths[start - 1] + if start < crumbs.len() { SEP } else { 0.0 };
            if used + w > rect.width() - 22.0 && start < crumbs.len() {
                break;
            }
            used += w;
            start -= 1;
        }
        let mut x = rect.left();
        if start > 0 {
            painter.text(Pos2::new(x + 8.0, cy), Align2::CENTER_CENTER, "…", font.clone(), p.text_faint);
            x += 18.0;
        }
        let last = crumbs.len() - 1;
        for (i, (label, loc)) in crumbs.iter().enumerate().skip(start) {
            if i > start {
                painter.text(
                    Pos2::new(x + SEP / 2.0, cy),
                    Align2::CENTER_CENTER,
                    icons::CARET_RIGHT,
                    theme::font(9.0),
                    p.text_faint,
                );
                x += SEP;
            }
            let w = widths[i].min(rect.right() - x);
            let r = Rect::from_min_size(Pos2::new(x, cy - 12.0), Vec2::new(w, 24.0));
            let resp = ui.interact(r, Id::new(("crumb", pane, i)), Sense::click());
            if resp.hovered() {
                painter.rect_filled(r, CornerRadius::same(6), p.control_hover);
            }
            let (f, color) = if i == last { (theme::semibold(13.0), p.text) } else { (font.clone(), p.text_dim) };
            draw_text(&painter, r.left() + 7.0, cy, w - 10.0, label, f, color, false);
            if resp.clicked() {
                if i == last {
                    self.edit_path();
                } else {
                    cmd = Some(Cmd::GoTo(loc.clone()));
                }
            }
            x += w;
        }
        // Freie Fläche: Klick wechselt in die Texteingabe.
        if rect.right() - x > 6.0 {
            let rest = Rect::from_min_max(Pos2::new(x, rect.top()), rect.max);
            let resp = ui.interact(rest, Id::new(("crumb-rest", pane)), Sense::click());
            if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::Text);
            }
            if resp.clicked() {
                self.edit_path();
            }
        }
        cmd
    }

    fn filter_ui(&mut self, ui: &mut Ui, rect: Rect, cx: &mut PaneCx) {
        let p = cx.p;
        let pane = cx.index;
        let id = Id::new(("filter", pane));
        let focused = ui.memory(|m| m.has_focus(id));
        let tab = &mut self.tabs[self.active];
        let searching = tab.search.is_some();
        // Lokale Ordner werden samt Unterordnern durchsucht; in Archiven wird nur der Ordner gefiltert.
        let local = tab.loc.is_local();
        let painter = ui.painter_at(rect);
        let border = if focused || searching { p.accent } else { p.border };
        painter.rect(rect, CornerRadius::same(7), p.field, Stroke::new(1.0, border), StrokeKind::Inside);
        painter.text(
            Pos2::new(rect.left() + 13.0, rect.center().y),
            Align2::CENTER_CENTER,
            icons::MAGNIFYING_GLASS,
            theme::font(13.0),
            if focused || searching { p.accent } else { p.text_faint },
        );
        if std::mem::take(&mut self.focus_filter) {
            ui.memory_mut(|m| m.request_focus(id));
        }
        // Rechts im Feld: „Leeren".
        let can_clear = !tab.filter.is_empty();
        let mut right = rect.right() - 3.0;
        let clear_r = can_clear.then(|| {
            right -= 20.0;
            Rect::from_center_size(Pos2::new(right + 10.0, rect.center().y), Vec2::splat(20.0))
        });
        let inner = Rect::from_min_max(
            Pos2::new(rect.left() + 24.0, rect.top() + 2.0),
            Pos2::new(right - 3.0, rect.bottom() - 2.0),
        );
        let hint = match (local, inner.width() > 150.0) {
            (true, true) => t("Suchen, auch in Unterordnern", "Search, including subfolders"),
            (true, false) => t("Suchen", "Search"),
            (false, _) => "Filter",
        };
        let edit = egui::TextEdit::singleline(&mut tab.filter)
            .id(id)
            .frame(egui::Frame::NONE)
            .font(theme::font(12.5))
            .hint_text(egui::RichText::new(hint).color(p.text_faint).size(12.0))
            .desired_width(inner.width());
        let resp = ui.put(inner, edit);
        let (enter, escape, now) = ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape), i.time));
        // Pfeil nach unten springt aus dem Suchfeld zurück in die Liste.
        if resp.has_focus() && ui.input(|i| i.key_pressed(Key::ArrowDown)) {
            ui.memory_mut(|m| m.surrender_focus(id));
        }
        let query_len = tab.filter.trim().chars().count();
        if resp.changed() {
            if query_len == 0 {
                tab.search_due = None;
                if searching {
                    tab.end_search(&mut cx.env, false);
                } else {
                    tab.rebuild_rows();
                }
            } else {
                if !searching {
                    // Sofort: den aktuellen Ordner filtern …
                    tab.cursor = 0;
                    tab.rebuild_rows();
                    tab.scroll_to = Some(0.0);
                    tab.scroll_y = 0.0;
                }
                if local && query_len >= SEARCH_MIN_CHARS {
                    // … und nach einer kurzen Tipp-Pause die Unterordner dazunehmen.
                    tab.search_due = Some(now + SEARCH_DELAY);
                } else {
                    tab.search_due = None;
                    if searching {
                        tab.end_search(&mut cx.env, true);
                    }
                }
            }
        }
        let same_query = tab.search.as_ref().is_some_and(|s| s.query == tab.filter.trim());
        if resp.lost_focus() && enter {
            tab.search_due = None;
            if query_len == 0 {
                tab.end_search(&mut cx.env, false);
            } else if !same_query {
                tab.start_search(&mut cx.env);
            }
        } else if resp.lost_focus() && escape {
            tab.clear_search(&mut cx.env);
        }
        if let Some(due) = tab.search_due {
            if now >= due {
                tab.search_due = None;
                if !same_query {
                    tab.start_search(&mut cx.env);
                }
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(due - now));
            }
        }
        if let Some(x_r) = clear_r {
            let c = ui.interact(x_r, id.with("clear"), Sense::click());
            painter.text(x_r.center(), Align2::CENTER_CENTER, icons::X, theme::font(11.0), if c.hovered() { p.text } else { p.text_faint });
            if c.clicked() {
                tab.clear_search(&mut cx.env);
            }
        }
    }

    fn header_ui(&mut self, ui: &mut Ui, rect: Rect, mode: ColMode, cx: &mut PaneCx) {
        let p = cx.p;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0, p.surface);
        painter.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, p.border_soft));
        let cols = layout_cols(rect, cx.env.cfg, mode);
        let cy = rect.center().y;
        let (sort, desc) = (self.tab().sort, self.tab().desc);
        let chosen: Vec<Col> = cx.env.cfg.columns.clone();
        // Aus dem Kontextmenü: Spalte ein- oder ausschalten; vom Ziehen: (gezogene Spalte, Stelle des Loslassens)
        let mut toggle_col: Option<Col> = None;
        let mut dragged_col: Option<(Col, f32, bool)> = None;
        let font = theme::semibold(11.5);
        let mut clicked: Option<SortKey> = None;
        // Aus dem Kontextmenü des Spaltenkopfs
        let (mut fit_all, mut reset_widths) = (false, false);

        // `col`: eine wählbare Spalte – sie lässt sich an eine andere Stelle ziehen.
        let mut cell = |ui: &mut Ui, range: (f32, f32), label: &str, key: Option<SortKey>, right: bool, salt: &str, col: Option<Col>| {
            let r = Rect::from_min_max(Pos2::new(range.0, rect.top()), Pos2::new(range.1, rect.bottom()));
            let active = key.is_some() && key == Some(sort);
            let mut color = if active { p.text } else { p.text_dim };
            if key.is_some() || col.is_some() {
                let sense = if col.is_some() { Sense::click_and_drag() } else { Sense::click() };
                let resp = ui.interact(r, Id::new(("head", cx.index, salt)), sense);
                if resp.hovered() {
                    color = p.text;
                }
                if let (true, Some(k)) = (resp.clicked(), key) {
                    clicked = Some(k);
                }
                if let (Some(col), Some(pos)) = (col, resp.interact_pointer_pos()) {
                    if resp.dragged() || resp.drag_stopped() {
                        dragged_col = Some((col, pos.x, resp.drag_stopped()));
                        color = p.accent;
                    }
                }
                resp.context_menu(|ui| {
                    menu_begin(ui, 270.0);
                    if menu_item(
                        ui,
                        p,
                        icons::ARROWS_LEFT_RIGHT,
                        t("Alle Spalten an Inhalt anpassen", "Fit all columns to content"),
                        "",
                        true,
                    ) {
                        fit_all = true;
                    }
                    if menu_item(ui, p, icons::ARROW_COUNTER_CLOCKWISE, t("Standardbreiten", "Default widths"), "", true) {
                        reset_widths = true;
                    }
                    // Welche Spalten gezeigt werden – gilt für beide Fenster
                    menu_separator(ui, p);
                    for choice in Col::ALL {
                        let icon = if chosen.contains(&choice) { icons::CHECK_SQUARE } else { icons::SQUARE };
                        if menu_item(ui, p, icon, col_title(choice), "", true) {
                            toggle_col = Some(choice);
                        }
                    }
                });
            }
            let caret = if active { 14.0 } else { 0.0 };
            let w = text_width(ui, label, font.clone()).min(r.width() - caret - 4.0);
            let x = if right { r.right() - w - caret - 4.0 } else { r.left() + 4.0 };
            draw_text(&painter, x, cy, w, label, font.clone(), color, false);
            if active {
                painter.text(
                    Pos2::new(x + w + 8.0, cy),
                    Align2::CENTER_CENTER,
                    if desc { icons::CARET_DOWN } else { icons::CARET_UP },
                    theme::font(9.0),
                    p.accent,
                );
            }
        };

        match mode {
            ColMode::Drives => {
                let (first, third) = match &self.tab().loc {
                    Location::Network { server: None } => ("Computer", ""),
                    Location::Network { server: Some(_) } => (t("Freigabe", "Share"), t("Pfad", "Path")),
                    _ => ("Name", t("Speicher / Ort", "Storage / location")),
                };
                cell(ui, (cols.name.0 + 24.0, cols.name.1), first, None, false, "name", None);
                if let Some(c) = cols.kind {
                    cell(ui, c, t("Beschreibung", "Description"), None, false, "kind", None);
                }
                if let Some(c) = cols.date {
                    cell(ui, c, third, None, false, "date", None);
                }
            }
            _ => {
                cell(ui, (cols.name.0 + 24.0, cols.name.1), "Name", Some(SortKey::Name), false, "name", None);
                let loc = self.tab().loc.clone();
                if let Some(c) = cols.note {
                    let label = if loc == Location::RecycleBin { t("Ursprünglicher Ort", "Original location") } else { t("Ordner", "Folder") };
                    cell(ui, c, label, None, false, "note", None);
                }
                for col in cols.order.iter().copied() {
                    let Some(c) = cols.range(col) else { continue };
                    // Einzug, Sortierschlüssel und Ausrichtung wie die Werte in den Zeilen
                    let (range, key, right, salt) = match col {
                        Col::Size => (c, Some(SortKey::Size), true, "size"),
                        Col::Kind => ((c.0 + 14.0, c.1), Some(SortKey::Ext), false, "kind"),
                        Col::Date => (c, Some(SortKey::Date), false, "date"),
                    };
                    // Das Datum meint im Papierkorb das Löschen.
                    let label = match (col, &loc) {
                        (Col::Date, Location::RecycleBin) => t("Gelöscht", "Deleted"),
                        _ => col_title(col),
                    };
                    cell(ui, range, label, key, right, salt, Some(col));
                }
            }
        }
        if let Some(k) = clicked {
            self.tab_mut().set_sort(k);
        }
        if let Some(col) = toggle_col {
            cx.env.cfg.toggle_column(col);
        }
        // Eine Spalte wird gezogen: Zielspalte hervorheben, beim Loslassen umsortieren.
        if let Some((col, x, dropped)) = dragged_col {
            let target = cols.order.iter().copied().find(|other| cols.range(*other).is_some_and(|c| x >= c.0 && x < c.1));
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            if let Some(target) = target.filter(|target| *target != col) {
                if let Some(c) = cols.range(target) {
                    let mark = Rect::from_min_max(Pos2::new(c.0, rect.top() + 3.0), Pos2::new(c.1, rect.bottom() - 3.0));
                    // Durchscheinend, damit die Überschrift der Zielspalte lesbar bleibt
                    let fill = egui::Color32::from_rgba_unmultiplied(p.accent.r(), p.accent.g(), p.accent.b(), 34);
                    ui.painter().rect(mark, CornerRadius::same(5), fill, Stroke::new(1.0, p.accent), StrokeKind::Inside);
                }
                if dropped {
                    cx.env.cfg.move_column(col, target);
                }
            }
        }

        // Trennlinien: immer sichtbar, ziehen ändert die Breite der Spalte rechts davon,
        // Doppelklick passt diese Spalte an ihren längsten Eintrag an.
        if mode == ColMode::Drives {
            return;
        }
        let mut fit: Vec<FitCol> = Vec::new();
        {
            let cfg = &mut *cx.env.cfg;
            let idle = theme::mix(p.border, p.text_faint, 0.45);
            let mut grip = |ui: &mut Ui, x: f32, width: &mut f32, col: FitCol, salt: &str| {
                let x = x.round();
                let r = Rect::from_center_size(Pos2::new(x, cy), Vec2::new(9.0, rect.height()));
                let resp = ui.interact(r, Id::new(("grip", cx.index, salt)), Sense::click_and_drag());
                let hot = resp.hovered() || resp.dragged();
                let (stroke, inset) = if hot { (Stroke::new(2.0, p.accent), 3.0) } else { (Stroke::new(1.0, idle), 6.0) };
                ui.painter().vline(x, egui::Rangef::new(rect.top() + inset, rect.bottom() - inset), stroke);
                if hot {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
                if resp.dragged() {
                    *width = (*width - resp.drag_delta().x).clamp(COL_MIN, COL_MAX);
                }
                if resp.double_clicked() {
                    fit.push(col);
                }
                resp.on_hover_text(t(
                    "Ziehen ändert die Breite · Doppelklick passt die Spalte an",
                    "Drag to change the width · double-click to fit the column",
                ));
            };
            if let Some(c) = cols.size {
                grip(ui, c.0 - 4.0, &mut cfg.col_size, FitCol::Size, "size");
            }
            if let Some(c) = cols.kind {
                grip(ui, c.0 + 7.0, &mut cfg.col_type, FitCol::Kind, "kind");
            }
            if let Some(c) = cols.date {
                grip(ui, c.0, &mut cfg.col_date, FitCol::Date, "date");
            }
            if reset_widths {
                let defaults = Config::default();
                cfg.col_size = defaults.col_size;
                cfg.col_type = defaults.col_type;
                cfg.col_date = defaults.col_date;
            }
        }
        if fit_all {
            fit = vec![FitCol::Size, FitCol::Kind, FitCol::Date];
        }
        for col in fit {
            let width = self.fit_width(ui, col, cx.types);
            let cfg = &mut *cx.env.cfg;
            match col {
                FitCol::Size => cfg.col_size = width,
                FitCol::Kind => cfg.col_type = width,
                FitCol::Date => cfg.col_date = width,
            }
        }
    }

    /// Breite, bei der der längste Eintrag (und die Überschrift) der Spalte gerade ganz zu sehen ist.
    fn fit_width(&self, ui: &Ui, col: FitCol, types: &mut HashMap<String, String>) -> f32 {
        let tab = self.tab();
        let row_font = theme::font(12.5);
        let head_font = theme::semibold(11.5);
        // Überschrift samt Sortierpfeil muss auch hineinpassen.
        let head = |label: &str| text_width(ui, label, head_font.clone()) + 14.0;
        let entries = || tab.rows.iter().map(|&i| &tab.entries[i as usize]).filter(|e| !e.is_parent);
        let (content, padding) = match col {
            FitCol::Size => {
                let texts = entries().filter_map(|e| {
                    if e.is_dir { tab.sizes.get(&e.name).map(|s| fmt_size(s.size)) } else { Some(fmt_size(e.size)) }
                });
                (widest(ui, texts, &row_font).max(head(t("Größe", "Size"))), 16.0)
            }
            FitCol::Kind => {
                let mut names: HashSet<String> = HashSet::new();
                for e in entries() {
                    let key = if e.is_dir { "\u{0}dir" } else { e.ext() };
                    if !types.contains_key(key) {
                        types.insert(key.to_owned(), win::type_name(e.ext(), e.is_dir));
                    }
                    if !names.contains(&types[key]) {
                        names.insert(types[key].clone());
                    }
                }
                (widest(ui, names.into_iter(), &row_font).max(head(t("Typ", "Type"))), 30.0)
            }
            FitCol::Date => {
                // Ziffern sind in der Schrift gleich breit: ein Muster je Schreibweise genügt.
                let samples = [
                    t("00.00.0000, 00:00", "0000-00-00, 00:00"),
                    t("Gestern, 00:00", "Yesterday, 00:00"),
                    t("Heute, 00:00", "Today, 00:00"),
                ]
                .into_iter()
                .map(str::to_owned);
                let any = entries().any(|e| e.modified.is_some());
                let content = if any { widest(ui, samples, &row_font) } else { 0.0 };
                (content.max(head(t("Geändert", "Modified"))), 14.0)
            }
        };
        (content + padding).ceil().clamp(COL_MIN, COL_MAX)
    }

    fn list_ui(&mut self, ui: &mut Ui, rect: Rect, mode: ColMode, cx: &mut PaneCx) {
        let p = cx.p;
        let pane = cx.index;
        let cols = layout_cols(rect, cx.env.cfg, mode);
        let tab = &mut self.tabs[self.active];
        let tab_id = tab.id;
        // Rasteransicht: so viele Kacheln je Zeile, wie nebeneinander passen; Übersichten bleiben Listen.
        let grid = tab.grid && mode != ColMode::Drives;
        // Strg+Mausrad über den Kacheln ändert ihre Größe.
        if grid && cx.interactive && ui.rect_contains_pointer(rect) {
            let zoom = ui.input(|i| i.zoom_delta());
            if zoom != 1.0 {
                cx.env.cfg.thumb_size = (cx.env.cfg.thumb_size * zoom).clamp(THUMB_MIN, THUMB_MAX);
            }
        }
        let thumb = cx.env.cfg.thumb_size.clamp(THUMB_MIN, THUMB_MAX);
        let (tile_min_w, tile_h) = tile_size(thumb);
        // Ändert sich die Größe, bleibt der Eintrag oben links in der obersten Zeile: dieselben Dateien, nur größer
        // oder kleiner. Über die ganze Bewegung (Mausrad, Regler) gilt derselbe Eintrag, sonst wanderte die Liste.
        let resized = grid && tab.thumb_seen > 0.0 && tab.thumb_seen != thumb && tab.line_h > 0.0;
        if resized {
            let now = ui.input(|i| i.time);
            let row = match tab.zoom_anchor {
                Some((row, at)) if now - at < 0.6 => row,
                _ => tab.row_near(tab.scroll_y),
            };
            tab.zoom_anchor = Some((row, now));
        }
        let top_row = if resized { tab.zoom_anchor.map(|(row, _)| row) } else { None };
        tab.thumb_seen = if grid { thumb } else { 0.0 };
        let per_line = if grid { ((rect.width() - 8.0) / tile_min_w).floor().max(1.0) as usize } else { 1 };
        let tile_w = if grid { ((rect.width() - 8.0) / per_line as f32).floor() } else { rect.width() };
        let line_h = if grid { tile_h } else { ROW_H };
        (tab.per_line, tab.tile_w, tab.line_h) = (per_line, tile_w, line_h);
        tab.plan_layout();
        if let Some(row) = top_row {
            tab.scroll_to = Some(tab.row_top(row.min(tab.rows.len().saturating_sub(1))));
        }
        if std::mem::take(&mut tab.reveal_pending) {
            tab.reveal_cursor();
        }

        let bg = ui.interact(rect, Id::new(("list-bg", tab_id)), Sense::click_and_drag());
        let (pressed, released, mods, pointer, now) =
            ui.input(|i| (i.pointer.primary_pressed(), i.pointer.any_released(), i.modifiers, i.pointer.latest_pos(), i.time));
        let double_click = ui.ctx().options(|o| o.input_options.max_double_click_delay);
        let dragging = DragAndDrop::has_payload_of_type::<DragPayload>(ui.ctx());
        let popup_open = egui::Popup::is_any_open(ui.ctx());

        let mut events: Vec<RowEvent> = Vec::new();
        let mut menu_cmds: Vec<Cmd> = Vec::new();
        let mut rename = tab.rename.take();
        let rename_id = Id::new(("rename", tab_id));

        let info = MenuInfo::of(tab, cx.can_paste);
        let mut area = egui::ScrollArea::vertical().id_salt(("rows", tab_id)).auto_shrink([false, false]);
        if let Some(y) = tab.scroll_to.take() {
            area = area.vertical_scroll_offset(y.max(0.0));
        }
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect).id_salt(("list", tab_id)));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        child.spacing_mut().item_spacing = Vec2::ZERO;

        let types = &mut *cx.types;
        let shell_icons = &mut *cx.icons;
        let thumbs = &mut *cx.thumbs;
        let icon_px = (16.0 * ui.ctx().pixels_per_point()).round().clamp(16.0, 64.0) as u32;
        let today = cx.today;
        let pane_active = cx.active;
        let interactive = cx.interactive;
        let tab_ref = &*tab;
        let content_h = tab_ref.content_height();
        let out = area.show_viewport(&mut child, |ui, viewport| {
            let width = ui.available_width();
            // Der ganze Inhalt gibt die Höhe vor; gezeichnet werden nur die Zeilen im Bild, jede an ihrer Stelle.
            ui.set_min_height(content_h);
            let (left, origin) = (ui.max_rect().left(), ui.max_rect().top());
            let visible = tab_ref.visible_lines(viewport.min.y, viewport.max.y);
            if grid {
                let scale = ui.ctx().pixels_per_point();
                let thumb_px = thumb_pixels(thumb * scale);
                let name_font = theme::font(12.5);
                for line in &visible {
                    if let Some(title) = line.title {
                        let r = Rect::from_min_size(Pos2::new(left, origin + line.top), Vec2::new(width, GROUP_H));
                        group_title(ui, r, title, line.rows.len(), p);
                        continue;
                    }
                    let line_r = Rect::from_min_size(Pos2::new(left, origin + line.top), Vec2::new(width, tile_h));
                    for (col, row) in line.rows.clone().enumerate() {
                        let ei = tab_ref.rows[row] as usize;
                        let e = &tab_ref.entries[ei];
                        let r = Rect::from_min_size(Pos2::new(line_r.left() + 4.0 + col as f32 * tile_w, line_r.top()), Vec2::new(tile_w, tile_h));
                        let resp = ui.interact(r, Id::new((tab_id, "row", ei)), Sense::click_and_drag());
                        let tile = r.shrink2(Vec2::new(3.0, 2.0));
                        let picture = Rect::from_center_size(Pos2::new(tile.center().x, tile.top() + 6.0 + thumb * 0.5), Vec2::splat(thumb));
                        let label = if e.is_parent { t("Übergeordneter Ordner", "Parent folder") } else { e.name.as_str() };
                        let name_w = text_width(ui, label, name_font.clone()).min(tile.width() - 10.0);
                        let name_r = Rect::from_center_size(Pos2::new(tile.center().x, picture.bottom() + 14.0), Vec2::new(name_w + 8.0, 20.0));

                        if interactive && !popup_open {
                            if pressed && resp.contains_pointer() {
                                events.push(RowEvent::Press(row, pointer.is_some_and(|pos| name_r.contains(pos))));
                            }
                            if resp.double_clicked() {
                                events.push(RowEvent::Open(row));
                            } else if resp.clicked() {
                                events.push(RowEvent::Click(row));
                            }
                            if resp.middle_clicked() {
                                events.push(RowEvent::Middle(row));
                            }
                            if resp.drag_started_by(egui::PointerButton::Primary) {
                                // Auf Bild oder Name begonnen: die Datei wird gezogen; daneben: Auswahlrechteck.
                                let origin = ui.input(|i| i.pointer.press_origin());
                                let on_item = !e.is_parent && origin.is_some_and(|pos| picture.contains(pos) || name_r.contains(pos));
                                events.push(RowEvent::DragStart(row, on_item));
                            }
                        }
                        if resp.secondary_clicked() {
                            events.push(RowEvent::Secondary(row));
                        }
                        if !e.is_parent {
                            resp.context_menu(|ui| entry_menu(ui, p, &info, &mut menu_cmds));
                        }
                        if !ui.is_rect_visible(r) {
                            continue;
                        }

                        let painter = ui.painter_at(r);
                        let marked = tab_ref.marked[ei];
                        let is_cursor = row == tab_ref.cursor && !tab_ref.unselected;
                        let target = marked || (is_cursor && tab_ref.marked_count == 0 && !e.is_parent);
                        if target {
                            painter.rect_filled(tile, CornerRadius::same(8), if pane_active { p.row_sel } else { p.row_sel_inactive });
                        } else if resp.hovered() && !dragging {
                            painter.rect_filled(tile, CornerRadius::same(8), p.row_hover);
                        }
                        if is_cursor && pane_active && (tab_ref.marked_count > 0 || e.is_parent) {
                            painter.rect_stroke(tile, CornerRadius::same(8), Stroke::new(1.0, p.accent), StrokeKind::Inside);
                        }

                        let (glyph, filled) = icons::glyph(icons::category(e));
                        let mut icon_color = icons::color(icons::category(e), p);
                        let mut text_color = p.text;
                        if e.is_dir && !e.is_parent && tab_ref.search.is_none() && tab_ref.empty_dirs.contains(&e.name) {
                            icon_color = theme::mix(p.surface, icon_color, 0.5);
                            text_color = theme::mix(p.surface, p.text, 0.6);
                        }
                        if e.hidden {
                            icon_color = theme::mix(p.surface, icon_color, 0.5);
                            text_color = p.text_faint;
                        }
                        // Suchtreffer liegen in verschiedenen Ordnern; sonst gilt der Ordner des Tabs.
                        let hit_dir = (mode == ColMode::Search).then(|| tab_ref.entry_dir(e));
                        let dir = match &hit_dir {
                            Some(loc) => loc.local_path(),
                            None => tab_ref.loc.local_path(),
                        };
                        let miniature = if e.is_dir { None } else { dir.and_then(|dir| thumbs.get(dir, &e.name, (e.modified, e.size), thumb_px)) };
                        match miniature {
                            Some((texture, size, loaded_px)) => {
                                // In die Fläche einpassen, aber kleine Bilder nicht aufblasen. Ist gerade eine größere
                                // Fassung unterwegs, wird die vorhandene solange gestreckt – außer das Bild selbst
                                // ist klein und füllte schon die bisherige Größe nicht aus.
                                let fills = size.max_elem() >= loaded_px as f32 * 0.9;
                                let stretch = if fills { (thumb_px as f32 / loaded_px.max(1) as f32).max(1.0) } else { 1.0 };
                                let fit = (thumb / size.x).min(thumb / size.y).min(stretch / scale);
                                let shown = Rect::from_center_size(picture.center(), size * fit);
                                let tint = if e.hidden { egui::Color32::from_white_alpha(130) } else { egui::Color32::WHITE };
                                painter.image(texture, shown, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), tint);
                            }
                            None => {
                                let glyph_size = (thumb * 0.52).round();
                                let font = if filled { theme::icon_fill(glyph_size) } else { theme::font(glyph_size) };
                                painter.text(picture.center(), Align2::CENTER_CENTER, glyph, font, icon_color);
                            }
                        }
                        // In Cloud-Ordnern: unten rechts am Bild, wo der Inhalt der Datei liegt.
                        if let Some((glyph, filled, on_pc)) = cloud_mark(e, tab_ref.in_cloud && mode == ColMode::Normal) {
                            let spot = Pos2::new(picture.right() - 9.0, picture.bottom() - 9.0);
                            painter.circle_filled(spot, 9.0, p.surface);
                            let font = if filled { theme::icon_fill(13.0) } else { theme::font(13.0) };
                            painter.text(spot, Align2::CENTER_CENTER, glyph, font, if on_pc { p.exe } else { p.text_dim });
                        }

                        let renaming = rename.as_mut().filter(|rn| rn.entry == ei);
                        if let Some(rn) = renaming {
                            let edit_r = Rect::from_min_max(
                                Pos2::new(tile.left() + 2.0, picture.bottom() + 3.0),
                                Pos2::new(tile.right() - 2.0, picture.bottom() + 25.0),
                            );
                            ui.painter().rect(edit_r, CornerRadius::same(5), p.field, Stroke::new(1.0, p.accent), StrokeKind::Inside);
                            if rn.fresh {
                                ui.memory_mut(|m| m.request_focus(rename_id));
                            }
                            let edit = egui::TextEdit::singleline(&mut rn.text)
                                .id(rename_id)
                                .frame(egui::Frame::NONE)
                                .font(name_font.clone())
                                .desired_width(edit_r.width() - 8.0);
                            let er = ui.put(edit_r.shrink2(Vec2::new(4.0, 1.0)), edit);
                            if rn.fresh {
                                rn.fresh = false;
                                let stem = if e.is_dir { rn.text.as_str() } else { split_ext(&rn.text).0 };
                                select_all(ui.ctx(), rename_id, stem.chars().count());
                            }
                            if er.lost_focus() {
                                if ui.input(|i| i.key_pressed(Key::Escape)) {
                                    events.push(RowEvent::RenameCancel);
                                } else {
                                    events.push(RowEvent::RenameCommit);
                                }
                            }
                        } else {
                            let color = if e.is_parent { p.text_faint } else { text_color };
                            draw_text(&painter, name_r.left() + 4.0, name_r.center().y, name_w, label, name_font.clone(), color, false);
                        }
                    }
                }
                return;
            }
            for line in &visible {
                if let Some(title) = line.title {
                    let r = Rect::from_min_size(Pos2::new(left, origin + line.top), Vec2::new(width, GROUP_H));
                    group_title(ui, r, title, line.rows.len(), p);
                    continue;
                }
                let row = line.rows.start;
                let ei = tab_ref.rows[row] as usize;
                let e = &tab_ref.entries[ei];
                let r = Rect::from_min_size(Pos2::new(left, origin + line.top), Vec2::new(width, ROW_H));
                let resp = ui.interact(r, Id::new((tab_id, "row", ei)), Sense::click_and_drag());
                let marked = tab_ref.marked[ei];
                let is_cursor = row == tab_ref.cursor && !tab_ref.unselected;

                if interactive && !popup_open {
                    if pressed && resp.contains_pointer() {
                        let label_x = cols.name.0 + 28.0;
                        let on_label = pointer.is_some_and(|pos| {
                            let w = text_width(ui, &e.name, theme::font(13.0)).min(cols.name.1 - label_x);
                            pos.x >= label_x - 4.0 && pos.x <= label_x + w + 6.0
                        });
                        events.push(RowEvent::Press(row, on_label));
                    }
                    if resp.double_clicked() {
                        events.push(RowEvent::Open(row));
                    } else if resp.clicked() {
                        events.push(RowEvent::Click(row));
                    }
                    if resp.middle_clicked() {
                        events.push(RowEvent::Middle(row));
                    }
                    if resp.drag_started_by(egui::PointerButton::Primary) {
                        let label_x = cols.name.0 + 28.0;
                        let origin = ui.input(|i| i.pointer.press_origin());
                        let on_item = !e.is_parent
                            && origin.is_some_and(|pos| {
                                let w = text_width(ui, &e.name, theme::font(13.0)).min(cols.name.1 - label_x);
                                pos.x >= cols.name.0 + 2.0 && pos.x <= label_x + w + 6.0
                            });
                        events.push(RowEvent::DragStart(row, on_item));
                    }
                }
                if resp.secondary_clicked() {
                    events.push(RowEvent::Secondary(row));
                }
                if !e.is_parent {
                    resp.context_menu(|ui| entry_menu(ui, p, &info, &mut menu_cmds));
                }
                if !ui.is_rect_visible(r) {
                    continue;
                }

                let painter = ui.painter_at(r);
                let target = marked || (is_cursor && tab_ref.marked_count == 0 && !e.is_parent);
                let pill = r.shrink2(Vec2::new(4.0, 0.5));
                if target {
                    painter.rect_filled(pill, CornerRadius::same(6), if pane_active { p.row_sel } else { p.row_sel_inactive });
                } else if resp.hovered() && !dragging {
                    painter.rect_filled(pill, CornerRadius::same(6), p.row_hover);
                }
                if is_cursor && pane_active && (tab_ref.marked_count > 0 || e.is_parent) {
                    painter.rect_stroke(pill, CornerRadius::same(6), Stroke::new(1.0, p.accent), StrokeKind::Inside);
                }

                let cy = r.center().y;
                let cat = if mode == ColMode::Drives {
                    match &e.target {
                        _ if e.is_parent => Cat::Parent,
                        _ if e.icon.is_some() => e.icon.unwrap_or(Cat::Folder),
                        Some(Location::Network { .. }) => Cat::Network,
                        // Freigaben sind Ordner, Cloud-Ordner liegen irgendwo unterhalb eines Laufwerks.
                        Some(Location::Local(path)) if vfs::unc_server(path).is_some() => Cat::Folder,
                        Some(Location::Local(path)) if path.parent().is_some() => Cat::Cloud,
                        _ => Cat::Drive,
                    }
                } else {
                    icons::category(e)
                };
                let (glyph, filled) = icons::glyph(cat);
                let mut icon_color = icons::color(cat, p);
                let mut text_color = p.text;
                let ghost = e.is_dir && !e.is_parent && tab_ref.search.is_none() && tab_ref.empty_dirs.contains(&e.name);
                if ghost {
                    // Leere Ordner: leicht zurückgenommen
                    icon_color = theme::mix(p.surface, icon_color, 0.5);
                    text_color = theme::mix(p.surface, p.text, 0.6);
                }
                if e.hidden {
                    icon_color = theme::mix(p.surface, icon_color, 0.5);
                    text_color = p.text_faint;
                }
                // Programme und Verknüpfungen zeigen ihr eigenes Symbol, sobald Windows es geliefert hat.
                let own_icon = if !e.is_dir && mode != ColMode::Drives && crate::shellicon::has_own_icon(e.ext()) {
                    let path = tab_ref.entry_dir(e).local_path().map(|dir| dir.join(&e.name));
                    path.and_then(|path| shell_icons.get(path, e.modified, icon_px))
                } else {
                    None
                };
                if let Some(texture) = own_icon {
                    let spot = Rect::from_center_size(Pos2::new(cols.name.0 + 14.0, cy), Vec2::splat(16.0));
                    let tint = if e.hidden { egui::Color32::from_white_alpha(130) } else { egui::Color32::WHITE };
                    painter.image(texture, spot, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), tint);
                } else {
                    painter.text(
                        Pos2::new(cols.name.0 + 14.0, cy),
                        Align2::CENTER_CENTER,
                        glyph,
                        if filled { theme::icon_fill(16.5) } else { theme::font(16.5) },
                        icon_color,
                    );
                }
                if e.link {
                    painter.text(
                        Pos2::new(cols.name.0 + 20.0, cy + 5.0),
                        Align2::CENTER_CENTER,
                        icons::ARROW_SQUARE_OUT,
                        theme::font(8.5),
                        p.text_dim,
                    );
                }
                let name_x = cols.name.0 + 28.0;
                let name_w = cols.name.1 - name_x;
                let renaming = rename.as_mut().filter(|rn| rn.entry == ei);
                if let Some(rn) = renaming {
                    let edit_r = Rect::from_min_max(Pos2::new(name_x - 5.0, r.top() + 1.0), Pos2::new(cols.name.1, r.bottom() - 1.0));
                    ui.painter().rect(edit_r, CornerRadius::same(5), p.field, Stroke::new(1.0, p.accent), StrokeKind::Inside);
                    if rn.fresh {
                        ui.memory_mut(|m| m.request_focus(rename_id));
                    }
                    let edit = egui::TextEdit::singleline(&mut rn.text)
                        .id(rename_id)
                        .frame(egui::Frame::NONE)
                        .font(theme::font(13.0))
                        .desired_width(edit_r.width() - 10.0);
                    let er = ui.put(edit_r.shrink2(Vec2::new(5.0, 1.0)), edit);
                    if rn.fresh {
                        rn.fresh = false;
                        let stem = if e.is_dir { rn.text.as_str() } else { split_ext(&rn.text).0 };
                        select_all(ui.ctx(), rename_id, stem.chars().count());
                    }
                    if er.lost_focus() {
                        if ui.input(|i| i.key_pressed(Key::Escape)) {
                            events.push(RowEvent::RenameCancel);
                        } else {
                            events.push(RowEvent::RenameCommit);
                        }
                    }
                } else {
                    let label = if e.is_parent { t("Übergeordneter Ordner", "Parent folder") } else { e.name.as_str() };
                    let color = if e.is_parent { p.text_faint } else { text_color };
                    // In Cloud-Ordnern steht am rechten Rand der Namensspalte, wo der Inhalt der Datei liegt.
                    let mark = cloud_mark(e, tab_ref.in_cloud && mode == ColMode::Normal);
                    let name_w = if mark.is_some() { name_w - 20.0 } else { name_w };
                    draw_text(&painter, name_x, cy, name_w, label, theme::font(13.0), color, false);
                    if let Some((glyph, filled, on_pc)) = mark {
                        let font = if filled { theme::icon_fill(13.0) } else { theme::font(13.0) };
                        let color = if on_pc { p.exe } else { p.text_faint };
                        painter.text(Pos2::new(cols.name.1 - 11.0, cy), Align2::CENTER_CENTER, glyph, font, color);
                    }
                }
                if e.is_parent {
                    continue;
                }

                let dim = if e.hidden || ghost { p.text_faint } else { p.text_dim };
                let small = theme::font(12.5);
                if mode == ColMode::Drives {
                    let note = e.note.as_deref().unwrap_or("");
                    let (label, space) = note.split_once('\u{1f}').unwrap_or((note, ""));
                    if let Some(c) = cols.kind {
                        draw_text(&painter, c.0 + 4.0, cy, c.1 - c.0 - 8.0, label, small.clone(), dim, false);
                    }
                    if let Some(c) = cols.date {
                        draw_text(&painter, c.0 + 4.0, cy, c.1 - c.0 - 8.0, space, small, dim, false);
                    }
                    continue;
                }
                if let (Some(c), Some(note)) = (cols.note, &e.note) {
                    let shown = match &**note {
                        "" => ".",
                        rel => rel,
                    };
                    draw_text(&painter, c.0 + 4.0, cy, c.1 - c.0 - 8.0, shown, small.clone(), p.text_faint, false);
                }
                if let Some(c) = cols.size {
                    let text = if e.is_dir {
                        match tab_ref.sizes.get(&e.name) {
                            Some(s) if s.done => fmt_size(s.size),
                            Some(s) => format!("{} …", fmt_size(s.size)),
                            None => String::new(),
                        }
                    } else {
                        fmt_size(e.size)
                    };
                    draw_text(&painter, c.0, cy, c.1 - c.0 - 4.0, &text, small.clone(), dim, true);
                }
                if let Some(c) = cols.kind {
                    let key = if e.is_dir { "\u{0}dir" } else { e.ext() };
                    if !types.contains_key(key) {
                        types.insert(key.to_owned(), win::type_name(e.ext(), e.is_dir));
                    }
                    draw_text(&painter, c.0 + 18.0, cy, c.1 - c.0 - 22.0, &types[key], small.clone(), dim, false);
                }
                if let (Some(c), Some(m)) = (cols.date, e.modified) {
                    draw_text(&painter, c.0 + 4.0, cy, c.1 - c.0 - 8.0, &fmt_time(m, today), small, dim, false);
                }
            }
        });
        tab.scroll_y = out.state.offset.y;
        tab.view_h = out.inner_rect.height();
        tab.rename = rename;

        // ---- Ereignisse der Zeilen
        let mut start_band = false;
        let mut open = false;
        let mut open_new_tab: Option<Location> = None;
        for ev in events {
            match ev {
                RowEvent::Press(row, on_label) => {
                    let ei = tab.rows[row] as usize;
                    // „Langsamer Doppelklick" wie im Explorer: ein Klick auf den Namen eines Eintrags,
                    // der schon gewählt war, führt ins Umbenennen – sofern kein Doppelklick daraus wird.
                    tab.rename_due = None;
                    let candidate = on_label
                        && pane_active
                        && !mods.shift
                        && !mods.command
                        && tab.cursor == row
                        && !tab.unselected
                        && tab.marked_count == 0
                        && tab.rename.is_none()
                        && tab.search.is_none()
                        && tab.loc.writable()
                        && !tab.entries[ei].is_parent;
                    tab.rename_armed = candidate.then_some(row);
                    // War nach einem Klick ins Leere nichts gewählt, gehört die alte Cursorzeile nicht zur Auswahl.
                    let was_unselected = std::mem::take(&mut tab.unselected);
                    if mods.shift {
                        tab.cursor = row;
                        tab.mark_range(tab.anchor, row);
                    } else if mods.command {
                        if tab.marked_count == 0 && tab.cursor != row && !was_unselected {
                            // Wie im Explorer: der bisher gewählte Eintrag bleibt Teil der Auswahl.
                            tab.toggle_row(tab.cursor);
                        }
                        tab.toggle_row(row);
                        tab.cursor = row;
                        tab.anchor = row;
                    } else if !tab.marked[ei] {
                        tab.clear_marks();
                        tab.cursor = row;
                        tab.anchor = row;
                    }
                }
                RowEvent::Click(row) => {
                    // Klick auf einen markierten Eintrag ohne Ziehen: Markierung auflösen.
                    if !mods.shift && !mods.command {
                        tab.clear_marks();
                        tab.cursor = row;
                        tab.anchor = row;
                        if tab.rename_armed.take() == Some(row) {
                            if let Some(e) = tab.entry_at(row) {
                                // Erst nach Ablauf der Doppelklick-Zeit, sonst würde Öffnen zum Umbenennen.
                                tab.rename_due = Some((e.name.clone(), now + double_click + 0.06));
                            }
                        }
                    }
                }
                RowEvent::Open(row) => {
                    tab.rename_armed = None;
                    tab.rename_due = None;
                    tab.cursor = row;
                    tab.unselected = false;
                    open = true;
                }
                RowEvent::Secondary(row) => {
                    tab.rename_armed = None;
                    tab.rename_due = None;
                    let ei = tab.rows[row] as usize;
                    if !tab.marked[ei] {
                        tab.clear_marks();
                        tab.cursor = row;
                        tab.unselected = false;
                        tab.anchor = row;
                    }
                }
                RowEvent::Middle(row) => {
                    if let Some(e) = tab.entry_at(row).filter(|e| e.is_dir && !e.is_parent) {
                        open_new_tab = Some(tab.entry_location(e));
                    }
                }
                RowEvent::DragStart(_, false) => {
                    // Neben dem Namen begonnen: Auswahlrechteck aufziehen statt Dateien zu ziehen.
                    tab.rename_armed = None;
                    tab.rename_due = None;
                    start_band = true;
                }
                RowEvent::DragStart(row, true) => {
                    tab.rename_armed = None;
                    tab.rename_due = None;
                    let ei = tab.rows[row] as usize;
                    if !tab.entries[ei].is_parent && !tab.loc.is_virtual() {
                        if !tab.marked[ei] {
                            tab.clear_marks();
                            tab.cursor = row;
                            tab.unselected = false;
                        }
                        let sources = tab.sources();
                        if !sources.is_empty() {
                            DragAndDrop::set_payload(ui.ctx(), DragPayload { sources, from_tab: tab_id });
                        }
                    }
                }
                RowEvent::RenameCommit => {
                    if let Some(rn) = tab.rename.take() {
                        let old = tab.entries[rn.entry].name.clone();
                        let new = rn.text.trim().to_owned();
                        if !new.is_empty() && new != old {
                            cx.actions.push(Action::Rename { pane, old, new });
                        }
                    }
                }
                RowEvent::RenameCancel => tab.rename = None,
            }
        }

        // ---- Auswahlrechteck: beginnt neben einem Namen oder im leeren Bereich
        if (start_band || bg.drag_started_by(egui::PointerButton::Primary)) && cx.interactive && !tab.loc.is_virtual() {
            if let Some(origin) = ui.input(|i| i.pointer.press_origin()) {
                let keep = mods.shift || mods.command;
                let base = if keep { tab.marked.clone() } else { vec![false; tab.marked.len()] };
                if !keep {
                    // Bis das Rechteck etwas erfasst, ist nichts gewählt – auch nicht die Zeile unter dem Cursor.
                    tab.clear_marks();
                    tab.unselected = true;
                }
                tab.rename_due = None;
                tab.band = Some(Band {
                    anchor: Pos2::new(origin.x, origin.y - out.inner_rect.top() + tab.scroll_y),
                    base,
                    toggle: mods.command,
                    last: None,
                });
            }
        }
        if let Some(mut band) = tab.band.take() {
            let down = ui.input(|i| i.pointer.primary_down());
            let view = out.inner_rect;
            if down && cx.interactive && band.base.len() == tab.marked.len() {
                if let Some(pos) = pointer {
                    // Am oberen/unteren Rand mitscrollen – je weiter draußen, desto schneller.
                    let dt = ui.input(|i| i.stable_dt).min(0.05);
                    let max_scroll = (content_h - view.height()).max(0.0);
                    let mut scroll = tab.scroll_y;
                    if pos.y < view.top() + 6.0 {
                        scroll -= ((view.top() + 6.0 - pos.y) * 14.0 + 200.0) * dt;
                    } else if pos.y > view.bottom() - 6.0 {
                        scroll += ((pos.y - view.bottom() + 6.0) * 14.0 + 200.0) * dt;
                    }
                    let scroll = scroll.clamp(0.0, max_scroll);
                    if (scroll - tab.scroll_y).abs() > 0.01 {
                        tab.scroll_y = scroll;
                        tab.scroll_to = Some(scroll);
                    }

                    let current = pos.y.clamp(view.top(), view.bottom()) - view.top() + tab.scroll_y;
                    let (y0, y1) = (band.anchor.y.min(current), band.anchor.y.max(current));
                    let n = tab.rows.len();
                    // Erfasster Bereich: [oben, unten – in ganzen Bildpunkten des Inhalts –, erste Spalte, letzte Spalte]
                    let range = if n == 0 || y1 < 0.0 || y0 >= content_h {
                        None
                    } else {
                        let col = |x: f32| (((x - rect.left() - 4.0) / tile_w).floor().max(0.0) as usize).min(per_line - 1);
                        let (x0, x1) = (band.anchor.x.min(pos.x), band.anchor.x.max(pos.x));
                        let (top, bottom) = (y0.max(0.0) as usize, y1.max(0.0) as usize);
                        Some(if per_line > 1 { [top, bottom, col(x0), col(x1)] } else { [top, bottom, 0, 0] })
                    };
                    if range != band.last || band.last.is_none() {
                        band.last = range;
                        for row in 0..n {
                            let ei = tab.rows[row] as usize;
                            // Erfasst ist, was das Rechteck in der Höhe berührt und in seinen Spalten liegt.
                            let (top, col) = (tab.row_top(row), tab.row_col(row));
                            let hit = range.is_some_and(|[_, _, c0, c1]| top + line_h > y0 && top <= y1 && col >= c0 && col <= c1)
                                && !tab.entries[ei].is_parent;
                            let base = band.base[ei];
                            tab.marked[ei] = if band.toggle { base ^ hit } else { base || hit };
                        }
                        tab.marked_count = tab.marked.iter().filter(|m| **m).count();
                    }
                    if n > 0 {
                        tab.cursor = tab.row_at(pos.x - rect.left(), current).unwrap_or(0).min(n - 1);
                    }

                    let a = Pos2::new(band.anchor.x.clamp(rect.left() + 1.0, rect.right() - 1.0), view.top() + band.anchor.y - tab.scroll_y);
                    let b = Pos2::new(pos.x.clamp(rect.left() + 1.0, rect.right() - 1.0), pos.y.clamp(view.top(), view.bottom()));
                    let fill = egui::Color32::from_rgba_unmultiplied(p.accent.r(), p.accent.g(), p.accent.b(), 38);
                    ui.painter_at(rect).rect(Rect::from_two_pos(a, b), CornerRadius::same(2), fill, Stroke::new(1.0, p.accent), StrokeKind::Inside);
                }
                tab.band = Some(band);
                ui.ctx().request_repaint();
            }
        }

        // ---- Verzögertes Umbenennen nach Klick auf den gewählten Namen
        if let Some((name, due)) = tab.rename_due.clone() {
            let still_valid = pane_active
                && cx.interactive
                && !popup_open
                && !dragging
                && tab.rename.is_none()
                && tab.marked_count == 0
                && tab.current().is_some_and(|e| e.name == name);
            if !still_valid {
                tab.rename_due = None;
            } else if now >= due {
                tab.rename_due = None;
                tab.start_rename();
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(due - now));
            }
        }

        // ---- Hintergrund: Klick ins Leere, Kontextmenü
        if bg.clicked() && cx.interactive {
            // Wie im Explorer: danach ist nichts mehr gewählt, auch nicht der Eintrag unter dem Cursor.
            tab.clear_marks();
            tab.unselected = true;
            tab.rename_armed = None;
            tab.rename_due = None;
        }
        bg.context_menu(|ui| background_menu(ui, p, &info, cx.env.cfg.show_hidden, &mut menu_cmds));

        // ---- Zustände ohne Zeilen
        let painter = ui.painter_at(rect);
        let center = rect.center();
        if let Some(err) = &tab.error {
            painter.text(center - Vec2::new(0.0, 26.0), Align2::CENTER_CENTER, icons::WARNING_CIRCLE, theme::font(30.0), p.warn);
            let w = (rect.width() - 40.0).min(520.0);
            let galley = painter.layout(err.clone(), theme::font(13.0), p.text_dim, w);
            painter.galley(Pos2::new(center.x - galley.size().x / 2.0, center.y), galley, p.text_dim);
        } else if tab.loading && tab.entries.len() <= 1 {
            if tab.load_started.elapsed().as_millis() > 150 {
                let text = match &tab.loc {
                    Location::Network { server: None } => t(
                        "Suche Computer im Netzwerk … das kann einige Sekunden dauern",
                        "Looking for computers on the network … this can take a few seconds",
                    ),
                    Location::Network { server: Some(_) } => t("Frage Freigaben ab …", "Querying shares …"),
                    Location::RecycleBin => t("Lese den Papierkorb …", "Reading the Recycle Bin …"),
                    Location::Device { .. } => t("Frage das Gerät ab …", "Querying the device …"),
                    _ => t("Lade …", "Loading …"),
                };
                painter.text(center, Align2::CENTER_CENTER, text, theme::font(13.0), p.text_faint);
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(120));
        } else if tab.rows.iter().all(|&i| tab.entries[i as usize].is_parent) {
            let text = if tab.search.as_ref().is_some_and(|s| s.running) {
                t("Suche läuft …", "Searching …")
            } else if tab.search.is_some() {
                t("Keine Treffer", "No matches")
            } else if !tab.filter.is_empty() {
                t("Kein Eintrag passt zum Filter", "No item matches the filter")
            } else {
                match &tab.loc {
                    Location::Network { server: None } => t(
                        "Keine Computer gefunden – eine Adresse wie \\\\NAS01 lässt sich mit Strg+L direkt eingeben",
                        "No computers found – an address like \\\\NAS01 can be entered directly with Ctrl+L",
                    ),
                    Location::Network { server: Some(_) } => {
                        t("Dieser Computer zeigt keine Freigaben", "This computer shows no shares")
                    }
                    Location::RecycleBin => t("Der Papierkorb ist leer", "The Recycle Bin is empty"),
                    Location::Device { path, .. } if path.trim_matches('/').is_empty() => t(
                        "Das Gerät zeigt nichts – bitte entsperren und die Dateiübertragung erlauben",
                        "The device shows nothing – please unlock it and allow file transfer",
                    ),
                    _ => t("Dieser Ordner ist leer", "This folder is empty"),
                }
            };
            painter.text(center, Align2::CENTER_CENTER, text, theme::font(13.0), p.text_faint);
        }

        // ---- Ablegen per Drag & Drop
        if dragging {
            if let Some(pos) = pointer.filter(|pos| rect.contains(*pos)) {
                let payload = DragAndDrop::payload::<DragPayload>(ui.ctx());
                let over_row = tab.row_at(pos.x - rect.left(), pos.y - out.inner_rect.top() + tab.scroll_y);
                let over = over_row.and_then(|row| tab.entry_at(row));
                let dragged_names: HashSet<&str> = payload
                    .as_ref()
                    .filter(|pl| pl.from_tab == tab_id)
                    .map(|pl| pl.sources.iter().map(|s| s.name.as_str()).collect())
                    .unwrap_or_default();
                let folder = over.filter(|e| e.is_dir && !e.is_parent && !dragged_names.contains(e.name.as_str()));
                let dest = match folder {
                    Some(e) => Some(tab.entry_dir(e).child(&e.name)),
                    None if payload.as_ref().is_some_and(|pl| pl.from_tab == tab_id) => None,
                    None => Some(tab.loc.clone()),
                };
                if let Some(dest) = dest.filter(|d| d.writable()) {
                    if folder.is_some() {
                        let row = over_row.unwrap_or(0);
                        let y = out.inner_rect.top() + tab.row_top(row) - tab.scroll_y;
                        let r = if per_line > 1 {
                            let x = rect.left() + 4.0 + tab.row_col(row) as f32 * tile_w;
                            Rect::from_min_size(Pos2::new(x, y), Vec2::new(tile_w, line_h)).shrink2(Vec2::new(3.0, 2.0))
                        } else {
                            Rect::from_min_size(Pos2::new(rect.left() + 4.0, y), Vec2::new(rect.width() - 8.0, ROW_H))
                        };
                        painter.rect(r, CornerRadius::same(6), theme::mix(p.surface, p.accent, 0.18), Stroke::new(1.5, p.accent), StrokeKind::Inside);
                    } else {
                        painter.rect_stroke(rect.shrink(2.0), CornerRadius::same(8), Stroke::new(2.0, p.accent), StrokeKind::Inside);
                    }
                    if released {
                        if let Some(payload) = DragAndDrop::take_payload::<DragPayload>(ui.ctx()) {
                            cx.actions.push(Action::Drop { pane, dest, payload, mods });
                        }
                    }
                }
            }
        }

        if open {
            cx.actions.push(Action::Cmd(pane, Cmd::Open));
        }
        if let Some(loc) = open_new_tab {
            self.add_tab(loc, &mut cx.env);
        }
        for c in menu_cmds {
            cx.actions.push(Action::Cmd(pane, c));
        }
    }

    fn status_ui(&mut self, ui: &mut Ui, rect: Rect, cx: &mut PaneCx) {
        let p = cx.p;
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius { nw: 0, ne: 0, sw: 9, se: 9 }, p.surface_alt);
        painter.hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, p.border_soft));
        let tab = self.tab();
        let cy = rect.center().y;
        let count = tab.entries.iter().filter(|e| !e.is_parent).count();
        let plural = |n: usize| {
            if n == 1 {
                t("1 Element", "1 item").to_owned()
            } else {
                tf!("{} Elemente", "{} items", crate::util::fmt_thousands(n as u64))
            }
        };

        // Geht es um genau einen Eintrag, steht er ausführlich da: der ganze Name (in der Rasteransicht ist er oft
        // gekürzt), die Größe und die Änderungszeit.
        let today = cx.today;
        let details = |e: &Entry| {
            let mut parts = vec![e.name.clone()];
            if e.is_dir {
                match tab.sizes.get(&e.name) {
                    Some(s) if s.done => parts.push(fmt_size(s.size)),
                    Some(s) => parts.push(format!("{} …", fmt_size(s.size))),
                    None => {}
                }
            } else {
                let bytes = crate::util::fmt_thousands(e.size);
                parts.push(if e.size < 1024 {
                    tf!("{bytes} Bytes", "{bytes} bytes")
                } else {
                    tf!("{} ({bytes} Bytes)", "{} ({bytes} bytes)", fmt_size(e.size))
                });
            }
            if let Some(modified) = e.modified {
                parts.push(fmt_time(modified, today));
            }
            parts.join("  ·  ")
        };

        let (left, accent) = if let Some(s) = &tab.search {
            let state = if s.running { t("Suche läuft", "Searching") } else { t("Suche beendet", "Search finished") };
            let hits = if count == 1 { t("Treffer", "match") } else { t("Treffer", "matches") };
            let checked = match s.checked {
                0 => String::new(),
                n => tf!("  ·  {} Dateien geprüft", "  ·  {} files checked", crate::util::fmt_thousands(n)),
            };
            (
                tf!(
                    "{state} – {} {hits} für {}{checked}",
                    "{state} – {} {hits} for {}{checked}",
                    crate::util::fmt_thousands(count as u64),
                    s.what()
                ),
                true,
            )
        } else if tab.marked_count > 0 {
            let mut marked = tab.entries.iter().zip(&tab.marked).filter(|(_, m)| **m).map(|(e, _)| e);
            let summary = tf!("{} von {} markiert", "{} of {} selected", tab.marked_count, count);
            if tab.marked_count == 1 {
                (marked.next().map_or(summary.clone(), |e| format!("{}  ·  {summary}", details(e))), true)
            } else {
                let bytes: u64 = marked.map(|e| if e.is_dir { tab.sizes.get(&e.name).map_or(0, |s| s.size) } else { e.size }).sum();
                (format!("{summary}  ·  {}", fmt_size(bytes)), true)
            }
        } else if !tab.filter.is_empty() {
            let shown = tab.rows.iter().filter(|&&i| !tab.entries[i as usize].is_parent).count();
            (tf!("{shown} von {} (gefiltert)", "{shown} of {} (filtered)", plural(count)), true)
        } else if let Some(e) = tab.current().filter(|e| !e.is_parent) {
            // Der Eintrag zuerst: wird es eng, fällt hinten die Anzahl weg und nicht der Name.
            (format!("{}  ·  {}", details(e), plural(count)), false)
        } else {
            (plural(count), false)
        };
        let right = match (&tab.loc, tab.space) {
            // Für den Ordner eines Telefons meldet Windows den Platz auf dem Laufwerk des PCs – das führte in die Irre.
            (Location::Local(path), _) if cx.places.iter().any(|place| place.phone && path.starts_with(&place.root)) => {
                t("Smartphone", "Phone").to_owned()
            }
            (Location::Local(_), Some((free, total))) if total > 0 => {
                tf!("{} frei von {}", "{} free of {}", fmt_size(free), fmt_size(total))
            }
            (Location::Zip { archive, .. }, _) => {
                let format = crate::archive::label(archive);
                tf!("{format}-Archiv · schreibgeschützt", "{format} archive · read-only")
            }
            (Location::Network { server: None }, _) if tab.loading => {
                t("Netzwerk wird noch durchsucht …", "Still scanning the network …").to_owned()
            }
            (Location::RecycleBin, _) => t("Rechtsklick: wiederherstellen", "Right-click: restore").to_owned(),
            (Location::Device { .. }, _) => t("Gerät am Kabel · nur lesen", "Device on a cable · read-only").to_owned(),
            _ => String::new(),
        };
        let font = theme::font(11.5);
        let right_w = if rect.width() > 380.0 { text_width(ui, &right, font.clone()) } else { 0.0 };
        // In der Rasteransicht: Regler für die Größe der Miniaturen, links neben der Angabe zum Speicherplatz.
        let grid = tab.grid && !tab.loc.is_virtual();
        let slider_w = if grid && rect.width() > 430.0 { 92.0 } else { 0.0 };
        let reserved = if slider_w > 0.0 { slider_w + 44.0 } else { 0.0 };
        if slider_w > 0.0 {
            let right_edge = rect.right() - 12.0 - right_w - if right_w > 0.0 { 18.0 } else { 0.0 };
            let track = Rect::from_min_max(Pos2::new(right_edge - slider_w, cy - 2.0), Pos2::new(right_edge, cy + 2.0));
            let grab = ui.interact(track.expand2(Vec2::new(7.0, 9.0)), Id::new(("thumb-size", cx.index)), Sense::click_and_drag());
            let size = &mut cx.env.cfg.thumb_size;
            if grab.dragged() || grab.clicked() {
                if let Some(pos) = grab.interact_pointer_pos() {
                    let part = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
                    *size = (THUMB_MIN + part * (THUMB_MAX - THUMB_MIN)).round();
                }
            }
            if grab.double_clicked() {
                *size = THUMB_DEFAULT;
            }
            let active = grab.hovered() || grab.dragged();
            if active {
                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
            }
            let part = ((size.clamp(THUMB_MIN, THUMB_MAX) - THUMB_MIN) / (THUMB_MAX - THUMB_MIN)).clamp(0.0, 1.0);
            let fill = if active { p.accent } else { p.text_faint };
            progress_bar(&painter, track, part, p.control_active, fill);
            painter.circle(Pos2::new(track.left() + track.width() * part, cy), if active { 5.5 } else { 4.5 }, fill, Stroke::new(2.0, p.surface_alt));
            painter.text(Pos2::new(track.left() - 13.0, cy), Align2::CENTER_CENTER, icons::SQUARES_FOUR, theme::font(12.0), p.text_faint);
            grab.on_hover_text(tf!(
                "Größe der Miniaturen – auch mit {} über den Bildern. Doppelklick: Normalgröße",
                "Thumbnail size – also with {} over the pictures. Double-click: default size",
                key("Strg+Mausrad")
            ));
        }
        draw_text(
            &painter,
            rect.left() + 12.0,
            cy,
            rect.width() - right_w - 36.0 - reserved,
            &left,
            font.clone(),
            if accent { p.accent } else { p.text_dim },
            false,
        );
        if right_w > 0.0 {
            painter.text(Pos2::new(rect.right() - 12.0, cy), Align2::RIGHT_CENTER, right, font, p.text_faint);
        }
    }
}

pub fn select_all(ctx: &egui::Context, id: Id, chars: usize) {
    use egui::text::{CCursor, CCursorRange};
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(chars))));
    egui::TextEdit::store_state(ctx, id, state);
}

// ---------------------------------------------------------------- Kontextmenüs

struct MenuInfo {
    count: usize,
    single_dir: bool,
    single_file: bool,
    any_dir: bool,
    is_zip: bool,
    local: bool,
    writable: bool,
    can_paste: bool,
    /// Die Einträge sind Dateien auf diesem PC
    on_disk: bool,
    /// Der Papierkorb: hier gibt es nur Wiederherstellen und endgültiges Löschen
    bin: bool,
    /// Ein Cloud-Ordner: Dateien lassen sich auf dem PC behalten oder freigeben
    cloud: bool,
    /// Einträge lassen sich löschen und wegschieben
    removable: bool,
    drives: bool,
}

impl MenuInfo {
    fn of(tab: &Tab, can_paste: bool) -> Self {
        let sel = tab.selection();
        let first = sel.first().map(|&i| &tab.entries[i]);
        Self {
            count: sel.len(),
            single_dir: sel.len() == 1 && first.is_some_and(|e| e.is_dir),
            single_file: sel.len() == 1 && first.is_some_and(|e| !e.is_dir),
            any_dir: sel.iter().any(|&i| tab.entries[i].is_dir),
            is_zip: sel.len() == 1 && first.is_some_and(|e| !e.is_dir && vfs::is_zip_name(&e.name)),
            local: tab.loc.is_local(),
            on_disk: tab.loc.is_local(),
            bin: tab.loc == Location::RecycleBin,
            cloud: tab.in_cloud && tab.search.is_none(),
            writable: tab.loc.writable(),
            removable: tab.loc.removable(),
            can_paste,
            drives: tab.loc.is_virtual(),
        }
    }
}

/// Die Überschrift über einer Gruppe: der Monat, daneben die Zahl seiner Einträge und eine feine Linie.
fn group_title(ui: &Ui, rect: Rect, title: &str, count: usize, p: &Palette) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    let y = rect.bottom() - 13.0;
    let font = theme::semibold(13.5);
    let width = text_width(ui, title, font.clone());
    draw_text(&painter, rect.left() + 12.0, y, width, title, font, p.text, false);
    let note = count.to_string();
    let note_w = text_width(ui, &note, theme::font(11.5));
    draw_text(&painter, rect.left() + 20.0 + width, y + 1.0, note_w, &note, theme::font(11.5), p.text_faint, false);
    let from = rect.left() + 30.0 + width + note_w;
    if from < rect.right() - 12.0 {
        painter.hline(from..=rect.right() - 12.0, y + 1.0, Stroke::new(1.0, p.border_soft));
    }
}

/// Das Zeichen für den Cloud-Zustand eines Eintrags: (Symbol, gefüllt, liegt auf dem PC). `in_cloud`: die Liste
/// zeigt einen Cloud-Ordner – nur dort heißt „keine Angabe" bei einer Datei, dass ihr Inhalt auf dem PC liegt.
fn cloud_mark(e: &Entry, in_cloud: bool) -> Option<(&'static str, bool, bool)> {
    if e.is_parent {
        return None;
    }
    match e.cloud {
        vfs::Cloud::Online => Some((icons::CLOUD, false, false)),
        vfs::Cloud::Pinned => Some((egui_phosphor::fill::CHECK_CIRCLE, true, true)),
        vfs::Cloud::Unknown if in_cloud && !e.is_dir => Some((icons::CHECK_CIRCLE, false, true)),
        vfs::Cloud::Unknown => None,
    }
}

fn entry_menu(ui: &mut Ui, p: &Palette, m: &MenuInfo, out: &mut Vec<Cmd>) {
    menu_begin(ui, 262.0);
    let mut item = |ui: &mut Ui, icon: &str, label: &str, key: &str, enabled: bool, cmd: Cmd| {
        if menu_item(ui, p, icon, label, key, enabled) {
            out.push(cmd);
        }
    };
    if m.bin {
        item(ui, icons::ARROW_U_UP_LEFT, t("Wiederherstellen", "Restore"), "", m.count > 0, Cmd::BinRestore);
        item(ui, icons::ARROW_SQUARE_OUT, t("Öffnen", "Open"), "Enter", m.count == 1, Cmd::Open);
        menu_separator(ui, p);
        item(ui, icons::TRASH, t("Endgültig löschen …", "Delete permanently …"), &key("Entf"), m.count > 0, Cmd::Delete);
        item(ui, icons::BROOM, t("Papierkorb leeren …", "Empty Recycle Bin …"), "", true, Cmd::BinEmpty);
        return;
    }
    item(ui, icons::ARROW_SQUARE_OUT, t("Öffnen", "Open"), "Enter", m.count == 1, Cmd::Open);
    if m.single_dir {
        item(ui, icons::TABS, t("In neuem Tab öffnen", "Open in new tab"), "", true, Cmd::OpenInNewTab);
        item(ui, icons::SQUARE_SPLIT_VERTICAL, t("Im anderen Fenster öffnen", "Open in other pane"), "", true, Cmd::OpenInOther);
    }
    if m.single_file && m.on_disk {
        item(ui, icons::APP_WINDOW, t("Öffnen mit …", "Open with …"), "", true, Cmd::OpenWith);
    }
    if m.drives {
        return;
    }
    menu_separator(ui, p);
    item(ui, icons::COPY, t("Ins andere Fenster kopieren", "Copy to other pane"), "F5", m.count > 0, Cmd::Copy);
    item(
        ui,
        icons::ARROW_RIGHT,
        t("Ins andere Fenster verschieben", "Move to other pane"),
        "F6",
        m.count > 0 && m.removable,
        Cmd::Move,
    );
    menu_separator(ui, p);
    item(ui, icons::COPY_SIMPLE, t("Kopieren", "Copy"), &key("Strg+C"), m.count > 0, Cmd::ClipCopy);
    item(ui, icons::SCISSORS, t("Ausschneiden", "Cut"), &key("Strg+X"), m.count > 0 && m.removable, Cmd::ClipCut);
    item(ui, icons::CLIPBOARD, t("Einfügen", "Paste"), &key("Strg+V"), m.can_paste && m.writable, Cmd::ClipPaste);
    menu_separator(ui, p);
    item(ui, icons::PENCIL_SIMPLE, t("Umbenennen", "Rename"), "F2", m.count == 1 && m.writable, Cmd::Rename);
    item(ui, icons::TRASH, t("Löschen", "Delete"), &key("Entf"), m.count > 0 && m.removable, Cmd::Delete);
    menu_separator(ui, p);
    if m.is_zip {
        item(ui, icons::FOLDER_OPEN, t("Entpacken in eigenen Ordner", "Extract to its own folder"), "", m.local, Cmd::Extract);
    }
    item(ui, icons::FILE_ZIP, t("Als ZIP packen …", "Pack as ZIP …"), "", m.count > 0 && m.local, Cmd::Pack);
    if m.any_dir {
        item(
            ui,
            icons::CALCULATOR,
            t("Ordnergröße berechnen", "Calculate folder size"),
            &key("Leertaste"),
            m.local,
            Cmd::CalcSizes,
        );
    }
    item(ui, icons::LINK_SIMPLE, t("Pfad kopieren", "Copy path"), "", m.count > 0, Cmd::CopyPath);
    if m.cloud && m.count > 0 {
        menu_separator(ui, p);
        item(ui, icons::CLOUD_ARROW_DOWN, t("Immer auf diesem PC behalten", "Always keep on this PC"), "", true, Cmd::CloudKeep);
        item(ui, icons::CLOUD, t("Speicher freigeben", "Free up space"), "", true, Cmd::CloudFree);
        menu_separator(ui, p);
    }
    if m.single_dir {
        item(ui, icons::STAR, t("Zu Favoriten hinzufügen", "Add to favorites"), "", true, Cmd::FavoriteSelected);
    }
    if m.on_disk {
        menu_separator(ui, p);
        item(ui, icons::FOLDER_NOTCH, t("Im Explorer anzeigen", "Show in Explorer"), "", m.count == 1, Cmd::Reveal);
        item(ui, icons::INFO, t("Eigenschaften", "Properties"), "Alt+Enter", m.count == 1, Cmd::Properties);
    }
}

fn background_menu(ui: &mut Ui, p: &Palette, m: &MenuInfo, show_hidden: bool, out: &mut Vec<Cmd>) {
    menu_begin(ui, 262.0);
    let mut item = |ui: &mut Ui, icon: &str, label: &str, key: &str, enabled: bool, cmd: Cmd| {
        if menu_item(ui, p, icon, label, key, enabled) {
            out.push(cmd);
        }
    };
    if m.bin {
        item(ui, icons::BROOM, t("Papierkorb leeren …", "Empty Recycle Bin …"), "", true, Cmd::BinEmpty);
        item(ui, icons::ARROWS_CLOCKWISE, t("Aktualisieren", "Refresh"), &key("Strg+R"), true, Cmd::Refresh);
        item(ui, icons::SELECTION_ALL, t("Alles markieren", "Select all"), &key("Strg+A"), true, Cmd::SelectAll);
        return;
    }
    item(ui, icons::CLIPBOARD, t("Einfügen", "Paste"), &key("Strg+V"), m.can_paste && m.writable, Cmd::ClipPaste);
    menu_separator(ui, p);
    item(ui, icons::FOLDER_PLUS, t("Neuer Ordner", "New folder"), "F7", m.writable, Cmd::NewFolder);
    item(ui, icons::FILE_PLUS, t("Neue Datei", "New file"), &key("Umschalt+F4"), m.local, Cmd::NewFile);
    menu_separator(ui, p);
    item(ui, icons::ARROWS_CLOCKWISE, t("Aktualisieren", "Refresh"), &key("Strg+R"), true, Cmd::Refresh);
    item(ui, icons::SELECTION_ALL, t("Alles markieren", "Select all"), &key("Strg+A"), !m.drives, Cmd::SelectAll);
    item(ui, icons::CALCULATOR, t("Alle Ordnergrößen berechnen", "Calculate all folder sizes"), "", m.local, Cmd::CalcAllSizes);
    item(
        ui,
        if show_hidden { icons::EYE_SLASH } else { icons::EYE },
        if show_hidden {
            t("Versteckte Dateien ausblenden", "Hide hidden files")
        } else {
            t("Versteckte Dateien anzeigen", "Show hidden files")
        },
        &key("Strg+H"),
        true,
        Cmd::ToggleHidden,
    );
    menu_separator(ui, p);
    item(
        ui,
        icons::STAR,
        t("Diesen Ordner zu Favoriten", "Add this folder to favorites"),
        &key("Strg+D"),
        !m.drives,
        Cmd::AddFavorite,
    );
    item(ui, icons::TABS, t("Neuer Tab", "New tab"), &key("Strg+T"), true, Cmd::NewTab);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recycle_bin_lists_the_latest_first_and_gives_the_old_view_back() {
        let mut tab = Tab::new(Location::Local(PathBuf::from("C:\\")), SortKey::Name, false);
        tab.grid = true;
        tab.set_loc(Location::RecycleBin);
        assert_eq!((tab.sort, tab.desc, tab.grid), (SortKey::Date, true, false));
        tab.set_loc(Location::Drives);
        assert_eq!((tab.sort, tab.desc, tab.grid), (SortKey::Name, false, true));
        // Ein Tab, der als Papierkorb beginnt, kennt kein „vorher".
        let mut tab = Tab::new(Location::RecycleBin, SortKey::Size, false);
        assert_eq!((tab.sort, tab.desc, tab.grid), (SortKey::Date, true, false));
        tab.set_loc(Location::Drives);
        assert_eq!((tab.sort, tab.desc, tab.grid), (SortKey::Name, false, false));
    }

    #[test]
    fn files_in_a_cloud_folder_show_where_their_content_is() {
        let mut file = Entry::new("a.txt".to_owned(), false, 1, None);
        assert_eq!(cloud_mark(&file, false), None);
        assert_eq!(cloud_mark(&file, true), Some((icons::CHECK_CIRCLE, false, true)));
        file.cloud = vfs::Cloud::Online;
        assert_eq!(cloud_mark(&file, false), Some((icons::CLOUD, false, false)));
        // Ordner ohne Angabe bleiben ohne Zeichen: was in ihnen liegt, ist nicht bekannt.
        assert_eq!(cloud_mark(&Entry::new("Ordner".to_owned(), true, 0, None), true), None);
        let mut tab = Tab::new(Location::Local(PathBuf::from("C:\\Users\\x\\OneDrive\\Bilder")), SortKey::Name, false);
        tab.note_cloud(&["c:\\users\\x\\onedrive\\".to_owned()]);
        assert!(tab.in_cloud);
        tab.set_loc(Location::Local(PathBuf::from("C:\\Users\\x\\OneDriveAlt")));
        tab.note_cloud(&["c:\\users\\x\\onedrive\\".to_owned()]);
        assert!(!tab.in_cloud);
    }

    #[test]
    fn only_the_lines_in_view_are_drawn() {
        // Ohne Gruppen: gleich hohe Zeilen, eine mehr als zu sehen.
        let mut plain = Tab::new(Location::Local(PathBuf::from("C:\\")), SortKey::Name, false);
        plain.entries = (0..10).map(|i| Entry::new(format!("{i}.txt"), false, 1, None)).collect();
        plain.marked = vec![false; 10];
        plain.rebuild_rows();
        assert!(plain.groups.is_empty());
        let rows: Vec<usize> = plain.visible_lines(ROW_H * 2.0, ROW_H * 4.0).iter().map(|line| line.rows.start).collect();
        assert_eq!(rows, [2, 3, 4]);
        assert_eq!(plain.content_height(), ROW_H * 10.0);
        assert_eq!((plain.row_below(1, 2), plain.row_above(1, 2), plain.row_near(ROW_H * 3.5)), (3, 0, 3));
    }
}
