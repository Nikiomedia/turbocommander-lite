//! Dünne Schicht über der Windows-API: Laufwerke, Shell, Zwischenablage, DPAPI, Titelleiste.

use std::ffi::{OsString, c_void};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::Storage::FileSystem::{
    CopyFileExW, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows_sys::Win32::UI::Shell::{
    DragQueryFileW, FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music,
    FOLDERID_Pictures, FOLDERID_Videos, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW, SHFILEINFOW,
    SHGFI_TYPENAME, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SHGetKnownFolderPath, SHQUERYRBINFO,
    SHQueryRecycleBinW, ShellExecuteExW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetCursorPos, SW_SHOWNORMAL};
use windows_sys::core::GUID;

const CF_HDROP: u32 = 15;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn wide_path(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

fn from_wide_buf(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

// ---------------------------------------------------------------- Laufwerke

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    Optical,
    Other,
}

#[derive(Clone, Debug)]
pub struct Drive {
    /// z. B. `C:\`
    pub root: String,
    pub label: String,
    pub kind: DriveKind,
    pub total: u64,
    pub free: u64,
}

impl Drive {
    pub fn letter(&self) -> &str {
        &self.root[..2]
    }

    pub fn kind_text(&self) -> &'static str {
        use crate::lang::t;
        match self.kind {
            DriveKind::Fixed => t("Lokaler Datenträger", "Local disk"),
            DriveKind::Removable => t("Wechseldatenträger", "Removable drive"),
            DriveKind::Network => t("Netzlaufwerk", "Network drive"),
            DriveKind::Optical => t("CD/DVD-Laufwerk", "CD/DVD drive"),
            DriveKind::Other => t("Laufwerk", "Drive"),
        }
    }

    pub fn caption(&self) -> String {
        if self.label.is_empty() {
            format!("{} ({})", self.kind_text(), self.letter())
        } else {
            format!("{} ({})", self.label, self.letter())
        }
    }
}

/// Kann bei getrennten Netzlaufwerken blockieren – nur im Hintergrund aufrufen.
pub fn drives() -> Vec<Drive> {
    let mut out = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let root = format!("{}:\\", (b'A' + i as u8) as char);
        let w = wide(&root);
        let kind = match unsafe { GetDriveTypeW(w.as_ptr()) } {
            2 => DriveKind::Removable,
            3 => DriveKind::Fixed,
            4 => DriveKind::Network,
            5 => DriveKind::Optical,
            _ => DriveKind::Other,
        };
        let mut name = [0u16; 261];
        let ok = unsafe {
            GetVolumeInformationW(
                w.as_ptr(),
                name.as_mut_ptr(),
                name.len() as u32,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                0,
            )
        };
        let label = if ok != 0 { from_wide_buf(&name) } else { String::new() };
        let (free, total) = disk_space(Path::new(&root)).unwrap_or((0, 0));
        out.push(Drive { root, label, kind, total, free });
    }
    out
}

/// (frei, gesamt) in Bytes.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let w = wide_path(path);
    let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) };
    if ok != 0 { Some((avail, total)) } else { None }
}

/// Laufwerk eines Pfads in der Form `D:\` – nur solche Laufwerke können einen Papierkorb haben (Freigaben nicht).
pub fn bin_root(path: &Path) -> Option<String> {
    let text = path.to_string_lossy();
    text.get(..3).filter(|r| r.ends_with(":\\") && r.as_bytes()[0].is_ascii_alphabetic()).map(|r| r.to_ascii_uppercase())
}

/// Hat das Laufwerk dieses Pfads einen Papierkorb? (Netzlaufwerke und viele USB-Sticks haben keinen.)
/// Windows zählt dafür den Inhalt des Papierkorbs und liest von der Platte: bei vollem Papierkorb oder nach
/// längerer Pause dauert das spürbar – deshalb nicht aus der Oberfläche heraus aufrufen (siehe `App::has_bin`).
pub fn has_recycle_bin(path: &Path) -> bool {
    let Some(root) = bin_root(path) else {
        return false;
    };
    let w = wide(&root);
    let mut info: SHQUERYRBINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHQUERYRBINFO>() as u32;
    unsafe { SHQueryRecycleBinW(w.as_ptr(), &mut info) == 0 }
}

// ---------------------------------------------------------------- Bekannte Ordner

