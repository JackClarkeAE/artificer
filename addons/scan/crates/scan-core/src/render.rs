//! Software mesh rendering: a small z-buffered rasterizer and a
//! dependency-free PNG writer.
//!
//! This lives in the core rather than the CLI because every surface
//! that shows a mesh — the snapshot command, CI images, the simulator
//! lab's live preview — draws through the same camera and shading, and
//! a preview that renders differently from the artifact it previews is
//! a diagnostic hazard, not a convenience.

use crate::mesh::TriangleMesh;
use artificer_geometry::{Point3, Vector3};

pub const BACKGROUND: [u8; 3] = [20, 22, 25];
pub const SCAN_GRAY: [u8; 3] = [184, 186, 191];

#[derive(Clone, Copy, Debug)]
pub struct Camera {
    pub theta: f64,
    pub phi: f64,
    /// Camera distance as a multiple of the model diagonal.
    pub radius_scale: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            theta: 0.6,
            phi: 0.7,
            radius_scale: 1.05,
        }
    }
}

impl Camera {
    pub const TOP: Self = Self {
        theta: 0.0,
        phi: 0.081,
        radius_scale: 1.05,
    };
}

pub struct Framebuffer {
    pub width: usize,
    pub height: usize,
    pub color: Vec<[u8; 3]>,
    pub depth: Vec<f32>,
}

impl Framebuffer {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            color: vec![BACKGROUND; width * height],
            depth: vec![f32::INFINITY; width * height],
        }
    }
}

/// View-space transform: world point relative to eye, expressed in the
/// camera basis (x right, y up, z toward the viewer).
pub struct View {
    eye: Point3,
    right: Vector3,
    up: Vector3,
    back: Vector3,
    /// Depth of the near plane, in millimetres, scaled to the model.
    near: f64,
}

impl View {
    pub fn new(camera: &Camera, center: Point3, diagonal: f64) -> Self {
        let radius = diagonal * camera.radius_scale;
        // The near plane has to scale with the model: a fixed absolute one
        // is either behind the geometry on a small part or in front of it on
        // a large one, and a vertex clipped to a near plane a micron from
        // the eye projects to coordinates in the hundreds of millions.
        let near = radius * 1.0e-3;
        let (st, ct) = camera.theta.sin_cos();
        let (sp, cp) = camera.phi.sin_cos();
        let eye = center + Vector3::new(radius * sp * ct, radius * sp * st, radius * cp);
        let back = normalize(eye - center);
        let world_up = Vector3::new(0.0, 0.0, 1.0);
        let right = normalize(world_up.cross(back));
        let up = back.cross(right);
        Self {
            eye,
            right,
            up,
            back,
            near,
        }
    }

    pub fn to_view(&self, p: Point3) -> Vector3 {
        let d = p - self.eye;
        Vector3::new(d.dot(self.right), d.dot(self.up), d.dot(self.back))
    }
}

/// Cuts a view-space triangle against the near plane, returning the up to
/// two triangles in front of it and how many there are.
///
/// A triangle with a vertex behind the eye has no projection, so the whole
/// triangle used to be skipped — which punches a hole in the surface the
/// moment the camera comes close to it, exactly when the operator is
/// inspecting something. Cutting keeps the part that is genuinely in view.
/// The polygon that survives one plane has three or four corners, which is
/// one or two triangles; nothing here allocates.
fn clip_to_near(triangle: [Vector3; 3], near: f64) -> ([[Vector3; 3]; 2], usize) {
    let ahead = |v: Vector3| -v.z - near;
    let mut polygon = [Vector3::default(); 4];
    let mut corners = 0usize;
    for index in 0..3 {
        let current = triangle[index];
        let next = triangle[(index + 1) % 3];
        let (from, to) = (ahead(current), ahead(next));
        if from >= 0.0 {
            polygon[corners] = current;
            corners += 1;
        }
        if (from >= 0.0) != (to >= 0.0) {
            // The edge crosses the plane exactly once; keep the crossing.
            polygon[corners] = current + (next - current) * (from / (from - to));
            corners += 1;
        }
    }
    match corners {
        3 => (
            [
                [polygon[0], polygon[1], polygon[2]],
                [Vector3::default(); 3],
            ],
            1,
        ),
        4 => (
            [
                [polygon[0], polygon[1], polygon[2]],
                [polygon[0], polygon[2], polygon[3]],
            ],
            2,
        ),
        _ => ([[Vector3::default(); 3]; 2], 0),
    }
}

