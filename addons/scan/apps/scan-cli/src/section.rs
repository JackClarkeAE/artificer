//! Cross-sections: the view a shaded render cannot give you.
//!
//! When a rebuilt surface goes missing, a shaded view of the outside of
//! the part is close to useless — the hole is behind something, or it is
//! facing away, or a neighbouring surface reads as the one you are
//! looking for. Cutting the scan and the rebuild with the *same* plane
//! and drawing the two outlines side by side makes it unmistakable: the
//! scan's section has a line where the rebuild's has nothing.
//!
//! The scan is closed, so its section is filled by scanline parity to
//! show solid material. The rebuild is a collection of trimmed surfaces
//! rather than a solid, so it is drawn as an outline over a ghost of the
//! scan's fill — anywhere the ghost shows through with no line on it is
//! geometry the reconstruction has lost.

use artificer_geometry::{Point3, Vector3};
use artificer_scan_core::mesh::TriangleMesh;
use artificer_scan_core::transform::RigidTransform;

use crate::snapshot::encode_png;

const BACKGROUND: [u8; 3] = [14, 16, 22];
const SCAN_FILL: [u8; 3] = [116, 124, 140];
const SCAN_LINE: [u8; 3] = [222, 230, 244];
const GHOST_FILL: [u8; 3] = [40, 44, 54];
const REBUILD_LINE: [u8; 3] = [104, 214, 178];
const MATCHED_LINE: [u8; 3] = [72, 88, 104];
const MISSING_LINE: [u8; 3] = [255, 72, 88];
const AXIS_LINE: [u8; 3] = [70, 60, 90];
const LABEL: [u8; 3] = [150, 160, 180];

/// Distance from a point to a segment, in plane millimetres.
fn point_segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let (wx, wy) = (p.0 - a.0, p.1 - a.1);
    let length_squared = vx * vx + vy * vy;
    let t = if length_squared < 1e-12 {
        0.0
    } else {
        ((wx * vx + wy * vy) / length_squared).clamp(0.0, 1.0)
    };
    (wx - t * vx).hypot(wy - t * vy)
}

/// True when some rebuilt segment runs within `reach` of this one — the
/// reconstruction accounts for that piece of the scan's outline.
fn covered(segment: &[(f64, f64); 2], rebuild: &[[(f64, f64); 2]], reach: f64) -> bool {
    let midpoint = (
        (segment[0].0 + segment[1].0) / 2.0,
        (segment[0].1 + segment[1].1) / 2.0,
    );
    rebuild
        .iter()
        .any(|other| point_segment_distance(midpoint, other[0], other[1]) <= reach)
}

/// One cut through the part.
pub struct Cut {
    pub label: String,
    pub origin: Point3,
    pub normal: Vector3,
    /// In-plane axes the section is drawn against.
    pub right: Vector3,
    pub up: Vector3,
}

/// Segments where the mesh crosses the plane, in plane coordinates.
fn plane_section(
    mesh: &TriangleMesh,
    transform: &RigidTransform,
    cut: &Cut,
) -> Vec<[(f64, f64); 2]> {
    let mut segments = Vec::new();
    for face in 0..mesh.triangles().len() {
        let corners: Vec<Point3> = mesh
            .triangle_points(face)
            .into_iter()
            .map(|p| transform.apply_point(p))
            .collect();
        let distances: Vec<f64> = corners
            .iter()
            .map(|p| (*p - cut.origin).dot(cut.normal))
            .collect();
        // Collect the points where the triangle's edges cross the plane.
        let mut hits: Vec<Point3> = Vec::new();
        for edge in 0..3 {
            let (a, b) = (edge, (edge + 1) % 3);
            let (da, db) = (distances[a], distances[b]);
            if (da > 0.0) == (db > 0.0) || da == db {
                continue;
            }
            let t = da / (da - db);
            let p = corners[a] + (corners[b] - corners[a]) * t;
            hits.push(p);
        }
        if hits.len() == 2 {
            let project = |p: Point3| {
                let v = p - cut.origin;
                (v.dot(cut.right), v.dot(cut.up))
            };
            segments.push([project(hits[0]), project(hits[1])]);
        }
    }
    segments
}

struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<[u8; 3]>,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![BACKGROUND; width * height],
        }
    }

    fn set(&mut self, x: i64, y: i64, color: [u8; 3]) {
        if x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height {
            self.pixels[y as usize * self.width + x as usize] = color;
        }
    }

    fn line(&mut self, a: (i64, i64), b: (i64, i64), color: [u8; 3], weight: i64) {
        let (mut x, mut y) = a;
        let dx = (b.0 - a.0).abs();
        let dy = -(b.1 - a.1).abs();
        let sx = if a.0 < b.0 { 1 } else { -1 };
        let sy = if a.1 < b.1 { 1 } else { -1 };
        let mut error = dx + dy;
        loop {
            for ox in -(weight - 1)..weight {
                for oy in -(weight - 1)..weight {
                    self.set(x + ox, y + oy, color);
                }
            }
            if x == b.0 && y == b.1 {
                break;
            }
            let doubled = 2 * error;
            if doubled >= dy {
                error += dy;
                x += sx;
            }
            if doubled <= dx {
                error += dx;
                y += sy;
            }
        }
    }
}

/// Maps plane coordinates into a panel's pixel box.
struct Frame {
    x0: usize,
    y0: usize,
    width: usize,
    height: usize,
    center: (f64, f64),
    scale: f64,
}

impl Frame {
    fn at(&self, p: (f64, f64)) -> (i64, i64) {
        let px = (self.x0 + self.width / 2) as f64 + (p.0 - self.center.0) * self.scale;
        // Screen y grows downward; the section's up axis grows upward.
        let py = (self.y0 + self.height / 2) as f64 - (p.1 - self.center.1) * self.scale;
        (px.round() as i64, py.round() as i64)
    }
}

/// Fills the closed section by scanline parity. Only meaningful for a
/// watertight mesh, which is why it is used for the scan and not for the
/// rebuild's collection of trimmed surfaces.
fn fill_section(canvas: &mut Canvas, frame: &Frame, segments: &[[(f64, f64); 2]], color: [u8; 3]) {
    for row in frame.y0..(frame.y0 + frame.height) {
        let y = row as f64 + 0.5;
        let mut crossings: Vec<f64> = Vec::new();
        for segment in segments {
            let a = frame.at(segment[0]);
            let b = frame.at(segment[1]);
            let (ay, by) = (a.1 as f64, b.1 as f64);
            if (ay > y) == (by > y) {
                continue;
            }
            let t = (y - ay) / (by - ay);
            crossings.push(a.0 as f64 + (b.0 as f64 - a.0 as f64) * t);
        }
        if crossings.len() < 2 {
            continue;
        }
        crossings.sort_by(f64::total_cmp);
        for pair in crossings.chunks(2) {
            if pair.len() < 2 {
                break;
            }
            for x in (pair[0].ceil() as i64)..=(pair[1].floor() as i64) {
                if x >= frame.x0 as i64 && x < (frame.x0 + frame.width) as i64 {
                    canvas.set(x, row as i64, color);
                }
            }
        }
    }
}

/// A 3x5 dot-matrix digit/letter set, enough for the panel captions.
fn glyph(c: char) -> [u8; 5] {
    match c.to_ascii_uppercase() {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        'A' => [0b111, 0b101, 0b111, 0b101, 0b101],
        'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'C' => [0b111, 0b100, 0b100, 0b100, 0b111],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'E' => [0b111, 0b100, 0b111, 0b100, 0b111],
        'G' => [0b111, 0b100, 0b101, 0b101, 0b111],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'N' => [0b101, 0b111, 0b111, 0b111, 0b101],
        'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        'S' => [0b111, 0b100, 0b111, 0b001, 0b111],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        '+' => [0b000, 0b010, 0b111, 0b010, 0b000],
        '-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        '.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        ':' => [0b000, 0b010, 0b000, 0b010, 0b000],
        '=' => [0b000, 0b111, 0b000, 0b111, 0b000],
        _ => [0; 5],
    }
}

