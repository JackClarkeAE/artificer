//! Visual snapshot rendering from arbitrary viewpoints (SVG and PNG).

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::{FaceRole, NativeKernel, Snapshot};
use artificer_protocol::{EntityRef, OperationReport, Point3, Vector3};
use serde::{Deserialize, Serialize};

use crate::api::debug::ApiError;
use crate::api::selectors::EntitySelector;

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
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotFormat {
    #[default]
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
    #[serde(default = "CameraSpec::isometric")]
    pub camera: CameraSpec,
    #[serde(default)]
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
    let projected = project_scene(snapshot, options, highlighted_entities);
    match options.format {
        SnapshotFormat::Svg => Ok(SnapshotOutput::Svg(write_svg(&projected, options, report))),
        SnapshotFormat::Png => Ok(SnapshotOutput::Png(rasterize_png(&projected, options))),
    }
}

/// One triangle on screen, painted back to front.
struct ScreenTriangle {
    points: [(f64, f64); 3],
    color: [u8; 3],
    opacity: f64,
    source_face: EntityRef,
}

/// One edge segment on screen.
struct ScreenLine {
    from: (f64, f64),
    to: (f64, f64),
    color: [u8; 3],
    width: f64,
}

/// A scene already projected into the requested camera, in pixel
/// coordinates with `y` down, sorted for a painter's algorithm.
struct ProjectedScene {
    width: f64,
    height: f64,
    triangles: Vec<ScreenTriangle>,
    lines: Vec<ScreenLine>,
}

const BACKGROUND: [u8; 3] = [0x0d, 0x11, 0x18];
const EDGE_COLOR: [u8; 3] = [0xd5, 0xe2, 0xee];
const HIGHLIGHT_FACE: [u8; 3] = [0xff, 0x9f, 0x43];
const HIGHLIGHT_EDGE: [u8; 3] = [0xff, 0x38, 0x38];

