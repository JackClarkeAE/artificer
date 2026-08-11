//! Display-density regression gate for analytic turned parts.
//!
//! Display tessellation never becomes modeling authority, so its chordal
//! deviation is a presentation budget. This test pins the practical
//! consequence: a palm-sized cylinder must stay in the hundreds of display
//! triangles, not the tens of thousands that the kernel approximation budget
//! (10 nm) would produce if it leaked into presentation sampling.

use artificer_kernel::{CancellationToken, NativeKernel};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, KernelCommand, PlanarCurve2,
    PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3, PrecisionPolicy,
    RequestId, Vector3,
};

#[test]
fn display_density_stays_bounded_for_a_palm_sized_cylinder() {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(0.0, 0.0),
                    radius: 25.0,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("display-density-cylinder"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance: 100.0,
        },
    };
    let outcome =
        NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
            .expect("cylinder should build");
    let scene = NativeKernel::debug_scene(&outcome.snapshot);

    eprintln!(
        "cylinder display density: triangles={} edges={}",
        scene.triangles.len(),
        scene.edges.len(),
    );
    assert!(
        (100..2_000).contains(&scene.triangles.len()),
        "a 50 mm cylinder should render with hundreds of display triangles, got {}",
        scene.triangles.len()
    );
    assert!(
        (50..1_000).contains(&scene.edges.len()),
        "a 50 mm cylinder should render with hundreds of display edge segments, got {}",
        scene.edges.len()
    );

    // The analytic measures remain exact regardless of display density.
    let measures = outcome.snapshot.measures();
    let exact_volume = std::f64::consts::PI * 25.0 * 25.0 * 100.0;
    assert!(((measures.volume - exact_volume) / exact_volume).abs() < 1.0e-9);
}

#[test]
fn a_full_rim_groups_into_one_logical_closed_edge() {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(0.0, 0.0),
                    radius: 25.0,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("rim-carrier-group"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance: 100.0,
        },
    };
    let outcome =
        NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
            .expect("cylinder should build");
    let scene = NativeKernel::debug_scene(&outcome.snapshot);

    // A rim source edge samples into many chords; a seam line samples into one.
    let rim_edge = scene
        .edges
        .iter()
        .find(|edge| {
            scene
                .edges
                .iter()
                .filter(|candidate| candidate.source_edge == edge.source_edge)
                .count()
                > 1
        })
        .expect("the cylinder exposes sampled rim edges")
        .source_edge;

    let group = NativeKernel::carrier_edge_group(&outcome.snapshot, rim_edge)
        .expect("rim edge should resolve");
    assert_eq!(
        group.len(),
        2,
        "a full circle is two exact semicircle edges around one carrier"
    );
    let circumference: f64 = group
        .iter()
        .map(|edge| NativeKernel::edge_length(&outcome.snapshot, *edge).expect("rim member"))
        .sum();
    let exact = 2.0 * std::f64::consts::PI * 25.0;
    assert!(((circumference - exact) / exact).abs() < 1.0e-12);

    // A seam generator stays its own logical edge.
    let seam_edge = scene
        .edges
        .iter()
        .find(|edge| {
            scene
                .edges
                .iter()
                .filter(|candidate| candidate.source_edge == edge.source_edge)
                .count()
                == 1
        })
        .expect("the cylinder exposes straight seam edges")
        .source_edge;
    let seam_group = NativeKernel::carrier_edge_group(&outcome.snapshot, seam_edge)
        .expect("seam edge should resolve");
    assert_eq!(seam_group, vec![seam_edge]);
}

