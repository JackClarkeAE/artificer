//! 2.5D milling (ADR 0057 §2.4): face, pocket, profile, drill and helical
//! bore, from the part's levels.
//!
//! Every height the part exposes from above is a level. Between two
//! levels the material to remove is the stock's footprint minus the part's
//! section just above the lower level: a ring around the outline (the
//! profile) and the holes in that section (the pockets, with any bosses in
//! them as islands). Pockets are cleared contour-parallel by repeated
//! inward offsets of the kernel's certified mitred offset, the last at the
//! tool radius as the wall pass, falling back to a raster where a neck
//! defeats the offset. Round holes are drilled when a drill of their size
//! exists and bored helically otherwise.

use artificer_kernel::NativeKernel;
use artificer_protocol::{
    ArcDirection, BooleanOperation, PlanarCurve2, PlanarLoop2, PlanarRegion2, Point2, Point3,
    PrecisionPolicy,
};

use crate::CamRefusal;
use crate::geom;
use crate::plan::{FeedRate, Machine, Move, Operation, OperationKind, Plan, Spindle};
use crate::recognise::MilledSetup;
use crate::tools::{Material, Tool, ToolKind, ToolLibrary, drill_feed, mill_feed};

/// Stepover between pocket contours, as a fraction of the diameter. At one
/// radius, contour-parallel clearing is complete whenever every offset
/// succeeds until the region vanishes.
pub const POCKET_STEPOVER: f64 = 0.5;
/// Stepover of the facing raster, as a fraction of the diameter.
pub const FACE_STEPOVER: f64 = 0.7;
/// How far above the stock top the safe plane sits.
pub const SAFE_ABOVE_STOCK: f64 = 10.0;
/// How far above the level being cut a rapid travels.
pub const RETRACT: f64 = 2.0;
/// The drill point's extra travel through a hole, as a fraction of the
/// diameter (a 118° point) plus a break-through.
pub const DRILL_POINT: f64 = 0.3;
pub const BREAK_THROUGH: f64 = 0.5;

