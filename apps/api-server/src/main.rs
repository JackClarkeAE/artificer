use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use artificer_kernel::CancellationToken;
use artificer_kernel::api::decompile::DecompileOptions;
use artificer_kernel::api::diff::ScriptDiff;
use artificer_kernel::api::export::{
    export_obj, export_step, export_step_faceted, export_stl_binary,
};
use artificer_kernel::api::scripting::{FileModules, compile_program_with, script_parameters};
use artificer_kernel::api::server::serve_stdio;
use artificer_kernel::api::session::Session;
use artificer_kernel::api::snapshot::{
    CameraSpec, SnapshotFormat, SnapshotOptions, SnapshotOutput, StandardView,
};

const USAGE: &str = "\
USAGE:
    artificer-api <COMMAND> [OPTIONS]

COMMANDS:
    serve                         Start the JSON-RPC server on stdin/stdout
    run <script.art>              Execute an .art script file and print result summary
    report <script.art>           Execute an .art script and print the session report as JSON
    params <script.art>           List the script's parameters without running it
    snapshot <script.art> <out>   Render a visual snapshot of the script: .png for a raster
                                  image, any other extension for SVG
    export <script.art> <out>     Export 3D geometry: .stl, .obj, or .step (exact B-rep;
                                  --faceted for the triangle surface model instead)
    journal <journal.json>        Replay a saved command journal; --art <out.art> writes it
                                  back as a script
    diff <a.art> <b.art>          Semantic diff of two scripts: parameters, steps and names;
                                  exits non-zero when they differ
    help                          Show this help message

OPTIONS:
    --param <KEY=VALUE>           Override a parameter the script declares (can be specified
                                  multiple times); a name the script does not declare is an error
    --view <PRESET>               With `snapshot`: the camera preset, one of isometric,
                                  trimetric, front, back, top, bottom, left or right
                                  (default isometric)
    --json                        With `run`: print the session report as JSON instead of prose.
                                  A failed run still prints the report, with `status: failed`
                                  and the failing step, and exits non-zero.
                                  With `params` and `diff`: print the result as JSON.
    --faceted                     With `export` to STEP: write the triangle surface model
    --module-path <DIR>           A directory to search for `use` modules, after the script's
                                  own directory (can be specified multiple times)
";

