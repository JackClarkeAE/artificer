//! The Loft command: one solid through profiles drawn on different planes
//! (ADR 0051).
//!
//! A loft is staged the way a plane is: an editor opens, the viewport shows
//! what confirming would build, and the contextual card carries the choices.
//! Its sections are regions of committed sketches, picked in the model view
//! in the order the loft runs through them. Every change to the picks or the
//! operation asks the kernel for the result at once, so the preview is the
//! solid the history will hold, or the reason there is none.

use artificer_kernel::{CancellationToken, DebugScene, NativeKernel, Snapshot};
use artificer_model::{
    BodyId, FeatureDraft, FeatureId, FeatureInput, FeatureKind, OutputDraft, ReplayAction,
    SketchId, SketchLoft, SketchLoftSection, SnapshotAssociation, sketch_region::sketch_region_at,
};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, DiagnosticSeverity, ExecuteRequest, KernelCommand, LoftOperation,
    OperationReport, PrecisionPolicy, RequestId,
};
use artificer_sketch::{ArrangementLimits, RegionSignature, build_arrangement};
use eframe::egui;
use egui::RichText;

use crate::{
    ArchivedBody, Attempt, DisplayedBody, KernelLabApp, ModelBodyKind, PendingOperation,
    WorkbenchBody, status_line, theme, viewport,
};

/// The viewport key a new-body loft preview is drawn under. No document body
/// can have it: body ids are allocated upward from one.
pub(crate) const LOFT_PREVIEW_BODY_KEY: u64 = u64::MAX - 0x10F7;

/// The colour a new-body loft preview is shaded in, so it reads as a proposal
/// rather than as a body that is already there.
pub(crate) const LOFT_PREVIEW_TINT: egui::Color32 = egui::Color32::from_rgb(96, 164, 232);

/// One section of a loft in its editor: regions of one committed sketch.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StagedLoftSection {
    pub(crate) sketch: SketchId,
    pub(crate) regions: Vec<RegionSignature>,
    /// Where each region is anchored in its sketch, which is how the model
    /// view names it: the pick highlight is drawn from these.
    pub(crate) anchors: Vec<[f64; 2]>,
}

/// What confirming the staged loft would build.
#[derive(Clone)]
pub(crate) struct LoftPreview {
    pub(crate) recipe: SketchLoft,
    pub(crate) snapshot: Snapshot,
    pub(crate) report: OperationReport,
    pub(crate) scene: DebugScene,
}

impl LoftPreview {
    /// Whether the kernel answered with an exact solid. A loft combined with
    /// a body through the faceted tier says so in its warnings.
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

/// A loft in its editor.
#[derive(Clone)]
pub(crate) struct StagedLoft {
    pub(crate) sections: Vec<StagedLoftSection>,
    pub(crate) operation: LoftOperation,
    /// The body an add or a cut changes.
    pub(crate) target: Option<BodyId>,
    pub(crate) preview: Option<LoftPreview>,
    /// Why there is no preview, when there is not.
    pub(crate) issue: Option<String>,
}

impl StagedLoft {
    fn recipe(&self) -> Result<SketchLoft, String> {
        SketchLoft::new(
            self.sections
                .iter()
                .map(|section| SketchLoftSection::new(section.sketch, section.regions.clone()))
                .collect(),
            self.operation,
        )
        .map_err(|error| error.to_string())
    }
}

/// A human name for a loft operation, as the card and the status line say it.
const fn operation_label(operation: LoftOperation) -> &'static str {
    match operation {
        LoftOperation::New => "New body",
        LoftOperation::Add => "Add",
        LoftOperation::Cut => "Cut",
    }
}

impl KernelLabApp {
    /// Opens the loft editor. A committed-sketch region that is already
    /// picked becomes the first section, so Loft works in either order (ADR
    /// 0041).
    pub(crate) fn stage_loft(&mut self) -> bool {
        if self.pending_operation.is_some() || !self.history_is_at_end() {
            return false;
        }
        let sections = self.loft_sections_from_selection();
        let target = self.plane_boolean_target();
        self.staged_loft = Some(StagedLoft {
            sections,
            operation: LoftOperation::New,
            target,
            preview: None,
            issue: None,
        });
        self.pending_operation = Some(PendingOperation::StageLoft { editing: None });
        self.refresh_loft_preview();
        self.document_status = Some(
            "Loft · click a profile in each sketch, in the order the loft runs through them"
                .to_owned(),
        );
        true
    }