fn known_folder(id: &GUID) -> Option<PathBuf> {
    let mut ptr: *mut u16 = null_mut();
    let hr = unsafe { SHGetKnownFolderPath(id, 0, null_mut(), &mut ptr) };
    if hr != 0 || ptr.is_null() {
        if !ptr.is_null() {
            unsafe { CoTaskMemFree(ptr as *const c_void) };
        }
        return None;
    }
    let mut len = 0usize;
    unsafe {
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let path = PathBuf::from(OsString::from_wide(std::slice::from_raw_parts(ptr, len)));
        CoTaskMemFree(ptr as *const c_void);
        Some(path)
    }
}

/// Sprache der Windows-Oberfläche: Deutsch, sonst Englisch.
pub fn system_lang() -> crate::lang::Lang {
    const GERMAN: u16 = 0x07;
    let id = unsafe { windows_sys::Win32::Globalization::GetUserDefaultUILanguage() };
    if id & 0x3ff == GERMAN { crate::lang::Lang::De } else { crate::lang::Lang::En }
}

/// Anzeigename eines Benutzerordners in der eingestellten Sprache. `key` ist der Name aus `user_folders`.
pub fn folder_label(key: &str) -> &str {
    use crate::lang::t;
    match key {
        "Dokumente" => t("Dokumente", "Documents"),
        "Bilder" => t("Bilder", "Pictures"),
        "Musik" => t("Musik", "Music"),
        "OneDrive (Geschäft)" => t("OneDrive (Geschäft)", "OneDrive (Business)"),
        "Dropbox (Geschäft)" => t("Dropbox (Geschäft)", "Dropbox (Business)"),
        other => other,
    }
}

/// (Kennung, Pfad) der üblichen Benutzerordner. Die Kennung ist der deutsche Name und bleibt in jeder
/// Sprache gleich (sie dient auch zum Wiederfinden); angezeigt wird `folder_label(kennung)`.
pub fn user_folders() -> Vec<(&'static str, PathBuf)> {
    let ids: [(&str, &GUID); 6] = [
        ("Desktop", &FOLDERID_Desktop),
        ("Dokumente", &FOLDERID_Documents),
        ("Downloads", &FOLDERID_Downloads),
        ("Bilder", &FOLDERID_Pictures),
        ("Musik", &FOLDERID_Music),
        ("Videos", &FOLDERID_Videos),
    ];
    ids.iter()
        .filter_map(|(name, id)| known_folder(id).map(|p| (*name, p)))
        .collect()
}

/// Lokale Ordner der Cloud-Dienste, soweit eingerichtet: OneDrive (privat/geschäftlich) und Dropbox.
pub fn cloud_folders() -> Vec<(&'static str, PathBuf)> {
    fn add(out: &mut Vec<(&'static str, PathBuf)>, label: &'static str, path: PathBuf) {
        if path.is_dir() && !out.iter().any(|(_, known)| *known == path) {
            out.push((label, path));
        }
    }
    let mut out: Vec<(&'static str, PathBuf)> = Vec::new();
    // OneDrive trägt seine Ordner als Umgebungsvariablen ein.
    for (label, var) in [("OneDrive", "OneDriveConsumer"), ("OneDrive", "OneDrive"), ("OneDrive (Geschäft)", "OneDriveCommercial")] {
        if let Ok(path) = std::env::var(var) {
            add(&mut out, label, PathBuf::from(path));
        }
    }
    // Dropbox legt den Speicherort in einer kleinen JSON-Datei ab.
    for base in ["LOCALAPPDATA", "APPDATA"] {
        let Ok(dir) = std::env::var(base) else { continue };
        let Ok(text) = std::fs::read_to_string(PathBuf::from(dir).join("Dropbox").join("info.json")) else { continue };
        let Ok(info) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        for (label, key) in [("Dropbox", "personal"), ("Dropbox (Geschäft)", "business")] {
            if let Some(path) = info[key]["path"].as_str() {
                add(&mut out, label, PathBuf::from(path));
            }
        }
    }
    if !out.iter().any(|(label, _)| label.starts_with("Dropbox")) {
        if let Ok(home) = std::env::var("USERPROFILE") {
            add(&mut out, "Dropbox", PathBuf::from(home).join("Dropbox"));
        }
    }
    out
}

// ---------------------------------------------------------------- Eingehängte Orte (Telefon, weitere Cloud-Dienste)

/// Zeichenkette aus dem Zweig des angemeldeten Benutzers; ein leerer `name` meint den Standardwert des Schlüssels.
fn reg_text(key: &str, name: &str) -> Option<String> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW};
    let (key, name) = (wide(key), wide(name));
    let mut buf = [0u16; 1024];
    let mut size = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            null_mut(),
            buf.as_mut_ptr().cast(),
            &mut size,
        )
    };
    (status == 0).then(|| from_wide_buf(&buf))
}

