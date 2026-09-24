//! Turning (ADR 0057 §2.3): face, rough, drill and bore, finish, groove,
//! part off, from the part's `(r, z)` section.
//!
//! The section is read once into an outside profile, its grooves, and its
//! bore. Roughing follows the profile's envelope — the profile with its
//! grooves filled in — offset by the finish allowance, in `z`-parallel
//! passes that retract along the envelope, the way a `G71` cycle does.
//! Finishing follows the exact envelope on the tool's theoretical tip point
//! with `G42` asked of the control for the nose radius. Grooves are plunged
//! with the parting blade, and the part is parted off last.

use artificer_protocol::{PlanarCurve2, PlanarLoop2, Point2, Point3};

use crate::CamRefusal;
use crate::geom;
use crate::plan::{FeedRate, Machine, Move, Operation, OperationKind, Plan, Spindle};
use crate::recognise::TurnedSetup;
use crate::tools::{Material, Tool, ToolKind, ToolLibrary, rpm_for};

/// Radial stock the roughing leaves for the finish pass, in millimetres.
pub const FINISH_ALLOWANCE_RADIAL: f64 = 0.5;
/// Axial stock the roughing leaves on every shoulder, in millimetres.
pub const FINISH_ALLOWANCE_AXIAL: f64 = 0.2;
/// How far a tool clears the stock before a rapid, in millimetres.
pub const CLEARANCE: f64 = 1.0;
/// How far in front of the bar the tool starts a pass.
pub const START_GAP: f64 = 2.0;
/// How far past the centre a facing or parting cut goes.
pub const PAST_CENTRE: f64 = 0.2;
/// The parting blade overlaps its previous plunge by this much in a groove.
pub const GROOVE_OVERLAP: f64 = 0.5;
/// How deep the centre drill goes.
pub const CENTRE_DRILL_DEPTH: f64 = 2.0;

/// One groove in the outside profile: two radial walls and a cylindrical
/// floor between them, cut with the parting blade.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Groove {
    /// The wall nearer the tailstock.
    pub front_z: f64,
    /// The wall nearer the chuck.
    pub back_z: f64,
    pub floor_radius: f64,
    pub rim_radius: f64,
}

/// The bore read from the section: its wall chain from the front face
/// inward, narrowing or straight, ending on the axis or, for a tube, at the
/// back face.
#[derive(Clone, Debug, PartialEq)]
pub struct Bore {
    /// From the front face's inner corner inward.
    pub chain: Vec<PlanarCurve2>,
    pub entry_radius: f64,
    /// The narrowest wall radius.
    pub min_radius: f64,
    /// Where the bore ends; `None` for a bore right through.
    pub bottom_z: Option<f64>,
}

/// The corner where the outside meets the back face, when it is chamfered
/// or rounded: a run of curves facing the chuck that climbs from the back
/// face's outer corner to the rim. A right-hand insert from the front
/// cannot reach it, but the parting blade's front corner can trace it
/// before parting off, the way a machinist breaks a back edge: the blade's
/// body trails on the chuck side, in the kerf it is about to cut anyway.
#[derive(Clone, Debug, PartialEq)]
pub struct BackCorner {
    /// The run from the rim down to the back face's outer corner, the way
    /// the blade traces it.
    pub curves: Vec<PlanarCurve2>,
    /// The radius where the run meets the outside profile.
    pub rim_radius: f64,
    /// The radius where the run meets the back face.
    pub inner_radius: f64,
    /// Where the run leaves the rim.
    pub front_z: f64,
}

/// How far, along the axis, a back corner may run for the parting blade to
/// cut it: a chamfer or round up to twice the blade's width. A longer taper
/// facing the chuck wants a second setup.
pub const BACK_CORNER_BLADE_WIDTHS: f64 = 2.0;

/// The section as the planner reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionReading {
    pub back_z: f64,
    /// The outside profile from the front outer corner to the back outer
    /// corner, exact, grooves included.
    pub outside: Vec<PlanarCurve2>,
    /// The outside profile with its grooves filled in and its back corner
    /// squared off at the rim, front to back.
    pub envelope: Vec<PlanarCurve2>,
    pub grooves: Vec<Groove>,
    pub back_corner: Option<BackCorner>,
    pub bore: Option<Bore>,
    pub front_outer_radius: f64,
    pub back_outer_radius: f64,
}

