//! Text as sketch geometry: glyph outlines from the bundled typeface,
//! flattened into closed line loops the exact kernel can extrude.
//!
//! The outlines a TrueType font stores are quadratic Béziers, which are
//! outside the line-and-circle vocabulary the arrangement and the kernel
//! certify. Each contour is therefore flattened to a polyline at a chord
//! tolerance proportional to the text height, so a letter is many short
//! exact lines rather than one approximate curve. The flattening is
//! deterministic: the same content and height yield the same vertices on
//! every platform, which is what a content-addressed document needs.

use std::fmt;

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, MetadataProvider};

use crate::geometry::SketchPoint2;

/// The typeface every sketch text is set in.
pub const TEXT_FONT_NAME: &str = "Ubuntu Light";

/// The most vertices one text recipe may produce. Beyond this the sketch's
/// own curve budget would refuse the transaction anyway; refusing here names
/// the reason.
pub const MAX_TEXT_OUTLINE_VERTICES: usize = 4_096;

/// Chord tolerance as a fraction of the text height.
///
/// At 1/400 of the height a 10 mm letter deviates from the true glyph by at
/// most 25 µm, well under a printable feature, while a typical letter costs
/// thirty to sixty segments rather than hundreds.
pub const TEXT_CHORD_TOLERANCE_RATIO: f64 = 1.0 / 400.0;

/// Why a text could not be turned into outlines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextOutlineError {
    /// Nothing but whitespace, or nothing at all.
    EmptyContent,
    /// The typeface has no glyph for this character.
    UnsupportedCharacter(char),
    /// The height is not a finite positive length.
    InvalidHeight,
    /// The outlines would exceed [`MAX_TEXT_OUTLINE_VERTICES`].
    TooManyVertices { requested: usize, limit: usize },
    /// The bundled font failed to parse; never expected, but never a panic.
    FontUnavailable,
}

impl fmt::Display for TextOutlineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyContent => write!(formatter, "the text has no visible characters"),
            Self::UnsupportedCharacter(character) => {
                write!(formatter, "the typeface has no glyph for {character:?}")
            }
            Self::InvalidHeight => write!(formatter, "the text height must be a positive length"),
            Self::TooManyVertices { requested, limit } => write!(
                formatter,
                "the text outlines need {requested} vertices; the limit is {limit}"
            ),
            Self::FontUnavailable => write!(formatter, "the bundled typeface could not be read"),
        }
    }
}

impl std::error::Error for TextOutlineError {}

/// One closed glyph contour, in sketch units, without a repeated final point.
#[derive(Clone, Debug, PartialEq)]
pub struct TextOutlineLoop {
    pub points: Vec<SketchPoint2>,
}

/// Outlines for one line of text, laid out left to right from the origin
/// along `+u`, with the baseline on `v = 0`.
#[derive(Clone, Debug, PartialEq)]
pub struct TextOutlines {
    pub loops: Vec<TextOutlineLoop>,
    /// Pen advance after the last glyph, in sketch units.
    pub advance: f64,
}

/// Lays out `content` at cap height `height` and flattens every glyph
/// contour to a closed polyline.
///
/// `height` is the height of a capital letter, the way a drafting standard
/// specifies lettering, not the em size. Whitespace advances the pen and
/// draws nothing.
pub fn text_outlines(content: &str, height: f64) -> Result<TextOutlines, TextOutlineError> {
    if !height.is_finite() || height <= 0.0 {
        return Err(TextOutlineError::InvalidHeight);
    }
    if content.chars().all(char::is_whitespace) {
        return Err(TextOutlineError::EmptyContent);
    }
    let font = FontRef::new(epaint_default_fonts::UBUNTU_LIGHT)
        .map_err(|_| TextOutlineError::FontUnavailable)?;
    let metrics = font.metrics(Size::unscaled(), LocationRef::default());
    let cap_height = metrics
        .cap_height
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(metrics.ascent);
    if !cap_height.is_finite() || cap_height <= 0.0 {
        return Err(TextOutlineError::FontUnavailable);
    }
    let scale = height / f64::from(cap_height);
    let charmap = font.charmap();
    let glyph_metrics = font.glyph_metrics(Size::unscaled(), LocationRef::default());
    let outlines = font.outline_glyphs();
    // Font units are integers on a grid of a few thousand per em, so the
    // chord tolerance is expressed there for the flattener.
    let tolerance_units = (height * TEXT_CHORD_TOLERANCE_RATIO / scale).max(1.0e-3);

    let mut loops = Vec::new();
    let mut pen_x = 0.0_f64;
    let mut vertices = 0_usize;
    for character in content.chars() {
        let glyph = charmap
            .map(character)
            .ok_or(TextOutlineError::UnsupportedCharacter(character))?;
        let advance = glyph_metrics.advance_width(glyph).map_or(0.0, f64::from);
        if !character.is_whitespace() {
            let Some(outline) = outlines.get(glyph) else {
                return Err(TextOutlineError::UnsupportedCharacter(character));
            };
            let mut flattener = Flattener::new(tolerance_units);
            outline
                .draw(
                    DrawSettings::unhinted(Size::unscaled(), LocationRef::default()),
                    &mut flattener,
                )
                .map_err(|_| TextOutlineError::UnsupportedCharacter(character))?;
            for contour in flattener.finish() {
                let points = contour
                    .into_iter()
                    .map(|(x, y)| SketchPoint2::new(x * scale + pen_x, y * scale))
                    .collect::<Vec<_>>();
                let points = dedup_closed(points, height * TEXT_CHORD_TOLERANCE_RATIO * 0.5);
                if points.len() < 3 || polygon_area(&points).abs() <= (height * 1.0e-6).powi(2) {
                    continue;
                }
                vertices += points.len();
                if vertices > MAX_TEXT_OUTLINE_VERTICES {
                    return Err(TextOutlineError::TooManyVertices {
                        requested: vertices,
                        limit: MAX_TEXT_OUTLINE_VERTICES,
                    });
                }
                loops.push(TextOutlineLoop { points });
            }
        }
        pen_x += advance * scale;
    }
    if loops.is_empty() {
        return Err(TextOutlineError::EmptyContent);
    }
    Ok(TextOutlines {
        loops,
        advance: pen_x,
    })
}

