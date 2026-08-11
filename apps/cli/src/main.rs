use std::error::Error;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use artificer_testkit::{
    CaseFailure, failure_bundle, parse_case_json, parse_journal_json, replay_journal, run_case,
    scene_svg, to_pretty_json,
};

const MAX_INPUT_JSON_BYTES: u64 = 16 * 1024 * 1024;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("kernel: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut arguments = std::env::args().skip(1);
    let Some(command) = arguments.next() else {
        return Err(CliError::usage().into());
    };

    match command.as_str() {
        "run" => {
            let case_path = required_path(arguments.next(), "run requires <case.json>")?;
            let remaining = arguments.collect::<Vec<_>>();
            let options = parse_run_options(&remaining)?;
            run_case_command(
                &case_path,
                options.journal.as_deref(),
                options.failure_bundle.as_deref(),
            )
        }
        "repeat" => {
            let case_path = required_path(arguments.next(), "repeat requires <case.json>")?;
            let remaining = arguments.collect::<Vec<_>>();
            let count = parse_repeat_options(&remaining)?;
            repeat_command(&case_path, count)
        }
        "replay" => {
            let journal_path = required_path(arguments.next(), "replay requires <journal.json>")?;
            reject_extra(arguments)?;
            replay_command(&journal_path)
        }
        "scene-svg" => {
            let case_path = required_path(arguments.next(), "scene-svg requires <case.json>")?;
            let output_path = required_path(arguments.next(), "scene-svg requires <output.svg>")?;
            reject_extra(arguments)?;
            scene_svg_command(&case_path, &output_path)
        }
        "help" | "--help" | "-h" => {
            println!("{}", CliError::USAGE);
            Ok(())
        }
        unknown => Err(CliError(format!(
            "unknown command `{unknown}`\n\n{}",
            CliError::USAGE
        ))
        .into()),
    }
}

fn run_case_command(
    case_path: &Path,
    journal_path: Option<&Path>,
    bundle_path: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    let case = parse_case_json(&read(case_path)?)?;
    let run = run_case(&case)?;

    if let Some(path) = journal_path {
        fs::write(path, to_pretty_json(&run.journal)?)?;
    }

    if !run.passed() {
        if let Some(path) = bundle_path {
            write_failure_bundle(path, case_path, &run)?;
        }
        return Err(CliError(format_failures(&run.case_id, &run.failures)).into());
    }
    match run.final_digest() {
        Some(digest) => println!("PASS {} · digest {digest}", run.case_id),
        None => println!("PASS {} · expected error path", run.case_id),
    }
    Ok(())
}

fn repeat_command(case_path: &Path, count: usize) -> Result<(), Box<dyn Error>> {
    let case = parse_case_json(&read(case_path)?)?;
    let first = run_case(&case)?;
    if !first.passed() {
        return Err(CliError(format_failures(&first.case_id, &first.failures)).into());
    }
    let expected = to_pretty_json(&first.journal)?;
    for iteration in 1..count {
        let run = run_case(&case)?;
        if !run.passed() {
            return Err(CliError(format!(
                "repeat {} failed at iteration {}\n{}",
                case.case_id,
                iteration + 1,
                format_failures(&run.case_id, &run.failures)
            ))
            .into());
        }
        if to_pretty_json(&run.journal)? != expected {
            return Err(CliError(format!(
                "repeat {} diverged at iteration {}",
                case.case_id,
                iteration + 1
            ))
            .into());
        }
    }
    match first.final_digest() {
        Some(digest) => println!(
            "REPEAT PASS {} · {count} runs · digest {digest}",
            case.case_id
        ),
        None => println!(
            "REPEAT PASS {} · {count} identical expected-error runs",
            case.case_id
        ),
    }
    Ok(())
}

fn write_failure_bundle(
    output: &Path,
    case_path: &Path,
    run: &artificer_testkit::CaseRun,
) -> Result<(), Box<dyn Error>> {
    let bundle = failure_bundle(run)?;
    fs::create_dir_all(output)?;
    fs::write(
        output.join("manifest.json"),
        to_pretty_json(&bundle.manifest)?,
    )?;
    fs::write(output.join("journal.json"), bundle.journal_json)?;
    fs::write(output.join("scene.svg"), bundle.scene_svg)?;
    fs::write(output.join("case.json"), read(case_path)?)?;
    println!("WROTE FAILURE BUNDLE {}", output.display());
    Ok(())
}

