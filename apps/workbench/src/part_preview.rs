//! Small isometric pictures of library parts.
//!
//! A part's picture is drawn once, when the part is saved into the library,
//! and kept beside its package; the library list then shows it without
//! evaluating anything. Drawing happens here on the CPU rather than through
//! the viewport's GPU path, because saving into the library must work where
//! no window, surface or adapter exists. The picture is small, so a z-buffered
//! triangle fill with a supersampled edge pass is all it needs.
//!
//! The view is the viewport's own isometric one, the orientation the ISO
//! button returns to, fitted to the part with a margin. The background is
//! transparent, so the picture sits on the light and dark themes alike.

use artificer_catalog::{
    CatalogError, CatalogStore, ContentDigest, PartPackage, PartPreview, PartPreviewFacts,
};
use artificer_kernel::{CancellationToken, DebugScene, NativeKernel};
use artificer_protocol::{
    CURRENT_PROTOCOL_VERSION, ExecuteRequest, PrecisionPolicy, RequestId, Vector3,
};
use artificer_ui_core::presentation::ViewState;

use crate::library_catalog::resolve_insertion;
use crate::part_library::{
    ALUMINIUM_EXTRUSION_20X20_NAME, LENGTH_PARAMETER_KEY, ParameterValueSource,
    PartInsertionIntent, PartParameterAssignment,
};

/// The length a library extrusion is drawn at: five times its section, so
/// the picture reads as a length of profile rather than a cube.
pub const SAMPLE_LENGTH_MM: f64 = 100.0;

/// The stored picture's width and height, in pixels. The list shows it at
/// half this, so it stays sharp on a high-density display.
pub const PREVIEW_SIZE_PX: u32 = 96;
/// Samples per pixel along each axis while drawing; averaged down after.
const SUPERSAMPLE: u32 = 3;
/// The empty border around the fitted part, as a share of the picture.
const MARGIN: f64 = 0.08;
/// The part's colour: the viewport's unassigned-body grey.
const BODY_RGB: [f64; 3] = [0.80, 0.82, 0.85];
/// The colour of the part's creases and outline.
const EDGE_RGB: [f64; 3] = [0.17, 0.19, 0.23];
const AMBIENT: f64 = 0.42;
const DIFFUSE: f64 = 0.58;

/// Saves a part into the library: publishes its package and keeps its
/// picture beside it.
///
/// The picture is drawn only when the store has none for this exact
/// package, so reopening the library draws nothing. A part that cannot be
/// drawn is still a part, and is published without a picture; a picture
/// that cannot be kept costs a redraw next time, not the part.
pub fn publish_with_preview(
    store: &CatalogStore,
    package: &PartPackage,
) -> Result<(ContentDigest, Option<PartPreview>), CatalogError> {
    let digest = store.publish(package)?;
    if let Some(kept) = store.preview(digest)? {
        return Ok((digest, Some(kept)));
    }
    let preview = draw_package_preview(package).ok();
    if let Some(preview) = &preview {
        let _ = store.save_preview(digest, preview);
    }
    Ok((digest, preview))
}

/// Draws the preview a package is shown with in the library: the part at its
/// sample length, and what it measures.
///
/// An extent is named as the parameter's rather than given as a number when
/// changing the parameter changes it, which is found by building the part at
/// a second length and comparing.
pub fn draw_package_preview(package: &PartPackage) -> Result<PartPreview, String> {
    if crate::saved_parts::is_saved_part(package) {
        return draw_saved_part_preview(package);
    }
    let scene = package_scene(package, SAMPLE_LENGTH_MM)?;
    let extents_mm = scene_extents(&scene).ok_or("the part has no extent to measure")?;
    let longer = scene_extents(&package_scene(package, SAMPLE_LENGTH_MM * 1.5)?)
        .ok_or("the part has no extent to measure")?;
    let driven_by = std::array::from_fn(|axis| {
        ((longer[axis] - extents_mm[axis]).abs() > 1.0e-6).then(|| "Length".to_owned())
    });
    Ok(PartPreview {
        image_png: render_isometric_png(&scene)?,
        facts: PartPreviewFacts {
            extents_mm,
            driven_by,
            sample: Some(format!("Length {SAMPLE_LENGTH_MM} mm")),
        },
    })
}

