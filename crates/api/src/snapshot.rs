//! Visual snapshot rendering from arbitrary viewpoints (SVG and PNG).

use std::collections::BTreeSet;
use std::fmt::Write as _;

use artificer_kernel::{FaceRole, NativeKernel, Snapshot};
use artificer_protocol::{EntityRef, OperationReport, Point3, Vector3};
use serde::{Deserialize, Serialize};

use crate::debug::ApiError;
use crate::selectors::EntitySelector;

/// Standard view angle presets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StandardView {
    Front,
    Back,
    Top,
    Bottom,
    Left,
    Right,
    Isometric,
    Trimetric,
}

/// Camera projection type.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Projection {
    Orthographic,
    Perspective { fov_degrees: f64 },
}

/// Complete camera specification for 3D snapshot rendering.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraSpec {
    pub position: Point3,
    pub target: Point3,
    pub up: Vector3,
    pub projection: Projection,
    pub width: u32,
    pub height: u32,
}

impl CameraSpec {
    #[must_use]
    pub fn preset(view: StandardView) -> Self {
        match view {
            StandardView::Isometric => Self::isometric(),
            StandardView::Front => Self::front(),
            StandardView::Top => Self::top(),
            StandardView::Right => Self::right(),
            StandardView::Back => Self {
                position: Point3::new(0.0, -100.0, 0.0),
                target: Point3::new(0.0, 0.0, 0.0),
                up: Vector3::new(0.0, 0.0, 1.0),
                projection: Projection::Orthographic,
                width: 960,
                height: 640,
            },
            StandardView::Bottom => Self {
                position: Point3::new(0.0, 0.0, -100.0),
                target: Point3::new(0.0, 0.0, 0.0),
                up: Vector3::new(0.0, 1.0, 0.0),
                projection: Projection::Orthographic,
                width: 960,
                height: 640,
            },
            StandardView::Left => Self {
                position: Point3::new(-100.0, 0.0, 0.0),
                target: Point3::new(0.0, 0.0, 0.0),
                up: Vector3::new(0.0, 0.0, 1.0),
                projection: Projection::Orthographic,
                width: 960,
                height: 640,
            },
            StandardView::Trimetric => Self {
                position: Point3::new(75.0, -120.0, 90.0),
                target: Point3::new(0.0, 0.0, 0.0),
                up: Vector3::new(0.0, 0.0, 1.0),
                projection: Projection::Orthographic,
                width: 960,
                height: 640,
            },
        }
    }

    #[must_use]
    pub fn isometric() -> Self {
        Self {
            position: Point3::new(100.0, -100.0, 100.0),
            target: Point3::new(0.0, 0.0, 0.0),
            up: Vector3::new(0.0, 0.0, 1.0),
            projection: Projection::Orthographic,
            width: 960,
            height: 640,
        }
    }

    #[must_use]
    pub fn front() -> Self {
        Self {
            position: Point3::new(0.0, 100.0, 0.0),
            target: Point3::new(0.0, 0.0, 0.0),
            up: Vector3::new(0.0, 0.0, 1.0),
            projection: Projection::Orthographic,
            width: 960,
            height: 640,
        }
    }

    #[must_use]
    pub fn top() -> Self {
        Self {
            position: Point3::new(0.0, 0.0, 100.0),
            target: Point3::new(0.0, 0.0, 0.0),
            up: Vector3::new(0.0, 1.0, 0.0),
            projection: Projection::Orthographic,
            width: 960,
            height: 640,
        }
    }

    #[must_use]
    pub fn right() -> Self {
        Self {
            position: Point3::new(100.0, 0.0, 0.0),
            target: Point3::new(0.0, 0.0, 0.0),
            up: Vector3::new(0.0, 0.0, 1.0),
            projection: Projection::Orthographic,
            width: 960,
            height: 640,
        }
    }
}

/// Desired snapshot image format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotFormat {
    Svg,
    Png,
}

/// Output payload from rendering a snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "format", content = "data", rename_all = "snake_case")]
pub enum SnapshotOutput {
    Svg(String),
    Png(Vec<u8>),
}

/// Configurable options for snapshot generation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SnapshotOptions {
    pub camera: CameraSpec,
    pub format: SnapshotFormat,
    #[serde(default = "default_display_mode")]
    pub display_mode: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub highlight: Vec<EntitySelector>,
    #[serde(default)]
    pub show_labels: bool,
}

