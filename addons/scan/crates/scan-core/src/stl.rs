//! STL import and export. Scanners and mesh tools emit both flavours, so the
//! reader sniffs ASCII versus binary rather than trusting the `solid` prefix.

use std::fmt;

use artificer_geometry::Point3;

use crate::mesh::TriangleMesh;

/// Coordinates within this distance weld to one vertex on import. STL stores
/// binary32 coordinates, so this absorbs round-trip noise without merging
/// real geometry at millimetre model scale.
pub const STL_WELD_TOLERANCE: f64 = 1e-6;

#[derive(Debug)]
pub enum StlError {
    Truncated,
    Malformed(String),
    EmptyMesh,
}

impl fmt::Display for StlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(f, "stl data ends before the declared triangle count"),
            Self::Malformed(detail) => write!(f, "malformed stl: {detail}"),
            Self::EmptyMesh => write!(f, "stl contains no valid triangles"),
        }
    }
}

impl std::error::Error for StlError {}

pub fn read_stl(bytes: &[u8]) -> Result<TriangleMesh, StlError> {
    let soup = if looks_ascii(bytes) {
        parse_ascii(bytes)?
    } else {
        parse_binary(bytes)?
    };
    TriangleMesh::from_triangle_soup(&soup, STL_WELD_TOLERANCE).ok_or(StlError::EmptyMesh)
}

pub fn write_binary_stl(mesh: &TriangleMesh) -> Vec<u8> {
    let mut out = vec![0u8; 80];
    out.extend_from_slice(&(mesh.triangles().len() as u32).to_le_bytes());
    for face in 0..mesh.triangles().len() {
        let normal = mesh.face_normal(face).unwrap_or_default();
        for value in [normal.x, normal.y, normal.z] {
            out.extend_from_slice(&(value as f32).to_le_bytes());
        }
        for point in mesh.triangle_points(face) {
            for value in [point.x, point.y, point.z] {
                out.extend_from_slice(&(value as f32).to_le_bytes());
            }
        }
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    out
}

/// A file starting with `solid` is only ASCII if triangle keywords follow;
/// plenty of binary exporters also write `solid` into the 80-byte header.
fn looks_ascii(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(512)];
    head.trim_ascii_start().starts_with(b"solid")
        && head
            .windows(5)
            .any(|window| window == b"facet" || window == b"endso")
}

fn parse_binary(bytes: &[u8]) -> Result<Vec<[Point3; 3]>, StlError> {
    if bytes.len() < 84 {
        return Err(StlError::Truncated);
    }
    let count = u32::from_le_bytes([bytes[80], bytes[81], bytes[82], bytes[83]]) as usize;
    let body = &bytes[84..];
    if body.len() < count * 50 {
        return Err(StlError::Truncated);
    }
    let mut soup = Vec::with_capacity(count);
    for triangle in 0..count {
        let record = &body[triangle * 50..triangle * 50 + 50];
        let mut corners = [Point3::default(); 3];
        for (corner, slot) in corners.iter_mut().enumerate() {
            // Skip the 12-byte facet normal; corner data starts at byte 12.
            let base = 12 + corner * 12;
            let read = |offset: usize| {
                f32::from_le_bytes([
                    record[base + offset],
                    record[base + offset + 1],
                    record[base + offset + 2],
                    record[base + offset + 3],
                ]) as f64
            };
            *slot = Point3::new(read(0), read(4), read(8));
        }
        soup.push(corners);
    }
    Ok(soup)
}

fn parse_ascii(bytes: &[u8]) -> Result<Vec<[Point3; 3]>, StlError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| StlError::Malformed("ascii stl is not valid utf-8".into()))?;
    let mut tokens = text.split_ascii_whitespace();
    let mut soup = Vec::new();
    let mut corners: Vec<Point3> = Vec::with_capacity(3);
    while let Some(token) = tokens.next() {
        if token != "vertex" {
            continue;
        }
        let mut coordinate = || -> Result<f64, StlError> {
            tokens
                .next()
                .and_then(|t| t.parse::<f64>().ok())
                .ok_or_else(|| StlError::Malformed("vertex without three coordinates".into()))
        };
        let point = Point3::new(coordinate()?, coordinate()?, coordinate()?);
        corners.push(point);
        if corners.len() == 3 {
            soup.push([corners[0], corners[1], corners[2]]);
            corners.clear();
        }
    }
    if !corners.is_empty() {
        return Err(StlError::Malformed("dangling vertices at end of file".into()));
    }
    Ok(soup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use artificer_geometry::Point3;

    fn unit_quad() -> TriangleMesh {
        let a = Point3::new(0.0, 0.0, 0.0);
        let b = Point3::new(1.0, 0.0, 0.0);
        let c = Point3::new(1.0, 1.0, 0.0);
        let d = Point3::new(0.0, 1.0, 0.0);
        TriangleMesh::from_triangle_soup(&[[a, b, c], [a, c, d]], 1e-9).unwrap()
    }

    #[test]
    fn binary_round_trip_preserves_topology_and_area() {
        let mesh = unit_quad();
        let restored = read_stl(&write_binary_stl(&mesh)).unwrap();
        assert_eq!(restored.positions().len(), 4);
        assert_eq!(restored.triangles().len(), 2);
        assert!((restored.surface_area() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ascii_parses_and_welds() {
        let text = "solid demo\n\
            facet normal 0 0 1\nouter loop\n\
            vertex 0 0 0\nvertex 1 0 0\nvertex 1 1 0\n\
            endloop\nendfacet\n\
            facet normal 0 0 1\nouter loop\n\
            vertex 0 0 0\nvertex 1 1 0\nvertex 0 1 0\n\
            endloop\nendfacet\n\
            endsolid demo\n";
        let mesh = read_stl(text.as_bytes()).unwrap();
        assert_eq!(mesh.positions().len(), 4);
        assert_eq!(mesh.triangles().len(), 2);
    }

    #[test]
    fn binary_header_starting_with_solid_still_parses() {
        let mesh = unit_quad();
        let mut bytes = write_binary_stl(&mesh);
        bytes[..5].copy_from_slice(b"solid");
        let restored = read_stl(&bytes).unwrap();
        assert_eq!(restored.triangles().len(), 2);
    }
}
