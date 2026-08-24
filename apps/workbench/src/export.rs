//! Deterministic interchange writers for committed viewport geometry.
//!
//! STL and this first STEP slice intentionally export the kernel's regularized
//! display tessellation in canonical millimetres. The native `.artificer` file
//! remains authoritative and retains feature/history data; interchange files
//! are downstream manufacturing/inspection views.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use artificer_protocol::Point3;

const MAX_EXPORT_TRIANGLES: usize = 5_000_000;
static EXPORT_SAVE_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ExportTriangle {
    pub body: u64,
    pub vertices: [Point3; 3],
}

pub(crate) fn write_ascii_stl(path: &Path, triangles: &[ExportTriangle]) -> Result<(), String> {
    validate_triangles(triangles)?;
    let mut output = String::with_capacity(triangles.len().saturating_mul(196));
    output.push_str("solid Artificer\n");
    for triangle in triangles {
        let normal = triangle_normal(triangle.vertices);
        output.push_str(&format!(
            "  facet normal {:.17e} {:.17e} {:.17e}\n    outer loop\n",
            normal[0], normal[1], normal[2]
        ));
        for vertex in triangle.vertices {
            output.push_str(&format!(
                "      vertex {:.17e} {:.17e} {:.17e}\n",
                vertex.x, vertex.y, vertex.z
            ));
        }
        output.push_str("    endloop\n  endfacet\n");
    }
    output.push_str("endsolid Artificer\n");
    atomic_write(path, output.as_bytes())
}

/// Writes an AP214 faceted surface model in canonical millimetres.
///
/// Each committed body receives its own open shell. This remains valid for
/// disconnected documents and avoids claiming analytic STEP/B-rep fidelity
/// before the kernel has a public exact-surface interchange boundary.
pub(crate) fn write_faceted_step(path: &Path, triangles: &[ExportTriangle]) -> Result<(), String> {
    validate_triangles(triangles)?;
    let mut output = String::with_capacity(triangles.len().saturating_mul(280));
    output.push_str(
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('Artificer faceted export'),'2;1');\n",
    );
    output.push_str(
        "FILE_NAME('Artificer.step','',('Artificer'),(''),'Artificer','Artificer','');\n",
    );
    output.push_str("FILE_SCHEMA(('AUTOMOTIVE_DESIGN_CC2'));\nENDSEC;\nDATA;\n");
    output.push_str("#1=APPLICATION_CONTEXT('automotive design');\n");
    output.push_str("#2=PRODUCT_CONTEXT('',#1,'mechanical');\n");
    output.push_str("#3=PRODUCT('Artificer','Artificer','',(#2));\n");
    output.push_str("#4=PRODUCT_DEFINITION_FORMATION('','',#3);\n");
    output.push_str("#5=PRODUCT_DEFINITION_CONTEXT('part definition',#1,'design');\n");
    output.push_str("#6=PRODUCT_DEFINITION('design','',#4,#5);\n");
    output.push_str("#7=PRODUCT_DEFINITION_SHAPE('','',#6);\n");
    output.push_str("#8=(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.));\n");
    output.push_str("#9=(NAMED_UNIT(*)PLANE_ANGLE_UNIT()SI_UNIT($,.RADIAN.));\n");
    output.push_str("#10=(NAMED_UNIT(*)SI_UNIT($,.STERADIAN.)SOLID_ANGLE_UNIT());\n");
    output.push_str("#11=UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-6),#8,'distance_accuracy_value','confusion accuracy');\n");
    output.push_str("#12=(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#11))GLOBAL_UNIT_ASSIGNED_CONTEXT((#8,#9,#10))REPRESENTATION_CONTEXT('',''));\n");

    let mut next = 13_u64;
    let mut bodies = BTreeMap::<u64, Vec<u64>>::new();
    for triangle in triangles {
        let mut points = [0_u64; 3];
        for (slot, vertex) in points.iter_mut().zip(triangle.vertices) {
            *slot = next;
            next += 1;
            output.push_str(&format!(
                "#{slot}=CARTESIAN_POINT('',({:.17e},{:.17e},{:.17e}));\n",
                vertex.x, vertex.y, vertex.z
            ));
        }
        let loop_id = next;
        let bound_id = next + 1;
        let face_id = next + 2;
        next += 3;
        output.push_str(&format!(
            "#{loop_id}=POLY_LOOP('',(#{},#{},#{}));\n",
            points[0], points[1], points[2]
        ));
        output.push_str(&format!(
            "#{bound_id}=FACE_OUTER_BOUND('',#{loop_id},.T.);\n"
        ));
        output.push_str(&format!("#{face_id}=FACE('',(#{bound_id}));\n"));
        bodies.entry(triangle.body).or_default().push(face_id);
    }

    let mut shell_ids = Vec::with_capacity(bodies.len());
    for (body, faces) in bodies {
        let shell_id = next;
        next += 1;
        shell_ids.push(shell_id);
        output.push_str(&format!(
            "#{shell_id}=OPEN_SHELL('Body {body}',({}));\n",
            entity_list(&faces)
        ));
    }
    let surface_id = next;
    let representation_id = next + 1;
    let relation_id = next + 2;
    output.push_str(&format!(
        "#{surface_id}=SHELL_BASED_SURFACE_MODEL('Artificer faceted bodies',({}));\n",
        entity_list(&shell_ids)
    ));
    output.push_str(&format!(
        "#{representation_id}=MANIFOLD_SURFACE_SHAPE_REPRESENTATION('',(#{surface_id}),#12);\n"
    ));
    output.push_str(&format!(
        "#{relation_id}=SHAPE_DEFINITION_REPRESENTATION(#7,#{representation_id});\n"
    ));
    output.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
    atomic_write(path, output.as_bytes())
}

