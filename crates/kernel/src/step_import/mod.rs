//! STEP import: a Part 21 B-rep read into the kernel's own vocabulary and
//! conventions (Track I of ADR 0056).
//!
//! The file is parsed by `artificer_step`, its first product's solids are
//! read face by face into the kernel's carriers and curves (`read`), the
//! faces are conformed to the kernel's conventions — outward surfaces,
//! pcurves, seams at azimuth `0` and `π`, pole edges, welded vertices and
//! edges — and assembled into shells and solids (`conform`), and the result
//! goes through the solid validator. Every step that cannot be taken is
//! named: a surface or curve the kernel does not carry, a rational spline,
//! a gap wider than the file's accuracy, a shell that does not close. When
//! any face is refused the part opens as a reference mesh instead
//! (`mesh`): the readable faces triangulated, the unreadable ones capped by
//! their boundaries, built as a faceted B-rep and labelled approximate,
//! with every refusal listed beside it. Never a silently approximated
//! solid.

use std::collections::BTreeMap;

use artificer_protocol::{
    Diagnostic, DiagnosticCode, DiagnosticMeasurement, DiagnosticSeverity, KernelStage,
    NumericInterval, PrecisionPolicy, QuantityKind,
};

use crate::topology::{FaceRole, Topology};
use crate::{faceted_boolean, validator};

mod conform;
mod mesh;
mod read;

use conform::Importer;
use read::{Reader, Refusal, find_shapes};

/// The diagnostic codes import can attach to a report.
pub(crate) mod codes {
    /// The text is not a Part 21 file.
    pub(crate) const SYNTAX_INVALID: &str = "STEP_SYNTAX_INVALID";
    /// An entity the reader needs is missing, malformed, or of a kind it
    /// does not read; also a file with no solid in it.
    pub(crate) const ENTITY_UNSUPPORTED: &str = "STEP_ENTITY_UNSUPPORTED";
    /// A face whose surface, curves or loops the kernel cannot carry.
    pub(crate) const FACE_UNSUPPORTED: &str = "STEP_FACE_UNSUPPORTED";
    /// A shell whose edges do not all bound two faces.
    pub(crate) const SHELL_OPEN: &str = "STEP_SHELL_OPEN";
    /// Geometry that should meet and does not, within the accuracy the file
    /// declares or the kernel's own agreement.
    pub(crate) const GAP_EXCEEDS_TOLERANCE: &str = "STEP_GAP_EXCEEDS_TOLERANCE";
    /// A rational B-spline whose weights differ.
    pub(crate) const RATIONAL_UNSUPPORTED: &str = "STEP_RATIONAL_UNSUPPORTED";
    /// A rational B-spline read as non-rational because its weights agree
    /// within the reading tolerance; the spread is the measurement.
    pub(crate) const RATIONAL_APPROXIMATED: &str = "STEP_RATIONAL_APPROXIMATED";
    /// A length unit the reader cannot convert.
    pub(crate) const UNIT_UNSUPPORTED: &str = "STEP_UNIT_UNSUPPORTED";
    /// The part opened as a reference mesh: the label of the approximate
    /// tier, with the refusals that sent it there listed beside it.
    pub(crate) const FACETED_APPROXIMATION: &str = "STEP_FACETED_APPROXIMATION";
    pub(crate) const BSPLINE_DEGREE_UNSUPPORTED: &str = "BSPLINE_DEGREE_UNSUPPORTED";
    pub(crate) const BSPLINE_UNCLAMPED_UNSUPPORTED: &str = "BSPLINE_UNCLAMPED_UNSUPPORTED";
    pub(crate) const BSPLINE_KNOTS_INVALID: &str = "BSPLINE_KNOTS_INVALID";
}

/// The rung an exact import reports.
pub(crate) const RUNG_EXACT: &str = "step-import/exact";
/// The rung a reference-mesh import reports; its suffix is what marks the
/// tier approximate.
pub(crate) const RUNG_FACETED: &str = "step-import/faceted";

/// Rational weights within this relative spread of one another are read as
/// one weight.
const RATIONAL_TOLERANCE: f64 = 1.0e-9;
/// The accuracy assumed for a file that declares none, in millimetres.
const DEFAULT_UNCERTAINTY: f64 = 1.0e-6;
/// The reference mesh's chord, as a fraction of the part's extent, and the
/// most triangles it is allowed before the chord is coarsened.
const MESH_CHORD_FRACTION: f64 = 1.0e-3;
const MESH_TRIANGLE_BUDGET: usize = 12_000;

