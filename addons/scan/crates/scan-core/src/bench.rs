//! A scoreboard for the pipeline over a corpus of known parts.
//!
//! Coverage percentages are blind to the failures that matter most. A
//! bore whose diameter drifts from 10.00 mm to 11.29 mm moves the area
//! totals by almost nothing, and twice in one session a change that
//! read as an *improvement* in coverage had made the geometry worse —
//! the invention figure fell while two of four bores were being fitted
//! a millimetre off their own axis. Percentages of area cannot see
//! that, because a wrong cylinder covers about as much area as a right
//! one.
//!
//! So the bench scores against what the part actually is: how many
//! bores it has and at what diameter, checked against the emitted
//! geometry rather than against the narration. Coverage still travels
//! alongside, because it answers a different question — how much of
//! the scan was described at all — and the two together are what makes
//! a change falsifiable.
//!
//! Fixtures are simulated from source CAD at a fixed seed, so a run is
//! reproducible from the repository and a scratchpad wipe costs nothing
//! but the time to regenerate.

use crate::mesh::TriangleMesh;
use crate::report::{ReverseOptions, reverse_engineer};
use crate::simulate::{SimulateOptions, simulate_scan};

/// One part, the scan to make of it, and what it is known to contain.
#[derive(Debug, Clone)]
pub struct Fixture {
    pub name: String,
    /// Path to the source CAD or mesh, resolved by the caller.
    pub source: String,
    pub simulate: SimulateOptions,
    /// How many bores the real part has.
    pub expect_bores: usize,
    /// Their true diameter (mm). Zero means "do not score diameters".
    pub bore_diameter: f64,
    /// How far a diameter may drift before it counts as wrong (mm).
    pub bore_tolerance: f64,
}

/// What one fixture scored.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    pub name: String,
    pub noise_sigma: f64,
    pub tolerance: f64,
    pub features: usize,
    pub triangles: usize,
    /// Fraction of the scanned surface the rebuild explains.
    pub explained: f64,
    /// Fraction of the emitted surface that lies nowhere near the scan.
    pub invented: f64,
    /// Fraction of the scan explained by analytic surfaces alone.
    pub analytic: f64,
    pub bores_expected: usize,
    pub bores_found: usize,
    /// Found bores whose diameter is within tolerance of the truth.
    pub bores_on_size: usize,
    /// Worst diameter error among the found bores (mm).
    pub worst_bore_error: f64,
    pub seconds: f64,
    pub slowest_stage: String,
    pub slowest_seconds: f64,
}

/// Reads a fixture manifest.
///
/// One fixture per non-empty, non-`#` line, as `key=value` pairs. The
/// format is deliberately plain: this tree carries no serialization
/// dependency, and a manifest that can be read at a glance is a
/// manifest whose ground truth can be checked at a glance.
///
/// ```text
/// name=spacer-n003 source=parts/spacer.step density=0.25 noise=0.03 seed=7 bores=4 bore_d=10.0
/// ```
pub fn parse_manifest(text: &str) -> Result<Vec<Fixture>, String> {
    let mut fixtures = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fixture = Fixture {
            name: String::new(),
            source: String::new(),
            // Whatever the simulator's own defaults are, including the
            // 0.35 mm spot. A scanner has a spot size; a bench that
            // silently drops it measures a sharper part than anyone
            // owns, and its numbers stop being comparable to the runs
            // the work was actually developed against.
            simulate: SimulateOptions::default(),
            expect_bores: 0,
            bore_diameter: 0.0,
            bore_tolerance: 0.15,
        };
        for field in line.split_whitespace() {
            let (key, value) = field
                .split_once('=')
                .ok_or_else(|| format!("line {}: `{field}` is not key=value", number + 1))?;
            let number_of = |what: &str| -> Result<f64, String> {
                value
                    .parse::<f64>()
                    .map_err(|_| format!("line {}: {what} `{value}` is not a number", number + 1))
            };
            match key {
                "name" => fixture.name = value.to_owned(),
                "source" => fixture.source = value.to_owned(),
                "density" => fixture.simulate.density = number_of("density")?,
                "smooth" => fixture.simulate.smooth = number_of("smooth")?,
                "noise" => fixture.simulate.noise = number_of("noise")?,
                "dropout" => fixture.simulate.dropout = number_of("dropout")? as usize,
                "dropout_size" => fixture.simulate.dropout_size = number_of("dropout_size")?,
                "seed" => fixture.simulate.seed = number_of("seed")? as u64,
                "bores" => fixture.expect_bores = number_of("bores")? as usize,
                "bore_d" => fixture.bore_diameter = number_of("bore_d")?,
                "bore_tol" => fixture.bore_tolerance = number_of("bore_tol")?,
                other => return Err(format!("line {}: unknown key `{other}`", number + 1)),
            }
        }
        if fixture.name.is_empty() {
            return Err(format!("line {}: fixture has no name", number + 1));
        }
        if fixture.source.is_empty() {
            return Err(format!(
                "line {}: fixture `{}` has no source",
                number + 1,
                fixture.name
            ));
        }
        fixtures.push(fixture);
    }
    Ok(fixtures)
}

