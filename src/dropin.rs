//! Ablegen von außen: Dateien aus dem Explorer – und Dinge, die noch gar keine Dateien auf der Platte sind, wie ein
//! Link aus der Adressleiste des Browsers, ein Bild aus einer Webseite oder ein Anhang aus einem Mailprogramm.
//!
//! Die Fensterbibliothek nimmt nur fertige Dateien an. Deshalb meldet sich hier ein eigenes Ablageziel am Fenster
//! an. Was als „Datei ohne Platz auf der Platte" kommt, wird zuerst in einen Zwischenordner geschrieben – ein Link
//! als `.url`-Datei, wie es auch der Explorer macht – und dann wie abgelegte Dateien an das Programm gemeldet.

use std::cell::Cell;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HGLOBAL, HWND, POINTL};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, IStream, STREAM_SEEK_SET, TYMED_HGLOBAL, TYMED_ISTREAM};
use windows::Win32::System::Ole::{
    DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl, RegisterDragDrop,
    ReleaseStgMedium, RevokeDragDrop,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::core::{PCWSTR, Ref, implement, w};

use crate::vfs::{Bus, Msg};

const CF_UNICODETEXT: u16 = 13;
const CF_HDROP: u16 = 15;
/// Bausteine einer Dateibeschreibung (FILEDESCRIPTORW): Größe eines Eintrags und wo darin was steht
const DESCRIPTOR: usize = 592;
const DESCRIPTOR_ATTRIBUTES: usize = 36;
const DESCRIPTOR_SIZE_HIGH: usize = 64;
const DESCRIPTOR_NAME: usize = 72;
const FD_ATTRIBUTES: u32 = 0x4;
const FD_FILESIZE: u32 = 0x40;

/// Was über dem Fenster schwebt
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    /// Nichts, womit sich etwas anfangen ließe
    Nothing,
    /// Dateien und Ordner von der Platte
    Files,
    /// Ein Link oder Dateien, die erst beim Ablegen geliefert werden
    Virtual,
}

fn format_id(name: PCWSTR) -> u16 {
    unsafe { windows_sys::Win32::System::DataExchange::RegisterClipboardFormatW(name.0) as u16 }
}

fn descriptor_format() -> u16 {
    format_id(w!("FileGroupDescriptorW"))
}

fn contents_format() -> u16 {
    format_id(w!("FileContents"))
}

fn etc(format: u16, index: i32, tymed: u32) -> FORMATETC {
    FORMATETC { cfFormat: format, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: index, tymed }
}

fn offers(data: &IDataObject, format: u16) -> bool {
    unsafe { data.QueryGetData(&etc(format, -1, TYMED_HGLOBAL.0 as u32)).0 == 0 }
}

unsafe fn copy_global(handle: HGLOBAL) -> Option<Vec<u8>> {
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    unsafe {
        let size = GlobalSize(handle.0);
        let start = GlobalLock(handle.0) as *const u8;
        if start.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(start, size).to_vec();
        GlobalUnlock(handle.0);
        Some(bytes)
    }
}

/// Der Inhalt eines Formats, das als Speicherblock geliefert wird.
fn global(data: &IDataObject, format: u16) -> Option<Vec<u8>> {
    unsafe {
        let mut medium = data.GetData(&etc(format, -1, TYMED_HGLOBAL.0 as u32)).ok()?;
        let bytes = if medium.tymed == TYMED_HGLOBAL.0 as u32 { copy_global(medium.u.hGlobal) } else { None };
        ReleaseStgMedium(&mut medium);
        bytes
    }
}

fn wide_text(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).take_while(|unit| *unit != 0).collect();
    String::from_utf16_lossy(&units)
}

fn narrow_text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

// ---------------------------------------------------------------- Dateien, die erst beim Ablegen geliefert werden

#[derive(Debug, PartialEq)]
struct Offered {
    index: i32,
    name: String,
    size: Option<u64>,
    directory: bool,
}