fn project_scene(
    snapshot: &Snapshot,
    options: &SnapshotOptions,
    highlighted: &BTreeSet<EntityRef>,
) -> ProjectedScene {
    let scene = NativeKernel::debug_scene(snapshot);
    let width = f64::from(options.camera.width.max(1));
    let height = f64::from(options.camera.height.max(1));

    let points = scene
        .triangles
        .iter()
        .flat_map(|t| t.vertices)
        .chain(scene.edges.iter().flat_map(|e| e.endpoints))
        .collect::<Vec<_>>();
    if points.is_empty() {
        return ProjectedScene {
            width,
            height,
            triangles: Vec::new(),
            lines: Vec::new(),
        };
    }

    // Camera basis: forward toward the target, right = forward × up,
    // true up = right × forward.
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
    let rx = f_unit.y * cam.up.z - f_unit.z * cam.up.y;
    let ry = f_unit.z * cam.up.x - f_unit.x * cam.up.z;
    let rz = f_unit.x * cam.up.y - f_unit.y * cam.up.x;
    let r_len = (rx * rx + ry * ry + rz * rz).sqrt();
    let r_unit = if r_len > 1e-9 {
        Vector3::new(rx / r_len, ry / r_len, rz / r_len)
    } else {
        Vector3::new(1.0, 0.0, 0.0)
    };
    let u_unit = Vector3::new(
        r_unit.y * f_unit.z - r_unit.z * f_unit.y,
        r_unit.z * f_unit.x - r_unit.x * f_unit.z,
        r_unit.x * f_unit.y - r_unit.y * f_unit.x,
    );

    // Camera space: X right, Y up, Z forward (growing away from the eye).
    // A perspective camera divides by depth measured from the eye, scaled
    // so the field of view spans the image height at the target distance.
    let perspective_scale = match cam.projection {
        Projection::Orthographic => None,
        Projection::Perspective { fov_degrees } => {
            let half = (fov_degrees.clamp(1.0, 179.0) * 0.5).to_radians();
            Some(f_len.max(1e-6) / half.tan().max(1e-9))
        }
    };
    let project = |p: Point3| -> (f64, f64, f64) {
        let vx = p.x - cam.position.x;
        let vy = p.y - cam.position.y;
        let vz = p.z - cam.position.z;
        let cx = vx * r_unit.x + vy * r_unit.y + vz * r_unit.z;
        let cy = vx * u_unit.x + vy * u_unit.y + vz * u_unit.z;
        let cz = vx * f_unit.x + vy * f_unit.y + vz * f_unit.z;
        match perspective_scale {
            Some(scale) => {
                let depth = cz.max(1e-6);
                (cx * scale / depth, cy * scale / depth, cz)
            }
            None => (cx, cy, cz),
        }
    };

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
        let sy = height * 0.5 - (cy - mid_cy) * scale;
        (sx, sy, cz)
    };

    // Back to front: camera `z` grows away from the eye, so the farthest
    // triangle has the largest `z` and is painted first.
    let mut triangles = scene.triangles.iter().collect::<Vec<_>>();
    triangles.sort_by(|a, b| {
        let za =
            (project(a.vertices[0]).2 + project(a.vertices[1]).2 + project(a.vertices[2]).2) / 3.0;
        let zb =
            (project(b.vertices[0]).2 + project(b.vertices[1]).2 + project(b.vertices[2]).2) / 3.0;
        zb.total_cmp(&za)
    });
    let wireframe = options.display_mode == "wireframe";
    let triangles = if wireframe {
        Vec::new()
    } else {
        triangles
            .into_iter()
            .map(|tri| {
                let is_highlighted = highlighted.contains(&tri.source_face);
                let color = if is_highlighted {
                    HIGHLIGHT_FACE
                } else {
                    face_color(tri.role)
                };
                ScreenTriangle {
                    points: tri.vertices.map(|v| {
                        let (x, y, _) = screen_point(v);
                        (x, y)
                    }),
                    color,
                    opacity: if is_highlighted { 0.95 } else { 0.86 },
                    source_face: tri.source_face,
                }
            })
            .collect()
    };

    // Tessellation seams inside one smooth carrier are not model edges;
    // only the hard edges are drawn, unless a wireframe was asked for.
    let lines = scene
        .edges
        .iter()
        .filter(|edge| wireframe || !edge.is_smooth)
        .map(|edge| {
            let is_highlighted = highlighted.contains(&edge.source_edge);
            let (x0, y0, _) = screen_point(edge.endpoints[0]);
            let (x1, y1, _) = screen_point(edge.endpoints[1]);
            ScreenLine {
                from: (x0, y0),
                to: (x1, y1),
                color: if is_highlighted {
                    HIGHLIGHT_EDGE
                } else {
                    EDGE_COLOR
                },
                width: if is_highlighted { 3.0 } else { 1.8 },
            }
        })
        .collect();

    ProjectedScene {
        width,
        height,
        triangles,
        lines,
    }
}

