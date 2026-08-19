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
use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU8, Ordering};

// ---------------------------------------------------------------------------
// Palettes. Every colour the workbench paints comes from one of these fields,
// so a new theme is a new value of `Palette` and nothing else: no call site
// anywhere knows which theme is active.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Palette {
    /// Application background behind and between the docked regions.
    pub bg: Color32,
    /// Docked chrome: header, side panels, confirmation rail.
    pub panel: Color32,
    /// Raised cards, inspector sections, and input surfaces.
    pub card: Color32,
    /// Hairline borders between chrome regions and around cards.
    pub border: Color32,
    /// Primary text on chrome.
    pub text: Color32,
    /// Secondary text: captions, group titles, de-emphasized values.
    pub muted: Color32,
    /// Command accent: active tools, links, primary actions.
    pub accent: Color32,
    /// Positive states: valid results, committed features, additive previews.
    pub good: Color32,
    /// Cautionary states: pending confirmation, stale data.
    pub warn: Color32,
    /// Failure states: rejected operations, invalid input, subtractive previews.
    pub bad: Color32,
    /// Ribbon strip fill, offset from the docked panels so the command surface
    /// reads as the topmost chrome layer.
    pub ribbon_fill: Color32,
    /// Fill behind widget rows in the bottom feature timeline.
    pub timeline_fill: Color32,
    /// Hovered interactive chrome.
    pub hover_fill: Color32,
    /// Pressed/active interactive chrome.
    pub active_fill: Color32,
    /// Fill for toggled-on (selected) chrome controls.
    pub selected_fill: Color32,
    /// Pale positive-state fill behind pending-confirmation chrome.
    pub good_fill: Color32,
    /// Top of the modeling-viewport gradient.
    pub viewport_top: Color32,
    /// Bottom of the modeling-viewport gradient.
    pub viewport_bottom: Color32,
    /// Whether egui should build its own widget defaults from a dark base.
    pub dark: bool,
    /// The sketch canvas: ground, grid, strokes, and markers.
    pub sketch: SketchColours,
}

/// Everything the sketch canvas paints. Kept as one block so the canvas can
/// follow the chrome's theme and be recoloured from the same editor, while
/// the canvas code still names its colours by role.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SketchColours {
    /// Canvas ground.
    pub background: Color32,
    /// Minor grid lines.
    pub grid_minor: Color32,
    /// Major grid lines.
    pub grid_major: Color32,
    /// Committed entity strokes.
    pub entity: Color32,
    /// Construction geometry: dashed, quieter than profile strokes.
    pub construction: Color32,
    /// The entity under the pointer.
    pub hovered: Color32,
    /// The selected entity, and a live (unconsumed) sketch's strokes.
    pub selected: Color32,
    /// A staged, not yet committed, candidate.
    pub pending: Color32,
    /// Geometry that cannot commit.
    pub invalid: Color32,
    /// The span Trim would remove.
    pub trim_hover: Color32,
    /// Snap markers on authored geometry.
    pub snap: Color32,
    /// Snap markers on the face-sketch support.
    pub snap_support: Color32,
    /// The sketch plane's first axis.
    pub axis_first: Color32,
    /// The sketch plane's second axis.
    pub axis_second: Color32,
    /// Dimension leaders, arrows, and text.
    pub dimension: Color32,
    /// A dimension that is driven by a typed value.
    pub dimension_locked: Color32,
    /// Fill behind dimension readouts.
    pub dimension_background: Color32,
    /// The host face of a face sketch.
    pub context_face: Color32,
    /// Edges of the host body behind a face sketch.
    pub context_edge: Color32,
    /// The selected face's boundary in a face sketch.
    pub context_selected_boundary: Color32,
    /// Fill of bounded profile cells; selected cells use it stronger.
    pub region_fill: Color32,
    /// Fill of the cell under the pointer.
    pub region_hover: Color32,
    /// Canvas overlay text: the plane prompt and the status line.
    pub overlay_text: Color32,
}

