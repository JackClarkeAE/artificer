//! `artificer-scan`: scan-to-CAD pipeline driver.
//!
//! Subcommands:
//! - `info <mesh>`                     mesh statistics
//! - `align <source> <target>`         best-fit (ICP) alignment
//! - `reverse <mesh>`                  segment, fit, canonicalize, report
//! - `demo`                            emit a synthetic test scan

use std::path::Path;
use std::process::ExitCode;

use artificer_scan_core::report::{report_summary, report_to_json};
use artificer_scan_core::snap::SnapPolicy;
use artificer_scan_core::stl::write_binary_stl;
use artificer_scan_core::{
    IcpParams, ReverseOptions, TriangleMesh, best_fit_align, reverse_engineer, synth,
};

mod snapshot;
mod viewer;

fn load_mesh(path: &str) -> Result<TriangleMesh, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let extension = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("stl") => artificer_scan_core::stl::read_stl(&bytes).map_err(|e| e.to_string()),
        Some("ply") => artificer_scan_core::ply::read_ply(&bytes).map_err(|e| e.to_string()),
        Some("obj") => artificer_scan_core::obj::read_obj(&bytes).map_err(|e| e.to_string()),
        _ => Err(format!(
            "unsupported mesh format for {path} (expected .stl, .ply, or .obj)"
        )),
    }
}

fn usage() -> String {
    "usage:\n\
     artificer-scan info <mesh.stl|mesh.ply>\n\
     artificer-scan align <source> <target> [--out aligned.stl]\n\
     artificer-scan reverse <mesh> [--tolerance MM] [--max-dihedral DEG] [--min-faces N]\n\
                            [--no-ransac] [--min-support N] [--ransac-epsilon MM]\n\
                            [--no-merge] [--min-feature MM2] [--no-datum] [--no-snap] [--json out.json]\n\
                            [--aligned-out mesh.stl] [--history plan.json] [--profile-out master.png]\n\
                            [--labels labels.bin]\n\
     artificer-scan view <mesh> [reverse options] [--out viewer.html]\n\
     artificer-scan snapshot <mesh> [reverse options] [--top] [--out snapshot.png]\n\
     artificer-scan rebuild <mesh> [reverse options] [--out model.stl] [--snapshot cmp.png]\n\
     artificer-scan demo [--out scan.stl]"
        .to_owned()
}

fn take_flag_value(args: &mut Vec<String>, flag: &str) -> Option<String> {
    let position = args.iter().position(|a| a == flag)?;
    if position + 1 >= args.len() {
        return None;
    }
    let value = args.remove(position + 1);
    args.remove(position);
    Some(value)
}

fn take_flag(args: &mut Vec<String>, flag: &str) -> bool {
    if let Some(position) = args.iter().position(|a| a == flag) {
        args.remove(position);
        true
    } else {
        false
    }
}

fn parse_reverse_options(args: &mut Vec<String>) -> Result<ReverseOptions, String> {
    let mut options = ReverseOptions::default();
    if let Some(value) = take_flag_value(args, "--tolerance") {
        options.tolerance = value
            .parse::<f64>()
            .map_err(|_| format!("bad tolerance {value}"))?;
    }
    if let Some(value) = take_flag_value(args, "--max-dihedral") {
        options.segmentation.max_dihedral_deg = value
            .parse::<f64>()
            .map_err(|_| format!("bad dihedral {value}"))?;
    }
    if let Some(value) = take_flag_value(args, "--min-faces") {
        options.segmentation.min_region_faces = value
            .parse::<usize>()
            .map_err(|_| format!("bad face count {value}"))?;
    }
    if take_flag(args, "--no-ransac") {
        options.ransac = None;
    }
    if let Some(value) = take_flag_value(args, "--min-support") {
        let count = value
            .parse::<usize>()
            .map_err(|_| format!("bad support count {value}"))?;
        if let Some(ransac) = &mut options.ransac {
            ransac.min_support_faces = count;
        }
    }
    if let Some(value) = take_flag_value(args, "--ransac-epsilon") {
        let epsilon = value
            .parse::<f64>()
            .map_err(|_| format!("bad epsilon {value}"))?;
        if let Some(ransac) = &mut options.ransac {
            ransac.epsilon = epsilon;
        }
    }
    if take_flag(args, "--no-merge") {
        options.merge_fragments = false;
    }
    if let Some(value) = take_flag_value(args, "--min-feature") {
        options.min_feature_area = value
            .parse::<f64>()
            .map_err(|_| format!("bad feature area {value}"))?;
    }
    if take_flag(args, "--no-datum") {
        options.auto_datum = false;
    }
    options.snap = if take_flag(args, "--no-snap") {
        None
    } else {
        Some(SnapPolicy::default())
    };
    Ok(options)
}