/// Reads the section into what the lathe needs, refusing what a right-hand
/// tool from the tailstock end cannot reach.
pub fn read_section(setup: &TurnedSetup) -> Result<SectionReading, CamRefusal> {
    let source = geom::normalised(&geom::counter_clockwise(&setup.section));
    let curves = &source.curves;
    if curves.len() < 3 {
        return Err(CamRefusal::TurnedUndercut {
            detail: "the section has fewer than three curves".to_owned(),
        });
    }
    let tolerance = 1.0e-9 * setup.length.max(setup.max_radius).max(1.0);
    let z_of = |p: Point2| p.y;
    let r_of = |p: Point2| p.x;
    // Start at the back inner corner: the lowest z, then the smallest r.
    let start = (0..curves.len())
        .min_by(|a, b| {
            let pa = geom::curve_start(&curves[*a]);
            let pb = geom::curve_start(&curves[*b]);
            z_of(pa)
                .total_cmp(&z_of(pb))
                .then_with(|| r_of(pa).total_cmp(&r_of(pb)))
        })
        .expect("non-empty");
    let ordered = curves[start..]
        .iter()
        .chain(curves[..start].iter())
        .cloned()
        .collect::<Vec<_>>();
    let back_z = z_of(geom::curve_start(&ordered[0]));
    let front_z = ordered
        .iter()
        .flat_map(|curve| [geom::curve_start(curve), geom::curve_end(curve)])
        .map(z_of)
        .fold(f64::NEG_INFINITY, f64::max);
    if front_z.abs() > tolerance {
        return Err(CamRefusal::TurnedUndercut {
            detail: format!("the front face is at z = {front_z}, not at the work origin"),
        });
    }

    // The back face: radial lines at the back z, moving outward.
    let mut index = 0;
    while index < ordered.len() {
        let curve = &ordered[index];
        let (s, e) = (geom::curve_start(curve), geom::curve_end(curve));
        let radial_at_back = matches!(curve, PlanarCurve2::Line { .. })
            && (z_of(s) - back_z).abs() <= tolerance
            && (z_of(e) - back_z).abs() <= tolerance;
        if !radial_at_back {
            break;
        }
        index += 1;
    }
    if index == 0 || index >= ordered.len() {
        return Err(CamRefusal::TurnedUndercut {
            detail: "the back face is not flat: the part-off cut cannot make it".to_owned(),
        });
    }
    let back_outer = geom::curve_start(&ordered[index]);
    let back_outer_radius = r_of(back_outer);

    // The outside profile: from the back outer corner forward until the
    // front is reached. Everything on it must face the tailstock, except a
    // groove's front wall.
    let mut outside_forward: Vec<PlanarCurve2> = Vec::new();
    while index < ordered.len() {
        let curve = ordered[index].clone();
        let end = geom::curve_end(&curve);
        outside_forward.push(curve);
        index += 1;
        if (z_of(end) - front_z).abs() <= tolerance {
            break;
        }
    }
    let front_outer = geom::curve_end(outside_forward.last().expect("non-empty"));
    if (z_of(front_outer) - front_z).abs() > tolerance {
        return Err(CamRefusal::TurnedUndercut {
            detail: "the outside profile never reaches the front face".to_owned(),
        });
    }
    let front_outer_radius = r_of(front_outer);

    // The back corner: a chamfer or round climbing from the back face's
    // outer corner to the rim, every curve facing the chuck and never
    // turning back. The envelope squares it off at the rim, and the parting
    // blade traces it last.
    let back_corner = read_back_corner(&outside_forward, tolerance);
    let mut envelope_forward: Vec<PlanarCurve2> = Vec::new();
    let mut cursor = 0;
    if let Some(corner) = &back_corner {
        envelope_forward.push(PlanarCurve2::Line {
            start: Point2::new(corner.rim_radius, back_z),
            end: Point2::new(corner.rim_radius, corner.front_z),
        });
        cursor = corner.curves.len();
    }

    // Grooves: (inward radial wall, cylindrical floor, outward radial wall)
    // returning to the same rim. Any other curve facing the chuck is an
    // undercut.
    let mut grooves = Vec::new();
    while cursor < outside_forward.len() {
        let curve = &outside_forward[cursor];
        if faces_chuck(curve) {
            // This must be a groove's front wall, preceded by its floor and
            // back wall already pushed onto the envelope.
            let wall_end = geom::curve_end(curve);
            let wall_start = geom::curve_start(curve);
            let is_radial = matches!(curve, PlanarCurve2::Line { .. })
                && (z_of(wall_end) - z_of(wall_start)).abs() <= tolerance;
            let floor = envelope_forward.pop();
            let back_wall = envelope_forward.pop();
            let groove = match (back_wall, floor) {
                (Some(back_wall), Some(floor)) if is_radial => {
                    let bw_s = geom::curve_start(&back_wall);
                    let bw_e = geom::curve_end(&back_wall);
                    let f_s = geom::curve_start(&floor);
                    let f_e = geom::curve_end(&floor);
                    let back_wall_radial = matches!(back_wall, PlanarCurve2::Line { .. })
                        && (z_of(bw_s) - z_of(bw_e)).abs() <= tolerance
                        && r_of(bw_e) < r_of(bw_s);
                    let floor_axial = matches!(floor, PlanarCurve2::Line { .. })
                        && (r_of(f_s) - r_of(f_e)).abs() <= tolerance;
                    let same_rim = (r_of(bw_s) - r_of(wall_end)).abs() <= tolerance;
                    if back_wall_radial && floor_axial && same_rim {
                        Some(Groove {
                            front_z: z_of(wall_start),
                            back_z: z_of(bw_s),
                            floor_radius: r_of(f_s),
                            rim_radius: r_of(bw_s),
                        })
                    } else {
                        None
                    }
                }
                _ => None,
            };
            let Some(groove) = groove else {
                return Err(CamRefusal::TurnedUndercut {
                    detail: format!(
                        "a face turned towards the chuck near z = {:.3} is not a groove with radial walls and a flat floor",
                        z_of(wall_start)
                    ),
                });
            };
            grooves.push(groove);
            // The envelope bridges the groove at its rim.
            envelope_forward.push(PlanarCurve2::Line {
                start: Point2::new(groove.rim_radius, groove.back_z),
                end: Point2::new(groove.rim_radius, groove.front_z),
            });
            cursor += 1;
            continue;
        }
        envelope_forward.push(curve.clone());
        cursor += 1;
    }
    // Merge collinear axial runs the groove bridges split.
    let envelope_forward = merge_axial_runs(&envelope_forward, tolerance);
    let envelope = envelope_forward
        .iter()
        .rev()
        .map(geom::reversed_curve)
        .collect::<Vec<_>>();
    let outside = outside_forward
        .iter()
        .rev()
        .map(geom::reversed_curve)
        .collect::<Vec<_>>();

    // The front face, then the bore if the loop does not reach the axis
    // there.
    while index < ordered.len() {
        let curve = &ordered[index];
        let (s, e) = (geom::curve_start(curve), geom::curve_end(curve));
        let radial_at_front = matches!(curve, PlanarCurve2::Line { .. })
            && (z_of(s) - front_z).abs() <= tolerance
            && (z_of(e) - front_z).abs() <= tolerance;
        if !radial_at_front {
            break;
        }
        index += 1;
    }
    let next_on_axis = ordered.get(index).is_some_and(|curve| {
        r_of(geom::curve_start(curve)).abs() <= tolerance
            && r_of(geom::curve_end(curve)).abs() <= tolerance
    });
    let bore = if index < ordered.len() && !next_on_axis {
        let entry = geom::curve_start(&ordered[index]);
        if (z_of(entry) - front_z).abs() > tolerance {
            return Err(CamRefusal::TurnedUndercut {
                detail: "the front face is not flat".to_owned(),
            });
        }
        let entry_radius = r_of(entry);
        let mut chain = Vec::new();
        let mut min_radius = entry_radius;
        let mut bottom_z = None;
        while index < ordered.len() {
            let curve = ordered[index].clone();
            let (s, e) = (geom::curve_start(&curve), geom::curve_end(&curve));
            let on_axis = r_of(s).abs() <= tolerance && r_of(e).abs() <= tolerance;
            if on_axis {
                break;
            }
            if r_of(e) > r_of(s) + tolerance || faces_chuck(&curve) {
                return Err(CamRefusal::BoreUndercut {
                    detail: format!(
                        "the bore widens or turns towards the chuck near z = {:.3}",
                        z_of(s)
                    ),
                });
            }
            if r_of(e) > tolerance {
                min_radius = min_radius.min(r_of(e));
            }
            if r_of(e).abs() <= tolerance {
                bottom_z = Some(z_of(e));
            }
            chain.push(curve);
            index += 1;
        }
        if bottom_z.is_none() && !setup.through_bore {
            return Err(CamRefusal::BoreUndercut {
                detail: "the bore neither reaches the axis nor passes through".to_owned(),
            });
        }
        Some(Bore {
            chain,
            entry_radius,
            min_radius,
            bottom_z,
        })
    } else {
        None
    };
    Ok(SectionReading {
        back_z,
        outside,
        envelope,
        grooves,
        back_corner,
        bore,
        front_outer_radius,
        back_outer_radius,
    })
}

