//! Reusable UI components (cards, buttons, chips, avatars, navigation items...).

use std::collections::BTreeSet;

use eframe::egui::{self, Align2, Color32, CursorIcon, FontId, Margin, RichText, Sense, Stroke, Vec2};

use super::theme::{self, CARD_RADIUS, RADIUS};
use crate::model::{Session, TabEntry};
use crate::pattern::{PatternKind, SitePattern};
use crate::workspace::{Workspace, WorkspaceColor};

pub fn session_label(s: &Session, active: bool) -> String {
    format!("{}{}", if active { "★ " } else { "" }, s.name)
}

/// Deterministic color for a label (tags / groups / hosts).
pub fn label_color(text: &str) -> Color32 {
    let h = text.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    let hue = (h % 360) as f32 / 360.0;
    egui::ecolor::Hsva::new(hue, 0.55, 0.78, 1.0).into()
}

pub fn ws_color(c: WorkspaceColor) -> Color32 {
    let [r, g, b] = c.rgb();
    Color32::from_rgb(r, g, b)
}

// ---------------------------------------------------------------- containers

/// A rounded surface with a subtle border.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let p = theme::palette(ui);
    egui::Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CARD_RADIUS)
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Card with a title and optional description.
pub fn section_card<R>(ui: &mut egui::Ui, title: &str, subtitle: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let r = card(ui, |ui| {
        ui.label(RichText::new(title).size(16.0).strong());
        if !subtitle.is_empty() {
            ui.label(RichText::new(subtitle).weak());
        }
        ui.add_space(8.0);
        add(ui)
    });
    ui.add_space(10.0);
    r
}

// ---------------------------------------------------------------- buttons

pub fn primary_button(ui: &egui::Ui, text: impl Into<String>) -> egui::Button<'static> {
    let p = theme::palette(ui);
    egui::Button::new(RichText::new(text.into()).color(p.on_accent).strong())
        .fill(p.accent)
        .stroke(Stroke::NONE)
        .corner_radius(RADIUS)
}

pub fn danger_button(ui: &egui::Ui, text: impl Into<String>) -> egui::Button<'static> {
    let p = theme::palette(ui);
    egui::Button::new(RichText::new(text.into()).color(p.danger)).stroke(Stroke::new(1.0, p.danger.gamma_multiply(0.5)))
}

/// Frameless square icon button with a hover background.
pub fn icon_button(ui: &mut egui::Ui, icon: &str, tooltip: &str) -> egui::Response {
    icon_button_colored(ui, icon, tooltip, None)
}

pub fn icon_button_colored(ui: &mut egui::Ui, icon: &str, tooltip: &str, color: Option<Color32>) -> egui::Response {
    let p = theme::palette(ui);
    let size = Vec2::splat(26.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        if resp.hovered() {
            ui.painter().rect_filled(rect, RADIUS, p.surface_hover);
        }
        let fg = color.unwrap_or(if resp.hovered() { p.text } else { p.muted });
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, icon, FontId::proportional(14.0), fg);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tooltip)
}

// ---------------------------------------------------------------- small pieces

pub enum ChipKind {
    Tag,
    Group,
}

/// Fixed-height rounded pill (does not stretch with the parent layout).
pub fn pill(ui: &mut egui::Ui, text: &str, fg: Color32, fill: Color32, sense: Sense) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(11.5), fg);
    let size = galley.size() + egui::vec2(16.0, 6.0);
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, rect.height() / 2.0, fill);
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, fg);
    }
    resp
}

/// A soft colored pill for tags / groups.
pub fn chip(ui: &mut egui::Ui, text: &str, kind: ChipKind) -> egui::Response {
    let color = label_color(text);
    let label = match kind {
        ChipKind::Tag => format!("#{text}"),
        ChipKind::Group => format!("▣ {text}"),
    };
    pill(ui, &label, color, color.gamma_multiply(0.16), Sense::hover())
}

/// A workspace pill: its icon and name in its color.
pub fn workspace_chip(ui: &mut egui::Ui, ws: &Workspace) -> egui::Response {
    let color = ws_color(ws.color);
    pill(ui, &ws.label(), color, color.gamma_multiply(0.16), Sense::hover())
}

/// Round color swatch for color pickers.
pub fn color_swatch(ui: &mut egui::Ui, color: Color32, selected: bool, tooltip: &str) -> egui::Response {
    let p = theme::palette(ui);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let c = rect.center();
        if selected {
            ui.painter().circle_stroke(c, 11.0, Stroke::new(2.0, p.text));
        } else if resp.hovered() {
            ui.painter().circle_stroke(c, 11.0, Stroke::new(1.0, p.muted));
        }
        ui.painter().circle_filled(c, 8.0, color);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tooltip)
}

/// Count badge, e.g. next to navigation items.
pub fn badge(ui: &mut egui::Ui, text: &str, highlighted: bool) {
    let p = theme::palette(ui);
    let (fill, fg) = if highlighted { (p.accent_soft(), p.accent_hover) } else { (p.surface_hover, p.muted) };
    pill(ui, text, fg, fill, Sense::hover());
}

/// Rounded square with the first letter of `text`, colored by `text` (favicon stand-in).
pub fn avatar(ui: &mut egui::Ui, text: &str, dimmed: bool) -> egui::Rect {
    avatar_sized(ui, text, dimmed, 30.0)
}

