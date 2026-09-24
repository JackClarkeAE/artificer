//! Sheet bodies (ADR 0056, Track S): a topology with shells and no solid.
//!
//! A sheet is what the kernel's `Topology` already carries once its solid
//! record is left off: faces on exact carriers, edges shared between the
//! faces that meet, and shells of edge-connected faces. What a sheet has
//! that a solid does not is a boundary — edges used by one coedge only —
//! and what it lacks is an inside, so it has an area and no volume. This
//! module holds the shared vocabulary every sheet operation reads: what a
//! sheet is, where its boundary is, how it is validated and measured, how a
//! face of one is turned over, and how a sheet result is committed as a
//! snapshot with its boundary edges named in the report.

use artificer_protocol::{
    Diagnostic as ProtocolDiagnostic, EntityKind, ExecuteRequest, HistoryRecord, HistoryRelation,
    KernelCommand, KernelError, KernelErrorCode, KernelStage, OperationReport, OperationRole,
    PrecisionPolicy, SnapshotId, ValidationProfile,
};

use crate::mirror::reverse_face_loops;
use crate::topology::{EdgeKey, Plane, Point2, Point3, Surface, Topology, Vector3};
use crate::validator::{self, DiagnosticCode, ShapeMeasures, ValidationReport};
use crate::{
    CancellationToken, ExecutionOutcome, Snapshot, entity_ref, error, generated_history,
    protocol_validation, public_measures, semantic_digest, simple_diagnostic, snapshot_id,
};

/// The rung names every sheet operation reports.
pub(crate) const SURFACE_EXTRUDE_RUNG: &str = "surface/extrude";
pub(crate) const SURFACE_REVOLVE_RUNG: &str = "surface/revolve";
pub(crate) const PLANAR_PATCH_RUNG: &str = "surface/patch";
pub(crate) const STITCH_SHEET_RUNG: &str = "stitch/sheet";
pub(crate) const STITCH_SOLID_RUNG: &str = "stitch/solid";
pub(crate) const THICKEN_RUNG: &str = "thicken/exact";
pub(crate) const THICKEN_APPROXIMATE_RUNG: &str = "thicken/approximate";
pub(crate) const TRIM_RUNG: &str = "trim/plane";

/// Whether a topology is a sheet body: shells with no solid.
pub(crate) fn is_sheet(topology: &Topology) -> bool {
    topology.solids.is_empty() && !topology.shells.is_empty()
}

/// How many coedges use each edge, by edge index.
pub(crate) fn edge_use_counts(topology: &Topology) -> Vec<usize> {
    let mut counts = vec![0_usize; topology.edges.len()];
    for coedge in &topology.coedges {
        if let Some(count) = counts.get_mut(coedge.value.edge.0) {
            *count += 1;
        }
    }
    counts
}

/// The boundary of a sheet: every edge used by exactly one coedge, in
/// edge order.
pub(crate) fn boundary_edges(topology: &Topology) -> Vec<EdgeKey> {
    edge_use_counts(topology)
        .into_iter()
        .enumerate()
        .filter(|(_, count)| *count == 1)
        .map(|(index, _)| EdgeKey(index))
        .collect()
}

/// Validates a sheet body: every family the solid validator runs, with an
/// edge used once admitted as a boundary edge, and the closed-shell and
/// solid families — the Euler characteristic and the positive volume —
/// set aside, since an open shell satisfies neither. The measures are the
/// sheet's: its area, and no volume.
pub(crate) fn validate_sheet(topology: &Topology, linear_tolerance: f64) -> ValidationReport {
    let mut report = validator::validate(topology, linear_tolerance);
    report
        .diagnostics
        .retain(|diagnostic| match diagnostic.code {
            DiagnosticCode::EdgeUseCount => diagnostic.measured != Some(1.0),
            // A sheet's shells belong to no solid.
            DiagnosticCode::ShellUseCount => diagnostic.measured != Some(0.0),
            DiagnosticCode::EulerCharacteristicInvalid | DiagnosticCode::SolidVolumeNonPositive => {
                false
            }
            _ => true,
        });
    report.measures = sheet_measures(topology, report.measures.bounds);
    report
}

