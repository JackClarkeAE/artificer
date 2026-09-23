//! The Revolve command: a sketch profile turned about an axis (ADR 0055).
//!
//! A revolve is staged the way a loft is: an editor opens, the viewport shows
//! what confirming would build, and the contextual card carries the choices.
//! Its profile is a region of a finished sketch, picked in the model view;
//! its axis is chosen on the card from everything that can serve as one — a
//! centreline drawn in the sketch, the sketch's own axes, or the document's
//! origin axes where they lie in the sketch's plane. It turns a full turn or
//! through an angle, one way, the other or both; an angle typed over document
//! variables stays with them (ADR 0052). Every change asks the kernel for the
//! result at once, so the preview is the solid the history will hold, or the
//! reason there is none.

use artificer_kernel::{DebugScene, NativeKernel, Snapshot};
use std::collections::BTreeMap;
use std::f64::consts::TAU;

use artificer_model::{
    BodyId, FeatureDraft, FeatureId, FeatureInput, FeatureKind, OriginAxis, OutputDraft,
    ParameterBinding, ParameterExpression, ParameterOverrides, ParameterUnit, ParameterValue,
    ParsedParameterEntry, QuantityKind, ReplayAction, RevolveAxis, RevolveDirection, RevolveExtent,
    SketchAxisDirection, SketchId, SketchRevolve, SnapshotAssociation, format_parameter_binding,
    parse_parameter_entry, revolve::origin_axis_in_frame, sketch_region::sketch_region_at,
};
use artificer_protocol::{OperationReport, PrecisionPolicy, SolidOperation};
use artificer_sketch::{EvaluatedCurve2, RegionSignature, SketchEntityRole};
use eframe::egui;
use egui::RichText;

use crate::{
    ArchivedBody, Attempt, DisplayedBody, KernelLabApp, ModelBodyKind, PendingOperation,
    WorkbenchBody, WorkbenchMode, status_line, theme, viewport,
};

/// What confirming the staged revolve would build.
#[derive(Clone)]
pub(crate) struct RevolvePreview {
    pub(crate) recipe: SketchRevolve,
    pub(crate) snapshot: Snapshot,
    pub(crate) report: OperationReport,
    pub(crate) scene: DebugScene,
}

impl RevolvePreview {
    /// Whether the kernel answered exactly. A revolve with a cone, torus or
    /// sphere face combined with a body answers on the faceted tier, and
    /// says so in its warnings.
    pub(crate) fn is_exact(&self) -> bool {
        !self
            .report
            .rung
            .as_deref()
            .is_some_and(|rung| rung.contains("faceted"))
            && !self
                .report
                .warnings
                .iter()
                .any(|warning| warning.code.as_str().contains("FACETED"))
    }
}

/// A revolve in its editor.
#[derive(Clone)]
pub(crate) struct StagedRevolve {
    pub(crate) sketch: Option<SketchId>,
    pub(crate) regions: Vec<RegionSignature>,
    /// Where each region is anchored in its sketch, which is how the model
    /// view names it: the pick highlight is drawn from these.
    pub(crate) anchors: Vec<[f64; 2]>,
    pub(crate) axis: Option<RevolveAxis>,
    pub(crate) operation: SolidOperation,
    /// The body an add or a cut changes.
    pub(crate) target: Option<BodyId>,
    /// A full turn, or the angle below.
    pub(crate) full_turn: bool,
    /// The angle it turns through when it stops short of a full turn, in
    /// radians: what the angle field last came to.
    pub(crate) angle: f64,
    pub(crate) direction: RevolveDirection,
    /// The angle field as typed.
    pub(crate) angle_text: String,
    /// The variables the angle follows, while it follows any.
    pub(crate) angle_link: Option<AngleLink>,
    pub(crate) preview: Option<RevolvePreview>,
    /// Why there is no preview, when there is not.
    pub(crate) issue: Option<String>,
}

/// An angle typed over document variables: `sweep`, `sweep / 2 + 10`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AngleLink {
    /// What was typed, shown back in the field.
    pub(crate) text: String,
    pub(crate) expression: ParameterExpression,
}

/// The angle a partial revolve starts on: a quarter turn.
const DEFAULT_REVOLVE_ANGLE: f64 = std::f64::consts::FRAC_PI_2;

impl StagedRevolve {
    fn new(target: Option<BodyId>) -> Self {
        Self {
            sketch: None,
            regions: Vec::new(),
            anchors: Vec::new(),
            axis: None,
            operation: SolidOperation::New,
            target,
            full_turn: true,
            angle: DEFAULT_REVOLVE_ANGLE,
            direction: RevolveDirection::Forward,
            angle_text: format_degrees(DEFAULT_REVOLVE_ANGLE),
            angle_link: None,
            preview: None,
            issue: None,
        }
    }

    /// How far the staged revolve turns.
    pub(crate) const fn extent(&self) -> RevolveExtent {
        if self.full_turn {
            RevolveExtent::FullTurn
        } else {
            RevolveExtent::Angle {
                radians: self.angle,
                direction: self.direction,
            }
        }
    }

    fn recipe(&self) -> Result<SketchRevolve, String> {
        let sketch = self
            .sketch
            .ok_or_else(|| "Click the profile to revolve in a finished sketch".to_owned())?;
        let axis = self
            .axis
            .ok_or_else(|| "Choose the axis to revolve about".to_owned())?;
        let recipe = SketchRevolve::new(
            sketch,
            self.regions.clone(),
            axis,
            self.extent(),
            self.operation,
        )
        .map_err(|error| error.to_string())?;
        // An angle typed over variables stays with them; a full turn has no
        // angle to follow.
        let expression = self
            .angle_link
            .as_ref()
            .filter(|_| !self.full_turn)
            .map(|link| link.expression.clone());
        recipe
            .with_angle_expression(expression)
            .map_err(|error| error.to_string())
    }
}

/// An angle in radians as the angle field shows it, in degrees.
fn format_degrees(radians: f64) -> String {
    let degrees = radians.to_degrees();
    let rounded = (degrees * 1.0e6).round() / 1.0e6;
    format!("{rounded}")
}

/// One thing a revolve can turn about, as the card offers it.
#[derive(Clone, Debug, PartialEq)]
pub struct RevolveAxisChoice {
    pub axis: RevolveAxis,
    pub label: String,
    /// Why it cannot serve this sketch, when it cannot.
    pub unavailable: Option<String>,
}

/// A human name for which way a partial revolve turns, as the card says it.
const fn direction_label(direction: RevolveDirection) -> &'static str {
    match direction {
        RevolveDirection::Forward => "One way",
        RevolveDirection::Reversed => "Other way",
        RevolveDirection::Symmetric => "Symmetric",
    }
}

/// A human name for a revolve operation, as the card and the status line
/// say it.
const fn operation_label(operation: SolidOperation) -> &'static str {
    match operation {
        SolidOperation::New => "New body",
        SolidOperation::Add => "Add",
        SolidOperation::Cut => "Cut",
    }
}

