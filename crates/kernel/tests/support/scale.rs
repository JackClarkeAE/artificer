//! Bodies of hundreds to thousands of faces, built through the public API
//! the way a user builds them, with their volumes in closed form (ADR 0056,
//! Track R).
//!
//! Every generator here is deterministic and cheap to describe, so the same
//! body can be built by a bench, a regression test and a profiling run and
//! be the same body each time. The sizes are chosen so the face count is a
//! simple function of `n`, and the volumes are what the arithmetic says
//! rather than what the kernel reports.
//!
//! The benches include this file by path, so it must stay free of test-only
//! dependencies.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Instant;

use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, ExecuteRequest,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, RotationQuaternion, SimilarityTransform3, Vector3,
};

/// The grid pitch of every hole and boss array, in millimetres.
pub const PITCH: f64 = 10.0;
/// The diameter of every drilled hole.
pub const HOLE_DIAMETER: f64 = 4.0;
/// The thickness of every plate.
pub const PLATE_THICKNESS: f64 = 5.0;
/// The diameter and height of every boss.
pub const BOSS_DIAMETER: f64 = 6.0;
pub const BOSS_HEIGHT: f64 = 4.0;
/// The radius of every hole-rim fillet.
pub const RIM_FILLET: f64 = 0.8;

/// The side of the square plate an `n × n` array sits on.
#[must_use]
pub fn plate_side(n: usize) -> f64 {
    PITCH * n as f64
}

/// The centre of grid cell `(i, j)` in the top face's own frame, whose
/// origin is the face centre.
#[must_use]
pub fn cell_centre(n: usize, i: usize, j: usize) -> (f64, f64) {
    let side = plate_side(n);
    (
        -side / 2.0 + PITCH / 2.0 + PITCH * i as f64,
        -side / 2.0 + PITCH / 2.0 + PITCH * j as f64,
    )
}

/// Runs a script into a fresh session and returns it, failing loudly.
pub fn run_script(source: &str) -> Session {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(
        outcome.succeeded(),
        "the fixture script should build: {:?}",
        outcome.failure
    );
    session
}

// ---------------------------------------------------------------------------
// A plate with an n × n grid of drilled holes
// ---------------------------------------------------------------------------

/// The script of a square plate of `side`, centred on the origin with its
/// top at `PLATE_THICKNESS`, with a hole drilled through it at every centre.
#[must_use]
pub fn plate_script(side: f64, centres: &[(f64, f64)]) -> String {
    let mut script = String::new();
    let _ = writeln!(
        script,
        "let plate = box(origin: [{}, {}, 0], size: [{side}, {side}, {PLATE_THICKNESS}], label: \"plate\");",
        -side / 2.0,
        -side / 2.0
    );
    let _ = writeln!(script, "let top = plate.face(\"top_face\");");
    for (index, (x, y)) in centres.iter().enumerate() {
        let _ = writeln!(
            script,
            "drill(face: top, center: [{x}, {y}], diameter: {HOLE_DIAMETER}, depth: {PLATE_THICKNESS}, label: \"h_{index}\");"
        );
    }
    script
}

/// The script of a plate with `n × n` holes drilled one at a time, every
/// hole through the whole thickness. Faces: `6 + 2·n²`.
#[must_use]
pub fn drilled_plate_script(n: usize) -> String {
    let centres = (0..n)
        .flat_map(|i| (0..n).map(move |j| cell_centre(n, i, j)))
        .collect::<Vec<_>>();
    plate_script(plate_side(n), &centres)
}

/// A plate with `n × n` drilled holes, as the session that built it.
#[must_use]
pub fn drilled_plate(n: usize) -> Session {
    run_script(&drilled_plate_script(n))
}

/// The same plate built at a different scale and world position: every
/// length multiplied by `scale`, the whole body offset by `translate` on
/// each axis. Construction is scale-invariant, so this must come back with
/// the same topology counts and tier as [`drilled_plate`] and a volume that
/// is the base volume times `scale³`.
#[must_use]
pub fn drilled_plate_at(n: usize, scale: f64, translate: f64) -> Session {
    let side = plate_side(n) * scale;
    let thickness = PLATE_THICKNESS * scale;
    let mut script = String::new();
    let _ = writeln!(
        script,
        "let plate = box(origin: [{}, {}, {translate}], size: [{side}, {side}, {thickness}], label: \"plate\");",
        -side / 2.0 + translate,
        -side / 2.0 + translate
    );
    let _ = writeln!(script, "let top = plate.face(\"top_face\");");
    let diameter = HOLE_DIAMETER * scale;
    for i in 0..n {
        for j in 0..n {
            let (x, y) = cell_centre(n, i, j);
            let _ = writeln!(
                script,
                "drill(face: top, center: [{}, {}], diameter: {diameter}, depth: {thickness}, label: \"h_{i}_{j}\");",
                x * scale,
                y * scale
            );
        }
    }
    run_script(&script)
}