/// The sketch colours that match the light chrome: a near-white ground, the
/// same command blue for strokes, and the amber the selection always used.
pub const LIGHT_SKETCH: SketchColours = SketchColours {
    background: Color32::from_rgb(252, 253, 254),
    grid_minor: Color32::from_rgb(228, 233, 239),
    grid_major: Color32::from_rgb(206, 214, 223),
    entity: Color32::from_rgb(36, 82, 148),
    construction: Color32::from_rgb(122, 136, 154),
    hovered: Color32::from_rgb(52, 148, 226),
    selected: Color32::from_rgb(206, 128, 16),
    pending: Color32::from_rgb(23, 122, 67),
    invalid: Color32::from_rgb(189, 57, 52),
    trim_hover: Color32::from_rgb(222, 104, 30),
    snap: Color32::from_rgb(182, 136, 10),
    snap_support: Color32::from_rgb(20, 132, 138),
    axis_first: Color32::from_rgb(214, 69, 69),
    axis_second: Color32::from_rgb(52, 158, 106),
    dimension: Color32::from_rgb(24, 112, 172),
    dimension_locked: Color32::from_rgb(23, 122, 67),
    dimension_background: Color32::from_rgb(255, 255, 255),
    context_face: Color32::from_rgb(226, 233, 240),
    context_edge: Color32::from_rgba_unmultiplied_const(96, 116, 140, 185),
    context_selected_boundary: Color32::from_rgb(18, 102, 189),
    region_fill: Color32::from_rgb(18, 102, 189),
    region_hover: Color32::from_rgb(206, 128, 16),
    overlay_text: Color32::from_rgb(84, 96, 108),
};

/// The sketch colours that match the dark chrome. The ground sits between
/// the viewport gradient's ends so the two workspaces read as one window,
/// the grid is a step above it, and every stroke and marker is lifted until
/// it carries on that ground.
pub const DARK_SKETCH: SketchColours = SketchColours {
    background: Color32::from_rgb(30, 35, 42),
    grid_minor: Color32::from_rgb(40, 46, 55),
    grid_major: Color32::from_rgb(54, 62, 73),
    entity: Color32::from_rgb(142, 186, 236),
    construction: Color32::from_rgb(132, 146, 164),
    hovered: Color32::from_rgb(120, 196, 255),
    selected: Color32::from_rgb(236, 168, 64),
    pending: Color32::from_rgb(90, 196, 138),
    invalid: Color32::from_rgb(233, 118, 111),
    trim_hover: Color32::from_rgb(242, 142, 72),
    snap: Color32::from_rgb(226, 184, 64),
    snap_support: Color32::from_rgb(72, 194, 200),
    axis_first: Color32::from_rgb(236, 98, 98),
    axis_second: Color32::from_rgb(92, 204, 142),
    dimension: Color32::from_rgb(124, 180, 236),
    dimension_locked: Color32::from_rgb(90, 196, 138),
    dimension_background: Color32::from_rgb(42, 48, 57),
    context_face: Color32::from_rgb(46, 53, 62),
    context_edge: Color32::from_rgba_unmultiplied_const(150, 168, 192, 185),
    context_selected_boundary: Color32::from_rgb(93, 165, 240),
    region_fill: Color32::from_rgb(93, 165, 240),
    region_hover: Color32::from_rgb(225, 173, 88),
    overlay_text: Color32::from_rgb(155, 167, 181),
};

/// The light professional-CAD chrome: near-white ribbon and panels, dark
/// legible text, a restrained command-blue accent, and a pale blue-gray
/// gradient viewport.
pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(233, 237, 242),
    panel: Color32::from_rgb(242, 244, 247),
    card: Color32::from_rgb(255, 255, 255),
    border: Color32::from_rgb(198, 205, 214),
    text: Color32::from_rgb(31, 38, 46),
    muted: Color32::from_rgb(91, 102, 114),
    accent: Color32::from_rgb(18, 102, 189),
    good: Color32::from_rgb(23, 122, 67),
    warn: Color32::from_rgb(168, 106, 0),
    bad: Color32::from_rgb(189, 57, 52),
    ribbon_fill: Color32::from_rgb(245, 246, 248),
    timeline_fill: Color32::from_rgb(238, 241, 245),
    hover_fill: Color32::from_rgb(223, 231, 240),
    active_fill: Color32::from_rgb(208, 220, 234),
    selected_fill: Color32::from_rgb(214, 230, 247),
    good_fill: Color32::from_rgb(224, 240, 231),
    viewport_top: Color32::from_rgb(251, 252, 253),
    viewport_bottom: Color32::from_rgb(195, 206, 219),
    dark: false,
    sketch: LIGHT_SKETCH,
};