/// Namen der Unterschlüssel im Zweig des angemeldeten Benutzers.
fn reg_subkeys(key: &str) -> Vec<String> {
    use windows_sys::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegEnumKeyExW, RegOpenKeyExW};
    let key = wide(key);
    let mut handle: HKEY = null_mut();
    if unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, KEY_READ, &mut handle) } != 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for index in 0..512 {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        let status =
            unsafe { RegEnumKeyExW(handle, index, name.as_mut_ptr(), &mut len, null_mut(), null_mut(), null_mut(), null_mut()) };
        if status != 0 {
            break;
        }
        out.push(String::from_utf16_lossy(&name[..len as usize]));
    }
    unsafe { RegCloseKey(handle) };
    out
}

/// Ein Ort, den ein Programm im Explorer neben „Dieser PC" eingehängt hat: der Ordner eines Cloud-Dienstes oder
/// ein Telefon, das über „Link zu Windows" verbunden ist.
#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub name: String,
    /// Wohin der Eintrag führt
    pub path: PathBuf,
    /// Der eingehängte Ordner selbst; beim Telefon liegt `path` darin
    pub root: PathBuf,
    pub phone: bool,
}

/// Aus den Angaben eines eingehängten Orts wird ein Eintrag: `provider` ist die Kennung des Anbieters
/// („CrossDevice!…" bei Telefonen), `root` der Ordner, `storage` sagt, ob darin ein Ordner „storage" liegt.
fn place_from(title: Option<String>, provider: &str, root: PathBuf, storage: bool) -> Place {
    let phone = provider.starts_with("CrossDevice!");
    let name = title
        .filter(|name| !name.trim().is_empty())
        .or_else(|| root.file_name().map(|name| name.to_string_lossy().into_owned()))
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    // Beim Telefon liegt der Speicher eine Ebene tiefer; daneben stehen nur Verknüpfungen für den Explorer.
    let path = if phone && storage { root.join("storage") } else { root.clone() };
    Place { name, path, root, phone }
}

/// Telefone und Cloud-Ordner, die Windows im Explorer neben „Dieser PC" zeigt und die `cloud_folders` nicht
/// schon kennt. Liest nur die Registrierung und prüft, ob es die Ordner gibt.
pub fn places() -> Vec<Place> {
    const SPACE: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Desktop\\NameSpace";
    let known: Vec<PathBuf> = cloud_folders().into_iter().map(|(_, path)| path).collect();
    let same = |a: &Path, b: &Path| a.as_os_str().eq_ignore_ascii_case(b.as_os_str());
    let mut out: Vec<Place> = Vec::new();
    for id in reg_subkeys(SPACE) {
        let class = format!("Software\\Classes\\CLSID\\{id}");
        // Nur was auf einen Ordner zeigt, lässt sich hier öffnen.
        let Some(folder) = reg_text(&format!("{class}\\Instance\\InitPropertyBag"), "TargetFolderPath") else { continue };
        let root = PathBuf::from(folder);
        if !root.is_dir() {
            continue;
        }
        let provider = reg_text(&format!("{SPACE}\\{id}"), "").unwrap_or_default();
        let storage = root.join("storage").is_dir();
        let place = place_from(reg_text(&class, ""), &provider, root.clone(), storage);
        let taken = |path: &Path| known.iter().any(|k| same(k, path)) || out.iter().any(|p| same(&p.path, path));
        if !taken(&root) && !taken(&place.path) {
            out.push(place);
        }
    }
    out
}

