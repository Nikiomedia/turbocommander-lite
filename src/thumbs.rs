//! Miniaturen für die Rasteransicht: die Vorschaubilder, die auch der Explorer zeigt (Bilder, Videos, PDF, Office …).
//!
//! Geladen wird in Hintergrund-Threads und nur für Kacheln, die gerade zu sehen sind. Scrollt man weiter, bevor eine
//! Kachel an der Reihe war, wird sie übersprungen. Die Threads starten erst mit der ersten Anfrage.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage, TextureHandle, TextureId, Vec2};

use crate::shellicon::Thumbnail;
use crate::vfs::{Bus, Msg};

/// Ab so vielen gemerkten Miniaturen werden die lange nicht gezeigten verworfen …
const LIMIT: usize = 1200;
/// … oder sobald sie zusammen so viele Bildpunkte haben (große Miniaturen brauchen entsprechend mehr Speicher).
const PIXEL_LIMIT: u64 = 24_000_000;
const WORKERS: usize = 2;
/// Meldet Windows „wird noch erstellt", wird nach diesen Pausen (Millisekunden) erneut gefragt; in OneDrive-Ordnern
/// ist die Miniatur meist nach einer Drittelsekunde da. Danach gilt die Datei als eine ohne Miniatur – bis zum
/// nächsten „Aktualisieren".
const RETRY_MS: [u64; 9] = [200, 300, 500, 800, 1200, 2000, 3000, 5000, 8000];

/// Daran wird erkannt, dass eine Datei sich geändert hat: Änderungszeit (sekundengenau) und Größe. Die Größe gehört
/// dazu, weil eine Datei, die gerade erst geschrieben wird, sonst innerhalb derselben Sekunde als unverändert gälte.
pub type Stamp = (Option<i64>, u64);

pub struct Request {
    pub dir: PathBuf,
    pub name: String,
    stamp: Stamp,
    pixels: u32,
    /// Nummer des Bildes der Oberfläche, in dem die Kachel zuletzt zu sehen war
    wanted: Arc<AtomicU64>,
}

pub enum Loaded {
    Image(ColorImage),
    /// Windows hat für diese Datei keine Miniatur.
    Nothing,
    /// Windows erstellt die Miniatur gerade erst.
    Later,
    /// Die Kachel war nicht mehr zu sehen, als sie an der Reihe war.
    Skipped,
}

enum State {
    Pending,
    Ready(TextureHandle),
    Missing,
}

struct Entry {
    stamp: Stamp,
    state: State,
    wanted: Arc<AtomicU64>,
    /// Für diese Größe (Pixel) ist die vorhandene Miniatur geladen; 0 = noch keine
    have: u32,
    /// Diese Größe ist gerade angefordert; 0 = nichts unterwegs
    asked: u32,
    /// Bis zu dieser Größe hat Windows nichts Besseres als die vorhandene Miniatur
    settled: u32,
    /// Windows war noch nicht fertig: frühestens dann wieder fragen
    retry_at: Option<Instant>,
    /// So oft hieß es schon „wird noch erstellt"
    tries: u8,
}

impl Entry {
    fn pixel_count(&self) -> u64 {
        match &self.state {
            State::Ready(texture) => {
                let [w, h] = texture.size();
                (w * h) as u64
            }
            _ => 0,
        }
    }
}

struct Shared {
    /// Die neueste Anfrage liegt oben: das sind die Kacheln, die gerade zu sehen sind.
    stack: Mutex<Vec<Request>>,
    wake: Condvar,
    frame: AtomicU64,
}

pub struct Thumbs {
    /// Ordner → Dateiname → Stand; zwei Stufen, damit das Nachschlagen ohne neu zusammengesetzte Pfade auskommt.
    dirs: HashMap<PathBuf, HashMap<String, Entry>>,
    count: usize,
    /// Bildpunkte aller geladenen Miniaturen zusammen
    pixels: u64,
    shared: Arc<Shared>,
    started: bool,
    bus: Bus,
}