/// The exact volume of [`drilled_plate`].
#[must_use]
pub fn drilled_plate_volume(n: usize) -> f64 {
    let side = plate_side(n);
    let radius = HOLE_DIAMETER / 2.0;
    side * side * PLATE_THICKNESS
        - (n * n) as f64 * std::f64::consts::PI * radius * radius * PLATE_THICKNESS
}

/// The faces a drilled plate has: six of the box and two half-cylinders
/// per hole.
#[must_use]
pub fn drilled_plate_faces(n: usize) -> u64 {
    6 + 2 * (n * n) as u64
}

// ---------------------------------------------------------------------------
// A plate with an n × n array of bosses, one per row extruded and patterned
// ---------------------------------------------------------------------------

/// The script of a plate with `n × n` bosses: the first boss of each row is
/// a circle sketched on the plate's top face and extruded as an addition,
/// then patterned along the row. Faces: `6 + 3·n²`.
#[must_use]
pub fn boss_array_script(n: usize) -> String {
    let side = plate_side(n);
    let mut script = String::new();
    let _ = writeln!(
        script,
        "let plate = box(origin: [{}, {}, 0], size: [{side}, {side}, {PLATE_THICKNESS}], label: \"plate\");",
        -side / 2.0,
        -side / 2.0
    );
    let _ = writeln!(script, "let top = plate.face(\"top_face\");");
    for j in 0..n {
        let (x, y) = cell_centre(n, 0, j);
        let _ = writeln!(
            script,
            "let s_{j} = sketch(on: top, entities: [circle(center: [{x}, {y}], diameter: {BOSS_DIAMETER})], label: \"s_{j}\");"
        );
        let _ = writeln!(
            script,
            "let b_{j} = extrude(sketch: s_{j}, distance: {BOSS_HEIGHT}, operation: \"add\", label: \"b_{j}\");"
        );
        if n > 1 {
            let _ = writeln!(
                script,
                "pattern(step: b_{j}, direction: [1, 0, 0], spacing: {PITCH}, count: {n}, label: \"row_{j}\");"
            );
        }
    }
    script
}

/// A plate with `n × n` bosses, as the session that built it.
#[must_use]
pub fn boss_array(n: usize) -> Session {
    run_script(&boss_array_script(n))
}

/// The exact volume of [`boss_array`].
#[must_use]
pub fn boss_array_volume(n: usize) -> f64 {
    let side = plate_side(n);
    let radius = BOSS_DIAMETER / 2.0;
    side * side * PLATE_THICKNESS
        + (n * n) as f64 * std::f64::consts::PI * radius * radius * BOSS_HEIGHT
}

/// The faces a boss array has: six of the box, and per boss two
/// half-cylinder walls and a top.
#[must_use]
pub fn boss_array_faces(n: usize) -> u64 {
    6 + 3 * (n * n) as u64
}

// ---------------------------------------------------------------------------
// A plate with n holes, every hole rim filleted
// ---------------------------------------------------------------------------

/// The smallest grid that holds `count` holes.
#[must_use]
pub fn grid_for(count: usize) -> usize {
    let mut n = 1;
    while n * n < count {
        n += 1;
    }
    n
}

/// The script of a plate with `count` drilled holes on a square grid and a
/// torus fillet on every hole's top rim: `count` exact hole-rim blends.
/// A rim is two half-circle edges, so each fillet names both by a point.
#[must_use]
pub fn filleted_holes_script(count: usize) -> String {
    let n = grid_for(count);
    let radius = HOLE_DIAMETER / 2.0;
    let mut script = drilled_plate_script(n);
    let mut placed = 0;
    'grid: for i in 0..n {
        for j in 0..n {
            if placed == count {
                break 'grid;
            }
            let (x, y) = cell_centre(n, i, j);
            // Halfway along each half-circle, clear of the seam at azimuth
            // zero where the rim arc and the bore's generator both end.
            let _ = writeln!(
                script,
                "fillet(edges: [nearest(point: [{x}, {}, {PLATE_THICKNESS}], kind: \"edge\"), nearest(point: [{x}, {}, {PLATE_THICKNESS}], kind: \"edge\")], radius: {RIM_FILLET}, label: \"f_{i}_{j}\");",
                y + radius,
                y - radius
            );
            placed += 1;
        }
    }
    script
}

