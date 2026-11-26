//! Schnellansicht: zeigt Bilder und Texte des aktuell gewählten Eintrags.
//! Für alles andere wird – wenn Windows eine liefert – die Miniatur der Datei gezeigt (PDF, Office, PSD,
//! Standbilder von Videos, Cover von Musik …).

use std::io::Read;
use std::path::{Path, PathBuf};

use eframe::egui::{self, Align2, Color32, ColorImage, Pos2, Rect, RichText, Stroke, TextureHandle, Vec2};

use crate::icons;
use crate::lang::t;
use crate::tf;
use crate::theme::{self, Palette};
use crate::util::{fmt_size, fmt_thousands, fmt_time, today};
use crate::vfs::{Bus, Entry, Location, Msg};
use crate::widgets::draw_text;

const TEXT_LIMIT: usize = 200 * 1024;
const IMAGE_LIMIT: u64 = 120 * 1024 * 1024;
const MAX_SIDE: u32 = 2400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewKey {
    pub path: PathBuf,
    pub modified: Option<i64>,
    pub size: u64,
}

pub enum PreviewData {
    /// `original` ist die Größe der Bilddatei; (0, 0) bei einer Miniatur von Windows.
    /// `note` erklärt, wenn das Bild nur ein Ersatz ist (Standbild eines Videos).
    Image { image: ColorImage, original: (u32, u32), note: Option<&'static str> },
    Text { text: String, truncated: bool },
    Nothing(&'static str),
}

enum Content {
    Empty,
    Loading,
    Image { texture: TextureHandle, original: (u32, u32), note: Option<&'static str> },
    Text { text: String, truncated: bool },
    Info(&'static str),
}

struct Subject {
    name: String,
    is_dir: bool,
    ext: String,
    size: u64,
    modified: Option<i64>,
}

pub struct Preview {
    key: Option<PreviewKey>,
    subject: Option<Subject>,
    content: Content,
    /// Stelle, zu der gerade mit der Maus gespult wird
    scrub: Option<f64>,
    bus: Option<Bus>,
}

impl Default for Preview {
    fn default() -> Self {
        Self { key: None, subject: None, content: Content::Empty, scrub: None, bus: None }
    }
}

impl Preview {
    /// Wird in jedem Frame mit dem aktuellen Eintrag aufgerufen; lädt nur bei Wechsel neu.
    pub fn set_target(&mut self, dir: &Location, entry: Option<&Entry>, bus: &Bus) {
        let Some(entry) = entry.filter(|e| !e.is_parent) else {
            self.close();
            return;
        };
        let local = dir.local_path().map(|d| d.join(&entry.name));
        let key = PreviewKey {
            path: local.clone().unwrap_or_else(|| PathBuf::from(dir.child(&entry.name).display())),
            modified: entry.modified,
            size: entry.size,
        };
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let ext = entry.ext().to_owned();
        self.subject = Some(Subject {
            name: entry.name.clone(),
            is_dir: entry.is_dir,
            ext: ext.clone(),
            size: entry.size,
            modified: entry.modified,
        });
        self.key = Some(key.clone());
        self.scrub = None;
        self.bus = Some(bus.clone());
        if entry.is_dir {
            self.content = Content::Info(t("Ordner", "Folder"));
        } else if local.is_none() {
            self.content =
                Content::Info(t("Vorschau gibt es nur für lokale Dateien", "Preview is only available for local files"));
        } else if entry.size == 0 {
            self.content = Content::Info(t("Die Datei ist leer", "The file is empty"));
        } else {
            self.content = Content::Loading;
            let bus = bus.clone();
            std::thread::spawn(move || {
                let data = if is_media(&ext) {
                    load_media(&key.path, is_video(&ext))
                } else if icons::is_previewable_image(&ext) {
                    load_image(&key.path, key.size)
                } else {
                    load_text(&key.path)
                };
                // Was sich weder als Bild noch als Text zeigen lässt: vielleicht hat Windows eine Miniatur.
                let data = match data {
                    PreviewData::Nothing(reason) => match crate::shellicon::thumbnail_waiting(&key.path, 1024) {
                        Some(image) => PreviewData::Image { image, original: (0, 0), note: None },
                        None => PreviewData::Nothing(reason),
                    },
                    shown => shown,
                };
                bus.send(Msg::Preview { key, data });
            });
        }
    }

    /// Wartet diese Schnellansicht auf genau diese Daten?
    pub fn awaits(&self, key: &PreviewKey) -> bool {
        self.key.as_ref() == Some(key) && matches!(self.content, Content::Loading)
    }

    /// Schnellansicht ausgeschaltet oder nichts gewählt: Inhalt freigeben (beendet auch eine Wiedergabe).
    pub fn close(&mut self) {
        if self.key.is_some() || !matches!(self.content, Content::Empty) {
            *self = Self::default();
        }
    }

    pub fn on_data(&mut self, ctx: &egui::Context, key: PreviewKey, data: PreviewData) {
        if self.key.as_ref() != Some(&key) {
            return;
        }
        self.content = match data {
            PreviewData::Image { image, original, note } => Content::Image {
                texture: ctx.load_texture("schnellansicht", image, egui::TextureOptions::LINEAR),
                original,
                note,
            },
            PreviewData::Text { text, truncated } => Content::Text { text, truncated },
            PreviewData::Nothing(reason) => Content::Info(reason),
        };
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, p: &Palette) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0, p.surface);

        let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 44.0));
        painter.rect_filled(header, 0, p.surface_alt);
        painter.hline(rect.x_range(), header.bottom(), Stroke::new(1.0, p.border_soft));
        let body = Rect::from_min_max(egui::pos2(rect.left(), header.bottom() + 1.0), rect.max);

        let Some(subject) = &self.subject else {
            painter.text(
                egui::pos2(header.left() + 14.0, header.center().y),
                Align2::LEFT_CENTER,
                t("Schnellansicht", "Quick view"),
                theme::semibold(13.0),
                p.text_dim,
            );
            painter.text(
                body.center(),
                Align2::CENTER_CENTER,
                t("Kein Element gewählt", "No item selected"),
                theme::font(13.0),
                p.text_faint,
            );
            return;
        };

        let cat = if subject.is_dir { icons::Cat::Folder } else { icons::category_of_ext(&subject.ext) };
        let (glyph, filled) = icons::glyph(cat);
        let icon_font = if filled { theme::icon_fill(20.0) } else { theme::font(20.0) };
        painter.text(
            egui::pos2(header.left() + 22.0, header.center().y),
            Align2::CENTER_CENTER,
            glyph,
            icon_font,
            icons::color(cat, p),
        );
        let mut details = Vec::new();
        if !subject.is_dir {
            details.push(tf!("{} ({} Bytes)", "{} ({} bytes)", fmt_size(subject.size), fmt_thousands(subject.size)));
        }
        if let Some(m) = subject.modified {
            details.push(fmt_time(m, today()));
        }
        match &self.content {
            Content::Image { original, .. } if original.0 > 0 => details.push(format!("{} × {} px", original.0, original.1)),
            _ => {}
        }
        let text_w = header.width() - 56.0;
        draw_text(&painter, header.left() + 42.0, header.top() + 14.0, text_w, &subject.name, theme::semibold(13.0), p.text, false);
        draw_text(
            &painter,
            header.left() + 42.0,
            header.top() + 31.0,
            text_w,
            &details.join("  ·  "),
            theme::font(11.5),
            p.text_dim,
            false,
        );

        // Großes Symbol mit einer Zeile Text darunter – für alles, was sich nicht zeigen lässt.
        let notice = |text: &str| {
            painter.text(
                body.center() - Vec2::new(0.0, 22.0),
                Align2::CENTER_CENTER,
                glyph,
                if filled { theme::icon_fill(56.0) } else { theme::font(56.0) },
                theme::mix(p.surface, icons::color(cat, p), 0.55),
            );
            let galley = crate::widgets::elided(&painter, text, theme::font(13.0), p.text_faint, body.width() - 24.0);
            painter.galley(egui::pos2(body.center().x - galley.size().x / 2.0, body.center().y + 22.0), galley, p.text_faint);
        };

        match &mut self.content {
            Content::Empty => {}
            Content::Loading => {
                painter.text(
                    body.center(),
                    Align2::CENTER_CENTER,
                    t("Lade Vorschau …", "Loading preview …"),
                    theme::font(13.0),
                    p.text_faint,
                );
            }
            Content::Info(text) => notice(text),
            Content::Image { texture, note, .. } => {
                let mut area = body.shrink(12.0);
                if let Some(note) = note.filter(|_| area.height() > 80.0) {
                    area.max.y -= 22.0;
                    let galley = crate::widgets::elided(&painter, note, theme::font(12.0), p.text_faint, area.width());
                    painter.galley(egui::pos2(area.center().x - galley.size().x / 2.0, area.bottom() + 6.0), galley, p.text_faint);
                }
                let size = texture.size_vec2();
                let scale = (area.width() / size.x).min(area.height() / size.y).min(1.0);
                let shown = Rect::from_center_size(area.center(), size * scale);
                painter.image(texture.id(), shown, Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)), Color32::WHITE);
            }
            Content::Text { text, truncated } => {
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body.shrink2(Vec2::new(4.0, 2.0))).id_salt("vorschau-text"));
                child.set_clip_rect(body);
                egui::ScrollArea::both().id_salt("vorschau-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
                    ui.add_space(6.0);
                    ui.add(
                        egui::Label::new(RichText::new(text.as_str()).font(theme::mono(12.0)).color(p.text))
                            .wrap_mode(egui::TextWrapMode::Extend),
                    );
                    if *truncated {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(t("… Vorschau gekürzt (erste 200 KB)", "… Preview truncated (first 200 KB)"))
                                .small()
                                .color(p.text_faint),
                        );
                    }
                    ui.add_space(6.0);
                });
            }
        }
    }
}

