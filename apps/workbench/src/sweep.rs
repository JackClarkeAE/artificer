//! The Sweep command: a sketch profile carried along a path (ADR 0055).
//!
//! A sweep is staged the way a revolve is: an editor opens, the viewport
//! shows what confirming would build, and the card carries the choices. Its
//! profile is a region of a finished sketch, picked in the model view. Its
//! path is chosen on the card from every open, smooth chain of curves in the
//! other finished sketches, since a path is usually a few lines and arcs
//! with no region to click. Every change asks the kernel for the result at
//! once, so the preview is the solid the history will hold, or the reason
//! there is none.

use artificer_kernel::{DebugScene, NativeKernel, Snapshot};
use artificer_model::{
    BodyId, FeatureDraft, FeatureId, FeatureInput, FeatureKind, OutputDraft, ReplayAction,
    SketchId, SketchSweep, SnapshotAssociation, SweepPath, loft::SketchLoftSection,
    sketch_region::sketch_region_at,
};
use artificer_protocol::{OperationReport, PrecisionPolicy, SolidOperation, SweepOrientation};
use artificer_sketch::{RegionSignature, SketchEntityId};
use eframe::egui;
use egui::RichText;

use crate::{
    ArchivedBody, Attempt, DisplayedBody, KernelLabApp, ModelBodyKind, PendingOperation,
    WorkbenchBody, WorkbenchMode, status_line, theme, viewport,
};

/// What confirming the staged sweep would build.
#[derive(Clone)]
pub(crate) struct SweepPreview {
    pub(crate) recipe: SketchSweep,
    pub(crate) snapshot: Snapshot,
    pub(crate) report: OperationReport,
    pub(crate) scene: DebugScene,
}

impl SweepPreview {
    /// How far a skinned sweep departs from the true one, when it is
    /// skinned.
    fn departure(&self) -> Option<f64> {
        self.report
            .warnings
            .iter()
            .find(|warning| warning.code.as_str() == "SWEEP_APPROXIMATION_TOLERANCE")
            .and_then(|warning| warning.measurement)
            .map(|measurement| measurement.measured)
    }
}

/// A sweep in its editor.
#[derive(Clone)]
pub(crate) struct StagedSweep {
    pub(crate) sketch: Option<SketchId>,
    pub(crate) regions: Vec<RegionSignature>,
    /// Where each region is anchored in its sketch, which is how the model
    /// view names it.
    pub(crate) anchors: Vec<[f64; 2]>,
    pub(crate) path: Option<SweepPath>,
    pub(crate) orientation: SweepOrientation,
    pub(crate) operation: SolidOperation,
    /// The body an add or a cut changes.
    pub(crate) target: Option<BodyId>,
    pub(crate) preview: Option<SweepPreview>,
    /// Why there is no preview, when there is not.
    pub(crate) issue: Option<String>,
}

impl StagedSweep {
    fn recipe(&self) -> Result<SketchSweep, String> {
        let sketch = self
            .sketch
            .ok_or_else(|| "Click the profile to sweep in a finished sketch".to_owned())?;
        let path = self
            .path
            .clone()
            .ok_or_else(|| "Choose the path to sweep along".to_owned())?;
        SketchSweep::new(
            SketchLoftSection::new(sketch, self.regions.clone()),
            path,
            self.orientation,
            self.operation,
        )
        .map_err(|error| error.to_string())
    }
}

/// One path a sweep can follow, as the card offers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SweepPathChoice {
    pub path: SweepPath,
    pub label: String,
}

/// Whether two paths are the same curves, whichever way they run.
fn same_curves(a: &SweepPath, b: &SweepPath) -> bool {
    a.sketch == b.sketch && a.entities == b.entities
}

const fn orientation_label(orientation: SweepOrientation) -> &'static str {
    match orientation {
        SweepOrientation::RotationMinimising => "Follow path",
        SweepOrientation::Fixed => "Keep orientation",
    }
}

const fn operation_label(operation: SolidOperation) -> &'static str {
    match operation {
        SolidOperation::New => "New body",
        SolidOperation::Add => "Add",
        SolidOperation::Cut => "Cut",
    }
}

