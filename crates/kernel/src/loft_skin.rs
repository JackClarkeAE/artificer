//! A smooth loft through several planar sections (ADR 0050).
//!
//! Three sections or more are brought to one segment correspondence by the
//! rules the two-section loft uses (ADR 0049): each loop turned to start
//! where the rungs to its neighbour are shortest, whole circles cut where
//! their neighbour starts, and every loop then cut exactly at every position
//! along its length where any of the others has a vertex, so that all of
//! them have a piece for every vertex any has. The pieces in one place along
//! the loops make one column, and each column is one wall: a B-spline
//! surface through the column's pieces, each piece a row of its net.
//!
//! Along the loop a row is the piece itself as a B-spline over the unit
//! interval — a spline reparameterised, a line as itself, an arc as its cubic
//! fit within the linear agreement — raised to one degree and refined to one
//! knot vector with the other rows. Across the sections every column of the
//! net is the curve of degree `min(3, n − 1)` through the rows' control
//! points at the sections' chord-length parameters, so the wall passes
//! through every section exactly and is C² along the loft between them: a
//! cubic from four sections, a single quadratic through three. The first and
//! last sections are the caps; a wall's first column is the rung it shares
//! with the wall before it, the same curve to the bit, because both are
//! interpolated through the same section vertices.
//!
//! What is certified is what the two-section loft certifies — sections on
//! distinct planes, each beyond its neighbours', holes that pair, walls that
//! neither pinch nor cross — and one thing more a curved loft can do and a
//! ruled one cannot: turn back on itself between two sections, which is
//! refused by name.

use artificer_protocol::{LoftSection, PrecisionPolicy};

use crate::analytic_extrusion::{
    BoundaryUse, allocate_id, push_cap_face, push_edge, push_loop, push_vertex,
};
use crate::bspline::{SplineSurface, common_basis};
use crate::loft_sections::{
    Cap, LoftSectionsError, Piece, SectionLoop, beyond, cap, cap_pcurve, cut_at, harmonised,
    parse_section, point_in_polygon, polygon_crosses, positions, rebased, reversed_pcurve, rotated,
    section_edge, segment_distance,
};
use crate::topology::{
    Curve2, Curve3, Edge, EdgeKey, Face, FaceKey, FaceRole, Orientation, ParameterRange, Point2,
    Point3, Record, Shell, ShellKey, Solid, Surface, Topology, Vector3, VertexKey,
};

/// One chain of matched loops, one per section, as walls.
#[derive(Clone, Debug)]
struct SkinnedLoop {
    /// The first and the last section's pieces, one per wall: the caps'
    /// edges.
    first: Vec<Piece>,
    last: Vec<Piece>,
    walls: Vec<SplineSurface>,
}

/// A smooth loft checked and ready to build.
#[derive(Clone, Debug)]
pub(crate) struct ValidatedSkinnedLoft {
    caps: [Cap; 2],
    /// The outer chain first, then each chain of matched holes.
    loops: Vec<SkinnedLoop>,
}

