//! Compilation of selected arrangement cells into exact kernel profiles.

use std::collections::{BTreeMap, BTreeSet};

use artificer_protocol::{PlanarLoop2, PlanarProfile2, PlanarRegion2, PrecisionPolicy};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ArrangementCell, ArrangementLoop, CurveIntersections, EvaluatedCurve2, FragmentEndpointKey,
    FragmentKey, RegionSignature, SketchArrangement, SketchPoint2, intersect_curves,
};

pub const MAX_PROFILE_REGIONS: usize = 32;
pub const MAX_PROFILE_LOOPS: usize = 128;
pub const MAX_PROFILE_CURVES: usize = 1_024;

#[derive(Clone, Debug, PartialEq)]
pub struct CompiledProfileSelection {
    pub profile: PlanarProfile2,
    pub selected_regions: Vec<RegionSignature>,
}

#[derive(Clone, Debug, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProfileCompileError {
    #[error("at least one arrangement region must be selected")]
    EmptySelection,
    #[error("the selected region no longer exists")]
    MissingRegion { signature: RegionSignature },
    #[error("selected cell union left an open or branching boundary")]
    OpenOrBranchingBoundary,
    /// The material touches itself at a point: a circle resting on a side, or
    /// cells that meet only at a corner. A face boundary that returns to the
    /// same junction is not a simple loop, so the kernel never sees it.
    #[error("selected cell union touches itself at a point")]
    PinchedBoundary,
    #[error("selected cell union produced a zero-area boundary")]
    ZeroAreaBoundary,
    #[error("a clockwise boundary was not contained by any material outer loop")]
    OrphanHole,
    #[error("profile resource `{resource}` requested {actual}, limit {limit}")]
    ResourceLimit {
        resource: &'static str,
        actual: usize,
        limit: usize,
    },
}

#[derive(Clone, Debug)]
struct DirectedUse {
    key: FragmentKey,
    curve: EvaluatedCurve2,
}

/// Compiles one or more selected minimal arrangement cells. Shared boundaries
/// cancel semantically, and every surviving curve is exported analytically.
pub fn compile_selected_profile(
    arrangement: &SketchArrangement,
    selected: &[RegionSignature],
    precision: &PrecisionPolicy,
) -> Result<CompiledProfileSelection, ProfileCompileError> {
    if selected.is_empty() {
        return Err(ProfileCompileError::EmptySelection);
    }
    let mut canonical_selection = selected.to_vec();
    canonical_selection.sort();
    canonical_selection.dedup();

    let mut cells = Vec::with_capacity(canonical_selection.len());
    for signature in &canonical_selection {
        cells.push(arrangement.cell(signature).ok_or_else(|| {
            ProfileCompileError::MissingRegion {
                signature: signature.clone(),
            }
        })?);
    }

    let surviving = cancel_shared_boundaries(&cells);
    if surviving.len() > MAX_PROFILE_CURVES {
        return Err(ProfileCompileError::ResourceLimit {
            resource: "curves",
            actual: surviving.len(),
            limit: MAX_PROFILE_CURVES,
        });
    }
    let loops = stitch_boundary_loops(surviving, precision)?;
    if loops.len() > MAX_PROFILE_LOOPS {
        return Err(ProfileCompileError::ResourceLimit {
            resource: "loops",
            actual: loops.len(),
            limit: MAX_PROFILE_LOOPS,
        });
    }

    let mut outers = Vec::new();
    let mut holes = Vec::new();
    for profile_loop in loops {
        if profile_loop.signed_area > 0.0 {
            outers.push(profile_loop);
        } else {
            holes.push(profile_loop);
        }
    }
    outers.sort_by(|first, second| {
        second
            .signed_area
            .total_cmp(&first.signed_area)
            .then_with(|| first.fragment_keys.cmp(&second.fragment_keys))
    });

    let mut region_holes: Vec<Vec<ArrangementLoop>> = vec![Vec::new(); outers.len()];
    for hole in holes {
        let sample = boundary_sample(&hole).ok_or(ProfileCompileError::OpenOrBranchingBoundary)?;
        let mut owner = None;
        for (outer_index, outer) in outers.iter().enumerate() {
            if point_in_analytic_loop(sample, outer, precision) {
                let area = outer.signed_area;
                if owner.is_none_or(|(_, owner_area)| area < owner_area) {
                    owner = Some((outer_index, area));
                }
            }
        }
        let Some((owner_index, _)) = owner else {
            return Err(ProfileCompileError::OrphanHole);
        };
        region_holes[owner_index].push(hole);
    }

    if outers.len() > MAX_PROFILE_REGIONS {
        return Err(ProfileCompileError::ResourceLimit {
            resource: "regions",
            actual: outers.len(),
            limit: MAX_PROFILE_REGIONS,
        });
    }
    let mut regions = Vec::with_capacity(outers.len());
    for (outer, mut holes) in outers.into_iter().zip(region_holes) {
        holes.sort_by(|first, second| first.fragment_keys.cmp(&second.fragment_keys));
        regions.push(PlanarRegion2 {
            outer: compile_loop(&outer),
            holes: holes.iter().map(compile_loop).collect(),
        });
    }
    regions.sort_by_key(|region| planar_loop_key(&region.outer));
    Ok(CompiledProfileSelection {
        profile: PlanarProfile2 { regions },
        selected_regions: canonical_selection,
    })
}