fn normalize(v: Vector3) -> Vector3 {
    let length = v.length();
    if length > 1e-12 {
        v / length
    } else {
        Vector3::new(0.0, 0.0, 1.0)
    }
}

/// Renders one pane of the model into the framebuffer viewport
/// `[x0, x0 + pane_width)`.
pub fn render_pane(
    frame: &mut Framebuffer,
    x0: usize,
    pane_width: usize,
    mesh: &TriangleMesh,
    view: &View,
    colors: Option<&[[u8; 3]]>,
) {
    let height = frame.height as f64;
    let width = pane_width as f64;
    let fov_scale = 1.0 / (0.4f64).tan();
    // Perspective: screen x = vx / -vz * scale, with -vz the depth. The
    // third component is the *reciprocal* of depth, because that is the
    // quantity that varies linearly across the screen under a perspective
    // projection — see the interpolation below.
    let project = |v: Vector3| -> Option<(f64, f64, f64)> {
        let depth = -v.z;
        if depth <= 0.0 {
            return None;
        }
        let ndc_x = v.x / depth * fov_scale / (width / height);
        let ndc_y = v.y / depth * fov_scale;
        Some((
            (ndc_x * 0.5 + 0.5) * width,
            (0.5 - ndc_y * 0.5) * height,
            depth.recip(),
        ))
    };
    for face in 0..mesh.triangles().len() {
        let [pa, pb, pc] = mesh.triangle_points(face);
        let (va, vb, vc) = (view.to_view(pa), view.to_view(pb), view.to_view(pc));
        // Face normal in view space for two-sided headlight shading. It
        // comes off the original triangle, so a cut piece is shaded like the
        // face it was cut from rather than like its own clipped corners.
        let normal = normalize((vb - va).cross(vc - va));
        let light = (0.30 + 0.62 * normal.z.abs() + 0.08 * normal.y.abs()).min(1.0);
        let base = colors.map_or(SCAN_GRAY, |c| c[face]);
        let shaded = [
            (base[0] as f64 * light) as u8,
            (base[1] as f64 * light) as u8,
            (base[2] as f64 * light) as u8,
        ];
        let (pieces, count) = clip_to_near([va, vb, vc], view.near);
        for piece in pieces.iter().take(count) {
            let (Some(a), Some(b), Some(c)) =
                (project(piece[0]), project(piece[1]), project(piece[2]))
            else {
                continue;
            };
            let min_x = a.0.min(b.0).min(c.0).floor().max(0.0) as usize;
            let max_x = (a.0.max(b.0).max(c.0).ceil() as usize).min(pane_width.saturating_sub(1));
            let min_y = a.1.min(b.1).min(c.1).floor().max(0.0) as usize;
            let max_y = (a.1.max(b.1).max(c.1).ceil() as usize).min(frame.height.saturating_sub(1));
            let area = (b.0 - a.0) * (c.1 - a.1) - (c.0 - a.0) * (b.1 - a.1);
            if area.abs() < 1e-9 {
                continue;
            }
            for y in min_y..=max_y {
                for x in min_x..=max_x {
                    let px = x as f64 + 0.5;
                    let py = y as f64 + 0.5;
                    let w0 = ((b.0 - a.0) * (py - a.1) - (px - a.0) * (b.1 - a.1)) / area;
                    let w1 = ((px - a.0) * (c.1 - a.1) - (c.0 - a.0) * (py - a.1)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }
                    // Depth itself is not linear in screen space under a
                    // perspective projection; its reciprocal is. Averaging
                    // the depths directly puts a steeply-angled triangle at
                    // the wrong distance and lets it win the buffer against
                    // a face that is genuinely nearer.
                    let inverse_depth = a.2 * w2 + b.2 * w1 + c.2 * w0;
                    if inverse_depth <= 0.0 {
                        continue;
                    }
                    let depth = inverse_depth.recip() as f32;
                    let index = y * frame.width + x0 + x;
                    if depth < frame.depth[index] {
                        frame.depth[index] = depth;
                        frame.color[index] = shaded;
                    }
                }
            }
        }
    }
}