/// A region of air between two levels, with what it is.
#[derive(Clone, Debug, PartialEq)]
struct Area {
    kind: AreaKind,
    region: PlanarRegion2,
    /// The height the material starts at.
    top: f64,
    /// The height the region's floor is at.
    floor: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AreaKind {
    /// Around the outline: the stock's footprint with the part cut out.
    Ring,
    /// Inside the part's section: a hole in it.
    Pocket,
}

/// Plans the mill program for a milled setup.
pub fn plan_milling(
    setup: &MilledSetup,
    library: &ToolLibrary,
    material: Material,
) -> Result<Plan, CamRefusal> {
    let origin = setup.origin();
    let shift = |p: Point2| Point2::new(p.x - origin.x, p.y - origin.y);
    let stock_min = Point2::new(setup.stock.min.x - origin.x, setup.stock.min.y - origin.y);
    let stock_max = Point2::new(setup.stock.max.x - origin.x, setup.stock.max.y - origin.y);
    let stock_top = setup.stock.max.z - origin.z;
    let stock_bottom = setup.stock.min.z - origin.z;
    let part_top = setup.top - origin.z;
    let safe = stock_top + SAFE_ABOVE_STOCK;
    let max_rpm = library.mill_max_rpm;
    let mut notes = Vec::new();

    // ---- The areas between levels -------------------------------------
    let mut heights = setup
        .levels
        .iter()
        .map(|level| level.height - origin.z)
        .collect::<Vec<_>>();
    heights.sort_by(|a, b| b.total_cmp(a));
    heights.dedup_by(|a, b| (*a - *b).abs() <= 1.0e-9);
    let mut areas: Vec<Area> = Vec::new();
    let mut above = part_top;
    let mut floors = heights.iter().skip(1).copied().collect::<Vec<_>>();
    floors.push(stock_bottom);
    for floor in floors {
        let section = setup
            .section_above(floor + origin.z)
            .map_err(|detail| CamRefusal::Outline { detail })?;
        let section = section
            .iter()
            .map(|region| translate_region(region, &shift))
            .collect::<Vec<_>>();
        // The ring: the stock footprint less every outer loop of the section.
        let ring = PlanarRegion2 {
            outer: geom::rectangle(stock_min, stock_max),
            holes: section
                .iter()
                .map(|region| geom::clockwise(&region.outer))
                .collect(),
        };
        areas.push(Area {
            kind: AreaKind::Ring,
            region: ring,
            top: above,
            floor,
        });
        // Every hole in the section is a pocket; a region nested inside it
        // is an island of that pocket.
        for region in &section {
            for hole in &region.holes {
                let outer = geom::counter_clockwise(hole);
                let islands = section
                    .iter()
                    .filter(|candidate| {
                        !std::ptr::eq(*candidate, region)
                            && geom::interior_point(&candidate.outer)
                                .is_some_and(|p| geom::point_in_loop(&outer, p))
                    })
                    .map(|candidate| geom::clockwise(&candidate.outer))
                    .collect::<Vec<_>>();
                areas.push(Area {
                    kind: AreaKind::Pocket,
                    region: PlanarRegion2 {
                        outer,
                        holes: islands,
                    },
                    top: above,
                    floor,
                });
            }
        }
        above = floor;
    }
    // A pocket that continues straight down through several levels is one
    // pocket: the same outline at the next level down joins the one above.
    let mut merged: Vec<Area> = Vec::new();
    for area in areas {
        if area.kind == AreaKind::Pocket
            && let Some(previous) = merged.iter_mut().rev().find(|previous| {
                previous.kind == AreaKind::Pocket
                    && (previous.floor - area.top).abs() <= 1.0e-9
                    && same_region(&previous.region, &area.region)
            })
        {
            previous.floor = area.floor;
            continue;
        }
        merged.push(area);
    }
    let areas = merged;

    // ---- Operations, one per area, then ordered by tool ---------------
    let face_tool = library
        .largest_end_mill(f64::INFINITY, f64::INFINITY)
        .ok_or_else(|| CamRefusal::NoToolFits {
            detail: "the library has no flat end mill".to_owned(),
        })?;
    let mut operations: Vec<(usize, Operation)> = Vec::new();
    let mut ordinal = 0_usize;

    // Face: rasters over the whole stock top down to the part's top.
    {
        let (rpm, feed) = mill_feed(face_tool, material, max_rpm);
        let radius = face_tool.radius();
        let stepover = face_tool.diameter * FACE_STEPOVER;
        let mut moves = Vec::new();
        let passes = ((stock_top - part_top) / face_tool.max_depth_of_cut)
            .ceil()
            .max(1.0) as usize;
        for pass in 0..passes {
            let z = if pass + 1 == passes {
                part_top
            } else {
                stock_top - (pass as f64 + 1.0) * face_tool.max_depth_of_cut
            };
            let x_start = stock_min.x - radius;
            let x_end = stock_max.x + radius;
            let mut y = stock_min.y;
            let mut forward = true;
            moves.push(Move::Rapid {
                to: Point3::new(x_start, y, safe),
            });
            moves.push(Move::Rapid {
                to: Point3::new(x_start, y, stock_top + RETRACT),
            });
            moves.push(Move::Feed {
                to: Point3::new(x_start, y, z),
            });
            loop {
                let to_x = if forward { x_end } else { x_start };
                moves.push(Move::Feed {
                    to: Point3::new(to_x, y, z),
                });
                if y - radius >= stock_max.y {
                    break;
                }
                y += stepover;
                if y - radius > stock_max.y {
                    y = stock_max.y + radius;
                }
                moves.push(Move::Feed {
                    to: Point3::new(to_x, y, z),
                });
                forward = !forward;
            }
            let last = moves.last().map(Move::end).expect("moves");
            moves.push(Move::Rapid {
                to: Point3::new(last.x, last.y, safe),
            });
        }
        operations.push((
            ordinal,
            Operation {
                kind: OperationKind::Face,
                name: format!("Face the stock top to z = {part_top:.2}"),
                tool: face_tool.number,
                spindle: Spindle::Rpm(rpm),
                feed: FeedRate::PerMinute(feed),
                moves,
                notes: Vec::new(),
            },
        ));
        ordinal += 1;
    }

    let mut drills: Vec<(usize, Operation)> = Vec::new();
    let mut helical: Vec<(usize, Operation)> = Vec::new();
    for area in &areas {
        // A round hole with nothing in it is drilled or bored, not pocketed.
        if area.kind == AreaKind::Pocket
            && area.region.holes.is_empty()
            && let Some((center, radius)) = full_circle(&area.region.outer)
        {
            let diameter = radius * 2.0;
            let through = (area.floor - stock_bottom).abs() <= 1.0e-9;
            if through && let Some(drill) = library.drill_of(diameter) {
                let (rpm, feed) = drill_feed(drill, material, max_rpm);
                let depth = area.floor - DRILL_POINT * diameter - BREAK_THROUGH;
                drills.push((
                    ordinal,
                    Operation {
                        kind: OperationKind::Drill,
                        name: format!(
                            "Drill Ø{diameter:.2} through at ({:.2}, {:.2})",
                            center.x, center.y
                        ),
                        tool: drill.number,
                        spindle: Spindle::Rpm(rpm),
                        feed: FeedRate::PerMinute(feed),
                        moves: vec![
                            Move::Rapid {
                                to: Point3::new(center.x, center.y, safe),
                            },
                            Move::Drill {
                                x: center.x,
                                y: center.y,
                                depth,
                                retract: area.top + RETRACT,
                                peck: Some(diameter),
                            },
                            Move::Rapid {
                                to: Point3::new(center.x, center.y, safe),
                            },
                        ],
                        notes: vec![
                            "G83 peck cycle, one diameter per peck; the point runs 0.3 D past the bottom plus 0.5 mm break-through.".to_owned(),
                        ],
                    },
                ));
                ordinal += 1;
                continue;
            }
            // Helical bore with the largest end mill that leaves a helix.
            let Some(mill) = library.largest_end_mill(diameter - 0.5, f64::INFINITY) else {
                return Err(CamRefusal::NoToolFits {
                    detail: format!("no end mill fits a Ø{diameter:.2} hole"),
                });
            };
            let (rpm, feed) = mill_feed(mill, material, max_rpm);
            let helix_radius = radius - mill.radius();
            let pitch = mill.max_depth_of_cut.min(area.top - area.floor).max(0.1);
            let mut moves = Vec::new();
            let start = Point2::new(center.x + helix_radius, center.y);
            moves.push(Move::Rapid {
                to: Point3::new(start.x, start.y, safe),
            });
            moves.push(Move::Rapid {
                to: Point3::new(start.x, start.y, area.top + RETRACT),
            });
            moves.push(Move::Feed {
                to: Point3::new(start.x, start.y, area.top),
            });
            let mut z = area.top;
            while z > area.floor + 1.0e-9 {
                z = (z - pitch).max(area.floor);
                // One turn as two half arcs, descending.
                let opposite = Point2::new(center.x - helix_radius, center.y);
                let mid_z = (moves.last().map(Move::end).map_or(z, |p| p.z) + z) / 2.0;
                moves.push(Move::Arc {
                    to: Point3::new(opposite.x, opposite.y, mid_z),
                    center,
                    clockwise: true,
                });
                moves.push(Move::Arc {
                    to: Point3::new(start.x, start.y, z),
                    center,
                    clockwise: true,
                });
            }
            // A flat turn at the bottom cleans the floor.
            let opposite = Point2::new(center.x - helix_radius, center.y);
            moves.push(Move::Arc {
                to: Point3::new(opposite.x, opposite.y, area.floor),
                center,
                clockwise: true,
            });
            moves.push(Move::Arc {
                to: Point3::new(start.x, start.y, area.floor),
                center,
                clockwise: true,
            });
            moves.push(Move::Rapid {
                to: Point3::new(center.x, center.y, area.floor + 0.1),
            });
            moves.push(Move::Rapid {
                to: Point3::new(center.x, center.y, safe),
            });
            helical.push((
                ordinal,
                Operation {
                    kind: OperationKind::HelicalBore,
                    name: format!(
                        "Helical bore Ø{diameter:.2} to z = {:.2} at ({:.2}, {:.2})",
                        area.floor, center.x, center.y
                    ),
                    tool: mill.number,
                    spindle: Spindle::Rpm(rpm),
                    feed: FeedRate::PerMinute(feed),
                    moves,
                    notes: vec![format!(
                        "No drill of Ø{diameter:.2}{}; bored with the Ø{:.0} end mill on a helix of radius {helix_radius:.2}.",
                        if through { "" } else { " for a flat floor" },
                        mill.diameter
                    )],
                },
            ));
            ordinal += 1;
            continue;
        }

        let (tool, sharp) = match area.kind {
            AreaKind::Pocket => {
                let corner_radius = inside_corner_radius(&area.region);
                choose_pocket_tool(library, &area.region, corner_radius)?
            }
            // Around the outline only the outline's own inside corners
            // constrain the tool; the stock's corners may keep their waste.
            AreaKind::Ring => {
                let outline_only = PlanarRegion2 {
                    outer: geom::rectangle(Point2::new(-1.0e6, -1.0e6), Point2::new(1.0e6, 1.0e6)),
                    holes: area.region.holes.clone(),
                };
                let corner_radius = inside_corner_radius(&PlanarRegion2 {
                    outer: PlanarLoop2 { curves: Vec::new() },
                    holes: area.region.holes.clone(),
                });
                choose_ring_tool(library, &outline_only, corner_radius)?
            }
        };
        let (rpm, feed) = mill_feed(tool, material, max_rpm);
        let mut area_notes = Vec::new();
        if sharp {
            area_notes.push(format!(
                "The outline has sharp inside corners; the Ø{:.0} tool leaves them with a {:.1} mm radius.",
                tool.diameter,
                tool.radius()
            ));
        }
        let (contours, fallback) = match area.kind {
            AreaKind::Pocket => pocket_contours(&area.region, tool),
            AreaKind::Ring => ring_contours(&area.region, tool, stock_min, stock_max),
        };
        if let Some(reason) = fallback {
            area_notes.push(reason);
        }
        if contours.is_empty() {
            return Err(CamRefusal::Pocket {
                detail: format!(
                    "the Ø{:.0} tool leaves no contour in the area at z = {:.2}",
                    tool.diameter, area.floor
                ),
            });
        }
        let mut moves = Vec::new();
        let doc = tool.max_depth_of_cut;
        let passes = ((area.top - area.floor) / doc).ceil().max(1.0) as usize;
        for pass in 0..passes {
            let z = if pass + 1 == passes {
                area.floor
            } else {
                area.top - (pass as f64 + 1.0) * doc
            };
            let z_above = if pass == 0 {
                area.top
            } else {
                area.top - pass as f64 * doc
            };
            emit_contours(&contours, z, z_above, area.top + RETRACT, safe, &mut moves);
        }
        let name = match area.kind {
            AreaKind::Pocket => format!(
                "Pocket to z = {:.2}, {} contour{}, {passes} pass{}",
                area.floor,
                contours.len(),
                if contours.len() == 1 { "" } else { "s" },
                if passes == 1 { "" } else { "es" }
            ),
            AreaKind::Ring => format!(
                "Profile the outline to z = {:.2}, {passes} pass{}",
                area.floor,
                if passes == 1 { "" } else { "es" }
            ),
        };
        operations.push((
            ordinal,
            Operation {
                kind: match area.kind {
                    AreaKind::Pocket => OperationKind::Pocket,
                    AreaKind::Ring => OperationKind::Profile,
                },
                name,
                tool: tool.number,
                spindle: Spindle::Rpm(rpm),
                feed: FeedRate::PerMinute(feed),
                moves,
                notes: area_notes,
            },
        ));
        ordinal += 1;
    }

    // Order: face, then pockets and profiles grouped by tool largest first
    // (pockets before profiles within a tool, shallow before deep), then
    // helical bores, then drills largest first.
    let face = operations.remove(0).1;
    let diameter_of = |number: u32| library.tool(number).map_or(0.0, |tool| tool.diameter);
    operations.sort_by(|(a_index, a), (b_index, b)| {
        diameter_of(b.tool)
            .total_cmp(&diameter_of(a.tool))
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a_index.cmp(b_index))
    });
    helical.sort_by(|(a_index, a), (b_index, b)| {
        diameter_of(b.tool)
            .total_cmp(&diameter_of(a.tool))
            .then_with(|| a_index.cmp(b_index))
    });
    drills.sort_by(|(a_index, a), (b_index, b)| {
        diameter_of(b.tool)
            .total_cmp(&diameter_of(a.tool))
            .then_with(|| a_index.cmp(b_index))
    });
    let mut ordered = vec![face];
    ordered.extend(operations.into_iter().map(|(_, operation)| operation));
    ordered.extend(helical.into_iter().map(|(_, operation)| operation));
    ordered.extend(drills.into_iter().map(|(_, operation)| operation));