/// Simulates the fixture's scan, runs the pipeline, and scores it.
///
/// Takes the source mesh already loaded so the core stays out of the
/// business of deciding what a path means.
pub fn score_fixture(fixture: &Fixture, source: &TriangleMesh, seconds: f64) -> Score {
    let scan = simulate_scan(source, &fixture.simulate);
    let report = reverse_engineer(&scan.mesh, &ReverseOptions::default());
    let mut score = Score {
        name: fixture.name.clone(),
        noise_sigma: report.noise_sigma,
        tolerance: report.tolerance,
        features: report.features.len(),
        triangles: scan.mesh.triangles().len(),
        explained: 0.0,
        invented: 0.0,
        analytic: 0.0,
        bores_expected: fixture.expect_bores,
        bores_found: 0,
        bores_on_size: 0,
        worst_bore_error: 0.0,
        seconds,
        slowest_stage: String::new(),
        slowest_seconds: 0.0,
    };
    for stage in &report.stages {
        if stage.seconds > score.slowest_seconds {
            score.slowest_seconds = stage.seconds;
            score.slowest_stage = stage.stage.clone();
        }
    }
    let Some(rebuilt) = crate::rebuild::rebuild_sharp(&scan.mesh, &report) else {
        return score;
    };
    let Some(alignment) = report.datum.as_ref() else {
        return score;
    };
    let (explained, total) =
        crate::coverage::explained_area(&scan.mesh, &rebuilt.mesh, alignment, report.tolerance);
    let (invented, emitted) =
        crate::coverage::invented_area(&scan.mesh, &rebuilt.mesh, alignment, report.tolerance);
    score.explained = explained / total.max(1e-9);
    score.invented = invented / emitted.max(1e-9);
    // The analytic share is measured on the certified surfaces alone:
    // pooling them with carried measured surface flatters the figure.
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
                .is_some_and(|f| !matches!(f.surface, crate::segment::SurfaceClass::Freeform))
        })
        .map(|(face, _)| rebuilt.mesh.triangle_points(face))
        .collect();
    if let Some(analytic) = TriangleMesh::from_triangle_soup(&certified, 1e-6) {
        let (exact, _) =
            crate::coverage::explained_area(&scan.mesh, &analytic, alignment, report.tolerance);
        score.analytic = exact / total.max(1e-9);
    }
    score.bores_found = rebuilt.bores.len();
    if fixture.bore_diameter > 0.0 {
        for bore in &rebuilt.bores {
            let error = (bore.diameter - fixture.bore_diameter).abs();
            score.worst_bore_error = score.worst_bore_error.max(error);
            if error <= fixture.bore_tolerance {
                score.bores_on_size += 1;
            }
        }
    }
    score
}

/// The scoreboard as a table.
pub fn table(scores: &[Score]) -> String {
    let mut out = String::from(
        "fixture              tri     feat  expl%  inv%  anly%   bores  worst-d   slowest\n",
    );
    for score in scores {
        out.push_str(&format!(
            "{:<20} {:>7} {:>6} {:>6.1} {:>5.1} {:>6.1}  {:>2}/{:<2}/{:<2} {:>7.3}   {} {:.0}s\n",
            truncate(&score.name, 20),
            score.triangles,
            score.features,
            100.0 * score.explained,
            100.0 * score.invented,
            100.0 * score.analytic,
            score.bores_on_size,
            score.bores_found,
            score.bores_expected,
            score.worst_bore_error,
            score.slowest_stage,
            score.slowest_seconds,
        ));
    }
    out.push_str(
        "\nbores read on-size / found / expected; worst-d is the largest diameter error (mm)\n",
    );
    out
}