impl Thumbs {
    pub fn new(bus: Bus) -> Self {
        let shared = Shared { stack: Mutex::new(Vec::new()), wake: Condvar::new(), frame: AtomicU64::new(0) };
        Self { dirs: HashMap::new(), count: 0, pixels: 0, shared: Arc::new(shared), started: false, bus }
    }

    /// Einmal je Bild der Oberfläche: daran erkennen die Arbeiter, welche Kacheln noch zu sehen sind.
    pub fn tick(&self) {
        self.shared.frame.fetch_add(1, Relaxed);
    }

    /// Die Miniatur, sobald sie geladen ist: Textur, ihre Größe und die Größe (Pixel), für die sie geladen wurde.
    /// Beim ersten Aufruf wird sie angefordert – und noch einmal, wenn sie später größer gebraucht wird; bis die
    /// größere Fassung da ist, kommt weiter die vorhandene zurück.
    pub fn get(&mut self, dir: &Path, name: &str, stamp: Stamp, pixels: u32) -> Option<(TextureId, Vec2, u32)> {
        let frame = self.shared.frame.load(Relaxed);
        let known = self.dirs.get_mut(dir).and_then(|files| files.get_mut(name)).filter(|entry| entry.stamp == stamp);
        let (shown, wanted) = match known {
            Some(entry) => {
                entry.wanted.store(frame, Relaxed);
                let shown = match &entry.state {
                    State::Ready(texture) => Some((texture.id(), texture.size_vec2(), entry.have)),
                    _ => None,
                };
                // Reicht die vorhandene, ist schon eine passende unterwegs oder hat Windows keine (bessere): nichts zu tun.
                if entry.have >= pixels || entry.asked >= pixels || entry.settled >= pixels || matches!(entry.state, State::Missing) {
                    return shown;
                }
                // Windows war beim letzten Mal noch nicht fertig: erst nach der Pause wieder fragen.
                if let Some(due) = entry.retry_at {
                    let now = Instant::now();
                    if now < due {
                        self.bus.repaint_after(due - now);
                        return shown;
                    }
                    entry.retry_at = None;
                }
                entry.asked = pixels;
                (shown, entry.wanted.clone())
            }
            None => {
                if self.count >= LIMIT || self.pixels >= PIXEL_LIMIT {
                    self.evict(frame);
                }
                let wanted = Arc::new(AtomicU64::new(frame));
                let entry =
                    Entry { stamp, state: State::Pending, wanted: wanted.clone(), have: 0, asked: pixels, settled: 0, retry_at: None, tries: 0 };
                if let Some(old) = self.dirs.entry(dir.to_path_buf()).or_default().insert(name.to_owned(), entry) {
                    // Dieselbe Datei in einer älteren Fassung
                    self.pixels = self.pixels.saturating_sub(old.pixel_count());
                } else {
                    self.count += 1;
                }
                (None, wanted)
            }
        };
        self.start();
        let request = Request { dir: dir.to_path_buf(), name: name.to_owned(), stamp, pixels, wanted };
        self.shared.stack.lock().unwrap_or_else(|e| e.into_inner()).push(request);
        self.shared.wake.notify_one();
        shown
    }

