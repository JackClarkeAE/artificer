//! Simulation: motions drive the stock model and the clock.
//!
//! A [`Motion`] is what a machine does between two points, whether it came
//! from the plan directly or from the interpreter reading the posted G-code
//! (ADR 0057 §2.6). The lathe simulation subtracts each cutting motion's
//! swept tool region from the exact section; the mill simulation lowers the
//! heightmap under the tool.

use artificer_protocol::{PlanarLoop2, PlanarRegion2, Point2, Point3};

use crate::CamRefusal;
use crate::geom;
use crate::plan::{FeedRate, Machine, Move, Plan, Spindle};
use crate::stock::LatheStock;
use crate::tools::{Tool, rpm_for};
use crate::turning::{swept_region, tool_region};

/// How a motion moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MotionKind {
    Rapid,
    Feed,
    Arc { center: Point2, clockwise: bool },
}

/// One motion of the machine between two points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Motion {
    pub from: Point3,
    pub to: Point3,
    pub kind: MotionKind,
    pub tool: u32,
    /// The operation this motion belongs to, when it came from a plan.
    pub operation: usize,
    pub feed: FeedRate,
    pub spindle: Spindle,
    /// The G-code line that produced it, one-based; zero from a plan.
    pub line: usize,
}

impl Motion {
    #[must_use]
    pub fn is_cutting(&self) -> bool {
        !matches!(self.kind, MotionKind::Rapid)
    }

    /// The path length in millimetres, the arc along its curve.
    #[must_use]
    pub fn length(&self, machine: Machine) -> f64 {
        match self.kind {
            MotionKind::Rapid | MotionKind::Feed => {
                let d = crate::space::sub(self.to, self.from);
                crate::space::length(d)
            }
            MotionKind::Arc { center, clockwise } => {
                let (from, to) = plane_points(self.from, self.to, machine);
                let (radius, _, sweep) = geom::arc_parameters(
                    center,
                    from,
                    to,
                    if clockwise {
                        artificer_protocol::ArcDirection::Clockwise
                    } else {
                        artificer_protocol::ArcDirection::CounterClockwise
                    },
                );
                let planar = radius * sweep.abs();
                let axial = match machine {
                    Machine::Mill => self.to.z - self.from.z,
                    Machine::Lathe => self.to.y - self.from.y,
                };
                planar.hypot(axial)
            }
        }
    }

    /// The motion sampled as a polyline, arcs to within `tolerance`.
    #[must_use]
    pub fn sampled(&self, machine: Machine, tolerance: f64) -> Vec<Point3> {
        match self.kind {
            MotionKind::Rapid | MotionKind::Feed => vec![self.from, self.to],
            MotionKind::Arc { center, clockwise } => {
                let (from, to) = plane_points(self.from, self.to, machine);
                let curve = artificer_protocol::PlanarCurve2::CircularArc {
                    center,
                    start: from,
                    end: to,
                    direction: if clockwise {
                        artificer_protocol::ArcDirection::Clockwise
                    } else {
                        artificer_protocol::ArcDirection::CounterClockwise
                    },
                };
                let points = geom::sample_curve(&curve, tolerance);
                let count = points.len().max(2);
                points
                    .into_iter()
                    .enumerate()
                    .map(|(index, p)| {
                        let t = index as f64 / (count - 1) as f64;
                        match machine {
                            Machine::Mill => Point3::new(
                                p.x,
                                p.y,
                                (self.to.z - self.from.z).mul_add(t, self.from.z),
                            ),
                            Machine::Lathe => Point3::new(p.x, 0.0, p.y),
                        }
                    })
                    .collect()
            }
        }
    }
}

/// The two working-plane coordinates of a point: `(x, y)` on the mill,
/// `(r, z)` on the lathe.
#[must_use]
pub fn plane_points(from: Point3, to: Point3, machine: Machine) -> (Point2, Point2) {
    match machine {
        Machine::Mill => (Point2::new(from.x, from.y), Point2::new(to.x, to.y)),
        Machine::Lathe => (Point2::new(from.x, from.z), Point2::new(to.x, to.z)),
    }
}

/// The retract the tool rapids to above a hole in a canned cycle, and the
/// little it backs off before the next peck (LinuxCNC's `G83`).
pub const PECK_BACKOFF: f64 = 0.5;

