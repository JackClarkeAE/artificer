//! Construction axes: lines a revolve can turn about (ADR 0055).
//!
//! An axis is staged the way a plane is (ADR 0048): from what is picked — a
//! straight edge, a curved face whose axis it takes, or two flat faces or a
//! flat face and a construction plane whose meeting it is — and committed as
//! a history feature with a chip of its own. Its recipe names that base, so a
//! rebuild finds it again where the model now puts it, and a revolve about
//! it follows.

use artificer_kernel::{NativeKernel, Snapshot};
use artificer_model::{
    BodyId, DatumAxisBase, DatumAxisError, DatumAxisPlane, DatumAxisRecipe, DatumAxisResolver,
    DatumFaceGeometry, DatumFaceRef, DatumPlaneError, DatumPlaneResolver, FeatureDraft, FeatureId,
    FeatureInput, FeatureKind, ReplayAction, ResolvedDatumAxis, ResolvedDatumPlane,
    SnapshotAssociation, datum_axis::planes_meet, persistent::PersistentRef,
};
use artificer_protocol::{EntityRef, Point3, Vector3};
use eframe::egui;
use egui::RichText;

use crate::{
    KernelLabApp, ModelPlaneResolver, PendingOperation, PersistentMissingOrAmbiguous,
    datum_face_geometry, status_line, theme, viewport,
};

/// A committed construction axis, as the viewport and the revolve card see
/// it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConstructionAxis {
    pub(crate) name: String,
    pub(crate) feature: FeatureId,
    pub(crate) line: ResolvedDatumAxis,
    pub(crate) visible: bool,
    pub(crate) description: String,
    /// The base did not resolve on the last rebuild; the axis stands where
    /// it last was.
    pub(crate) stale: bool,
}

/// One of the planes a staged axis is the meeting of.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StagedAxisPlane {
    Face { body: BodyId, face: EntityRef },
    Plane(FeatureId),
}

/// What a staged axis is made from, as picked.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum StagedAxisBase {
    Edge {
        body: BodyId,
        edge: EntityRef,
    },
    Face {
        body: BodyId,
        face: EntityRef,
    },
    Planes {
        first: StagedAxisPlane,
        second: StagedAxisPlane,
    },
    /// Reopened from a committed recipe.
    Recipe(DatumAxisBase),
}

/// An axis in its editor.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StagedAxis {
    pub(crate) base: StagedAxisBase,
    /// The line the base makes, before any flip.
    pub(crate) line: ResolvedDatumAxis,
    pub(crate) flip: bool,
}

impl StagedAxis {
    /// Where the axis goes as staged.
    pub(crate) const fn resolved(&self) -> ResolvedDatumAxis {
        if self.flip {
            ResolvedDatumAxis {
                direction: Vector3::new(
                    -self.line.direction.x,
                    -self.line.direction.y,
                    -self.line.direction.z,
                ),
                ..self.line
            }
        } else {
            self.line
        }
    }

    fn describe(&self) -> &'static str {
        match &self.base {
            StagedAxisBase::Edge { .. } => "Along an edge",
            StagedAxisBase::Face { .. } => "Through a curved face's axis",
            StagedAxisBase::Planes { .. } => "Where two planes meet",
            StagedAxisBase::Recipe(_) => "Construction axis",
        }
    }
}

/// The shortest an axis is drawn each way.
const MIN_HALF_LENGTH: f64 = 0.5;

/// How many dashes an axis is drawn as.
const AXIS_DASHES: usize = 12;

impl KernelLabApp {
    /// Rebuilds the list of committed axes from the document.
    pub(crate) fn sync_construction_axes_from_document(&mut self) {
        let mut axes = Vec::new();
        for feature in self.document.features() {
            let ReplayAction::DatumAxis(recipe) = &feature.action else {
                continue;
            };
            if feature.kind != FeatureKind::DatumAxis
                || !self.document.feature_is_active(feature.id).unwrap_or(false)
                || feature.state.suppressed
                || feature.committed.is_none()
            {
                continue;
            }
            let mut description = recipe.base.describe();
            if let Some(first) = description.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            axes.push(ConstructionAxis {
                name: feature.label.clone(),
                feature: feature.id,
                line: recipe.cached(),
                visible: recipe.visible,
                description,
                stale: self.stale_axes.contains(&feature.id),
            });
        }
        self.construction_axes = axes;
    }

