//! Discovering the relationships a part was built to, and re-solving to
//! them.
//!
//! Every surface here is fitted on its own evidence, so two walls a
//! machinist set parallel come back 0.2 degrees apart and a bore that was
//! reamed to 42 comes back at 42.003. Neither error is large and both are
//! wrong in a way that matters: a model whose faces are *nearly* square
//! cannot be dimensioned, cannot be edited, and will not sew.
//!
//! What a designer actually specified is a small set of directions — a
//! frame — with every face either along one or across it. Recovering
//! that frame is worth more than any individual fit, because it corrects
//! every member at once and it is the thing a drawing is written in.
//!
//! **The constraint has to be tested, never assumed.** This pipeline has
//! already paid for that lesson once: the gear's hub cones run a genuine
//! 0.37 mm eccentric to its bore, and forcing them concentric — an
//! entirely reasonable-looking assumption — inflated their deviation from
//! 0.06 mm to 0.29 and lost 2,600 mm² of surface. So each feature is
//! offered its frame and refitted against its own samples with that
//! direction locked; it joins only if it still explains the scan it came
//! from. A part that really is skew stays skew, and says so.

use artificer_geometry::{Point3, Vector3};

use crate::datum::DatumAlignment;
use crate::fit::{PlaneFit, fit_cylinder_with_axis};
use crate::mesh::TriangleMesh;
use crate::report::FeatureRecord;
use crate::segment::SurfaceClass;

/// How far off square a relationship may be and still be read as one the
/// part was built to. Beyond this the surfaces are simply at an angle.
const ANGLE_TOLERANCE_DEG: f64 = 1.5;
/// A feature must reach this area (mm²) to vote on a frame. Slivers
/// carry directions fitted to almost nothing.
const MIN_VOTING_AREA: f64 = 30.0;
/// A locked refit may be this much worse than the free one before the
/// relationship is judged to be absent rather than merely tight.
const SLACK: f64 = 1.6;

/// A right-handed frame of three directions, and what it explains.
pub struct Frame {
    pub axes: [Vector3; 3],
    /// Feature ids that refitted successfully to this frame.
    pub members: Vec<usize>,
    /// Their total area (mm²).
    pub area: f64,
    /// The largest angular correction applied, in degrees.
    pub worst_correction: f64,
}

pub struct ConstrainOutcome {
    pub frames: Vec<Frame>,
    /// Features offered a frame that refused it — genuinely skew.
    pub refused: usize,
    pub refused_area: f64,
}

/// The direction that characterises a surface, if it has one.
fn direction(surface: &SurfaceClass) -> Option<Vector3> {
    match surface {
        SurfaceClass::Plane(fit) => Some(fit.normal),
        SurfaceClass::Cylinder(fit) => Some(fit.axis),
        SurfaceClass::Cone(fit) => Some(fit.axis),
        SurfaceClass::Blend(fit) => Some(fit.axis),
        _ => None,
    }
}

/// Whether two directions are square to each other: parallel, or at a
/// right angle. Both are the same relationship as far as a frame is
/// concerned — they say the two surfaces share one.
fn square(a: Vector3, b: Vector3, cos_tolerance: f64, sin_tolerance: f64) -> bool {
    let dot = a.dot(b).abs();
    dot > cos_tolerance || dot < sin_tolerance
}

/// Groups features whose directions are square to one another, directly
/// or through a chain of others.
fn groups(
    entries: &[(usize, Vector3, f64)],
    cos_tolerance: f64,
    sin_tolerance: f64,
) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..entries.len()).collect();
    fn root(parent: &mut [usize], mut node: usize) -> usize {
        while parent[node] != node {
            parent[node] = parent[parent[node]];
            node = parent[node];
        }
        node
    }
    for i in 0..entries.len() {
        for j in (i + 1)..entries.len() {
            if square(entries[i].1, entries[j].1, cos_tolerance, sin_tolerance) {
                let (ri, rj) = (root(&mut parent, i), root(&mut parent, j));
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }
    let mut buckets: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for index in 0..entries.len() {
        let key = root(&mut parent, index);
        buckets.entry(key).or_default().push(index);
    }
    let mut out: Vec<Vec<usize>> = buckets.into_values().collect();
    // Deterministic order: biggest first, then by first member.
    out.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[0].cmp(&b[0])));
    out
}

