//! Offline snapshot renderer: the side-by-side viewer image without a
//! browser. A small z-buffered software rasterizer draws the original
//! scan (left) and the classified segmentation (right) with the same
//! camera and shading as the HTML viewer, and a dependency-free PNG
//! writer (zlib stored blocks) emits the file.

use artificer_geometry::{Point3, Vector3};

use crate::viewer::DisplayModel;

const BACKGROUND: [u8; 3] = [20, 22, 25];
const SCAN_GRAY: [u8; 3] = [184, 186, 191];

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

struct Framebuffer {
    width: usize,
    height: usize,
    color: Vec<[u8; 3]>,
    depth: Vec<f32>,
}

impl Framebuffer {
    fn new(width: usize, height: usize) -> Self {
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
struct View {
    eye: Point3,
    right: Vector3,
    up: Vector3,
    back: Vector3,
}

impl View {
    fn new(camera: &Camera, center: Point3, diagonal: f64) -> Self {
        let radius = diagonal * camera.radius_scale;
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
        }
    }

    fn to_view(&self, p: Point3) -> Vector3 {
        let d = p - self.eye;
        Vector3::new(d.dot(self.right), d.dot(self.up), d.dot(self.back))
    }
}

fn normalize(v: Vector3) -> Vector3 {
    let length = v.length();
    if length > 1e-12 { v / length } else { Vector3::new(0.0, 0.0, 1.0) }
}

/// Renders one pane of the model into the framebuffer viewport
/// `[x0, x0 + pane_width)`.
fn render_pane(
    frame: &mut Framebuffer,
    x0: usize,
    pane_width: usize,
    model: &DisplayModel,
    view: &View,
    colors: Option<&[[u8; 3]]>,
) {
    let mesh = &model.mesh;
    let height = frame.height as f64;
    let width = pane_width as f64;
    let fov_scale = 1.0 / (0.4f64).tan();
    // Perspective: screen x = vx / -vz * scale, with -vz the depth.
    let project = |v: Vector3| -> Option<(f64, f64, f32)> {
        let depth = -v.z;
        if depth <= 1e-6 {
            return None;
        }
        let ndc_x = v.x / depth * fov_scale / (width / height);
        let ndc_y = v.y / depth * fov_scale;
        Some((
            (ndc_x * 0.5 + 0.5) * width,
            (0.5 - ndc_y * 0.5) * height,
            depth as f32,
        ))
    };
    for face in 0..mesh.triangles().len() {
        let [pa, pb, pc] = mesh.triangle_points(face);
        let (va, vb, vc) = (view.to_view(pa), view.to_view(pb), view.to_view(pc));
        let (Some(a), Some(b), Some(c)) = (project(va), project(vb), project(vc)) else {
            continue;
        };
        // Face normal in view space for two-sided headlight shading.
        let normal = normalize((vb - va).cross(vc - va));
        let light = (0.30 + 0.62 * normal.z.abs() + 0.08 * normal.y.abs()).min(1.0);
        let base = colors.map_or(SCAN_GRAY, |c| c[face]);
        let shaded = [
            (base[0] as f64 * light) as u8,
            (base[1] as f64 * light) as u8,
            (base[2] as f64 * light) as u8,
        ];
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
                let depth = (a.2 as f64 * w2 + b.2 as f64 * w1 + c.2 as f64 * w0) as f32;
                let index = y * frame.width + x0 + x;
                if depth < frame.depth[index] {
                    frame.depth[index] = depth;
                    frame.color[index] = shaded;
                }
            }
        }
    }
}