    let mut tools: Vec<Tool> = Vec::new();
    for operation in &ordered {
        if !tools.iter().any(|tool| tool.number == operation.tool)
            && let Some(tool) = library.tool(operation.tool)
        {
            tools.push(tool.clone());
        }
    }
    notes.push(format!(
        "Stock {:.1} × {:.1} × {:.1} mm, {:.1} mm per side and {:.1} mm on top; work origin at the {}; spindle along world {}.",
        stock_max.x - stock_min.x,
        stock_max.y - stock_min.y,
        stock_top - stock_bottom,
        setup.allowances.side,
        setup.allowances.top,
        setup.work_origin.label(),
        setup.axis_label
    ));
    notes.push(
        "Through cuts stop at the part's bottom face; give the machine a spoilboard clearance below it.".to_owned(),
    );
    Ok(Plan {
        machine: Machine::Mill,
        material,
        setup_name: format!(
            "Milled part, {:.1} × {:.1} × {:.1} mm",
            stock_max.x - stock_min.x - 2.0 * setup.allowances.side,
            stock_max.y - stock_min.y - 2.0 * setup.allowances.side,
            part_top - stock_bottom
        ),
        operations: ordered,
        tools,
        safe_height: safe,
        rapid_rate: library.rapid_rate,
        tool_change_seconds: library.tool_change_seconds,
        notes,
    })
}

