//! Exact whole-body patterns.
//!
//! An instance of a pattern is the body itself under a rigid translation,
//! so every carrier stays exactly the carrier it was: a cylinder patterns
//! as cylinders and a blend as blends, with the same face, edge and vertex
//! counts per copy and the same volume. Nothing here tessellates.
//!
//! What differs between patterns is how the copies meet. Instances that
//! clear one another are separate solids of one body, placed side by side
//! in a single topology. Instances that touch or overlap are merged
//! material, which belongs to the Boolean ladder rather than here, so the
//! caller unions them through the engine instead.

use crate::topology::{CoedgeKey, EdgeKey, FaceKey, LoopKey, ShellKey, Vector3, VertexKey};
use crate::topology::{EntityId, Record, Topology};

/// Whether every instance of a pattern along a unit `direction` clears the
/// one before it.
///
/// The body's own extent along the direction is bounded by the support of
/// its axis-aligned bounds, which no committed carrier can exceed. When
/// the step is longer than that extent, a plane perpendicular to the
/// direction separates each pair of instances, so the copies are disjoint
/// without any Boolean classification. The test is conservative: a body
/// whose bounds overlap may still have disjoint copies, and those go
/// through the Boolean ladder instead of being assumed apart.
pub(crate) fn instances_clear_one_another(
    bounds: Option<artificer_protocol::Aabb3>,
    direction: Vector3,
    spacing: f64,
    tolerance: f64,
) -> bool {
    let Some(bounds) = bounds else {
        return false;
    };
    let extent = direction.x.abs() * (bounds.max.x - bounds.min.x)
        + direction.y.abs() * (bounds.max.y - bounds.min.y)
        + direction.z.abs() * (bounds.max.z - bounds.min.z);
    extent.is_finite() && spacing > extent + tolerance
}

/// Appends `addition` to `base` as further solids of one topology.
///
/// Every arena index and every entity identifier is shifted past the ones
/// already in use, so the two bodies keep their own incidence exactly and
/// nothing collides. Both operands must be valid on their own, and their
/// solids must be disjoint; this merges records, it does not classify
/// material.
pub(crate) fn merge_disjoint(base: &Topology, addition: &Topology) -> Topology {
    let mut output = base.clone();
    let vertices = base.vertices.len();
    let edges = base.edges.len();
    let coedges = base.coedges.len();
    let loops = base.loops.len();
    let faces = base.faces.len();
    let shells = base.shells.len();
    let identifiers = highest_identifier(base);
    let renumber = |record: EntityId| EntityId::from_raw(record.get() + identifiers);

    for vertex in &addition.vertices {
        output.vertices.push(Record {
            id: renumber(vertex.id),
            value: vertex.value,
        });
    }
    for edge in &addition.edges {
        let mut value = edge.value;
        value.vertices = value.vertices.map(|key| VertexKey(key.0 + vertices));
        output.edges.push(Record {
            id: renumber(edge.id),
            value,
        });
    }
    for coedge in &addition.coedges {
        let mut value = coedge.value;
        value.edge = EdgeKey(value.edge.0 + edges);
        output.coedges.push(Record {
            id: renumber(coedge.id),
            value,
        });
    }
    for loop_record in &addition.loops {
        let mut value = loop_record.value.clone();
        for coedge in &mut value.coedges {
            *coedge = CoedgeKey(coedge.0 + coedges);
        }
        output.loops.push(Record {
            id: renumber(loop_record.id),
            value,
        });
    }
    for face in &addition.faces {
        let mut value = face.value.clone();
        value.outer_loop = LoopKey(value.outer_loop.0 + loops);
        for inner in &mut value.inner_loops {
            *inner = LoopKey(inner.0 + loops);
        }
        output.faces.push(Record {
            id: renumber(face.id),
            value,
        });
    }
    for shell in &addition.shells {
        let mut value = shell.value.clone();
        for face in &mut value.faces {
            *face = FaceKey(face.0 + faces);
        }
        output.shells.push(Record {
            id: renumber(shell.id),
            value,
        });
    }
    for solid in &addition.solids {
        let mut value = solid.value.clone();
        value.outer_shell = ShellKey(value.outer_shell.0 + shells);
        for inner in &mut value.inner_shells {
            *inner = ShellKey(inner.0 + shells);
        }
        output.solids.push(Record {
            id: renumber(solid.id),
            value,
        });
    }
    output
}

/// The largest identifier any record in `topology` uses, so a merge can
/// start numbering past it.
fn highest_identifier(topology: &Topology) -> u64 {
    let mut highest = 0;
    let mut consider = |id: EntityId| highest = highest.max(id.get());
    for record in &topology.vertices {
        consider(record.id);
    }
    for record in &topology.edges {
        consider(record.id);
    }
    for record in &topology.coedges {
        consider(record.id);
    }
    for record in &topology.loops {
        consider(record.id);
    }
    for record in &topology.faces {
        consider(record.id);
    }
    for record in &topology.shells {
        consider(record.id);
    }
    for record in &topology.solids {
        consider(record.id);
    }
    highest
}
