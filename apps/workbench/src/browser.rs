//! The model Browser: the document tree docked on the left.
//!
//! The tree is where objects are *named*: bodies, sketches, and planes each
//! get a row, every row's eye toggles viewport visibility, and rows are
//! selectable so other commands can act on what the Browser holds. Bodies
//! support multi-selection (Cmd/Ctrl toggles one row, Shift extends from the
//! active body) because transform features like Mirror apply to a set, not to
//! one implicit body. A right-click on a row opens the Browser context menu,
//! sharing the floating-menu renderer with the viewport's right-click menu.

use std::collections::BTreeSet;

use eframe::egui;
use egui::{FontId, Frame, Margin, RichText, Stroke};

use artificer_model::JointKind;

use crate::command_icons::{CommandIcon, paint_command_icon};
use crate::sketch::SketchPlane;
use crate::theme;
use crate::{
    KernelLabApp, SolidFeaturePreset, WorkbenchMode, browser_body_object_name, origin_plane_label,
};

/// What a right-click in the Browser landed on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BrowserContextTarget {
    /// A body row, by ordinal: ordinals are the stable body identity across
    /// document rebuilds, indices are not.
    Body(u32),
    /// A committed sketch row, by index into the sketches list.
    Sketch(usize),
    ConstructionPlane(u64),
    OriginPlane(SketchPlane),
}

/// The Browser's floating right-click menu.
#[derive(Clone, Copy)]
pub(crate) struct BrowserContextMenu {
    position: egui::Pos2,
    target: BrowserContextTarget,
    /// The right-click that opened the menu is still in this frame's input, so
    /// a "was anything clicked elsewhere" test would close the menu on the
    /// very frame it appears.
    just_opened: bool,
}

/// One command the Browser context menu offers. Labels are also the accessible
/// names, and each is distinct from every other accessible name in the shell —
/// including the viewport context menu's — because an ambiguous name is not a
/// name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BrowserContextCommand {
    SetActiveBody,
    AssignMaterial,
    ExportBodyStl,
    ExportBodyStep,
    HideSelectedBodies,
    ShowSelectedBodies,
    IsolateSelectedBodies,
    UnhideAllBodies,
    MirrorSelectedBodies,
    EditSketch,
    SelectSketch,
    ExportSketchDxf,
    HideSketch,
    ShowSketch,
    SelectPlane,
    HidePlane,
    ShowPlane,
}

impl BrowserContextCommand {
    const fn label(self) -> &'static str {
        match self {
            Self::SetActiveBody => "Set as active body",
            Self::AssignMaterial => "Assign material…",
            Self::ExportBodyStl => "Export this body as STL",
            Self::ExportBodyStep => "Export this body as STEP",
            Self::HideSelectedBodies => "Hide selected bodies",
            Self::ShowSelectedBodies => "Show selected bodies",
            Self::IsolateSelectedBodies => "Isolate selected bodies",
            Self::UnhideAllBodies => "Unhide all bodies",
            Self::MirrorSelectedBodies => "Mirror across selected plane",
            Self::EditSketch => "Edit this sketch",
            Self::SelectSketch => "Select this sketch",
            Self::ExportSketchDxf => "Export this sketch as DXF",
            Self::HideSketch => "Hide this sketch",
            Self::ShowSketch => "Show this sketch",
            Self::SelectPlane => "Select this plane",
            Self::HidePlane => "Hide this plane",
            Self::ShowPlane => "Show this plane",
        }
    }
}

/// What one frame of a floating context menu resolved to.
pub(crate) struct FloatingMenu {
    pub(crate) chosen: Option<usize>,
    pub(crate) escape: bool,
    pub(crate) clicked_elsewhere: bool,
}

/// The one floating right-click menu renderer, shared by the viewport and
/// Browser menus so the two cannot drift apart visually or behaviourally.
pub(crate) fn floating_context_menu(
    context: &egui::Context,
    id_source: &'static str,
    position: egui::Pos2,
    labels: &[&'static str],
    just_opened: bool,
) -> FloatingMenu {
    let item_height = 22.0;
    let item_spacing = 2.0;
    let margin = 5.0;
    let inner = egui::vec2(
        208.0,
        labels.len() as f32 * item_height + (labels.len().saturating_sub(1)) as f32 * item_spacing,
    );
    let size = inner + egui::Vec2::splat(margin * 2.0);
    let screen = context.content_rect();
    let origin = egui::pos2(
        position
            .x
            .clamp(screen.left(), (screen.right() - size.x).max(screen.left())),
        position
            .y
            .clamp(screen.top(), (screen.bottom() - size.y).max(screen.top())),
    );
    let area = egui::Area::new(egui::Id::new(id_source))
        .fixed_pos(origin)
        // Without a size hint the first frame runs egui's constrain pass
        // against an unknown size and lands the menu mid-screen; the very
        // first click aimed at an item then misses.
        .default_size(size)
        .constrain(false)
        .order(egui::Order::Foreground)
        .show(context, |ui| {
            // The margin and the gaps between rows sense nothing, so without
            // this backstop a click on the menu's own padding falls through to
            // whatever sits underneath and reopens a different menu.
            ui.interact(
                egui::Rect::from_min_size(origin, size),
                egui::Id::new((id_source, "backstop")),
                egui::Sense::click(),
            );
            Frame::new()
                .fill(theme::panel())
                .stroke(Stroke::new(1.0, theme::border()))
                .corner_radius(4)
                .inner_margin(Margin::same(margin as i8))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = item_spacing;
                    ui.set_min_size(inner);
                    ui.set_max_width(inner.x);
                    let mut chosen = None;
                    let mut first_item = None;
                    for (index, label) in labels.iter().enumerate() {
                        let response = ui.add_sized(
                            [inner.x, item_height],
                            egui::Button::new(
                                RichText::new(*label)
                                    .font(FontId::proportional(12.0))
                                    .color(theme::text()),
                            )
                            .frame(false),
                        );
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, *label)
                        });
                        first_item.get_or_insert(response.id);
                        if response.clicked() {
                            chosen = Some(index);
                        }
                    }
                    // A menu nobody can reach from the keyboard is not a menu.
                    // Focus the first command as it opens so Tab, arrows, and
                    // a screen reader all start inside it rather than at the
                    // far end of the shell.
                    if just_opened && let Some(id) = first_item {
                        ui.ctx().memory_mut(|memory| memory.request_focus(id));
                    }
                    chosen
                })
                .inner
        });
    // `Response` pointer queries read the context's input themselves, so they
    // are sampled before the keyboard read rather than inside it.
    let clicked_elsewhere = area.response.clicked_elsewhere();
    let escape = context.input(|input| input.key_pressed(egui::Key::Escape));
    FloatingMenu {
        chosen: area.inner,
        escape,
        clicked_elsewhere,
    }
}