/// The back corner at the start of the outside profile (read back to
/// front): the longest run of lines and circular arcs that face the chuck
/// while climbing outward and forward, ending where the profile stops facing
/// the chuck. `None` when the profile starts squarely at the rim.
fn read_back_corner(outside_forward: &[PlanarCurve2], tolerance: f64) -> Option<BackCorner> {
    let climbs = |curve: &PlanarCurve2| -> bool {
        if !matches!(
            curve,
            PlanarCurve2::Line { .. } | PlanarCurve2::CircularArc { .. }
        ) {
            return false;
        }
        let points = geom::sample_curve(curve, 1.0e-4);
        let (start, end) = (geom::curve_start(curve), geom::curve_end(curve));
        end.x > start.x + tolerance
            && end.y > start.y + tolerance
            && points.windows(2).all(|pair| {
                pair[1].x >= pair[0].x - tolerance && pair[1].y >= pair[0].y - tolerance
            })
    };
    let count = outside_forward
        .iter()
        .take_while(|curve| faces_chuck(curve) && climbs(curve))
        .count();
    if count == 0 || count == outside_forward.len() {
        // A profile that faces the chuck all the way to the front is a
        // taper the blade cannot be asked to trace; the groove reader
        // refuses it by name.
        return None;
    }
    let run = &outside_forward[..count];
    let inner = geom::curve_start(&run[0]);
    let rim = geom::curve_end(run.last()?);
    Some(BackCorner {
        curves: run.iter().rev().map(geom::reversed_curve).collect(),
        rim_radius: rim.x,
        inner_radius: inner.x,
        front_z: rim.y,
    })
}

/// Whether any of a curve's outward normal points towards the chuck (`-z`)
/// on a counter-clockwise section: what a right-hand tool from the front
/// cannot reach.
fn faces_chuck(curve: &PlanarCurve2) -> bool {
    match curve {
        PlanarCurve2::Line { start, end } => end.x > start.x + 1.0e-12,
        PlanarCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } => {
            let (_, start_angle, sweep) = geom::arc_parameters(*center, *start, *end, *direction);
            let convex = sweep >= 0.0;
            let steps = 16;
            (0..=steps).any(|step| {
                let angle = sweep.mul_add(f64::from(step) / f64::from(steps), start_angle);
                let outward_z = if convex { angle.sin() } else { -angle.sin() };
                outward_z < -1.0e-9
            })
        }
        _ => true,
    }
}

fn merge_axial_runs(curves: &[PlanarCurve2], tolerance: f64) -> Vec<PlanarCurve2> {
    let mut merged: Vec<PlanarCurve2> = Vec::new();
    for curve in curves {
        if let (
            Some(PlanarCurve2::Line {
                start: prev_start,
                end: prev_end,
            }),
            PlanarCurve2::Line { start, end },
        ) = (merged.last(), curve)
            && (prev_start.x - prev_end.x).abs() <= tolerance
            && (start.x - end.x).abs() <= tolerance
            && (prev_end.x - start.x).abs() <= tolerance
            && geom::same_point(*prev_end, *start)
        {
            let start = *prev_start;
            merged.pop();
            merged.push(PlanarCurve2::Line { start, end: *end });
            continue;
        }
        merged.push(curve.clone());
    }
    merged
}

