//! Ordering the recovered operations into a feature tree.
//!
//! Everything upstream produces an unordered bag: a revolved profile, a
//! toothing pattern, some extrude and revolve instances, fillets,
//! chamfers, and whatever had no analytic form. A CAD model is not a
//! bag. It is a sequence — a base solid, then material added and taken
//! away in an order that makes each step's references exist, with the
//! finishing operations last — and that sequence is what a designer
//! edits and what makes the model replayable at all.
//!
//! Two questions have to be answered from evidence rather than assumed.
//!
//! **Which way does a feature act?** A boss adds material and a bore
//! takes it away, and the surfaces themselves say which: a wall whose
//! measured normals point away from its own axis has material on the
//! inside and is a boss; one whose normals point inward is a bore. The
//! revolved-profile stage already reads bosses and bores exactly this
//! way, and the same test settles an extrusion by asking whether its
//! walls face away from the sketch's own centre.
//!
//! **What is the base?** The largest thing that can stand on its own —
//! the revolved stack if there is one, otherwise the biggest additive
//! instance. Everything else is cut or added into it. Where a casting
//! leaves surface that has no analytic form at all, the base is that
//! measured body, which is exactly the hybrid the commercial packages
//! build: an organic solid with machined features booleaned into it.
//!
//! Fillets and chamfers are last, always. They are finishing operations
//! that reference edges which only exist once the solids they round have
//! been combined, and a tree that applies them earlier is one that
//! cannot be replayed.

use artificer_geometry::{Point3, Vector3};

use crate::datum::DatumAlignment;
use crate::mesh::TriangleMesh;
use crate::report::FeatureRecord;

/// What a step does to the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The solid everything else is applied to.
    Base,
    /// Adds material: a boss, a pad, a stub.
    Add,
    /// Removes material: a bore, a pocket, a slot.
    Cut,
    /// Rounds or breaks an edge once the solids exist.
    Finish,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Base => "base",
            Role::Add => "add",
            Role::Cut => "cut",
            Role::Finish => "finish",
        }
    }
}

/// One operation in the tree, in the order it must be replayed.
#[derive(Clone, Debug)]
pub struct Step {
    pub role: Role,
    /// The operation kind, matching the history proposal's `type`.
    pub operation: String,
    /// Which entry of that kind, in the plan's own ordering.
    pub index: usize,
    /// A human-readable description for the report.
    pub label: String,
    /// Surface area the step accounts for (mm²), largest first within
    /// its role.
    pub area: f64,
}

#[derive(Clone, Debug, Default)]
pub struct FeatureTree {
    pub steps: Vec<Step>,
    /// Steps whose direction could not be read from the evidence and
    /// which defaulted to adding material.
    pub undecided: usize,
}

/// Whether a feature's material lies outside the axis (a boss) or
/// inside it (a bore), by the area-weighted agreement between its
/// measured normals and the outward radial direction.
///
/// `None` when the evidence does not favour either — a flat cap has no
/// radial opinion at all, and guessing from one would be inventing.
fn faces_outward(
    mesh: &TriangleMesh,
    feature: &FeatureRecord,
    axis_point: Point3,
    axis: Vector3,
    to_frame: &crate::transform::RigidTransform,
) -> Option<bool> {
    /// Below this agreement the surface is square to the axis rather
    /// than around it, and says nothing about inside or outside.
    const DECISIVE: f64 = 0.2;
    let stride = (feature.faces.len() / 500).max(1);
    let (mut sum, mut weight) = (0.0, 0.0);
    for &face in feature.faces.iter().step_by(stride) {
        let Some(normal) = mesh.face_normal(face as usize) else {
            continue;
        };
        let normal = to_frame.apply_vector(normal);
        let offset = to_frame.apply_point(mesh.face_centroid(face as usize)) - axis_point;
        let radial = offset - axis * offset.dot(axis);
        let length = radial.length();
        if length < 1e-9 {
            continue;
        }
        let area = mesh.face_area(face as usize);
        sum += area * normal.dot(radial / length);
        weight += area;
    }
    if weight <= 0.0 {
        return None;
    }
    let agreement = sum / weight;
    (agreement.abs() >= DECISIVE).then_some(agreement > 0.0)
}