/// The scoreboard as a baseline file: line-oriented so `git diff` shows
/// exactly which number moved.
pub fn to_text(scores: &[Score]) -> String {
    let mut out = String::from("# artificer-scan bench baseline\n");
    for s in scores {
        out.push_str(&format!(
            "name={} tri={} feat={} sigma={:.4} tol={:.4} expl={:.4} inv={:.4} anly={:.4} \
             on_size={} found={} expected={} worst_d={:.4} slowest={} slowest_s={:.1}\n",
            s.name,
            s.triangles,
            s.features,
            s.noise_sigma,
            s.tolerance,
            s.explained,
            s.invented,
            s.analytic,
            s.bores_on_size,
            s.bores_found,
            s.bores_expected,
            s.worst_bore_error,
            if s.slowest_stage.is_empty() {
                "-"
            } else {
                &s.slowest_stage
            },
            s.slowest_seconds,
        ));
    }
    out
}

/// Reads a baseline written by [`to_text`].
pub fn from_text(text: &str) -> Vec<Score> {
    let mut scores = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut score = Score {
            name: String::new(),
            noise_sigma: 0.0,
            tolerance: 0.0,
            features: 0,
            triangles: 0,
            explained: 0.0,
            invented: 0.0,
            analytic: 0.0,
            bores_expected: 0,
            bores_found: 0,
            bores_on_size: 0,
            worst_bore_error: 0.0,
            seconds: 0.0,
            slowest_stage: String::new(),
            slowest_seconds: 0.0,
        };
        for field in line.split_whitespace() {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            let f = value.parse::<f64>().unwrap_or(0.0);
            match key {
                "name" => score.name = value.to_owned(),
                "tri" => score.triangles = f as usize,
                "feat" => score.features = f as usize,
                "sigma" => score.noise_sigma = f,
                "tol" => score.tolerance = f,
                "expl" => score.explained = f,
                "inv" => score.invented = f,
                "anly" => score.analytic = f,
                "on_size" => score.bores_on_size = f as usize,
                "found" => score.bores_found = f as usize,
                "expected" => score.bores_expected = f as usize,
                "worst_d" => score.worst_bore_error = f,
                "slowest" => score.slowest_stage = value.to_owned(),
                "slowest_s" => score.slowest_seconds = f,
                _ => {}
            }
        }
        if !score.name.is_empty() {
            scores.push(score);
        }
    }
    scores
}