/// Liest die Liste der angebotenen Dateien (FILEGROUPDESCRIPTORW).
fn offered(bytes: &[u8]) -> Vec<Offered> {
    let number = |at: usize| bytes.get(at..at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let mut out = Vec::new();
    for index in 0..number(0).min(10_000) as usize {
        let base = 4 + index * DESCRIPTOR;
        let Some(entry) = bytes.get(base..base + DESCRIPTOR) else { break };
        let flags = number(base);
        let name = wide_text(&entry[DESCRIPTOR_NAME..]);
        let size = ((number(base + DESCRIPTOR_SIZE_HIGH) as u64) << 32) | number(base + DESCRIPTOR_SIZE_HIGH + 4) as u64;
        out.push(Offered {
            index: index as i32,
            name,
            size: (flags & FD_FILESIZE != 0).then_some(size),
            directory: flags & FD_ATTRIBUTES != 0 && number(base + DESCRIPTOR_ATTRIBUTES) & 0x10 != 0,
        });
    }
    out
}

/// Ein Name, unter dem sich die Datei anlegen lässt: ohne Pfad, ohne unzulässige Zeichen, nicht zu lang.
fn safe_name(name: &str) -> String {
    let leaf = name.rsplit(['\\', '/']).next().unwrap_or(name);
    let clean: String = leaf
        .chars()
        .map(|c| if matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') || (c as u32) < 32 { '_' } else { c })
        .collect();
    let clean = clean.trim().trim_end_matches(['.', ' ']).to_owned();
    let (stem, ext) = match clean.rfind('.') {
        Some(dot) if dot > 0 && clean.len() - dot <= 12 => (&clean[..dot], &clean[dot..]),
        _ => (clean.as_str(), ""),
    };
    let stem: String = stem.chars().take(120).collect();
    let stem = stem.trim_end_matches(['.', ' ']);
    match (stem, ext) {
        // Ein Link ohne Titel
        (".url", "") | ("", ".url") => "Link.url".to_owned(),
        ("", _) => format!("Datei{ext}"),
        _ => format!("{stem}{ext}"),
    }
}

/// Ein Pfad in `dir`, den es noch nicht gibt: der Name selbst oder mit angehängter Nummer.
fn free_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    };
    (2..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|path| !path.exists()).unwrap_or(first)
}

fn copy_stream(stream: &IStream, out: &mut File) -> bool {
    let mut buffer = vec![0u8; 64 * 1024];
    unsafe {
        // Manche Quellen übergeben den Strom am Ende statt am Anfang.
        let _ = stream.Seek(0, STREAM_SEEK_SET, None);
        loop {
            let mut read = 0u32;
            if stream.Read(buffer.as_mut_ptr().cast(), buffer.len() as u32, Some(&mut read)).is_err() {
                return false;
            }
            if read == 0 {
                return true;
            }
            if out.write_all(&buffer[..read as usize]).is_err() {
                return false;
            }
        }
    }
}

/// Schreibt den Inhalt der angebotenen Datei Nummer `index` nach `out`.
fn write_contents(data: &IDataObject, item: &Offered, out: &mut File) -> bool {
    unsafe {
        let wanted = (TYMED_ISTREAM.0 | TYMED_HGLOBAL.0) as u32;
        let Ok(mut medium) = data.GetData(&etc(contents_format(), item.index, wanted)) else { return false };
        let done = if medium.tymed == TYMED_ISTREAM.0 as u32 {
            (*medium.u.pstm).as_ref().is_some_and(|stream| copy_stream(stream, out))
        } else if medium.tymed == TYMED_HGLOBAL.0 as u32 {
            match copy_global(medium.u.hGlobal) {
                Some(mut bytes) => {
                    match item.size {
                        Some(size) => bytes.truncate(size.min(bytes.len() as u64) as usize),
                        // Ohne Größenangabe: ein Speicherblock ist oft etwas größer als sein Inhalt. Bei Verknüpfungen
                        // (Text) fallen die Nullen am Ende weg; bei anderem bleibt der Block, wie er ist.
                        None if item.name.to_ascii_lowercase().ends_with(".url") => {
                            while bytes.last() == Some(&0) {
                                bytes.pop();
                            }
                        }
                        None => {}
                    }
                    out.write_all(&bytes).is_ok()
                }
                None => false,
            }
        } else {
            false
        };
        ReleaseStgMedium(&mut medium);
        done
    }
}

