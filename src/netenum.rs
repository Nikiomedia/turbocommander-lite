//! Netzwerkumgebung auflisten: Computer im Netz und die Freigaben eines Computers.
//!
//! Gefragt wird die Windows-Shell – dieselbe Quelle wie der Ordner „Netzwerk" im Explorer. Damit erscheinen
//! auch Geräte, die Windows über die Netzwerkerkennung findet (NAS, andere PCs), und gespeicherte
//! Zugangsdaten werden mitbenutzt. Die Aufrufe können einige Sekunden dauern und gehören in einen Hintergrund-Thread.

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx};
use windows::Win32::UI::Shell::{
    BHID_EnumItems, FOLDERID_NetworkFolder, IEnumShellItems, IShellItem, KF_FLAG_DEFAULT, SHCreateItemFromParsingName,
    SHGetKnownFolderItem, SIGDN, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY,
};
use windows::core::HSTRING;

use crate::tf;

/// COM für die Dauer eines Aufrufs bereitstellen (jeder Thread braucht das selbst).
pub(crate) struct ComGuard(bool);

impl ComGuard {
    pub(crate) fn new() -> Self {
        Self(unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok())
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

pub(crate) fn name_of(item: &IShellItem, kind: SIGDN) -> Option<String> {
    unsafe {
        let raw = item.GetDisplayName(kind).ok()?;
        let text = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const core::ffi::c_void));
        text
    }
}

/// (Anzeigename, Pfad wie in der Adresszeile) aller Einträge eines Shell-Ordners.
pub(crate) fn children(item: &IShellItem) -> windows::core::Result<Vec<(String, String)>> {
    unsafe {
        let list: IEnumShellItems = item.BindToHandler(None::<&IBindCtx>, &BHID_EnumItems)?;
        let mut out = Vec::new();
        loop {
            let mut slot: [Option<IShellItem>; 1] = [None];
            let mut fetched = 0u32;
            if list.Next(&mut slot, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            let Some(child) = slot[0].take() else { break };
            if let (Some(name), Some(path)) = (name_of(&child, SIGDN_NORMALDISPLAY), name_of(&child, SIGDN_DESKTOPABSOLUTEPARSING)) {
                out.push((name, path));
            }
        }
        Ok(out)
    }
}

/// Die Ordner aus dem Schnellzugriff des Explorers – angeheftete und häufig verwendete, in seiner Reihenfolge:
/// (Name, wie ihn der Explorer zeigt, Pfad).
pub fn quick_access() -> Vec<(String, std::path::PathBuf)> {
    let _com = ComGuard::new();
    let home = HSTRING::from("shell:::{679f85cb-0220-4080-b29b-5540cc05aab6}");
    let found = unsafe { SHCreateItemFromParsingName::<_, _, IShellItem>(&home, None::<&IBindCtx>) }
        .and_then(|item| children(&item))
        .unwrap_or_default();
    // Dort stehen auch zuletzt verwendete Dateien; gefragt sind nur die Ordner.
    found
        .into_iter()
        .map(|(name, path)| (name, std::path::PathBuf::from(path)))
        .filter(|(_, path)| path.is_absolute() && path.is_dir())
        .collect()
}

/// Ergebnis der letzten Suche – die Suche dauert oft über zehn Sekunden, da hilft ein Gedächtnis.
static LAST_COMPUTERS: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);

pub fn remembered_computers() -> Option<Vec<String>> {
    LAST_COMPUTERS.lock().ok().and_then(|guard| guard.clone())
}

/// Sucht die Computer und merkt sich das Ergebnis.
pub fn find_computers() -> Result<Vec<String>, String> {
    let found = computers()?;
    if let Ok(mut guard) = LAST_COMPUTERS.lock() {
        *guard = Some(found.clone());
    }
    Ok(found)
}

/// Fügt zwei Namenslisten ohne Dubletten zusammen (Groß-/Kleinschreibung egal), alphabetisch sortiert.
pub fn merge_names(a: &[String], b: &[String]) -> Vec<String> {
    let mut all: Vec<String> = a.iter().chain(b).cloned().collect();
    all.sort_by_key(|n| n.to_lowercase());
    all.dedup_by_key(|n| n.to_lowercase());
    all
}

/// Namen der Computer in der Netzwerkumgebung (ohne führende `\\`).
pub fn computers() -> Result<Vec<String>, String> {
    let _com = ComGuard::new();
    let found = unsafe { SHGetKnownFolderItem::<IShellItem>(&FOLDERID_NetworkFolder, KF_FLAG_DEFAULT, None) }
        .and_then(|folder| children(&folder))
        .map_err(|e| tf!("Die Netzwerkumgebung lässt sich nicht lesen: {}", "The network cannot be browsed: {}", e.message()))?;
    let mut names: Vec<String> = found
        .into_iter()
        // Nur Computer mit Dateifreigaben haben einen Pfad der Form \\Name; Mediengeräte und Drucker nicht.
        .filter_map(|(_, path)| {
            let server = path.strip_prefix("\\\\")?;
            (!server.is_empty() && !server.contains('\\')).then(|| server.to_owned())
        })
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup_by_key(|n| n.to_lowercase());
    Ok(names)
}

/// Namen der Freigaben eines Computers.
pub fn shares(server: &str) -> Result<Vec<String>, String> {
    let _com = ComGuard::new();
    let root = format!("\\\\{server}");
    let found = unsafe { SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(root.as_str()), None::<&IBindCtx>) }
        .and_then(|item| children(&item))
        .map_err(|e| {
            tf!(
                "„{server}\" ist nicht erreichbar oder verlangt eine Anmeldung ({}). \
                 Melde dich einmal im Windows-Explorer an und speichere dort die Zugangsdaten.",
                "“{server}” cannot be reached or requires you to sign in ({}). \
                 Sign in once in Windows File Explorer and save the credentials there.",
                e.message().trim()
            )
        })?;
    let prefix = format!("{}\\", root.to_lowercase());
    let mut names: Vec<String> = found
        .into_iter()
        .filter_map(|(_, path)| {
            let lower = path.to_lowercase();
            let share = lower.strip_prefix(&prefix)?;
            (!share.is_empty() && !share.contains('\\')).then(|| path[prefix.len()..].to_owned())
        })
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Probe am echten Netzwerk; gibt nur Anzahlen und gekürzte Namen aus.
    #[test]
    #[ignore = "fragt die echte Netzwerkumgebung ab"]
    fn probe_network() {
        let mask = |s: &str| format!("{}…({} Zeichen)", s.chars().take(2).collect::<String>(), s.chars().count());
        let start = std::time::Instant::now();
        let found = computers();
        println!("Computer: {:?} nach {:?}", found.as_ref().map(|v| v.iter().map(|n| mask(n)).collect::<Vec<_>>()), start.elapsed());
        if let Ok(list) = found {
            for server in list.iter().take(3) {
                let start = std::time::Instant::now();
                let result = shares(server);
                println!(
                    "Freigaben von {}: {:?} nach {:?}",
                    mask(server),
                    result.map(|v| v.len()).map_err(|e| e.chars().take(60).collect::<String>()),
                    start.elapsed()
                );
            }
        }
    }
}