    /// Opens the axis editor on what is picked.
    pub(crate) fn stage_construction_axis(&mut self) {
        if self.pending_operation.is_some() || !self.history_is_at_end() {
            return;
        }
        let staged = match self.staged_axis_from_selection() {
            Ok(staged) => staged,
            Err(error) => {
                self.document_status = Some(format!("Axis creation rejected: {error}"));
                return;
            }
        };
        let description = staged.describe();
        self.staged_axis = Some(staged);
        self.pending_operation = Some(PendingOperation::StageAxis { editing: None });
        self.clear_model_entity_selection();
        self.document_status = Some(format!(
            "{description} · flip it if it should run the other way, then confirm with Enter or the green tick"
        ));
    }

    /// Reads what the user has picked as the base of a new axis.
    fn staged_axis_from_selection(&self) -> Result<StagedAxis, String> {
        let body_of = |key: viewport::BodyInstanceKey| {
            self.bodies
                .iter()
                .find(|body| body.id.get() == key.get())
                .ok_or_else(|| "the selected body is no longer available".to_owned())
        };
        let staged = |base, line| StagedAxis {
            base,
            line,
            flip: false,
        };
        if let Some(edge) = self
            .selected_edges
            .first()
            .copied()
            .or_else(|| self.selected_edge())
        {
            let body = body_of(edge.body)?;
            let ends = NativeKernel::straight_edge_ends(&body.body.snapshot, edge.edge)
                .map_err(|error| error.to_string())?
                .ok_or_else(|| {
                    "the edge is curved; pick a straight edge, or the curved face whose axis you want"
                        .to_owned()
                })?;
            return Ok(staged(
                StagedAxisBase::Edge {
                    body: body.id,
                    edge: edge.edge,
                },
                edge_line(ends).ok_or_else(|| "the edge has no length".to_owned())?,
            ));
        }
        let flat = |snapshot: &Snapshot, face: EntityRef| {
            datum_face_geometry(snapshot, face)
                .ok()
                .and_then(face_plane)
        };
        match self.selected_faces.as_slice() {
            [face] => {
                let body = body_of(face.body)?;
                if let Some(axis) = NativeKernel::face_axis(&body.body.snapshot, face.face)
                    .map_err(|error| error.to_string())?
                {
                    return Ok(staged(
                        StagedAxisBase::Face {
                            body: body.id,
                            face: face.face,
                        },
                        ResolvedDatumAxis {
                            origin: axis.origin,
                            direction: axis.direction,
                            half_length: axis.half_length.max(MIN_HALF_LENGTH),
                        },
                    ));
                }
                let Some(face_plane) = flat(&body.body.snapshot, face.face) else {
                    return Err("the face has no axis and is not flat".to_owned());
                };
                // A flat face alone has no axis; with a construction plane
                // picked beside it, the two meet in one.
                let Some(plane) = self
                    .selected_construction_plane
                    .and_then(|id| self.construction_planes.iter().find(|plane| plane.id == id))
                else {
                    return Err(
                        "a flat face alone has no axis; pick a second flat face, or a construction plane, with it"
                            .to_owned(),
                    );
                };
                let plane_plane = face_plane_of(plane.frame, plane.half_u.max(plane.half_v))
                    .ok_or_else(|| "the construction plane has no normal".to_owned())?;
                let line = planes_meet(
                    (face_plane.0, face_plane.1),
                    (plane_plane.0, plane_plane.1),
                    face_plane.2.max(plane_plane.2),
                )
                .map_err(|error| error.to_string())?;
                Ok(staged(
                    StagedAxisBase::Planes {
                        first: StagedAxisPlane::Face {
                            body: body.id,
                            face: face.face,
                        },
                        second: StagedAxisPlane::Plane(plane.feature),
                    },
                    line,
                ))
            }
            [first, second] => {
                let first_body = body_of(first.body)?;
                let second_body = body_of(second.body)?;
                let (Some(first_plane), Some(second_plane)) = (
                    flat(&first_body.body.snapshot, first.face),
                    flat(&second_body.body.snapshot, second.face),
                ) else {
                    return Err("two faces meet in an axis only when both are flat".to_owned());
                };
                let line = planes_meet(
                    (first_plane.0, first_plane.1),
                    (second_plane.0, second_plane.1),
                    first_plane.2.max(second_plane.2),
                )
                .map_err(|error| error.to_string())?;
                Ok(staged(
                    StagedAxisBase::Planes {
                        first: StagedAxisPlane::Face {
                            body: first_body.id,
                            face: first.face,
                        },
                        second: StagedAxisPlane::Face {
                            body: second_body.id,
                            face: second.face,
                        },
                    },
                    line,
                ))
            }
            _ => {
                Err("select a straight edge, a curved face, or two flat faces that meet".to_owned())
            }
        }
    }