/// Markiert eine Datei oder einen Ordner in einem Cloud-Ordner als „immer auf diesem PC behalten" (`keep`) oder
/// gibt ihren Platz frei – dann bleibt der Inhalt nur online. Das Laden oder Verwerfen erledigt danach der
/// Cloud-Dienst; so macht es auch `attrib +P -U` bzw. `attrib -P +U`.
pub fn set_cloud_keep(path: &Path, keep: bool) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{GetFileAttributesW, INVALID_FILE_ATTRIBUTES, SetFileAttributesW};
    use crate::vfs::Cloud;
    let w = wide_path(path);
    let attrs = unsafe { GetFileAttributesW(w.as_ptr()) };
    if attrs == INVALID_FILE_ATTRIBUTES {
        return Err(io::Error::last_os_error());
    }
    let wanted = if keep { (attrs & !Cloud::UNPINNED) | Cloud::PINNED } else { (attrs & !Cloud::PINNED) | Cloud::UNPINNED };
    if wanted != attrs && unsafe { SetFileAttributesW(w.as_ptr(), wanted) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Wie `set_cloud_keep`, bei Ordnern samt allem darin. Liefert, wie viele Einträge sich nicht umstellen ließen.
pub fn set_cloud_keep_tree(path: &Path, keep: bool) -> usize {
    let mut failed = 0;
    let mut waiting = vec![path.to_path_buf()];
    while let Some(next) = waiting.pop() {
        if set_cloud_keep(&next, keep).is_err() {
            failed += 1;
        }
        // Bei einer Datei gibt es nichts zu lesen.
        let Ok(items) = std::fs::read_dir(&next) else { continue };
        for item in items.flatten() {
            match item.file_type() {
                // Verknüpfte Ordner führen aus dem Cloud-Ordner hinaus.
                Ok(kind) if kind.is_symlink() => {}
                Ok(kind) if kind.is_dir() => waiting.push(item.path()),
                _ => {
                    if set_cloud_keep(&item.path(), keep).is_err() {
                        failed += 1;
                    }
                }
            }
        }
    }
    failed
}

// ---------------------------------------------------------------- Shell

pub fn shell_open(path: &Path) -> bool {
    shell_execute(path, None)
}

pub fn shell_open_with(path: &Path) -> bool {
    shell_execute(path, Some("openas"))
}

/// Startet ein Programm mit Parametern im angegebenen Ordner. Windows sucht den Namen wie im „Ausführen"-Dialog:
/// volle Pfade, Programme im Suchpfad und registrierte Programmnamen.
pub fn shell_run(program: &str, params: &str, dir: &Path, hidden: bool) -> bool {
    let (file, params, dir) = (wide(program), wide(params), wide_path(dir));
    // 0 = SW_HIDE: ohne sichtbares Fenster, für Tests
    let show = if hidden { 0 } else { SW_SHOWNORMAL };
    let res = unsafe { ShellExecuteW(null_mut(), null(), file.as_ptr(), params.as_ptr(), dir.as_ptr(), show) };
    res as usize > 32
}

fn shell_execute(path: &Path, verb: Option<&str>) -> bool {
    let file = wide_path(path);
    let verb_w = verb.map(wide);
    let dir = path.parent().map(wide_path);
    let res = unsafe {
        ShellExecuteW(
            null_mut(),
            verb_w.as_ref().map_or(null(), |v| v.as_ptr()),
            file.as_ptr(),
            null(),
            dir.as_ref().map_or(null(), |d| d.as_ptr()),
            SW_SHOWNORMAL,
        )
    };
    res as usize > 32
}

/// Öffnet den Eigenschaften-Dialog von Windows.
pub fn shell_properties(path: &Path) -> bool {
    let file = wide_path(path);
    let verb = wide("properties");
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_INVOKEIDLIST;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    unsafe { ShellExecuteExW(&mut info) != 0 }
}

pub fn reveal_in_explorer(path: &Path) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .spawn();
}

/// Typname wie im Explorer („PNG-Datei", „Dateiordner"). Nur die Endung zählt.
pub fn type_name(ext: &str, is_dir: bool) -> String {
    use crate::lang::t;
    // Windows liefert die Typnamen in seiner eigenen Sprache. Ist im Programm die andere eingestellt,
    // stünde sonst eine deutsche Spalte in einer englischen Oberfläche (oder umgekehrt): dann einfache eigene Namen.
    if crate::lang::lang() != system_lang() {
        return if is_dir {
            t("Dateiordner", "File folder").to_owned()
        } else if ext.is_empty() {
            t("Datei", "File").to_owned()
        } else {
            crate::tf!("{}-Datei", "{} file", ext.to_uppercase())
        };
    }
    let probe = if is_dir { wide("ordner") } else { wide(&format!("x.{ext}")) };
    let attrs: u32 = if is_dir { 0x10 } else { 0x80 };
    let mut info: SHFILEINFOW = unsafe { std::mem::zeroed() };
    let res = unsafe {
        SHGetFileInfoW(
            probe.as_ptr(),
            attrs,
            &mut info,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES,
        )
    };
    let name = if res != 0 { from_wide_buf(&info.szTypeName) } else { String::new() };
    if !name.is_empty() {
        name
    } else if is_dir {
        t("Dateiordner", "File folder").to_owned()
    } else if ext.is_empty() {
        t("Datei", "File").to_owned()
    } else {
        crate::tf!("{}-Datei", "{} file", ext.to_uppercase())
    }
}

