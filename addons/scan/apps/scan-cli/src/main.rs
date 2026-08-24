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

mod section;
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
        Some("step" | "stp") => artificer_scan_core::step::read_step(&bytes, 0.03)
            .map(|(mesh, notes)| {
                for note in notes {
                    println!("  {note}");
                }
                mesh
            })
            .map_err(|e| e.to_string()),
        _ => Err(format!(
            "unsupported mesh format for {path} (expected .stl, .ply, .obj, or .step)"
        )),
    }
}

fn usage() -> String {
    "usage:\n\
     artificer-scan info <mesh.stl|mesh.ply> [--health]\n\
     artificer-scan align <source> <target> [--out aligned.stl]\n\
     artificer-scan reverse <mesh> [--tolerance MM] [--max-dihedral DEG] [--min-faces N]\n\
                            [--no-ransac] [--min-support N] [--ransac-epsilon MM]\n\
     [--no-consolidate]\n\
                            [--no-merge] [--min-feature MM2] [--no-datum] [--datum-candidate N]\n\
                            [--no-snap] [--snap-max MM] [--json out.json]\n\
                            [--aligned-out mesh.stl] [--history plan.json] [--profile-out master.png]\n\
                            [--labels labels.bin]\n\
     artificer-scan view <mesh> [reverse options] [--out viewer.html]\n\
     artificer-scan snapshot <mesh> [reverse options] [--top] [--out snapshot.png]\n\
     artificer-scan rebuild <mesh> [reverse options] [--out model.stl] [--snapshot cmp.png]\n\
                            [--edges edges.obj] [--sew-triage triage.png]\n\
     artificer-scan sections <mesh> [reverse options] [--meridians N] [--levels N]\n\
                             [--panel PX] [--gap MM] [--fixed-scale]\n                             [--z-from MM] [--z-to MM] [--z-step MM] [--out sections.png]\n\
     artificer-scan simulate <mesh> [--density MM] [--smooth MM] [--noise MM]\n\
                             [--dropout N] [--dropout-size MM] [--seed N]\n\
                             [--out scan.stl] [--snapshot cmp.png]\n\
     artificer-scan bench [--manifest bench/fixtures.txt] [--only NAME]\n\
     \x20                    [--baseline file] [--write-baseline file]\n\
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
    // Consolidation is where a large scan spends nearly all its time —
    // 3185 s of a 3369 s rail run — so being able to leave it out is
    // what makes an 8-million-face part answerable at all while a
    // question about fitting, rather than about consolidation, is
    // being asked.
    if take_flag(args, "--no-consolidate") {
        options.consolidate = false;
    }
    if let Some(value) = take_flag_value(args, "--min-feature") {
        options.min_feature_area = value
            .parse::<f64>()
            .map_err(|_| format!("bad feature area {value}"))?;
    }
    if take_flag(args, "--no-datum") {
        options.auto_datum = false;
    }
    if let Some(value) = take_flag_value(args, "--datum-candidate") {
        options.datum_choice = Some(
            value
                .parse::<usize>()
                .map_err(|_| format!("bad datum candidate {value}"))?,
        );
    }
    if take_flag(args, "--no-adaptive-tolerance") {
        options.adaptive_tolerance = false;
    }
    options.snap = if take_flag(args, "--no-snap") {
        None
    } else {
        let mut policy = SnapPolicy::default();
        // How far a canonicalization may move a measured dimension is a
        // judgement about the part, not about the algorithm: a casting
        // tolerates more than a ground bore. Default it, expose it.
        if let Some(value) = take_flag_value(args, "--snap-max") {
            policy.length_tolerance = value
                .parse::<f64>()
                .map_err(|_| format!("bad snap limit {value}"))?;
        }
        Some(policy)
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
            let health = take_flag(&mut args, "--health");
            let path = args.first().ok_or_else(usage)?;
            let mesh = load_mesh(path)?;
            if health {
                // Measure before repairing: a scan with no holes needs no
                // hole filling, and filling anyway invents material.
                println!(
                    "{}",
                    artificer_scan_core::hygiene::inspect(&mesh).describe()
                );
            }
            println!("vertices:  {}", mesh.positions().len());
            println!("triangles: {}", mesh.triangles().len());
            println!("area:      {:.3} mm^2", mesh.surface_area());
            if let Some(bounds) = mesh.bounds() {
                println!(
                    "bounds:    ({:.3} {:.3} {:.3}) to ({:.3} {:.3} {:.3})",
                    bounds.min.x,
                    bounds.min.y,
                    bounds.min.z,
                    bounds.max.x,
                    bounds.max.y,
                    bounds.max.z
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
            println!(
                "rotation:    [{:+.6} {:+.6} {:+.6}]",
                r[0][0], r[0][1], r[0][2]
            );
            println!(
                "             [{:+.6} {:+.6} {:+.6}]",
                r[1][0], r[1][1], r[1][2]
            );
            println!(
                "             [{:+.6} {:+.6} {:+.6}]",
                r[2][0], r[2][1], r[2][2]
            );
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
            println!(
                "estimated scan noise sigma {:.3} mm (windows x{:.2}, effective tolerance {:.3})",
                report.noise_sigma,
                (10.0 * report.noise_sigma.sqrt()).clamp(1.0, 3.0),
                report.tolerance
            );
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
                std::fs::write(
                    history_path,
                    artificer_scan_core::plan_to_history_json(plan),
                )
                .map_err(|e| format!("cannot write {history_path}: {e}"))?;
            }
            if let Some(profile_path) = &profile_path {
                let profile = report
                    .plan
                    .as_ref()
                    .and_then(|plan| plan.master_profiles.first())
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
            if let Some(alignment) = report.datum.as_ref() {
                let motions = artificer_scan_core::kinematic::survey(
                    &mesh,
                    &report.features,
                    alignment,
                    options.min_feature_area.max(50.0),
                );
                if !motions.is_empty() {
                    println!("motions that sweep this part:");
                    for line in motions {
                        println!("{line}");
                    }
                }
            }
            if let Some(labels_path) = labels_path {
                println!("face labels written to {labels_path}");
            }
            Ok(())
        }
        "view" => {
            let options = parse_reverse_options(&mut args)?;
            let out =
                take_flag_value(&mut args, "--out").unwrap_or_else(|| "viewer.html".to_owned());
            let path = args.first().ok_or_else(usage)?;
            let mesh = load_mesh(path)?;
            let report = reverse_engineer(&mesh, &options);
            println!(
                "estimated scan noise sigma {:.3} mm (windows x{:.2}, effective tolerance {:.3})",
                report.noise_sigma,
                (10.0 * report.noise_sigma.sqrt()).clamp(1.0, 3.0),
                report.tolerance
            );
            for note in &report.demotions {
                println!("  demoted: {note}");
            }
            let rebuilt = artificer_scan_core::rebuild_sharp(&mesh, &report);
            let name = Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("scan");
            let html = viewer::build_viewer_html(&mesh, &report, rebuilt.as_ref(), name);
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
            println!(
                "estimated scan noise sigma {:.3} mm (windows x{:.2}, effective tolerance {:.3})",
                report.noise_sigma,
                (10.0 * report.noise_sigma.sqrt()).clamp(1.0, 3.0),
                report.tolerance
            );
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
            let out =
                take_flag_value(&mut args, "--out").unwrap_or_else(|| "rebuilt.stl".to_owned());
            let snapshot_path = take_flag_value(&mut args, "--snapshot");
            let triage_path = take_flag_value(&mut args, "--sew-triage");
            let edges_out = take_flag_value(&mut args, "--edges");
            let path = args.first().ok_or_else(usage)?;
            let mesh = load_mesh(path)?;
            let report = reverse_engineer(&mesh, &options);
            println!(
                "estimated scan noise sigma {:.3} mm (windows x{:.2}, effective tolerance {:.3})",
                report.noise_sigma,
                (10.0 * report.noise_sigma.sqrt()).clamp(1.0, 3.0),
                report.tolerance
            );
            for note in &report.demotions {
                println!("  demoted: {note}");
            }
            let rebuilt = artificer_scan_core::rebuild_sharp(&mesh, &report)
                .ok_or("rebuild needs a datum frame (auto-datum found none)")?;
            std::fs::write(&out, write_binary_stl(&rebuilt.mesh))
                .map_err(|e| format!("cannot write {out}: {e}"))?;
            println!(
                "sharp rebuild written to {out} ({} triangles)",
                rebuilt.mesh.triangles().len()
            );
            if !rebuilt.edges.is_empty() {
                // The edges are the topology, so they are worth looking
                // at on their own rather than only being counted.
                if let Some(path) = &edges_out {
                    let mut obj = String::from("# artificer-scan shared edges\n");
                    let mut base = 1usize;
                    for edge in &rebuilt.edges {
                        for point in &edge.points {
                            obj.push_str(&format!("v {} {} {}\n", point.x, point.y, point.z));
                        }
                        obj.push_str(&format!("o edge_{}_{}\n", edge.between.0, edge.between.1));
                        obj.push('l');
                        for offset in 0..edge.points.len() {
                            obj.push_str(&format!(" {}", base + offset));
                        }
                        obj.push('\n');
                        base += edge.points.len();
                    }
                    obj.push_str("o corners\n");
                    for corner in &rebuilt.corners {
                        obj.push_str(&format!(
                            "v {} {} {}\n",
                            corner.at.x, corner.at.y, corner.at.z
                        ));
                    }
                    for offset in 0..rebuilt.corners.len() {
                        obj.push_str(&format!("p {}\n", base + offset));
                    }
                    std::fs::write(path, obj).map_err(|e| format!("cannot write {path}: {e}"))?;
                    println!("  shared edges written to {path}");
                }
                let total: f64 = rebuilt.edges.iter().map(|edge| edge.length()).sum();
                let pairs: std::collections::HashSet<(usize, usize)> =
                    rebuilt.edges.iter().map(|edge| edge.between).collect();
                println!(
                    "  {} shared edge(s) between {} face pairs, {:.0} mm of curve; {} exact corner(s)",
                    rebuilt.edges.len(),
                    pairs.len(),
                    total,
                    rebuilt.corners.len()
                );
            }
            // The honest headline. The report's "classified" figure counts
            // a face as a success once it belongs to any feature, which
            // stays high even when those features are buckets nothing can
            // emit; this is the share of the scan the model actually draws.
            //
            // Measured at the tolerance the run actually used, not at
            // the one the operator typed: on a noisy scan the adaptive
            // floor lifts the working tolerance, and scoring a smooth
            // surface against the original band asks it to reproduce
            // the noise. Only a photocopy of the crumple can pass that,
            // so the honest model scored *worse* than the dishonest one
            // — the measurement was wrong, not the geometry.
            if let Some(alignment) = report.datum.as_ref() {
                let (explained, total) = artificer_scan_core::coverage::explained_area(
                    &mesh,
                    &rebuilt.mesh,
                    alignment,
                    report.tolerance,
                );
                let (invented, emitted) = artificer_scan_core::coverage::invented_area(
                    &mesh,
                    &rebuilt.mesh,
                    alignment,
                    report.tolerance,
                );
                println!(
                    "rebuild explains {:.1}% of the scanned surface ({:.0} of {:.0} mm^2 within {:.3} mm)",
                    100.0 * explained / total.max(1e-9),
                    explained,
                    total,
                    report.tolerance
                );
                println!(
                    "  and invents {:.1}% of its own surface ({:.0} of {:.0} mm^2 lies nowhere near the scan)",
                    100.0 * invented / emitted.max(1e-9),
                    invented,
                    emitted
                );
                // Not all of that model is the same kind of thing. What
                // sits on a fitted surface is certifiable; what is cast
                // or organic is carried as measured surface, and a
                // coverage figure that pools the two flatters itself.
                // Measure the analytic part on its own.
                let certified: Vec<[artificer_geometry::Point3; 3]> = rebuilt
                    .mesh
                    .triangles()
                    .iter()
                    .enumerate()
                    .filter(|(face, _)| {
                        report
                            .features
                            .iter()
                            .find(|f| f.id == rebuilt.feature_of_face[*face])
                            .is_some_and(|f| {
                                !matches!(f.surface, artificer_scan_core::SurfaceClass::Freeform)
                            })
                    })
                    .map(|(face, _)| rebuilt.mesh.triangle_points(face))
                    .collect();
                if certified.len() < rebuilt.mesh.triangles().len()
                    && let Some(analytic) =
                        artificer_scan_core::mesh::TriangleMesh::from_triangle_soup(
                            &certified, 1e-6,
                        )
                {
                    {
                        let (exact, _) = artificer_scan_core::coverage::explained_area(
                            &mesh,
                            &analytic,
                            alignment,
                            report.tolerance,
                        );
                        println!(
                            "  of which analytic surfaces explain {:.1}% ({:.0} mm^2); the rest is \
                             measured surface with no analytic form",
                            100.0 * exact / total.max(1e-9),
                            exact
                        );
                    }
                }
                // Attribute the invention, so it is a work list rather
                // than a number to feel bad about.
                if invented > 0.0 {
                    let flags = artificer_scan_core::coverage::invented_flags(
                        &mesh,
                        &rebuilt.mesh,
                        alignment,
                        report.tolerance,
                    );
                    let mut by_feature: std::collections::HashMap<usize, f64> =
                        std::collections::HashMap::new();
                    for (face, &bad) in flags.iter().enumerate() {
                        if bad {
                            *by_feature.entry(rebuilt.feature_of_face[face]).or_default() +=
                                rebuilt.mesh.face_area(face);
                        }
                    }
                    // Where it sits, not just how much of it there is.
                    for patch in artificer_scan_core::coverage::invented_patches(
                        &mesh,
                        &rebuilt.mesh,
                        alignment,
                        report.tolerance,
                        &rebuilt.feature_of_face,
                    )
                    .iter()
                    .take(6)
                    {
                        let label = report
                            .features
                            .iter()
                            .find(|f| f.id == patch.feature)
                            .map_or_else(
                                || format!("#{}", patch.feature),
                                |f| {
                                    format!(
                                        "#{} {}",
                                        f.id,
                                        artificer_scan_core::finalize::feature_label(&f.surface)
                                    )
                                },
                            );
                        println!("     {}", patch.describe(&label));
                    }
                    let mut worst: Vec<(usize, f64)> = by_feature.into_iter().collect();
                    worst.sort_by(|a, b| b.1.total_cmp(&a.1));
                    for (id, area) in worst.iter().take(5) {
                        let label = report
                            .features
                            .iter()
                            .find(|f| f.id == *id)
                            .map(|f| artificer_scan_core::finalize::feature_label(&f.surface))
                            .unwrap_or_else(|| "unknown".to_owned());
                        println!("     #{id} {label}: {area:.0} mm^2 invented");
                    }
                }
            }
            for note in &rebuilt.notes {
                println!("  note: {note}");
            }
            for note in &rebuilt.skipped {
                println!("  skipped: {note}");
            }
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
                // A close-up from a lower angle for edge-level inspection.
                let close = snapshot::Camera {
                    theta: 0.6,
                    phi: 1.0,
                    radius_scale: 0.62,
                };
                let close_png = snapshot::render_comparison(
                    &display_scan,
                    &rebuilt.mesh,
                    &colors,
                    &close,
                    1800,
                    760,
                );
                let close_path = snapshot_path.replace(".png", "_close.png");
                std::fs::write(&close_path, close_png)
                    .map_err(|e| format!("cannot write {close_path}: {e}"))?;
                println!("close-up written to {close_path}");
            }
            if let Some(triage_path) = triage_path {
                // The watertightness work list, drawn: the rebuild dimmed
                // to a backdrop and every open edge end marked by cause.
                // Left pane carries the recovered topology (edges as thin
                // ribbons) so the marks have curves to sit on.
                let alignment = report.datum.as_ref().expect("datum present after rebuild");
                use artificer_scan_core::sew::OpenCause;
                let cause_color = |cause: OpenCause| -> [u8; 3] {
                    match cause {
                        OpenCause::TwoFaces => [255, 140, 0],
                        OpenCause::Tangent => [0, 200, 255],
                        OpenCause::Singular => [255, 0, 200],
                        OpenCause::Runaway => [255, 40, 40],
                        OpenCause::MissingSurface => [255, 220, 0],
                    }
                };
                let diagonal = rebuilt.mesh.bounds_diagonal().max(1.0);
                let mut positions: Vec<artificer_geometry::Point3> =
                    rebuilt.mesh.positions().to_vec();
                let mut triangles: Vec<[u32; 3]> = rebuilt.mesh.triangles().to_vec();
                // The backdrop stays gray so the marks carry the color.
                let mut colors: Vec<[u8; 3]> = vec![[104, 110, 118]; triangles.len()];
                let radius = diagonal / 240.0;
                for open in &rebuilt.open_ends {
                    let base = positions.len() as u32;
                    let c = open.at;
                    positions.extend([
                        artificer_geometry::Point3::new(c.x - radius, c.y, c.z),
                        artificer_geometry::Point3::new(c.x + radius, c.y, c.z),
                        artificer_geometry::Point3::new(c.x, c.y - radius, c.z),
                        artificer_geometry::Point3::new(c.x, c.y + radius, c.z),
                        artificer_geometry::Point3::new(c.x, c.y, c.z - radius),
                        artificer_geometry::Point3::new(c.x, c.y, c.z + radius),
                    ]);
                    // The octahedron's eight faces.
                    for tri in [
                        [0u32, 2, 4],
                        [2, 1, 4],
                        [1, 3, 4],
                        [3, 0, 4],
                        [2, 0, 5],
                        [1, 2, 5],
                        [3, 1, 5],
                        [0, 3, 5],
                    ] {
                        triangles.push([base + tri[0], base + tri[1], base + tri[2]]);
                        colors.push(cause_color(open.cause));
                    }
                }
                let marked = artificer_scan_core::TriangleMesh::new(positions, triangles)
                    .ok_or("triage mesh construction failed")?;
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
                let png = snapshot::render_comparison(
                    &display_scan,
                    &marked,
                    &colors,
                    &snapshot::Camera::default(),
                    1800,
                    760,
                );
                std::fs::write(&triage_path, png)
                    .map_err(|e| format!("cannot write {triage_path}: {e}"))?;
                println!("sew triage written to {triage_path}");
                println!(
                    "  legend: orange two-face end, cyan tangent boundary, magenta singular triple,                      red runaway root, yellow missing carrier"
                );
                // The same triage as text: the first few of each cause,
                // located, because reading numbers beats reading pixels.
                let mut by_cause: Vec<(OpenCause, Vec<&artificer_scan_core::sew::OpenEnd>)> =
                    Vec::new();
                for open in &rebuilt.open_ends {
                    match by_cause.iter_mut().find(|(cause, _)| *cause == open.cause) {
                        Some((_, list)) => list.push(open),
                        None => by_cause.push((open.cause, vec![open])),
                    }
                }
                by_cause.sort_by_key(|(cause, list)| (std::cmp::Reverse(list.len()), *cause));
                for (cause, list) in &by_cause {
                    println!("  {} x {}", list.len(), cause.describe());
                    for open in list.iter().take(4) {
                        println!(
                            "      at ({:+.2} {:+.2} {:+.2}), edge {}",
                            open.at.x, open.at.y, open.at.z, open.edge
                        );
                    }
                    if list.len() > 4 {
                        println!("      ... and {} more", list.len() - 4);
                    }
                }
            }
            Ok(())
        }
        "sections" => {
            let options = parse_reverse_options(&mut args)?;
            let out =
                take_flag_value(&mut args, "--out").unwrap_or_else(|| "sections.png".to_owned());
            let meridians = take_flag_value(&mut args, "--meridians")
                .map(|v| v.parse::<usize>())
                .transpose()
                .map_err(|_| "--meridians expects a count")?
                .unwrap_or(2);
            let levels = take_flag_value(&mut args, "--levels")
                .map(|v| v.parse::<usize>())
                .transpose()
                .map_err(|_| "--levels expects a count")?
                .unwrap_or(3);
            let panel = take_flag_value(&mut args, "--panel")
                .map(|v| v.parse::<usize>())
                .transpose()
                .map_err(|_| "--panel expects a pixel size")?
                .unwrap_or(520);
            let reach = take_flag_value(&mut args, "--gap")
                .map(|v| v.parse::<f64>())
                .transpose()
                .map_err(|_| "--gap expects millimetres")?
                .unwrap_or(0.6);
            let fixed_scale = take_flag(&mut args, "--fixed-scale");
            let path = args.first().ok_or_else(usage)?;
            let mesh = load_mesh(path)?;
            let report = reverse_engineer(&mesh, &options);
            println!(
                "estimated scan noise sigma {:.3} mm (windows x{:.2}, effective tolerance {:.3})",
                report.noise_sigma,
                (10.0 * report.noise_sigma.sqrt()).clamp(1.0, 3.0),
                report.tolerance
            );
            for note in &report.demotions {
                println!("  demoted: {note}");
            }
            let rebuilt = artificer_scan_core::rebuild_sharp(&mesh, &report)
                .ok_or("sections need a datum frame (auto-datum found none)")?;
            let alignment = report.datum.as_ref().expect("datum present after rebuild");
            let aligned = mesh.transformed(&alignment.transform);
            let (z_low, z_high) = aligned
                .positions()
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
                    (lo.min(p.z), hi.max(p.z))
                });
            // An explicit stepped range walks one feature; otherwise the
            // cuts sample the whole part.
            let z_step = take_flag_value(&mut args, "--z-step")
                .map(|v| v.parse::<f64>())
                .transpose()
                .map_err(|_| "--z-step expects millimetres")?;
            let cuts = match z_step {
                Some(step) => {
                    let from = take_flag_value(&mut args, "--z-from")
                        .map(|v| v.parse::<f64>())
                        .transpose()
                        .map_err(|_| "--z-from expects millimetres")?
                        .unwrap_or(z_low);
                    let to = take_flag_value(&mut args, "--z-to")
                        .map(|v| v.parse::<f64>())
                        .transpose()
                        .map_err(|_| "--z-to expects millimetres")?
                        .unwrap_or(z_high);
                    section::stepped_levels(from, to, step)
                }
                None => section::default_cuts(z_low, z_high, meridians, levels),
            };
            let png = section::render_sections(
                &mesh,
                &rebuilt.mesh,
                &alignment.transform,
                &cuts,
                panel,
                reach,
                fixed_scale,
            );
            std::fs::write(&out, png).map_err(|e| format!("cannot write {out}: {e}"))?;
            println!("{} cross-sections written to {out}", cuts.len());
            let missing =
                section::missing_report(&mesh, &rebuilt.mesh, &alignment.transform, &cuts, reach);
            if missing.is_empty() {
                println!("every cut is accounted for within {reach} mm");
            } else {
                println!("outline the rebuild does not account for:");
                for line in &missing {
                    println!("{line}");
                }
            }
            for note in &rebuilt.skipped {
                println!("  skipped: {note}");
            }
            Ok(())
        }
        "simulate" => {
            // A scanner in a flag set: ideal mesh in, plausible scan
            // out, deterministic under --seed so a fixture is a
            // command line rather than a lost file.
            let mut options = artificer_scan_core::simulate::SimulateOptions::default();
            if let Some(value) = take_flag_value(&mut args, "--density") {
                options.density = value
                    .parse::<f64>()
                    .map_err(|_| format!("bad density {value}"))?;
            }
            if let Some(value) = take_flag_value(&mut args, "--smooth") {
                options.smooth = value
                    .parse::<f64>()
                    .map_err(|_| format!("bad smooth radius {value}"))?;
            }
            if let Some(value) = take_flag_value(&mut args, "--noise") {
                options.noise = value
                    .parse::<f64>()
                    .map_err(|_| format!("bad noise sigma {value}"))?;
            }
            if let Some(value) = take_flag_value(&mut args, "--dropout") {
                options.dropout = value
                    .parse::<usize>()
                    .map_err(|_| format!("bad dropout count {value}"))?;
            }
            if let Some(value) = take_flag_value(&mut args, "--dropout-size") {
                options.dropout_size = value
                    .parse::<f64>()
                    .map_err(|_| format!("bad dropout size {value}"))?;
            }
            if let Some(value) = take_flag_value(&mut args, "--seed") {
                options.seed = value
                    .parse::<u64>()
                    .map_err(|_| format!("bad seed {value}"))?;
            }
            let out = take_flag_value(&mut args, "--out")
                .unwrap_or_else(|| "simulated_scan.stl".to_owned());
            let snapshot_path = take_flag_value(&mut args, "--snapshot");
            let path = args.first().ok_or_else(usage)?;
            let mesh = load_mesh(path)?;
            let scan = artificer_scan_core::simulate::simulate_scan(&mesh, &options);
            for note in &scan.notes {
                println!("  {note}");
            }
            std::fs::write(&out, write_binary_stl(&scan.mesh))
                .map_err(|e| format!("cannot write {out}: {e}"))?;
            println!(
                "simulated scan written to {out} ({} triangles, {:.0} mm^2)",
                scan.mesh.triangles().len(),
                scan.mesh.surface_area()
            );
            if let Some(snapshot_path) = snapshot_path {
                let colors = vec![[142, 148, 158]; scan.mesh.triangles().len()];
                let png = snapshot::render_comparison(
                    &mesh,
                    &scan.mesh,
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
        "bench" => {
            let manifest_path = take_flag_value(&mut args, "--manifest")
                .unwrap_or_else(|| "bench/fixtures.txt".to_owned());
            let only = take_flag_value(&mut args, "--only");
            let baseline_path = take_flag_value(&mut args, "--baseline");
            let write_path = take_flag_value(&mut args, "--write-baseline");
            let text = std::fs::read_to_string(&manifest_path)
                .map_err(|e| format!("cannot read {manifest_path}: {e}"))?;
            let fixtures = artificer_scan_core::bench::parse_manifest(&text)?;
            let wanted: Vec<_> = fixtures
                .iter()
                .filter(|f| only.as_deref().is_none_or(|name| f.name == name))
                .collect();
            if wanted.is_empty() {
                return Err(match &only {
                    Some(name) => format!("no fixture named `{name}` in {manifest_path}"),
                    None => format!("{manifest_path} lists no fixtures"),
                });
            }
            // A sweep is hours long and the machine it runs on is not
            // guaranteed to stay up for all of them. Keep what each
            // fixture earned the moment it earns it, and let a later
            // run skip what is already scored, so an interrupted sweep
            // costs the fixture it died on and nothing else.
            let resume = take_flag(&mut args, "--resume");
            let mut scores: Vec<artificer_scan_core::bench::Score> = Vec::new();
            if resume
                && let Some(path) = &write_path
                && let Ok(text) = std::fs::read_to_string(path)
            {
                scores = artificer_scan_core::bench::from_text(&text);
                if !scores.is_empty() {
                    println!("resuming: {} fixture(s) already scored", scores.len());
                }
            }
            for fixture in wanted {
                if resume && scores.iter().any(|s| s.name == fixture.name) {
                    println!("skipping {} (already in baseline)", fixture.name);
                    continue;
                }
                // Say which fixture before running it: these take
                // minutes each, and a silent bench is indistinguishable
                // from a hung one.
                println!("running {} ({})...", fixture.name, fixture.source);
                // One unreadable part must not cost the other four
                // hours of an unattended sweep.
                let source = match load_mesh(&fixture.source) {
                    Ok(source) => source,
                    Err(problem) => {
                        eprintln!("  skipped {}: {problem}", fixture.name);
                        continue;
                    }
                };
                let started = std::time::Instant::now();
                let score = artificer_scan_core::bench::score_fixture(
                    fixture,
                    &source,
                    started.elapsed().as_secs_f64(),
                );
                let score = artificer_scan_core::bench::Score {
                    seconds: started.elapsed().as_secs_f64(),
                    ..score
                };
                println!(
                    "  {} bore(s) on size of {} found ({} expected), worst d error {:.3} mm, {:.0}s",
                    score.bores_on_size,
                    score.bores_found,
                    score.bores_expected,
                    score.worst_bore_error,
                    score.seconds
                );
                scores.push(score);
                if let Some(path) = &write_path {
                    std::fs::write(path, artificer_scan_core::bench::to_text(&scores))
                        .map_err(|e| format!("cannot write {path}: {e}"))?;
                }
            }
            println!();
            print!("{}", artificer_scan_core::bench::table(&scores));
            if let Some(path) = &baseline_path {
                let text = std::fs::read_to_string(path)
                    .map_err(|e| format!("cannot read {path}: {e}"))?;
                let baseline = artificer_scan_core::bench::from_text(&text);
                println!();
                print!(
                    "{}",
                    artificer_scan_core::bench::compare(&baseline, &scores)
                );
            }
            if let Some(path) = &write_path {
                println!("\nbaseline written to {path}");
            }
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