fn text(canvas: &mut Canvas, x: usize, y: usize, message: &str, scale: usize) {
    let mut cursor = x;
    for c in message.chars() {
        if c == ' ' {
            cursor += 4 * scale;
            continue;
        }
        let rows = glyph(c);
        for (row_index, row) in rows.iter().enumerate() {
            for column in 0..3 {
                if row & (1 << (2 - column)) != 0 {
                    for sx in 0..scale {
                        for sy in 0..scale {
                            canvas.set(
                                (cursor + column * scale + sx) as i64,
                                (y + row_index * scale + sy) as i64,
                                LABEL,
                            );
                        }
                    }
                }
            }
        }
        cursor += 4 * scale;
    }
}

/// Renders every cut as a row of two panels — scan on the left, rebuild
/// on the right over a ghost of the scan — sharing one mapping so the two
/// are directly comparable.
pub fn render_sections(
    scan: &TriangleMesh,
    rebuild: &TriangleMesh,
    transform: &RigidTransform,
    cuts: &[Cut],
    panel: usize,
    reach: f64,
    fixed_scale: bool,
) -> Vec<u8> {
    let identity = RigidTransform::IDENTITY;
    let gutter = 12usize;
    let caption = 22usize;
    let width = panel * 3 + gutter * 4;
    let height = cuts.len() * (panel + caption + gutter) + gutter;
    let mut canvas = Canvas::new(width, height);
    // One mapping for every cut, so a slice sweep can be read as a
    // sequence: a feature that shrinks with height looks like it shrinks,
    // instead of every slice being blown up to fill its own panel and all
    // of them looking the same size.
    let shared = fixed_scale.then(|| {
        let mut lo = (f64::INFINITY, f64::INFINITY);
        let mut hi = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for cut in cuts {
            for segment in plane_section(scan, transform, cut) {
                for point in segment {
                    lo = (lo.0.min(point.0), lo.1.min(point.1));
                    hi = (hi.0.max(point.0), hi.1.max(point.1));
                }
            }
        }
        (lo, hi)
    });

    for (index, cut) in cuts.iter().enumerate() {
        let scan_segments = plane_section(scan, transform, cut);
        // The rebuild is already in the datum frame.
        let rebuild_segments = plane_section(rebuild, &identity, cut);
        let top = gutter + index * (panel + caption + gutter);

        // One shared mapping, driven by the scan so the rebuild's own
        // extents can never silently rescale the comparison.
        let (mut lo, mut hi) = shared.unwrap_or((
            (f64::INFINITY, f64::INFINITY),
            (f64::NEG_INFINITY, f64::NEG_INFINITY),
        ));
        if shared.is_none() {
            for segment in scan_segments.iter().chain(rebuild_segments.iter()) {
                for point in segment {
                    lo = (lo.0.min(point.0), lo.1.min(point.1));
                    hi = (hi.0.max(point.0), hi.1.max(point.1));
                }
            }
        }
        if !lo.0.is_finite() {
            continue;
        }
        let span = (hi.0 - lo.0).max(hi.1 - lo.1).max(1e-6);
        let scale = (panel as f64 * 0.92) / span;
        let center = ((lo.0 + hi.0) / 2.0, (lo.1 + hi.1) / 2.0);

        let mut missing_length = 0.0;
        let mut total_length = 0.0;
        for column in 0..3 {
            let frame = Frame {
                x0: gutter + column * (panel + gutter),
                y0: top,
                width: panel,
                height: panel,
                center,
                scale,
            };
            let fill = if column == 0 { SCAN_FILL } else { GHOST_FILL };
            fill_section(&mut canvas, &frame, &scan_segments, fill);
            // Axis marker, so a radial gap is easy to place.
            canvas.line(frame.at((0.0, hi.1)), frame.at((0.0, lo.1)), AXIS_LINE, 1);
            match column {
                0 => {
                    for segment in &scan_segments {
                        canvas.line(frame.at(segment[0]), frame.at(segment[1]), SCAN_LINE, 1);
                    }
                }
                1 => {
                    for segment in &rebuild_segments {
                        canvas.line(frame.at(segment[0]), frame.at(segment[1]), REBUILD_LINE, 1);
                    }
                }
                _ => {
                    // Every piece of the scan's outline the rebuild does
                    // not account for, in red. This is the panel that
                    // answers "what is missing" without needing an eye
                    // for it.
                    for segment in &scan_segments {
                        let length =
                            (segment[1].0 - segment[0].0).hypot(segment[1].1 - segment[0].1);
                        total_length += length;
                        let color = if covered(segment, &rebuild_segments, reach) {
                            MATCHED_LINE
                        } else {
                            missing_length += length;
                            MISSING_LINE
                        };
                        let weight = if color == MISSING_LINE { 2 } else { 1 };
                        canvas.line(frame.at(segment[0]), frame.at(segment[1]), color, weight);
                    }
                }
            }
            let title = match column {
                0 => "SCAN".to_owned(),
                1 => "REBUILD".to_owned(),
                _ => format!(
                    "MISSING {:.0} PCT",
                    100.0 * missing_length / total_length.max(1e-9)
                ),
            };
            text(
                &mut canvas,
                frame.x0 + 4,
                top + panel + 6,
                &format!("{title}  {}", cut.label),
                2,
            );
        }
    }
    encode_png(width, height, &canvas.pixels)
}