/// One planar sketch curve in sketch-plane coordinates, canonical millimetres.
/// The vocabulary is exactly the sketch kernel's: lines, circular arcs, and
/// circles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SketchExportCurve {
    Line {
        start: [f64; 2],
        end: [f64; 2],
    },
    Circle {
        center: [f64; 2],
        radius: f64,
    },
    /// Counter-clockwise from `start_degrees` to `end_degrees`, the DXF arc
    /// convention.
    Arc {
        center: [f64; 2],
        radius: f64,
        start_degrees: f64,
        end_degrees: f64,
    },
}

/// Writes a minimal DXF (R12 entity vocabulary) of one sketch's curves.
///
/// A sketch is already a 2D drawing in its own plane, which is exactly what
/// DXF consumers — laser cutters, CAM nesting, drawing packages — want from
/// it. Only the ENTITIES section is emitted, in millimetres, on layer 0;
/// every mainstream reader accepts that form.
pub(crate) fn write_sketch_dxf(path: &Path, curves: &[SketchExportCurve]) -> Result<(), String> {
    if curves.is_empty() {
        return Err("the sketch has no curves to export".into());
    }
    let finite = |values: &[f64]| values.iter().all(|value| value.is_finite());
    for curve in curves {
        let sound = match *curve {
            SketchExportCurve::Line { start, end } => finite(&start) && finite(&end),
            SketchExportCurve::Circle { center, radius } => {
                finite(&center) && radius.is_finite() && radius > 0.0
            }
            SketchExportCurve::Arc {
                center,
                radius,
                start_degrees,
                end_degrees,
            } => {
                finite(&center)
                    && radius.is_finite()
                    && radius > 0.0
                    && start_degrees.is_finite()
                    && end_degrees.is_finite()
            }
        };
        if !sound {
            return Err("the sketch contains a non-finite or degenerate curve".into());
        }
    }
    let mut output = String::with_capacity(curves.len().saturating_mul(96) + 64);
    output.push_str("0\nSECTION\n2\nENTITIES\n");
    for curve in curves {
        match *curve {
            SketchExportCurve::Line { start, end } => {
                output.push_str(&format!(
                    "0\nLINE\n8\n0\n10\n{:.9}\n20\n{:.9}\n11\n{:.9}\n21\n{:.9}\n",
                    start[0], start[1], end[0], end[1]
                ));
            }
            SketchExportCurve::Circle { center, radius } => {
                output.push_str(&format!(
                    "0\nCIRCLE\n8\n0\n10\n{:.9}\n20\n{:.9}\n40\n{radius:.9}\n",
                    center[0], center[1]
                ));
            }
            SketchExportCurve::Arc {
                center,
                radius,
                start_degrees,
                end_degrees,
            } => {
                output.push_str(&format!(
                    "0\nARC\n8\n0\n10\n{:.9}\n20\n{:.9}\n40\n{radius:.9}\n50\n{start_degrees:.9}\n51\n{end_degrees:.9}\n",
                    center[0], center[1]
                ));
            }
        }
    }
    output.push_str("0\nENDSEC\n0\nEOF\n");
    atomic_write(path, output.as_bytes())
}

