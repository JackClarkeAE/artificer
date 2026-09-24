//! A fillet whose radius changes along a straight convex edge between two
//! flat faces, published as an approximation and labelled as one (ADR 0056,
//! F5, first slice).
//!
//! With a radius that runs linearly from one end of the edge to the other,
//! the rolling ball's centre still runs along a straight line and the band
//! it sweeps is a cone of revolution about that line — exact, and in the
//! vocabulary. What is not in the vocabulary is how that band *ends*: the
//! cap square to the edge cuts the leaning cone in a conic that no pcurve
//! this kernel carries can write on a cone. Rather than a carrier the
//! validator cannot certify, the band is carried here as the faceted tier
//! would carry it, and says so.
//!
//! The removal solid — the corner the ball cannot reach, swept along the
//! edge — is lofted between two polygonal sections, one at each end, whose
//! arc is replaced by a polyline of equal chords. Every vertex of the section
//! is an affine function of the radius, so corresponding segments of the two
//! sections are parallel and every wall of the loft is a plane, which the
//! loft recognises as such; the difference through the Boolean ladder is
//! then exact for that polyhedron. The approximation is entirely in the
//! tool, and it is measured: the chords' sagitta at the larger radius is the
//! band's greatest departure from the true cone, and the material removed is
//! certified to lie between the closed form for the cone and that form plus
//! what the chords add.
//!
//! The result is `Tier::Approximate`, under a rung ending in `/faceted`,
//! with a warning that carries the measured deviation.

use artificer_protocol::{
    BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION, DiagnosticMeasurement,
    DiagnosticSeverity, EntityKind, EntityRef, KernelStage, LoftOperation, LoftSection,
    NumericInterval, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2, PlanarRegion2,
    Point2 as ProtocolPoint2, Point3 as ProtocolPoint3, PrecisionPolicy, QuantityKind, RequestId,
    Vector3 as ProtocolVector3,
};

use crate::edge_finish_apart::{Wedge, body_reach, edges_run_out_of_the_body, read_wedge};
use crate::topology::Point3;
use crate::{
    ExecutionOutcome, KernelError, KernelErrorCode, ProtocolDiagnostic, ProtocolDiagnosticCode,
    Snapshot, error, simple_diagnostic,
};

/// The rung this route publishes under: approximate, and named so.
pub(crate) const RUNG: &str = "variable-radius/faceted";

/// The caveat every result of this route carries.
pub(crate) const APPROXIMATION: &str = "EDGE_FINISH_VARIABLE_RADIUS_FACETED_APPROXIMATION";

/// The fewest and the most chords the band's arc is drawn with.
const CHORDS: (usize, usize) = (8, 128);

fn refuse(snapshot: &Snapshot, code: &'static str, message: impl Into<String>) -> KernelError {
    let message = message.into();
    error(
        KernelErrorCode::InvalidInput,
        KernelStage::Preflight,
        snapshot.id(),
        message.clone(),
        vec![simple_diagnostic(code, KernelStage::Preflight, &message)],
    )
}