impl KernelLabApp {
    /// Opens the revolve editor.
    ///
    /// A sketch still being drawn is finished first, as Extrude does, and
    /// becomes the revolve's sketch. A region already picked becomes its
    /// profile, and a sketch with only one region needs no pick at all, so
    /// Revolve works in either order (ADR 0041). The axis starts on the
    /// sketch's centreline when it has one.
    pub(crate) fn stage_revolve(&mut self) -> bool {
        if self.pending_operation.is_some() || !self.history_is_at_end() {
            return false;
        }
        if self.workbench_mode == WorkbenchMode::Sketch
            && !self.sketch_finished
            && !self.sketch.authoring().operations().is_empty()
            && !self.finish_sketch_now()
        {
            return false;
        }
        let mut staged = StagedRevolve::new(self.plane_boolean_target());
        if let Some((sketch, regions, anchors)) = self.revolve_profile_from_selection() {
            staged.axis = self.default_revolve_axis(sketch);
            staged.sketch = Some(sketch);
            staged.regions = regions;
            staged.anchors = anchors;
        }
        self.staged_revolve = Some(staged);
        self.pending_operation = Some(PendingOperation::StageRevolve { editing: None });
        self.refresh_revolve_preview();
        self.document_status =
            Some("Revolve · click the profile, then choose the axis on the card".to_owned());
        true
    }

    /// The picked regions of the active finished sketch, or its only region.
    fn revolve_profile_from_selection(
        &mut self,
    ) -> Option<(SketchId, Vec<RegionSignature>, Vec<[f64; 2]>)> {
        let sketch = self
            .active_sketch_index
            .and_then(|index| self.sketches.get(index))
            .filter(|sketch| sketch.finished && !sketch.consumed)
            .and_then(|sketch| sketch.id)?;
        let precision = PrecisionPolicy::default();
        let mut anchors = self.sketch.selected_region_canonical_anchors();
        if anchors.is_empty() {
            anchors = self.sketch_region_anchors(sketch);
            if anchors.len() != 1 {
                return None;
            }
        }
        let (regions, anchors): (Vec<_>, Vec<_>) = anchors
            .into_iter()
            .filter_map(|anchor| {
                sketch_region_at(&self.document, sketch, anchor, precision)
                    .map(|region| (region, anchor))
            })
            .unzip();
        (!regions.is_empty()).then_some((sketch, regions, anchors))
    }

    /// An interior point of every region a finished sketch has.
    pub(crate) fn sketch_region_anchors(&self, sketch: SketchId) -> Vec<[f64; 2]> {
        let Some(payload) = self.document.sketch(sketch).and_then(|record| {
            self.document
                .sketch_payload(sketch, record.geometry_revision)
        }) else {
            return Vec::new();
        };
        let Some(inputs) = payload
            .authoring()
            .and_then(|authoring| authoring.arrangement_inputs().ok())
        else {
            return Vec::new();
        };
        let precision = PrecisionPolicy::default();
        let arrangement = artificer_sketch::build_arrangement(
            &inputs,
            &precision,
            artificer_sketch::ArrangementLimits::default(),
        );
        arrangement
            .cells
            .iter()
            .filter_map(|cell| arrangement.cell_interior_sample(cell, &precision))
            .map(|anchor| [anchor.u, anchor.v])
            .collect()
    }

    /// The straight construction lines of a finished sketch, in the order
    /// they were drawn: its centrelines.
    fn sketch_centrelines(&self, sketch: SketchId) -> Vec<artificer_sketch::SketchEntityId> {
        let Some(authoring) = self
            .document
            .sketch(sketch)
            .and_then(|record| {
                self.document
                    .sketch_payload(sketch, record.geometry_revision)
            })
            .and_then(|payload| payload.authoring())
        else {
            return Vec::new();
        };
        authoring
            .active_entities()
            .filter(|entity| entity.role == SketchEntityRole::Construction)
            .filter(|entity| {
                matches!(
                    authoring.evaluated_curve(entity.id),
                    Ok(EvaluatedCurve2::Line { .. })
                )
            })
            .map(|entity| entity.id)
            .collect()
    }

    /// The axis a revolve of `sketch` starts on: its first centreline.
    fn default_revolve_axis(&self, sketch: SketchId) -> Option<RevolveAxis> {
        self.sketch_centrelines(sketch)
            .first()
            .map(|entity| RevolveAxis::SketchLine { entity: *entity })
    }

    /// Everything the staged revolve could turn about, in the order the card
    /// lists it, each saying why it cannot serve when it cannot.
    #[must_use]
    pub fn revolve_axis_choices(&self) -> Vec<RevolveAxisChoice> {
        let Some(sketch) = self
            .staged_revolve
            .as_ref()
            .and_then(|staged| staged.sketch)
        else {
            return Vec::new();
        };
        let mut choices = self
            .sketch_centrelines(sketch)
            .into_iter()
            .enumerate()
            .map(|(index, entity)| RevolveAxisChoice {
                axis: RevolveAxis::SketchLine { entity },
                label: format!("Centreline {}", index + 1),
                unavailable: None,
            })
            .collect::<Vec<_>>();
        for (axis, label) in [
            (SketchAxisDirection::U, "Sketch horizontal axis"),
            (SketchAxisDirection::V, "Sketch vertical axis"),
        ] {
            choices.push(RevolveAxisChoice {
                axis: RevolveAxis::SketchAxis { axis },
                label: label.to_owned(),
                unavailable: None,
            });
        }
        let frame = self.document.sketch_frame(sketch);
        for axis in [OriginAxis::X, OriginAxis::Y, OriginAxis::Z] {
            let unavailable = frame.map_or_else(
                || Some("The sketch has no plane yet".to_owned()),
                |frame| {
                    origin_axis_in_frame(axis, frame, PrecisionPolicy::default())
                        .err()
                        .map(|error| error.to_string())
                },
            );
            choices.push(RevolveAxisChoice {
                axis: RevolveAxis::OriginAxis { axis },
                label: format!("Origin {}", axis.label()),
                unavailable,
            });
        }
        // Every construction axis, by name: one that stands out of the
        // sketch's plane is offered but cannot be chosen.
        for axis in &self.construction_axes {
            let unavailable = frame.map_or_else(
                || Some("The sketch has no plane yet".to_owned()),
                |frame| {
                    artificer_model::revolve::line_in_frame(
                        axis.line.origin,
                        axis.line.direction,
                        frame,
                        PrecisionPolicy::default(),
                    )
                    .is_none()
                    .then(|| format!("{} does not lie in the sketch's plane", axis.name))
                },
            );
            choices.push(RevolveAxisChoice {
                axis: RevolveAxis::DatumAxis { axis: axis.feature },
                label: axis.name.clone(),
                unavailable,
            });
        }
        choices
    }