/// Collects a glyph's path commands into flattened contours, in font units.
struct Flattener {
    tolerance: f64,
    contours: Vec<Vec<(f64, f64)>>,
    current: Vec<(f64, f64)>,
}

impl Flattener {
    fn new(tolerance: f64) -> Self {
        Self {
            tolerance,
            contours: Vec::new(),
            current: Vec::new(),
        }
    }

    fn last(&self) -> (f64, f64) {
        self.current.last().copied().unwrap_or((0.0, 0.0))
    }

    fn flush(&mut self) {
        if self.current.len() >= 2 {
            let contour = std::mem::take(&mut self.current);
            self.contours.push(contour);
        } else {
            self.current.clear();
        }
    }

    fn finish(mut self) -> Vec<Vec<(f64, f64)>> {
        self.flush();
        self.contours
    }

    /// Segments needed so a Bézier of the given control polygon stays within
    /// the tolerance of its chords. The bound `n ≥ sqrt(L / (8·tol))` with
    /// `L` the largest second difference of the control points is the
    /// standard flatness estimate for quadratics; cubics use the same bound
    /// with the larger of their two second differences.
    fn segment_count(&self, second_difference: f64) -> usize {
        let count = (second_difference / (8.0 * self.tolerance)).sqrt().ceil();
        if count.is_finite() {
            (count as usize).clamp(1, 64)
        } else {
            1
        }
    }
}

impl OutlinePen for Flattener {
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush();
        self.current.push((f64::from(x), f64::from(y)));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.current.push((f64::from(x), f64::from(y)));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        let (x0, y0) = self.last();
        let (cx, cy, x1, y1) = (f64::from(cx0), f64::from(cy0), f64::from(x), f64::from(y));
        let second = ((x0 - 2.0 * cx + x1).powi(2) + (y0 - 2.0 * cy + y1).powi(2)).sqrt();
        let segments = self.segment_count(second);
        for step in 1..=segments {
            let t = step as f64 / segments as f64;
            let s = 1.0 - t;
            let px = s * s * x0 + 2.0 * s * t * cx + t * t * x1;
            let py = s * s * y0 + 2.0 * s * t * cy + t * t * y1;
            self.current.push((px, py));
        }
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let (x0, y0) = self.last();
        let (ax, ay, bx, by, x1, y1) = (
            f64::from(cx0),
            f64::from(cy0),
            f64::from(cx1),
            f64::from(cy1),
            f64::from(x),
            f64::from(y),
        );
        let first = ((x0 - 2.0 * ax + bx).powi(2) + (y0 - 2.0 * ay + by).powi(2)).sqrt();
        let last = ((ax - 2.0 * bx + x1).powi(2) + (ay - 2.0 * by + y1).powi(2)).sqrt();
        let segments = self.segment_count(first.max(last) * 0.75);
        for step in 1..=segments {
            let t = step as f64 / segments as f64;
            let s = 1.0 - t;
            let px = s * s * s * x0 + 3.0 * s * s * t * ax + 3.0 * s * t * t * bx + t * t * t * x1;
            let py = s * s * s * y0 + 3.0 * s * s * t * ay + 3.0 * s * t * t * by + t * t * t * y1;
            self.current.push((px, py));
        }
    }

    fn close(&mut self) {
        self.flush();
    }
}

