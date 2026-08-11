use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn canonical_case() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/cases/m0-cuboid.json")
}

fn transform_case() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/cases/m1-transform-similarity.json")
}

fn extrusion_case() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/cases/m4-extrude-rectangle.json")
}

fn face_chain_case() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/cases/m4-face-chain.json")
}

fn artifact(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("artificer-cli-{}-{name}", std::process::id()))
}

#[test]
fn committed_transform_case_runs_and_replays_through_the_cli() {
    let binary = env!("CARGO_BIN_EXE_artificer-cli");
    let journal = artifact("transform-journal.json");
    let run = Command::new(binary)
        .args([
            "run",
            transform_case().to_str().unwrap(),
            "--journal",
            journal.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(String::from_utf8_lossy(&run.stdout).contains("PASS m1.transform-similarity"));

    let replay = Command::new(binary)
        .args(["replay", journal.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert!(String::from_utf8_lossy(&replay.stdout).contains("REPLAY PASS"));
    let _ = fs::remove_file(journal);
}

#[test]
fn native_extrusion_case_runs_and_replays_through_the_cli() {
    let binary = env!("CARGO_BIN_EXE_artificer-cli");
    let journal = artifact("extrusion-journal.json");
    let run = Command::new(binary)
        .args([
            "run",
            extrusion_case().to_str().unwrap(),
            "--journal",
            journal.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(String::from_utf8_lossy(&run.stdout).contains("PASS m4.extrude-rectangle"));

    let replay = Command::new(binary)
        .args(["replay", journal.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert!(String::from_utf8_lossy(&replay.stdout).contains("REPLAY PASS"));
    let _ = fs::remove_file(journal);
}

#[test]
fn repeated_face_feature_case_runs_and_replays_through_the_cli() {
    let binary = env!("CARGO_BIN_EXE_artificer-cli");
    let journal = artifact("face-chain-journal.json");
    let run = Command::new(binary)
        .args([
            "run",
            face_chain_case().to_str().unwrap(),
            "--journal",
            journal.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(String::from_utf8_lossy(&run.stdout).contains("PASS m4.face-chain"));

    let replay = Command::new(binary)
        .args(["replay", journal.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert!(String::from_utf8_lossy(&replay.stdout).contains("REPLAY PASS m4.face-chain"));
    let _ = fs::remove_file(journal);
}

#[test]
fn run_replay_and_svg_use_the_same_native_case() {
    let binary = env!("CARGO_BIN_EXE_artificer-cli");
    let journal = artifact("journal.json");
    let svg = artifact("scene.svg");

    let run = Command::new(binary)
        .args([
            "run",
            canonical_case().to_str().unwrap(),
            "--journal",
            journal.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(String::from_utf8_lossy(&run.stdout).contains("PASS m0.cuboid-2x3x4"));

    let replay = Command::new(binary)
        .args(["replay", journal.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert!(String::from_utf8_lossy(&replay.stdout).contains("REPLAY PASS"));

    let scene = Command::new(binary)
        .args([
            "scene-svg",
            canonical_case().to_str().unwrap(),
            svg.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        scene.status.success(),
        "{}",
        String::from_utf8_lossy(&scene.stderr)
    );
    let rendered = fs::read_to_string(&svg).unwrap();
    assert_eq!(rendered.matches("<polygon ").count(), 12);
    assert_eq!(rendered.matches("<line ").count(), 12);

    let _ = fs::remove_file(journal);
    let _ = fs::remove_file(svg);
}

#[test]
fn repeat_command_proves_identical_journals() {
    let output = Command::new(env!("CARGO_BIN_EXE_artificer-cli"))
        .args([
            "repeat",
            canonical_case().to_str().unwrap(),
            "--count",
            "100",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("100 runs"));
}

#[test]
fn mismatch_writes_a_portable_failure_bundle() {
    let binary = env!("CARGO_BIN_EXE_artificer-cli");
    let source = fs::read_to_string(canonical_case()).unwrap();
    let malformed = source.replacen("\"vertices\": 8", "\"vertices\": 9", 1);
    let case = artifact("mismatch-case.json");
    let bundle = artifact("failure-bundle");
    fs::write(&case, malformed).unwrap();
    let output = Command::new(binary)
        .args([
            "run",
            case.to_str().unwrap(),
            "--failure-bundle",
            bundle.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    for name in ["manifest.json", "journal.json", "scene.svg", "case.json"] {
        assert!(bundle.join(name).is_file(), "missing {name}");
    }
    let _ = fs::remove_file(case);
    let _ = fs::remove_dir_all(bundle);
}