fn hex(color: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

fn write_svg(
    scene: &ProjectedScene,
    options: &SnapshotOptions,
    report: Option<&OperationReport>,
) -> String {
    let width = scene.width;
    let height = scene.height;
    let mut svg = String::new();
    writeln!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{width}\" height=\"{height}\" viewBox=\"0 0 {width} {height}\">"
    )
    .unwrap();
    writeln!(
        svg,
        "  <rect width=\"100%\" height=\"100%\" fill=\"{}\"/>",
        hex(BACKGROUND)
    )
    .unwrap();
    if scene.triangles.is_empty() && scene.lines.is_empty() {
        writeln!(
            svg,
            "  <text x=\"{x}\" y=\"{y}\" fill=\"#7a889b\" font-family=\"sans-serif\" font-size=\"18\" text-anchor=\"middle\">Empty Model</text>",
            x = width * 0.5,
            y = height * 0.5,
        )
        .unwrap();
        svg.push_str("</svg>\n");
        return svg;
    }

    svg.push_str("  <g stroke-linejoin=\"round\">\n");
    for tri in &scene.triangles {
        writeln!(
            svg,
            "    <polygon points=\"{:.2},{:.2} {:.2},{:.2} {:.2},{:.2}\" fill=\"{}\" fill-opacity=\"{}\" data-face=\"{}\"/>",
            tri.points[0].0,
            tri.points[0].1,
            tri.points[1].0,
            tri.points[1].1,
            tri.points[2].0,
            tri.points[2].1,
            hex(tri.color),
            tri.opacity,
            tri.source_face.entity
        )
        .unwrap();
    }
    svg.push_str("  </g>\n");

    svg.push_str("  <g fill=\"none\" stroke-linecap=\"round\">\n");
    for line in &scene.lines {
        writeln!(
            svg,
            "    <line x1=\"{:.2}\" y1=\"{:.2}\" x2=\"{:.2}\" y2=\"{:.2}\" stroke=\"{}\" stroke-width=\"{}\"/>",
            line.from.0,
            line.from.1,
            line.to.0,
            line.to.1,
            hex(line.color),
            line.width
        )
        .unwrap();
    }
    svg.push_str("  </g>\n");

    if options.show_labels
        && let Some(r) = report
    {
        writeln!(
            svg,
            "  <text x=\"24\" y=\"{:.0}\" fill=\"#7a889b\" font-family=\"monospace\" font-size=\"13\">{}</text>",
            height - 18.0,
            r.topology
        )
        .unwrap();
    }

    svg.push_str("</svg>\n");
    svg
}

/// An RGB canvas with a painter's-algorithm triangle fill and a
/// thick-line stroke: enough for a snapshot, with no image dependency.
struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<[u8; 3]>,
}

impl Canvas {
    fn new(width: usize, height: usize, background: [u8; 3]) -> Self {
        Self {
            width,
            height,
            pixels: vec![background; width * height],
        }
    }

    fn blend(&mut self, x: usize, y: usize, color: [u8; 3], opacity: f64) {
        if x >= self.width || y >= self.height {
            return;
        }
        let pixel = &mut self.pixels[y * self.width + x];
        for channel in 0..3 {
            let current = f64::from(pixel[channel]);
            let target = f64::from(color[channel]);
            pixel[channel] = (current + (target - current) * opacity)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }

    fn fill_triangle(&mut self, points: [(f64, f64); 3], color: [u8; 3], opacity: f64) {
        let min_y = points
            .iter()
            .map(|p| p.1)
            .fold(f64::INFINITY, f64::min)
            .floor()
            .max(0.0);
        let max_y = points
            .iter()
            .map(|p| p.1)
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil()
            .min(self.height as f64 - 1.0);
        let min_x = points
            .iter()
            .map(|p| p.0)
            .fold(f64::INFINITY, f64::min)
            .floor()
            .max(0.0);
        let max_x = points
            .iter()
            .map(|p| p.0)
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil()
            .min(self.width as f64 - 1.0);
        if !(min_y <= max_y && min_x <= max_x) {
            return;
        }
        let [a, b, c] = points;
        let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
        if area.abs() < 1e-12 {
            return;
        }
        for y in (min_y as usize)..=(max_y as usize) {
            let py = y as f64 + 0.5;
            for x in (min_x as usize)..=(max_x as usize) {
                let px = x as f64 + 0.5;
                let w0 = ((b.0 - a.0) * (py - a.1) - (b.1 - a.1) * (px - a.0)) / area;
                let w1 = ((c.0 - b.0) * (py - b.1) - (c.1 - b.1) * (px - b.0)) / area;
                let w2 = ((a.0 - c.0) * (py - c.1) - (a.1 - c.1) * (px - c.0)) / area;
                if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                    self.blend(x, y, color, opacity);
                }
            }
        }
    }