/// A 22×22 frameless eye button: open eye when the object is drawn in the
/// viewport, closed eye when it is hidden. The accessible name doubles as the
/// hover text and always says what a click will do.
fn visibility_toggle(ui: &mut egui::Ui, visible: bool, action_label: &str) -> egui::Response {
    let response = ui.add_sized(
        [22.0, 22.0],
        egui::Button::new("").frame(false).corner_radius(2),
    );
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, action_label));
    let icon_rect = egui::Rect::from_center_size(response.rect.center(), egui::vec2(15.0, 15.0));
    let (icon, color) = if visible {
        (CommandIcon::Visible, theme::text())
    } else {
        (CommandIcon::Hidden, theme::muted())
    };
    paint_command_icon(ui.painter(), icon_rect, icon, color);
    response.on_hover_text(action_label)
}

/// The marker slot at the left of every Browser row, matching the eye's
/// 15×15 icon so the two columns read as one grid.
const ROW_MARKER_SIZE: egui::Vec2 = egui::Vec2::splat(15.0);

/// One interactive Browser row: a painted marker, then a truncating label,
/// in a single full-width click target. The marker rides inside the button
/// as a custom atom so selection and hover chrome cover it exactly as they
/// covered the old text prefix, and the trailing grow atom pins the content
/// to the left edge. Markers are painted vectors because the embedded font
/// has no glyphs for them. `icon_color` fixes the marker colour; leave it
/// `None` to follow the label's state-dependent text colour.
fn browser_row_button(
    ui: &mut egui::Ui,
    icon: CommandIcon,
    icon_color: Option<egui::Color32>,
    text: impl Into<egui::WidgetText>,
    width: f32,
    selected: bool,
) -> egui::Response {
    let marker = egui::Id::new("browser_row_marker");
    let layout = egui::Button::new((
        egui::Atom::custom(marker, ROW_MARKER_SIZE),
        text.into(),
        egui::Atom::grow(),
    ))
    .frame(false)
    .selected(selected)
    .corner_radius(2)
    .truncate()
    .small()
    .min_size(egui::vec2(width, 22.0))
    .atom_ui(ui);
    if let Some(rect) = layout.rect(marker) {
        let color = icon_color.unwrap_or_else(|| {
            ui.style()
                .interact_selectable(&layout.response, selected)
                .text_color()
        });
        paint_command_icon(ui.painter(), rect, icon, color);
    }
    layout.response
}

/// A non-interactive Browser row: a painted marker and a truncating label,
/// with the full text as both the hover text and the accessible name, as
/// the plain-label rows had before the markers were painted.
fn browser_icon_text_row(ui: &mut egui::Ui, icon: CommandIcon, text: &str, color: egui::Color32) {
    let marker = egui::Id::new("browser_row_marker");
    let layout = egui::AtomLayout::new((
        egui::Atom::custom(marker, ROW_MARKER_SIZE),
        RichText::new(text)
            .font(FontId::proportional(11.5))
            .color(color),
    ))
    .align2(egui::Align2::LEFT_CENTER)
    .sense(egui::Sense::hover())
    .wrap_mode(egui::TextWrapMode::Truncate)
    .min_size(egui::vec2(ui.available_width(), 24.0))
    .show(ui);
    if let Some(rect) = layout.rect(marker) {
        paint_command_icon(ui.painter(), rect, icon, color);
    }
    let response = layout.response;
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    response.on_hover_text(text);
}