/// The dark chrome. Not an inversion of the light one: the neutrals keep a
/// slight blue bias toward the accent so the chrome reads as one family, the
/// accent and state colours are lifted until they carry on a dark ground, and
/// the viewport gradient stays darker than the panels so the model still reads
/// as the lit surface in the window.
pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(24, 28, 34),
    panel: Color32::from_rgb(32, 37, 44),
    card: Color32::from_rgb(42, 48, 57),
    border: Color32::from_rgb(62, 70, 81),
    text: Color32::from_rgb(228, 233, 239),
    muted: Color32::from_rgb(155, 167, 181),
    accent: Color32::from_rgb(93, 165, 240),
    good: Color32::from_rgb(90, 196, 138),
    warn: Color32::from_rgb(225, 173, 88),
    bad: Color32::from_rgb(233, 118, 111),
    ribbon_fill: Color32::from_rgb(36, 42, 50),
    timeline_fill: Color32::from_rgb(29, 34, 41),
    hover_fill: Color32::from_rgb(50, 58, 68),
    active_fill: Color32::from_rgb(62, 72, 85),
    selected_fill: Color32::from_rgb(31, 60, 90),
    good_fill: Color32::from_rgb(29, 54, 41),
    viewport_top: Color32::from_rgb(46, 53, 62),
    viewport_bottom: Color32::from_rgb(20, 24, 29),
    dark: true,
    sketch: DARK_SKETCH,
};

/// A theme the user can choose.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorkbenchTheme {
    #[default]
    Light,
    Dark,
}

impl WorkbenchTheme {
    pub const ALL: [Self; 2] = [Self::Light, Self::Dark];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// The theme's built-in palette, before any user edits.
    #[must_use]
    pub const fn default_palette(self) -> Palette {
        match self {
            Self::Light => LIGHT,
            Self::Dark => DARK,
        }
    }

    /// The theme's palette as currently in force: the built-in one, or the
    /// user's edited copy.
    #[must_use]
    pub fn palette(self) -> Palette {
        palette_for(self)
    }

    const fn index(self) -> usize {
        match self {
            Self::Light => 0,
            Self::Dark => 1,
        }
    }

    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }
}

/// The active theme, as an index into `WorkbenchTheme::ALL`.
///
/// A process-wide atomic rather than a value threaded through every paint call:
/// the palette is read hundreds of times per frame from code that has no other
/// reason to know about application state, and a `Relaxed` load of a `u8` is
/// cheaper than the plumbing would be. Changing it is a user action, so no
/// ordering between the write and the next frame's reads is required.
/// The theme in force. Dark is the default: a CAD viewport is a lit object on a
/// dark ground, and a light chrome around it makes the model the dimmer half of
/// its own window.
static ACTIVE_THEME: AtomicU8 = AtomicU8::new(1);

#[must_use]
pub fn active_theme() -> WorkbenchTheme {
    match ACTIVE_THEME.load(Ordering::Relaxed) {
        0 => WorkbenchTheme::Light,
        _ => WorkbenchTheme::Dark,
    }
}

/// Chooses the theme. Callers must re-run [`install_style`] afterwards so the
/// widget defaults egui derives from the palette are rebuilt too.
pub fn set_active_theme(theme: WorkbenchTheme) {
    ACTIVE_THEME.store(theme.index() as u8, Ordering::Relaxed);
}

/// The palette in force for each theme: the built-in values until the user
/// edits a colour. Read on every paint call; an uncontended read lock on a
/// `Copy` value is the price of letting colours change at run time.
static PALETTES: RwLock<[Palette; 2]> = RwLock::new([LIGHT, DARK]);

#[must_use]
pub fn palette() -> Palette {
    palette_for(active_theme())
}

#[must_use]
pub fn palette_for(theme: WorkbenchTheme) -> Palette {
    PALETTES
        .read()
        .map_or(theme.default_palette(), |palettes| palettes[theme.index()])
}

