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
//! but the time to regenerate. A source written `synth:NAME` is a part
//! built in code ([`crate::synth::named_part`]) rather than read from a
//! file, so those fixtures need nothing but the repository — and where
//! the part carries a known freeform surface, the rebuild is scored
//! against that surface too, which no file-based fixture can offer.
//!
//! Freeform is scored as its own question. A region no analytic surface
//! explains is carried as measured mesh, which reads as "explained" in
//! the coverage totals while being nothing a CAD system can hold, so
//! the share of the scan left freeform, and how much of a known
//! freeform surface arrives as a *surface*, travel beside the totals.

use crate::mesh::TriangleMesh;
use crate::report::{ReverseOptions, reverse_engineer};
use crate::segment::SurfaceClass;
use crate::simulate::{SimulateOptions, simulate_scan};

/// Prefix naming a part built in code rather than read from a file.
pub const SYNTH_PREFIX: &str = "synth:";

/// The mesh a `synth:NAME` source names, or `None` when the source is
/// a file path for the caller to read.
pub fn synthetic_source(source: &str) -> Option<Result<TriangleMesh, String>> {
    let name = source.strip_prefix(SYNTH_PREFIX)?;
    Some(crate::synth::named_part(name).ok_or_else(|| format!("no synthetic part named `{name}`")))
}

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
    /// Share of the scan's area that ends the pipeline as measured mesh:
    /// freeform that neither an analytic surface nor a B-spline patch
    /// carries.
    pub freeform: f64,
    /// Area-weighted RMS of the analytic fits against their own faces
    /// (mm), and the worst single deviation among them.
    pub analytic_rms: f64,
    pub analytic_max: f64,
    /// Share of the rebuilt shell's walked edges that sew, and the edge
    /// ends left open. A solid is 1 and 0.
    pub sewn: f64,
    pub open_ends: usize,
    /// Against a synthetic part's known freeform surface, where the
    /// fixture has one: the rebuilt model's RMS and worst deviation
    /// from it (mm) over the true surface's interior...
    pub truth_rms: f64,
    pub truth_max: f64,
    /// ...and the share of that interior the model carries as a CAD
    /// surface rather than as measured mesh. Negative when the fixture
    /// has no truth.
    pub truth_cad: f64,
    /// B-spline patches made, and the share of the scan's area their
    /// trimmed surfaces explain.
    pub spline_patches: usize,
    pub spline: f64,
    /// Area-weighted RMS of the patches against their own samples (mm)
    /// and the worst single deviation among them.
    pub spline_rms: f64,
    pub spline_max: f64,
    /// Seconds the spline stage took.
    pub spline_seconds: f64,
}

impl Score {
    /// A score with nothing measured yet.
    pub fn empty() -> Self {
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
            freeform: 0.0,
            analytic_rms: 0.0,
            analytic_max: 0.0,
            sewn: 0.0,
            open_ends: 0,
            truth_rms: 0.0,
            truth_max: 0.0,
            truth_cad: -1.0,
            spline_patches: 0,
            spline: 0.0,
            spline_rms: 0.0,
            spline_max: 0.0,
            spline_seconds: 0.0,
        }
    }
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
        bores_expected: fixture.expect_bores,
        seconds,
        truth_cad: -1.0,
        ..Score::empty()
    };
    for stage in &report.stages {
        if stage.seconds > score.slowest_seconds {
            score.slowest_seconds = stage.seconds;
            score.slowest_stage = stage.stage.clone();
        }
    }
    let freeform: f64 = report
        .features
        .iter()
        .filter(|f| {
            matches!(f.surface, SurfaceClass::Freeform)
                && !report.splines.iter().any(|patch| patch.feature == f.id)
        })
        .map(|f| f.area)
        .sum();
    score.freeform = freeform / report.total_area.max(1e-9);
    (score.analytic_rms, score.analytic_max) = analytic_deviation(&report);
    score.spline_patches = report.splines.len();
    {
        let (mut squared, mut area) = (0.0f64, 0.0f64);
        for patch in &report.splines {
            squared += patch.area * patch.fit.deviation.rms * patch.fit.deviation.rms;
            area += patch.area;
            score.spline_max = score.spline_max.max(patch.fit.deviation.max_abs);
        }
        score.spline_rms = (squared / area.max(1e-9)).sqrt();
    }
    score.spline_seconds = report
        .stages
        .iter()
        .find(|stage| stage.stage == "spline-fit")
        .map_or(0.0, |stage| stage.seconds);
    let Some(rebuilt) = crate::rebuild::rebuild_sharp(&scan.mesh, &report) else {
        return score;
    };
    let Some(alignment) = report.datum.as_ref() else {
        return score;
    };
    score.sewn = rebuilt.shell.watertight_fraction();
    score.open_ends = rebuilt.open_ends.len();
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
    // The patches' own share, measured on their trimmed surfaces alone.
    let patches: Vec<[artificer_geometry::Point3; 3]> = report
        .splines
        .iter()
        .flat_map(|patch| patch.tessellate(crate::freeform::TESSELLATION_STEP))
        .collect();
    if let Some(splines) = TriangleMesh::from_triangle_soup(&patches, 1e-6) {
        let (carried, _) =
            crate::coverage::explained_area(&scan.mesh, &splines, alignment, report.tolerance);
        score.spline = carried / total.max(1e-9);
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
    if let Some(truth) = fixture
        .source
        .strip_prefix(SYNTH_PREFIX)
        .and_then(crate::synth::ground_truth)
    {
        // A B-spline patch is a CAD surface as much as a plane is; what
        // does not count is measured mesh carried as it was scanned.
        let certified = |face: usize| {
            report
                .features
                .iter()
                .find(|f| f.id == rebuilt.feature_of_face[face])
                .is_some_and(|f| {
                    !matches!(f.surface, SurfaceClass::Freeform)
                        || report.splines.iter().any(|patch| patch.feature == f.id)
                })
        };
        let against = score_truth(&scan.mesh, &rebuilt.mesh, alignment, truth, certified);
        (score.truth_rms, score.truth_max, score.truth_cad) = against;
    }
    score
}