// ---------------------------------------------------------------- Links

fn looks_like_url(text: &str) -> bool {
    let text = text.trim();
    let Some((scheme, rest)) = text.split_once("://") else { return false };
    !scheme.is_empty()
        && scheme.len() <= 16
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && !rest.is_empty()
        && text.len() < 4000
        && !text.contains(char::is_whitespace)
}

/// Adresse und – soweit bekannt – Titel eines abgelegten Links.
fn link(data: &IDataObject) -> Option<(String, Option<String>)> {
    // Firefox: Adresse und Titel in zwei Zeilen
    if let Some(bytes) = global(data, format_id(w!("text/x-moz-url"))) {
        let text = wide_text(&bytes);
        let mut lines = text.lines();
        if let Some(url) = lines.next().map(str::trim).filter(|url| looks_like_url(url)) {
            let title = lines.next().map(str::trim).filter(|title| !title.is_empty() && *title != url);
            return Some((url.to_owned(), title.map(str::to_owned)));
        }
    }
    let url = global(data, format_id(w!("UniformResourceLocatorW")))
        .map(|bytes| wide_text(&bytes))
        .or_else(|| global(data, format_id(w!("UniformResourceLocator"))).map(|bytes| narrow_text(&bytes)))
        .or_else(|| global(data, CF_UNICODETEXT).map(|bytes| wide_text(&bytes)))?;
    let url = url.trim();
    looks_like_url(url).then(|| (url.to_owned(), None))
}

/// Name für die Verknüpfung, wenn kein Titel mitkommt: die Adresse ohne Protokoll und ohne „www.".
fn title_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest).trim_end_matches('/');
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    rest.replace('/', " - ")
}

/// Inhalt einer Internetverknüpfung (`.url`), wie Windows sie anlegt. Zeichen außerhalb von ASCII werden kodiert,
/// weil solche Dateien ohne Angabe einer Zeichenkodierung gelesen werden.
fn url_file(url: &str) -> String {
    use std::fmt::Write;
    let mut encoded = String::with_capacity(url.len());
    for byte in url.bytes() {
        if byte.is_ascii_graphic() {
            encoded.push(byte as char);
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    format!("[InternetShortcut]\r\nURL={encoded}\r\n")
}

/// Schreibt nach `dir`, was das Datenobjekt an Dateien mitbringt, die es auf der Platte noch nicht gibt, und
/// liefert ihre Pfade. Ein Link ohne mitgelieferte Datei wird selbst als `.url` angelegt.
fn spool(data: &IDataObject, dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if std::fs::create_dir_all(dir).is_err() {
        return paths;
    }
    for item in global(data, descriptor_format()).map(|bytes| offered(&bytes)).unwrap_or_default() {
        if item.directory {
            continue;
        }
        let path = free_path(dir, &safe_name(&item.name));
        let Ok(mut file) = File::create(&path) else { continue };
        if write_contents(data, &item, &mut file) {
            paths.push(path);
        } else {
            drop(file);
            let _ = std::fs::remove_file(&path);
        }
    }
    if paths.is_empty() {
        if let Some((url, title)) = link(data) {
            paths.extend(link_file(dir, &url, title.as_deref()));
        }
    }
    paths
}

/// Legt in `dir` eine Internetverknüpfung an, benannt nach dem Titel oder – ohne Titel – nach der Adresse.
pub fn link_file(dir: &Path, url: &str, title: Option<&str>) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).ok()?;
    let title = title.map_or_else(|| title_of(url), str::to_owned);
    let path = free_path(dir, &safe_name(&format!("{title}.url")));
    std::fs::write(&path, url_file(url)).ok()?;
    Some(path)
}

