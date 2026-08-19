//! The workbench command ribbon.
//!
//! A renderer over [`crate::commands::COMMANDS`], not a layout written by hand.
//! A tab strip picks one taxonomy branch, each tab draws its captioned groups,
//! and each group draws its commands at one of two weights. Nothing here knows
//! what any individual command is called or when it applies: the table says how
//! a command is presented, `command_availability` says whether it can run now,
//! and `run_command` performs it.
//!
//! Every command here only stages intents or immediate presentation changes;
//! kernel execution stays behind the shared confirmation dispatcher in the
//! crate root.

use std::borrow::Cow;

use egui::{FontId, RichText, Sense, Stroke, Vec2, vec2};

use artificer_protocol::BooleanOperation;

use crate::command_icons::{CommandIcon, paint_command_icon};
use crate::commands::{
    CommandDescriptor, CommandSize, ModelCommand, RibbonGroupId, RibbonTab, groups_for_tab,
};
use crate::presentation::ActiveTool;
use crate::sketch_toolbar::{
    SketchOperationGate, SketchToolCapabilities, ToolVariant, render_sketch_toolbar,
};
use crate::theme::{self, ribbon_group};
use crate::{KernelLabApp, SolidFeaturePreset, WorkbenchMode, shell_button_activated, viewport};

/// Whether a command can run right now, and in plain words why not when it
/// cannot. A disabled control that cannot say why is a dead end.
pub(crate) enum CommandAvailability {
    Enabled,
    Disabled(Cow<'static, str>),
}

impl CommandAvailability {
    const fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled)
    }

    fn disabled(reason: impl Into<Cow<'static, str>>) -> Self {
        Self::Disabled(reason.into())
    }
}

const LARGE_ICON: f32 = 26.0;
const LARGE_BUTTON: Vec2 = vec2(62.0, 54.0);
const SMALL_ICON: f32 = 16.0;
// 24 px is the smallest hit target the workbench allows itself; the
// minimum-window guard in `tests/ui.rs` holds every ribbon button to it.
// The width is the widest small caption — `Properties`, 49 px at 10.5 pt —
// after the 24 px icon column, with 2 px to spare. At 86 the sketch tab did
// not fit the 1040 px minimum window: its last button ended at 1081.
const SMALL_BUTTON: Vec2 = vec2(78.0, 24.0);