/// What moved since the baseline.
///
/// Reports every fixture, including the ones that did not move, so a
/// silent fixture is visibly silent rather than merely absent — a
/// comparison that only lists regressions cannot be told apart from
/// one that failed to run.
pub fn compare(baseline: &[Score], current: &[Score]) -> String {
    let mut out =
        String::from("fixture              expl%      inv%     anly%   on-size  worst-d\n");
    let mut regressed = 0;
    for now in current {
        let Some(was) = baseline.iter().find(|b| b.name == now.name) else {
            out.push_str(&format!(
                "{:<20} (new fixture, no baseline)\n",
                truncate(&now.name, 20)
            ));
            continue;
        };
        // A bore that leaves tolerance, or invention that climbs, is a
        // regression however the area totals move.
        let worse = now.bores_on_size < was.bores_on_size
            || now.invented > was.invented + 0.002
            || now.explained < was.explained - 0.002;
        if worse {
            regressed += 1;
        }
        out.push_str(&format!(
            "{:<20} {:>+6.2} {:>+9.2} {:>+9.2} {:>+8} {:>+8.3}{}\n",
            truncate(&now.name, 20),
            100.0 * (now.explained - was.explained),
            100.0 * (now.invented - was.invented),
            100.0 * (now.analytic - was.analytic),
            now.bores_on_size as i64 - was.bores_on_size as i64,
            now.worst_bore_error - was.worst_bore_error,
            if worse { "   REGRESSED" } else { "" },
        ));
    }
    for was in baseline {
        if !current.iter().any(|n| n.name == was.name) {
            out.push_str(&format!(
                "{:<20} (in baseline, not run)\n",
                truncate(&was.name, 20)
            ));
        }
    }
    out.push_str(&format!(
        "\n{} fixture(s) compared, {regressed} regressed\n",
        current.len()
    ));
    out
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_owned()
    } else {
        text.chars().take(width - 1).chain(['~']).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_line_carries_the_scan_and_the_ground_truth() {
        let fixtures = parse_manifest(
            "# a comment\n\nname=spacer source=a/b.step density=0.25 noise=0.03 seed=7 \
             bores=4 bore_d=10.0 bore_tol=0.2\n",
        )
        .expect("parses");
        assert_eq!(fixtures.len(), 1);
        let fixture = &fixtures[0];
        assert_eq!(fixture.name, "spacer");
        assert_eq!(fixture.source, "a/b.step");
        assert_eq!(fixture.simulate.seed, 7);
        assert!((fixture.simulate.noise - 0.03).abs() < 1e-9);
        // An omitted key keeps the simulator's own default, spot
        // included, rather than quietly idealising the scan.
        assert_eq!(fixture.simulate.smooth, SimulateOptions::default().smooth);
        assert_eq!(fixture.expect_bores, 4);
        assert!((fixture.bore_diameter - 10.0).abs() < 1e-9);
    }

    #[test]
    fn a_malformed_manifest_says_which_line_and_why() {
        let error = parse_manifest("name=a source=b wobble=3\n").expect_err("rejected");
        assert!(error.contains("line 1"), "{error}");
        assert!(error.contains("wobble"), "{error}");
        let error = parse_manifest("name=a source=b noise=fast\n").expect_err("rejected");
        assert!(error.contains("not a number"), "{error}");
    }

    #[test]
    fn a_baseline_round_trips() {
        let scores = vec![Score {
            name: "spacer".to_owned(),
            noise_sigma: 0.0301,
            tolerance: 0.0591,
            features: 2900,
            triangles: 1177190,
            explained: 0.993,
            invented: 0.036,
            analytic: 0.963,
            bores_expected: 4,
            bores_found: 4,
            bores_on_size: 4,
            worst_bore_error: 0.04,
            seconds: 0.0,
            slowest_stage: "coaxial-unify".to_owned(),
            slowest_seconds: 3219.4,
        }];
        let read = from_text(&to_text(&scores));
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].name, "spacer");
        assert_eq!(read[0].bores_on_size, 4);
        assert!((read[0].invented - 0.036).abs() < 1e-6);
        assert_eq!(read[0].slowest_stage, "coaxial-unify");
    }

    #[test]
    fn a_bore_leaving_tolerance_reads_as_a_regression_though_coverage_improves() {
        // The exact shape of the trap this bench exists for: invention
        // fell, so every area figure looks better, while a bore drifted
        // out of size. Coverage alone would have called this a win.
        let was = Score {
            name: "spacer".to_owned(),
            bores_on_size: 4,
            invented: 0.049,
            explained: 0.992,
            worst_bore_error: 0.04,
            ..blank()
        };
        let now = Score {
            name: "spacer".to_owned(),
            bores_on_size: 2,
            invented: 0.036,
            explained: 0.993,
            worst_bore_error: 1.29,
            ..blank()
        };
        let report = compare(&[was], &[now]);
        assert!(report.contains("REGRESSED"), "{report}");
        assert!(report.contains("1 regressed"), "{report}");
    }

    #[test]
    fn a_fixture_that_did_not_run_is_named_rather_than_missing() {
        let was = Score {
            name: "rail".to_owned(),
            ..blank()
        };
        let report = compare(&[was], &[]);
        assert!(report.contains("not run"), "{report}");
    }

    fn blank() -> Score {
        Score {
            name: String::new(),
            noise_sigma: 0.0,
            tolerance: 0.0,
            features: 0,
            triangles: 0,
            explained: 0.0,
            invented: 0.0,
            analytic: 0.0,
            bores_expected: 0,
            bores_found: 0,
            bores_on_size: 0,
            worst_bore_error: 0.0,
            seconds: 0.0,
            slowest_stage: String::new(),
            slowest_seconds: 0.0,
        }
    }
}
