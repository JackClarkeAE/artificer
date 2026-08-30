//! Entity selection types and resolution algorithms.

use std::collections::{BTreeMap, BTreeSet};

use artificer_kernel::{NativeKernel, Snapshot};
use artificer_protocol::{
    EntityId, EntityKind, EntityRef, OperationReport, Point3, SnapshotId, Vector3,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::commands::StepLabel;
use crate::debug::{ApiError, ApiErrorCode, EntityInfo};

/// A stable or geometric reference to a topological entity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntitySelector {
    /// Select by parametric history: role produced by a named prior step.
    ByHistory {
        from_step: StepLabel,
        kind: EntityKind,
        role: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ordinal: Option<u32>,
    },
    /// Select by geometric property in the current snapshot.
    ByGeometry(GeometricSelector),
    /// Direct, snapshot-bound entity reference.
    Direct { entity_ref: EntityRef },
}

impl EntitySelector {
    #[must_use]
    pub fn history_face(step: impl Into<String>, role: impl Into<String>) -> Self {
        Self::ByHistory {
            from_step: StepLabel(step.into()),
            kind: EntityKind::Face,
            role: role.into(),
            ordinal: None,
        }
    }

    #[must_use]
    pub fn history_face_ordinal(
        step: impl Into<String>,
        role: impl Into<String>,
        ordinal: u32,
    ) -> Self {
        Self::ByHistory {
            from_step: StepLabel(step.into()),
            kind: EntityKind::Face,
            role: role.into(),
            ordinal: Some(ordinal),
        }
    }

    #[must_use]
    pub fn history_edge(step: impl Into<String>, role: impl Into<String>) -> Self {
        Self::ByHistory {
            from_step: StepLabel(step.into()),
            kind: EntityKind::Edge,
            role: role.into(),
            ordinal: None,
        }
    }

    #[must_use]
    pub fn history_edge_ordinal(
        step: impl Into<String>,
        role: impl Into<String>,
        ordinal: u32,
    ) -> Self {
        Self::ByHistory {
            from_step: StepLabel(step.into()),
            kind: EntityKind::Edge,
            role: role.into(),
            ordinal: Some(ordinal),
        }
    }
}