/// Replaces one theme's palette. The change is visible to the next paint
/// call; callers must re-run [`install_style`] when the active theme is the
/// one edited so egui's derived widget defaults follow.
pub fn set_palette(theme: WorkbenchTheme, palette: Palette) {
    if let Ok(mut palettes) = PALETTES.write() {
        palettes[theme.index()] = palette;
    }
}

/// Restores one theme's built-in palette.
pub fn reset_palette(theme: WorkbenchTheme) {
    set_palette(theme, theme.default_palette());
}

/// Whether a theme's palette differs from its built-in values.
#[must_use]
pub fn palette_is_customised(theme: WorkbenchTheme) -> bool {
    palette_for(theme) != theme.default_palette()
}

// ---------------------------------------------------------------------------
// Persistence. Colours travel as RGBA bytes, so the file is plain to read and
// hand-edit and does not depend on egui's own serialisation.
// ---------------------------------------------------------------------------

/// The theme choice and any edited palettes, as written to disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemePreferences {
    #[serde(default = "current_theme_preferences_version")]
    pub version: u32,
    pub active: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<PaletteRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dark: Option<PaletteRecord>,
}

pub const CURRENT_THEME_PREFERENCES_VERSION: u32 = 1;

const fn current_theme_preferences_version() -> u32 {
    CURRENT_THEME_PREFERENCES_VERSION
}

impl ThemePreferences {
    /// Captures the theme in force and every palette that differs from its
    /// built-in values.
    #[must_use]
    pub fn capture() -> Self {
        let record = |theme: WorkbenchTheme| {
            palette_is_customised(theme).then(|| PaletteRecord::from_palette(palette_for(theme)))
        };
        Self {
            version: CURRENT_THEME_PREFERENCES_VERSION,
            active: active_theme().label().to_owned(),
            light: record(WorkbenchTheme::Light),
            dark: record(WorkbenchTheme::Dark),
        }
    }

    /// Installs the recorded theme and palettes. A palette the file does not
    /// carry is the built-in one. Callers re-run [`install_style`] after.
    pub fn apply(&self) {
        for (theme, record) in [
            (WorkbenchTheme::Light, &self.light),
            (WorkbenchTheme::Dark, &self.dark),
        ] {
            set_palette(
                theme,
                record.as_ref().map_or(theme.default_palette(), |record| {
                    record.to_palette(theme.default_palette())
                }),
            );
        }
        let active = WorkbenchTheme::ALL
            .into_iter()
            .find(|theme| theme.label().eq_ignore_ascii_case(&self.active))
            .unwrap_or_default();
        set_active_theme(active);
    }
}

/// One palette as RGBA bytes per role. Every field is optional on the way
/// in, so a file from an earlier release that lacks a newer role still
/// applies; the missing role keeps the built-in value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PaletteRecord {
    pub chrome: std::collections::BTreeMap<String, [u8; 4]>,
    pub sketch: std::collections::BTreeMap<String, [u8; 4]>,
}

fn rgba(color: Color32) -> [u8; 4] {
    color.to_srgba_unmultiplied()
}

fn color(bytes: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(bytes[0], bytes[1], bytes[2], bytes[3])
}

impl PaletteRecord {
    #[must_use]
    pub fn from_palette(palette: Palette) -> Self {
        let mut record = Self::default();
        for (name, value) in palette.chrome_roles() {
            record.chrome.insert(name.to_owned(), rgba(value));
        }
        for (name, value) in palette.sketch.roles() {
            record.sketch.insert(name.to_owned(), rgba(value));
        }
        record
    }

    /// The recorded colours laid over `base`.
    #[must_use]
    pub fn to_palette(&self, base: Palette) -> Palette {
        let mut palette = base;
        for (name, bytes) in &self.chrome {
            if let Some(slot) = palette.chrome_role_mut(name) {
                *slot = color(*bytes);
            }
        }
        for (name, bytes) in &self.sketch {
            if let Some(slot) = palette.sketch.role_mut(name) {
                *slot = color(*bytes);
            }
        }
        palette
    }
}

