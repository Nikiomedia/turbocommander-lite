//! Selbst gezeichnete Bausteine: Symbol-Schaltflächen, gekürzter Text, Menüeinträge.

use std::sync::Arc;

use eframe::egui::text::{LayoutJob, TextWrapping};
use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Galley, Id, Painter, Pos2, Rect, Response, Sense, Ui, Vec2};

use crate::theme::{self, Palette};

pub fn text_width(ui: &Ui, text: &str, font: FontId) -> f32 {
    ui.fonts_mut(|f| f.layout_no_wrap(text.to_owned(), font, Color32::WHITE)).size().x
}

/// Einzeilig, bei Platzmangel mit „…" gekürzt.
pub fn elided(painter: &Painter, text: &str, font: FontId, color: Color32, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping::truncate_at_width(max_width.max(1.0));
    painter.ctx().fonts_mut(|f| f.layout_job(job))
}

/// Zeichnet Text links- oder rechtsbündig, vertikal zentriert auf `y`.
pub fn draw_text(painter: &Painter, x: f32, y: f32, max_width: f32, text: &str, font: FontId, color: Color32, right: bool) {
    if max_width < 6.0 || text.is_empty() {
        return;
    }
    let galley = elided(painter, text, font, color, max_width);
    let size = galley.size();
    let pos_x = if right { x + max_width - size.x } else { x };
    painter.galley(Pos2::new(pos_x, (y - size.y / 2.0).round()), galley, color);
}

pub fn icon_button(ui: &Ui, rect: Rect, id: Id, icon: &str, p: &Palette, enabled: bool, active: bool) -> Response {
    let resp = ui.interact(rect, id, if enabled { Sense::click() } else { Sense::hover() });
    let painter = ui.painter();
    let bg = if !enabled {
        None
    } else if resp.is_pointer_button_down_on() {
        Some(p.control_active)
    } else if resp.hovered() {
        Some(p.control_hover)
    } else if active {
        Some(p.accent_soft)
    } else {
        None
    };
    if let Some(bg) = bg {
        painter.rect_filled(rect, CornerRadius::same(7), bg);
    }
    let color = if !enabled {
        theme::mix(p.surface, p.text_faint, 0.55)
    } else if active {
        p.accent
    } else if resp.hovered() {
        p.text
    } else {
        p.text_dim
    };
    painter.text(rect.center(), Align2::CENTER_CENTER, icon, theme::font(16.0), color);
    resp
}

pub fn menu_begin(ui: &mut Ui, width: f32) {
    ui.set_min_width(width);
    ui.set_max_width(width);
    ui.spacing_mut().item_spacing = Vec2::new(0.0, 1.0);
}

/// Menüzeile mit Symbol, Text und Tastenkürzel. Liefert `true` beim Klick.
pub fn menu_item(ui: &mut Ui, p: &Palette, icon: &str, label: &str, shortcut: &str, enabled: bool) -> bool {
    let width = ui.max_rect().width().max(160.0);
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(width, 28.0), if enabled { Sense::click() } else { Sense::hover() });
    if !ui.is_rect_visible(rect) {
        return false;
    }
    let painter = ui.painter();
    if enabled && resp.hovered() {
        painter.rect_filled(rect, CornerRadius::same(6), p.control_hover);
    }
    let text_color = if enabled { p.text } else { p.text_faint };
    let icon_color = if enabled { p.text_dim } else { p.text_faint };
    let y = rect.center().y;
    if !icon.is_empty() {
        painter.text(Pos2::new(rect.left() + 17.0, y), Align2::CENTER_CENTER, icon, theme::font(15.0), icon_color);
    }
    let shortcut_w = if shortcut.is_empty() { 0.0 } else { text_width(ui, shortcut, theme::font(11.5)) + 14.0 };
    draw_text(painter, rect.left() + 34.0, y, rect.width() - 42.0 - shortcut_w, label, theme::font(13.0), text_color, false);
    if !shortcut.is_empty() {
        painter.text(Pos2::new(rect.right() - 10.0, y), Align2::RIGHT_CENTER, shortcut, theme::font(11.5), p.text_faint);
    }
    enabled && resp.clicked()
}

pub fn menu_separator(ui: &mut Ui, p: &Palette) {
    let width = ui.max_rect().width().max(160.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 9.0), Sense::hover());
    ui.painter().hline(rect.shrink2(Vec2::new(6.0, 0.0)).x_range(), rect.center().y, egui::Stroke::new(1.0, p.border_soft));
}

pub fn menu_caption(ui: &mut Ui, p: &Palette, text: &str) {
    let width = ui.max_rect().width().max(160.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 22.0), Sense::hover());
    ui.painter().text(
        Pos2::new(rect.left() + 10.0, rect.center().y + 1.0),
        Align2::LEFT_CENTER,
        text,
        theme::semibold(11.0),
        p.text_faint,
    );
}

/// Kontrollkästchen im eigenen Stil.
pub fn checkbox(ui: &mut Ui, p: &Palette, value: &mut bool, label: &str) -> Response {
    let font = theme::font(13.0);
    let text_w = text_width(ui, label, font.clone());
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(26.0 + text_w, 22.0), Sense::click());
    if resp.clicked() {
        *value = !*value;
        resp.mark_changed();
    }
    let enabled = ui.is_enabled();
    let square = Rect::from_center_size(Pos2::new(rect.left() + 8.0, rect.center().y), Vec2::splat(16.0));
    let painter = ui.painter();
    if *value {
        painter.rect_filled(square, CornerRadius::same(4), if enabled { p.accent } else { p.text_faint });
        painter.text(square.center(), Align2::CENTER_CENTER, crate::icons::CHECK, theme::font(12.0), p.on_accent);
    } else {
        let edge = if resp.hovered() { p.accent } else { theme::mix(p.border, p.text_faint, 0.7) };
        painter.rect(square, CornerRadius::same(4), p.field, egui::Stroke::new(1.0, edge), egui::StrokeKind::Inside);
    }
    painter.text(
        Pos2::new(square.right() + 8.0, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font,
        if enabled { p.text } else { p.text_faint },
    );
    resp
}

/// Fortschrittsbalken mit abgerundeten Enden.
pub fn progress_bar(painter: &Painter, rect: Rect, fraction: f32, track: Color32, fill: Color32) {
    let radius = CornerRadius::same((rect.height() / 2.0).round() as u8);
    painter.rect_filled(rect, radius, track);
    let w = (rect.width() * fraction.clamp(0.0, 1.0)).max(rect.height().min(rect.width()) * (fraction > 0.0) as u8 as f32);
    if w > 0.0 {
        painter.rect_filled(Rect::from_min_size(rect.min, Vec2::new(w, rect.height())), radius, fill);
    }
}

/// Primäre Schaltfläche für Dialoge.
pub fn primary_button(ui: &mut Ui, p: &Palette, text: &str, enabled: bool) -> Response {
    let button = egui::Button::new(egui::RichText::new(text).color(if enabled { p.on_accent } else { p.text_faint }))
        .fill(if enabled { p.accent } else { p.control })
        .stroke(egui::Stroke::NONE)
        .min_size(Vec2::new(112.0, 30.0));
    ui.add_enabled(enabled, button)
}

pub fn plain_button(ui: &mut Ui, text: &str) -> Response {
    ui.add(egui::Button::new(text).min_size(Vec2::new(96.0, 30.0)))
}

pub fn danger_button(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    let button = egui::Button::new(egui::RichText::new(text).color(Color32::WHITE))
        .fill(p.danger)
        .stroke(egui::Stroke::NONE)
        .min_size(Vec2::new(112.0, 30.0));
    ui.add(button)
}