impl KernelLabApp {
    pub(crate) fn model_browser(&mut self, ui: &mut egui::Ui) {
        // Body ordinals can disappear across undo, reload, and history moves;
        // a selection naming a body that no longer exists selects nothing.
        let live_ordinals = self
            .bodies
            .iter()
            .map(|body| body.ordinal)
            .collect::<BTreeSet<_>>();
        self.browser_selected_bodies
            .retain(|ordinal| live_ordinals.contains(ordinal));
        let mut context_request: Option<(egui::Pos2, BrowserContextTarget)> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().interact_size.y = 24.0;
                egui::CollapsingHeader::new(
                    RichText::new("Document 1 · Root")
                        .font(FontId::proportional(12.5))
                        .color(theme::text())
                        .strong(),
                )
                .id_salt("browser_document")
                .default_open(true)
                .show(ui, |ui| {
                    egui::CollapsingHeader::new(
                        RichText::new("Origin")
                            .font(FontId::proportional(12.0))
                            .color(theme::text())
                            .strong(),
                    )
                    .id_salt("browser_origin")
                    .default_open(true)
                    .show(ui, |ui| {
                        for plane in SketchPlane::ALL {
                            let has_other_plane_sketch =
                                !self.sketch.entities().is_empty() && self.sketch.plane() != plane;
                            let enabled =
                                self.pending_operation.is_none() && !has_other_plane_sketch;
                            let selected = self.selected_origin_plane == plane
                                && self.selected_construction_plane.is_none();
                            let response = ui.add_enabled(
                                enabled,
                                egui::Button::new(origin_plane_label(plane))
                                    .frame(false)
                                    .selected(selected)
                                    .corner_radius(2)
                                    .min_size(egui::vec2(ui.available_width(), 24.0)),
                            );
                            if response.clicked() {
                                self.select_origin_plane(plane);
                            }
                            if response.secondary_clicked() {
                                let position = response
                                    .interact_pointer_pos()
                                    .unwrap_or_else(|| response.rect.left_bottom());
                                context_request =
                                    Some((position, BrowserContextTarget::OriginPlane(plane)));
                            }
                            if has_other_plane_sketch {
                                response.on_disabled_hover_text(
                                    "This first profile slice owns one plane per document.",
                                );
                            }
                        }
                    });
                    if !self.construction_planes.is_empty() {
                        let rows = self
                            .construction_planes
                            .iter()
                            .filter(|plane| self.construction_plane_is_active(plane))
                            .map(|plane| {
                                (
                                    plane.id,
                                    plane.name.clone(),
                                    plane.visible,
                                    self.selected_construction_plane == Some(plane.id),
                                )
                            })
                            .collect::<Vec<_>>();
                        let mut visibility_change = None;
                        let mut selected_plane = None;
                        egui::CollapsingHeader::new(
                            RichText::new(format!("Construction ({})", rows.len()))
                                .font(FontId::proportional(12.0))
                                .color(theme::text())
                                .strong(),
                        )
                        .id_salt("browser_construction_planes")
                        .default_open(true)
                        .show(ui, |ui| {
                            for (id, name, visible, selected) in rows {
                                ui.horizontal(|ui| {
                                    let action_label = if visible {
                                        format!("Hide {name}")
                                    } else {
                                        format!("Show {name}")
                                    };
                                    if visibility_toggle(ui, visible, &action_label).clicked() {
                                        visibility_change = Some((id, !visible));
                                    }
                                    let response = browser_row_button(
                                        ui,
                                        CommandIcon::Plane,
                                        None,
                                        name.clone(),
                                        (ui.available_width() - 2.0).max(24.0),
                                        selected,
                                    );
                                    // "Plane 1" alone would collide with the
                                    // history stop of the same name, and an
                                    // ambiguous name is not a name.
                                    response.widget_info(|| {
                                        egui::WidgetInfo::labeled(
                                            egui::WidgetType::Button,
                                            true,
                                            format!("Select {name}"),
                                        )
                                    });
                                    let response = response.on_hover_text(format!(
                                        "Select {name} as a sketch support plane"
                                    ));
                                    if response.clicked() {
                                        selected_plane = Some(id);
                                    }
                                    if response.secondary_clicked() {
                                        let position = response
                                            .interact_pointer_pos()
                                            .unwrap_or_else(|| response.rect.left_bottom());
                                        context_request = Some((
                                            position,
                                            BrowserContextTarget::ConstructionPlane(id),
                                        ));
                                    }
                                });
                            }
                        });
                        if let Some((id, visible)) = visibility_change
                            && let Some(plane) = self
                                .construction_planes
                                .iter_mut()
                                .find(|plane| plane.id == id)
                        {
                            plane.visible = visible;
                        }
                        if let Some(id) = selected_plane {
                            self.selected_construction_plane = Some(id);
                            self.clear_model_entity_selection();
                        }
                    }
                    let body_rows = self
                        .bodies
                        .iter()
                        .enumerate()
                        .map(|(index, body)| {
                            let component = self
                                .document
                                .component_instances()
                                .iter()
                                .find(|component| component.bodies.contains(&body.id))
                                .map(|component| (component.id.get(), component.label.clone()));
                            (
                                index,
                                body.ordinal,
                                body.kind,
                                body.body.report.topology.solids,
                                body.visible,
                                body.ordinal == self.active_body_ordinal,
                                component,
                            )
                        })
                        .collect::<Vec<_>>();
                    let mut body_visibility_change = None;
                    let mut body_click = None;
                    for (index, ordinal, kind, solid_count, visible, active, component) in body_rows
                    {
                        ui.horizontal(|ui| {
                            let object_name = browser_body_object_name(ordinal, solid_count);
                            let visibility_label = if visible {
                                format!("Hide {object_name}")
                            } else {
                                format!("Show {object_name}")
                            };
                            if visibility_toggle(ui, visible, &visibility_label).clicked() {
                                body_visibility_change = Some((index, !visible));
                            }
                            let (body_icon, body_label, visible_body_label) = component
                                .map_or_else(
                                    || {
                                        let label =
                                            format!("{object_name} · {}", kind.browser_label());
                                        (CommandIcon::Body, label.clone(), label)
                                    },
                                    |(instance, label)| {
                                        (
                                            CommandIcon::Component,
                                            format!("{label} · component {instance}"),
                                            format!("C{instance} · {label}"),
                                        )
                                    },
                                );
                            let in_selection = self.browser_selected_bodies.contains(&ordinal);
                            let label_width = (ui.available_width() - 6.0).max(24.0);
                            let response = browser_row_button(
                                ui,
                                body_icon,
                                None,
                                visible_body_label,
                                label_width,
                                active || in_selection,
                            );
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    true,
                                    &body_label,
                                )
                            });
                            let response = response.on_hover_text(&body_label);
                            if response.clicked() {
                                let modifiers = ui.input(|input| input.modifiers);
                                body_click = Some((index, ordinal, modifiers));
                            }
                            if response.secondary_clicked() {
                                let position = response
                                    .interact_pointer_pos()
                                    .unwrap_or_else(|| response.rect.left_bottom());
                                context_request =
                                    Some((position, BrowserContextTarget::Body(ordinal)));
                            }
                        });
                    }
                    if let Some((index, visible)) = body_visibility_change {
                        self.set_body_visibility(index, visible);
                    }
                    if let Some((index, ordinal, modifiers)) = body_click {
                        self.browser_body_row_clicked(index, ordinal, modifiers);
                    }

