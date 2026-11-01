//! Visual theme: dark and light palettes, spacing, radii and text sizes.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Shadow, Stroke, TextStyle, Theme, ThemePreference, Visuals,
    style::{Selection, WidgetVisuals},
};

pub struct Palette {
    /// App background.
    pub bg: Color32,
    /// Sidebar / header background.
    pub chrome: Color32,
    /// Cards and popups.
    pub surface: Color32,
    pub surface_hover: Color32,
    /// Text inputs.
    pub input: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub success: Color32,
    pub warn: Color32,
    pub danger: Color32,
}

impl Palette {
    pub fn accent_soft(&self) -> Color32 {
        self.accent.gamma_multiply(0.18)
    }
}

pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x0f, 0x11, 0x16),
    chrome: Color32::from_rgb(0x14, 0x17, 0x1d),
    surface: Color32::from_rgb(0x1a, 0x1d, 0x25),
    surface_hover: Color32::from_rgb(0x22, 0x26, 0x30),
    input: Color32::from_rgb(0x0c, 0x0e, 0x12),
    border: Color32::from_rgb(0x2a, 0x2e, 0x39),
    text: Color32::from_rgb(0xe6, 0xe8, 0xee),
    muted: Color32::from_rgb(0x8b, 0x91, 0xa0),
    accent: Color32::from_rgb(0x7c, 0x6c, 0xf6),
    accent_hover: Color32::from_rgb(0x8f, 0x81, 0xff),
    on_accent: Color32::WHITE,
    success: Color32::from_rgb(0x3f, 0xb9, 0x7f),
    warn: Color32::from_rgb(0xe0, 0xa8, 0x4a),
    danger: Color32::from_rgb(0xef, 0x5a, 0x6f),
};

pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xf6, 0xf7, 0xfa),
    chrome: Color32::from_rgb(0xee, 0xf0, 0xf5),
    surface: Color32::WHITE,
    surface_hover: Color32::from_rgb(0xf0, 0xf1, 0xf7),
    input: Color32::WHITE,
    border: Color32::from_rgb(0xdf, 0xe2, 0xea),
    text: Color32::from_rgb(0x1a, 0x1d, 0x24),
    muted: Color32::from_rgb(0x69, 0x6f, 0x7f),
    accent: Color32::from_rgb(0x5b, 0x4b, 0xe0),
    accent_hover: Color32::from_rgb(0x4c, 0x3c, 0xd0),
    on_accent: Color32::WHITE,
    success: Color32::from_rgb(0x1f, 0x9d, 0x62),
    warn: Color32::from_rgb(0xb7, 0x7b, 0x12),
    danger: Color32::from_rgb(0xd6, 0x36, 0x4f),
};

pub fn palette(ui: &egui::Ui) -> &'static Palette {
    if ui.visuals().dark_mode { &DARK } else { &LIGHT }
}

pub const RADIUS: u8 = 8;
pub const CARD_RADIUS: u8 = 12;

/// Installs the dark and light styles. The active one follows `preference`.
pub fn install(ctx: &egui::Context, preference: ThemePreference) {
    for (theme, p) in [(Theme::Dark, &DARK), (Theme::Light, &LIGHT)] {
        ctx.style_mut_of(theme, |style| {
            style.visuals = visuals(theme, p);
            style.interaction.selectable_labels = false;
            style.interaction.tooltip_delay = 0.35;
            style.animation_time = 0.12;

            let s = &mut style.spacing;
            s.item_spacing = egui::vec2(8.0, 6.0);
            s.button_padding = egui::vec2(10.0, 5.0);
            s.interact_size.y = 28.0;
            s.menu_margin = Margin::same(8);
            s.window_margin = Margin::same(14);
            s.combo_width = 140.0;
            s.icon_width = 16.0;
            s.icon_width_inner = 9.0;
            s.indent = 16.0;

            style.text_styles = [
                (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
                (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
                (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
                (TextStyle::Heading, FontId::new(21.0, FontFamily::Proportional)),
                (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
            ]
            .into();
        });
    }
    ctx.set_theme(preference);
}

fn widget(bg: Color32, weak_bg: Color32, stroke: Stroke, fg: Color32) -> WidgetVisuals {
    WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: weak_bg,
        bg_stroke: stroke,
        corner_radius: CornerRadius::same(RADIUS),
        fg_stroke: Stroke::new(1.0, fg),
        expansion: 0.0,
    }
}

fn visuals(theme: Theme, p: &Palette) -> Visuals {
    let mut v = match theme {
        Theme::Dark => Visuals::dark(),
        Theme::Light => Visuals::light(),
    };
    let shadow_alpha = if theme == Theme::Dark { 110 } else { 28 };

    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.faint_bg_color = p.surface;
    v.extreme_bg_color = p.input;
    v.text_edit_bg_color = Some(p.input);
    v.code_bg_color = p.surface_hover;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(CARD_RADIUS);
    v.menu_corner_radius = CornerRadius::same(10);
    v.window_shadow = Shadow { offset: [0, 10], blur: 30, spread: 0, color: Color32::from_black_alpha(shadow_alpha) };
    v.popup_shadow = Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(shadow_alpha) };
    v.hyperlink_color = p.accent;
    v.warn_fg_color = p.warn;
    v.error_fg_color = p.danger;
    v.weak_text_color = Some(p.muted);
    v.selection = Selection { bg_fill: p.accent.gamma_multiply(0.28), stroke: Stroke::new(1.0, p.accent_hover) };
    v.indent_has_left_vline = false;
    v.collapsing_header_frame = false;
    v.striped = false;

    let w = &mut v.widgets;
    w.noninteractive = widget(p.surface, p.surface, Stroke::new(1.0, p.border), p.text);
    w.inactive = widget(p.input, p.surface_hover, Stroke::new(1.0, p.border), p.text);
    w.hovered = widget(p.input, p.surface_hover, Stroke::new(1.0, p.accent.gamma_multiply(0.7)), p.text);
    w.active = widget(p.input, p.accent_soft(), Stroke::new(1.0, p.accent), p.text);
    w.open = widget(p.input, p.surface_hover, Stroke::new(1.0, p.accent.gamma_multiply(0.7)), p.text);
    v
}

// ---- theme preference persistence (localStorage of the extension origin) ----

const PREF_KEY: &str = "sm_theme";

pub fn load_preference() -> ThemePreference {
    let stored = web_sys::window().and_then(|w| w.local_storage().ok().flatten()).and_then(|s| s.get_item(PREF_KEY).ok().flatten());
    match stored.as_deref() {
        Some("dark") => ThemePreference::Dark,
        Some("light") => ThemePreference::Light,
        _ => ThemePreference::System,
    }
}

pub fn save_preference(p: ThemePreference) {
    let value = match p {
        ThemePreference::Dark => "dark",
        ThemePreference::Light => "light",
        ThemePreference::System => "system",
    };
    if let Some(s) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = s.set_item(PREF_KEY, value);
    }
}