/// Renders two meshes side by side — the raw framebuffer, for callers
/// that show pixels directly rather than writing a file.
pub fn render_comparison_rgb(
    left: &TriangleMesh,
    right: &TriangleMesh,
    right_colors: Option<&[[u8; 3]]>,
    camera: &Camera,
    width: usize,
    height: usize,
) -> Framebuffer {
    let mut frame = Framebuffer::new(width, height);
    let bounds = left.bounds();
    let (center, diagonal) = bounds.map_or((Point3::default(), 1.0), |b| {
        (
            Point3::new(
                (b.min.x + b.max.x) / 2.0,
                (b.min.y + b.max.y) / 2.0,
                (b.min.z + b.max.z) / 2.0,
            ),
            (b.max - b.min).length(),
        )
    });
    let view = View::new(camera, center, diagonal);
    let pane = width / 2;
    render_pane(&mut frame, 0, pane, left, &view, None);
    render_pane(&mut frame, pane, width - pane, right, &view, right_colors);
    for y in 0..height {
        frame.color[y * width + pane] = [42, 46, 53];
    }
    frame
}

/// Renders two different meshes side by side: the scan (gray, left) and
/// the rebuild or simulation (right), same camera, encoded as PNG.
pub fn render_comparison(
    left: &TriangleMesh,
    right: &TriangleMesh,
    right_colors: &[[u8; 3]],
    camera: &Camera,
    width: usize,
    height: usize,
) -> Vec<u8> {
    let frame = render_comparison_rgb(left, right, Some(right_colors), camera, width, height);
    encode_png(width, height, &frame.color)
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (n, entry) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *entry = c;
    }
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc = table[((crc ^ byte as u32) & 0xff) as usize] ^ (crc >> 8);
    }
    crc ^ 0xffff_ffff
}