    /// Turns the staged axis to run the other way.
    pub fn set_staged_axis_flip(&mut self, flip: bool) {
        if let Some(staged) = self.staged_axis.as_mut() {
            staged.flip = flip;
        }
    }

    /// Names what the staged axis is made from by persistent reference, so
    /// a rebuild can find it again.
    fn staged_axis_recipe_base(&self, base: &StagedAxisBase) -> Result<DatumAxisBase, String> {
        let name = |entity: EntityRef, body: BodyId| {
            self.persistent_ref_for_entity_in(entity, None, body)
                .ok_or_else(|| {
                    "the picked geometry has no unique history to follow; pick it on the body's latest state"
                        .to_owned()
                })
        };
        let plane = |plane: &StagedAxisPlane| -> Result<DatumAxisPlane, String> {
            Ok(match plane {
                StagedAxisPlane::Face { body, face } => DatumAxisPlane::Face(DatumFaceRef {
                    body: *body,
                    face: name(*face, *body)?,
                }),
                StagedAxisPlane::Plane(plane) => DatumAxisPlane::Plane { plane: *plane },
            })
        };
        Ok(match base {
            StagedAxisBase::Edge { body, edge } => DatumAxisBase::Edge {
                body: *body,
                edge: name(*edge, *body)?,
            },
            StagedAxisBase::Face { body, face } => DatumAxisBase::Face(DatumFaceRef {
                body: *body,
                face: name(*face, *body)?,
            }),
            StagedAxisBase::Planes { first, second } => DatumAxisBase::Planes {
                first: plane(first)?,
                second: plane(second)?,
            },
            StagedAxisBase::Recipe(base) => base.clone(),
        })
    }

