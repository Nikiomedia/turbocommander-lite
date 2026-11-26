//! Farbpaletten (hell/dunkel), Schriften und das egui-Styling.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle,
    Vec2,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeMode {
    Light,
    Dark,
}

impl ThemeMode {
    pub fn toggled(self) -> Self {
        match self {
            ThemeMode::Light => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::Light,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Palette {
    pub dark: bool,
    /// Fensterhintergrund (zwischen den Karten)
    pub bg: Color32,
    /// Karten: Dateilisten
    pub surface: Color32,
    /// Leisten innerhalb einer Karte
    pub surface_alt: Color32,
    /// Dialoge und Menüs
    pub elevated: Color32,
    pub field: Color32,
    pub control: Color32,
    pub control_hover: Color32,
    pub control_active: Color32,
    pub border: Color32,
    pub border_soft: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub row_hover: Color32,
    pub row_sel: Color32,
    pub row_sel_inactive: Color32,
    pub folder: Color32,
    pub image: Color32,
    pub video: Color32,
    pub audio: Color32,
    pub archive: Color32,
    pub code: Color32,
    pub doc: Color32,
    pub pdf: Color32,
    pub exe: Color32,
    pub danger: Color32,
    pub success: Color32,
    pub warn: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Palette {
    pub fn of(mode: ThemeMode) -> Self {
        match mode {
            ThemeMode::Dark => Self::dark(),
            ThemeMode::Light => Self::light(),
        }
    }

    pub fn dark() -> Self {
        Self {
            dark: true,
            bg: rgb(0x0F1115),
            surface: rgb(0x171A21),
            surface_alt: rgb(0x1B1F27),
            elevated: rgb(0x1F232D),
            field: rgb(0x12151B),
            control: rgb(0x232833),
            control_hover: rgb(0x2B3140),
            control_active: rgb(0x343B4D),
            border: rgb(0x2A2F3B),
            border_soft: rgb(0x20242D),
            text: rgb(0xE7E9EE),
            text_dim: rgb(0xA3AAB7),
            text_faint: rgb(0x6C7482),
            accent: rgb(0x5B8DEF),
            accent_soft: rgb(0x22304D),
            on_accent: rgb(0xFFFFFF),
            row_hover: rgb(0x1D2129),
            row_sel: rgb(0x26365A),
            row_sel_inactive: rgb(0x232833),
            folder: rgb(0xE6B450),
            image: rgb(0x5CC8A8),
            video: rgb(0xC08CF5),
            audio: rgb(0xF08DB2),
            archive: rgb(0xE9A15B),
            code: rgb(0x6CB6FF),
            doc: rgb(0x93A4BE),
            pdf: rgb(0xF0706A),
            exe: rgb(0x9AD26B),
            danger: rgb(0xF0706A),
            success: rgb(0x5CC88A),
            warn: rgb(0xE8B44C),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            bg: rgb(0xECEEF3),
            surface: rgb(0xFFFFFF),
            surface_alt: rgb(0xF6F7FA),
            elevated: rgb(0xFFFFFF),
            field: rgb(0xFFFFFF),
            control: rgb(0xF0F2F6),
            control_hover: rgb(0xE5E9F0),
            control_active: rgb(0xD9DEE8),
            border: rgb(0xDADFE7),
            border_soft: rgb(0xE8EBF0),
            text: rgb(0x1A1D24),
            text_dim: rgb(0x566070),
            text_faint: rgb(0x8A93A2),
            accent: rgb(0x2F6FED),
            accent_soft: rgb(0xE3EDFF),
            on_accent: rgb(0xFFFFFF),
            row_hover: rgb(0xF2F5FA),
            row_sel: rgb(0xD9E6FF),
            row_sel_inactive: rgb(0xEBEDF2),
            folder: rgb(0xE2A52F),
            image: rgb(0x1F9D7A),
            video: rgb(0x8B4FD9),
            audio: rgb(0xD2478A),
            archive: rgb(0xC97A1E),
            code: rgb(0x1E7FD6),
            doc: rgb(0x5E6B80),
            pdf: rgb(0xD9453F),
            exe: rgb(0x4B9B2B),
            danger: rgb(0xD9453F),
            success: rgb(0x2E9E5B),
            warn: rgb(0xC98A10),
        }
    }

    pub fn rgb_of(c: Color32) -> [u8; 3] {
        [c.r(), c.g(), c.b()]
    }
}

/// Mischt `over` mit Deckkraft `t` (0..1) über `base`.
pub fn mix(base: Color32, over: Color32, t: f32) -> Color32 {
    let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(l(base.r(), over.r()), l(base.g(), over.g()), l(base.b(), over.b()))
}

// ---------------------------------------------------------------- Schriften

pub const FAMILY_SEMIBOLD: &str = "semibold";
pub const FAMILY_ICON_FILL: &str = "phosphor-fill";

pub fn font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_SEMIBOLD.into()))
}

pub fn icon_fill(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_ICON_FILL.into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// Lädt Segoe UI direkt aus Windows (vertrautes Schriftbild, keine eingebettete Kopie nötig)
/// und hängt die Phosphor-Symbole als Rückfall an.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".to_owned());
    let load = |fonts: &mut FontDefinitions, key: &str, files: &[&str]| -> bool {
        for file in files {
            if let Ok(bytes) = std::fs::read(format!("{windir}\\Fonts\\{file}")) {
                fonts.font_data.insert(key.to_owned(), Arc::new(FontData::from_owned(bytes)));
                return true;
            }
        }
        false
    };

    if load(&mut fonts, "ui", &["segoeui.ttf"]) {
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "ui".to_owned());
    }
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);

    let mut semibold = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    if load(&mut fonts, "ui-semibold", &["seguisb.ttf", "segoeuib.ttf"]) {
        semibold.insert(0, "ui-semibold".to_owned());
    }
    fonts.families.insert(FontFamily::Name(FAMILY_SEMIBOLD.into()), semibold);

    egui_phosphor::add_font_bytes_as_family(
        &mut fonts,
        FAMILY_ICON_FILL,
        egui_phosphor::Variant::Fill.font_bytes(),
    );

    if load(&mut fonts, "mono", &["consola.ttf", "CascadiaMono.ttf"]) {
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "mono".to_owned());
    }
    ctx.set_fonts(fonts);
}