/// Whether two regions have the same loops, to the point agreement.
fn same_region(first: &PlanarRegion2, second: &PlanarRegion2) -> bool {
    if first.holes.len() != second.holes.len() {
        return false;
    }
    crate::stock::loops_agree(&first.outer, &second.outer, 1.0e-9).is_ok()
        && first.holes.iter().all(|hole| {
            second
                .holes
                .iter()
                .any(|other| crate::stock::loops_agree(hole, other, 1.0e-9).is_ok())
        })
}

fn translate_region(region: &PlanarRegion2, shift: &dyn Fn(Point2) -> Point2) -> PlanarRegion2 {
    PlanarRegion2 {
        outer: translate_loop(&region.outer, shift),
        holes: region
            .holes
            .iter()
            .map(|hole| translate_loop(hole, shift))
            .collect(),
    }
}

fn translate_loop(source: &PlanarLoop2, shift: &dyn Fn(Point2) -> Point2) -> PlanarLoop2 {
    PlanarLoop2 {
        curves: source
            .curves
            .iter()
            .map(|curve| match curve {
                PlanarCurve2::Line { start, end } => PlanarCurve2::Line {
                    start: shift(*start),
                    end: shift(*end),
                },
                PlanarCurve2::CircularArc {
                    center,
                    start,
                    end,
                    direction,
                } => PlanarCurve2::CircularArc {
                    center: shift(*center),
                    start: shift(*start),
                    end: shift(*end),
                    direction: *direction,
                },
                PlanarCurve2::Circle {
                    center,
                    radius,
                    direction,
                } => PlanarCurve2::Circle {
                    center: shift(*center),
                    radius: *radius,
                    direction: *direction,
                },
                other => other.clone(),
            })
            .collect(),
    }
}