// ---------------------------------------------------------------- Zwischenablage (Dateien)

fn drop_effect_format() -> u32 {
    let name = wide("Preferred DropEffect");
    unsafe { RegisterClipboardFormatW(name.as_ptr()) }
}

/// Legt Dateien so in die Zwischenablage, dass auch der Explorer sie einfügen kann.
pub fn clipboard_set_files(paths: &[PathBuf], cut: bool) -> bool {
    if paths.is_empty() {
        return false;
    }
    // DROPFILES-Kopf (20 Bytes) + doppelt null-terminierte UTF-16-Liste
    let mut list: Vec<u16> = Vec::new();
    for p in paths {
        list.extend(p.as_os_str().encode_wide());
        list.push(0);
    }
    list.push(0);
    const HEADER: usize = 20;
    let bytes = HEADER + list.len() * 2;
    unsafe {
        if OpenClipboard(null_mut()) == 0 {
            return false;
        }
        EmptyClipboard();
        let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if hmem.is_null() {
            CloseClipboard();
            return false;
        }
        let ptr = GlobalLock(hmem) as *mut u8;
        if ptr.is_null() {
            CloseClipboard();
            return false;
        }
        std::ptr::write_bytes(ptr, 0, bytes);
        // pFiles = Offset der Liste, fWide = 1
        (ptr as *mut u32).write_unaligned(HEADER as u32);
        (ptr.add(16) as *mut u32).write_unaligned(1);
        std::ptr::copy_nonoverlapping(list.as_ptr() as *const u8, ptr.add(HEADER), list.len() * 2);
        GlobalUnlock(hmem);
        SetClipboardData(CF_HDROP, hmem);

        let effect = GlobalAlloc(GMEM_MOVEABLE, 4);
        if !effect.is_null() {
            let p = GlobalLock(effect) as *mut u32;
            if !p.is_null() {
                p.write_unaligned(if cut { 2 } else { 5 });
                GlobalUnlock(effect);
                SetClipboardData(drop_effect_format(), effect);
            }
        }
        CloseClipboard();
    }
    true
}

/// Dateien aus der Zwischenablage; der zweite Wert sagt, ob „Ausschneiden" gemeint war.
pub fn clipboard_get_files() -> Option<(Vec<PathBuf>, bool)> {
    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP) == 0 {
            return None;
        }
        if OpenClipboard(null_mut()) == 0 {
            return None;
        }
        let hdrop = GetClipboardData(CF_HDROP);
        if hdrop.is_null() {
            CloseClipboard();
            return None;
        }
        let count = DragQueryFileW(hdrop, u32::MAX, null_mut(), 0);
        let mut paths = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, null_mut(), 0) as usize;
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(hdrop, i, buf.as_mut_ptr(), buf.len() as u32);
            paths.push(PathBuf::from(OsString::from_wide(&buf[..len])));
        }
        let mut cut = false;
        let effect = GetClipboardData(drop_effect_format());
        if !effect.is_null() {
            let p = GlobalLock(effect) as *const u32;
            if !p.is_null() {
                cut = p.read_unaligned() & 2 != 0;
                GlobalUnlock(effect);
            }
        }
        CloseClipboard();
        if paths.is_empty() { None } else { Some((paths, cut)) }
    }
}

pub fn clipboard_has_files() -> bool {
    unsafe { IsClipboardFormatAvailable(CF_HDROP) != 0 }
}

// ---------------------------------------------------------------- Fenster

/// Färbt die native Titelleiste passend zum Design (Windows 11; ältere Systeme ignorieren es).
pub fn style_titlebar(hwnd: isize, dark: bool, caption: [u8; 3], text: [u8; 3], border: [u8; 3]) {
    let hwnd = hwnd as HWND;
    let colorref = |c: [u8; 3]| (c[0] as u32) | ((c[1] as u32) << 8) | ((c[2] as u32) << 16);
    let set = |attr: u32, value: u32| unsafe {
        DwmSetWindowAttribute(hwnd, attr, &value as *const u32 as *const c_void, 4);
    };
    set(20, dark as u32); // DWMWA_USE_IMMERSIVE_DARK_MODE
    set(34, colorref(border)); // DWMWA_BORDER_COLOR
    set(35, colorref(caption)); // DWMWA_CAPTION_COLOR
    set(36, colorref(text)); // DWMWA_TEXT_COLOR
}