/// Builds the best right-handed frame for a group of directions.
///
/// The frame is grown from the heaviest direction rather than averaged
/// blindly, because a group's members disagree about which of the three
/// axes each of them is: only once every direction has been assigned to
/// an axis can the axes themselves be improved. Assignment and averaging
/// then alternate, which is the same shape as any alignment problem and
/// settles in a handful of passes.
fn frame_of(entries: &[(usize, Vector3, f64)], group: &[usize]) -> Option<[Vector3; 3]> {
    let inlier = ANGLE_TOLERANCE_DEG.to_radians().cos();
    let heaviest = group
        .iter()
        .copied()
        .max_by(|&a, &b| entries[a].2.total_cmp(&entries[b].2))?;
    let mut primary = entries[heaviest].1;
    // A second direction as far from the first as the group offers.
    let across = group
        .iter()
        .copied()
        .filter(|&index| entries[index].1.dot(primary).abs() < 0.7)
        .max_by(|&a, &b| entries[a].2.total_cmp(&entries[b].2));
    let mut secondary = match across {
        Some(index) => entries[index].1,
        None => {
            let aside = if primary.x.abs() < 0.9 {
                Vector3::new(1.0, 0.0, 0.0)
            } else {
                Vector3::new(0.0, 1.0, 0.0)
            };
            primary.cross(aside)
        }
    };
    for _ in 0..8 {
        // Orthonormalize the current guess.
        let length = primary.length();
        if length < 1e-9 {
            return None;
        }
        primary = primary / length;
        secondary = secondary - primary * secondary.dot(primary);
        let length = secondary.length();
        if length < 1e-9 {
            return None;
        }
        secondary = secondary / length;
        let third = primary.cross(secondary);
        let axes = [primary, secondary, third];
        // Re-average each axis over the directions nearest it.
        let mut sums = [Vector3::new(0.0, 0.0, 0.0); 3];
        for &index in group {
            let (_, direction, weight) = entries[index];
            let mut best = (0usize, 0.0);
            for (slot, axis) in axes.iter().enumerate() {
                let alignment = direction.dot(*axis).abs();
                if alignment > best.1 {
                    best = (slot, alignment);
                }
            }
            // Only the frame's own inliers get to refine it. Squareness
            // is transitive, so a group is a *chain* and not a consensus:
            // averaging an axis over all 524 directions the pump chained
            // together drags it somewhere generic that fits none of them,
            // and the frame then claims almost nothing. This is the same
            // distinction RANSAC draws between a candidate's support and
            // the whole point set.
            if best.1 <= inlier {
                continue;
            }
            let sign = if direction.dot(axes[best.0]) < 0.0 {
                -1.0
            } else {
                1.0
            };
            sums[best.0] = sums[best.0] + direction * (sign * weight);
        }
        if sums[0].length() > 1e-9 {
            primary = sums[0];
        }
        if sums[1].length() > 1e-9 {
            secondary = sums[1];
        } else {
            secondary = axes[1];
        }
    }
    let length = primary.length();
    if length < 1e-9 {
        return None;
    }
    let primary = primary / length;
    let secondary = secondary - primary * secondary.dot(primary);
    let length = secondary.length();
    if length < 1e-9 {
        return None;
    }
    let secondary = secondary / length;
    Some([primary, secondary, primary.cross(secondary)])
}

/// The frame axis a direction should be locked to, with its sign.
fn nearest_axis(axes: &[Vector3; 3], direction: Vector3) -> Vector3 {
    let mut best = (axes[0], 0.0);
    for axis in axes {
        let alignment = direction.dot(*axis).abs();
        if alignment > best.1 {
            best = (*axis, alignment);
        }
    }
    if direction.dot(best.0) < 0.0 {
        best.0 * -1.0
    } else {
        best.0
    }
}