/// Orders a plan's operations into a replayable tree.
pub fn order_tree(
    mesh: &TriangleMesh,
    features: &[FeatureRecord],
    plan: &crate::reconstruct::ReconstructionPlan,
    alignment: Option<&DatumAlignment>,
    organic_area: f64,
) -> FeatureTree {
    let identity = crate::transform::RigidTransform::IDENTITY;
    let to_frame = alignment.map_or(&identity, |a| &a.transform);
    let feature_by_id = |id: usize| features.iter().find(|f| f.id == id);
    let mut tree = FeatureTree::default();
    let mut solids: Vec<Step> = Vec::new();

    // The revolved stack, if there is one, is a single solid.
    if !plan.segments.is_empty() {
        let area: f64 = plan
            .segments
            .iter()
            .map(|segment| {
                std::f64::consts::TAU
                    * (segment.inner_radius + segment.outer_radius)
                    * (segment.z1 - segment.z0).abs()
            })
            .sum();
        solids.push(Step {
            role: Role::Add,
            operation: "make_revolved_annulus".to_owned(),
            index: 0,
            label: format!(
                "revolved profile of {} annulus segment(s)",
                plan.segments.len()
            ),
            area,
        });
    }
    // A repeated band — a gear's toothing — is one operation.
    if plan.pattern.is_some() {
        solids.push(Step {
            role: Role::Add,
            operation: "circular_pattern_proposal".to_owned(),
            index: 0,
            label: "circular pattern band".to_owned(),
            area: 0.0,
        });
    }
    // Cast or organic surface: a measured body the rest is cut into.
    if organic_area > 0.0 {
        solids.push(Step {
            role: Role::Add,
            operation: "measured_body".to_owned(),
            index: 0,
            label: "measured body (cast or organic surface, no analytic form)".to_owned(),
            area: organic_area,
        });
    }

    // Instances: direction read from their own walls.
    for (index, instance) in plan.instances.revolves.iter().enumerate() {
        let outward = instance
            .members
            .iter()
            .filter_map(|&id| feature_by_id(id))
            .filter_map(|feature| {
                faces_outward(mesh, feature, instance.axis_point, instance.axis, to_frame)
                    .map(|outward| (feature.area, outward))
            })
            .fold((0.0, 0.0), |(add, cut), (area, outward)| {
                if outward {
                    (add + area, cut)
                } else {
                    (add, cut + area)
                }
            });
        let role = if outward.0 == 0.0 && outward.1 == 0.0 {
            tree.undecided += 1;
            Role::Add
        } else if outward.0 >= outward.1 {
            Role::Add
        } else {
            Role::Cut
        };
        solids.push(Step {
            role,
            operation: "revolve_instance_proposal".to_owned(),
            index,
            label: format!(
                "revolve of {} surface(s) about ({:+.3} {:+.3} {:+.3})",
                instance.members.len(),
                instance.axis.x,
                instance.axis.y,
                instance.axis.z
            ),
            area: instance.area,
        });
    }
    for (index, instance) in plan.instances.extrusions.iter().enumerate() {
        // An extrusion's "axis" for this purpose is its sweep direction
        // through the sketch's own centre: walls facing away from that
        // centre bound a pad, walls facing in bound a pocket.
        let centre = if instance.lines.is_empty() {
            (0.0, 0.0)
        } else {
            let count = instance.lines.len() as f64 * 2.0;
            instance.lines.iter().fold((0.0, 0.0), |acc, line| {
                (
                    acc.0 + (line.from.0 + line.to.0) / count,
                    acc.1 + (line.from.1 + line.to.1) / count,
                )
            })
        };
        // Rebuild the sketch frame the instance used.
        let aside = if instance.direction.x.abs() < 0.9 {
            Vector3::new(1.0, 0.0, 0.0)
        } else {
            Vector3::new(0.0, 1.0, 0.0)
        };
        let across = instance.direction.cross(aside);
        let u = across / across.length();
        let v = instance.direction.cross(u);
        let axis_point = Point3::default() + u * centre.0 + v * centre.1;
        let outward = instance
            .members
            .iter()
            .filter_map(|&id| feature_by_id(id))
            .filter_map(|feature| {
                faces_outward(mesh, feature, axis_point, instance.direction, to_frame)
                    .map(|outward| (feature.area, outward))
            })
            .fold((0.0, 0.0), |(add, cut), (area, outward)| {
                if outward {
                    (add + area, cut)
                } else {
                    (add, cut + area)
                }
            });
        let role = if outward.0 == 0.0 && outward.1 == 0.0 {
            tree.undecided += 1;
            Role::Add
        } else if outward.0 >= outward.1 {
            Role::Add
        } else {
            Role::Cut
        };
        solids.push(Step {
            role,
            operation: "extrude_instance_proposal".to_owned(),
            index,
            label: format!(
                "extrude of {} surface(s) along ({:+.3} {:+.3} {:+.3}), {:.1} mm deep",
                instance.members.len(),
                instance.direction.x,
                instance.direction.y,
                instance.direction.z,
                instance.span.1 - instance.span.0
            ),
            area: instance.area,
        });
    }

    // The base is the largest additive solid; the rest keep their roles.
    solids.sort_by(|a, b| {
        // Adds before cuts, then largest first, then by kind and index
        // so the order never depends on anything but the geometry.
        let rank = |role: Role| match role {
            Role::Base | Role::Add => 0,
            Role::Cut => 1,
            Role::Finish => 2,
        };
        rank(a.role)
            .cmp(&rank(b.role))
            .then_with(|| b.area.total_cmp(&a.area))
            .then_with(|| a.operation.cmp(&b.operation))
            .then_with(|| a.index.cmp(&b.index))
    });
    if let Some(first) = solids.first_mut()
        && first.role == Role::Add
    {
        first.role = Role::Base;
    }
    tree.steps = solids;
    // Finishing last, always: these reference edges that exist only once
    // the solids above have been combined.
    for (index, fillet) in plan.fillets.iter().enumerate() {
        tree.steps.push(Step {
            role: Role::Finish,
            operation: "finish_edge_proposal".to_owned(),
            index,
            label: format!("fillet r {:.2} at z {:+.2}", fillet.radius, fillet.z),
            area: 0.0,
        });
    }
    for (index, chamfer) in plan.chamfers.iter().enumerate() {
        tree.steps.push(Step {
            role: Role::Finish,
            operation: "finish_edge_proposal".to_owned(),
            index: plan.fillets.len() + index,
            label: format!("chamfer {:.2} mm at z {:+.2}", chamfer.distance, chamfer.z),
            area: 0.0,
        });
    }
    tree
}