/// The centre and radius when a loop is one whole circle, as a `Circle` or
/// as arcs of one circle closing on themselves.
#[must_use]
pub fn full_circle(source: &PlanarLoop2) -> Option<(Point2, f64)> {
    let mut found: Option<(Point2, f64)> = None;
    for curve in &source.curves {
        let (center, radius) = match curve {
            PlanarCurve2::Circle { center, radius, .. } => (*center, *radius),
            PlanarCurve2::CircularArc { center, start, .. } => {
                (*center, geom::distance(*center, *start))
            }
            _ => return None,
        };
        match found {
            None => found = Some((center, radius)),
            Some((c, r)) => {
                if geom::distance(c, center) > 1.0e-9 || (r - radius).abs() > 1.0e-9 {
                    return None;
                }
            }
        }
    }
    found
}

/// The tightest corner the tool has to reach into: the smallest radius of
/// any arc turning towards the air, or zero where two curves meet in a
/// sharp turn towards it. Infinite when the region has no such corner.
#[must_use]
pub fn inside_corner_radius(region: &PlanarRegion2) -> f64 {
    let mut radius = f64::INFINITY;
    for source in std::iter::once(&region.outer).chain(region.holes.iter()) {
        let curves = geom::normalised(source).curves;
        let count = curves.len();
        for index in 0..count {
            let current = &curves[index];
            if let PlanarCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            } = current
            {
                let (r, _, sweep) = geom::arc_parameters(*center, *start, *end, *direction);
                if sweep > 0.0 {
                    radius = radius.min(r);
                }
            }
            let next = &curves[(index + 1) % count];
            let incoming = end_direction(current);
            let outgoing = start_direction(next);
            let turn = incoming.x.mul_add(outgoing.y, -(incoming.y * outgoing.x));
            let dot = incoming.x.mul_add(outgoing.x, incoming.y * outgoing.y);
            let angle = turn.atan2(dot);
            if angle > 1.0e-6 {
                radius = 0.0;
            }
        }
    }
    radius
}

fn start_direction(curve: &PlanarCurve2) -> Point2 {
    match curve {
        PlanarCurve2::Line { start, end } => unit(Point2::new(end.x - start.x, end.y - start.y)),
        PlanarCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } => {
            let (_, start_angle, sweep) = geom::arc_parameters(*center, *start, *end, *direction);
            let s = sweep.signum();
            Point2::new(-start_angle.sin() * s, start_angle.cos() * s)
        }
        _ => Point2::new(1.0, 0.0),
    }
}