/// A plate with `count` holes, every rim filleted, as the session that
/// built it.
#[must_use]
pub fn filleted_holes(count: usize) -> Session {
    run_script(&filleted_holes_script(count))
}

/// The exact volume of [`filleted_holes`]: the drilled plate less, per
/// fillet, the material a quarter-torus band takes off a rim. The band
/// sweeps the corner square `r²` less the quarter disc `πr²/4` round the
/// rim's circle of radius `R + r/2`... which is Pappus: area `(1 − π/4)·r²`
/// at a centroid `R + r·(10 − 3π)/(3·(4 − π))` from the axis.
#[must_use]
pub fn filleted_holes_volume(count: usize) -> f64 {
    let n = grid_for(count);
    let hole_radius = HOLE_DIAMETER / 2.0;
    let r = RIM_FILLET;
    let area = (1.0 - std::f64::consts::FRAC_PI_4) * r * r;
    let centroid_offset =
        r * (10.0 - 3.0 * std::f64::consts::PI) / (3.0 * (4.0 - std::f64::consts::PI));
    let band = std::f64::consts::TAU * (hole_radius + centroid_offset) * area;
    drilled_plate_volume(n) - count as f64 * band
}

// ---------------------------------------------------------------------------
// A prism whose profile is a regular polygon of thousands of curves
// ---------------------------------------------------------------------------

/// A circle of `arcs` equal circular-arc curves about `center`: a profile of
/// `arcs` analytic curves, so it exercises the analytic profile path and the
/// curve cap the way a straight-edge polygon cannot (that path routes to the
/// legacy 256-vertex prism). Each arc is one curve, so the profile carries
/// exactly `arcs` curves.
#[must_use]
pub fn arc_disc(center: (f64, f64), radius: f64, arcs: usize) -> PlanarProfile2 {
    let point = |index: usize| {
        let angle = std::f64::consts::TAU * index as f64 / arcs as f64;
        Point2::new(
            radius.mul_add(angle.cos(), center.0),
            radius.mul_add(angle.sin(), center.1),
        )
    };
    let center = Point2::new(center.0, center.1);
    let curves = (0..arcs)
        .map(|index| PlanarCurve2::CircularArc {
            center,
            start: point(index),
            end: point((index + 1) % arcs),
            direction: ArcDirection::CounterClockwise,
        })
        .collect();
    PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 { curves },
            holes: vec![],
        }],
    }
}

#[must_use]
pub fn execute_request(command: KernelCommand, expected: &Snapshot) -> ExecuteRequest {
    ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("scale-fixture"),
        expected_snapshot: expected.id(),
        precision: PrecisionPolicy::default(),
        command,
    }
}

/// Runs one command on a snapshot, failing loudly.
#[must_use]
pub fn execute(input: &Snapshot, command: KernelCommand) -> Snapshot {
    NativeKernel::execute(
        input,
        &execute_request(command, input),
        &CancellationToken::new(),
    )
    .expect("the scale fixture command should execute")
    .snapshot
}

/// A prism of height `height` on an `arcs`-arc circle of `radius`. Its
/// volume is the circle's, `π·radius²·height`, exactly.
#[must_use]
pub fn arc_disc_prism(center: (f64, f64), radius: f64, arcs: usize, height: f64) -> Snapshot {
    execute(
        &NativeKernel::empty(),
        KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: arc_disc(center, radius, arcs),
            distance: height,
        },
    )
}

/// The volume of an [`arc_disc_prism`]: the full circle's, exactly.
#[must_use]
pub fn arc_disc_prism_volume(radius: f64, height: f64) -> f64 {
    std::f64::consts::PI * radius * radius * height
}

