//! The `artificer-api` command, driven the way a shell would drive it.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_artificer-api");

fn artificer(args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("the command runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A directory of this test's own to write files into.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("artificer-api-cli-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn example(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/kernel/examples")
        .join(name)
        .to_str()
        .unwrap()
        .to_owned()
}

#[test]
fn the_output_extension_chooses_png_or_svg() {
    let dir = scratch("snapshot");
    let script = dir.join("block.art");
    std::fs::write(&script, "let b = box(size: [20, 10, 5], label: \"b\");\n").unwrap();
    let script = script.to_str().unwrap();

    let png = dir.join("block.png");
    let output = artificer(&["snapshot", script, png.to_str().unwrap(), "--view", "top"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let bytes = std::fs::read(&png).unwrap();
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");

    let svg = dir.join("block.svg");
    let output = artificer(&["snapshot", script, svg.to_str().unwrap()]);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = std::fs::read_to_string(&svg).unwrap();
    assert!(text.contains("<svg"), "not an SVG: {text}");
}

#[test]
fn an_override_the_script_does_not_declare_is_refused() {
    let hub = example("flanged_hub.art");
    let output = artificer(&["run", &hub, "--param", "nosuch=5"]);
    assert!(!output.status.success());
    let message = stderr(&output);
    assert!(message.contains("nosuch"), "{message}");
    // The declared names are listed, so the typo is easy to fix.
    assert!(message.contains("hub_radius"), "{message}");

    // A declared one is taken, on every command that compiles a script.
    for command in ["run", "report"] {
        let output = artificer(&[command, &hub, "--param", "bolt_count=6"]);
        assert!(output.status.success(), "{command}: {}", stderr(&output));
    }
    let output = artificer(&["diff", &hub, &hub, "--param", "nosuch=1"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("nosuch"));
}

#[test]
fn a_value_that_is_not_a_number_names_its_key() {
    let output = artificer(&[
        "run",
        &example("flanged_hub.art"),
        "--param",
        "hub_radius=abc",
    ]);
    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(stderr.contains("hub_radius"), "{stderr}");
    assert!(stderr.contains("abc"), "{stderr}");
}

#[test]
fn an_export_without_an_extension_is_refused() {
    let dir = scratch("export");
    let out = dir.join("hub");
    let output = artificer(&["export", &example("flanged_hub.art"), out.to_str().unwrap()]);
    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(stderr.contains("extension"), "{stderr}");
    assert!(!out.exists(), "nothing is written without a format");
}

#[test]
fn the_flanged_hub_example_runs() {
    let output = artificer(&["run", &example("flanged_hub.art")]);
    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = stdout(&output);
    assert!(stdout.contains("Success!"), "{stdout}");
    assert!(stdout.contains("hub_rim"), "{stdout}");
}

#[test]
fn help_lists_every_command() {
    let output = artificer(&["help"]);
    assert!(output.status.success());
    let stdout = stdout(&output);
    for command in [
        "serve", "run", "report", "params", "snapshot", "export", "journal", "diff", "help",
    ] {
        assert!(
            stdout
                .lines()
                .any(|line| line.trim_start().starts_with(command)),
            "`{command}` is missing from the help:\n{stdout}"
        );
    }
    for option in ["--param", "--view", "--json", "--faceted", "--module-path"] {
        assert!(stdout.contains(option), "`{option}` is missing:\n{stdout}");
    }
}

#[test]
fn a_reader_that_stops_early_ends_the_report_quietly() {
    let mut child = Command::new(BIN)
        .args(["report", &example("flanged_hub.art")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the command runs");
    let mut pipe = child.stdout.take().unwrap();
    let mut first = [0u8; 16];
    pipe.read_exact(&mut first).unwrap();
    // Closing the pipe is what `| head` does once it has its lines.
    drop(pipe);
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert!(!stderr.contains("Broken pipe"), "{stderr}");
}