/// The offset envelope as a polyline, front to back: every axial run moved
/// out by the radial allowance, every radial run forward by the axial one,
/// sloped runs along their normal, and corners mitred.
fn offset_envelope(
    envelope: &[PlanarCurve2],
    radial: f64,
    axial: f64,
    back_stop: f64,
) -> Vec<Point2> {
    // Sample the envelope into a polyline first; arcs become chords here
    // because roughing only needs to stop short of them.
    let mut points: Vec<Point2> = Vec::new();
    for curve in envelope {
        for p in geom::sample_curve(curve, 0.01) {
            if points.last().is_none_or(|last| !geom::same_point(*last, p)) {
                points.push(p);
            }
        }
    }
    if points.len() < 2 {
        return points;
    }
    // Offset each segment.
    let mut lines: Vec<(Point2, Point2)> = Vec::new();
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let dr = b.x - a.x;
        let dz = b.y - a.y;
        let length = dr.hypot(dz);
        if length <= 1.0e-12 {
            continue;
        }
        // Outward is to the right of travel going front to back (material on
        // the left going front to back means... the section is CCW, so going
        // back the material lies to the right; outward is left).
        let (nr, nz) = (-dz / length, dr / length);
        let (nr, nz) = if nr < 0.0 { (-nr, -nz) } else { (nr, nz) };
        // A shoulder moves forward by the axial allowance; a cylinder and
        // a slope move out by the radial one.
        let distance = if nr.abs() <= 1.0e-9 { axial } else { radial };
        let shift = Point2::new(nr * distance, nz * distance);
        lines.push((
            Point2::new(a.x + shift.x, a.y + shift.y),
            Point2::new(b.x + shift.x, b.y + shift.y),
        ));
    }
    let mut offset: Vec<Point2> = Vec::new();
    // The front end: the first offset line extended to the front face's
    // own offset plane, z = axial.
    if let Some((a, b)) = lines.first() {
        offset.push(extend_to_z(*a, *b, axial).unwrap_or(*a));
    }
    for pair in lines.windows(2) {
        let (a1, b1) = pair[0];
        let (a2, b2) = pair[1];
        match line_intersection(a1, b1, a2, b2) {
            Some(mitre) => offset.push(mitre),
            None => {
                offset.push(b1);
                offset.push(a2);
            }
        }
    }
    if let Some((a, b)) = lines.last() {
        offset.push(extend_to_z(*a, *b, back_stop).unwrap_or(*b));
    }
    offset
}

fn extend_to_z(a: Point2, b: Point2, z: f64) -> Option<Point2> {
    let dz = b.y - a.y;
    if dz.abs() <= 1.0e-12 {
        return None;
    }
    let t = (z - a.y) / dz;
    Some(Point2::new((b.x - a.x).mul_add(t, a.x), z))
}

fn line_intersection(a1: Point2, b1: Point2, a2: Point2, b2: Point2) -> Option<Point2> {
    let d1 = Point2::new(b1.x - a1.x, b1.y - a1.y);
    let d2 = Point2::new(b2.x - a2.x, b2.y - a2.y);
    let denominator = d1.x.mul_add(d2.y, -(d1.y * d2.x));
    if denominator.abs() <= 1.0e-12 {
        return None;
    }
    let t = ((a2.x - a1.x) * d2.y - (a2.y - a1.y) * d2.x) / denominator;
    Some(Point2::new(d1.x.mul_add(t, a1.x), d1.y.mul_add(t, a1.y)))
}

/// Where a `z`-parallel pass at `radius` meets the offset envelope, walking
/// from the front: the first point where the envelope's radius reaches it.
fn envelope_stop(offset: &[Point2], radius: f64) -> Option<(usize, Point2)> {
    for (index, pair) in offset.windows(2).enumerate() {
        let (a, b) = (pair[0], pair[1]);
        if b.x + 1.0e-12 >= radius && a.x <= radius + 1.0e-12 {
            if (b.x - a.x).abs() <= 1.0e-12 {
                return Some((index, Point2::new(radius, a.y.min(b.y))));
            }
            let t = ((radius - a.x) / (b.x - a.x)).clamp(0.0, 1.0);
            return Some((index, Point2::new(radius, (b.y - a.y).mul_add(t, a.y))));
        }
    }
    None
}

fn lathe_point(r: f64, z: f64) -> Point3 {
    Point3::new(r, 0.0, z)
}

fn surface_speed(tool: &Tool, material: Material, max_rpm: f64) -> Spindle {
    Spindle::SurfaceSpeed {
        metres_per_minute: tool.feed_for(material).surface_speed,
        max_rpm,
    }
}

fn per_revolution(tool: &Tool, material: Material) -> FeedRate {
    FeedRate::PerRevolution(tool.feed_for(material).chip_load)
}