    /// Whether a click on a sketch region in the model view is a revolve
    /// pick.
    pub(crate) fn revolve_pick_active(&self) -> bool {
        matches!(
            self.pending_operation,
            Some(PendingOperation::StageRevolve { .. })
        ) && self.staged_revolve.is_some()
    }

    /// A region picked in the model view while the revolve editor is open.
    ///
    /// A region of another sketch starts the profile over in that sketch,
    /// and the axis over on its centreline. A region of the same sketch
    /// replaces the profile, or, with `additive`, is added to it or taken
    /// out of it.
    pub fn pick_revolve_region(
        &mut self,
        sketch_index: usize,
        anchor: [f64; 2],
        additive: bool,
    ) -> bool {
        let Some(sketch) = self
            .sketches
            .get(sketch_index)
            .filter(|sketch| sketch.finished)
            .and_then(|sketch| sketch.id)
        else {
            self.document_status =
                Some("Finish the sketch before revolving its profile".to_owned());
            return false;
        };
        let Some(region) =
            sketch_region_at(&self.document, sketch, anchor, PrecisionPolicy::default())
        else {
            return false;
        };
        let default_axis = self.default_revolve_axis(sketch);
        let Some(staged) = self.staged_revolve.as_mut() else {
            return false;
        };
        if staged.sketch == Some(sketch) {
            if let Some(index) = staged
                .regions
                .iter()
                .position(|existing| *existing == region)
            {
                if additive && staged.regions.len() > 1 {
                    staged.regions.remove(index);
                    staged.anchors.remove(index);
                }
            } else if additive {
                staged.regions.push(region);
                staged.anchors.push(anchor);
            } else {
                staged.regions = vec![region];
                staged.anchors = vec![anchor];
            }
        } else {
            staged.sketch = Some(sketch);
            staged.regions = vec![region];
            staged.anchors = vec![anchor];
            staged.axis = default_axis;
        }
        self.refresh_revolve_preview();
        true
    }

    /// Chooses the axis the staged revolve turns about.
    pub fn set_revolve_axis(&mut self, axis: RevolveAxis) {
        if let Some(staged) = self.staged_revolve.as_mut()
            && staged.axis != Some(axis)
        {
            staged.axis = Some(axis);
            self.refresh_revolve_preview();
        }
    }

    /// Chooses what the staged revolve does: a body of its own, or joined to
    /// or taken from the body it is staged over.
    pub fn set_revolve_operation(&mut self, operation: SolidOperation) {
        if let Some(staged) = self.staged_revolve.as_mut()
            && staged.operation != operation
        {
            staged.operation = operation;
            self.refresh_revolve_preview();
        }
    }

    /// Turns the staged revolve a full turn, or through its angle.
    pub fn set_revolve_full_turn(&mut self, full_turn: bool) {
        if let Some(staged) = self.staged_revolve.as_mut()
            && staged.full_turn != full_turn
        {
            staged.full_turn = full_turn;
            self.refresh_revolve_preview();
        }
    }

    /// Chooses which way a partial revolve turns from its sketch.
    pub fn set_revolve_direction(&mut self, direction: RevolveDirection) {
        if let Some(staged) = self.staged_revolve.as_mut()
            && staged.direction != direction
        {
            staged.direction = direction;
            self.refresh_revolve_preview();
        }
    }

    /// Takes what was typed in the angle field: degrees, `90`, or arithmetic
    /// over document variables, `sweep / 2`. An entry that names variables
    /// stays linked to them, so the revolve follows when they change. The
    /// revolve then stops short of a full turn.
    pub fn enter_revolve_angle(&mut self, text: &str) -> bool {
        if self.staged_revolve.is_none() {
            return false;
        }
        let entry = self.revolve_angle_entry(text);
        let Some(staged) = self.staged_revolve.as_mut() else {
            return false;
        };
        staged.angle_text = text.trim().to_owned();
        match entry {
            Ok((radians, link)) => {
                staged.angle = radians;
                staged.angle_link = link;
                staged.full_turn = false;
                self.refresh_revolve_preview();
                true
            }
            Err(message) => {
                self.document_status = Some(format!("Revolve angle: {message}"));
                false
            }
        }
    }

    /// What an angle entry comes to, in radians, and the link it makes when
    /// it names variables.
    fn revolve_angle_entry(&self, text: &str) -> Result<(f64, Option<AngleLink>), String> {
        let names = self
            .document
            .parameters()
            .records()
            .iter()
            .map(|record| (record.spec.key.clone(), record.id))
            .collect::<BTreeMap<_, _>>();
        let parsed = parse_parameter_entry(text, ParameterUnit::Degree, &|name: &str| {
            names.get(name).copied()
        })
        .map_err(|error| error.to_string())?;
        let not_an_angle = || "that is not an angle".to_owned();
        let (radians, link) = match parsed {
            ParsedParameterEntry::Literal(ParameterValue::Quantity { value }) => {
                let radians = match value.unit {
                    ParameterUnit::Degree => value.magnitude.to_radians(),
                    ParameterUnit::Radian => value.magnitude,
                    _ => return Err(not_an_angle()),
                };
                (radians, None)
            }
            ParsedParameterEntry::Literal(_) => return Err(not_an_angle()),
            ParsedParameterEntry::Expression(expression) => {
                let evaluated = self
                    .document
                    .evaluate_parameters(&ParameterOverrides::default())
                    .map_err(|error| error.to_string())?;
                let ParameterValue::Quantity { value } = expression
                    .evaluate_with(&evaluated)
                    .map_err(|error| error.to_string())?
                else {
                    return Err(not_an_angle());
                };
                if value.unit.quantity_kind() != QuantityKind::Angle {
                    return Err(not_an_angle());
                }
                // Canonical angles are radians.
                let link = (!expression.referenced_parameters().is_empty()).then(|| AngleLink {
                    text: text.trim().to_owned(),
                    expression,
                });
                (value.magnitude, link)
            }
        };
        if !(radians.is_finite() && radians > 0.0 && radians < TAU) {
            return Err(format!(
                "{}° is not more than nothing and less than a full turn; choose Full turn for 360°",
                format_degrees(radians)
            ));
        }
        Ok((radians, link))
    }

