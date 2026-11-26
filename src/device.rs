//! Per Kabel angeschlossene Geräte – Telefone und Kameras, die sich nicht als Laufwerk melden (MTP).
//!
//! Solche Geräte haben keine Dateipfade. Gelesen wird deshalb über die Windows-Shell, auf demselben Weg wie im
//! Explorer: Ordner auflisten, Dateien auf den PC kopieren. Das Kopieren übernimmt Windows selbst, mit seinem
//! Fortschrittsfenster und seinen Rückfragen. Geschrieben und gelöscht wird auf dem Gerät nicht.
//!
//! Alle Aufrufe können dauern (das Gerät antwortet langsam) und gehören in einen Hintergrund-Thread.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{FILETIME, PROPERTYKEY};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, IBindCtx};
use windows::Win32::System::SystemServices::{SFGAO_FOLDER, SFGAO_STREAM};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOF_NO_UI, FOF_NOCONFIRMMKDIR, FOLDERID_ComputerFolder, FileOperation, IEnumShellItems, IFileOperation,
    IFileOperationProgressSink, IShellItem, IShellItem2, KF_FLAG_DEFAULT, SHCreateItemFromParsingName,
    SHGetKnownFolderItem, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY, SIGDN_PARENTRELATIVEEDITING,
};
use windows::core::{GUID, HSTRING, Interface, PCWSTR};

use crate::lang::t;
use crate::netenum::{ComGuard, children, name_of};
use crate::tf;
use crate::vfs::Entry;

/// Für automatische Prüfläufe (`--device ordner`): dieser Ordner erscheint als Gerät „Testgerät".
pub static TEST_DEVICE: OnceLock<String> = OnceLock::new();

const STORAGE: GUID = GUID::from_u128(0xb725f130_47ef_101a_a5f1_02608c9eebac);
const PKEY_SIZE: PROPERTYKEY = PROPERTYKEY { fmtid: STORAGE, pid: 12 };
const PKEY_DATE_MODIFIED: PROPERTYKEY = PROPERTYKEY { fmtid: STORAGE, pid: 14 };

/// Shell-Pfade schon gesehener Einträge: (Gerät, Pfad) → Pfad in der Shell. So muss ein Ordner tief im Gerät nicht
/// bei jedem Zugriff von oben her gesucht werden.
static KNOWN: Mutex<Option<HashMap<(String, String), String>>> = Mutex::new(None);

fn remember(id: &str, path: &str, shell_path: String) {
    if let Ok(mut known) = KNOWN.lock() {
        let map = known.get_or_insert_with(HashMap::new);
        // Ein Gerät mit sehr vielen Dateien soll den Speicher nicht füllen.
        if map.len() > 200_000 {
            map.clear();
        }
        map.insert((id.to_owned(), path.to_owned()), shell_path);
    }
}

fn recalled(id: &str, path: &str) -> Option<String> {
    KNOWN.lock().ok()?.as_ref()?.get(&(id.to_owned(), path.to_owned())).cloned()
}

/// Geräte am Kabel nennt Windows mit ihrer Geräteschnittstelle („…\\?\usb#vid_…"); Laufwerke haben Pfade, und
/// Medienserver im Netz eine Kennung, die mit „uuid:" beginnt.
fn is_device_path(path: &str) -> bool {
    path.contains("\\\\?\\")
}

/// (Kennung, Anzeigename) der angeschlossenen Geräte.
pub fn devices() -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(folder) = TEST_DEVICE.get() {
        out.push((folder.clone(), t("Testgerät", "Test device").to_owned()));
    }
    let _com = ComGuard::new();
    let found = unsafe { SHGetKnownFolderItem::<IShellItem>(&FOLDERID_ComputerFolder, KF_FLAG_DEFAULT, None) }
        .and_then(|pc| children(&pc))
        .unwrap_or_default();
    out.extend(found.into_iter().filter(|(_, path)| is_device_path(path)).map(|(name, path)| (path, name)));
    out
}

fn join(path: &str, name: &str) -> String {
    if path.ends_with('/') { format!("{path}{name}") } else { format!("{path}/{name}") }
}