/// Plans the lathe program for a turned setup.
pub fn plan_turning(
    setup: &TurnedSetup,
    library: &ToolLibrary,
    material: Material,
) -> Result<Plan, CamRefusal> {
    let reading = read_section(setup)?;
    let rough = library
        .first_of_kind(ToolKind::TurningInsertRough)
        .ok_or_else(|| CamRefusal::NoToolFits {
            detail: "the library has no roughing insert".to_owned(),
        })?;
    let finish = library
        .first_of_kind(ToolKind::TurningInsertFinish)
        .ok_or_else(|| CamRefusal::NoToolFits {
            detail: "the library has no finishing insert".to_owned(),
        })?;
    let blade = library
        .first_of_kind(ToolKind::PartingBlade)
        .ok_or_else(|| CamRefusal::NoToolFits {
            detail: "the library has no parting blade".to_owned(),
        })?;
    let max_rpm = library.lathe_max_rpm;
    let stock = setup.stock;
    let z_start = stock.front + START_GAP;
    let r_clear = stock.radius + CLEARANCE;
    let back_z = reading.back_z;
    let part_off_z = back_z;
    // Roughing and finishing run past the part-off line so the blade only
    // has the part's own radius to cut through.
    let back_stop = part_off_z - blade.diameter - 0.5;
    let mut operations = Vec::new();
    let mut tools: Vec<Tool> = Vec::new();
    let mut use_tool = |tool: &Tool| {
        if !tools.iter().any(|held| held.number == tool.number) {
            tools.push(tool.clone());
        }
        tool.number
    };
    let mut notes = Vec::new();

    // ---- Face -----------------------------------------------------------
    {
        let mut moves = Vec::new();
        let passes = (stock.front / rough.max_depth_of_cut).ceil().max(1.0) as usize;
        for pass in 0..passes {
            let z = if pass + 1 == passes {
                0.0
            } else {
                stock.front - (pass as f64 + 1.0) * rough.max_depth_of_cut
            };
            moves.push(Move::Rapid {
                to: lathe_point(r_clear, z_start),
            });
            moves.push(Move::Rapid {
                to: lathe_point(r_clear, z),
            });
            moves.push(Move::Feed {
                to: lathe_point(-PAST_CENTRE, z),
            });
            moves.push(Move::Rapid {
                to: lathe_point(-PAST_CENTRE, z + CLEARANCE),
            });
            moves.push(Move::Rapid {
                to: lathe_point(r_clear, z + CLEARANCE),
            });
        }
        operations.push(Operation {
            kind: OperationKind::Face,
            name: format!(
                "Face the front, {passes} pass{}",
                if passes == 1 { "" } else { "es" }
            ),
            tool: use_tool(rough),
            spindle: surface_speed(rough, material, max_rpm),
            feed: per_revolution(rough, material),
            moves,
            notes: Vec::new(),
        });
    }

    // ---- Rough ----------------------------------------------------------
    let offset = offset_envelope(
        &reading.envelope,
        FINISH_ALLOWANCE_RADIAL,
        FINISH_ALLOWANCE_AXIAL,
        back_stop,
    );
    if offset.len() < 2 {
        return Err(CamRefusal::TurnedUndercut {
            detail: "the outside profile is empty".to_owned(),
        });
    }
    let min_offset_radius = offset.iter().map(|p| p.x).fold(f64::INFINITY, f64::min);
    {
        let doc = rough.max_depth_of_cut;
        let mut radii = Vec::new();
        let mut r = stock.radius - doc;
        while r > min_offset_radius + 1.0e-9 {
            radii.push(r);
            r -= doc;
        }
        if radii
            .last()
            .is_none_or(|last| (last - min_offset_radius).abs() > 1.0e-6)
            && min_offset_radius < stock.radius - 1.0e-9
        {
            radii.push(min_offset_radius);
        }
        let mut moves = Vec::new();
        let mut previous_radius = stock.radius;
        for radius in &radii {
            let Some((stop_index, stop)) = envelope_stop(&offset, *radius) else {
                continue;
            };
            moves.push(Move::Rapid {
                to: lathe_point(r_clear, z_start),
            });
            moves.push(Move::Rapid {
                to: lathe_point(*radius, z_start),
            });
            moves.push(Move::Feed {
                to: lathe_point(stop.x, stop.y),
            });
            // Retract along the envelope up to the previous pass radius.
            let mut cursor = stop_index + 1;
            while cursor < offset.len() && offset[cursor].x <= previous_radius + 1.0e-9 {
                let p = offset[cursor];
                if p.x >= *radius - 1.0e-9 {
                    moves.push(Move::Feed {
                        to: lathe_point(p.x, p.y),
                    });
                }
                cursor += 1;
            }
            if let Some((_, exit)) = envelope_stop(&offset, previous_radius)
                && exit.y < stop.y
            {
                moves.push(Move::Feed {
                    to: lathe_point(exit.x, exit.y),
                });
            }
            // Back out along the pass first, then clear radially: the
            // insert's leaning edge leaves a sliver beside the tip at the
            // end of a pass, so a radial move there would rub it.
            let last = moves
                .last()
                .map(Move::end)
                .unwrap_or(lathe_point(stop.x, stop.y));
            moves.push(Move::Rapid {
                to: lathe_point(last.x, last.z + CLEARANCE),
            });
            moves.push(Move::Rapid {
                to: lathe_point(last.x + CLEARANCE, last.z + CLEARANCE),
            });
            moves.push(Move::Rapid {
                to: lathe_point(last.x + CLEARANCE, z_start),
            });
            previous_radius = *radius;
        }
        operations.push(Operation {
            kind: OperationKind::Rough,
            name: format!(
                "Rough the outside, {} passes at {:.1} mm, leaving {:.1} mm",
                radii.len(),
                doc,
                FINISH_ALLOWANCE_RADIAL
            ),
            tool: use_tool(rough),
            spindle: surface_speed(rough, material, max_rpm),
            feed: per_revolution(rough, material),
            moves,
            notes: vec![
                "Roughing stops on the envelope: grooves are filled in and plunged later."
                    .to_owned(),
            ],
        });
    }

    // ---- Drill and bore -------------------------------------------------
    if let Some(bore) = &reading.bore {
        plan_bore(
            bore,
            setup,
            library,
            material,
            z_start,
            &mut operations,
            &mut use_tool,
            &mut notes,
        )?;
    }

    // ---- Finish ---------------------------------------------------------
    {
        let mut moves = Vec::new();
        let first = geom::curve_start(&reading.envelope[0]);
        moves.push(Move::Rapid {
            to: lathe_point(r_clear, z_start),
        });
        moves.push(Move::Rapid {
            to: lathe_point(first.x, z_start),
        });
        moves.push(Move::Feed {
            to: lathe_point(first.x, first.y),
        });
        for curve in &reading.envelope {
            match curve {
                PlanarCurve2::Line { end, .. } => moves.push(Move::Feed {
                    to: lathe_point(end.x, end.y),
                }),
                PlanarCurve2::CircularArc {
                    center,
                    end,
                    direction,
                    ..
                } => moves.push(Move::Arc {
                    to: lathe_point(end.x, end.y),
                    center: *center,
                    clockwise: *direction == artificer_protocol::ArcDirection::Clockwise,
                }),
                _ => {}
            }
        }
        let last = geom::curve_end(reading.envelope.last().expect("non-empty"));
        moves.push(Move::Feed {
            to: lathe_point(last.x, back_stop),
        });
        moves.push(Move::Rapid {
            to: lathe_point(last.x, back_stop + CLEARANCE),
        });
        moves.push(Move::Rapid {
            to: lathe_point(r_clear, back_stop + CLEARANCE),
        });
        moves.push(Move::Rapid {
            to: lathe_point(r_clear, z_start),
        });
        operations.push(Operation {
            kind: OperationKind::Finish,
            name: "Finish the outside on the profile".to_owned(),
            tool: use_tool(finish),
            spindle: surface_speed(finish, material, max_rpm),
            feed: per_revolution(finish, material),
            moves,
            notes: vec![format!(
                "Programmed on the theoretical tip; G42 applies the {:.1} mm nose radius on the control. Simulated as a sharp tip.",
                finish.corner_radius
            )],
        });
    }

    // ---- Grooves --------------------------------------------------------
    for (ordinal, groove) in reading.grooves.iter().enumerate() {
        let width = groove.front_z - groove.back_z;
        if width < blade.diameter - 1.0e-9 {
            return Err(CamRefusal::GrooveUnsupported {
                detail: format!(
                    "the groove at z = {:.3} is {:.3} mm wide, narrower than the {:.1} mm parting blade",
                    groove.front_z, width, blade.diameter
                ),
            });
        }
        let mut plunges = Vec::new();
        let mut z = groove.front_z;
        let last = groove.back_z + blade.diameter;
        loop {
            plunges.push(z);
            if z <= last + 1.0e-9 {
                break;
            }
            z = (z - (blade.diameter - GROOVE_OVERLAP)).max(last);
        }
        let mut moves = Vec::new();
        for z in &plunges {
            moves.push(Move::Rapid {
                to: lathe_point(r_clear, z_start),
            });
            moves.push(Move::Rapid {
                to: lathe_point(r_clear, *z),
            });
            moves.push(Move::Feed {
                to: lathe_point(groove.floor_radius, *z),
            });
            moves.push(Move::Rapid {
                to: lathe_point(r_clear, *z),
            });
        }
        operations.push(Operation {
            kind: OperationKind::Groove,
            name: format!(
                "Groove {} at z = {:.1}, {} plunge{}",
                ordinal + 1,
                groove.front_z,
                plunges.len(),
                if plunges.len() == 1 { "" } else { "s" }
            ),
            tool: use_tool(blade),
            spindle: surface_speed(blade, material, max_rpm),
            feed: per_revolution(blade, material),
            moves,
            notes: Vec::new(),
        });
    }

    // ---- Back corner ----------------------------------------------------
    if let Some(corner) = &reading.back_corner {
        let reach = corner.front_z - back_z;
        let limit = BACK_CORNER_BLADE_WIDTHS * blade.diameter;
        if reach > limit + 1.0e-9 {
            return Err(CamRefusal::TurnedUndercut {
                detail: format!(
                    "the back corner runs {reach:.3} mm along the axis, more than the {limit:.1} mm the {:.1} mm parting blade can trace; a taper facing the chuck needs a second setup",
                    blade.diameter
                ),
            });
        }
        // The blade's front corner comes in at the rim and follows the run
        // down to the back face. Its body trails on the chuck side, in the
        // kerf the part-off cuts next, so nothing it sweeps is the part's.
        let mut moves = vec![
            Move::Rapid {
                to: lathe_point(r_clear, z_start),
            },
            Move::Rapid {
                to: lathe_point(r_clear, corner.front_z),
            },
            Move::Feed {
                to: lathe_point(corner.rim_radius, corner.front_z),
            },
        ];
        for curve in &corner.curves {
            match curve {
                PlanarCurve2::Line { end, .. } => moves.push(Move::Feed {
                    to: lathe_point(end.x, end.y),
                }),
                PlanarCurve2::CircularArc {
                    center,
                    end,
                    direction,
                    ..
                } => moves.push(Move::Arc {
                    to: lathe_point(end.x, end.y),
                    center: *center,
                    clockwise: *direction == artificer_protocol::ArcDirection::Clockwise,
                }),
                _ => {}
            }
        }
        // Out through the kerf the blade's own body has just opened.
        moves.push(Move::Rapid {
            to: lathe_point(r_clear, back_z),
        });
        moves.push(Move::Rapid {
            to: lathe_point(r_clear, z_start),
        });
        let rounded = corner
            .curves
            .iter()
            .any(|curve| matches!(curve, PlanarCurve2::CircularArc { .. }));
        operations.push(Operation {
            kind: OperationKind::BackCorner,
            name: format!(
                "{} the back corner with the parting blade, Ø{:.1} to Ø{:.1}",
                if rounded { "Round" } else { "Chamfer" },
                corner.rim_radius * 2.0,
                corner.inner_radius * 2.0
            ),
            tool: use_tool(blade),
            spindle: surface_speed(blade, material, max_rpm),
            feed: per_revolution(blade, material),
            moves,
            notes: vec![
                "Traced on the blade's front corner before parting off; the blade's body runs in the part-off kerf."
                    .to_owned(),
            ],
        });
    }

    // ---- Part off -------------------------------------------------------
    {
        let moves = vec![
            Move::Rapid {
                to: lathe_point(r_clear, z_start),
            },
            Move::Rapid {
                to: lathe_point(r_clear, part_off_z),
            },
            Move::Feed {
                to: lathe_point(-PAST_CENTRE, part_off_z),
            },
            Move::Rapid {
                to: lathe_point(r_clear, part_off_z),
            },
            Move::Rapid {
                to: lathe_point(r_clear, z_start),
            },
        ];
        operations.push(Operation {
            kind: OperationKind::PartOff,
            name: format!("Part off at z = {part_off_z:.1}"),
            tool: use_tool(blade),
            spindle: surface_speed(blade, material, max_rpm),
            feed: per_revolution(blade, material),
            moves,
            notes: Vec::new(),
        });
    }

    notes.push(format!(
        "Bar Ø{:.1} × {:.1} mm; {:.1} mm radial and {:.1} mm facing allowance; work origin on the axis at the front face.",
        stock.radius * 2.0,
        stock.front - stock.back,
        setup.allowances.radial,
        setup.allowances.facing
    ));
    Ok(Plan {
        machine: Machine::Lathe,
        material,
        setup_name: format!(
            "Turned part, {:.1} mm long, Ø{:.1} max",
            setup.length,
            setup.max_radius * 2.0
        ),
        operations,
        tools,
        safe_height: r_clear,
        rapid_rate: library.rapid_rate,
        tool_change_seconds: library.tool_change_seconds,
        notes,
    })
}