pub fn is_media(ext: &str) -> bool {
    is_video(ext) || is_audio(ext)
}

pub fn is_video(ext: &str) -> bool {
    matches!(ext, "mp4" | "m4v" | "mov" | "mkv" | "avi" | "wmv" | "webm" | "mpg" | "mpeg" | "ts" | "m2ts" | "mts" | "3gp" | "asf")
}

pub fn is_audio(ext: &str) -> bool {
    matches!(ext, "mp3" | "wav" | "flac" | "m4a" | "aac" | "wma" | "ogg" | "opus" | "aiff")
}

/// Hinweis unter dem Standbild eines Videos oder dem Cover eines Musikstücks.
fn play_in_full() -> &'static str {
    t(
        "Videos und Musik abspielen: in der Vollversion von TurboCommander",
        "Playing videos and music: in the full version of TurboCommander",
    )
}

/// Lite: Videos und Musik werden nicht abgespielt. Gezeigt wird das Bild, das Windows für die Datei hat
/// (Standbild bzw. Cover), bei Videos ohne Miniatur notfalls ein Standbild von ffmpeg.
fn load_media(path: &Path, video: bool) -> PreviewData {
    let image = crate::shellicon::thumbnail_waiting(path, 1024).or_else(|| if video { ffmpeg_still(path) } else { None });
    match image {
        Some(image) => PreviewData::Image { image, original: (0, 0), note: Some(play_in_full()) },
        None => PreviewData::Nothing(play_in_full()),
    }
}