/// Area-weighted RMS over the analytic fits and the worst single
/// deviation among them — how well the surfaces that *were* recognized
/// describe their own material.
fn analytic_deviation(report: &crate::report::ReverseReport) -> (f64, f64) {
    let (mut squared, mut area, mut worst) = (0.0f64, 0.0f64, 0.0f64);
    for feature in &report.features {
        let deviation = match &feature.surface {
            SurfaceClass::Plane(fit) => fit.deviation,
            SurfaceClass::Cylinder(fit) => fit.deviation,
            SurfaceClass::Sphere(fit) => fit.deviation,
            SurfaceClass::Cone(fit) => fit.deviation,
            SurfaceClass::Blend(fit) | SurfaceClass::Torus(fit) => fit.deviation,
            _ => continue,
        };
        squared += feature.area * deviation.rms * deviation.rms;
        area += feature.area;
        worst = worst.max(deviation.max_abs);
    }
    ((squared / area.max(1e-9)).sqrt(), worst)
}

/// The rebuilt model against a synthetic part's known surface: RMS and
/// worst deviation (mm) of the model's geometry lying over the true
/// surface's interior, and the share of that interior — measured as the
/// scan's own area there — the model carries on `certified` faces, which
/// is to say as a surface rather than as measured mesh.
///
/// The model is in the datum frame and the truth in the part's own, so
/// every rebuilt vertex is carried back before it is asked.
fn score_truth(
    scan: &TriangleMesh,
    rebuilt: &TriangleMesh,
    alignment: &crate::datum::DatumAlignment,
    truth: crate::synth::GroundTruth,
    certified: impl Fn(usize) -> bool,
) -> (f64, f64, f64) {
    let back = alignment.transform.inverse();
    let (mut squared, mut weight, mut worst) = (0.0f64, 0.0f64, 0.0f64);
    let mut carried = 0.0f64;
    for face in 0..rebuilt.triangles().len() {
        let corners = rebuilt.triangle_points(face).map(|p| back.apply_point(p));
        let centroid = artificer_geometry::Point3::new(
            (corners[0].x + corners[1].x + corners[2].x) / 3.0,
            (corners[0].y + corners[1].y + corners[2].y) / 3.0,
            (corners[0].z + corners[1].z + corners[2].z) / 3.0,
        );
        if truth(centroid).is_none() {
            continue;
        }
        let area = rebuilt.face_area(face);
        for point in corners.into_iter().chain([centroid]) {
            if let Some(distance) = truth(point) {
                squared += area * distance * distance;
                weight += area;
                worst = worst.max(distance.abs());
            }
        }
        if certified(face) {
            carried += area;
        }
    }
    let true_area: f64 = (0..scan.triangles().len())
        .filter(|&face| truth(scan.face_centroid(face)).is_some())
        .map(|face| scan.face_area(face))
        .sum();
    (
        (squared / weight.max(1e-12)).sqrt(),
        worst,
        carried / true_area.max(1e-9),
    )
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
    out.push_str(
        "\nfixture              free%  an-rms  an-max   spl% patch spl-rms spl-max spl-s  \
         sewn%  open  truth-rms truth-max  cad%   secs\n",
    );
    for score in scores {
        let truth = if score.truth_cad < 0.0 {
            format!("{:>9} {:>9} {:>5}", "-", "-", "-")
        } else {
            format!(
                "{:>9.4} {:>9.3} {:>5.1}",
                score.truth_rms,
                score.truth_max,
                100.0 * score.truth_cad
            )
        };
        out.push_str(&format!(
            "{:<20} {:>5.1} {:>7.4} {:>7.3} {:>6.1} {:>5} {:>7.4} {:>7.3} {:>5.1} {:>6.1} {:>5}  \
             {truth} {:>6.1}\n",
            truncate(&score.name, 20),
            100.0 * score.freeform,
            score.analytic_rms,
            score.analytic_max,
            100.0 * score.spline,
            score.spline_patches,
            score.spline_rms,
            score.spline_max,
            score.spline_seconds,
            100.0 * score.sewn,
            score.open_ends,
            score.seconds,
        ));
    }
    out.push_str(
        "\nfree% is the scan area left as measured mesh; an-rms/an-max the analytic fits against \
         their own faces (mm);\nspl% the scan area B-spline patches explain, spl-rms/spl-max the \
         patches against their samples (mm),\nspl-s the stage's seconds; sewn% the rebuilt \
         shell's sewn edges; truth-* the model against a\nsynthetic part's known surface (mm), \
         cad% the share of that surface carried as a CAD surface\n(analytic or B-spline) rather \
         than measured mesh\n",
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
             on_size={} found={} expected={} worst_d={:.4} slowest={} slowest_s={:.1} \
             secs={:.1} free={:.4} an_rms={:.4} an_max={:.4} sewn={:.4} open={}",
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
            s.seconds,
            s.freeform,
            s.analytic_rms,
            s.analytic_max,
            s.sewn,
            s.open_ends,
        ));
        if s.truth_cad >= 0.0 {
            out.push_str(&format!(
                " truth_rms={:.4} truth_max={:.4} truth_cad={:.4}",
                s.truth_rms, s.truth_max, s.truth_cad
            ));
        }
        if s.spline_patches > 0 {
            out.push_str(&format!(
                " patches={} spl={:.4} spl_rms={:.4} spl_max={:.4} spl_s={:.1}",
                s.spline_patches, s.spline, s.spline_rms, s.spline_max, s.spline_seconds
            ));
        }
        out.push('\n');
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
        let mut score = Score::empty();
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
                "secs" => score.seconds = f,
                "free" => score.freeform = f,
                "an_rms" => score.analytic_rms = f,
                "an_max" => score.analytic_max = f,
                "sewn" => score.sewn = f,
                "open" => score.open_ends = f as usize,
                "truth_rms" => score.truth_rms = f,
                "truth_max" => score.truth_max = f,
                "truth_cad" => score.truth_cad = f,
                "patches" => score.spline_patches = f as usize,
                "spl" => score.spline = f,
                "spl_rms" => score.spline_rms = f,
                "spl_max" => score.spline_max = f,
                "spl_s" => score.spline_seconds = f,
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
    let mut out = String::from(
        "fixture              expl%      inv%     anly%   on-size  worst-d     free%      spl%      cad%\n",
    );
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
        let cad = if now.truth_cad >= 0.0 && was.truth_cad >= 0.0 {
            format!("{:>+9.2}", 100.0 * (now.truth_cad - was.truth_cad))
        } else {
            format!("{:>9}", "-")
        };
        out.push_str(&format!(
            "{:<20} {:>+6.2} {:>+9.2} {:>+9.2} {:>+8} {:>+8.3} {:>+9.2} {:>+9.2} {cad}{}\n",
            truncate(&now.name, 20),
            100.0 * (now.explained - was.explained),
            100.0 * (now.invented - was.invented),
            100.0 * (now.analytic - was.analytic),
            now.bores_on_size as i64 - was.bores_on_size as i64,
            now.worst_bore_error - was.worst_bore_error,
            100.0 * (now.freeform - was.freeform),
            100.0 * (now.spline - was.spline),
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
            seconds: 12.5,
            slowest_stage: "coaxial-unify".to_owned(),
            slowest_seconds: 3219.4,
            freeform: 0.081,
            analytic_rms: 0.0213,
            analytic_max: 0.412,
            sewn: 0.171,
            open_ends: 96,
            truth_rms: 0.0182,
            truth_max: 0.094,
            truth_cad: 0.35,
            spline_patches: 2,
            spline: 0.362,
            spline_rms: 0.0244,
            spline_max: 0.559,
            spline_seconds: 2.3,
        }];
        let read = from_text(&to_text(&scores));
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].name, "spacer");
        assert_eq!(read[0].bores_on_size, 4);
        assert!((read[0].invented - 0.036).abs() < 1e-6);
        assert_eq!(read[0].slowest_stage, "coaxial-unify");
        assert!((read[0].freeform - 0.081).abs() < 1e-6);
        assert_eq!(read[0].open_ends, 96);
        assert!((read[0].truth_cad - 0.35).abs() < 1e-6);
        assert_eq!(read[0].spline_patches, 2);
        assert!((read[0].spline - 0.362).abs() < 1e-6);
        // A line from before these columns existed reads as "no truth".
        let old = from_text("name=rail tri=10 feat=2 expl=0.9\n");
        assert!(old[0].truth_cad < 0.0);
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

    #[test]
    fn a_synthetic_source_is_built_in_code_and_a_path_is_left_to_the_caller() {
        let mesh = synthetic_source("synth:freeform-block")
            .expect("a synth source")
            .expect("a known part");
        assert!(mesh.triangles().len() > 1000);
        assert!(
            synthetic_source("synth:no-such-part")
                .expect("synth")
                .is_err()
        );
        assert!(synthetic_source("bench/parts/wheel-spacer.step").is_none());
    }

    fn blank() -> Score {
        Score::empty()
    }
}
