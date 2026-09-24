//! Stitch (ADR 0056, S3): several sheets welded along their boundaries
//! into one body, and into a solid when every boundary edge pairs.
//!
//! The sheets are merged into one topology and every boundary edge is
//! matched against every other: two that share both endpoints and their
//! midpoint within the precision policy's linear agreement, scaled to the
//! bodies as the Boolean's sewer scales it, are one edge, and the vertices
//! they end at are welded through a union-find so a corner three patches
//! meet at becomes one vertex. Nothing is moved: the agreement is what the
//! validator holds every edge and pcurve to, so a wider weld would only
//! publish a body the validator refuses. A boundary edge that lines up
//! with another — same length within a twentieth, endpoints within a tenth
//! of its length — but not within the agreement is a gap, and the stitch
//! refuses by name with the gap measured rather than leave a seam open by
//! accident. A boundary edge that lines up with nothing stays a boundary
//! edge: an open result is a sheet.
//!
//! Faces meeting across a welded edge must traverse it in opposite senses.
//! Where they do not, the sheet on the far side was drawn facing the other
//! way, and it is turned over as a whole; a set that cannot be made
//! consistent is refused. A component whose every edge pairs encloses a
//! volume: it is turned to face outward, if it does not already, and
//! becomes a solid.

use std::collections::BTreeMap;

use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, KernelError, KernelErrorCode, KernelStage, PrecisionPolicy,
    SnapshotId, StitchRequest,
};

use crate::analytic_extrusion::{allocate_id, merge_topologies};
use crate::sheet::{self, SheetResult};
use crate::sheet_sew::{assign_shells, face_components};
use crate::topology::{
    CoedgeKey, EdgeKey, EntityId, Point3, Record, Shell, ShellKey, Solid, Topology, VertexKey,
};
use crate::{
    CancellationToken, ExecutionOutcome, NativeKernel, Snapshot, error, simple_diagnostic,
};

/// Why a stitch was refused.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum StitchError {
    /// Two boundary edges line up but not within the tolerance.
    Gap { gap: f64, allowed: f64 },
    /// The sheets cannot be given one consistent orientation.
    Orientation,
    /// A closed component encloses no volume either way up.
    Degenerate,
    /// A face could not be turned over.
    Reversal,
}

/// The stitched body.
pub(crate) struct Stitched {
    pub(crate) topology: Topology,
    /// Whether every boundary edge paired, so the result is solid.
    pub(crate) closed: bool,
}