pub(crate) fn validate_skinned_loft(
    sections: &[LoftSection],
    precision: PrecisionPolicy,
) -> Result<ValidatedSkinnedLoft, LoftSectionsError> {
    let count = sections.len();
    if count < 3 {
        return Err(LoftSectionsError::TooFewSections);
    }
    let minimum = precision
        .modeling_resolution
        .max(precision.min_feature_size);
    let mut parsed = sections
        .iter()
        .map(|section| parse_section(section, precision))
        .collect::<Result<Vec<_>, _>>()?;

    // The loft runs from the first section to the last, through the others
    // in order. At each section it runs from the one before toward the one
    // after, and the section's normal is turned that way, its loops reversed
    // if they wound the other way about it.
    let centroids = parsed
        .iter()
        .map(|section| section.outer.centroid)
        .collect::<Vec<_>>();
    let mut toward = Vec::with_capacity(count);
    for (index, section) in parsed.iter_mut().enumerate() {
        let direction = centroids[(index + 1).min(count - 1)] - centroids[index.saturating_sub(1)];
        let normal = section.frame.normal;
        if normal.dot(direction) < 0.0 {
            toward.push(normal * -1.0);
            section.outer = section.outer.reversed();
            section.holes = section.holes.iter().map(SectionLoop::reversed).collect();
        } else {
            toward.push(normal);
        }
    }
    for index in 0..count - 1 {
        let (low, high) = (&parsed[index], &parsed[index + 1]);
        let parallel = low.frame.normal.cross(high.frame.normal).length()
            <= precision.angular_agreement_radians;
        if parallel
            && (high.frame.origin - low.frame.origin)
                .dot(low.frame.normal)
                .abs()
                <= minimum
        {
            return Err(LoftSectionsError::Coplanar);
        }
        if !beyond(high, low, toward[index], 1.0, minimum)
            || !beyond(low, high, toward[index + 1], -1.0, minimum)
        {
            return Err(LoftSectionsError::CrossesPlane);
        }
    }

    // Holes pair from each section to the next by nearest centroid.
    let holes = parsed[0].holes.len();
    if parsed.iter().any(|section| section.holes.len() != holes) {
        return Err(LoftSectionsError::HoleCountMismatch);
    }
    let mut chains = vec![
        parsed
            .iter()
            .map(|section| section.outer.clone())
            .collect::<Vec<_>>(),
    ];
    for hole in &parsed[0].holes {
        chains.push(vec![hole.clone()]);
    }
    for index in 1..count {
        let mut unmatched = (0..holes).collect::<Vec<_>>();
        for chain in chains.iter_mut().skip(1) {
            let previous = chain[index - 1].centroid;
            let Some((slot, _)) = unmatched
                .iter()
                .enumerate()
                .min_by(|(_, left), (_, right)| {
                    let left = parsed[index].holes[**left].centroid.distance(previous);
                    let right = parsed[index].holes[**right].centroid.distance(previous);
                    left.total_cmp(&right)
                })
            else {
                return Err(LoftSectionsError::HoleCountMismatch);
            };
            chain.push(parsed[index].holes[unmatched.remove(slot)].clone());
        }
    }

    // Each section's place along the loft, by the chord lengths between the
    // outer loops' centroids.
    let mut parameters = vec![0.0];
    let mut walked = 0.0;
    for pair in centroids.windows(2) {
        walked += pair[1].distance(pair[0]);
        parameters.push(walked);
    }
    if walked.is_nan() || walked <= minimum {
        return Err(LoftSectionsError::Coplanar);
    }
    let parameters = parameters
        .iter()
        .enumerate()
        .map(|(index, length)| {
            if index + 1 == count {
                1.0
            } else {
                length / walked
            }
        })
        .collect::<Vec<_>>();

    let snap = 16.0 * minimum;
    let mut loops = Vec::with_capacity(chains.len());
    for chain in &chains {
        let pieces = correspond_all(chain, snap).ok_or(LoftSectionsError::RungsCross)?;
        let columns = pieces[0].len();
        let mut walls = Vec::with_capacity(columns);
        for column in 0..columns {
            let rows = pieces
                .iter()
                .map(|section| section[column].spline(precision.linear_agreement))
                .collect::<Option<Vec<_>>>()
                .and_then(|rows| common_basis(&rows))
                .ok_or(LoftSectionsError::WallDegenerate)?;
            let surface = SplineSurface::skinned(&rows, &parameters)
                .ok_or(LoftSectionsError::WallDegenerate)?;
            let scale = surface.scale().max(1.0);
            if surface.least_normal(surface.domain()) <= precision.linear_agreement * scale {
                return Err(LoftSectionsError::WallDegenerate);
            }
            walls.push(surface);
        }
        loops.push(SkinnedLoop {
            first: pieces[0].clone(),
            last: pieces[count - 1].clone(),
            walls,
        });
    }

    let direction = |v: f64| {
        let index = parameters
            .windows(2)
            .position(|pair| v <= pair[1])
            .unwrap_or(count - 2);
        let span = parameters[index + 1] - parameters[index];
        let t = ((v - parameters[index]) / span).clamp(0.0, 1.0);
        let blended = toward[index] * (1.0 - t) + toward[index + 1] * t;
        blended / blended.length()
    };
    skin_runs_forward(&loops, &parameters, &direction)?;
    skin_clear(&loops, &parameters, &direction, minimum)?;
    rungs_clear(&loops, minimum)?;

    let caps = [
        cap(parsed[0].frame, toward[0] * -1.0),
        cap(parsed[count - 1].frame, toward[count - 1]),
    ];
    Ok(ValidatedSkinnedLoft { caps, loops })
}