/// Expands one programmed move into motions from `current`, the canned
/// cycles into their rapids and feeds exactly as the interpreter does.
pub fn expand_move(
    current: &mut Point3,
    m: &Move,
    tool: u32,
    operation: usize,
    feed: FeedRate,
    spindle: Spindle,
    line: usize,
    out: &mut Vec<Motion>,
) {
    let mut push = |from: Point3, to: Point3, kind: MotionKind| {
        if from != to {
            out.push(Motion {
                from,
                to,
                kind,
                tool,
                operation,
                feed,
                spindle,
                line,
            });
        }
    };
    match *m {
        Move::Rapid { to } => {
            push(*current, to, MotionKind::Rapid);
            *current = to;
        }
        Move::Feed { to } => {
            push(*current, to, MotionKind::Feed);
            *current = to;
        }
        Move::Arc {
            to,
            center,
            clockwise,
        } => {
            push(*current, to, MotionKind::Arc { center, clockwise });
            *current = to;
        }
        Move::Drill {
            x,
            y,
            depth,
            retract,
            peck,
        } => {
            // Rapid over the hole at the current height, rapid to the
            // retract plane, feed (pecking) to depth, rapid back up.
            let over = Point3::new(x, y, current.z.max(retract));
            push(*current, over, MotionKind::Rapid);
            let plane = Point3::new(x, y, retract);
            push(over, plane, MotionKind::Rapid);
            let mut position = plane;
            match peck {
                Some(peck) if peck > 0.0 => {
                    let mut bottom = retract;
                    while bottom > depth + 1.0e-12 {
                        bottom = (bottom - peck).max(depth);
                        let target = Point3::new(x, y, bottom);
                        push(position, target, MotionKind::Feed);
                        push(target, plane, MotionKind::Rapid);
                        position = plane;
                        if bottom > depth + 1.0e-12 {
                            let back = Point3::new(x, y, bottom + PECK_BACKOFF);
                            push(position, back, MotionKind::Rapid);
                            position = back;
                        }
                    }
                }
                _ => {
                    let target = Point3::new(x, y, depth);
                    push(position, target, MotionKind::Feed);
                    push(target, plane, MotionKind::Rapid);
                }
            }
            *current = plane;
        }
    }
}

/// The plan's own motions, operation by operation, from the safe position.
#[must_use]
pub fn motions_from_plan(plan: &Plan) -> Vec<Motion> {
    let mut out = Vec::new();
    let mut current = start_position(plan);
    for (index, operation) in plan.operations.iter().enumerate() {
        for m in &operation.moves {
            expand_move(
                &mut current,
                m,
                operation.tool,
                index,
                operation.feed,
                operation.spindle,
                0,
                &mut out,
            );
        }
    }
    out
}

/// Where the machine sits before the program: at the safe height over the
/// origin on the mill, clear of the bar's front on the lathe.
#[must_use]
pub fn start_position(plan: &Plan) -> Point3 {
    match plan.machine {
        Machine::Mill => Point3::new(0.0, 0.0, plan.safe_height),
        Machine::Lathe => Point3::new(plan.safe_height, 0.0, plan.safe_height),
    }
}

/// How long a motion takes, in seconds.
///
/// A rapid runs at the rapid rate. A feed per minute is what it says. A
/// feed per revolution under constant surface speed turns at the spindle
/// speed the mean radius of the move gives, capped by the spindle: an
/// approximation on a facing cut, where the speed rises as the tool nears
/// the centre.
#[must_use]
pub fn motion_seconds(motion: &Motion, machine: Machine, rapid_rate: f64) -> f64 {
    let length = motion.length(machine);
    let rate = match motion.kind {
        MotionKind::Rapid => rapid_rate,
        _ => match motion.feed {
            FeedRate::PerMinute(rate) => rate,
            FeedRate::PerRevolution(per_rev) => {
                let rpm = match motion.spindle {
                    Spindle::Rpm(rpm) => rpm,
                    Spindle::SurfaceSpeed {
                        metres_per_minute,
                        max_rpm,
                    } => {
                        let mean_radius = ((motion.from.x + motion.to.x) / 2.0).abs();
                        rpm_for(metres_per_minute, mean_radius * 2.0, max_rpm)
                    }
                };
                per_rev * rpm
            }
        },
    };
    if rate <= 0.0 {
        return 0.0;
    }
    length / rate * 60.0
}

/// One thing that went wrong during a simulation.
#[derive(Clone, Debug, PartialEq)]
pub struct Collision {
    pub motion: usize,
    pub line: usize,
    pub detail: String,
}

/// A lathe simulation: the section after every motion that changed it.
#[derive(Clone, Debug)]
pub struct LatheSimulation {
    pub motions: Vec<Motion>,
    /// The stock before any motion.
    pub initial: LatheStock,
    /// `(motion index, stock after it)` for every motion that cut.
    pub states: Vec<(usize, LatheStock)>,
    /// When each motion ends, in seconds from the start.
    pub ends: Vec<f64>,
    pub collisions: Vec<Collision>,
    pub total_seconds: f64,
}

