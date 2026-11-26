//! Die Lite-Ausgabe und was die Vollversion von TurboCommander zusätzlich kann.

use crate::icons;
use crate::lang::t;

/// Name dieser Ausgabe, wie er im Programm erscheint
pub const NAME: &str = "TurboCommander Lite";

/// Wo es die Vollversion gibt: der Beitrag auf Patreon. Leer: der Knopf „Zur Vollversion" fehlt.
pub const FULL_VERSION_URL: &str = "https://www.patreon.com/nikomedia/posts/turbocommander-0-176452581";

/// Was die Vollversion zusätzlich kann: (Symbol, Titel, Beschreibung)
pub fn full_features() -> [(&'static str, &'static str, &'static str); 12] {
    [
        (
            icons::TEXTBOX,
            t("Mehrfach umbenennen", "Batch rename"),
            t("Ersetzen (auch RegEx), nummerieren, Datum einsetzen – mit Live-Vorschau und Vorlagen", "Replace (also RegEx), number, insert dates – with live preview and presets"),
        ),
        (icons::GLOBE, "FTP, FTPS, SFTP", t("Server wie Ordner öffnen, Passwörter verschlüsselt gespeichert", "Open servers like folders, passwords stored encrypted")),
        (
            icons::ARROWS_LEFT_RIGHT,
            t("Ordner vergleichen und abgleichen", "Compare and sync folders"),
            t("Samt Unterordnern, auch gegen Server, in eine oder beide Richtungen", "Including subfolders, also against servers, one way or both"),
        ),
        (
            icons::PLAY,
            t("Videos und Musik abspielen", "Play videos and music"),
            t("Direkt in der Schnellansicht, mit Lautstärke und Zeitleiste", "Right in the quick view, with volume and timeline"),
        ),
        (
            icons::FILM_STRIP,
            t("Media Info und Medienspalten", "Media info and media columns"),
            t("Codecs, Bitraten, Tonspuren; Auflösung und Dauer als Spalten", "Codecs, bitrates, audio tracks; resolution and length as columns"),
        ),
        (
            icons::MAGNIFYING_GLASS,
            t("Suche in Dateien", "Search in files"),
            t("Text in Dateien und Office-Dokumenten, nach Größe und Datum", "Text in files and Office documents, by size and date"),
        ),
        (
            icons::LOCK_KEY,
            t("Was sperrt diese Datei?", "What is locking this file?"),
            t("Zeigt das Programm, das eine Datei offen hält, und gibt sie frei", "Shows the program keeping a file open and releases it"),
        ),
        (
            icons::WRENCH,
            t("Eigene Werkzeuge", "Custom tools"),
            t("Markierte Dateien mit Strg+1 … 9 an beliebige Programme übergeben", "Hand selected files to any program with Ctrl+1 … 9"),
        ),
        (
            icons::STACK,
            t("Arbeitsbereiche", "Workspaces"),
            t("Die Tabs beider Fenster unter einem Namen speichern", "Save the tabs of both panes under one name"),
        ),
        (
            icons::IMAGES_SQUARE,
            t("Galerie", "Gallery"),
            t("Alle Bilder und Videos nach Monaten, samt Telefon", "All pictures and videos by month, phone included"),
        ),
        (icons::FILE_ZIP, t("7z, RAR und TAR", "7z, RAR and TAR"), t("Weitere Archive wie Ordner öffnen und entpacken", "Open and extract more archive formats like folders")),
        (
            icons::ARROWS_DOWN_UP,
            t("Übertragungsliste", "Transfer list"),
            t("Aufträge anhalten, umsortieren und dort fortsetzen, wo sie stehen blieben", "Pause, reorder and resume transfers where they stopped"),
        ),
    ]
}