fn cancel_shared_boundaries(cells: &[&ArrangementCell]) -> Vec<DirectedUse> {
    let mut uses: BTreeMap<FragmentKey, DirectedUse> = BTreeMap::new();
    for cell in cells {
        for profile_loop in std::iter::once(&cell.outer).chain(cell.holes.iter()) {
            for (key, curve) in profile_loop
                .fragment_keys
                .iter()
                .cloned()
                .zip(profile_loop.curves.iter().copied())
            {
                let reverse = key.reversed();
                if uses.remove(&reverse).is_none() {
                    uses.insert(key.clone(), DirectedUse { key, curve });
                }
            }
        }
    }
    uses.into_values().collect()
}

fn stitch_boundary_loops(
    uses: Vec<DirectedUse>,
    precision: &PrecisionPolicy,
) -> Result<Vec<ArrangementLoop>, ProfileCompileError> {
    let mut periodic = Vec::new();
    let mut ordinary = BTreeMap::new();
    let mut outgoing: BTreeMap<FragmentEndpointKey, Vec<FragmentKey>> = BTreeMap::new();
    for directed in uses {
        if directed.key.start == directed.key.end
            && matches!(directed.key.start, FragmentEndpointKey::PeriodicSeam { .. })
        {
            periodic.push(ArrangementLoop {
                half_edges: Vec::new(),
                curves: vec![directed.curve],
                fragment_keys: vec![directed.key],
                signed_area: directed.curve.signed_area_contribution(),
            });
            continue;
        }
        outgoing
            .entry(directed.key.start.clone())
            .or_default()
            .push(directed.key.clone());
        ordinary.insert(directed.key.clone(), directed);
    }
    if outgoing.values().any(|outputs| outputs.len() > 1) {
        return Err(ProfileCompileError::PinchedBoundary);
    }
    for outputs in outgoing.values_mut() {
        outputs.sort();
    }

    let mut unused: BTreeSet<_> = ordinary.keys().cloned().collect();
    let mut loops = periodic;
    while let Some(start_key) = unused.iter().next().cloned() {
        let start_endpoint = start_key.start.clone();
        let mut current_key = start_key;
        let mut keys = Vec::new();
        let mut curves = Vec::new();
        for _ in 0..=ordinary.len() {
            if !unused.remove(&current_key) {
                return Err(ProfileCompileError::OpenOrBranchingBoundary);
            }
            let directed = &ordinary[&current_key];
            keys.push(directed.key.clone());
            curves.push(directed.curve);
            let destination = directed.key.end.clone();
            if destination == start_endpoint {
                break;
            }
            let Some(next) = outgoing.get(&destination).and_then(|items| items.first()) else {
                return Err(ProfileCompileError::OpenOrBranchingBoundary);
            };
            current_key = next.clone();
        }
        if keys.last().map(|key| &key.end) != Some(&start_endpoint) {
            return Err(ProfileCompileError::OpenOrBranchingBoundary);
        }
        let signed_area: f64 = curves
            .iter()
            .map(|curve| curve.signed_area_contribution())
            .sum();
        if signed_area.abs() <= precision.min_feature_size * precision.min_feature_size {
            return Err(ProfileCompileError::ZeroAreaBoundary);
        }
        loops.push(ArrangementLoop {
            half_edges: Vec::new(),
            curves,
            fragment_keys: keys,
            signed_area,
        });
    }
    Ok(loops)
}

fn compile_loop(profile_loop: &ArrangementLoop) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: profile_loop
            .curves
            .iter()
            .copied()
            .map(EvaluatedCurve2::to_planar_curve)
            .collect(),
    }
}