    /// Asks the kernel for the staged revolve as its picks now stand.
    fn refresh_revolve_preview(&mut self) {
        let Some(staged) = self.staged_revolve.as_ref() else {
            return;
        };
        let outcome = (|| {
            if staged.regions.is_empty() {
                return Err("Click the profile to revolve in a finished sketch".to_owned());
            }
            let recipe = staged.recipe()?;
            let action = ReplayAction::SketchRevolve(recipe.clone())
                .resolve_sketch_regions(&self.document, PrecisionPolicy::default())
                .map_err(|error| error.to_string())?;
            let ReplayAction::Kernel(command) = action else {
                return Err("the revolve did not resolve to a kernel command".to_owned());
            };
            let input = if staged.operation == SolidOperation::New {
                self.empty_snapshot.clone()
            } else {
                self.solid_target_snapshot(staged.target).ok_or_else(|| {
                    "An add or cut revolve needs a visible body to combine with".to_owned()
                })?
            };
            let (snapshot, report) = self.execute_preview_command(&input, command, "revolve")?;
            Ok(RevolvePreview {
                recipe,
                scene: NativeKernel::debug_scene(&snapshot),
                snapshot,
                report,
            })
        })();
        let staged = self
            .staged_revolve
            .as_mut()
            .expect("the staged revolve was present above");
        match outcome {
            Ok(preview) => {
                staged.preview = Some(preview);
                staged.issue = None;
            }
            Err(issue) => {
                staged.preview = None;
                staged.issue = Some(issue);
            }
        }
    }

    /// Commits the staged revolve, or rewrites the revolve it was reopened
    /// on.
    pub(crate) fn commit_staged_revolve(&mut self, editing: Option<FeatureId>) {
        self.refresh_revolve_preview();
        let Some(staged) = self.staged_revolve.clone() else {
            self.pending_operation = None;
            return;
        };
        let Some(preview) = staged.preview.clone() else {
            self.document_status = Some(format!(
                "Revolve not built: {}",
                staged
                    .issue
                    .unwrap_or_else(|| "it has nothing to build".to_owned())
            ));
            return;
        };
        let target = staged
            .target
            .filter(|_| staged.operation != SolidOperation::New);
        if let Some(feature) = editing {
            self.apply_revolve_edit(feature, preview.recipe, target);
            return;
        }
        let association = SnapshotAssociation::new(
            preview.report.input_snapshot,
            preview.report.output_snapshot,
            preview.report.semantic_digest,
        );
        let mut next_document = self.document.clone();
        let label = Self::next_document_feature_label(&next_document, FeatureKind::Revolve);
        let sketch = preview.recipe.sketch;
        let mut draft = FeatureDraft::new(
            FeatureKind::Revolve,
            label.clone(),
            ReplayAction::SketchRevolve(preview.recipe.clone()),
        )
        .with_commit(association)
        .with_input(FeatureInput::Sketch(sketch));
        // A construction axis it turns about is an input: moving the axis
        // rebuilds the revolve, and the axis cannot be deleted under it.
        if let RevolveAxis::DatumAxis { axis } = preview.recipe.axis {
            draft = draft.with_input(FeatureInput::Feature(axis));
        }
        // An angle that follows variables reads them, so changing one
        // rebuilds the revolve and none can be deleted from under it.
        for parameter in preview.recipe.parameter_references() {
            draft = draft.with_parameter(parameter);
        }
        draft = match target {
            Some(body) => draft
                .with_input(FeatureInput::Body(body))
                .with_output(OutputDraft::ModifyBody(body)),
            None => draft.with_output(OutputDraft::CreateBody {
                label: format!("Body {}", self.next_body_ordinal),
            }),
        };
        let appended = match next_document.append_feature(draft) {
            Ok(appended) => appended,
            Err(error) => {
                self.document_status = Some(format!("Revolve history rejected: {error}"));
                return;
            }
        };
        // The profile is spent the way an extruded one is: hidden, and back
        // when the revolve is suppressed or undone.
        if let Err(error) = next_document.auto_hide_sketch_consumed_by(sketch, appended.feature) {
            self.document_status =
                Some(format!("The revolved sketch could not be hidden: {error}"));
            return;
        }
        let displayed = DisplayedBody {
            scene: preview.scene,
            snapshot: preview.snapshot,
            report: preview.report.clone(),
        };
        self.document = next_document;
        self.archive_feature_report(appended.feature, preview.report);
        if !self
            .body_archive
            .iter()
            .any(|entry| entry.body.snapshot.id() == displayed.snapshot.id())
        {
            self.body_archive.push(ArchivedBody {
                body: displayed.clone(),
                kind: ModelBodyKind::Revolved,
            });
        }
        if let Some(body) = target {
            if let Some(existing) = self.bodies.iter_mut().find(|entry| entry.id == body) {
                existing.body = displayed.clone();
                existing.last_feature = appended.feature;
                existing.kind = ModelBodyKind::Revolved;
            }
            if self.active_body_id() == Some(body) {
                self.displayed = Some(displayed);
                self.model_body_kind = ModelBodyKind::Revolved;
            }
        } else if let Some(body) = appended.created_bodies.first().copied() {
            let ordinal = self.next_body_ordinal;
            self.next_body_ordinal = self.next_body_ordinal.saturating_add(1);
            self.active_body_ordinal = ordinal;
            self.bodies.push(WorkbenchBody {
                material: None,
                colour: None,
                id: body,
                last_feature: appended.feature,
                ordinal,
                body: displayed.clone(),
                kind: ModelBodyKind::Revolved,
                visible: true,
            });
            self.displayed = Some(displayed);
            self.model_body_kind = ModelBodyKind::Revolved;
        }
        self.body_pivot = self
            .displayed
            .as_ref()
            .and_then(|body| body.report.bounds.map(crate::presentation::bounds_center));
        self.staged_revolve = None;
        self.pending_operation = None;
        self.history_scrub_position = self.document.history_position();
        self.selected_history_feature = Some(appended.feature);
        self.restore_runtime_from_document();
        self.sync_feature_preview_from_document();
        self.last_attempt = Attempt::Accepted {
            operation: "Revolve",
        };
        self.document_status = Some(format!(
            "{label} committed · {}",
            operation_label(staged.operation)
        ));
    }

