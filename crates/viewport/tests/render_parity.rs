use std::f64::consts::FRAC_PI_4;

use artificer_kernel::{CancellationToken, DebugScene, NativeKernel};
use artificer_protocol::{
    Aabb3, ExecuteRequest, KernelCommand, Point3, PrecisionPolicy, RequestId, Vector3,
    CURRENT_PROTOCOL_VERSION,
};
use artificer_ui_core::presentation::{
    ActiveTool, DisplayTransform, ProjectionMode, SectionCutPlane, ViewState,
};
use artificer_viewport::{
    show_document_with_feature_drag, BodyInstanceKey, DocumentBodyInstance, FeaturePreviewDragState,
    ModelDisplayMode,
};
use egui_kittest::Harness;

fn cuboid_fixture() -> (DebugScene, Aabb3, Point3) {
    let input = NativeKernel::empty();
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::from("viewport/render-parity-cuboid"),
        expected_snapshot: input.id(),
        precision: PrecisionPolicy::default(),
        command: KernelCommand::MakeCuboid {
            origin: Point3::new(-10.0, -10.0, -10.0),
            size_x: 20.0,
            size_y: 20.0,
            size_z: 20.0,
        },
    };
    let outcome = NativeKernel::execute(&input, &request, &CancellationToken::new())
        .expect("cuboid fixture");
    let bounds = outcome.report.bounds.expect("cuboid bounds");
    let pivot = Point3::new(
        (bounds.min.x + bounds.max.x) * 0.5,
        (bounds.min.y + bounds.max.y) * 0.5,
        (bounds.min.z + bounds.max.z) * 0.5,
    );
    (NativeKernel::debug_scene(&outcome.snapshot), bounds, pivot)
}

#[test]
fn test_perspective_projection_rendering() {
    let (scene, bounds, pivot) = cuboid_fixture();
    let body_key = BodyInstanceKey::new(1);
    let mut view = ViewState::default();
    view.projection_mode = ProjectionMode::Perspective {
        fov_y_radians: FRAC_PI_4,
    };
    view.frame(bounds);
    let mut display_transform = DisplayTransform::default();
    let mut drag_state = FeaturePreviewDragState::default();
    let mut edge_frame_memo = None;

    let mut harness = Harness::builder()
        .with_size([800.0, 600.0])
        .wgpu()
        .build_ui(|ui| {
            let body = DocumentBodyInstance::new(body_key, &scene, Some(bounds), pivot);

            let _ = show_document_with_feature_drag(
                ui,
                &[body],
                Some(bounds),
                true,
                ModelDisplayMode::ShadedEdges,
                None,
                None,
                None,
                &[],
                &[],
                &[],
                Some(body_key),
                ActiveTool::Select,
                &mut display_transform,
                &mut view,
                0.0,
                None,
                &[],
                &[],
                &[],
                None,
                None,
                &mut drag_state,
                &mut edge_frame_memo,
                artificer_ui_core::navigation::NavigationPreset::Artificer.bindings(),
            );
        });

    harness.step();
}

#[test]
fn test_section_cut_plane_rendering() {
    let (scene, bounds, pivot) = cuboid_fixture();
    let body_key = BodyInstanceKey::new(1);
    let mut view = ViewState::default();
    view.section_cut_plane = Some(SectionCutPlane::new(
        Vector3::new(0.0, 0.0, 1.0),
        0.0,
    ));
    view.frame(bounds);
    let mut display_transform = DisplayTransform::default();
    let mut drag_state = FeaturePreviewDragState::default();
    let mut edge_frame_memo = None;

    let mut harness = Harness::builder()
        .with_size([800.0, 600.0])
        .wgpu()
        .build_ui(|ui| {
            let body = DocumentBodyInstance::new(body_key, &scene, Some(bounds), pivot);

            let _ = show_document_with_feature_drag(
                ui,
                &[body],
                Some(bounds),
                true,
                ModelDisplayMode::ShadedEdges,
                None,
                None,
                None,
                &[],
                &[],
                &[],
                Some(body_key),
                ActiveTool::Select,
                &mut display_transform,
                &mut view,
                0.0,
                None,
                &[],
                &[],
                &[],
                None,
                None,
                &mut drag_state,
                &mut edge_frame_memo,
                artificer_ui_core::navigation::NavigationPreset::Artificer.bindings(),
            );
        });

    harness.step();
}

#[test]
fn test_display_modes_render_cleanly() {
    let (scene, bounds, pivot) = cuboid_fixture();
    let body_key = BodyInstanceKey::new(1);

    for mode in [
        ModelDisplayMode::ShadedEdges,
        ModelDisplayMode::Diagnostic,
        ModelDisplayMode::Wireframe,
        ModelDisplayMode::HiddenLinesRemoved,
    ] {
        let mut view = ViewState::default();
        view.frame(bounds);
        let mut display_transform = DisplayTransform::default();
        let mut drag_state = FeaturePreviewDragState::default();
        let mut edge_frame_memo = None;

        let mut harness = Harness::builder()
            .with_size([800.0, 600.0])
            .wgpu()
            .build_ui(|ui| {
                let body = DocumentBodyInstance::new(body_key, &scene, Some(bounds), pivot);

                let _ = show_document_with_feature_drag(
                    ui,
                    &[body],
                    Some(bounds),
                    true,
                    mode,
                    None,
                    None,
                    None,
                    &[],
                    &[],
                    &[],
                    Some(body_key),
                    ActiveTool::Select,
                    &mut display_transform,
                    &mut view,
                    0.0,
                    None,
                    &[],
                    &[],
                    &[],
                    None,
                    None,
                    &mut drag_state,
                    &mut edge_frame_memo,
                    artificer_ui_core::navigation::NavigationPreset::Artificer.bindings(),
                );
            });

        harness.step();
    }
}

#[test]
fn test_gpu_fill_backend_rendering() {
    let (scene, bounds, pivot) = cuboid_fixture();
    let body_key = BodyInstanceKey::new(1);

    for backend in [
        artificer_ui_core::presentation::FillBackend::GpuOnly,
        artificer_ui_core::presentation::FillBackend::CpuOnly,
        artificer_ui_core::presentation::FillBackend::Auto,
    ] {
        let mut view = ViewState::default();
        view.fill_backend = backend;
        view.frame(bounds);
        let mut display_transform = DisplayTransform::default();
        let mut drag_state = FeaturePreviewDragState::default();
        let mut edge_frame_memo = None;

        let mut harness = Harness::builder()
            .with_size([800.0, 600.0])
            .wgpu()
            .build_ui(|ui| {
                let body = DocumentBodyInstance::new(body_key, &scene, Some(bounds), pivot);

                let _ = show_document_with_feature_drag(
                    ui,
                    &[body],
                    Some(bounds),
                    true,
                    ModelDisplayMode::ShadedEdges,
                    None,
                    None,
                    None,
                    &[],
                    &[],
                    &[],
                    Some(body_key),
                    ActiveTool::Select,
                    &mut display_transform,
                    &mut view,
                    0.0,
                    None,
                    &[],
                    &[],
                    &[],
                    None,
                    None,
                    &mut drag_state,
                    &mut edge_frame_memo,
                    artificer_ui_core::navigation::NavigationPreset::Artificer.bindings(),
                );
            });

        harness.step();
    }
}
