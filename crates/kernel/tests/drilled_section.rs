//! A cylinder drilled through is a tube, and reads as one.
//!
//! The drill's cylinder runs its axis down from the face it was drilled
//! from. The section extractor added the wall's parameter span to the
//! carrier's origin without turning it round for an axis running against
//! the section's, so the bore stood above the body, the section would not
//! chain, and the body was not a solid of revolution at all: both rims of
//! a bored cylinder could not be chamfered in one call, and CAM milled it.

use std::collections::BTreeMap;

use artificer_kernel::api::scripting::NoModules;
use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, NativeKernel};

const PI: f64 = std::f64::consts::PI;

const BORED_CYLINDER: &str = "let s = sketch(on: \"XY\", entities: [circle(center: [0, 0], radius: 20)], label: \"s\");\nlet cyl = extrude(sketch: s, distance: 60, label: \"cyl\");\ndrill(face: faces(\">Z\"), center: [0, 0], diameter: 10, depth: 60, label: \"bore\");\n";

fn run(script: &str) -> Session {
    let mut session = Session::new();
    let outcome = session.run_script_with(
        script,
        &BTreeMap::new(),
        &NoModules,
        &CancellationToken::default(),
    );
    assert!(outcome.failure.is_none(), "{:?}", outcome.failure);
    session
}

#[test]
fn a_drilled_cylinder_reads_as_a_tube() {
    let session = run(BORED_CYLINDER);
    let section = NativeKernel::turned_section(&session.snapshot).expect("a tube's section");
    assert!(section.closed, "a bore clear of the axis: {section:?}");
    assert_eq!(section.curves.len(), 4, "{:?}", section.curves);
    // Every corner of the section is one of the tube's four: the bore's
    // wall runs the body's own height, not above it.
    for curve in &section.curves {
        let artificer_protocol::PlanarCurve2::Line { start, end } = curve else {
            panic!("a straight-sided section: {curve:?}");
        };
        for point in [start, end] {
            assert!(
                (point.x - 5.0).abs() < 1.0e-9 || (point.x - 20.0).abs() < 1.0e-9,
                "{point:?}"
            );
            assert!(
                point.y.abs() < 1.0e-9 || (point.y - 60.0).abs() < 1.0e-9,
                "{point:?}"
            );
        }
    }
}

#[test]
fn both_rims_of_a_drilled_cylinder_chamfer_in_one_call() {
    let session = run(&format!(
        "{BORED_CYLINDER}chamfer(edges: [faces(\">Z\").rim(), faces(\"<Z\").rim()], distance: 2, label: \"c\");"
    ));
    let report = session.report();
    let body = report.body.as_ref().expect("a body");
    assert_eq!(format!("{:?}", body.tier), "Exact");
    assert_eq!(
        body.faces.len(),
        10,
        "two caps, two walls, two rims, each in halves"
    );
    // The tube less two 45° chamfer rings on its outer rims.
    let tube = PI * (400.0 - 25.0) * 60.0;
    let ring = PI * (2.0 * 400.0 - (20.0_f64.powi(3) - 18.0_f64.powi(3)) / 3.0);
    let volume = session.snapshot.measures().volume;
    assert!(
        (volume - (tube - 2.0 * ring)).abs() < 1.0e-6,
        "{volume} vs {}",
        tube - 2.0 * ring
    );
}