    /// Reopens a committed revolve in its editor.
    ///
    /// As with a loft (ADR 0036), the history rolls back to just before the
    /// revolve, so what is on screen is what it was built from, and
    /// confirming rewrites the revolve in its slot and replays what follows.
    pub(crate) fn begin_revolve_edit(&mut self, feature: FeatureId) -> bool {
        if self.pending_operation.is_some() {
            return false;
        }
        let Some((index, recipe, target)) = self
            .document
            .features()
            .iter()
            .enumerate()
            .find(|(_, node)| node.id == feature)
            .and_then(|(index, node)| match &node.action {
                ReplayAction::SketchRevolve(recipe) => Some((
                    index,
                    recipe.clone(),
                    node.inputs.iter().find_map(|input| match input {
                        FeatureInput::Body(body) => Some(*body),
                        FeatureInput::Feature(_) | FeatureInput::Sketch(_) => None,
                    }),
                )),
                _ => None,
            })
        else {
            self.document_status = Some("That feature is not a revolve".to_owned());
            return false;
        };
        if !self.move_history_cursor(index) {
            return false;
        }
        let anchors = recipe
            .regions
            .iter()
            .filter_map(|region| self.region_anchor(recipe.sketch, region))
            .collect();
        let mut staged = StagedRevolve::new(target.or_else(|| self.plane_boolean_target()));
        staged.sketch = Some(recipe.sketch);
        staged.regions = recipe.regions.clone();
        staged.anchors = anchors;
        staged.axis = Some(recipe.axis);
        staged.operation = recipe.operation;
        if let RevolveExtent::Angle { radians, direction } = recipe.extent {
            staged.full_turn = false;
            staged.angle = radians;
            staged.direction = direction;
            staged.angle_text = format_degrees(radians);
        }
        // An angle that follows variables reopens as the expression, so
        // confirming the edit keeps the link rather than freezing a number.
        if let Some(expression) = &recipe.angle_expression {
            let names = self
                .document
                .parameters()
                .records()
                .iter()
                .map(|record| (record.id, record.spec.key.clone()))
                .collect::<BTreeMap<_, _>>();
            let text = format_parameter_binding(
                &ParameterBinding::Expression {
                    expression: expression.clone(),
                },
                &|id| names.get(&id).cloned(),
            );
            staged.angle_text.clone_from(&text);
            staged.angle_link = Some(AngleLink {
                text,
                expression: expression.clone(),
            });
        }
        self.staged_revolve = Some(staged);
        self.pending_operation = Some(PendingOperation::StageRevolve {
            editing: Some(feature),
        });
        self.refresh_revolve_preview();
        self.document_status = Some(
            "Editing the revolve · pick the profile or change the axis, then confirm".to_owned(),
        );
        true
    }

    fn apply_revolve_edit(
        &mut self,
        feature: FeatureId,
        recipe: SketchRevolve,
        target: Option<BodyId>,
    ) {
        self.staged_revolve = None;
        self.pending_operation = None;
        let mut inputs = vec![FeatureInput::Sketch(recipe.sketch)];
        if let RevolveAxis::DatumAxis { axis } = recipe.axis {
            inputs.push(FeatureInput::Feature(axis));
        }
        // The body a revolve changes is its branch, which an edit keeps.
        let original_target = self.document.feature(feature).and_then(|node| {
            node.inputs.iter().find_map(|input| match input {
                FeatureInput::Body(body) => Some(*body),
                FeatureInput::Feature(_) | FeatureInput::Sketch(_) => None,
            })
        });
        if original_target != target {
            self.document_status = Some(
                "A revolve keeps the body it builds or changes; make a new revolve for a different one"
                    .to_owned(),
            );
            self.move_history_cursor(self.document.features().len());
            return;
        }
        if let Some(body) = target {
            inputs.push(FeatureInput::Body(body));
        }
        let parameter_inputs = recipe.parameter_references().into_iter().collect();
        match self.document.replace_feature_recipe(
            feature,
            ReplayAction::SketchRevolve(recipe),
            inputs,
            parameter_inputs,
        ) {
            Ok(_) => {
                self.move_history_cursor(self.document.features().len());
                self.selected_history_feature = Some(feature);
                if self.rebuild_document_from(feature) {
                    self.activate_body_made_by(feature);
                    self.document_status =
                        Some("Revolve rewritten; everything after it rebuilt".to_owned());
                }
            }
            Err(error) => {
                self.document_status = Some(format!("Revolve edit rejected: {error}"));
                self.move_history_cursor(self.document.features().len());
            }
        }
    }

    /// Makes the body a feature builds or changes the active one again, as
    /// it was when the feature was reopened: rolling the history back to
    /// edit the feature took that body off the screen.
    pub(crate) fn activate_body_made_by(&mut self, feature: FeatureId) {
        let Some(body) = self.document.feature(feature).and_then(|node| {
            node.outputs.iter().find_map(|output| match output {
                artificer_model::FeatureOutput::Body(body) => Some(*body),
                artificer_model::FeatureOutput::Sketch { .. } => None,
            })
        }) else {
            return;
        };
        if let Some(index) = self
            .bodies
            .iter()
            .position(|candidate| candidate.id == body)
        {
            self.activate_body(index);
        }
    }

    /// Abandons the revolve editor. An edit puts the whole model back.
    pub(crate) fn cancel_staged_revolve(&mut self, editing: Option<FeatureId>) {
        self.staged_revolve = None;
        self.pending_operation = None;
        if editing.is_some() {
            self.move_history_cursor(self.document.features().len());
            self.document_status = Some("Revolve edit abandoned".to_owned());
        }
    }

    /// The staged profile's regions, so the model view draws them picked.
    pub(crate) fn revolve_region_selections(&self) -> Vec<viewport::ModelSketchRegionSelection> {
        let Some(staged) = self.staged_revolve.as_ref() else {
            return Vec::new();
        };
        let Some(sketch_index) = self
            .sketches
            .iter()
            .position(|sketch| sketch.id.is_some() && sketch.id == staged.sketch)
        else {
            return Vec::new();
        };
        staged
            .anchors
            .iter()
            .map(|anchor| viewport::ModelSketchRegionSelection {
                sketch_index,
                anchor: *anchor,
            })
            .collect()
    }