pub fn avatar_sized(ui: &mut egui::Ui, text: &str, dimmed: bool, size: f32) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let mut color = label_color(text);
    if dimmed {
        color = color.gamma_multiply(0.45);
    }
    let letter = text
        .trim_start_matches("www.")
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "•".into());
    ui.painter().rect_filled(rect, RADIUS, color.gamma_multiply(0.2));
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, letter, FontId::proportional(size / 2.0), color);
    rect
}

/// Color of a tab's status dot: loaded, hibernated or not open.
pub fn tab_state_color(ui: &egui::Ui, e: &TabEntry) -> Color32 {
    let p = theme::palette(ui);
    if !e.is_open() {
        p.muted
    } else if e.discarded {
        p.warn
    } else {
        p.success
    }
}

/// Small status dot drawn at the bottom-right corner of an avatar.
pub fn status_dot(ui: &egui::Ui, avatar: egui::Rect, fill: Color32) {
    let p = theme::palette(ui);
    let center = avatar.right_bottom() - Vec2::splat(2.0);
    ui.painter().circle_filled(center, 5.0, p.surface);
    ui.painter().circle_filled(center, 3.5, fill);
}

/// Full-width navigation entry (sidebar). Returns the click response.
pub fn nav_item(ui: &mut egui::Ui, icon: &str, label: &str, selected: bool, trailing: Option<(&str, bool)>) -> egui::Response {
    let p = theme::palette(ui);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 32.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let fill = if selected {
            p.accent_soft()
        } else if resp.hovered() {
            p.surface_hover
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, RADIUS, fill);
        let fg = if selected { p.accent_hover } else { p.text };
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect.shrink2(egui::vec2(10.0, 0.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        child.label(RichText::new(icon).color(if selected { p.accent_hover } else { p.muted }));
        child.add(egui::Label::new(RichText::new(label).color(fg)).truncate());
        if let Some((text, hl)) = trailing {
            child.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| badge(ui, text, hl));
        }
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Pill-style segmented control. Returns true if the value changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let p = theme::palette(ui);
    let mut changed = false;
    egui::Frame::new()
        .fill(p.input)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(RADIUS + 2)
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(|ui| {
                for (v, label) in options {
                    let selected = *value == *v;
                    let text = RichText::new(*label).color(if selected { p.on_accent } else { p.muted });
                    let btn = egui::Button::new(text)
                        .fill(if selected { p.accent } else { Color32::TRANSPARENT })
                        .stroke(Stroke::NONE)
                        .corner_radius(RADIUS);
                    if ui.add(btn).on_hover_cursor(CursorIcon::PointingHand).clicked() && !selected {
                        *value = *v;
                        changed = true;
                    }
                }
            });
        });
    changed
}

/// Floating notification in the bottom-right corner.
pub fn toast(ctx: &egui::Context, text: &str, error: bool, dark: bool) -> bool {
    let p = if dark { &theme::DARK } else { &theme::LIGHT };
    let mut dismissed = false;
    egui::Area::new(egui::Id::unique("toast"))
        .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -16.0))
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(p.surface)
                .stroke(Stroke::new(1.0, if error { p.danger.gamma_multiply(0.6) } else { p.border }))
                .corner_radius(10)
                .shadow(ui.visuals().popup_shadow)
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_max_width(420.0);
                    ui.horizontal(|ui| {
                        let (icon, color) = if error { ("⚠", p.danger) } else { ("✔", p.success) };
                        ui.label(RichText::new(icon).color(color));
                        ui.add(egui::Label::new(RichText::new(text).color(p.text)).wrap());
                        if icon_button(ui, "🗙", "Dismiss").clicked() {
                            dismissed = true;
                        }
                    });
                });
        });
    dismissed
}

pub fn empty_state(ui: &mut egui::Ui, icon: &str, title: &str, hint: &str) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(icon).size(34.0).weak());
        ui.add_space(6.0);
        ui.label(RichText::new(title).size(16.0).strong());
        if !hint.is_empty() {
            ui.label(RichText::new(hint).weak());
        }
    });
}

pub fn format_time(ms: f64) -> String {
    if ms <= 0.0 {
        return "never".into();
    }
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms));
    String::from(d.to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED))
}

/// Comma separated list -> trimmed non-empty set.
pub fn parse_list(s: &str) -> BTreeSet<String> {
    s.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect()
}

/// Kind selector + value field for a [`SitePattern`].
pub fn pattern_editor(ui: &mut egui::Ui, id: &str, p: &mut SitePattern, width: f32) {
    egui::ComboBox::from_id_salt(id).selected_text(p.kind.label()).width(170.0).show_ui(ui, |ui| {
        for k in PatternKind::ALL {
            ui.selectable_value(&mut p.kind, k, k.label());
        }
    });
    let hint = match p.kind {
        PatternKind::Host => "www.example.com",
        PatternKind::Domain => "example.com",
        PatternKind::UrlPrefix => "https://example.com/app/",
        PatternKind::Glob => "*.example.* or https://*/login*",
        PatternKind::Regex => r"^https://(www\.)?example\.",
    };
    ui.add(egui::TextEdit::singleline(&mut p.value).hint_text(hint).desired_width(width).margin(egui::vec2(8.0, 5.0)));
}

/// Inline validation message for a pattern.
pub fn pattern_error(p: &SitePattern) -> Option<String> {
    if p.value.trim().is_empty() {
        return None;
    }
    p.compile().err()
}

/// Single-line text input with the app's padding.
pub fn text_input<'a>(text: &'a mut String, hint: &str, width: f32) -> egui::TextEdit<'a> {
    egui::TextEdit::singleline(text).hint_text(hint.to_string()).desired_width(width).margin(egui::vec2(8.0, 5.0))
}
