//! The general Boolean's frontier (ADR 0056, Track B): cones, spheres and
//! tori through the analytic engine where the matrix answers, and the
//! numerical intersection rung where it does not.
//!
//! Every expectation is derived here, never recorded from the kernel: frustum
//! arithmetic, Pappus, the spherical cap, and where no closed form exists an
//! independent slice quadrature of the two analytic solids written in the
//! test. Every pair also satisfies conservation, `V(A) + V(B) = V(A ∪ B) +
//! V(A ∩ B)`, and validates clean.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, NativeKernel};
use artificer_protocol::{Tier, ValidationProfile};

fn run(source: &str) -> Session {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    let validation = NativeKernel::validate(&session.snapshot, ValidationProfile::Solid);
    assert!(
        validation.valid,
        "the result must validate: {:?}",
        validation.diagnostics
    );
    session
}

fn volume(session: &Session) -> f64 {
    session.snapshot.measures().volume
}

fn rung_of(session: &Session, label: &str) -> String {
    session
        .report()
        .steps
        .iter()
        .find(|step| step.label == label)
        .and_then(|step| step.rung.clone())
        .unwrap_or_default()
}

fn tier_of(session: &Session, label: &str) -> Tier {
    session
        .report()
        .steps
        .iter()
        .find(|step| step.label == label)
        .map(|step| step.tier)
        .expect("the step is recorded")
}

fn assert_close(actual: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: {actual} is not {expected} (off by {}, tolerance {tolerance})",
        actual - expected
    );
}

/// Two bodies `a` and `b` built by `setup`, combined every way. Asserts
/// conservation to `tolerance` and hands back the union, difference and
/// intersection sessions for the caller's own closed forms.
fn conserved(setup: &str, tolerance: f64) -> (Session, Session, Session) {
    let union = run(&format!(
        "{setup}\nunion(target: a, tool: b, label: \"combined\");\n"
    ));
    let difference = run(&format!(
        "{setup}\ndifference(target: a, tool: b, label: \"combined\");\n"
    ));
    let intersection = run(&format!(
        "{setup}\nintersection(target: a, tool: b, label: \"combined\");\n"
    ));
    let a = run(&format!("{setup}\n"));
    let volume_a = a
        .report()
        .steps
        .iter()
        .find(|step| step.label == "a")
        .map(|step| step.volume)
        .expect("a is a step");
    let volume_b = a
        .report()
        .steps
        .iter()
        .find(|step| step.label == "b")
        .map(|step| step.volume)
        .expect("b is a step");
    assert_close(
        volume_a + volume_b,
        volume(&union) + volume(&intersection),
        tolerance,
        "conservation: V(A) + V(B) = V(A ∪ B) + V(A ∩ B)",
    );
    assert_close(
        volume_a,
        volume(&difference) + volume(&intersection),
        tolerance,
        "conservation: V(A) = V(A − B) + V(A ∩ B)",
    );
    (union, difference, intersection)
}

// ---------------------------------------------------------------------------
// B1: the analytic engine widened to cones, spheres and tori
// ---------------------------------------------------------------------------