/// Every chain's loops cut to one correspondence, piece for piece.
///
/// The first loop with vertices of its own keeps its start. Each loop after
/// it is turned to start at whichever of its vertices makes it lie closest to
/// the loop before, and a whole circle is cut where its neighbour toward that
/// loop starts; circles before it are cut the same way working back. If every
/// loop is a whole circle they are cut at one direction from their centres,
/// as two circles are. Then every loop is cut at every position along its
/// length — a fraction of the length from its start — where any loop has a
/// vertex, a position within `snap` of one of its own counting as that one,
/// so that all come out with as many pieces. `None` when they do not.
fn correspond_all(loops: &[SectionLoop], snap: f64) -> Option<Vec<Vec<Piece>>> {
    let count = loops.len();
    let mut aligned: Vec<Vec<Piece>> = vec![Vec::new(); count];
    match loops.iter().position(|section| !section.full_circle) {
        None => {
            aligned[0] = loops[0].pieces.clone();
            let origin = arc_center(loops[0].pieces[0])?;
            let reach = aligned[0][0].start() - origin;
            for index in 1..count {
                let center = arc_center(loops[index].pieces[0])?;
                aligned[index] = vec![rebased(loops[index].pieces[0], center + reach)];
            }
        }
        Some(first) => {
            aligned[first] = loops[first].pieces.clone();
            for index in (0..first).rev() {
                let target = aligned[index + 1][0].start();
                aligned[index] = vec![rebased(loops[index].pieces[0], target)];
            }
            for index in first + 1..count {
                aligned[index] = if loops[index].full_circle {
                    vec![rebased(
                        loops[index].pieces[0],
                        aligned[index - 1][0].start(),
                    )]
                } else {
                    best_rotation(&loops[index].pieces, &aligned[index - 1])
                };
            }
        }
    }

    let totals = aligned
        .iter()
        .map(|pieces| pieces.iter().map(|piece| piece.length()).sum::<f64>())
        .collect::<Vec<_>>();
    let shortest = totals.iter().copied().fold(f64::INFINITY, f64::min);
    if shortest.is_nan() || shortest <= 0.0 {
        return None;
    }
    let tolerance = snap / shortest;
    let starts = aligned
        .iter()
        .map(|pieces| positions(pieces).0)
        .collect::<Vec<_>>();
    let mut union = starts
        .iter()
        .flat_map(|positions| positions.iter().skip(1).copied())
        .collect::<Vec<_>>();
    union.sort_by(f64::total_cmp);
    let mut merged: Vec<f64> = Vec::new();
    for position in union {
        if merged
            .last()
            .is_none_or(|last| position - *last > tolerance)
            && 1.0 - position > tolerance
        {
            merged.push(position);
        }
    }
    let mut cut = aligned
        .iter()
        .zip(&starts)
        .map(|(pieces, own)| {
            let cuts = merged
                .iter()
                .copied()
                .filter(|position| own.iter().all(|start| (start - position).abs() > tolerance))
                .collect::<Vec<_>>();
            cut_at(pieces, &cuts)
        })
        .collect::<Vec<_>>();
    let pieces = cut[0].len();
    if cut.iter().any(|section| section.len() != pieces) {
        return None;
    }
    // A wall needs two rungs of its own: loops of one whole piece each are
    // halved.
    if pieces == 1 {
        cut = cut
            .into_iter()
            .map(|section| section[0].split(&[0.5]))
            .collect();
    }
    Some(cut.into_iter().map(harmonised).collect())
}