fn main() -> ExitCode {
    match run_app() {
        Ok(()) => ExitCode::SUCCESS,
        // The reader went away (`report part.art | head`): there is nobody
        // to tell, and nothing went wrong.
        Err(error) if error.is::<OutputClosed>() => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("artificer-api error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Standard output was closed by whoever was reading it.
#[derive(Debug)]
struct OutputClosed;

impl fmt::Display for OutputClosed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("standard output was closed")
    }
}

impl Error for OutputClosed {}

/// Writes one line to standard output. Every line the program prints goes
/// through here, so a closed pipe ends the run quietly instead of as a
/// panic from `println!`.
fn emit(text: impl fmt::Display) -> Result<(), Box<dyn Error>> {
    let mut stdout = io::stdout().lock();
    match writeln!(stdout, "{text}").and_then(|()| stdout.flush()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Err(Box::new(OutputClosed)),
        Err(error) => Err(error.into()),
    }
}

/// The flags every script command accepts.
struct Flags {
    params: BTreeMap<String, f64>,
    view: StandardView,
    json: bool,
    faceted: bool,
    module_path: Vec<PathBuf>,
}

impl Default for Flags {
    fn default() -> Self {
        Self {
            params: BTreeMap::new(),
            view: StandardView::Isometric,
            json: false,
            faceted: false,
            module_path: Vec::new(),
        }
    }
}

impl Flags {
    /// How `use` lines resolve for a script at `script`: beside it first,
    /// then along `--module-path`.
    fn modules(&self, script: &Path) -> FileModules {
        let mut modules = FileModules::beside(script);
        for directory in &self.module_path {
            modules = modules.with_search_path(directory.clone());
        }
        modules
    }

    /// Refuses a `--param` whose name none of the scripts declares: a typo
    /// would otherwise be accepted and silently change nothing.
    fn check_params(&self, sources: &[&str]) -> Result<(), Box<dyn Error>> {
        if self.params.is_empty() {
            return Ok(());
        }
        let mut declared = BTreeSet::new();
        for source in sources {
            for parameter in script_parameters(source)? {
                declared.insert(parameter.name);
            }
        }
        let unknown = self
            .params
            .keys()
            .filter(|name| !declared.contains(*name))
            .cloned()
            .collect::<Vec<_>>();
        if unknown.is_empty() {
            return Ok(());
        }
        let declared = if declared.is_empty() {
            "the script declares no parameters".to_owned()
        } else {
            format!(
                "the script declares {}",
                declared.into_iter().collect::<Vec<_>>().join(", ")
            )
        };
        Err(format!(
            "--param names {} the script does not declare: {}; {declared}",
            if unknown.len() == 1 {
                "a parameter"
            } else {
                "parameters"
            },
            unknown.join(", ")
        )
        .into())
    }
}

fn run_app() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        emit(USAGE)?;
        return Ok(());
    };

    match command.as_str() {
        "serve" => {
            eprintln!("artificer-api: JSON-RPC server listening on stdin/stdout...");
            serve_stdio()?;
            Ok(())
        }
        "run" | "report" => {
            let script_path = required_arg(args.next(), "run requires <script.art>")?;
            let flags = parse_flags(args)?;
            let json = flags.json || command == "report";
            let source = fs::read_to_string(&script_path)?;
            flags.check_params(&[&source])?;
            let modules = flags.modules(Path::new(&script_path));

            let mut session = Session::new();
            let token = CancellationToken::default();
            if json {
                let outcome = session.run_script_with(&source, &flags.params, &modules, &token);
                let failed = !outcome.succeeded();
                let report = session.report_with(outcome.failure);
                emit(serde_json::to_string_pretty(&report)?)?;
                if failed {
                    return Err(
                        "the script did not run to completion; see the report's `failure`".into(),
                    );
                }
                return Ok(());
            }

            let program = compile_program_with(&source, &flags.params, &modules)?;
            for cmd in program.commands {
                let res = session.execute(cmd, &token)?;
                emit(format!(
                    "  \u{2713} {}: {} ({:?}){}",
                    res.step_label,
                    res.topology,
                    res.elapsed(),
                    match res.rung.as_deref() {
                        Some(rung) => format!(" [{rung}, {:?}]", res.tier),
                        None => String::new(),
                    }
                ))?;
            }

            emit(format!("Success! Final snapshot {}", session.snapshot.id()))?;
            if let Some(bounds) = session.snapshot.measures().bounds {
                emit(format!("Bounds: [{}, {}]", bounds.min, bounds.max))?;
            }
            Ok(())
        }
        "params" => {
            let script_path = required_arg(args.next(), "params requires <script.art>")?;
            let flags = parse_flags(args)?;
            let source = fs::read_to_string(&script_path)?;
            let parameters = script_parameters(&source)?;
            if flags.json {
                emit(serde_json::to_string_pretty(&parameters)?)?;
                return Ok(());
            }
            if parameters.is_empty() {
                emit("The script declares no parameters.")?;
            }
            for parameter in parameters {
                let unit = parameter
                    .unit
                    .as_deref()
                    .map_or(String::new(), |unit| format!(" [{unit}]"));
                let range = match (parameter.min, parameter.max) {
                    (Some(min), Some(max)) => format!(" in {min}..{max}"),
                    _ => String::new(),
                };
                let description = parameter
                    .description
                    .as_deref()
                    .map_or(String::new(), |text| format!("  \u{2014} {text}"));
                emit(format!(
                    "  {}: {}{unit}{range} = {}{description}  (line {})",
                    parameter.name, parameter.param_type, parameter.default_text, parameter.line
                ))?;
            }
            Ok(())
        }
        "snapshot" => {
            let script_path = required_arg(args.next(), "snapshot requires <script.art>")?;
            let out_path = required_arg(args.next(), "snapshot requires <output.png|output.svg>")?;
            let flags = parse_flags(args)?;
            let source = fs::read_to_string(&script_path)?;
            flags.check_params(&[&source])?;
            let modules = flags.modules(Path::new(&script_path));
            let program = compile_program_with(&source, &flags.params, &modules)?;

            let mut session = Session::new();
            let token = CancellationToken::default();
            for cmd in program.commands {
                session.execute(cmd, &token)?;
            }

            // The extension chooses the format: `.png` is a raster image,
            // anything else is SVG.
            let format = match extension_of(&out_path).as_deref() {
                Some("png") => SnapshotFormat::Png,
                _ => SnapshotFormat::Svg,
            };
            let snap_opts = SnapshotOptions {
                camera: CameraSpec::preset(flags.view),
                format,
                ..Default::default()
            };
            let snap_out = session.snapshot(snap_opts)?;
            match snap_out {
                SnapshotOutput::Svg(svg) => fs::write(&out_path, svg)?,
                SnapshotOutput::Png(bytes) => fs::write(&out_path, bytes)?,
            }
            emit(format!("Wrote snapshot to {out_path}"))?;
            Ok(())
        }
        "export" => {
            let script_path = required_arg(args.next(), "export requires <script.art>")?;
            let out_path = required_arg(args.next(), "export requires <output_file>")?;
            let flags = parse_flags(args)?;
            let Some(ext) = extension_of(&out_path) else {
                return Err(format!(
                    "export needs an output extension to choose the format: `{out_path}` has none; use .stl, .obj or .step"
                )
                .into());
            };
            let source = fs::read_to_string(&script_path)?;
            flags.check_params(&[&source])?;
            let modules = flags.modules(Path::new(&script_path));
            let program = compile_program_with(&source, &flags.params, &modules)?;

            let mut session = Session::new();
            let token = CancellationToken::default();
            for cmd in program.commands {
                session.execute(cmd, &token)?;
            }

            let name = Path::new(&script_path)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("model");
            match ext.as_str() {
                "stl" => {
                    let bytes = export_stl_binary(&session.snapshot)?;
                    fs::write(&out_path, bytes)?;
                }
                "obj" => {
                    let obj = export_obj(&session.snapshot, name)?;
                    fs::write(&out_path, obj)?;
                }
                "step" | "stp" => {
                    // Exact B-rep unless the caller asked for facets.
                    let step = if flags.faceted {
                        export_step_faceted(&session.snapshot, name)
                    } else {
                        export_step(&session.snapshot, name)?
                    };
                    fs::write(&out_path, step)?;
                    emit(format!(
                        "Exported {} STEP to {out_path}",
                        if flags.faceted {
                            "faceted"
                        } else {
                            "exact B-rep"
                        }
                    ))?;
                    return Ok(());
                }
                other => {
                    return Err(format!("Unsupported export format: .{other}").into());
                }
            }
            emit(format!("Exported model to {out_path}"))?;
            Ok(())
        }
        "journal" => {
            let journal_path = required_arg(args.next(), "journal requires <journal.json>")?;
            let rest = args.collect::<Vec<_>>();
            let art_path = match rest.as_slice() {
                [] => None,
                [flag, path] if flag == "--art" => Some(PathBuf::from(path)),
                _ => return Err(format!("journal takes only --art <out.art>\n\n{USAGE}").into()),
            };
            let json = fs::read_to_string(&journal_path)?;
            let session = Session::from_journal(&json)?;
            emit(format!(
                "Replayed journal successfully. Snapshot: {}. Topology: {}",
                session.snapshot.id(),
                session.snapshot.counts()
            ))?;
            if let Some(art_path) = art_path {
                let script = session.to_art(&DecompileOptions::default())?;
                fs::write(&art_path, script)?;
                emit(format!("Wrote {}", art_path.display()))?;
            }
            Ok(())
        }
        "diff" => {
            let a_path = required_arg(args.next(), "diff requires <a.art> <b.art>")?;
            let b_path = required_arg(args.next(), "diff requires <a.art> <b.art>")?;
            let flags = parse_flags(args)?;
            let a_source = fs::read_to_string(&a_path)?;
            let b_source = fs::read_to_string(&b_path)?;
            // An override may belong to either side: a parameter one script
            // adds is a legitimate thing to set.
            flags.check_params(&[&a_source, &b_source])?;
            let old =
                compile_program_with(&a_source, &flags.params, &flags.modules(Path::new(&a_path)))?;
            let new =
                compile_program_with(&b_source, &flags.params, &flags.modules(Path::new(&b_path)))?;
            let diff = ScriptDiff::between(&old, &new);
            if flags.json {
                emit(serde_json::to_string_pretty(&diff)?)?;
            } else if diff.is_empty() {
                emit("No semantic difference.")?;
            } else {
                for line in diff.lines() {
                    emit(format!("  {line}"))?;
                }
            }
            if diff.is_empty() {
                Ok(())
            } else {
                Err("the scripts differ".into())
            }
        }
        "help" | "--help" | "-h" => {
            emit(USAGE)?;
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

/// The output file's extension, lower-cased, when it has one.
fn extension_of(path: &str) -> Option<String> {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
}

fn parse_flags(args: impl Iterator<Item = String>) -> Result<Flags, Box<dyn Error>> {
    let mut flags = Flags::default();
    let mut iter = args.peekable();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--param" => {
                let Some(val_str) = iter.next() else {
                    return Err("--param requires KEY=VALUE".into());
                };
                let parts = val_str.split_once('=');
                let Some((k, v)) = parts else {
                    return Err(format!("Invalid --param `{val_str}`, expected KEY=VALUE").into());
                };
                let key = k.trim();
                let value = v.trim();
                let num: f64 = match value {
                    "true" => 1.0,
                    "false" => 0.0,
                    number => number.parse().map_err(|_| {
                        format!(
                            "--param {key}: `{value}` is not a number; a parameter takes a number, `true` or `false`"
                        )
                    })?,
                };
                flags.params.insert(key.to_owned(), num);
            }
            "--view" => {
                let Some(view_str) = iter.next() else {
                    return Err(
                        "--view requires a preset (isometric, trimetric, front, top, right)".into(),
                    );
                };
                flags.view = match view_str.to_lowercase().as_str() {
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
            "--json" => flags.json = true,
            "--faceted" => flags.faceted = true,
            "--module-path" => {
                let Some(directory) = iter.next() else {
                    return Err("--module-path requires a directory".into());
                };
                flags.module_path.push(PathBuf::from(directory));
            }
            other => return Err(format!("Unknown option `{other}`\n\n{USAGE}").into()),
        }
    }

    Ok(flags)
}
