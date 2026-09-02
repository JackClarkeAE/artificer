use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use artificer_kernel::CancellationToken;
use artificer_kernel::api::export::{export_obj, export_stl_binary};
use artificer_kernel::api::scripting::compile_script;
use artificer_kernel::api::server::serve_stdio;
use artificer_kernel::api::session::Session;
use artificer_kernel::api::snapshot::{CameraSpec, SnapshotOptions, SnapshotOutput, StandardView};

const USAGE: &str = "\
USAGE:
    artificer-api <COMMAND> [OPTIONS]

COMMANDS:
    serve                         Start the JSON-RPC server on stdin/stdout
    run <script.art>              Execute an .art script file and print result summary
    snapshot <script.art> <out>   Render an SVG visual snapshot of the script
    export <script.art> <out>     Export 3D geometry (.stl or .obj)
    journal <journal.json>        Replay a saved command journal
    help                          Show this help message

OPTIONS:
    --param <KEY=VALUE>           Override script parameter (can be specified multiple times)
";

fn main() -> ExitCode {
    match run_app() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("artificer-api error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_app() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        println!("{USAGE}");
        return Ok(());
    };

    match command.as_str() {
        "serve" => {
            eprintln!("artificer-api: JSON-RPC server listening on stdin/stdout...");
            serve_stdio()?;
            Ok(())
        }
        "run" => {
            let script_path = required_arg(args.next(), "run requires <script.art>")?;
            let params = parse_param_flags(args)?;
            let source = fs::read_to_string(&script_path)?;
            let commands = compile_script(&source, &params)?;

            let mut session = Session::new();
            let token = CancellationToken::default();
            for cmd in commands {
                let res = session.execute(cmd, &token)?;
                println!(
                    "  \u{2713} {}: {} ({:?})",
                    res.step_label,
                    res.topology,
                    res.elapsed()
                );
            }

            println!("Success! Final snapshot {}", session.snapshot.id());
            if let Some(bounds) = session.snapshot.measures().bounds {
                println!("Bounds: [{}, {}]", bounds.min, bounds.max);
            }
            Ok(())
        }
        "snapshot" => {
            let script_path = required_arg(args.next(), "snapshot requires <script.art>")?;
            let out_path = required_arg(args.next(), "snapshot requires <output.svg>")?;
            let (params, view) = parse_cli_flags(args)?;
            let source = fs::read_to_string(&script_path)?;
            let commands = compile_script(&source, &params)?;

            let mut session = Session::new();
            let token = CancellationToken::default();
            for cmd in commands {
                session.execute(cmd, &token)?;
            }

            let snap_opts = SnapshotOptions {
                camera: CameraSpec::preset(view),
                ..Default::default()
            };
            let snap_out = session.snapshot(snap_opts)?;
            match snap_out {
                SnapshotOutput::Svg(svg) => fs::write(&out_path, svg)?,
                SnapshotOutput::Png(bytes) => fs::write(&out_path, bytes)?,
            }
            println!("Wrote snapshot to {out_path}");
            Ok(())
        }
        "export" => {
            let script_path = required_arg(args.next(), "export requires <script.art>")?;
            let out_path = required_arg(args.next(), "export requires <output_file>")?;
            let params = parse_param_flags(args)?;
            let source = fs::read_to_string(&script_path)?;
            let commands = compile_script(&source, &params)?;

            let mut session = Session::new();
            let token = CancellationToken::default();
            for cmd in commands {
                session.execute(cmd, &token)?;
            }

            let out_path_buf = PathBuf::from(&out_path);
            let ext = out_path_buf
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("stl");

            match ext {
                "stl" => {
                    let bytes = export_stl_binary(&session.snapshot)?;
                    fs::write(&out_path, bytes)?;
                }
                "obj" => {
                    let obj = export_obj(&session.snapshot, "model")?;
                    fs::write(&out_path, obj)?;
                }
                other => {
                    return Err(format!("Unsupported export format: .{other}").into());
                }
            }
            println!("Exported model to {out_path}");
            Ok(())
        }
        "journal" => {
            let journal_path = required_arg(args.next(), "journal requires <journal.json>")?;
            let json = fs::read_to_string(&journal_path)?;
            let session = Session::from_journal(&json)?;
            println!(
                "Replayed journal successfully. Snapshot: {}. Topology: {}",
                session.snapshot.id(),
                session.snapshot.counts()
            );
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!("{USAGE}");
            Ok(())
        }
        unknown => {
            eprintln!("Unknown command `{unknown}`\n\n{USAGE}");
            Err(format!("Unknown command `{unknown}`").into())
        }
    }
}

fn required_arg(arg: Option<String>, err_msg: &str) -> Result<String, Box<dyn Error>> {
    arg.ok_or_else(|| err_msg.into())
}

fn parse_cli_flags(
    args: impl Iterator<Item = String>,
) -> Result<(BTreeMap<String, f64>, StandardView), Box<dyn Error>> {
    let mut params = BTreeMap::new();
    let mut view = StandardView::Isometric;
    let mut iter = args.peekable();

    while let Some(arg) = iter.next() {
        if arg == "--param" {
            let Some(val_str) = iter.next() else {
                return Err("--param requires KEY=VALUE".into());
            };
            let parts = val_str.split_once('=');
            let Some((k, v)) = parts else {
                return Err(format!("Invalid --param `{val_str}`, expected KEY=VALUE").into());
            };
            let num: f64 = v.parse()?;
            params.insert(k.trim().to_owned(), num);
        } else if arg == "--view" {
            let Some(view_str) = iter.next() else {
                return Err(
                    "--view requires a preset (isometric, trimetric, front, top, right)".into(),
                );
            };
            view = match view_str.to_lowercase().as_str() {
                "isometric" | "iso" => StandardView::Isometric,
                "trimetric" | "tri" => StandardView::Trimetric,
                "front" => StandardView::Front,
                "top" => StandardView::Top,
                "right" => StandardView::Right,
                "back" => StandardView::Back,
                "bottom" => StandardView::Bottom,
                "left" => StandardView::Left,
                other => return Err(format!("Unknown view preset: {other}").into()),
            };
        }
    }

    Ok((params, view))
}

fn parse_param_flags(
    args: impl Iterator<Item = String>,
) -> Result<BTreeMap<String, f64>, Box<dyn Error>> {
    let (params, _) = parse_cli_flags(args)?;
    Ok(params)
}