fn item_at(shell_path: &str) -> Option<IShellItem> {
    unsafe { SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(shell_path), None::<&IBindCtx>) }.ok()
}

/// Der Name eines Eintrags mit Endung – unabhängig davon, ob der Explorer Endungen ausblendet.
fn real_name(item: &IShellItem) -> Option<String> {
    name_of(item, SIGDN_PARENTRELATIVEEDITING).filter(|name| !name.is_empty()).or_else(|| name_of(item, SIGDN_NORMALDISPLAY))
}

/// Ruft `each` für jeden Eintrag eines Ordners auf, bis es `false` liefert.
fn for_each_child(folder: &IShellItem, mut each: impl FnMut(IShellItem) -> bool) -> windows::core::Result<()> {
    unsafe {
        let list: IEnumShellItems = folder.BindToHandler(None::<&IBindCtx>, &BHID_EnumItems)?;
        loop {
            let mut slot: [Option<IShellItem>; 1] = [None];
            let mut fetched = 0u32;
            if list.Next(&mut slot, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            let Some(child) = slot[0].take() else { break };
            if !each(child) {
                break;
            }
        }
    }
    Ok(())
}

/// Sucht den Eintrag zu einem Pfad im Gerät: erst im Gedächtnis, sonst Ordner für Ordner von oben.
fn resolve(id: &str, path: &str) -> Option<IShellItem> {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return item_at(id);
    }
    if let Some(item) = recalled(id, &format!("/{trimmed}")).and_then(|shell_path| item_at(&shell_path)) {
        return Some(item);
    }
    let mut item = item_at(id)?;
    for segment in trimmed.split('/') {
        let mut found = None;
        for_each_child(&item, |child| {
            if real_name(&child).as_deref() == Some(segment) {
                found = Some(child);
            }
            found.is_none()
        })
        .ok()?;
        item = found?;
    }
    Some(item)
}

fn gone() -> String {
    t("Das Gerät antwortet nicht – ist es noch angeschlossen und entsperrt?", "The device does not answer – is it still connected and unlocked?")
        .to_owned()
}

fn filetime_secs(time: FILETIME) -> Option<i64> {
    let ticks = ((time.dwHighDateTime as u64) << 32) | time.dwLowDateTime as u64;
    // 100-Nanosekunden-Schritte seit 1601; null heißt „unbekannt".
    (ticks > 0).then(|| (ticks / 10_000_000) as i64 - 11_644_473_600)
}

/// Die Einträge eines Ordners im Gerät.
pub fn list(id: &str, path: &str) -> Result<Vec<Entry>, String> {
    let _com = ComGuard::new();
    let folder = resolve(id, path).ok_or_else(gone)?;
    let mut out = Vec::new();
    for_each_child(&folder, |child| {
        let Some(name) = real_name(&child) else { return true };
        // Gepackte Dateien gelten in der Shell als Ordner und Datei zugleich; hier sind sie Dateien.
        let flags = unsafe { child.GetAttributes(SFGAO_FOLDER | SFGAO_STREAM) }.map(|flags| flags.0).unwrap_or(0);
        let is_dir = flags & SFGAO_FOLDER.0 != 0 && flags & SFGAO_STREAM.0 == 0;
        let (size, modified) = match child.cast::<IShellItem2>() {
            Ok(details) => unsafe {
                (details.GetUInt64(&PKEY_SIZE).unwrap_or(0), details.GetFileTime(&PKEY_DATE_MODIFIED).ok().and_then(filetime_secs))
            },
            Err(_) => (0, None),
        };
        if let Some(shell_path) = name_of(&child, SIGDN_DESKTOPABSOLUTEPARSING) {
            remember(id, &join(&format!("/{}", path.trim_matches('/')), &name).replace("//", "/"), shell_path);
        }
        out.push(Entry::new(name, is_dir, if is_dir { 0 } else { size }, modified));
        true
    })
    .map_err(|e| tf!("Das Gerät lässt sich nicht lesen: {}", "The device cannot be read: {}", e.message()))?;
    Ok(out)
}