impl KernelLabApp {
    /// Opens the sweep editor. A sketch still being drawn is finished first
    /// and becomes the profile's sketch; its picked or only region is the
    /// profile, and the path starts on the first one the other sketches
    /// offer.
    pub(crate) fn stage_sweep(&mut self) -> bool {
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
        let mut staged = StagedSweep {
            sketch: None,
            regions: Vec::new(),
            anchors: Vec::new(),
            path: None,
            orientation: SweepOrientation::RotationMinimising,
            operation: SolidOperation::New,
            target: self.plane_boolean_target(),
            preview: None,
            issue: None,
        };
        if let Some((sketch, regions, anchors)) = self.sweep_profile_from_selection() {
            staged.sketch = Some(sketch);
            staged.regions = regions;
            staged.anchors = anchors;
        }
        self.staged_sweep = Some(staged);
        self.pending_operation = Some(PendingOperation::StageSweep { editing: None });
        self.default_sweep_path();
        self.refresh_sweep_preview();
        self.document_status =
            Some("Sweep · click the profile, then choose the path on the card".to_owned());
        true
    }

    /// The picked regions of the active finished sketch, or its only region.
    fn sweep_profile_from_selection(
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

    /// Every path the staged sweep could follow: each open, smooth chain of
    /// curves in a finished sketch other than the profile's, in sketch
    /// order.
    #[must_use]
    pub fn sweep_path_choices(&self) -> Vec<SweepPathChoice> {
        let profile = self.staged_sweep.as_ref().and_then(|staged| staged.sketch);
        let mut choices = Vec::new();
        for sketch in &self.sketches {
            let Some(id) = sketch.id.filter(|id| Some(*id) != profile) else {
                continue;
            };
            if !sketch.finished {
                continue;
            }
            let Some(authoring) = self
                .document
                .sketch(id)
                .and_then(|record| self.document.sketch_payload(id, record.geometry_revision))
                .and_then(|payload| payload.authoring())
            else {
                continue;
            };
            let mut covered = std::collections::BTreeSet::<SketchEntityId>::new();
            let mut chains = 0;
            for entity in authoring.active_entities() {
                if covered.contains(&entity.id) {
                    continue;
                }
                let chain = authoring.tangent_chain_through(entity.id);
                covered.extend(chain.iter().copied());
                if chain.is_empty() || authoring.ordered_chain(&chain).is_err() {
                    continue;
                }
                chains += 1;
                choices.push(SweepPathChoice {
                    label: format!(
                        "Sketch {} path {chains} · {} {}",
                        sketch.ordinal,
                        chain.len(),
                        if chain.len() == 1 { "curve" } else { "curves" }
                    ),
                    path: SweepPath {
                        sketch: id,
                        entities: chain,
                        reversed: false,
                    },
                });
            }
        }
        choices
    }

    /// Starts the staged sweep on the first path offered, if it has none.
    fn default_sweep_path(&mut self) {
        if self
            .staged_sweep
            .as_ref()
            .is_some_and(|staged| staged.path.is_none())
        {
            let first = self
                .sweep_path_choices()
                .into_iter()
                .next()
                .map(|choice| choice.path);
            if let Some(staged) = self.staged_sweep.as_mut() {
                staged.path = first;
            }
        }
    }

    /// Whether a click on a sketch region in the model view is a sweep pick.
    pub(crate) fn sweep_pick_active(&self) -> bool {
        matches!(
            self.pending_operation,
            Some(PendingOperation::StageSweep { .. })
        ) && self.staged_sweep.is_some()
    }

    /// A region picked in the model view while the sweep editor is open: it
    /// becomes the profile, or with `additive` joins it or leaves it.
    pub fn pick_sweep_region(
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
            self.document_status = Some("Finish the sketch before sweeping its profile".to_owned());
            return false;
        };
        let Some(region) =
            sketch_region_at(&self.document, sketch, anchor, PrecisionPolicy::default())
        else {
            return false;
        };
        let Some(staged) = self.staged_sweep.as_mut() else {
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
            // A path in the new profile's own sketch can no longer serve.
            if staged
                .path
                .as_ref()
                .is_some_and(|path| path.sketch == sketch)
            {
                staged.path = None;
            }
        }
        self.default_sweep_path();
        self.refresh_sweep_preview();
        true
    }

