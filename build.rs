// Erzeugt das Symbol der exe aus derselben Zeichenroutine wie das Fenstersymbol
// und bettet es samt Versionsangaben, Herausgeber und Anwendungsmanifest als Windows-Ressource ein.

include!("src/icon_art.rs");

/// ICO-Datei mit unkomprimierten 32-Bit-Bildern (von jeder Windows-Version lesbar).
fn build_ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<(u32, Vec<u8>)> = sizes
        .iter()
        .map(|&size| {
            let rgba = render_icon(size);
            let mask_row = size.div_ceil(32) * 4;
            let mut dib = Vec::with_capacity((40 + size * size * 4 + mask_row * size) as usize);
            dib.extend_from_slice(&40u32.to_le_bytes());
            dib.extend_from_slice(&(size as i32).to_le_bytes());
            dib.extend_from_slice(&((size * 2) as i32).to_le_bytes()); // Bild + Maske
            dib.extend_from_slice(&1u16.to_le_bytes());
            dib.extend_from_slice(&32u16.to_le_bytes());
            dib.extend_from_slice(&[0u8; 24]);
            for y in (0..size).rev() {
                for x in 0..size {
                    let i = ((y * size + x) * 4) as usize;
                    dib.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
                }
            }
            dib.extend(std::iter::repeat_n(0u8, (mask_row * size) as usize));
            (size, dib)
        })
        .collect();

    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len() as u32;
    for (size, dib) in &images {
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(dib.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += dib.len() as u32;
    }
    for (_, dib) in images {
        out.extend_from_slice(&dib);
    }
    out
}

/// Anwendungsmanifest: weist die exe als gewöhnliches Programm aus, das mit den Rechten des
/// angemeldeten Nutzers läuft. Es enthält bewusst nur das, was am Verhalten nichts ändert
/// (keine DPI-, Kompatibilitäts- oder Steuerelement-Einträge).
fn manifest() -> String {
    let part = |name: &str| std::env::var(name).unwrap_or_else(|_| "0".into());
    let version = format!(
        "{}.{}.{}.0",
        part("CARGO_PKG_VERSION_MAJOR"),
        part("CARGO_PKG_VERSION_MINOR"),
        part("CARGO_PKG_VERSION_PATCH")
    );
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="TurboCommanderLite" version="{version}"/>
  <description>TurboCommander Lite</description>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#
    )
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon_art.rs");
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = out_dir.join("turbocommander.ico");
    std::fs::write(&ico, build_ico(&[16, 20, 24, 32, 40, 48, 64, 128, 256])).unwrap();

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap());
    res.set("ProductName", "TurboCommander Lite");
    res.set("FileDescription", "TurboCommander Lite");
    res.set("OriginalFilename", "TurboCommanderLite.exe");
    // Herausgeber und Urheber wie in LICENSE; ohne Sonderzeichen, damit die Ressource überall gleich gelesen wird.
    res.set("CompanyName", "Niko Schatz");
    res.set("LegalCopyright", "Copyright (c) 2026 Niko Schatz");
    res.set_manifest(&manifest());
    if let Err(e) = res.compile() {
        println!("cargo:warning=Das Symbol konnte nicht in die exe eingebettet werden: {e}");
    }
}