fn boundary_sample(profile_loop: &ArrangementLoop) -> Option<SketchPoint2> {
    let curve = *profile_loop.curves.first()?;
    curve
        .evaluate(if curve.is_periodic() { 0.125 } else { 0.5 })
        .ok()
}

fn point_in_analytic_loop(
    point: SketchPoint2,
    profile_loop: &ArrangementLoop,
    precision: &PrecisionPolicy,
) -> bool {
    let max_u = profile_loop
        .curves
        .iter()
        .map(|curve| curve.bounds().max.u)
        .fold(point.u + 1.0, f64::max);
    let ray = EvaluatedCurve2::Line {
        start: point,
        end: SketchPoint2::new(max_u + (max_u - point.u).abs().max(1.0) * 2.0, point.v),
    };
    let mut parameters = Vec::new();
    for curve in &profile_loop.curves {
        if let CurveIntersections::Points { intersections } =
            intersect_curves(ray, *curve, precision)
        {
            parameters.extend(
                intersections
                    .into_iter()
                    .filter(|intersection| {
                        intersection.first_parameter > precision.parameter_resolution
                    })
                    .map(|intersection| intersection.first_parameter),
            );
        }
    }
    parameters.sort_by(f64::total_cmp);
    parameters.dedup_by(|second, first| {
        (*first - *second).abs() <= precision.parameter_resolution.max(f64::EPSILON * 64.0)
    });
    parameters.len() % 2 == 1
}