/// Draws a saved part at the values it was saved with. An extent is named
/// as a length parameter's when building the part with that parameter half
/// as large again moves it.
fn draw_saved_part_preview(package: &PartPackage) -> Result<PartPreview, String> {
    use artificer_catalog::{ParameterDomain, RealQuantity};
    use std::collections::BTreeMap;

    let scene_at = |values: &BTreeMap<String, f64>| -> Result<DebugScene, String> {
        crate::saved_parts::evaluate_saved_part(package, values)
            .map(|evaluated| NativeKernel::debug_scene(&evaluated.outcome.snapshot))
            .map_err(|error| error.to_string())
    };
    let scene = scene_at(&BTreeMap::new())?;
    let extents_mm = scene_extents(&scene).ok_or("the part has no extent to measure")?;
    let mut driven_by: [Option<String>; 3] = Default::default();
    let mut sample = Vec::new();
    for spec in package.definition().parameters() {
        let ParameterDomain::Real {
            quantity,
            default: Some(default),
            rules,
            ..
        } = spec.domain()
        else {
            continue;
        };
        let value = default.get();
        sample.push(match quantity {
            RealQuantity::Length => format!("{} {} mm", spec.label(), trim(value)),
            RealQuantity::Angle => format!("{} {}°", spec.label(), trim(value.to_degrees())),
            RealQuantity::Scalar => format!("{} {}", spec.label(), trim(value)),
        });
        if *quantity != RealQuantity::Length || value <= 0.0 {
            continue;
        }
        let limit = rules
            .maximum()
            .map_or(f64::INFINITY, |maximum| maximum.get());
        let moved = if value * 1.5 <= limit {
            value * 1.5
        } else {
            value * 0.75
        };
        let values = BTreeMap::from([(spec.id().as_str().to_owned(), moved)]);
        let Some(other) = scene_at(&values)
            .ok()
            .and_then(|scene| scene_extents(&scene))
        else {
            continue;
        };
        for axis in 0..3 {
            if driven_by[axis].is_none() && (other[axis] - extents_mm[axis]).abs() > 1.0e-6 {
                driven_by[axis] = Some(spec.label().to_owned());
            }
        }
    }
    Ok(PartPreview {
        image_png: render_isometric_png(&scene)?,
        facts: PartPreviewFacts {
            extents_mm,
            driven_by,
            sample: (!sample.is_empty()).then(|| sample.join(", ")),
        },
    })
}

