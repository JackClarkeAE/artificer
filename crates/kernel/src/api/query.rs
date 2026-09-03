//! Read-only inspection and geometric query interface.

use std::collections::BTreeMap;

use crate::{EdgeDescription, FaceDescription, NativeKernel};
use artificer_protocol::{Aabb3, EntityId, EntityKind, Point3, SnapshotId, TopologyCounts};
use serde::{Deserialize, Serialize};

use crate::api::debug::{ApiError, ApiErrorCode, EntityInfo};
use crate::api::selectors::{EntitySelector, resolve_selector};
use crate::api::session::Session;

/// What a selected entity is, read off its exact carrier.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntityDescription {
    Face(FaceDescription),
    Edge(EdgeDescription),
}

impl EntityDescription {
    /// The entity in words.
    #[must_use]
    pub fn summary(&self) -> &str {
        match self {
            Self::Face(face) => &face.summary,
            Self::Edge(edge) => &edge.summary,
        }
    }
}

/// Summary information for a body in the current session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BodyInfo {
    pub label: String,
    pub snapshot_id: SnapshotId,
    pub topology: TopologyCounts,
    pub bounds: Option<Aabb3>,
}

/// Topology details for all faces, edges, and vertices.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TopologyInfo {
    pub faces: Vec<EntityInfo>,
    pub edges: Vec<EntityInfo>,
    pub vertices: Vec<EntityInfo>,
}

/// A target for distance or dimension measurement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MeasureTarget {
    Entity(EntitySelector),
    Point(Point3),
}

/// Result of a geometric measurement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Measurement {
    pub distance: f64,
    pub from_description: String,
    pub to_description: String,
}

/// Description of a parametric feature step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureInfo {
    pub index: usize,
    pub label: String,
    pub command_type: String,
    pub active: bool,
}

/// Query handle providing read-only inspection methods for a session.
pub struct QueryHandle<'a> {
    session: &'a Session,
}

impl<'a> QueryHandle<'a> {
    #[must_use]
    pub fn new(session: &'a Session) -> Self {
        Self { session }
    }

    #[must_use]
    pub fn bodies(&self) -> Vec<BodyInfo> {
        let counts = self.session.snapshot.counts();
        let bounds = self.session.snapshot.measures().bounds;
        vec![BodyInfo {
            label: "default".to_owned(),
            snapshot_id: self.session.snapshot.id(),
            topology: counts,
            bounds,
        }]
    }

    pub fn topology(&self) -> Result<TopologyInfo, ApiError> {
        let scene = NativeKernel::debug_scene(&self.session.snapshot);
        let mut face_entities: BTreeMap<EntityId, EntityInfo> = BTreeMap::new();
        for tri in &scene.triangles {
            face_entities
                .entry(tri.source_face.entity)
                .or_insert_with(|| EntityInfo {
                    kind: EntityKind::Face,
                    entity_ref: tri.source_face,
                    geometry_description: format!("Face {}", tri.source_face.entity),
                    role: None,
                    ordinal: None,
                });
        }

        let mut edge_entities: BTreeMap<EntityId, EntityInfo> = BTreeMap::new();
        for edge in &scene.edges {
            edge_entities
                .entry(edge.source_edge.entity)
                .or_insert_with(|| EntityInfo {
                    kind: EntityKind::Edge,
                    entity_ref: edge.source_edge,
                    geometry_description: format!("Edge {}", edge.source_edge.entity),
                    role: None,
                    ordinal: None,
                });
        }

        let mut vertex_entities: BTreeMap<EntityId, EntityInfo> = BTreeMap::new();
        for v in &scene.vertices {
            vertex_entities
                .entry(v.source_vertex.entity)
                .or_insert_with(|| EntityInfo {
                    kind: EntityKind::Vertex,
                    entity_ref: v.source_vertex,
                    geometry_description: format!("Vertex {}", v.source_vertex.entity),
                    role: None,
                    ordinal: None,
                });
        }

        Ok(TopologyInfo {
            faces: face_entities.into_values().collect(),
            edges: edge_entities.into_values().collect(),
            vertices: vertex_entities.into_values().collect(),
        })
    }

    pub fn entity_info(&self, selector: &EntitySelector) -> Result<EntityInfo, ApiError> {
        let entity_ref = resolve_selector(
            selector,
            &self.session.snapshot,
            &self.session.step_order,
            &self.session.step_reports,
        )?;

        let role_name = match selector {
            EntitySelector::ByHistory { role, ordinal, .. } => Some(format!(
                "{role}{}",
                ordinal.map_or(String::new(), |o| format!("[{o}]"))
            )),
            _ => None,
        };

        Ok(EntityInfo {
            kind: entity_ref.kind,
            entity_ref,
            geometry_description: format!("{:?} {}", entity_ref.kind, entity_ref.entity),
            role: role_name,
            ordinal: None,
        })
    }