macro_rules! colour_roles {
    ($type:ty, $roles:ident, $role_mut:ident, [$($name:ident),* $(,)?]) => {
        impl $type {
            /// Every colour role by its stable name, in declaration order.
            #[must_use]
            pub fn $roles(&self) -> Vec<(&'static str, Color32)> {
                vec![$((stringify!($name), self.$name)),*]
            }

            /// Mutable access to one colour role by its stable name.
            pub fn $role_mut(&mut self, name: &str) -> Option<&mut Color32> {
                match name {
                    $(stringify!($name) => Some(&mut self.$name),)*
                    _ => None,
                }
            }
        }
    };
}

colour_roles!(
    Palette,
    chrome_roles,
    chrome_role_mut,
    [
        bg,
        panel,
        card,
        border,
        text,
        muted,
        accent,
        good,
        warn,
        bad,
        ribbon_fill,
        timeline_fill,
        hover_fill,
        active_fill,
        selected_fill,
        good_fill,
        viewport_top,
        viewport_bottom,
    ]
);

colour_roles!(
    SketchColours,
    roles,
    role_mut,
    [
        background,
        grid_minor,
        grid_major,
        entity,
        construction,
        hovered,
        selected,
        pending,
        invalid,
        trim_hover,
        snap,
        snap_support,
        axis_first,
        axis_second,
        dimension,
        dimension_locked,
        dimension_background,
        context_face,
        context_edge,
        context_selected_boundary,
        region_fill,
        region_hover,
        overlay_text,
    ]
);

/// The sketch canvas colours of the theme in force.
#[must_use]
pub fn sketch() -> SketchColours {
    palette().sketch
}

macro_rules! palette_accessors {
    ($($name:ident),* $(,)?) => {
        $(
            #[must_use]
            pub fn $name() -> Color32 {
                palette().$name
            }
        )*
    };
}

palette_accessors!(
    bg,
    panel,
    card,
    border,
    text,
    muted,
    accent,
    good,
    warn,
    bad,
    ribbon_fill,
    timeline_fill,
    hover_fill,
    active_fill,
    selected_fill,
    good_fill,
    viewport_top,
    viewport_bottom,
);

// ---------------------------------------------------------------------------
// Style installation
// ---------------------------------------------------------------------------

/// Installs the light workbench style on the egui context. Idempotent; called
/// once per created context. The same style is installed for both theme slots
/// so a host- or harness-selected dark preference cannot reintroduce dark
/// widget chrome under the light workbench palette.
pub fn install_style(context: &egui::Context) {
    let palette = palette();
    context.set_theme(if palette.dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    for theme in [egui::Theme::Light, egui::Theme::Dark] {
        install_theme_slot(context, theme, palette);
    }
}

fn install_theme_slot(context: &egui::Context, theme: egui::Theme, palette: Palette) {
    // Both slots are installed from the active palette so a host- or
    // harness-selected theme preference cannot reintroduce widget chrome from
    // the palette the user did not choose.
    let mut style = (*context.style_of(theme)).clone();
    style.visuals = if palette.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
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
    style.visuals.panel_fill = palette.panel;
    style.visuals.window_fill = palette.card;
    style.visuals.extreme_bg_color = palette.card;
    style.visuals.faint_bg_color = palette.timeline_fill;
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.text);
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.border);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, palette.text);
    style.visuals.widgets.inactive.bg_fill = palette.hover_fill.gamma_multiply(0.85);
    style.visuals.widgets.inactive.weak_bg_fill = palette.hover_fill.gamma_multiply(0.85);
    style.visuals.widgets.hovered.bg_fill = palette.hover_fill;
    style.visuals.widgets.hovered.weak_bg_fill = palette.hover_fill;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, palette.border);
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, palette.text);
    style.visuals.widgets.active.bg_fill = palette.active_fill;
    style.visuals.widgets.active.weak_bg_fill = palette.active_fill;
    style.visuals.widgets.active.fg_stroke = Stroke::new(2.0, palette.text);
    style.visuals.widgets.open.bg_fill = palette.panel;
    style.visuals.widgets.open.weak_bg_fill = palette.panel;
    style.visuals.widgets.open.fg_stroke = Stroke::new(1.0, palette.text);
    style.visuals.widgets.open.bg_stroke = Stroke::new(1.0, palette.border);
    style.visuals.selection.bg_fill = palette.selected_fill;
    style.visuals.selection.stroke = Stroke::new(1.5, palette.accent);
    style.visuals.hyperlink_color = palette.accent;
    style.visuals.window_stroke = Stroke::new(1.0, palette.border);
    style.visuals.dark_mode = palette.dark;
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
                                    .color(muted()),
                            );
                        },
                    );
                    content.inner
                })
                .inner;
            ui.separator();
            output
        })
        .inner
    })
    .inner
}