fn load_image(path: &Path, size: u64) -> PreviewData {
    if size > IMAGE_LIMIT {
        return PreviewData::Nothing(t("Das Bild ist zu groß für die Vorschau", "The image is too large to preview"));
    }
    let decoded = image::ImageReader::open(path).and_then(|r| r.with_guessed_format()).ok().and_then(|r| r.decode().ok());
    let Some(img) = decoded else {
        return PreviewData::Nothing(t("Das Bild lässt sich nicht lesen", "The image cannot be read"));
    };
    let original = (img.width(), img.height());
    let img = if original.0 > MAX_SIDE || original.1 > MAX_SIDE { img.thumbnail(MAX_SIDE, MAX_SIDE) } else { img };
    let rgba = img.to_rgba8();
    let dims = [rgba.width() as usize, rgba.height() as usize];
    PreviewData::Image { image: ColorImage::from_rgba_unmultiplied(dims, rgba.as_raw()), original, note: None }
}

/// Holt mit ffmpeg (falls installiert und im Suchpfad) ein Standbild aus einem Video.
fn ffmpeg_still(path: &Path) -> Option<ColorImage> {
    use std::os::windows::process::CommandExt;
    const NO_WINDOW: u32 = 0x0800_0000;
    // Erst eine Sekunde hinter dem Anfang (dort ist selten Schwarzbild), bei sehr kurzen Clips ganz vorn.
    for start in ["1", "0"] {
        let output = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-loglevel", "error", "-ss", start, "-i"])
            .arg(path)
            .args(["-frames:v", "1", "-vf", "scale='min(1600,iw)':-2", "-f", "image2pipe", "-vcodec", "bmp", "-"])
            .creation_flags(NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if let Ok(img) = image::load_from_memory_with_format(&output.stdout, image::ImageFormat::Bmp) {
            let rgba = img.to_rgba8();
            let dims = [rgba.width() as usize, rgba.height() as usize];
            return Some(ColorImage::from_rgba_unmultiplied(dims, rgba.as_raw()));
        }
    }
    None
}

fn load_text(path: &Path) -> PreviewData {
    let Ok(file) = std::fs::File::open(path) else {
        return PreviewData::Nothing(t("Die Datei lässt sich nicht öffnen", "The file cannot be opened"));
    };
    let mut bytes = Vec::with_capacity(TEXT_LIMIT + 1);
    if file.take(TEXT_LIMIT as u64 + 1).read_to_end(&mut bytes).is_err() {
        return PreviewData::Nothing(t("Die Datei lässt sich nicht lesen", "The file cannot be read"));
    }
    let truncated = bytes.len() > TEXT_LIMIT;
    bytes.truncate(TEXT_LIMIT);

    let text = if bytes.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        if bytes.iter().take(8192).any(|&b| b == 0) {
            return PreviewData::Nothing(t("Keine Vorschau für diesen Dateityp", "No preview for this file type"));
        }
        let body = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
        match std::str::from_utf8(body) {
            Ok(s) => s.to_owned(),
            // Abgeschnittenes UTF-8 am Ende ist kein Grund, alles als Latin-1 zu lesen.
            Err(e) if e.valid_up_to() + 4 >= body.len() => String::from_utf8_lossy(body).into_owned(),
            Err(_) => body.iter().map(|&b| b as char).collect(),
        }
    };
    PreviewData::Text { text: text.replace('\t', "    ").replace('\r', ""), truncated }
}