    /// The picked regions of the active committed sketch, as a first section.
    fn loft_sections_from_selection(&mut self) -> Vec<StagedLoftSection> {
        let Some(sketch) = self
            .active_sketch_index
            .and_then(|index| self.sketches.get(index))
            .filter(|sketch| sketch.finished && !sketch.consumed)
            .and_then(|sketch| sketch.id)
        else {
            return Vec::new();
        };
        let anchors = self.sketch.selected_region_canonical_anchors();
        let precision = PrecisionPolicy::default();
        let (regions, anchors): (Vec<_>, Vec<_>) = anchors
            .into_iter()
            .filter_map(|anchor| {
                sketch_region_at(&self.document, sketch, anchor, precision)
                    .map(|region| (region, anchor))
            })
            .unzip();
        if regions.is_empty() {
            return Vec::new();
        }
        vec![StagedLoftSection {
            sketch,
            regions,
            anchors,
        }]
    }

    /// Whether a click on a sketch region in the model view is a loft pick.
    pub(crate) fn loft_pick_active(&self) -> bool {
        matches!(
            self.pending_operation,
            Some(PendingOperation::StageLoft { .. })
        ) && self.staged_loft.is_some()
    }

    /// A region picked in the model view while the loft editor is open.
    ///
    /// A region of a sketch that is not a section yet adds a section at the
    /// end. A region of a sketch that already is one replaces that section's
    /// regions, or, with `additive`, is added to them or taken out of them.
    pub fn pick_loft_region(
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
            self.document_status = Some("Finish the sketch before lofting through it".to_owned());
            return false;
        };
        let Some(region) =
            sketch_region_at(&self.document, sketch, anchor, PrecisionPolicy::default())
        else {
            return false;
        };
        let Some(staged) = self.staged_loft.as_mut() else {
            return false;
        };
        match staged
            .sections
            .iter_mut()
            .find(|section| section.sketch == sketch)
        {
            Some(section) => {
                if let Some(index) = section
                    .regions
                    .iter()
                    .position(|existing| *existing == region)
                {
                    if additive && section.regions.len() > 1 {
                        section.regions.remove(index);
                        section.anchors.remove(index);
                    }
                } else if additive {
                    section.regions.push(region);
                    section.anchors.push(anchor);
                } else {
                    section.regions = vec![region];
                    section.anchors = vec![anchor];
                }
            }
            None => staged.sections.push(StagedLoftSection {
                sketch,
                regions: vec![region],
                anchors: vec![anchor],
            }),
        }
        self.refresh_loft_preview();
        true
    }

    /// Takes one section out of the staged loft.
    pub fn remove_loft_section(&mut self, index: usize) {
        if let Some(staged) = self.staged_loft.as_mut()
            && index < staged.sections.len()
        {
            staged.sections.remove(index);
            self.refresh_loft_preview();
        }
    }

    /// Moves one section a place earlier in the order the loft runs through.
    pub fn move_loft_section_earlier(&mut self, index: usize) {
        if let Some(staged) = self.staged_loft.as_mut()
            && index > 0
            && index < staged.sections.len()
        {
            staged.sections.swap(index - 1, index);
            self.refresh_loft_preview();
        }
    }

    /// Chooses what the staged loft does: a body of its own, or joined to or
    /// taken from the body it is staged over.
    pub fn set_loft_operation(&mut self, operation: LoftOperation) {
        if let Some(staged) = self.staged_loft.as_mut()
            && staged.operation != operation
        {
            staged.operation = operation;
            self.refresh_loft_preview();
        }
    }

    /// The body an add or cut loft combines with, when one is staged.
    fn loft_target_snapshot(&self, target: Option<BodyId>) -> Option<Snapshot> {
        let body = target?;
        self.bodies
            .iter()
            .find(|candidate| candidate.id == body)
            .map(|candidate| candidate.body.snapshot.clone())
    }

    /// Asks the kernel for the staged loft as its picks now stand.
    fn refresh_loft_preview(&mut self) {
        let Some(staged) = self.staged_loft.as_ref() else {
            return;
        };
        let outcome = (|| {
            if staged.sections.len() < 2 {
                return Err(match staged.sections.len() {
                    0 => "Click a profile in the sketch the loft starts from".to_owned(),
                    _ => "Click a profile in a sketch on another plane".to_owned(),
                });
            }
            let recipe = staged.recipe()?;
            let action = ReplayAction::SketchLoft(recipe.clone())
                .resolve_sketch_regions(&self.document, PrecisionPolicy::default())
                .map_err(|error| error.to_string())?;
            let ReplayAction::Kernel(command) = action else {
                return Err("the loft did not resolve to a kernel command".to_owned());
            };
            let input = if staged.operation == LoftOperation::New {
                self.empty_snapshot.clone()
            } else {
                self.loft_target_snapshot(staged.target).ok_or_else(|| {
                    "An add or cut loft needs a visible body to combine with".to_owned()
                })?
            };
            let (snapshot, report) = self.execute_loft_command(&input, command)?;
            Ok(LoftPreview {
                recipe,
                scene: NativeKernel::debug_scene(&snapshot),
                snapshot,
                report,
            })
        })();
        let staged = self
            .staged_loft
            .as_mut()
            .expect("the staged loft was present above");
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

    fn execute_loft_command(
        &self,
        input: &Snapshot,
        command: KernelCommand,
    ) -> Result<(Snapshot, OperationReport), String> {
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new(format!("workbench-{}-loft-preview", self.request_serial)),
            expected_snapshot: input.id(),
            precision: input.precision_policy().unwrap_or_default(),
            command,
        };
        NativeKernel::execute(input, &request, &CancellationToken::new())
            .map(|outcome| (outcome.snapshot, outcome.report))
            .map_err(|error| {
                // The first named diagnostic is the reason a person can act
                // on; the error's own message is the kernel's summary.
                error
                    .diagnostics
                    .iter()
                    .find(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
                    .map_or_else(
                        || error.to_string(),
                        |diagnostic| {
                            format!("{} ({})", diagnostic.message, diagnostic.code.as_str())
                        },
                    )
            })
    }

    /// Commits the staged loft, or rewrites the loft it was reopened on.
    pub(crate) fn commit_staged_loft(&mut self, editing: Option<FeatureId>) {
        self.refresh_loft_preview();
        let Some(staged) = self.staged_loft.clone() else {
            self.pending_operation = None;
            return;
        };
        let Some(preview) = staged.preview.clone() else {
            self.document_status = Some(format!(
                "Loft not built: {}",
                staged
                    .issue
                    .unwrap_or_else(|| "it has nothing to build".to_owned())
            ));
            return;
        };
        let target = staged
            .target
            .filter(|_| staged.operation != LoftOperation::New);
        if let Some(feature) = editing {
            self.apply_loft_edit(feature, preview.recipe, target);
            return;
        }
        let association = SnapshotAssociation::new(
            preview.report.input_snapshot,
            preview.report.output_snapshot,
            preview.report.semantic_digest,
        );
        let mut next_document = self.document.clone();
        let label = Self::next_document_feature_label(&next_document, FeatureKind::Loft);
        let sketches = preview.recipe.sketches().collect::<Vec<_>>();
        let mut draft = FeatureDraft::new(
            FeatureKind::Loft,
            label.clone(),
            ReplayAction::SketchLoft(preview.recipe.clone()),
        )
        .with_commit(association);
        for sketch in &sketches {
            draft = draft.with_input(FeatureInput::Sketch(*sketch));
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
                self.document_status = Some(format!("Loft history rejected: {error}"));
                return;
            }
        };
        // The sections are spent the way an extruded profile is: hidden, and
        // back when the loft is suppressed or undone.
        for sketch in &sketches {
            if let Err(error) =
                next_document.auto_hide_sketch_consumed_by(*sketch, appended.feature)
            {
                self.document_status = Some(format!("Loft sections could not be hidden: {error}"));
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
                kind: ModelBodyKind::Lofted,
            });
        }
        if let Some(body) = target {
            if let Some(existing) = self.bodies.iter_mut().find(|entry| entry.id == body) {
                existing.body = displayed.clone();
                existing.last_feature = appended.feature;
                existing.kind = ModelBodyKind::Lofted;
            }
            if self.active_body_id() == Some(body) {
                self.displayed = Some(displayed);
                self.model_body_kind = ModelBodyKind::Lofted;
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
                kind: ModelBodyKind::Lofted,
                visible: true,
            });
            self.displayed = Some(displayed);
            self.model_body_kind = ModelBodyKind::Lofted;
        }
        self.body_pivot = self
            .displayed
            .as_ref()
            .and_then(|body| body.report.bounds.map(crate::presentation::bounds_center));
        self.staged_loft = None;
        self.pending_operation = None;
        self.history_scrub_position = self.document.history_position();
        self.selected_history_feature = Some(appended.feature);
        self.restore_runtime_from_document();
        self.sync_feature_preview_from_document();
        self.last_attempt = Attempt::Accepted { operation: "Loft" };
        self.document_status = Some(format!(
            "{label} committed · {} through {} sections",
            operation_label(staged.operation),
            sketches.len()
        ));
    }

    /// Reopens a committed loft in its editor.
    ///
    /// As with a plane or an extrusion (ADR 0036), the history rolls back to
    /// just before the loft, so what is on screen is what it was built from,
    /// and confirming rewrites the loft in its slot and replays what follows.
    pub(crate) fn begin_loft_edit(&mut self, feature: FeatureId) -> bool {
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
                ReplayAction::SketchLoft(recipe) => Some((
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
            self.document_status = Some("That feature is not a loft".to_owned());
            return false;
        };
        if !self.move_history_cursor(index) {
            return false;
        }
        let sections = recipe
            .sections
            .iter()
            .map(|section| StagedLoftSection {
                sketch: section.sketch,
                anchors: section
                    .regions
                    .iter()
                    .filter_map(|region| self.loft_region_anchor(section.sketch, region))
                    .collect(),
                regions: section.regions.clone(),
            })
            .collect();
        self.staged_loft = Some(StagedLoft {
            sections,
            operation: recipe.operation,
            target: target.or_else(|| self.plane_boolean_target()),
            preview: None,
            issue: None,
        });
        self.pending_operation = Some(PendingOperation::StageLoft {
            editing: Some(feature),
        });
        self.refresh_loft_preview();
        self.document_status = Some(
            "Editing the loft · pick profiles or change the operation, then confirm".to_owned(),
        );
        true
    }

    /// Where a region of a committed sketch is anchored in the model view.
    fn loft_region_anchor(&self, sketch: SketchId, region: &RegionSignature) -> Option<[f64; 2]> {
        let record = self.document.sketch(sketch)?;
        let payload = self
            .document
            .sketch_payload(sketch, record.geometry_revision)?;
        let inputs = payload.authoring()?.arrangement_inputs().ok()?;
        let precision = PrecisionPolicy::default();
        let arrangement = build_arrangement(&inputs, &precision, ArrangementLimits::default());
        let cell = arrangement
            .cells
            .iter()
            .find(|cell| cell.signature == *region)?;
        let anchor = arrangement.cell_interior_sample(cell, &precision)?;
        Some([anchor.u, anchor.v])
    }

    fn apply_loft_edit(&mut self, feature: FeatureId, recipe: SketchLoft, target: Option<BodyId>) {
        self.staged_loft = None;
        self.pending_operation = None;
        let mut inputs = recipe
            .sketches()
            .map(FeatureInput::Sketch)
            .collect::<Vec<_>>();
        // The body a loft changes is its branch, which an edit keeps: turning
        // a new body into a cut, or a cut into a new body, is a different
        // feature and is refused rather than rewritten under the same name.
        let original_target = self.document.feature(feature).and_then(|node| {
            node.inputs.iter().find_map(|input| match input {
                FeatureInput::Body(body) => Some(*body),
                FeatureInput::Feature(_) | FeatureInput::Sketch(_) => None,
            })
        });
        if original_target != target {
            self.document_status = Some(
                "A loft keeps the body it builds or changes; make a new loft for a different one"
                    .to_owned(),
            );
            self.move_history_cursor(self.document.features().len());
            return;
        }
        if let Some(body) = target {
            inputs.push(FeatureInput::Body(body));
        }
        match self.document.replace_feature_action_and_inputs(
            feature,
            ReplayAction::SketchLoft(recipe),
            inputs,
        ) {
            Ok(_) => {
                self.move_history_cursor(self.document.features().len());
                self.selected_history_feature = Some(feature);
                if self.rebuild_document_from(feature) {
                    self.document_status =
                        Some("Loft rewritten; everything after it rebuilt".to_owned());
                }
            }
            Err(error) => {
                self.document_status = Some(format!("Loft edit rejected: {error}"));
                self.move_history_cursor(self.document.features().len());
            }
        }
    }

    /// Abandons the loft editor. An edit puts the whole model back.
    pub(crate) fn cancel_staged_loft(&mut self, editing: Option<FeatureId>) {
        self.staged_loft = None;
        self.pending_operation = None;
        if editing.is_some() {
            self.move_history_cursor(self.document.features().len());
            self.document_status = Some("Loft edit abandoned".to_owned());
        }
    }

    /// The staged sections' regions, so the model view draws them picked.
    pub(crate) fn loft_region_selections(&self) -> Vec<viewport::ModelSketchRegionSelection> {
        let Some(staged) = self.staged_loft.as_ref() else {
            return Vec::new();
        };
        staged
            .sections
            .iter()
            .filter_map(|section| {
                let sketch_index = self
                    .sketches
                    .iter()
                    .position(|sketch| sketch.id == Some(section.sketch))?;
                Some(section.anchors.iter().map(move |anchor| {
                    viewport::ModelSketchRegionSelection {
                        sketch_index,
                        anchor: *anchor,
                    }
                }))
            })
            .flatten()
            .collect()
    }

    fn sketch_ordinal(&self, sketch: SketchId) -> Option<u32> {
        self.sketches
            .iter()
            .find(|candidate| candidate.id == Some(sketch))
            .map(|candidate| candidate.ordinal)
    }

    /// The LOFT card: the sections in order, the operation, and what the
    /// kernel made of them.
    pub(crate) fn loft_controls(&mut self, ui: &mut egui::Ui) {
        let Some(staged) = self.staged_loft.clone() else {
            return;
        };
        let editing = matches!(
            self.pending_operation,
            Some(PendingOperation::StageLoft { editing: Some(_) })
        );
        let (title, colour) = match (&staged.preview, editing) {
            (Some(_), true) => ("EDITING LOFT", theme::warn()),
            (Some(preview), false) if preview.is_exact() => ("LOFT PREVIEW", theme::good()),
            (Some(_), false) => ("LOFT PREVIEW · FACETED", theme::warn()),
            (None, _) => ("LOFT", theme::muted()),
        };
        status_line(ui, title, colour);
        let mut remove = None;
        let mut earlier = None;
        for (index, section) in staged.sections.iter().enumerate() {
            ui.horizontal(|ui| {
                let name = self.sketch_ordinal(section.sketch).map_or_else(
                    || "Sketch".to_owned(),
                    |ordinal| format!("Sketch {ordinal}"),
                );
                let regions = match section.regions.len() {
                    1 => "1 region".to_owned(),
                    count => format!("{count} regions"),
                };
                ui.label(
                    RichText::new(format!("{} · {name} · {regions}", index + 1))
                        .color(theme::text()),
                );
                let response = ui
                    .small_button("×")
                    .on_hover_text("Take this section out of the loft");
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        format!("Remove loft section {}", index + 1),
                    )
                });
                if response.clicked() {
                    remove = Some(index);
                }
                if index > 0 {
                    let response = ui
                        .small_button("Earlier")
                        .on_hover_text("Run through this section one place earlier");
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            true,
                            format!("Move loft section {} earlier", index + 1),
                        )
                    });
                    if response.clicked() {
                        earlier = Some(index);
                    }
                }
            });
        }
        ui.label(
            RichText::new(if staged.sections.len() < 2 {
                "Click a profile in each sketch, in order"
            } else {
                "Click a profile in another sketch to add a section · Shift-click adds a region"
            })
            .small()
            .color(theme::muted()),
        );
        let can_combine = staged.target.is_some();
        ui.horizontal(|ui| {
            for operation in [LoftOperation::New, LoftOperation::Add, LoftOperation::Cut] {
                let enabled = operation == LoftOperation::New || can_combine;
                let response = ui
                    .add_enabled(
                        enabled && !editing,
                        egui::Button::new(operation_label(operation))
                            .selected(staged.operation == operation),
                    )
                    .on_hover_text(match operation {
                        LoftOperation::New => "Build the loft as a body of its own",
                        LoftOperation::Add => "Join the loft to the active body",
                        LoftOperation::Cut => "Take the loft away from the active body",
                    });
                if response.clicked() {
                    self.set_loft_operation(operation);
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
                        "Exact: every wall is a plane, a cylinder, a cone or a ruled surface"
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
        if let Some(index) = remove {
            self.remove_loft_section(index);
        } else if let Some(index) = earlier {
            self.move_loft_section_earlier(index);
        }
    }

    /// How many sections the staged loft has, for tests and the status line.
    #[must_use]
    pub fn staged_loft_section_count(&self) -> Option<usize> {
        self.staged_loft
            .as_ref()
            .map(|staged| staged.sections.len())
    }

    /// Why the staged loft has no preview, when it has none.
    #[must_use]
    pub fn staged_loft_issue(&self) -> Option<String> {
        self.staged_loft
            .as_ref()
            .and_then(|staged| staged.issue.clone())
    }

    /// Whether the staged loft has a preview the kernel built.
    #[must_use]
    pub fn staged_loft_has_preview(&self) -> bool {
        self.staged_loft
            .as_ref()
            .is_some_and(|staged| staged.preview.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ConstructionPlane, FeaturePreviewKind, PendingPlaneSketch, SketchGeometry, SketchPlane,
        SketchPoint, TimelineContextCommand,
    };
    use artificer_model::{FeatureOutput, SketchSupportRecipe};

    fn point(u: f64, v: f64) -> SketchPoint {
        SketchPoint::new(u, v)
    }

    /// Commits `geometry` on the canvas as it stands and finishes the sketch.
    fn finish_sketch_with(app: &mut KernelLabApp, geometry: SketchGeometry) -> usize {
        app.sketch
            .stage_geometry(geometry)
            .expect("the geometry stages");
        app.sketch.commit_pending().expect("the geometry commits");
        app.sketch_revision = app.sketch_revision.saturating_add(1);
        app.feature_preview
            .commit_sketch_revision(app.sketch_revision);
        app.stage_finish_sketch();
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        app.enter_model_mode();
        app.sketches.len() - 1
    }

    /// An XY plane at `offset`, committed.
    fn plane_at(app: &mut KernelLabApp, offset: f64) -> ConstructionPlane {
        app.clear_model_entity_selection();
        app.selected_construction_plane = None;
        app.selected_origin_plane = SketchPlane::XY;
        app.stage_construction_plane();
        app.set_staged_plane_offset(offset);
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        app.construction_planes
            .last()
            .cloned()
            .expect("the plane was committed")
    }

    fn sketch_on_plane(
        app: &mut KernelLabApp,
        plane: &ConstructionPlane,
        geometry: SketchGeometry,
    ) -> usize {
        app.selected_construction_plane = Some(plane.id);
        app.begin_construction_plane_sketch(plane.id);
        if let Some(PendingPlaneSketch::Construction(id)) = app.pending_plane_sketch.take() {
            app.open_construction_plane_sketch(id);
        }
        finish_sketch_with(app, geometry)
    }

    /// A 4 × 4 square on XY and a circle of radius 1 on a plane 10 above it.
    fn square_and_circle() -> (KernelLabApp, ConstructionPlane, usize, usize) {
        let mut app = KernelLabApp::default();
        let square = finish_sketch_with(
            &mut app,
            SketchGeometry::rectangle(point(-2.0, -2.0), point(2.0, 2.0)),
        );
        let plane = plane_at(&mut app, 10.0);
        let circle = sketch_on_plane(
            &mut app,
            &plane,
            SketchGeometry::circle(point(0.0, 0.0), point(1.0, 0.0)),
        );
        (app, plane, square, circle)
    }

    fn loft_feature(app: &KernelLabApp) -> FeatureId {
        app.document
            .features()
            .iter()
            .rev()
            .find(|node| node.kind == FeatureKind::Loft)
            .expect("a loft is in the history")
            .id
    }

    fn loft_body(app: &KernelLabApp, feature: FeatureId) -> &WorkbenchBody {
        let body = app
            .document
            .feature(feature)
            .and_then(|node| {
                node.outputs.iter().find_map(|output| match output {
                    FeatureOutput::Body(body) => Some(*body),
                    FeatureOutput::Sketch { .. } => None,
                })
            })
            .expect("the loft made a body");
        app.bodies
            .iter()
            .find(|candidate| candidate.id == body)
            .expect("the body is on screen")
    }

    /// The frustum-like solid between a 4 × 4 square and a unit circle ten
    /// above it: bounded by both, and smaller than the prism on the square.
    fn assert_square_to_circle(body: &WorkbenchBody, top: f64) {
        let measures = body.body.snapshot.measures();
        let bounds = measures.bounds.expect("the loft has bounds");
        assert!(bounds.min.z.abs() < 1.0e-6, "{bounds:?}");
        assert!((bounds.max.z - top).abs() < 1.0e-6, "{bounds:?}");
        let volume = measures.volume;
        assert!(volume > 0.0 && volume < 16.0 * top, "volume {volume}");
    }

    #[test]
    fn a_loft_runs_from_a_square_to_a_circle_on_another_plane() {
        let (mut app, _, square, circle) = square_and_circle();
        assert!(app.stage_loft());
        assert_eq!(app.staged_loft_section_count(), Some(0));
        assert!(app.staged_loft_issue().is_some(), "nothing picked yet");
        assert!(app.pick_loft_region(square, [0.5, 0.5], false));
        assert!(!app.staged_loft_has_preview(), "one section is not a loft");
        assert!(app.pick_loft_region(circle, [0.1, 0.1], false));
        assert_eq!(app.staged_loft_section_count(), Some(2));
        assert!(
            app.staged_loft_has_preview(),
            "{:?}",
            app.staged_loft_issue()
        );
        assert_eq!(app.loft_region_selections().len(), 2);
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);

        let loft = loft_feature(&app);
        assert_square_to_circle(loft_body(&app, loft), 10.0);
        // Its sections are spent the way an extruded profile is.
        assert!(app.sketches.iter().filter(|sketch| sketch.consumed).count() >= 2);
        // It has a chip of its own in the history, named by its feature, and
        // the chip offers the editor, a name and suppression.
        assert!(
            app.feature_preview
                .entries
                .iter()
                .any(|entry| entry.kind == FeaturePreviewKind::Loft && entry.label() == "Loft 1")
        );
        assert_eq!(
            app.timeline_context_commands(loft),
            vec![
                TimelineContextCommand::EditLoft,
                TimelineContextCommand::Rename,
                TimelineContextCommand::Suppress,
            ]
        );
    }

    /// The loft names its sketches, and the circle's sketch names its plane,
    /// so moving the plane from the history moves the top of the loft.
    #[test]
    fn a_loft_follows_the_plane_its_section_is_drawn_on() {
        let (mut app, plane, square, circle) = square_and_circle();
        assert!(app.stage_loft());
        app.pick_loft_region(square, [0.5, 0.5], false);
        app.pick_loft_region(circle, [0.1, 0.1], false);
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        let loft = loft_feature(&app);
        let circle_sketch = app.sketches[circle].id.expect("committed");
        let payload = app
            .document
            .sketch_payload(
                circle_sketch,
                app.document
                    .sketch(circle_sketch)
                    .expect("in the document")
                    .geometry_revision,
            )
            .expect("a payload");
        assert_eq!(
            payload.support,
            SketchSupportRecipe::DatumPlane {
                plane: plane.feature
            }
        );

        assert!(app.begin_plane_edit(plane.feature));
        app.set_staged_plane_offset(25.0);
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        assert_square_to_circle(loft_body(&app, loft), 25.0);
    }

    #[test]
    fn a_loft_is_reopened_in_its_editor_and_rewritten_in_place() {
        let (mut app, _, square, circle) = square_and_circle();
        assert!(app.stage_loft());
        app.pick_loft_region(square, [0.5, 0.5], false);
        app.pick_loft_region(circle, [0.1, 0.1], false);
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        let loft = loft_feature(&app);
        let features = app.document.features().len();

        assert!(app.begin_loft_edit(loft));
        assert!(matches!(
            app.pending_operation,
            Some(PendingOperation::StageLoft { editing: Some(_) })
        ));
        assert_eq!(app.staged_loft_section_count(), Some(2));
        assert!(
            app.staged_loft_has_preview(),
            "{:?}",
            app.staged_loft_issue()
        );
        // The editor highlights the sections it reopened on.
        assert_eq!(app.loft_region_selections().len(), 2);
        // Running the other way round builds the same solid upside down.
        app.move_loft_section_earlier(1);
        assert!(
            app.staged_loft_has_preview(),
            "{:?}",
            app.staged_loft_issue()
        );
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        assert_eq!(
            app.document.features().len(),
            features,
            "the edit rewrites the loft rather than adding one"
        );
        assert_eq!(app.document.history_position(), features);
        let ReplayAction::SketchLoft(recipe) =
            &app.document.feature(loft).expect("still there").action
        else {
            panic!("still a loft");
        };
        assert_eq!(
            recipe.sketches().collect::<Vec<_>>(),
            vec![
                app.sketches[circle].id.expect("committed"),
                app.sketches[square].id.expect("committed"),
            ]
        );
        assert_square_to_circle(loft_body(&app, loft), 10.0);
    }

    /// A saved loft replays on load: its sections come back from their
    /// sketches, the circle on its plane, and the solid is the same one.
    #[test]
    fn a_loft_survives_saving_and_reopening() {
        let (mut app, _, square, circle) = square_and_circle();
        assert!(app.stage_loft());
        app.pick_loft_region(square, [0.5, 0.5], false);
        app.pick_loft_region(circle, [0.1, 0.1], false);
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        let loft = loft_feature(&app);
        let digest = loft_body(&app, loft).body.snapshot.semantic_digest();

        let json = app.workspace_document_json().expect("the workspace saves");
        let mut restored = KernelLabApp::default();
        restored
            .load_workspace_json(&json)
            .expect("the workspace reopens");
        let restored_loft = loft_feature(&restored);
        assert_eq!(restored_loft, loft);
        let body = loft_body(&restored, restored_loft);
        assert_eq!(body.body.snapshot.semantic_digest(), digest);
        assert_square_to_circle(body, 10.0);
    }

    /// Through three sections the loft is smooth (ADR 0050): a square up to
    /// a circle and out to a smaller square, one body whose walls carry on
    /// through the middle section rather than creasing at it.
    #[test]
    fn a_loft_runs_smoothly_through_three_sections() {
        let (mut app, _, square, circle) = square_and_circle();
        let upper = plane_at(&mut app, 20.0);
        let top = sketch_on_plane(
            &mut app,
            &upper,
            SketchGeometry::rectangle(point(-1.0, -1.0), point(1.0, 1.0)),
        );
        assert!(app.stage_loft());
        app.pick_loft_region(square, [0.5, 0.5], false);
        app.pick_loft_region(circle, [0.1, 0.1], false);
        app.pick_loft_region(top, [0.1, 0.1], false);
        assert_eq!(app.staged_loft_section_count(), Some(3));
        assert!(
            app.staged_loft_has_preview(),
            "{:?}",
            app.staged_loft_issue()
        );
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        let loft = loft_feature(&app);
        let body = loft_body(&app, loft);
        let measures = body.body.snapshot.measures();
        let bounds = measures.bounds.expect("the loft has bounds");
        assert!(bounds.min.z.abs() < 1.0e-6, "{bounds:?}");
        assert!((bounds.max.z - 20.0).abs() < 1.0e-6, "{bounds:?}");
        // Bounded by the prism on the largest section.
        assert!(
            measures.volume > 0.0 && measures.volume < 16.0 * 20.0,
            "volume {}",
            measures.volume
        );
    }

    /// Two sections in one plane cannot be lofted. The kernel refuses by
    /// name, the card says why, and confirming builds nothing.
    #[test]
    fn a_loft_between_two_sketches_in_one_plane_is_refused_by_name() {
        let mut app = KernelLabApp::default();
        let first = finish_sketch_with(
            &mut app,
            SketchGeometry::rectangle(point(-2.0, -2.0), point(2.0, 2.0)),
        );
        let plane = plane_at(&mut app, 0.0);
        let second = sketch_on_plane(
            &mut app,
            &plane,
            SketchGeometry::circle(point(0.0, 0.0), point(1.0, 0.0)),
        );
        let features = app.document.features().len();
        assert!(app.stage_loft());
        app.pick_loft_region(first, [0.5, 0.5], false);
        app.pick_loft_region(second, [0.1, 0.1], false);
        assert!(!app.staged_loft_has_preview());
        let issue = app.staged_loft_issue().expect("a reason");
        assert!(issue.contains("LOFT_SECTIONS_COPLANAR"), "{issue}");
        app.confirm_pending_operation();
        assert_eq!(app.document.features().len(), features);
        assert!(app.staged_loft.is_some(), "the editor stays open to fix it");
    }

    /// Cut runs the loft out of the body it is staged over.
    #[test]
    fn a_loft_cuts_the_active_body() {
        let (mut app, _, square, circle) = square_and_circle();
        let target = app.active_body_id().expect("the base body is active");
        let before = app
            .bodies
            .iter()
            .find(|body| body.id == target)
            .map(|body| body.body.snapshot.measures().volume)
            .expect("the base body has a volume");
        assert!(app.stage_loft());
        app.pick_loft_region(square, [0.5, 0.5], false);
        app.pick_loft_region(circle, [0.1, 0.1], false);
        app.set_loft_operation(LoftOperation::Cut);
        assert!(
            app.staged_loft_has_preview(),
            "{:?}",
            app.staged_loft_issue()
        );
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        let after = app
            .bodies
            .iter()
            .find(|body| body.id == target)
            .map(|body| body.body.snapshot.measures().volume)
            .expect("the cut body has a volume");
        assert!(after < before, "before {before}, after {after}");
        let loft = loft_feature(&app);
        assert!(
            app.document
                .feature(loft)
                .expect("committed")
                .inputs
                .contains(&FeatureInput::Body(target))
        );
    }
}