/// Width of the name column in the property grid. Fixed rather than
/// content-derived so every value in the panel starts on the same column
/// whatever card it belongs to.
pub const PROPERTY_NAME_WIDTH: f32 = 92.0;

/// One row of the two-column property grid: a muted name, then its value.
///
/// Measurements read down a column far faster than they read as prose. A panel
/// that says "Volume 32.000 mm³" on one line and "Centre of mass [0, 0, 2] mm"
/// on the next makes the reader parse each sentence to find the number; the
/// same two facts as name/value rows are one glance.
pub fn property_row(ui: &mut egui::Ui, name: &str, value: &str) -> egui::Response {
    property_row_colored(ui, name, value, text())
}

pub fn property_row_colored(
    ui: &mut egui::Ui,
    name: &str,
    value: &str,
    colour: Color32,
) -> egui::Response {
    ui.horizontal(|ui| {
        // The name column is allocated and painted rather than laid out, so it
        // is exactly one width for every row: a value column that shifts with
        // the length of its own name is not a column.
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(PROPERTY_NAME_WIDTH, 15.0), egui::Sense::hover());
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            name,
            FontId::proportional(11.0),
            muted(),
        );
        let response = ui.add(
            egui::Label::new(RichText::new(value).small().color(colour))
                .selectable(false)
                .wrap(),
        );
        // The painted name is invisible to assistive technology, so the row
        // announces itself as one "name: value" fact.
        let announcement = format!("{name}: {value}");
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &announcement)
        });
        response
    })
    .inner
}

/// A property whose value does not exist yet, said in the value column rather
/// than as a sentence somewhere else, so the row still lines up.
pub fn property_row_unavailable(ui: &mut egui::Ui, name: &str, reason: &str) -> egui::Response {
    property_row_colored(ui, name, reason, muted())
}