    fn stroke_line(&mut self, from: (f64, f64), to: (f64, f64), color: [u8; 3], width: f64) {
        let dx = to.0 - from.0;
        let dy = to.1 - from.1;
        let length = dx.hypot(dy);
        let steps = (length.ceil() as usize).max(1);
        let radius = (width * 0.5).max(0.5);
        let reach = radius.ceil() as i64;
        for step in 0..=steps {
            let t = step as f64 / steps as f64;
            let cx = from.0 + dx * t;
            let cy = from.1 + dy * t;
            for oy in -reach..=reach {
                for ox in -reach..=reach {
                    let px = cx.round() as i64 + ox;
                    let py = cy.round() as i64 + oy;
                    if px < 0 || py < 0 {
                        continue;
                    }
                    let distance = (px as f64 + 0.5 - cx).hypot(py as f64 + 0.5 - cy);
                    let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0);
                    if coverage > 0.0 {
                        self.blend(px as usize, py as usize, color, coverage);
                    }
                }
            }
        }
    }
}

fn rasterize_png(scene: &ProjectedScene, options: &SnapshotOptions) -> Vec<u8> {
    let width = options.camera.width.max(1) as usize;
    let height = options.camera.height.max(1) as usize;
    let mut canvas = Canvas::new(width, height, BACKGROUND);
    for tri in &scene.triangles {
        canvas.fill_triangle(tri.points, tri.color, tri.opacity);
    }
    for line in &scene.lines {
        canvas.stroke_line(line.from, line.to, line.color, line.width);
    }
    encode_png(&canvas)
}

/// PNG without an image dependency: RGB8, filter type zero on every row,
/// and a zlib stream of stored (uncompressed) deflate blocks. Larger than a
/// compressed file, but exact, deterministic, and read by every viewer.
fn encode_png(canvas: &Canvas) -> Vec<u8> {
    let mut raw = Vec::with_capacity(canvas.height * (canvas.width * 3 + 1));
    for row in canvas.pixels.chunks(canvas.width) {
        raw.push(0);
        for pixel in row {
            raw.extend_from_slice(pixel);
        }
    }

    let mut zlib = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65_535).peekable();
    if blocks.peek().is_none() {
        zlib.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    }
    while let Some(block) = blocks.next() {
        let last = blocks.peek().is_none();
        zlib.push(u8::from(last));
        let len = block.len() as u16;
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut png = Vec::with_capacity(zlib.len() + 64);
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(canvas.width as u32).to_be_bytes());
    header.extend_from_slice(&(canvas.height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    write_chunk(&mut png, b"IHDR", &header);
    write_chunk(&mut png, b"IDAT", &zlib);
    write_chunk(&mut png, b"IEND", &[]);
    png
}

fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = Vec::with_capacity(4 + data.len());
    body.extend_from_slice(kind);
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn adler32(data: &[u8]) -> u32 {
    let mut a = 1_u32;
    let mut b = 0_u32;
    for byte in data {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

fn face_color(role: FaceRole) -> [u8; 3] {
    match role {
        FaceRole::PositiveZ | FaceRole::ExtrusionTop => [0x3b, 0x82, 0xf6],
        FaceRole::NegativeZ | FaceRole::ExtrusionBottom => [0x1d, 0x4e, 0xd8],
        FaceRole::PositiveX => [0x25, 0x63, 0xeb],
        FaceRole::NegativeX => [0x1e, 0x40, 0xaf],
        FaceRole::PositiveY | FaceRole::ExtrusionSide(_) => [0x60, 0xa5, 0xfa],
        FaceRole::NegativeY => [0x93, 0xc5, 0xfd],
        FaceRole::FeatureEnd => [0x10, 0xb9, 0x81],
        FaceRole::FeatureSide(_) => [0x34, 0xd3, 0x99],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_and_adler_match_their_reference_vectors() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }

    #[test]
    fn a_png_is_well_formed_and_paints_what_was_drawn() {
        let mut canvas = Canvas::new(8, 4, [0, 0, 0]);
        canvas.fill_triangle([(0.0, 0.0), (8.0, 0.0), (0.0, 4.0)], [255, 0, 0], 1.0);
        assert_eq!(canvas.pixels[0], [255, 0, 0]);
        assert_eq!(canvas.pixels[3 * 8 + 7], [0, 0, 0]);
        let png = encode_png(&canvas);
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }
}