impl KernelLabApp {
    pub(crate) fn command_ribbon(&mut self, ui: &mut egui::Ui) {
        let operation_pending = self.operation_confirmation_pending();
        if !self.shell.visibility().command_ribbon {
            ui.horizontal_centered(|ui| {
                let response = ui.add_sized([24.0, 22.0], egui::Button::new("+").frame(false));
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        "Expand command ribbon",
                    )
                });
                if shell_button_activated(ui, &response, operation_pending) {
                    self.shell.set_command_ribbon(true);
                }
                ui.label(
                    RichText::new(format!("{} workspace", self.workbench_mode.label()))
                        .color(theme::muted()),
                );
                if let Some(pending) = self.pending_operation {
                    ui.label(RichText::new(pending.title()).color(theme::warn()));
                }
            });
            return;
        }

        // The tab strip lives in the header, not here: two rows of tabs stacked
        // one above the other — workspace above, ribbon below — was the jarring
        // part, and they were always the same choice said twice.
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
            // Groups already end in a separator and carry their own caption
            // row; the panel's 5 px item spacing on both sides of every
            // separator was 19 px of air per boundary, the single largest
            // consumer of width at the 1040 px minimum window.
            ui.spacing_mut().item_spacing.x = 2.0;
            let response = ui
                .add_sized([24.0, 22.0], egui::Button::new("−").frame(false))
                .on_hover_text("Collapse command ribbon");
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Collapse command ribbon")
            });
            if shell_button_activated(ui, &response, operation_pending) {
                self.shell.set_command_ribbon(false);
            }
            self.ribbon_groups(ui);
        });
    }

    /// The tab strip, drawn in the header beside the document name.
    ///
    /// Model and Sketch are the workspace: picking one enters it, which is what
    /// the separate "Model mode / Sketch mode" pair used to do a row above. They
    /// were always the same choice said twice, so there is now one control.
    /// View is a ribbon tab only — it changes what is shown, never the
    /// workspace — and is therefore reachable while an operation is pending.
    pub(crate) fn ribbon_tab_strip(&mut self, ui: &mut egui::Ui) {
        let active = self.active_ribbon_tab();
        let operation_pending = self.pending_operation.is_some();
        ui.spacing_mut().item_spacing.x = 1.0;
        for tab in RibbonTab::ALL {
            let selected = tab == active;
            let switches_workspace = tab != RibbonTab::View;
            let enabled = !switches_workspace || !operation_pending;
            let response = ui.add_enabled(
                enabled,
                egui::Button::new(
                    RichText::new(tab.label())
                        .font(FontId::proportional(12.0))
                        .color(if selected {
                            theme::text()
                        } else {
                            theme::muted()
                        }),
                )
                .frame(false)
                .corner_radius(2)
                .min_size(vec2(62.0, 26.0)),
            );
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Button,
                    enabled,
                    selected,
                    tab.accessible_name(),
                )
            });
            if selected {
                ui.painter().line_segment(
                    [
                        egui::pos2(response.rect.left() + 6.0, response.rect.bottom() - 1.0),
                        egui::pos2(response.rect.right() - 6.0, response.rect.bottom() - 1.0),
                    ],
                    Stroke::new(2.0, theme::accent()),
                );
            }
            if !enabled {
                response.on_disabled_hover_text(
                    "Confirm or cancel the pending operation before changing workspaces.",
                );
            } else if response.clicked() {
                match tab {
                    RibbonTab::Model => {
                        self.ribbon_tab = None;
                        self.enter_model_mode();
                    }
                    RibbonTab::Sketch => {
                        self.ribbon_tab = None;
                        self.enter_sketch_mode();
                    }
                    RibbonTab::View => self.ribbon_tab = Some((self.workbench_mode, tab)),
                }
            }
        }
    }

    /// The tab whose commands are showing. Following the workspace by default
    /// is what makes the Sketch tab appear the moment a sketch opens, the way a
    /// contextual tab does elsewhere; an explicit pick overrides it until the
    /// workspace changes again.
    fn active_ribbon_tab(&self) -> RibbonTab {
        let workspace_tab = match self.workbench_mode {
            WorkbenchMode::Model => RibbonTab::Model,
            WorkbenchMode::Sketch => RibbonTab::Sketch,
        };
        self.ribbon_tab
            .filter(|(mode, _)| *mode == self.workbench_mode)
            .map_or(workspace_tab, |(_, tab)| tab)
    }

    fn ribbon_groups(&mut self, ui: &mut egui::Ui) {
        let tab = self.active_ribbon_tab();
        // The drawing tools are the sketch crate's own registry-driven toolbar,
        // rendered whole. It leads the Sketch tab because it is what the tab is
        // for; every other group here comes from this crate's table.
        if tab == RibbonTab::Sketch {
            let group = RibbonGroupId::SketchTools;
            ribbon_group(ui, group.caption(), group.stable_key(), |ui| {
                self.sketch_tool_grid(ui);
            });
        }
        for (group, members) in groups_for_tab(tab) {
            ribbon_group(ui, group.caption(), group.stable_key(), |ui| {
                self.ribbon_group_commands(ui, group, &members);
                if group == RibbonGroupId::Boolean {
                    self.boolean_operand_panel(ui);
                }
            });
        }
    }

    fn ribbon_group_commands(
        &mut self,
        ui: &mut egui::Ui,
        group: RibbonGroupId,
        members: &[&'static CommandDescriptor],
    ) {
        let large = members
            .iter()
            .filter(|descriptor| descriptor.size == CommandSize::Large);
        for descriptor in large {
            self.large_command_button(ui, descriptor, group);
        }
        let small = members
            .iter()
            .filter(|descriptor| descriptor.size == CommandSize::Small)
            .collect::<Vec<_>>();
        // Small commands stack three to a column, so a six-command group reads
        // as one block rather than a long unbroken strip.
        for column in small.chunks(3) {
            ui.vertical(|ui| {
                for descriptor in column {
                    self.small_command_button(ui, descriptor);
                }
            });
        }
    }

    fn large_command_button(
        &mut self,
        ui: &mut egui::Ui,
        descriptor: &'static CommandDescriptor,
        group: RibbonGroupId,
    ) {
        let availability = self.command_availability(descriptor.command);
        let enabled = availability.is_enabled();
        let active = self.command_is_active(descriptor.command);
        // The groups that publish geometry carry the ribbon's only emphasis, so
        // "what do I press to make a solid" is answerable at a glance.
        let primary = matches!(
            group,
            RibbonGroupId::Solid | RibbonGroupId::SketchSolid | RibbonGroupId::Complete
        );
        let (response, painter) = ui.allocate_painter(
            LARGE_BUTTON,
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let rect = response.rect;
        let hovered = response.hovered() && enabled;
        let fill = if !enabled {
            egui::Color32::TRANSPARENT
        } else if active {
            theme::selected_fill()
        } else if hovered {
            theme::card()
        } else if primary {
            theme::card().gamma_multiply(0.7)
        } else {
            egui::Color32::TRANSPARENT
        };
        let outline = if active {
            theme::accent()
        } else if hovered || (primary && enabled) {
            theme::border()
        } else {
            egui::Color32::TRANSPARENT
        };
        painter.rect(
            rect,
            4.0,
            fill,
            Stroke::new(1.0, outline),
            egui::StrokeKind::Inside,
        );
        let tint = if enabled {
            theme::text()
        } else {
            theme::muted().gamma_multiply(0.6)
        };
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(rect.center().x, rect.top() + 6.0 + LARGE_ICON / 2.0),
            Vec2::splat(LARGE_ICON),
        );
        paint_command_icon(
            &painter,
            icon_rect,
            self.command_icon(descriptor),
            if enabled {
                theme::accent()
            } else {
                theme::muted().gamma_multiply(0.6)
            },
        );
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 5.0),
            egui::Align2::CENTER_BOTTOM,
            self.command_label(descriptor),
            FontId::proportional(10.5),
            tint,
        );
        self.finish_command_button(ui, response, descriptor, availability);
    }

    fn small_command_button(&mut self, ui: &mut egui::Ui, descriptor: &'static CommandDescriptor) {
        let availability = self.command_availability(descriptor.command);
        let enabled = availability.is_enabled();
        let active = self.command_is_active(descriptor.command);
        let (response, painter) = ui.allocate_painter(
            SMALL_BUTTON,
            if enabled {
                Sense::click()
            } else {
                Sense::hover()
            },
        );
        let rect = response.rect;
        let hovered = response.hovered() && enabled;
        let fill = if !enabled {
            egui::Color32::TRANSPARENT
        } else if active {
            theme::selected_fill()
        } else if hovered {
            theme::card()
        } else {
            egui::Color32::TRANSPARENT
        };
        painter.rect(
            rect,
            3.0,
            fill,
            Stroke::new(
                1.0,
                if active {
                    theme::accent()
                } else {
                    egui::Color32::TRANSPARENT
                },
            ),
            egui::StrokeKind::Inside,
        );
        let icon_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + 3.0 + SMALL_ICON / 2.0, rect.center().y),
            Vec2::splat(SMALL_ICON),
        );
        paint_command_icon(
            &painter,
            icon_rect,
            self.command_icon(descriptor),
            if enabled {
                theme::accent()
            } else {
                theme::muted().gamma_multiply(0.6)
            },
        );
        painter.text(
            egui::pos2(icon_rect.right() + 5.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            self.command_label(descriptor),
            FontId::proportional(10.5),
            if enabled {
                theme::text()
            } else {
                theme::muted().gamma_multiply(0.6)
            },
        );
        self.finish_command_button(ui, response, descriptor, availability);
    }

    /// Accessibility, tooltip and activation, identical for both button sizes.
    fn finish_command_button(
        &mut self,
        ui: &egui::Ui,
        response: egui::Response,
        descriptor: &'static CommandDescriptor,
        availability: CommandAvailability,
    ) {
        let enabled = availability.is_enabled();
        let name = self.command_accessible_name(descriptor);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, name));
        let shortcut = descriptor.shortcut;
        let tooltip = descriptor.tooltip;
        let response = match availability {
            CommandAvailability::Enabled => response.on_hover_ui(|ui| {
                ui.label(RichText::new(name).strong());
                ui.label(RichText::new(tooltip).small().color(theme::muted()));
                if let Some(shortcut) = shortcut {
                    ui.label(
                        RichText::new(format!("Keyboard: {shortcut}"))
                            .small()
                            .color(theme::muted()),
                    );
                }
            }),
            CommandAvailability::Disabled(reason) => response.on_hover_ui(|ui| {
                ui.label(RichText::new(name).strong());
                ui.label(RichText::new(tooltip).small().color(theme::muted()));
                ui.label(RichText::new(reason.as_ref()).small().color(theme::warn()));
            }),
        };
        if response.clicked() {
            self.run_command(descriptor.command, ui.ctx());
        }
    }

    fn sketch_tool_grid(&mut self, ui: &mut egui::Ui) {
        let capabilities = SketchToolCapabilities::default();
        let gate = if self.pending_operation.is_some() || self.sketch.has_pending_edit() {
            SketchOperationGate::AwaitingConfirmation
        } else {
            SketchOperationGate::Ready
        };
        let output = render_sketch_toolbar(
            ui,
            &mut self.sketch_toolbar,
            self.active_sketch_tool,
            gate,
            &capabilities,
        );
        if let Some(variant) = output.chosen {
            self.activate_sketch_tool_variant(variant);
        }
    }

    /// While a Boolean is staged the group becomes its operand panel: the picks
    /// are the operation's real input and belong on screen, not only in the
    /// status line.
    fn boolean_operand_panel(&mut self, ui: &mut egui::Ui) {
        let Some(crate::PendingOperation::BooleanBodies { keep_tools, .. }) =
            self.pending_operation
        else {
            return;
        };
        ui.vertical(|ui| {
            let mut keep = keep_tools;
            if ui
                .checkbox(&mut keep, "Keep tools")
                .on_hover_text(
                    "Leave every tool body in the workspace after the Boolean instead of consuming it.",
                )
                .changed()
            {
                self.set_boolean_keep_tools(keep);
            }
            ui.label(
                RichText::new(self.boolean_operand_summary())
                    .small()
                    .color(theme::muted()),
            )
            .on_hover_text("Click a body to add it as a tool; click it again to remove it.");
        });
    }

    pub(crate) fn activate_sketch_tool_variant(&mut self, variant: ToolVariant) {
        if self.sketch.set_exact_tool(variant) {
            self.active_sketch_tool = variant;
        }
    }

    // ---- per-command presentation ------------------------------------------

    fn command_icon(&self, descriptor: &CommandDescriptor) -> CommandIcon {
        match descriptor.command {
            ModelCommand::PlayMotion if self.motion.playing => CommandIcon::Stop,
            _ => descriptor.icon,
        }
    }

    /// The visible label. Only the two commands whose meaning genuinely changes
    /// with state override the table.
    fn command_label(&self, descriptor: &CommandDescriptor) -> &'static str {
        match descriptor.command {
            ModelCommand::NewSketch => match self.sketch_entry_action() {
                SketchEntryAction::OnSelectedFace => "On face",
                SketchEntryAction::New => "New sketch",
                SketchEntryAction::Create => "Sketch",
                SketchEntryAction::Edit => "Edit sketch",
            },
            ModelCommand::PlayMotion if self.motion.playing => "Stop",
            ModelCommand::ToggleTheme => theme::active_theme().other().label(),
            _ => descriptor.label,
        }
    }

    /// The accessible name, which is what the UI tests and assistive technology
    /// use. It stays the long, unambiguous form even where the visible label is
    /// abbreviated to fit a ribbon button.
    fn command_accessible_name(&self, descriptor: &CommandDescriptor) -> &'static str {
        match descriptor.command {
            ModelCommand::NewSketch => match self.sketch_entry_action() {
                SketchEntryAction::OnSelectedFace => "Sketch on selected face",
                SketchEntryAction::New => "New sketch",
                SketchEntryAction::Create => "Create sketch",
                SketchEntryAction::Edit => "Edit sketch",
            },
            ModelCommand::PlayMotion if self.motion.playing => "Stop motion",
            _ => descriptor.accessible_name,
        }
    }

    fn command_is_active(&self, command: ModelCommand) -> bool {
        match command {
            ModelCommand::Move => self.active_tool == ActiveTool::Move,
            ModelCommand::Rotate => self.active_tool == ActiveTool::Rotate,
            ModelCommand::Scale => self.active_tool == ActiveTool::Scale,
            ModelCommand::Select => self.active_tool == ActiveTool::Select,
            ModelCommand::Measure => self.active_tool == ActiveTool::Measure,
            ModelCommand::Orbit => self.active_tool == ActiveTool::Orbit,
            ModelCommand::ToggleEdges => self.edge_overlay && !self.model_display_mode.is_shaded(),
            ModelCommand::ToggleShaded => self.model_display_mode.is_shaded(),
            ModelCommand::ToggleSnap => self.sketch.snap_settings().enabled,
            ModelCommand::ShowBrowser => self.shell.visibility().model_browser,
            // The properties palette has no hidden state of its own; the
            // command raises and focuses it, so it is never "on".
            ModelCommand::ShowHistory => self.shell.visibility().feature_timeline,
            ModelCommand::ToggleOriginPlanes => self.show_origin_planes,
            ModelCommand::ToggleTheme => theme::active_theme() == theme::WorkbenchTheme::Dark,
            ModelCommand::PlayMotion => self.motion.playing,
            _ => false,
        }
    }

    /// Which sketch the Create group would open, which decides both its name
    /// and whether it is available at all.
    fn sketch_entry_action(&self) -> SketchEntryAction {
        if self.selected_face.is_some() {
            return SketchEntryAction::OnSelectedFace;
        }
        let starts_new_origin_sketch = !self.sketch.entities().is_empty()
            && (self.sketch_finished
                || self.extruded_sketch_revision == Some(self.sketch_revision)
                || !self.sketch_support_is_current());
        if starts_new_origin_sketch {
            SketchEntryAction::New
        } else if self.sketch.entities().is_empty() {
            SketchEntryAction::Create
        } else {
            SketchEntryAction::Edit
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SketchEntryAction {
    OnSelectedFace,
    New,
    Create,
    Edit,
}

impl KernelLabApp {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn command_availability(&self, command: ModelCommand) -> CommandAvailability {
        // Two conditions gate almost everything that changes the model, so they
        // are answered once, in the same words, for every command.
        let free = |app: &Self| -> Option<CommandAvailability> {
            if app.pending_operation.is_some() {
                return Some(CommandAvailability::disabled(
                    "Confirm or cancel the pending operation first.",
                ));
            }
            if !app.history_is_at_end() {
                return Some(CommandAvailability::disabled(
                    "Move the history marker to the end before creating another feature.",
                ));
            }
            None
        };
        match command {
            ModelCommand::NewSketch => {
                if let Some(blocked) = free(self) {
                    return blocked;
                }
                if self.selected_face.is_some() && self.active_component_instance().is_some() {
                    return CommandAvailability::disabled(
                        "Library components are immutable occurrences. Edit the source part or create an independent workspace sketch.",
                    );
                }
                let action = self.sketch_entry_action();
                if !self.sketch_support_is_current()
                    && action != SketchEntryAction::OnSelectedFace
                    && action != SketchEntryAction::New
                {
                    return CommandAvailability::disabled(
                        "The prior face sketch is read-only after the body changed. Select a current face to start the next sketch.",
                    );
                }
                CommandAvailability::Enabled
            }
            ModelCommand::ConstructionPlane => {
                if let Some(blocked) = free(self) {
                    return blocked;
                }
                if (1..=2).contains(&self.selected_faces.len()) {
                    CommandAvailability::Enabled
                } else {
                    CommandAvailability::disabled(
                        "Select one planar face for a coincident plane, or two parallel faces for a midplane.",
                    )
                }
            }
            ModelCommand::Extrude => self.extrude_availability(),
            ModelCommand::Revolve => self.preset_feature_availability(SolidFeaturePreset::Revolve),
            ModelCommand::Hole => self.preset_feature_availability(SolidFeaturePreset::Hole),
            ModelCommand::Rib => self.preset_feature_availability(SolidFeaturePreset::Rib),
            ModelCommand::Mirror => self.preset_feature_availability(SolidFeaturePreset::Mirror),
            ModelCommand::Pattern => {
                self.preset_feature_availability(SolidFeaturePreset::LinearPattern)
            }
            ModelCommand::Chamfer => self.preset_feature_availability(SolidFeaturePreset::Chamfer),
            ModelCommand::Fillet => self.preset_feature_availability(SolidFeaturePreset::Fillet),
            ModelCommand::Combine | ModelCommand::Subtract | ModelCommand::Intersect => {
                if let Some(blocked) = free(self) {
                    return blocked;
                }
                if self.active_component_instance().is_some() {
                    return CommandAvailability::disabled(
                        "Library component geometry is immutable in this workspace.",
                    );
                }
                let target = self.active_body_id();
                let has_tool = target.is_some_and(|target| {
                    self.bodies
                        .iter()
                        .any(|body| body.id != target && body.visible)
                });
                if has_tool {
                    CommandAvailability::Enabled
                } else {
                    CommandAvailability::disabled(
                        "A Boolean needs a second visible body to use as a tool.",
                    )
                }
            }
            ModelCommand::Move | ModelCommand::Rotate => {
                if let Some(name) = self
                    .active_component_instance()
                    .and_then(|component| self.document.joint_for_child(component.id))
                    .map(|joint| joint.name.clone())
                {
                    return CommandAvailability::disabled(format!(
                        "This component is constrained by {name}; joint-coordinate editing comes next."
                    ));
                }
                if self.transform_tools_available() {
                    CommandAvailability::Enabled
                } else {
                    CommandAvailability::disabled("Select a body to transform.")
                }
            }
            ModelCommand::Scale => {
                if self.active_component_instance().is_some() {
                    return CommandAvailability::disabled(
                        "Component occurrences preserve exact authored size; change a part parameter instead.",
                    );
                }
                if self.scale_tool_available() {
                    CommandAvailability::Enabled
                } else {
                    CommandAvailability::disabled("Select a body to scale.")
                }
            }
            ModelCommand::ExitSketch => {
                if self.pending_operation.is_some() {
                    CommandAvailability::disabled("Confirm or cancel the pending operation first.")
                } else {
                    CommandAvailability::Enabled
                }
            }
            ModelCommand::FinishSketch => {
                if self.pending_operation.is_some() {
                    return CommandAvailability::disabled(
                        "Confirm or cancel the pending operation first.",
                    );
                }
                if self.sketch_creation_draft_active() {
                    return CommandAvailability::disabled(
                        "Complete or cancel the stroke in progress first.",
                    );
                }
                if self.sketch.authoring().operations().is_empty() {
                    return CommandAvailability::disabled("Draw something before finishing.");
                }
                CommandAvailability::Enabled
            }
            ModelCommand::Select
            | ModelCommand::Measure
            | ModelCommand::Orbit
            | ModelCommand::FrameVisible
            | ModelCommand::Home
            | ModelCommand::PlayMotion
            | ModelCommand::FrameSketch
            | ModelCommand::ToggleSnap => CommandAvailability::Enabled,
            ModelCommand::ToggleEdges => {
                if self.model_display_mode.is_shaded() {
                    CommandAvailability::disabled(
                        "Visible grey edges are always enabled in Shaded mode.",
                    )
                } else {
                    CommandAvailability::Enabled
                }
            }
            // There is no sketch properties palette to show. A control that
            // looks live and does nothing is worse than one that says why.
            ModelCommand::ShowProperties => {
                if self.workbench_mode == WorkbenchMode::Sketch {
                    CommandAvailability::disabled(
                        "The sketch workspace has no properties palette. Dimensions are on the canvas, and the dimension tool edits committed ones.",
                    )
                } else {
                    CommandAvailability::Enabled
                }
            }
            ModelCommand::ToggleShaded
            | ModelCommand::ShowBrowser
            | ModelCommand::ShowHistory
            | ModelCommand::ToggleOriginPlanes
            | ModelCommand::ToggleTheme => CommandAvailability::Enabled,
        }
    }

    fn preset_feature_availability(&self, preset: SolidFeaturePreset) -> CommandAvailability {
        if self.pending_operation.is_some() {
            return CommandAvailability::disabled("Confirm or cancel the pending operation first.");
        }
        if !self.history_is_at_end() {
            return CommandAvailability::disabled(
                "Move the history marker to the end before creating another feature.",
            );
        }
        let ready = match preset {
            SolidFeaturePreset::Revolve => true,
            SolidFeaturePreset::Hole | SolidFeaturePreset::Rib => self.selected_face.is_some(),
            SolidFeaturePreset::Mirror | SolidFeaturePreset::LinearPattern => {
                self.active_body_id().is_some()
            }
            SolidFeaturePreset::Chamfer | SolidFeaturePreset::Fillet => {
                !self.selected_edges.is_empty()
            }
        };
        if ready {
            CommandAvailability::Enabled
        } else {
            CommandAvailability::disabled(match preset {
                SolidFeaturePreset::Hole | SolidFeaturePreset::Rib => "Select a planar face first.",
                SolidFeaturePreset::Mirror | SolidFeaturePreset::LinearPattern => {
                    "Activate a body first."
                }
                _ => "Select at least one edge first.",
            })
        }
    }

    /// Extrude is the one command that serves two operations — extruding the
    /// active sketch and pushing the selected face — so its reasons stay
    /// enumerated in the order the user is most likely to have hit them.
    fn extrude_availability(&self) -> CommandAvailability {
        let linked_sketch_support = self
            .sketch_support
            .body()
            .is_some_and(|body| self.component_for_body(body).is_some());
        let linked_active_body = self.active_component_instance().is_some();
        let eligibility = self.sketch_extrusion_eligibility();
        let already_extruded = self.extruded_sketch_revision == Some(self.sketch_revision);
        let distance_valid = self.extrusion_distance_is_valid();
        let sketch_edit_complete =
            !self.sketch.has_pending_edit() && !self.sketch_creation_draft_active();
        // With bounded profiles drawn but none picked — or a pick that cannot
        // become a solid — Extrude is still the right button to press: it
        // hands the canvas to Select and says where to click, instead of
        // greying out behind a tooltip.
        let awaiting_profile_pick =
            self.workbench_mode == WorkbenchMode::Sketch && eligibility.wants_profile_pick();
        let sketch_enabled = self.pending_operation.is_none()
            && self.history_is_at_end()
            && !already_extruded
            && distance_valid
            && sketch_edit_complete
            && !linked_sketch_support
            && (eligibility.can_stage() || awaiting_profile_pick);
        let active_sketch_consumed = self
            .active_sketch_index
            .and_then(|index| self.sketches.get(index))
            .is_none_or(|sketch| sketch.consumed);
        let push_pull_support = self.selected_face_push_pull_support();
        let push_pull_enabled = self.pending_operation.is_none()
            && self.history_is_at_end()
            && self.workbench_mode == WorkbenchMode::Model
            && distance_valid
            && active_sketch_consumed
            && !linked_active_body
            && push_pull_support.is_some();
        if sketch_enabled || push_pull_enabled {
            return CommandAvailability::Enabled;
        }
        CommandAvailability::disabled(if self.pending_operation.is_some() {
            "Confirm or cancel the pending operation first.".to_owned()
        } else if !self.history_is_at_end() {
            "Move the history marker to the end before creating another feature.".to_owned()
        } else if !distance_valid {
            "Enter a finite, non-zero extrusion distance.".to_owned()
        } else if linked_sketch_support || (self.selected_face.is_some() && linked_active_body) {
            "Library component geometry is immutable in this workspace; edit its source definition or place another component.".to_owned()
        } else if self.selected_face.is_some() && push_pull_support.is_none() {
            "Direct push/pull requires one unholed planar extrusion cap.".to_owned()
        } else if self.selected_face.is_some() && !active_sketch_consumed {
            "Finish or consume the active sketch before pushing the selected face.".to_owned()
        } else if already_extruded {
            "Select an eligible face to push/pull, or create another sketch.".to_owned()
        } else if !sketch_edit_complete {
            "Complete or cancel the active sketch edit first.".to_owned()
        } else {
            eligibility.visible_reason().unwrap_or_else(|| {
                "Create an eligible closed sketch before starting extrusion.".to_owned()
            })
        })
    }

    fn run_command(&mut self, command: ModelCommand, context: &egui::Context) {
        match command {
            ModelCommand::NewSketch => {
                if self.sketch_entry_action() == SketchEntryAction::New {
                    self.begin_new_origin_sketch();
                } else {
                    self.enter_sketch_mode();
                }
            }
            ModelCommand::ConstructionPlane => self.stage_construction_plane(),
            ModelCommand::Extrude => {
                let eligibility = self.sketch_extrusion_eligibility();
                if self.workbench_mode == WorkbenchMode::Sketch && eligibility.wants_profile_pick()
                {
                    self.begin_profile_pick_for_extrusion(eligibility);
                    return;
                }
                let staged = if eligibility.can_stage()
                    && self.extruded_sketch_revision != Some(self.sketch_revision)
                {
                    self.stage_sketch_extrusion()
                } else {
                    self.stage_face_push_pull()
                };
                // Staging no longer docks a panel: the contextual card carries
                // the operation's controls, over the viewport, beside the rail
                // that will commit it.
                let _ = staged;
            }
            ModelCommand::Revolve => self.stage_preset_feature(SolidFeaturePreset::Revolve),
            ModelCommand::Hole => self.stage_preset_feature(SolidFeaturePreset::Hole),
            ModelCommand::Rib => self.stage_preset_feature(SolidFeaturePreset::Rib),
            ModelCommand::Mirror => self.stage_preset_feature(SolidFeaturePreset::Mirror),
            ModelCommand::Pattern => self.stage_preset_feature(SolidFeaturePreset::LinearPattern),
            ModelCommand::Chamfer => self.stage_preset_feature(SolidFeaturePreset::Chamfer),
            ModelCommand::Fillet => self.stage_preset_feature(SolidFeaturePreset::Fillet),
            ModelCommand::Combine => self.stage_body_boolean(BooleanOperation::Union),
            ModelCommand::Subtract => self.stage_body_boolean(BooleanOperation::Difference),
            ModelCommand::Intersect => self.stage_body_boolean(BooleanOperation::Intersection),
            ModelCommand::Move => self.active_tool = ActiveTool::Move,
            ModelCommand::Rotate => self.active_tool = ActiveTool::Rotate,
            ModelCommand::Scale => self.active_tool = ActiveTool::Scale,
            ModelCommand::Select => self.active_tool = ActiveTool::Select,
            ModelCommand::Measure => self.active_tool = ActiveTool::Measure,
            ModelCommand::Orbit => self.active_tool = ActiveTool::Orbit,
            ModelCommand::FrameVisible => self.frame_visible_body(context),
            ModelCommand::Home => self.reset_view(context),
            ModelCommand::ToggleEdges => self.edge_overlay = !self.edge_overlay,
            ModelCommand::ToggleShaded => {
                self.model_display_mode = if self.model_display_mode.is_shaded() {
                    viewport::ModelDisplayMode::Diagnostic
                } else {
                    viewport::ModelDisplayMode::ShadedEdges
                };
            }
            ModelCommand::PlayMotion => self.toggle_animation(context),
            ModelCommand::ShowBrowser => self.shell.set_model_browser(true),
            ModelCommand::ShowProperties => self.show_properties_tab(),
            ModelCommand::ShowHistory => self.shell.set_feature_timeline(true),
            ModelCommand::ToggleTheme => {
                theme::set_active_theme(theme::active_theme().other());
                // egui derives its own widget defaults from the palette, so the
                // style has to be rebuilt before anything else paints.
                theme::install_style(context);
                context.request_repaint();
            }
            ModelCommand::FinishSketch => {
                self.finish_sketch_now();
            }
            ModelCommand::ExitSketch => {
                self.enter_model_mode();
            }
            ModelCommand::ToggleOriginPlanes => {
                self.show_origin_planes = !self.show_origin_planes;
            }
            ModelCommand::FrameSketch => self.frame_active_sketch(),
            ModelCommand::ToggleSnap => {
                let mut settings = self.sketch.snap_settings();
                settings.enabled = !settings.enabled;
                self.sketch.set_snap_settings(settings);
            }
        }
    }
}