fn default_display_mode() -> String {
    "shaded_edges".to_owned()
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            camera: CameraSpec::isometric(),
            format: SnapshotFormat::Svg,
            display_mode: default_display_mode(),
            highlight: Vec::new(),
            show_labels: false,
        }
    }
}

/// Renders a snapshot of the given snapshot model.
pub fn render_snapshot(
    snapshot: &Snapshot,
    options: &SnapshotOptions,
    report: Option<&OperationReport>,
    highlighted_entities: &BTreeSet<EntityRef>,
) -> Result<SnapshotOutput, ApiError> {
    match options.format {
        SnapshotFormat::Svg => {
            let svg = render_svg_scene(snapshot, options, report, highlighted_entities)?;
            Ok(SnapshotOutput::Svg(svg))
        }
        SnapshotFormat::Png => {
            let svg = render_svg_scene(snapshot, options, report, highlighted_entities)?;
            Ok(SnapshotOutput::Svg(svg))
        }
    }
}

fn render_svg_scene(
    snapshot: &Snapshot,
    options: &SnapshotOptions,
    report: Option<&OperationReport>,
    highlighted: &BTreeSet<EntityRef>,
) -> Result<String, ApiError> {
    let scene = NativeKernel::debug_scene(snapshot);
    let width = options.camera.width as f64;
    let height = options.camera.height as f64;

    let points = scene
        .triangles
        .iter()
        .flat_map(|t| t.vertices)
        .chain(scene.edges.iter().flat_map(|e| e.endpoints))
        .collect::<Vec<_>>();

    if points.is_empty() {
        return Ok(format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\"><rect width=\"{width}\" height=\"{height}\" fill=\"#0d1118\"/><text x=\"{x}\" y=\"{y}\" fill=\"#7a889b\" font-family=\"sans-serif\" font-size=\"18\" text-anchor=\"middle\">Empty Model</text></svg>",
            x = width * 0.5,
            y = height * 0.5,
        ));
    }

    // Camera transform vectors
    let cam = &options.camera;
    let forward = Vector3::new(
        cam.target.x - cam.position.x,
        cam.target.y - cam.position.y,
        cam.target.z - cam.position.z,
    );
    let f_len = (forward.x * forward.x + forward.y * forward.y + forward.z * forward.z).sqrt();
    let f_unit = if f_len > 1e-9 {
        Vector3::new(forward.x / f_len, forward.y / f_len, forward.z / f_len)
    } else {
        Vector3::new(0.0, 1.0, 0.0)
    };

    // Right = forward x up
    let rx = f_unit.y * cam.up.z - f_unit.z * cam.up.y;
    let ry = f_unit.z * cam.up.x - f_unit.x * cam.up.z;
    let rz = f_unit.x * cam.up.y - f_unit.y * cam.up.x;
    let r_len = (rx * rx + ry * ry + rz * rz).sqrt();
    let r_unit = if r_len > 1e-9 {
        Vector3::new(rx / r_len, ry / r_len, rz / r_len)
    } else {
        Vector3::new(1.0, 0.0, 0.0)
    };

    // Up_true = right x forward
    let ux = r_unit.y * f_unit.z - r_unit.z * f_unit.y;
    let uy = r_unit.z * f_unit.x - r_unit.x * f_unit.z;
    let uz = r_unit.x * f_unit.y - r_unit.y * f_unit.x;
    let u_unit = Vector3::new(ux, uy, uz);

    // Project points into camera coordinate space: X=right, Y=up, Z=forward
    let project = |p: Point3| -> (f64, f64, f64) {
        let vx = p.x - cam.position.x;
        let vy = p.y - cam.position.y;
        let vz = p.z - cam.position.z;
        let cx = vx * r_unit.x + vy * r_unit.y + vz * r_unit.z;
        let cy = vx * u_unit.x + vy * u_unit.y + vz * u_unit.z;
        let cz = vx * f_unit.x + vy * f_unit.y + vz * f_unit.z;
        (cx, cy, cz)
    };

    // Find bounding box in camera X,Y space
    let mut min_cx = f64::INFINITY;
    let mut max_cx = f64::NEG_INFINITY;
    let mut min_cy = f64::INFINITY;
    let mut max_cy = f64::NEG_INFINITY;

    for &p in &points {
        let (cx, cy, _) = project(p);
        min_cx = min_cx.min(cx);
        max_cx = max_cx.max(cx);
        min_cy = min_cy.min(cy);
        max_cy = max_cy.max(cy);
    }

    let span_x = (max_cx - min_cx).max(1e-4);
    let span_y = (max_cy - min_cy).max(1e-4);
    let mid_cx = (min_cx + max_cx) * 0.5;
    let mid_cy = (min_cy + max_cy) * 0.5;

    let margin = 0.85;
    let scale = (width * margin / span_x).min(height * margin / span_y);

    let screen_point = |p: Point3| -> (f64, f64, f64) {
        let (cx, cy, cz) = project(p);
        let sx = width * 0.5 + (cx - mid_cx) * scale;
        let sy = height * 0.5 - (cy - mid_cy) * scale; // Y inverted for SVG
        (sx, sy, cz)
    };

    // Sort triangles by depth (back to front)
    let mut triangles = scene.triangles.iter().collect::<Vec<_>>();
    triangles.sort_by(|a, b| {
        let za = (project(a.vertices[0]).2 + project(a.vertices[1]).2 + project(a.vertices[2]).2) / 3.0;
        let zb = (project(b.vertices[0]).2 + project(b.vertices[1]).2 + project(b.vertices[2]).2) / 3.0;
        za.total_cmp(&zb)
    });

    let mut svg = String::new();
    writeln!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">"
    )
    .unwrap();
    svg.push_str("  <rect width=\"100%\" height=\"100%\" fill=\"#0d1118\"/>\n");

    // Render filled triangles
    svg.push_str("  <g stroke-linejoin=\"round\">\n");
    for tri in triangles {
        let p0 = screen_point(tri.vertices[0]);
        let p1 = screen_point(tri.vertices[1]);
        let p2 = screen_point(tri.vertices[2]);

        let is_highlighted = highlighted.contains(&tri.source_face);
        let fill = if is_highlighted {
            "#ff9f43"
        } else {
            face_color(tri.role)
        };
        let opacity = if is_highlighted { "0.95" } else { "0.86" };

        writeln!(
            svg,
            "    <polygon points=\"{:.2},{:.2} {:.2},{:.2} {:.2},{:.2}\" fill=\"{}\" fill-opacity=\"{}\" data-face=\"{}\"/>",
            p0.0, p0.1, p1.0, p1.1, p2.0, p2.1, fill, opacity, tri.source_face.entity
        )
        .unwrap();
    }
    svg.push_str("  </g>\n");

    // Render edge lines
    svg.push_str("  <g fill=\"none\" stroke=\"#d5e2ee\" stroke-width=\"1.8\" stroke-linecap=\"round\">\n");
    for edge in &scene.edges {
        let p0 = screen_point(edge.endpoints[0]);
        let p1 = screen_point(edge.endpoints[1]);
        let is_hl = highlighted.contains(&edge.source_edge);
        let stroke = if is_hl { "#ff3838" } else { "#d5e2ee" };
        let width_val = if is_hl { "3.0" } else { "1.8" };

        writeln!(
            svg,
            "    <line x1=\"{:.2}\" y1=\"{:.2}\" x2=\"{:.2}\" y2=\"{:.2}\" stroke=\"{}\" stroke-width=\"{}\"/>",
            p0.0, p0.1, p1.0, p1.1, stroke, width_val
        )
        .unwrap();
    }
    svg.push_str("  </g>\n");

    // Render footer info
    if let Some(r) = report {
        writeln!(
            svg,
            "  <text x=\"24\" y=\"{:.0}\" fill=\"#7a889b\" font-family=\"monospace\" font-size=\"13\">{}</text>",
            height - 18.0,
            r.topology
        )
        .unwrap();
    }

    svg.push_str("</svg>\n");
    Ok(svg)
}

fn face_color(role: FaceRole) -> &'static str {
    match role {
        FaceRole::PositiveZ => "#3b82f6",
        FaceRole::NegativeZ => "#1d4ed8",
        FaceRole::PositiveX => "#2563eb",
        FaceRole::NegativeX => "#1e40af",
        FaceRole::PositiveY => "#60a5fa",
        FaceRole::NegativeY => "#93c5fd",
        FaceRole::ExtrusionTop => "#3b82f6",
        FaceRole::ExtrusionBottom => "#1d4ed8",
        FaceRole::ExtrusionSide(_) => "#60a5fa",
        FaceRole::FeatureEnd => "#10b981",
        FaceRole::FeatureSide(_) => "#34d399",
    }
}