    /// Chooses the path the staged sweep follows. Choosing the one it
    /// already follows keeps the way it runs.
    pub fn set_sweep_path(&mut self, path: SweepPath) {
        if let Some(staged) = self.staged_sweep.as_mut()
            && !staged
                .path
                .as_ref()
                .is_some_and(|current| same_curves(current, &path))
        {
            staged.path = Some(path);
            self.refresh_sweep_preview();
        }
    }

    /// Runs the staged sweep's path the other way, from its far end.
    pub fn reverse_sweep_path(&mut self) {
        if let Some(path) = self
            .staged_sweep
            .as_mut()
            .and_then(|staged| staged.path.as_mut())
        {
            path.reversed = !path.reversed;
            self.refresh_sweep_preview();
        }
    }

    /// Chooses how the profile is carried along the path.
    pub fn set_sweep_orientation(&mut self, orientation: SweepOrientation) {
        if let Some(staged) = self.staged_sweep.as_mut()
            && staged.orientation != orientation
        {
            staged.orientation = orientation;
            self.refresh_sweep_preview();
        }
    }

    /// Chooses what the staged sweep does to the body it is staged over.
    pub fn set_sweep_operation(&mut self, operation: SolidOperation) {
        if let Some(staged) = self.staged_sweep.as_mut()
            && staged.operation != operation
        {
            staged.operation = operation;
            self.refresh_sweep_preview();
        }
    }