/// A sheet's measures: the sum of its faces' exact areas, no volume and no
/// centroid.
pub(crate) fn sheet_measures(
    topology: &Topology,
    bounds: Option<validator::Bounds3>,
) -> ShapeMeasures {
    let surface_area = topology
        .faces
        .iter()
        .map(|face| face_area(topology, &face.value).unwrap_or(f64::NAN))
        .sum::<f64>();
    ShapeMeasures {
        bounds,
        surface_area,
        signed_volume: 0.0,
        centroid: None,
    }
}

/// One face's exact area, by the same closed forms the body measures use.
pub(crate) fn face_area(topology: &Topology, face: &crate::topology::Face) -> Option<f64> {
    let parameter_area =
        validator::face_parameter_area_and_moment(topology, face).map(|(area, _)| area.abs())?;
    let jacobian = match face.surface {
        Surface::Plane(plane) => plane.u.cross(plane.v).length(),
        Surface::Cylinder(cylinder) => cylinder.radius * cylinder.axis.length(),
        Surface::Torus(torus) => return crate::torus_face_area(topology, face, torus),
        Surface::Sphere(sphere) => return crate::sphere_face_area(topology, face, sphere),
        Surface::Cone(cone) => return crate::cone_face_area(topology, face, cone),
        Surface::Ruled(ruled) => return crate::ruled_face_area(topology, face, ruled),
        Surface::Bspline(surface) => return crate::spline_face_area(topology, face, surface),
    };
    let area = parameter_area * jacobian;
    area.is_finite().then_some(area)
}

/// Whether a command may take a sheet as its input snapshot. Everything
/// else that edits the current body assumes a solid, and refuses a sheet
/// by name rather than reading its first solid.
pub(crate) fn command_accepts_sheet(command: &KernelCommand) -> bool {
    matches!(
        command,
        KernelCommand::ThickenSheet { .. }
            | KernelCommand::TrimSheetByPlane { .. }
            | KernelCommand::TransformSnapshot { .. }
            | KernelCommand::MirrorSnapshot { .. }
            | KernelCommand::LinearPatternSnapshot { .. }
    )
}

/// The refusal for an operation that needs a solid and was given a sheet.
pub(crate) fn unsupported_here(snapshot: SnapshotId, what: &str) -> KernelError {
    let message = format!(
        "{what} needs a solid body and the current body is a sheet; thicken it, or stitch it \
         closed, first"
    );
    error(
        KernelErrorCode::Unsupported,
        KernelStage::Preflight,
        snapshot,
        message.clone(),
        vec![simple_diagnostic(
            "SHEET_UNSUPPORTED_HERE",
            KernelStage::Preflight,
            &message,
        )],
    )
}

/// The refusal for a sheet operation given a body that is not a sheet.
pub(crate) fn not_a_sheet(snapshot: SnapshotId, what: &str) -> KernelError {
    let message =
        format!("{what} needs a sheet body; the current body is a solid, or there is no body yet");
    error(
        KernelErrorCode::InvalidInput,
        KernelStage::Preflight,
        snapshot,
        message.clone(),
        vec![simple_diagnostic(
            "SHEET_INPUT_REQUIRED",
            KernelStage::Preflight,
            &message,
        )],
    )
}

/// A refusal by name from a sheet operation.
pub(crate) fn refuse(
    snapshot: SnapshotId,
    code: KernelErrorCode,
    name: &'static str,
    message: impl Into<String>,
) -> KernelError {
    let message = message.into();
    error(
        code,
        KernelStage::Construction,
        snapshot,
        message.clone(),
        vec![simple_diagnostic(name, KernelStage::Construction, &message)],
    )
}

/// Turns one face over: its carrier is reparameterised so that its normal
/// points the other way, and its loops are walked the other way, by the
/// kernel's own convention (the one the mirror uses): a plane swaps its
/// axes, a revolved carrier negates its angular sign, a ruled carrier walks
/// its rails backwards and a B-spline surface walks `u` the other way.
pub(crate) fn reverse_face(topology: &mut Topology, face_index: usize) -> Result<(), &'static str> {
    let mirror: fn(Point2) -> Point2 = {
        let face = &mut topology.faces[face_index].value;
        match &mut face.surface {
            Surface::Plane(plane) => {
                *plane = Plane::new(plane.origin, plane.v, plane.u);
                |point: Point2| Point2::new(point.y, point.x)
            }
            Surface::Cylinder(cylinder) => {
                cylinder.angular_sign = -cylinder.angular_sign;
                |point: Point2| Point2::new(-point.x, point.y)
            }
            Surface::Cone(cone) => {
                cone.angular_sign = -cone.angular_sign;
                |point: Point2| Point2::new(-point.x, point.y)
            }
            Surface::Torus(torus) => {
                torus.angular_sign = -torus.angular_sign;
                |point: Point2| Point2::new(-point.x, point.y)
            }
            Surface::Sphere(sphere) => {
                sphere.angular_sign = -sphere.angular_sign;
                |point: Point2| Point2::new(-point.x, point.y)
            }
            Surface::Ruled(ruled) => {
                *ruled = ruled.reversed_u();
                |point: Point2| Point2::new(1.0 - point.x, point.y)
            }
            Surface::Bspline(surface) => {
                *surface = surface.reversed_u();
                |point: Point2| Point2::new(-point.x, point.y)
            }
        }
    };
    reverse_face_loops(topology, face_index, mirror, &|cylinder| cylinder)
        .map_err(|_| "a face carries a curve-on-surface that cannot be turned over")
}

