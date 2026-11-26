//! Dialoge: Eingaben, Bestätigungen, Konflikte, Tastenkürzel.

use std::path::PathBuf;

use eframe::egui::{self, Align, Align2, Color32, CornerRadius, Id, Key, Layout, RichText, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::app::App;
use crate::backup::BackupFile;
use crate::icons;
use crate::lang::{key, t};
use crate::ops::{Answer, Conflict, Request, Source};
use crate::pane::select_all;
use crate::tf;
use crate::theme::{self, Palette};
use crate::util::{fmt_size, fold_key, invalid_name_reason, split_ext};
use crate::vfs::{Location, Msg, io_msg};
use crate::widgets::{checkbox, danger_button, plain_button, primary_button};
use crate::win;

pub enum InputKind {
    NewFolder,
    NewFile,
    Pack(Vec<PathBuf>),
    /// Eigener Name für den Favoriten mit dieser Nummer; der Ordner dahinter bleibt, wie er heißt
    Favorite(usize),
}

pub enum Dialog {
    /// Rückfrage vor dem Zurücknehmen einer Dateioperation; `title` nennt den Auftrag
    Undo { title: String, records: Vec<crate::ops::Undo> },
    Input { kind: InputKind, title: &'static str, value: String, fresh: bool },
    /// `no_more`: das Kästchen „künftig ohne Rückfrage" ist angekreuzt
    /// `bin`: das Laufwerk, dessen Papierkorb über `permanent` entscheidet (sonst steht es von vornherein fest)
    Delete { sources: Vec<Source>, permanent: bool, bin: Option<String>, no_more: bool },
    /// `no_more`: das Kästchen „künftig ohne Rückfrage" ist angekreuzt
    Transfer { sources: Vec<Source>, dest: String, mv: bool, fresh: bool, error: Option<String>, no_more: bool },
    /// Einstellen, wovor nachgefragt wird
    Asking,
    /// Rückfrage vor dem endgültigen Löschen aus dem Papierkorb: `names` und `ids` der gewählten Elemente, oder
    /// mit `all` der ganze Papierkorb
    Bin { names: Vec<String>, ids: Vec<std::ffi::OsString>, all: bool },
    Errors { title: String, lines: Vec<String> },
    Shortcuts,
    /// Schließen, während noch Aufgaben laufen
    Quit,
    /// Was die Vollversion zusätzlich kann
    FullVersion,
    BackupExport,
    BackupImport { path: PathBuf, file: Box<BackupFile> },
}

enum Step {
    Keep,
    Close,
    Then(Box<dyn FnOnce(&mut App, &egui::Context)>),
}

fn modal<R>(ctx: &egui::Context, p: &Palette, id: &str, width: f32, add: impl FnOnce(&mut Ui) -> R) -> (R, bool) {
    let resp = egui::Modal::new(Id::new(id)).frame(theme::dialog_frame(p)).show(ctx, |ui| {
        ui.set_width(width.min(ctx.content_rect().width() - 60.0).max(240.0));
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
        add(ui)
    });
    let close = resp.should_close();
    (resp.inner, close)
}

fn title(ui: &mut Ui, p: &Palette, icon: &str, color: Color32, text: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
        ui.painter().rect_filled(rect, CornerRadius::same(10), theme::mix(p.elevated, color, 0.16));
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, theme::font(18.0), color);
        ui.add_space(2.0);
        ui.label(RichText::new(text).font(theme::semibold(16.0)).color(p.text));
    });
    ui.add_space(2.0);
}

fn names_summary(ui: &mut Ui, p: &Palette, sources: &[Source]) {
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border_soft))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 3.0;
            for s in sources.iter().take(5) {
                ui.horizontal(|ui| {
                    let (glyph, color) = if s.is_dir { (icons::FOLDER, p.folder) } else { (icons::FILE, p.doc) };
                    ui.label(RichText::new(glyph).size(14.0).color(color));
                    ui.add(egui::Label::new(RichText::new(&s.name).color(p.text)).truncate());
                });
            }
            if sources.len() > 5 {
                ui.label(RichText::new(tf!("… und {} weitere", "… and {} more", sources.len() - 5)).color(p.text_faint));
            }
        });
}

/// „1 Favorit" / „3 Favoriten"
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn count_text(n: usize) -> String {
    if n == 1 { t("1 Element", "1 item").to_owned() } else { tf!("{n} Elemente", "{n} items") }
}

impl App {
    pub(crate) fn dialogs_ui(&mut self, ctx: &egui::Context) {
        self.conflict_ui(ctx);
        self.dialog_ui(ctx);
    }