/// Die Dateien und Ordner eines Datenobjekts, das von der Platte kommt (CF_HDROP).
fn file_paths(data: &IDataObject) -> Vec<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::UI::Shell::DragQueryFileW;
    unsafe {
        let Ok(mut medium) = data.GetData(&etc(CF_HDROP, -1, TYMED_HGLOBAL.0 as u32)) else { return Vec::new() };
        let mut paths = Vec::new();
        if medium.tymed == TYMED_HGLOBAL.0 as u32 {
            let list = medium.u.hGlobal.0;
            let count = DragQueryFileW(list, u32::MAX, std::ptr::null_mut(), 0);
            for index in 0..count {
                let length = DragQueryFileW(list, index, std::ptr::null_mut(), 0) as usize;
                let mut name = vec![0u16; length + 1];
                let written = DragQueryFileW(list, index, name.as_mut_ptr(), name.len() as u32) as usize;
                paths.push(PathBuf::from(std::ffi::OsString::from_wide(&name[..written.min(length)])));
            }
        }
        ReleaseStgMedium(&mut medium);
        paths
    }
}

fn kind_of(data: &IDataObject) -> Kind {
    if offers(data, CF_HDROP) {
        Kind::Files
    } else if offers(data, descriptor_format()) || link(data).is_some() {
        Kind::Virtual
    } else {
        Kind::Nothing
    }
}

// ---------------------------------------------------------------- Das Ablageziel des Fensters

#[implement(IDropTarget)]
struct Target {
    hwnd: isize,
    bus: Bus,
    kind: Cell<Kind>,
}

/// Was die Quelle als Ergebnis hört. Immer „Kopieren", nie „Verschieben": ob verschoben wird, entscheidet das
/// Programm selbst und erledigt es dann auch selbst – sonst löschte zusätzlich die Quelle ihre Dateien.
fn effect_for(kind: Kind, allowed: DROPEFFECT) -> DROPEFFECT {
    match kind {
        Kind::Nothing => DROPEFFECT_NONE,
        _ if allowed.contains(DROPEFFECT_COPY) => DROPEFFECT_COPY,
        Kind::Virtual if allowed.contains(DROPEFFECT_LINK) => DROPEFFECT_LINK,
        _ => DROPEFFECT_NONE,
    }
}

fn answer(kind: Kind, effect: *mut DROPEFFECT) {
    if !effect.is_null() {
        unsafe { *effect = effect_for(kind, *effect) };
    }
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        self.kind.set(data.as_ref().map_or(Kind::Nothing, kind_of));
        answer(self.kind.get(), effect);
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        answer(self.kind.get(), effect);
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        self.kind.set(Kind::Nothing);
        Ok(())
    }

    fn Drop(&self, data: Ref<IDataObject>, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        let kind = self.kind.replace(Kind::Nothing);
        let (paths, temp) = match (data.as_ref(), kind) {
            (Some(data), Kind::Files) => (file_paths(data), false),
            (Some(data), Kind::Virtual) => (spool(data, &spool_dir()), true),
            _ => (Vec::new(), false),
        };
        answer(if paths.is_empty() { Kind::Nothing } else { kind }, effect);
        if !paths.is_empty() {
            let pos = crate::win::screen_to_window(self.hwnd, pt.x, pt.y);
            // MK_SHIFT = 4, MK_CONTROL = 8
            self.bus.send(Msg::Dropped { paths, pos, shift: keys.0 & 4 != 0, ctrl: keys.0 & 8 != 0, temp });
        }
        Ok(())
    }
}

/// Zwischenordner für eine Ablage; alte räumt das Programm beim Start weg.
pub fn spool_dir() -> PathBuf {
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join("TurboCommanderLite").join(format!("ablage-{stamp:x}"))
}