    /// Confirms the axis editor: a new axis is appended to the history, and
    /// an edited one has its recipe rewritten in place and everything built
    /// on it replayed.
    pub(crate) fn commit_staged_axis(&mut self, editing: Option<FeatureId>) {
        let Some(staged) = self.staged_axis.clone() else {
            self.pending_operation = None;
            return;
        };
        let base = match self.staged_axis_recipe_base(&staged.base) {
            Ok(base) => base,
            Err(error) => {
                self.document_status = Some(format!("Axis rejected: {error}"));
                return;
            }
        };
        let mut recipe = DatumAxisRecipe::new(base, staged.resolved());
        recipe.flip = staged.flip;
        if let Err(error) = recipe.validate() {
            self.document_status = Some(format!("Axis rejected: {error}"));
            return;
        }
        if let Some(feature) = editing {
            self.apply_axis_edit(feature, recipe);
            return;
        }
        // An axis reads the body its base names, and any plane it is the
        // meeting of; a second body's latest feature is a dependency.
        let bodies = recipe.base.bodies();
        let mut inputs = bodies
            .first()
            .map(|body| vec![FeatureInput::Body(*body)])
            .unwrap_or_default();
        inputs.extend(recipe.base.planes().into_iter().map(FeatureInput::Feature));
        let dependencies = bodies
            .iter()
            .skip(1)
            .filter_map(|body| self.document.body(*body).map(|record| record.last_feature))
            .collect::<Vec<_>>();
        let snapshot = bodies
            .first()
            .and_then(|body| self.bodies.iter().find(|candidate| candidate.id == *body))
            .map(|body| &body.body.snapshot)
            .or_else(|| self.displayed.as_ref().map(|displayed| &displayed.snapshot))
            .unwrap_or(&self.empty_snapshot);
        let association =
            SnapshotAssociation::new(snapshot.id(), snapshot.id(), snapshot.semantic_digest());
        let label = Self::next_document_feature_label(&self.document, FeatureKind::DatumAxis);
        let mut draft = FeatureDraft::new(
            FeatureKind::DatumAxis,
            label.clone(),
            ReplayAction::DatumAxis(recipe),
        )
        .with_commit(association);
        for input in inputs {
            draft = draft.with_input(input);
        }
        for dependency in dependencies {
            draft = draft.with_dependency(dependency);
        }
        match self.document.append_feature(draft) {
            Ok(appended) => {
                self.staged_axis = None;
                self.pending_operation = None;
                self.selected_history_feature = Some(appended.feature);
                self.history_scrub_position = self.document.history_position();
                self.sync_construction_axes_from_document();
                self.sync_feature_preview_from_document();
                self.document_status =
                    Some(format!("{label} committed · a revolve can turn about it"));
            }
            Err(error) => {
                self.document_status = Some(format!("Axis rejected: {error}"));
            }
        }
    }

    /// Reopens a committed axis in its editor. As with a plane, the history
    /// rolls back to just before it, and confirming rewrites the recipe in
    /// its slot and replays what follows.
    pub(crate) fn begin_axis_edit(&mut self, feature: FeatureId) -> bool {
        if self.pending_operation.is_some() {
            return false;
        }
        let Some((index, recipe)) = self
            .document
            .features()
            .iter()
            .enumerate()
            .find(|(_, node)| node.id == feature)
            .and_then(|(index, node)| match &node.action {
                ReplayAction::DatumAxis(recipe) => Some((index, recipe.clone())),
                _ => None,
            })
        else {
            self.document_status = Some("That feature is not an axis".to_owned());
            return false;
        };
        if !self.move_history_cursor(index) {
            return false;
        }
        let cached = recipe.cached();
        let line = if recipe.flip {
            ResolvedDatumAxis {
                direction: Vector3::new(
                    -cached.direction.x,
                    -cached.direction.y,
                    -cached.direction.z,
                ),
                ..cached
            }
        } else {
            cached
        };
        self.staged_axis = Some(StagedAxis {
            base: StagedAxisBase::Recipe(recipe.base),
            line,
            flip: recipe.flip,
        });
        self.pending_operation = Some(PendingOperation::StageAxis {
            editing: Some(feature),
        });
        self.document_status =
            Some("Editing the axis · flip it, then confirm or press Escape".to_owned());
        true
    }

    fn apply_axis_edit(&mut self, feature: FeatureId, recipe: DatumAxisRecipe) {
        self.staged_axis = None;
        self.pending_operation = None;
        match self
            .document
            .replace_feature_action(feature, ReplayAction::DatumAxis(recipe))
        {
            Ok(_) => {
                self.move_history_cursor(self.document.features().len());
                self.selected_history_feature = Some(feature);
                if self.rebuild_document_from(feature) {
                    self.document_status =
                        Some("Axis rewritten; everything after it rebuilt".to_owned());
                }
            }
            Err(error) => {
                self.document_status = Some(format!("Axis edit rejected: {error}"));
                self.move_history_cursor(self.document.features().len());
            }
        }
    }