#[allow(clippy::too_many_arguments)]
fn plan_bore(
    bore: &Bore,
    setup: &TurnedSetup,
    library: &ToolLibrary,
    material: Material,
    z_start: f64,
    operations: &mut Vec<Operation>,
    use_tool: &mut dyn FnMut(&Tool) -> u32,
    notes: &mut Vec<String>,
) -> Result<(), CamRefusal> {
    let max_rpm = library.lathe_max_rpm;
    let through_z = setup.stock.back;
    // The drill goes right through a tube, and to the bottom of a blind
    // bore: its point stays above the floor, which the bar then faces flat.
    let (drill_tip_z, bottom_z) = match bore.bottom_z {
        Some(bottom) => (bottom, bottom),
        None => (through_z + 0.5, through_z + 0.5),
    };
    let diameter = bore.min_radius * 2.0;
    let drill = library
        .largest_drill_within(diameter)
        .ok_or_else(|| CamRefusal::NoToolFits {
            detail: format!("no drill is narrow enough for a Ø{diameter:.3} bore"),
        })?;
    if let Some(centre) = library.first_of_kind(ToolKind::CentreDrill) {
        let (rpm, per_rev) = {
            let feed = centre.feed_for(material);
            (
                rpm_for(feed.surface_speed, centre.diameter, max_rpm).round(),
                feed.chip_load,
            )
        };
        operations.push(Operation {
            kind: OperationKind::CentreDrill,
            name: "Centre drill".to_owned(),
            tool: use_tool(centre),
            spindle: Spindle::Rpm(rpm),
            feed: FeedRate::PerRevolution(per_rev),
            moves: vec![
                Move::Rapid {
                    to: lathe_point(0.0, z_start),
                },
                Move::Feed {
                    to: lathe_point(0.0, -CENTRE_DRILL_DEPTH),
                },
                Move::Rapid {
                    to: lathe_point(0.0, z_start),
                },
            ],
            notes: Vec::new(),
        });
    }
    {
        let feed = drill.feed_for(material);
        let rpm = rpm_for(feed.surface_speed, drill.diameter, max_rpm).round();
        // Peck: out to clear chips every two diameters.
        let mut moves = vec![Move::Rapid {
            to: lathe_point(0.0, z_start),
        }];
        let peck = drill.diameter * 2.0;
        let mut z = 0.0;
        while z > drill_tip_z + 1.0e-9 {
            z = (z - peck).max(drill_tip_z);
            moves.push(Move::Feed {
                to: lathe_point(0.0, z),
            });
            moves.push(Move::Rapid {
                to: lathe_point(0.0, z_start),
            });
            if z > drill_tip_z + 1.0e-9 {
                moves.push(Move::Rapid {
                    to: lathe_point(0.0, z + 0.5),
                });
            }
        }
        operations.push(Operation {
            kind: OperationKind::Drill,
            name: format!("Drill Ø{:.1} to z = {:.1}", drill.diameter, drill_tip_z),
            tool: use_tool(drill),
            spindle: Spindle::Rpm(rpm),
            feed: FeedRate::PerRevolution(feed.chip_load),
            moves,
            notes: vec!["Pecked every two diameters.".to_owned()],
        });
    }
    let drill_radius = drill.diameter / 2.0;
    let needs_boring = bore.entry_radius > drill_radius + 1.0e-9
        || bore.chain.len() > 1
        || bore.bottom_z.is_some();
    if !needs_boring {
        return Ok(());
    }
    let bar = library
        .first_of_kind(ToolKind::BoringBar)
        .ok_or_else(|| CamRefusal::NoToolFits {
            detail: "the library has no boring bar".to_owned(),
        })?;
    if drill.diameter < bar.diameter + 1.0 {
        return Err(CamRefusal::NoToolFits {
            detail: format!(
                "the Ø{:.1} boring bar needs a hole of at least Ø{:.1}; the bore is Ø{:.1}",
                bar.diameter,
                bar.diameter + 1.0,
                diameter
            ),
        });
    }
    // The bore's envelope, front to back, as a polyline of (r, z): wall
    // radii never grow going in, so each rough pass at a radius stops where
    // the offset chain narrows below it.
    let mut chain_points: Vec<Point2> = Vec::new();
    for curve in &bore.chain {
        for p in geom::sample_curve(curve, 0.01) {
            if chain_points
                .last()
                .is_none_or(|last| !geom::same_point(*last, p))
            {
                chain_points.push(p);
            }
        }
    }
    let stop_for = |radius: f64| -> f64 {
        // The deepest z where the chain's radius is still at least `radius`
        // (plus the finish allowance inward: the pass stays inside the wall).
        let mut z = 0.0_f64;
        for pair in chain_points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let inner = a.x.min(b.x) - FINISH_ALLOWANCE_RADIAL;
            if inner >= radius - 1.0e-9 {
                z = z.min(b.y.min(a.y));
            } else {
                // Interpolate on a sloped run; a flat step stops at its z.
                if (a.x - b.x).abs() > 1.0e-9 && a.y > b.y {
                    let t =
                        ((a.x - FINISH_ALLOWANCE_RADIAL - radius) / (a.x - b.x)).clamp(0.0, 1.0);
                    z = z.min((b.y - a.y).mul_add(t, a.y));
                }
                break;
            }
        }
        z + FINISH_ALLOWANCE_AXIAL
    };
    {
        let doc = bar.max_depth_of_cut;
        let mut radii = Vec::new();
        let mut r = drill_radius + doc;
        let target = bore.entry_radius - FINISH_ALLOWANCE_RADIAL;
        while r < target - 1.0e-9 {
            radii.push(r);
            r += doc;
        }
        if target > drill_radius + 1.0e-9
            && radii
                .last()
                .is_none_or(|last| (last - target).abs() > 1.0e-6)
        {
            radii.push(target);
        }
        let mut moves = Vec::new();
        for radius in &radii {
            let stop = stop_for(*radius).max(bottom_z + FINISH_ALLOWANCE_AXIAL);
            moves.push(Move::Rapid {
                to: lathe_point(*radius - CLEARANCE, z_start),
            });
            moves.push(Move::Rapid {
                to: lathe_point(*radius, z_start),
            });
            moves.push(Move::Feed {
                to: lathe_point(*radius, stop),
            });
            moves.push(Move::Rapid {
                to: lathe_point(*radius, stop + CLEARANCE),
            });
            moves.push(Move::Rapid {
                to: lathe_point(*radius - CLEARANCE, stop + CLEARANCE),
            });
            moves.push(Move::Rapid {
                to: lathe_point(*radius - CLEARANCE, z_start),
            });
        }
        if !moves.is_empty() {
            operations.push(Operation {
                kind: OperationKind::BoreRough,
                name: format!("Rough bore, {} passes", radii.len()),
                tool: use_tool(bar),
                spindle: surface_speed(bar, material, max_rpm),
                feed: per_revolution(bar, material),
                moves,
                notes: Vec::new(),
            });
        }
    }
    {
        let mut moves = vec![
            Move::Rapid {
                to: lathe_point(bore.entry_radius - CLEARANCE, z_start),
            },
            Move::Rapid {
                to: lathe_point(bore.entry_radius, z_start),
            },
            Move::Feed {
                to: lathe_point(bore.entry_radius, 0.0),
            },
        ];
        for curve in &bore.chain {
            match curve {
                PlanarCurve2::Line { end, .. } => moves.push(Move::Feed {
                    to: lathe_point(end.x, end.y),
                }),
                PlanarCurve2::CircularArc {
                    center,
                    end,
                    direction,
                    ..
                } => moves.push(Move::Arc {
                    to: lathe_point(end.x, end.y),
                    center: *center,
                    clockwise: *direction == artificer_protocol::ArcDirection::Clockwise,
                }),
                _ => {}
            }
        }
        let last = geom::curve_end(bore.chain.last().expect("non-empty"));
        if bore.bottom_z.is_some() {
            // Face the floor flat past the centre, over the drill's point.
            moves.push(Move::Feed {
                to: lathe_point(-PAST_CENTRE, last.y),
            });
            moves.push(Move::Rapid {
                to: lathe_point(-PAST_CENTRE, z_start),
            });
        } else {
            moves.push(Move::Feed {
                to: lathe_point(last.x, through_z + 0.5),
            });
            moves.push(Move::Rapid {
                to: lathe_point(last.x, through_z + 0.5 + CLEARANCE),
            });
            moves.push(Move::Rapid {
                to: lathe_point(last.x - CLEARANCE, through_z + 0.5 + CLEARANCE),
            });
            moves.push(Move::Rapid {
                to: lathe_point(last.x - CLEARANCE, z_start),
            });
        }
        operations.push(Operation {
            kind: OperationKind::BoreFinish,
            name: "Finish bore on the profile".to_owned(),
            tool: use_tool(bar),
            spindle: surface_speed(bar, material, max_rpm),
            feed: per_revolution(bar, material),
            moves,
            notes: vec![format!(
                "Programmed on the theoretical tip; G41 applies the {:.1} mm nose radius on the control.",
                bar.corner_radius
            )],
        });
    }
    if bore.bottom_z.is_some() {
        notes.push("The bore floor is faced to the centre with the boring bar, which a real bar cannot quite do; the drill point's cone is what would remain.".to_owned());
    }
    Ok(())
}

