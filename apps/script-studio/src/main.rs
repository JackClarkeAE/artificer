use std::path::PathBuf;

use artificer_script_studio::ScriptStudio;

fn main() -> eframe::Result<()> {
    // `artificer-script-studio [script.art]`: the one argument is a script to
    // open. Anything else is a usage error worth saying out loud rather than
    // silently opening the welcome script.
    let mut arguments = std::env::args_os().skip(1);
    let script = arguments.next().map(PathBuf::from);
    if arguments.next().is_some() {
        eprintln!("usage: artificer-script-studio [script.art]");
        std::process::exit(2);
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Artificer · Script Studio")
            .with_inner_size([1360.0, 840.0])
            .with_min_inner_size([960.0, 600.0]),
        // The same anti-aliasing the workbench uses: the silhouette of a
        // shaded body is the one place a mesh fill shows stair steps.
        multisampling: 4,
        ..Default::default()
    };

    eframe::run_native(
        "Artificer · Script Studio",
        options,
        Box::new(move |creation_context| Ok(Box::new(ScriptStudio::new(creation_context, script)))),
    )
}