/// An arc-disc prism scaled and moved: radius and height times `scale`, the
/// whole body offset by `translate` on each axis.
#[must_use]
pub fn arc_disc_prism_at(
    radius: f64,
    arcs: usize,
    height: f64,
    scale: f64,
    translate: f64,
) -> Snapshot {
    execute(
        &NativeKernel::empty(),
        KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(translate, translate, translate),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile: arc_disc((0.0, 0.0), radius * scale, arcs),
            distance: height * scale,
        },
    )
}

// ---------------------------------------------------------------------------
// Second operands, transforms and Booleans
// ---------------------------------------------------------------------------

/// The snapshot moved by `translation` and scaled by `scale` about the
/// origin, through the committed similarity transform.
#[must_use]
pub fn transformed(snapshot: &Snapshot, translation: Vector3, scale: f64) -> Snapshot {
    execute(
        snapshot,
        KernelCommand::TransformSnapshot {
            transform: SimilarityTransform3 {
                translation,
                rotation: RotationQuaternion::IDENTITY,
                uniform_scale: scale,
            },
        },
    )
}

#[must_use]
pub fn boolean_request(
    target: &Snapshot,
    tool: &Snapshot,
    operation: BooleanOperation,
) -> BooleanRequest {
    BooleanRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("scale-boolean"),
        expected_target_snapshot: target.id(),
        expected_tool_snapshot: tool.id(),
        precision: PrecisionPolicy::default(),
        operation,
    }
}

/// Runs a Boolean between two snapshots, failing loudly.
#[must_use]
pub fn boolean(target: &Snapshot, tool: &Snapshot, operation: BooleanOperation) -> Snapshot {
    NativeKernel::execute_boolean(
        target,
        tool,
        &boolean_request(target, tool, operation),
        &CancellationToken::new(),
    )
    .expect("the scale fixture Boolean should execute")
    .snapshot
}

/// How much narrower than the first plate the second plate of a two-plate
/// Boolean is: its sides then run a millimetre clear of the first plate's
/// outer holes rather than along its sides.
pub const SECOND_PLATE_INSET: f64 = 4.0;

/// Where the second plate goes: half a thickness up, so the slabs differ
/// and the prism rung stands aside. Its holes sit on the half-pitch grid,
/// `5·√2` mm from the first plate's, so no two carriers coincide or touch.
pub const SECOND_PLATE_OFFSET: Vector3 = Vector3::new(0.0, 0.0, PLATE_THICKNESS / 2.0);

/// The second plate of a two-plate Boolean at size `n`: `(n−1)²` holes on
/// the half-pitch grid, lifted by [`SECOND_PLATE_OFFSET`]. Faces:
/// `6 + 2·(n−1)²`.
#[must_use]
pub fn second_drilled_plate(n: usize) -> Snapshot {
    let side = plate_side(n) - SECOND_PLATE_INSET;
    let centres = (0..n - 1)
        .flat_map(|i| {
            (0..n - 1).map(move |j| {
                let (x, y) = cell_centre(n, i, j);
                (x + PITCH / 2.0, y + PITCH / 2.0)
            })
        })
        .collect::<Vec<_>>();
    let session = run_script(&plate_script(side, &centres));
    transformed(&session.snapshot, SECOND_PLATE_OFFSET, 1.0)
}

/// The exact volume of [`second_drilled_plate`].
#[must_use]
pub fn second_drilled_plate_volume(n: usize) -> f64 {
    let side = plate_side(n) - SECOND_PLATE_INSET;
    let radius = HOLE_DIAMETER / 2.0;
    side * side * PLATE_THICKNESS
        - ((n - 1) * (n - 1)) as f64 * std::f64::consts::PI * radius * radius * PLATE_THICKNESS
}

/// The exact volume of the intersection of a drilled plate with its second
/// plate: the second plate's footprint over the shared half thickness, less
/// every hole of both plates, all of which lie inside that footprint.
#[must_use]
pub fn two_plate_intersection_volume(n: usize) -> f64 {
    let side = plate_side(n) - SECOND_PLATE_INSET;
    let depth = PLATE_THICKNESS - SECOND_PLATE_OFFSET.z;
    let radius = HOLE_DIAMETER / 2.0;
    let holes = n * n + (n - 1) * (n - 1);
    side * side * depth - holes as f64 * std::f64::consts::PI * radius * radius * depth
}