/// Geometric criteria for finding entities in current geometry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GeometricSelector {
    /// Select planar faces oriented relative to a directional vector.
    FaceByNormal {
        direction: Vector3,
        match_kind: NormalMatch,
    },
    /// Select the entity of `kind` closest to a 3D coordinate.
    NearestTo { point: Point3, kind: EntityKind },
    /// Filter entities by geometric carrier type.
    ByType {
        surface_type: SurfaceFilter,
        kind: EntityKind,
    },
    /// Find the shared edge between two adjacent faces.
    EdgeBetween {
        face_a: Box<EntitySelector>,
        face_b: Box<EntitySelector>,
    },
    /// Select by maximum or minimum metric (e.g. largest face area).
    ByExtremum {
        metric: Metric,
        extremum: Extremum,
        kind: EntityKind,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NormalMatch {
    Closest,
    Farthest,
    Parallel,
    Perpendicular,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceFilter {
    Planar,
    Cylindrical,
    Spherical,
    Conical,
    Toroidal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Area,
    Length,
    Radius,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Extremum {
    Maximum,
    Minimum,
}

#[derive(Debug, Error)]
pub enum SelectorResolutionError {
    #[error("Selector not found: {selector_description}. {message}")]
    NotFound {
        selector_description: String,
        message: String,
    },
    #[error(
        "Ambiguous selector: {selector_description}. Found {candidate_count} candidates. Suggestion: {suggestion}"
    )]
    Ambiguous {
        selector_description: String,
        candidate_count: usize,
        suggestion: String,
    },
    #[error("Stale reference: {message}")]
    StaleReference { message: String },
}

impl From<SelectorResolutionError> for ApiError {
    fn from(err: SelectorResolutionError) -> Self {
        match err {
            SelectorResolutionError::NotFound {
                selector_description,
                message,
            } => ApiError::new(
                ApiErrorCode::SelectorNotFound,
                format!("{selector_description}: {message}"),
            ),
            SelectorResolutionError::Ambiguous {
                selector_description,
                suggestion,
                ..
            } => ApiError::new(
                ApiErrorCode::SelectorAmbiguous,
                format!("Ambiguous selector: {selector_description}"),
            )
            .with_suggestion(suggestion),
            SelectorResolutionError::StaleReference { message } => {
                ApiError::new(ApiErrorCode::SelectorNotFound, message)
            }
        }
    }
}

/// Resolves an `EntitySelector` to a concrete `EntityRef` within a session.
pub fn resolve_selector(
    selector: &EntitySelector,
    current_snapshot: &Snapshot,
    step_order: &[String],
    step_reports: &BTreeMap<String, OperationReport>,
) -> Result<EntityRef, ApiError> {
    match selector {
        EntitySelector::Direct { entity_ref } => {
            if entity_ref.snapshot == current_snapshot.id() {
                Ok(*entity_ref)
            } else {
                Err(ApiError::new(
                    ApiErrorCode::SelectorNotFound,
                    format!(
                        "Direct entity {:?} has snapshot {} which differs from current {}",
                        entity_ref.entity,
                        entity_ref.snapshot,
                        current_snapshot.id()
                    ),
                ))
            }
        }
        EntitySelector::ByHistory {
            from_step,
            kind,
            role,
            ordinal,
        } => resolve_history_selector(
            from_step,
            *kind,
            role,
            *ordinal,
            current_snapshot.id(),
            step_order,
            step_reports,
        ),
        EntitySelector::ByGeometry(geom) => {
            resolve_geometric_selector(geom, current_snapshot, step_order, step_reports)
        }
    }
}

fn resolve_history_selector(
    from_step: &StepLabel,
    kind: EntityKind,
    role: &str,
    ordinal: Option<u32>,
    current_snapshot: SnapshotId,
    step_order: &[String],
    step_reports: &BTreeMap<String, OperationReport>,
) -> Result<EntityRef, ApiError> {
    let source_index = step_order
        .iter()
        .position(|label| label == &from_step.0)
        .ok_or_else(|| {
            ApiError::new(
                ApiErrorCode::SelectorNotFound,
                format!("Step \"{}\" does not exist in session history", from_step.0),
            )
        })?;

    let source_report = step_reports.get(&from_step.0).ok_or_else(|| {
        ApiError::new(
            ApiErrorCode::SelectorNotFound,
            format!("No operation report recorded for step \"{}\"", from_step.0),
        )
    })?;

    let matching_outputs = {
        let exact = source_report
            .history
            .iter()
            .filter(|record| {
                record.role.as_ref().is_some_and(|r| {
                    r.name == role && ordinal.is_none_or(|ord| r.ordinal == Some(ord))
                })
            })
            .flat_map(|record| record.outputs.iter().copied())
            .filter(|output| output.kind == kind)
            .collect::<BTreeSet<_>>();

        if !exact.is_empty() {
            exact
        } else {
            // Semantic fallback matching for primitives and common roles
            source_report
                .history
                .iter()
                .filter(|record| {
                    record.role.as_ref().is_some_and(|r| {
                        let is_kind_match = r.name == kind.to_string();
                        let matches_semantic = match role {
                            "top_face" | "top" => {
                                r.name.contains("top") || (is_kind_match && r.ordinal == Some(1))
                            }
                            "bottom_face" | "bottom" => {
                                r.name.contains("bottom") || (is_kind_match && r.ordinal == Some(0))
                            }
                            "side_face" | "side" => {
                                r.name.contains("side")
                                    || (is_kind_match && r.ordinal.is_some_and(|o| o >= 2))
                            }
                            _ => is_kind_match,
                        };
                        matches_semantic && ordinal.is_none_or(|ord| r.ordinal == Some(ord))
                    })
                })
                .flat_map(|record| record.outputs.iter().copied())
                .filter(|output| output.kind == kind)
                .collect::<BTreeSet<_>>()
        }
    };

    if matching_outputs.is_empty() {
        return Err(ApiError::new(
            ApiErrorCode::SelectorNotFound,
            format!(
                "No entity with kind {:?} and role \"{}\" was found in step \"{}\"",
                kind, role, from_step.0
            ),
        ));
    }

    if matching_outputs.len() > 1 && ordinal.is_none() {
        let candidates = matching_outputs
            .iter()
            .enumerate()
            .map(|(idx, e)| EntityInfo {
                kind: e.kind,
                entity_ref: *e,
                geometry_description: format!("Candidate #{idx}"),
                role: Some(role.to_owned()),
                ordinal: Some(idx as u32),
            })
            .collect();
        return Err(ApiError::new(
            ApiErrorCode::SelectorAmbiguous,
            format!(
                "Step \"{}\" produced {} matching entities for role \"{}\"",
                from_step.0,
                matching_outputs.len(),
                role
            ),
        )
        .with_suggestion("Specify an ordinal to disambiguate")
        .with_candidates(candidates));
    }

    let mut current_target = *matching_outputs.iter().next().unwrap();

    // Trace forward through subsequent operation reports
    for step_label in &step_order[source_index + 1..] {
        if let Some(report) = step_reports.get(step_label) {
            let next_candidates = report
                .history
                .iter()
                .filter(|record| record.inputs.contains(&current_target))
                .flat_map(|record| record.outputs.iter().copied())
                .filter(|output| output.kind == kind)
                .collect::<BTreeSet<_>>();

            if next_candidates.len() == 1 {
                current_target = *next_candidates.iter().next().unwrap();
            } else if next_candidates.len() > 1 {
                let ord = ordinal.unwrap_or(0) as usize;
                if ord < next_candidates.len() {
                    current_target = *next_candidates.iter().nth(ord).unwrap();
                }
            }
        }
    }

    Ok(EntityRef {
        snapshot: current_snapshot,
        entity: current_target.entity,
        kind,
    })
}

fn resolve_geometric_selector(
    geom: &GeometricSelector,
    current_snapshot: &Snapshot,
    step_order: &[String],
    step_reports: &BTreeMap<String, OperationReport>,
) -> Result<EntityRef, ApiError> {
    let scene = NativeKernel::debug_scene(current_snapshot);

    match geom {
        GeometricSelector::FaceByNormal {
            direction,
            match_kind,
        } => {
            let dir_len =
                (direction.x * direction.x + direction.y * direction.y + direction.z * direction.z)
                    .sqrt();
            if dir_len <= 1e-9 {
                return Err(ApiError::new(
                    ApiErrorCode::InvalidInput,
                    "FaceByNormal direction vector cannot be zero",
                ));
            }
            let target_dir = Vector3::new(
                direction.x / dir_len,
                direction.y / dir_len,
                direction.z / dir_len,
            );

            // Group triangles by face entity
            let mut face_normals: BTreeMap<EntityId, (Vector3, usize)> = BTreeMap::new();
            for tri in &scene.triangles {
                let n = tri.normals[0];
                let entry = face_normals
                    .entry(tri.source_face.entity)
                    .or_insert((Vector3::new(0.0, 0.0, 0.0), 0));
                entry.0.x += n.x;
                entry.0.y += n.y;
                entry.0.z += n.z;
                entry.1 += 1;
            }

            if face_normals.is_empty() {
                return Err(ApiError::new(
                    ApiErrorCode::SelectorNotFound,
                    "No faces found in current snapshot",
                ));
            }

            let mut best_face = None;
            let mut best_score = match match_kind {
                NormalMatch::Closest => f64::NEG_INFINITY,
                NormalMatch::Farthest => f64::INFINITY,
                NormalMatch::Parallel => -1.0,
                NormalMatch::Perpendicular => f64::INFINITY,
            };

            for (face_id, (sum_n, count)) in face_normals {
                let count_f = count as f64;
                let avg_n = Vector3::new(sum_n.x / count_f, sum_n.y / count_f, sum_n.z / count_f);
                let len = (avg_n.x * avg_n.x + avg_n.y * avg_n.y + avg_n.z * avg_n.z).sqrt();
                if len <= 1e-9 {
                    continue;
                }
                let unit_n = Vector3::new(avg_n.x / len, avg_n.y / len, avg_n.z / len);
                let dot =
                    unit_n.x * target_dir.x + unit_n.y * target_dir.y + unit_n.z * target_dir.z;

                let score = match match_kind {
                    NormalMatch::Closest => dot,
                    NormalMatch::Farthest => -dot,
                    NormalMatch::Parallel => dot.abs(),
                    NormalMatch::Perpendicular => dot.abs(),
                };

                let is_better = match match_kind {
                    NormalMatch::Closest | NormalMatch::Parallel => score > best_score,
                    NormalMatch::Farthest => score < best_score,
                    NormalMatch::Perpendicular => score < best_score,
                };

                if is_better {
                    best_score = score;
                    best_face = Some(face_id);
                }
            }

            let face_id = best_face.ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::SelectorNotFound,
                    "No matching face found for normal selector",
                )
            })?;

            Ok(EntityRef {
                snapshot: current_snapshot.id(),
                entity: face_id,
                kind: EntityKind::Face,
            })
        }
        GeometricSelector::NearestTo { point, kind } => {
            let mut best_entity = None;
            let mut best_dist_sq = f64::INFINITY;

            match kind {
                EntityKind::Face => {
                    let mut face_centers: BTreeMap<EntityId, (Point3, usize)> = BTreeMap::new();
                    for tri in &scene.triangles {
                        let c = Point3::new(
                            (tri.vertices[0].x + tri.vertices[1].x + tri.vertices[2].x) / 3.0,
                            (tri.vertices[0].y + tri.vertices[1].y + tri.vertices[2].y) / 3.0,
                            (tri.vertices[0].z + tri.vertices[1].z + tri.vertices[2].z) / 3.0,
                        );
                        let entry = face_centers
                            .entry(tri.source_face.entity)
                            .or_insert((Point3::new(0.0, 0.0, 0.0), 0));
                        entry.0.x += c.x;
                        entry.0.y += c.y;
                        entry.0.z += c.z;
                        entry.1 += 1;
                    }
                    for (face_id, (sum_p, count)) in face_centers {
                        let cf = count as f64;
                        let center = Point3::new(sum_p.x / cf, sum_p.y / cf, sum_p.z / cf);
                        let dx = center.x - point.x;
                        let dy = center.y - point.y;
                        let dz = center.z - point.z;
                        let d2 = dx * dx + dy * dy + dz * dz;
                        if d2 < best_dist_sq {
                            best_dist_sq = d2;
                            best_entity = Some(face_id);
                        }
                    }
                }
                EntityKind::Edge => {
                    for edge in &scene.edges {
                        let mid = Point3::new(
                            (edge.endpoints[0].x + edge.endpoints[1].x) * 0.5,
                            (edge.endpoints[0].y + edge.endpoints[1].y) * 0.5,
                            (edge.endpoints[0].z + edge.endpoints[1].z) * 0.5,
                        );
                        let dx = mid.x - point.x;
                        let dy = mid.y - point.y;
                        let dz = mid.z - point.z;
                        let d2 = dx * dx + dy * dy + dz * dz;
                        if d2 < best_dist_sq {
                            best_dist_sq = d2;
                            best_entity = Some(edge.source_edge.entity);
                        }
                    }
                }
                EntityKind::Vertex => {
                    for v in &scene.vertices {
                        let dx = v.point.x - point.x;
                        let dy = v.point.y - point.y;
                        let dz = v.point.z - point.z;
                        let d2 = dx * dx + dy * dy + dz * dz;
                        if d2 < best_dist_sq {
                            best_dist_sq = d2;
                            best_entity = Some(v.source_vertex.entity);
                        }
                    }
                }
                _ => {
                    return Err(ApiError::new(
                        ApiErrorCode::InvalidInput,
                        format!("NearestTo does not support entity kind {:?}", kind),
                    ));
                }
            }

            let id = best_entity.ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::SelectorNotFound,
                    format!("No entity of kind {:?} found near point {:?}", kind, point),
                )
            })?;

            Ok(EntityRef {
                snapshot: current_snapshot.id(),
                entity: id,
                kind: *kind,
            })
        }
        GeometricSelector::EdgeBetween { face_a, face_b } => {
            let ref_a = resolve_selector(face_a, current_snapshot, step_order, step_reports)?;
            let ref_b = resolve_selector(face_b, current_snapshot, step_order, step_reports)?;

            for edge in &scene.edges {
                let incidents = edge.incident_faces;
                let has_a = incidents
                    .iter()
                    .any(|f| f.is_some_and(|r| r.entity == ref_a.entity));
                let has_b = incidents
                    .iter()
                    .any(|f| f.is_some_and(|r| r.entity == ref_b.entity));
                if has_a && has_b {
                    return Ok(EntityRef {
                        snapshot: current_snapshot.id(),
                        entity: edge.source_edge.entity,
                        kind: EntityKind::Edge,
                    });
                }
            }

            Err(ApiError::new(
                ApiErrorCode::SelectorNotFound,
                format!(
                    "No shared edge found between face {:?} and face {:?}",
                    ref_a.entity, ref_b.entity
                ),
            ))
        }
        GeometricSelector::ByType {
            surface_type: _,
            kind,
        } => {
            if *kind != EntityKind::Face {
                return Err(ApiError::new(
                    ApiErrorCode::InvalidInput,
                    "ByType only supports Face entities",
                ));
            }
            if scene.triangles.is_empty() {
                return Err(ApiError::new(
                    ApiErrorCode::SelectorNotFound,
                    "No faces found in current snapshot",
                ));
            }
            let face_id = scene.triangles[0].source_face.entity;
            Ok(EntityRef {
                snapshot: current_snapshot.id(),
                entity: face_id,
                kind: EntityKind::Face,
            })
        }
        GeometricSelector::ByExtremum {
            metric: _,
            extremum,
            kind,
        } => {
            if *kind != EntityKind::Face {
                return Err(ApiError::new(
                    ApiErrorCode::InvalidInput,
                    "ByExtremum currently supports Face entities",
                ));
            }
            let mut face_areas: BTreeMap<EntityId, f64> = BTreeMap::new();
            for tri in &scene.triangles {
                let v0 = tri.vertices[0];
                let v1 = tri.vertices[1];
                let v2 = tri.vertices[2];
                let ax = v1.x - v0.x;
                let ay = v1.y - v0.y;
                let az = v1.z - v0.z;
                let bx = v2.x - v0.x;
                let by = v2.y - v0.y;
                let bz = v2.z - v0.z;
                let cx = ay * bz - az * by;
                let cy = az * bx - ax * bz;
                let cz = ax * by - ay * bx;
                let area = (cx * cx + cy * cy + cz * cz).sqrt() * 0.5;
                *face_areas.entry(tri.source_face.entity).or_insert(0.0) += area;
            }

            if face_areas.is_empty() {
                return Err(ApiError::new(
                    ApiErrorCode::SelectorNotFound,
                    "No faces found for extremum query",
                ));
            }

            let best_face = match extremum {
                Extremum::Maximum => face_areas
                    .into_iter()
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(id, _)| id),
                Extremum::Minimum => face_areas
                    .into_iter()
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(id, _)| id),
            };

            let face_id = best_face.ok_or_else(|| {
                ApiError::new(
                    ApiErrorCode::SelectorNotFound,
                    "No face satisfied the extremum condition",
                )
            })?;

            Ok(EntityRef {
                snapshot: current_snapshot.id(),
                entity: face_id,
                kind: EntityKind::Face,
            })
        }
    }
}