fn end_direction(curve: &PlanarCurve2) -> Point2 {
    match curve {
        PlanarCurve2::Line { start, end } => unit(Point2::new(end.x - start.x, end.y - start.y)),
        PlanarCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } => {
            let (_, start_angle, sweep) = geom::arc_parameters(*center, *start, *end, *direction);
            let angle = start_angle + sweep;
            let s = sweep.signum();
            Point2::new(-angle.sin() * s, angle.cos() * s)
        }
        _ => Point2::new(1.0, 0.0),
    }
}

fn unit(v: Point2) -> Point2 {
    let length = v.x.hypot(v.y);
    if length <= 0.0 {
        v
    } else {
        Point2::new(v.x / length, v.y / length)
    }
}

/// The largest flat end mill that fits the region: its radius at most the
/// inside corner radius when the corners are round, and an inward offset by
/// its radius that leaves something. Returns whether the corners are sharp.
fn choose_pocket_tool<'a>(
    library: &'a ToolLibrary,
    region: &PlanarRegion2,
    corner_radius: f64,
) -> Result<(&'a Tool, bool), CamRefusal> {
    let sharp = corner_radius <= 1.0e-9;
    let mut candidates = library
        .of_kind(ToolKind::FlatEndMill)
        .filter(|tool| sharp || tool.radius() <= corner_radius + 1.0e-9)
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        b.diameter
            .total_cmp(&a.diameter)
            .then_with(|| b.flutes.cmp(&a.flutes))
    });
    for tool in candidates {
        let fits = match offset_loop(&region.outer, tool.radius()) {
            Ok(loops) => !loops.is_empty(),
            Err(_) => false,
        };
        if fits {
            return Ok((tool, sharp));
        }
    }
    Err(CamRefusal::NoToolFits {
        detail: format!(
            "no end mill in the library fits an area whose inside corners are {:.2} mm",
            corner_radius
        ),
    })
}

/// The largest flat end mill that follows the outline: its radius at most
/// the outline's inside corner radius when those corners are round, and an
/// outward offset by its radius that succeeds.
fn choose_ring_tool<'a>(
    library: &'a ToolLibrary,
    region: &PlanarRegion2,
    corner_radius: f64,
) -> Result<(&'a Tool, bool), CamRefusal> {
    let sharp = corner_radius <= 1.0e-9;
    let mut candidates = library
        .of_kind(ToolKind::FlatEndMill)
        .filter(|tool| sharp || tool.radius() <= corner_radius + 1.0e-9)
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        b.diameter
            .total_cmp(&a.diameter)
            .then_with(|| b.flutes.cmp(&a.flutes))
    });
    for tool in candidates {
        let fits = region
            .holes
            .iter()
            .all(|hole| offset_loop(hole, -tool.radius()).is_ok_and(|loops| !loops.is_empty()));
        if fits {
            return Ok((tool, sharp));
        }
    }
    Err(CamRefusal::NoToolFits {
        detail: format!(
            "no end mill in the library follows an outline whose inside corners are {:.2} mm",
            corner_radius
        ),
    })
}

/// The kernel's certified offset, after every arc that the offset would
/// collapse to a point is made the sharp corner its neighbours meet in: a
/// fillet of radius `r` offset inward by `r` is exactly its centre, which is
/// where the tool of that radius has to pass.
pub fn offset_loop(source: &PlanarLoop2, distance: f64) -> Result<Vec<PlanarLoop2>, String> {
    let sharpened = sharpen_collapsing_arcs(source, distance.abs());
    NativeKernel::offset_loop(&sharpened, distance).map_err(|error| error.to_string())
}

/// Replaces every arc turning towards the air whose radius is at most
/// `distance` and whose neighbours are lines by the point those lines meet
/// in.
fn sharpen_collapsing_arcs(source: &PlanarLoop2, distance: f64) -> PlanarLoop2 {
    let curves = geom::normalised(source).curves;
    let count = curves.len();
    if count < 3 {
        return source.clone();
    }
    let mut collapse = vec![None; count];
    for index in 0..count {
        let PlanarCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } = &curves[index]
        else {
            continue;
        };
        let (radius, _, sweep) = geom::arc_parameters(*center, *start, *end, *direction);
        if sweep <= 0.0 || radius > distance + 1.0e-9 {
            continue;
        }
        let previous = &curves[(index + count - 1) % count];
        let next = &curves[(index + 1) % count];
        if let (
            PlanarCurve2::Line { start: a1, end: b1 },
            PlanarCurve2::Line { start: a2, end: b2 },
        ) = (previous, next)
            && let Some(meeting) = line_meeting(*a1, *b1, *a2, *b2)
        {
            collapse[index] = Some(meeting);
        }
    }
    if collapse.iter().all(Option::is_none) {
        return source.clone();
    }
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        if collapse[index].is_some() {
            continue;
        }
        let mut curve = curves[index].clone();
        let before = collapse[(index + count - 1) % count];
        let after = collapse[(index + 1) % count];
        if let PlanarCurve2::Line { start, end } = &mut curve {
            if let Some(meeting) = before {
                *start = meeting;
            }
            if let Some(meeting) = after {
                *end = meeting;
            }
        }
        out.push(curve);
    }
    PlanarLoop2 { curves: out }
}