/// Paints the standard modeling-viewport backdrop: a vertical gradient from
/// near-white to pale blue-gray, in the mainstream CAD tradition.
pub fn paint_viewport_gradient(painter: &egui::Painter, rect: egui::Rect) {
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), viewport_top());
    mesh.colored_vertex(rect.right_top(), viewport_top());
    mesh.colored_vertex(rect.right_bottom(), viewport_bottom());
    mesh.colored_vertex(rect.left_bottom(), viewport_bottom());
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

    /// Every theme, not only the active one. A dark palette that has never had
    /// its contrast checked is how "supports dark mode" turns into "has a dark
    /// mode nobody can read".
    #[test]
    fn chrome_text_meets_wcag_aa_contrast_in_every_theme() {
        for theme in WorkbenchTheme::ALL {
            let palette = theme.default_palette();
            for background in [
                palette.bg,
                palette.panel,
                palette.card,
                palette.ribbon_fill,
                palette.timeline_fill,
            ] {
                assert!(
                    contrast_ratio(palette.text, background) >= 4.5,
                    "{}: primary text must stay legible on chrome",
                    theme.label()
                );
                assert!(
                    contrast_ratio(palette.muted, background) >= 4.5,
                    "{}: secondary text must stay legible on chrome",
                    theme.label()
                );
                assert!(
                    contrast_ratio(palette.accent, background) >= 3.0,
                    "{}: accent chrome must stay distinguishable",
                    theme.label()
                );
            }
        }
    }

    #[test]
    fn state_colors_stay_legible_in_every_theme() {
        for theme in WorkbenchTheme::ALL {
            let palette = theme.default_palette();
            for state in [palette.good, palette.warn, palette.bad] {
                assert!(
                    contrast_ratio(state, palette.panel) >= 3.0,
                    "{}: state colors must read on chrome",
                    theme.label()
                );
            }
        }
    }

    /// A theme whose surfaces are indistinguishable from one another is one
    /// flat sheet: the panel/card/ribbon separation is what makes the docked
    /// regions readable as regions.
    #[test]
    fn every_theme_separates_its_chrome_surfaces() {
        for theme in WorkbenchTheme::ALL {
            let palette = theme.default_palette();
            for (name, first, second) in [
                ("bg/panel", palette.bg, palette.panel),
                ("panel/card", palette.panel, palette.card),
                ("panel/ribbon", palette.panel, palette.ribbon_fill),
            ] {
                let difference = relative_luminance(first) - relative_luminance(second);
                assert!(
                    difference.abs() > 0.002,
                    "{}: {name} are the same surface",
                    theme.label()
                );
            }
        }
    }

    #[test]
    fn choosing_a_theme_changes_what_every_accessor_returns() {
        set_active_theme(WorkbenchTheme::Dark);
        assert_eq!(active_theme(), WorkbenchTheme::Dark);
        assert_eq!(text(), DARK.text);
        assert!(palette().dark);
        set_active_theme(WorkbenchTheme::Light);
        assert_eq!(text(), LIGHT.text);
        assert!(!palette().dark);
    }

    /// The sketch canvas follows the chrome: its strokes must carry on its
    /// ground in both themes, and the ground must sit with the viewport.
    #[test]
    fn sketch_strokes_carry_on_their_canvas_in_every_theme() {
        for theme in WorkbenchTheme::ALL {
            let palette = theme.default_palette();
            let sketch = palette.sketch;
            for (name, stroke) in [
                ("entity", sketch.entity),
                ("hovered", sketch.hovered),
                ("selected", sketch.selected),
                ("pending", sketch.pending),
                ("invalid", sketch.invalid),
                ("dimension", sketch.dimension),
                ("overlay_text", sketch.overlay_text),
                ("axis_first", sketch.axis_first),
                ("axis_second", sketch.axis_second),
            ] {
                let ratio = contrast_ratio(stroke, sketch.background);
                assert!(
                    ratio >= 3.0,
                    "{}: sketch {name} contrast {ratio:.2} on the canvas",
                    theme.label()
                );
            }
            assert_eq!(
                relative_luminance(sketch.background) < 0.5,
                palette.dark,
                "{}: the canvas ground must be on the chrome's side of mid-grey",
                theme.label()
            );
            assert!(
                relative_luminance(sketch.grid_minor) != relative_luminance(sketch.background),
                "{}: the grid must be visible",
                theme.label()
            );
        }
    }

    #[test]
    fn edited_palettes_round_trip_through_preferences_and_missing_roles_keep_defaults() {
        let original = WorkbenchTheme::Dark.default_palette();
        let mut edited = original;
        edited.sketch.background = Color32::from_rgb(1, 2, 3);
        edited.accent = Color32::from_rgb(200, 20, 20);
        set_palette(WorkbenchTheme::Dark, edited);
        assert!(palette_is_customised(WorkbenchTheme::Dark));
        assert!(!palette_is_customised(WorkbenchTheme::Light));

        let captured = ThemePreferences::capture();
        assert!(captured.dark.is_some());
        assert!(captured.light.is_none(), "an unedited theme is not written");
        let json = serde_json::to_string(&captured).unwrap();

        reset_palette(WorkbenchTheme::Dark);
        assert_eq!(palette_for(WorkbenchTheme::Dark), original);
        let restored: ThemePreferences = serde_json::from_str(&json).unwrap();
        restored.apply();
        assert_eq!(palette_for(WorkbenchTheme::Dark), edited);

        // A file from an earlier release that names fewer roles, or one that
        // names an unknown role, still applies what it has.
        let mut sparse = captured.clone();
        let record = sparse.dark.as_mut().unwrap();
        record.sketch.retain(|name, _| name == "background");
        record.chrome.clear();
        record
            .chrome
            .insert("not_a_role".to_owned(), [9, 9, 9, 255]);
        sparse.apply();
        let applied = palette_for(WorkbenchTheme::Dark);
        assert_eq!(applied.sketch.background, Color32::from_rgb(1, 2, 3));
        assert_eq!(applied.accent, original.accent);
        reset_palette(WorkbenchTheme::Dark);
    }
}