fn arc_center(piece: Piece) -> Option<Point3> {
    match piece {
        Piece::Arc { center, .. } => Some(center),
        _ => None,
    }
}

/// The loop `pieces` turned to start at whichever of its vertices makes it
/// lie closest to `previous`: the summed squared distance, both ways, between
/// each loop's vertices and the point of the other at the same fraction of
/// its length.
fn best_rotation(pieces: &[Piece], previous: &[Piece]) -> Vec<Piece> {
    let cost = |candidate: &[Piece]| {
        let one_way = |from: &[Piece], to: &[Piece]| {
            let (starts, _) = positions(from);
            from.iter()
                .zip(&starts)
                .map(|(piece, position)| {
                    let offset = piece.start() - point_along(to, *position);
                    offset.dot(offset)
                })
                .sum::<f64>()
        };
        one_way(candidate, previous) + one_way(previous, candidate)
    };
    (0..pieces.len())
        .map(|offset| rotated(pieces, offset))
        .min_by(|left, right| cost(left).total_cmp(&cost(right)))
        .unwrap_or_else(|| pieces.to_vec())
}

/// The point `position` of the way along a loop by length.
fn point_along(pieces: &[Piece], position: f64) -> Point3 {
    let (starts, shares) = positions(pieces);
    let index = starts
        .iter()
        .rposition(|start| *start <= position)
        .unwrap_or(0);
    let fraction = if shares[index] > 0.0 {
        ((position - starts[index]) / shares[index]).clamp(0.0, 1.0)
    } else {
        0.0
    };
    match pieces[index] {
        Piece::Spline { curve } => curve.point(curve.parameter_at_fraction(fraction)),
        piece => piece.point_at(fraction),
    }
}

/// Where along a wall's `v` the samples of the checks below fall: every
/// section, and seven more between each two.
fn heights(parameters: &[f64]) -> Vec<f64> {
    let mut heights = vec![parameters[0]];
    for pair in parameters.windows(2) {
        for step in 1..=8 {
            heights.push((pair[1] - pair[0]).mul_add(f64::from(step) / 8.0, pair[0]));
        }
    }
    heights
}

/// The walls run along the loft everywhere: the rate `∂S/∂v` has a positive
/// part along the loft's direction there. A wall that turns back between two
/// sections folds through itself.
fn skin_runs_forward(
    loops: &[SkinnedLoop],
    parameters: &[f64],
    direction: &dyn Fn(f64) -> Vector3,
) -> Result<(), LoftSectionsError> {
    for wall in loops.iter().flat_map(|skin| &skin.walls) {
        let (u_min, u_max, _, _) = wall.domain();
        let mut us = Vec::new();
        for (low, high) in wall.spans(0, u_min, u_max) {
            for step in 0..4 {
                us.push((high - low).mul_add(f64::from(step) / 4.0, low));
            }
        }
        us.push(u_max);
        for v in heights(parameters) {
            let along = direction(v);
            for u in &us {
                let (_, _, rate) = wall.frame(Point2::new(*u, v));
                let lead = 1.0e-9f64.mul_add(-rate.length(), rate.dot(along));
                if lead.is_nan() || lead <= 0.0 {
                    return Err(LoftSectionsError::SkinFolds);
                }
            }
        }
    }
    Ok(())
}