    pub fn on_loaded(&mut self, ctx: &egui::Context, request: Request, loaded: Loaded) {
        let Some(files) = self.dirs.get_mut(&request.dir) else { return };
        let Some(entry) = files.get_mut(&request.name).filter(|entry| entry.stamp == request.stamp) else { return };
        if entry.asked == request.pixels {
            entry.asked = 0;
        }
        let before = entry.pixel_count();
        match loaded {
            // Eine Antwort auf eine ältere, kleinere Anfrage ersetzt keine größere Fassung.
            Loaded::Image(_) if request.pixels < entry.have => {}
            Loaded::Image(image) => {
                entry.state = State::Ready(ctx.load_texture("miniatur", image, egui::TextureOptions::LINEAR));
                entry.have = request.pixels;
                entry.tries = 0;
            }
            // Noch in Arbeit: nach einer Pause fragt die Kachel erneut, sofern sie dann noch zu sehen ist.
            Loaded::Later if usize::from(entry.tries) < RETRY_MS.len() => {
                let wait = Duration::from_millis(RETRY_MS[usize::from(entry.tries)]);
                entry.tries += 1;
                entry.retry_at = Some(Instant::now() + wait);
                ctx.request_repaint_after(wait);
            }
            // Eine größere Fassung gibt es nicht: die vorhandene bleibt, ohne dass immer wieder gefragt wird.
            Loaded::Nothing | Loaded::Later if matches!(entry.state, State::Ready(_)) => entry.settled = request.pixels,
            Loaded::Nothing | Loaded::Later => entry.state = State::Missing,
            // Die größere Fassung wurde übersprungen: die vorhandene bleibt, beim nächsten Zeigen wird neu gefragt.
            Loaded::Skipped if matches!(entry.state, State::Ready(_)) => {}
            // Vergessen: wird die Kachel wieder sichtbar, fragt sie neu an.
            Loaded::Skipped => {
                files.remove(&request.name);
                self.count = self.count.saturating_sub(1);
                return;
            }
        }
        self.pixels = self.pixels.saturating_sub(before) + entry.pixel_count();
    }

    /// „Aktualisieren": Dateien, für die es bisher keine Miniatur gab, werden beim nächsten Zeigen noch einmal
    /// angefragt – etwa weil die Datei beim ersten Versuch noch geschrieben oder von OneDrive hochgeladen wurde.
    pub fn retry_missing(&mut self) {
        for files in self.dirs.values_mut() {
            files.retain(|_, entry| !matches!(entry.state, State::Missing));
            for entry in files.values_mut() {
                entry.settled = 0;
            }
        }
        self.dirs.retain(|_, files| !files.is_empty());
        self.count = self.dirs.values().map(HashMap::len).sum();
    }

    /// Verwirft, was seit einer Weile nicht mehr gezeigt wurde. Reicht das bei großen Miniaturen nicht, bleibt nur,
    /// was gerade zu sehen ist.
    fn evict(&mut self, frame: u64) {
        for keep_frames in [240, 4] {
            for files in self.dirs.values_mut() {
                // Was gerade geladen wird, bleibt: die Antwort darauf kommt noch.
                files.retain(|_, entry| {
                    (matches!(entry.state, State::Pending) && entry.asked > 0) || frame.saturating_sub(entry.wanted.load(Relaxed)) < keep_frames
                });
            }
            self.dirs.retain(|_, files| !files.is_empty());
            self.count = self.dirs.values().map(HashMap::len).sum();
            self.pixels = self.dirs.values().flat_map(HashMap::values).map(Entry::pixel_count).sum();
            if self.pixels < PIXEL_LIMIT {
                break;
            }
        }
    }

    fn start(&mut self) {
        if self.started {
            return;
        }
        self.started = true;
        for number in 0..WORKERS {
            let (shared, bus) = (self.shared.clone(), self.bus.clone());
            let _ = std::thread::Builder::new().name(format!("miniaturen-{number}")).spawn(move || {
                loop {
                    let request = {
                        let mut stack = shared.stack.lock().unwrap_or_else(|e| e.into_inner());
                        loop {
                            if let Some(request) = stack.pop() {
                                break request;
                            }
                            stack = shared.wake.wait(stack).unwrap_or_else(|e| e.into_inner());
                        }
                    };
                    // Zwei Bilder Spielraum: die Kachel wird erst im Lauf eines Bildes neu gemeldet.
                    let stale = shared.frame.load(Relaxed) > request.wanted.load(Relaxed) + 2;
                    let loaded = if stale {
                        Loaded::Skipped
                    } else {
                        match crate::shellicon::thumbnail(&request.dir.join(&request.name), request.pixels) {
                            Thumbnail::Image(image) => Loaded::Image(image),
                            Thumbnail::Nothing => Loaded::Nothing,
                            Thumbnail::Later => Loaded::Later,
                        }
                    };
                    bus.send(Msg::Thumb { request, loaded });
                }
            });
        }
    }
}