/// Mausposition in Fenster-Pixeln (auch während eines Drag & Drop aus dem Explorer).
pub fn cursor_in_window(hwnd: isize) -> Option<(f32, f32)> {
    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        if GetCursorPos(&mut pt) == 0 {
            return None;
        }
        if ScreenToClient(hwnd as HWND, &mut pt) == 0 {
            return None;
        }
    }
    Some((pt.x as f32, pt.y as f32))
}

/// Rechnet eine Stelle auf dem Bildschirm in eine Stelle im Fenster um (beides in Pixeln).
pub fn screen_to_window(hwnd: isize, x: i32, y: i32) -> Option<(f32, f32)> {
    let mut pt = POINT { x, y };
    if hwnd == 0 || unsafe { ScreenToClient(hwnd as HWND, &mut pt) } == 0 {
        return None;
    }
    Some((pt.x as f32, pt.y as f32))
}

// ---------------------------------------------------------------- Dateidialoge

fn file_dialog(owner: Option<isize>, save: bool, title: &str, default_name: &str, label: &str, ext: &str, dir: Option<&Path>) -> Option<PathBuf> {
    use windows_sys::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, GetSaveFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR,
        OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let mut file = [0u16; 1024];
    for (i, unit) in default_name.encode_utf16().take(1000).enumerate() {
        file[i] = unit;
    }
    // Paare „Beschriftung\0Muster\0", am Ende ein zusätzliches \0
    // Ohne Endung (etwa für Schlüsseldateien) gibt es nur „Alle Dateien".
    let filter: Vec<u16> = if ext.is_empty() {
        crate::lang::t("Alle Dateien (*.*)\0*.*\0\0", "All files (*.*)\0*.*\0\0").to_owned()
    } else {
        crate::tf!(
            "{label} (*.{ext})\0*.{ext}\0Alle Dateien (*.*)\0*.*\0\0",
            "{label} (*.{ext})\0*.{ext}\0All files (*.*)\0*.*\0\0"
        )
    }
    .encode_utf16()
    .collect();
    let title_w = wide(title);
    let ext_w = wide(ext);
    let dir_w = dir.map(wide_path);
    let mut ofn: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = owner.map_or(null_mut(), |h| h as HWND);
    ofn.lpstrFilter = filter.as_ptr();
    ofn.lpstrFile = file.as_mut_ptr();
    ofn.nMaxFile = file.len() as u32;
    ofn.lpstrTitle = title_w.as_ptr();
    ofn.lpstrDefExt = ext_w.as_ptr();
    ofn.lpstrInitialDir = dir_w.as_ref().map_or(null(), |d| d.as_ptr());
    ofn.Flags = OFN_EXPLORER | OFN_NOCHANGEDIR | OFN_HIDEREADONLY | OFN_PATHMUSTEXIST | if save { OFN_OVERWRITEPROMPT } else { OFN_FILEMUSTEXIST };
    let ok = unsafe { if save { GetSaveFileNameW(&mut ofn) } else { GetOpenFileNameW(&mut ofn) } };
    (ok != 0).then(|| PathBuf::from(from_wide_buf(&file)))
}

/// Windows-Dialog „Speichern unter". Blockiert – in einem eigenen Thread aufrufen.
pub fn save_dialog(owner: Option<isize>, title: &str, default_name: &str, label: &str, ext: &str, dir: Option<&Path>) -> Option<PathBuf> {
    file_dialog(owner, true, title, default_name, label, ext, dir)
}

/// Windows-Dialog „Öffnen". Blockiert – in einem eigenen Thread aufrufen.
pub fn open_dialog(owner: Option<isize>, title: &str, label: &str, ext: &str, dir: Option<&Path>) -> Option<PathBuf> {
    file_dialog(owner, false, title, "", label, ext, dir)
}

/// Privat belegter Arbeitsspeicher dieses Prozesses in MB (für Messungen beim Entwickeln).
pub fn private_memory_mb() -> f64 {
    use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    let mut info: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut info as *mut _ as *mut PROCESS_MEMORY_COUNTERS, size) };
    if ok != 0 { info.PrivateUsage as f64 / (1024.0 * 1024.0) } else { 0.0 }
}

