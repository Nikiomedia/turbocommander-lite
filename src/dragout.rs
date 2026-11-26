//! Dateien aus TurboCommander heraus in andere Programme ziehen (Windows-Drag & Drop per OLE).
//!
//! Innerhalb des Fensters läuft das Ziehen über die eigene Oberfläche (mit Zielhervorhebung). Sobald die Maus
//! über einem fremden Fenster steht, übernimmt Windows: dafür wird hier ein Datenobjekt der Shell erzeugt –
//! dasselbe, das auch der Explorer beim Ziehen verwendet, also mit allen Formaten, die Programme erwarten.

use std::path::PathBuf;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{IBindCtx, IDataObject};
use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, IDropSource};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    BHID_DataObject, ILFree, IShellItemArray, SHCreateShellItemArrayFromIDLists, SHDoDragDrop, SHParseDisplayName,
};
use windows::core::HSTRING;

/// Datenobjekt der Windows-Shell für die angegebenen Dateien und Ordner.
pub fn data_object(paths: &[PathBuf]) -> Option<IDataObject> {
    unsafe {
        let mut pidls: Vec<*mut ITEMIDLIST> = Vec::with_capacity(paths.len());
        for path in paths {
            let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
            let parsed = SHParseDisplayName(&HSTRING::from(path.as_os_str()), None::<&IBindCtx>, &mut pidl, 0, None);
            if parsed.is_ok() && !pidl.is_null() {
                pidls.push(pidl);
            }
        }
        let list: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const ITEMIDLIST).collect();
        let object = if list.is_empty() {
            None
        } else {
            SHCreateShellItemArrayFromIDLists(&list)
                .and_then(|array: IShellItemArray| array.BindToHandler::<_, IDataObject>(None::<&IBindCtx>, &BHID_DataObject))
                .ok()
        };
        for pidl in pidls {
            ILFree(Some(pidl as *const ITEMIDLIST));
        }
        object
    }
}

/// Übergibt das Ziehen an Windows. Blockiert, bis die Maustaste losgelassen wurde, und muss im Thread
/// des Fensters laufen. Liefert, was das Ziel getan hat: 1 = kopiert, 2 = verschoben, 0 = abgebrochen.
pub fn drag_out(hwnd: isize, paths: &[PathBuf]) -> u32 {
    let Some(object) = data_object(paths) else { return 0 };
    let allowed = DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK;
    unsafe { SHDoDragDrop(Some(HWND(hwnd as *mut _)), &object, None::<&IDropSource>, allowed).map(|e| e.0).unwrap_or(0) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::ffi::OsStringExt;
    use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
    use windows::Win32::System::Ole::{OleInitialize, ReleaseStgMedium};

    /// Enthält das Datenobjekt die Dateiliste (CF_HDROP) so, wie andere Programme sie lesen?
    #[test]
    fn data_object_carries_the_file_list() {
        let dir = std::env::temp_dir().join(format!("tc-drag-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Ordner äöü")).unwrap();
        std::fs::write(dir.join("eins.mkv"), "x").unwrap();
        std::fs::write(dir.join("zwei mit Leerzeichen.mkv"), "y").unwrap();
        let paths = vec![dir.join("eins.mkv"), dir.join("zwei mit Leerzeichen.mkv"), dir.join("Ordner äöü")];

        unsafe {
            let _ = OleInitialize(None);
            let object = data_object(&paths).expect("kein Datenobjekt");
            let format = FORMATETC {
                cfFormat: 15, // CF_HDROP
                ptd: std::ptr::null_mut(),
                dwAspect: DVASPECT_CONTENT.0,
                lindex: -1,
                tymed: TYMED_HGLOBAL.0 as u32,
            };
            let mut medium = object.GetData(&format).expect("CF_HDROP fehlt");
            let hdrop = medium.u.hGlobal.0;
            use windows_sys::Win32::UI::Shell::DragQueryFileW;
            let count = DragQueryFileW(hdrop, u32::MAX, std::ptr::null_mut(), 0);
            let mut got = Vec::new();
            for i in 0..count {
                let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0) as usize;
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(hdrop, i, buf.as_mut_ptr(), buf.len() as u32);
                got.push(PathBuf::from(std::ffi::OsString::from_wide(&buf[..len])));
            }
            ReleaseStgMedium(&mut medium);
            assert_eq!(got, paths);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_files_give_no_object() {
        unsafe {
            let _ = OleInitialize(None);
        }
        assert!(data_object(&[PathBuf::from("Z:\\gibt\\es\\nicht\\datei.xyz")]).is_none());
        assert!(data_object(&[]).is_none());
    }
}