/// Names what the rebuild is missing, per cut, as text.
///
/// The picture shows you *that* something is missing; this says *what*.
/// Unmatched pieces of the scan's outline are clustered by proximity and
/// each cluster reported with its extent and the surface it implies — in
/// a meridian cut a vertical run is a cylinder, a horizontal run a flat
/// annulus, anything between a cone.
pub fn missing_report(
    scan: &TriangleMesh,
    rebuild: &TriangleMesh,
    transform: &RigidTransform,
    cuts: &[Cut],
    reach: f64,
) -> Vec<String> {
    const CLUSTER_REACH: f64 = 2.0;
    const REPORTABLE: f64 = 1.5;
    let identity = RigidTransform::IDENTITY;
    let mut lines = Vec::new();
    for cut in cuts {
        let scan_segments = plane_section(scan, transform, cut);
        let rebuild_segments = plane_section(rebuild, &identity, cut);
        let unmatched: Vec<&[(f64, f64); 2]> = scan_segments
            .iter()
            .filter(|segment| !covered(segment, &rebuild_segments, reach))
            .collect();
        // Greedy proximity clustering over segment midpoints.
        struct Cluster {
            x: (f64, f64),
            y: (f64, f64),
            length: f64,
            run: (f64, f64),
        }
        let mut clusters: Vec<Cluster> = Vec::new();
        for segment in unmatched {
            let mid = (
                (segment[0].0 + segment[1].0) / 2.0,
                (segment[0].1 + segment[1].1) / 2.0,
            );
            let length = (segment[1].0 - segment[0].0).hypot(segment[1].1 - segment[0].1);
            let slot = clusters.iter().position(|c| {
                mid.0 >= c.x.0 - CLUSTER_REACH
                    && mid.0 <= c.x.1 + CLUSTER_REACH
                    && mid.1 >= c.y.0 - CLUSTER_REACH
                    && mid.1 <= c.y.1 + CLUSTER_REACH
            });
            match slot {
                Some(index) => {
                    let cluster = &mut clusters[index];
                    cluster.x = (cluster.x.0.min(mid.0), cluster.x.1.max(mid.0));
                    cluster.y = (cluster.y.0.min(mid.1), cluster.y.1.max(mid.1));
                    cluster.length += length;
                    cluster.run.0 += (segment[1].0 - segment[0].0).abs();
                    cluster.run.1 += (segment[1].1 - segment[0].1).abs();
                }
                None => clusters.push(Cluster {
                    x: (mid.0, mid.0),
                    y: (mid.1, mid.1),
                    length,
                    run: (
                        (segment[1].0 - segment[0].0).abs(),
                        (segment[1].1 - segment[0].1).abs(),
                    ),
                }),
            }
        }
        clusters.sort_by(|a, b| b.length.total_cmp(&a.length));
        for cluster in clusters.iter().filter(|c| c.length >= REPORTABLE) {
            let meridian = cut.up.z.abs() > 0.5;
            let shape = if !meridian {
                "arc"
            } else if cluster.run.1 > 3.0 * cluster.run.0 {
                "CYLINDER (vertical run)"
            } else if cluster.run.0 > 3.0 * cluster.run.1 {
                "flat annulus (horizontal run)"
            } else {
                "cone or fillet (slanted run)"
            };
            if meridian {
                lines.push(format!(
                    "  {}: {shape}, {:.1} mm of outline at radius {:.2}..{:.2}, z {:+.2}..{:+.2}",
                    cut.label,
                    cluster.length,
                    cluster.x.0.abs().min(cluster.x.1.abs()),
                    cluster.x.0.abs().max(cluster.x.1.abs()),
                    cluster.y.0,
                    cluster.y.1
                ));
            } else {
                // A level cut lives in (x, y); radius and azimuth are what
                // actually locate a feature on a revolved part.
                let corners = [
                    (cluster.x.0, cluster.y.0),
                    (cluster.x.0, cluster.y.1),
                    (cluster.x.1, cluster.y.0),
                    (cluster.x.1, cluster.y.1),
                ];
                let radii: Vec<f64> = corners.iter().map(|(x, y)| x.hypot(*y)).collect();
                let mid = (
                    (cluster.x.0 + cluster.x.1) / 2.0,
                    (cluster.y.0 + cluster.y.1) / 2.0,
                );
                lines.push(format!(
                    "  {}: {:.1} mm of outline at radius {:.2}..{:.2}, azimuth {:+.0} deg",
                    cut.label,
                    cluster.length,
                    radii.iter().copied().fold(f64::INFINITY, f64::min),
                    radii.iter().copied().fold(f64::NEG_INFINITY, f64::max),
                    mid.1.atan2(mid.0).to_degrees()
                ));
            }
        }
    }
    lines
}

