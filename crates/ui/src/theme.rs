// SPDX-License-Identifier: GPL-3.0-or-later
//! The jdance visual system ("Graphite"): palette, fonts and widget style.
//!
//! An independent look (spec "DAW UI clean room", §1): graphite grounds,
//! IBM Plex Sans for the interface and IBM Plex Mono for numbers; amber
//! marks active steps and playback, blue marks selection and navigation,
//! teal marks automation.

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, Stroke, Theme,
    Visuals, vec2,
};
use std::sync::Arc;

pub(crate) const BG: Color32 = Color32::from_rgb(0x0E, 0x10, 0x13);
pub(crate) const BAR: Color32 = Color32::from_rgb(0x13, 0x16, 0x1A);
pub(crate) const PANEL: Color32 = Color32::from_rgb(0x17, 0x1A, 0x1E);
pub(crate) const SIDEBAR: Color32 = Color32::from_rgb(0x12, 0x15, 0x18);
pub(crate) const RAISED: Color32 = Color32::from_rgb(0x25, 0x2A, 0x30);
pub(crate) const FIELD: Color32 = Color32::from_rgb(0x0F, 0x12, 0x15);
pub(crate) const LINE: Color32 = Color32::from_rgb(0x2A, 0x30, 0x37);
pub(crate) const LINE_STRONG: Color32 = Color32::from_rgb(0x39, 0x40, 0x4A);
pub(crate) const TEXT: Color32 = Color32::from_rgb(0xE9, 0xEC, 0xEF);
pub(crate) const MUTED: Color32 = Color32::from_rgb(0xA7, 0xAF, 0xB8);
pub(crate) const AMBER: Color32 = Color32::from_rgb(0xF5, 0xB5, 0x44);
pub(crate) const ON_AMBER: Color32 = Color32::from_rgb(0x1B, 0x14, 0x05);
pub(crate) const BLUE: Color32 = Color32::from_rgb(0x5A, 0xA9, 0xFF);
pub(crate) const BLUE_BG: Color32 = Color32::from_rgb(0x1F, 0x35, 0x50);
pub(crate) const TEAL: Color32 = Color32::from_rgb(0x3C, 0xC9, 0xB0);
pub(crate) const GREEN: Color32 = Color32::from_rgb(0x5F, 0xA8, 0x6B);
pub(crate) const OK: Color32 = Color32::from_rgb(0x8F, 0xD1, 0x9E);
pub(crate) const WARN: Color32 = Color32::from_rgb(0xFF, 0xC2, 0x7A);
pub(crate) const ERROR: Color32 = Color32::from_rgb(0xFF, 0x9C, 0x8F);
pub(crate) const RED: Color32 = Color32::from_rgb(0xFF, 0x8A, 0x7A);

// UI-RD §1: the 2026 redesign tokens.
/// Secondary labels: section captions, units, hints.
pub(crate) const DIM: Color32 = Color32::from_rgb(0x7E, 0x87, 0x92);
/// Section cards (UI-RD §1.3).
pub(crate) const CARD: Color32 = Color32::from_rgb(0x13, 0x16, 0x1A);
pub(crate) const CARD_LINE: Color32 = Color32::from_rgb(0x22, 0x27, 0x2D);
/// The position display and other "LCD" fields.
pub(crate) const LCD: Color32 = Color32::from_rgb(0x0A, 0x0C, 0x0E);
pub(crate) const LCD_LINE: Color32 = Color32::from_rgb(0x1E, 0x23, 0x29);
/// Key badges (UI-RD §1.5).
pub(crate) const KBD_BG: Color32 = Color32::from_rgb(0x17, 0x1A, 0x1E);
pub(crate) const KBD_LINE: Color32 = Color32::from_rgb(0x2F, 0x35, 0x3D);
/// Lamps.
pub(crate) const LED_ON: Color32 = GREEN;
pub(crate) const LED_OFF: Color32 = Color32::from_rgb(0x3A, 0x40, 0x47);
/// The playhead: a solid 2 px line.
pub(crate) const PLAYHEAD: Color32 = Color32::from_rgb(0xF2, 0xF4, 0xF6);
/// Scale highlight in the Piano Roll (UI-RD §1.1): indigo rows.
pub(crate) const SCALE_ROW: Color32 = Color32::from_rgb(0x1A, 0x1D, 0x2C);
pub(crate) const SCALE_TONIC: Color32 = Color32::from_rgb(0x24, 0x2A, 0x48);
pub(crate) const SCALE_ACCENT: Color32 = Color32::from_rgb(0x9A, 0xA5, 0xFF);
/// Outline of a note outside the scale.
pub(crate) const OUT_OF_SCALE: Color32 = ERROR;
/// Preview and "listen to the result".
pub(crate) const PREVIEW: Color32 = TEAL;
/// Slide notes and the Slide button.
pub(crate) const SLIDE: Color32 = Color32::from_rgb(0xB4, 0x8C, 0xF0);
/// Scale controls: dots on the keys, the scale label, the Scale window.
pub(crate) const SCALE_UI: Color32 = Color32::from_rgb(0x6F, 0xCF, 0x97);