/// Refits one feature with its direction locked, and reports the result
/// only if it still explains the feature's own samples.
fn locked_refit(
    surface: &SurfaceClass,
    axis: Vector3,
    points: &[Point3],
    tolerance: f64,
) -> Option<SurfaceClass> {
    match surface {
        SurfaceClass::Plane(_) => {
            let offset = points
                .iter()
                .map(|p| (*p - Point3::default()).dot(axis))
                .sum::<f64>()
                / (points.len().max(1) as f64);
            let origin = Point3::default() + axis * offset;
            let (mut sum, mut worst) = (0.0, 0.0f64);
            for point in points {
                let distance = (*point - origin).dot(axis);
                sum += distance * distance;
                worst = worst.max(distance.abs());
            }
            let rms = (sum / points.len().max(1) as f64).sqrt();
            if rms > tolerance * SLACK.max(1.0) || !rms.is_finite() {
                return None;
            }
            Some(SurfaceClass::Plane(PlaneFit {
                origin,
                normal: axis,
                deviation: crate::fit::DeviationStats {
                    rms,
                    max_abs: worst,
                },
            }))
        }
        SurfaceClass::Cylinder(fit) => {
            let locked = fit_cylinder_with_axis(points, axis)?;
            (locked.deviation.rms <= (fit.deviation.rms * SLACK).max(tolerance))
                .then_some(SurfaceClass::Cylinder(locked))
        }
        _ => None,
    }
}

/// Finds the frames a part was built to and re-solves its surfaces to
/// them, leaving alone every surface that disagrees.
pub fn constrain_features(
    mesh: &TriangleMesh,
    features: &mut [FeatureRecord],
    alignment: Option<&DatumAlignment>,
    tolerance: f64,
) -> ConstrainOutcome {
    let identity = crate::transform::RigidTransform::IDENTITY;
    let to_frame = alignment.map_or(&identity, |a| &a.transform);
    let entries: Vec<(usize, Vector3, f64)> = features
        .iter()
        .enumerate()
        .filter(|(_, feature)| feature.area >= MIN_VOTING_AREA)
        .filter_map(|(index, feature)| {
            direction(&feature.surface).map(|d| (index, d, feature.area))
        })
        .collect();
    let cos_tolerance = ANGLE_TOLERANCE_DEG.to_radians().cos();
    let sin_tolerance = ANGLE_TOLERANCE_DEG.to_radians().sin();
    let mut outcome = ConstrainOutcome {
        frames: Vec::new(),
        refused: 0,
        refused_area: 0.0,
    };
    for group in groups(&entries, cos_tolerance, sin_tolerance) {
        // Squareness is transitive, so a whole part usually collapses
        // into ONE group — on the pump, every direction chained into a
        // single component, one frame was built from it, and the six
        // surfaces that happened to sit near those three axes were all
        // it could claim. Every other frame in the part was lost.
        //
        // So frames are PEELED, not assigned: build the best frame the
        // remaining directions support, let it claim what is already
        // square to it, and ask the rest again. Anything offered a frame
        // leaves the pool whether it accepted or refused, which is what
        // makes this terminate.
        let mut pool = group;
        while pool.len() >= 2 {
            let (frame, claimed) = peel_frame(
                &entries,
                &pool,
                mesh,
                features,
                to_frame,
                tolerance,
                &mut outcome,
            );
            if claimed.is_empty() {
                break;
            }
            pool.retain(|index| !claimed.contains(index));
            if let Some(frame) = frame {
                outcome.frames.push(frame);
            }
        }
    }
    outcome.frames.sort_by(|a, b| b.area.total_cmp(&a.area));
    outcome
}

