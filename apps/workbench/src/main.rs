use artificer_workbench::KernelLabApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Artificer · Workbench")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([1040.0, 700.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Artificer · Workbench",
        options,
        Box::new(|creation_context| Ok(Box::new(KernelLabApp::new(creation_context)))),
    )
}