fn replay_command(journal_path: &Path) -> Result<(), Box<dyn Error>> {
    let journal = parse_journal_json(&read(journal_path)?)?;
    let replay = replay_journal(&journal)?;
    if !replay.passed() {
        return Err(CliError(format_failures(&replay.case_id, &replay.failures)).into());
    }
    match replay.final_report {
        Some(report) => println!(
            "REPLAY PASS {} · digest {}",
            replay.case_id, report.semantic_digest
        ),
        None => println!("REPLAY PASS {} · expected error path", replay.case_id),
    }
    Ok(())
}

fn scene_svg_command(case_path: &Path, output_path: &Path) -> Result<(), Box<dyn Error>> {
    let case = parse_case_json(&read(case_path)?)?;
    let run = run_case(&case)?;
    if !run.passed() {
        return Err(CliError(format_failures(&run.case_id, &run.failures)).into());
    }
    let svg = scene_svg(&run)?;
    fs::write(output_path, svg)?;
    println!("WROTE {}", output_path.display());
    Ok(())
}

#[derive(Default)]
struct RunOptions {
    journal: Option<PathBuf>,
    failure_bundle: Option<PathBuf>,
}

fn parse_run_options(arguments: &[String]) -> Result<RunOptions, Box<dyn Error>> {
    let mut options = RunOptions::default();
    let mut index = 0;
    while index < arguments.len() {
        let value = arguments.get(index + 1).ok_or_else(|| {
            CliError(format!(
                "{} requires a path\n\n{}",
                arguments[index],
                CliError::USAGE
            ))
        })?;
        match arguments[index].as_str() {
            "--journal" if options.journal.is_none() => options.journal = Some(value.into()),
            "--failure-bundle" if options.failure_bundle.is_none() => {
                options.failure_bundle = Some(value.into());
            }
            unknown => {
                return Err(CliError(format!(
                    "unknown or duplicate run option `{unknown}`\n\n{}",
                    CliError::USAGE
                ))
                .into());
            }
        }
        index += 2;
    }
    Ok(options)
}

fn parse_repeat_options(arguments: &[String]) -> Result<usize, Box<dyn Error>> {
    match arguments {
        [] => Ok(100),
        [flag, count] if flag == "--count" => {
            let count = count
                .parse::<usize>()
                .map_err(|_| CliError("repeat count must be a positive integer".to_owned()))?;
            if count == 0 {
                return Err(CliError("repeat count must be greater than zero".to_owned()).into());
            }
            Ok(count)
        }
        _ => Err(CliError(format!("invalid repeat options\n\n{}", CliError::USAGE)).into()),
    }
}

fn required_path(value: Option<String>, message: &str) -> Result<PathBuf, Box<dyn Error>> {
    value
        .map(PathBuf::from)
        .ok_or_else(|| CliError(format!("{message}\n\n{}", CliError::USAGE)).into())
}

fn reject_extra(mut arguments: impl Iterator<Item = String>) -> Result<(), Box<dyn Error>> {
    if let Some(extra) = arguments.next() {
        return Err(CliError(format!(
            "unexpected argument `{extra}`\n\n{}",
            CliError::USAGE
        ))
        .into());
    }
    Ok(())
}

fn read(path: &Path) -> Result<String, Box<dyn Error>> {
    let file = fs::File::open(path)
        .map_err(|error| CliError(format!("could not read {}: {error}", path.display())))?;
    let mut input = String::new();
    file.take(MAX_INPUT_JSON_BYTES + 1)
        .read_to_string(&mut input)
        .map_err(|error| CliError(format!("could not read {}: {error}", path.display())))?;
    if input.len() as u64 > MAX_INPUT_JSON_BYTES {
        return Err(CliError(format!(
            "refusing to read {}: JSON input exceeds the {} MiB safety limit",
            path.display(),
            MAX_INPUT_JSON_BYTES / (1024 * 1024)
        ))
        .into());
    }
    Ok(input)
}

fn format_failures(case_id: &str, failures: &[CaseFailure]) -> String {
    let details = failures
        .iter()
        .map(|failure| format!("  {}: {}", failure.step_id, failure.message))
        .collect::<Vec<_>>()
        .join("\n");
    format!("FAIL {case_id}\n{details}")
}

#[derive(Debug)]
struct CliError(String);

impl CliError {
    const USAGE: &'static str = "Usage:\n  kernel run <case.json> [--journal <output.json>] [--failure-bundle <directory>]\n  kernel repeat <case.json> [--count <positive integer>]\n  kernel replay <journal.json>\n  kernel scene-svg <case.json> <output.svg>";

    fn usage() -> Self {
        Self(Self::USAGE.to_owned())
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for CliError {}