/// Kopiert Einträge eines Geräteordners in einen Ordner auf diesem PC. Die Übertragung macht Windows; mit `quiet`
/// ohne Fenster und Rückfragen (zum Öffnen einer einzelnen Datei aus einem frischen Zwischenordner).
pub fn copy_out(id: &str, path: &str, names: &[String], dest: &Path, quiet: bool) -> Result<(), String> {
    let _com = ComGuard::new();
    let failed = |e: windows::core::Error| tf!("Kopieren vom Gerät: {}", "Copying from the device: {}", e.message());
    unsafe {
        let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(failed)?;
        operation.SetOperationFlags(if quiet { FOF_NO_UI } else { FOF_NOCONFIRMMKDIR }).map_err(failed)?;
        let target: IShellItem =
            SHCreateItemFromParsingName(&HSTRING::from(dest.as_os_str()), None::<&IBindCtx>).map_err(failed)?;
        for name in names {
            let item = resolve(id, &join(path, name)).ok_or_else(gone)?;
            operation.CopyItem(&item, &target, PCWSTR::null(), None::<&IFileOperationProgressSink>).map_err(failed)?;
        }
        operation.PerformOperations().map_err(failed)?;
        if operation.GetAnyOperationsAborted().map(|aborted| aborted.as_bool()).unwrap_or(false) {
            return Err(t("Das Kopieren vom Gerät wurde abgebrochen.", "Copying from the device was cancelled.").to_owned());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_cabled_devices_count_as_devices() {
        assert!(is_device_path("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}\\\\\\?\\usb#vid_04e8&pid_6860#r5#{6ac27878-a6fa}"));
        assert!(!is_device_path("C:\\"));
        assert!(!is_device_path("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}\\uuid:0f99d1ee-1463-4611-9d8b"));
    }

    #[test]
    fn times_from_the_device_become_unix_seconds() {
        // 1. Januar 2026, 00:00 UTC
        let ticks = (1_767_225_600u64 + 11_644_473_600) * 10_000_000;
        let time = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
        assert_eq!(filetime_secs(time), Some(1_767_225_600));
        assert_eq!(filetime_secs(FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }), None);
    }

    /// Ein gewöhnlicher Ordner dient als Gerät: aufgelistet und kopiert wird über dieselben Aufrufe der Shell.
    #[test]
    fn a_folder_standing_in_for_a_device_is_listed_and_copied_from() {
        let base = std::env::temp_dir().join(format!("tc-device-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let camera = base.join("gerät").join("DCIM").join("Camera");
        let out = base.join("ziel");
        std::fs::create_dir_all(&camera).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(camera.join("IMG_0001.jpg"), b"bild").unwrap();
        std::fs::write(camera.join("notiz.txt"), b"text!").unwrap();
        std::fs::create_dir_all(camera.join("Serie")).unwrap();
        std::fs::write(camera.join("Serie").join("a.jpg"), b"a").unwrap();
        let id = base.join("gerät").to_string_lossy().into_owned();

        let top: Vec<(String, bool)> = list(&id, "/").unwrap().into_iter().map(|e| (e.name, e.is_dir)).collect();
        assert_eq!(top, [("DCIM".to_owned(), true)]);
        let mut inside: Vec<(String, bool, u64)> =
            list(&id, "/DCIM/Camera").unwrap().into_iter().map(|e| (e.name, e.is_dir, e.size)).collect();
        inside.sort();
        assert_eq!(inside, [("IMG_0001.jpg".to_owned(), false, 4), ("Serie".to_owned(), true, 0), ("notiz.txt".to_owned(), false, 5)]);
        assert!(list(&id, "/DCIM/Camera").unwrap().iter().all(|e| e.is_dir || e.modified.is_some()));
        assert!(list(&id, "/gibt/es/nicht").is_err());

        copy_out(&id, "/DCIM/Camera", &["IMG_0001.jpg".to_owned(), "Serie".to_owned()], &out, true).unwrap();
        assert_eq!(std::fs::read(out.join("IMG_0001.jpg")).unwrap(), b"bild");
        assert_eq!(std::fs::read(out.join("Serie").join("a.jpg")).unwrap(), b"a");
        assert!(!out.join("notiz.txt").exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