/// A ball of radius 8 whose centre stands 5 above a plate's top face, so
/// the plate takes a cap of height 3 out of it.
const SPHERE_ON_PLATE: &str = "let a = box(origin: [0, -30, 0], size: [60, 60, 10], label: \"a\");
let ball_section = sketch(on: \"XZ\", label: \"ball_section\", entities: [
    arc(center: [30, 15], radius: 8, start_angle: -90, end_angle: 90),
    line(start: [30, 23], end: [30, 7]),
]);
let b = revolve(sketch: ball_section, axis: [0, 0, 1], axis_origin: [30, 0, 0], label: \"b\");
";

#[test]
fn a_sphere_seated_on_a_plate_joins_exactly() {
    let plate = 60.0 * 60.0 * 10.0;
    let ball = 4.0 / 3.0 * PI * 512.0;
    let cap = PI * 9.0 * (24.0 - 3.0) / 3.0;
    let (union, difference, intersection) = conserved(SPHERE_ON_PLATE, 1.0e-9 * plate);
    assert_close(volume(&union), plate + ball - cap, 1.0e-9 * plate, "union");
    assert_close(
        volume(&difference),
        plate - cap,
        1.0e-9 * plate,
        "difference",
    );
    assert_close(volume(&intersection), cap, 1.0e-9 * plate, "intersection");
    for session in [&union, &difference, &intersection] {
        assert_eq!(rung_of(session, "combined"), "boolean/analytic");
        assert_eq!(tier_of(session, "combined"), Tier::Exact);
    }
    assert!(union.report().body.expect("body").surfaces.spheres >= 1);
}

/// A drafted boss — a frustum from radius 12 to 8 over 12 of height —
/// standing on a plate, then counterbored coaxially to radius 5, 6 deep.
const DRAFTED_BOSS: &str =
    "let plate = box(origin: [-30, -30, 0], size: [60, 60, 10], label: \"plate\");
let boss_section = sketch(on: \"XZ\", label: \"boss_section\", entities: [
    line(start: [0, 10], end: [12, 10]),
    line(start: [12, 10], end: [8, 22]),
    line(start: [8, 22], end: [0, 22]),
    line(start: [0, 22], end: [0, 10]),
]);
revolve(sketch: boss_section, axis: [0, 0, 1], operation: \"add\", label: \"boss\");
let bore_section = sketch(on: \"XZ\", label: \"bore_section\", entities: [
    rect(origin: [0, 16], width: 5, height: 9),
]);
revolve(sketch: bore_section, axis: [0, 0, 1], operation: \"cut\", label: \"counterbore\");
";

#[test]
fn a_coaxial_counterbore_cuts_into_a_drafted_boss_on_a_plate() {
    let session = run(DRAFTED_BOSS);
    let plate = 60.0 * 60.0 * 10.0;
    let frustum = PI * 12.0 / 3.0 * (144.0 + 12.0 * 8.0 + 64.0);
    let counterbore = PI * 25.0 * 6.0;
    assert_close(
        volume(&session),
        plate + frustum - counterbore,
        1.0e-9 * plate,
        "volume",
    );
    assert_eq!(rung_of(&session, "boss"), "revolve/boolean-analytic");
    assert_eq!(rung_of(&session, "counterbore"), "revolve/boolean-analytic");
    assert_eq!(session.report().tier, Tier::Exact);
    assert!(session.report().body.expect("body").surfaces.cones >= 1);
}

/// A torus band on a cylinder's top rim, then a bore drilled through the
/// body well inside the band: the bore never meets the band, and the
/// separation test proves it, so the answer stays exact.
const BORE_CLEARING_A_BLEND: &str = "let post = cylinder(radius: 20, height: 30, label: \"post\");
fillet(edges: [nearest(point: [20, 0, 30], kind: \"edge\"), nearest(point: [-20, 0, 30], kind: \"edge\")], radius: 3, label: \"rim\");
drill(face: nearest(point: [0, 0, 30]), center: [8, 0], diameter: 6, depth: 30, label: \"bore\");
";

#[test]
fn a_bore_that_clears_a_torus_blend_stays_exact() {
    let session = run(BORE_CLEARING_A_BLEND);
    // The rim fillet takes the corner outside its quarter circle: a section
    // of area r²(1 − π/4) whose centroid sits r/6/(1 − π/4) in from the
    // corner's inner edge, turned about the axis (Pappus).
    let (radius, r) = (20.0_f64, 3.0_f64);
    let fillet = 2.0 * PI * r * r * ((radius - r) * (1.0 - PI / 4.0) + r / 6.0);
    let expected = PI * radius * radius * 30.0 - fillet - PI * 9.0 * 30.0;
    assert_close(volume(&session), expected, 1.0e-9 * expected, "volume");
    assert_eq!(rung_of(&session, "rim"), "edge-finish/rim-blend");
    assert_eq!(rung_of(&session, "bore"), "face-feature/analytic-boolean");
    assert_eq!(session.report().tier, Tier::Exact);
    assert!(session.report().body.expect("body").surfaces.tori >= 1);
}

// ---------------------------------------------------------------------------
// B2: the numerical intersection rung
// ---------------------------------------------------------------------------

/// Composite ten-point Gauss–Legendre over `[a, b]` in `panels` panels,
/// walked through `x = a + (b − a)(3t² − 2t³)` so an integrand with a
/// square-root end still converges exponentially.
fn integrate(a: f64, b: f64, panels: usize, f: &dyn Fn(f64) -> f64) -> f64 {
    const NODES: [(f64, f64); 10] = [
        (-0.973_906_528_517_171_7, 0.066_671_344_308_688_1),
        (-0.865_063_366_688_984_5, 0.149_451_349_150_580_6),
        (-0.679_409_568_299_024_4, 0.219_086_362_515_982),
        (-0.433_395_394_129_247_2, 0.269_266_719_309_996_3),
        (-0.148_874_338_981_631_2, 0.295_524_224_714_752_9),
        (0.148_874_338_981_631_2, 0.295_524_224_714_752_9),
        (0.433_395_394_129_247_2, 0.269_266_719_309_996_3),
        (0.679_409_568_299_024_4, 0.219_086_362_515_982),
        (0.865_063_366_688_984_5, 0.149_451_349_150_580_6),
        (0.973_906_528_517_171_7, 0.066_671_344_308_688_1),
    ];
    let span = b - a;
    let step = 1.0 / panels as f64;
    let mut total = 0.0;
    for panel in 0..panels {
        let low = step * panel as f64;
        let (half, middle) = (0.5 * step, low + 0.5 * step);
        for (node, weight) in NODES {
            let t = middle + half * node;
            let x = a + span * t * t * (3.0 - 2.0 * t);
            let rate = 6.0 * span * t * (1.0 - t);
            total += weight * rate * f(x);
        }
    }
    total * 0.5 * step
}

/// The area two discs share.
fn lens(first: (f64, f64), first_radius: f64, second: (f64, f64), second_radius: f64) -> f64 {
    let d = (first.0 - second.0).hypot(first.1 - second.1);
    let (r1, r2) = (first_radius, second_radius);
    if d >= r1 + r2 {
        return 0.0;
    }
    if d <= (r1 - r2).abs() {
        return PI * r1.min(r2).powi(2);
    }
    let a1 = ((d * d + r1 * r1 - r2 * r2) / (2.0 * d * r1))
        .clamp(-1.0, 1.0)
        .acos();
    let a2 = ((d * d + r2 * r2 - r1 * r1) / (2.0 * d * r2))
        .clamp(-1.0, 1.0)
        .acos();
    let triangle = 0.5
        * ((-d + r1 + r2) * (d + r1 - r2) * (d - r1 + r2) * (d + r1 + r2))
            .max(0.0)
            .sqrt();
    r1 * r1 * a1 + r2 * r2 * a2 - triangle
}

/// The blend-then-drill example: a flanged hub whose flange rim is rounded
/// by a torus band, then a bolt hole drilled down through that band. The
/// hole's cylinder meets the torus off its axis, which the numerical rung
/// traces; every other pair is in the matrix.
#[test]
fn a_hole_through_a_torus_band_is_traced_numerically() {
    let session = run(include_str!("../examples/blend_then_drill.art"));
    // The hub: bore 6, hub 20 by 40 tall, flange 45 by 8 thick; the rim
    // fillet of 2 at radius 45; the hole of radius 3 centred at radius 43.
    let hub = PI * (400.0 - 36.0) * 40.0 + PI * (2025.0 - 400.0) * 8.0;
    let (flange, r) = (45.0_f64, 2.0_f64);
    let fillet = 2.0 * PI * r * r * ((flange - r) * (1.0 - PI / 4.0) + r / 6.0);
    // The hole removes the flange's height under its disc: 8 inside radius
    // 43, the fillet's quarter circle over the band, nothing past the rim.
    // Integrated in polar coordinates about the hub's axis, the hole's disc
    // spans the azimuths |θ| ≤ α(ρ) at each radius.
    let (centre, hole) = (43.0_f64, 3.0_f64);
    let width = |rho: f64| {
        let cosine = (rho * rho + centre * centre - hole * hole) / (2.0 * centre * rho);
        2.0 * cosine.clamp(-1.0, 1.0).acos()
    };
    let inner = integrate(centre - hole, centre, 400, &|rho| 8.0 * width(rho) * rho);
    // Over the band, ρ = 43 + 2 sin s makes the quarter circle's height
    // 6 + 2 cos s and its rate 2 cos s ds: smooth to the rim.
    let band = integrate(0.0, PI / 2.0, 400, &|s| {
        let rho = centre + r * s.sin();
        (6.0 + r * s.cos()) * width(rho) * rho * r * s.cos()
    });
    let expected = hub - fillet - inner - band;
    assert_close(volume(&session), expected, 1.0e-7 * expected, "volume");
    assert_eq!(
        rung_of(&session, "rim_hole"),
        "face-feature/numerical-boolean"
    );
    assert_eq!(tier_of(&session, "rim_hole"), Tier::Approximate);
    let step = session
        .report()
        .steps
        .iter()
        .find(|step| step.label == "rim_hole")
        .expect("the hole is a step")
        .clone();
    let warning = step
        .warnings
        .iter()
        .find(|warning| warning.code == "BOOLEAN_INTERSECTION_APPROXIMATED")
        .expect("the approximation is labelled");
    assert!(
        warning.message.contains("traced numerically"),
        "{}",
        warning.message
    );
    assert!(session.report().body.expect("body").surfaces.tori >= 1);
    // The band is drawn from its loops: no facet of the torus (the tube of
    // radius 2 about the circle of radius 43 at height 6) reaches into the
    // hole of radius 3 about (43, 0) beyond the sagitta of a display chord
    // along its rim.
    let scene = NativeKernel::debug_scene(&session.snapshot);
    let mut on_band = 0;
    for triangle in &scene.triangles {
        for vertex in &triangle.vertices {
            let ring = vertex.x.hypot(vertex.y);
            if ((ring - 43.0).hypot(vertex.z - 6.0) - 2.0).abs() < 1.0e-6 {
                on_band += 1;
                assert!(
                    (vertex.x - 43.0).hypot(vertex.y) >= 3.0 - 0.05,
                    "a facet of the band reaches into the hole at ({}, {}, {})",
                    vertex.x,
                    vertex.y,
                    vertex.z
                );
            }
        }
    }
    assert!(on_band > 100, "the band is drawn: {on_band} vertices on it");
}

/// A ball of radius 10 with a bore of radius 3 whose axis passes 4 from the
/// centre. The bore's cylinder meets the sphere off its axis: two closed
/// loops, traced numerically.
const SPHERE_OFF_CENTRE_BORE: &str =
    "let ball_section = sketch(on: \"XZ\", label: \"ball_section\", entities: [
    arc(center: [0, 0], radius: 10, start_angle: -90, end_angle: 90),
    line(start: [0, 10], end: [0, -10]),
]);
let a = revolve(sketch: ball_section, axis: [0, 0, 1], label: \"a\");
let b = cylinder(center: [4, 0, -15], axis: [0, 0, 1], radius: 3, height: 30, label: \"b\");
";

#[test]
fn a_sphere_cut_by_an_off_centre_bore_is_traced_numerically() {
    // The bore removes ∬ 2·√(R² − x² − y²) over its disc, which lies well
    // inside the ball's shadow: a smooth integrand in polar coordinates
    // about the bore's own axis.
    let removed = integrate(0.0, 3.0, 200, &|s| {
        integrate(0.0, 2.0 * PI, 200, &|phi| {
            let (x, y) = (4.0 + s * phi.cos(), s * phi.sin());
            2.0 * (100.0 - x * x - y * y).sqrt() * s
        })
    });
    let ball = 4.0 / 3.0 * PI * 1000.0;
    let bore = PI * 9.0 * 30.0;
    let (union, difference, intersection) = conserved(SPHERE_OFF_CENTRE_BORE, 1.0e-7 * ball);
    assert_close(
        volume(&difference),
        ball - removed,
        1.0e-7 * ball,
        "difference",
    );
    assert_close(
        volume(&intersection),
        removed,
        1.0e-7 * ball,
        "intersection",
    );
    assert_close(
        volume(&union),
        ball + bore - removed,
        1.0e-7 * ball,
        "union",
    );
    for session in [&union, &difference, &intersection] {
        assert_eq!(rung_of(session, "combined"), "boolean/numerical");
        assert_eq!(tier_of(session, "combined"), Tier::Approximate);
    }
    // The display draws the ball's face from its loops rather than as the
    // whole parameter rectangle: no facet of it reaches into the bore
    // beyond the sagitta of a display chord along the rim, the reach every
    // polygon inscribed in a hole has. Every vertex on the ball (|p| = 10)
    // is within that of the bore's radius from the bore's axis; the grid
    // drawing put vertices on the axis itself.
    let scene = NativeKernel::debug_scene(&difference.snapshot);
    let mut on_ball = 0;
    for triangle in &scene.triangles {
        for vertex in &triangle.vertices {
            let radius = (vertex.x * vertex.x + vertex.y * vertex.y + vertex.z * vertex.z).sqrt();
            if (radius - 10.0).abs() < 1.0e-6 {
                on_ball += 1;
                let reach = (vertex.x - 4.0).hypot(vertex.y);
                assert!(
                    reach >= 3.0 - 0.05,
                    "a facet of the ball reaches into the bore at ({}, {}, {})",
                    vertex.x,
                    vertex.y,
                    vertex.z
                );
            }
        }
    }
    assert!(on_ball > 100, "the ball is drawn: {on_ball} vertices on it");
}

/// Two tori of major radius 10 on parallel axes 10 apart, so their centre
/// circles cross at 60°, with tubes of radius 3 and 2. Equal tubes would
/// touch at the crossings the way two equal pipes with meeting axes do —
/// a tangency the tracer refuses — so the tubes differ.
const TWO_TORI: &str = "let ring_a = sketch(on: \"XZ\", label: \"ring_a\", entities: [
    circle(center: [10, 0], radius: 3),
]);
let a = revolve(sketch: ring_a, axis: [0, 0, 1], label: \"a\");
let ring_b = sketch(on: \"XZ\", label: \"ring_b\", entities: [
    circle(center: [20, 0], radius: 2),
]);
let b = revolve(sketch: ring_b, axis: [0, 0, 1], axis_origin: [10, 0, 0], label: \"b\");
";

#[test]
fn two_tori_crossing_are_traced_numerically() {
    // At height z each torus is an annulus about its own axis, between the
    // radii 10 ∓ √(9 − z²) and 10 ∓ √(4 − z²); the two annuli share the
    // lens areas of their outer and inner discs, and z = 2 sin s keeps the
    // integrand smooth where the thinner tube ends.
    let shared = integrate(-PI / 2.0, PI / 2.0, 400, &|s| {
        let z = 2.0 * s.sin();
        let (wide, narrow) = ((9.0 - z * z).sqrt(), 2.0 * s.cos());
        let (outer_a, inner_a) = (10.0 + wide, 10.0 - wide);
        let (outer_b, inner_b) = (10.0 + narrow, 10.0 - narrow);
        let a = (0.0, 0.0);
        let b = (10.0, 0.0);
        let area = lens(a, outer_a, b, outer_b)
            - lens(a, outer_a, b, inner_b)
            - lens(a, inner_a, b, outer_b)
            + lens(a, inner_a, b, inner_b);
        area * 2.0 * s.cos()
    });
    let torus_a = 2.0 * PI * 10.0 * PI * 9.0;
    let torus_b = 2.0 * PI * 10.0 * PI * 4.0;
    let (union, difference, intersection) = conserved(TWO_TORI, 1.0e-7 * torus_a);
    assert_close(
        volume(&intersection),
        shared,
        1.0e-7 * torus_a,
        "intersection",
    );
    assert_close(
        volume(&difference),
        torus_a - shared,
        1.0e-7 * torus_a,
        "difference",
    );
    assert_close(
        volume(&union),
        torus_a + torus_b - shared,
        1.0e-7 * torus_a,
        "union",
    );
    for session in [&union, &difference, &intersection] {
        assert_eq!(rung_of(session, "combined"), "boolean/numerical");
        assert_eq!(tier_of(session, "combined"), Tier::Approximate);
    }
}

/// A cone frustum, radius 10 at its base and 4 at its top over a height of
/// 20, cut by a slab whose near face is the plane `x + z = 12`: the plane
/// meets the cone in an ellipse the matrix does not name, clear of both
/// rims.
const CONE_AND_OBLIQUE_PLANE: &str = "let cone_section = sketch(on: \"XZ\", label: \"cone_section\", entities: [
    line(start: [0, 0], end: [10, 0]),
    line(start: [10, 0], end: [4, 20]),
    line(start: [4, 20], end: [0, 20]),
    line(start: [0, 20], end: [0, 0]),
]);
let a = revolve(sketch: cone_section, axis: [0, 0, 1], label: \"a\");
let slab_sketch = sketch(on: plane(origin: [0, 0, 12], normal: [1, 0, 1], x_axis: [0, 1, 0]), label: \"slab_sketch\", entities: [
    rect(center: [0, 0], width: 80, height: 80),
]);
let b = extrude(sketch: slab_sketch, distance: 40, label: \"b\");
";

#[test]
fn a_cone_cut_by_an_oblique_plane_is_traced_numerically() {
    // Beyond the plane the cone's disc at height z, of radius 10 − 0.3z,
    // keeps the segment past the chord x = 12 − z: nothing below z = 2/0.7,
    // where the chord first enters the disc, the whole disc above z = 22/1.3,
    // where it leaves, and in between a segment whose area grows as the
    // three-halves power from either end, which z = m + h·sin s smooths.
    let rho = |z: f64| 10.0 - 0.3 * z;
    let segment = |z: f64| {
        let (rho, d) = (rho(z), 12.0 - z);
        rho * rho * (d / rho).clamp(-1.0, 1.0).acos() - d * (rho * rho - d * d).max(0.0).sqrt()
    };
    let (enters, leaves): (f64, f64) = (2.0 / 0.7, 22.0 / 1.3);
    let (middle, half) = (0.5 * (enters + leaves), 0.5 * (leaves - enters));
    let removed = integrate(-PI / 2.0, PI / 2.0, 400, &|s| {
        segment(half.mul_add(s.sin(), middle)) * half * s.cos()
    }) + integrate(leaves, 20.0, 100, &|z| PI * rho(z) * rho(z));
    let frustum = PI * 20.0 / 3.0 * (100.0 + 40.0 + 16.0);
    let (union, difference, intersection) = conserved(CONE_AND_OBLIQUE_PLANE, 1.0e-7 * frustum);
    assert_close(
        volume(&difference),
        frustum - removed,
        1.0e-7 * frustum,
        "difference",
    );
    assert_close(
        volume(&intersection),
        removed,
        1.0e-7 * frustum,
        "intersection",
    );
    let slab = 80.0 * 80.0 * 40.0;
    assert_close(
        volume(&union),
        frustum + slab - removed,
        1.0e-7 * slab,
        "union",
    );
    for session in [&union, &difference, &intersection] {
        assert_eq!(rung_of(session, "combined"), "boolean/numerical");
        assert_eq!(tier_of(session, "combined"), Tier::Approximate);
    }
}