/// The walls do not cross one another between the sections: at every sampled
/// height the loops they trace, seen along the loft there, are simple, and
/// each hole's lies inside the outer one's and clear of the other holes'.
fn skin_clear(
    loops: &[SkinnedLoop],
    parameters: &[f64],
    direction: &dyn Fn(f64) -> Vector3,
    minimum: f64,
) -> Result<(), LoftSectionsError> {
    for v in heights(parameters) {
        let normal = direction(v);
        let seed = if normal.x.abs() < 0.9 {
            Vector3::new(1.0, 0.0, 0.0)
        } else {
            Vector3::new(0.0, 1.0, 0.0)
        };
        let across = normal.cross(seed);
        let across = across / across.length();
        let up = normal.cross(across);
        let polygons = loops
            .iter()
            .map(|skin| {
                skin.walls
                    .iter()
                    .flat_map(|wall| {
                        (0..16).map(move |index| {
                            let point = wall.evaluate(Point2::new(f64::from(index) / 16.0, v));
                            Point2::new(point.as_vector().dot(across), point.as_vector().dot(up))
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for (index, polygon) in polygons.iter().enumerate() {
            if polygon_crosses(polygon, polygon, true, minimum) {
                return Err(LoftSectionsError::RungsCross);
            }
            if index > 0 {
                if !point_in_polygon(polygon[0], &polygons[0]) {
                    return Err(LoftSectionsError::RungsCross);
                }
                for (other_index, other) in polygons[..index].iter().enumerate() {
                    let nested = other_index > 0
                        && (point_in_polygon(polygon[0], other)
                            || point_in_polygon(other[0], polygon));
                    if nested || polygon_crosses(polygon, other, false, minimum) {
                        return Err(LoftSectionsError::RungsCross);
                    }
                }
            }
        }
    }
    Ok(())
}

/// No two rungs — each a curve from the first section to the last — may
/// come within the feature floor of each other, judged on fine polylines of
/// them.
fn rungs_clear(loops: &[SkinnedLoop], minimum: f64) -> Result<(), LoftSectionsError> {
    let rungs = loops
        .iter()
        .flat_map(|skin| &skin.walls)
        .map(|wall| {
            (0..=32)
                .map(|index| wall.evaluate(Point2::new(0.0, f64::from(index) / 32.0)))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    for (index, first) in rungs.iter().enumerate() {
        for second in &rungs[index + 1..] {
            for a in first.windows(2) {
                for b in second.windows(2) {
                    if segment_distance((a[0], a[1]), (b[0], b[1])) <= minimum {
                        return Err(LoftSectionsError::RungsCross);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Builds the loft: the two caps, then the walls chain by chain.
pub(crate) fn build_skinned_loft(loft: &ValidatedSkinnedLoft) -> Topology {
    let mut topology = Topology::default();
    let mut next_id = 1_u64;
    struct Keys {
        bottom: Vec<(EdgeKey, (Curve3, ParameterRange))>,
        top: Vec<(EdgeKey, (Curve3, ParameterRange))>,
        rungs: Vec<EdgeKey>,
    }
    let mut keys = Vec::with_capacity(loft.loops.len());
    for skin in &loft.loops {
        let count = skin.walls.len();
        let last_row = |wall: SplineSurface| wall.counts()[1] - 1;
        let bottom_vertices = skin
            .first
            .iter()
            .map(|piece| push_vertex(&mut topology, &mut next_id, piece.start()))
            .collect::<Vec<VertexKey>>();
        let top_vertices = skin
            .last
            .iter()
            .map(|piece| push_vertex(&mut topology, &mut next_id, piece.start()))
            .collect::<Vec<VertexKey>>();
        let mut edges_of = |pieces: &[Piece], vertices: &[VertexKey], top: bool| {
            pieces
                .iter()
                .enumerate()
                .map(|(index, piece)| {
                    let wall = skin.walls[index];
                    let row = if top { last_row(wall) } else { 0 };
                    let (curve, parameter_range) = section_edge(*piece, wall, row);
                    let key = push_edge(
                        &mut topology,
                        &mut next_id,
                        Edge {
                            vertices: [vertices[index], vertices[(index + 1) % count]],
                            curve,
                            parameter_range,
                        },
                    );
                    (key, (curve, parameter_range))
                })
                .collect::<Vec<_>>()
        };
        let bottom = edges_of(&skin.first, &bottom_vertices, false);
        let top = edges_of(&skin.last, &top_vertices, true);
        let rungs = (0..count)
            .filter_map(|index| {
                let rung = skin.walls[index].column(0)?;
                Some(push_edge(
                    &mut topology,
                    &mut next_id,
                    Edge {
                        vertices: [bottom_vertices[index], top_vertices[index]],
                        curve: Curve3::Bspline { curve: rung },
                        parameter_range: ParameterRange::new(0.0, 1.0),
                    },
                ))
            })
            .collect::<Vec<_>>();
        keys.push(Keys { bottom, top, rungs });
    }

    // The caps. Each loop runs about the loft, which is the last cap's own
    // outward sense and against the first's, so the first walks its loops
    // backwards.
    for (cap_index, role) in [(0, FaceRole::ExtrusionBottom), (1, FaceRole::ExtrusionTop)] {
        let plane = loft.caps[cap_index].plane;
        let loops = keys
            .iter()
            .map(|keys| {
                let edges = if cap_index == 0 {
                    &keys.bottom
                } else {
                    &keys.top
                };
                let mut uses = edges
                    .iter()
                    .map(|(edge, curve)| BoundaryUse {
                        edge: *edge,
                        orientation: Orientation::Forward,
                        curve: cap_pcurve(plane, *curve),
                    })
                    .collect::<Vec<_>>();
                if cap_index == 0 {
                    uses.reverse();
                    for boundary_use in &mut uses {
                        boundary_use.orientation = Orientation::Reverse;
                        boundary_use.curve = reversed_pcurve(boundary_use.curve);
                    }
                }
                push_loop(&mut topology, &mut next_id, uses)
            })
            .collect::<Vec<_>>();
        push_cap_face(
            &mut topology,
            &mut next_id,
            Surface::Plane(plane),
            &loops,
            role,
        );
    }

    let square = [
        Point2::new(0.0, 0.0),
        Point2::new(1.0, 0.0),
        Point2::new(1.0, 1.0),
        Point2::new(0.0, 1.0),
    ];
    let mut ordinal = 0_u32;
    for (skin, keys) in loft.loops.iter().zip(&keys) {
        let count = skin.walls.len();
        for index in 0..count {
            let next = (index + 1) % count;
            let loop_key = push_loop(
                &mut topology,
                &mut next_id,
                [
                    (
                        keys.bottom[index].0,
                        Orientation::Forward,
                        [square[0], square[1]],
                    ),
                    (
                        keys.rungs[next],
                        Orientation::Forward,
                        [square[1], square[2]],
                    ),
                    (
                        keys.top[index].0,
                        Orientation::Reverse,
                        [square[2], square[3]],
                    ),
                    (
                        keys.rungs[index],
                        Orientation::Reverse,
                        [square[3], square[0]],
                    ),
                ]
                .into_iter()
                .map(|(edge, orientation, points)| BoundaryUse {
                    edge,
                    orientation,
                    curve: Curve2::line_segment(points),
                })
                .collect(),
            );
            topology.faces.push(Record {
                id: allocate_id(&mut next_id),
                value: Face {
                    surface: Surface::Bspline(skin.walls[index]),
                    outer_loop: loop_key,
                    inner_loops: Vec::new(),
                    role: FaceRole::ExtrusionSide(ordinal),
                },
            });
            ordinal += 1;
        }
    }

    let shell_key = ShellKey(topology.shells.len());
    topology.shells.push(Record {
        id: allocate_id(&mut next_id),
        value: Shell {
            faces: (0..topology.faces.len()).map(FaceKey).collect(),
        },
    });
    topology.solids.push(Record {
        id: allocate_id(&mut next_id),
        value: Solid {
            outer_shell: shell_key,
            inner_shells: Vec::new(),
        },
    });
    topology
}