/// Drops consecutive points closer than `tolerance`, including a final point
/// that repeats the first, so every remaining segment has a real length.
fn dedup_closed(points: Vec<SketchPoint2>, tolerance: f64) -> Vec<SketchPoint2> {
    let mut kept: Vec<SketchPoint2> = Vec::with_capacity(points.len());
    for point in points {
        if kept
            .last()
            .is_some_and(|last| distance(*last, point) <= tolerance)
        {
            continue;
        }
        kept.push(point);
    }
    while kept.len() > 1 && distance(kept[0], kept[kept.len() - 1]) <= tolerance {
        kept.pop();
    }
    kept
}

fn distance(first: SketchPoint2, second: SketchPoint2) -> f64 {
    (first.u - second.u).hypot(first.v - second.v)
}

/// Signed area of a closed polygon by the shoelace formula.
pub fn polygon_area(points: &[SketchPoint2]) -> f64 {
    let mut twice = 0.0;
    for index in 0..points.len() {
        let first = points[index];
        let second = points[(index + 1) % points.len()];
        twice += first.u * second.v - second.u * first.v;
    }
    twice * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(outlines: &TextOutlines) -> (f64, f64, f64, f64) {
        let mut min_u = f64::INFINITY;
        let mut max_u = f64::NEG_INFINITY;
        let mut min_v = f64::INFINITY;
        let mut max_v = f64::NEG_INFINITY;
        for outline in &outlines.loops {
            for point in &outline.points {
                min_u = min_u.min(point.u);
                max_u = max_u.max(point.u);
                min_v = min_v.min(point.v);
                max_v = max_v.max(point.v);
            }
        }
        (min_u, max_u, min_v, max_v)
    }

    #[test]
    fn a_capital_letter_is_exactly_the_requested_height_on_the_baseline() {
        let outlines = text_outlines("H", 10.0).expect("H has an outline");
        assert_eq!(outlines.loops.len(), 1, "H is one contour");
        let (_, _, min_v, max_v) = bounds(&outlines);
        assert!(min_v.abs() < 1.0e-9, "the baseline is v = 0, got {min_v}");
        assert!((max_v - 10.0).abs() < 1.0e-6, "cap height {max_v}");
        assert!(outlines.advance > 0.0);
    }

    #[test]
    fn a_counter_becomes_its_own_loop_and_whitespace_only_advances() {
        let outlines = text_outlines("O", 10.0).expect("O has outlines");
        assert_eq!(outlines.loops.len(), 2, "outer contour and its counter");
        let areas = outlines
            .loops
            .iter()
            .map(|outline| polygon_area(&outline.points))
            .collect::<Vec<_>>();
        // A TrueType outer contour and its counter wind opposite ways.
        assert!(areas[0] * areas[1] < 0.0, "{areas:?}");

        let spaced = text_outlines("O O", 10.0).expect("spaced text");
        assert_eq!(spaced.loops.len(), 4);
        let solo = text_outlines("O", 10.0).unwrap();
        assert!(spaced.advance > 2.0 * solo.advance);
    }

    #[test]
    fn every_segment_has_a_length_and_the_chords_stay_close_to_the_curve() {
        let outlines = text_outlines("Sg8", 20.0).expect("text");
        for outline in &outlines.loops {
            assert!(outline.points.len() >= 3);
            for index in 0..outline.points.len() {
                let first = outline.points[index];
                let second = outline.points[(index + 1) % outline.points.len()];
                assert!(distance(first, second) > 20.0 * TEXT_CHORD_TOLERANCE_RATIO * 0.5);
            }
        }
        // A letter at 20 mm is dense enough to read as a curve but not
        // hundreds of vertices: the tolerance is doing its job.
        let vertices = outlines
            .loops
            .iter()
            .map(|outline| outline.points.len())
            .sum::<usize>();
        assert!((60..1500).contains(&vertices), "{vertices} vertices");
    }

    #[test]
    fn layout_is_deterministic_and_refuses_what_it_cannot_set() {
        let first = text_outlines("Artificer 9", 8.0).unwrap();
        let second = text_outlines("Artificer 9", 8.0).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            text_outlines("   ", 8.0),
            Err(TextOutlineError::EmptyContent)
        );
        assert_eq!(
            text_outlines("A", 0.0),
            Err(TextOutlineError::InvalidHeight)
        );
        assert_eq!(
            text_outlines("\u{1F600}", 8.0),
            Err(TextOutlineError::UnsupportedCharacter('\u{1F600}'))
        );
    }
}
