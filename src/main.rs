//! TurboCommander Lite – schneller Zwei-Fenster-Dateimanager für Windows (kostenlose Ausgabe).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod archive;
mod backup;
mod bin;
mod config;
mod device;
mod dialogs;
mod dragout;
mod dropin;
mod edition;
mod icon_art;
mod icons;
mod lang;
mod netenum;
mod ops;
mod pane;
mod preview;
mod rename;
mod script;
mod search;
mod shellicon;
mod theme;
mod thumbs;
mod util;
mod vfs;
mod widgets;
mod win;
mod zipfs;

#[cfg(test)]
mod ops_test;

use std::path::PathBuf;

use eframe::egui;

use crate::config::Config;
use crate::theme::ThemeMode;

/// Startparameter – gedacht für automatische Bildschirmfotos beim Entwickeln:
/// `--shot bild.png --left C:\ --right D:\ --theme dark --size 1200x800 --demo rename --side`
fn parse_args() -> app::Args {
    let mut args = app::Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--shot" => args.shot = it.next().map(PathBuf::from),
            "--left" => args.left = it.next(),
            "--right" => args.right = it.next(),
            "--demo" => args.demo = it.next(),
            "--script" => args.script = it.next(),

            "--lang" => {
                args.lang = match it.next().as_deref() {
                    Some("de") => Some(lang::Lang::De),
                    Some("en") => Some(lang::Lang::En),
                    _ => None,
                }
            }
            // Schreibt das Programmsymbol als PNG (für die Projektseite) und beendet sich.
            "--export-icon" => {
                if let Some(path) = it.next() {
                    let size = 256;
                    let _ = image::save_buffer(path, &icon_art::render_icon(size), size, size, image::ColorType::Rgba8);
                }
                std::process::exit(0);
            }
            // Für Prüfläufe: dieser Ordner gilt als Cloud-Ordner (Zeichen „auf dem PC" an den Dateien).
            "--cloud" => {
                if let Some(folder) = it.next() {
                    let _ = app::TEST_CLOUD.set(PathBuf::from(folder));
                }
            }
            // Für Prüfläufe: dieser Ordner erscheint unter „Dieser PC" als Gerät am Kabel.
            "--device" => {
                if let Some(folder) = it.next() {
                    let _ = device::TEST_DEVICE.set(folder);
                }
            }
            "--side" => args.side_by_side = true,
            "--theme" => {
                args.theme = match it.next().as_deref() {
                    Some("dark") => Some(ThemeMode::Dark),
                    Some("light") => Some(ThemeMode::Light),
                    _ => None,
                }
            }
            "--size" => {
                args.size = it.next().and_then(|s| {
                    let (w, h) = s.split_once('x')?;
                    Some((w.parse().ok()?, h.parse().ok()?))
                })
            }
            // Ein Ordner als einfacher Parameter öffnet ihn im ersten Fenster.
            other if !other.starts_with("--") && args.left.is_none() => args.left = Some(other.to_owned()),
            _ => {}
        }
    }
    args
}

/// In der Release-exe gibt es keine Konsole: Abstürze landen in einer Protokolldatei.
fn install_panic_log() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let dir = Config::dir();
        let _ = std::fs::create_dir_all(&dir);
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let text = format!("[{stamp}] {info}\n");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("absturz.log"))
            .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()));
        default(info);
    }));
}

fn main() -> eframe::Result<()> {
    let process_start = std::time::Instant::now();
    install_panic_log();
    let mut args = parse_args();
    args.process_start = Some(process_start);
    let shot = args.shot.is_some();
    // Die Sprache muss feststehen, bevor die ersten Texte entstehen (schon die Voreinstellungen enthalten welche):
    // erst die von Windows, dann – falls gewählt – die aus den Einstellungen oder vom Startparameter.
    lang::set_lang(args.lang.unwrap_or_else(win::system_lang));
    let cfg = if shot { Config::default() } else { Config::load() };
    if let Some(chosen) = args.lang.or(cfg.lang) {
        lang::set_lang(chosen);
    }

    let icon = egui::IconData { rgba: icon_art::render_icon(64), width: 64, height: 64 };
    let size = args.size.unwrap_or((1180.0, 860.0));
    let viewport = egui::ViewportBuilder::default()
        .with_title("TurboCommander Lite")
        .with_app_id("TurboCommanderLite")
        .with_icon(icon)
        .with_min_inner_size([520.0, 380.0])
        .with_inner_size([size.0, size.1])
        .with_drag_and_drop(true);
    let options = eframe::NativeOptions {
        viewport,
        // OpenGL: im Vergleich mit wgpu/DirectX auf diesem System sechsmal schneller gestartet und ein Viertel des Speichers.
        renderer: eframe::Renderer::Glow,
        // Fenstergröße und -position merkt sich eframe selbst.
        persist_window: !shot,
        // Im Bildschirmfoto-Modus einen eigenen, leeren Speicherort nehmen, damit die verlangte Größe gilt.
        persistence_path: shot.then(|| std::env::temp_dir().join(format!("TurboCommanderLite-shot-{}.ron", std::process::id()))),
        ..Default::default()
    };
    // Der Name bestimmt auch den Speicherort von eframe (Fensterlage): getrennt von der Vollversion.
    eframe::run_native(
        "TurboCommander Lite",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, cfg, args)))),
    )
}
