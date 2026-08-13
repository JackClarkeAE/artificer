//! Exactness gates for the per-vertex display normals (ADR 0026, P1).
//!
//! Smooth shading is only worth having if the normals are the carrier's own,
//! evaluated in closed form. Two properties make that checkable without a
//! renderer: every normal must equal the analytic normal derived independently
//! here, and a vertex shared by two triangles of one carrier must carry
//! bit-identical normals — the tripwire against anyone "fixing" shading by
//! averaging mesh geometry, which would quietly reintroduce facet artefacts and
//! make the display scene depend on tessellation density.

use std::collections::HashMap;

use artificer_kernel::{CancellationToken, DebugScene, NativeKernel};
use artificer_protocol::{
    ArcDirection, CURRENT_PROTOCOL_VERSION, ExecuteRequest, KernelCommand, PlanarCurve2,
    PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2, Point2, Point3, PrecisionPolicy,
    RequestId, Vector3,
};

fn cylinder_scene(radius: f64, height: f64) -> DebugScene {
    let profile = PlanarProfile2 {
        regions: vec![PlanarRegion2 {
            outer: PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: Point2::new(0.0, 0.0),
                    radius,
                    direction: ArcDirection::CounterClockwise,
                }],
            },
            holes: vec![],
        }],
    };
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("display-normals-cylinder"),
        expected_snapshot: NativeKernel::empty().id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::ExtrudePlanarProfile {
            frame: PlanarFrame3::new(
                Point3::new(0.0, 0.0, 0.0),
                Vector3::new(1.0, 0.0, 0.0),
                Vector3::new(0.0, 1.0, 0.0),
            ),
            profile,
            distance: height,
        },
    };
    let outcome =
        NativeKernel::execute(&NativeKernel::empty(), &request, &CancellationToken::new())
            .expect("cylinder should build");
    NativeKernel::debug_scene(&outcome.snapshot)
}

#[test]
fn every_display_normal_is_a_unit_vector() {
    let scene = cylinder_scene(25.0, 100.0);
    for triangle in &scene.triangles {
        for normal in triangle.normals {
            let length = normal
                .x
                .mul_add(normal.x, normal.y.mul_add(normal.y, normal.z * normal.z))
                .sqrt();
            assert!(
                (length - 1.0).abs() <= 1.0e-12,
                "normal {normal:?} is not unit: |n| = {length}"
            );
        }
    }
}

#[test]
fn curved_normals_are_the_exact_carrier_normals() {
    let radius = 25.0;
    let scene = cylinder_scene(radius, 100.0);
    let mut curved_vertices = 0_usize;
    for triangle in &scene.triangles {
        for (vertex, normal) in triangle.vertices.into_iter().zip(triangle.normals) {
            if normal.z.abs() > 1.0e-12 {
                // A cap: the plane normal is the exact extrusion direction.
                assert_eq!(normal.x, 0.0);
                assert_eq!(normal.y, 0.0);
                assert!(normal.z == 1.0 || normal.z == -1.0, "cap normal {normal:?}");
                continue;
            }
            // The wall: the independently derived analytic normal is the
            // radial direction at that vertex, exactly perpendicular to the
            // axis and pointing away from it.
            curved_vertices += 1;
            let distance = vertex.x.hypot(vertex.y);
            assert!(
                (distance - radius).abs() <= 1.0e-9,
                "wall vertex {vertex:?} is off the carrier: r = {distance}"
            );
            assert!(
                (normal.x - vertex.x / distance).abs() <= 1.0e-12
                    && (normal.y - vertex.y / distance).abs() <= 1.0e-12,
                "wall normal {normal:?} is not radial at {vertex:?}"
            );
            assert_eq!(
                normal.z, 0.0,
                "wall normal must be perpendicular to the axis"
            );
        }
    }
    assert!(
        curved_vertices > 100,
        "expected a tessellated wall, got {curved_vertices} curved vertices"
    );
}

#[test]
fn a_shared_vertex_carries_bit_identical_normals() {
    let scene = cylinder_scene(25.0, 100.0);
    let mut seen: HashMap<(u64, [u64; 3]), [u64; 3]> = HashMap::new();
    for triangle in &scene.triangles {
        for (vertex, normal) in triangle.vertices.into_iter().zip(triangle.normals) {
            let key = (
                triangle.source_face.entity.0,
                [vertex.x.to_bits(), vertex.y.to_bits(), vertex.z.to_bits()],
            );
            let bits = [normal.x.to_bits(), normal.y.to_bits(), normal.z.to_bits()];
            if let Some(previous) = seen.insert(key, bits) {
                assert_eq!(
                    previous, bits,
                    "vertex {vertex:?} on face {} shades with two different normals; \
                     display normals must be evaluated from the carrier, never averaged \
                     from adjacent facets",
                    triangle.source_face.entity.0
                );
            }
        }
    }
    assert!(
        seen.len() > 100,
        "expected many distinct shaded vertices, got {}",
        seen.len()
    );
}

#[test]
fn a_curved_facet_varies_across_its_own_vertices() {
    // The point of the milestone: a wall triangle no longer has one normal.
    let scene = cylinder_scene(25.0, 100.0);
    let varying = scene
        .triangles
        .iter()
        .filter(|triangle| {
            let [first, second, third] = triangle.normals;
            first != second || second != third
        })
        .count();
    assert!(
        varying > 50,
        "smooth shading needs per-vertex normals on the wall, found {varying} varying facets"
    );
}