fn trim(value: f64) -> String {
    let text = format!("{value:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// Builds the package's part at one length, as an insertion would.
fn package_scene(package: &PartPackage, length_mm: f64) -> Result<DebugScene, String> {
    let definition = package.definition();
    let revision = definition.revision();
    let intent = PartInsertionIntent {
        staging_id: 0,
        definition_key: definition.id().as_str().to_owned(),
        definition_revision: [revision.major(), revision.minor(), revision.patch()],
        definition_digest: package.content_digest().to_hex(),
        display_name: ALUMINIUM_EXTRUSION_20X20_NAME.to_owned(),
        parameters: vec![PartParameterAssignment {
            key: LENGTH_PARAMETER_KEY.to_owned(),
            display_name: "Length".to_owned(),
            value: length_mm,
            source: ParameterValueSource::Entered,
        }],
    };
    let resolved = resolve_insertion(package, &intent).map_err(|error| error.to_string())?;
    let empty = NativeKernel::empty();
    let outcome = NativeKernel::execute(
        &empty,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("library-preview"),
            expected_snapshot: empty.id(),
            precision: PrecisionPolicy::default(),
            command: resolved.command().clone(),
        },
        &CancellationToken::new(),
    )
    .map_err(|error| error.to_string())?;
    Ok(NativeKernel::debug_scene(&outcome.snapshot))
}

/// Draws `scene` in the isometric view, fitted, and returns it as a PNG.
pub fn render_isometric_png(scene: &DebugScene) -> Result<Vec<u8>, String> {
    let rgba = render_isometric_rgba(scene)?;
    encode_png(&rgba, PREVIEW_SIZE_PX, PREVIEW_SIZE_PX)
}

/// The drawn part's bounding box along X, Y and Z, in its own units.
#[must_use]
pub fn scene_extents(scene: &DebugScene) -> Option<[f64; 3]> {
    let mut low = [f64::INFINITY; 3];
    let mut high = [f64::NEG_INFINITY; 3];
    for point in scene
        .triangles
        .iter()
        .flat_map(|triangle| triangle.vertices.iter())
    {
        for (axis, value) in [point.x, point.y, point.z].into_iter().enumerate() {
            low[axis] = low[axis].min(value);
            high[axis] = high[axis].max(value);
        }
    }
    low.iter()
        .all(|value| value.is_finite())
        .then(|| [high[0] - low[0], high[1] - low[1], high[2] - low[2]])
}

/// A point in picture space: across, down, and toward the viewer.
#[derive(Clone, Copy)]
struct Projected {
    x: f64,
    y: f64,
    depth: f64,
}

fn render_isometric_rgba(scene: &DebugScene) -> Result<Vec<u8>, String> {
    if scene.triangles.is_empty() {
        return Err("the part has no faces to draw".into());
    }
    let view = ViewState::default();
    let camera = |x: f64, y: f64, z: f64| {
        let projection = view.project_direction(Vector3::new(x, y, z));
        Projected {
            x: projection.coordinates[0],
            y: projection.coordinates[1],
            depth: projection.depth,
        }
    };

    let triangles = scene
        .triangles
        .iter()
        .map(|triangle| {
            let corners = triangle
                .vertices
                .map(|point| camera(point.x, point.y, point.z));
            let normals = triangle
                .normals
                .map(|normal| camera(normal.x, normal.y, normal.z));
            (corners, normals)
        })
        .collect::<Vec<_>>();
    let edges = scene
        .edges
        .iter()
        .filter(|edge| !edge.is_smooth && !edge.is_tangent)
        .map(|edge| {
            edge.endpoints
                .map(|point| camera(point.x, point.y, point.z))
        })
        .collect::<Vec<_>>();

    let (mut low_x, mut low_y, mut high_x, mut high_y) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    let (mut low_depth, mut high_depth) = (f64::INFINITY, f64::NEG_INFINITY);
    for point in triangles.iter().flat_map(|(corners, _)| corners.iter()) {
        low_x = low_x.min(point.x);
        high_x = high_x.max(point.x);
        low_y = low_y.min(point.y);
        high_y = high_y.max(point.y);
        low_depth = low_depth.min(point.depth);
        high_depth = high_depth.max(point.depth);
    }
    let span = (high_x - low_x).max(high_y - low_y);
    if !span.is_finite() || span <= f64::EPSILON {
        return Err("the part has no extent to fit".into());
    }

    let size = f64::from(PREVIEW_SIZE_PX * SUPERSAMPLE);
    let scale = size * (1.0 - 2.0 * MARGIN) / span;
    let offset_x = (size - (high_x - low_x) * scale) / 2.0 - low_x * scale;
    let offset_y = (size - (high_y - low_y) * scale) / 2.0 - low_y * scale;
    let to_pixels = |point: Projected| Projected {
        x: point.x * scale + offset_x,
        y: point.y * scale + offset_y,
        depth: point.depth,
    };

    let side = (PREVIEW_SIZE_PX * SUPERSAMPLE) as usize;
    let mut colour = vec![[0.0_f64; 4]; side * side];
    let mut depth = vec![f64::NEG_INFINITY; side * side];

    // Lit from above, behind the viewer's left shoulder. Picture space runs
    // across and down, so up is negative.
    let light = normalized([-0.45, -0.70, 0.55]);
    for (corners, normals) in &triangles {
        let corners = corners.map(to_pixels);
        let shades = normals.map(|normal| {
            let normal = normalized([normal.x, normal.y, normal.depth]);
            let facing = normal[0] * light[0] + normal[1] * light[1] + normal[2] * light[2];
            AMBIENT + DIFFUSE * facing.max(0.0)
        });
        fill_triangle(&corners, &shades, side, &mut colour, &mut depth);
    }

    // Creases and outlines, drawn over the faces they bound. The bias lets
    // an edge win against the faces it lies on and lose against faces in
    // front of it.
    let bias = (high_depth - low_depth).abs().max(span) * 2.0e-3;
    let thickness = f64::from(SUPERSAMPLE) * 0.55;
    for [start, end] in &edges {
        draw_edge(
            to_pixels(*start),
            to_pixels(*end),
            thickness,
            bias,
            side,
            &mut colour,
            &depth,
        );
    }

    Ok(downsample(&colour, side))
}

fn normalized(vector: [f64; 3]) -> [f64; 3] {
    let length = (vector[0] * vector[0] + vector[1] * vector[1] + vector[2] * vector[2]).sqrt();
    if length > f64::EPSILON {
        vector.map(|component| component / length)
    } else {
        [0.0, 0.0, 1.0]
    }
}

fn fill_triangle(
    corners: &[Projected; 3],
    shades: &[f64; 3],
    side: usize,
    colour: &mut [[f64; 4]],
    depth: &mut [f64],
) {
    let [a, b, c] = *corners;
    let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    if area.abs() <= f64::EPSILON {
        return;
    }
    let limit = side as f64 - 1.0;
    let left = a.x.min(b.x).min(c.x).floor().clamp(0.0, limit) as usize;
    let right = a.x.max(b.x).max(c.x).ceil().clamp(0.0, limit) as usize;
    let top = a.y.min(b.y).min(c.y).floor().clamp(0.0, limit) as usize;
    let bottom = a.y.max(b.y).max(c.y).ceil().clamp(0.0, limit) as usize;
    for row in top..=bottom {
        let y = row as f64 + 0.5;
        for column in left..=right {
            let x = column as f64 + 0.5;
            let weight_a = ((b.x - x) * (c.y - y) - (b.y - y) * (c.x - x)) / area;
            let weight_b = ((c.x - x) * (a.y - y) - (c.y - y) * (a.x - x)) / area;
            let weight_c = 1.0 - weight_a - weight_b;
            if weight_a < 0.0 || weight_b < 0.0 || weight_c < 0.0 {
                continue;
            }
            let here = weight_a * a.depth + weight_b * b.depth + weight_c * c.depth;
            let index = row * side + column;
            if here <= depth[index] {
                continue;
            }
            depth[index] = here;
            let shade = weight_a * shades[0] + weight_b * shades[1] + weight_c * shades[2];
            colour[index] = [
                BODY_RGB[0] * shade,
                BODY_RGB[1] * shade,
                BODY_RGB[2] * shade,
                1.0,
            ];
        }
    }
}

fn draw_edge(
    start: Projected,
    end: Projected,
    thickness: f64,
    bias: f64,
    side: usize,
    colour: &mut [[f64; 4]],
    depth: &[f64],
) {
    let length = ((end.x - start.x).powi(2) + (end.y - start.y).powi(2)).sqrt();
    let steps = (length * 2.0).ceil().max(1.0) as usize;
    let reach = thickness.ceil() as isize;
    for step in 0..=steps {
        let along = step as f64 / steps as f64;
        let x = start.x + (end.x - start.x) * along;
        let y = start.y + (end.y - start.y) * along;
        let here = start.depth + (end.depth - start.depth) * along;
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                let column = x.floor() as isize + dx;
                let row = y.floor() as isize + dy;
                if column < 0 || row < 0 || column >= side as isize || row >= side as isize {
                    continue;
                }
                let centre_x = column as f64 + 0.5;
                let centre_y = row as f64 + 0.5;
                if (centre_x - x).powi(2) + (centre_y - y).powi(2) > thickness * thickness {
                    continue;
                }
                let index = row as usize * side + column as usize;
                if here + bias < depth[index] {
                    continue;
                }
                colour[index] = [EDGE_RGB[0], EDGE_RGB[1], EDGE_RGB[2], 1.0];
            }
        }
    }
}

