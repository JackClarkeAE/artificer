//! PLY import (ascii and binary little-endian), the other format scanners
//! commonly export. Unknown properties are skipped by size so vendor extras
//! (color, confidence, quality) never break the parse.

use std::fmt;

use artificer_geometry::Point3;

use crate::mesh::TriangleMesh;

#[derive(Debug)]
pub enum PlyError {
    Malformed(String),
    Unsupported(String),
    Truncated,
}

impl fmt::Display for PlyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(detail) => write!(f, "malformed ply: {detail}"),
            Self::Unsupported(detail) => write!(f, "unsupported ply: {detail}"),
            Self::Truncated => write!(f, "ply data ends before the declared element counts"),
        }
    }
}

impl std::error::Error for PlyError {}

#[derive(Clone, Copy, PartialEq)]
enum Scalar {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl Scalar {
    fn parse(token: &str) -> Option<Self> {
        Some(match token {
            "char" | "int8" => Self::I8,
            "uchar" | "uint8" => Self::U8,
            "short" | "int16" => Self::I16,
            "ushort" | "uint16" => Self::U16,
            "int" | "int32" => Self::I32,
            "uint" | "uint32" => Self::U32,
            "float" | "float32" => Self::F32,
            "double" | "float64" => Self::F64,
            _ => return None,
        })
    }