    /// Describes the face or edge a selector names: its carrier with the
    /// numbers that define it, exact area or length, and a centre.
    pub fn describe(&self, selector: &EntitySelector) -> Result<EntityDescription, ApiError> {
        let entity = resolve_selector(
            selector,
            &self.session.snapshot,
            &self.session.step_order,
            &self.session.step_reports,
        )?;
        match entity.kind {
            EntityKind::Face => NativeKernel::describe_face(&self.session.snapshot, entity)
                .map(EntityDescription::Face)
                .map_err(ApiError::from),
            EntityKind::Edge => NativeKernel::describe_edge(&self.session.snapshot, entity)
                .map(EntityDescription::Edge)
                .map_err(ApiError::from),
            other => Err(ApiError::new(
                ApiErrorCode::InvalidInput,
                format!("Only faces and edges are described; the selector named a {other:?}"),
            )),
        }
    }

    pub fn measure(
        &self,
        from: &MeasureTarget,
        to: &MeasureTarget,
    ) -> Result<Measurement, ApiError> {
        let p_from = self.target_point(from)?;
        let p_to = self.target_point(to)?;

        let dx = p_to.x - p_from.x;
        let dy = p_to.y - p_from.y;
        let dz = p_to.z - p_from.z;
        let distance = (dx * dx + dy * dy + dz * dz).sqrt();

        Ok(Measurement {
            distance,
            from_description: format!("{p_from:?}"),
            to_description: format!("{p_to:?}"),
        })
    }

    fn target_point(&self, target: &MeasureTarget) -> Result<Point3, ApiError> {
        match target {
            MeasureTarget::Point(p) => Ok(*p),
            MeasureTarget::Entity(sel) => {
                let entity_ref = resolve_selector(
                    sel,
                    &self.session.snapshot,
                    &self.session.step_order,
                    &self.session.step_reports,
                )?;
                let scene = NativeKernel::debug_scene(&self.session.snapshot);
                match entity_ref.kind {
                    EntityKind::Vertex => {
                        let v = scene
                            .vertices
                            .iter()
                            .find(|v| v.source_vertex.entity == entity_ref.entity)
                            .ok_or_else(|| {
                                ApiError::new(
                                    ApiErrorCode::SelectorNotFound,
                                    "Vertex position unavailable",
                                )
                            })?;
                        Ok(v.point)
                    }
                    EntityKind::Edge => {
                        let edge = scene
                            .edges
                            .iter()
                            .find(|e| e.source_edge.entity == entity_ref.entity)
                            .ok_or_else(|| {
                                ApiError::new(
                                    ApiErrorCode::SelectorNotFound,
                                    "Edge position unavailable",
                                )
                            })?;
                        Ok(Point3::new(
                            (edge.endpoints[0].x + edge.endpoints[1].x) * 0.5,
                            (edge.endpoints[0].y + edge.endpoints[1].y) * 0.5,
                            (edge.endpoints[0].z + edge.endpoints[1].z) * 0.5,
                        ))
                    }
                    EntityKind::Face => {
                        let mut sum_p = Point3::new(0.0, 0.0, 0.0);
                        let mut count = 0usize;
                        for tri in &scene.triangles {
                            if tri.source_face.entity == entity_ref.entity {
                                sum_p.x +=
                                    (tri.vertices[0].x + tri.vertices[1].x + tri.vertices[2].x)
                                        / 3.0;
                                sum_p.y +=
                                    (tri.vertices[0].y + tri.vertices[1].y + tri.vertices[2].y)
                                        / 3.0;
                                sum_p.z +=
                                    (tri.vertices[0].z + tri.vertices[1].z + tri.vertices[2].z)
                                        / 3.0;
                                count += 1;
                            }
                        }
                        if count == 0 {
                            return Err(ApiError::new(
                                ApiErrorCode::SelectorNotFound,
                                "Face geometry unavailable",
                            ));
                        }
                        let cf = count as f64;
                        Ok(Point3::new(sum_p.x / cf, sum_p.y / cf, sum_p.z / cf))
                    }
                    _ => Err(ApiError::new(
                        ApiErrorCode::InvalidInput,
                        format!(
                            "Cannot measure position of entity kind {:?}",
                            entity_ref.kind
                        ),
                    )),
                }
            }
        }
    }

    pub fn bounds(&self) -> Result<Aabb3, ApiError> {
        self.session
            .snapshot
            .measures()
            .bounds
            .ok_or_else(|| ApiError::new(ApiErrorCode::KernelError, "Model bounds unavailable"))
    }

    #[must_use]
    pub fn features(&self) -> Vec<FeatureInfo> {
        self.session
            .journal
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| FeatureInfo {
                index,
                label: entry.label.clone(),
                command_type: format!("{:?}", entry.command),
                active: true,
            })
            .collect()
    }
}