// ---------------------------------------------------------------- Styling

pub fn apply(ctx: &egui::Context, p: &Palette) {
    ctx.set_theme(if p.dark { egui::Theme::Dark } else { egui::Theme::Light });

    let mut style = egui::Style::default();
    style.visuals = if p.dark { egui::Visuals::dark() } else { egui::Visuals::light() };

    style.text_styles = [
        (TextStyle::Small, font(11.5)),
        (TextStyle::Body, font(13.0)),
        (TextStyle::Button, font(13.0)),
        (TextStyle::Heading, semibold(17.0)),
        (TextStyle::Monospace, mono(12.5)),
    ]
    .into();

    let sp = &mut style.spacing;
    sp.item_spacing = Vec2::new(8.0, 6.0);
    sp.button_padding = Vec2::new(10.0, 5.0);
    sp.interact_size = Vec2::new(28.0, 26.0);
    sp.menu_margin = Margin::same(6);
    sp.window_margin = Margin::same(18);
    sp.icon_width = 16.0;
    sp.icon_width_inner = 9.0;
    sp.combo_width = 120.0;
    sp.scroll = egui::style::ScrollStyle::floating();
    sp.scroll.bar_width = 9.0;
    sp.scroll.floating_width = 5.0;
    sp.scroll.floating_allocated_width = 0.0;
    sp.scroll.bar_inner_margin = 2.0;
    sp.scroll.bar_outer_margin = 2.0;
    sp.scroll.dormant_handle_opacity = 0.35;
    sp.scroll.active_handle_opacity = 0.6;
    sp.scroll.interact_handle_opacity = 0.9;
    sp.scroll.dormant_background_opacity = 0.0;
    sp.scroll.active_background_opacity = 0.0;
    sp.scroll.interact_background_opacity = 0.25;

    style.interaction.selectable_labels = false;
    style.interaction.tooltip_delay = 0.45;
    style.animation_time = 0.10;

    let v = &mut style.visuals;
    v.panel_fill = p.bg;
    v.window_fill = p.elevated;
    v.extreme_bg_color = p.field;
    v.text_edit_bg_color = Some(p.field);
    v.faint_bg_color = p.surface_alt;
    v.code_bg_color = p.surface_alt;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(12);
    v.menu_corner_radius = CornerRadius::same(10);
    let (window_alpha, popup_alpha): (u8, u8) = if p.dark { (150, 112) } else { (46, 34) };
    v.window_shadow = Shadow { offset: [0, 14], blur: 44, spread: 0, color: Color32::from_black_alpha(window_alpha) };
    v.popup_shadow = Shadow { offset: [0, 8], blur: 26, spread: 0, color: Color32::from_black_alpha(popup_alpha) };
    v.selection.bg_fill = mix(p.surface, p.accent, if p.dark { 0.45 } else { 0.28 });
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.danger;
    v.text_cursor.stroke = Stroke::new(1.5, p.accent);
    v.weak_text_color = Some(p.text_faint);

    let radius = CornerRadius::same(7);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = radius;

    w.inactive.bg_fill = p.control;
    w.inactive.weak_bg_fill = p.control;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = radius;
    w.inactive.expansion = 0.0;

    w.hovered.bg_fill = p.control_hover;
    w.hovered.weak_bg_fill = p.control_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, mix(p.border, p.text_faint, 0.5));
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = radius;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.control_active;
    w.active.weak_bg_fill = p.control_active;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = radius;
    w.active.expansion = 0.0;

    w.open.bg_fill = p.control_hover;
    w.open.weak_bg_fill = p.control_hover;
    w.open.bg_stroke = Stroke::new(1.0, p.border);
    w.open.fg_stroke = Stroke::new(1.0, p.text);
    w.open.corner_radius = radius;
    w.open.expansion = 0.0;

    ctx.set_global_style(style);
}

/// Rahmen für Dialoge.
pub fn dialog_frame(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.elevated)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::same(20))
        .shadow(Shadow {
            offset: [0, 18],
            blur: 60,
            spread: 0,
            color: Color32::from_black_alpha(if p.dark { 170 } else { 60 }),
        })
}