/// Meldet das eigene Ablageziel am Fenster an (anstelle dessen der Fensterbibliothek). Muss im Thread des Fensters
/// laufen. Liefert `false`, wenn es nicht geklappt hat.
pub fn install(hwnd: isize, bus: Bus) -> bool {
    let window = HWND(hwnd as *mut _);
    let target: IDropTarget = Target { hwnd, bus, kind: Cell::new(Kind::Nothing) }.into();
    unsafe {
        // Scheitert das Abmelden, war nichts angemeldet – das Anmelden klappt dann trotzdem.
        let _ = RevokeDragDrop(window);
        RegisterDragDrop(window, &target).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use windows::Win32::Foundation::{E_NOTIMPL, S_FALSE, S_OK};
    use windows::Win32::System::Com::{IAdviseSink, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, STGMEDIUM};
    use windows::Win32::System::Ole::OleInitialize;
    use windows::Win32::UI::Shell::SHCreateMemStream;
    use windows::core::{BOOL, HRESULT};

    /// Ein Datenobjekt, wie es ein Browser beim Ziehen übergibt: beliebige Formate, als Speicherblock oder Strom.
    #[implement(IDataObject)]
    struct Fake {
        /// (Format, Nummer, Inhalt, als Strom liefern)
        items: Vec<(u16, i32, Vec<u8>, bool)>,
    }

    impl Fake {
        fn find(&self, wanted: *const FORMATETC) -> Option<&(u16, i32, Vec<u8>, bool)> {
            let wanted = unsafe { wanted.as_ref()? };
            self.items.iter().find(|(format, index, _, stream)| {
                let tymed = if *stream { TYMED_ISTREAM.0 } else { TYMED_HGLOBAL.0 } as u32;
                *format == wanted.cfFormat && (*index == wanted.lindex || (*index == 0 && wanted.lindex == -1)) && wanted.tymed & tymed != 0
            })
        }
    }

    impl IDataObject_Impl for Fake_Impl {
        fn GetData(&self, wanted: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
            use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
            let Some((_, _, bytes, stream)) = self.find(wanted) else { return Err(windows::core::Error::from_hresult(HRESULT(0x80040064u32 as i32))) };
            let mut medium = STGMEDIUM::default();
            unsafe {
                if *stream {
                    medium.tymed = TYMED_ISTREAM.0 as u32;
                    let memory = SHCreateMemStream(Some(bytes));
                    // Wie manche Quellen: der Strom steht am Ende.
                    if let Some(memory) = &memory {
                        let _ = memory.Seek(0, windows::Win32::System::Com::STREAM_SEEK_END, None);
                    }
                    medium.u.pstm = std::mem::ManuallyDrop::new(memory);
                } else {
                    medium.tymed = TYMED_HGLOBAL.0 as u32;
                    // Etwas größer als der Inhalt, wie es bei Speicherblöcken vorkommt
                    let handle = GlobalAlloc(GMEM_MOVEABLE | 0x40, bytes.len() + 24);
                    let start = GlobalLock(handle) as *mut u8;
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), start, bytes.len());
                    GlobalUnlock(handle);
                    medium.u.hGlobal = HGLOBAL(handle);
                }
            }
            Ok(medium)
        }
        fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }
        fn QueryGetData(&self, wanted: *const FORMATETC) -> HRESULT {
            if self.find(wanted).is_some() { S_OK } else { S_FALSE }
        }
        fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, _: *mut FORMATETC) -> HRESULT {
            E_NOTIMPL
        }
        fn SetData(&self, _: *const FORMATETC, _: *const STGMEDIUM, _: BOOL) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }
        fn EnumFormatEtc(&self, _: u32) -> windows::core::Result<IEnumFORMATETC> {
            Err(E_NOTIMPL.into())
        }
        fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Ref<IAdviseSink>) -> windows::core::Result<u32> {
            Err(E_NOTIMPL.into())
        }
        fn DUnadvise(&self, _: u32) -> windows::core::Result<()> {
            Err(E_NOTIMPL.into())
        }
        fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
            Err(E_NOTIMPL.into())
        }
    }

    fn fake(items: Vec<(u16, i32, Vec<u8>, bool)>) -> IDataObject {
        Fake { items }.into()
    }

    fn wide(text: &str) -> Vec<u8> {
        text.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
    }

    /// Dateiliste, wie ein Browser sie mitschickt: Namen und – wenn angegeben – Größen
    fn group(files: &[(&str, Option<u64>)]) -> Vec<u8> {
        let mut out = (files.len() as u32).to_le_bytes().to_vec();
        for (name, size) in files {
            let mut entry = vec![0u8; DESCRIPTOR];
            if let Some(size) = size {
                entry[..4].copy_from_slice(&FD_FILESIZE.to_le_bytes());
                entry[DESCRIPTOR_SIZE_HIGH..DESCRIPTOR_SIZE_HIGH + 4].copy_from_slice(&((size >> 32) as u32).to_le_bytes());
                entry[DESCRIPTOR_SIZE_HIGH + 4..DESCRIPTOR_SIZE_HIGH + 8].copy_from_slice(&(*size as u32).to_le_bytes());
            }
            let wide = wide(name);
            entry[DESCRIPTOR_NAME..DESCRIPTOR_NAME + wide.len()].copy_from_slice(&wide);
            out.extend(entry);
        }
        out
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tc-ablage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_link_from_the_address_bar_becomes_a_url_file() {
        unsafe {
            let _ = OleInitialize(None);
        }
        // Chrome und Edge: die Verknüpfung kommt fertig mit, benannt nach dem Titel der Seite.
        let shortcut = "[InternetShortcut]\r\nURL=https://www.beispiel.de/seite?x=1\r\n";
        let chrome = fake(vec![
            (descriptor_format(), -1, group(&[("Beispiel: Startseite | Mehr.url", None)]), false),
            (contents_format(), 0, shortcut.as_bytes().to_vec(), false),
            (format_id(w!("UniformResourceLocatorW")), -1, wide("https://www.beispiel.de/seite?x=1"), false),
        ]);
        assert_eq!(kind_of(&chrome), Kind::Virtual);
        let dir = temp("chrome");
        let paths = spool(&chrome, &dir);
        assert_eq!(names(&paths), ["Beispiel_ Startseite _ Mehr.url"]);
        assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), shortcut, "ohne angehängte Nullen");

        // Nur die Adresse, keine fertige Datei: die Verknüpfung entsteht hier.
        let plain = fake(vec![(format_id(w!("UniformResourceLocatorW")), -1, wide("https://www.beispiel.de/pfad/zur seite/?a=ä#teil"), false)]);
        assert_eq!(kind_of(&plain), Kind::Nothing, "Leerzeichen: das ist keine Adresse");
        let plain = fake(vec![(format_id(w!("UniformResourceLocatorW")), -1, wide("https://www.beispiel.de/pfad/seite/?a=ä#teil"), false)]);
        let paths = spool(&plain, &dir);
        assert_eq!(names(&paths), ["beispiel.de - pfad - seite.url"]);
        assert_eq!(std::fs::read_to_string(&paths[0]).unwrap(), "[InternetShortcut]\r\nURL=https://www.beispiel.de/pfad/seite/?a=%C3%A4#teil\r\n");

        // Firefox: Adresse und Titel in einem eigenen Format
        let firefox = fake(vec![(format_id(w!("text/x-moz-url")), -1, wide("https://beispiel.de/\nMeine Seite"), false)]);
        let paths = spool(&firefox, &dir);
        assert_eq!(names(&paths), ["Meine Seite.url"]);

        // Als Text gezogene Adresse – und Text, der keine ist
        let text = fake(vec![(CF_UNICODETEXT, -1, wide("  https://beispiel.de/a  "), false)]);
        assert_eq!(names(&spool(&text, &dir)), ["beispiel.de - a.url"]);
        let words = fake(vec![(CF_UNICODETEXT, -1, wide("nur ein Satz aus einer Seite"), false)]);
        assert_eq!(kind_of(&words), Kind::Nothing);
        assert!(spool(&words, &dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn offered_files_are_written_from_streams_and_memory() {
        unsafe {
            let _ = OleInitialize(None);
        }
        let picture: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
        let data = fake(vec![
            (descriptor_format(), -1, group(&[("bild.png", Some(picture.len() as u64)), ("unter\\notiz.txt", Some(5)), ("bild.png", None), ("fehlt.bin", None)]), false),
            (contents_format(), 0, picture.clone(), true),
            (contents_format(), 1, b"Hallo".to_vec(), false),
            (contents_format(), 2, vec![7u8; 10], true),
        ]);
        let dir = temp("dateien");
        let paths = spool(&data, &dir);
        assert_eq!(names(&paths), ["bild.png", "notiz.txt", "bild (2).png"], "ohne Inhalt keine Datei");
        assert_eq!(std::fs::read(&paths[0]).unwrap(), picture, "Strom, der am Ende stand");
        assert_eq!(std::fs::read(&paths[1]).unwrap(), b"Hallo", "Speicherblock auf die angegebene Größe gekürzt");
        assert_eq!(std::fs::read(&paths[2]).unwrap(), vec![7u8; 10]);
        assert!(!dir.join("fehlt.bin").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_are_made_safe() {
        assert_eq!(safe_name("a/b\\c: d?.url"), "c_ d_.url");
        assert_eq!(safe_name("  Titel.  "), "Titel");
        assert_eq!(safe_name("..."), "Datei");
        assert_eq!(safe_name(".url"), "Link.url");
        assert_eq!(safe_name(&format!("{}.url", "x".repeat(300))).chars().count(), 124);
        assert!(looks_like_url("ftp://server/pfad") && looks_like_url("https://a.b"));
        assert!(!looks_like_url("C:\\Ordner") && !looks_like_url("://x") && !looks_like_url("kein link://x y"));
        assert_eq!(title_of("https://www.beispiel.de/"), "beispiel.de");
    }

    #[test]
    fn the_drop_target_reports_files_and_links() {
        unsafe {
            let _ = OleInitialize(None);
        }
        let (tx, rx) = mpsc::channel();
        let bus = Bus::new(tx, eframe::egui::Context::default());
        let target: IDropTarget = Target { hwnd: 0, bus, kind: Cell::new(Kind::Nothing) }.into();
        let point = POINTL { x: 10, y: 10 };
        let all = DROPEFFECT_COPY | DROPEFFECT_LINK | windows::Win32::System::Ole::DROPEFFECT_MOVE;
        let drop = |data: &IDataObject, keys: u32, allowed: DROPEFFECT| unsafe {
            let mut effect = allowed;
            target.DragEnter(data, MODIFIERKEYS_FLAGS(keys), point, &mut effect).unwrap();
            let entered = effect;
            let mut effect = allowed;
            target.DragOver(MODIFIERKEYS_FLAGS(keys), point, &mut effect).unwrap();
            assert_eq!(effect, entered);
            let mut effect = allowed;
            target.Drop(data, MODIFIERKEYS_FLAGS(keys), point, &mut effect).unwrap();
            effect
        };

        // Ein Link: kommt als Datei im Zwischenordner an
        let link = fake(vec![(format_id(w!("UniformResourceLocatorW")), -1, wide("https://beispiel.de/"), false)]);
        assert_eq!(drop(&link, 0, all), DROPEFFECT_COPY);
        match rx.try_recv() {
            Ok(Msg::Dropped { paths, temp: true, shift: false, ctrl: false, .. }) => {
                assert_eq!(names(&paths), ["beispiel.de.url"]);
                let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
            }
            _ => panic!("keine Meldung über den abgelegten Link"),
        }
        // Erlaubt die Quelle nur „Verknüpfen", wird das gemeldet.
        assert_eq!(drop(&link, 0, DROPEFFECT_LINK), DROPEFFECT_LINK);
        match rx.try_recv() {
            Ok(Msg::Dropped { paths, .. }) => {
                let _ = std::fs::remove_dir_all(paths[0].parent().unwrap());
            }
            _ => panic!("keine Meldung"),
        }

        // Dateien von der Platte: Pfade unverändert, Umschalt wird gemeldet, die Quelle hört immer „kopiert".
        let dir = temp("ziel");
        std::fs::create_dir_all(dir.join("Ordner")).unwrap();
        std::fs::write(dir.join("eins äöü.txt"), "x").unwrap();
        let files = crate::dragout::data_object(&[dir.join("eins äöü.txt"), dir.join("Ordner")]).expect("kein Datenobjekt");
        assert_eq!(drop(&files, 4, all), DROPEFFECT_COPY);
        match rx.try_recv() {
            Ok(Msg::Dropped { paths, temp: false, shift: true, ctrl: false, .. }) => {
                assert_eq!(paths, [dir.join("eins äöü.txt"), dir.join("Ordner")]);
            }
            _ => panic!("keine Meldung über die abgelegten Dateien"),
        }

        // Nichts Brauchbares: abgelehnt, keine Meldung
        let words = fake(vec![(CF_UNICODETEXT, -1, wide("nur Text"), false)]);
        assert_eq!(drop(&words, 0, all), DROPEFFECT_NONE);
        assert!(rx.try_recv().is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
