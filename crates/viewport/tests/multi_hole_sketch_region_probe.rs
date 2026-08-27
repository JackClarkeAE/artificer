use artificer_protocol::Point3;
use artificer_viewport::{ModelSketchOverlay, ModelSketchRegion};

#[test]
fn test_triangulate_multi_hole_region() {
    // Outer rectangle: 0..100 in X, 0..100 in Y at Z=0 (Counter-Clockwise)
    let outer = vec![
        Point3::new(0.0, 0.0, 0.0),
        Point3::new(100.0, 0.0, 0.0),
        Point3::new(100.0, 100.0, 0.0),
        Point3::new(0.0, 100.0, 0.0),
    ];

    // Hole 1: Circle at (25, 50), radius 10 (Clockwise for a hole)
    let hole_circle: Vec<Point3> = (0..32)
        .map(|i| {
            let angle = -std::f64::consts::TAU * (i as f64) / 32.0;
            Point3::new(25.0 + 10.0 * angle.cos(), 50.0 + 10.0 * angle.sin(), 0.0)
        })
        .collect();

    // Hole 2: Slot at (75, 50), length 20, width 10 (Clockwise for a hole)
    let mut hole_slot = Vec::new();
    for i in 0..16 {
        let angle = -std::f64::consts::PI * (i as f64) / 16.0 + std::f64::consts::FRAC_PI_2;
        hole_slot.push(Point3::new(
            80.0 + 5.0 * angle.cos(),
            50.0 + 5.0 * angle.sin(),
            0.0,
        ));
    }
    for i in 0..16 {
        let angle = -std::f64::consts::PI * (i as f64) / 16.0 - std::f64::consts::FRAC_PI_2;
        hole_slot.push(Point3::new(
            70.0 + 5.0 * angle.cos(),
            50.0 + 5.0 * angle.sin(),
            0.0,
        ));
    }

    let region = ModelSketchRegion::new(
        outer.clone(),
        vec![hole_circle.clone(), hole_slot.clone()],
        [50.0, 10.0],
    );

    assert_eq!(region.anchor(), [50.0, 10.0]);

    let overlay =
        ModelSketchOverlay::new(Vec::new(), Vec::new(), false).selectable(0, vec![region]);

    assert_eq!(overlay.region_count(), 1);
}