    /// Abandons the axis editor. An edit puts the whole model back.
    pub(crate) fn cancel_staged_axis(&mut self, editing: Option<FeatureId>) {
        self.staged_axis = None;
        self.pending_operation = None;
        if editing.is_some() {
            self.move_history_cursor(self.document.features().len());
            self.document_status = Some("Axis edit abandoned".to_owned());
        }
    }

    /// Deletes an axis nothing is built on.
    pub(crate) fn delete_construction_axis(&mut self, feature: FeatureId) {
        match self.document.remove_datum_axis(feature) {
            Ok(removed) => {
                self.history_scrub_position = self.document.history_position();
                self.selected_history_feature = None;
                self.sync_construction_axes_from_document();
                self.sync_feature_preview_from_document();
                self.document_status = Some(format!("{} deleted", removed.label));
            }
            Err(artificer_model::DocumentError::FeatureInUse { dependent, .. }) => {
                let name = self
                    .document
                    .feature(dependent)
                    .map_or_else(|| dependent.to_string(), |node| node.label.clone());
                self.document_status =
                    Some(format!("The axis cannot be deleted: {name} is built on it"));
            }
            Err(error) => {
                self.document_status = Some(format!("The axis cannot be deleted: {error}"));
            }
        }
    }

    /// The CONSTRUCTION AXIS card.
    pub(crate) fn axis_controls(&mut self, ui: &mut egui::Ui) {
        let Some(staged) = self.staged_axis.clone() else {
            return;
        };
        let editing = matches!(
            self.pending_operation,
            Some(PendingOperation::StageAxis { editing: Some(_) })
        );
        status_line(
            ui,
            if editing {
                "EDITING AXIS"
            } else {
                "CONSTRUCTION AXIS"
            },
            if editing {
                theme::warn()
            } else {
                theme::good()
            },
        );
        ui.label(RichText::new(staged.describe()).color(theme::text()));
        let mut flip = staged.flip;
        let response = ui
            .checkbox(&mut flip, "Flip direction")
            .on_hover_text("Run the axis the other way; a revolve about it turns the other way");
        response.widget_info(|| {
            egui::WidgetInfo::selected(
                egui::WidgetType::Checkbox,
                true,
                flip,
                "Axis flip direction",
            )
        });
        if response.changed() {
            self.set_staged_axis_flip(flip);
        }
        ui.label(
            RichText::new("Enter or the tick confirms · Escape abandons")
                .small()
                .color(theme::muted()),
        );
    }

    /// The committed axes and the staged one, drawn as dashed lines.
    pub(crate) fn construction_axis_overlays(&self) -> Vec<viewport::ModelSketchOverlay> {
        let mut overlays = self
            .construction_axes
            .iter()
            .filter(|axis| axis.visible)
            .filter(|axis| {
                !matches!(
                    self.pending_operation,
                    Some(PendingOperation::StageAxis { editing: Some(editing) }) if editing == axis.feature
                )
            })
            .map(|axis| axis_overlay(axis.line, true))
            .collect::<Vec<_>>();
        if let Some(staged) = &self.staged_axis {
            overlays.push(axis_overlay(staged.resolved(), false));
        }
        overlays
    }

    /// The names of the committed construction axes, in history order.
    #[must_use]
    pub fn construction_axis_names(&self) -> Vec<String> {
        self.construction_axes
            .iter()
            .map(|axis| axis.name.clone())
            .collect()
    }

    /// The staged axis's line, while one is staged.
    #[must_use]
    pub fn staged_axis_line(&self) -> Option<ResolvedDatumAxis> {
        self.staged_axis.as_ref().map(StagedAxis::resolved)
    }
}

