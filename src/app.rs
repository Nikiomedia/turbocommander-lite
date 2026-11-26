//! Das Hauptfenster: Leisten, zwei Dateifenster, Tastenkürzel und alle Befehle.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, CornerRadius, CursorIcon, DragAndDrop, Id, Key, LayerId, Modifiers, Order, Pos2, Rect, Sense, Stroke,
    StrokeKind, Ui, UiBuilder, Vec2,
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::config::{Config, Favorite, PaneState, TabState};
use crate::dialogs::{Dialog, InputKind, pack_name};
use crate::icons;
use crate::lang::{key, t};
use crate::ops::{Jobs, Request, Source};
use crate::pane::{Action, Cmd, DirSize, DragPayload, Env, Pane, PaneCx, Tab};
use crate::preview::Preview;
use crate::tf;
use crate::theme::{self, Palette, ThemeMode};
use crate::util::{fmt_size, invalid_name_reason, today};
use crate::vfs::{Bus, Location, Msg, SortKey, io_msg};
use crate::widgets::{draw_text, icon_button, menu_begin, menu_item, menu_separator, progress_bar, text_width};
use crate::{backup, script, win};

const TOP_H: f32 = 46.0;
pub(crate) const BOTTOM_H: f32 = 46.0;
const GAP: f32 = 8.0;

/// Startparameter (vor allem für automatische Bildschirmfotos beim Entwickeln).
#[derive(Default)]
pub struct Args {
    pub shot: Option<PathBuf>,
    pub left: Option<String>,
    pub right: Option<String>,
    pub theme: Option<ThemeMode>,
    pub lang: Option<crate::lang::Lang>,
    pub demo: Option<String>,
    pub side_by_side: bool,
    pub size: Option<(f32, f32)>,
    /// Abgespielte Eingaben, siehe `script.rs`
    pub script: Option<String>,
    pub process_start: Option<Instant>,
}

struct Toast {
    text: String,
    error: bool,
    born: Instant,
}

/// Etwas, das sich mit Strg+Z zurücknehmen lässt
pub(crate) enum UndoEntry {
    /// Umbenannt: Paare (alt, neu) in diesem Ordner
    Rename { loc: Location, pairs: Vec<(String, String)> },
    /// Kopiert, verschoben oder in den Papierkorb gelegt; `title` nennt den Auftrag
    Ops { title: String, records: Vec<crate::ops::Undo> },
}

macro_rules! env {
    ($s:expr) => {
        Env { bus: &$s.bus, cfg: &mut $s.cfg }
    };
}

pub struct App {
    pub(crate) cfg: Config,
    mode: ThemeMode,
    pub(crate) pal: Palette,
    pub(crate) panes: [Pane; 2],
    pub(crate) active: usize,
    pane_rects: [Rect; 2],
    rx: Receiver<Msg>,
    pub(crate) bus: Bus,
    pub(crate) jobs: Jobs,
    drives: Vec<win::Drive>,
    /// Je Laufwerk (`D:\`): hat es einen Papierkorb? `None`: die Frage läuft noch. Die Auskunft von Windows kann
    /// dauern, deshalb wird sie im Hintergrund geholt, sobald ein Ordner des Laufwerks gezeigt wird – beim Löschen
    /// liegt sie dann schon vor.
    bins: HashMap<String, Option<bool>>,
    folders: Vec<(&'static str, PathBuf)>,
    /// Verbundene Telefone und weitere Cloud-Dienste, wie sie beim Start eingehängt waren
    places: Vec<win::Place>,
    /// Per Kabel angeschlossene Geräte: (Kennung, Name); kommt zusammen mit den Laufwerken aus dem Hintergrund
    devices: Vec<(String, String)>,
    /// Cloud-Ordner (OneDrive, Dropbox …): Pfade in Kleinbuchstaben mit abschließendem `\\`
    cloud_roots: Vec<String>,
    types: HashMap<String, String>,
    pub(crate) dialog: Option<Dialog>,
    toasts: Vec<Toast>,
    /// Schnellansicht je Fenster (ein- und ausgeschaltet über `cfg.quick_view`)
    preview: [Preview; 2],
    pub(crate) shell_icons: crate::shellicon::ShellIcons,
    thumbs: crate::thumbs::Thumbs,
    watcher: Option<RecommendedWatcher>,
    /// Die Ordner aller offenen Tabs, die gerade auf Änderungen überwacht werden
    watched: HashSet<PathBuf>,
    dirty: HashMap<PathBuf, Instant>,
    /// Hatte das Fenster im letzten Bild den Fokus? Beim Zurückkehren werden die Ordner nachgeprüft.
    was_focused: bool,
    pub(crate) hwnd: Option<isize>,
    /// Das eigene Ablageziel ist am Fenster angemeldet; sonst kommen abgelegte Dateien über die Fensterbibliothek.
    own_drop_target: bool,
    titlebar_mode: Option<ThemeMode>,
    tab_pressed: Option<Modifiers>,
    paste_down: bool,
    /// Kopierte Elemente, die nicht in die Windows-Zwischenablage passen (ZIP)
    clip_internal: Option<(Vec<Source>, bool)>,
    /// Was sich mit Strg+Z zurücknehmen lässt, das Jüngste zuletzt
    pub(crate) undo_stack: Vec<UndoEntry>,
    args: Args,
    started: Instant,
    frames: u64,
    demo_at: Option<(u64, Instant)>,
    shot_sent: bool,
    script: VecDeque<script::Step>,
    script_wait: u32,
    /// Bis dahin ruht das Skript (Befehl „sleep")
    script_until: Option<Instant>,
    /// Solange das Skript ruht, wird nicht ständig neu gezeichnet (Befehl „idle"): wie im normalen Betrieb
    script_idle: bool,
    /// Vom Skript vorgegeben: das Fenster gilt als im Vordergrund oder im Hintergrund (Befehle „focus", „blur")
    script_focus: Option<bool>,
    script_last: Instant,
    frame_times: Vec<Duration>,
    first_frame: Option<Duration>,
    quit_confirmed: bool,
    /// Gerade an Windows übergebenes Ziehen: falls die Dateien doch wieder im eigenen Fenster landen
    own_drag: Option<(Vec<Source>, Instant)>,
    /// Nach einem von Windows geführten Ziehen der Oberfläche das Loslassen der Maustaste nachreichen
    inject_release: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, mut cfg: Config, args: Args) -> Self {
        let window_ready = args.process_start.map(|t| t.elapsed());
        let ctx = cc.egui_ctx.clone();
        theme::install_fonts(&ctx);
        let fonts_ready = args.process_start.map(|t| t.elapsed());
        let system = ctx.system_theme().map(|t| if t == egui::Theme::Dark { ThemeMode::Dark } else { ThemeMode::Light });
        let mode = args.theme.or(cfg.theme).or(system).unwrap_or(ThemeMode::Dark);
        let pal = Palette::of(mode);
        theme::apply(&ctx, &pal);
        // Doppelklick so schnell oder langsam werten, wie es in Windows eingestellt ist.
        ctx.options_mut(|o| o.input_options.max_double_click_delay = win::double_click_secs());

        let (tx, rx) = mpsc::channel();
        let bus = Bus::new(tx, ctx.clone());

        if args.side_by_side {
            cfg.stacked = false;
        }
        for (i, text) in [&args.left, &args.right].into_iter().enumerate() {
            if let Some(loc) = text.as_deref().and_then(Location::parse) {
                let tab = TabState { loc, sort: SortKey::Name, desc: false, grid: false };
                if args.shot.is_some() {
                    cfg.panes[i] = PaneState { tabs: vec![tab], active: 0 };
                } else {
                    // Ein beim Start übergebener Ordner kommt als zusätzlicher Tab dazu, statt die gemerkten zu ersetzen.
                    let pane = &mut cfg.panes[i];
                    match pane.tabs.iter().position(|t| t.loc == tab.loc) {
                        Some(existing) => pane.active = existing,
                        None => {
                            pane.tabs.push(tab);
                            pane.active = pane.tabs.len() - 1;
                        }
                    }
                    if i == 0 {
                        cfg.active_pane = 0;
                    }
                }
            }
        }
        let transfers_serial = cfg.transfers_serial;
        let panes = [Pane::new(&cfg.panes[0]), Pane::new(&cfg.panes[1])];
        let active = cfg.active_pane.min(1);
        if args.shot.is_some() {
            eprintln!(
                "Start: Fenster bereit nach {window_ready:.0?}, Schriften geladen nach {fonts_ready:.0?}; Speicher jetzt {:.0} MB",
                win::private_memory_mb()
            );
        }

        {
            let bus = bus.clone();
            std::thread::spawn(move || {
                bus.send(Msg::Drives(win::drives()));
                bus.send(Msg::Devices(crate::device::devices()));
            });
        }
        // Aus ZIP-Archiven zum Öffnen geholte Dateien liegen im Temp-Ordner: Altes (über einen Tag) aufräumen.
        std::thread::spawn(|| {
            let Ok(rd) = std::fs::read_dir(std::env::temp_dir().join("TurboCommanderLite")) else { return };
            for entry in rd.flatten() {
                let age = entry.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok());
                if age.is_some_and(|a| a > Duration::from_secs(24 * 3600)) {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        });
        let watcher = {
            let bus = bus.clone();
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let Ok(ev) = res else { return };
                use notify::EventKind::*;
                if !matches!(ev.kind, Create(_) | Remove(_) | Modify(_)) {
                    return;
                }
                let mut last: Option<PathBuf> = None;
                for path in &ev.paths {
                    if let Some(parent) = path.parent() {
                        if last.as_deref() != Some(parent) {
                            last = Some(parent.to_path_buf());
                            bus.send(Msg::FsChanged(parent.to_path_buf()));
                        }
                    }
                }
            })
            .ok()
        };