/// Level cuts at a fixed increment across a stated height range — the
/// form you want when walking a specific feature rather than sampling the
/// whole part.
pub fn stepped_levels(z_from: f64, z_to: f64, step: f64) -> Vec<Cut> {
    let mut cuts = Vec::new();
    if step <= 0.0 {
        return cuts;
    }
    let count = (((z_to - z_from) / step).round() as i64).max(0);
    for index in 0..=count {
        let z = z_from + step * index as f64;
        cuts.push(Cut {
            label: format!("Z {z:+.2}"),
            origin: Point3::new(0.0, 0.0, z),
            normal: Vector3::new(0.0, 0.0, 1.0),
            right: Vector3::new(1.0, 0.0, 0.0),
            up: Vector3::new(0.0, 1.0, 0.0),
        });
    }
    cuts
}

/// Meridian cuts through the axis plus level cuts across it, spanning the
/// part's height.
pub fn default_cuts(z_low: f64, z_high: f64, meridians: usize, levels: usize) -> Vec<Cut> {
    let mut cuts = Vec::new();
    for index in 0..meridians {
        let angle = std::f64::consts::PI * index as f64 / meridians.max(1) as f64;
        // Cut plane contains the axis; the section is drawn as radius
        // across against height up, so a cylinder is a vertical line.
        cuts.push(Cut {
            label: format!("MERIDIAN {:.0}", angle.to_degrees()),
            origin: Point3::new(0.0, 0.0, 0.0),
            normal: Vector3::new(-angle.sin(), angle.cos(), 0.0),
            right: Vector3::new(angle.cos(), angle.sin(), 0.0),
            up: Vector3::new(0.0, 0.0, 1.0),
        });
    }
    for index in 0..levels {
        let t = (index as f64 + 0.5) / levels.max(1) as f64;
        let z = z_low + (z_high - z_low) * t;
        cuts.push(Cut {
            label: format!("Z {z:+.1}"),
            origin: Point3::new(0.0, 0.0, z),
            normal: Vector3::new(0.0, 0.0, 1.0),
            right: Vector3::new(1.0, 0.0, 0.0),
            up: Vector3::new(0.0, 1.0, 0.0),
        });
    }
    cuts
}