/// Turns every face of a sheet over, so the sheet faces the other way.
pub(crate) fn reverse_sheet(topology: &mut Topology) -> Result<(), &'static str> {
    for face_index in 0..topology.faces.len() {
        reverse_face(topology, face_index)?;
    }
    Ok(())
}

/// The unit outward normal of a face at a point on its carrier.
pub(crate) fn face_normal_at(face: &crate::topology::Face, point: Point3) -> Option<Vector3> {
    face.surface.outward_normal_at(point)
}

/// What a sheet operation hands back to be committed.
pub(crate) struct SheetResult {
    pub(crate) topology: Topology,
    pub(crate) rung: &'static str,
    pub(crate) warnings: Vec<ProtocolDiagnostic>,
}

/// Commits a sheet operation's result as a snapshot: validated as a sheet
/// when it has no solid and as a solid when it has, measured accordingly,
/// with every entity generated and every boundary edge of a sheet named
/// `boundary_edge[n]` in the history so a later step can select it.
pub(crate) fn commit(
    input: &Snapshot,
    request: &ExecuteRequest,
    cancellation: &CancellationToken,
    result: SheetResult,
) -> Result<ExecutionOutcome, KernelError> {
    commit_with_precision(input.id, request.precision, cancellation, result)
}

pub(crate) fn commit_with_precision(
    input: SnapshotId,
    precision: PrecisionPolicy,
    cancellation: &CancellationToken,
    result: SheetResult,
) -> Result<ExecutionOutcome, KernelError> {
    let SheetResult {
        topology,
        rung,
        warnings,
    } = result;
    crate::check_cancelled(input, cancellation, KernelStage::Construction)?;
    let sheet = is_sheet(&topology);
    let internal_validation = if sheet {
        validate_sheet(&topology, precision.linear_agreement)
    } else {
        validator::validate(&topology, precision.linear_agreement)
    };
    let profile = if sheet {
        ValidationProfile::Sheet
    } else {
        ValidationProfile::Solid
    };
    let validation = protocol_validation(input, profile, &internal_validation);
    if !validation.valid {
        return Err(error(
            KernelErrorCode::ValidationFailed,
            KernelStage::Validation,
            input,
            "candidate topology failed validation and was not committed",
            validation.diagnostics,
        ));
    }
    crate::check_cancelled(input, cancellation, KernelStage::Commit)?;
    let digest = semantic_digest(&topology, precision);
    let output = snapshot_id(digest);
    let measures = public_measures(internal_validation.measures);
    let snapshot = Snapshot {
        id: output,
        semantic_digest: digest,
        precision: Some(precision),
        topology,
        measures,
    };
    let mut history = generated_history(&snapshot);
    if sheet {
        for (ordinal, edge) in boundary_edges(&snapshot.topology).into_iter().enumerate() {
            let id = snapshot.topology.edges[edge.0].id.get();
            history.push(HistoryRecord {
                relation: HistoryRelation::Generated,
                inputs: Vec::new(),
                outputs: vec![entity_ref(snapshot.id, id, EntityKind::Edge)],
                role: Some(OperationRole::new("boundary_edge", Some(ordinal as u32))),
            });
        }
    }
    let mut report = OperationReport {
        input_snapshot: input,
        output_snapshot: output,
        semantic_digest: digest,
        topology: snapshot.counts(),
        bounds: measures.bounds,
        history,
        validation,
        warnings,
        rung: Some(rung.to_owned()),
    };
    report.sort_deterministically();
    Ok(ExecutionOutcome { snapshot, report })
}
