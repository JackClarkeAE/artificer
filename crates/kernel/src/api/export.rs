//! Geometry export routines (STL binary, STL ASCII, Wavefront OBJ).

use std::fmt::Write as _;

pub use crate::StepPlacement;
use crate::{NativeKernel, Snapshot};
use serde::{Deserialize, Serialize};

use crate::api::debug::ApiError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    StlBinary,
    StlAscii,
    Obj,
    /// AP214 `advanced_brep_shape_representation`: the analytic B-rep
    /// itself, nothing tessellated.
    Step,
    /// AP214 faceted surface model: the display triangles in a STEP
    /// wrapper.
    StepFaceted,
}

/// Exports the snapshot's solids as exact AP214 B-rep STEP text. Every
/// face keeps its analytic carrier and every edge its exact curve, so a
/// reader recovers the same volume and area the kernel measures.
pub fn export_step(snapshot: &Snapshot, product_name: &str) -> Result<String, ApiError> {
    NativeKernel::export_step(snapshot, &header_name(product_name)).map_err(ApiError::from)
}

/// Exports several snapshots as the solids of one STEP product.
pub fn export_step_bodies(
    bodies: &[(&Snapshot, &str)],
    product_name: &str,
) -> Result<String, ApiError> {
    NativeKernel::export_step_bodies(bodies, &header_name(product_name)).map_err(ApiError::from)
}

/// Exports several snapshots, each under a rigid placement, as the solids
/// of one STEP product: an assembly's occurrences in their positions.
pub fn export_step_bodies_placed(
    bodies: &[(&Snapshot, &str, StepPlacement)],
    product_name: &str,
) -> Result<String, ApiError> {
    NativeKernel::export_step_bodies_placed(bodies, &header_name(product_name))
        .map_err(ApiError::from)
}