/// The exact volume of the union of a drilled plate with its second plate.
#[must_use]
pub fn two_plate_union_volume(n: usize) -> f64 {
    drilled_plate_volume(n) + second_drilled_plate_volume(n) - two_plate_intersection_volume(n)
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

/// The process's user plus system CPU time in seconds, from the kernel's
/// own accounting where it is published (Linux `/proc`), otherwise the
/// wall clock, so a table always has a number.
#[must_use]
pub fn cpu_seconds() -> f64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let wall = START.get_or_init(Instant::now).elapsed().as_secs_f64();
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
        return wall;
    };
    // Fields after the parenthesised command name; utime is the 14th field
    // of the line and stime the 15th, both in clock ticks.
    let Some(after) = stat.rfind(')') else {
        return wall;
    };
    let fields = stat[after + 1..].split_whitespace().collect::<Vec<_>>();
    let (Some(utime), Some(stime)) = (fields.get(11), fields.get(12)) else {
        return wall;
    };
    let ticks = utime.parse::<f64>().unwrap_or(0.0) + stime.parse::<f64>().unwrap_or(0.0);
    ticks / clock_ticks_per_second()
}

fn clock_ticks_per_second() -> f64 {
    // `sysconf(_SC_CLK_TCK)` is 100 on every Linux the kernel is built for;
    // `/proc` publishes no other value here.
    100.0
}

/// One measurement: how long a stage took, in wall and CPU milliseconds.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
    pub wall_ms: f64,
    pub cpu_ms: f64,
}

/// Times one closure.
pub fn timed<T>(work: impl FnOnce() -> T) -> (T, Timing) {
    let cpu = cpu_seconds();
    let wall = Instant::now();
    let value = work();
    let timing = Timing {
        wall_ms: wall.elapsed().as_secs_f64() * 1000.0,
        cpu_ms: (cpu_seconds() - cpu) * 1000.0,
    };
    (value, timing)
}

/// A row of the timing table a scale test prints.
#[derive(Clone, Debug)]
pub struct TimingRow {
    pub fixture: String,
    pub faces: u64,
    pub stages: Vec<(&'static str, Timing)>,
}

/// Prints rows as one table, so the numbers can be read off a test log and
/// copied into a report.
pub fn print_timing_table(title: &str, rows: &[TimingRow]) {
    println!();
    println!("{title} (wall ms / cpu ms)");
    let stages: Vec<&'static str> = rows
        .iter()
        .flat_map(|row| row.stages.iter().map(|(name, _)| *name))
        .fold(Vec::new(), |mut names, name| {
            if !names.contains(&name) {
                names.push(name);
            }
            names
        });
    let mut header = format!("| {:<28} | {:>6} |", "fixture", "faces");
    for stage in &stages {
        let _ = write!(header, " {stage:>18} |");
    }
    println!("{header}");
    let mut rule = format!("|{}|{}|", "-".repeat(30), "-".repeat(8));
    for _ in &stages {
        let _ = write!(rule, "{}|", "-".repeat(20));
    }
    println!("{rule}");
    for row in rows {
        let mut line = format!("| {:<28} | {:>6} |", row.fixture, row.faces);
        for stage in &stages {
            match row.stages.iter().find(|(name, _)| name == stage) {
                Some((_, timing)) => {
                    let _ = write!(line, " {:>8.1} / {:>7.1} |", timing.wall_ms, timing.cpu_ms);
                }
                None => {
                    let _ = write!(line, " {:>18} |", "-");
                }
            }
        }
        println!("{line}");
    }
    println!();
}

/// Prints the kernel's stage totals since the last call, when the crate was
/// built with `perf-spans` and `ARTIFICER_PERF_REPORT` is set; otherwise
/// nothing, since there is nothing to print.
pub fn print_stage_totals(title: &str) {
    let totals = artificer_kernel::perf::take_stage_totals();
    if totals.is_empty() {
        return;
    }
    println!("  stages of {title}:");
    for (task, total) in totals {
        println!(
            "    {task:<40} {:>5} calls {:>9.1} ms",
            total.calls,
            total.elapsed.as_secs_f64() * 1000.0
        );
    }
}

/// The slowest and fastest step of a session, by the session's own clock:
/// for a body built one feature at a time, how the per-feature cost grew.
#[must_use]
pub fn step_time_spread(session: &Session) -> (u64, u64) {
    let times = session.step_elapsed_ms.values().copied();
    (times.clone().min().unwrap_or(0), times.max().unwrap_or(0))
}
