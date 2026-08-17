//! Offline snapshot renderer: the side-by-side viewer image without a
//! browser. The z-buffered rasterizer and PNG writer live in
//! `artificer_scan_core::render` (the simulator lab previews through
//! the same code); this module keeps the CLI-side compositions — the
//! DisplayModel side-by-side and the profile plot.

use artificer_geometry::Point3;

pub use artificer_scan_core::render::{
    BACKGROUND, Camera, Framebuffer, View, encode_png, render_comparison, render_pane,
};

use crate::viewer::DisplayModel;

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
    render_pane(&mut frame, 0, pane, &model.mesh, &view, None);
    render_pane(
        &mut frame,
        pane,
        width - pane,
        &model.mesh,
        &view,
        Some(&model.colors),
    );
    for y in 0..height {
        frame.color[y * width + pane] = [42, 46, 53];
    }
    encode_png(width, height, &frame.color)
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
        let color = if (r_line.round() as i64) % 5 == 0 {
            major
        } else {
            minor
        };
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
        let color = if (x_line.round() as i64) % 5 == 0 {
            major
        } else {
            minor
        };
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
        let color = if repeat == 1 {
            [120, 220, 140]
        } else {
            [90, 150, 105]
        };
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
