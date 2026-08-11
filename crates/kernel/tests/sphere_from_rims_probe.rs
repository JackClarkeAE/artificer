//! Revolved poles: section arcs that terminate on the axis.
//!
//! Filleting a cylinder rim at the cylinder's own radius consumes the cap
//! exactly, and the arc left behind is centred on the axis, so the swept
//! carrier is a sphere rather than a torus of zero major radius. Expectations
//! are the classical closed forms, written independently of the kernel's own
//! per-face integrals so the two derivations cannot share a mistake.

use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, EdgeFinishKind, EntityRef, ExecuteRequest,
    KernelCommand, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2,
    Point3, PrecisionPolicy, RequestId, ValidationProfile, Vector3,
};

const RADIUS: f64 = 5.0;
const HEIGHT: f64 = 10.0;
const PI: f64 = std::f64::consts::PI;

fn cylinder() -> Snapshot {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(0.0, 0.0),
                    radius: RADIUS,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("pole-base"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance: HEIGHT,
        },
    };
    NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("cylinder should build")
        .snapshot
}

fn rim_at(snapshot: &Snapshot, height: f64) -> Vec<EntityRef> {
    let scene = NativeKernel::debug_scene(snapshot);
    let mut found = Vec::new();
    for edge in &scene.edges {
        let [first, second] = edge.endpoints;
        if (first.z - height).abs() < 1.0e-9
            && (second.z - height).abs() < 1.0e-9
            && (first.x.hypot(first.y) - RADIUS).abs() < 1.0e-6
            && !found.contains(&edge.source_edge)
        {
            found.push(edge.source_edge);
        }
    }
    found
}

fn try_fillet(
    base: &Snapshot,
    targets: Vec<EntityRef>,
    distance: f64,
    tag: &str,
) -> Result<Snapshot, String> {
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new(tag),
        expected_snapshot: base.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: targets,
            kind: EdgeFinishKind::Fillet,
            distance,
        },
    };
    NativeKernel::execute(base, &request, &CancellationToken::new())
        .map(|outcome| outcome.snapshot)
        .map_err(|error| format!("{error:?}"))
}

fn fillet(base: &Snapshot, targets: Vec<EntityRef>, distance: f64, tag: &str) -> Snapshot {
    try_fillet(base, targets, distance, tag)
        .unwrap_or_else(|error| panic!("{tag} should build: {error}"))
}

fn assert_close(actual: f64, expected: f64, label: &str) {
    assert!(
        (actual - expected).abs() <= 1.0e-9 * expected.abs().max(1.0),
        "{label}: {actual} vs {expected}"
    );
}

#[test]
fn filleting_both_rims_at_the_cylinder_radius_yields_a_sphere() {
    let base = cylinder();
    let mut targets = rim_at(&base, 0.0);
    targets.extend(rim_at(&base, HEIGHT));
    assert_eq!(targets.len(), 4, "both rims are two semicircles each");

    let sphere = fillet(&base, targets, RADIUS, "sphere");
    let report = NativeKernel::validate(&sphere, ValidationProfile::Solid);
    assert!(report.valid, "sphere should validate: {:?}", report);
    assert_close(
        sphere.measures().volume,
        4.0 / 3.0 * PI * RADIUS.powi(3),
        "sphere volume",
    );
    assert_close(
        sphere.measures().surface_area,
        4.0 * PI * RADIUS * RADIUS,
        "sphere area",
    );
}

#[test]
fn filleting_one_rim_at_the_cylinder_radius_yields_a_domed_cylinder() {
    let base = cylinder();
    let domed = fillet(&base, rim_at(&base, HEIGHT), RADIUS, "dome");
    let report = NativeKernel::validate(&domed, ValidationProfile::Solid);
    assert!(report.valid, "dome should validate: {:?}", report);

    // A hemispherical cap replaces the top: the wall keeps h - r of its
    // length, and the dome contributes half a sphere.
    let barrel = PI * RADIUS * RADIUS * (HEIGHT - RADIUS);
    let dome = 2.0 / 3.0 * PI * RADIUS.powi(3);
    assert_close(domed.measures().volume, barrel + dome, "domed volume");

    let base_disk = PI * RADIUS * RADIUS;
    let wall = 2.0 * PI * RADIUS * (HEIGHT - RADIUS);
    let cap = 2.0 * PI * RADIUS * RADIUS;
    assert_close(
        domed.measures().surface_area,
        base_disk + wall + cap,
        "domed area",
    );
}

#[test]
fn a_radius_larger_than_the_cylinder_still_refuses() {
    let base = cylinder();
    // Overshoot has no certified answer: the tangency foot falls off the cap.
    assert!(
        try_fillet(&base, rim_at(&base, HEIGHT), RADIUS * 1.5, "overshoot").is_err(),
        "a fillet wider than the cap must reject"
    );
}

#[test]
fn the_sphere_carries_real_pole_closure_and_no_degenerate_torus() {
    let base = cylinder();
    let mut targets = rim_at(&base, 0.0);
    targets.extend(rim_at(&base, HEIGHT));
    let sphere = fillet(&base, targets, RADIUS, "sphere-shape");

    // Centroid sits at the sphere's own centre, half way up the original
    // cylinder — an independent check that the two hemispheres are balanced.
    let centroid = sphere.measures().centroid.expect("a solid has a centroid");
    for (value, expected) in [
        (centroid.x, 0.0),
        (centroid.y, 0.0),
        (centroid.z, HEIGHT / 2.0),
    ] {
        assert!(
            (value - expected).abs() <= 1.0e-9 * HEIGHT,
            "centroid {value} vs {expected}"
        );
    }

    // Four faces: two hemispheres, each split into half-patches at the seam.
    let counts = sphere.counts();
    assert_eq!(counts.faces, 4, "two hemispheres as half-patches");
    assert_eq!(counts.solids, 1);

    // Every sampled surface point must sit on the sphere of radius RADIUS
    // about its centre. This is the check that a zero-major-radius torus
    // would fail while still closing topologically.
    let scene = NativeKernel::debug_scene(&sphere);
    assert!(!scene.triangles.is_empty());
    for triangle in &scene.triangles {
        for point in triangle.vertices {
            let radial = (point.x * point.x + point.y * point.y).sqrt();
            let axial = point.z - HEIGHT / 2.0;
            let distance = radial.hypot(axial);
            assert!(
                (distance - RADIUS).abs() <= 1.0e-6 * RADIUS,
                "surface point {point:?} lies {distance} from the centre, not {RADIUS}"
            );
        }
    }
}