/// Renders the side-by-side snapshot: original scan left, segmentation
/// right, divider between.
pub fn render_side_by_side(
    model: &DisplayModel,
    camera: &Camera,
    width: usize,
    height: usize,
) -> Vec<u8> {
    let mut frame = Framebuffer::new(width, height);
    let bounds = model.mesh.bounds();
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
    render_pane(&mut frame, 0, pane, model, &view, None);
    render_pane(&mut frame, pane, width - pane, model, &view, Some(&model.colors));
    for y in 0..height {
        frame.color[y * width + pane] = [42, 46, 53];
    }
    encode_png(width, height, &frame.color)
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (n, entry) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
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
fn encode_png(width: usize, height: usize, pixels: &[[u8; 3]]) -> Vec<u8> {
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

/// Plots the master tooth profile: three repeated sectors of the
/// `(azimuth, radius)` polyline, unrolled to arc-length millimetres, with
/// a faint millimetre grid. Shows the sweepable cross-section and its
/// continuity across sector boundaries.
pub fn render_profile_plot(
    profile: &artificer_scan_core::MasterProfile,
    width: usize,
    height: usize,
) -> Vec<u8> {
    let mut pixels = vec![BACKGROUND; width * height];
    let radii: Vec<f64> = profile.points.iter().map(|(_, r)| *r).collect();
    let r_low = radii.iter().fold(f64::INFINITY, |a, &b| a.min(b));
    let r_high = radii.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
    let r_mid = (r_low + r_high) / 2.0;
    let sector = std::f64::consts::TAU / profile.count as f64;
    let x_span = 3.0 * sector * r_mid;
    let y_pad = ((r_high - r_low) * 0.15).max(0.5);
    let (y_low, y_high) = (r_low - y_pad, r_high + y_pad);
    let margin = 40.0;
    let to_px = |x: f64, r: f64| -> (f64, f64) {
        (
            margin + x / x_span * (width as f64 - 2.0 * margin),
            (height as f64 - margin)
                - (r - y_low) / (y_high - y_low) * (height as f64 - 2.0 * margin),
        )
    };
    // Millimetre grid, brighter every 5 mm.
    let grid = |pixels: &mut Vec<[u8; 3]>, value: u8| -> [u8; 3] {
        let _ = pixels;
        [value, value + 2, value + 4]
    };
    let minor = grid(&mut pixels, 34);
    let major = grid(&mut pixels, 52);
    let mut r_line = y_low.ceil();
    while r_line <= y_high {
        let (_, y) = to_px(0.0, r_line);
        let color = if (r_line.round() as i64) % 5 == 0 { major } else { minor };
        let row = y.round() as isize;
        if row >= 0 && (row as usize) < height {
            for x in margin as usize..(width - margin as usize) {
                pixels[row as usize * width + x] = color;
            }
        }
        r_line += 1.0;
    }
    let mut x_line = 0.0;
    while x_line <= x_span {
        let (x, _) = to_px(x_line, 0.0);
        let color = if (x_line.round() as i64) % 5 == 0 { major } else { minor };
        let column = x.round() as isize;
        if column >= 0 && (column as usize) < width {
            for y in margin as usize..(height - margin as usize) {
                pixels[y * width + column as usize] = color;
            }
        }
        x_line += 1.0;
    }
    // Sector boundary markers.
    for k in 0..=3 {
        let (x, _) = to_px(k as f64 * sector * r_mid, 0.0);
        let column = (x.round() as isize).clamp(0, width as isize - 1) as usize;
        for y in (margin as usize / 2)..(height - margin as usize / 2) {
            pixels[y * width + column] = [70, 74, 82];
        }
    }
    // The profile, three sectors, thick polyline.
    let mut plot = |x0: f64, y0: f64, x1: f64, y1: f64, color: [u8; 3]| {
        let steps = ((x1 - x0).abs().max((y1 - y0).abs()).ceil() as usize).max(1);
        for step in 0..=steps {
            let t = step as f64 / steps as f64;
            let x = x0 + (x1 - x0) * t;
            let y = y0 + (y1 - y0) * t;
            for dy in -1..=1isize {
                for dx in -1..=1isize {
                    let px = x.round() as isize + dx;
                    let py = y.round() as isize + dy;
                    if px >= 0 && py >= 0 && (px as usize) < width && (py as usize) < height {
                        pixels[py as usize * width + px as usize] = color;
                    }
                }
            }
        }
    };
    for repeat in 0..3 {
        let color = if repeat == 1 { [120, 220, 140] } else { [90, 150, 105] };
        let offset = repeat as f64 * sector * r_mid;
        for pair in profile.points.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (x0, y0) = to_px(offset + a.0 * r_mid, a.1);
            let (x1, y1) = to_px(offset + b.0 * r_mid, b.1);
            plot(x0, y0, x1, y1, color);
        }
    }
    encode_png(width, height, &pixels)
}
