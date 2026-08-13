//! Central visual design system for the Artificer workbench.
//!
//! Every chrome color, font role, and shared chrome widget lives here so the
//! workbench reads as one product rather than a collection of debug panels.
//! The palette is a light professional-CAD chrome in the mainstream CAD/mainstream CAD
//! tradition: near-white ribbon and panels, dark legible text, a restrained
//! command-blue accent, and a pale blue-gray gradient viewport. Geometry and
//! transaction state never live here; the module knows nothing about the
//! kernel or the model document.

use egui::{Color32, FontId, RichText, Stroke};

// ---------------------------------------------------------------------------
// Core chrome tokens. Names are stable across the crate; values define the
// light workbench theme.
// ---------------------------------------------------------------------------

/// Application background behind and between the docked regions.
pub const BG: Color32 = Color32::from_rgb(233, 237, 242);
/// Docked chrome: header, side panels, confirmation rail.
pub const PANEL: Color32 = Color32::from_rgb(242, 244, 247);
/// Raised cards, inspector sections, and input surfaces.
pub const CARD: Color32 = Color32::from_rgb(255, 255, 255);
/// Hairline borders between chrome regions and around cards.
pub const BORDER: Color32 = Color32::from_rgb(198, 205, 214);
/// Primary text on chrome.
pub const TEXT: Color32 = Color32::from_rgb(31, 38, 46);
/// Secondary text: captions, group titles, de-emphasized values.
pub const MUTED: Color32 = Color32::from_rgb(91, 102, 114);
/// Command accent: active tools, links, primary actions.
pub const ACCENT: Color32 = Color32::from_rgb(18, 102, 189);
/// Positive states: valid results, committed features, additive previews.
pub const GOOD: Color32 = Color32::from_rgb(23, 122, 67);
/// Cautionary states: pending confirmation, stale data.
pub const WARN: Color32 = Color32::from_rgb(168, 106, 0);
/// Failure states: rejected operations, invalid input, subtractive previews.
pub const BAD: Color32 = Color32::from_rgb(189, 57, 52);

/// Ribbon strip fill, slightly lighter than the docked panels so the command
/// surface reads as the topmost chrome layer.
pub const RIBBON_FILL: Color32 = Color32::from_rgb(245, 246, 248);
/// Fill behind widget rows in the bottom feature timeline.
pub const TIMELINE_FILL: Color32 = Color32::from_rgb(238, 241, 245);
/// Hovered interactive chrome.
pub const HOVER_FILL: Color32 = Color32::from_rgb(223, 231, 240);
/// Pressed/active interactive chrome.
pub const ACTIVE_FILL: Color32 = Color32::from_rgb(208, 220, 234);
/// Fill for toggled-on (selected) chrome controls.
pub const SELECTED_FILL: Color32 = Color32::from_rgb(214, 230, 247);
/// Pale positive-state fill behind pending-confirmation chrome.
pub const GOOD_FILL: Color32 = Color32::from_rgb(224, 240, 231);

/// Top of the modeling-viewport gradient.
pub const VIEWPORT_TOP: Color32 = Color32::from_rgb(251, 252, 253);
/// Bottom of the modeling-viewport gradient.
pub const VIEWPORT_BOTTOM: Color32 = Color32::from_rgb(195, 206, 219);

// ---------------------------------------------------------------------------
// Style installation
// ---------------------------------------------------------------------------

/// Installs the light workbench style on the egui context. Idempotent; called
/// once per created context. The same style is installed for both theme slots
/// so a host- or harness-selected dark preference cannot reintroduce dark
/// widget chrome under the light workbench palette.
pub fn install_style(context: &egui::Context) {
    context.set_theme(egui::Theme::Light);
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        install_theme_slot(context, theme);
    }
}

fn install_theme_slot(context: &egui::Context, theme: egui::Theme) {
    // Both slots start from the stock light style so no dark-slot widget
    // default (window title bars, hover text, open-header fills) can leak
    // through when the host requests the dark theme.
    let mut style = (*context.style_of(egui::Theme::Light)).clone();
    style.visuals = egui::Visuals::light();
    style.spacing.item_spacing = egui::vec2(5.0, 4.0);
    style.spacing.button_padding = egui::vec2(7.0, 4.0);
    style.spacing.interact_size.y = 28.0;
    style
        .text_styles
        .insert(egui::TextStyle::Heading, FontId::proportional(16.0));
    style
        .text_styles
        .insert(egui::TextStyle::Body, FontId::proportional(13.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, FontId::proportional(12.5));
    style
        .text_styles
        .insert(egui::TextStyle::Small, FontId::proportional(11.0));
    style
        .text_styles
        .insert(egui::TextStyle::Monospace, FontId::monospace(11.0));
    style.visuals.panel_fill = PANEL;
    style.visuals.window_fill = CARD;
    style.visuals.extreme_bg_color = CARD;
    style.visuals.faint_bg_color = Color32::from_rgb(236, 239, 243);
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_rgb(44, 52, 61));
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(231, 235, 240);
    style.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(231, 235, 240);
    style.visuals.widgets.hovered.bg_fill = HOVER_FILL;
    style.visuals.widgets.hovered.weak_bg_fill = HOVER_FILL;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, Color32::from_rgb(160, 176, 194));
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, TEXT);
    style.visuals.widgets.active.bg_fill = ACTIVE_FILL;
    style.visuals.widgets.active.weak_bg_fill = ACTIVE_FILL;
    style.visuals.widgets.active.fg_stroke = Stroke::new(2.0, TEXT);
    style.visuals.widgets.open.bg_fill = PANEL;
    style.visuals.widgets.open.weak_bg_fill = PANEL;
    style.visuals.widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    style.visuals.widgets.open.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.selection.bg_fill = SELECTED_FILL;
    style.visuals.selection.stroke = Stroke::new(1.5, ACCENT);
    style.visuals.hyperlink_color = ACCENT;
    style.visuals.window_stroke = Stroke::new(1.0, BORDER);
    style.visuals.dark_mode = false;
    style.visuals.override_text_color = None;
    context.set_style_of(theme, style);
}

