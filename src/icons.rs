//! Symbole (Phosphor) und die Zuordnung Dateityp → Symbol und Farbe.

use eframe::egui::Color32;

pub use egui_phosphor::regular::*;

use crate::theme::Palette;
use crate::vfs::Entry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cat {
    Parent,
    Folder,
    Drive,
    Network,
    Cloud,
    Phone,
    Device,
    Bin,
    Image,
    Video,
    Audio,
    Archive,
    Code,
    Web,
    Doc,
    Sheet,
    Slides,
    Pdf,
    Text,
    Exe,
    Disk,
    Font,
    Other,
}

pub fn category_of_ext(ext: &str) -> Cat {
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "ico" | "svg" | "psd" | "heic" | "avif"
        | "raw" | "cr2" | "nef" | "exr" | "hdr" | "tga" | "ai" => Cat::Image,
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "mpg" | "mpeg" | "flv" | "ts" | "m2ts" | "vob" => {
            Cat::Video
        }
        "mp3" | "wav" | "flac" | "ogg" | "m4a" | "aac" | "wma" | "opus" | "mid" | "midi" | "aiff" => Cat::Audio,
        "zip" | "rar" | "7z" | "tar" | "gz" | "bz2" | "xz" | "zst" | "cab" | "tgz" => Cat::Archive,
        "rs" | "py" | "js" | "jsx" | "tsx" | "c" | "cpp" | "h" | "hpp" | "cs" | "java" | "go" | "php" | "rb"
        | "sh" | "ps1" | "bat" | "cmd" | "json" | "xml" | "yml" | "yaml" | "toml" | "sql" | "lua" | "kt" | "swift"
        | "vue" | "svelte" | "dart" => Cat::Code,
        "html" | "htm" | "css" | "scss" | "less" | "url" => Cat::Web,
        "doc" | "docx" | "odt" | "rtf" | "pages" | "epub" => Cat::Doc,
        "xls" | "xlsx" | "ods" | "csv" | "tsv" => Cat::Sheet,
        "ppt" | "pptx" | "odp" | "key" => Cat::Slides,
        "pdf" => Cat::Pdf,
        "txt" | "md" | "log" | "ini" | "cfg" | "conf" | "nfo" | "bak" | "srt" | "vtt" => Cat::Text,
        "exe" | "msi" | "com" | "scr" | "dll" | "sys" | "lnk" | "appx" => Cat::Exe,
        "iso" | "img" | "vhd" | "vhdx" | "vmdk" | "bin" => Cat::Disk,
        "ttf" | "otf" | "woff" | "woff2" => Cat::Font,
        _ => Cat::Other,
    }
}

pub fn category(e: &Entry) -> Cat {
    if e.is_parent {
        Cat::Parent
    } else if e.is_dir {
        Cat::Folder
    } else {
        category_of_ext(e.ext())
    }
}

/// Liefert (Symbol, gefüllte Variante?).
pub fn glyph(cat: Cat) -> (&'static str, bool) {
    match cat {
        Cat::Parent => (ARROW_BEND_LEFT_UP, false),
        Cat::Folder => (FOLDER, true),
        Cat::Drive => (HARD_DRIVE, false),
        Cat::Network => (NETWORK, false),
        Cat::Cloud => (CLOUD, false),
        Cat::Phone => (DEVICE_MOBILE, false),
        Cat::Device => (DEVICES, false),
        Cat::Bin => (TRASH, false),
        Cat::Image => (FILE_IMAGE, false),
        Cat::Video => (FILE_VIDEO, false),
        Cat::Audio => (FILE_AUDIO, false),
        Cat::Archive => (FILE_ZIP, false),
        Cat::Code => (FILE_CODE, false),
        Cat::Web => (FILE_HTML, false),
        Cat::Doc => (FILE_DOC, false),
        Cat::Sheet => (FILE_XLS, false),
        Cat::Slides => (FILE_PPT, false),
        Cat::Pdf => (FILE_PDF, false),
        Cat::Text => (FILE_TEXT, false),
        Cat::Exe => (APP_WINDOW, false),
        Cat::Disk => (DISC, false),
        Cat::Font => (TEXT_AA, false),
        Cat::Other => (FILE, false),
    }
}

pub fn color(cat: Cat, p: &Palette) -> Color32 {
    match cat {
        Cat::Parent => p.text_faint,
        Cat::Folder => p.folder,
        Cat::Drive | Cat::Network | Cat::Cloud | Cat::Phone | Cat::Device => p.accent,
        Cat::Bin => p.text_dim,
        Cat::Image => p.image,
        Cat::Video => p.video,
        Cat::Audio => p.audio,
        Cat::Archive => p.archive,
        Cat::Code | Cat::Web => p.code,
        Cat::Doc => p.code,
        Cat::Sheet => p.exe,
        Cat::Slides => p.archive,
        Cat::Pdf => p.pdf,
        Cat::Exe => p.exe,
        Cat::Disk => p.doc,
        Cat::Text | Cat::Font | Cat::Other => p.doc,
    }
}

/// Kann die Schnellansicht den Typ als Bild zeigen?
pub fn is_previewable_image(ext: &str) -> bool {
    matches!(ext, "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "ico")
}