/// In Windows eingestellte Doppelklick-Zeit in Sekunden (Systemsteuerung → Maus).
pub fn double_click_secs() -> f64 {
    let ms = unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() };
    (ms as f64 / 1000.0).clamp(0.2, 0.9)
}

/// Steht die Maus gerade über einem Fenster, das nicht zu uns gehört (auch wenn es unseres verdeckt)?
pub fn cursor_over_foreign_window(hwnd: isize) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GA_ROOT, GetAncestor, WindowFromPoint};
    let mut pt = POINT { x: 0, y: 0 };
    unsafe {
        if GetCursorPos(&mut pt) == 0 {
            return false;
        }
        let under = WindowFromPoint(pt);
        if under.is_null() {
            return true;
        }
        GetAncestor(under, GA_ROOT) != hwnd as HWND
    }
}

/// Meldet dem eigenen Fenster „linke Maustaste losgelassen". Nötig nach einem von Windows geführten Ziehen:
/// das Loslassen bekommt dann Windows mit, das Fenster selbst aber nicht.
pub fn post_left_button_up(hwnd: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_LBUTTONUP};
    let (x, y) = cursor_in_window(hwnd).unwrap_or((0.0, 0.0));
    let lparam = ((y as i32 as u16 as u32) << 16 | (x as i32 as u16 as u32)) as isize;
    unsafe { PostMessageW(hwnd as HWND, WM_LBUTTONUP, 0, lparam) };
}