/// What an import produced.
pub(crate) struct ImportOutcome {
    pub(crate) topology: Topology,
    pub(crate) rung: &'static str,
    pub(crate) warnings: Vec<Diagnostic>,
    /// The STEP entity id each kernel face came from, by face index.
    pub(crate) face_sources: Vec<u64>,
    /// The STEP entity id each kernel edge came from, by edge index, where
    /// it came from one.
    pub(crate) edge_sources: Vec<Option<u64>>,
}

/// Why nothing could be imported, with the refusals as diagnostics.
pub(crate) struct ImportFailure {
    pub(crate) message: String,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

impl Refusal {
    fn diagnostic(&self, severity: DiagnosticSeverity) -> Diagnostic {
        let mut details = BTreeMap::new();
        if let Some(entity) = self.entity {
            details.insert("entity".to_owned(), format!("#{entity}"));
        }
        Diagnostic {
            code: DiagnosticCode::new(self.code),
            severity,
            stage: KernelStage::Construction,
            message: self.text(),
            subjects: Vec::new(),
            path: Vec::new(),
            measurement: self.measured.and_then(|(measured, allowed)| {
                (measured.is_finite() && allowed.is_finite()).then_some(DiagnosticMeasurement {
                    quantity: if self.code == codes::RATIONAL_APPROXIMATED
                        || self.code == codes::SHELL_OPEN
                    {
                        QuantityKind::Unitless
                    } else {
                        QuantityKind::Length
                    },
                    measured,
                    allowed: NumericInterval {
                        min: None,
                        max: Some(allowed),
                    },
                })
            }),
            details,
        }
    }
}

fn failure(message: impl Into<String>, refusals: &[Refusal]) -> ImportFailure {
    ImportFailure {
        message: message.into(),
        diagnostics: refusals
            .iter()
            .map(|refusal| refusal.diagnostic(DiagnosticSeverity::Error))
            .collect(),
    }
}

/// Reads a Part 21 file into a kernel topology.
pub(crate) fn import_step(
    text: &str,
    precision: PrecisionPolicy,
) -> Result<ImportOutcome, ImportFailure> {
    let file = artificer_step::parse(text).map_err(|error| {
        failure(
            format!("the text is not a Part 21 file: {error}"),
            &[Refusal::new(codes::SYNTAX_INVALID, None, error.to_string())],
        )
    })?;
    let usable = |factor: f64| factor.is_finite() && factor > 0.0;
    if !usable(file.units.length_to_mm) || !usable(file.units.angle_to_radians) {
        return Err(failure(
            "the file's units cannot be converted",
            &[Refusal::new(
                codes::UNIT_UNSUPPORTED,
                None,
                format!(
                    "a length unit of {} the reader cannot convert",
                    file.units.length_unit_name
                ),
            )],
        ));
    }
    let weld = file
        .units
        .uncertainty_mm
        .unwrap_or(DEFAULT_UNCERTAINTY)
        .max(precision.linear_agreement);
    let reader = Reader {
        graph: &file.graph,
        scale: file.units.length_to_mm,
        angle: file.units.angle_to_radians,
        weld,
        rational_tolerance: RATIONAL_TOLERANCE,
    };
    let (shapes, occurrences) =
        find_shapes(&reader).map_err(|refusal| failure(refusal.text(), &[refusal]))?;
    if shapes.is_empty() {
        return Err(failure(
            "the file holds no solid or shell model to import",
            &[Refusal::new(
                codes::ENTITY_UNSUPPORTED,
                None,
                "no MANIFOLD_SOLID_BREP, BREP_WITH_VOIDS, FACETED_BREP or SHELL_BASED_SURFACE_MODEL in the file",
            )],
        ));
    }
    let mut notes = Vec::new();
    if occurrences > 0 {
        notes.push(format!(
            "the file is an assembly with {occurrences} occurrence(s); the first product's shape was \
             read in its own frame and the others were not placed"
        ));
    }

    // The exact route, and once more turned inside out if the file's
    // shells face into their material.
    let mut importer = Importer::new(&reader, shapes.clone());
    let mut refusals = match exact_route(&mut importer, precision, false) {
        Ok(outcome) => return Ok(finish(outcome, &notes)),
        Err(ExactDecline::Inverted) => {
            importer = Importer::new(&reader, shapes.clone());
            match exact_route(&mut importer, precision, true) {
                Ok(outcome) => return Ok(finish(outcome, &notes)),
                Err(ExactDecline::Inverted) => vec![Refusal::new(
                    codes::GAP_EXCEEDS_TOLERANCE,
                    None,
                    "the shells enclose no positive volume either way round",
                )],
                Err(ExactDecline::Refused(refusals)) => refusals,
                Err(ExactDecline::Fatal(refusal)) => {
                    return Err(failure(refusal.text(), &[refusal]));
                }
            }
        }
        Err(ExactDecline::Refused(refusals)) => refusals,
        Err(ExactDecline::Fatal(refusal)) => return Err(failure(refusal.text(), &[refusal])),
    };
    refusals.sort_by(|a, b| a.entity.cmp(&b.entity).then_with(|| a.code.cmp(b.code)));
    refusals.dedup();

    // The reference mesh.
    let extent = importer.extent();
    let epsilon = precision
        .linear_agreement
        .max(precision.modeling_resolution)
        .max(1.0e-8)
        * 16.0;
    let mut chord = (extent * MESH_CHORD_FRACTION).max(epsilon * 4.0);
    let mut polygons = Vec::new();
    for _ in 0..5 {
        polygons = mesh::build(&importer, chord);
        if polygons.len() <= MESH_TRIANGLE_BUDGET {
            break;
        }
        chord *= 2.0;
    }
    let topology = faceted_boolean::topology_from_reference_polygons(polygons, precision);
    let Some(topology) = topology else {
        return Err(failure(
            "the part could not be read exactly and its reference mesh does not close",
            &refusals,
        ));
    };
    let validation = validator::validate(&topology, precision.linear_agreement);
    if !validation.diagnostics.is_empty() {
        let mut diagnostics = refusals.clone();
        diagnostics.push(Refusal::new(
            codes::SHELL_OPEN,
            None,
            format!(
                "the reference mesh is not a closed solid: {}",
                validation
                    .diagnostics
                    .iter()
                    .take(4)
                    .map(|diagnostic| format!(
                        "{} at {}",
                        diagnostic.code.as_str(),
                        diagnostic.path
                    ))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        ));
        return Err(failure(
            "the part could not be read exactly and its reference mesh does not close",
            &diagnostics,
        ));
    }
    let face_sources: Vec<u64> = topology
        .faces
        .iter()
        .map(|face| match face.value.role {
            FaceRole::FeatureSide(ordinal) => importer
                .faces
                .get(ordinal as usize)
                .map_or(0, |face| face.id),
            _ => 0,
        })
        .collect();
    let edge_sources = vec![None; topology.edges.len()];
    let mut warnings = Vec::with_capacity(refusals.len() + 1);
    let mut summary = format!(
        "The part opened as a reference mesh: {} of its {} faces could not be read into the \
         kernel's vocabulary, so every face was triangulated ({} triangles at a {chord:.3} mm chord) \
         and the body is a faceted approximation. Its faces, edges and measures approximate the \
         part rather than certifying it.",
        refusals
            .iter()
            .filter(|refusal| refusal.entity.is_some())
            .count(),
        importer.faces.len(),
        topology.faces.len(),
    );
    for note in &notes {
        summary.push(' ');
        summary.push_str(note);
    }
    summary.push_str(" The refusals: ");
    summary.push_str(
        &refusals
            .iter()
            .map(|refusal| format!("{} ({})", refusal.code, refusal.text()))
            .collect::<Vec<_>>()
            .join("; "),
    );
    // The deviation: the mesh stands off the surfaces it stands in for by
    // up to the chord, where the exact tier holds to the linear agreement.
    warnings.push(Diagnostic {
        code: DiagnosticCode::new(codes::FACETED_APPROXIMATION),
        severity: DiagnosticSeverity::Warning,
        stage: KernelStage::Construction,
        message: summary,
        subjects: Vec::new(),
        path: Vec::new(),
        measurement: Some(DiagnosticMeasurement {
            quantity: QuantityKind::Length,
            measured: chord,
            allowed: NumericInterval {
                min: None,
                max: Some(precision.linear_agreement),
            },
        }),
        details: BTreeMap::new(),
    });
    warnings.extend(
        refusals
            .iter()
            .map(|refusal| refusal.diagnostic(DiagnosticSeverity::Warning)),
    );
    warnings.extend(
        importer
            .approximations
            .iter()
            .map(|refusal| refusal.diagnostic(DiagnosticSeverity::Warning)),
    );
    Ok(ImportOutcome {
        topology,
        rung: RUNG_FACETED,
        warnings,
        face_sources,
        edge_sources,
    })
}

enum ExactDecline {
    /// Every face was read but the shells face into their material.
    Inverted,
    /// One or more faces or shells were refused, by name.
    Refused(Vec<Refusal>),
    /// The file's structure could not be read at all.
    Fatal(Refusal),
}

/// The exact route: every face conformed, assembled and validated.
fn exact_route(
    importer: &mut Importer<'_>,
    precision: PrecisionPolicy,
    flip: bool,
) -> Result<ImportOutcome, ExactDecline> {
    importer.read_faces(flip).map_err(ExactDecline::Fatal)?;
    importer.read_edges();
    let (built, mut refusals) = importer.build_faces();
    refusals.extend(importer.assemble(&built));
    if !refusals.is_empty() {
        return Err(ExactDecline::Refused(refusals));
    }
    let validation = validator::validate(&importer.topology, precision.linear_agreement);
    if validation.diagnostics.is_empty() {
        return Ok(ImportOutcome {
            topology: importer.topology.clone(),
            rung: RUNG_EXACT,
            warnings: importer
                .approximations
                .iter()
                .map(|refusal| refusal.diagnostic(DiagnosticSeverity::Warning))
                .collect(),
            face_sources: importer.face_sources.clone(),
            edge_sources: importer.edge_sources.clone(),
        });
    }
    let only_inverted = !flip
        && validation.diagnostics.iter().all(|diagnostic| {
            matches!(
                diagnostic.code,
                validator::DiagnosticCode::SolidVolumeNonPositive
                    | validator::DiagnosticCode::FaceOrientationInvalid
            ) && (diagnostic.path.starts_with("shape/") || diagnostic.path.starts_with("shell/"))
        });
    if only_inverted {
        return Err(ExactDecline::Inverted);
    }
    // The conformed B-rep does not meet the kernel's agreement: name the
    // faces, with the worst measure each.
    let mut by_face: BTreeMap<String, (validator::DiagnosticCode, f64, f64)> = BTreeMap::new();
    for diagnostic in &validation.diagnostics {
        let subject = diagnostic
            .path
            .split('/')
            .take(2)
            .collect::<Vec<_>>()
            .join("/");
        let entry = by_face.entry(subject).or_insert((
            diagnostic.code,
            diagnostic.measured.unwrap_or(f64::NAN),
            diagnostic.allowed.unwrap_or(f64::NAN),
        ));
        if diagnostic.measured.unwrap_or(0.0) > entry.1 {
            *entry = (
                diagnostic.code,
                diagnostic.measured.unwrap_or(f64::NAN),
                diagnostic.allowed.unwrap_or(f64::NAN),
            );
        }
    }
    let refusals = by_face
        .into_iter()
        .map(|(subject, (code, measured, allowed))| {
            let entity = subject
                .strip_prefix("face/")
                .and_then(|id| id.parse::<u64>().ok())
                .and_then(|kernel_id| {
                    importer
                        .topology
                        .faces
                        .iter()
                        .position(|face| face.id.get() == kernel_id)
                        .and_then(|index| importer.face_sources.get(index).copied())
                });
            let mut refusal = Refusal::new(
                codes::GAP_EXCEEDS_TOLERANCE,
                entity,
                format!(
                    "the conformed face does not meet the kernel's agreement: {} at {subject}",
                    code.as_str()
                ),
            );
            if measured.is_finite() && allowed.is_finite() {
                refusal = refusal.with_measure(measured, allowed);
            }
            refusal
        })
        .collect();
    Err(ExactDecline::Refused(refusals))
}

/// An exact import with the file-level notes (an assembly's unplaced
/// occurrences) attached as information.
fn finish(mut outcome: ImportOutcome, notes: &[String]) -> ImportOutcome {
    for note in notes {
        outcome.warnings.push(Diagnostic {
            code: DiagnosticCode::new(codes::ENTITY_UNSUPPORTED),
            severity: DiagnosticSeverity::Info,
            stage: KernelStage::Construction,
            message: note.clone(),
            subjects: Vec::new(),
            path: Vec::new(),
            measurement: None,
            details: BTreeMap::new(),
        });
    }
    outcome
}