const MONO: &str = "plex-mono";
const STRONG: &str = "plex-semibold";

/// Monospaced numbers (positions, values, sizes).
pub(crate) fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(MONO.into()))
}

/// Semibold interface text (titles, names).
pub(crate) fn strong(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(STRONG.into()))
}

/// Installs fonts and visuals. Called once, on the first frame.
pub(crate) fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let add = |fonts: &mut FontDefinitions, name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    add(
        &mut fonts,
        "plex-sans",
        include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf"),
    );
    add(
        &mut fonts,
        STRONG,
        include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf"),
    );
    add(
        &mut fonts,
        MONO,
        include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"),
    );
    // Plex first; egui's defaults stay as fallbacks for missing glyphs.
    let defaults = fonts.families[&FontFamily::Proportional].clone();
    let with = |first: &str| {
        let mut v = vec![first.to_owned()];
        v.extend(defaults.iter().cloned());
        v
    };
    fonts
        .families
        .insert(FontFamily::Proportional, with("plex-sans"));
    fonts.families.insert(FontFamily::Monospace, with(MONO));
    fonts
        .families
        .insert(FontFamily::Name(MONO.into()), with(MONO));
    fonts
        .families
        .insert(FontFamily::Name(STRONG.into()), with(STRONG));
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals_of(Theme::Dark, visuals());
    ctx.style_mut_of(Theme::Dark, |style| {
        let s = &mut style.spacing;
        s.item_spacing = vec2(6.0, 6.0);
        s.button_padding = vec2(10.0, 4.0);
        s.interact_size.y = 26.0;
        s.window_margin = Margin::same(12);
        s.menu_margin = Margin::same(6);
        s.slider_rail_height = 6.0;
        s.menu_spacing = 2.0;
        s.icon_width = 14.0;
        s.icon_width_inner = 8.0;
        // UI-RD §1.2: element text Sans 12, panel titles SemiBold 12.5.
        use egui::TextStyle as T;
        style
            .text_styles
            .insert(T::Body, FontId::proportional(12.5));
        style
            .text_styles
            .insert(T::Button, FontId::proportional(12.0));
        style
            .text_styles
            .insert(T::Small, FontId::proportional(10.5));
        style.text_styles.insert(T::Monospace, mono(11.5));
        style.text_styles.insert(T::Heading, strong(14.0));
    });
}

fn visuals() -> Visuals {
    let mut v = Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.window_stroke = Stroke::new(1.0, LINE);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(9);
    v.window_shadow = egui::epaint::Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(140),
    };
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0, 6],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(120),
    };
    v.extreme_bg_color = FIELD;
    v.faint_bg_color = Color32::from_rgb(0x16, 0x19, 0x1D);
    v.code_bg_color = FIELD;
    v.text_edit_bg_color = Some(FIELD);
    v.hyperlink_color = Color32::from_rgb(0x7F, 0xB8, 0xFF);
    v.warn_fg_color = WARN;
    v.error_fg_color = ERROR;
    v.selection.bg_fill = BLUE_BG;
    v.selection.stroke = Stroke::new(1.0, BLUE);
    v.slider_trailing_fill = true;
    let r = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = PANEL;
    w.noninteractive.weak_bg_fill = PANEL;
    w.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.noninteractive.corner_radius = r;
    w.inactive.bg_fill = RAISED;
    w.inactive.weak_bg_fill = RAISED;
    w.inactive.bg_stroke = Stroke::new(1.0, LINE_STRONG);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = r;
    w.hovered.bg_fill = Color32::from_rgb(0x2E, 0x34, 0x3B);
    w.hovered.weak_bg_fill = Color32::from_rgb(0x2E, 0x34, 0x3B);
    w.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0x4A, 0x52, 0x5C));
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    w.hovered.corner_radius = r;
    w.active.bg_fill = BLUE_BG;
    w.active.weak_bg_fill = BLUE_BG;
    w.active.bg_stroke = Stroke::new(1.0, BLUE);
    w.active.fg_stroke = Stroke::new(1.0, TEXT);
    w.active.corner_radius = r;
    w.open = w.active;
    w.open.bg_fill = RAISED;
    w.open.weak_bg_fill = RAISED;
    v
}

/// Color of a track or lane in the default palette.
pub(crate) fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}