fn planar_loop_key(profile_loop: &PlanarLoop2) -> Vec<(u8, u64, u64)> {
    profile_loop
        .curves
        .iter()
        .map(|curve| match curve {
            artificer_protocol::PlanarCurve2::Line { start, end } => (
                0,
                start.x.to_bits() ^ start.y.to_bits(),
                end.x.to_bits() ^ end.y.to_bits(),
            ),
            artificer_protocol::PlanarCurve2::CircularArc { start, end, .. } => (
                1,
                start.x.to_bits() ^ start.y.to_bits(),
                end.x.to_bits() ^ end.y.to_bits(),
            ),
            artificer_protocol::PlanarCurve2::Circle { center, radius, .. } => {
                (2, center.x.to_bits() ^ center.y.to_bits(), radius.to_bits())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ArrangementInputCurve, ArrangementLimits, CurveDirection, SketchEntityId, SketchPointId,
        build_arrangement,
    };

    fn pid(raw: u64) -> SketchPointId {
        SketchPointId::new(raw).unwrap()
    }

    fn eid(raw: u64) -> SketchEntityId {
        SketchEntityId::new(raw).unwrap()
    }

    fn rectangle(
        entity_base: u64,
        point_base: u64,
        x0: f64,
        x1: f64,
    ) -> Vec<ArrangementInputCurve> {
        let points = [
            SketchPoint2::new(x0, 0.0),
            SketchPoint2::new(x1, 0.0),
            SketchPoint2::new(x1, 2.0),
            SketchPoint2::new(x0, 2.0),
        ];
        (0..4)
            .map(|index| {
                ArrangementInputCurve::line(
                    eid(entity_base + index as u64),
                    pid(point_base + index as u64),
                    pid(point_base + ((index + 1) % 4) as u64),
                    points[index],
                    points[(index + 1) % 4],
                )
            })
            .collect()
    }

    #[test]
    fn circle_compiles_without_polygonization() {
        let precision = PrecisionPolicy::default();
        let arrangement = build_arrangement(
            &[ArrangementInputCurve::circle(
                eid(1),
                SketchPoint2::new(0.0, 0.0),
                2.0,
                CurveDirection::CounterClockwise,
            )],
            &precision,
            ArrangementLimits::default(),
        );
        let compiled = compile_selected_profile(
            &arrangement,
            &[arrangement.cells[0].signature.clone()],
            &precision,
        )
        .unwrap();
        assert_eq!(compiled.profile.regions.len(), 1);
        assert!(matches!(
            compiled.profile.regions[0].outer.curves[0],
            artificer_protocol::PlanarCurve2::Circle { .. }
        ));
    }

    #[test]
    fn adjacent_cells_cancel_their_shared_fragment() {
        let precision = PrecisionPolicy::default();
        let mut curves = rectangle(1, 1, 0.0, 4.0);
        curves.push(ArrangementInputCurve::line(
            eid(10),
            pid(10),
            pid(11),
            SketchPoint2::new(2.0, 0.0),
            SketchPoint2::new(2.0, 2.0),
        ));
        let arrangement = build_arrangement(&curves, &precision, ArrangementLimits::default());
        assert_eq!(arrangement.cells.len(), 2, "{:?}", arrangement.diagnostics);
        let selected: Vec<_> = arrangement
            .cells
            .iter()
            .map(|cell| cell.signature.clone())
            .collect();
        let compiled = compile_selected_profile(&arrangement, &selected, &precision).unwrap();
        assert_eq!(compiled.profile.regions.len(), 1);
        assert_eq!(compiled.profile.curve_count(), 6);
    }

    #[test]
    fn both_halves_of_a_chord_split_disc_compile_to_the_whole_disc() {
        // The chord cancels; the two arcs are different fragments of the
        // same circle and must both survive, not cancel each other.
        let precision = PrecisionPolicy::default();
        let curves = [
            ArrangementInputCurve::circle(
                eid(1),
                SketchPoint2::new(0.0, 0.0),
                2.0,
                CurveDirection::CounterClockwise,
            ),
            ArrangementInputCurve::line(
                eid(2),
                pid(1),
                pid(2),
                SketchPoint2::new(-3.0, 0.5),
                SketchPoint2::new(3.0, 0.5),
            ),
        ];
        let arrangement = build_arrangement(&curves, &precision, ArrangementLimits::default());
        assert_eq!(arrangement.cells.len(), 2, "{:?}", arrangement.diagnostics);
        let selected: Vec<_> = arrangement
            .cells
            .iter()
            .map(|cell| cell.signature.clone())
            .collect();
        let compiled = compile_selected_profile(&arrangement, &selected, &precision).unwrap();
        assert_eq!(compiled.profile.regions.len(), 1);
        assert_eq!(compiled.profile.curve_count(), 2);
        assert!(compiled.profile.regions[0].holes.is_empty());
        let area: f64 = compiled.profile.regions[0]
            .outer
            .curves
            .iter()
            .map(|curve| match *curve {
                artificer_protocol::PlanarCurve2::CircularArc {
                    center,
                    start,
                    end,
                    direction,
                } => EvaluatedCurve2::CircularArc {
                    center: SketchPoint2::new(center.x, center.y),
                    start: SketchPoint2::new(start.x, start.y),
                    end: SketchPoint2::new(end.x, end.y),
                    direction: match direction {
                        artificer_protocol::ArcDirection::CounterClockwise => {
                            CurveDirection::CounterClockwise
                        }
                        artificer_protocol::ArcDirection::Clockwise => CurveDirection::Clockwise,
                    },
                }
                .signed_area_contribution(),
                _ => panic!("the disc is bounded by arcs alone"),
            })
            .sum();
        assert!((area - 4.0 * std::f64::consts::PI).abs() < 1.0e-9, "{area}");
    }

    #[test]
    fn a_compiled_cell_chains_bit_exactly_where_an_arc_meets_a_line() {
        // The kernel reads a loop whose uses do not share endpoint bits as
        // open. An arc endpoint is trigonometry and a line's is arithmetic;
        // both must land on the junction's one point.
        let precision = PrecisionPolicy::default();
        let curves = [
            ArrangementInputCurve::circle(
                eid(1),
                SketchPoint2::new(0.3, -0.7),
                2.0,
                CurveDirection::CounterClockwise,
            ),
            ArrangementInputCurve::line(
                eid(2),
                pid(1),
                pid(2),
                SketchPoint2::new(-3.0, 0.5),
                SketchPoint2::new(3.0, 0.1),
            ),
        ];
        let arrangement = build_arrangement(&curves, &precision, ArrangementLimits::default());
        assert_eq!(arrangement.cells.len(), 2, "{:?}", arrangement.diagnostics);
        for cell in &arrangement.cells {
            let compiled = compile_selected_profile(
                &arrangement,
                std::slice::from_ref(&cell.signature),
                &precision,
            )
            .unwrap();
            let loop_curves = &compiled.profile.regions[0].outer.curves;
            assert_eq!(loop_curves.len(), 2);
            let endpoints = |curve: &artificer_protocol::PlanarCurve2| match *curve {
                artificer_protocol::PlanarCurve2::Line { start, end }
                | artificer_protocol::PlanarCurve2::CircularArc { start, end, .. } => (start, end),
                artificer_protocol::PlanarCurve2::Circle { .. } => panic!("no full circles here"),
            };
            for index in 0..loop_curves.len() {
                let (_, end) = endpoints(&loop_curves[index]);
                let (next_start, _) = endpoints(&loop_curves[(index + 1) % loop_curves.len()]);
                assert_eq!(end.x.to_bits(), next_start.x.to_bits());
                assert_eq!(end.y.to_bits(), next_start.y.to_bits());
            }
        }
    }
}