/// A straight edge's line: through its middle, from its start to its end.
fn edge_line(ends: [Point3; 2]) -> Option<ResolvedDatumAxis> {
    let along = Vector3::new(
        ends[1].x - ends[0].x,
        ends[1].y - ends[0].y,
        ends[1].z - ends[0].z,
    );
    let length = (along.x * along.x + along.y * along.y + along.z * along.z).sqrt();
    (length.is_finite() && length > 1.0e-12).then(|| ResolvedDatumAxis {
        origin: Point3::new(
            f64::midpoint(ends[0].x, ends[1].x),
            f64::midpoint(ends[0].y, ends[1].y),
            f64::midpoint(ends[0].z, ends[1].z),
        ),
        direction: Vector3::new(along.x / length, along.y / length, along.z / length),
        half_length: (length * 0.5).max(MIN_HALF_LENGTH),
    })
}

/// A flat face as the meeting of planes needs it: a point on it, its unit
/// normal, and how far it reaches.
fn face_plane(geometry: DatumFaceGeometry) -> Option<(Point3, Vector3, f64)> {
    face_plane_of(
        geometry.frame,
        geometry.half_extent[0].max(geometry.half_extent[1]),
    )
}

fn face_plane_of(
    frame: artificer_protocol::PlanarFrame3,
    reach: f64,
) -> Option<(Point3, Vector3, f64)> {
    let (u, v) = (frame.u, frame.v);
    let normal = Vector3::new(
        u.y * v.z - u.z * v.y,
        u.z * v.x - u.x * v.z,
        u.x * v.y - u.y * v.x,
    );
    let length = (normal.x * normal.x + normal.y * normal.y + normal.z * normal.z).sqrt();
    (length.is_finite() && length > 1.0e-12).then(|| {
        (
            frame.origin,
            Vector3::new(normal.x / length, normal.y / length, normal.z / length),
            reach,
        )
    })
}

/// An axis drawn as a dashed line along its length.
fn axis_overlay(line: ResolvedDatumAxis, subdued: bool) -> viewport::ModelSketchOverlay {
    let at = |fraction: f64| {
        let along = line.half_length * (2.0 * fraction - 1.0);
        Point3::new(
            line.direction.x.mul_add(along, line.origin.x),
            line.direction.y.mul_add(along, line.origin.y),
            line.direction.z.mul_add(along, line.origin.z),
        )
    };
    let step = 1.0 / AXIS_DASHES as f64;
    let dashes = (0..AXIS_DASHES)
        .map(|index| {
            let start = index as f64 * step;
            [at(start), at(start + step * 0.6)]
        })
        .collect();
    // The far end is marked, so which way the axis runs can be seen.
    viewport::ModelSketchOverlay::new(vec![at(1.0)], dashes, subdued)
}

impl DatumAxisResolver for ModelPlaneResolver<'_> {
    fn edge(&self, _body: BodyId, edge: &PersistentRef) -> Result<[Point3; 2], DatumAxisError> {
        let (edge, snapshot) = self
            .find(edge, None)
            .map_err(|_| DatumAxisError::EdgeMissing)?;
        NativeKernel::straight_edge_ends(snapshot, edge)
            .map_err(|_| DatumAxisError::EdgeMissing)?
            .ok_or(DatumAxisError::EdgeNotStraight)
    }

    fn face_axis(&self, face: &DatumFaceRef) -> Result<ResolvedDatumAxis, DatumAxisError> {
        let (entity, snapshot) = self.find(&face.face, None).map_err(|reason| match reason {
            PersistentMissingOrAmbiguous::Missing => DatumAxisError::FaceMissing,
            PersistentMissingOrAmbiguous::Ambiguous => DatumAxisError::FaceAmbiguous,
        })?;
        let axis = NativeKernel::face_axis(snapshot, entity)
            .map_err(|_| DatumAxisError::FaceMissing)?
            .ok_or(DatumAxisError::FaceHasNoAxis)?;
        Ok(ResolvedDatumAxis {
            origin: axis.origin,
            direction: axis.direction,
            half_length: axis.half_length,
        })
    }

    fn face_plane(&self, face: &DatumFaceRef) -> Result<DatumFaceGeometry, DatumAxisError> {
        DatumPlaneResolver::face(self, face).map_err(|error| match error {
            DatumPlaneError::FaceMissing => DatumAxisError::FaceMissing,
            DatumPlaneError::FaceAmbiguous => DatumAxisError::FaceAmbiguous,
            _ => DatumAxisError::FaceNotPlanar,
        })
    }

    fn plane(&self, plane: FeatureId) -> Result<ResolvedDatumPlane, DatumAxisError> {
        DatumPlaneResolver::plane(self, plane).map_err(|_| DatumAxisError::UnknownPlane(plane))
    }
}