fn adler32(bytes: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in bytes {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = Vec::with_capacity(4 + data.len());
    body.extend_from_slice(kind);
    body.extend_from_slice(data);
    let crc = crc32(&body);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Minimal PNG encoder: 8-bit RGB, zlib stream of stored (uncompressed)
/// deflate blocks. Every PNG reader accepts stored blocks.
pub fn encode_png(width: usize, height: usize, pixels: &[[u8; 3]]) -> Vec<u8> {
    let mut raw = Vec::with_capacity(height * (1 + width * 3));
    for y in 0..height {
        raw.push(0);
        for x in 0..width {
            raw.extend_from_slice(&pixels[y * width + x]);
        }
    }
    let mut deflate = vec![0x78, 0x01];
    for (index, chunk) in raw.chunks(65_535).enumerate() {
        let last = (index + 1) * 65_535 >= raw.len();
        deflate.push(u8::from(last));
        deflate.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        deflate.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
        deflate.extend_from_slice(chunk);
    }
    deflate.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &deflate);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synth;

    fn v(x: f64, y: f64, z: f64) -> Vector3 {
        Vector3::new(x, y, z)
    }

    /// Depth is `-z`, so a vertex is in front of a near plane at `near` when
    /// `-z >= near`.
    #[test]
    fn a_triangle_entirely_in_front_is_passed_through_whole() {
        let triangle = [v(0.0, 0.0, -10.0), v(1.0, 0.0, -10.0), v(0.0, 1.0, -12.0)];
        let (pieces, count) = clip_to_near(triangle, 1.0);
        assert_eq!(count, 1);
        assert_eq!(pieces[0], triangle);
    }

    #[test]
    fn a_triangle_entirely_behind_is_dropped() {
        let triangle = [v(0.0, 0.0, 1.0), v(1.0, 0.0, 2.0), v(0.0, 1.0, 0.5)];
        assert_eq!(clip_to_near(triangle, 1.0).1, 0);
    }

    #[test]
    fn one_vertex_behind_the_near_plane_becomes_two_triangles() {
        // Two corners in front, one behind: the surviving polygon is a quad,
        // which is two triangles. Dropping it whole is what used to put a
        // hole in the surface when the camera came close.
        let triangle = [v(0.0, 0.0, -10.0), v(4.0, 0.0, -10.0), v(0.0, 4.0, 4.0)];
        let (pieces, count) = clip_to_near(triangle, 1.0);
        assert_eq!(count, 2, "a quad is two triangles");
        for piece in pieces.iter().take(count) {
            for corner in piece {
                assert!(
                    -corner.z >= 1.0 - 1e-9,
                    "every surviving corner must be in front of the near plane: {corner:?}"
                );
            }
        }
    }

    #[test]
    fn two_vertices_behind_the_near_plane_leave_one_triangle() {
        let triangle = [v(0.0, 0.0, -10.0), v(4.0, 0.0, 4.0), v(0.0, 4.0, 4.0)];
        let (pieces, count) = clip_to_near(triangle, 1.0);
        assert_eq!(count, 1);
        for corner in &pieces[0] {
            assert!(
                -corner.z >= 1.0 - 1e-9,
                "corner behind the plane: {corner:?}"
            );
        }
    }

    #[test]
    fn clipping_lands_the_cut_exactly_on_the_near_plane() {
        // The crossing point is what a naive drop-the-triangle renderer
        // never computes, so its position is worth pinning.
        let triangle = [v(0.0, 0.0, -3.0), v(0.0, 0.0, 1.0), v(0.0, 2.0, -3.0)];
        let (pieces, count) = clip_to_near(triangle, 1.0);
        assert!(count >= 1);
        let on_plane = pieces
            .iter()
            .take(count)
            .flatten()
            .filter(|corner| (-corner.z - 1.0).abs() < 1e-9)
            .count();
        assert!(on_plane >= 1, "the cut has to sit on the plane");
    }

    #[test]
    fn a_near_face_hides_a_far_one_whatever_angle_they_meet_at() {
        // Perspective-correct depth matters most for steeply-angled
        // geometry: interpolating depth linearly across the screen puts the
        // middle of a slanted triangle nearer than it is, which is how a far
        // surface used to punch through a near one.
        let mesh = synth::plate_with_boss();
        let bounds = mesh.bounds().expect("the fixture has extent");
        let center = Point3::new(
            (bounds.min.x + bounds.max.x) / 2.0,
            (bounds.min.y + bounds.max.y) / 2.0,
            (bounds.min.z + bounds.max.z) / 2.0,
        );
        let diagonal = (bounds.max - bounds.min).length();
        let view = View::new(&Camera::default(), center, diagonal);
        let mut frame = Framebuffer::new(96, 96);
        render_pane(&mut frame, 0, 96, &mesh, &view, None);

        let drawn = frame.depth.iter().filter(|d| d.is_finite()).count();
        assert!(
            drawn > 500,
            "the plate should cover much of the frame: {drawn}"
        );
        assert!(
            frame.depth.iter().all(|d| !d.is_nan() && *d > 0.0),
            "no pixel may take a negative or NaN depth"
        );
        // Every written depth has to sit between the near plane and the far
        // side of the model, which a mis-interpolated depth would not.
        let far = diagonal * 4.0;
        assert!(
            frame
                .depth
                .iter()
                .filter(|d| d.is_finite())
                .all(|&d| f64::from(d) > 0.0 && f64::from(d) < far),
            "a depth landed outside the model's own extent"
        );
    }
}