/// The tree as report lines.
pub fn tree_summary(tree: &FeatureTree) -> String {
    if tree.steps.is_empty() {
        return String::new();
    }
    let mut out = format!(
        "feature tree ({} step(s), in replay order):\n",
        tree.steps.len()
    );
    for (order, step) in tree.steps.iter().enumerate() {
        out.push_str(&format!(
            "  {:>2}. {:<6} {}\n",
            order + 1,
            step.role.label(),
            step.label
        ));
    }
    if tree.undecided > 0 {
        out.push_str(&format!(
            "  ({} step(s) had no inside/outside evidence and default to adding material)\n",
            tree.undecided
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::{CylinderFit, DeviationStats};
    use crate::instance::{Instances, RevolveInstance};
    use crate::reconstruct::ReconstructionPlan;
    use crate::segment::SurfaceClass;
    use crate::synth;

    /// A bore's walls face inward, so it must be read as a cut — and a
    /// boss's walls face outward, so it must be read as an addition.
    /// The same geometry with its normals reversed is the other one.
    #[test]
    fn a_bore_cuts_and_a_boss_adds() {
        let (radius, height) = (5.0, 12.0);
        let outward = synth::open_cylinder_soup(radius, height, 64, 6);
        // A bore is the same wall with its triangles wound the other way.
        let inward: Vec<[Point3; 3]> = outward
            .iter()
            .map(|triangle| [triangle[0], triangle[2], triangle[1]])
            .collect();
        for (soup, expect_add) in [(outward, true), (inward, false)] {
            let mesh = TriangleMesh::from_triangle_soup(&soup, 1e-6).expect("mesh");
            let faces: Vec<u32> = (0..mesh.triangles().len() as u32).collect();
            let features = vec![FeatureRecord {
                id: 0,
                surface: SurfaceClass::Cylinder(CylinderFit {
                    axis_point: Point3::default(),
                    axis: Vector3::new(0.0, 0.0, 1.0),
                    radius,
                    deviation: DeviationStats {
                        rms: 0.0,
                        max_abs: 0.0,
                    },
                }),
                face_count: faces.len(),
                area: std::f64::consts::TAU * radius * height,
                faces,
                notes: Vec::new(),
            }];
            let plan = ReconstructionPlan {
                instances: Instances {
                    revolves: vec![RevolveInstance {
                        axis_point: Point3::default(),
                        axis: Vector3::new(0.0, 0.0, 1.0),
                        members: vec![0],
                        area: 100.0,
                        profile: Vec::new(),
                        residual: 0.0,
                    }],
                    ..Default::default()
                },
                ..Default::default()
            };
            let tree = order_tree(&mesh, &features, &plan, None, 0.0);
            assert_eq!(tree.steps.len(), 1);
            let role = tree.steps[0].role;
            if expect_add {
                assert_eq!(role, Role::Base, "a lone boss is the base solid");
            } else {
                assert_eq!(role, Role::Cut, "a bore removes material");
            }
            assert_eq!(tree.undecided, 0, "a wall always has a radial opinion");
        }
    }

    /// Fillets come last however the plan lists them, and the largest
    /// additive solid becomes the base.
    #[test]
    fn finishing_is_last_and_the_biggest_solid_is_the_base() {
        let mesh = TriangleMesh::from_triangle_soup(
            &synth::box_soup(Point3::default(), Vector3::new(4.0, 4.0, 4.0), 2),
            1e-6,
        )
        .expect("mesh");
        let plan = ReconstructionPlan {
            segments: vec![crate::reconstruct::ProfileSegment {
                z0: 0.0,
                z1: 10.0,
                inner_radius: 0.0,
                outer_radius: 20.0,
            }],
            fillets: vec![crate::reconstruct::FilletProposal {
                radius: 2.0,
                z: 5.0,
                at_radius: 20.0,
                matched_corner: true,
            }],
            ..Default::default()
        };
        let tree = order_tree(&mesh, &[], &plan, None, 0.0);
        assert_eq!(tree.steps.first().map(|step| step.role), Some(Role::Base));
        assert_eq!(tree.steps.last().map(|step| step.role), Some(Role::Finish));
    }
}