                    let sketch_rows = self
                        .sketches
                        .iter()
                        .enumerate()
                        .filter(|(_, sketch)| {
                            sketch
                                .id
                                .is_none_or(|id| self.document.sketch(id).is_some())
                        })
                        .map(|(index, sketch)| {
                            (
                                index,
                                sketch.ordinal,
                                sketch.support.label(),
                                sketch.finished,
                                sketch.visible,
                                sketch.consumed,
                                self.active_sketch_index == Some(index),
                            )
                        })
                        .collect::<Vec<_>>();
                    let mut sketch_visibility_change = None;
                    let mut edit_sketch = None;
                    for (index, ordinal, support, finished, visible, consumed, active) in
                        sketch_rows
                    {
                        ui.horizontal(|ui| {
                            let visibility_label = if visible {
                                format!("Hide Sketch {ordinal}")
                            } else {
                                format!("Show Sketch {ordinal}")
                            };
                            if visibility_toggle(ui, visible, &visibility_label).clicked() {
                                sketch_visibility_change = Some((index, !visible));
                            }
                            let state = if finished || consumed {
                                "finished"
                            } else {
                                "editing"
                            };
                            let row_color = if visible {
                                theme::accent()
                            } else {
                                theme::muted()
                            };
                            let response = browser_row_button(
                                ui,
                                CommandIcon::Sketch,
                                Some(row_color),
                                RichText::new(format!("Sketch {ordinal} · {support} · {state}"))
                                    .color(row_color),
                                (ui.available_width() - 2.0).max(24.0),
                                active,
                            )
                            .on_hover_text(format!("Open Sketch {ordinal} for editing"));
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    true,
                                    format!("Edit Sketch {ordinal}"),
                                )
                            });
                            if response.clicked() {
                                edit_sketch = Some(index);
                            }
                            if response.secondary_clicked() {
                                let position = response
                                    .interact_pointer_pos()
                                    .unwrap_or_else(|| response.rect.left_bottom());
                                context_request =
                                    Some((position, BrowserContextTarget::Sketch(index)));
                            }
                        });
                    }
                    if self.workbench_mode == WorkbenchMode::Sketch
                        && self.active_sketch_index.is_none()
                        && self.sketch.entities().is_empty()
                    {
                        browser_icon_text_row(
                            ui,
                            CommandIcon::Sketch,
                            &format!(
                                "Sketch {} · {} · empty",
                                self.feature_preview.current_sketch_ordinal(),
                                self.sketch_support.label()
                            ),
                            theme::accent(),
                        );
                    }
                    if let Some((index, visible)) = sketch_visibility_change {
                        self.set_sketch_visibility(index, visible);
                    }
                    if let Some(index) = edit_sketch {
                        // The row's left-click is the contextually correct
                        // action for a sketch: open it for editing. The eye
                        // handles visibility and the right-click menu carries
                        // everything else.
                        self.edit_committed_sketch(index);
                    }

                    let joint_rows = self
                        .document
                        .joints()
                        .iter()
                        .map(|joint| {
                            (
                                joint.id,
                                joint.name.clone(),
                                joint.child,
                                joint.kind,
                                joint.enabled,
                            )
                        })
                        .collect::<Vec<_>>();
                    if !joint_rows.is_empty() {
                        egui::CollapsingHeader::new(
                            RichText::new(format!("Joints ({})", joint_rows.len()))
                                .font(FontId::proportional(12.0))
                                .color(theme::text())
                                .strong(),
                        )
                        .id_salt("browser_joints")
                        .default_open(true)
                        .show(ui, |ui| {
                            for (id, name, child, kind, enabled) in joint_rows {
                                let kind = match kind {
                                    JointKind::Fixed => "Fixed",
                                    JointKind::Revolute { .. } => "Revolute",
                                };
                                browser_icon_text_row(
                                    ui,
                                    if enabled {
                                        CommandIcon::Joint
                                    } else {
                                        CommandIcon::JointDisabled
                                    },
                                    &format!("{name} · {kind} · C{} · {id}", child.get()),
                                    if enabled {
                                        theme::good()
                                    } else {
                                        theme::muted()
                                    },
                                );
                            }
                        });
                    }
                });
            });
        if let Some((position, target)) = context_request {
            self.open_browser_context_menu(position, target);
        }
    }

    /// One left click on an origin plane row, shared with the context menu's
    /// "Select this plane".
    pub(crate) fn select_origin_plane(&mut self, plane: SketchPlane) {
        self.selected_origin_plane = plane;
        self.selected_construction_plane = None;
        if self.sketch.entities().is_empty() {
            let _ = self.sketch.set_plane(plane);
        }
    }

    /// One left click on a body row. Plain click makes the row the whole
    /// selection and the active body; Cmd/Ctrl toggles the row in and out of
    /// the selection without moving the active body; Shift selects the run of
    /// rows between the active body and the click.
    fn browser_body_row_clicked(&mut self, index: usize, ordinal: u32, modifiers: egui::Modifiers) {
        if modifiers.command {
            if !self.browser_selected_bodies.remove(&ordinal) {
                self.browser_selected_bodies.insert(ordinal);
            }
            return;
        }
        if modifiers.shift {
            let order = self
                .bodies
                .iter()
                .map(|body| body.ordinal)
                .collect::<Vec<_>>();
            let anchor = order
                .iter()
                .position(|&candidate| candidate == self.active_body_ordinal)
                .unwrap_or(0);
            let clicked = order
                .iter()
                .position(|&candidate| candidate == ordinal)
                .unwrap_or(anchor);
            let (from, to) = if anchor <= clicked {
                (anchor, clicked)
            } else {
                (clicked, anchor)
            };
            self.browser_selected_bodies = order[from..=to].iter().copied().collect();
            return;
        }
        self.browser_selected_bodies.clear();
        self.browser_selected_bodies.insert(ordinal);
        self.activate_body(index);
        self.clear_model_entity_selection();
    }

    /// Indices of the browser-selected bodies, in Browser row order. Stale
    /// ordinals drop out silently: what no longer exists is not selected.
    pub(crate) fn browser_selected_body_indices(&self) -> Vec<usize> {
        self.bodies
            .iter()
            .enumerate()
            .filter(|(_, body)| self.browser_selected_bodies.contains(&body.ordinal))
            .map(|(index, _)| index)
            .collect()
    }

    /// The labels the open Browser context menu offers, top to bottom, empty
    /// while the menu is closed. For semantic UI tests, the Browser twin of
    /// [`KernelLabApp::model_context_menu_labels`].
    #[must_use]
    pub fn browser_context_menu_labels(&self) -> Vec<&'static str> {
        self.browser_context_menu
            .map(|menu| {
                self.browser_context_commands(menu.target)
                    .iter()
                    .map(|command| command.label())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Ordinals of the Browser-selected bodies, in row order. For semantic UI
    /// tests.
    #[must_use]
    pub fn browser_selected_body_ordinals(&self) -> Vec<u32> {
        self.browser_selected_bodies.iter().copied().collect()
    }

    fn open_browser_context_menu(&mut self, position: egui::Pos2, target: BrowserContextTarget) {
        if self.workbench_mode != WorkbenchMode::Model || self.pending_operation.is_some() {
            return;
        }
        if let BrowserContextTarget::Body(ordinal) = target {
            // A right-click acts on the selection it lands in. Landing outside
            // the current selection makes the row the selection, exactly as a
            // left click would.
            if !self.browser_selected_bodies.contains(&ordinal) {
                self.browser_selected_bodies.clear();
                self.browser_selected_bodies.insert(ordinal);
                if let Some(index) = self.bodies.iter().position(|body| body.ordinal == ordinal) {
                    self.activate_body(index);
                    self.clear_model_entity_selection();
                }
            }
        }
        // One right-click menu at a time: the Browser menu replaces the
        // viewport menu and vice versa.
        self.model_context_menu = None;
        self.browser_context_menu = Some(BrowserContextMenu {
            position,
            target,
            just_opened: true,
        });
    }

    fn browser_context_commands(&self, target: BrowserContextTarget) -> Vec<BrowserContextCommand> {
        let mut commands = Vec::new();
        match target {
            BrowserContextTarget::Body(ordinal) => {
                let selected = self.browser_selected_body_indices();
                if ordinal != self.active_body_ordinal {
                    commands.push(BrowserContextCommand::SetActiveBody);
                }
                if selected.iter().any(|&index| self.bodies[index].visible) {
                    commands.push(BrowserContextCommand::HideSelectedBodies);
                }
                if selected.iter().any(|&index| {
                    !self.bodies[index].visible && self.body_visibility_grantable(index)
                }) {
                    commands.push(BrowserContextCommand::ShowSelectedBodies);
                }
                if self
                    .bodies
                    .iter()
                    .enumerate()
                    .any(|(index, body)| body.visible && !selected.contains(&index))
                {
                    commands.push(BrowserContextCommand::IsolateSelectedBodies);
                }
                if self.bodies.iter().any(|body| !body.visible) {
                    commands.push(BrowserContextCommand::UnhideAllBodies);
                }
                if self.history_is_at_end() {
                    commands.push(BrowserContextCommand::MirrorSelectedBodies);
                }
                commands.push(BrowserContextCommand::AssignMaterial);
                commands.push(BrowserContextCommand::ExportBodyStl);
                commands.push(BrowserContextCommand::ExportBodyStep);
            }
            BrowserContextTarget::Sketch(index) => {
                let Some(sketch) = self.sketches.get(index) else {
                    return commands;
                };
                if self.history_is_at_end() {
                    commands.push(BrowserContextCommand::EditSketch);
                }
                if self.active_sketch_index != Some(index) && self.history_is_at_end() {
                    commands.push(BrowserContextCommand::SelectSketch);
                }
                commands.push(BrowserContextCommand::ExportSketchDxf);
                commands.push(if sketch.visible {
                    BrowserContextCommand::HideSketch
                } else {
                    BrowserContextCommand::ShowSketch
                });
            }
            BrowserContextTarget::ConstructionPlane(id) => {
                let Some(plane) = self.construction_planes.iter().find(|plane| plane.id == id)
                else {
                    return commands;
                };
                if self.selected_construction_plane != Some(id) {
                    commands.push(BrowserContextCommand::SelectPlane);
                }
                commands.push(if plane.visible {
                    BrowserContextCommand::HidePlane
                } else {
                    BrowserContextCommand::ShowPlane
                });
            }
            BrowserContextTarget::OriginPlane(plane) => {
                let has_other_plane_sketch =
                    !self.sketch.entities().is_empty() && self.sketch.plane() != plane;
                let already_selected = self.selected_origin_plane == plane
                    && self.selected_construction_plane.is_none();
                if !has_other_plane_sketch && !already_selected {
                    commands.push(BrowserContextCommand::SelectPlane);
                }
            }
        }
        commands
    }

    /// Whether showing this body is the Browser's to grant: a body inside a
    /// hidden or suppressed component stays hidden either way.
    fn body_visibility_grantable(&self, index: usize) -> bool {
        self.component_for_body(self.bodies[index].id)
            .is_none_or(|component| component.visible && !component.suppressed)
    }

    pub(crate) fn show_all_eligible_bodies(&mut self) {
        for index in 0..self.bodies.len() {
            if !self.bodies[index].visible && self.body_visibility_grantable(index) {
                self.set_body_visibility(index, true);
            }
        }
    }

    /// The Browser's right-click menu.
    pub(crate) fn show_browser_context_menu(&mut self, context: &egui::Context) {
        if self.workbench_mode != WorkbenchMode::Model || self.pending_operation.is_some() {
            self.browser_context_menu = None;
            return;
        }
        let Some(mut menu) = self.browser_context_menu else {
            return;
        };
        let commands = self.browser_context_commands(menu.target);
        if commands.is_empty() {
            self.browser_context_menu = None;
            return;
        }
        let labels = commands
            .iter()
            .map(|command| command.label())
            .collect::<Vec<_>>();
        let outcome = floating_context_menu(
            context,
            "browser_context_menu",
            menu.position,
            &labels,
            menu.just_opened,
        );
        if let Some(chosen) = outcome.chosen {
            self.browser_context_menu = None;
            self.run_browser_context_command(commands[chosen], menu.target);
            context.request_repaint();
            return;
        }
        if outcome.escape || (!menu.just_opened && outcome.clicked_elsewhere) {
            self.browser_context_menu = None;
            return;
        }
        menu.just_opened = false;
        self.browser_context_menu = Some(menu);
    }

    fn run_browser_context_command(
        &mut self,
        command: BrowserContextCommand,
        target: BrowserContextTarget,
    ) {
        match command {
            BrowserContextCommand::SetActiveBody => {
                if let BrowserContextTarget::Body(ordinal) = target
                    && let Some(index) = self.bodies.iter().position(|body| body.ordinal == ordinal)
                {
                    self.activate_body(index);
                    self.clear_model_entity_selection();
                }
            }
            BrowserContextCommand::HideSelectedBodies => {
                for index in self.browser_selected_body_indices() {
                    if self.bodies[index].visible {
                        self.set_body_visibility(index, false);
                    }
                }
            }
            BrowserContextCommand::ShowSelectedBodies => {
                for index in self.browser_selected_body_indices() {
                    if !self.bodies[index].visible && self.body_visibility_grantable(index) {
                        self.set_body_visibility(index, true);
                    }
                }
            }
            BrowserContextCommand::IsolateSelectedBodies => {
                let keep = self.browser_selected_body_indices();
                for index in 0..self.bodies.len() {
                    if !keep.contains(&index) && self.bodies[index].visible {
                        self.set_body_visibility(index, false);
                    }
                }
            }
            BrowserContextCommand::UnhideAllBodies => self.show_all_eligible_bodies(),
            BrowserContextCommand::MirrorSelectedBodies => {
                self.stage_preset_feature(SolidFeaturePreset::Mirror);
            }
            BrowserContextCommand::AssignMaterial => {
                if let BrowserContextTarget::Body(ordinal) = target
                    && let Some(index) = self.bodies.iter().position(|body| body.ordinal == ordinal)
                {
                    // The material picker lives in the properties popout and
                    // follows the active body, so the command is: make this
                    // body the one the picker drives, then open the picker.
                    self.activate_body(index);
                    self.document_properties_open = true;
                    self.document_status = Some(format!(
                        "Pick a material in the MATERIAL card to assign it to Body {ordinal}"
                    ));
                }
            }
            BrowserContextCommand::ExportBodyStl | BrowserContextCommand::ExportBodyStep => {
                if let BrowserContextTarget::Body(ordinal) = target {
                    self.export_single_body(
                        ordinal,
                        command == BrowserContextCommand::ExportBodyStep,
                    );
                }
            }
            BrowserContextCommand::EditSketch => {
                if let BrowserContextTarget::Sketch(index) = target {
                    self.edit_committed_sketch(index);
                }
            }
            BrowserContextCommand::SelectSketch => {
                if let BrowserContextTarget::Sketch(index) = target {
                    self.activate_committed_sketch(index);
                }
            }
            BrowserContextCommand::ExportSketchDxf => {
                if let BrowserContextTarget::Sketch(index) = target {
                    self.export_sketch_dxf(index);
                }
            }
            BrowserContextCommand::HideSketch | BrowserContextCommand::ShowSketch => {
                if let BrowserContextTarget::Sketch(index) = target {
                    self.set_sketch_visibility(index, command == BrowserContextCommand::ShowSketch);
                }
            }
            BrowserContextCommand::SelectPlane => match target {
                BrowserContextTarget::ConstructionPlane(id) => {
                    self.selected_construction_plane = Some(id);
                    self.clear_model_entity_selection();
                }
                BrowserContextTarget::OriginPlane(plane) => {
                    let has_other_plane_sketch =
                        !self.sketch.entities().is_empty() && self.sketch.plane() != plane;
                    if !has_other_plane_sketch {
                        self.select_origin_plane(plane);
                    }
                }
                BrowserContextTarget::Body(_) | BrowserContextTarget::Sketch(_) => {}
            },
            BrowserContextCommand::HidePlane | BrowserContextCommand::ShowPlane => {
                if let BrowserContextTarget::ConstructionPlane(id) = target
                    && let Some(plane) = self
                        .construction_planes
                        .iter_mut()
                        .find(|plane| plane.id == id)
                {
                    plane.visible = command == BrowserContextCommand::ShowPlane;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConstructionPlane, ConstructionPlaneSource};
    use artificer_protocol::{
        EntityId, EntityKind, EntityRef, PlanarFrame3, Point3, SnapshotId, Vector3,
    };

    /// A default document plus one standalone cuboid, so body selection has
    /// two all-planar bodies to work with.
    fn app_with_two_bodies() -> KernelLabApp {
        let mut app = KernelLabApp::default();
        crate::push_test_cuboid_body(&mut app, "Second", Point3::new(0.9, 1.1, 1.2));
        assert!(app.bodies.len() >= 2);
        app
    }

    fn selected(app: &KernelLabApp) -> Vec<u32> {
        app.browser_selected_bodies.iter().copied().collect()
    }

    /// A synthetic offset plane parallel to YZ at `x`, active at every
    /// history stop because it belongs to no feature.
    fn offset_yz_plane(app: &KernelLabApp, id: u64, x: f64) -> ConstructionPlane {
        ConstructionPlane {
            id,
            name: format!("Offset {id}"),
            feature: None,
            frame: PlanarFrame3::new(
                Point3::new(x, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
                Vector3::new(0.0, 0.0, 1.0),
            ),
            half_u: 10.0,
            half_v: 10.0,
            visible: true,
            source: ConstructionPlaneSource::OnFace {
                body: app.bodies[0].id,
                face: EntityRef {
                    snapshot: SnapshotId::ZERO,
                    entity: EntityId(1),
                    kind: EntityKind::Face,
                },
            },
        }
    }

    #[test]
    fn body_multi_selection_follows_click_modifiers() {
        let mut app = app_with_two_bodies();
        let first = app.bodies[0].ordinal;
        let second = app.bodies[1].ordinal;

        app.browser_body_row_clicked(0, first, egui::Modifiers::NONE);
        assert_eq!(selected(&app), vec![first]);
        assert_eq!(app.active_body_ordinal, first);

        // Cmd/Ctrl toggles membership without moving the active body.
        app.browser_body_row_clicked(1, second, egui::Modifiers::COMMAND);
        assert_eq!(selected(&app), vec![first, second]);
        assert_eq!(app.active_body_ordinal, first);
        app.browser_body_row_clicked(1, second, egui::Modifiers::COMMAND);
        assert_eq!(selected(&app), vec![first]);

        // Shift selects the run of rows from the active body to the click.
        app.browser_body_row_clicked(1, second, egui::Modifiers::SHIFT);
        assert_eq!(selected(&app), vec![first, second]);
        assert_eq!(app.active_body_ordinal, first);

        // A plain click collapses the selection back to one row.
        app.browser_body_row_clicked(1, second, egui::Modifiers::NONE);
        assert_eq!(selected(&app), vec![second]);
        assert_eq!(app.active_body_ordinal, second);
    }

    #[test]
    fn stale_selection_ordinals_select_nothing() {
        let mut app = app_with_two_bodies();
        app.browser_selected_bodies.insert(9_999);
        let indices = app.browser_selected_body_indices();
        assert!(indices.is_empty());
    }

    #[test]
    fn browser_context_commands_match_target_state() {
        let mut app = app_with_two_bodies();
        let first = app.bodies[0].ordinal;
        let second = app.bodies[1].ordinal;

        // The right-click flow selects the row it lands on first.
        app.browser_body_row_clicked(0, first, egui::Modifiers::NONE);
        assert_eq!(
            app.browser_context_commands(BrowserContextTarget::Body(first)),
            vec![
                BrowserContextCommand::HideSelectedBodies,
                BrowserContextCommand::IsolateSelectedBodies,
                BrowserContextCommand::MirrorSelectedBodies,
                BrowserContextCommand::AssignMaterial,
                BrowserContextCommand::ExportBodyStl,
                BrowserContextCommand::ExportBodyStep,
            ],
        );
        let for_inactive = app.browser_context_commands(BrowserContextTarget::Body(second));
        assert!(for_inactive.contains(&BrowserContextCommand::SetActiveBody));

        // Hiding a body brings the show/unhide entries out.
        app.set_body_visibility(0, false);
        let commands = app.browser_context_commands(BrowserContextTarget::Body(first));
        assert!(commands.contains(&BrowserContextCommand::ShowSelectedBodies));
        assert!(commands.contains(&BrowserContextCommand::UnhideAllBodies));
        assert!(!commands.contains(&BrowserContextCommand::HideSelectedBodies));

        // Origin planes offer selection only while unselected.
        assert_eq!(
            app.browser_context_commands(BrowserContextTarget::OriginPlane(SketchPlane::XY)),
            Vec::new(),
        );
        assert_eq!(
            app.browser_context_commands(BrowserContextTarget::OriginPlane(SketchPlane::YZ)),
            vec![BrowserContextCommand::SelectPlane],
        );

        // Construction planes offer selection and their visibility flip.
        let plane = offset_yz_plane(&app, 7, 5.0);
        app.construction_planes.push(plane);
        assert_eq!(
            app.browser_context_commands(BrowserContextTarget::ConstructionPlane(7)),
            vec![
                BrowserContextCommand::SelectPlane,
                BrowserContextCommand::HidePlane,
            ],
        );
        app.run_browser_context_command(
            BrowserContextCommand::SelectPlane,
            BrowserContextTarget::ConstructionPlane(7),
        );
        assert_eq!(app.selected_construction_plane, Some(7));
        assert_eq!(
            app.browser_context_commands(BrowserContextTarget::ConstructionPlane(7)),
            vec![BrowserContextCommand::HidePlane],
        );
    }

    #[test]
    fn hide_and_isolate_act_on_the_whole_selection() {
        let mut app = app_with_two_bodies();
        let first = app.bodies[0].ordinal;
        let second = app.bodies[1].ordinal;
        app.browser_body_row_clicked(0, first, egui::Modifiers::NONE);
        app.browser_body_row_clicked(1, second, egui::Modifiers::COMMAND);

        app.run_browser_context_command(
            BrowserContextCommand::HideSelectedBodies,
            BrowserContextTarget::Body(first),
        );
        assert!(app.bodies.iter().all(|body| !body.visible));

        app.run_browser_context_command(
            BrowserContextCommand::ShowSelectedBodies,
            BrowserContextTarget::Body(first),
        );
        assert!(app.bodies.iter().all(|body| body.visible));

        // Isolating the first body alone hides only the second.
        app.browser_body_row_clicked(0, first, egui::Modifiers::NONE);
        app.run_browser_context_command(
            BrowserContextCommand::IsolateSelectedBodies,
            BrowserContextTarget::Body(first),
        );
        assert!(app.bodies[0].visible);
        assert!(!app.bodies[1].visible);
    }

    #[test]
    fn mirror_commits_across_the_selected_construction_plane() {
        let mut app = KernelLabApp::default();
        let plane = offset_yz_plane(&app, 11, 5.0);
        app.construction_planes.push(plane);
        app.run_browser_context_command(
            BrowserContextCommand::SelectPlane,
            BrowserContextTarget::ConstructionPlane(11),
        );
        let before = app.displayed_measures().unwrap().centroid.unwrap().x;
        app.stage_preset_feature(SolidFeaturePreset::Mirror);
        assert!(app.confirm_pending_operation());
        let after = app.displayed_measures().unwrap().centroid.unwrap().x;
        // Reflection across x = 5 sends x to 10 - x.
        assert!(
            (after - (10.0 - before)).abs() < 1e-6,
            "expected {} to reflect to {}, got {after}",
            before,
            10.0 - before,
        );
    }

    #[test]
    fn mirror_applies_to_every_selected_body() {
        let mut app = app_with_two_bodies();
        let first = app.bodies[0].ordinal;
        let second = app.bodies[1].ordinal;
        let plane = offset_yz_plane(&app, 21, 5.0);
        app.construction_planes.push(plane);
        app.run_browser_context_command(
            BrowserContextCommand::SelectPlane,
            BrowserContextTarget::ConstructionPlane(21),
        );
        app.browser_body_row_clicked(0, first, egui::Modifiers::NONE);
        app.browser_body_row_clicked(1, second, egui::Modifiers::COMMAND);
        let centroids_before = app
            .bodies
            .iter()
            .map(|body| body.body.snapshot.measures().centroid.unwrap().x)
            .collect::<Vec<_>>();

        app.stage_preset_feature(SolidFeaturePreset::Mirror);
        assert!(app.confirm_pending_operation());
        assert!(app.pending_operation.is_none());

        for (body, before) in app.bodies.iter().zip(centroids_before) {
            let after = body.body.snapshot.measures().centroid.unwrap().x;
            assert!(
                (after - (10.0 - before)).abs() < 1e-6,
                "body {} expected {} to reflect to {}, got {after}",
                body.ordinal,
                before,
                10.0 - before,
            );
        }
    }
}
