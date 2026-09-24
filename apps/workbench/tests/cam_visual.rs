//! Render parity for the CAM tab (ADR 0057 gate 5): the translucent stock
//! ghost, the remaining stock, the tool and the toolpath draw over the
//! part, and the frame is held under pixel review.

use artificer_workbench::KernelLabApp;
use egui::accesskit::Role;
use egui_kittest::{Harness, OsThreshold, SnapshotOptions, kittest::Queryable as _};

fn differing_rgba_pixels(left: &[u8], right: &[u8]) -> usize {
    assert_eq!(left.len(), right.len());
    left.chunks_exact(4)
        .zip(right.chunks_exact(4))
        .filter(|(left, right)| left != right)
        .count()
}

/// The canonical cuboid on the CAM tab with its plan staged and the clock
/// half way through: the stock ghost around the part, the heightmap where
/// the face and profile passes have been, the tool on its path.
#[test]
fn cam_simulation_snapshot() {
    let snapshot_directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots");
    let mut harness = Harness::builder()
        .with_size([1280.0, 800.0])
        .with_pixels_per_point(1.0)
        .with_step_dt(1.0 / 60.0)
        .with_theme(egui::Theme::Dark)
        .with_os(egui::os::OperatingSystem::Nix)
        .with_options(
            SnapshotOptions::new()
                .output_path(snapshot_directory)
                .failed_pixel_count_threshold(OsThreshold::new(0).linux(400).windows(400)),
        )
        .wgpu()
        .build_eframe(|creation_context| KernelLabApp::new_paused(creation_context));
    harness.run();
    harness
        .get_by_role_and_label(Role::Button, "CAM ribbon tab")
        .click_accesskit();
    harness.run();
    harness.remove_cursor();
    harness.run();
    let before = harness.render().expect("the CAM tab renders before a plan");

    harness
        .get_by_role_and_label(Role::Button, "Auto-CAM")
        .click_accesskit();
    harness.run();
    let total = harness
        .state()
        .cam_plan_summary()
        .expect("a plan")
        .total_seconds;
    harness.state_mut().set_cam_time(total * 0.5);
    harness.remove_cursor();
    harness.run();
    let during = harness.render().expect("the simulation renders");
    assert!(
        differing_rgba_pixels(before.as_raw(), during.as_raw()) > 2_000,
        "the stock, tool and toolpath must visibly draw over the part"
    );
    harness.snapshot("workbench_cam_simulation_1280");
}