/// Fillets one convex straight edge with a radius running linearly from
/// `radii[0]` at the edge's first vertex to `radii[1]` at its second.
pub(crate) fn finish(
    input: &Snapshot,
    target: EntityRef,
    radii: [f64; 2],
    precision: PrecisionPolicy,
) -> Result<ExecutionOutcome, KernelError> {
    if target.snapshot != input.id() || target.kind != EntityKind::Edge {
        return Err(refuse(
            input,
            "VARIABLE_RADIUS_TARGET_INVALID",
            "The target must be an edge of the body this feature is being built on.",
        ));
    }
    let floor = precision.min_feature_size;
    if radii
        .iter()
        .any(|radius| !radius.is_finite() || *radius < floor)
    {
        return Err(refuse(
            input,
            "VARIABLE_RADIUS_DISTANCE_INVALID",
            "Both radii of a variable fillet must be positive lengths above the feature floor.",
        ));
    }
    let edge = input
        .topology
        .edges
        .iter()
        .position(|edge| edge.id.get() == target.entity.0)
        .ok_or_else(|| {
            refuse(
                input,
                "VARIABLE_RADIUS_TARGET_INVALID",
                "That edge is not part of this body.",
            )
        })?;
    let wedge = read_wedge(&input.topology, edge, precision).map_err(|apart| {
        refuse(
            input,
            "VARIABLE_RADIUS_EDGE_UNSUPPORTED",
            format!(
                "A variable-radius fillet takes a convex straight edge between two flat faces. {}",
                apart.message
            ),
        )
    })?;
    let largest = radii[0].max(radii[1]);
    if !edges_run_out_of_the_body(&input.topology, &[target], largest) {
        return Err(refuse(
            input,
            "VARIABLE_RADIUS_EDGE_UNSUPPORTED",
            "A variable-radius fillet runs out past both ends of its edge, and this edge runs \
             on into material at one of them. Finish it at a constant radius instead.",
        ));
    }

    // How finely the arc is drawn: chords whose sagitta at the larger radius
    // meets the approximation budget, within the bounds above.
    let half = wedge.interior / 2.0;
    let sweep = std::f64::consts::PI - wedge.interior;
    let budget = precision.approximation_budget.max(1.0e-12);
    let chord_angle = 2.0 * (1.0 - budget / largest).clamp(-1.0, 1.0).acos();
    let chords = if chord_angle > 0.0 {
        (sweep / chord_angle).ceil() as usize
    } else {
        CHORDS.1
    }
    .clamp(CHORDS.0, CHORDS.1);
    let sagitta = largest * (1.0 - (sweep / (2.0 * chords as f64)).cos());

    // The two sections, each run out past its end — far enough for a
    // constant radius, and no further than keeps the extrapolated radius at
    // half its end value for a changing one.
    let slope = (radii[1] - radii[0]) / wedge.length;
    let reach = body_reach(&input.topology, largest);
    let overshoot = |radius: f64| {
        if slope.abs() <= f64::EPSILON {
            reach
        } else {
            reach.min(radius / (2.0 * slope.abs()))
        }
    };
    let stations = [
        (
            wedge.endpoints[0] + wedge.along * -overshoot(radii[0]),
            radii[0] - slope * overshoot(radii[0]),
        ),
        (
            wedge.endpoints[1] + wedge.along * overshoot(radii[1]),
            radii[1] + slope * overshoot(radii[1]),
        ),
    ];
    let lateral = 2.0 * largest;
    let sections = stations
        .iter()
        .map(|(origin, radius)| section(&wedge, *origin, *radius, half, sweep, chords, lateral))
        .collect::<Vec<_>>();
    let empty = crate::NativeKernel::empty();
    let tool = crate::NativeKernel::execute(
        &empty,
        &crate::ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("variable-radius-tool"),
            expected_snapshot: empty.id(),
            precision,
            command: artificer_protocol::KernelCommand::LoftPlanarSections {
                sections,
                operation: LoftOperation::New,
            },
        },
        &crate::CancellationToken::new(),
    )
    .map_err(|error| {
        refuse(
            input,
            "VARIABLE_RADIUS_CONSTRUCTION_FAILED",
            format!("The fillet's own removal solid could not be built ({error})."),
        )
    })?
    .snapshot;

    let request = BooleanRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("variable-radius-fillet"),
        expected_target_snapshot: input.id(),
        expected_tool_snapshot: tool.id(),
        precision,
        operation: BooleanOperation::Difference,
    };
    let mut outcome = crate::NativeKernel::execute_boolean(
        input,
        &tool,
        &request,
        &crate::CancellationToken::new(),
    )
    .map_err(|error| {
        refuse(
            input,
            "VARIABLE_RADIUS_CONSTRUCTION_FAILED",
            format!("The fillet's removal solid could not be cut from the body ({error})."),
        )
    })?;
    if outcome
        .report
        .rung
        .as_deref()
        .is_some_and(|rung| rung.ends_with("/faceted"))
    {
        return Err(refuse(
            input,
            "VARIABLE_RADIUS_CONSTRUCTION_FAILED",
            "The fillet's removal solid reached the Boolean's faceted tier, which would stack a \
             second approximation on the first; nothing is published from it.",
        ));
    }

    // Certify the removal against the cone's closed form. The chords lie
    // inside the arc, so the polyhedron takes at least the cone's corner and
    // at most that plus the segments between the chords and the arc.
    let corner = |radius: f64| radius * radius * (half.cos() / half.sin() - 0.5 * sweep);
    let mean_square = (radii[0] * radii[0] + radii[0] * radii[1] + radii[1] * radii[1]) / 3.0;
    let exact = corner(1.0) * mean_square * wedge.length;
    let segments = 0.5 * (sweep - chords as f64 * (sweep / chords as f64).sin());
    let extra = segments * mean_square * wedge.length;
    let removed = input.measures().volume - outcome.snapshot.measures().volume;
    let slack = precision
        .linear_agreement
        .max(1.0e-9)
        .mul_add(input.measures().volume, 1.0e-9);
    if removed < exact - slack || removed > exact + extra + slack {
        return Err(refuse(
            input,
            "VARIABLE_RADIUS_CONSTRUCTION_FAILED",
            format!(
                "The variable fillet removed {removed:.6} where its cone's corner is {exact:.6} \
                 and the chords could add at most {extra:.6}; nothing is published from a route \
                 that cannot prove its own answer."
            ),
        ));
    }

    outcome.report.rung = Some(RUNG.to_owned());
    outcome.report.warnings.push(ProtocolDiagnostic {
        code: ProtocolDiagnosticCode::new(APPROXIMATION),
        severity: DiagnosticSeverity::Warning,
        stage: KernelStage::Construction,
        message: format!(
            "A fillet whose radius changes along its edge is carried as {chords} flat facets \
             rather than as the cone it truly is, because the cone's ends lie outside this \
             kernel's curve vocabulary. The facets depart from the true band by at most \
             {sagitta:.3e} (the chords' sagitta at the larger radius), against an approximation \
             budget of {budget:.3e}; the volume removed is certified between the cone's closed \
             form and that form plus the chords' segments. The body's band faces, edges and \
             measures approximate the true fillet rather than certifying it."
        ),
        subjects: vec![artificer_protocol::DiagnosticSubject::Entity { entity: target }],
        path: Vec::new(),
        measurement: Some(DiagnosticMeasurement {
            quantity: QuantityKind::Length,
            measured: sagitta,
            allowed: NumericInterval {
                min: None,
                max: Some(budget),
            },
        }),
        details: std::collections::BTreeMap::new(),
    });
    Ok(outcome)
}