/// The section a tool region sweeps as it moves from `from` to `to`: the
/// convex hull of the region at both ends, for a convex region.
#[must_use]
pub fn swept_region(region: &[Point2], from: Point2, to: Point2) -> PlanarLoop2 {
    let mut points = Vec::with_capacity(region.len() * 2);
    for p in region {
        points.push(Point2::new(p.x + from.x, p.y + from.y));
        points.push(Point2::new(p.x + to.x, p.y + to.y));
    }
    geom::polygon(&geom::convex_hull(&points))
}

/// A lathe tool's cutting region in `(r, z)` relative to its programmed
/// point, counter-clockwise, convex.
#[must_use]
pub fn tool_region(tool: &Tool) -> Vec<Point2> {
    // Insert bodies lean 3° off the shoulder and off the axis.
    let lean = 3.0_f64.to_radians().tan();
    let reach = 6.0;
    match tool.kind {
        ToolKind::TurningInsertRough | ToolKind::TurningInsertFinish => vec![
            Point2::new(0.0, 0.0),
            Point2::new(reach, reach * lean),
            Point2::new(reach * lean, reach),
        ],
        ToolKind::BoringBar => vec![
            Point2::new(0.0, 0.0),
            Point2::new(-reach * lean, reach),
            Point2::new(-reach, reach * lean),
        ],
        ToolKind::PartingBlade => {
            let width = tool.diameter;
            let height = tool.flute_length.max(width);
            vec![
                Point2::new(0.0, -width),
                Point2::new(height, -width),
                Point2::new(height, 0.0),
                Point2::new(0.0, 0.0),
            ]
        }
        ToolKind::Drill | ToolKind::CentreDrill => {
            let radius = tool.diameter / 2.0;
            let cone = radius / 59.0_f64.to_radians().tan();
            let length = tool.flute_length.max(cone + 1.0);
            vec![
                Point2::new(0.0, 0.0),
                Point2::new(radius, cone),
                Point2::new(radius, length),
                Point2::new(-radius, length),
                Point2::new(-radius, cone),
            ]
        }
        ToolKind::FlatEndMill | ToolKind::BallEndMill => {
            let radius = tool.diameter / 2.0;
            vec![
                Point2::new(-radius, 0.0),
                Point2::new(radius, 0.0),
                Point2::new(radius, tool.flute_length),
                Point2::new(-radius, tool.flute_length),
            ]
        }
    }
}