fn validate_triangles(triangles: &[ExportTriangle]) -> Result<(), String> {
    if triangles.is_empty() {
        return Err("there are no visible committed triangles to export".into());
    }
    if triangles.len() > MAX_EXPORT_TRIANGLES {
        return Err(format!(
            "export contains {} triangles, above the {} triangle safety limit",
            triangles.len(),
            MAX_EXPORT_TRIANGLES
        ));
    }
    if triangles
        .iter()
        .flat_map(|triangle| triangle.vertices)
        .any(|point| !point.x.is_finite() || !point.y.is_finite() || !point.z.is_finite())
    {
        return Err("export geometry contains a non-finite coordinate".into());
    }
    Ok(())
}

fn triangle_normal(vertices: [Point3; 3]) -> [f64; 3] {
    let first = [
        vertices[1].x - vertices[0].x,
        vertices[1].y - vertices[0].y,
        vertices[1].z - vertices[0].z,
    ];
    let second = [
        vertices[2].x - vertices[0].x,
        vertices[2].y - vertices[0].y,
        vertices[2].z - vertices[0].z,
    ];
    let normal = [
        first[1] * second[2] - first[2] * second[1],
        first[2] * second[0] - first[0] * second[2],
        first[0] * second[1] - first[1] * second[0],
    ];
    let length = normal.iter().map(|value| value * value).sum::<f64>().sqrt();
    if length <= f64::EPSILON {
        [0.0; 3]
    } else {
        normal.map(|value| value / length)
    }
}

fn entity_list(ids: &[u64]) -> String {
    ids.iter()
        .map(|id| format!("#{id}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "the export path has no parent directory".to_owned())?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "the export path has no valid file name".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let unique = EXPORT_SAVE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".{file_name}.{}.{unique}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(contents)
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        fs::rename(&temporary, path).map_err(|error| error.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triangle() -> ExportTriangle {
        ExportTriangle {
            body: 1,
            vertices: [
                Point3::new(0.0, 0.0, 0.0),
                Point3::new(1.0, 0.0, 0.0),
                Point3::new(0.0, 1.0, 0.0),
            ],
        }
    }

    #[test]
    fn stl_contains_normals_and_canonical_vertices() {
        let root = std::env::temp_dir().join(format!(
            "artificer-stl-export-{}-{}",
            std::process::id(),
            EXPORT_SAVE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let path = root.join("fixture.stl");
        write_ascii_stl(&path, &[triangle()]).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("solid Artificer"));
        assert!(text.contains("facet normal"));
        assert!(text.contains("vertex 1.00000000000000000e0"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dxf_carries_lines_circles_and_arcs_and_refuses_an_empty_sketch() {
        let root = std::env::temp_dir().join(format!(
            "artificer-dxf-export-{}-{}",
            std::process::id(),
            EXPORT_SAVE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let path = root.join("fixture.dxf");
        write_sketch_dxf(
            &path,
            &[
                SketchExportCurve::Line {
                    start: [0.0, 0.0],
                    end: [4.0, 0.0],
                },
                SketchExportCurve::Circle {
                    center: [2.0, 1.0],
                    radius: 0.5,
                },
                SketchExportCurve::Arc {
                    center: [0.0, 0.0],
                    radius: 2.0,
                    start_degrees: 0.0,
                    end_degrees: 90.0,
                },
            ],
        )
        .unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("0\nSECTION\n2\nENTITIES\n"));
        for token in ["LINE", "CIRCLE", "ARC", "0\nEOF\n"] {
            assert!(text.contains(token), "missing {token}");
        }
        assert!(write_sketch_dxf(&path, &[]).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn step_has_units_surface_model_and_product_relation() {
        let root = std::env::temp_dir().join(format!(
            "artificer-step-export-{}-{}",
            std::process::id(),
            EXPORT_SAVE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let path = root.join("fixture.step");
        write_faceted_step(&path, &[triangle()]).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("ISO-10303-21"));
        assert!(text.contains("SI_UNIT(.MILLI.,.METRE.)"));
        assert!(text.contains("SHELL_BASED_SURFACE_MODEL"));
        assert!(text.contains("SHAPE_DEFINITION_REPRESENTATION"));
        let _ = fs::remove_dir_all(root);
    }
}