// ---------------------------------------------------------------------------
// Shared chrome widgets
// ---------------------------------------------------------------------------

/// Every ribbon group reserves the same content height so that the captions
/// underneath land on one baseline across the whole ribbon. A caption that
/// floats to wherever its own group happened to end reads as debris.
pub const RIBBON_GROUP_CONTENT_HEIGHT: f32 = 76.0;

/// One captioned command group inside the ribbon, Office-ribbon style: the
/// command row sits on top and the muted group caption is centered underneath,
/// followed by a vertical separator before the next group.
pub fn ribbon_group<R>(
    ui: &mut egui::Ui,
    title: &'static str,
    id: &'static str,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.push_id(id, |ui| {
        ui.horizontal(|ui| {
            let output = ui
                .vertical(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(4.0, 2.0);
                    let content = ui.scope(|ui| {
                        ui.set_min_height(RIBBON_GROUP_CONTENT_HEIGHT);
                        ui.spacing_mut().button_padding = egui::vec2(7.0, 3.0);
                        ui.spacing_mut().interact_size.y = 26.0;
                        ui.visuals_mut().widgets.inactive.bg_fill = Color32::TRANSPARENT;
                        ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                        // Top-aligned, so every group's commands start on the
                        // same line however tall the group turns out to be.
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                            contents(ui)
                        })
                        .inner
                    });
                    ui.add_space(1.0);
                    // The caption is centered under this group's commands, not
                    // under the remaining ribbon width.
                    let caption_width = content.response.rect.width().max(24.0);
                    ui.allocate_ui_with_layout(
                        egui::vec2(caption_width, 12.0),
                        egui::Layout::top_down(egui::Align::Center),
                        |ui| {
                            ui.label(
                                RichText::new(title)
                                    .font(FontId::proportional(9.5))
                                    .color(MUTED),
                            );
                        },
                    );
                    content.inner
                })
                .inner;
            ui.add_space(3.0);
            ui.separator();
            output
        })
        .inner
    })
    .inner
}

/// Paints the standard modeling-viewport backdrop: a vertical gradient from
/// near-white to pale blue-gray, in the mainstream CAD tradition.
pub fn paint_viewport_gradient(painter: &egui::Painter, rect: egui::Rect) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), VIEWPORT_TOP);
    mesh.colored_vertex(rect.right_top(), VIEWPORT_TOP);
    mesh.colored_vertex(rect.right_bottom(), VIEWPORT_BOTTOM);
    mesh.colored_vertex(rect.left_bottom(), VIEWPORT_BOTTOM);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relative_luminance(color: Color32) -> f64 {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
    }

    fn contrast_ratio(foreground: Color32, background: Color32) -> f64 {
        let lighter = relative_luminance(foreground).max(relative_luminance(background));
        let darker = relative_luminance(foreground).min(relative_luminance(background));
        (lighter + 0.05) / (darker + 0.05)
    }

    #[test]
    fn chrome_text_meets_wcag_aa_contrast_on_every_chrome_surface() {
        for background in [BG, PANEL, CARD, RIBBON_FILL, TIMELINE_FILL] {
            assert!(
                contrast_ratio(TEXT, background) >= 4.5,
                "primary text must stay legible on chrome"
            );
            assert!(
                contrast_ratio(MUTED, background) >= 4.5,
                "secondary text must stay legible on chrome"
            );
            assert!(
                contrast_ratio(ACCENT, background) >= 3.0,
                "accent chrome must stay distinguishable"
            );
        }
    }

    #[test]
    fn state_colors_stay_legible_on_light_chrome() {
        for state in [GOOD, WARN, BAD] {
            assert!(
                contrast_ratio(state, PANEL) >= 3.0,
                "state colors must read on light chrome"
            );
        }
    }
}
