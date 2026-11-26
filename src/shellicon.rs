//! Echte Windows-Symbole für Dateien, die ihr eigenes Symbol mitbringen (Programme, Verknüpfungen).
//!
//! Geladen wird im Hintergrund über die Shell – dieselbe Quelle wie im Explorer. Bis ein Symbol da ist,
//! zeigt die Liste das gezeichnete Ersatzsymbol; nichts wartet auf die Festplatte.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};

use eframe::egui::{self, Color32, ColorImage, TextureHandle, TextureId};
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits, GetObjectW, HBITMAP,
    HGDIOBJ,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize, IBindCtx};
use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY, SIIGBF_THUMBNAILONLY};
use windows::core::HSTRING;

use crate::vfs::{Bus, Msg};

/// Pfad und Änderungszeit: wird eine exe ersetzt, lädt ihr Symbol neu.
pub type IconKey = (PathBuf, Option<i64>);

/// Mehr Symbole werden nicht gleichzeitig gehalten (ein Symbol belegt wenige Kilobyte).
const LIMIT: usize = 4000;

enum Slot {
    Pending,
    Ready(TextureHandle),
    Missing,
}

pub struct ShellIcons {
    slots: HashMap<IconKey, Slot>,
    tx: Sender<(IconKey, u32)>,
}

/// Dateitypen, bei denen jede Datei ihr eigenes Symbol hat.
pub fn has_own_icon(ext: &str) -> bool {
    matches!(ext, "exe" | "lnk" | "url" | "ico" | "scr" | "msi" | "cpl")
}

impl ShellIcons {
    pub fn new(bus: Bus) -> Self {
        let (tx, rx) = mpsc::channel::<(IconKey, u32)>();
        let _ = std::thread::Builder::new().name("symbole".into()).spawn(move || {
            let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
            // Die neueste Anfrage zuerst: das sind die Zeilen, die gerade zu sehen sind.
            let mut stack: Vec<(IconKey, u32)> = Vec::new();
            loop {
                if stack.is_empty() {
                    match rx.recv() {
                        Ok(request) => stack.push(request),
                        Err(_) => break,
                    }
                }
                while let Ok(request) = rx.try_recv() {
                    stack.push(request);
                }
                if let Some((key, size)) = stack.pop() {
                    let image = load(&key.0, size);
                    bus.send(Msg::Icon { key, image });
                }
            }
            if com {
                unsafe { CoUninitialize() };
            }
        });
        Self { slots: HashMap::new(), tx }
    }

    /// Liefert das Symbol, sobald es geladen ist; fordert es beim ersten Aufruf an.
    pub fn get(&mut self, path: PathBuf, modified: Option<i64>, pixels: u32) -> Option<TextureId> {
        let key = (path, modified);
        match self.slots.get(&key) {
            Some(Slot::Ready(texture)) => Some(texture.id()),
            Some(_) => None,
            None => {
                if self.slots.len() >= LIMIT {
                    self.slots.clear();
                }
                self.slots.insert(key.clone(), Slot::Pending);
                let _ = self.tx.send((key, pixels));
                None
            }
        }
    }

    pub fn on_loaded(&mut self, ctx: &egui::Context, key: IconKey, image: Option<ColorImage>) {
        let slot = match image {
            Some(image) => Slot::Ready(ctx.load_texture("symbol", image, egui::TextureOptions::LINEAR)),
            None => Slot::Missing,
        };
        self.slots.insert(key, slot);
    }
}

fn load(path: &Path, size: u32) -> Option<ColorImage> {
    unsafe {
        let name = HSTRING::from(path.as_os_str());
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&name, None::<&IBindCtx>).ok()?;
        let bitmap = factory.GetImage(SIZE { cx: size as i32, cy: size as i32 }, SIIGBF_ICONONLY).ok()?;
        let image = pixels(bitmap);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        image
    }
}

