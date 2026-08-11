//! The workbench command ribbon.
//!
//! One captioned command surface across both workspaces: the Model ribbon is a
//! single row of Office-style captioned groups, and the Sketch ribbon hosts
//! the compact tool grid beside its completion and view groups. Every command
//! here only stages intents or immediate presentation changes; kernel
//! execution stays behind the shared confirmation dispatcher in the crate
//! root.

use egui::{RichText, Stroke};

use artificer_protocol::BooleanOperation;

use crate::presentation::ActiveTool;
use crate::sketch_toolbar::{
    SketchOperationGate, SketchToolCapabilities, ToolVariant, render_sketch_toolbar,
};
use crate::theme::{ACCENT, BORDER, CARD, MUTED, SELECTED_FILL, TEXT, WARN, ribbon_group};
use crate::{KernelLabApp, SolidFeaturePreset, WorkbenchMode, shell_button_activated, viewport};

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
                        .color(MUTED),
                );
                if let Some(pending) = self.pending_operation {
                    ui.label(RichText::new(pending.title()).color(WARN));
                }
            });
            return;
        }

        match self.workbench_mode {
            // The single-row model ribbon keeps its established vertically
            // centred placement and therefore does not churn unrelated model
            // snapshots when the taller Sketch toolbar changes.
            WorkbenchMode::Model => {
                ui.horizontal_centered(|ui| {
                    self.expanded_command_ribbon_contents(ui, operation_pending);
                });
            }
            WorkbenchMode::Sketch => {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                    // Establish the tall sketch ribbon row before the small
                    // collapse control participates in horizontal layout.
                    // Otherwise egui can clamp the icon grid to the 28 px
                    // collapse button and let row two cross the panel edge.
                    ui.set_min_height(88.0);
                    self.expanded_command_ribbon_contents(ui, operation_pending);
                });
            }
        }
    }

    fn expanded_command_ribbon_contents(&mut self, ui: &mut egui::Ui, operation_pending: bool) {
        let response = ui
            .add_sized([24.0, 28.0], egui::Button::new("−").frame(false))
            .on_hover_text("Collapse command ribbon");
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Collapse command ribbon")
        });
        if shell_button_activated(ui, &response, operation_pending) {
            self.shell.set_command_ribbon(false);
        }
        match self.workbench_mode {
            WorkbenchMode::Model => self.model_command_groups(ui),
            WorkbenchMode::Sketch => self.sketch_command_groups(ui),
        }
    }

    fn model_command_groups(&mut self, ui: &mut egui::Ui) {
        let transform_tools_available = self.transform_tools_available();
        ribbon_group(ui, "CREATE", "create_commands", |ui| {
            let selected_linked_component =
                self.selected_face.is_some() && self.active_component_instance().is_some();
            let support_current = self.sketch_support_is_current();
            let starts_new_origin_sketch = self.selected_face.is_none()
                && !self.sketch.entities().is_empty()
                && (self.sketch_finished
                    || self.extruded_sketch_revision == Some(self.sketch_revision)
                    || !support_current);
            let action = if self.selected_face.is_some() {
                "Sketch on selected face"
            } else if starts_new_origin_sketch {
                "New sketch"
            } else if self.sketch.entities().is_empty() {
                "Create sketch"
            } else {
                "Edit sketch"
            };
            let enabled = self.pending_operation.is_none()
                && self.history_is_at_end()
                && !selected_linked_component
                && (support_current || self.selected_face.is_some() || starts_new_origin_sketch);
            let response = ui.add_enabled(enabled, egui::Button::new(action));
            let response = if selected_linked_component {
                response.on_disabled_hover_text(
                    "Library components are immutable occurrences. Edit the source part or create an independent workspace sketch.",
                )
            } else if !support_current && self.selected_face.is_none() && !starts_new_origin_sketch
            {
                response.on_disabled_hover_text(
                    "The prior face sketch is read-only after the body changed. Select a current face to start the next sketch.",
                )
            } else {
                response
            };
            if response.clicked() {
                if starts_new_origin_sketch {
                    self.begin_new_origin_sketch();
                } else {
                    self.enter_sketch_mode();
                }
            }
            let plane_selection_valid = (1..=2).contains(&self.selected_faces.len());
            let plane = ui
                .add_enabled(
                    self.pending_operation.is_none()
                        && self.history_is_at_end()
                        && plane_selection_valid,
                    egui::Button::new("Plane"),
                )
                .on_hover_text(
                    "Construction plane · select one planar face for a coincident plane or two parallel faces for a midplane",
                );
            if plane.clicked() {
                self.stage_construction_plane();
            }
        });
        self.extrude_command_group(ui);
        ribbon_group(ui, "BOOLEAN", "body_boolean_commands", |ui| {
            let target = self.active_body_id();
            let has_tool = target.is_some_and(|target| {
                self.bodies
                    .iter()
                    .any(|body| body.id != target && body.visible)
            });
            let enabled = self.pending_operation.is_none()
                && self.history_is_at_end()
                && has_tool
                && self.active_component_instance().is_none();
            ui.add_enabled_ui(enabled, |ui| {
                ui.menu_button("Boolean...", |ui| {
                    for (label, operation) in [
                        ("Combine", BooleanOperation::Union),
                        ("Subtract", BooleanOperation::Difference),
                        ("Intersect", BooleanOperation::Intersection),
                    ] {
                        if ui
                            .button(label)
                            .on_hover_text(
                                "Uses the active body as target. Click the tool bodies in the viewport, then confirm.",
                            )
                            .clicked()
                        {
                            self.stage_body_boolean(operation);
                            ui.close();
                        }
                    }
                })
                .response
                .on_hover_text("Boolean operations");
            });
            // While a Boolean is staged the group becomes its operand panel:
            // the picks are the operation's real input and belong on screen,
            // not only in the status line.
            if let Some(crate::PendingOperation::BooleanBodies { keep_tools, .. }) =
                self.pending_operation
            {
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
                    egui::RichText::new(self.boolean_operand_summary())
                        .small()
                        .color(crate::theme::MUTED),
                )
                .on_hover_text("Click a body to add it as a tool; click it again to remove it.");
            }
        });
        ribbon_group(ui, "FEATURES", "solid_feature_presets", |ui| {
            let free = self.pending_operation.is_none() && self.history_is_at_end();
            ui.add_enabled_ui(free, |ui| {
                ui.menu_button("Features...", |ui| {
                    for (label, preset, enabled) in [
                        ("Revolve", SolidFeaturePreset::Revolve, true),
                        (
                            "Hole",
                            SolidFeaturePreset::Hole,
                            self.selected_face.is_some(),
                        ),
                        ("Rib", SolidFeaturePreset::Rib, self.selected_face.is_some()),
                        (
                            "Mirror",
                            SolidFeaturePreset::Mirror,
                            self.active_body_id().is_some(),
                        ),
                        (
                            "Pattern",
                            SolidFeaturePreset::LinearPattern,
                            self.active_body_id().is_some(),
                        ),
                        (
                            "Chamfer",
                            SolidFeaturePreset::Chamfer,
                            !self.selected_edges.is_empty(),
                        ),
                        (
                            "Fillet",
                            SolidFeaturePreset::Fillet,
                            !self.selected_edges.is_empty(),
                        ),
                    ] {
                        let response = ui
                            .add_enabled(enabled, egui::Button::new(label))
                            .on_hover_text(preset.detail());
                        if response.clicked() {
                            self.stage_preset_feature(preset);
                            ui.close();
                        }
                    }
                })
                .response
                .on_hover_text("Solid features");
            });
        });
        ribbon_group(ui, "MODIFY", "transform_commands", |ui| {
            let component_constraint = self.active_component_instance().and_then(|component| {
                self.document
                    .joint_for_child(component.id)
                    .map(|joint| joint.name.clone())
            });
            for tool in ActiveTool::ALL.into_iter().filter(|tool| {
                matches!(
                    tool,
                    ActiveTool::Move | ActiveTool::Rotate | ActiveTool::Scale
                )
            }) {
                let label = format!("{}  {}", tool.shortcut(), tool.label());
                let tool_enabled = if tool == ActiveTool::Scale {
                    self.scale_tool_available()
                } else {
                    transform_tools_available
                };
                let response = ui.add_enabled(
                    tool_enabled,
                    egui::Button::new(tool.shortcut())
                        .selected(self.active_tool == tool)
                        .corner_radius(6),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, tool_enabled, label.clone())
                });
                if response.clicked() {
                    self.active_tool = tool;
                }
                if matches!(tool, ActiveTool::Move | ActiveTool::Rotate)
                    && component_constraint.is_some()
                {
                    response.on_disabled_hover_text(format!(
                        "This component is constrained by {}; joint-coordinate editing comes next.",
                        component_constraint
                            .as_deref()
                            .expect("the constraint was checked above")
                    ));
                } else if tool == ActiveTool::Scale && self.active_component_instance().is_some() {
                    response.on_disabled_hover_text(
                        "Component occurrences preserve exact authored size; change a part parameter instead.",
                    );
                } else {
                    response.on_hover_text(format!(
                        "{} transform-preview tool · keyboard {}",
                        tool.label(),
                        tool.shortcut()
                    ));
                }
            }
        });
        ribbon_group(ui, "SELECT / VIEW", "selection_view_commands", |ui| {
            for tool in ActiveTool::ALL.into_iter().filter(|tool| {
                matches!(
                    tool,
                    ActiveTool::Select | ActiveTool::Measure | ActiveTool::Orbit
                )
            }) {
                let label = format!("{}  {}", tool.shortcut(), tool.label());
                let response = ui.add(
                    egui::Button::new(tool.shortcut())
                        .selected(self.active_tool == tool)
                        .corner_radius(6),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone())
                });
                if response.clicked() {
                    self.active_tool = tool;
                }
                response.on_hover_text(tool.label());
            }
            if ui
                .button("Frame")
                .on_hover_text("Frame all visible bodies")
                .clicked()
            {
                self.frame_visible_body(ui.ctx());
            }
            let shaded = self.model_display_mode.is_shaded();
            let edges = ui
                .add_enabled(
                    !shaded,
                    egui::Button::new("Edges").selected(self.edge_overlay),
                )
                .on_hover_text(if shaded {
                    "Visible grey edges are always enabled in Shaded mode"
                } else {
                    "Toggle diagnostic source-edge overlay"
                });
            if edges.clicked() {
                self.edge_overlay = !self.edge_overlay;
            }
            if ui
                .add(egui::Button::new("Shaded").selected(shaded))
                .on_hover_text(
                    "Toggle standard grey shaded-with-visible-edges display; diagnostic mode retains face roles and labels",
                )
                .clicked()
            {
                self.model_display_mode = if shaded {
                    viewport::ModelDisplayMode::Diagnostic
                } else {
                    viewport::ModelDisplayMode::ShadedEdges
                };
            }
        });
        ribbon_group(ui, "MOTION", "motion_commands", |ui| {
            let active_motion = self.active_motion_name();
            let motion_label = if self.motion.playing {
                "Stop motion"
            } else {
                "Play motion"
            };
            if ui
                .button(motion_label)
                .on_hover_text(format!(
                    "Play {active_motion} temporarily; Stop restores the authored pose"
                ))
                .clicked()
            {
                self.toggle_animation(ui.ctx());
            }
            if ui.button("Home").on_hover_text("Reset view").clicked() {
                self.reset_view(ui.ctx());
            }
        });
    }

    fn sketch_command_groups(&mut self, ui: &mut egui::Ui) {
        ribbon_group(ui, "SKETCH", "sketch_tool_grid", |ui| {
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
        });
        ribbon_group(ui, "COMPLETE", "sketch_complete_commands", |ui| {
            let finish_enabled = self.pending_operation.is_none()
                && !self.sketch_creation_draft_active()
                && !self.sketch.authoring().operations().is_empty();
            let response = ui.add_enabled(finish_enabled, egui::Button::new("Finish"));
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    finish_enabled,
                    "Finish sketch command",
                )
            });
            if response.clicked() {
                self.finish_sketch_now();
            }
        });
        self.extrude_command_group(ui);
        ribbon_group(ui, "VIEW", "sketch_view_commands", |ui| {
            if ui.button("Frame sketch").clicked() {
                self.frame_active_sketch();
            }
            let mut settings = self.sketch.snap_settings();
            if ui.toggle_value(&mut settings.enabled, "Snap").changed() {
                self.sketch.set_snap_settings(settings);
            }
        });
    }

    pub(crate) fn activate_sketch_tool_variant(&mut self, variant: ToolVariant) {
        if self.sketch.set_exact_tool(variant) {
            self.active_sketch_tool = variant;
        }
    }

    fn extrude_command_group(&mut self, ui: &mut egui::Ui) {
        ribbon_group(ui, "SOLID", "persistent_extrude_command", |ui| {
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
            let sketch_enabled = self.pending_operation.is_none()
                && self.history_is_at_end()
                && !already_extruded
                && distance_valid
                && sketch_edit_complete
                && !linked_sketch_support
                && eligibility.can_stage();
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
            let enabled = sketch_enabled || push_pull_enabled;
            let label = if enabled {
                RichText::new("Extrude").color(TEXT).strong()
            } else {
                RichText::new("Extrude").color(MUTED)
            };
            let response = ui.add_enabled(
                enabled,
                egui::Button::new(label)
                    .fill(if enabled { SELECTED_FILL } else { CARD })
                    .stroke(Stroke::new(1.0, if enabled { ACCENT } else { BORDER }))
                    .corner_radius(4),
            );
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, "Extrude")
            });
            let response = if enabled {
                response.on_hover_text(if push_pull_enabled {
                    "Move the complete selected face with a signed live preview."
                } else if self.sketch_finished {
                    "Start a live extrusion preview."
                } else {
                    "Start a live preview; the green tick finishes the sketch and publishes the extrusion together."
                })
            } else {
                let reason = if self.pending_operation.is_some() {
                    "Confirm or cancel the pending operation first.".to_owned()
                } else if !self.history_is_at_end() {
                    "Move the history marker to the end before creating another feature.".to_owned()
                } else if !distance_valid {
                    "Enter a finite, non-zero extrusion distance.".to_owned()
                } else if linked_sketch_support
                    || (self.selected_face.is_some() && linked_active_body)
                {
                    "Library component geometry is immutable in this workspace; edit its source definition or place another component.".to_owned()
                } else if self.selected_face.is_some() && push_pull_support.is_none() {
                    "Direct push/pull requires one unholed planar extrusion cap.".to_owned()
                } else if self.selected_face.is_some() && !active_sketch_consumed {
                    "Finish or consume the active sketch before pushing the selected face."
                        .to_owned()
                } else if already_extruded {
                    "Select an eligible face to push/pull, or create another sketch.".to_owned()
                } else if !sketch_edit_complete {
                    "Complete or cancel the active sketch edit first.".to_owned()
                } else {
                    eligibility.visible_reason().unwrap_or_else(|| {
                        "Create an eligible closed sketch before starting extrusion.".to_owned()
                    })
                };
                response.on_disabled_hover_text(reason)
            };
            if response.clicked() {
                let staged = if sketch_enabled {
                    self.stage_sketch_extrusion()
                } else {
                    self.stage_face_push_pull()
                };
                if staged {
                    self.show_properties_tab();
                }
            }
        });
    }
}