    const fn size(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::I32 | Self::U32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }
}

enum Property {
    Scalar {
        name: String,
        kind: Scalar,
    },
    List {
        name: String,
        count: Scalar,
        item: Scalar,
    },
}

struct Element {
    name: String,
    count: usize,
    properties: Vec<Property>,
}

enum Format {
    Ascii,
    BinaryLittleEndian,
}

pub fn read_ply(bytes: &[u8]) -> Result<TriangleMesh, PlyError> {
    let (format, elements, body_offset) = parse_header(bytes)?;
    let body = &bytes[body_offset..];
    let (positions, faces) = match format {
        Format::Ascii => read_ascii_body(body, &elements)?,
        Format::BinaryLittleEndian => read_binary_body(body, &elements)?,
    };
    let mut triangles = Vec::new();
    for polygon in faces {
        if polygon.len() < 3 {
            return Err(PlyError::Malformed(
                "face with fewer than 3 vertices".into(),
            ));
        }
        for i in 1..polygon.len() - 1 {
            triangles.push([polygon[0], polygon[i], polygon[i + 1]]);
        }
    }
    TriangleMesh::new(positions, triangles)
        .ok_or_else(|| PlyError::Malformed("face index out of range or degenerate".into()))
}

fn parse_header(bytes: &[u8]) -> Result<(Format, Vec<Element>, usize), PlyError> {
    let mut offset = 0usize;
    let mut lines = Vec::new();
    loop {
        let end = bytes[offset..]
            .iter()
            .position(|&b| b == b'\n')
            .ok_or(PlyError::Truncated)?;
        let line = std::str::from_utf8(&bytes[offset..offset + end])
            .map_err(|_| PlyError::Malformed("header is not utf-8".into()))?
            .trim_end_matches('\r')
            .to_owned();
        offset += end + 1;
        let is_end = line.trim() == "end_header";
        lines.push(line);
        if is_end {
            break;
        }
    }
    if lines.first().map(String::as_str) != Some("ply") {
        return Err(PlyError::Malformed("missing ply magic line".into()));
    }
    let mut format = None;
    let mut elements: Vec<Element> = Vec::new();
    for line in &lines[1..] {
        let mut tokens = line.split_ascii_whitespace();
        match tokens.next() {
            Some("format") => {
                format = Some(match tokens.next() {
                    Some("ascii") => Format::Ascii,
                    Some("binary_little_endian") => Format::BinaryLittleEndian,
                    Some(other) => {
                        return Err(PlyError::Unsupported(format!("format {other}")));
                    }
                    None => return Err(PlyError::Malformed("format line without type".into())),
                });
            }
            Some("element") => {
                let name = tokens
                    .next()
                    .ok_or_else(|| PlyError::Malformed("element without name".into()))?;
                let count = tokens
                    .next()
                    .and_then(|t| t.parse::<usize>().ok())
                    .ok_or_else(|| PlyError::Malformed("element without count".into()))?;
                elements.push(Element {
                    name: name.to_owned(),
                    count,
                    properties: Vec::new(),
                });
            }
            Some("property") => {
                let element = elements
                    .last_mut()
                    .ok_or_else(|| PlyError::Malformed("property before any element".into()))?;
                let first = tokens
                    .next()
                    .ok_or_else(|| PlyError::Malformed("property without type".into()))?;
                if first == "list" {
                    let count = tokens
                        .next()
                        .and_then(Scalar::parse)
                        .ok_or_else(|| PlyError::Malformed("bad list count type".into()))?;
                    let item = tokens
                        .next()
                        .and_then(Scalar::parse)
                        .ok_or_else(|| PlyError::Malformed("bad list item type".into()))?;
                    let name = tokens
                        .next()
                        .ok_or_else(|| PlyError::Malformed("list without name".into()))?;
                    element.properties.push(Property::List {
                        name: name.to_owned(),
                        count,
                        item,
                    });
                } else {
                    let kind = Scalar::parse(first)
                        .ok_or_else(|| PlyError::Unsupported(format!("property type {first}")))?;
                    let name = tokens
                        .next()
                        .ok_or_else(|| PlyError::Malformed("property without name".into()))?;
                    element.properties.push(Property::Scalar {
                        name: name.to_owned(),
                        kind,
                    });
                }
            }
            _ => {}
        }
    }
    let format = format.ok_or_else(|| PlyError::Malformed("missing format line".into()))?;
    Ok((format, elements, offset))
}

fn is_face_list(name: &str) -> bool {
    name == "vertex_indices" || name == "vertex_index"
}

fn read_ascii_body(
    body: &[u8],
    elements: &[Element],
) -> Result<(Vec<Point3>, Vec<Vec<u32>>), PlyError> {
    let text = std::str::from_utf8(body)
        .map_err(|_| PlyError::Malformed("ascii body is not utf-8".into()))?;
    let mut tokens = text.split_ascii_whitespace();
    let mut next = |context: &str| -> Result<f64, PlyError> {
        tokens
            .next()
            .and_then(|t| t.parse::<f64>().ok())
            .ok_or_else(|| PlyError::Malformed(format!("expected number in {context}")))
    };
    let mut positions = Vec::new();
    let mut faces = Vec::new();
    for element in elements {
        for _ in 0..element.count {
            let mut xyz = [f64::NAN; 3];
            for property in &element.properties {
                match property {
                    Property::Scalar { name, .. } => {
                        let value = next(&element.name)?;
                        match name.as_str() {
                            "x" => xyz[0] = value,
                            "y" => xyz[1] = value,
                            "z" => xyz[2] = value,
                            _ => {}
                        }
                    }
                    Property::List { name, .. } => {
                        let count = next(&element.name)? as usize;
                        let mut indices = Vec::with_capacity(count);
                        for _ in 0..count {
                            indices.push(next(&element.name)? as u32);
                        }
                        if element.name == "face" && is_face_list(name) {
                            faces.push(indices);
                        }
                    }
                }
            }
            if element.name == "vertex" {
                positions.push(Point3::new(xyz[0], xyz[1], xyz[2]));
            }
        }
    }
    Ok((positions, faces))
}

fn read_binary_body(
    body: &[u8],
    elements: &[Element],
) -> Result<(Vec<Point3>, Vec<Vec<u32>>), PlyError> {
    let mut cursor = 0usize;
    let mut read_scalar = |kind: Scalar, body: &[u8]| -> Result<f64, PlyError> {
        let size = kind.size();
        let slice = body.get(cursor..cursor + size).ok_or(PlyError::Truncated)?;
        cursor += size;
        Ok(match kind {
            Scalar::I8 => slice[0] as i8 as f64,
            Scalar::U8 => slice[0] as f64,
            Scalar::I16 => i16::from_le_bytes([slice[0], slice[1]]) as f64,
            Scalar::U16 => u16::from_le_bytes([slice[0], slice[1]]) as f64,
            Scalar::I32 => i32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]) as f64,
            Scalar::U32 => u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]) as f64,
            Scalar::F32 => f32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]) as f64,
            Scalar::F64 => f64::from_le_bytes([
                slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
            ]),
        })
    };
    let mut positions = Vec::new();
    let mut faces = Vec::new();
    for element in elements {
        for _ in 0..element.count {
            let mut xyz = [f64::NAN; 3];
            for property in &element.properties {
                match property {
                    Property::Scalar { name, kind } => {
                        let value = read_scalar(*kind, body)?;
                        match name.as_str() {
                            "x" => xyz[0] = value,
                            "y" => xyz[1] = value,
                            "z" => xyz[2] = value,
                            _ => {}
                        }
                    }
                    Property::List { name, count, item } => {
                        let entries = read_scalar(*count, body)? as usize;
                        let mut indices = Vec::with_capacity(entries);
                        for _ in 0..entries {
                            indices.push(read_scalar(*item, body)? as u32);
                        }
                        if element.name == "face" && is_face_list(name) {
                            faces.push(indices);
                        }
                    }
                }
            }
            if element.name == "vertex" {
                positions.push(Point3::new(xyz[0], xyz[1], xyz[2]));
            }
        }
    }
    Ok((positions, faces))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_quad_with_extras_parses() {
        let text = "ply\n\
            format ascii 1.0\n\
            comment made by a scanner\n\
            element vertex 4\n\
            property float x\nproperty float y\nproperty float z\n\
            property uchar red\n\
            element face 2\n\
            property list uchar int vertex_indices\n\
            end_header\n\
            0 0 0 255\n1 0 0 255\n1 1 0 255\n0 1 0 255\n\
            3 0 1 2\n3 0 2 3\n";
        let mesh = read_ply(text.as_bytes()).unwrap();
        assert_eq!(mesh.positions().len(), 4);
        assert_eq!(mesh.triangles().len(), 2);
        assert!((mesh.surface_area() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn binary_little_endian_quad_parses() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(
            b"ply\nformat binary_little_endian 1.0\n\
              element vertex 4\n\
              property float x\nproperty float y\nproperty float z\n\
              element face 1\n\
              property list uchar int vertex_indices\n\
              end_header\n",
        );
        for (x, y, z) in [
            (0.0f32, 0.0f32, 0.0f32),
            (1.0, 0.0, 0.0),
            (1.0, 1.0, 0.0),
            (0.0, 1.0, 0.0),
        ] {
            bytes.extend_from_slice(&x.to_le_bytes());
            bytes.extend_from_slice(&y.to_le_bytes());
            bytes.extend_from_slice(&z.to_le_bytes());
        }
        bytes.push(4);
        for index in [0i32, 1, 2, 3] {
            bytes.extend_from_slice(&index.to_le_bytes());
        }
        let mesh = read_ply(&bytes).unwrap();
        assert_eq!(mesh.positions().len(), 4);
        // The quad face is fan-triangulated.
        assert_eq!(mesh.triangles().len(), 2);
    }
}