    fn dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(mut dlg) = self.dialog.take() else { return };
        let p = self.pal.clone();
        let step = match &mut dlg {
            Dialog::Input { kind, title: heading, value, fresh } => self.input_ui(ctx, &p, kind, heading, value, fresh),
            Dialog::Delete { sources, permanent, no_more, .. } => Self::delete_ui(ctx, &p, sources, *permanent, no_more),
            Dialog::Transfer { sources, dest, mv, fresh, error, no_more } => {
                self.transfer_ui(ctx, &p, sources, dest, *mv, fresh, error, no_more)
            }
            Dialog::Asking => self.asking_ui(ctx, &p),
            Dialog::Errors { title: heading, lines } => Self::errors_ui(ctx, &p, heading, lines),
            Dialog::Shortcuts => Self::shortcuts_ui(ctx, &p),
            Dialog::Quit => Self::quit_ui(ctx, &p),
            Dialog::FullVersion => Self::full_version_ui(ctx, &p),
            Dialog::Bin { names, ids, all } => Self::bin_ui(ctx, &p, names, ids, *all),
            Dialog::BackupExport => self.backup_export_ui(ctx, &p),
            Dialog::BackupImport { path, file } => Self::backup_import_ui(ctx, &p, path, file),
            Dialog::Undo { title: heading, records } => Self::undo_ui(ctx, &p, heading, records),
        };
        match step {
            Step::Keep => self.dialog = Some(dlg),
            Step::Close => {}
            Step::Then(f) => f(self, ctx),
        }
    }

    fn input_ui(
        &self,
        ctx: &egui::Context,
        p: &Palette,
        kind: &mut InputKind,
        heading: &str,
        value: &mut String,
        fresh: &mut bool,
    ) -> Step {
        let tab = self.panes[self.active].tab();
        let trimmed = value.trim().to_owned();
        let final_name = match kind {
            InputKind::Pack(_) if !trimmed.to_ascii_lowercase().ends_with(".zip") => format!("{trimmed}.zip"),
            _ => trimmed.clone(),
        };
        let problem: Option<String> = if trimmed.is_empty() {
            Some(String::new())
        } else if matches!(kind, InputKind::Favorite(_)) {
            // Nur eine Beschriftung: alles ist erlaubt, auch ein Name, den schon ein anderer Favorit trägt.
            None
        } else if let Some(reason) = invalid_name_reason(&final_name) {
            Some(reason.to_owned())
        } else if tab.entries.iter().any(|e| e.key == fold_key(&final_name)) {
            Some(match kind {
                InputKind::Pack(_) => t("Dieses Archiv existiert bereits", "This archive already exists").to_owned(),
                _ => t("Ein Element mit diesem Namen existiert bereits", "An item with this name already exists").to_owned(),
            })
        } else {
            None
        };
        let (icon, action) = match kind {
            InputKind::NewFolder => (icons::FOLDER_PLUS, t("Erstellen", "Create")),
            InputKind::NewFile => (icons::FILE_PLUS, t("Erstellen", "Create")),
            InputKind::Pack(_) => (icons::FILE_ZIP, t("Packen", "Pack")),
            InputKind::Favorite(_) => (icons::STAR, t("Umbenennen", "Rename")),
        };
        let about = match kind {
            // Wohin der Favorit führt – der Name auf dem Knopf sagt das danach nicht mehr unbedingt.
            InputKind::Favorite(index) => self.cfg.favorites.get(*index).map(|fav| fav.loc.display()).unwrap_or_default(),
            _ => tf!("In „{}\"", "In “{}”", tab.loc.leaf()),
        };
        let id = Id::new("dialog-input");
        let ((confirm, cancel), close) = modal(ctx, p, "dlg-input", 420.0, |ui| {
            title(ui, p, icon, p.accent, heading);
            if !about.is_empty() {
                ui.label(RichText::new(&about).color(p.text_dim));
            }
            if *fresh {
                ui.memory_mut(|m| m.request_focus(id));
            }
            let resp = ui.add(
                egui::TextEdit::singleline(value)
                    .id(id)
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::symmetric(10, 8))
                    .font(theme::font(14.0)),
            );
            // Im allerersten Frame misst egui den Dialog nur aus und vergibt noch keinen Fokus:
            // deshalb so lange anfordern, bis das Feld ihn wirklich hat.
            if *fresh && resp.has_focus() {
                *fresh = false;
                select_all(ui.ctx(), id, value.chars().count());
            }
            let entered = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let message = problem.clone().unwrap_or_default();
            ui.label(RichText::new(if message.is_empty() { " " } else { message.as_str() }).small().color(p.danger));
            let mut result = (false, false);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if primary_button(ui, p, action, problem.is_none()).clicked() || (entered && problem.is_none()) {
                    result.0 = true;
                }
                if plain_button(ui, t("Abbrechen", "Cancel")).clicked() {
                    result.1 = true;
                }
            });
            if entered && problem.is_some() {
                ui.memory_mut(|m| m.request_focus(id));
            }
            result
        });
        if cancel || close {
            return Step::Close;
        }
        if !confirm {
            return Step::Keep;
        }
        let kind = std::mem::replace(kind, InputKind::NewFolder);
        Step::Then(Box::new(move |app, _| app.finish_input(kind, final_name)))
    }

    fn delete_ui(ctx: &egui::Context, p: &Palette, sources: &[Source], permanent: bool, no_more: &mut bool) -> Step {
        let ((confirm, cancel), close) = modal(ctx, p, "dlg-delete", 430.0, |ui| {
            title(ui, p, icons::TRASH, p.danger, &tf!("{} löschen?", "Delete {}?", count_text(sources.len())));
            names_summary(ui, p, sources);
            let note = if permanent {
                t(
                    "Die Elemente werden endgültig gelöscht und landen nicht im Papierkorb.",
                    "The items will be deleted permanently and will not go to the Recycle Bin.",
                )
            } else {
                t("Die Elemente werden in den Papierkorb verschoben.", "The items will be moved to the Recycle Bin.")
            };
            ui.label(RichText::new(note).color(p.text_dim));
            ui.add_space(2.0);
            let quiet = if permanent {
                t("Künftig ohne Rückfrage endgültig löschen", "From now on, delete permanently without asking")
            } else {
                t("Künftig ohne Rückfrage in den Papierkorb", "From now on, move to the Recycle Bin without asking")
            };
            let tick = checkbox(ui, p, no_more, quiet);
            // Enter gehört der Schaltfläche: ein angewähltes Kästchen würde davon wieder umgeschaltet.
            if tick.has_focus() {
                tick.surrender_focus();
            }
            tick.on_hover_text(t(
                "Die Rückfrage lässt sich im Menü unter „Rückfragen\" wieder einschalten.",
                "Asking can be switched on again in the menu under “Confirmations”.",
            ));
            ui.add_space(4.0);
            let mut result = (false, false);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if permanent {
                    t("Endgültig löschen", "Delete permanently")
                } else {
                    t("In den Papierkorb", "Move to Recycle Bin")
                };
                if danger_button(ui, p, label).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                    result.0 = true;
                }
                if plain_button(ui, t("Abbrechen", "Cancel")).clicked() {
                    result.1 = true;
                }
            });
            result
        });
        if cancel || close {
            return Step::Close;
        }
        if !confirm {
            return Step::Keep;
        }
        let (sources, no_more) = (sources.to_vec(), *no_more);
        Step::Then(Box::new(move |app, _| {
            if no_more {
                app.stop_asking(if permanent { crate::config::Ask::Delete } else { crate::config::Ask::Trash });
            }
            app.jobs.start(Request::Delete { sources, permanent }, &app.bus);
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn transfer_ui(
        &self,
        ctx: &egui::Context,
        p: &Palette,
        sources: &[Source],
        dest: &mut String,
        mv: bool,
        fresh: &mut bool,
        error: &mut Option<String>,
        no_more: &mut bool,
    ) -> Step {
        let verb = if mv { t("verschieben", "Move") } else { t("kopieren", "Copy") };
        let id = Id::new("dialog-dest");
        let what = if mv { crate::config::Ask::Move } else { crate::config::Ask::Copy };
        // Ist die Rückfrage schon abgeschaltet und der Dialog fragt nur nach dem Ziel, gibt es nichts anzukreuzen.
        let offer_quiet = self.cfg.asks(what);
        let ((confirm, cancel), close) = modal(ctx, p, "dlg-transfer", 520.0, |ui| {
            let icon = if mv { icons::ARROW_RIGHT } else { icons::COPY };
            title(ui, p, icon, p.accent, &tf!("{0} {1}", "{1} {0}", count_text(sources.len()), verb));
            names_summary(ui, p, sources);
            ui.label(RichText::new(t("Nach", "To")).color(p.text_dim));
            if *fresh {
                ui.memory_mut(|m| m.request_focus(id));
            }
            let resp = ui.add(
                egui::TextEdit::singleline(dest)
                    .id(id)
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::symmetric(10, 8)),
            );
            if *fresh && resp.has_focus() {
                *fresh = false;
                let len = dest.chars().count();
                select_all(ui.ctx(), id, len);
            }
            if resp.changed() {
                *error = None;
            }
            let entered = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            ui.label(RichText::new(error.as_deref().unwrap_or(" ")).small().color(p.danger));
            if offer_quiet {
                let quiet = if mv {
                    t("Künftig ohne Rückfrage ins andere Fenster verschieben", "From now on, move to the other pane without asking")
                } else {
                    t("Künftig ohne Rückfrage ins andere Fenster kopieren", "From now on, copy to the other pane without asking")
                };
                let tick = checkbox(ui, p, no_more, quiet);
                // Enter gehört dem Zielfeld: nach dem Ankreuzen bekommt es den Fokus zurück, damit Enter bestätigt.
                if tick.clicked() || tick.has_focus() {
                    ui.memory_mut(|m| m.request_focus(id));
                }
                tick.on_hover_text(t(
                    "Die Rückfrage lässt sich im Menü unter „Rückfragen\" wieder einschalten.",
                    "Asking can be switched on again in the menu under “Confirmations”.",
                ));
                ui.add_space(4.0);
            }
            let mut result = (entered, false);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if primary_button(ui, p, if mv { t("Verschieben", "Move") } else { t("Kopieren", "Copy") }, true).clicked() {
                    result.0 = true;
                }
                if plain_button(ui, t("Abbrechen", "Cancel")).clicked() {
                    result.1 = true;
                }
            });
            result
        });
        if cancel || close {
            return Step::Close;
        }
        if !confirm {
            return Step::Keep;
        }
        match Location::parse(dest).filter(|l| l.writable()) {
            Some(target) => {
                let sources = sources.to_vec();
                let stop = offer_quiet && *no_more;
                Step::Then(Box::new(move |app, _| {
                    if stop {
                        app.stop_asking(what);
                    }
                    app.jobs.start(Request::Transfer { sources, dest: target, mv }, &app.bus);
                    // Wie bei Commandern üblich: nach dem Start ist die Markierung erledigt.
                    app.panes[app.active].tab_mut().clear_marks();
                }))
            }
            None => {
                *error = Some(
                    t(
                        "Dieser Zielordner wurde nicht gefunden oder ist schreibgeschützt.",
                        "This destination folder was not found or is read-only.",
                    )
                    .to_owned(),
                );
                *fresh = true;
                Step::Keep
            }
        }
    }

    fn errors_ui(
        ctx: &egui::Context,
        p: &Palette,
        heading: &str,
        lines: &[String],
    ) -> Step {
        let (ok, close) = modal(ctx, p, "dlg-errors", 540.0, |ui| {
            title(ui, p, icons::WARNING_CIRCLE, p.warn, heading);
            egui::Frame::new()
                .fill(p.surface)
                .stroke(Stroke::new(1.0, p.border_soft))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for line in lines.iter().take(200) {
                            ui.add(egui::Label::new(RichText::new(line).color(p.text_dim)).wrap());
                        }
                        if lines.len() > 200 {
                            ui.label(
                                RichText::new(tf!("… und {} weitere", "… and {} more", lines.len() - 200)).color(p.text_faint),
                            );
                        }
                    });
                });
            let mut ok = false;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ok = primary_button(ui, p, "OK", true).clicked() || ui.input(|i| i.key_pressed(Key::Enter));
            });
            ok
        });
        if ok || close { Step::Close } else { Step::Keep }
    }

    fn backup_export_ui(&self, ctx: &egui::Context, p: &Palette) -> Step {
        let ((confirm, cancel), close) = modal(ctx, p, "dlg-backup-save", 500.0, |ui| {
            title(ui, p, icons::FLOPPY_DISK, p.accent, t("Einstellungen sichern", "Back up settings"));
            let summary = tf!(
                "In die Sicherung kommen {}, die Spaltenbreiten und die Darstellung.",
                "The backup will contain {}, the column widths and the appearance.",
                plural(self.cfg.favorites.len(), t("Favorit", "favorite"), t("Favoriten", "favorites"))
            );
            ui.add(egui::Label::new(RichText::new(summary).color(p.text_dim)).wrap());
            ui.add(
                egui::Label::new(
                    RichText::new(t(
                        "Die Vollversion von TurboCommander kann diese Sicherung ebenfalls wiederherstellen.",
                        "The full version of TurboCommander can restore this backup as well.",
                    ))
                    .small()
                    .color(p.text_faint),
                )
                .wrap(),
            );
            ui.add_space(2.0);
            let mut result = (false, false);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if primary_button(ui, p, t("Speichern unter …", "Save as …"), true).clicked() {
                    result.0 = true;
                }
                if plain_button(ui, t("Abbrechen", "Cancel")).clicked() {
                    result.1 = true;
                }
            });
            result
        });
        if cancel || close {
            return Step::Close;
        }
        if !confirm {
            return Step::Keep;
        }
        Step::Then(Box::new(move |app, _| app.start_backup()))
    }

    fn backup_import_ui(ctx: &egui::Context, p: &Palette, path: &std::path::Path, file: &BackupFile) -> Step {
        let ((confirm, cancel), close) = modal(ctx, p, "dlg-backup-restore", 500.0, |ui| {
            title(ui, p, icons::ARROW_COUNTER_CLOCKWISE, p.accent, t("Sicherung wiederherstellen", "Restore backup"));
            egui::Frame::new()
                .fill(p.surface)
                .stroke(Stroke::new(1.0, p.border_soft))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 3.0;
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.add(egui::Label::new(RichText::new(name).font(theme::semibold(13.0)).color(p.text)).truncate());
                    ui.label(RichText::new(tf!("Erstellt am {}", "Created on {}", file.created)).color(p.text_dim));
                    ui.label(
                        RichText::new(plural(file.config.favorites.len(), t("Favorit", "favorite"), t("Favoriten", "favorites")))
                            .color(p.text_dim),
                    );
                });
            ui.add(
                egui::Label::new(
                    RichText::new(t(
                        "Favoriten und Darstellung werden durch den Stand der Sicherung ersetzt. \
                         Offene Tabs bleiben, wie sie sind.",
                        "Favorites and appearance are replaced with the state from the backup. \
                         Open tabs stay as they are.",
                    ))
                    .color(p.text_dim),
                )
                .wrap(),
            );
            ui.add_space(2.0);
            let mut result = (false, false);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if primary_button(ui, p, t("Wiederherstellen", "Restore"), true).clicked() {
                    result.0 = true;
                }
                if plain_button(ui, t("Abbrechen", "Cancel")).clicked() {
                    result.1 = true;
                }
            });
            result
        });
        if cancel || close {
            return Step::Close;
        }
        if !confirm {
            return Step::Keep;
        }
        let file = file.clone();
        Step::Then(Box::new(move |app, ctx| app.finish_restore(file, ctx)))
    }

    /// Wovor nachgefragt wird: vier Kästchen, die sofort gelten.
    fn asking_ui(&mut self, ctx: &egui::Context, p: &Palette) -> Step {
        use crate::config::Ask;
        let (done, close) = modal(ctx, p, "dlg-asking", 440.0, |ui| {
            title(ui, p, icons::CHECK_SQUARE, p.accent, t("Rückfragen", "Confirmations"));
            ui.label(RichText::new(t("TurboCommander fragt nach, bevor es …", "TurboCommander asks before it …")).color(p.text_dim));
            ui.add_space(2.0);
            for what in Ask::ALL {
                let label = match what {
                    Ask::Copy => t("… ins andere Fenster kopiert (F5)", "… copies to the other pane (F5)"),
                    Ask::Move => t("… ins andere Fenster verschiebt (F6)", "… moves to the other pane (F6)"),
                    Ask::Trash => t("… in den Papierkorb löscht (Entf)", "… moves to the Recycle Bin (Del)"),
                    Ask::Delete => t("… endgültig löscht (Umschalt+Entf)", "… deletes permanently (Shift+Del)"),
                };
                let mut on = self.cfg.asks(what);
                if checkbox(ui, p, &mut on, label).changed() {
                    self.cfg.set_asks(what, on);
                }
            }
            ui.add_space(2.0);
            ui.add(
                egui::Label::new(
                    RichText::new(t(
                        "Ohne Rückfrage geht Kopieren und Verschieben geradewegs in den Ordner des anderen Fensters. \
                         Kopieren, Verschieben und der Papierkorb lassen sich mit Strg+Z zurücknehmen, endgültiges Löschen nicht.",
                        "Without asking, copying and moving go straight to the folder of the other pane. \
                         Copying, moving and the Recycle Bin can be undone with Ctrl+Z, deleting permanently cannot.",
                    ))
                    .small()
                    .color(p.text_faint),
                )
                .wrap(),
            );
            ui.add_space(4.0);
            let mut done = false;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                done = primary_button(ui, p, t("Fertig", "Done"), true).clicked() || ui.input(|i| i.key_pressed(Key::Enter));
            });
            done
        });
        if done || close {
            self.save_config();
            Step::Close
        } else {
            Step::Keep
        }
    }

    /// Rückfrage vor dem endgültigen Löschen aus dem Papierkorb (`all`: er wird ganz geleert).
    fn bin_ui(ctx: &egui::Context, p: &Palette, names: &[String], ids: &mut Vec<std::ffi::OsString>, all: bool) -> Step {
        let mut go = None;
        let (_, close) = modal(ctx, p, "dlg-bin", 440.0, |ui| {
            let heading = if all {
                t("Papierkorb leeren?", "Empty the Recycle Bin?").to_owned()
            } else {
                tf!("{} endgültig löschen?", "Delete {} permanently?", count_text(names.len()))
            };
            title(ui, p, icons::TRASH, p.danger, &heading);
            let text = if all {
                t(
                    "Alles im Papierkorb wird endgültig gelöscht – auf allen Laufwerken. Das lässt sich nicht zurücknehmen.",
                    "Everything in the Recycle Bin is deleted permanently – on all drives. This cannot be undone.",
                )
            } else {
                t(
                    "Die Elemente werden aus dem Papierkorb entfernt. Das lässt sich nicht zurücknehmen.",
                    "The items are removed from the Recycle Bin. This cannot be undone.",
                )
            };
            ui.add(egui::Label::new(RichText::new(text).color(p.text_dim)).wrap());
            for name in names.iter().take(6) {
                ui.add(egui::Label::new(RichText::new(name).color(p.text)).truncate());
            }
            if names.len() > 6 {
                ui.label(RichText::new(tf!("… und {} weitere", "… and {} more", names.len() - 6)).color(p.text_faint));
            }
            ui.add_space(4.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if all { t("Leeren", "Empty") } else { t("Endgültig löschen", "Delete permanently") };
                if danger_button(ui, p, label).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                    go = Some(true);
                }
                if plain_button(ui, t("Abbrechen", "Cancel")).clicked() {
                    go = Some(false);
                }
            });
        });
        let Some(go) = go.or(close.then_some(false)) else { return Step::Keep };
        if !go {
            return Step::Close;
        }
        let ids = std::mem::take(ids);
        Step::Then(Box::new(move |app, _| {
            let bus = app.bus.clone();
            std::thread::spawn(move || {
                let done = if all { crate::bin::empty() } else { crate::bin::purge(&ids).map(|_| ()) };
                if let Err(why) = done {
                    bus.error(why);
                }
                bus.send(Msg::Refresh { loc: Location::RecycleBin, focus: None });
            });
        }))
    }

    /// Was die Vollversion zusätzlich kann – und, wenn hinterlegt, der Weg dorthin.
    fn full_version_ui(ctx: &egui::Context, p: &Palette) -> Step {
        let url = crate::edition::FULL_VERSION_URL;
        let ((ok, go), close) = modal(ctx, p, "dlg-full", 760.0, |ui| {
            title(ui, p, icons::SPARKLE, p.accent, t("TurboCommander – die Vollversion", "TurboCommander – the full version"));
            ui.add(
                egui::Label::new(
                    RichText::new(t(
                        "Diese kostenlose Lite-Ausgabe hat alles für den Alltag mit Dateien. Die Vollversion kann zusätzlich:",
                        "This free Lite edition has everything for everyday file work. The full version adds:",
                    ))
                    .color(p.text_dim),
                )
                .wrap(),
            );
            ui.add_space(4.0);
            let features = crate::edition::full_features();
            let half = features.len().div_ceil(2);
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height() - 220.0).max(120.0)).show(ui, |ui| {
                ui.columns(2, |cols| {
                    for (c, chunk) in features.chunks(half).enumerate() {
                        let ui = &mut cols[c];
                        ui.spacing_mut().item_spacing.y = 10.0;
                        for (icon, name, about) in chunk {
                            ui.horizontal_top(|ui| {
                                let (rect, _) = ui.allocate_exact_size(Vec2::new(24.0, 20.0), Sense::hover());
                                ui.painter().text(rect.center(), Align2::CENTER_CENTER, *icon, theme::font(17.0), p.accent);
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 1.0;
                                    ui.label(RichText::new(*name).font(theme::semibold(12.5)).color(p.text));
                                    ui.add(egui::Label::new(RichText::new(*about).size(11.5).color(p.text_dim)).wrap());
                                });
                            });
                        }
                    }
                });
            });
            ui.add_space(6.0);
            let mut result = (false, false);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if url.is_empty() {
                    result.0 = primary_button(ui, p, t("Schließen", "Close"), true).clicked();
                } else {
                    result.1 = primary_button(ui, p, t("Zur Vollversion …", "Get the full version …"), true).clicked();
                    result.0 = plain_button(ui, t("Schließen", "Close")).clicked();
                }
            });
            result
        });
        if go {
            let _ = win::shell_run(url, "", &std::env::temp_dir(), false);
            return Step::Close;
        }
        if ok || close { Step::Close } else { Step::Keep }
    }

    fn quit_ui(ctx: &egui::Context, p: &Palette) -> Step {
        let ((quit, stay), close) = modal(ctx, p, "dlg-quit", 440.0, |ui| {
            title(ui, p, icons::WARNING_CIRCLE, p.warn, t("Es laufen noch Aufgaben", "Tasks are still running"));
            ui.add(
                egui::Label::new(
                    RichText::new(t(
                        "Beim Beenden werden laufende Kopier- und Löschvorgänge abgebrochen. \
                         Eine gerade übertragene Datei bleibt dann unvollständig.",
                        "Quitting cancels running copy and delete operations. \
                         A file that is currently being transferred will be left incomplete.",
                    ))
                    .color(p.text_dim),
                )
                .wrap(),
            );
            ui.add_space(4.0);
            let mut result = (false, false);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if primary_button(ui, p, t("Weiterarbeiten", "Keep working"), true).clicked() {
                    result.1 = true;
                }
                if plain_button(ui, t("Trotzdem beenden", "Quit anyway")).clicked() {
                    result.0 = true;
                }
            });
            result
        });
        if stay || close {
            Step::Close
        } else if quit {
            Step::Then(Box::new(|app, ctx| app.quit_now(ctx)))
        } else {
            Step::Keep
        }
    }

    fn shortcuts_ui(ctx: &egui::Context, p: &Palette) -> Step {
        let keys: &[(String, &str)] = &[
            ("Tab".to_owned(), t("Fenster wechseln", "Switch pane")),
            ("Enter".to_owned(), t("Öffnen", "Open")),
            (key("Rücktaste"), t("Übergeordneter Ordner", "Parent folder")),
            ("Alt + ← / →".to_owned(), t("Zurück / Vorwärts", "Back / Forward")),
            ("F2".to_owned(), t("Umbenennen", "Rename")),
            (t("F3 oder Strg+Q", "F3 or Ctrl+Q").to_owned(), t("Schnellansicht", "Quick view")),
            ("F5".to_owned(), t("Ins andere Fenster kopieren", "Copy to other pane")),
            ("F6".to_owned(), t("Ins andere Fenster verschieben", "Move to other pane")),
            ("F7".to_owned(), t("Neuer Ordner", "New folder")),
            (key("Umschalt+F4"), t("Neue Datei", "New file")),
            (
                t("F8 oder Entf", "F8 or Del").to_owned(),
                t("Löschen (mit Umschalt: endgültig)", "Delete (with Shift: permanently)"),
            ),
            ("F9".to_owned(), t("Fenster über-/nebeneinander", "Panes stacked / side by side")),
            (key("Strg+C / X / V"), t("Kopieren / Ausschneiden / Einfügen", "Copy / Cut / Paste")),
            (key("Strg+A"), t("Alles markieren", "Select all")),
            (key("Einfg"), t("Markieren und weiter", "Select and move down")),
            (key("Leertaste"), t("Markieren, Ordnergröße berechnen", "Select, calculate folder size")),
            (key("Strg+F"), t("Suchen, auch in Unterordnern", "Search, including subfolders")),
            (key("Strg+L"), t("Pfad eingeben", "Enter path")),
            (key("Strg+T / W"), t("Neuer Tab / Tab schließen", "New tab / Close tab")),
            (key("Strg+Tab"), t("Nächster Tab", "Next tab")),
            (key("Strg+R"), t("Aktualisieren", "Refresh")),
            (key("Strg+H"), t("Versteckte Dateien ein/aus", "Show/hide hidden files")),
            (key("Strg+D"), t("Ordner zu Favoriten", "Add folder to favorites")),
            (key("Strg+U"), t("Fenster tauschen", "Swap panes")),
            (key("Strg+Z"), t("Kopieren, Verschieben, Löschen oder Umbenennen rückgängig", "Undo copy, move, delete or rename")),
            ("Alt+Enter".to_owned(), t("Eigenschaften", "Properties")),
            (key("Strg+B"), t("Miniaturansicht ein/aus", "Thumbnail view on/off")),
            (key("Strg+Mausrad"), t("Größe der Miniaturen", "Thumbnail size")),
            ("Esc".to_owned(), t("Markierung / Filter aufheben", "Clear selection / filter")),
        ];
        let (ok, close) = modal(ctx, p, "dlg-keys", 700.0, |ui| {
            title(ui, p, icons::KEYBOARD, p.accent, t("Tastenkürzel", "Keyboard shortcuts"));
            let half = keys.len().div_ceil(2);
            ui.columns(2, |cols| {
                for (c, chunk) in keys.chunks(half).enumerate() {
                    let ui = &mut cols[c];
                    ui.spacing_mut().item_spacing.y = 5.0;
                    for (key, what) in chunk {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(Vec2::new(112.0, 22.0), Sense::hover());
                            ui.painter().rect(rect, CornerRadius::same(5), p.control, Stroke::new(1.0, p.border), StrokeKind::Inside);
                            ui.painter().text(rect.center(), Align2::CENTER_CENTER, key.as_str(), theme::semibold(11.0), p.text_dim);
                            ui.label(RichText::new(*what).color(p.text).size(12.5));
                        });
                    }
                }
            });
            ui.add_space(6.0);
            let mut ok = false;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} {}", crate::edition::NAME, env!("CARGO_PKG_VERSION"))).small().color(p.text_faint),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ok = primary_button(ui, p, t("Schließen", "Close"), true).clicked();
                });
            });
            ok
        });
        if ok || close { Step::Close } else { Step::Keep }
    }

    fn undo_ui(ctx: &egui::Context, p: &Palette, heading: &str, records: &mut Vec<crate::ops::Undo>) -> Step {
        let mut go = None;
        let (_, close) = modal(ctx, p, "dlg-undo", 540.0, |ui| {
            title(ui, p, icons::ARROW_U_UP_LEFT, p.accent, t("Rückgängig machen?", "Undo this?"));
            ui.add(egui::Label::new(RichText::new(heading).font(theme::semibold(13.5)).color(p.text)).truncate());
            // Das zuletzt Getane wird zuerst zurückgenommen – in dieser Reihenfolge steht es auch hier.
            for line in records.iter().rev().flat_map(|record| record.describe()) {
                ui.add(egui::Label::new(RichText::new(line).color(p.text_dim)).wrap());
            }
            ui.add_space(4.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if primary_button(ui, p, t("Zurücknehmen", "Undo"), true).clicked() {
                    go = Some(true);
                }
                if plain_button(ui, t("Abbrechen", "Cancel")).clicked() {
                    go = Some(false);
                }
            });
        });
        let Some(go) = go.or(close.then_some(false)) else { return Step::Keep };
        let (heading, records) = (heading.to_owned(), std::mem::take(records));
        Step::Then(Box::new(move |app, _| {
            if go {
                app.jobs.start(Request::Undo(records), &app.bus);
            } else {
                // Nicht zurückgenommen: der Schritt bleibt für später erhalten.
                app.remember(crate::app::UndoEntry::Ops { title: heading, records });
            }
        }))
    }

    fn conflict_ui(&mut self, ctx: &egui::Context) {
        let Some((job, conflict)) = self.jobs.pending_conflict() else { return };
        let p = self.pal.clone();
        let Conflict { name, dest, src_size, dst_size } = conflict;
        let (answer, close) = modal(ctx, &p, "dlg-conflict", 520.0, |ui| {
            title(ui, &p, icons::FILES, p.warn, t("Datei existiert bereits", "File already exists"));
            ui.add(egui::Label::new(RichText::new(&name).font(theme::semibold(13.5)).color(p.text)).truncate());
            ui.add(egui::Label::new(RichText::new(format!("in {dest}")).color(p.text_dim)).truncate());
            ui.label(
                RichText::new(tf!(
                    "Vorhanden: {}   ·   Neu: {}",
                    "Existing: {}   ·   New: {}",
                    fmt_size(dst_size),
                    fmt_size(src_size)
                ))
                .color(p.text_dim),
            );
            ui.add_space(4.0);
            let mut answer = None;
            ui.horizontal_wrapped(|ui| {
                if primary_button(ui, &p, t("Überschreiben", "Overwrite"), true).clicked() {
                    answer = Some(Answer::Overwrite);
                }
                if plain_button(ui, t("Überspringen", "Skip")).clicked() {
                    answer = Some(Answer::Skip);
                }
                if plain_button(ui, t("Beide behalten", "Keep both")).clicked() {
                    answer = Some(Answer::KeepBoth);
                }
            });
            ui.horizontal_wrapped(|ui| {
                if plain_button(ui, t("Alle überschreiben", "Overwrite all")).clicked() {
                    answer = Some(Answer::OverwriteAll);
                }
                if plain_button(ui, t("Alle überspringen", "Skip all")).clicked() {
                    answer = Some(Answer::SkipAll);
                }
                if plain_button(ui, t("Abbrechen", "Cancel")).clicked() {
                    answer = Some(Answer::Cancel);
                }
            });
            answer
        });
        let answer = answer.or(close.then_some(Answer::Cancel));
        if let (Some(answer), Some(job)) = (answer, self.jobs.get(job)) {
            job.shared.respond(answer);
        }
    }

    // ------------------------------------------------------------ Wirkung der Dialoge

    fn finish_input(&mut self, kind: InputKind, name: String) {
        let loc = self.panes[self.active].tab().loc.clone();
        match kind {
            InputKind::NewFolder | InputKind::NewFile => {
                let folder = matches!(kind, InputKind::NewFolder);
                match &loc {
                    Location::Local(dir) => {
                        let path = dir.join(&name);
                        let result = if folder {
                            std::fs::create_dir(&path)
                        } else {
                            std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map(|_| ())
                        };
                        match result {
                            Ok(()) => self.bus.send(Msg::Refresh { loc, focus: Some(name) }),
                            Err(e) => self.bus.error(tf!(
                                "„{name}\" konnte nicht angelegt werden: {}",
                                "“{name}” could not be created: {}",
                                io_msg(&e)
                            )),
                        }
                    }
                    _ => self.bus.error(t("Hier kann nichts angelegt werden.", "Nothing can be created here.")),
                }
            }
            InputKind::Favorite(index) => {
                if let Some(fav) = self.cfg.favorites.get_mut(index) {
                    fav.name = name;
                    self.save_config();
                }
            }
            InputKind::Pack(sources) => {
                if let Location::Local(dir) = &loc {
                    let target = dir.join(&name);
                    self.jobs.start(Request::Pack { sources, target }, &self.bus);
                }
            }
        }
    }

}

/// Vorschlag für den Archivnamen beim Packen.
pub fn pack_name(sources: &[Source], folder: &Location) -> String {
    match sources {
        [one] => {
            if one.is_dir {
                one.name.clone()
            } else {
                split_ext(&one.name).0.to_owned()
            }
        }
        _ => folder.leaf().trim_end_matches(':').to_owned(),
    }
}