/// Exports the snapshot's display tessellation as a faceted STEP surface
/// model, for consumers that want triangles in a STEP wrapper.
#[must_use]
pub fn export_step_faceted(snapshot: &Snapshot, product_name: &str) -> String {
    NativeKernel::export_step_faceted(snapshot, &header_name(product_name))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExportResult {
    pub format: ExportFormat,
    #[serde(skip)]
    pub data: Vec<u8>,
    pub triangle_count: usize,
}

/// The facet normal of one triangle from its winding, which is what STL
/// readers expect; a carrier normal at one vertex differs from it on every
/// curved face.
fn facet_normal(vertices: [artificer_protocol::Point3; 3]) -> artificer_protocol::Vector3 {
    let [a, b, c] = vertices;
    let ab = (b.x - a.x, b.y - a.y, b.z - a.z);
    let ac = (c.x - a.x, c.y - a.y, c.z - a.z);
    let n = (
        ab.1 * ac.2 - ab.2 * ac.1,
        ab.2 * ac.0 - ab.0 * ac.2,
        ab.0 * ac.1 - ab.1 * ac.0,
    );
    let length = (n.0 * n.0 + n.1 * n.1 + n.2 * n.2).sqrt();
    if length > 0.0 && length.is_finite() {
        artificer_protocol::Vector3::new(n.0 / length, n.1 / length, n.2 / length)
    } else {
        artificer_protocol::Vector3::new(0.0, 0.0, 0.0)
    }
}

/// A name safe to embed in a text header: one line, ASCII printable.
fn header_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_graphic() || character == ' ' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// Exports the snapshot tessellation as a binary STL byte buffer.
pub fn export_stl_binary(snapshot: &Snapshot) -> Result<Vec<u8>, ApiError> {
    let scene = NativeKernel::debug_scene(snapshot);
    let triangle_count = scene.triangles.len();
    let count = u32::try_from(triangle_count).map_err(|_| {
        ApiError::new(
            crate::api::debug::ApiErrorCode::InvalidInput,
            "The model has more triangles than binary STL can count",
        )
    })?;

    let mut buffer = Vec::with_capacity(84 + triangle_count * 50);

    // 80-byte header
    let mut header = [0u8; 80];
    let title = b"Artificer CAD Export";
    header[..title.len()].copy_from_slice(title);
    buffer.extend_from_slice(&header);

    // Number of triangles (u32, little-endian)
    buffer.extend_from_slice(&count.to_le_bytes());

    for tri in &scene.triangles {
        let n = facet_normal(tri.vertices);
        let v0 = tri.vertices[0];
        let v1 = tri.vertices[1];
        let v2 = tri.vertices[2];

        // Normal (3 x f32)
        buffer.extend_from_slice(&(n.x as f32).to_le_bytes());
        buffer.extend_from_slice(&(n.y as f32).to_le_bytes());
        buffer.extend_from_slice(&(n.z as f32).to_le_bytes());

        // Vertices (3 x 3 x f32)
        buffer.extend_from_slice(&(v0.x as f32).to_le_bytes());
        buffer.extend_from_slice(&(v0.y as f32).to_le_bytes());
        buffer.extend_from_slice(&(v0.z as f32).to_le_bytes());

        buffer.extend_from_slice(&(v1.x as f32).to_le_bytes());
        buffer.extend_from_slice(&(v1.y as f32).to_le_bytes());
        buffer.extend_from_slice(&(v1.z as f32).to_le_bytes());

        buffer.extend_from_slice(&(v2.x as f32).to_le_bytes());
        buffer.extend_from_slice(&(v2.y as f32).to_le_bytes());
        buffer.extend_from_slice(&(v2.z as f32).to_le_bytes());

        // Attribute byte count (u16)
        buffer.extend_from_slice(&0u16.to_le_bytes());
    }

    Ok(buffer)
}

/// Exports the snapshot tessellation as an ASCII STL string.
pub fn export_stl_ascii(snapshot: &Snapshot, solid_name: &str) -> Result<String, ApiError> {
    let scene = NativeKernel::debug_scene(snapshot);
    let solid_name = header_name(solid_name);
    let mut out = String::new();

    writeln!(out, "solid {solid_name}").unwrap();
    for tri in &scene.triangles {
        let n = facet_normal(tri.vertices);
        let v0 = tri.vertices[0];
        let v1 = tri.vertices[1];
        let v2 = tri.vertices[2];

        writeln!(out, "  facet normal {} {} {}", n.x, n.y, n.z).unwrap();
        writeln!(out, "    outer loop").unwrap();
        writeln!(out, "      vertex {} {} {}", v0.x, v0.y, v0.z).unwrap();
        writeln!(out, "      vertex {} {} {}", v1.x, v1.y, v1.z).unwrap();
        writeln!(out, "      vertex {} {} {}", v2.x, v2.y, v2.z).unwrap();
        writeln!(out, "    endloop").unwrap();
        writeln!(out, "  endfacet").unwrap();
    }
    writeln!(out, "endsolid {solid_name}").unwrap();

    Ok(out)
}

/// Exports the snapshot tessellation as a Wavefront OBJ string.
pub fn export_obj(snapshot: &Snapshot, model_name: &str) -> Result<String, ApiError> {
    let scene = NativeKernel::debug_scene(snapshot);
    let model_name = header_name(model_name);
    let mut out = String::new();

    writeln!(out, "# Artificer Wavefront OBJ Export: {model_name}").unwrap();
    writeln!(out, "o {model_name}").unwrap();

    let mut vertex_index = 1usize;
    for tri in &scene.triangles {
        for v in &tri.vertices {
            writeln!(out, "v {} {} {}", v.x, v.y, v.z).unwrap();
        }
        for n in &tri.normals {
            writeln!(out, "vn {} {} {}", n.x, n.y, n.z).unwrap();
        }
        writeln!(
            out,
            "f {v0}//{v0} {v1}//{v1} {v2}//{v2}",
            v0 = vertex_index,
            v1 = vertex_index + 1,
            v2 = vertex_index + 2
        )
        .unwrap();
        vertex_index += 3;
    }

    Ok(out)
}