        Self {
            cfg,
            mode,
            pal,
            panes,
            active,
            pane_rects: [Rect::NOTHING; 2],
            rx,
            shell_icons: crate::shellicon::ShellIcons::new(bus.clone()),
            thumbs: crate::thumbs::Thumbs::new(bus.clone()),
            bus,
            jobs: {
                let mut jobs = Jobs::default();
                jobs.serial = transfers_serial;
                jobs
            },
            drives: Vec::new(),
            bins: HashMap::new(),
            folders: {
                let mut folders = win::user_folders();
                folders.extend(win::cloud_folders());
                folders
            },
            places: win::places(),
            devices: Vec::new(),
            cloud_roots: cloud_roots(),
            types: HashMap::new(),
            dialog: None,
            toasts: Vec::new(),
            preview: [Preview::default(), Preview::default()],
            watcher,
            watched: HashSet::new(),
            dirty: HashMap::new(),
            was_focused: false,
            hwnd: None,
            own_drop_target: false,
            titlebar_mode: None,
            tab_pressed: None,
            paste_down: false,
            clip_internal: None,
            undo_stack: Vec::new(),
            args,
            started: Instant::now(),
            frames: 0,
            demo_at: None,
            shot_sent: false,
            script: VecDeque::new(),
            script_wait: 0,
            script_until: None,
            script_idle: false,
            script_focus: None,
            script_last: Instant::now(),
            frame_times: Vec::new(),
            first_frame: None,
            quit_confirmed: false,
            own_drag: None,
            inject_release: false,
        }
    }

    /// Bricht laufende Aufgaben ab und schließt das Fenster.
    pub(crate) fn quit_now(&mut self, ctx: &egui::Context) {
        self.quit_confirmed = true;
        for job in self.jobs.running() {
            job.shared.cancel.store(true, Relaxed);
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn other(&self) -> usize {
        1 - self.active
    }

    /// Das Kästchen „künftig ohne Rückfrage" wurde angekreuzt: abschalten und sagen, wo es sich zurückstellen lässt.
    pub(crate) fn stop_asking(&mut self, what: crate::config::Ask) {
        use crate::config::Ask;
        let done = match what {
            Ask::Copy => t("Ab jetzt wird ohne Rückfrage ins andere Fenster kopiert.", "From now on, items are copied to the other pane without asking."),
            Ask::Move => t("Ab jetzt wird ohne Rückfrage ins andere Fenster verschoben.", "From now on, items are moved to the other pane without asking."),
            Ask::Trash => t("Ab jetzt geht es ohne Rückfrage in den Papierkorb.", "From now on, items go to the Recycle Bin without asking."),
            Ask::Delete => t("Ab jetzt wird ohne Rückfrage endgültig gelöscht.", "From now on, items are deleted permanently without asking."),
        };
        let back = t("Wieder einschalten: Menü → Rückfragen", "To switch it back on: menu → Confirmations");
        self.cfg.set_asks(what, false);
        self.save_config();
        self.toast(format!("{done}\n{back}"), false);
    }

    pub(crate) fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toasts.push(Toast { text: text.into(), error, born: Instant::now() });
    }

    /// Legt etwas auf den Rückgängig-Stapel; mehr als zwanzig Schritte werden nicht aufgehoben.
    pub(crate) fn remember(&mut self, entry: UndoEntry) {
        self.undo_stack.push(entry);
        if self.undo_stack.len() > 20 {
            self.undo_stack.remove(0);
        }
    }

    pub(crate) fn save_config(&mut self) {
        self.cfg.panes = [self.panes[0].state(), self.panes[1].state()];
        self.cfg.active_pane = self.active;
        self.cfg.theme = Some(self.mode);
        if self.args.shot.is_none() {
            self.cfg.save();
        }
    }

    pub(crate) fn goto(&mut self, loc: Location) {
        self.panes[self.active].tab_mut().navigate(loc, &mut env!(self));
    }

    fn refresh_drives(&self) {
        let bus = self.bus.clone();
        std::thread::spawn(move || {
                bus.send(Msg::Drives(win::drives()));
                bus.send(Msg::Devices(crate::device::devices()));
            });
    }

    /// Fragt Windows im Hintergrund, ob das Laufwerk einen Papierkorb hat; die Antwort kommt als `Msg::RecycleBin`.
    fn ask_bin(&mut self, root: &str) {
        self.bins.entry(root.to_owned()).or_insert(None);
        let (bus, root) = (self.bus.clone(), root.to_owned());
        std::thread::spawn(move || {
            let has = win::has_recycle_bin(Path::new(&root));
            bus.send(Msg::RecycleBin { root, has });
        });
    }

    /// Hat das Laufwerk einen Papierkorb? Die gemerkte Auskunft gilt sofort – so öffnet sich die Rückfrage beim
    /// Löschen ohne Wartezeit. Im Hintergrund wird trotzdem neu gefragt: hinter dem Buchstaben kann inzwischen ein
    /// anderes Laufwerk stecken, eine offene Rückfrage richtet sich dann nach der neuen Antwort.
    fn has_bin(&mut self, root: &str) -> bool {
        let started = Instant::now();
        let known = self.bins.get(root).copied().flatten();
        let has = match known {
            Some(has) => {
                self.ask_bin(root);
                has
            }
            // Noch keine Auskunft (das Laufwerk wurde eben erst geöffnet): dann muss gewartet werden.
            None => {
                let has = win::has_recycle_bin(Path::new(root));
                self.bins.insert(root.to_owned(), Some(has));
                has
            }
        };
        if self.args.shot.is_some() {
            let how = if known.is_some() { "gemerkt" } else { "bei Windows erfragt" };
            eprintln!("Papierkorb auf {root}: {has}, {how} in {:.2?}", started.elapsed());
        }
        has
    }

    // ------------------------------------------------------------ Nachrichten aus dem Hintergrund

    fn tab_by_id(&mut self, id: u64) -> Option<&mut Tab> {
        self.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()).find(|t| t.id == id)
    }

    pub(crate) fn reload_where(&mut self, pred: impl Fn(&Location) -> bool, focus: Option<&str>) {
        for i in 0..2 {
            for t in &mut self.panes[i].tabs {
                if t.generation > 0 && pred(&t.loc) {
                    if let Some(name) = focus {
                        t.focus_name = Some(name.to_owned());
                    }
                    t.reload(&mut env!(self));
                }
            }
        }
    }

    fn on_msg(&mut self, ctx: &egui::Context, msg: Msg) {
        match msg {
            Msg::Listed { tab, generation, loc, result } => {
                let show_hidden = self.cfg.show_hidden;
                let measure = self.args.shot.is_some();
                let bus = self.bus.clone();
                let cloud_roots = self.cloud_roots.clone();
                // Ergebnis einer abgeschlossenen Netzwerksuche merken (für den nächsten Besuch).
                if let (Location::Network { server: None }, Ok(listing)) = (&loc, &result) {
                    if !listing.pending {
                        self.cfg.known_computers = listing.entries.iter().map(|e| e.name.clone()).collect();
                    }
                }
                // Schon jetzt klären, ob das Laufwerk einen Papierkorb hat – nicht erst, wenn gelöscht wird.
                if let Some(root) = loc.local_path().and_then(win::bin_root).filter(|root| !self.bins.contains_key(root)) {
                    self.ask_bin(&root);
                }
                if let Some(t) = self.tab_by_id(tab).filter(|t| t.loc == loc) {
                    let sort_start = Instant::now();
                    let waited = t.load_started.elapsed();
                    t.on_listed(generation, result, show_hidden, &bus);
                    t.note_cloud(&cloud_roots);
                    if measure {
                        eprintln!(
                            "Ordner mit {} Einträgen: gelesen nach {waited:.1?}, sortiert und aufbereitet in {:.1?} ({:.2} s nach dem Start)",
                            t.entries.len(),
                            sort_start.elapsed(),
                            self.started.elapsed().as_secs_f64()
                        );
                    }
                }
            }
            Msg::EmptyDirs { tab, loc, names, done } => {
                if let Some(t) = self.tab_by_id(tab).filter(|t| t.loc == loc) {
                    t.on_empty_dirs(names, done);
                }
            }
            Msg::DirSize { tab, loc, name, size, done } => {
                if let Some(t) = self.tab_by_id(tab).filter(|t| t.loc == loc) {
                    t.sizes.insert(name, DirSize { size, done });
                }
            }
            Msg::SearchHits { tab, search, hits, checked } => {
                if let Some(t) = self.tab_by_id(tab) {
                    t.on_search_hits(search, hits, checked);
                }
            }
            Msg::SearchDone { tab, search } => {
                if let Some(t) = self.tab_by_id(tab) {
                    t.on_search_done(search);
                }
            }
            Msg::Preview { key, data } => {
                if let Some(preview) = self.preview.iter_mut().find(|v| v.awaits(&key)) {
                    preview.on_data(ctx, key, data);
                }
            }
            Msg::Icon { key, image } => self.shell_icons.on_loaded(ctx, key, image),
            Msg::FsChanged(path) => {
                self.dirty.entry(path).or_insert_with(Instant::now);
            }
            Msg::Drives(drives) => self.drives = drives,
            Msg::Devices(devices) => self.devices = devices,
            Msg::QuickAccess(folders) => {
                let mut added = 0;
                for (name, path) in folders {
                    let known = self.cfg.favorites.iter().any(|f| {
                        f.loc.local_path().is_some_and(|have| have.as_os_str().eq_ignore_ascii_case(path.as_os_str()))
                    });
                    if !known {
                        // Der Name, wie ihn der Explorer zeigt („Musik" statt „Music")
                        let loc = Location::Local(path);
                        let name = if name.trim().is_empty() { loc.leaf() } else { name };
                        self.cfg.favorites.push(Favorite { name, loc });
                        added += 1;
                    }
                }
                if added > 0 {
                    self.save_config();
                }
                let text = match added {
                    0 => t("Alle Ordner aus dem Schnellzugriff sind schon Favoriten.", "All Quick access folders are favorites already.")
                        .to_owned(),
                    1 => t("1 Ordner aus dem Schnellzugriff übernommen", "1 folder taken over from Quick access").to_owned(),
                    n => tf!("{n} Ordner aus dem Schnellzugriff übernommen", "{n} folders taken over from Quick access"),
                };
                self.toast(text, false);
            }
            Msg::RecycleBin { root, has } => {
                // Eine offene Rückfrage, die sich nach diesem Laufwerk richtet, sagt jetzt das Richtige.
                if let Some(Dialog::Delete { permanent, bin: Some(asked), .. }) = &mut self.dialog {
                    if *asked == root {
                        *permanent = !has;
                    }
                }
                self.bins.insert(root, Some(has));
            }
            Msg::Thumb { request, loaded } => self.thumbs.on_loaded(ctx, request, loaded),
            Msg::Dropped { paths, pos, shift, ctrl, temp } => {
                let mods = Modifiers { shift, ctrl, command: ctrl, ..Modifiers::NONE };
                self.drop_paths(ctx, paths, pos, mods, temp);
            }
            Msg::Toast { text, error } => self.toast(text, error),
            Msg::OpenTemp(path) => {
                if !win::shell_open(&path) {
                    self.toast(t("Die Datei lässt sich nicht öffnen.", "The file cannot be opened."), true);
                }
            }
            Msg::Refresh { loc, focus } => self.reload_where(|l| *l == loc, focus.as_deref()),
            Msg::BackupPicked(path) => self.open_backup(path),
            Msg::Renamed { loc, done, errors, undo } => {
                let focus = if done.len() == 1 { Some(done[0].1.clone()) } else { None };
                self.reload_where(|l| *l == loc, focus.as_deref());
                if !errors.is_empty() {
                    self.dialog = Some(Dialog::Errors {
                        title: t("Nicht alles konnte umbenannt werden", "Not everything could be renamed").to_owned(),
                        lines: errors,
                    });
                } else if done.len() > 1 {
                    let what = if undo { t("zurückbenannt", "renamed back") } else { t("umbenannt", "renamed") };
                    self.toast(
                        tf!("{} Elemente {what} · Strg+Z macht es rückgängig", "{} items {what} · Ctrl+Z undoes it", done.len()),
                        false,
                    );
                }
                if !undo && !done.is_empty() {
                    self.remember(UndoEntry::Rename { loc, pairs: done });
                }
            }
            Msg::JobFinished { id } => {
                let Some(job) = self.jobs.remove(id) else { return };
                let errors = std::mem::take(&mut *job.shared.errors.lock().unwrap());
                let cancelled = job.shared.cancel.load(Relaxed);
                // Was der Auftrag getan hat, lässt sich mit Strg+Z zurücknehmen (auch der erledigte Teil nach einem Abbruch).
                let records = std::mem::take(&mut *job.shared.undo.lock().unwrap());
                if !records.is_empty() {
                    self.remember(UndoEntry::Ops { title: job.title.clone(), records });
                }
                let touched = job.touched.clone();
                self.reload_where(|l| touched.contains(l), None);
                // Was gelöscht wurde, liegt jetzt im Papierkorb.
                self.reload_where(|l| *l == Location::RecycleBin, None);
                self.refresh_drives();
                if !errors.is_empty() {
                    let title = format!(
                        "{} – {}",
                        job.title,
                        if errors.len() == 1 {
                            t("1 Problem", "1 problem").to_owned()
                        } else {
                            tf!("{} Probleme", "{} problems", errors.len())
                        }
                    );
                    if self.dialog.is_none() {
                        self.dialog = Some(Dialog::Errors { title, lines: errors });
                    } else {
                        self.toast(title, true);
                    }
                } else if cancelled {
                    self.toast(tf!("Abgebrochen: {}", "Canceled: {}", job.title), false);
                } else if !job.quiet && job.started.elapsed() > Duration::from_millis(1500) {
                    self.toast(tf!("Fertig: {}", "Done: {}", job.title), false);
                }
                // Der nächste wartende Auftrag ist an der Reihe.
                self.jobs.pump(&self.bus);
            }
        }
    }

    /// Überwacht die Ordner aller offenen Tabs – auch der Tabs im Hintergrund, damit sie beim nächsten Zeigen
    /// wissen, dass sie veraltet sind. Ein Ordner bleibt auch überwacht, während er neu gelesen wird: was in dieser
    /// Zeit geschieht, ginge sonst verloren.
    fn sync_watcher(&mut self) {
        let Some(w) = self.watcher.as_mut() else { return };
        let mut want: HashSet<&Path> = HashSet::new();
        for t in self.panes.iter().flat_map(|pane| &pane.tabs) {
            // Erst nach erfolgreichem Laden beobachten: bei unerreichbaren Netzpfaden würde das Anmelden hängen.
            if t.search.is_none() && t.listed.as_ref() == Some(&t.loc) {
                want.extend(t.loc.local_path());
            }
        }
        if want.len() == self.watched.len() && want.iter().all(|path| self.watched.contains(*path)) {
            return;
        }
        let gone: Vec<PathBuf> = self.watched.iter().filter(|path| !want.contains(path.as_path())).cloned().collect();
        let new: Vec<PathBuf> = want.iter().filter(|path| !self.watched.contains(**path)).map(|path| path.to_path_buf()).collect();
        for path in gone {
            let _ = w.unwatch(&path);
            self.watched.remove(&path);
        }
        for path in new {
            let _ = w.watch(&path, RecursiveMode::NonRecursive);
            self.watched.insert(path);
        }
    }

    /// Sicherheitsnetz zur Überwachung, wenn das Fenster wieder den Fokus bekommt: Windows meldet nicht jede
    /// Änderung (bei sehr vielen auf einmal gehen Meldungen verloren, manche Netz- und Cloud-Laufwerke melden gar
    /// nichts). An der Änderungszeit eines Ordners ist aber abzulesen, ob in ihm etwas hinzukam oder verschwand –
    /// dann wird er behandelt, als wäre die Meldung gekommen. Geprüft wird im Hintergrund: ein nicht mehr
    /// erreichbares Laufwerk darf die Oberfläche nicht aufhalten.
    fn recheck_folders(&self) {
        let mut folders: Vec<(PathBuf, Option<std::time::SystemTime>)> = Vec::new();
        for t in self.panes.iter().flat_map(|pane| &pane.tabs) {
            if t.search.is_some() || t.listed.as_ref() != Some(&t.loc) {
                continue;
            }
            if let Some(path) = t.loc.local_path() {
                if !folders.iter().any(|(known, stamp)| known == path && *stamp == t.dir_modified) {
                    folders.push((path.to_path_buf(), t.dir_modified));
                }
            }
        }
        if folders.is_empty() {
            return;
        }
        let bus = self.bus.clone();
        std::thread::spawn(move || {
            for (path, listed) in folders {
                if crate::vfs::dir_modified(&path) != listed {
                    bus.send(Msg::FsChanged(path));
                }
            }
        });
    }

    /// Lädt geänderte Ordner neu – gebündelt, damit viele Änderungen nur einen Neuaufbau auslösen.
    fn flush_dirty(&mut self, ctx: &egui::Context) {
        if self.dirty.is_empty() {
            return;
        }
        const SETTLE: Duration = Duration::from_millis(250);
        let now = Instant::now();
        let due: Vec<PathBuf> = self.dirty.iter().filter(|(_, t)| now.duration_since(**t) >= SETTLE).map(|(p, _)| p.clone()).collect();
        for path in due {
            self.dirty.remove(&path);
            let loc = Location::Local(path.clone());
            let mut retry = false;
            for i in 0..2 {
                let shown = self.panes[i].active;
                for index in 0..self.panes[i].tabs.len() {
                    let t = &mut self.panes[i].tabs[index];
                    if t.loc != loc || t.search.is_some() || t.generation == 0 {
                        continue;
                    }
                    if index != shown {
                        // Im Hintergrund: erst neu laden, wenn der Tab wieder gezeigt wird.
                        t.stale = true;
                    } else if t.loading || t.rename.is_some() {
                        retry = true;
                    } else {
                        t.reload(&mut env!(self));
                    }
                }
            }
            if retry {
                self.dirty.insert(path, now);
            }
        }
        if !self.dirty.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    // ------------------------------------------------------------ Tastatur

    fn handle_keys(&mut self, ctx: &egui::Context, modal: bool) {
        let tab_key = self.tab_pressed.take();
        let v_down = ctx.input(|i| i.focused) && win::key_down(0x11) && win::key_down(0x56);
        let paste = v_down && !self.paste_down;
        self.paste_down = v_down;
        if modal {
            return;
        }
        let typing = ctx.text_edit_focused();
        if !typing {
            // Fokus auf Schaltflächen stört nur: Enter und Leertaste gehören der Dateiliste.
            ctx.memory_mut(|m| {
                if let Some(id) = m.focused() {
                    m.surrender_focus(id);
                }
            });
        }
        let popup = egui::Popup::is_any_open(ctx);
        if typing || popup {
            return;
        }

        if let Some(mods) = tab_key {
            if mods.ctrl {
                let pane = &mut self.panes[self.active];
                let n = pane.tabs.len();
                pane.active = if mods.shift { (pane.active + n - 1) % n } else { (pane.active + 1) % n };
            } else {
                self.active = self.other();
            }
        }

        let mut cmds: Vec<Cmd> = Vec::new();
        let mut escape = false;
        ctx.input_mut(|i| {
            let ctrl = Modifiers::CTRL;
            // Vor Strg+F prüfen: das einfache Kürzel passt sonst auch mit gedrückter Umschalttaste.
            if i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::F) {
                cmds.push(Cmd::FullOnly(t("Suche in Dateien, nach Größe und Datum", "Search in files, by size and date")));
            }
            let pairs = [
                (Key::T, Cmd::NewTab),
                (Key::W, Cmd::CloseTab),
                (Key::L, Cmd::EditPath),
                (Key::F, Cmd::FocusFilter),
                (Key::R, Cmd::Refresh),
                (Key::M, Cmd::FullOnly(t("Mehrfach umbenennen", "Batch rename"))),
                (Key::I, Cmd::FullOnly("Media Info")),
                (Key::G, Cmd::FullOnly(t("Ordner vergleichen", "Compare folders"))),
                (Key::B, Cmd::ToggleGrid),
                (Key::J, Cmd::FullOnly(t("Übertragungsliste", "Transfer list"))),
                (Key::H, Cmd::ToggleHidden),
                (Key::D, Cmd::AddFavorite),
                (Key::A, Cmd::SelectAll),
                (Key::U, Cmd::SwapPanes),
                (Key::Q, Cmd::QuickView),
                (Key::Z, Cmd::Undo),
            ];
            for (key, cmd) in pairs {
                if i.consume_key(ctrl, key) {
                    cmds.push(cmd);
                }
            }
            // Strg+1 … Strg+9 starten in der Vollversion die eigenen Werkzeuge.
            let digits = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
            if digits.into_iter().any(|key| i.consume_key(ctrl, key)) {
                cmds.push(Cmd::FullOnly(t("Eigene Werkzeuge", "Custom tools")));
            }
            for ev in &i.events {
                match ev {
                    egui::Event::Copy => cmds.push(Cmd::ClipCopy),
                    // Windows kennt Umschalt+Entf auch als altes Kürzel für „Ausschneiden"; die Oberflächen-Bibliothek
                    // meldet deshalb „Ausschneiden" und verschluckt die Taste. In einer Dateiliste heißt
                    // Umschalt+Entf aber: endgültig löschen. Strg+X bleibt Ausschneiden.
                    egui::Event::Cut if i.modifiers.shift && !i.modifiers.ctrl => cmds.push(Cmd::DeletePermanent),
                    egui::Event::Cut => cmds.push(Cmd::ClipCut),
                    _ => {}
                }
            }
            let m = i.modifiers;
            if m.alt && !m.ctrl {
                if i.key_pressed(Key::ArrowLeft) {
                    cmds.push(Cmd::Back);
                }
                if i.key_pressed(Key::ArrowRight) {
                    cmds.push(Cmd::Forward);
                }
                if i.key_pressed(Key::ArrowUp) {
                    cmds.push(Cmd::Up);
                }
                if i.key_pressed(Key::Enter) {
                    cmds.push(Cmd::Properties);
                }
                if i.key_pressed(Key::D) {
                    cmds.push(Cmd::EditPath);
                }
            }
            if !m.alt && !m.ctrl {
                let plain = [
                    (Key::F1, Cmd::Shortcuts),
                    (Key::F2, Cmd::Rename),
                    (Key::F3, Cmd::QuickView),
                    (Key::F5, Cmd::Copy),
                    (Key::F6, Cmd::Move),
                    (Key::F7, Cmd::NewFolder),
                    (Key::F9, Cmd::ToggleLayout),
                ];
                for (key, cmd) in plain {
                    if i.key_pressed(key) && !m.shift {
                        cmds.push(cmd);
                    }
                }
                if i.key_pressed(Key::F4) && m.shift {
                    cmds.push(Cmd::NewFile);
                }
                if i.key_pressed(Key::Delete) || i.key_pressed(Key::F8) {
                    cmds.push(if m.shift { Cmd::DeletePermanent } else { Cmd::Delete });
                }
                escape = i.key_pressed(Key::Escape);
            }
        });
        if paste {
            cmds.push(Cmd::ClipPaste);
        }
        if escape {
            let tab = self.panes[self.active].tab_mut();
            if tab.marked_count > 0 {
                tab.clear_marks();
            } else if tab.search.is_some() || !tab.filter.is_empty() {
                tab.clear_search(&mut env!(self));
            } else if self.cfg.quick_view[self.active] {
                self.cfg.quick_view[self.active] = false;
            }
        }
        if self.panes[self.active].tab().rename.is_none() {
            if let Some(cmd) = self.panes[self.active].tab_mut().keys(ctx, &env!(self)) {
                cmds.push(cmd);
            }
        }
        for cmd in cmds {
            self.run(cmd, ctx);
        }
    }

    // ------------------------------------------------------------ Befehle

    fn apply(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Activate(i) => self.active = i.min(1),
            Action::Cmd(pane, cmd) => {
                self.active = pane.min(1);
                self.run(cmd, ctx);
            }
            Action::Rename { pane, old, new } => self.rename_one(pane.min(1), old, new),
            Action::OpenRemote(source) => {
                self.jobs.start(Request::OpenRemote { source }, &self.bus);
            }
            Action::OpenBackup(path) => self.open_backup(path),
            Action::MoveTab { from, tab, pos } => {
                let from = from.min(1);
                let to = 1 - from;
                if self.pane_rects[to].contains(pos) {
                    match self.panes[from].take_tab(tab) {
                        Some(moved) => self.panes[to].insert_tab(moved),
                        // Der einzige Tab bleibt; drüben öffnet sich derselbe Ordner.
                        None => {
                            if let Some(loc) = self.panes[from].tabs.iter().find(|t| t.id == tab).map(|t| t.loc.clone()) {
                                self.panes[to].add_tab(loc, &mut env!(self));
                            }
                        }
                    }
                    self.active = to;
                }
            }
            Action::Drop { pane, dest, payload, mods } => {
                self.active = pane.min(1);
                let mv = drop_moves(&payload.sources, &dest, mods);
                self.jobs.start(Request::Transfer { sources: payload.sources.clone(), dest, mv }, &self.bus);
            }
        }
    }

    fn current_local(&self) -> Option<PathBuf> {
        let t = self.panes[self.active].tab();
        let e = t.current().filter(|e| !e.is_parent)?;
        t.entry_dir(e).local_path().map(|d| d.join(&e.name))
    }

    fn current_dir_target(&self) -> Option<Location> {
        let t = self.panes[self.active].tab();
        let e = t.current().filter(|e| e.is_dir && !e.is_parent)?;
        Some(t.entry_location(e))
    }

    fn run(&mut self, cmd: Cmd, ctx: &egui::Context) {
        let a = self.active;
        let o = self.other();
        match cmd {
            Cmd::Open => {
                if let Some(action) = self.panes[a].open_current(&mut env!(self)) {
                    self.apply(action, ctx);
                }
            }
            Cmd::OpenWith => {
                if let Some(path) = self.current_local() {
                    win::shell_open_with(&path);
                }
            }
            Cmd::OpenInNewTab => {
                if let Some(loc) = self.current_dir_target() {
                    self.panes[a].add_tab(loc, &mut env!(self));
                }
            }
            Cmd::OpenInOther => {
                if let Some(loc) = self.current_dir_target() {
                    self.panes[o].tab_mut().navigate(loc, &mut env!(self));
                }
            }
            Cmd::Reveal => {
                if let Some(path) = self.current_local() {
                    win::reveal_in_explorer(&path);
                }
            }
            Cmd::Properties => {
                if let Some(path) = self.current_local() {
                    win::shell_properties(&path);
                }
            }
            Cmd::FullOnly(feature) => self.toast(
                tf!(
                    "„{feature}\" gibt es in der Vollversion von TurboCommander.",
                    "“{feature}” is part of the full version of TurboCommander."
                ),
                false,
            ),
            Cmd::Copy | Cmd::Move => {
                let mv = cmd == Cmd::Move;
                let sources = self.panes[a].tab().sources();
                let dest = self.panes[o].tab().loc.clone();
                if sources.is_empty() || self.panes[a].tab().loc.is_virtual() {
                    self.toast(t("Nichts ausgewählt.", "Nothing selected."), false);
                } else if self.panes[a].tab().loc == Location::RecycleBin {
                    self.toast(
                        t("Aus dem Papierkorb bitte erst wiederherstellen.", "Please restore items from the Recycle Bin first."),
                        false,
                    );
                } else if mv && !self.panes[a].tab().loc.removable() {
                    self.toast(
                        t(
                            "Von hier kann nicht verschoben werden – nur kopieren.",
                            "Nothing can be moved from here – only copied.",
                        ),
                        true,
                    );
                } else {
                    use crate::config::Ask;
                    // Ohne Rückfrage geht es geradewegs in den Ordner des anderen Fensters – sofern dort etwas
                    // abgelegt werden kann und es nicht derselbe Ordner ist; sonst fragt der Dialog nach dem Ziel.
                    let ask = self.cfg.asks(if mv { Ask::Move } else { Ask::Copy });
                    if !ask && dest.writable() && sources.iter().all(|s| s.dir != dest) {
                        self.jobs.start(Request::Transfer { sources, dest, mv }, &self.bus);
                        self.panes[a].tab_mut().clear_marks();
                    } else {
                        let dest = if dest.writable() { dest.display() } else { String::new() };
                        self.dialog = Some(Dialog::Transfer { sources, dest, mv, fresh: true, error: None, no_more: false });
                    }
                }
            }
            Cmd::Delete | Cmd::DeletePermanent => {
                let tab = self.panes[a].tab();
                // Im Papierkorb heißt Löschen: endgültig entfernen.
                if tab.loc == Location::RecycleBin {
                    let chosen = tab.selected_entries();
                    let ids: Vec<std::ffi::OsString> = chosen.iter().filter_map(crate::bin::id_of).collect();
                    if !ids.is_empty() {
                        let names = chosen.iter().map(|e| e.name.clone()).collect();
                        self.dialog = Some(Dialog::Bin { names, ids, all: false });
                    }
                    return;
                }
                let sources = tab.sources();
                if sources.is_empty() {
                    return;
                }
                if !tab.loc.removable() {
                    self.toast(t("Hier kann nicht gelöscht werden.", "Nothing can be deleted here."), true);
                    return;
                }
                // Ohne Papierkorb (Netzlaufwerk, mancher USB-Stick) wird endgültig gelöscht – und das auch so gesagt.
                let forced = cmd == Cmd::DeletePermanent;
                let local = sources.first().and_then(|s| s.dir.local_path()).map(win::bin_root);
                let (no_bin, bin) = match local {
                    // Nur hier entscheidet der Papierkorb des Laufwerks, ob endgültig gelöscht wird.
                    Some(Some(root)) if !forced => (!self.has_bin(&root), Some(root)),
                    Some(None) => (true, None),
                    _ => (false, None),
                };
                let permanent = forced || no_bin;
                let what = if permanent { crate::config::Ask::Delete } else { crate::config::Ask::Trash };
                if self.cfg.asks(what) {
                    self.dialog = Some(Dialog::Delete { sources, permanent, bin, no_more: false });
                } else {
                    // Die Rückfrage ist abgeschaltet (Kästchen im Löschen-Dialog; zurückstellen: Menü → Rückfragen).
                    self.jobs.start(Request::Delete { sources, permanent }, &self.bus);
                }
            }
            Cmd::Rename => {
                let tab = self.panes[a].tab_mut();
                if tab.search.is_some() {
                    self.toast(
                        t(
                            "In Suchergebnissen bitte erst zum Ordner wechseln.",
                            "In search results, please go to the folder first.",
                        ),
                        false,
                    );
                } else if tab.loc.writable() {
                    tab.start_rename();
                }
            }
            Cmd::NewFolder | Cmd::NewFile => {
                let loc = &self.panes[a].tab().loc;
                let folder = cmd == Cmd::NewFolder;
                if (folder && loc.writable()) || loc.is_local() {
                    self.dialog = Some(Dialog::Input {
                        kind: if folder { InputKind::NewFolder } else { InputKind::NewFile },
                        title: if folder { t("Neuer Ordner", "New folder") } else { t("Neue Datei", "New file") },
                        value: if folder {
                            t("Neuer Ordner", "New folder").to_owned()
                        } else {
                            t("Neue Datei.txt", "New file.txt").to_owned()
                        },
                        fresh: true,
                    });
                } else {
                    self.toast(t("Hier kann nichts angelegt werden.", "Nothing can be created here."), true);
                }
            }
            Cmd::ClipCopy | Cmd::ClipCut => {
                let cut = cmd == Cmd::ClipCut;
                let tab = self.panes[a].tab();
                let sources = tab.sources();
                if sources.is_empty() || tab.loc.is_virtual() || tab.loc == Location::RecycleBin {
                    return;
                }
                let n = sources.len();
                let local: Option<Vec<PathBuf>> =
                    sources.iter().map(|s| s.dir.local_path().map(|d| d.join(&s.name))).collect();
                match local {
                    Some(paths) => {
                        self.clip_internal = None;
                        win::clipboard_set_files(&paths, cut);
                    }
                    None => self.clip_internal = Some((sources, cut)),
                }
                let what = if cut { t("ausgeschnitten", "cut") } else { t("kopiert", "copied") };
                self.toast(
                    format!(
                        "{} {what}",
                        if n == 1 { t("1 Element", "1 item").to_owned() } else { tf!("{n} Elemente", "{n} items") }
                    ),
                    false,
                );
            }
            Cmd::ClipPaste => {
                let dest = self.panes[a].tab().loc.clone();
                if !dest.writable() {
                    self.toast(t("Hier kann nichts eingefügt werden.", "Nothing can be pasted here."), true);
                    return;
                }
                if let Some((sources, cut)) = self.clip_internal.clone() {
                    if cut {
                        self.clip_internal = None;
                    }
                    self.jobs.start(Request::Transfer { sources, dest, mv: cut }, &self.bus);
                } else if let Some((paths, cut)) = win::clipboard_get_files() {
                    let sources = sources_from_paths(&paths);
                    if !sources.is_empty() {
                        self.jobs.start(Request::Transfer { sources, dest, mv: cut }, &self.bus);
                    }
                }
            }
            Cmd::CopyPath => {
                let tab = self.panes[a].tab();
                let text: Vec<String> = tab.sources().iter().map(|s| s.location().display()).collect();
                if !text.is_empty() {
                    ctx.copy_text(text.join("\r\n"));
                    self.toast(t("Pfad kopiert", "Path copied"), false);
                }
            }
            Cmd::Refresh => {
                self.thumbs.retry_missing();
                self.panes[a].tab_mut().reload(&mut env!(self));
                self.refresh_drives();
            }
            Cmd::Pack => {
                let tab = self.panes[a].tab();
                let sources = tab.sources();
                let paths: Option<Vec<PathBuf>> = sources.iter().map(|s| s.dir.local_path().map(|d| d.join(&s.name))).collect();
                match paths {
                    Some(paths) if !paths.is_empty() && tab.loc.is_local() => {
                        self.dialog = Some(Dialog::Input {
                            kind: InputKind::Pack(paths),
                            title: t("Als ZIP packen", "Pack as ZIP"),
                            value: pack_name(&sources, &tab.loc),
                            fresh: true,
                        });
                    }
                    _ => self.toast(
                        t("Packen gibt es für lokale Dateien und Ordner.", "Packing is available for local files and folders."),
                        false,
                    ),
                }
            }
            Cmd::Extract => {
                let tab = self.panes[a].tab();
                let (Some(archive), Location::Local(dir)) = (self.current_local(), &tab.loc) else { return };
                let mut stem = archive.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                // Aus „quellen.tar.gz" wird der Ordner „quellen", nicht „quellen.tar".
                if stem.to_lowercase().ends_with(".tar") {
                    stem.truncate(stem.len() - 4);
                }
                match crate::archive::list(&archive, "") {
                    Ok(listing) => {
                        let root = Location::Zip { archive: archive.clone(), inner: String::new() };
                        let sources: Vec<Source> = listing
                            .entries
                            .into_iter()
                            .map(|e| Source { dir: root.clone(), name: e.name, is_dir: e.is_dir, size: e.size })
                            .collect();
                        let dest = Location::Local(dir.join(&stem));
                        if let Err(e) = std::fs::create_dir_all(dir.join(&stem)) {
                            self.toast(
                                tf!(
                                    "Zielordner konnte nicht angelegt werden: {}",
                                    "The destination folder could not be created: {}",
                                    io_msg(&e)
                                ),
                                true,
                            );
                            return;
                        }
                        self.jobs.start(Request::Transfer { sources, dest, mv: false }, &self.bus);
                    }
                    Err(e) => self.toast(e, true),
                }
            }
            Cmd::CalcSizes | Cmd::CalcAllSizes => {
                let tab = self.panes[a].tab_mut();
                let chosen: Vec<String> = tab.selection().into_iter().filter(|&i| tab.entries[i].is_dir).map(|i| tab.entries[i].name.clone()).collect();
                let names = if cmd == Cmd::CalcAllSizes || chosen.is_empty() {
                    tab.entries.iter().filter(|e| e.is_dir && !e.is_parent).map(|e| e.name.clone()).collect()
                } else {
                    chosen
                };
                tab.calc_sizes(names, &env!(self));
            }
            Cmd::ImportQuickAccess => {
                let bus = self.bus.clone();
                std::thread::spawn(move || bus.send(Msg::QuickAccess(crate::netenum::quick_access())));
            }
            Cmd::BinRestore => {
                let tab = self.panes[a].tab();
                let ids: Vec<std::ffi::OsString> = tab.selected_entries().iter().filter_map(crate::bin::id_of).collect();
                if ids.is_empty() || tab.loc != Location::RecycleBin {
                    return;
                }
                let bus = self.bus.clone();
                std::thread::spawn(move || {
                    match crate::bin::restore(&ids) {
                        Ok(1) => bus.send(Msg::Toast { text: t("1 Element wiederhergestellt", "1 item restored").to_owned(), error: false }),
                        Ok(count) => bus.send(Msg::Toast {
                            text: tf!("{count} Elemente wiederhergestellt", "{count} items restored"),
                            error: false,
                        }),
                        Err(why) => bus.error(why),
                    }
                    bus.send(Msg::Refresh { loc: Location::RecycleBin, focus: None });
                });
            }
            Cmd::BinEmpty => self.dialog = Some(Dialog::Bin { names: Vec::new(), ids: Vec::new(), all: true }),
            Cmd::CloudKeep | Cmd::CloudFree => {
                let keep = cmd == Cmd::CloudKeep;
                let tab = self.panes[a].tab();
                let paths: Vec<PathBuf> =
                    tab.sources().iter().filter_map(|s| s.dir.local_path().map(|dir| dir.join(&s.name))).collect();
                if paths.is_empty() || !tab.in_cloud {
                    return;
                }
                let (bus, loc) = (self.bus.clone(), tab.loc.clone());
                let text = if keep {
                    t("Wird auf diesen PC geladen …", "Downloading to this PC …")
                } else {
                    t("Speicher wird freigegeben …", "Freeing up space …")
                };
                self.toast(text, false);
                std::thread::spawn(move || {
                    let failed: usize = paths.iter().map(|path| win::set_cloud_keep_tree(path, keep)).sum();
                    if failed > 0 {
                        bus.error(tf!("{failed} Einträge ließen sich nicht umstellen.", "{failed} items could not be changed."));
                    }
                    bus.send(Msg::Refresh { loc, focus: None });
                });
            }
            Cmd::SelectAll => self.panes[a].tab_mut().mark_all(),
            Cmd::InvertSelection => self.panes[a].tab_mut().invert_marks(),
            Cmd::AddFavorite | Cmd::FavoriteSelected => {
                let loc = if cmd == Cmd::FavoriteSelected {
                    self.current_dir_target()
                } else {
                    Some(self.panes[a].tab().loc.clone())
                };
                let Some(loc) = loc else { return };
                if self.cfg.favorites.iter().any(|f| f.loc == loc) {
                    self.toast(t("Ist bereits ein Favorit.", "Already a favorite."), false);
                } else {
                    let name = loc.leaf();
                    self.cfg.favorites.push(Favorite { name: name.clone(), loc });
                    self.save_config();
                    self.toast(tf!("„{name}\" zu den Favoriten hinzugefügt", "“{name}” added to favorites"), false);
                }
            }
            Cmd::ToggleGrid => {
                self.panes[a].tab_mut().toggle_grid();
                self.save_config();
            }
            Cmd::ToggleHidden => {
                self.cfg.show_hidden = !self.cfg.show_hidden;
                self.reload_where(|_| true, None);
                self.save_config();
            }
            Cmd::QuickView => self.cfg.quick_view[self.active] = !self.cfg.quick_view[self.active],
            Cmd::ToggleLayout => {
                self.cfg.stacked = !self.cfg.stacked;
                self.save_config();
            }
            Cmd::ToggleTheme => {
                self.set_theme(self.mode.toggled(), ctx);
                self.save_config();
            }
            Cmd::ToggleLang => {
                let next = if crate::lang::english() { crate::lang::Lang::De } else { crate::lang::Lang::En };
                self.set_lang(next);
                self.save_config();
            }
            Cmd::FullVersion => self.dialog = Some(Dialog::FullVersion),
            Cmd::SwapPanes => {
                self.panes.swap(0, 1);
                self.active = o;
            }
            Cmd::SameFolder => {
                let loc = self.panes[a].tab().loc.clone();
                self.panes[o].tab_mut().navigate(loc, &mut env!(self));
            }
            Cmd::Undo => match self.undo_stack.pop() {
                Some(UndoEntry::Rename { loc, pairs }) => {
                    let back: Vec<(String, String)> = pairs.into_iter().map(|(old, new)| (new, old)).collect();
                    self.spawn_rename(loc, back, true);
                }
                // Dateioperationen erst nach einer Rückfrage: dabei werden wieder Dateien bewegt oder entfernt.
                Some(UndoEntry::Ops { title, records }) => self.dialog = Some(Dialog::Undo { title, records }),
                None => self.toast(t("Es gibt nichts, das sich zurücknehmen lässt.", "There is nothing to undo."), false),
            },
            Cmd::FocusFilter => self.panes[a].focus_filter = true,
            Cmd::Asking => self.dialog = Some(Dialog::Asking),
            Cmd::EditPath => self.panes[a].edit_path(),
            Cmd::NewTab => {
                let loc = self.panes[a].tab().loc.clone();
                self.panes[a].add_tab(loc, &mut env!(self));
            }
            Cmd::CloseTab => {
                let index = self.panes[a].active;
                self.panes[a].close_tab(index);
            }
            Cmd::Back => self.panes[a].tab_mut().go_back(&mut env!(self)),
            Cmd::Forward => self.panes[a].tab_mut().go_forward(&mut env!(self)),
            Cmd::Up => self.panes[a].tab_mut().go_up(&mut env!(self)),
            Cmd::Shortcuts => self.dialog = Some(Dialog::Shortcuts),
            Cmd::BackupSave => self.dialog = Some(Dialog::BackupExport),
            Cmd::BackupRestore => {
                let (owner, bus, dir) = (self.hwnd, self.bus.clone(), self.documents_dir());
                std::thread::spawn(move || {
                    let picked = win::open_dialog(
                        owner,
                        t("Sicherung wiederherstellen", "Restore backup"),
                        t("TurboCommander-Sicherung", "TurboCommander backup"),
                        backup::EXT,
                        dir.as_deref(),
                    );
                    if let Some(path) = picked {
                        bus.send(Msg::BackupPicked(path));
                    }
                });
            }
            Cmd::GoTo(loc) => self.goto(loc),
        }
    }

    pub(crate) fn set_theme(&mut self, mode: ThemeMode, ctx: &egui::Context) {
        if mode != self.mode {
            self.mode = mode;
            self.pal = Palette::of(mode);
            theme::apply(ctx, &self.pal);
        }
    }

    /// Schaltet die Sprache um. Texte entstehen in der Sprache, die gerade gilt – was schon im Speicher
    /// liegt (Übersichten wie „Dieser PC", Typnamen), wird deshalb neu aufgebaut.
    pub(crate) fn set_lang(&mut self, lang: crate::lang::Lang) {
        // Favoriten, die noch den Standardnamen ihres Benutzerordners tragen, wechseln die Sprache mit. Ein selbst
        // vergebener Name bleibt – auch wenn er zufällig so lautet wie ein anderer dieser Ordner.
        let before: Vec<(&str, String, Location)> =
            self.folders.iter().map(|(key, path)| (*key, win::folder_label(key).to_owned(), Location::Local(path.clone()))).collect();
        crate::lang::set_lang(lang);
        for fav in &mut self.cfg.favorites {
            if let Some((key, _, _)) = before.iter().find(|(_, label, loc)| *label == fav.name && *loc == fav.loc) {
                fav.name = win::folder_label(key).to_owned();
            }
        }
        self.cfg.lang = Some(lang);
        self.types.clear();
        self.reload_where(|loc| loc.is_virtual(), None);
    }

    fn documents_dir(&self) -> Option<PathBuf> {
        self.folders.iter().find(|(name, _)| *name == "Dokumente").map(|(_, path)| path.clone())
    }

    /// Fragt nach dem Speicherort und schreibt die Sicherung (im Hintergrund, der Dialog blockiert sonst).
    pub(crate) fn start_backup(&mut self) {
        self.save_config();
        let (cfg, owner, bus, dir) = (self.cfg.clone(), self.hwnd, self.bus.clone(), self.documents_dir());
        let name = tf!(
            "TurboCommander-Sicherung-{}.{}",
            "TurboCommander-Backup-{}.{}",
            chrono::Local::now().format("%Y-%m-%d"),
            backup::EXT
        );
        std::thread::spawn(move || {
            let Some(path) = win::save_dialog(
                owner,
                t("Einstellungen sichern", "Back up settings"),
                &name,
                t("TurboCommander-Sicherung", "TurboCommander backup"),
                backup::EXT,
                dir.as_deref(),
            ) else {
                return;
            };
            let written = backup::create(&cfg).and_then(|text| std::fs::write(&path, text).map_err(|e| io_msg(&e)));
            match written {
                Ok(()) => bus.send(Msg::Toast {
                    text: tf!(
                        "Sicherung gespeichert: {}",
                        "Backup saved: {}",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ),
                    error: false,
                }),
                Err(e) => {
                    bus.error(tf!("Die Sicherung konnte nicht gespeichert werden: {e}", "The backup could not be saved: {e}"))
                }
            }
        });
    }

    pub(crate) fn open_backup(&mut self, path: PathBuf) {
        match backup::load(&path) {
            Ok(file) => {
                self.dialog = Some(Dialog::BackupImport { path, file: Box::new(file) });
            }
            Err(e) => self.toast(e, true),
        }
    }

    pub(crate) fn finish_restore(&mut self, file: backup::BackupFile, ctx: &egui::Context) {
        backup::apply(&mut self.cfg, &file);
        if let Some(mode) = self.cfg.theme {
            self.set_theme(mode, ctx);
        }
        self.reload_where(|_| true, None);
        self.save_config();
        self.toast(
            tf!(
                "Wiederhergestellt: {}",
                "Restored: {}",
                crate::dialogs::plural(self.cfg.favorites.len(), t("Favorit", "favorite"), t("Favoriten", "favorites"))
            ),
            false,
        );
    }

    fn spawn_rename(&mut self, loc: Location, pairs: Vec<(String, String)>, undo: bool) {
        let bus = self.bus.clone();
        std::thread::spawn(move || {
            let (done, errors) = crate::rename::execute(&loc, &pairs);
            bus.send(Msg::Renamed { loc, done, errors, undo });
        });
    }

    fn rename_one(&mut self, pane: usize, old: String, new: String) {
        if let Some(reason) = invalid_name_reason(&new) {
            self.toast(tf!("„{new}\": {reason}", "“{new}”: {reason}"), true);
            return;
        }
        let loc = self.panes[pane].tab().loc.clone();
        self.spawn_rename(loc, vec![(old, new)], false);
    }

    // ------------------------------------------------------------ Zeichnen

    fn style_window(&mut self, frame: &eframe::Frame) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if self.hwnd.is_none() {
            if let Ok(handle) = frame.window_handle() {
                if let RawWindowHandle::Win32(h) = handle.as_raw() {
                    self.hwnd = Some(h.hwnd.get());
                    // Ab jetzt nimmt das Fenster auch Links aus dem Browser und Ähnliches entgegen.
                    self.own_drop_target = crate::dropin::install(h.hwnd.get(), self.bus.clone());
                }
            }
        }
        if let (Some(hwnd), true) = (self.hwnd, self.titlebar_mode != Some(self.mode)) {
            let p = &self.pal;
            win::style_titlebar(hwnd, p.dark, Palette::rgb_of(p.bg), Palette::rgb_of(p.text), Palette::rgb_of(p.border));
            self.titlebar_mode = Some(self.mode);
        }
    }

    fn top_bar(&mut self, ui: &mut Ui, rect: Rect, actions: &mut Vec<Action>) {
        let p = self.pal.clone();
        let p = &p;
        let cy = rect.center().y + 1.0;
        let a = self.active;
        let push = |actions: &mut Vec<Action>, cmd: Cmd| actions.push(Action::Cmd(a, cmd));

        // Rechts: Umschalter
        let mut x = rect.right() - 12.0 - 30.0;
        let slot = |x: f32| Rect::from_center_size(Pos2::new(x + 15.0, cy), Vec2::splat(30.0));

        let menu = icon_button(ui, slot(x), Id::new("top-menu"), icons::LIST, p, true, false);
        egui::Popup::menu(&menu).width(280.0).show(|ui| {
            menu_begin(ui, 280.0);
            let mut item = |ui: &mut Ui, icon: &str, label: &str, key: &str, cmd: Cmd| {
                if menu_item(ui, p, icon, label, key, true) {
                    actions.push(Action::Cmd(a, cmd));
                }
            };
            item(ui, icons::FOLDER_PLUS, t("Neuer Ordner", "New folder"), "F7", Cmd::NewFolder);
            item(ui, icons::FILE_PLUS, t("Neue Datei", "New file"), &key("Umschalt+F4"), Cmd::NewFile);
            // Der Eintrag nennt, was zurückgenommen würde.
            let undo_label = match self.undo_stack.last() {
                Some(UndoEntry::Rename { .. }) => t("Umbenennen rückgängig", "Undo rename").to_owned(),
                Some(UndoEntry::Ops { records, .. }) => match records.last() {
                    Some(crate::ops::Undo::Copied { .. }) => t("Kopieren rückgängig …", "Undo copy …").to_owned(),
                    Some(crate::ops::Undo::Moved { .. }) => t("Verschieben rückgängig …", "Undo move …").to_owned(),
                    _ => t("Löschen rückgängig …", "Undo delete …").to_owned(),
                },
                None => t("Rückgängig", "Undo").to_owned(),
            };
            item(ui, icons::ARROW_U_UP_LEFT, &undo_label, &key("Strg+Z"), Cmd::Undo);
            menu_separator(ui, p);
            item(ui, icons::FILE_ZIP, t("Als ZIP packen …", "Pack as ZIP …"), "", Cmd::Pack);
            item(ui, icons::CALCULATOR, t("Alle Ordnergrößen berechnen", "Calculate all folder sizes"), "", Cmd::CalcAllSizes);
            item(ui, icons::SELECTION_ALL, t("Alles markieren", "Select all"), &key("Strg+A"), Cmd::SelectAll);
            item(ui, icons::SWAP, t("Markierung umkehren", "Invert selection"), "", Cmd::InvertSelection);
            menu_separator(ui, p);
            item(ui, icons::ARROWS_LEFT_RIGHT, t("Fenster tauschen", "Swap panes"), &key("Strg+U"), Cmd::SwapPanes);
            item(ui, icons::EQUALS, t("Anderes Fenster: gleicher Ordner", "Other pane: same folder"), "", Cmd::SameFolder);
            item(
                ui,
                icons::PUSH_PIN,
                t("Schnellzugriff des Explorers als Favoriten", "Explorer's Quick access as favorites"),
                "",
                Cmd::ImportQuickAccess,
            );
            let hidden = self.cfg.show_hidden;
            item(
                ui,
                if hidden { icons::EYE_SLASH } else { icons::EYE },
                if hidden {
                    t("Versteckte Dateien ausblenden", "Hide hidden files")
                } else {
                    t("Versteckte Dateien anzeigen", "Show hidden files")
                },
                &key("Strg+H"),
                Cmd::ToggleHidden,
            );
            item(
                ui,
                icons::SQUARES_FOUR,
                if self.panes[self.active].tab().grid {
                    t("Als Liste anzeigen", "Show as list")
                } else {
                    t("Als Miniaturen anzeigen", "Show as thumbnails")
                },
                &key("Strg+B"),
                Cmd::ToggleGrid,
            );
            menu_separator(ui, p);
            item(ui, icons::CHECK_SQUARE, t("Rückfragen …", "Confirmations …"), "", Cmd::Asking);
            menu_separator(ui, p);
            item(ui, icons::FLOPPY_DISK, t("Einstellungen sichern …", "Back up settings …"), "", Cmd::BackupSave);
            item(
                ui,
                icons::ARROW_COUNTER_CLOCKWISE,
                t("Sicherung wiederherstellen …", "Restore backup …"),
                "",
                Cmd::BackupRestore,
            );
            menu_separator(ui, p);
            item(ui, icons::KEYBOARD, t("Tastenkürzel", "Keyboard shortcuts"), "F1", Cmd::Shortcuts);
            item(ui, icons::SPARKLE, t("Was die Vollversion mehr kann …", "What the full version adds …"), "", Cmd::FullVersion);
        });
        menu.on_hover_text(t("Menü", "Menu"));
        x -= 32.0;

        let theme_icon = if p.dark { icons::SUN } else { icons::MOON };
        let tip = if p.dark { t("Helles Design", "Light theme") } else { t("Dunkles Design", "Dark theme") };
        if icon_button(ui, slot(x), Id::new("top-theme"), theme_icon, p, true, false).on_hover_text(tip).clicked() {
            push(actions, Cmd::ToggleTheme);
        }
        x -= 32.0;
        // Sprachumschalter: zeigt die eingestellte Sprache, ein Klick wechselt zur anderen.
        {
            let english = crate::lang::english();
            let r = slot(x);
            let resp = ui.interact(r, Id::new("top-lang"), Sense::click());
            if resp.is_pointer_button_down_on() {
                ui.painter().rect_filled(r, CornerRadius::same(7), p.control_active);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(7), p.control_hover);
            }
            let color = if resp.hovered() { p.text } else { p.text_dim };
            ui.painter().text(r.center(), Align2::CENTER_CENTER, if english { "EN" } else { "DE" }, theme::semibold(12.0), color);
            let tip = if english { "Language: English – switch to German (Deutsch)" } else { "Sprache: Deutsch – auf Englisch (English) umschalten" };
            if resp.on_hover_text(tip).clicked() {
                push(actions, Cmd::ToggleLang);
            }
        }
        x -= 32.0;
        let (layout_icon, tip) = if self.cfg.stacked {
            (icons::SQUARE_SPLIT_VERTICAL, t("Fenster nebeneinander (F9)", "Panes side by side (F9)"))
        } else {
            (icons::SQUARE_SPLIT_HORIZONTAL, t("Fenster übereinander (F9)", "Panes stacked (F9)"))
        };
        if icon_button(ui, slot(x), Id::new("top-layout"), layout_icon, p, true, false).on_hover_text(tip).clicked() {
            push(actions, Cmd::ToggleLayout);
        }
        x -= 32.0;
        if icon_button(ui, slot(x), Id::new("top-quick"), icons::EYE, p, true, self.cfg.quick_view[a])
            .on_hover_text(t("Schnellansicht (F3)", "Quick view (F3)"))
            .clicked()
        {
            push(actions, Cmd::QuickView);
        }
        x -= 32.0;
        let grid = self.panes[a].tab().grid;
        let tip = tf!("Miniaturansicht ({})", "Thumbnail view ({})", key("Strg+B"));
        if icon_button(ui, slot(x), Id::new("top-grid"), icons::SQUARES_FOUR, p, true, grid).on_hover_text(tip).clicked() {
            push(actions, Cmd::ToggleGrid);
        }
        let right_limit = x - 10.0;

        // Links: Favoriten
        enum FavAct {
            Open(usize),
            NewTab(usize),
            Other(usize),
            Rename(usize),
            Remove(usize),
            Left(usize),
            Right(usize),
        }
        let mut fav_act: Option<FavAct> = None;
        let painter = ui.painter_at(rect);
        let font = theme::font(12.5);
        let mut fx = rect.left() + 10.0;
        let count = self.cfg.favorites.len();
        let mut overflow_from = count;
        for (i, fav) in self.cfg.favorites.iter().enumerate() {
            let w = text_width(ui, &fav.name, font.clone()).min(150.0) + 38.0;
            if fx + w > right_limit - 66.0 {
                overflow_from = i;
                break;
            }
            let r = Rect::from_min_size(Pos2::new(fx, cy - 14.0), Vec2::new(w, 28.0));
            let resp = ui.interact(r, Id::new(("fav", i)), Sense::click());
            let fill = if resp.is_pointer_button_down_on() {
                p.control_active
            } else if resp.hovered() {
                p.control_hover
            } else {
                p.surface
            };
            painter.rect(r, CornerRadius::same(14), fill, Stroke::new(1.0, p.border), StrokeKind::Inside);
            let (glyph, color, filled) = match &fav.loc {
                Location::Zip { .. } => (icons::FILE_ZIP, p.archive, false),
                Location::Drives => (icons::DESKTOP, p.accent, false),
                Location::Network { .. } => (icons::NETWORK, p.accent, false),
                Location::RecycleBin => (icons::TRASH, p.text_dim, false),
                Location::Device { .. } => (icons::DEVICES, p.accent, false),
                Location::Local(_) => (icons::FOLDER, p.folder, true),
            };
            painter.text(
                Pos2::new(r.left() + 16.0, cy),
                Align2::CENTER_CENTER,
                glyph,
                if filled { theme::icon_fill(14.5) } else { theme::font(14.5) },
                color,
            );
            draw_text(&painter, r.left() + 27.0, cy, w - 36.0, &fav.name, font.clone(), p.text, false);
            if resp.clicked() {
                fav_act = Some(FavAct::Open(i));
            }
            if resp.middle_clicked() {
                fav_act = Some(FavAct::NewTab(i));
            }
            resp.context_menu(|ui| {
                menu_begin(ui, 236.0);
                if menu_item(ui, p, icons::TABS, t("In neuem Tab öffnen", "Open in new tab"), "", true) {
                    fav_act = Some(FavAct::NewTab(i));
                }
                if menu_item(
                    ui,
                    p,
                    icons::SQUARE_SPLIT_VERTICAL,
                    t("Im anderen Fenster öffnen", "Open in other pane"),
                    "",
                    true,
                ) {
                    fav_act = Some(FavAct::Other(i));
                }
                menu_separator(ui, p);
                if menu_item(ui, p, icons::PENCIL_SIMPLE, t("Umbenennen …", "Rename …"), "", true) {
                    fav_act = Some(FavAct::Rename(i));
                }
                if menu_item(ui, p, icons::ARROW_LEFT, t("Nach links", "Move left"), "", i > 0) {
                    fav_act = Some(FavAct::Left(i));
                }
                if menu_item(ui, p, icons::ARROW_RIGHT, t("Nach rechts", "Move right"), "", i + 1 < count) {
                    fav_act = Some(FavAct::Right(i));
                }
                menu_separator(ui, p);
                if menu_item(ui, p, icons::TRASH, t("Aus Favoriten entfernen", "Remove from favorites"), "", true) {
                    fav_act = Some(FavAct::Remove(i));
                }
            })
            .map(|_| ());
            resp.on_hover_text(fav.loc.display());
            fx += w + 6.0;
        }
        if overflow_from < count {
            let r = Rect::from_center_size(Pos2::new(fx + 14.0, cy), Vec2::splat(28.0));
            let more = icon_button(ui, r, Id::new("fav-more"), icons::DOTS_THREE, p, true, false);
            egui::Popup::menu(&more).width(240.0).show(|ui| {
                menu_begin(ui, 240.0);
                for (i, fav) in self.cfg.favorites.iter().enumerate().skip(overflow_from) {
                    if menu_item(ui, p, icons::FOLDER, &fav.name, "", true) {
                        fav_act = Some(FavAct::Open(i));
                    }
                }
            });
            fx += 32.0;
        }
        let add_r = Rect::from_center_size(Pos2::new(fx + 14.0, cy), Vec2::splat(28.0));
        let star = icon_button(ui, add_r, Id::new("fav-add"), icons::STAR, p, true, false);
        let mut import = false;
        star.context_menu(|ui| {
            menu_begin(ui, 310.0);
            let label = t("Schnellzugriff des Explorers übernehmen", "Take over Explorer's Quick access");
            if menu_item(ui, p, icons::PUSH_PIN, label, "", true) {
                import = true;
            }
        });
        let tip = t(
            "Aktuellen Ordner zu den Favoriten (Strg+D) · Rechtsklick: Schnellzugriff des Explorers übernehmen",
            "Add current folder to favorites (Ctrl+D) · right-click: take over Explorer's Quick access",
        );
        if star.on_hover_text(tip).clicked() {
            push(actions, Cmd::AddFavorite);
        }
        if import {
            push(actions, Cmd::ImportQuickAccess);
        }

        match fav_act {
            Some(FavAct::Open(i)) => push(actions, Cmd::GoTo(self.cfg.favorites[i].loc.clone())),
            Some(FavAct::NewTab(i)) => {
                let loc = self.cfg.favorites[i].loc.clone();
                self.panes[a].add_tab(loc, &mut env!(self));
            }
            Some(FavAct::Other(i)) => {
                let loc = self.cfg.favorites[i].loc.clone();
                let o = self.other();
                self.panes[o].tab_mut().navigate(loc, &mut env!(self));
            }
            Some(FavAct::Rename(i)) => {
                self.dialog = Some(Dialog::Input {
                    kind: InputKind::Favorite(i),
                    title: t("Favorit umbenennen", "Rename favorite"),
                    value: self.cfg.favorites[i].name.clone(),
                    fresh: true,
                });
            }
            Some(FavAct::Remove(i)) => {
                self.cfg.favorites.remove(i);
                self.save_config();
            }
            Some(FavAct::Left(i)) => {
                self.cfg.favorites.swap(i, i - 1);
                self.save_config();
            }
            Some(FavAct::Right(i)) => {
                self.cfg.favorites.swap(i, i + 1);
                self.save_config();
            }
            None => {}
        }
    }

    fn panes_ui(&mut self, ui: &mut Ui, rect: Rect, actions: &mut Vec<Action>, interactive: bool) {
        // Zählt die Bilder der Oberfläche: daran merken die Miniatur-Arbeiter, welche Kacheln noch zu sehen sind.
        self.thumbs.tick();
        let p = self.pal.clone();
        let stacked = self.cfg.stacked;
        let total = if stacked { rect.height() } else { rect.width() } - GAP;
        let first = (total * self.cfg.split).round();
        let (ra, rb, split_r) = if stacked {
            let a = Rect::from_min_size(rect.min, Vec2::new(rect.width(), first));
            let b = Rect::from_min_max(Pos2::new(rect.left(), a.bottom() + GAP), rect.max);
            let s = Rect::from_min_max(a.left_bottom(), b.right_top());
            (a, b, s)
        } else {
            let a = Rect::from_min_size(rect.min, Vec2::new(first, rect.height()));
            let b = Rect::from_min_max(Pos2::new(a.right() + GAP, rect.top()), rect.max);
            let s = Rect::from_min_max(a.right_top(), b.left_bottom());
            (a, b, s)
        };
        self.pane_rects = [ra, rb];

        // Trenner zum Ziehen
        let split = ui.interact(split_r, Id::new("pane-split"), Sense::click_and_drag());
        if split.hovered() || split.dragged() {
            ui.ctx().set_cursor_icon(if stacked { CursorIcon::ResizeVertical } else { CursorIcon::ResizeHorizontal });
            let grip = if stacked { Vec2::new(44.0, 3.0) } else { Vec2::new(3.0, 44.0) };
            ui.painter().rect_filled(Rect::from_center_size(split_r.center(), grip), CornerRadius::same(2), p.accent);
        }
        if split.dragged() {
            let delta = if stacked { split.drag_delta().y } else { split.drag_delta().x };
            if total > 100.0 {
                self.cfg.split = (self.cfg.split + delta / total).clamp(0.15, 0.85);
            }
        }
        if split.double_clicked() {
            self.cfg.split = 0.5;
        }

        for i in 0..2 {
            if self.cfg.quick_view[i] {
                let tab = self.panes[i].tab();
                let dir = tab.current().map(|e| tab.entry_dir(e));
                self.preview[i].set_target(dir.as_ref().unwrap_or(&tab.loc), tab.current(), &self.bus);
            } else {
                self.preview[i].close();
            }
        }
        let today = today();
        let can_paste = self.clip_internal.is_some() || win::clipboard_has_files();
        for (i, r) in [ra, rb].into_iter().enumerate() {
            let mut child = ui.new_child(UiBuilder::new().max_rect(r).id_salt(("pane", i)));
            child.set_clip_rect(r.intersect(ui.clip_rect()));
            let [first, second] = &mut self.preview;
            let preview = if i == 0 { first } else { second };
            let mut cx = PaneCx {
                preview: self.cfg.quick_view[i].then_some(preview),
                icons: &mut self.shell_icons,
                thumbs: &mut self.thumbs,
                p: &p,
                env: env!(self),
                index: i,
                active: i == self.active,
                interactive,
                actions,
                drives: &self.drives,
                folders: &self.folders,
                places: &self.places,
                devices: &self.devices,
                types: &mut self.types,
                today,
                can_paste,
            };
            self.panes[i].ui(&mut child, r, &mut cx);
        }
    }

    fn bottom_bar(&mut self, ui: &mut Ui, rect: Rect, actions: &mut Vec<Action>) {
        let p = self.pal.clone();
        let p = &p;
        let cy = rect.center().y - 1.0;
        let painter = ui.painter_at(rect);
        let a = self.active;

        // Laufende Aufgaben rechts
        let mut right = rect.right() - 12.0;
        let running: Vec<_> = self.jobs.running().collect();
        if let Some(job) = running.first() {
            let w = (rect.width() * 0.36).clamp(190.0, 380.0);
            let r = Rect::from_min_max(Pos2::new(right - w, cy - 15.0), Pos2::new(right, cy + 15.0));
            painter.rect(r, CornerRadius::same(8), p.surface, Stroke::new(1.0, p.border), StrokeKind::Inside);
            let sh = &job.shared;
            let scanning = sh.scanning.load(Relaxed);
            let fraction = sh.fraction();
            let cancel_r = Rect::from_center_size(Pos2::new(r.right() - 15.0, cy), Vec2::splat(22.0));
            let cancel = icon_button(ui, cancel_r, Id::new(("job-cancel", job.id)), icons::X, p, true, false);
            if cancel.on_hover_text(t("Abbrechen", "Cancel")).clicked() {
                sh.cancel.store(true, Relaxed);
            }
            let pct = if scanning { "…".to_owned() } else { format!("{:.0} %", fraction * 100.0) };
            let pct_w = text_width(ui, &pct, theme::semibold(11.5)) + 6.0;
            let mut title = job.title.clone();
            if running.len() > 1 {
                title.push_str(&format!("  (+{})", running.len() - 1));
            }
            draw_text(&painter, r.left() + 12.0, cy - 2.0, r.width() - pct_w - 48.0, &title, theme::font(12.0), p.text, false);
            painter.text(Pos2::new(r.right() - 30.0, cy - 2.0), Align2::RIGHT_CENTER, pct, theme::semibold(11.5), p.accent);
            let bar = Rect::from_min_max(Pos2::new(r.left() + 12.0, r.bottom() - 6.0), Pos2::new(r.right() - 32.0, r.bottom() - 3.0));
            if scanning {
                // Hin- und herlaufender Abschnitt, solange die Gesamtmenge unbekannt ist
                let t = (ui.input(|i| i.time) * 0.9).fract() as f32;
                painter.rect_filled(bar, CornerRadius::same(2), p.control_active);
                let seg = bar.width() * 0.28;
                let sx = bar.left() + (bar.width() - seg) * (1.0 - (t * 2.0 - 1.0).abs());
                painter.rect_filled(Rect::from_min_size(Pos2::new(sx, bar.top()), Vec2::new(seg, bar.height())), CornerRadius::same(2), p.accent);
            } else {
                progress_bar(&painter, bar, fraction, p.control_active, p.accent);
            }
            let detail = sh.current.lock().unwrap().clone();
            let done = sh.done_bytes.load(Relaxed);
            let all = sh.total_bytes.load(Relaxed);
            ui.interact(r.with_max_x(cancel_r.left()), Id::new(("job-info", job.id)), Sense::hover()).on_hover_text(tf!(
                "{detail}\n{} von {}",
                "{detail}\n{} of {}",
                fmt_size(done),
                fmt_size(all)
            ));
            right = r.left() - 10.0;
        }

        // Funktionstasten links
        let overview = self.panes[a].tab().loc.is_virtual();
        let selection = !overview && !self.panes[a].tab().selection().is_empty();
        let writable = self.panes[a].tab().loc.writable();
        let removable = self.panes[a].tab().loc.removable();
        let can_create = writable;
        // Taste, Beschriftung, Symbol, Befehl, verfügbar, Hinweis
        let items: Vec<(String, String, &str, Cmd, bool, String)> = [
            ("F2", t("Umbenennen", "Rename"), icons::PENCIL_SIMPLE, Cmd::Rename, selection && writable, t("Umbenennen", "Rename")),
            ("F3", t("Ansicht", "View"), icons::EYE, Cmd::QuickView, true, t("Schnellansicht ein/aus", "Quick view on/off")),
            ("F5", t("Kopieren", "Copy"), icons::COPY, Cmd::Copy, selection, t("Ins andere Fenster kopieren", "Copy to other pane")),
            (
                "F6",
                t("Verschieben", "Move"),
                icons::ARROW_RIGHT,
                Cmd::Move,
                selection && removable,
                t("Ins andere Fenster verschieben", "Move to other pane"),
            ),
            ("F7", t("Ordner", "Folder"), icons::FOLDER_PLUS, Cmd::NewFolder, can_create, t("Neuer Ordner", "New folder")),
            (
                "F8",
                t("Löschen", "Delete"),
                icons::TRASH,
                Cmd::Delete,
                selection && (removable || self.panes[a].tab().loc == Location::RecycleBin),
                t("Löschen (Umschalt: endgültig)", "Delete (Shift: permanently)"),
            ),
        ]
        .into_iter()
        .map(|(key, label, icon, cmd, enabled, tip)| (key.to_owned(), label.to_owned(), icon, cmd, enabled, tip.to_owned()))
        .collect();
        // Die festen Knöpfe entscheiden, ob Platz für Beschriftungen ist.
        let fixed = items.len();
        let key_font = theme::semibold(10.5);
        let label_font = theme::font(12.5);
        let avail = right - rect.left() - 12.0;
        let key_w: Vec<f32> = items.iter().map(|it| text_width(ui, &it.0, key_font.clone()) + 12.0).collect();
        let label_w: Vec<f32> = items.iter().map(|it| text_width(ui, &it.1, label_font.clone())).collect();
        let full: f32 = key_w.iter().zip(&label_w).take(fixed).map(|(k, l)| k + l + 26.0).sum::<f32>() + 6.0 * 6.0;
        let compact = full > avail;
        let mut x = rect.left() + 12.0;
        for (i, (key, label, icon, cmd, enabled, tip)) in items.into_iter().enumerate() {
            let (key, label) = (key.as_str(), label.as_str());
            let w = if compact { key_w[i] + 38.0 } else { key_w[i] + label_w[i] + 26.0 };
            if x + w > right {
                break;
            }
            let r = Rect::from_min_size(Pos2::new(x, cy - 15.0), Vec2::new(w, 30.0));
            let resp = ui.interact(r, Id::new(("fn", key)), if enabled { Sense::click() } else { Sense::hover() });
            let fill = if !enabled {
                p.bg
            } else if resp.is_pointer_button_down_on() {
                p.control_active
            } else if resp.hovered() {
                p.control_hover
            } else {
                p.surface
            };
            let stroke = if enabled { p.border } else { p.border_soft };
            painter.rect(r, CornerRadius::same(8), fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
            let badge = Rect::from_min_size(Pos2::new(r.left() + 6.0, cy - 9.0), Vec2::new(key_w[i], 18.0));
            painter.rect_filled(badge, CornerRadius::same(5), if enabled { p.control } else { p.border_soft });
            painter.text(badge.center(), Align2::CENTER_CENTER, key, key_font.clone(), if enabled { p.text_dim } else { p.text_faint });
            let color = if enabled { p.text } else { p.text_faint };
            if compact {
                painter.text(Pos2::new(badge.right() + 16.0, cy), Align2::CENTER_CENTER, icon, theme::font(15.0), color);
            } else {
                painter.text(Pos2::new(badge.right() + 8.0, cy), Align2::LEFT_CENTER, label, label_font.clone(), color);
            }
            if enabled && resp.clicked() {
                actions.push(Action::Cmd(a, cmd));
            }
            resp.on_hover_text(tip);
            x += w + 6.0;
        }
    }

    fn drag_badge(&self, ctx: &egui::Context) {
        let Some(payload) = DragAndDrop::payload::<DragPayload>(ctx) else { return };
        let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) else { return };
        let mods = ctx.input(|i| i.modifiers);
        let p = &self.pal;
        let n = payload.sources.len();
        let count = if n == 1 { payload.sources[0].name.clone() } else { tf!("{n} Elemente", "{n} items") };
        let target = (0..2).find(|&i| self.pane_rects[i].contains(pos)).map(|i| self.panes[i].tab().loc.clone());
        let text = match target {
            Some(dest) if dest.writable() => {
                format!(
                    "{count}  ·  {}",
                    if drop_moves(&payload.sources, &dest, mods) { t("verschieben", "move") } else { t("kopieren", "copy") }
                )
            }
            _ => count,
        };
        let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("drag-badge")));
        let galley = painter.layout_no_wrap(text, theme::font(12.5), p.on_accent);
        let r = Rect::from_min_size(pos + Vec2::new(14.0, 18.0), galley.size() + Vec2::new(20.0, 10.0));
        painter.rect_filled(r, CornerRadius::same(8), p.accent);
        painter.galley(r.min + Vec2::new(10.0, 5.0), galley, p.on_accent);
        ctx.set_cursor_icon(CursorIcon::Grabbing);
    }

    fn toasts_ui(&mut self, ctx: &egui::Context, full: Rect) {
        self.toasts.retain(|t| t.born.elapsed() < Duration::from_millis(if t.error { 7000 } else { 3200 }));
        if self.toasts.is_empty() {
            return;
        }
        let p = &self.pal;
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("toasts")));
        let mut y = full.bottom() - BOTTOM_H - 10.0;
        for t in self.toasts.iter().rev().take(4) {
            let galley = painter.layout(t.text.clone(), theme::font(13.0), p.text, (full.width() - 120.0).clamp(120.0, 460.0));
            let size = galley.size() + Vec2::new(50.0, 22.0);
            let r = Rect::from_min_size(Pos2::new(full.right() - 16.0 - size.x, y - size.y), size);
            let shadow = egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: egui::Color32::from_black_alpha(if p.dark { 120 } else { 40 }) };
            painter.add(shadow.as_shape(r, CornerRadius::same(10)));
            painter.rect(r, CornerRadius::same(10), p.elevated, Stroke::new(1.0, p.border), StrokeKind::Inside);
            let (glyph, color) = if t.error { (icons::WARNING_CIRCLE, p.danger) } else { (icons::CHECK_CIRCLE, p.success) };
            painter.text(Pos2::new(r.left() + 20.0, r.top() + 11.0 + 9.0), Align2::CENTER_CENTER, glyph, theme::font(17.0), color);
            painter.galley(r.min + Vec2::new(38.0, 11.0), galley, p.text);
            y -= size.y + 8.0;
        }
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    /// Zieht der Nutzer lokale Dateien aus dem Fenster hinaus, übernimmt Windows das Ziehen –
    /// so lassen sie sich in andere Programme (Explorer, Editoren, Konverter …) fallen lassen.
    fn drag_out_check(&mut self, ctx: &egui::Context) {
        let (Some(hwnd), None) = (self.hwnd, &self.args.shot) else { return };
        let Some(payload) = DragAndDrop::payload::<DragPayload>(ctx) else { return };
        // ZIP-Einträge existieren nicht als Dateien auf der Platte und bleiben im Programm.
        let paths: Option<Vec<PathBuf>> =
            payload.sources.iter().map(|s| s.dir.local_path().map(|dir| dir.join(&s.name))).collect();
        let Some(paths) = paths else { return };
        if !win::cursor_over_foreign_window(hwnd) {
            return;
        }
        DragAndDrop::clear_payload(ctx);
        ctx.stop_dragging();
        // Blockiert, bis losgelassen wurde; solange zeichnet Windows den Mauszeiger samt Dateisymbol.
        crate::dragout::drag_out(hwnd, &paths);
        // Das Loslassen hat Windows abgefangen – dem eigenen Fenster nachreichen, sonst „klebt" die Maustaste.
        win::post_left_button_up(hwnd);
        self.inject_release = true;
        self.own_drag = Some((payload.sources.clone(), Instant::now()));
        // Das Ziel kann die Dateien verschoben haben.
        let dirs: Vec<Location> = payload.sources.iter().map(|s| s.dir.clone()).collect();
        self.reload_where(|loc| dirs.contains(loc), None);
        ctx.request_repaint();
    }

    /// Ohne eigenes Ablageziel: abgelegte Dateien, wie die Fensterbibliothek sie meldet.
    fn dropped_files(&mut self, ctx: &egui::Context) {
        if self.own_drop_target {
            return;
        }
        let paths: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if paths.is_empty() {
            return;
        }
        // Während eines von Windows geführten Ziehens kennt egui die Mausposition nicht – Windows fragen.
        let pos = self.hwnd.and_then(win::cursor_in_window);
        let mods = ctx.input(|i| i.modifiers);
        self.drop_paths(ctx, paths, pos, mods, false);
    }

    /// Dateien, die von außen im Fenster abgelegt wurden: in den Ordner unter der Maus oder den des Fensters.
    /// `temp`: sie liegen in einem Zwischenordner (Link aus dem Browser, Anhang) und werden von dort verschoben.
    fn drop_paths(&mut self, ctx: &egui::Context, paths: Vec<PathBuf>, pos: Option<(f32, f32)>, mods: Modifiers, temp: bool) {
        let ppp = ctx.pixels_per_point();
        let pos = pos.map(|(x, y)| Pos2::new(x / ppp, y / ppp));
        let target = pos.and_then(|pos| (0..2).find(|&i| self.pane_rects[i].contains(pos))).unwrap_or(self.active);
        let dest = match pos {
            Some(pos) => self.panes[target].drop_location_at(pos),
            None => self.panes[target].tab().loc.clone(),
        };
        if !dest.writable() {
            self.toast(t("Hier kann nichts abgelegt werden.", "Nothing can be dropped here."), true);
            return;
        }
        // Eigene Dateien, die hinaus- und wieder hereingezogen wurden: wie Ziehen innerhalb des Programms behandeln.
        let own = self.own_drag.take().filter(|(sources, at)| {
            !temp
                && at.elapsed() < Duration::from_secs(5)
                && sources.len() == paths.len()
                && sources.iter().all(|s| s.dir.local_path().is_some_and(|d| paths.contains(&d.join(&s.name))))
        });
        let (sources, mv) = match own {
            Some((sources, _)) => {
                if sources.iter().all(|s| s.dir == dest) {
                    return; // wieder dort losgelassen, wo sie herkamen
                }
                let mv = drop_moves(&sources, &dest, mods);
                (sources, mv)
            }
            // Aus dem Zwischenordner wird verschoben, damit dort nichts liegen bleibt.
            None => (sources_from_paths(&paths), temp || mods.shift),
        };
        if !sources.is_empty() {
            self.active = target;
            self.jobs.start(Request::Transfer { sources, dest, mv }, &self.bus);
        }
    }

    // ------------------------------------------------------------ Bildschirmfoto (Entwicklung)

    fn shot_step(&mut self, ctx: &egui::Context) {
        let Some(path) = self.args.shot.clone() else { return };
        // Der Prüflauf zeichnet ständig neu, damit das Skript vorankommt – außer während „idle": dann weckt ihn,
        // wie im normalen Betrieb, nur ein Ereignis (und am Ende die Uhr).
        match self.script_until.filter(|_| self.script_idle).and_then(|until| until.checked_duration_since(Instant::now())) {
            Some(rest) => ctx.request_repaint_after(rest),
            None => ctx.request_repaint(),
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = image {
            let _ = image::save_buffer(&path, img.as_raw(), img.width() as u32, img.height() as u32, image::ColorType::Rgba8);
            let recent = &self.frame_times[self.frame_times.len().saturating_sub(40)..];
            if !recent.is_empty() {
                let avg = recent.iter().sum::<Duration>().as_secs_f64() * 1000.0 / recent.len() as f64;
                let max = recent.iter().max().map_or(0.0, |d| d.as_secs_f64() * 1000.0);
                eprintln!(
                    "Frame: Ø {avg:.2} ms, max {max:.2} ms; Start bis erstes Bild: {:?}; Speicher {:.0} MB",
                    self.first_frame,
                    win::private_memory_mb()
                );
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let loaded = self.panes.iter().all(|p| !p.tab().loading);
        match self.demo_at {
            None => {
                if loaded && self.started.elapsed() > Duration::from_millis(700) {
                    self.demo(ctx);
                    self.demo_at = Some((self.frames, Instant::now()));
                    self.script = self.args.script.as_deref().map(script::parse).unwrap_or_default();
                    self.script_last = Instant::now();
                }
            }
            Some((frame, at)) => {
                let script_done = self.script.is_empty() && self.script_wait == 0 && self.script_last.elapsed() > Duration::from_millis(700);
                if !self.shot_sent && script_done && self.frames > frame + 8 && at.elapsed() > Duration::from_millis(900) {
                    self.shot_sent = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                }
            }
        }
    }

    fn demo(&mut self, ctx: &egui::Context) {
        let Some(demo) = self.args.demo.clone() else { return };
        for part in demo.split(',') {
            match part.trim() {
                "marks" => {
                    let tab = self.panes[self.active].tab_mut();
                    let n = tab.rows.len();
                    for row in (2..n.min(9)).step_by(2) {
                        let i = tab.rows[row] as usize;
                        if !tab.entries[i].is_parent && !tab.marked[i] {
                            tab.marked[i] = true;
                            tab.marked_count += 1;
                        }
                    }
                    tab.cursor = 3.min(n.saturating_sub(1));
                }
                "quick" => self.cfg.quick_view[self.active] = true,
                // Für Prüfläufe: ohne die Überwachung der Ordner – als meldete Windows keine Änderungen
                "nowatch" => self.watcher = None,
                "delete" => self.run(Cmd::Delete, ctx),
                "full" => self.run(Cmd::FullVersion, ctx),
                "copy" => self.run(Cmd::Copy, ctx),
                "keys" => self.run(Cmd::Shortcuts, ctx),
                "newfolder" => self.run(Cmd::NewFolder, ctx),
                "other" => self.active = self.other(),
                "sizes" => self.run(Cmd::CalcAllSizes, ctx),
                "toast" => self.toast(
                    t("12 Elemente umbenannt · Strg+Z macht es rückgängig", "12 items renamed · Ctrl+Z undoes it"),
                    false,
                ),
                "backup" => self.run(Cmd::BackupSave, ctx),
                other => {
                    if let Some(name) = other.strip_prefix("cursor=") {
                        let tab = self.panes[self.active].tab_mut();
                        if let Some(row) = tab.row_of_name(name) {
                            tab.cursor = row;
                            tab.reveal_cursor();
                        }
                    } else if let Some(text) = other.strip_prefix("filter=") {
                        let tab = self.panes[self.active].tab_mut();
                        tab.filter = text.to_owned();
                        tab.rebuild_rows();
                    } else if let Some(path) = other.strip_prefix("writebackup=") {
                        // Beispiel-Sicherung erzeugen
                        if let Ok(text) = backup::create(&self.cfg) {
                            let _ = std::fs::write(path, text);
                        }
                    } else if let Some(url) = other.strip_prefix("droplink=") {
                        // Wie ein aus dem Browser hereingezogener Link: erst in den Zwischenordner, dann ablegen
                        if let Some(path) = crate::dropin::link_file(&crate::dropin::spool_dir(), url, None) {
                            self.bus.send(Msg::Dropped { paths: vec![path], pos: None, shift: false, ctrl: false, temp: true });
                        }
                    } else if let Some(path) = other.strip_prefix("dropfile=") {
                        let paths = vec![PathBuf::from(path)];
                        self.bus.send(Msg::Dropped { paths, pos: None, shift: false, ctrl: false, temp: false });
                    } else if let Some(path) = other.strip_prefix("restore=") {
                        self.open_backup(PathBuf::from(path));
                    } else if let Some(text) = other.strip_prefix("search=") {
                        let tab = self.panes[self.active].tab_mut();
                        tab.filter = text.to_owned();
                        tab.start_search(&mut env!(self));
                    }
                }
            }
        }
    }
}

/// Wie im Explorer: innerhalb eines Laufwerks wird verschoben, sonst kopiert.
/// Umschalt erzwingt Verschieben, Strg erzwingt Kopieren.
fn drop_moves(sources: &[Source], dest: &Location, mods: Modifiers) -> bool {
    if mods.shift {
        return true;
    }
    if mods.ctrl || mods.command {
        return false;
    }
    match (sources.first().map(|s| &s.dir), dest) {
        (Some(Location::Local(a)), Location::Local(b)) => a.components().next() == b.components().next(),
        _ => false,
    }
}

fn sources_from_paths(paths: &[PathBuf]) -> Vec<Source> {
    paths
        .iter()
        .filter_map(|path| {
            let dir = path.parent()?.to_path_buf();
            let name = path.file_name()?.to_string_lossy().into_owned();
            let meta = std::fs::metadata(path).ok()?;
            Some(Source { dir: Location::Local(dir), name, is_dir: meta.is_dir(), size: meta.len() })
        })
        .collect()
}

/// Für automatische Prüfläufe (`--cloud ordner`): dieser Ordner gilt als Cloud-Ordner.
pub static TEST_CLOUD: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Die Cloud-Ordner dieses PCs (OneDrive, Dropbox und was Windows sonst eingehängt hat – ohne Telefone): Pfade in
/// Kleinbuchstaben mit abschließendem `\\`, zum Vergleich mit dem Ordner eines Tabs.
fn cloud_roots() -> Vec<String> {
    let clouds = win::cloud_folders().into_iter().map(|(_, path)| path).chain(TEST_CLOUD.get().cloned());
    let places = win::places().into_iter().filter(|place| !place.phone).map(|place| place.root);
    clouds
        .chain(places)
        .map(|path| {
            let mut folded = path.to_string_lossy().to_lowercase();
            if !folded.ends_with('\\') {
                folded.push('\\');
            }
            folded
        })
        .collect()
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(msg) = self.rx.try_recv() {
            self.on_msg(ctx, msg);
        }
        for i in 0..2 {
            let tab = self.panes[i].tab_mut();
            if tab.generation == 0 && !tab.loading {
                tab.request(&mut env!(self));
            } else if tab.stale && !tab.loading && tab.rename.is_none() {
                // Der Ordner hat sich geändert, während der Tab im Hintergrund lag: jetzt wird er gezeigt.
                tab.stale = false;
                tab.reload(&mut env!(self));
            }
        }
        let focused = ctx.input(|i| i.focused);
        if focused && !self.was_focused {
            self.recheck_folders();
        }
        self.was_focused = focused;
        self.sync_watcher();
        self.flush_dirty(ctx);
        if self.jobs.running().next().is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let frame_start = Instant::now();
        let ctx = ui.ctx().clone();
        self.frames += 1;
        if self.first_frame.is_none() {
            self.first_frame = Some(self.args.process_start.unwrap_or(self.started).elapsed());
            if self.args.shot.is_some() {
                eprintln!("Speicher vor dem ersten Bild: {:.0} MB", win::private_memory_mb());
            }
        } else if self.frames == 3 && self.args.shot.is_some() {
            eprintln!("Ablageziel: {}", if self.own_drop_target { "eigenes" } else { "Fensterbibliothek" });
            eprintln!("Speicher nach zwei Bildern: {:.0} MB", win::private_memory_mb());
        }
        self.style_window(frame);

        let full = ui.max_rect();
        ui.painter().rect_filled(full, 0, self.pal.bg);

        // Fenster schließen, während noch kopiert wird: erst nachfragen.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_confirmed && self.jobs.running().next().is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.dialog = Some(Dialog::Quit);
        }

        let modal = self.dialog.is_some() || self.jobs.pending_conflict().is_some();
        self.handle_keys(&ctx, modal);
        self.dropped_files(&ctx);

        let top = Rect::from_min_size(full.min, Vec2::new(full.width(), TOP_H));
        let bottom = Rect::from_min_max(Pos2::new(full.left(), full.bottom() - BOTTOM_H), full.max);
        let middle = Rect::from_min_max(Pos2::new(full.left() + GAP, top.bottom()), Pos2::new(full.right() - GAP, bottom.top()));

        let mut actions: Vec<Action> = Vec::new();
        self.top_bar(ui, top, &mut actions);
        if middle.height() > 60.0 && middle.width() > 120.0 {
            self.panes_ui(ui, middle, &mut actions, !modal);
        }
        self.bottom_bar(ui, bottom, &mut actions);
        for action in actions {
            self.apply(action, &ctx);
        }

        self.drag_out_check(&ctx);
        self.drag_badge(&ctx);
        self.dialogs_ui(&ctx);
        self.toasts_ui(&ctx, full);
        self.shot_step(&ctx);
        if self.args.shot.is_some() {
            self.frame_times.push(frame_start.elapsed());
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if std::mem::take(&mut self.inject_release) {
            // Sonst hielte die Oberfläche die Taste weiter für gedrückt, bis zum nächsten echten Klick.
            let ppp = ctx.pixels_per_point();
            let pos = self.hwnd.and_then(win::cursor_in_window).map_or(Pos2::ZERO, |(x, y)| Pos2::new(x / ppp, y / ppp));
            raw.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            });
        }
        if self.args.shot.is_some() {
            // Skript-Modus: echte Maus ausblenden, vorbereitete Eingaben einspielen.
            raw.events.retain(|e| !script::is_real_pointer_event(e));
            if self.demo_at.is_some() {
                if self.script_until.is_some_and(|until| Instant::now() < until) {
                    self.script_last = Instant::now();
                } else if self.script_wait > 0 {
                    self.script_wait -= 1;
                } else if let Some(step) = self.script.pop_front() {
                    self.script_until = (step.sleep_ms > 0).then(|| Instant::now() + Duration::from_millis(step.sleep_ms));
                    self.script_idle = step.idle;
                    self.script_focus = step.focus.or(self.script_focus);
                    raw.events.push(egui::Event::ModifiersChanged(step.modifiers));
                    raw.events.extend(step.events);
                    self.script_wait = step.wait;
                    self.script_last = Instant::now();
                }
            }
            if let Some(focused) = self.script_focus {
                raw.focused = focused;
            }
        }
        // Tab wechselt das Fenster, statt (wie in egui üblich) den Fokus über Schaltflächen zu schieben.
        if self.dialog.is_some() {
            return;
        }
        let mut pressed: Option<Modifiers> = None;
        raw.events.retain(|e| match e {
            egui::Event::Key { key: Key::Tab, pressed: down, modifiers, .. } => {
                if *down {
                    pressed = Some(*modifiers);
                }
                false
            }
            _ => true,
        });
        if let Some(mods) = pressed {
            if ctx.text_edit_focused() {
                ctx.memory_mut(|m| {
                    if let Some(id) = m.focused() {
                        m.surrender_focus(id);
                    }
                });
            } else {
                self.tab_pressed = Some(mods);
            }
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.pal.bg.to_normalized_gamma_f32()
    }

    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_config();
    }
}