/// One section of the removal solid, square to the edge at `origin` with
/// the ball's radius there: the corner region between the two faces and the
/// arc, the arc drawn as `chords` equal chords, and the two straight sides
/// run out into the air past each face.
fn section(
    wedge: &Wedge,
    origin: Point3,
    radius: f64,
    half: f64,
    sweep: f64,
    chords: usize,
    lateral: f64,
) -> LoftSection {
    let [first_normal, second_normal] = wedge.normals;
    let [into_first, into_second] = wedge.into;
    let centre = origin + wedge.bisector * (radius / half.sin());
    // The band meets the first face at the foot of the perpendicular from
    // the ball's centre, and the second likewise; the arc between them turns
    // from the second face's normal to the first's.
    let toe = centre + first_normal * radius;
    let heel = centre + second_normal * radius;
    let behind_first = heel + into_first * -lateral;
    let behind_second = toe + into_second * -lateral;
    let far = origin + into_first * -lateral + into_second * -lateral;
    let mut points = vec![far, behind_first, heel];
    let sine = sweep.sin();
    for step in 1..chords {
        let t = step as f64 / chords as f64;
        let direction = if sine.abs() <= f64::EPSILON {
            second_normal
        } else {
            (second_normal * ((1.0 - t) * sweep).sin() + first_normal * (t * sweep).sin()) / sine
        };
        points.push(centre + direction * radius);
    }
    points.push(toe);
    points.push(behind_second);

    let u = into_first;
    let v = wedge.along.cross(u);
    let plane = |point: Point3| {
        let delta = point - origin;
        ProtocolPoint2::new(delta.dot(u), delta.dot(v))
    };
    let planar = points.iter().map(|point| plane(*point)).collect::<Vec<_>>();
    // Wound to enclose material: the sign of the polygon's area is the
    // frame's handedness across the wedge, so it is measured, not assumed.
    let twice_area: f64 = (0..planar.len())
        .map(|index| {
            let start = planar[index];
            let end = planar[(index + 1) % planar.len()];
            start.x.mul_add(end.y, -(end.x * start.y))
        })
        .sum();
    let ordered: Vec<ProtocolPoint2> = if twice_area >= 0.0 {
        planar
    } else {
        planar.into_iter().rev().collect()
    };
    let curves = (0..ordered.len())
        .map(|index| PlanarCurve2::Line {
            start: ordered[index],
            end: ordered[(index + 1) % ordered.len()],
        })
        .collect();
    LoftSection {
        frame: PlanarFrame3::new(
            ProtocolPoint3::new(origin.x, origin.y, origin.z),
            ProtocolVector3::new(u.x, u.y, u.z),
            ProtocolVector3::new(v.x, v.y, v.z),
        ),
        profile: PlanarProfile2 {
            regions: vec![PlanarRegion2 {
                outer: PlanarLoop2 { curves },
                holes: Vec::new(),
            }],
        },
    }
}