fn line_meeting(a1: Point2, b1: Point2, a2: Point2, b2: Point2) -> Option<Point2> {
    let d1 = Point2::new(b1.x - a1.x, b1.y - a1.y);
    let d2 = Point2::new(b2.x - a2.x, b2.y - a2.y);
    let denominator = d1.x.mul_add(d2.y, -(d1.y * d2.x));
    if denominator.abs() <= 1.0e-12 {
        return None;
    }
    let t = ((a2.x - a1.x) * d2.y - (a2.y - a1.y) * d2.x) / denominator;
    Some(Point2::new(d1.x.mul_add(t, a1.x), d1.y.mul_add(t, a1.y)))
}

/// The pocket's contours innermost first: inward offsets of the outer loop
/// at the tool radius plus every stepover, less the islands offset outward
/// by the same. Falls back to a raster inside the wall contour where an
/// offset fails before the region vanishes, and says so.
fn pocket_contours(region: &PlanarRegion2, tool: &Tool) -> (Vec<Vec<PlanarLoop2>>, Option<String>) {
    let radius = tool.radius();
    let step = tool.diameter * POCKET_STEPOVER;
    let mut rings: Vec<Vec<PlanarLoop2>> = Vec::new();
    let mut distance = radius;
    let mut fallback = None;
    loop {
        match offset_region(region, distance) {
            Ok(loops) if loops.is_empty() => break,
            Ok(loops) => rings.push(loops),
            Err(reason) => {
                fallback = Some(format!(
                    "The offset at {distance:.2} mm failed ({reason}); the interior is rastered instead."
                ));
                break;
            }
        }
        distance += step;
    }
    if let Some(reason) = fallback.clone() {
        let _ = reason;
        // Raster the wall contour's interior, then keep the wall pass.
        if let Some(wall) = rings.first().cloned() {
            let raster = raster_loops(&wall, step);
            rings = vec![raster, wall];
        }
    }
    rings.reverse();
    (rings, fallback)
}

/// The ring's contours: outward offsets of the outline loops from the tool
/// radius out until the stock's corners are covered, outermost first so the
/// wall pass comes last.
fn ring_contours(
    region: &PlanarRegion2,
    tool: &Tool,
    stock_min: Point2,
    stock_max: Point2,
) -> (Vec<Vec<PlanarLoop2>>, Option<String>) {
    let radius = tool.radius();
    let step = tool.diameter * POCKET_STEPOVER;
    // How far the stock's corners are from the outlines.
    let corners = [
        stock_min,
        stock_max,
        Point2::new(stock_min.x, stock_max.y),
        Point2::new(stock_max.x, stock_min.y),
    ];
    let reach = corners
        .iter()
        .map(|corner| {
            region
                .holes
                .iter()
                .map(|hole| geom::distance_to_loop(hole, *corner))
                .fold(f64::INFINITY, f64::min)
        })
        .fold(0.0_f64, f64::max);
    let mut rings: Vec<Vec<PlanarLoop2>> = Vec::new();
    let mut distance = radius;
    let mut fallback = None;
    loop {
        let mut loops = Vec::new();
        for hole in &region.holes {
            match offset_loop(hole, -distance) {
                Ok(offset) => loops.extend(offset),
                Err(error) => {
                    fallback = Some(format!(
                        "The outward offset at {distance:.2} mm failed ({error}); the outer passes stop there."
                    ));
                }
            }
        }
        if loops.is_empty() {
            break;
        }
        rings.push(loops);
        if distance + radius >= reach || fallback.is_some() {
            break;
        }
        distance += step;
    }
    // Outermost first.
    rings.reverse();
    (rings, fallback)
}