#[test]
fn top_rim_fillet_builds_an_exact_validated_torus_blend() {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(0.0, 0.0),
                    radius: 25.0,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("rim-fillet-base"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance: 100.0,
        },
    };
    let base = NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("cylinder should build");

    // The top rim: a sampled curved scene edge whose chords sit at z = 100.
    let scene = NativeKernel::debug_scene(&base.snapshot);
    let rim_edge = scene
        .edges
        .iter()
        .find(|edge| {
            let curved = scene
                .edges
                .iter()
                .filter(|candidate| candidate.source_edge == edge.source_edge)
                .count()
                > 1;
            curved && (edge.endpoints[0].z - 100.0).abs() < 1.0e-9
        })
        .expect("top rim should expose sampled circle edges")
        .source_edge;

    let fillet = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("rim-fillet-10mm"),
        expected_snapshot: base.snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdge {
            target_edge: rim_edge,
            kind: artificer_protocol::EdgeFinishKind::Fillet,
            distance: 10.0,
        },
    };
    let filleted = NativeKernel::execute(&base.snapshot, &fillet, &CancellationToken::new())
        .expect("a 10 mm rim fillet on a 50 mm cylinder must commit");

    // Closed-form expectation: cylinder to h-f, cap column, plus the
    // Pappus quarter-torus band (ADR 0023).
    let (r, f, h) = (25.0_f64, 10.0_f64, 100.0_f64);
    let pi = std::f64::consts::PI;
    let expected_volume = pi * r * r * (h - f)
        + pi * (r - f) * (r - f) * f
        + pi * pi * f * f * (r - f) / 2.0
        + 2.0 / 3.0 * pi * f * f * f;
    let measures = filleted.snapshot.measures();
    assert!(
        ((measures.volume - expected_volume) / expected_volume).abs() < 1.0e-9,
        "volume {} should equal the exact closed form {}",
        measures.volume,
        expected_volume
    );

    let expected_area = pi * r * r // bottom cap
        + pi * (r - f) * (r - f) // top cap
        + 2.0 * pi * r * (h - f) // wall
        + 2.0 * pi * f * ((r - f) * pi / 2.0 + f); // torus band
    assert!(
        ((measures.surface_area - expected_area) / expected_area).abs() < 1.0e-9,
        "area {} should equal the exact closed form {}",
        measures.surface_area,
        expected_area
    );

    // A single-rim fillet is deliberately asymmetric, so its centre of mass
    // pins the band's axial moment rather than cancelling it. Derived here by
    // slicing: below h - f the section is the full disk; above it the section
    // is the shrunk cap dilated by w = sqrt(f^2 - t^2), sitting at h - f + t.
    let wall_top = h - f;
    let lower_volume = pi * r * r * wall_top;
    let spine = r - f;
    let band_volume = pi
        * (spine * spine).mul_add(
            f,
            (2.0 * spine).mul_add(pi * f * f / 4.0, 2.0 * f.powi(3) / 3.0),
        );
    // The same integrals weighted by t: ∫t·w dt = f³/3 and ∫t·w² dt = f⁴/4.
    let band_moment = wall_top.mul_add(
        band_volume,
        pi * (spine * spine).mul_add(
            f * f / 2.0,
            (2.0 * spine).mul_add(f.powi(3) / 3.0, f.powi(4) / 4.0),
        ),
    );
    let expected_centre =
        lower_volume.mul_add(wall_top / 2.0, band_moment) / (lower_volume + band_volume);
    let centre = measures.centroid.expect("a filleted cylinder has a centre");
    assert!(
        ((centre.z - expected_centre) / h).abs() < 1.0e-9,
        "centre of mass z {} should equal the exact closed form {expected_centre}",
        centre.z
    );
    assert!(centre.x.abs() < 1.0e-9 && centre.y.abs() < 1.0e-9);

    // A blend band only has to keep a positive ring radius over its own
    // quarter turn, so a fillet just under the wall radius is sound even
    // though it leaves a very small cap.
    let near_limit = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("rim-fillet-near-limit"),
        expected_snapshot: base.snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdge {
            target_edge: rim_edge,
            kind: artificer_protocol::EdgeFinishKind::Fillet,
            distance: 24.9999,
        },
    };
    let tight = NativeKernel::execute(&base.snapshot, &near_limit, &CancellationToken::new())
        .expect("a fillet just inside the wall radius is sound");
    assert!(
        artificer_kernel::NativeKernel::validate(
            &tight.snapshot,
            artificer_protocol::ValidationProfile::Solid
        )
        .valid
    );

    // Rejection matrix: a radius strictly beyond the wall still rejects.
    //
    // A radius *equal* to the wall is no longer rejected. It consumes the cap
    // exactly and leaves an arc centred on the axis, which sweeps a
    // hemispherical dome — a constructible solid, gated in
    // `sphere_from_rims_probe.rs`. Only overshoot, where the tangency foot
    // falls off the cap entirely, has no certified answer.
    for bad in [25.000_1, 100.0] {
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("rim-fillet-invalid"),
            expected_snapshot: base.snapshot.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::FinishEdge {
                target_edge: rim_edge,
                kind: artificer_protocol::EdgeFinishKind::Fillet,
                distance: bad,
            },
        };
        NativeKernel::execute(&base.snapshot, &request, &CancellationToken::new())
            .expect_err("an oversized rim fillet must reject");
    }
}