pub enum Thumbnail {
    Image(ColorImage),
    /// Windows hat für diese Datei keine Miniatur.
    Nothing,
    /// Windows erstellt die Miniatur gerade erst: gleich noch einmal fragen.
    Later,
}

/// „Wird noch erstellt": so antwortet Windows beim ersten Mal für Dateien in OneDrive und anderen Cloud-Ordnern –
/// die Miniatur entsteht dann nebenher und ist nach rund einer Drittelsekunde da.
const WTS_E_EXTRACTIONPENDING: u32 = 0x8004_B205;
const WTS_E_EXTRACTIONTIMEDOUT: u32 = 0x8004_B201;
const WTS_E_SURROGATEUNAVAILABLE: u32 = 0x8004_B202;

/// Miniatur einer Datei, wie sie der Explorer zeigt (PDF, Office, PSD …).
/// Kann dauern und gehört in einen Hintergrund-Thread.
pub fn thumbnail(path: &Path, size: u32) -> Thumbnail {
    unsafe {
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let image = (|| {
            let name = HSTRING::from(path.as_os_str());
            let Ok(factory) = SHCreateItemFromParsingName::<_, _, IShellItemImageFactory>(&name, None::<&IBindCtx>) else {
                return Thumbnail::Nothing;
            };
            let bitmap = match factory.GetImage(SIZE { cx: size as i32, cy: size as i32 }, SIIGBF_THUMBNAILONLY) {
                Ok(bitmap) => bitmap,
                Err(e) if matches!(e.code().0 as u32, WTS_E_EXTRACTIONPENDING | WTS_E_EXTRACTIONTIMEDOUT | WTS_E_SURROGATEUNAVAILABLE) => {
                    return Thumbnail::Later;
                }
                Err(_) => return Thumbnail::Nothing,
            };
            let image = pixels(bitmap);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            image.map_or(Thumbnail::Nothing, Thumbnail::Image)
        })();
        if com {
            CoUninitialize();
        }
        image
    }
}

/// Wie [`thumbnail`], wartet aber bis zu drei Sekunden, wenn Windows die Miniatur gerade erst erstellt –
/// für Stellen, die ohnehin in einem eigenen Thread auf genau dieses eine Bild warten.
pub fn thumbnail_waiting(path: &Path, size: u32) -> Option<ColorImage> {
    for pause in [0, 150, 250, 400, 600, 800, 800] {
        std::thread::sleep(std::time::Duration::from_millis(pause));
        match thumbnail(path, size) {
            Thumbnail::Image(image) => return Some(image),
            Thumbnail::Nothing => return None,
            Thumbnail::Later => {}
        }
    }
    None
}

/// Liest ein 32-Bit-Bitmap der Shell (BGRA, Farben bereits mit der Deckkraft multipliziert).
unsafe fn pixels(bitmap: HBITMAP) -> Option<ColorImage> {
    unsafe {
        let mut meta = BITMAP::default();
        if GetObjectW(HGDIOBJ(bitmap.0), size_of::<BITMAP>() as i32, Some((&raw mut meta).cast())) == 0 {
            return None;
        }
        let (w, h) = (meta.bmWidth, meta.bmHeight.abs());
        if w <= 0 || h <= 0 || w > 4096 || h > 4096 {
            return None;
        }
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // von oben nach unten
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut data = vec![0u8; (w * h * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let lines = GetDIBits(dc, bitmap, 0, h as u32, Some(data.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        let _ = DeleteDC(dc);
        if lines == 0 {
            return None;
        }
        // Sehr alte Symbole haben keinen Alphakanal: dann gilt alles als deckend.
        let opaque = data.chunks_exact(4).all(|px| px[3] == 0);
        let pixels = data
            .chunks_exact(4)
            .map(|px| {
                if opaque {
                    Color32::from_rgb(px[2], px[1], px[0])
                } else {
                    Color32::from_rgba_premultiplied(px[2], px[1], px[0], px[3])
                }
            })
            .collect();
        Some(ColorImage::new([w as usize, h as usize], pixels))
    }
}