/// Stitches sheets within the precision policy's agreement.
pub(crate) fn stitch(
    sheets: Vec<Topology>,
    precision: PrecisionPolicy,
) -> Result<Stitched, StitchError> {
    let merged = merge_topologies(sheets);
    let scale = merged
        .vertices
        .iter()
        .map(|vertex| {
            let point = vertex.value.point;
            point.x.abs().max(point.y.abs()).max(point.z.abs())
        })
        .fold(1.0_f64, f64::max);
    let allowed = crate::sheet_sew::weld_distance(precision, scale);

    // The boundary, with what each edge looks like from outside.
    let uses = sheet::edge_use_counts(&merged);
    let boundary: Vec<usize> = uses
        .iter()
        .enumerate()
        .filter(|(_, count)| **count == 1)
        .map(|(index, _)| index)
        .collect();
    let ends = |edge: usize| -> [Point3; 2] { merged.edges[edge].value.endpoints() };
    let middle = |edge: usize| -> Point3 {
        let edge = &merged.edges[edge].value;
        edge.curve
            .evaluate((edge.parameter_range.start + edge.parameter_range.end) / 2.0)
    };
    let length = |edge: usize| merged.edges[edge].value.length();
    // The gap between two boundary edges, the better way round: the
    // farthest apart their corresponding endpoints and midpoints are.
    let gap = |first: usize, second: usize| -> (f64, bool) {
        let [a0, a1] = ends(first);
        let [b0, b1] = ends(second);
        let aligned = a0.distance(b0).max(a1.distance(b1));
        let swapped = a0.distance(b1).max(a1.distance(b0));
        let along = middle(first).distance(middle(second));
        if aligned <= swapped {
            (aligned.max(along), true)
        } else {
            (swapped.max(along), false)
        }
    };

    let mut paired = vec![false; merged.edges.len()];
    // Welded edge -> (kept edge, whether it runs the same way).
    let mut welded: BTreeMap<usize, (usize, bool)> = BTreeMap::new();
    let mut union = UnionFind::new(merged.vertices.len());
    for (position, &first) in boundary.iter().enumerate() {
        if paired[first] {
            continue;
        }
        let mut best: Option<(usize, f64, bool)> = None;
        for &second in &boundary[position + 1..] {
            if paired[second] {
                continue;
            }
            let longer = length(first).max(length(second));
            if (length(first) - length(second)).abs() > 0.05 * longer {
                continue;
            }
            let (distance, same_way) = gap(first, second);
            if distance > 0.1 * longer {
                continue;
            }
            if best.is_none_or(|(_, previous, _)| distance < previous) {
                best = Some((second, distance, same_way));
            }
        }
        let Some((second, distance, same_way)) = best else {
            continue;
        };
        if distance > allowed {
            return Err(StitchError::Gap {
                gap: distance,
                allowed,
            });
        }
        paired[first] = true;
        paired[second] = true;
        welded.insert(second, (first, same_way));
        let [a0, a1] = merged.edges[first].value.vertices;
        let [b0, b1] = merged.edges[second].value.vertices;
        if same_way {
            union.join(a0.0, b0.0);
            union.join(a1.0, b1.0);
        } else {
            union.join(a0.0, b1.0);
            union.join(a1.0, b0.0);
        }
    }

    // Rebuild with the welds applied: one vertex per union, the welded
    // edges dropped, their coedges pointed at the kept edge.
    let mut topology = Topology::default();
    let mut next_id = 1_u64;
    let mut vertex_map = vec![usize::MAX; merged.vertices.len()];
    for (index, vertex) in merged.vertices.iter().enumerate() {
        let root = union.find(index);
        if vertex_map[root] == usize::MAX {
            vertex_map[root] = topology.vertices.len();
            topology.vertices.push(Record {
                id: allocate_id(&mut next_id),
                value: vertex.value,
            });
        }
        vertex_map[index] = vertex_map[root];
    }
    let mut edge_map = vec![usize::MAX; merged.edges.len()];
    for (index, edge) in merged.edges.iter().enumerate() {
        if welded.contains_key(&index) {
            continue;
        }
        edge_map[index] = topology.edges.len();
        let mut value = edge.value;
        value.vertices = value.vertices.map(|key| VertexKey(vertex_map[key.0]));
        topology.edges.push(Record {
            id: allocate_id(&mut next_id),
            value,
        });
    }
    for (index, &(kept, _)) in &welded {
        edge_map[*index] = edge_map[kept];
    }
    for coedge in &merged.coedges {
        let mut value = coedge.value;
        let source = value.edge.0;
        if let Some((_, same_way)) = welded.get(&source)
            && !same_way
        {
            value.orientation = value.orientation.reversed();
        }
        value.edge = EdgeKey(edge_map[source]);
        topology.coedges.push(Record {
            id: allocate_id(&mut next_id),
            value,
        });
    }
    for loop_record in &merged.loops {
        topology.loops.push(Record {
            id: allocate_id(&mut next_id),
            value: loop_record.value.clone(),
        });
    }
    for face in &merged.faces {
        topology.faces.push(Record {
            id: allocate_id(&mut next_id),
            value: face.value.clone(),
        });
    }

    orient_consistently(&mut topology)?;
    assign_shells(&mut topology, &mut next_id);

    let closed = sheet::edge_use_counts(&topology)
        .iter()
        .all(|count| *count == 2);
    if closed {
        close_into_solids(&mut topology, &mut next_id)?;
    }
    Ok(Stitched { topology, closed })
}

