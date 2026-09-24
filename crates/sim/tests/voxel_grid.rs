//! The grid a study stands on: a body voxelised through the kernel, counted
//! against its exact volume, and its surface cells named by face.

use std::collections::BTreeMap;

use artificer_kernel::api::session::Session;
use artificer_kernel::{CancellationToken, NativeKernel, Snapshot};
use artificer_protocol::{EntityRef, Point3};

fn build(source: &str) -> Snapshot {
    let mut session = Session::new();
    let outcome = session.run_script(source, &BTreeMap::new(), &CancellationToken::default());
    assert!(outcome.succeeded(), "{:?}", outcome.failure);
    session.snapshot.clone()
}

fn cuboid(origin: [f64; 3], size: [f64; 3]) -> Snapshot {
    build(&format!(
        "let b = box(origin: [{}, {}, {}], size: [{}, {}, {}], label: \"b\");\n",
        origin[0], origin[1], origin[2], size[0], size[1], size[2]
    ))
}

/// The plate the stress-concentration gate uses: a bar with a round hole
/// through its thickness.
fn plate_with_hole() -> Snapshot {
    build(
        "let plate = box(origin: [0, 0, 0], size: [40, 20, 2], label: \"plate\");\n\
         let bore = cylinder(center: [20, 10, -1], axis: [0, 0, 1], radius: 4, height: 4, label: \"bore\");\n\
         let holed = difference(target: plate, tool: bore, label: \"holed\");\n",
    )
}

#[test]
fn a_box_voxel_count_matches_its_volume_to_within_the_boundary_layer() {
    let snapshot = cuboid([0.0, 0.0, 0.0], [10.0, 10.0, 100.0]);
    let volume = snapshot.measures().volume;
    let area = snapshot.measures().surface_area;
    for cell in [2.5, 1.25, 0.8] {
        let grid = NativeKernel::voxelise(&snapshot, cell);
        assert!(!grid.is_empty(), "cell {cell}");
        let counted = grid.solid_volume();
        let boundary_layer = area * cell;
        assert!(
            (counted - volume).abs() <= boundary_layer,
            "cell {cell}: {counted} mm³ counted against {volume} mm³ exact, layer {boundary_layer}"
        );
    }
    // A box that is a whole number of cells is exactly its volume.
    let exact = NativeKernel::voxelise(&snapshot, 2.5);
    assert_eq!(exact.dims(), [4, 4, 40]);
    assert_eq!(exact.solid_count(), 4 * 4 * 40);
    assert_eq!(exact.solid_volume(), volume);
}

#[test]
fn every_face_of_a_box_owns_the_surface_cells_on_it() {
    let snapshot = cuboid([0.0, 0.0, 0.0], [10.0, 10.0, 100.0]);
    let grid = NativeKernel::voxelise(&snapshot, 2.5);
    let faces = NativeKernel::describe_faces(&snapshot);
    assert_eq!(grid.faces().len(), 6, "{:?}", grid.faces());
    for face in grid.faces() {
        let description = &faces[&face.entity.0];
        let cells = grid.cells_on_face(face).count();
        // A face of area A has A / cell² surface cells lying on it, each
        // facing out the way the face does.
        let expected = (description.area / (2.5 * 2.5)).round() as usize;
        assert_eq!(cells, expected, "{}", description.summary);
        for cell in grid.cells_on_face(face) {
            let outward = [
                f64::from(cell.outward[0]),
                f64::from(cell.outward[1]),
                f64::from(cell.outward[2]),
            ];
            let dot = outward[0] * description.normal.x
                + outward[1] * description.normal.y
                + outward[2] * description.normal.z;
            assert!(dot > 0.99, "{} faces {outward:?}", description.summary);
            assert!(
                !grid.neighbour_is_solid(cell.cell, cell.outward),
                "a surface cell faces a void"
            );
        }
    }
}

#[test]
fn a_hole_is_carved_out_and_its_wall_is_named() {
    let snapshot = plate_with_hole();
    let grid = NativeKernel::voxelise(&snapshot, 1.0);
    let volume = snapshot.measures().volume;
    let area = snapshot.measures().surface_area;
    assert!(
        (grid.solid_volume() - volume).abs() <= area * 1.0,
        "{} against {volume}",
        grid.solid_volume()
    );
    // The middle of the hole is void; the plate around it is solid.
    let centre = grid.cell_of(Point3::new(20.0, 10.0, 1.0)).unwrap();
    assert!(!grid.is_solid(centre));
    assert!(grid.is_solid(grid.cell_of(Point3::new(5.0, 5.0, 1.0)).unwrap()));
    // The bore wall is a cylindrical face (the kernel splits a full turn
    // into two halves) with cells on it, facing inward.
    let faces = NativeKernel::describe_faces(&snapshot);
    let wall: Vec<EntityRef> = grid
        .faces()
        .into_iter()
        .filter(|face| faces[&face.entity.0].geometry.surface_kind() == "cylinder")
        .collect();
    assert!(
        (1..=2).contains(&wall.len()),
        "a bore wall in one or two faces: {wall:?}"
    );
    let wall_cells = wall
        .iter()
        .map(|face| grid.cells_on_face(*face).count())
        .sum::<usize>();
    assert!(wall_cells >= 16, "{wall_cells} cells on the bore wall");
    for cell in wall.iter().flat_map(|face| grid.cells_on_face(*face)) {
        let centre = grid.cell_centre(cell.cell);
        let stepped = Point3::new(
            centre.x + f64::from(cell.outward[0]),
            centre.y + f64::from(cell.outward[1]),
            centre.z + f64::from(cell.outward[2]),
        );
        let towards_axis =
            (stepped.x - 20.0).hypot(stepped.y - 10.0) < (centre.x - 20.0).hypot(centre.y - 10.0);
        assert!(towards_axis, "a bore wall cell faces the bore");
    }
}

#[test]
fn the_exact_point_query_agrees_with_the_grid_on_a_box() {
    let snapshot = cuboid([1.0, 2.0, 3.0], [4.0, 5.0, 6.0]);
    assert_eq!(
        NativeKernel::point_in_solid(&snapshot, Point3::new(3.0, 4.5, 6.0)),
        Some(true)
    );
    assert_eq!(
        NativeKernel::point_in_solid(&snapshot, Point3::new(0.0, 4.5, 6.0)),
        Some(false)
    );
    assert_eq!(
        NativeKernel::point_in_solid(&snapshot, Point3::new(3.0, 4.5, 20.0)),
        Some(false)
    );
    let grid = NativeKernel::voxelise(&snapshot, 0.5);
    for cell in grid.solid_cells().step_by(7) {
        let centre = grid.cell_centre(cell);
        assert_eq!(
            NativeKernel::point_in_solid(&snapshot, centre),
            Some(true),
            "{centre:?}"
        );
    }
}

#[test]
fn a_degenerate_request_gives_an_empty_grid() {
    let snapshot = cuboid([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
    assert!(NativeKernel::voxelise(&snapshot, 0.0).is_empty());
    assert!(NativeKernel::voxelise(&snapshot, f64::NAN).is_empty());
    assert!(
        NativeKernel::voxelise(&snapshot, 1.0e-6).is_empty(),
        "past MAX_VOXELS"
    );
    assert!(NativeKernel::voxelise(&NativeKernel::empty(), 1.0).is_empty());
}
