//! Wavefront OBJ import: `v` positions and `f` faces (fan-triangulated).
//! Texture/normal indices and other statements are ignored.

use std::fmt;

use artificer_geometry::Point3;

use crate::mesh::TriangleMesh;

#[derive(Debug)]
pub enum ObjError {
    Malformed(String),
    EmptyMesh,
}

impl fmt::Display for ObjError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(detail) => write!(f, "malformed obj: {detail}"),
            Self::EmptyMesh => write!(f, "obj contains no faces"),
        }
    }
}

impl std::error::Error for ObjError {}

pub fn read_obj(bytes: &[u8]) -> Result<TriangleMesh, ObjError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ObjError::Malformed("obj is not valid utf-8".into()))?;
    let mut positions: Vec<Point3> = Vec::new();
    let mut triangles: Vec<[u32; 3]> = Vec::new();
    for (line_number, line) in text.lines().enumerate() {
        let mut tokens = line.split_ascii_whitespace();
        match tokens.next() {
            Some("v") => {
                let mut coordinate = || -> Result<f64, ObjError> {
                    tokens
                        .next()
                        .and_then(|t| t.parse::<f64>().ok())
                        .ok_or_else(|| {
                            ObjError::Malformed(format!("bad vertex at line {}", line_number + 1))
                        })
                };
                positions.push(Point3::new(coordinate()?, coordinate()?, coordinate()?));
            }
            Some("f") => {
                let mut indices: Vec<u32> = Vec::new();
                for token in tokens {
                    let index_text = token.split('/').next().unwrap_or(token);
                    let raw: i64 = index_text.parse().map_err(|_| {
                        ObjError::Malformed(format!("bad face index at line {}", line_number + 1))
                    })?;
                    let resolved = if raw > 0 {
                        raw - 1
                    } else {
                        positions.len() as i64 + raw
                    };
                    if resolved < 0 || resolved as usize >= positions.len() {
                        return Err(ObjError::Malformed(format!(
                            "face index out of range at line {}",
                            line_number + 1
                        )));
                    }
                    indices.push(resolved as u32);
                }
                for i in 1..indices.len().saturating_sub(1) {
                    let (a, b, c) = (indices[0], indices[i], indices[i + 1]);
                    if a != b && b != c && a != c {
                        triangles.push([a, b, c]);
                    }
                }
            }
            _ => {}
        }
    }
    if triangles.is_empty() {
        return Err(ObjError::EmptyMesh);
    }
    TriangleMesh::new(positions, triangles)
        .ok_or_else(|| ObjError::Malformed("non-finite vertex or bad topology".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_with_slashes_parses() {
        let text = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nf 1/1/1 2/2/2 3/3/3 4/4/4\n";
        let mesh = read_obj(text.as_bytes()).unwrap();
        assert_eq!(mesh.positions().len(), 4);
        assert_eq!(mesh.triangles().len(), 2);
        assert!((mesh.surface_area() - 1.0).abs() < 1e-12);
    }
}
