use artificer_workbench::KernelLabApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Artificer · Workbench")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([1040.0, 700.0]),
        // egui feathers its own strokes, which is why the edge pass already
        // looks good, but a mesh fill has hard triangle boundaries that
        // feathering never sees: the silhouette of a shaded body is the one
        // place the viewport still shows stair steps. Four samples is the
        // cheapest setting that removes them (ADR 0026, P2).
        multisampling: 4,
        ..Default::default()
    };

    eframe::run_native(
        "Artificer · Workbench",
        options,
        Box::new(|creation_context| Ok(Box::new(KernelLabApp::new(creation_context)))),
    )
}