/// The region offset inward by `distance`: the outer loop in, the islands
/// out, combined through the kernel's Boolean when there are islands.
fn offset_region(region: &PlanarRegion2, distance: f64) -> Result<Vec<PlanarLoop2>, String> {
    let outer = offset_loop(&region.outer, distance)?;
    let Some(outer) = outer.into_iter().next() else {
        return Ok(Vec::new());
    };
    if region.holes.is_empty() {
        return Ok(vec![outer]);
    }
    let mut islands = Vec::new();
    for hole in &region.holes {
        let grown = offset_loop(hole, -distance)?;
        for grown in grown {
            islands.push(PlanarRegion2 {
                outer: geom::counter_clockwise(&grown),
                holes: Vec::new(),
            });
        }
    }
    let result = NativeKernel::profile_boolean(
        &[PlanarRegion2 {
            outer,
            holes: Vec::new(),
        }],
        &islands,
        BooleanOperation::Difference,
        PrecisionPolicy::default(),
    )
    .map_err(|e| e.to_string())?;
    let mut loops = Vec::new();
    for piece in result {
        loops.push(piece.outer);
        loops.extend(piece.holes);
    }
    Ok(loops)
}

/// Zigzag scan lines inside a set of loops, as open two-point loops.
fn raster_loops(walls: &[PlanarLoop2], step: f64) -> Vec<PlanarLoop2> {
    let Some((min, max)) = walls
        .iter()
        .map(geom::bounds)
        .fold(None, geom::bounds_union)
    else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    let mut y = min.y + step / 2.0;
    let mut forward = true;
    while y < max.y {
        let mut crossings = Vec::new();
        for wall in walls {
            crossings.extend(geom::loop_crossings(wall, y));
        }
        crossings.sort_by(f64::total_cmp);
        for pair in crossings.chunks(2) {
            if pair.len() < 2 {
                continue;
            }
            let (a, b) = if forward {
                (pair[0], pair[1])
            } else {
                (pair[1], pair[0])
            };
            lines.push(PlanarLoop2 {
                curves: vec![PlanarCurve2::Line {
                    start: Point2::new(a, y),
                    end: Point2::new(b, y),
                }],
            });
        }
        forward = !forward;
        y += step;
    }
    lines
}

/// Emits one depth pass over the contours: a ramp into the first loop, then
/// every loop in order, then a retract.
fn emit_contours(
    contours: &[Vec<PlanarLoop2>],
    z: f64,
    z_above: f64,
    retract: f64,
    safe: f64,
    moves: &mut Vec<Move>,
) {
    let mut first = true;
    for ring in contours {
        for source in ring {
            let Some(start) = source.curves.first().map(geom::curve_start) else {
                continue;
            };
            let open =
                source.curves.len() == 1 && matches!(source.curves[0], PlanarCurve2::Line { .. });
            if first {
                moves.push(Move::Rapid {
                    to: Point3::new(start.x, start.y, safe),
                });
                moves.push(Move::Rapid {
                    to: Point3::new(start.x, start.y, retract),
                });
                moves.push(Move::Feed {
                    to: Point3::new(start.x, start.y, z_above),
                });
                // Ramp down along the first curve and come back.
                if let Some(curve) = source.curves.first() {
                    let samples = geom::sample_curve(curve, 0.05);
                    let ramp_to = if samples.len() < 3 {
                        geom::curve_end(curve)
                    } else {
                        samples[samples.len() / 2]
                    };
                    moves.push(Move::Feed {
                        to: Point3::new(ramp_to.x, ramp_to.y, z),
                    });
                    moves.push(Move::Feed {
                        to: Point3::new(start.x, start.y, z),
                    });
                }
                first = false;
            } else {
                moves.push(Move::Feed {
                    to: Point3::new(start.x, start.y, z),
                });
            }
            for curve in &source.curves {
                match curve {
                    PlanarCurve2::Line { end, .. } => moves.push(Move::Feed {
                        to: Point3::new(end.x, end.y, z),
                    }),
                    PlanarCurve2::CircularArc {
                        center,
                        end,
                        direction,
                        ..
                    } => moves.push(Move::Arc {
                        to: Point3::new(end.x, end.y, z),
                        center: *center,
                        clockwise: *direction == ArcDirection::Clockwise,
                    }),
                    PlanarCurve2::Circle {
                        center,
                        radius,
                        direction,
                    } => {
                        let clockwise = *direction == ArcDirection::Clockwise;
                        let right = Point3::new(center.x + radius, center.y, z);
                        let left = Point3::new(center.x - radius, center.y, z);
                        moves.push(Move::Arc {
                            to: left,
                            center: *center,
                            clockwise,
                        });
                        moves.push(Move::Arc {
                            to: right,
                            center: *center,
                            clockwise,
                        });
                    }
                    PlanarCurve2::Bspline { .. } => {}
                }
            }
            if open {
                continue;
            }
        }
    }
    if let Some(last) = moves.last().map(Move::end) {
        moves.push(Move::Rapid {
            to: Point3::new(last.x, last.y, safe),
        });
    }
}