/// Builds one frame from the pool and returns it with every entry it
/// offered itself to — accepted or refused.
#[allow(clippy::too_many_arguments)]
fn peel_frame(
    entries: &[(usize, Vector3, f64)],
    pool: &[usize],
    mesh: &TriangleMesh,
    features: &mut [FeatureRecord],
    to_frame: &crate::transform::RigidTransform,
    tolerance: f64,
    outcome: &mut ConstrainOutcome,
) -> (Option<Frame>, std::collections::HashSet<usize>) {
    let mut offered = std::collections::HashSet::new();
    {
        let group: Vec<usize> = pool.to_vec();
        let Some(axes) = frame_of(entries, &group) else {
            return (None, offered);
        };
        let mut frame = Frame {
            axes,
            members: Vec::new(),
            area: 0.0,
            worst_correction: 0.0,
        };
        for &slot in &group {
            let (index, direction_now, area) = entries[slot];
            let axis = nearest_axis(&axes, direction_now);
            let correction = direction_now.dot(axis).clamp(-1.0, 1.0).acos().to_degrees();
            // Squareness chains: A is parallel to B, B is square to C,
            // and the group holds them all. That is what a frame IS, but
            // it means membership does not imply *this* feature was ever
            // near *this* axis — drift accumulates along the chain, and
            // the gear first reported a surface turned 13.9 degrees to
            // join. A frame may only claim what was already square to it.
            if correction > ANGLE_TOLERANCE_DEG {
                continue;
            }
            offered.insert(slot);
            let points: Vec<Point3> = features[index]
                .faces
                .iter()
                .flat_map(|&face| mesh.triangle_points(face as usize))
                .map(|corner| to_frame.apply_point(corner))
                .collect();
            if points.len() < 6 {
                continue;
            }
            match locked_refit(&features[index].surface, axis, &points, tolerance) {
                Some(surface) => {
                    features[index].surface = surface;
                    features[index].notes.push(format!(
                        "axis locked to a shared frame, turned {correction:.3} deg to reach it"
                    ));
                    frame.members.push(features[index].id);
                    frame.area += area;
                    frame.worst_correction = frame.worst_correction.max(correction);
                }
                None => {
                    // It was offered the relationship and its own samples
                    // said no. That is a finding, not a failure.
                    outcome.refused += 1;
                    outcome.refused_area += area;
                }
            }
        }
        ((frame.members.len() >= 2).then_some(frame), offered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fit::DeviationStats;

    fn plane(id: usize, normal: Vector3, offset: f64) -> FeatureRecord {
        FeatureRecord {
            id,
            surface: SurfaceClass::Plane(PlaneFit {
                origin: Point3::default() + normal * offset,
                normal,
                deviation: DeviationStats {
                    rms: 0.0,
                    max_abs: 0.0,
                },
            }),
            face_count: 0,
            area: 500.0,
            faces: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// Faces a degree off square must be pulled onto one frame.
    #[test]
    fn near_square_faces_share_a_frame() {
        let entries = vec![
            (0usize, Vector3::new(0.0, 0.0, 1.0), 900.0),
            (1, Vector3::new(0.9998, 0.0175, 0.0), 600.0),
            (2, Vector3::new(-0.0175, 0.9998, 0.0), 500.0),
        ];
        let cos = 1.5f64.to_radians().cos();
        let sin = 1.5f64.to_radians().sin();
        let found = groups(&entries, cos, sin);
        assert_eq!(found.len(), 1, "all three are square to one another");
        let axes = frame_of(&entries, &found[0]).expect("frame");
        for (a, b) in [(0, 1), (1, 2), (0, 2)] {
            assert!(
                axes[a].dot(axes[b]).abs() < 1e-9,
                "the frame must be orthogonal"
            );
        }
        // The heaviest direction anchors it.
        assert!(
            axes.iter()
                .any(|axis| axis.dot(entries[0].1).abs() > 0.9999),
            "the frame should keep the heaviest direction"
        );
    }

    /// A part that is genuinely skew must stay skew: a face at 30 degrees
    /// belongs to no frame the others share.
    #[test]
    fn a_skew_face_is_not_dragged_square() {
        let entries = vec![
            (0usize, Vector3::new(0.0, 0.0, 1.0), 900.0),
            (1, Vector3::new(1.0, 0.0, 0.0), 600.0),
            (2, Vector3::new(0.5, 0.0, 0.866), 500.0),
        ];
        let cos = 1.5f64.to_radians().cos();
        let sin = 1.5f64.to_radians().sin();
        let found = groups(&entries, cos, sin);
        let with_skew = found
            .iter()
            .find(|group| group.contains(&2))
            .expect("the skew face is somewhere");
        assert_eq!(
            with_skew.len(),
            1,
            "a 30 degree face shares a frame with nothing"
        );
    }

    /// The refit has the last word: a plane whose samples do not lie on
    /// the locked direction keeps its own.
    #[test]
    fn a_refit_that_fails_keeps_the_free_fit() {
        let mut features = vec![plane(0, Vector3::new(0.0, 0.0, 1.0), 0.0)];
        // Samples on a steeply sloped surface, nothing like z = 0.
        let points: Vec<Point3> = (0..20)
            .map(|i| Point3::new(i as f64, 0.0, i as f64 * 0.5))
            .collect();
        let locked = locked_refit(
            &features[0].surface,
            Vector3::new(0.0, 0.0, 1.0),
            &points,
            0.15,
        );
        assert!(locked.is_none(), "a bad locked fit must be refused");
        assert!(matches!(features.remove(0).surface, SurfaceClass::Plane(_)));
    }
}