    /// Asks the kernel for the staged sweep as its picks now stand.
    fn refresh_sweep_preview(&mut self) {
        let Some(staged) = self.staged_sweep.as_ref() else {
            return;
        };
        let outcome = (|| {
            if staged.regions.is_empty() {
                return Err("Click the profile to sweep in a finished sketch".to_owned());
            }
            let recipe = staged.recipe()?;
            let action = ReplayAction::SketchSweep(recipe.clone())
                .resolve_sketch_regions(&self.document, PrecisionPolicy::default())
                .map_err(|error| error.to_string())?;
            let ReplayAction::Kernel(command) = action else {
                return Err("the sweep did not resolve to a kernel command".to_owned());
            };
            let input = if staged.operation == SolidOperation::New {
                self.empty_snapshot.clone()
            } else {
                self.solid_target_snapshot(staged.target).ok_or_else(|| {
                    "An add or cut sweep needs a visible body to combine with".to_owned()
                })?
            };
            let (snapshot, report) = self.execute_preview_command(&input, command, "sweep")?;
            Ok(SweepPreview {
                recipe,
                scene: NativeKernel::debug_scene(&snapshot),
                snapshot,
                report,
            })
        })();
        let staged = self
            .staged_sweep
            .as_mut()
            .expect("the staged sweep was present above");
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

    /// Commits the staged sweep, or rewrites the sweep it was reopened on.
    ///
    /// The preview is not built again: every change on the card rebuilt it,
    /// nothing else can change the document while the editor is open, and a
    /// skinned sweep is too costly to build twice for nothing.
    pub(crate) fn commit_staged_sweep(&mut self, editing: Option<FeatureId>) {
        let Some(staged) = self.staged_sweep.clone() else {
            self.pending_operation = None;
            return;
        };
        let Some(preview) = staged.preview.clone() else {
            self.document_status = Some(format!(
                "Sweep not built: {}",
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
            self.apply_sweep_edit(feature, preview.recipe, target);
            return;
        }
        let association = SnapshotAssociation::new(
            preview.report.input_snapshot,
            preview.report.output_snapshot,
            preview.report.semantic_digest,
        );
        let mut next_document = self.document.clone();
        let label = Self::next_document_feature_label(&next_document, FeatureKind::Sweep);
        let [profile_sketch, path_sketch] = preview.recipe.sketches();
        let mut draft = FeatureDraft::new(
            FeatureKind::Sweep,
            label.clone(),
            ReplayAction::SketchSweep(preview.recipe.clone()),
        )
        .with_commit(association)
        .with_input(FeatureInput::Sketch(profile_sketch))
        .with_input(FeatureInput::Sketch(path_sketch));
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
                self.document_status = Some(format!("Sweep history rejected: {error}"));
                return;
            }
        };
        // Both sketches are spent, as an extruded one is: hidden, and back
        // when the sweep is suppressed or undone.
        for sketch in [profile_sketch, path_sketch] {
            if let Err(error) = next_document.auto_hide_sketch_consumed_by(sketch, appended.feature)
            {
                self.document_status =
                    Some(format!("The swept sketch could not be hidden: {error}"));
                return;
            }
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
                kind: ModelBodyKind::Swept,
            });
        }
        if let Some(body) = target {
            if let Some(existing) = self.bodies.iter_mut().find(|entry| entry.id == body) {
                existing.body = displayed.clone();
                existing.last_feature = appended.feature;
                existing.kind = ModelBodyKind::Swept;
            }
            if self.active_body_id() == Some(body) {
                self.displayed = Some(displayed);
                self.model_body_kind = ModelBodyKind::Swept;
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
                kind: ModelBodyKind::Swept,
                visible: true,
            });
            self.displayed = Some(displayed);
            self.model_body_kind = ModelBodyKind::Swept;
        }
        self.body_pivot = self
            .displayed
            .as_ref()
            .and_then(|body| body.report.bounds.map(crate::presentation::bounds_center));
        self.staged_sweep = None;
        self.pending_operation = None;
        self.history_scrub_position = self.document.history_position();
        self.selected_history_feature = Some(appended.feature);
        self.restore_runtime_from_document();
        self.sync_feature_preview_from_document();
        self.last_attempt = Attempt::Accepted { operation: "Sweep" };
        self.document_status = Some(format!(
            "{label} committed · {}",
            operation_label(staged.operation)
        ));
    }

    /// Reopens a committed sweep in its editor, the history rolled back to
    /// just before it.
    pub(crate) fn begin_sweep_edit(&mut self, feature: FeatureId) -> bool {
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
                ReplayAction::SketchSweep(recipe) => Some((
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
            self.document_status = Some("That feature is not a sweep".to_owned());
            return false;
        };
        if !self.move_history_cursor(index) {
            return false;
        }
        let anchors = recipe
            .profile
            .regions
            .iter()
            .filter_map(|region| self.region_anchor(recipe.profile.sketch, region))
            .collect();
        self.staged_sweep = Some(StagedSweep {
            sketch: Some(recipe.profile.sketch),
            regions: recipe.profile.regions.clone(),
            anchors,
            path: Some(recipe.path.clone()),
            orientation: recipe.orientation,
            operation: recipe.operation,
            target: target.or_else(|| self.plane_boolean_target()),
            preview: None,
            issue: None,
        });
        self.pending_operation = Some(PendingOperation::StageSweep {
            editing: Some(feature),
        });
        self.refresh_sweep_preview();
        self.document_status = Some(
            "Editing the sweep · pick the profile or change the path, then confirm".to_owned(),
        );
        true
    }

    fn apply_sweep_edit(
        &mut self,
        feature: FeatureId,
        recipe: SketchSweep,
        target: Option<BodyId>,
    ) {
        self.staged_sweep = None;
        self.pending_operation = None;
        let original_target = self.document.feature(feature).and_then(|node| {
            node.inputs.iter().find_map(|input| match input {
                FeatureInput::Body(body) => Some(*body),
                FeatureInput::Feature(_) | FeatureInput::Sketch(_) => None,
            })
        });
        if original_target != target {
            self.document_status = Some(
                "A sweep keeps the body it builds or changes; make a new sweep for a different one"
                    .to_owned(),
            );
            self.move_history_cursor(self.document.features().len());
            return;
        }
        let mut inputs = recipe
            .sketches()
            .into_iter()
            .map(FeatureInput::Sketch)
            .collect::<Vec<_>>();
        if let Some(body) = target {
            inputs.push(FeatureInput::Body(body));
        }
        match self.document.replace_feature_action_and_inputs(
            feature,
            ReplayAction::SketchSweep(recipe),
            inputs,
        ) {
            Ok(_) => {
                self.move_history_cursor(self.document.features().len());
                self.selected_history_feature = Some(feature);
                if self.rebuild_document_from(feature) {
                    self.activate_body_made_by(feature);
                    self.document_status =
                        Some("Sweep rewritten; everything after it rebuilt".to_owned());
                }
            }
            Err(error) => {
                self.document_status = Some(format!("Sweep edit rejected: {error}"));
                self.move_history_cursor(self.document.features().len());
            }
        }
    }

    /// Abandons the sweep editor. An edit puts the whole model back.
    pub(crate) fn cancel_staged_sweep(&mut self, editing: Option<FeatureId>) {
        self.staged_sweep = None;
        self.pending_operation = None;
        if editing.is_some() {
            self.move_history_cursor(self.document.features().len());
            self.document_status = Some("Sweep edit abandoned".to_owned());
        }
    }

    /// The staged profile's regions, so the model view draws them picked.
    pub(crate) fn sweep_region_selections(&self) -> Vec<viewport::ModelSketchRegionSelection> {
        let Some(staged) = self.staged_sweep.as_ref() else {
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

    /// The SWEEP card: the profile, the path, how the profile is carried,
    /// the operation, and what the kernel made of them.
    pub(crate) fn sweep_feature_controls(&mut self, ui: &mut egui::Ui) {
        let Some(staged) = self.staged_sweep.clone() else {
            return;
        };
        let editing = matches!(
            self.pending_operation,
            Some(PendingOperation::StageSweep { editing: Some(_) })
        );
        let (title, colour) = match (&staged.preview, editing) {
            (Some(_), true) => ("EDITING SWEEP", theme::warn()),
            (Some(_), false) => ("SWEEP PREVIEW", theme::good()),
            (None, _) => ("SWEEP", theme::muted()),
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
                if count == 1 {
                    format!("{name} · 1 region")
                } else {
                    format!("{name} · {count} regions")
                }
            }
        };
        ui.label(RichText::new(format!("Profile · {profile}")).color(theme::text()));
        let choices = self.sweep_path_choices();
        ui.label(RichText::new("Path").small().color(theme::muted()));
        if choices.is_empty() {
            ui.label(
                RichText::new(
                    "Draw the path in another sketch: lines, arcs or splines joined end to end and tangent where they meet",
                )
                .small()
                .color(theme::warn()),
            );
        }
        let mut chosen = None;
        ui.horizontal_wrapped(|ui| {
            for choice in &choices {
                let selected = staged
                    .path
                    .as_ref()
                    .is_some_and(|path| same_curves(path, &choice.path));
                let response = ui
                    .add(egui::Button::new(&choice.label).selected(selected))
                    .on_hover_text("Sweep along this path");
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::Button,
                        true,
                        selected,
                        format!("Sweep path {}", choice.label),
                    )
                });
                if response.clicked() {
                    chosen = Some(choice.path.clone());
                }
            }
        });
        if let Some(path) = chosen {
            self.set_sweep_path(path);
        }
        if let Some(path) = staged.path.as_ref() {
            let response = ui
                .add(egui::Button::new("Reverse path").selected(path.reversed))
                .on_hover_text("Start the sweep from the path's other end");
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::Button,
                    true,
                    path.reversed,
                    "Sweep reverse path",
                )
            });
            if response.clicked() {
                self.reverse_sweep_path();
            }
        }
        let mut orientation = None;
        ui.horizontal(|ui| {
            for candidate in [SweepOrientation::RotationMinimising, SweepOrientation::Fixed] {
                let label = orientation_label(candidate);
                let response = ui
                    .add(egui::Button::new(label).selected(staged.orientation == candidate))
                    .on_hover_text(match candidate {
                        SweepOrientation::RotationMinimising => {
                            "Turn the profile with the path, twisting it as little as possible"
                        }
                        SweepOrientation::Fixed => {
                            "Keep the profile facing the way it was drawn; it only moves along the path"
                        }
                    });
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::Button,
                        true,
                        staged.orientation == candidate,
                        format!("Sweep orientation {label}"),
                    )
                });
                if response.clicked() {
                    orientation = Some(candidate);
                }
            }
        });
        if let Some(orientation) = orientation {
            self.set_sweep_orientation(orientation);
        }
        let can_combine = staged.target.is_some();
        ui.horizontal(|ui| {
            for operation in [
                SolidOperation::New,
                SolidOperation::Add,
                SolidOperation::Cut,
            ] {
                let enabled = (operation == SolidOperation::New || can_combine) && !editing;
                let response = ui.add_enabled(
                    enabled,
                    egui::Button::new(operation_label(operation))
                        .selected(staged.operation == operation),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::selected(
                        egui::WidgetType::Button,
                        enabled,
                        staged.operation == operation,
                        format!("Sweep operation {}", operation_label(operation)),
                    )
                });
                if response.clicked() {
                    self.set_sweep_operation(operation);
                }
            }
        });
        match (&staged.preview, &staged.issue) {
            (Some(preview), _) => {
                let note = preview.departure().map_or_else(
                    || "Exact: the path is straight, or an arc about an axis in the profile's plane".to_owned(),
                    |departure| {
                        format!(
                            "Skinned through copies of the profile · within {} of the true sweep",
                            self.length_unit().format(departure.max(1.0e-9))
                        )
                    },
                );
                ui.label(RichText::new(note).small().color(theme::muted()));
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

    /// Why the staged sweep has no preview, when it has none.
    #[must_use]
    pub fn staged_sweep_issue(&self) -> Option<String> {
        self.staged_sweep
            .as_ref()
            .and_then(|staged| staged.issue.clone())
    }

    /// Whether the staged sweep has a preview the kernel built.
    #[must_use]
    pub fn staged_sweep_has_preview(&self) -> bool {
        self.staged_sweep
            .as_ref()
            .is_some_and(|staged| staged.preview.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        FeaturePreviewKind, SketchGeometry, SketchPlane, SketchPoint, TimelineContextCommand,
    };
    use artificer_model::FeatureOutput;

    const PI: f64 = std::f64::consts::PI;

    fn point(u: f64, v: f64) -> SketchPoint {
        SketchPoint::new(u, v)
    }

    /// A disc of radius 1 at the origin of the XY plane, finished.
    fn disc_on_xy(app: &mut KernelLabApp) -> usize {
        let disc = app
            .sketch
            .stage_geometry(SketchGeometry::circle(point(0.0, 0.0), point(1.0, 0.0)))
            .expect("the disc stages");
        app.commit_sketch_stroke(disc);
        assert!(app.finish_sketch_now(), "{:?}", app.document_status);
        app.enter_model_mode();
        app.sketches.len() - 1
    }

    /// `curves` drawn on the XZ plane, in order, and finished. The sketch's
    /// u runs along world X and its v along world Z.
    fn path_on_xz(app: &mut KernelLabApp, curves: &[SketchGeometry]) -> usize {
        app.open_origin_plane_sketch(SketchPlane::XZ);
        for curve in curves {
            let entity = app.sketch.stage_geometry(*curve).expect("the curve stages");
            app.commit_sketch_stroke(entity);
        }
        assert!(app.finish_sketch_now(), "{:?}", app.document_status);
        app.enter_model_mode();
        app.sketches.len() - 1
    }

    fn preview(app: &KernelLabApp) -> &SweepPreview {
        app.staged_sweep
            .as_ref()
            .and_then(|staged| staged.preview.as_ref())
            .unwrap_or_else(|| panic!("no preview: {:?}", app.staged_sweep_issue()))
    }

    fn assert_close(actual: f64, expected: f64, what: &str) {
        assert!(
            ((actual - expected) / expected).abs() < 1.0e-9,
            "{what}: {actual} should be {expected}"
        );
    }

    fn sweep_feature(app: &KernelLabApp) -> FeatureId {
        app.document
            .features()
            .iter()
            .rev()
            .find(|node| node.kind == FeatureKind::Sweep)
            .expect("a sweep is in the history")
            .id
    }

    fn swept_body(app: &KernelLabApp, feature: FeatureId) -> &WorkbenchBody {
        let body = app
            .document
            .feature(feature)
            .and_then(|node| {
                node.outputs.iter().find_map(|output| match output {
                    FeatureOutput::Body(body) => Some(*body),
                    FeatureOutput::Sketch { .. } => None,
                })
            })
            .expect("the sweep made a body");
        app.bodies
            .iter()
            .find(|candidate| candidate.id == body)
            .expect("the body is on screen")
    }

    /// A disc swept up a leaning line is an oblique cylinder: exact, and by
    /// Cavalieri its volume is the disc's area times the rise, whatever the
    /// lean. Confirmed, it is a feature with its own chip and editor, and
    /// both its sketches are spent.
    #[test]
    fn a_disc_swept_up_a_line_is_an_exact_oblique_cylinder() {
        let mut app = KernelLabApp::default();
        let disc = disc_on_xy(&mut app);
        let path = path_on_xz(
            &mut app,
            &[SketchGeometry::Segment {
                start: point(0.0, 0.0),
                end: point(2.0, 5.0),
            }],
        );
        assert!(
            app.command_availability(crate::commands::ModelCommand::Sweep)
                .is_enabled()
        );
        assert!(app.stage_sweep(), "{:?}", app.document_status);
        // The path sketch was the active one and has no region: the profile
        // waits for a click, but the path is already chosen.
        let choices = app.sweep_path_choices();
        assert_eq!(
            choices
                .iter()
                .map(|choice| choice.label.as_str())
                .collect::<Vec<_>>(),
            vec![format!(
                "Sketch {} path 1 · 1 curve",
                app.sketches[path].ordinal
            )]
        );
        assert!(!app.staged_sweep_has_preview());
        assert!(app.pick_sweep_region(disc, [0.1, 0.1], false));
        assert_eq!(app.sweep_region_selections().len(), 1);
        assert_close(
            preview(&app).snapshot.measures().volume,
            PI * 5.0,
            "Cavalieri",
        );
        assert_eq!(preview(&app).report.rung.as_deref(), Some("sweep/straight"));
        assert!(
            preview(&app).departure().is_none(),
            "a straight sweep is exact"
        );

        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        assert!(app.staged_sweep.is_none(), "the staging is spent");
        let feature = sweep_feature(&app);
        let body = swept_body(&app, feature);
        assert_eq!(body.kind, ModelBodyKind::Swept);
        let measures = body.body.snapshot.measures();
        assert_close(measures.volume, PI * 5.0, "committed");
        let bounds = measures.bounds.expect("bounds");
        assert_close(bounds.max.x, 3.0, "the far end leans with the path");
        assert_close(bounds.max.z, 5.0, "the far end");
        for index in [disc, path] {
            let sketch = app.sketches[index].id.expect("committed");
            assert!(!app.document.sketch(sketch).expect("the sketch").visible);
        }
        assert!(
            app.feature_preview
                .entries
                .iter()
                .any(|entry| entry.kind == FeaturePreviewKind::Sweep && entry.label() == "Sweep 1")
        );
        assert_eq!(
            app.timeline_context_commands(feature),
            vec![
                TimelineContextCommand::EditSweep,
                TimelineContextCommand::Rename,
                TimelineContextCommand::Suppress,
            ]
        );
    }

    /// Round a quarter arc whose axis lies in the disc's plane, a sweep is a
    /// quarter revolve, exact: Pappus gives the disc's area times the length
    /// of the arc. Run the other way the path starts along the disc's own
    /// plane, which is refused by name and builds nothing.
    #[test]
    fn a_disc_swept_round_an_arc_is_an_exact_revolve() {
        let mut app = KernelLabApp::default();
        let disc = disc_on_xy(&mut app);
        // Up from the origin and bending over towards -X, about Y through
        // (-3, 0, 0).
        path_on_xz(
            &mut app,
            &[SketchGeometry::Arc {
                center: point(-3.0, 0.0),
                start: point(0.0, 0.0),
                end: point(-3.0, 3.0),
            }],
        );
        assert!(app.stage_sweep());
        assert!(app.pick_sweep_region(disc, [0.1, 0.1], false));
        assert_close(
            preview(&app).snapshot.measures().volume,
            PI * 3.0 * PI / 2.0,
            "Pappus",
        );
        assert_eq!(preview(&app).report.rung.as_deref(), Some("sweep/revolve"));

        app.reverse_sweep_path();
        assert!(!app.staged_sweep_has_preview());
        let issue = app.staged_sweep_issue().expect("a refusal says why");
        assert!(issue.contains("plane"), "{issue}");
        assert!(app.confirm_pending_operation());
        assert!(
            app.document
                .features()
                .iter()
                .all(|node| node.kind != FeatureKind::Sweep),
            "a refused sweep commits nothing"
        );
        assert!(
            app.document_status
                .as_deref()
                .is_some_and(|status| status.starts_with("Sweep not built")),
            "{:?}",
            app.document_status
        );

        app.reverse_sweep_path();
        assert!(
            app.staged_sweep_has_preview(),
            "{:?}",
            app.staged_sweep_issue()
        );
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        let feature = sweep_feature(&app);
        assert_close(
            swept_body(&app, feature).body.snapshot.measures().volume,
            PI * 3.0 * PI / 2.0,
            "committed",
        );
    }

    /// Reopened, a sweep runs its path the other way and rewrites itself in
    /// place; saved and opened again, it replays from its two sketches.
    #[test]
    fn a_sweep_is_edited_in_place_and_replays_from_its_sketches() {
        let mut app = KernelLabApp::default();
        let disc = disc_on_xy(&mut app);
        path_on_xz(
            &mut app,
            &[SketchGeometry::Segment {
                start: point(0.0, 0.0),
                end: point(0.0, 5.0),
            }],
        );
        assert!(app.stage_sweep());
        assert!(app.pick_sweep_region(disc, [0.1, 0.1], false));
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        let feature = sweep_feature(&app);
        let features = app.document.features().len();

        assert!(app.feature_has_an_editor(feature));
        assert!(app.begin_sweep_edit(feature), "{:?}", app.document_status);
        assert!(matches!(
            app.pending_operation,
            Some(PendingOperation::StageSweep { editing: Some(_) })
        ));
        assert_eq!(app.sweep_region_selections().len(), 1);
        assert!(
            app.staged_sweep_has_preview(),
            "{:?}",
            app.staged_sweep_issue()
        );
        // Run from its top, the path carries the disc downwards from where
        // it lies.
        app.reverse_sweep_path();
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        assert_eq!(
            app.document.features().len(),
            features,
            "rewritten, not added"
        );
        let measures = swept_body(&app, feature).body.snapshot.measures();
        assert_close(measures.volume, PI * 5.0, "edited");
        let bounds = measures.bounds.expect("bounds");
        assert_close(bounds.min.z, -5.0, "downwards");
        assert!(bounds.max.z.abs() < 1.0e-9, "{bounds:?}");

        let json = app.native_document_json().expect("the document saves");
        let hydrated = crate::document_replay::hydrate_document_json_with_options(
            &json,
            crate::document_replay::HydrationOptions::default(),
        )
        .expect("the document replays");
        assert!(hydrated.document.features().iter().any(|node| matches!(
            &node.action,
            ReplayAction::SketchSweep(recipe) if recipe.path.entities.len() == 1
        )));
    }

    /// A sweep needs a profile and a path in another sketch: with fewer than
    /// two sketches the command says so rather than staging nothing.
    #[test]
    fn a_sweep_needs_a_profile_and_a_path() {
        let mut app = KernelLabApp::default();
        assert!(
            !app.command_availability(crate::commands::ModelCommand::Sweep)
                .is_enabled()
        );
        disc_on_xy(&mut app);
        assert!(
            !app.command_availability(crate::commands::ModelCommand::Sweep)
                .is_enabled()
        );
    }
}