impl LatheSimulation {
    /// Runs `motions` over `stock` with the plan's tools.
    pub fn run(plan: &Plan, motions: Vec<Motion>, stock: LatheStock) -> Result<Self, CamRefusal> {
        let mut current = stock.clone();
        let mut states = Vec::new();
        let mut ends = Vec::with_capacity(motions.len());
        let mut collisions = Vec::new();
        let mut clock = 0.0;
        let mut previous_tool = None;
        for (index, motion) in motions.iter().enumerate() {
            if previous_tool.is_some_and(|tool| tool != motion.tool) {
                clock += plan.tool_change_seconds;
            }
            previous_tool = Some(motion.tool);
            clock += motion_seconds(motion, Machine::Lathe, plan.rapid_rate);
            ends.push(clock);
            let Some(tool) = plan.tool(motion.tool) else {
                collisions.push(Collision {
                    motion: index,
                    line: motion.line,
                    detail: format!("tool T{} is not in the plan", motion.tool),
                });
                continue;
            };
            if motion.is_cutting() {
                let region = tool_region(tool);
                let points = motion.sampled(Machine::Lathe, 0.005);
                for pair in points.windows(2) {
                    let from = Point2::new(pair[0].x, pair[0].z);
                    let to = Point2::new(pair[1].x, pair[1].z);
                    let swept = swept_region(&region, from, to);
                    current.cut(&swept)?;
                }
                states.push((index, current.clone()));
            } else {
                // A rapid must not pass through material: sample the tip's
                // path against the current section.
                let steps = (motion.length(Machine::Lathe) / 0.25).ceil().max(1.0) as usize;
                let inside = (0..=steps).any(|step| {
                    let t = step as f64 / steps as f64;
                    let p = Point2::new(
                        (motion.to.x - motion.from.x).mul_add(t, motion.from.x),
                        (motion.to.z - motion.from.z).mul_add(t, motion.from.z),
                    );
                    current.contains_with_margin(p, 1.0e-6)
                });
                if inside {
                    collisions.push(Collision {
                        motion: index,
                        line: motion.line,
                        detail: format!(
                            "rapid from ({:.3}, {:.3}) to ({:.3}, {:.3}) passes through stock",
                            motion.from.x, motion.from.z, motion.to.x, motion.to.z
                        ),
                    });
                }
            }
        }
        Ok(Self {
            motions,
            initial: stock,
            states,
            ends,
            collisions,
            total_seconds: clock,
        })
    }

    /// The final stock.
    #[must_use]
    pub fn final_stock(&self) -> &LatheStock {
        self.states.last().map_or(&self.initial, |(_, stock)| stock)
    }

    /// The stock as it stands after motion `index`.
    #[must_use]
    pub fn stock_after(&self, index: usize) -> &LatheStock {
        match self.states.binary_search_by(|(at, _)| at.cmp(&index)) {
            Ok(found) => &self.states[found].1,
            Err(0) => &self.initial,
            Err(insert) => &self.states[insert - 1].1,
        }
    }

    /// The motion in progress at `seconds`, and how far along it is.
    #[must_use]
    pub fn at(&self, seconds: f64) -> Option<(usize, f64)> {
        motion_at(&self.ends, seconds)
    }
}

/// The motion in progress at `seconds` and its fraction done, given the
/// times motions end at.
#[must_use]
pub fn motion_at(ends: &[f64], seconds: f64) -> Option<(usize, f64)> {
    if ends.is_empty() {
        return None;
    }
    let index = ends
        .partition_point(|end| *end < seconds)
        .min(ends.len() - 1);
    let start = if index == 0 { 0.0 } else { ends[index - 1] };
    let duration = ends[index] - start;
    let fraction = if duration <= 0.0 {
        1.0
    } else {
        ((seconds - start) / duration).clamp(0.0, 1.0)
    };
    Some((index, fraction))
}

/// The tool's position part-way along a motion.
#[must_use]
pub fn position_along(motion: &Motion, machine: Machine, fraction: f64) -> Point3 {
    let points = motion.sampled(machine, 0.01);
    if points.len() < 2 {
        return motion.to;
    }
    let total = points
        .windows(2)
        .map(|pair| crate::space::length(crate::space::sub(pair[1], pair[0])))
        .sum::<f64>();
    let mut target = total * fraction.clamp(0.0, 1.0);
    for pair in points.windows(2) {
        let step = crate::space::length(crate::space::sub(pair[1], pair[0]));
        if target <= step || step <= 0.0 {
            let t = if step <= 0.0 { 1.0 } else { target / step };
            return Point3::new(
                (pair[1].x - pair[0].x).mul_add(t, pair[0].x),
                (pair[1].y - pair[0].y).mul_add(t, pair[0].y),
                (pair[1].z - pair[0].z).mul_add(t, pair[0].z),
            );
        }
        target -= step;
    }
    motion.to
}

/// The tool region at a lathe position, as a loop in `(r, z)`.
#[must_use]
pub fn lathe_tool_at(tool: &Tool, position: Point3) -> PlanarLoop2 {
    let region = tool_region(tool);
    geom::polygon(
        &region
            .iter()
            .map(|p| Point2::new(p.x + position.x, p.y + position.z))
            .collect::<Vec<_>>(),
    )
}

/// A region as the workbench draws it: its outer loop sampled.
#[must_use]
pub fn region_outline(region: &PlanarRegion2, tolerance: f64) -> Vec<Point2> {
    geom::sample_loop(&region.outer, tolerance)
}