    /// The REVOLVE card: the profile, the axis, the operation, and what the
    /// kernel made of them.
    pub(crate) fn revolve_controls(&mut self, ui: &mut egui::Ui) {
        let Some(staged) = self.staged_revolve.clone() else {
            return;
        };
        let editing = matches!(
            self.pending_operation,
            Some(PendingOperation::StageRevolve { editing: Some(_) })
        );
        let (title, colour) = match (&staged.preview, editing) {
            (Some(_), true) => ("EDITING REVOLVE", theme::warn()),
            (Some(preview), false) if preview.is_exact() => ("REVOLVE PREVIEW", theme::good()),
            (Some(_), false) => ("REVOLVE PREVIEW · FACETED", theme::warn()),
            (None, _) => ("REVOLVE", theme::muted()),
        };
        status_line(ui, title, colour);
        let profile = match (staged.sketch, staged.regions.len()) {
            (None, _) | (_, 0) => "No profile picked".to_owned(),
            (Some(sketch), count) => {
                let name = self
                    .sketches
                    .iter()
                    .find(|candidate| candidate.id == Some(sketch))
                    .map_or_else(
                        || "Sketch".to_owned(),
                        |candidate| format!("Sketch {}", candidate.ordinal),
                    );
                match count {
                    1 => format!("{name} · 1 region"),
                    count => format!("{name} · {count} regions"),
                }
            }
        };
        ui.label(RichText::new(format!("Profile · {profile}")).color(theme::text()));
        ui.label(
            RichText::new("Click a profile in a finished sketch · Shift-click adds a region")
                .small()
                .color(theme::muted()),
        );
        let choices = self.revolve_axis_choices();
        if !choices.is_empty() {
            ui.label(RichText::new("Axis").small().color(theme::muted()));
            let mut chosen = None;
            ui.horizontal_wrapped(|ui| {
                for choice in &choices {
                    let response = ui
                        .add_enabled(
                            choice.unavailable.is_none(),
                            egui::Button::new(&choice.label)
                                .selected(staged.axis == Some(choice.axis)),
                        )
                        .on_hover_text("Revolve about this axis")
                        .on_disabled_hover_text(choice.unavailable.clone().unwrap_or_default());
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            choice.unavailable.is_none(),
                            format!("Revolve axis {}", choice.label),
                        )
                    });
                    if response.clicked() {
                        chosen = Some(choice.axis);
                    }
                }
            });
            if let Some(axis) = chosen {
                self.set_revolve_axis(axis);
            }
        }
        self.revolve_extent_controls(ui, &staged);
        let can_combine = staged.target.is_some();
        ui.horizontal(|ui| {
            for operation in [
                SolidOperation::New,
                SolidOperation::Add,
                SolidOperation::Cut,
            ] {
                let enabled = operation == SolidOperation::New || can_combine;
                let response = ui
                    .add_enabled(
                        enabled && !editing,
                        egui::Button::new(operation_label(operation))
                            .selected(staged.operation == operation),
                    )
                    .on_hover_text(match operation {
                        SolidOperation::New => "Build the revolve as a body of its own",
                        SolidOperation::Add => "Join the revolve to the active body",
                        SolidOperation::Cut => "Take the revolve away from the active body",
                    });
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        enabled && !editing,
                        format!("Revolve operation {}", operation_label(operation)),
                    )
                });
                if response.clicked() {
                    self.set_revolve_operation(operation);
                }
            }
        });
        match (&staged.preview, &staged.issue) {
            (Some(preview), _) => {
                let warning = preview
                    .report
                    .warnings
                    .first()
                    .map(|warning| warning.message.clone());
                ui.label(
                    RichText::new(warning.unwrap_or_else(|| {
                        "Exact: every face is a plane, a cylinder, a cone, a sphere or a torus"
                            .to_owned()
                    }))
                    .small()
                    .color(if preview.is_exact() {
                        theme::muted()
                    } else {
                        theme::warn()
                    }),
                );
            }
            (None, Some(issue)) => {
                ui.label(RichText::new(issue).small().color(theme::warn()));
            }
            (None, None) => {}
        }
        ui.label(
            RichText::new("Enter or the tick confirms · Escape abandons")
                .small()
                .color(theme::muted()),
        );
    }

    /// The card's extent row: a full turn or an angle, the angle itself —
    /// degrees or variables — and which way it turns.
    fn revolve_extent_controls(&mut self, ui: &mut egui::Ui, staged: &StagedRevolve) {
        ui.label(RichText::new("Extent").small().color(theme::muted()));
        let mut full_turn = None;
        ui.horizontal(|ui| {
            for (full, label, hover) in [
                (true, "Full turn", "Turn the profile all the way round"),
                (
                    false,
                    "Angle",
                    "Turn the profile through an angle and close it with the profile at each end",
                ),
            ] {
                let response = ui
                    .add(egui::Button::new(label).selected(staged.full_turn == full))
                    .on_hover_text(hover);
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        format!("Revolve extent {label}"),
                    )
                });
                if response.clicked() {
                    full_turn = Some(full);
                }
            }
        });
        if let Some(full) = full_turn {
            self.set_revolve_full_turn(full);
        }
        if staged.full_turn {
            return;
        }
        // Degrees, or arithmetic over document variables: `sweep`,
        // `sweep / 2`. Evaluated when the field is left; an entry that names
        // variables stays linked to them.
        let mut text = staged.angle_text.clone();
        let response = ui.add(
            egui::TextEdit::singleline(&mut text)
                .desired_width(ui.available_width().min(160.0))
                .hint_text("degrees or variables…"),
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Revolve angle")
        });
        if response.changed()
            && let Some(current) = self.staged_revolve.as_mut()
        {
            current.angle_text.clone_from(&text);
        }
        if response.lost_focus() {
            self.enter_revolve_angle(&text);
        }
        if let Some(link) = &staged.angle_link {
            ui.label(
                RichText::new(format!(
                    "Follows {} · changing the variable rebuilds this revolve",
                    link.text
                ))
                .small()
                .color(theme::accent()),
            );
        }
        let mut direction = None;
        ui.horizontal(|ui| {
            for candidate in [
                RevolveDirection::Forward,
                RevolveDirection::Reversed,
                RevolveDirection::Symmetric,
            ] {
                let label = direction_label(candidate);
                let response = ui
                    .add(egui::Button::new(label).selected(staged.direction == candidate))
                    .on_hover_text(match candidate {
                        RevolveDirection::Forward => "Turn right-handed about the axis as it runs",
                        RevolveDirection::Reversed => "Turn the other way round the axis",
                        RevolveDirection::Symmetric => {
                            "Turn half the angle each way, with the sketch in the middle"
                        }
                    });
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        format!("Revolve direction {label}"),
                    )
                });
                if response.clicked() {
                    direction = Some(candidate);
                }
            }
        });
        if let Some(direction) = direction {
            self.set_revolve_direction(direction);
        }
    }

    /// How far the staged revolve turns.
    #[must_use]
    pub fn staged_revolve_extent(&self) -> Option<RevolveExtent> {
        self.staged_revolve.as_ref().map(StagedRevolve::extent)
    }

    /// Which variables the staged revolve's angle follows, as typed, while
    /// it follows any.
    #[must_use]
    pub fn staged_revolve_angle_follows(&self) -> Option<String> {
        self.staged_revolve
            .as_ref()
            .filter(|staged| !staged.full_turn)
            .and_then(|staged| staged.angle_link.as_ref())
            .map(|link| link.text.clone())
    }

    /// Why the staged revolve has no preview, when it has none.
    #[must_use]
    pub fn staged_revolve_issue(&self) -> Option<String> {
        self.staged_revolve
            .as_ref()
            .and_then(|staged| staged.issue.clone())
    }

    /// Whether the staged revolve has a preview the kernel built.
    #[must_use]
    pub fn staged_revolve_has_preview(&self) -> bool {
        self.staged_revolve
            .as_ref()
            .is_some_and(|staged| staged.preview.is_some())
    }

    /// The axis the staged revolve turns about, if one is chosen.
    #[must_use]
    pub fn staged_revolve_axis(&self) -> Option<RevolveAxis> {
        self.staged_revolve.as_ref().and_then(|staged| staged.axis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SketchGeometry, SketchPlane, SketchPoint};
    use artificer_model::FeatureOutput;

    const PI: f64 = std::f64::consts::PI;
    const TAU: f64 = std::f64::consts::TAU;

    /// A 1 × 3 rectangle at `r` in [1, 2] on the XZ plane, beside a
    /// centreline drawn up the sketch's vertical axis, still being drawn.
    fn rectangle_beside_a_centreline(app: &mut KernelLabApp) {
        app.open_origin_plane_sketch(SketchPlane::XZ);
        let rectangle = app
            .sketch
            .stage_geometry(SketchGeometry::Rectangle {
                first: SketchPoint::new(1.0, 0.0),
                opposite: SketchPoint::new(2.0, 3.0),
            })
            .expect("the section stages");
        app.commit_sketch_stroke(rectangle);
        let centreline = app
            .sketch
            .stage_geometry_with_role(
                SketchGeometry::Segment {
                    start: SketchPoint::new(0.0, 0.0),
                    end: SketchPoint::new(0.0, 3.0),
                },
                crate::sketch::SketchEntityRole::Construction,
            )
            .expect("the centreline stages");
        app.commit_sketch_stroke(centreline);
    }

    fn preview_volume(app: &KernelLabApp) -> f64 {
        app.staged_revolve
            .as_ref()
            .and_then(|staged| staged.preview.as_ref())
            .map(|preview| preview.snapshot.measures().volume)
            .unwrap_or_else(|| panic!("no preview: {:?}", app.staged_revolve_issue()))
    }

    fn assert_close(actual: f64, expected: f64, what: &str) {
        assert!(
            ((actual - expected) / expected).abs() < 1.0e-9,
            "{what}: {actual} should be {expected}"
        );
    }

    fn revolve_feature(app: &KernelLabApp) -> FeatureId {
        app.document
            .features()
            .iter()
            .rev()
            .find(|node| node.kind == FeatureKind::Revolve)
            .expect("a revolve is in the history")
            .id
    }

    fn revolved_body(app: &KernelLabApp, feature: FeatureId) -> &WorkbenchBody {
        let body = app
            .document
            .feature(feature)
            .and_then(|node| {
                node.outputs.iter().find_map(|output| match output {
                    FeatureOutput::Body(body) => Some(*body),
                    FeatureOutput::Sketch { .. } => None,
                })
            })
            .expect("the revolve made a body");
        app.bodies
            .iter()
            .find(|candidate| candidate.id == body)
            .expect("the body is on screen")
    }

    /// Every axis the card offers turns the profile about the line it names,
    /// and an origin axis that does not lie in the sketch's plane is offered
    /// but unavailable, saying why.
    #[test]
    fn a_profile_revolves_about_every_axis_the_card_offers() {
        let mut app = KernelLabApp::default();
        rectangle_beside_a_centreline(&mut app);
        assert!(app.stage_revolve(), "{:?}", app.document_status);
        let choices = app.revolve_axis_choices();
        let labels = choices
            .iter()
            .map(|choice| (choice.label.as_str(), choice.unavailable.is_none()))
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![
                ("Centreline 1", true),
                ("Sketch horizontal axis", true),
                ("Sketch vertical axis", true),
                ("Origin X axis", true),
                ("Origin Y axis", false),
                ("Origin Z axis", true),
            ]
        );
        assert_eq!(app.staged_revolve_axis(), Some(choices[0].axis));
        // About the centreline, the vertical axis and the world Z axis: the
        // same tube, r in [1, 2] and 3 tall.
        let tube = PI * (4.0 - 1.0) * 3.0;
        assert_close(preview_volume(&app), tube, "centreline");
        for index in [2, 5] {
            app.set_revolve_axis(choices[index].axis);
            assert_close(preview_volume(&app), tube, choices[index].label.as_str());
        }
        // About the horizontal axis, the section's bottom lies on the axis:
        // a disc of radius 3, 1 thick.
        app.set_revolve_axis(choices[1].axis);
        assert_close(preview_volume(&app), PI * 9.0, "horizontal axis");
        // The world X axis is the sketch's horizontal axis here.
        app.set_revolve_axis(choices[3].axis);
        assert_close(preview_volume(&app), PI * 9.0, "origin X axis");

        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        let feature = revolve_feature(&app);
        assert_close(
            revolved_body(&app, feature).body.snapshot.measures().volume,
            PI * 9.0,
            "committed",
        );
        // The sketch is spent, as an extruded one is.
        let sketch = app
            .document
            .feature(feature)
            .and_then(|node| {
                node.inputs.iter().find_map(|input| match input {
                    FeatureInput::Sketch(sketch) => Some(*sketch),
                    _ => None,
                })
            })
            .expect("the sketch is an input");
        assert!(!app.document.sketch(sketch).expect("the sketch").visible);
        assert!(
            app.feature_preview
                .entries
                .iter()
                .any(|entry| entry.kind == crate::FeaturePreviewKind::Revolve
                    && entry.label() == "Revolve 1")
        );
    }

    /// Reopened, a revolve takes a new axis and rewrites itself in place;
    /// saved and opened again, it replays from its sketch.
    #[test]
    fn a_revolve_is_edited_in_place_and_replays_from_its_sketch() {
        let mut app = KernelLabApp::default();
        rectangle_beside_a_centreline(&mut app);
        assert!(app.stage_revolve());
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        let feature = revolve_feature(&app);
        let features = app.document.features().len();

        assert!(app.feature_has_an_editor(feature));
        assert!(app.begin_revolve_edit(feature), "{:?}", app.document_status);
        app.set_revolve_axis(RevolveAxis::SketchAxis {
            axis: SketchAxisDirection::U,
        });
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        assert_eq!(
            app.document.features().len(),
            features,
            "rewritten, not added"
        );
        assert_close(
            revolved_body(&app, feature).body.snapshot.measures().volume,
            PI * 9.0,
            "edited",
        );

        let json = app.native_document_json().expect("the document saves");
        let hydrated = crate::document_replay::hydrate_document_json_with_options(
            &json,
            crate::document_replay::HydrationOptions::default(),
        )
        .expect("the document replays");
        assert!(hydrated.document.features().iter().any(|node| matches!(
            &node.action,
            ReplayAction::SketchRevolve(recipe)
                if recipe.axis == RevolveAxis::SketchAxis { axis: SketchAxisDirection::U }
        )));
    }

    fn preview_centroid(app: &KernelLabApp) -> artificer_protocol::Point3 {
        app.staged_revolve
            .as_ref()
            .and_then(|staged| staged.preview.as_ref())
            .and_then(|preview| preview.snapshot.measures().centroid)
            .unwrap_or_else(|| panic!("no preview: {:?}", app.staged_revolve_issue()))
    }

    /// Turned through an angle, a revolve sweeps its share of the tube, one
    /// way, the other, or both; an angle typed over a variable follows it and
    /// reopens as it; and a full turn has no angle to follow.
    #[test]
    fn a_revolve_turns_through_an_angle_one_way_the_other_or_both() {
        let mut app = KernelLabApp::default();
        let sweep = app
            .document
            .add_parameter(
                artificer_model::ParameterSpec::new(
                    "sweep",
                    "sweep",
                    artificer_model::ParameterType::Quantity(QuantityKind::Angle),
                )
                .with_display_unit(ParameterUnit::Degree),
                ParameterBinding::literal(ParameterValue::quantity(45.0, ParameterUnit::Degree)),
            )
            .expect("the variable is added");
        rectangle_beside_a_centreline(&mut app);
        assert!(app.stage_revolve());
        let tube = PI * (4.0 - 1.0) * 3.0;
        assert_eq!(app.staged_revolve_extent(), Some(RevolveExtent::FullTurn));
        assert_close(preview_volume(&app), tube, "full turn");

        // The sketch is on XZ and the centreline runs up +Z, so turning one
        // way from the profile on +X goes towards +Y.
        assert!(app.enter_revolve_angle("90"), "{:?}", app.document_status);
        assert_close(preview_volume(&app), tube / 4.0, "a quarter");
        assert!(preview_centroid(&app).y > 0.5);
        app.set_revolve_direction(RevolveDirection::Reversed);
        assert_close(preview_volume(&app), tube / 4.0, "the other way");
        assert!(preview_centroid(&app).y < -0.5);
        app.set_revolve_direction(RevolveDirection::Symmetric);
        assert!(preview_centroid(&app).y.abs() < 1.0e-9);

        // Out of range is refused and changes nothing.
        assert!(!app.enter_revolve_angle("400"));
        assert!(!app.enter_revolve_angle("5 mm"));
        assert_close(preview_volume(&app), tube / 4.0, "unchanged");

        assert!(
            app.enter_revolve_angle("sweep * 4"),
            "{:?}",
            app.document_status
        );
        assert_eq!(
            app.staged_revolve_angle_follows().as_deref(),
            Some("sweep * 4")
        );
        assert_close(preview_volume(&app), tube / 2.0, "half, from the variable");
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        let feature = revolve_feature(&app);
        assert_eq!(
            app.document
                .feature(feature)
                .expect("the revolve")
                .parameter_inputs,
            vec![sweep],
            "the revolve reads the variable its angle names"
        );
        assert!(app.document.clone().remove_parameter(sweep).is_err());

        // Reopened, it keeps the link and the direction.
        assert!(app.begin_revolve_edit(feature), "{:?}", app.document_status);
        assert_eq!(
            app.staged_revolve_angle_follows().as_deref(),
            Some("sweep * 4")
        );
        assert_eq!(
            app.staged_revolve_extent(),
            Some(RevolveExtent::Angle {
                radians: PI,
                direction: RevolveDirection::Symmetric,
            })
        );
        // A full turn lets the variable go.
        app.set_revolve_full_turn(true);
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        assert!(
            app.document
                .feature(feature)
                .expect("the revolve")
                .parameter_inputs
                .is_empty()
        );
        assert_close(
            revolved_body(&app, feature).body.snapshot.measures().volume,
            tube,
            "a full turn again",
        );
    }

    /// A triangle with one leg on the centreline turns into a pointed cone:
    /// a third of the cylinder it stands in.
    #[test]
    fn a_triangle_against_its_centreline_revolves_into_a_cone() {
        let mut app = KernelLabApp::default();
        app.open_origin_plane_sketch(SketchPlane::XZ);
        for (start, end) in [((0.0, 0.0), (2.0, 0.0)), ((2.0, 0.0), (0.0, 3.0))] {
            let side = app
                .sketch
                .stage_geometry(SketchGeometry::Segment {
                    start: SketchPoint::new(start.0, start.1),
                    end: SketchPoint::new(end.0, end.1),
                })
                .expect("the side stages");
            app.commit_sketch_stroke(side);
        }
        let centreline = app
            .sketch
            .stage_geometry_with_role(
                SketchGeometry::Segment {
                    start: SketchPoint::new(0.0, 0.0),
                    end: SketchPoint::new(0.0, 3.0),
                },
                crate::sketch::SketchEntityRole::Construction,
            )
            .expect("the centreline stages");
        app.commit_sketch_stroke(centreline);
        let closing = app
            .sketch
            .stage_geometry(SketchGeometry::Segment {
                start: SketchPoint::new(0.0, 3.0),
                end: SketchPoint::new(0.0, 0.0),
            })
            .expect("the leg on the axis stages");
        app.commit_sketch_stroke(closing);
        assert!(app.stage_revolve(), "{:?}", app.document_status);
        assert_close(preview_volume(&app), PI * 4.0 * 3.0 / 3.0, "cone");
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
    }

    /// A circle drawn inside the section is a hole in its region, and the
    /// revolve sweeps it as a ring-shaped cavity: the tube less a torus, by
    /// Pappus.
    #[test]
    fn a_hole_in_the_section_revolves_into_a_cavity() {
        let mut app = KernelLabApp::default();
        rectangle_beside_a_centreline(&mut app);
        let hole = app
            .sketch
            .stage_geometry(SketchGeometry::Circle {
                center: SketchPoint::new(1.5, 1.5),
                rim: SketchPoint::new(1.75, 1.5),
            })
            .expect("the hole stages");
        app.commit_sketch_stroke(hole);
        assert!(app.stage_revolve(), "{:?}", app.document_status);
        // The sketch has two regions now, the ring and the disc inside it;
        // the ring is the one to turn.
        let sketch_index = app.sketches.len() - 1;
        let ring = app.sketch_region_anchors(app.sketches[sketch_index].id.expect("finished"));
        let anchor = ring
            .into_iter()
            .find(|anchor| (anchor[0] - 1.5).hypot(anchor[1] - 1.5) > 0.3)
            .expect("the ring offers an anchor");
        assert!(app.pick_revolve_region(sketch_index, anchor, false));
        let tube = PI * (4.0 - 1.0) * 3.0;
        let torus = PI * 0.25 * 0.25 * TAU * 1.5;
        assert_close(preview_volume(&app), tube - torus, "tube with a cavity");
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
    }

    /// Added to or cut from the body it is staged over, a revolve changes
    /// that body; a bore through the corner of the starting block takes a
    /// quarter of a cylinder away, exactly.
    #[test]
    fn a_revolve_cuts_the_body_it_is_staged_over() {
        let mut app = KernelLabApp::default();
        let before = app.displayed_measures().expect("the block").volume;
        app.open_origin_plane_sketch(SketchPlane::XZ);
        let bore = app
            .sketch
            .stage_geometry(SketchGeometry::Rectangle {
                first: SketchPoint::new(0.0, -1.0),
                opposite: SketchPoint::new(0.5, 5.0),
            })
            .expect("the section stages");
        app.commit_sketch_stroke(bore);
        assert!(app.stage_revolve());
        app.set_revolve_axis(RevolveAxis::OriginAxis {
            axis: OriginAxis::Z,
        });
        app.set_revolve_operation(SolidOperation::Cut);
        assert!(
            app.staged_revolve_has_preview(),
            "{:?}",
            app.staged_revolve_issue()
        );
        let bodies = app.bodies.len();
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        assert_eq!(app.bodies.len(), bodies, "a cut makes no new body");
        let after = app.displayed_measures().expect("the cut block").volume;
        assert_close(after, before - PI * 0.25 * 4.0 / 4.0, "block less bore");
    }
}