pub fn key_down(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_linked_phone_opens_its_storage_and_keeps_its_name() {
        let root = PathBuf::from("C:\\Users\\x\\CrossDevice\\S22 Ultra");
        let phone = place_from(Some("S22 Ultra von Nikolaus".into()), "CrossDevice!!S-1-5-21!Root", root.clone(), true);
        assert_eq!(phone, Place { name: "S22 Ultra von Nikolaus".into(), path: root.join("storage"), root: root.clone(), phone: true });
        // Ohne den Ordner „storage" bleibt es beim eingehängten Ordner.
        assert_eq!(place_from(None, "CrossDevice!!x", root.clone(), false).path, root);
        // Ein Cloud-Dienst ohne eigenen Titel heißt wie sein Ordner – auch wenn darin ein „storage" liegt.
        let cloud = place_from(Some(" ".into()), "Nextcloud!S-1-5-21!1", PathBuf::from("D:\\Wolke"), true);
        assert_eq!((cloud.name.as_str(), cloud.path.as_path(), cloud.phone), ("Wolke", Path::new("D:\\Wolke"), false));
    }

    #[test]
    fn keeping_and_freeing_set_the_cloud_attributes() {
        use crate::vfs::Cloud;
        use std::os::windows::fs::MetadataExt;
        let dir = std::env::temp_dir().join(format!("tc-cloud-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("unter")).unwrap();
        std::fs::write(dir.join("a.txt"), b"a").unwrap();
        std::fs::write(dir.join("unter").join("b.txt"), b"b").unwrap();
        let state = |name: &str| std::fs::metadata(dir.join(name)).unwrap().file_attributes() & (Cloud::PINNED | Cloud::UNPINNED);
        assert_eq!(set_cloud_keep_tree(&dir, true), 0);
        assert_eq!((state("a.txt"), state("unter"), state("unter\\b.txt")), (Cloud::PINNED, Cloud::PINNED, Cloud::PINNED));
        assert_eq!(set_cloud_keep_tree(&dir.join("unter"), false), 0);
        assert_eq!((state("a.txt"), state("unter"), state("unter\\b.txt")), (Cloud::PINNED, Cloud::UNPINNED, Cloud::UNPINNED));
        assert!(set_cloud_keep(&dir.join("fehlt.txt"), true).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drives_and_type_names() {
        let all = drives();
        assert!(all.iter().any(|d| d.total > 0), "kein bereites Laufwerk gefunden");
        assert!(!type_name("", true).is_empty());
        assert!(!type_name("txt", false).is_empty());
        assert!(!user_folders().is_empty());
        assert!(has_recycle_bin(&std::env::temp_dir()), "Systemlaufwerk ohne Papierkorb?");
        assert!(!has_recycle_bin(Path::new("\\\\server\\freigabe\\ordner")));
        assert_eq!(bin_root(Path::new("d:\\Claude\\Ordner")).as_deref(), Some("D:\\"));
        assert_eq!(bin_root(Path::new("C:\\")).as_deref(), Some("C:\\"));
        assert_eq!(bin_root(Path::new("\\\\server\\freigabe\\ordner")), None);
        assert_eq!(bin_root(Path::new("C:")), None);
        // Aus einem Hintergrund-Thread gefragt – so ruft das Programm die Auskunft ab.
        let asked = std::thread::spawn(|| has_recycle_bin(&std::env::temp_dir())).join().unwrap();
        assert!(asked, "die Auskunft muss auch außerhalb des Fenster-Threads stimmen");
    }

    #[test]
    fn copy_file_reports_progress_and_can_cancel() {
        let dir = std::env::temp_dir().join(format!("tc-win-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("quelle.bin");
        std::fs::write(&src, vec![7u8; 3_000_000]).unwrap();
        let mut seen = 0u64;
        copy_file(&src, &dir.join("kopie.bin"), &mut |n| {
            seen += n;
            true
        })
        .unwrap();
        assert_eq!(seen, 3_000_000);
        assert_eq!(std::fs::metadata(dir.join("kopie.bin")).unwrap().len(), 3_000_000);
        let err = copy_file(&src, &dir.join("abbruch.bin"), &mut |_| false).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Interrupted);
        assert!(!dir.join("abbruch.bin").exists(), "abgebrochene Kopie blieb liegen");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Öffnet kurz den echten „Speichern unter"-Dialog und schließt ihn wieder.
    #[test]
    #[ignore = "zeigt für einen Moment einen Windows-Dialog"]
    fn save_dialog_really_opens() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_CLOSE};
        let title = "TurboCommander-Dialogtest";
        let worker = std::thread::spawn(move || save_dialog(None, title, "probe.tcbackup", "Test", "tcbackup", None));
        let title_w = wide(title);
        let mut found = false;
        for _ in 0..100 {
            let hwnd = unsafe { FindWindowW(null(), title_w.as_ptr()) };
            if !hwnd.is_null() {
                found = true;
                std::thread::sleep(std::time::Duration::from_millis(300));
                unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(found, "der Dialog ist nicht erschienen");
        assert_eq!(worker.join().unwrap(), None);
    }

    /// Überschreibt die Zwischenablage – daher nur auf ausdrücklichen Wunsch.
    #[test]
    #[ignore = "überschreibt die Windows-Zwischenablage"]
    fn clipboard_roundtrip() {
        let dir = std::env::temp_dir().join(format!("tc-clip-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = vec![dir.join("eins.txt"), dir.join("zwei äöü.txt")];
        for p in &paths {
            std::fs::write(p, "x").unwrap();
        }
        assert!(clipboard_set_files(&paths, true));
        assert!(clipboard_has_files());
        assert_eq!(clipboard_get_files(), Some((paths.clone(), true)));
        assert!(clipboard_set_files(&paths, false));
        assert_eq!(clipboard_get_files(), Some((paths, false)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------- Kopieren mit Fortschritt

struct CopyCtx<'a> {
    last: i64,
    on_progress: &'a mut dyn FnMut(u64) -> bool,
}

unsafe extern "system" fn copy_progress(
    _total: i64,
    transferred: i64,
    _stream_size: i64,
    _stream_transferred: i64,
    _stream_number: u32,
    _reason: u32,
    _src: *mut c_void,
    _dst: *mut c_void,
    data: *const c_void,
) -> u32 {
    let ctx = unsafe { &mut *(data as *mut CopyCtx) };
    let delta = (transferred - ctx.last).max(0) as u64;
    ctx.last = transferred;
    if (ctx.on_progress)(delta) { 0 } else { 1 }
}

/// Kopiert eine Datei mit nativer Geschwindigkeit; Zeitstempel und Attribute bleiben erhalten.
/// `on_progress` bekommt die neu übertragenen Bytes und liefert `false` zum Abbrechen.
pub fn copy_file(src: &Path, dst: &Path, on_progress: &mut dyn FnMut(u64) -> bool) -> io::Result<()> {
    let (s, d) = (wide_path(src), wide_path(dst));
    let mut ctx = CopyCtx { last: 0, on_progress };
    let ok = unsafe {
        CopyFileExW(
            s.as_ptr(),
            d.as_ptr(),
            Some(copy_progress),
            &mut ctx as *mut CopyCtx as *const c_void,
            null_mut(),
            0,
        )
    };
    if ok != 0 {
        Ok(())
    } else {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(1235) {
            Err(io::Error::new(io::ErrorKind::Interrupted, crate::lang::t("abgebrochen", "canceled")))
        } else {
            Err(err)
        }
    }
}