#[cfg(test)]
mod tests {
    use artificer_model::{FeatureInput, RevolveAxis};

    use super::*;
    use crate::{FeaturePreviewKind, SketchGeometry, SketchPlane, SketchPoint};

    const PI: f64 = std::f64::consts::PI;

    fn body_key(app: &KernelLabApp) -> viewport::BodyInstanceKey {
        viewport::BodyInstanceKey::new(app.active_body_id().expect("a body").get())
    }

    /// The starting block's upright edge at x = 2, y = 0, which lies in the
    /// XZ plane.
    fn upright_edge(app: &KernelLabApp) -> viewport::DocumentEdgeSelection {
        let edge = app
            .displayed
            .as_ref()
            .expect("the block is displayed")
            .scene
            .edges
            .iter()
            .find(|edge| {
                edge.endpoints
                    .iter()
                    .all(|point| (point.x - 2.0).abs() < 1.0e-9 && point.y.abs() < 1.0e-9)
            })
            .expect("the block's edge at x = 2, y = 0")
            .source_edge;
        viewport::DocumentEdgeSelection {
            body: body_key(app),
            edge,
        }
    }

    /// The block's face whose outward normal is `normal`.
    fn face_facing(app: &KernelLabApp, normal: [f64; 3]) -> viewport::DocumentFaceSelection {
        let face = app
            .displayed
            .as_ref()
            .expect("the block is displayed")
            .scene
            .triangles
            .iter()
            .find(|triangle| {
                let [a, b, c] = triangle.vertices;
                let (u, v) = (
                    [b.x - a.x, b.y - a.y, b.z - a.z],
                    [c.x - a.x, c.y - a.y, c.z - a.z],
                );
                let facet = [
                    u[1] * v[2] - u[2] * v[1],
                    u[2] * v[0] - u[0] * v[2],
                    u[0] * v[1] - u[1] * v[0],
                ];
                let length = facet.iter().map(|x| x * x).sum::<f64>().sqrt();
                length > 0.0
                    && facet.iter().zip(normal).map(|(a, b)| a * b).sum::<f64>() / length > 0.999
            })
            .expect("a face points that way")
            .source_face;
        viewport::DocumentFaceSelection {
            body: body_key(app),
            face,
        }
    }

    fn assert_close(actual: f64, expected: f64, what: &str) {
        assert!(
            (actual - expected).abs() < 1.0e-9 * expected.abs().max(1.0),
            "{what}: {actual} should be {expected}"
        );
    }