#[test]
fn both_rim_fillets_stay_exact_and_centered() {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(0.0, 0.0),
                    radius: 25.0,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("rim-fillet-both-base"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance: 100.0,
        },
    };
    let base = NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("cylinder should build");
    let scene = NativeKernel::debug_scene(&base.snapshot);
    let rim_at = |height: f64| {
        scene
            .edges
            .iter()
            .find(|edge| {
                scene
                    .edges
                    .iter()
                    .filter(|candidate| candidate.source_edge == edge.source_edge)
                    .count()
                    > 1
                    && (edge.endpoints[0].z - height).abs() < 1.0e-9
            })
            .expect("rim edge")
            .source_edge
    };
    let fillet = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("rim-fillet-both"),
        expected_snapshot: base.snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: vec![rim_at(0.0), rim_at(100.0)],
            kind: artificer_protocol::EdgeFinishKind::Fillet,
            distance: 10.0,
        },
    };
    let filleted = NativeKernel::execute(&base.snapshot, &fillet, &CancellationToken::new())
        .expect("both rim fillets must commit");
    let (r, f, h) = (25.0_f64, 10.0_f64, 100.0_f64);
    let pi = std::f64::consts::PI;
    let band =
        pi * (r - f) * (r - f) * f + pi * pi * f * f * (r - f) / 2.0 + 2.0 / 3.0 * pi * f * f * f;
    let expected_volume = pi * r * r * (h - 2.0 * f) + 2.0 * band;
    let measures = filleted.snapshot.measures();
    assert!(
        ((measures.volume - expected_volume) / expected_volume).abs() < 1.0e-9,
        "volume {} vs closed form {}",
        measures.volume,
        expected_volume
    );
    let centroid = measures.centroid.expect("axisymmetric centroid");
    assert!(
        (centroid.z - h / 2.0).abs() < 1.0e-9,
        "centroid z {}",
        centroid.z
    );
    assert!(centroid.x.abs() < 1.0e-9 && centroid.y.abs() < 1.0e-9);
}

#[test]
fn both_rim_chamfers_cut_exact_cone_bands() {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(0.0, 0.0),
                    radius: 25.0,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("rim-chamfer-base"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance: 100.0,
        },
    };
    let base = NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
        .expect("cylinder should build");
    let scene = NativeKernel::debug_scene(&base.snapshot);
    let rim_at = |height: f64| {
        scene
            .edges
            .iter()
            .find(|edge| {
                scene
                    .edges
                    .iter()
                    .filter(|candidate| candidate.source_edge == edge.source_edge)
                    .count()
                    > 1
                    && (edge.endpoints[0].z - height).abs() < 1.0e-9
            })
            .expect("rim edge")
            .source_edge
    };

    // Top-only chamfer: exact frustum closed forms.
    let (r, d, h) = (25.0_f64, 10.0_f64, 100.0_f64);
    let pi = std::f64::consts::PI;
    let top = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("rim-chamfer-top"),
        expected_snapshot: base.snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdge {
            target_edge: rim_at(100.0),
            kind: artificer_protocol::EdgeFinishKind::Chamfer,
            distance: d,
        },
    };
    let chamfered = NativeKernel::execute(&base.snapshot, &top, &CancellationToken::new())
        .expect("a 10 mm rim chamfer on a 50 mm cylinder must commit");
    let frustum = pi * d / 3.0 * (r * r + r * (r - d) + (r - d) * (r - d));
    let expected_volume = pi * r * r * (h - d) + frustum;
    let measures = chamfered.snapshot.measures();
    assert!(
        ((measures.volume - expected_volume) / expected_volume).abs() < 1.0e-9,
        "volume {} vs closed form {}",
        measures.volume,
        expected_volume
    );
    let expected_area = pi * r * r
        + pi * (r - d) * (r - d)
        + 2.0 * pi * r * (h - d)
        + pi * (2.0 * r - d) * d * std::f64::consts::SQRT_2;
    assert!(
        ((measures.surface_area - expected_area) / expected_area).abs() < 1.0e-9,
        "area {} vs closed form {}",
        measures.surface_area,
        expected_area
    );

    // Both rims: symmetric, centroid back at mid height.
    let both = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("rim-chamfer-both"),
        expected_snapshot: base.snapshot.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::FinishEdges {
            target_edges: vec![rim_at(0.0), rim_at(100.0)],
            kind: artificer_protocol::EdgeFinishKind::Chamfer,
            distance: d,
        },
    };
    let both = NativeKernel::execute(&base.snapshot, &both, &CancellationToken::new())
        .expect("both rim chamfers must commit");
    let expected_volume = pi * r * r * (h - 2.0 * d) + 2.0 * frustum;
    let measures = both.snapshot.measures();
    assert!(
        ((measures.volume - expected_volume) / expected_volume).abs() < 1.0e-9,
        "double-chamfer volume {} vs {}",
        measures.volume,
        expected_volume
    );
    let centroid = measures.centroid.expect("axisymmetric centroid");
    assert!(
        (centroid.z - h / 2.0).abs() < 1.0e-9,
        "centroid z {}",
        centroid.z
    );
}