/// Turns faces over until every welded edge is traversed in opposite
/// senses by the two faces that share it.
fn orient_consistently(topology: &mut Topology) -> Result<(), StitchError> {
    let mut edge_coedges: Vec<Vec<(usize, CoedgeKey)>> = vec![Vec::new(); topology.edges.len()];
    for (face_index, face) in topology.faces.iter().enumerate() {
        for loop_key in face.value.loops() {
            for coedge_key in &topology.loops[loop_key.0].value.coedges {
                let edge = topology.coedges[coedge_key.0].value.edge;
                edge_coedges[edge.0].push((face_index, *coedge_key));
            }
        }
    }
    let mut visited = vec![false; topology.faces.len()];
    for start in 0..topology.faces.len() {
        if visited[start] {
            continue;
        }
        visited[start] = true;
        let mut stack = vec![start];
        while let Some(face_index) = stack.pop() {
            let loops: Vec<_> = topology.faces[face_index].value.loops().collect();
            for loop_key in loops {
                let coedges = topology.loops[loop_key.0].value.coedges.clone();
                for coedge_key in coedges {
                    let edge = topology.coedges[coedge_key.0].value.edge;
                    let mine = topology.coedges[coedge_key.0].value.orientation;
                    for &(other_face, other_coedge) in &edge_coedges[edge.0] {
                        if other_face == face_index {
                            continue;
                        }
                        let theirs = topology.coedges[other_coedge.0].value.orientation;
                        let consistent = theirs == mine.reversed();
                        if visited[other_face] {
                            if !consistent {
                                return Err(StitchError::Orientation);
                            }
                            continue;
                        }
                        if !consistent {
                            sheet::reverse_face(topology, other_face)
                                .map_err(|_| StitchError::Reversal)?;
                        }
                        visited[other_face] = true;
                        stack.push(other_face);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Makes every closed component a solid facing outward.
fn close_into_solids(topology: &mut Topology, next_id: &mut u64) -> Result<(), StitchError> {
    let components = face_components(topology);
    let count = components
        .iter()
        .copied()
        .max()
        .map_or(0, |label| label + 1);
    let mut solids = Vec::with_capacity(count);
    for (shell_index, shell) in topology.shells.clone().iter().enumerate() {
        let encloses = |topology: &Topology| {
            let mut probe = topology.clone();
            probe.shells = vec![Record {
                id: EntityId::from_raw(1_000_000),
                value: Shell {
                    faces: shell.value.faces.clone(),
                },
            }];
            probe.solids = vec![Record {
                id: EntityId::from_raw(1_000_001),
                value: Solid {
                    outer_shell: ShellKey(0),
                    inner_shells: Vec::new(),
                },
            }];
            crate::validator::calculate_exact_shell_measures(&probe, None).is_some()
        };
        if !encloses(topology) {
            // Inside out: every face of the component turned over.
            for face in &shell.value.faces {
                sheet::reverse_face(topology, face.0).map_err(|_| StitchError::Reversal)?;
            }
            if !encloses(topology) {
                return Err(StitchError::Degenerate);
            }
        }
        solids.push(Record {
            id: allocate_id(next_id),
            value: Solid {
                outer_shell: ShellKey(shell_index),
                inner_shells: Vec::new(),
            },
        });
    }
    topology.solids = solids;
    Ok(())
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(count: usize) -> Self {
        Self {
            parent: (0..count).collect(),
        }
    }

    fn find(&mut self, index: usize) -> usize {
        let mut root = index;
        while self.parent[root] != root {
            root = self.parent[root];
        }
        let mut current = index;
        while self.parent[current] != root {
            let next = self.parent[current];
            self.parent[current] = root;
            current = next;
        }
        root
    }

    fn join(&mut self, left: usize, right: usize) {
        let left = self.find(left);
        let right = self.find(right);
        if left != right {
            self.parent[right.max(left)] = right.min(left);
        }
    }
}

impl NativeKernel {
    /// Stitches several sheet snapshots into one body (ADR 0056, S3).
    ///
    /// Every boundary edge that pairs with another within the precision
    /// policy's agreement, scaled to the bodies, is welded; a boundary edge
    /// that lines up with another but falls outside it refuses the stitch
    /// by name with the gap measured (`STITCH_GAP_EXCEEDS_TOLERANCE`). A
    /// set whose every edge pairs becomes a solid, validated as one;
    /// otherwise the result is a sheet with the edges that did not pair as
    /// its boundary.
    pub fn stitch_sheets(
        sheets: &[&Snapshot],
        request: &StitchRequest,
        cancellation: &CancellationToken,
    ) -> Result<ExecutionOutcome, KernelError> {
        let input = sheets.first().map_or(SnapshotId::ZERO, |sheet| sheet.id);
        if request.protocol_version != CURRENT_PROTOCOL_VERSION {
            return Err(error(
                KernelErrorCode::Unsupported,
                KernelStage::Protocol,
                input,
                format!(
                    "protocol version {} is unsupported; expected {}",
                    request.protocol_version, CURRENT_PROTOCOL_VERSION
                ),
                vec![simple_diagnostic(
                    "PROTOCOL_VERSION_UNSUPPORTED",
                    KernelStage::Protocol,
                    "The request protocol version is not supported by this kernel build.",
                )],
            ));
        }
        if sheets.is_empty() {
            return Err(sheet::refuse(
                input,
                KernelErrorCode::InvalidInput,
                "STITCH_NOTHING_TO_STITCH",
                "A stitch needs at least one sheet.",
            ));
        }
        if request.expected_snapshots.len() != sheets.len()
            || request
                .expected_snapshots
                .iter()
                .zip(sheets)
                .any(|(expected, sheet)| *expected != sheet.id)
        {
            return Err(error(
                KernelErrorCode::StaleSnapshot,
                KernelStage::Preflight,
                input,
                "the stitch names snapshots other than the ones supplied",
                vec![simple_diagnostic(
                    "STALE_SNAPSHOT",
                    KernelStage::Preflight,
                    "Expected snapshots do not match the supplied immutable inputs.",
                )],
            ));
        }
        crate::validate_precision(input, request.precision)?;
        for sheet_snapshot in sheets {
            if sheet_snapshot
                .precision
                .is_some_and(|precision| precision != request.precision)
            {
                return Err(error(
                    KernelErrorCode::PrecisionPolicyMismatch,
                    KernelStage::Preflight,
                    input,
                    "precision policy cannot change within a snapshot lineage",
                    vec![simple_diagnostic(
                        "PRECISION_POLICY_MISMATCH",
                        KernelStage::Preflight,
                        "The request precision policy differs from a sheet's policy.",
                    )],
                ));
            }
            if !sheet::is_sheet(&sheet_snapshot.topology) {
                return Err(sheet::not_a_sheet(sheet_snapshot.id, "A stitch"));
            }
        }
        crate::check_cancelled(input, cancellation, KernelStage::Preflight)?;
        let stitched = stitch(
            sheets.iter().map(|sheet| sheet.topology.clone()).collect(),
            request.precision,
        )
        .map_err(|reason| match reason {
            StitchError::Gap { gap, allowed } => {
                let message = format!(
                    "two boundary edges line up but are {gap:.3e} apart, beyond the agreement \
                     of {allowed:.3e} the stitch welds within; build the sheets so their edges \
                     meet"
                );
                let mut diagnostic = simple_diagnostic(
                    "STITCH_GAP_EXCEEDS_TOLERANCE",
                    KernelStage::Construction,
                    &message,
                );
                crate::attach_measurement(
                    &mut diagnostic,
                    artificer_protocol::QuantityKind::Length,
                    gap,
                    artificer_protocol::NumericInterval {
                        min: None,
                        max: Some(allowed),
                    },
                );
                error(
                    KernelErrorCode::InvalidInput,
                    KernelStage::Construction,
                    input,
                    message,
                    vec![diagnostic],
                )
            }
            StitchError::Orientation => sheet::refuse(
                input,
                KernelErrorCode::InvalidInput,
                "STITCH_ORIENTATION_CONFLICT",
                "The sheets cannot be given one consistent side: turning one over to agree with a neighbour puts it at odds with another.",
            ),
            StitchError::Degenerate => sheet::refuse(
                input,
                KernelErrorCode::InvalidInput,
                "STITCH_ENCLOSES_NOTHING",
                "The stitched sheets close but enclose no volume.",
            ),
            StitchError::Reversal => sheet::refuse(
                input,
                KernelErrorCode::Unsupported,
                "STITCH_FACE_REVERSAL_UNSUPPORTED",
                "A face carries a curve-on-surface that cannot be turned over.",
            ),
        })?;
        let rung = if stitched.closed {
            sheet::STITCH_SOLID_RUNG
        } else {
            sheet::STITCH_SHEET_RUNG
        };
        sheet::commit_with_precision(
            input,
            request.precision,
            cancellation,
            SheetResult {
                topology: stitched.topology,
                rung,
                warnings: Vec::new(),
            },
        )
    }
}