fn run() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let command = if args.is_empty() {
        return Err(usage());
    } else {
        args.remove(0)
    };
    match command.as_str() {
        "info" => {
            let path = args.first().ok_or_else(usage)?;
            let mesh = load_mesh(path)?;
            println!("vertices:  {}", mesh.positions().len());
            println!("triangles: {}", mesh.triangles().len());
            println!("area:      {:.3} mm^2", mesh.surface_area());
            if let Some(bounds) = mesh.bounds() {
                println!(
                    "bounds:    ({:.3} {:.3} {:.3}) to ({:.3} {:.3} {:.3})",
                    bounds.min.x, bounds.min.y, bounds.min.z,
                    bounds.max.x, bounds.max.y, bounds.max.z
                );
                println!("diagonal:  {:.3} mm", mesh.bounds_diagonal());
            }
            Ok(())
        }
        "align" => {
            let out = take_flag_value(&mut args, "--out");
            let [source_path, target_path] = args.as_slice() else {
                return Err(usage());
            };
            let source = load_mesh(source_path)?;
            let target = load_mesh(target_path)?;
            let result = best_fit_align(&source, &target, IcpParams::default())
                .ok_or("alignment failed: not enough valid correspondences")?;
            println!(
                "converged in {} iteration(s), rms {:.4} mm, {:.0}% inliers",
                result.iterations,
                result.rms,
                result.inlier_fraction * 100.0
            );
            let r = result.transform.rotation;
            let t = result.transform.translation;
            println!("rotation:    [{:+.6} {:+.6} {:+.6}]", r[0][0], r[0][1], r[0][2]);
            println!("             [{:+.6} {:+.6} {:+.6}]", r[1][0], r[1][1], r[1][2]);
            println!("             [{:+.6} {:+.6} {:+.6}]", r[2][0], r[2][1], r[2][2]);
            println!("translation: ({:+.4} {:+.4} {:+.4})", t.x, t.y, t.z);
            if let Some(out) = out {
                let aligned = source.transformed(&result.transform);
                std::fs::write(&out, write_binary_stl(&aligned))
                    .map_err(|e| format!("cannot write {out}: {e}"))?;
                println!("aligned mesh written to {out}");
            }
            Ok(())
        }
        "reverse" => {
            let options = parse_reverse_options(&mut args)?;
            let json_path = take_flag_value(&mut args, "--json");
            let aligned_path = take_flag_value(&mut args, "--aligned-out");
            let history_path = take_flag_value(&mut args, "--history");
            let profile_path = take_flag_value(&mut args, "--profile-out");
            let labels_path = take_flag_value(&mut args, "--labels");
            let path = args.first().ok_or_else(usage)?;
            let mesh = load_mesh(path)?;
            let report = reverse_engineer(&mesh, &options);
            // Write the deliverables before printing: a truncated stdout
            // pipe must not lose them.
            if let Some(json_path) = &json_path {
                std::fs::write(json_path, report_to_json(&report))
                    .map_err(|e| format!("cannot write {json_path}: {e}"))?;
            }
            if let Some(aligned_path) = &aligned_path {
                let aligned = match &report.datum {
                    Some(alignment) => mesh.transformed(&alignment.transform),
                    None => mesh.clone(),
                };
                std::fs::write(aligned_path, write_binary_stl(&aligned))
                    .map_err(|e| format!("cannot write {aligned_path}: {e}"))?;
            }
            if let Some(history_path) = &history_path {
                let plan = report
                    .plan
                    .as_ref()
                    .ok_or("no reconstruction plan (datum alignment did not run)")?;
                std::fs::write(history_path, artificer_scan_core::plan_to_history_json(plan))
                    .map_err(|e| format!("cannot write {history_path}: {e}"))?;
            }
            if let Some(profile_path) = &profile_path {
                let profile = report
                    .plan
                    .as_ref()
                    .and_then(|plan| plan.master_profile.as_ref())
                    .ok_or("no master profile (no pattern feature was accepted)")?;
                let png = snapshot::render_profile_plot(profile, 1400, 700);
                std::fs::write(profile_path, png)
                    .map_err(|e| format!("cannot write {profile_path}: {e}"))?;
            }
            if let Some(labels_path) = &labels_path {
                // Face-ownership map: little-endian u32 feature id per
                // triangle, in mesh face order; u32::MAX = unowned.
                let mut labels = vec![u32::MAX; mesh.triangles().len()];
                for feature in &report.features {
                    for &face in &feature.faces {
                        labels[face as usize] = feature.id as u32;
                    }
                }
                let mut bytes = Vec::with_capacity(labels.len() * 4);
                for label in labels {
                    bytes.extend_from_slice(&label.to_le_bytes());
                }
                std::fs::write(labels_path, bytes)
                    .map_err(|e| format!("cannot write {labels_path}: {e}"))?;
            }
            print!("{}", report_summary(&report));
            if let Some(json_path) = json_path {
                println!("report written to {json_path}");
            }
            if let Some(aligned_path) = aligned_path {
                println!("datum-aligned mesh written to {aligned_path}");
            }
            if let Some(history_path) = history_path {
                println!("history proposal written to {history_path}");
            }
            if let Some(profile_path) = profile_path {
                println!("master profile plot written to {profile_path}");
            }
            if let Some(labels_path) = labels_path {
                println!("face labels written to {labels_path}");
            }
            Ok(())
        }
        "view" => {
            let options = parse_reverse_options(&mut args)?;
            let out = take_flag_value(&mut args, "--out").unwrap_or_else(|| "viewer.html".to_owned());
            let path = args.first().ok_or_else(usage)?;
            let mesh = load_mesh(path)?;
            let report = reverse_engineer(&mesh, &options);
            let name = Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("scan");
            let html = viewer::build_viewer_html(&mesh, &report, name);
            std::fs::write(&out, html).map_err(|e| format!("cannot write {out}: {e}"))?;
            println!("viewer written to {out}");
            Ok(())
        }
        "snapshot" => {
            let options = parse_reverse_options(&mut args)?;
            let top = take_flag(&mut args, "--top");
            let out =
                take_flag_value(&mut args, "--out").unwrap_or_else(|| "snapshot.png".to_owned());
            let path = args.first().ok_or_else(usage)?;
            let mesh = load_mesh(path)?;
            let report = reverse_engineer(&mesh, &options);
            let model = viewer::display_model(&mesh, &report);
            let camera = if top {
                snapshot::Camera::TOP
            } else {
                snapshot::Camera::default()
            };
            let png = snapshot::render_side_by_side(&model, &camera, 1800, 760);
            std::fs::write(&out, png).map_err(|e| format!("cannot write {out}: {e}"))?;
            println!("snapshot written to {out}");
            Ok(())
        }
        "rebuild" => {
            let options = parse_reverse_options(&mut args)?;
            let out = take_flag_value(&mut args, "--out").unwrap_or_else(|| "rebuilt.stl".to_owned());
            let snapshot_path = take_flag_value(&mut args, "--snapshot");
            let path = args.first().ok_or_else(usage)?;
            let mesh = load_mesh(path)?;
            let report = reverse_engineer(&mesh, &options);
            let rebuilt = artificer_scan_core::rebuild_sharp(&mesh, &report)
                .ok_or("rebuild needs a datum frame (auto-datum found none)")?;
            std::fs::write(&out, write_binary_stl(&rebuilt.mesh))
                .map_err(|e| format!("cannot write {out}: {e}"))?;
            println!(
                "sharp rebuild written to {out} ({} triangles)",
                rebuilt.mesh.triangles().len()
            );
            if let Some(snapshot_path) = snapshot_path {
                let alignment = report.datum.as_ref().expect("datum present after rebuild");
                let aligned_scan = mesh.transformed(&alignment.transform);
                let (display_scan, _) = if aligned_scan.triangles().len() > 160_000 {
                    let mut cell = aligned_scan.bounds_diagonal() / 260.0;
                    let mut result = aligned_scan.simplified_by_clustering(cell);
                    while result.0.triangles().len() > 160_000 {
                        cell *= 1.35;
                        result = aligned_scan.simplified_by_clustering(cell);
                    }
                    result
                } else {
                    (aligned_scan.clone(), Vec::new())
                };
                let colors: Vec<[u8; 3]> = rebuilt
                    .feature_of_face
                    .iter()
                    .map(|&id| viewer::feature_color(id + 1))
                    .collect();
                let png = snapshot::render_comparison(
                    &display_scan,
                    &rebuilt.mesh,
                    &colors,
                    &snapshot::Camera::default(),
                    1800,
                    760,
                );
                std::fs::write(&snapshot_path, png)
                    .map_err(|e| format!("cannot write {snapshot_path}: {e}"))?;
                println!("comparison written to {snapshot_path}");
            }
            Ok(())
        }
        "demo" => {
            let out = take_flag_value(&mut args, "--out").unwrap_or_else(|| "scan.stl".to_owned());
            let mesh = synth::plate_with_boss();
            std::fs::write(&out, write_binary_stl(&mesh))
                .map_err(|e| format!("cannot write {out}: {e}"))?;
            println!(
                "synthetic scan written to {out} ({} triangles)",
                mesh.triangles().len()
            );
            Ok(())
        }
        _ => Err(usage()),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}