    /// An axis along the block's edge is a feature with a chip; a revolve on
    /// the XZ plane turns about it; and it cannot be deleted from under the
    /// revolve.
    #[test]
    fn a_revolve_turns_about_an_axis_along_an_edge() {
        let mut app = KernelLabApp::default();
        app.selected_edges = vec![upright_edge(&app)];
        app.stage_construction_axis();
        let line = app.staged_axis_line().expect("the axis is staged");
        assert_close(line.origin.x, 2.0, "on the edge");
        assert_close(line.origin.z, 2.0, "at its middle");
        assert_close(line.direction.z.abs(), 1.0, "along it");
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        assert_eq!(app.construction_axis_names(), vec!["Axis 1".to_owned()]);
        let axis = app.construction_axes[0].feature;
        assert!(
            app.feature_preview
                .entries
                .iter()
                .any(|entry| entry.kind == FeaturePreviewKind::Axis && entry.label() == "Axis 1")
        );
        assert!(app.feature_has_an_editor(axis));
        assert!(!app.construction_axis_overlays().is_empty(), "it is drawn");

        // A rectangle from x = 3 to 4, 1 tall, on the XZ plane: r in [1, 2]
        // about the edge.
        app.open_origin_plane_sketch(SketchPlane::XZ);
        let rectangle = app
            .sketch
            .stage_geometry(SketchGeometry::Rectangle {
                first: SketchPoint::new(3.0, 0.0),
                opposite: SketchPoint::new(4.0, 1.0),
            })
            .expect("the section stages");
        app.commit_sketch_stroke(rectangle);
        assert!(app.stage_revolve(), "{:?}", app.document_status);
        let choice = app
            .revolve_axis_choices()
            .into_iter()
            .find(|choice| choice.label == "Axis 1")
            .expect("the axis is offered");
        assert_eq!(choice.unavailable, None);
        app.set_revolve_axis(choice.axis);
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        let revolve = app
            .document
            .features()
            .iter()
            .rev()
            .find(|node| node.kind == FeatureKind::Revolve)
            .expect("the revolve is in the history");
        assert!(revolve.inputs.contains(&FeatureInput::Feature(axis)));
        assert!(matches!(
            &revolve.action,
            ReplayAction::SketchRevolve(recipe) if recipe.axis == RevolveAxis::DatumAxis { axis }
        ));
        let volume = app.displayed_measures().expect("the revolve").volume;
        assert_close(volume, PI * (4.0 - 1.0) * 1.0, "revolved about the edge");

        app.delete_construction_axis(axis);
        assert!(
            app.document_status
                .as_deref()
                .is_some_and(|status| status.contains("is built on it")),
            "{:?}",
            app.document_status
        );
        assert_eq!(app.construction_axis_names().len(), 1);
    }

    /// Two flat faces meet in an axis; reopened and flipped, it runs the
    /// other way along the same line.
    #[test]
    fn an_axis_where_two_faces_meet_is_flipped_in_its_editor() {
        let mut app = KernelLabApp::default();
        app.selected_faces = vec![
            face_facing(&app, [0.0, 0.0, 1.0]),
            face_facing(&app, [0.0, -1.0, 0.0]),
        ];
        app.stage_construction_axis();
        let line = app.staged_axis_line().expect("the faces meet");
        assert_close(line.direction.x.abs(), 1.0, "along x");
        assert_close(line.origin.y, 0.0, "on the front face");
        assert_close(line.origin.z, 4.0, "on the top face");
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        let axis = app.construction_axes[0].feature;
        let before = app.construction_axes[0].line.direction.x;

        assert!(app.begin_axis_edit(axis), "{:?}", app.document_status);
        app.set_staged_axis_flip(true);
        assert!(app.confirm_pending_operation(), "{:?}", app.document_status);
        let recipe = app.document.datum_axis(axis).expect("still an axis");
        assert!(recipe.flip);
        assert_close(recipe.direction.x, -before, "flipped");
        assert_close(recipe.origin.z, 4.0, "on the same line");
        assert!(app.stale_axes.is_empty());

        // Nothing is built on it, so it deletes.
        app.delete_construction_axis(axis);
        assert!(app.construction_axis_names().is_empty());
    }

    #[test]
    fn a_flat_face_alone_has_no_axis() {
        let mut app = KernelLabApp::default();
        app.selected_faces = vec![face_facing(&app, [0.0, 0.0, 1.0])];
        app.stage_construction_axis();
        assert!(app.staged_axis.is_none());
        assert!(
            app.document_status
                .as_deref()
                .is_some_and(|status| status.contains("a flat face alone has no axis")),
            "{:?}",
            app.document_status
        );
    }
}