/// Averages each block of samples into one pixel, weighting colour by
/// coverage so the part's rim blends into the transparent background.
fn downsample(colour: &[[f64; 4]], side: usize) -> Vec<u8> {
    let samples = SUPERSAMPLE as usize;
    let size = PREVIEW_SIZE_PX as usize;
    let mut rgba = Vec::with_capacity(size * size * 4);
    for row in 0..size {
        for column in 0..size {
            let mut sum = [0.0_f64; 4];
            for sample_row in 0..samples {
                for sample_column in 0..samples {
                    let sample = colour
                        [(row * samples + sample_row) * side + column * samples + sample_column];
                    for channel in 0..3 {
                        sum[channel] += sample[channel] * sample[3];
                    }
                    sum[3] += sample[3];
                }
            }
            let coverage = sum[3] / (samples * samples) as f64;
            let to_byte = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
            if sum[3] > 0.0 {
                rgba.extend_from_slice(&[
                    to_byte(sum[0] / sum[3]),
                    to_byte(sum[1] / sum[3]),
                    to_byte(sum[2] / sum[3]),
                    to_byte(coverage),
                ]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    rgba
}

fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| format!("preview PNG header: {error}"))?;
    writer
        .write_image_data(rgba)
        .map_err(|error| format!("preview PNG data: {error}"))?;
    writer
        .finish()
        .map_err(|error| format!("preview PNG: {error}"))?;
    Ok(bytes)
}

/// Decodes a stored preview for display. `None` for anything that is not an
/// eight-bit RGB or RGBA PNG of a sensible size.
#[must_use]
pub fn decode_png(bytes: &[u8]) -> Option<egui::ColorImage> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let (width, height) = {
        let info = reader.info();
        (info.width as usize, info.height as usize)
    };
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return None;
    }
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buffer).ok()?;
    if frame.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let data = &buffer[..frame.buffer_size()];
    match frame.color_type {
        png::ColorType::Rgba => Some(egui::ColorImage::from_rgba_unmultiplied(
            [width, height],
            data,
        )),
        png::ColorType::Rgb => Some(egui::ColorImage::from_rgb([width, height], data)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use artificer_protocol::{KernelCommand, Point3};

    use super::*;
    use crate::library_catalog::builtin_aluminium_extrusion_package;

    fn cuboid_scene(size: [f64; 3]) -> DebugScene {
        let empty = NativeKernel::empty();
        let outcome = NativeKernel::execute(
            &empty,
            &ExecuteRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new("preview-fixture"),
                expected_snapshot: empty.id(),
                precision: PrecisionPolicy::default(),
                command: KernelCommand::MakeCuboid {
                    origin: Point3::new(0.0, 0.0, 0.0),
                    size_x: size[0],
                    size_y: size[1],
                    size_z: size[2],
                },
            },
            &CancellationToken::new(),
        )
        .expect("the fixture cuboid builds");
        NativeKernel::debug_scene(&outcome.snapshot)
    }

    /// The picture is the part, fitted: it fills the frame up to the margin
    /// on its long side, is centred, is transparent around it, and shows its
    /// faces in more than one shade.
    #[test]
    fn a_part_is_drawn_isometric_fitted_and_on_a_clear_background() {
        let scene = cuboid_scene([20.0, 20.0, 100.0]);
        let rgba = render_isometric_rgba(&scene).expect("the cuboid draws");
        let size = PREVIEW_SIZE_PX as usize;
        let alpha = |x: usize, y: usize| rgba[(y * size + x) * 4 + 3];
        assert_eq!(alpha(0, 0), 0, "the corner is background");
        assert_eq!(alpha(size / 2, size / 2), 255, "the centre is the part");

        let covered_rows = (0..size)
            .filter(|&y| (0..size).any(|x| alpha(x, y) > 0))
            .collect::<Vec<_>>();
        let first = *covered_rows.first().unwrap();
        let last = *covered_rows.last().unwrap();
        let margin = (MARGIN * size as f64).round() as usize;
        // A tall part fits by its height: its top and bottom sit on the
        // margins, give or take a pixel.
        assert!(first.abs_diff(margin) <= 1, "top at {first}");
        assert!((size - 1 - last).abs_diff(margin) <= 1, "bottom at {last}");

        let shades = (0..size * size)
            .filter(|index| rgba[index * 4 + 3] == 255)
            .map(|index| rgba[index * 4])
            .collect::<std::collections::BTreeSet<_>>();
        assert!(shades.len() >= 3, "faces and edges are told apart");
    }

    #[test]
    fn the_png_round_trips_and_extents_are_measured() {
        let scene = cuboid_scene([20.0, 30.0, 100.0]);
        let png = render_isometric_png(&scene).expect("encodes");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        let image = decode_png(&png).expect("decodes");
        assert_eq!(
            image.size,
            [PREVIEW_SIZE_PX as usize, PREVIEW_SIZE_PX as usize]
        );
        let extents = scene_extents(&scene).expect("measured");
        for (measured, expected) in extents.into_iter().zip([20.0, 30.0, 100.0]) {
            assert!((measured - expected).abs() < 1.0e-9, "{extents:?}");
        }
        assert!(decode_png(b"not a png").is_none());
    }

    /// The built-in extrusion is drawn at its sample length, and its length
    /// is reported as the parameter's while its section is measured.
    #[test]
    fn the_built_in_part_is_drawn_and_its_length_is_its_parameter() {
        let package = builtin_aluminium_extrusion_package().expect("the built-in seals");
        let preview = draw_package_preview(&package).expect("the built-in draws");
        assert!(decode_png(&preview.image_png).is_some());
        let facts = &preview.facts;
        assert_eq!(
            facts.driven_by,
            [None, None, Some("Length".to_owned())],
            "{facts:?}"
        );
        for (measured, expected) in facts
            .extents_mm
            .into_iter()
            .zip([20.0, 20.0, SAMPLE_LENGTH_MM])
        {
            assert!((measured - expected).abs() < 1.0e-9, "{facts:?}");
        }
        assert_eq!(facts.sample.as_deref(), Some("Length 100 mm"));
    }
}
