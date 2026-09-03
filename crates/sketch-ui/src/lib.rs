//! Deterministic, kernel-free interaction and rendering for a planar sketch.
//!
//! The canvas deliberately distinguishes committed sketch entities from a
//! pending edit. Creation tools stage one entity after their final click;
//! the owning workbench decides when to call [`SketchCanvasState::commit_pending`]
//! or [`SketchCanvasState::cancel_pending`]. Camera navigation and selection
//! remain immediate because neither changes model truth.

pub mod sketch_toolbar;

use egui::{
    Align2, Color32, CursorIcon, FontId, Id, PointerButton, Pos2, Rect, Response, Sense, Stroke,
    Ui, Vec2, WidgetInfo, WidgetType,
};
use std::collections::{BTreeMap, BTreeSet};

use artificer_geometry::{
    Orientation2, Point2, ProfileClassification, ProfileWinding, classify_profile, orient2d,
};
use artificer_protocol::{
    MAX_EXTRUSION_PROFILE_VERTICES, MAX_PLANAR_PROFILE_CURVES, MAX_PLANAR_PROFILE_LOOPS,
    MAX_PLANAR_PROFILE_REGIONS, PlanarProfile2, PrecisionPolicy,
};
use artificer_sketch::{
    Angle as CoreAngle, ArrangementCell as CoreArrangementCell,
    ArrangementLimits as CoreArrangementLimits, ArrangementLoop as CoreArrangementLoop,
    CircularPatternDistribution as CoreCircularPatternDistribution,
    ConfirmationSource as CoreConfirmationSource, CurveDirection as CoreCurveDirection,
    CurveIntersections as CoreCurveIntersections, EvaluatedCurve2 as CoreEvaluatedCurve2,
    FilletBranchHints as CoreFilletBranchHints, Integer as CoreInteger, Length as CoreLength,
    MAX_CURVE_EDITS_PER_TRANSACTION, MAX_POLYGON_SIDES as CORE_MAX_POLYGON_SIDES,
    MIN_POLYGON_SIDES as CORE_MIN_POLYGON_SIDES, PointInput as CorePointInput,
    ProfileCompileError as CoreProfileCompileError, RegionSignature as CoreRegionSignature,
    RetirementPolicy as CoreRetirementPolicy, SignedLength as CoreSignedLength,
    SketchArrangement as CoreSketchArrangement, SketchChain as CoreSketchChain,
    SketchConstraintId as CoreConstraintId, SketchConstraintKind as CoreConstraintKind,
    SketchCurve2 as CoreCurve2, SketchDefinition as CoreSketchDefinition,
    SketchEntityId as CoreEntityId, SketchEntityRole as CoreEntityRole,
    SketchOperationId as CoreOperationId, SketchOutputRef as CoreOutputRef,
    SketchPoint2 as CorePoint2, SketchPointId as CorePointId, SketchRecipe as CoreRecipe,
    SketchRevision as CoreSketchRevision, SketchSnapKey as CoreSnapKey,
    SketchTransaction as CoreTransaction, SketchUndoJournal as CoreUndoJournal,
    SketchValue as CoreValue, TrimCurve as CoreTrimCurve, build_arrangement, chain_geometry,
    compile_selected_profile, connected_chain, hit_test_curves, intersect_curves,
    query_snap_candidates, select_trim_span,
};
use artificer_ui_core::drag_handle::{DragHandlePhase, DragHandleState, PointerSample};

use crate::sketch_toolbar::ToolVariant;

/// Lying on a support edge captures inside a tighter band than its named
/// points, keeping the pull "light" while tracing an outline.
const SUPPORT_EDGE_RADIUS_RATIO: f32 = 0.6;

/// The sketch canvas paints from the workbench palette, so it follows the
/// chrome's theme and the same colour editor. Each role is read at paint
/// time; see `artificer_ui_core::theme::SketchColours` for what each is.
fn sketch_colours() -> artificer_ui_core::theme::SketchColours {
    artificer_ui_core::theme::sketch()
}

fn translucent(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

const MIN_POINTS_PER_UNIT: f64 = 4.0;
const MAX_POINTS_PER_UNIT: f64 = 4_000.0;
const DEFAULT_POINTS_PER_UNIT: f64 = 56.0;
const MIN_ENTITY_LENGTH: f64 = 1.0e-9;
const MAX_GRID_LINES_PER_AXIS: usize = 512;
const TARGET_GRID_SPACING_POINTS: f64 = 28.0;
/// Snapping coarsens on a different threshold from the drawn grid. A grid line
/// has to be *readable* at 28 points apart; a snap target only has to be
/// *reachable*, which the pointer manages at a few points. Reusing the drawing
/// threshold would coarsen snapping at ordinary zoom, quietly moving points the
/// user placed deliberately.
const TARGET_SNAP_SPACING_POINTS: f64 = 6.0;
const MAJOR_GRID_INTERVAL: u64 = 5;
const MAX_ABS_SKETCH_COORDINATE: f64 = 1.0e9;
const CONTEXT_FIT_PADDING_POINTS: f32 = 42.0;
const MIN_ARC_SWEEP_DEGREES: f64 = 1.0e-6;
const MAX_ARC_SWEEP_DEGREES: f64 = 360.0 - MIN_ARC_SWEEP_DEGREES;
const DIMENSION_WIDGET_SIZE: Vec2 = Vec2::new(96.0, 20.0);
const DEFAULT_POLYGON_SIDES: u16 = 6;
const DEFAULT_RECTANGULAR_PATTERN_COLUMNS: u16 = 3;
const DEFAULT_RECTANGULAR_PATTERN_ROWS: u16 = 2;
const DEFAULT_CIRCULAR_PATTERN_COUNT: u16 = 4;
const DEFAULT_TOOL_LENGTH: f64 = 5.0;
const DEFAULT_FILLET_RADIUS: f64 = 1.0;
const DEFAULT_OFFSET_DISTANCE: f64 = 1.0;
const DEFAULT_CHAMFER_DISTANCE: f64 = 1.0;
const DEFAULT_POLYGON_DIAMETER: f64 = 10.0;
const DEFAULT_SLOT_LENGTH: f64 = 10.0;
const DEFAULT_SLOT_WIDTH: f64 = 2.0;
const PATTERN_MANIPULATOR_HIT_RADIUS_POINTS: f32 = 11.0;

/// Maximum relative radius disagreement accepted for an authored circular arc.
///
/// The arc tool projects its third click onto the start radius before staging
/// geometry, so interactive arcs normally have identical radii. Programmatic
/// callers receive the same explicit, scale-relative validation instead of an
/// arbitrary endpoint being silently moved onto the circle.
pub const ARC_RADIUS_RELATIVE_TOLERANCE: f64 = 1.0e-9;

/// One of the three origin-aligned planes available to the initial sketch lab.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SketchPlane {
    #[default]
    XY,
    YZ,
    XZ,
}

impl SketchPlane {
    pub const ALL: [Self; 3] = [Self::XY, Self::YZ, Self::XZ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::XY => "XY",
            Self::YZ => "YZ",
            Self::XZ => "XZ",
        }
    }

    /// Horizontal and vertical world-axis labels in canvas order.
    #[must_use]
    pub const fn axis_labels(self) -> [&'static str; 2] {
        match self {
            Self::XY => ["X", "Y"],
            Self::YZ => ["Y", "Z"],
            Self::XZ => ["X", "Z"],
        }
    }

    /// Maps a planar coordinate into the corresponding origin-aligned world plane.
    #[must_use]
    pub const fn to_world(self, point: SketchPoint) -> [f64; 3] {
        match self {
            Self::XY => [point.u, point.v, 0.0],
            Self::YZ => [0.0, point.u, point.v],
            Self::XZ => [point.u, 0.0, point.v],
        }
    }

    /// Projects a world coordinate onto this plane. The normal component is ignored.
    #[must_use]
    pub const fn from_world(self, point: [f64; 3]) -> SketchPoint {
        match self {
            Self::XY => SketchPoint::new(point[0], point[1]),
            Self::YZ => SketchPoint::new(point[1], point[2]),
            Self::XZ => SketchPoint::new(point[0], point[2]),
        }
    }

    #[must_use]
    pub const fn normal(self) -> [f64; 3] {
        match self {
            Self::XY => [0.0, 0.0, 1.0],
            Self::YZ => [1.0, 0.0, 0.0],
            Self::XZ => [0.0, -1.0, 0.0],
        }
    }
}

/// Immediate interaction mode within the sketch canvas.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SketchTool {
    #[default]
    Select,
    Point,
    Line,
    CentreLine,
    Rectangle,
    Circle,
    Arc,
}

impl SketchTool {
    pub const ALL: [Self; 7] = [
        Self::Select,
        Self::Point,
        Self::Line,
        Self::CentreLine,
        Self::Rectangle,
        Self::Circle,
        Self::Arc,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Select => "Select",
            Self::Point => "Point",
            Self::Line => "Line",
            Self::CentreLine => "Centre line",
            Self::Rectangle => "Rectangle",
            Self::Circle => "Circle",
            Self::Arc => "Arc",
        }
    }

    const fn cursor(self) -> CursorIcon {
        match self {
            Self::Select => CursorIcon::PointingHand,
            Self::Point
            | Self::Line
            | Self::CentreLine
            | Self::Rectangle
            | Self::Circle
            | Self::Arc => CursorIcon::Crosshair,
        }
    }
}

/// A point in the selected plane's `(u, v)` coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SketchPoint {
    pub u: f64,
    pub v: f64,
}

impl SketchPoint {
    #[must_use]
    pub const fn new(u: f64, v: f64) -> Self {
        Self { u, v }
    }

    #[must_use]
    pub const fn is_finite(self) -> bool {
        self.u.is_finite() && self.v.is_finite()
    }

    #[must_use]
    pub fn distance_squared(self, other: Self) -> f64 {
        let du = self.u - other.u;
        let dv = self.v - other.v;
        du.mul_add(du, dv * dv)
    }
}

/// One body triangle already projected into the active sketch's `(u, v)` frame.
///
/// Projected tessellation is presentation-only. It is never considered by
/// sketch snapping, selection, dimensions, profile certification, or kernel
/// input. Snapping reads [`SketchViewportContext::snap_curves`] instead, which
/// carries analytic support geometry rather than chords.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SketchContextTriangle {
    pub vertices: [SketchPoint; 3],
    /// Brightness of the face relative to the palette's context colour, in
    /// `0.0..=1.0`. Faces parallel to the sketch plane are brightest; a face
    /// that slopes away from it, or lies deep below it, is darker.
    pub shade: f32,
    pub layer: SketchContextLayer,
}

impl SketchContextTriangle {
    #[must_use]
    pub const fn new(vertices: [SketchPoint; 3]) -> Self {
        Self {
            vertices,
            shade: 1.0,
            layer: SketchContextLayer::Body,
        }
    }

    #[must_use]
    pub const fn with_shade(mut self, shade: f32) -> Self {
        self.shade = shade;
        self
    }

    #[must_use]
    pub const fn with_layer(mut self, layer: SketchContextLayer) -> Self {
        self.layer = layer;
        self
    }
}

/// Where a projected body element sits relative to the sketch surface.
///
/// The body itself is always drawn when sketching on a face. Geometry below
/// the surface is hidden by the face in a true view and only appears when
/// the user asks to project it, so it is painted as an x-ray in its own
/// colour and never mistaken for the visible body.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SketchContextLayer {
    /// On or above the sketch surface: the host face and anything raised
    /// from it.
    #[default]
    Body,
    /// Below the sketch surface: pockets, bores, and walls under the face.
    Below,
}

/// One body edge already projected into the active sketch's `(u, v)` frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SketchContextEdge {
    pub endpoints: [SketchPoint; 2],
    pub layer: SketchContextLayer,
}

impl SketchContextEdge {
    #[must_use]
    pub const fn new(endpoints: [SketchPoint; 2]) -> Self {
        Self {
            endpoints,
            layer: SketchContextLayer::Body,
        }
    }

    #[must_use]
    pub const fn with_layer(mut self, layer: SketchContextLayer) -> Self {
        self.layer = layer;
        self
    }
}

/// One analytic support curve available to snapping, in the sketch `(u, v)`
/// frame.
///
/// These are the sketch support's own exact boundary curves, not tessellation.
/// Snapping to them yields the same coordinates the kernel would evaluate, so a
/// hole's centre is the circle's centre rather than a chord average. Snapped
/// results are still authored as literal coordinates: this reference geometry
/// creates no constraint and never enters a profile.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SketchContextCurve {
    Segment {
        start: SketchPoint,
        end: SketchPoint,
    },
    /// `center + radius * (u cos t + v sin t)` over the `start..end` span,
    /// with `u` and `v` an orthonormal pair in the sketch frame.
    Arc {
        center: SketchPoint,
        u: [f64; 2],
        v: [f64; 2],
        radius: f64,
        start: f64,
        end: f64,
    },
}

/// Parameter slack below which an arc span is treated as a closed circle.
const FULL_TURN_EPSILON: f64 = 1.0e-9;

impl SketchContextCurve {
    #[must_use]
    pub const fn segment(start: SketchPoint, end: SketchPoint) -> Self {
        Self::Segment { start, end }
    }

    #[must_use]
    pub fn evaluate(self, parameter: f64) -> SketchPoint {
        match self {
            Self::Segment { start, end } => SketchPoint::new(
                (end.u - start.u).mul_add(parameter, start.u),
                (end.v - start.v).mul_add(parameter, start.v),
            ),
            Self::Arc {
                center,
                u,
                v,
                radius,
                ..
            } => {
                let (sine, cosine) = parameter.sin_cos();
                SketchPoint::new(
                    radius.mul_add(v[0].mul_add(sine, u[0] * cosine), center.u),
                    radius.mul_add(v[1].mul_add(sine, u[1] * cosine), center.v),
                )
            }
        }
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        match self {
            Self::Segment { start, end } => start.is_finite() && end.is_finite(),
            Self::Arc {
                center,
                u,
                v,
                radius,
                start,
                end,
            } => {
                center.is_finite()
                    && u.into_iter().all(f64::is_finite)
                    && v.into_iter().all(f64::is_finite)
                    && radius.is_finite()
                    && radius > 0.0
                    && start.is_finite()
                    && end.is_finite()
            }
        }
    }

    /// A closed loop has no distinguished endpoints or midpoint.
    #[must_use]
    pub fn is_closed(self) -> bool {
        match self {
            Self::Segment { .. } => false,
            Self::Arc { start, end, .. } => {
                (end - start).abs() >= std::f64::consts::TAU - FULL_TURN_EPSILON
            }
        }
    }

    #[must_use]
    pub fn endpoints(self) -> Option<[SketchPoint; 2]> {
        if self.is_closed() {
            return None;
        }
        Some(match self {
            Self::Segment { start, end } => [start, end],
            Self::Arc { start, end, .. } => [self.evaluate(start), self.evaluate(end)],
        })
    }

    /// The point that halves the curve's arc length.
    #[must_use]
    pub fn midpoint(self) -> Option<SketchPoint> {
        if self.is_closed() {
            return None;
        }
        Some(match self {
            Self::Segment { .. } => self.evaluate(0.5),
            Self::Arc { start, end, .. } => self.evaluate(f64::midpoint(start, end)),
        })
    }

    #[must_use]
    pub const fn center(self) -> Option<SketchPoint> {
        match self {
            Self::Segment { .. } => None,
            Self::Arc { center, .. } => Some(center),
        }
    }

    /// Points where the curve crosses the sketch axes through its centre.
    ///
    /// Only quadrants inside the arc's own span are returned, so a half-round
    /// slot end offers the two quadrants it actually reaches.
    #[must_use]
    pub fn quadrants(self) -> Vec<SketchPoint> {
        let Self::Arc {
            center,
            u,
            v,
            radius,
            start,
            end,
        } = self
        else {
            return Vec::new();
        };
        [[1.0, 0.0], [0.0, 1.0], [-1.0, 0.0], [0.0, -1.0]]
            .into_iter()
            .filter_map(|direction: [f64; 2]| {
                // The direction's parameter satisfies `cos t = d·u`, `sin t = d·v`.
                let parameter = direction[0]
                    .mul_add(v[0], direction[1] * v[1])
                    .atan2(direction[0].mul_add(u[0], direction[1] * u[1]));
                span_contains(start, end, parameter).then(|| {
                    SketchPoint::new(
                        radius.mul_add(direction[0], center.u),
                        radius.mul_add(direction[1], center.v),
                    )
                })
            })
            .collect()
    }

    /// The nearest point on the curve itself, clamped to its own span.
    #[must_use]
    pub fn closest_point(self, target: SketchPoint) -> Option<SketchPoint> {
        match self {
            Self::Segment { start, end } => {
                let (du, dv) = (end.u - start.u, end.v - start.v);
                let length_squared = du.mul_add(du, dv * dv);
                if length_squared <= 0.0 || !length_squared.is_finite() {
                    return None;
                }
                let parameter = ((target.u - start.u).mul_add(du, (target.v - start.v) * dv)
                    / length_squared)
                    .clamp(0.0, 1.0);
                Some(self.evaluate(parameter))
            }
            Self::Arc {
                center,
                u,
                v,
                start,
                end,
                ..
            } => {
                let (du, dv) = (target.u - center.u, target.v - center.v);
                if du.mul_add(du, dv * dv) <= 0.0 {
                    // Directly on the centre there is no nearest rim point.
                    return None;
                }
                let parameter = du
                    .mul_add(v[0], dv * v[1])
                    .atan2(du.mul_add(u[0], dv * u[1]));
                if span_contains(start, end, parameter) {
                    return Some(self.evaluate(parameter));
                }
                // Outside the span the nearest rim point is the closer end.
                let endpoints = self.endpoints()?;
                Some(
                    if target.distance_squared(endpoints[0])
                        <= target.distance_squared(endpoints[1])
                    {
                        endpoints[0]
                    } else {
                        endpoints[1]
                    },
                )
            }
        }
    }
}

/// Whether `parameter` lies inside the directed span from `start` to `end`,
/// comparing angles modulo a full turn so orientation and winding both work.
fn span_contains(start: f64, end: f64, parameter: f64) -> bool {
    let sweep = end - start;
    if sweep.abs() >= std::f64::consts::TAU - FULL_TURN_EPSILON {
        return true;
    }
    let (low, high) = if sweep >= 0.0 {
        (start, end)
    } else {
        (end, start)
    };
    let offset = (parameter - low).rem_euclid(std::f64::consts::TAU);
    offset <= high - low + FULL_TURN_EPSILON
}

/// Optional read-only body projection rendered behind a sketch.
///
/// `auto_fit_key` is owned by the caller. A new key fits the complete body
/// projection once while centering the selected face; subsequent frames
/// preserve user pan and zoom. Leave the key as `None` to draw context without
/// changing the current sketch camera.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SketchViewportContext<'a> {
    /// Front-facing projected triangles in painter order (far to near).
    pub triangles: &'a [SketchContextTriangle],
    /// Visible projected B-rep edges; hidden-edge filtering belongs upstream.
    pub edges: &'a [SketchContextEdge],
    pub selected_face_boundary: &'a [SketchPoint],
    pub selected_face_inner_boundaries: &'a [Vec<SketchPoint>],
    /// Analytic support geometry offered to snapping, outer loop first.
    pub snap_curves: &'a [SketchContextCurve],
    pub auto_fit_key: Option<SketchContextFitKey>,
    /// Labels for the authoritative face-frame U and V directions.
    pub axis_labels: Option<[&'static str; 2]>,
}

/// Stable identity for a projected sketch context and its selected support.
///
/// Keeping the full caller-provided digest prevents a reused face ordinal in a
/// later immutable snapshot from suppressing the new context's first fit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SketchContextFitKey {
    pub context_digest: [u8; 32],
    pub selected_entity: u64,
}

impl SketchContextFitKey {
    #[must_use]
    pub const fn new(context_digest: [u8; 32], selected_entity: u64) -> Self {
        Self {
            context_digest,
            selected_entity,
        }
    }
}

impl<'a> SketchViewportContext<'a> {
    #[must_use]
    pub const fn new(
        triangles: &'a [SketchContextTriangle],
        edges: &'a [SketchContextEdge],
    ) -> Self {
        Self {
            triangles,
            edges,
            selected_face_boundary: &[],
            selected_face_inner_boundaries: &[],
            snap_curves: &[],
            auto_fit_key: None,
            axis_labels: None,
        }
    }

    /// Offers the support's analytic curves to snapping.
    #[must_use]
    pub const fn with_snap_curves(mut self, snap_curves: &'a [SketchContextCurve]) -> Self {
        self.snap_curves = snap_curves;
        self
    }

    #[must_use]
    pub const fn with_selected_face(
        mut self,
        boundary: &'a [SketchPoint],
        auto_fit_key: SketchContextFitKey,
    ) -> Self {
        self.selected_face_boundary = boundary;
        self.auto_fit_key = Some(auto_fit_key);
        self
    }

    /// Adds exact face-owned void boundaries to the highlighted support.
    #[must_use]
    pub const fn with_selected_face_inner_boundaries(
        mut self,
        inner_boundaries: &'a [Vec<SketchPoint>],
    ) -> Self {
        self.selected_face_inner_boundaries = inner_boundaries;
        self
    }

    #[must_use]
    pub const fn with_axis_labels(mut self, axis_labels: [&'static str; 2]) -> Self {
        self.axis_labels = Some(axis_labels);
        self
    }
}

/// Stable, document-local identity for a sketch entity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SketchEntityId(u64);

impl SketchEntityId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Geometry represented by the first visual sketch slice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SketchGeometry {
    Point(SketchPoint),
    Segment {
        start: SketchPoint,
        end: SketchPoint,
    },
    /// Axis-aligned in sketch-plane coordinates.
    Rectangle {
        first: SketchPoint,
        opposite: SketchPoint,
    },
    Circle {
        center: SketchPoint,
        rim: SketchPoint,
    },
    /// Counter-clockwise arc from `start` toward the direction of `end`.
    Arc {
        center: SketchPoint,
        start: SketchPoint,
        end: SketchPoint,
    },
}

const MAX_DISPLAY_CURVE_SEGMENTS: usize = 64;

/// Deterministic, renderer-neutral outline for displaying a sketch entity.
///
/// Points remain in the sketch's authoritative `(u, v)` frame. Consumers can
/// embed them on an origin plane or face support without depending on egui or
/// duplicating analytic-curve sampling rules. Closed outlines omit a repeated
/// final point and expose closure separately.
#[derive(Clone, Debug, PartialEq)]
pub struct SketchDisplayPolyline {
    pub points: Vec<SketchPoint>,
    pub closed: bool,
}

impl SketchDisplayPolyline {
    /// Ordered line segments, including the final closing edge when needed.
    pub fn segments(&self) -> impl Iterator<Item = [SketchPoint; 2]> + '_ {
        self.points
            .windows(2)
            .map(|window| [window[0], window[1]])
            .chain(
                (self.closed && self.points.len() >= 2)
                    .then(|| [self.points[self.points.len() - 1], self.points[0]]),
            )
    }
}

impl SketchGeometry {
    #[must_use]
    pub const fn point(position: SketchPoint) -> Self {
        Self::Point(position)
    }

    #[must_use]
    pub const fn segment(start: SketchPoint, end: SketchPoint) -> Self {
        Self::Segment { start, end }
    }

    #[must_use]
    pub const fn rectangle(first: SketchPoint, opposite: SketchPoint) -> Self {
        Self::Rectangle { first, opposite }
    }

    #[must_use]
    pub const fn circle(center: SketchPoint, rim: SketchPoint) -> Self {
        Self::Circle { center, rim }
    }

    #[must_use]
    pub const fn arc(center: SketchPoint, start: SketchPoint, end: SketchPoint) -> Self {
        Self::Arc { center, start, end }
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        match self {
            Self::Point(point) => point.is_finite(),
            Self::Segment { start, end }
            | Self::Rectangle {
                first: start,
                opposite: end,
            } => {
                start.is_finite()
                    && end.is_finite()
                    && (end.u - start.u).is_finite()
                    && (end.v - start.v).is_finite()
            }
            Self::Circle { center, rim } => {
                center.is_finite() && rim.is_finite() && center.distance_squared(rim).is_finite()
            }
            Self::Arc { center, start, end } => {
                center.is_finite()
                    && start.is_finite()
                    && end.is_finite()
                    && center.distance_squared(start).is_finite()
                    && center.distance_squared(end).is_finite()
                    && arc_sweep(center, start, end).is_finite()
            }
        }
    }

    #[must_use]
    pub fn is_degenerate(self) -> bool {
        match self {
            Self::Point(_) => false,
            Self::Segment { start, end } => {
                start.distance_squared(end) <= MIN_ENTITY_LENGTH.powi(2)
            }
            Self::Rectangle { first, opposite } => {
                (first.u - opposite.u).abs() <= MIN_ENTITY_LENGTH
                    || (first.v - opposite.v).abs() <= MIN_ENTITY_LENGTH
            }
            Self::Circle { center, rim } => {
                center.distance_squared(rim) <= MIN_ENTITY_LENGTH.powi(2)
            }
            Self::Arc { center, start, end } => {
                center.distance_squared(start) <= MIN_ENTITY_LENGTH.powi(2)
                    || center.distance_squared(end) <= MIN_ENTITY_LENGTH.powi(2)
                    || arc_sweep(center, start, end) <= 1.0e-9
            }
        }
    }

    /// Canonical rectangle corners in counter-clockwise order, if applicable.
    #[must_use]
    pub fn rectangle_corners(self) -> Option<[SketchPoint; 4]> {
        let Self::Rectangle { first, opposite } = self else {
            return None;
        };
        let min_u = first.u.min(opposite.u);
        let max_u = first.u.max(opposite.u);
        let min_v = first.v.min(opposite.v);
        let max_v = first.v.max(opposite.v);
        Some([
            SketchPoint::new(min_u, min_v),
            SketchPoint::new(max_u, min_v),
            SketchPoint::new(max_u, max_v),
            SketchPoint::new(min_u, max_v),
        ])
    }

    /// Produces a bounded display outline for model-space sketch overlays.
    ///
    /// Invalid or degenerate analytic geometry is rejected instead of emitting
    /// NaNs into a viewport mesh. Curves use at most 64 segments, matching the
    /// interactive sketch renderer's current visual fidelity.
    #[must_use]
    pub fn display_polyline(self) -> Option<SketchDisplayPolyline> {
        if !self.is_finite() || self.is_degenerate() {
            return None;
        }
        let (points, closed) = match self {
            Self::Point(point) => (vec![point], false),
            Self::Segment { start, end } => (vec![start, end], false),
            Self::Rectangle { .. } => (
                self.rectangle_corners()
                    .expect("the rectangle branch always supplies corners")
                    .to_vec(),
                true,
            ),
            Self::Circle { center, rim } => {
                let radius = center.distance_squared(rim).sqrt();
                let start_angle = (rim.v - center.v).atan2(rim.u - center.u);
                let points = (0..MAX_DISPLAY_CURVE_SEGMENTS)
                    .map(|index| {
                        let angle = std::f64::consts::TAU.mul_add(
                            index as f64 / MAX_DISPLAY_CURVE_SEGMENTS as f64,
                            start_angle,
                        );
                        SketchPoint::new(
                            radius.mul_add(angle.cos(), center.u),
                            radius.mul_add(angle.sin(), center.v),
                        )
                    })
                    .collect();
                (points, true)
            }
            Self::Arc { center, start, end } => {
                let radius = center.distance_squared(start).sqrt();
                let start_angle = (start.v - center.v).atan2(start.u - center.u);
                let sweep = arc_sweep(center, start, end);
                let segment_count =
                    ((sweep / std::f64::consts::TAU) * MAX_DISPLAY_CURVE_SEGMENTS as f64)
                        .ceil()
                        .clamp(2.0, MAX_DISPLAY_CURVE_SEGMENTS as f64) as usize;
                let points = (0..=segment_count)
                    .map(|index| {
                        let angle = sweep.mul_add(index as f64 / segment_count as f64, start_angle);
                        SketchPoint::new(
                            radius.mul_add(angle.cos(), center.u),
                            radius.mul_add(angle.sin(), center.v),
                        )
                    })
                    .collect();
                (points, false)
            }
        };
        Some(SketchDisplayPolyline { points, closed })
    }

    /// The center or midpoint of the sketch geometry, if well-defined.
    #[must_use]
    pub fn center(self) -> Option<SketchPoint> {
        match self {
            Self::Point(point) => Some(point),
            Self::Segment { start, end } => Some(SketchPoint::new(
                (start.u + end.u) * 0.5,
                (start.v + end.v) * 0.5,
            )),
            Self::Rectangle { first, opposite } => Some(SketchPoint::new(
                (first.u + opposite.u) * 0.5,
                (first.v + opposite.v) * 0.5,
            )),
            Self::Circle { center, .. } => Some(center),
            Self::Arc { center, .. } => Some(center),
        }
    }

    /// Translates the sketch geometry by the given delta in sketch plane coordinates.
    #[must_use]
    pub fn translate(self, delta_u: f64, delta_v: f64) -> Self {
        match self {
            Self::Point(p) => Self::Point(SketchPoint::new(p.u + delta_u, p.v + delta_v)),
            Self::Segment { start, end } => Self::Segment {
                start: SketchPoint::new(start.u + delta_u, start.v + delta_v),
                end: SketchPoint::new(end.u + delta_u, end.v + delta_v),
            },
            Self::Rectangle { first, opposite } => Self::Rectangle {
                first: SketchPoint::new(first.u + delta_u, first.v + delta_v),
                opposite: SketchPoint::new(opposite.u + delta_u, opposite.v + delta_v),
            },
            Self::Circle { center, rim } => Self::Circle {
                center: SketchPoint::new(center.u + delta_u, center.v + delta_v),
                rim: SketchPoint::new(rim.u + delta_u, rim.v + delta_v),
            },
            Self::Arc { center, start, end } => Self::Arc {
                center: SketchPoint::new(center.u + delta_u, center.v + delta_v),
                start: SketchPoint::new(start.u + delta_u, start.v + delta_v),
                end: SketchPoint::new(end.u + delta_u, end.v + delta_v),
            },
        }
    }

    /// Reshapes the sketch geometry using the specified drag handle and coordinate delta.
    #[must_use]
    pub fn reshape(self, handle: SketchDragHandle, delta_u: f64, delta_v: f64) -> Self {
        match (self, handle) {
            (_, SketchDragHandle::Translate) => self.translate(delta_u, delta_v),
            (Self::Point(p), _) => Self::Point(SketchPoint::new(p.u + delta_u, p.v + delta_v)),
            (Self::Segment { start, end }, SketchDragHandle::StartPoint) => Self::Segment {
                start: SketchPoint::new(start.u + delta_u, start.v + delta_v),
                end,
            },
            (Self::Segment { start, end }, SketchDragHandle::EndPoint) => Self::Segment {
                start,
                end: SketchPoint::new(end.u + delta_u, end.v + delta_v),
            },
            (Self::Segment { .. }, _) => self.translate(delta_u, delta_v),
            (Self::Rectangle { .. }, SketchDragHandle::RectangleCorner(corner_idx)) => {
                let Some(corners) = self.rectangle_corners() else {
                    return self;
                };
                let opp_idx = (corner_idx + 2) % 4;
                let fixed_corner = corners[opp_idx];
                let moving_corner = corners[corner_idx];
                let new_corner =
                    SketchPoint::new(moving_corner.u + delta_u, moving_corner.v + delta_v);
                Self::Rectangle {
                    first: fixed_corner,
                    opposite: new_corner,
                }
            }
            (Self::Rectangle { first, opposite }, SketchDragHandle::RectangleSide(side_idx)) => {
                let min_u = first.u.min(opposite.u);
                let max_u = first.u.max(opposite.u);
                let min_v = first.v.min(opposite.v);
                let max_v = first.v.max(opposite.v);
                match side_idx {
                    0 => {
                        let new_min_v = min_v + delta_v;
                        Self::Rectangle {
                            first: SketchPoint::new(min_u, max_v),
                            opposite: SketchPoint::new(max_u, new_min_v),
                        }
                    }
                    1 => {
                        let new_max_u = max_u + delta_u;
                        Self::Rectangle {
                            first: SketchPoint::new(min_u, min_v),
                            opposite: SketchPoint::new(new_max_u, max_v),
                        }
                    }
                    2 => {
                        let new_max_v = max_v + delta_v;
                        Self::Rectangle {
                            first: SketchPoint::new(min_u, min_v),
                            opposite: SketchPoint::new(max_u, new_max_v),
                        }
                    }
                    3 => {
                        let new_min_u = min_u + delta_u;
                        Self::Rectangle {
                            first: SketchPoint::new(max_u, min_v),
                            opposite: SketchPoint::new(new_min_u, max_v),
                        }
                    }
                    _ => self.translate(delta_u, delta_v),
                }
            }
            (Self::Circle { center, rim }, SketchDragHandle::CircleRim) => {
                let current_radius = center.distance_squared(rim).sqrt();
                let rim_dir = if current_radius > 1e-9 {
                    (
                        (rim.u - center.u) / current_radius,
                        (rim.v - center.v) / current_radius,
                    )
                } else {
                    (1.0, 0.0)
                };
                let radial_delta = delta_u.mul_add(rim_dir.0, delta_v * rim_dir.1);
                let new_radius = (current_radius + radial_delta).max(0.01);
                let new_rim = SketchPoint::new(
                    new_radius.mul_add(rim_dir.0, center.u),
                    new_radius.mul_add(rim_dir.1, center.v),
                );
                Self::Circle {
                    center,
                    rim: new_rim,
                }
            }
            (Self::Arc { center, start, end }, SketchDragHandle::StartPoint) => {
                let new_start = SketchPoint::new(start.u + delta_u, start.v + delta_v);
                Self::Arc {
                    center,
                    start: new_start,
                    end,
                }
            }
            (Self::Arc { center, start, end }, SketchDragHandle::EndPoint) => {
                let new_end = SketchPoint::new(end.u + delta_u, end.v + delta_v);
                Self::Arc {
                    center,
                    start,
                    end: new_end,
                }
            }
            (Self::Arc { center, start, end }, SketchDragHandle::ArcCurve) => {
                let current_radius = center.distance_squared(start).sqrt();
                let start_dir = if current_radius > 1e-9 {
                    (
                        (start.u - center.u) / current_radius,
                        (start.v - center.v) / current_radius,
                    )
                } else {
                    (1.0, 0.0)
                };
                let end_dir = if current_radius > 1e-9 {
                    (
                        (end.u - center.u) / current_radius,
                        (end.v - center.v) / current_radius,
                    )
                } else {
                    (0.0, 1.0)
                };
                let radial_delta = delta_u.mul_add(start_dir.0, delta_v * start_dir.1);
                let new_radius = (current_radius + radial_delta).max(0.01);
                Self::Arc {
                    center,
                    start: SketchPoint::new(
                        new_radius.mul_add(start_dir.0, center.u),
                        new_radius.mul_add(start_dir.1, center.v),
                    ),
                    end: SketchPoint::new(
                        new_radius.mul_add(end_dir.0, center.u),
                        new_radius.mul_add(end_dir.1, center.v),
                    ),
                }
            }
            _ => self.translate(delta_u, delta_v),
        }
    }

    fn control_points(self) -> GeometryPoints {
        match self {
            Self::Point(point) => GeometryPoints::one(point),
            Self::Segment { start, end } => {
                let mid = SketchPoint::new((start.u + end.u) * 0.5, (start.v + end.v) * 0.5);
                GeometryPoints::three(start, end, mid)
            }
            Self::Rectangle { first, opposite } => {
                let corners = self
                    .rectangle_corners()
                    .expect("the rectangle branch always supplies corners");
                let center =
                    SketchPoint::new((first.u + opposite.u) * 0.5, (first.v + opposite.v) * 0.5);
                let mid_bottom = SketchPoint::new(
                    (corners[0].u + corners[1].u) * 0.5,
                    (corners[0].v + corners[1].v) * 0.5,
                );
                let mid_right = SketchPoint::new(
                    (corners[1].u + corners[2].u) * 0.5,
                    (corners[1].v + corners[2].v) * 0.5,
                );
                let mid_top = SketchPoint::new(
                    (corners[2].u + corners[3].u) * 0.5,
                    (corners[2].v + corners[3].v) * 0.5,
                );
                let mid_left = SketchPoint::new(
                    (corners[3].u + corners[0].u) * 0.5,
                    (corners[3].v + corners[0].v) * 0.5,
                );
                GeometryPoints::nine([
                    corners[0], corners[1], corners[2], corners[3], center, mid_bottom, mid_right,
                    mid_top, mid_left,
                ])
            }
            Self::Circle { center, rim } => GeometryPoints::two(center, rim),
            Self::Arc { center, start, end } => GeometryPoints::three(center, start, end),
        }
    }
}

/// A specific handle or feature of a sketch entity being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SketchDragHandle {
    /// Move the entire entity
    Translate,
    /// Corner of a rectangle (index 0..4 in rectangle_corners)
    RectangleCorner(usize),
    /// Side of a rectangle (0: Bottom, 1: Right, 2: Top, 3: Left)
    RectangleSide(usize),
    /// Rim of a circle (adjust radius / diameter)
    CircleRim,
    /// Start point of a line / arc / slot
    StartPoint,
    /// End point of a line / arc / slot
    EndPoint,
    /// Arc curve radius adjustment
    ArcCurve,
}

impl SketchDragHandle {
    #[must_use]
    pub const fn cursor(self) -> CursorIcon {
        match self {
            Self::Translate => CursorIcon::Move,
            Self::RectangleCorner(0 | 2) => CursorIcon::ResizeNwSe,
            Self::RectangleCorner(1 | 3) => CursorIcon::ResizeNeSw,
            Self::RectangleCorner(_) => CursorIcon::Grab,
            Self::RectangleSide(0 | 2) => CursorIcon::ResizeVertical,
            Self::RectangleSide(1 | 3) => CursorIcon::ResizeHorizontal,
            Self::RectangleSide(_) => CursorIcon::Grab,
            Self::CircleRim | Self::ArcCurve => CursorIcon::ResizeEast,
            Self::StartPoint | Self::EndPoint => CursorIcon::Crosshair,
        }
    }
}

#[must_use]
pub fn hit_test_drag_handle(
    geometry: SketchGeometry,
    view: SketchView,
    rect: Rect,
    position: Pos2,
    hit_radius: f32,
) -> SketchDragHandle {
    match geometry {
        SketchGeometry::Point(_) => SketchDragHandle::Translate,
        SketchGeometry::Segment { start, end } => {
            let start_pos = view.sketch_to_screen(rect, start);
            let end_pos = view.sketch_to_screen(rect, end);
            if start_pos.distance(position) <= hit_radius {
                SketchDragHandle::StartPoint
            } else if end_pos.distance(position) <= hit_radius {
                SketchDragHandle::EndPoint
            } else {
                SketchDragHandle::Translate
            }
        }
        SketchGeometry::Rectangle { .. } => {
            if let Some(center) = geometry.center() {
                let center_pos = view.sketch_to_screen(rect, center);
                if center_pos.distance(position) <= hit_radius {
                    return SketchDragHandle::Translate;
                }
            }
            if let Some(corners) = geometry.rectangle_corners() {
                let screen_corners = corners.map(|pt| view.sketch_to_screen(rect, pt));
                for (index, corner_pos) in screen_corners.iter().enumerate() {
                    if corner_pos.distance(position) <= hit_radius {
                        return SketchDragHandle::RectangleCorner(index);
                    }
                }
                let mut best_side = None;
                let mut min_side_dist = hit_radius;
                for index in 0..4 {
                    let d = point_segment_distance(
                        position,
                        screen_corners[index],
                        screen_corners[(index + 1) % 4],
                    );
                    if d <= min_side_dist {
                        min_side_dist = d;
                        best_side = Some(index);
                    }
                }
                if let Some(side) = best_side {
                    return SketchDragHandle::RectangleSide(side);
                }
            }
            SketchDragHandle::Translate
        }
        SketchGeometry::Circle { center, rim } => {
            let center_pos = view.sketch_to_screen(rect, center);
            if center_pos.distance(position) <= hit_radius {
                return SketchDragHandle::Translate;
            }
            let rim_pos = view.sketch_to_screen(rect, rim);
            let radius = center_pos.distance(rim_pos);
            let dist_to_center = center_pos.distance(position);
            if (dist_to_center - radius).abs() <= hit_radius + 4.0 {
                return SketchDragHandle::CircleRim;
            }
            SketchDragHandle::Translate
        }
        SketchGeometry::Arc { center, start, end } => {
            let center_pos = view.sketch_to_screen(rect, center);
            if center_pos.distance(position) <= hit_radius {
                return SketchDragHandle::Translate;
            }
            let start_pos = view.sketch_to_screen(rect, start);
            if start_pos.distance(position) <= hit_radius {
                return SketchDragHandle::StartPoint;
            }
            let end_pos = view.sketch_to_screen(rect, end);
            if end_pos.distance(position) <= hit_radius {
                return SketchDragHandle::EndPoint;
            }
            SketchDragHandle::ArcCurve
        }
    }
}

/// A stable semantic identity for a live sketch measurement.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SketchDimensionKind {
    U,
    V,
    Length,
    AngleDegrees,
    DeltaU,
    DeltaV,
    Width,
    Height,
    Diameter,
    Radius,
    SweepDegrees,
}

impl SketchDimensionKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::U => "U coordinate",
            Self::V => "V coordinate",
            Self::Length => "Line length",
            Self::AngleDegrees => "Line angle",
            Self::DeltaU => "Line delta U",
            Self::DeltaV => "Line delta V",
            Self::Width => "Rectangle width",
            Self::Height => "Rectangle height",
            Self::Diameter => "Circle diameter",
            Self::Radius => "Arc radius",
            Self::SweepDegrees => "Arc sweep",
        }
    }

    const fn short_label(self) -> &'static str {
        match self {
            Self::U => "U",
            Self::V => "V",
            Self::Length => "L",
            Self::AngleDegrees => "ANGLE",
            Self::DeltaU => "DU",
            Self::DeltaV => "DV",
            Self::Width => "W",
            Self::Height => "H",
            Self::Diameter => "DIA",
            Self::Radius => "R",
            Self::SweepDegrees => "SWEEP",
        }
    }

    /// Whether this dimension earns a box on the canvas, as opposed to a row in
    /// the sketch panel.
    ///
    /// A line has two degrees of freedom, and the canvas was showing four
    /// numbers for them: length and angle describe it in polar terms, delta U
    /// and delta V describe the same line in Cartesian ones. Painting both
    /// parameterisations draws the line twice and stacks four boxes over the
    /// geometry being drawn.
    ///
    /// So the canvas carries one complete set and the panel carries everything.
    /// Nothing is lost — the deltas stay editable under LIVE DIMENSIONS — and
    /// the rule generalises: a kind that restates what another kind already
    /// says belongs in the panel. Every other geometry is already minimal (a
    /// point is U and V, a rectangle is width and height), so only the line's
    /// redundant pair is suppressed here.
    const fn shows_on_canvas(self) -> bool {
        !matches!(self, Self::DeltaU | Self::DeltaV)
    }

    const fn is_angle(self) -> bool {
        matches!(self, Self::AngleDegrees | Self::SweepDegrees)
    }

    const fn is_length_magnitude(self) -> bool {
        matches!(
            self,
            Self::Length | Self::Width | Self::Height | Self::Diameter | Self::Radius
        )
    }
}

/// Public, allocation-friendly observation used by the inspector and UI tests.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DimensionReadout {
    pub kind: SketchDimensionKind,
    pub value: f64,
    pub locked: bool,
    pub editable: bool,
}

/// A numeric value is retained for correction instead of corrupting geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DimensionInputError {
    Empty,
    NotANumber,
    NonFinite,
    NonPositive,
    CoordinateOutOfRange,
    SweepOutOfRange,
    DegenerateGeometry,
}

impl DimensionInputError {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Empty => "Enter a value",
            Self::NotANumber => "Enter a number",
            Self::NonFinite => "Value must be finite",
            Self::NonPositive => "Length must be greater than zero",
            Self::CoordinateOutOfRange => "Coordinate is outside the supported range",
            Self::SweepOutOfRange => "Sweep must be greater than 0 and less than 360 degrees",
            Self::DegenerateGeometry => "Dimension would create degenerate geometry",
        }
    }
}

/// Retained validation result for one typed active-tool parameter.
///
/// The editor keeps its text and the last valid numeric value separately. A
/// rejected edit is therefore visible and correctable without ever poisoning
/// the live preview or the recipe staged behind the universal confirmation
/// gate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolInputError {
    Empty,
    NotANumber,
    NonFinite,
    NonPositive,
    NonInteger,
    OutOfRange,
    PatternLimit,
    DegeneratePattern,
    ZeroLength,
    SlotLengthNotGreaterThanWidth,
}

impl ToolInputError {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Empty => "Enter a value",
            Self::NotANumber => "Enter a number",
            Self::NonFinite => "Value must be finite",
            Self::NonPositive => "Value must be greater than zero",
            Self::NonInteger => "Enter a whole number",
            Self::OutOfRange => "Value is outside the supported range",
            Self::PatternLimit => "Pattern must create between 2 and 256 total instances",
            Self::DegeneratePattern => "Pattern spacing or angular extent must be non-zero",
            Self::ZeroLength => "Length must be non-zero",
            Self::SlotLengthNotGreaterThanWidth => {
                "Centre slot overall length must be greater than its width"
            }
        }
    }
}

/// Retained validation for one committed-recipe parameter edit.
///
/// Numeric syntax/domain failures and exact replay failures remain distinct:
/// a perfectly valid positive radius can still be too large for the selected
/// corner, for example. In either case the user's text remains visible while
/// the last exact preview stays behind the workbench confirmation gate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecipeParameterError {
    Numeric(ToolInputError),
    ReplayRejected,
}

impl RecipeParameterError {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Numeric(error) => error.label(),
            Self::ReplayRejected => {
                "Value does not fit the selected feature or its dependent geometry"
            }
        }
    }
}

/// One concise, inspector-ready scalar from the selected authored recipe.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectedRecipeParameter {
    pub stable_key: &'static str,
    pub label: &'static str,
    pub text: String,
    pub unit: &'static str,
    pub editable: bool,
    pub read_only_reason: Option<&'static str>,
    pub error: Option<RecipeParameterError>,
}

/// Read-only projection of the selected operation's persistent design intent.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectedRecipeEditorView {
    pub title: &'static str,
    pub parameters: Vec<SelectedRecipeParameter>,
    pub reference_note: &'static str,
}

#[derive(Clone, Debug)]
struct RetainedRecipeParameter {
    stable_key: &'static str,
    label: &'static str,
    text: String,
    unit: &'static str,
    value: Option<f64>,
    domain: ToolNumberDomain,
    read_only_reason: Option<&'static str>,
    error: Option<RecipeParameterError>,
}

impl RetainedRecipeParameter {
    fn literal(
        stable_key: &'static str,
        label: &'static str,
        value: f64,
        unit: &'static str,
        domain: ToolNumberDomain,
    ) -> Self {
        Self {
            stable_key,
            label,
            text: format_tool_number(value),
            unit,
            value: Some(value),
            domain,
            read_only_reason: None,
            error: None,
        }
    }

    fn bound(
        stable_key: &'static str,
        label: &'static str,
        unit: &'static str,
        domain: ToolNumberDomain,
    ) -> Self {
        Self {
            stable_key,
            label,
            text: "Bound input".to_owned(),
            unit,
            value: None,
            domain,
            read_only_reason: Some("Driven by a model input; edit the owning parameter instead"),
            error: None,
        }
    }

    /// A free-text parameter: the characters a text recipe sets.
    fn text(stable_key: &'static str, label: &'static str, content: &str) -> Self {
        Self {
            stable_key,
            label,
            text: content.to_owned(),
            unit: "",
            value: None,
            domain: ToolNumberDomain::Text,
            read_only_reason: None,
            error: None,
        }
    }

    const fn is_text(&self) -> bool {
        matches!(self.domain, ToolNumberDomain::Text)
    }

    fn view(&self) -> SelectedRecipeParameter {
        SelectedRecipeParameter {
            stable_key: self.stable_key,
            label: self.label,
            text: self.text.clone(),
            unit: self.unit,
            editable: self.value.is_some() || self.is_text(),
            read_only_reason: self.read_only_reason,
            error: self.error,
        }
    }
}

#[derive(Clone, Debug)]
struct SelectedRecipeEditor {
    subject: SketchEntityId,
    operation: CoreOperationId,
    original_recipe: CoreRecipe,
    title: &'static str,
    reference_note: &'static str,
    parameters: Vec<RetainedRecipeParameter>,
}

#[derive(Clone, Copy, Debug)]
enum ToolNumberDomain {
    Positive,
    Finite,
    NonZero,
    NonZeroLength,
    Integer {
        minimum: u16,
        maximum: u16,
    },
    /// Not a number at all: free text, kept verbatim.
    Text,
}

#[derive(Clone, Debug)]
struct RetainedToolNumber {
    text: String,
    value: f64,
    error: Option<ToolInputError>,
    user_edited: bool,
}

impl RetainedToolNumber {
    fn new(default: f64) -> Self {
        Self {
            text: format_tool_number(default),
            value: default,
            error: None,
            user_edited: false,
        }
    }

    fn edit(&mut self, text: String, domain: ToolNumberDomain) {
        self.text = text;
        self.user_edited = true;
        self.error = validate_tool_number(&self.text, domain).map_or_else(Some, |value| {
            self.value = value;
            None
        });
    }

    fn restore_last_valid(&mut self) {
        self.text = format_tool_number(self.value);
        self.error = None;
    }

    fn sync_live_value(&mut self, value: f64, domain: ToolNumberDomain) {
        let formatted = format_tool_number(value);
        if !self.user_edited && validate_tool_number(&formatted, domain).is_ok() {
            self.value = value;
            self.text = formatted;
            self.error = None;
        }
    }

    fn set_manipulator_value(&mut self, value: f64, domain: ToolNumberDomain) {
        let formatted = format_tool_number(value);
        if validate_tool_number(&formatted, domain).is_ok() {
            self.value = value;
            self.text = formatted;
            self.error = None;
            self.user_edited = true;
        }
    }
}

#[derive(Debug, Default)]
struct ActiveToolInputs {
    numbers: BTreeMap<(ToolVariant, &'static str), RetainedToolNumber>,
    flags: BTreeMap<(ToolVariant, &'static str), bool>,
    texts: BTreeMap<(ToolVariant, &'static str), String>,
}

/// Default free-text value of a tool field, when the field is text rather
/// than a number or a flag.
fn tool_text_default(variant: ToolVariant, stable_key: &'static str) -> Option<&'static str> {
    match (variant, stable_key) {
        (ToolVariant::Text, "content") => Some(DEFAULT_TEXT_CONTENT),
        _ => None,
    }
}

const DEFAULT_TEXT_CONTENT: &str = "TEXT";
const DEFAULT_TEXT_HEIGHT: f64 = 10.0;

fn format_tool_number(value: f64) -> String {
    if value.fract().abs() <= f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    }
}

fn validate_tool_number(text: &str, domain: ToolNumberDomain) -> Result<f64, ToolInputError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(ToolInputError::Empty);
    }
    let value = trimmed
        .parse::<f64>()
        .map_err(|_| ToolInputError::NotANumber)?;
    if !value.is_finite() {
        return Err(ToolInputError::NonFinite);
    }
    match domain {
        ToolNumberDomain::Positive if value <= PrecisionPolicy::default().min_feature_size => {
            Err(ToolInputError::NonPositive)
        }
        ToolNumberDomain::NonZero if value.abs() <= PrecisionPolicy::default().min_feature_size => {
            Err(ToolInputError::DegeneratePattern)
        }
        ToolNumberDomain::NonZeroLength
            if value.abs() <= PrecisionPolicy::default().min_feature_size =>
        {
            Err(ToolInputError::ZeroLength)
        }
        ToolNumberDomain::Integer { minimum, maximum }
            if value.fract() != 0.0 || value < f64::from(minimum) || value > f64::from(maximum) =>
        {
            if value.fract() != 0.0 {
                Err(ToolInputError::NonInteger)
            } else {
                Err(ToolInputError::OutOfRange)
            }
        }
        ToolNumberDomain::Positive
        | ToolNumberDomain::Finite
        | ToolNumberDomain::NonZero
        | ToolNumberDomain::NonZeroLength
        | ToolNumberDomain::Integer { .. }
        | ToolNumberDomain::Text => Ok(value),
    }
}

fn tool_number_spec(
    variant: ToolVariant,
    stable_key: &'static str,
) -> Option<(f64, ToolNumberDomain)> {
    let positive = ToolNumberDomain::Positive;
    let finite = ToolNumberDomain::Finite;
    let non_zero = ToolNumberDomain::NonZero;
    match (variant, stable_key) {
        (ToolVariant::InnerDiameterPolygon | ToolVariant::OuterDiameterPolygon, "sides") => Some((
            f64::from(DEFAULT_POLYGON_SIDES),
            ToolNumberDomain::Integer {
                minimum: CORE_MIN_POLYGON_SIDES,
                maximum: CORE_MAX_POLYGON_SIDES,
            },
        )),
        (ToolVariant::Text, "height") => Some((DEFAULT_TEXT_HEIGHT, positive)),
        (ToolVariant::Text, "angle") => Some((0.0, finite)),
        (ToolVariant::Fillet, "radius") => Some((DEFAULT_FILLET_RADIUS, positive)),
        // Signed: the sign is which side of the chain the copy lands on, which
        // is a thing to type as much as a thing to point at.
        (ToolVariant::Offset, "distance") => Some((DEFAULT_OFFSET_DISTANCE, non_zero)),
        (ToolVariant::Chamfer | ToolVariant::TwoDistanceChamfer, "distance_1") => {
            Some((DEFAULT_CHAMFER_DISTANCE, positive))
        }
        (ToolVariant::TwoDistanceChamfer, "distance_2") => {
            Some((DEFAULT_CHAMFER_DISTANCE, positive))
        }
        (ToolVariant::RectangularPattern, "count_u") => Some((
            f64::from(DEFAULT_RECTANGULAR_PATTERN_COLUMNS),
            ToolNumberDomain::Integer {
                minimum: 1,
                maximum: 256,
            },
        )),
        (ToolVariant::RectangularPattern, "count_v") => Some((
            f64::from(DEFAULT_RECTANGULAR_PATTERN_ROWS),
            ToolNumberDomain::Integer {
                minimum: 1,
                maximum: 256,
            },
        )),
        (ToolVariant::RectangularPattern, "spacing_u" | "spacing_v") => {
            Some((DEFAULT_TOOL_LENGTH, non_zero))
        }
        (ToolVariant::CircularPattern, "count") => Some((
            f64::from(DEFAULT_CIRCULAR_PATTERN_COUNT),
            ToolNumberDomain::Integer {
                minimum: 2,
                maximum: 256,
            },
        )),
        (ToolVariant::CircularPattern, "extent") => Some((360.0, non_zero)),
        (ToolVariant::InnerDiameterPolygon, "inner_diameter")
        | (ToolVariant::OuterDiameterPolygon, "outer_diameter") => {
            Some((DEFAULT_POLYGON_DIAMETER, positive))
        }
        (ToolVariant::InnerDiameterPolygon | ToolVariant::OuterDiameterPolygon, "rotation") => {
            Some((0.0, finite))
        }
        (ToolVariant::TwoPointSlot, "centre_distance")
        | (ToolVariant::CentreToOuterPointSlot, "overall_length") => {
            Some((DEFAULT_SLOT_LENGTH, positive))
        }
        (ToolVariant::TwoPointSlot | ToolVariant::CentreToOuterPointSlot, "width") => {
            Some((DEFAULT_SLOT_WIDTH, positive))
        }
        (ToolVariant::TwoPointSlot | ToolVariant::CentreToOuterPointSlot, "angle") => {
            Some((0.0, finite))
        }
        _ => None,
    }
}

fn tool_flag_default(variant: ToolVariant, stable_key: &'static str) -> Option<bool> {
    match (variant, stable_key) {
        // On by default, as it is in the reference: a click on one wall of an
        // outline almost always means the outline.
        (ToolVariant::Offset, "chain_selection") => Some(true),
        (ToolVariant::RectangularPattern, "second_direction") => Some(false),
        (ToolVariant::CircularPattern, "full_circle" | "rotate_instances") => Some(true),
        _ => None,
    }
}

const BOUND_REFERENCE_NOTE: &str =
    "Placement points and referenced geometry are retained exactly and are read-only here.";

fn literal_length_parameter(
    stable_key: &'static str,
    label: &'static str,
    value: &CoreValue<CoreLength>,
) -> RetainedRecipeParameter {
    match value {
        CoreValue::Literal(value) => RetainedRecipeParameter::literal(
            stable_key,
            label,
            value.get(),
            "mm",
            ToolNumberDomain::Positive,
        ),
        CoreValue::Input(_) => {
            RetainedRecipeParameter::bound(stable_key, label, "mm", ToolNumberDomain::Positive)
        }
    }
}

fn literal_signed_length_parameter(
    stable_key: &'static str,
    label: &'static str,
    value: &CoreValue<CoreSignedLength>,
    allow_zero: bool,
) -> RetainedRecipeParameter {
    let domain = if allow_zero {
        ToolNumberDomain::Finite
    } else {
        ToolNumberDomain::NonZeroLength
    };
    match value {
        CoreValue::Literal(value) => {
            RetainedRecipeParameter::literal(stable_key, label, value.get(), "mm", domain)
        }
        CoreValue::Input(_) => RetainedRecipeParameter::bound(stable_key, label, "mm", domain),
    }
}

fn literal_integer_parameter(
    stable_key: &'static str,
    label: &'static str,
    value: &CoreValue<CoreInteger>,
    minimum: u16,
    maximum: u16,
) -> RetainedRecipeParameter {
    let domain = ToolNumberDomain::Integer { minimum, maximum };
    match value {
        CoreValue::Literal(value) => {
            RetainedRecipeParameter::literal(stable_key, label, f64::from(value.get()), "", domain)
        }
        CoreValue::Input(_) => RetainedRecipeParameter::bound(stable_key, label, "", domain),
    }
}

fn literal_angle_parameter(
    stable_key: &'static str,
    label: &'static str,
    value: &CoreValue<CoreAngle>,
) -> RetainedRecipeParameter {
    match value {
        CoreValue::Literal(value) => RetainedRecipeParameter::literal(
            stable_key,
            label,
            value.get().to_degrees(),
            "°",
            ToolNumberDomain::Finite,
        ),
        CoreValue::Input(_) => {
            RetainedRecipeParameter::bound(stable_key, label, "°", ToolNumberDomain::Finite)
        }
    }
}

fn centre_circle_diameter_parameter(radius: &CoreValue<CoreLength>) -> RetainedRecipeParameter {
    match radius {
        CoreValue::Literal(radius) => RetainedRecipeParameter::literal(
            "diameter",
            "Diameter",
            radius.get() * 2.0,
            "mm",
            ToolNumberDomain::Positive,
        ),
        CoreValue::Input(_) => {
            RetainedRecipeParameter::bound("diameter", "Diameter", "mm", ToolNumberDomain::Positive)
        }
    }
}

fn selected_recipe_editor_for(
    subject: SketchEntityId,
    operation: CoreOperationId,
    recipe: CoreRecipe,
) -> SelectedRecipeEditor {
    let (title, parameters, reference_note) = match &recipe {
        CoreRecipe::TwoPointRectangle { width, height, .. } => (
            "Two-point rectangle",
            vec![
                literal_signed_length_parameter("width", "Width", width, false),
                literal_signed_length_parameter("height", "Height", height, false),
            ],
            BOUND_REFERENCE_NOTE,
        ),
        CoreRecipe::CentrePointRectangle { width, height, .. } => (
            "Centre-point rectangle",
            vec![
                literal_length_parameter("width", "Width", width),
                literal_length_parameter("height", "Height", height),
            ],
            BOUND_REFERENCE_NOTE,
        ),
        CoreRecipe::CentrePointCircle { radius, .. } => (
            "Centre-point circle",
            vec![centre_circle_diameter_parameter(radius)],
            "The exact radius is half the displayed diameter; centre and analytic seam stay fixed.",
        ),
        CoreRecipe::InnerDiameterPolygon {
            inner_diameter,
            sides,
            rotation,
            ..
        } => (
            "Inner-diameter polygon",
            vec![
                literal_integer_parameter(
                    "sides",
                    "Sides",
                    sides,
                    CORE_MIN_POLYGON_SIDES,
                    CORE_MAX_POLYGON_SIDES,
                ),
                literal_length_parameter("inner_diameter", "Inner diameter", inner_diameter),
                // A drafter dimensions a side, not a diameter. Offered beside
                // the diameter rather than instead of it: both drive the same
                // single number, so either can be typed.
                polygon_side_parameter(inner_diameter, sides, true),
                literal_angle_parameter("rotation", "Rotation", rotation),
            ],
            BOUND_REFERENCE_NOTE,
        ),
        CoreRecipe::OuterDiameterPolygon {
            outer_diameter,
            sides,
            rotation,
            ..
        } => (
            "Outer-diameter polygon",
            vec![
                literal_integer_parameter(
                    "sides",
                    "Sides",
                    sides,
                    CORE_MIN_POLYGON_SIDES,
                    CORE_MAX_POLYGON_SIDES,
                ),
                literal_length_parameter("outer_diameter", "Outer diameter", outer_diameter),
                // A drafter dimensions a side, not a diameter. Offered beside
                // the diameter rather than instead of it: both drive the same
                // single number, so either can be typed.
                polygon_side_parameter(outer_diameter, sides, false),
                literal_angle_parameter("rotation", "Rotation", rotation),
            ],
            BOUND_REFERENCE_NOTE,
        ),
        CoreRecipe::Text {
            content,
            height,
            angle,
            ..
        } => (
            "Text",
            vec![
                RetainedRecipeParameter::text("content", "Text", content),
                literal_length_parameter("height", "Height", height),
                literal_angle_parameter("angle", "Angle", angle),
            ],
            "The baseline anchor stays fixed; edit the text, its capital height, or its angle.",
        ),
        CoreRecipe::TwoPointSlot { width, .. } => (
            "Two-point slot",
            vec![literal_length_parameter("width", "Width", width)],
            "Both cap-centre references stay fixed; edit Width to resize the analytic rails and caps.",
        ),
        CoreRecipe::CentreOuterPointSlot {
            overall_length,
            width,
            angle,
            ..
        } => (
            "Centre-to-outer-point slot",
            vec![
                literal_length_parameter("overall_length", "Overall length", overall_length),
                literal_length_parameter("width", "Width", width),
                literal_angle_parameter("angle", "Angle", angle),
            ],
            BOUND_REFERENCE_NOTE,
        ),
        CoreRecipe::RectangularPattern {
            columns,
            rows,
            column_spacing,
            row_spacing,
            direction,
            ..
        } => (
            "Rectangular pattern",
            vec![
                literal_integer_parameter("columns", "Columns", columns, 1, 256),
                literal_integer_parameter("rows", "Rows", rows, 1, 256),
                literal_signed_length_parameter(
                    "column_spacing",
                    "Column spacing",
                    column_spacing,
                    false,
                ),
                literal_signed_length_parameter("row_spacing", "Row spacing", row_spacing, true),
                literal_angle_parameter("direction", "Direction", direction),
            ],
            "Source curve IDs are retained; count changes preserve every matching semantic output ID.",
        ),
        CoreRecipe::Offset { distance, .. } => (
            "Offset",
            vec![literal_signed_length_parameter(
                "distance", "Distance", distance, false,
            )],
            "The chain and the side it was taken on are retained; only the distance is edited here.",
        ),
        CoreRecipe::CircularPattern {
            count, total_angle, ..
        } => (
            "Circular pattern",
            vec![
                literal_integer_parameter("count", "Count", count, 2, 256),
                literal_angle_parameter("total_angle", "Total angle", total_angle),
            ],
            "Source curve IDs, centre, distribution, and rotate-instances intent stay fixed.",
        ),
        CoreRecipe::Fillet { radius, .. } | CoreRecipe::FilletWithHints { radius, .. } => (
            "2D fillet",
            vec![literal_length_parameter("radius", "Radius", radius)],
            "Source branches and persisted branch hints stay fixed during radius replay.",
        ),
        CoreRecipe::Chamfer {
            first_distance,
            second_distance,
            ..
        } => {
            let parameters = if first_distance == second_distance {
                vec![literal_length_parameter(
                    "distance",
                    "Distance",
                    first_distance,
                )]
            } else {
                vec![
                    literal_length_parameter("first_distance", "Distance 1", first_distance),
                    literal_length_parameter("second_distance", "Distance 2", second_distance),
                ]
            };
            (
                "2D chamfer",
                parameters,
                "Source branches stay fixed. Equal values remain an equal-distance chamfer.",
            )
        }
        CoreRecipe::LegacyImportedProfile { .. } => (
            "Imported profile",
            Vec::new(),
            "Imported compatibility geometry has no recoverable primitive parameters.",
        ),
        // A line stores two points, not a length and a bearing, so those two
        // numbers have to be derived here and turned back into an end point on
        // apply. They are only *drivable* when the end is a literal position:
        // an end bound to an existing point takes its length from that binding,
        // and letting a typed value fight the reference would silently break
        // one of them.
        CoreRecipe::Line { start, end } | CoreRecipe::CentreLine { start, end } => {
            match (start, end) {
                (CorePointInput::Position(start), CorePointInput::Position(end)) => (
                    "Authored line",
                    vec![
                        RetainedRecipeParameter::literal(
                            "length",
                            "Length",
                            (end.u - start.u).hypot(end.v - start.v),
                            "mm",
                            ToolNumberDomain::Positive,
                        ),
                        RetainedRecipeParameter::literal(
                            "angle",
                            "Angle",
                            (end.v - start.v).atan2(end.u - start.u).to_degrees(),
                            "\u{b0}",
                            ToolNumberDomain::Finite,
                        ),
                    ],
                    "The start point stays fixed; Length and Angle move the end point.",
                ),
                _ => (
                    "Authored line",
                    Vec::new(),
                    "An end point bound to existing geometry takes its length and angle from that reference.",
                ),
            }
        }
        CoreRecipe::Point { .. }
        | CoreRecipe::Polyline { .. }
        | CoreRecipe::TwoPointCircle { .. }
        | CoreRecipe::CentreStartEndArc { .. }
        | CoreRecipe::FitPointSpline { .. }
        | CoreRecipe::ControlVertexSpline { .. }
        | CoreRecipe::Trim { .. } => (
            "Authored sketch feature",
            Vec::new(),
            "This feature's point, branch, or trim references are read-only in the first parameter-editing pass.",
        ),
    };
    SelectedRecipeEditor {
        subject,
        operation,
        original_recipe: recipe,
        title,
        reference_note,
        parameters,
    }
}

fn recipe_parameter_value(editor: &SelectedRecipeEditor, key: &'static str) -> Option<f64> {
    editor
        .parameters
        .iter()
        .find(|parameter| parameter.stable_key == key)
        .and_then(|parameter| parameter.value)
}

fn recipe_parameter_text(editor: &SelectedRecipeEditor, key: &'static str) -> Option<String> {
    editor
        .parameters
        .iter()
        .find(|parameter| {
            parameter.stable_key == key && matches!(parameter.domain, ToolNumberDomain::Text)
        })
        .map(|parameter| parameter.text.clone())
}

/// The recipe a text click stages: one line of `content` at capital height
/// `height`, its baseline through `anchor` at `angle` radians from `+u`.
fn text_recipe(anchor: SketchPoint, content: &str, height: f64, angle: f64) -> Option<CoreRecipe> {
    if content.chars().all(char::is_whitespace) {
        return None;
    }
    Some(CoreRecipe::Text {
        anchor: core_point_input(anchor),
        content: content.to_owned(),
        height: CoreValue::Literal(CoreLength::new(height).ok()?),
        angle: CoreValue::Literal(CoreAngle::radians(angle).ok()?),
    })
}

/// The outline segments of `content` placed at `anchor`, for the live
/// preview under the pointer before the anchor click. An unsettable text
/// (empty, or a glyph the typeface lacks) previews nothing rather than
/// something misleading.
fn text_preview_geometries(
    anchor: SketchPoint,
    content: &str,
    height: f64,
    angle: f64,
) -> Vec<SketchGeometry> {
    let Ok(outlines) = artificer_sketch::text::text_outlines(content, height) else {
        return Vec::new();
    };
    let (sin, cos) = angle.sin_cos();
    let place = |point: artificer_sketch::SketchPoint2| {
        SketchPoint::new(
            anchor.u + point.u * cos - point.v * sin,
            anchor.v + point.u * sin + point.v * cos,
        )
    };
    let mut geometries = Vec::new();
    for outline in &outlines.loops {
        for index in 0..outline.points.len() {
            let start = place(outline.points[index]);
            let end = place(outline.points[(index + 1) % outline.points.len()]);
            geometries.push(SketchGeometry::segment(start, end));
        }
    }
    geometries
}

fn replace_literal_length(
    target: &mut CoreValue<CoreLength>,
    value: Option<f64>,
) -> Result<(), ()> {
    if let Some(value) = value {
        *target = CoreValue::Literal(CoreLength::new(value).map_err(|_| ())?);
    }
    Ok(())
}

fn replace_literal_signed_length(
    target: &mut CoreValue<CoreSignedLength>,
    value: Option<f64>,
) -> Result<(), ()> {
    if let Some(value) = value {
        *target = CoreValue::Literal(CoreSignedLength::new(value).map_err(|_| ())?);
    }
    Ok(())
}

fn replace_literal_integer(target: &mut CoreValue<CoreInteger>, value: Option<f64>) {
    if let Some(value) = value {
        *target = CoreValue::Literal(CoreInteger::new(value as u16));
    }
}

fn replace_literal_angle(
    target: &mut CoreValue<CoreAngle>,
    degrees: Option<f64>,
) -> Result<(), ()> {
    if let Some(degrees) = degrees {
        *target = CoreValue::Literal(CoreAngle::radians(degrees.to_radians()).map_err(|_| ())?);
    }
    Ok(())
}

/// A regular polygon's side length, from the diameter that actually drives it.
///
/// The recipe stores a diameter because that is what the tool draws with, but a
/// drafter dimensions a *side*. The two are rigidly related for a regular
/// n-gon, so the side is offered as a driver and converted back here — which is
/// why changing one side changes all of them. It cannot do anything else: there
/// is one number underneath.
///
/// Circumradius form (outer): `side = 2 R sin(pi/n)`, so `D = side / sin(pi/n)`.
/// Apothem form (inner): `side = 2 a tan(pi/n)`, so `d = side / tan(pi/n)`.
/// The `side` driver for a polygon, derived from whichever diameter it stores.
fn polygon_side_parameter(
    diameter: &CoreValue<CoreLength>,
    sides: &CoreValue<CoreInteger>,
    inner: bool,
) -> RetainedRecipeParameter {
    let derived = match (diameter, sides) {
        (CoreValue::Literal(diameter), CoreValue::Literal(sides)) => {
            polygon_side_from_diameter(diameter.get(), u32::from(sides.get()), inner)
        }
        _ => None,
    };
    derived.map_or_else(
        || RetainedRecipeParameter::bound("side", "Side", "mm", ToolNumberDomain::Positive),
        |side| {
            RetainedRecipeParameter::literal("side", "Side", side, "mm", ToolNumberDomain::Positive)
        },
    )
}

fn polygon_side_from_diameter(diameter: f64, sides: u32, inner: bool) -> Option<f64> {
    let n = f64::from(sides);
    if !(3.0..=1024.0).contains(&n) || !diameter.is_finite() {
        return None;
    }
    let quarter_turn = std::f64::consts::PI / n;
    let factor = if inner {
        quarter_turn.tan()
    } else {
        quarter_turn.sin()
    };
    (factor.is_finite() && factor > 0.0).then_some(diameter * factor)
}

fn polygon_diameter_from_side(side: f64, sides: u32, inner: bool) -> Option<f64> {
    let n = f64::from(sides);
    if !(3.0..=1024.0).contains(&n) || !side.is_finite() || side <= 0.0 {
        return None;
    }
    let quarter_turn = std::f64::consts::PI / n;
    let factor = if inner {
        quarter_turn.tan()
    } else {
        quarter_turn.sin()
    };
    (factor.is_finite() && factor > 0.0).then(|| side / factor)
}

fn literal_side_count(sides: &CoreValue<CoreInteger>) -> Option<u32> {
    match sides {
        CoreValue::Literal(count) => Some(u32::from(count.get())),
        CoreValue::Input(_) => None,
    }
}

fn literal_length_value(length: &CoreValue<CoreLength>) -> Option<f64> {
    match length {
        CoreValue::Literal(value) => Some(value.get()),
        CoreValue::Input(_) => None,
    }
}

fn literal_signed_length_value(length: &CoreValue<CoreSignedLength>) -> Option<f64> {
    match length {
        CoreValue::Literal(value) => Some(value.get()),
        CoreValue::Input(_) => None,
    }
}

/// The diameter a polygon should now carry, given that its `side` and its
/// diameter drive the same single number and nothing records which box was
/// typed into.
///
/// The editor's texts were derived from the recipe as it was, so a text that
/// still says what the recipe implied was not typed into, and the one that
/// moved is the edit. Comparing the two texts against *each other* — as this
/// used to — misreads every edit: a typed diameter leaves the derived side
/// text stale, so the side looked edited and the diameter snapped back; and a
/// changed side count made the untouched side text disagree with the new
/// implication, so the diameter was recomputed from a side nobody typed. A
/// side count on its own keeps the diameter — a diameter polygon is stored by
/// its diameter — and `None` leaves the literal exactly as it was, so a
/// six-decimal round trip through the text never drifts it either.
fn polygon_driven_diameter(
    original_diameter: Option<f64>,
    original_count: Option<u32>,
    new_count: Option<u32>,
    typed_diameter: Option<f64>,
    typed_side: Option<f64>,
    inner: bool,
) -> Option<f64> {
    // Texts carry six decimals, so an untouched value round-trips to within
    // 5e-7 of its origin; anything past this is a hand on the keyboard.
    const UNCHANGED: f64 = 5.0e-6;
    let moved = |typed: Option<f64>, origin: Option<f64>| {
        typed
            .zip(origin)
            .is_some_and(|(typed, origin)| (typed - origin).abs() > UNCHANGED)
    };
    let original_side = original_diameter
        .zip(original_count)
        .and_then(|(diameter, count)| polygon_side_from_diameter(diameter, count, inner));
    if moved(typed_diameter, original_diameter) {
        typed_diameter
    } else if moved(typed_side, original_side) {
        typed_side
            .zip(new_count)
            .and_then(|(side, count)| polygon_diameter_from_side(side, count, inner))
    } else {
        None
    }
}

fn rebuilt_selected_recipe(editor: &SelectedRecipeEditor) -> Result<CoreRecipe, ()> {
    let mut recipe = editor.original_recipe.clone();
    match &mut recipe {
        CoreRecipe::TwoPointRectangle { width, height, .. } => {
            replace_literal_signed_length(width, recipe_parameter_value(editor, "width"))?;
            replace_literal_signed_length(height, recipe_parameter_value(editor, "height"))?;
        }
        CoreRecipe::CentrePointRectangle { width, height, .. } => {
            replace_literal_length(width, recipe_parameter_value(editor, "width"))?;
            replace_literal_length(height, recipe_parameter_value(editor, "height"))?;
        }
        CoreRecipe::Line { start, end } | CoreRecipe::CentreLine { start, end } => {
            if let (CorePointInput::Position(origin), CorePointInput::Position(tip)) =
                (&*start, &mut *end)
            {
                let current_length = (tip.u - origin.u).hypot(tip.v - origin.v);
                let current_angle = (tip.v - origin.v).atan2(tip.u - origin.u);
                let length = recipe_parameter_value(editor, "length").unwrap_or(current_length);
                let angle =
                    recipe_parameter_value(editor, "angle").map_or(current_angle, f64::to_radians);
                if !length.is_finite() || !angle.is_finite() || length <= 0.0 {
                    return Err(());
                }
                *tip = CorePoint2::new(
                    origin.u + length * angle.cos(),
                    origin.v + length * angle.sin(),
                );
            }
        }
        CoreRecipe::CentrePointCircle { radius, .. } => {
            replace_literal_length(
                radius,
                recipe_parameter_value(editor, "diameter").map(|diameter| diameter * 0.5),
            )?;
        }
        CoreRecipe::InnerDiameterPolygon {
            inner_diameter,
            sides,
            rotation,
            ..
        } => {
            let original_count = literal_side_count(sides);
            let original_diameter = literal_length_value(inner_diameter);
            replace_literal_integer(sides, recipe_parameter_value(editor, "sides"));
            let driven = polygon_driven_diameter(
                original_diameter,
                original_count,
                literal_side_count(sides),
                recipe_parameter_value(editor, "inner_diameter"),
                recipe_parameter_value(editor, "side"),
                true,
            );
            replace_literal_length(inner_diameter, driven)?;
            replace_literal_angle(rotation, recipe_parameter_value(editor, "rotation"))?;
        }
        CoreRecipe::OuterDiameterPolygon {
            outer_diameter,
            sides,
            rotation,
            ..
        } => {
            let original_count = literal_side_count(sides);
            let original_diameter = literal_length_value(outer_diameter);
            replace_literal_integer(sides, recipe_parameter_value(editor, "sides"));
            let driven = polygon_driven_diameter(
                original_diameter,
                original_count,
                literal_side_count(sides),
                recipe_parameter_value(editor, "outer_diameter"),
                recipe_parameter_value(editor, "side"),
                false,
            );
            replace_literal_length(outer_diameter, driven)?;
            replace_literal_angle(rotation, recipe_parameter_value(editor, "rotation"))?;
        }
        CoreRecipe::Text {
            content,
            height,
            angle,
            ..
        } => {
            if let Some(text) = recipe_parameter_text(editor, "content") {
                *content = text;
            }
            replace_literal_length(height, recipe_parameter_value(editor, "height"))?;
            replace_literal_angle(angle, recipe_parameter_value(editor, "angle"))?;
        }
        CoreRecipe::TwoPointSlot { width, .. } => {
            replace_literal_length(width, recipe_parameter_value(editor, "width"))?;
        }
        CoreRecipe::CentreOuterPointSlot {
            overall_length,
            width,
            angle,
            ..
        } => {
            replace_literal_length(
                overall_length,
                recipe_parameter_value(editor, "overall_length"),
            )?;
            replace_literal_length(width, recipe_parameter_value(editor, "width"))?;
            replace_literal_angle(angle, recipe_parameter_value(editor, "angle"))?;
        }
        CoreRecipe::RectangularPattern {
            columns,
            rows,
            column_spacing,
            row_spacing,
            direction,
            ..
        } => {
            replace_literal_integer(columns, recipe_parameter_value(editor, "columns"));
            replace_literal_integer(rows, recipe_parameter_value(editor, "rows"));
            replace_literal_signed_length(
                column_spacing,
                recipe_parameter_value(editor, "column_spacing"),
            )?;
            replace_literal_signed_length(
                row_spacing,
                recipe_parameter_value(editor, "row_spacing"),
            )?;
            replace_literal_angle(direction, recipe_parameter_value(editor, "direction"))?;
        }
        CoreRecipe::CircularPattern {
            count, total_angle, ..
        } => {
            replace_literal_integer(count, recipe_parameter_value(editor, "count"));
            replace_literal_angle(total_angle, recipe_parameter_value(editor, "total_angle"))?;
        }
        CoreRecipe::Offset { distance, .. } => {
            replace_literal_signed_length(distance, recipe_parameter_value(editor, "distance"))?;
        }
        CoreRecipe::Fillet { radius, .. } | CoreRecipe::FilletWithHints { radius, .. } => {
            replace_literal_length(radius, recipe_parameter_value(editor, "radius"))?;
        }
        CoreRecipe::Chamfer {
            first_distance,
            second_distance,
            ..
        } => {
            if let Some(distance) = recipe_parameter_value(editor, "distance") {
                replace_literal_length(first_distance, Some(distance))?;
                replace_literal_length(second_distance, Some(distance))?;
            } else {
                replace_literal_length(
                    first_distance,
                    recipe_parameter_value(editor, "first_distance"),
                )?;
                replace_literal_length(
                    second_distance,
                    recipe_parameter_value(editor, "second_distance"),
                )?;
            }
        }
        CoreRecipe::LegacyImportedProfile { .. }
        | CoreRecipe::Point { .. }
        | CoreRecipe::Polyline { .. }
        | CoreRecipe::TwoPointCircle { .. }
        | CoreRecipe::CentreStartEndArc { .. }
        | CoreRecipe::FitPointSpline { .. }
        | CoreRecipe::ControlVertexSpline { .. }
        | CoreRecipe::Trim { .. } => {}
    }
    Ok(recipe)
}

fn translate_core_point_input(input: &mut CorePointInput, delta_u: f64, delta_v: f64) {
    if let CorePointInput::Position(pos) = input {
        *pos = CorePoint2::new(pos.u + delta_u, pos.v + delta_v);
    }
}

fn translate_core_recipe(recipe: &mut CoreRecipe, delta_u: f64, delta_v: f64) {
    match recipe {
        CoreRecipe::Point { position } => {
            *position = CorePoint2::new(position.u + delta_u, position.v + delta_v);
        }
        CoreRecipe::Line { start, end } | CoreRecipe::CentreLine { start, end } => {
            translate_core_point_input(start, delta_u, delta_v);
            translate_core_point_input(end, delta_u, delta_v);
        }
        CoreRecipe::Polyline { vertices, .. } => {
            for vertex in vertices {
                translate_core_point_input(vertex, delta_u, delta_v);
            }
        }
        CoreRecipe::TwoPointRectangle { first_corner, .. } => {
            translate_core_point_input(first_corner, delta_u, delta_v);
        }
        CoreRecipe::CentrePointRectangle { center, .. } => {
            translate_core_point_input(center, delta_u, delta_v);
        }
        CoreRecipe::CentrePointCircle { center, .. } => {
            translate_core_point_input(center, delta_u, delta_v);
        }
        CoreRecipe::TwoPointCircle {
            first_diameter_point,
            second_diameter_point,
            ..
        } => {
            translate_core_point_input(first_diameter_point, delta_u, delta_v);
            translate_core_point_input(second_diameter_point, delta_u, delta_v);
        }
        CoreRecipe::CentreStartEndArc {
            center, start, end, ..
        } => {
            translate_core_point_input(center, delta_u, delta_v);
            translate_core_point_input(start, delta_u, delta_v);
            translate_core_point_input(end, delta_u, delta_v);
        }
        CoreRecipe::InnerDiameterPolygon { center, .. }
        | CoreRecipe::OuterDiameterPolygon { center, .. } => {
            translate_core_point_input(center, delta_u, delta_v);
        }
        CoreRecipe::Text { anchor, .. } => {
            translate_core_point_input(anchor, delta_u, delta_v);
        }
        CoreRecipe::TwoPointSlot {
            first_cap_center,
            second_cap_center,
            ..
        } => {
            translate_core_point_input(first_cap_center, delta_u, delta_v);
            translate_core_point_input(second_cap_center, delta_u, delta_v);
        }
        _ => {}
    }
}

fn reshape_core_recipe(
    recipe: &mut CoreRecipe,
    handle: SketchDragHandle,
    delta_u: f64,
    delta_v: f64,
) {
    match (recipe, handle) {
        (recipe, SketchDragHandle::Translate) => {
            translate_core_recipe(recipe, delta_u, delta_v);
        }
        (
            CoreRecipe::Line { start, .. } | CoreRecipe::CentreLine { start, .. },
            SketchDragHandle::StartPoint,
        ) => {
            translate_core_point_input(start, delta_u, delta_v);
        }
        (
            CoreRecipe::Line { end, .. } | CoreRecipe::CentreLine { end, .. },
            SketchDragHandle::EndPoint,
        ) => {
            translate_core_point_input(end, delta_u, delta_v);
        }
        (
            CoreRecipe::TwoPointRectangle {
                first_corner,
                width,
                height,
            },
            SketchDragHandle::RectangleCorner(corner_idx),
        ) => {
            let current_w = literal_signed_length_value(width).unwrap_or(0.0);
            let current_h = literal_signed_length_value(height).unwrap_or(0.0);
            let first = match first_corner {
                CorePointInput::Position(pos) => *pos,
                _ => return,
            };
            let opp = CorePoint2::new(first.u + current_w, first.v + current_h);
            let min_u = first.u.min(opp.u);
            let max_u = first.u.max(opp.u);
            let min_v = first.v.min(opp.v);
            let max_v = first.v.max(opp.v);
            let corners = [
                CorePoint2::new(min_u, min_v),
                CorePoint2::new(max_u, min_v),
                CorePoint2::new(max_u, max_v),
                CorePoint2::new(min_u, max_v),
            ];
            let opp_idx = (corner_idx + 2) % 4;
            let fixed_c = corners[opp_idx];
            let moving_c = corners[corner_idx];
            let new_c = CorePoint2::new(moving_c.u + delta_u, moving_c.v + delta_v);
            *first_corner = CorePointInput::Position(fixed_c);
            let _ = replace_literal_signed_length(width, Some(new_c.u - fixed_c.u));
            let _ = replace_literal_signed_length(height, Some(new_c.v - fixed_c.v));
        }
        (
            CoreRecipe::TwoPointRectangle {
                first_corner,
                width,
                height,
            },
            SketchDragHandle::RectangleSide(side_idx),
        ) => {
            let current_w = literal_signed_length_value(width).unwrap_or(0.0);
            let current_h = literal_signed_length_value(height).unwrap_or(0.0);
            let first = match first_corner {
                CorePointInput::Position(pos) => *pos,
                _ => return,
            };
            let opp = CorePoint2::new(first.u + current_w, first.v + current_h);
            let min_u = first.u.min(opp.u);
            let max_u = first.u.max(opp.u);
            let min_v = first.v.min(opp.v);
            let max_v = first.v.max(opp.v);
            match side_idx {
                0 => {
                    *first_corner = CorePointInput::Position(CorePoint2::new(min_u, max_v));
                    let _ = replace_literal_signed_length(width, Some(max_u - min_u));
                    let _ = replace_literal_signed_length(height, Some((min_v + delta_v) - max_v));
                }
                1 => {
                    *first_corner = CorePointInput::Position(CorePoint2::new(min_u, min_v));
                    let _ = replace_literal_signed_length(width, Some((max_u + delta_u) - min_u));
                    let _ = replace_literal_signed_length(height, Some(max_v - min_v));
                }
                2 => {
                    *first_corner = CorePointInput::Position(CorePoint2::new(min_u, min_v));
                    let _ = replace_literal_signed_length(width, Some(max_u - min_u));
                    let _ = replace_literal_signed_length(height, Some((max_v + delta_v) - min_v));
                }
                3 => {
                    *first_corner = CorePointInput::Position(CorePoint2::new(max_u, min_v));
                    let _ = replace_literal_signed_length(width, Some((min_u + delta_u) - max_u));
                    let _ = replace_literal_signed_length(height, Some(max_v - min_v));
                }
                _ => {}
            }
        }
        (
            CoreRecipe::CentrePointRectangle { width, height, .. },
            SketchDragHandle::RectangleCorner(_),
        ) => {
            let current_w = literal_length_value(width).unwrap_or(1.0);
            let current_h = literal_length_value(height).unwrap_or(1.0);
            let new_w = delta_u.abs().mul_add(2.0, current_w).max(0.01);
            let new_h = delta_v.abs().mul_add(2.0, current_h).max(0.01);
            let _ = replace_literal_length(width, Some(new_w));
            let _ = replace_literal_length(height, Some(new_h));
        }
        (
            CoreRecipe::CentrePointRectangle { width, height, .. },
            SketchDragHandle::RectangleSide(side_idx),
        ) => {
            let current_w = literal_length_value(width).unwrap_or(1.0);
            let current_h = literal_length_value(height).unwrap_or(1.0);
            if side_idx == 1 || side_idx == 3 {
                let new_w = delta_u.abs().mul_add(2.0, current_w).max(0.01);
                let _ = replace_literal_length(width, Some(new_w));
            } else {
                let new_h = delta_v.abs().mul_add(2.0, current_h).max(0.01);
                let _ = replace_literal_length(height, Some(new_h));
            }
        }
        (CoreRecipe::CentrePointCircle { radius, .. }, SketchDragHandle::CircleRim) => {
            let current_r = literal_length_value(radius).unwrap_or(1.0);
            let delta = delta_u.hypot(delta_v) * if delta_u + delta_v >= 0.0 { 1.0 } else { -1.0 };
            let new_r = (current_r + delta).max(0.01);
            let _ = replace_literal_length(radius, Some(new_r));
        }
        (CoreRecipe::CentreStartEndArc { start, .. }, SketchDragHandle::StartPoint) => {
            translate_core_point_input(start, delta_u, delta_v);
        }
        (CoreRecipe::CentreStartEndArc { end, .. }, SketchDragHandle::EndPoint) => {
            translate_core_point_input(end, delta_u, delta_v);
        }
        (
            CoreRecipe::TwoPointSlot {
                first_cap_center, ..
            },
            SketchDragHandle::StartPoint,
        ) => {
            translate_core_point_input(first_cap_center, delta_u, delta_v);
        }
        (
            CoreRecipe::TwoPointSlot {
                second_cap_center, ..
            },
            SketchDragHandle::EndPoint,
        ) => {
            translate_core_point_input(second_cap_center, delta_u, delta_v);
        }
        (
            CoreRecipe::TwoPointSlot { width, .. },
            SketchDragHandle::CircleRim | SketchDragHandle::RectangleSide(_),
        ) => {
            let current_w = literal_length_value(width).unwrap_or(1.0);
            let new_w = delta_u.hypot(delta_v).mul_add(2.0, current_w).max(0.01);
            let _ = replace_literal_length(width, Some(new_w));
        }
        (recipe, _) => {
            translate_core_recipe(recipe, delta_u, delta_v);
        }
    }
}

/// Keyboard ownership returned to the workbench-wide shortcut arbiter.
///
/// `egui` consumes filtered key events, while the workbench deliberately reads
/// raw Enter/Escape events for its universal operation gate. These claims are
/// therefore explicit rather than relying on widget focus as an implicit side
/// channel.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DimensionKeyClaims {
    pub enter: bool,
    pub escape: bool,
    pub confirmation_blocked: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DimensionTarget {
    Draft,
    Pending(SketchEntityId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DimensionPhase {
    Point,
    Line,
    Rectangle,
    CentreRectangle,
    Circle,
    TwoPointCircle,
    SlotWidth,
    ArcRadius,
    ArcSweep,
    /// Two fixed endpoints plus one editable directed sweep. Radius is
    /// necessarily derived from that sweep and the endpoint chord.
    ThreePointArc,
}

impl DimensionPhase {
    const fn can_stage(self) -> bool {
        !matches!(self, Self::ArcRadius)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DimensionField {
    readout: DimensionReadout,
}

impl DimensionField {
    const fn editable(kind: SketchDimensionKind, value: f64) -> Self {
        Self {
            readout: DimensionReadout {
                kind,
                value,
                locked: false,
                editable: true,
            },
        }
    }

    const fn derived(kind: SketchDimensionKind, value: f64) -> Self {
        Self {
            readout: DimensionReadout {
                kind,
                value,
                locked: false,
                editable: false,
            },
        }
    }
}

#[derive(Clone, Debug)]
struct DimensionEditOriginal {
    geometry: SketchGeometry,
    fields: Vec<DimensionField>,
    three_point_arc: Option<ThreePointArcConstraint>,
}

#[derive(Clone, Copy, Debug)]
struct ThreePointArcConstraint {
    first: SketchPoint,
    second: SketchPoint,
    direction: CoreCurveDirection,
}

#[derive(Clone, Debug)]
struct DimensionSession {
    target: DimensionTarget,
    phase: DimensionPhase,
    geometry: SketchGeometry,
    fields: Vec<DimensionField>,
    active: Option<usize>,
    buffer: String,
    edit_original: Option<DimensionEditOriginal>,
    error: Option<DimensionInputError>,
    focus_next_frame: bool,
    serial: u64,
    three_point_arc: Option<ThreePointArcConstraint>,
}

impl DimensionSession {
    fn from_geometry(target: DimensionTarget, geometry: SketchGeometry, serial: u64) -> Self {
        let phase = match geometry {
            SketchGeometry::Point(_) => DimensionPhase::Point,
            SketchGeometry::Segment { .. } => DimensionPhase::Line,
            SketchGeometry::Rectangle { .. } => DimensionPhase::Rectangle,
            SketchGeometry::Circle { .. } => DimensionPhase::Circle,
            SketchGeometry::Arc { .. } => DimensionPhase::ArcSweep,
        };
        Self::new(target, phase, geometry, serial)
    }

    fn arc_radius(center: SketchPoint, serial: u64) -> Self {
        Self::new(
            DimensionTarget::Draft,
            DimensionPhase::ArcRadius,
            SketchGeometry::segment(center, center),
            serial,
        )
    }

    fn three_point_arc(
        first: SketchPoint,
        second: SketchPoint,
        through: SketchPoint,
        serial: u64,
    ) -> Option<Self> {
        let solution = three_point_arc_solution(first, second, through)?;
        let geometry = three_point_arc_geometry(first, second, solution);
        let mut session = Self::new(
            DimensionTarget::Draft,
            DimensionPhase::ThreePointArc,
            geometry,
            serial,
        );
        session.three_point_arc = Some(ThreePointArcConstraint {
            first,
            second,
            direction: solution.direction,
        });
        Some(session)
    }

    fn new(
        target: DimensionTarget,
        phase: DimensionPhase,
        geometry: SketchGeometry,
        serial: u64,
    ) -> Self {
        let fields = dimension_fields_for_geometry(phase, geometry);
        Self {
            target,
            phase,
            geometry,
            fields,
            active: None,
            buffer: String::new(),
            edit_original: None,
            error: None,
            focus_next_frame: false,
            serial,
            three_point_arc: None,
        }
    }

    fn readouts(&self) -> impl Iterator<Item = DimensionReadout> + '_ {
        self.fields.iter().map(|field| field.readout)
    }

    fn active_kind(&self) -> Option<SketchDimensionKind> {
        self.active
            .and_then(|index| self.fields.get(index))
            .map(|field| field.readout.kind)
    }

    fn field_index(&self, kind: SketchDimensionKind) -> Option<usize> {
        self.fields
            .iter()
            .position(|field| field.readout.kind == kind)
    }

    fn value(&self, kind: SketchDimensionKind) -> f64 {
        self.field_index(kind)
            .and_then(|index| self.fields.get(index))
            .map_or(0.0, |field| field.readout.value)
    }

    fn follows_pointer(&self, kind: SketchDimensionKind) -> bool {
        self.field_index(kind)
            .is_some_and(|index| !self.fields[index].readout.locked && self.active != Some(index))
    }

    fn set_pointer_value(&mut self, kind: SketchDimensionKind, value: f64) {
        if self.follows_pointer(kind)
            && let Some(index) = self.field_index(kind)
        {
            self.fields[index].readout.value = normalized_zero(value);
        }
    }

    fn update_pointer(&mut self, point: SketchPoint) {
        match (self.phase, self.geometry) {
            (DimensionPhase::Point, _) => {}
            (DimensionPhase::Line, SketchGeometry::Segment { start, end: _ }) => {
                let raw_du = point.u - start.u;
                let raw_dv = point.v - start.v;
                let raw_length = raw_du.hypot(raw_dv);
                let length_follows_pointer = self.follows_pointer(SketchDimensionKind::Length);
                let angle_follows_pointer = self.follows_pointer(SketchDimensionKind::AngleDegrees);
                self.set_pointer_value(SketchDimensionKind::Length, raw_length);
                if raw_length > MIN_ENTITY_LENGTH {
                    self.set_pointer_value(
                        SketchDimensionKind::AngleDegrees,
                        raw_dv.atan2(raw_du).to_degrees(),
                    );
                }
                let length = self.value(SketchDimensionKind::Length);
                let angle = self.value(SketchDimensionKind::AngleDegrees).to_radians();
                let end = if length_follows_pointer && angle_follows_pointer {
                    point
                } else if length > MIN_ENTITY_LENGTH {
                    SketchPoint::new(
                        length.mul_add(angle.cos(), start.u),
                        length.mul_add(angle.sin(), start.v),
                    )
                } else {
                    start
                };
                self.geometry = SketchGeometry::segment(start, end);
            }
            (DimensionPhase::Rectangle, SketchGeometry::Rectangle { first, opposite }) => {
                let raw_du = point.u - first.u;
                let raw_dv = point.v - first.v;
                self.set_pointer_value(SketchDimensionKind::Width, raw_du.abs());
                self.set_pointer_value(SketchDimensionKind::Height, raw_dv.abs());
                let old_du = opposite.u - first.u;
                let old_dv = opposite.v - first.v;
                let sign_u =
                    nonzero_sign(raw_du).unwrap_or_else(|| nonzero_sign(old_du).unwrap_or(1.0));
                let sign_v =
                    nonzero_sign(raw_dv).unwrap_or_else(|| nonzero_sign(old_dv).unwrap_or(1.0));
                self.geometry = SketchGeometry::rectangle(
                    first,
                    SketchPoint::new(
                        sign_u.mul_add(self.value(SketchDimensionKind::Width), first.u),
                        sign_v.mul_add(self.value(SketchDimensionKind::Height), first.v),
                    ),
                );
            }
            (DimensionPhase::CentreRectangle, SketchGeometry::Rectangle { first, opposite }) => {
                let center =
                    SketchPoint::new((first.u + opposite.u) * 0.5, (first.v + opposite.v) * 0.5);
                let raw_du = point.u - center.u;
                let raw_dv = point.v - center.v;
                self.set_pointer_value(SketchDimensionKind::Width, raw_du.abs() * 2.0);
                self.set_pointer_value(SketchDimensionKind::Height, raw_dv.abs() * 2.0);
                let old_du = opposite.u - center.u;
                let old_dv = opposite.v - center.v;
                let sign_u =
                    nonzero_sign(raw_du).unwrap_or_else(|| nonzero_sign(old_du).unwrap_or(1.0));
                let sign_v =
                    nonzero_sign(raw_dv).unwrap_or_else(|| nonzero_sign(old_dv).unwrap_or(1.0));
                let half_width = self.value(SketchDimensionKind::Width) * 0.5;
                let half_height = self.value(SketchDimensionKind::Height) * 0.5;
                self.geometry = SketchGeometry::rectangle(
                    SketchPoint::new(
                        (-sign_u).mul_add(half_width, center.u),
                        (-sign_v).mul_add(half_height, center.v),
                    ),
                    SketchPoint::new(
                        sign_u.mul_add(half_width, center.u),
                        sign_v.mul_add(half_height, center.v),
                    ),
                );
            }
            (DimensionPhase::Circle, SketchGeometry::Circle { center, rim }) => {
                let raw_radius = center.distance_squared(point).sqrt();
                let diameter_locked = self
                    .field_index(SketchDimensionKind::Diameter)
                    .is_some_and(|index| self.fields[index].readout.locked);
                self.set_pointer_value(SketchDimensionKind::Diameter, raw_radius * 2.0);
                let direction = unit_direction(center, point)
                    .or_else(|| unit_direction(center, rim))
                    .unwrap_or((1.0, 0.0));
                let radius = self.value(SketchDimensionKind::Diameter) * 0.5;
                self.geometry = SketchGeometry::circle(
                    center,
                    if diameter_locked {
                        SketchPoint::new(
                            radius.mul_add(direction.0, center.u),
                            radius.mul_add(direction.1, center.v),
                        )
                    } else {
                        point
                    },
                );
            }
            (DimensionPhase::TwoPointCircle, SketchGeometry::Circle { center, rim }) => {
                let first =
                    SketchPoint::new(center.u.mul_add(2.0, -rim.u), center.v.mul_add(2.0, -rim.v));
                let raw_diameter = first.distance_squared(point).sqrt();
                let diameter_locked = self
                    .field_index(SketchDimensionKind::Diameter)
                    .is_some_and(|index| self.fields[index].readout.locked);
                self.set_pointer_value(SketchDimensionKind::Diameter, raw_diameter);
                let direction = unit_direction(first, point)
                    .or_else(|| unit_direction(first, rim))
                    .unwrap_or((1.0, 0.0));
                let diameter = self.value(SketchDimensionKind::Diameter);
                let second = if diameter_locked {
                    SketchPoint::new(
                        diameter.mul_add(direction.0, first.u),
                        diameter.mul_add(direction.1, first.v),
                    )
                } else {
                    point
                };
                self.geometry = SketchGeometry::circle(midpoint(first, second), second);
            }
            (DimensionPhase::ArcRadius, SketchGeometry::Segment { start: center, end }) => {
                let raw_radius = center.distance_squared(point).sqrt();
                let radius_locked = self
                    .field_index(SketchDimensionKind::Radius)
                    .is_some_and(|index| self.fields[index].readout.locked);
                self.set_pointer_value(SketchDimensionKind::Radius, raw_radius);
                let direction = unit_direction(center, point)
                    .or_else(|| unit_direction(center, end))
                    .unwrap_or((1.0, 0.0));
                let radius = self.value(SketchDimensionKind::Radius);
                self.geometry = SketchGeometry::segment(
                    center,
                    if radius_locked {
                        SketchPoint::new(
                            radius.mul_add(direction.0, center.u),
                            radius.mul_add(direction.1, center.v),
                        )
                    } else {
                        point
                    },
                );
            }
            (
                DimensionPhase::ArcSweep,
                SketchGeometry::Arc {
                    center,
                    start,
                    end: _,
                },
            ) => {
                let raw_sweep = sweep_degrees_toward(center, start, point);
                let sweep_locked = self
                    .field_index(SketchDimensionKind::SweepDegrees)
                    .is_some_and(|index| self.fields[index].readout.locked);
                let radius_locked = self
                    .field_index(SketchDimensionKind::Radius)
                    .is_some_and(|index| self.fields[index].readout.locked);
                self.set_pointer_value(SketchDimensionKind::SweepDegrees, raw_sweep);
                let radius = self.value(SketchDimensionKind::Radius);
                let start_direction = unit_direction(center, start).unwrap_or((1.0, 0.0));
                let canonical_start = if radius_locked {
                    SketchPoint::new(
                        radius.mul_add(start_direction.0, center.u),
                        radius.mul_add(start_direction.1, center.v),
                    )
                } else {
                    start
                };
                let sweep = self.value(SketchDimensionKind::SweepDegrees).to_radians();
                let end_direction = rotate_direction(start_direction, sweep);
                let canonical_end = if sweep > 0.0 && !sweep_locked {
                    arc_endpoint(center, canonical_start, point)
                } else if sweep > 0.0 {
                    SketchPoint::new(
                        radius.mul_add(end_direction.0, center.u),
                        radius.mul_add(end_direction.1, center.v),
                    )
                } else {
                    canonical_start
                };
                self.geometry = SketchGeometry::arc(center, canonical_start, canonical_end);
            }
            (DimensionPhase::ThreePointArc, _)
                if self.follows_pointer(SketchDimensionKind::SweepDegrees) =>
            {
                let Some(mut constraint) = self.three_point_arc else {
                    return;
                };
                if let Some(solution) =
                    three_point_arc_solution(constraint.first, constraint.second, point)
                {
                    constraint.direction = solution.direction;
                    self.three_point_arc = Some(constraint);
                    self.geometry =
                        three_point_arc_geometry(constraint.first, constraint.second, solution);
                    let sweep = directed_three_point_arc_sweep(
                        constraint.first,
                        constraint.second,
                        solution,
                    )
                    .to_degrees();
                    self.set_pointer_value(SketchDimensionKind::SweepDegrees, sweep);
                    self.error = None;
                } else {
                    self.error = Some(DimensionInputError::DegenerateGeometry);
                }
            }
            _ => {}
        }
        self.refresh_derived_values();
    }

    fn begin_edit(&mut self, index: usize) -> bool {
        let Some(field) = self.fields.get(index) else {
            return false;
        };
        if !field.readout.editable {
            return false;
        }
        self.active = Some(index);
        self.buffer = format_input_value(field.readout.value);
        self.edit_original = Some(DimensionEditOriginal {
            geometry: self.geometry,
            fields: self.fields.clone(),
            three_point_arc: self.three_point_arc,
        });
        self.error = None;
        self.focus_next_frame = true;
        true
    }

    fn begin_kind(&mut self, kind: SketchDimensionKind) -> bool {
        self.field_index(kind)
            .is_some_and(|index| self.begin_edit(index))
    }

    fn begin_first_editable(&mut self, backwards: bool) -> bool {
        let next = if backwards {
            self.fields.iter().rposition(|field| field.readout.editable)
        } else {
            self.fields.iter().position(|field| field.readout.editable)
        };
        next.is_some_and(|index| self.begin_edit(index))
    }

    fn cycle(
        &mut self,
        backwards: bool,
        names: &BTreeMap<String, f64>,
    ) -> Result<(), DimensionInputError> {
        let Some(active) = self.active else {
            self.begin_first_editable(backwards);
            return Ok(());
        };
        self.accept(names)?;
        let len = self.fields.len();
        for offset in 1..=len {
            let index = if backwards {
                (active + len - (offset % len)) % len
            } else {
                (active + offset) % len
            };
            if self.fields[index].readout.editable {
                self.begin_edit(index);
                break;
            }
        }
        Ok(())
    }

    fn apply_buffer_live(
        &mut self,
        names: &BTreeMap<String, f64>,
    ) -> Result<(), DimensionInputError> {
        let Some(active) = self.active else {
            return Ok(());
        };
        let kind = self.fields[active].readout.kind;
        let value = parse_dimension_value(kind, &self.buffer, names)?;
        let previous_geometry = self.geometry;
        let previous_fields = self.fields.clone();
        if let Err(error) = self.apply_value(active, value) {
            self.geometry = previous_geometry;
            self.fields = previous_fields;
            return Err(error);
        }
        self.error = None;
        Ok(())
    }

    fn accept(&mut self, names: &BTreeMap<String, f64>) -> Result<(), DimensionInputError> {
        self.apply_buffer_live(names)?;
        self.active = None;
        self.edit_original = None;
        self.error = None;
        self.focus_next_frame = false;
        Ok(())
    }

    fn cancel_edit(&mut self) -> bool {
        let Some(original) = self.edit_original.take() else {
            return false;
        };
        self.geometry = original.geometry;
        self.fields = original.fields;
        self.three_point_arc = original.three_point_arc;
        self.active = None;
        self.buffer.clear();
        self.error = None;
        self.focus_next_frame = false;
        true
    }

    fn apply_value(&mut self, active: usize, mut value: f64) -> Result<(), DimensionInputError> {
        let kind = self.fields[active].readout.kind;
        if kind == SketchDimensionKind::AngleDegrees {
            value = normalized_angle_degrees(value);
        }
        self.fields[active].readout.value = normalized_zero(value);
        self.fields[active].readout.locked = true;
        self.rebuild_geometry();
        if !self.geometry.is_finite() || !geometry_coordinates_supported(self.geometry) {
            return Err(DimensionInputError::CoordinateOutOfRange);
        }
        self.refresh_derived_values();
        Ok(())
    }

    fn rebuild_geometry(&mut self) {
        if self.phase == DimensionPhase::ThreePointArc {
            if let Some(constraint) = self.three_point_arc
                && let Some(geometry) = three_point_arc_geometry_for_sweep(
                    constraint,
                    self.value(SketchDimensionKind::SweepDegrees).to_radians(),
                )
            {
                self.geometry = geometry;
            }
            return;
        }
        self.geometry = match self.geometry {
            SketchGeometry::Point(_) => SketchGeometry::point(SketchPoint::new(
                self.value(SketchDimensionKind::U),
                self.value(SketchDimensionKind::V),
            )),
            SketchGeometry::Segment { start, end: _ } if self.phase == DimensionPhase::Line => {
                let length = self.value(SketchDimensionKind::Length);
                let angle = self.value(SketchDimensionKind::AngleDegrees).to_radians();
                SketchGeometry::segment(
                    start,
                    SketchPoint::new(
                        length.mul_add(angle.cos(), start.u),
                        length.mul_add(angle.sin(), start.v),
                    ),
                )
            }
            SketchGeometry::Segment { start, end } if self.phase == DimensionPhase::SlotWidth => {
                let direction = unit_direction(start, end).unwrap_or((0.0, 1.0));
                let half_width = self.value(SketchDimensionKind::Width) * 0.5;
                SketchGeometry::segment(
                    start,
                    SketchPoint::new(
                        half_width.mul_add(direction.0, start.u),
                        half_width.mul_add(direction.1, start.v),
                    ),
                )
            }
            SketchGeometry::Segment { start, end } => {
                let radius = self.value(SketchDimensionKind::Radius);
                let direction = unit_direction(start, end).unwrap_or((1.0, 0.0));
                SketchGeometry::segment(
                    start,
                    SketchPoint::new(
                        radius.mul_add(direction.0, start.u),
                        radius.mul_add(direction.1, start.v),
                    ),
                )
            }
            SketchGeometry::Rectangle { first, opposite }
                if self.phase == DimensionPhase::CentreRectangle =>
            {
                let center = midpoint(first, opposite);
                let half_width = self.value(SketchDimensionKind::Width) * 0.5;
                let half_height = self.value(SketchDimensionKind::Height) * 0.5;
                let sign_u = nonzero_sign(opposite.u - center.u).unwrap_or(1.0);
                let sign_v = nonzero_sign(opposite.v - center.v).unwrap_or(1.0);
                SketchGeometry::rectangle(
                    SketchPoint::new(
                        (-sign_u).mul_add(half_width, center.u),
                        (-sign_v).mul_add(half_height, center.v),
                    ),
                    SketchPoint::new(
                        sign_u.mul_add(half_width, center.u),
                        sign_v.mul_add(half_height, center.v),
                    ),
                )
            }
            SketchGeometry::Rectangle { first, opposite } => {
                let sign_u = nonzero_sign(opposite.u - first.u).unwrap_or(1.0);
                let sign_v = nonzero_sign(opposite.v - first.v).unwrap_or(1.0);
                SketchGeometry::rectangle(
                    first,
                    SketchPoint::new(
                        sign_u.mul_add(self.value(SketchDimensionKind::Width), first.u),
                        sign_v.mul_add(self.value(SketchDimensionKind::Height), first.v),
                    ),
                )
            }
            SketchGeometry::Circle { center, rim }
                if self.phase == DimensionPhase::TwoPointCircle =>
            {
                let first =
                    SketchPoint::new(center.u.mul_add(2.0, -rim.u), center.v.mul_add(2.0, -rim.v));
                let diameter = self.value(SketchDimensionKind::Diameter);
                let direction = unit_direction(first, rim).unwrap_or((1.0, 0.0));
                let second = SketchPoint::new(
                    diameter.mul_add(direction.0, first.u),
                    diameter.mul_add(direction.1, first.v),
                );
                SketchGeometry::circle(midpoint(first, second), second)
            }
            SketchGeometry::Circle { center, rim } => {
                let radius = self.value(SketchDimensionKind::Diameter) * 0.5;
                let direction = unit_direction(center, rim).unwrap_or((1.0, 0.0));
                SketchGeometry::circle(
                    center,
                    SketchPoint::new(
                        radius.mul_add(direction.0, center.u),
                        radius.mul_add(direction.1, center.v),
                    ),
                )
            }
            SketchGeometry::Arc { center, start, .. } => {
                let radius = self.value(SketchDimensionKind::Radius);
                let sweep = self.value(SketchDimensionKind::SweepDegrees).to_radians();
                let start_direction = unit_direction(center, start).unwrap_or((1.0, 0.0));
                let canonical_start = SketchPoint::new(
                    radius.mul_add(start_direction.0, center.u),
                    radius.mul_add(start_direction.1, center.v),
                );
                let end_direction = rotate_direction(start_direction, sweep);
                SketchGeometry::arc(
                    center,
                    canonical_start,
                    SketchPoint::new(
                        radius.mul_add(end_direction.0, center.u),
                        radius.mul_add(end_direction.1, center.v),
                    ),
                )
            }
        };
    }

    fn refresh_derived_values(&mut self) {
        if self.phase == DimensionPhase::ThreePointArc {
            if let SketchGeometry::Arc { center, start, .. } = self.geometry
                && let Some(index) = self.field_index(SketchDimensionKind::Radius)
            {
                self.fields[index].readout.value = center.distance_squared(start).sqrt();
            }
            return;
        }
        let SketchGeometry::Segment { start, end } = self.geometry else {
            return;
        };
        if self.phase != DimensionPhase::Line {
            return;
        }
        let du = normalized_zero(end.u - start.u);
        let dv = normalized_zero(end.v - start.v);
        if let Some(index) = self.field_index(SketchDimensionKind::DeltaU) {
            self.fields[index].readout.value = du;
        }
        if let Some(index) = self.field_index(SketchDimensionKind::DeltaV) {
            self.fields[index].readout.value = dv;
        }
    }

    fn three_point_arc_recipe(&self) -> Option<CoreRecipe> {
        if self.phase != DimensionPhase::ThreePointArc || self.error.is_some() {
            return None;
        }
        let constraint = self.three_point_arc?;
        let SketchGeometry::Arc { center, .. } = self.geometry else {
            return None;
        };
        Some(CoreRecipe::CentreStartEndArc {
            center: core_point_input(center),
            start: core_point_input(constraint.first),
            end: core_point_input(constraint.second),
            direction: constraint.direction,
        })
    }
}

fn dimension_fields_for_geometry(
    phase: DimensionPhase,
    geometry: SketchGeometry,
) -> Vec<DimensionField> {
    match (phase, geometry) {
        (DimensionPhase::Point, SketchGeometry::Point(position)) => vec![
            DimensionField::editable(SketchDimensionKind::U, position.u),
            DimensionField::editable(SketchDimensionKind::V, position.v),
        ],
        (DimensionPhase::Line, SketchGeometry::Segment { start, end }) => {
            let du = normalized_zero(end.u - start.u);
            let dv = normalized_zero(end.v - start.v);
            vec![
                DimensionField::editable(SketchDimensionKind::Length, du.hypot(dv)),
                DimensionField::editable(
                    SketchDimensionKind::AngleDegrees,
                    normalized_angle_degrees(dv.atan2(du).to_degrees()),
                ),
                DimensionField::derived(SketchDimensionKind::DeltaU, du),
                DimensionField::derived(SketchDimensionKind::DeltaV, dv),
            ]
        }
        (
            DimensionPhase::Rectangle | DimensionPhase::CentreRectangle,
            SketchGeometry::Rectangle { first, opposite },
        ) => vec![
            DimensionField::editable(SketchDimensionKind::Width, (opposite.u - first.u).abs()),
            DimensionField::editable(SketchDimensionKind::Height, (opposite.v - first.v).abs()),
        ],
        (
            DimensionPhase::Circle | DimensionPhase::TwoPointCircle,
            SketchGeometry::Circle { center, rim },
        ) => {
            vec![DimensionField::editable(
                SketchDimensionKind::Diameter,
                center.distance_squared(rim).sqrt() * 2.0,
            )]
        }
        (DimensionPhase::ArcRadius, SketchGeometry::Segment { start, end }) => {
            vec![DimensionField::editable(
                SketchDimensionKind::Radius,
                start.distance_squared(end).sqrt(),
            )]
        }
        (DimensionPhase::ArcSweep, SketchGeometry::Arc { center, start, end }) => vec![
            DimensionField::editable(
                SketchDimensionKind::Radius,
                center.distance_squared(start).sqrt(),
            ),
            DimensionField::editable(
                SketchDimensionKind::SweepDegrees,
                arc_sweep(center, start, end).to_degrees(),
            ),
        ],
        (DimensionPhase::ThreePointArc, SketchGeometry::Arc { center, start, end }) => vec![
            DimensionField::derived(
                SketchDimensionKind::Radius,
                center.distance_squared(start).sqrt(),
            ),
            DimensionField::editable(
                SketchDimensionKind::SweepDegrees,
                arc_sweep(center, start, end).to_degrees(),
            ),
        ],
        (DimensionPhase::SlotWidth, SketchGeometry::Segment { start, end }) => {
            vec![DimensionField::editable(
                SketchDimensionKind::Width,
                start.distance_squared(end).sqrt() * 2.0,
            )]
        }
        _ => Vec::new(),
    }
}

fn dimension_phase_for_geometry(geometry: SketchGeometry) -> DimensionPhase {
    match geometry {
        SketchGeometry::Point(_) => DimensionPhase::Point,
        SketchGeometry::Segment { .. } => DimensionPhase::Line,
        SketchGeometry::Rectangle { .. } => DimensionPhase::Rectangle,
        SketchGeometry::Circle { .. } => DimensionPhase::Circle,
        SketchGeometry::Arc { .. } => DimensionPhase::ArcSweep,
    }
}

fn dimension_phase_accepts_geometry(phase: DimensionPhase, geometry: SketchGeometry) -> bool {
    matches!(
        (phase, geometry),
        (DimensionPhase::Point, SketchGeometry::Point(_))
            | (DimensionPhase::Line, SketchGeometry::Segment { .. })
            | (DimensionPhase::Rectangle, SketchGeometry::Rectangle { .. })
            | (
                DimensionPhase::CentreRectangle,
                SketchGeometry::Rectangle { .. }
            )
            | (DimensionPhase::Circle, SketchGeometry::Circle { .. })
            | (
                DimensionPhase::TwoPointCircle,
                SketchGeometry::Circle { .. }
            )
            | (DimensionPhase::SlotWidth, SketchGeometry::Segment { .. })
            | (DimensionPhase::ArcSweep, SketchGeometry::Arc { .. })
            | (DimensionPhase::ThreePointArc, SketchGeometry::Arc { .. })
    )
}

/// Evaluates a plain arithmetic entry over named document variables.
///
/// The grammar mirrors the parametric table's textual form minus units:
/// numbers, names, `+ - * /`, parentheses, unary minus. Everything here is a
/// bare magnitude — lengths in millimetres — because that is what dimension
/// fields hold. Returns `None` for anything that fails to parse or divide.
fn evaluate_named_expression(text: &str, names: &BTreeMap<String, f64>) -> Option<f64> {
    struct Evaluator<'entry> {
        tokens: Vec<NamedToken>,
        cursor: usize,
        names: &'entry BTreeMap<String, f64>,
    }
    #[derive(Clone, Debug, PartialEq)]
    enum NamedToken {
        Number(f64),
        Name(String),
        Plus,
        Minus,
        Star,
        Slash,
        Open,
        Close,
    }
    fn tokenize(text: &str) -> Option<Vec<NamedToken>> {
        let mut tokens = Vec::new();
        let mut characters = text.chars().peekable();
        while let Some(&character) = characters.peek() {
            match character {
                ' ' | '\t' => {
                    characters.next();
                }
                '+' => {
                    characters.next();
                    tokens.push(NamedToken::Plus);
                }
                '-' => {
                    characters.next();
                    tokens.push(NamedToken::Minus);
                }
                '*' => {
                    characters.next();
                    tokens.push(NamedToken::Star);
                }
                '/' => {
                    characters.next();
                    tokens.push(NamedToken::Slash);
                }
                '(' => {
                    characters.next();
                    tokens.push(NamedToken::Open);
                }
                ')' => {
                    characters.next();
                    tokens.push(NamedToken::Close);
                }
                '0'..='9' | '.' => {
                    let mut digits = String::new();
                    while let Some(&digit) = characters.peek() {
                        if digit.is_ascii_digit() || digit == '.' {
                            digits.push(digit);
                            characters.next();
                        } else {
                            break;
                        }
                    }
                    tokens.push(NamedToken::Number(digits.parse().ok()?));
                }
                letter if letter.is_alphabetic() || letter == '_' => {
                    let mut name = String::new();
                    while let Some(&piece) = characters.peek() {
                        if piece.is_alphanumeric() || piece == '_' {
                            name.push(piece);
                            characters.next();
                        } else {
                            break;
                        }
                    }
                    tokens.push(NamedToken::Name(name));
                }
                _ => return None,
            }
        }
        Some(tokens)
    }
    impl Evaluator<'_> {
        fn expression(&mut self) -> Option<f64> {
            let mut left = self.term()?;
            loop {
                match self.tokens.get(self.cursor) {
                    Some(NamedToken::Plus) => {
                        self.cursor += 1;
                        left += self.term()?;
                    }
                    Some(NamedToken::Minus) => {
                        self.cursor += 1;
                        left -= self.term()?;
                    }
                    _ => return Some(left),
                }
            }
        }
        fn term(&mut self) -> Option<f64> {
            let mut left = self.factor()?;
            loop {
                match self.tokens.get(self.cursor) {
                    Some(NamedToken::Star) => {
                        self.cursor += 1;
                        left *= self.factor()?;
                    }
                    Some(NamedToken::Slash) => {
                        self.cursor += 1;
                        let divisor = self.factor()?;
                        left /= divisor;
                    }
                    _ => return Some(left),
                }
            }
        }
        fn factor(&mut self) -> Option<f64> {
            let token = self.tokens.get(self.cursor)?.clone();
            self.cursor += 1;
            match token {
                NamedToken::Minus => Some(-self.factor()?),
                NamedToken::Plus => self.factor(),
                NamedToken::Number(value) => Some(value),
                NamedToken::Name(name) => self.names.get(&name).copied(),
                NamedToken::Open => {
                    let inner = self.expression()?;
                    match self.tokens.get(self.cursor) {
                        Some(NamedToken::Close) => {
                            self.cursor += 1;
                            Some(inner)
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        }
    }
    let tokens = tokenize(text)?;
    // A bare number is not this evaluator's business, and an entry with no
    // name in it gains nothing from it either; requiring a name keeps plain
    // typo'd numbers reporting "not a number" rather than evaluating oddly.
    if !tokens
        .iter()
        .any(|token| matches!(token, NamedToken::Name(_)))
    {
        return None;
    }
    let mut evaluator = Evaluator {
        tokens,
        cursor: 0,
        names,
    };
    let value = evaluator.expression()?;
    (evaluator.cursor == evaluator.tokens.len() && value.is_finite()).then_some(value)
}

fn parse_dimension_value(
    kind: SketchDimensionKind,
    text: &str,
    names: &BTreeMap<String, f64>,
) -> Result<f64, DimensionInputError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(DimensionInputError::Empty);
    }
    let value = text.parse::<f64>().map_or_else(
        |_| evaluate_named_expression(text, names).ok_or(DimensionInputError::NotANumber),
        Ok,
    )?;
    if !value.is_finite() {
        return Err(DimensionInputError::NonFinite);
    }
    if kind.is_length_magnitude() && value <= MIN_ENTITY_LENGTH {
        return Err(DimensionInputError::NonPositive);
    }
    if kind == SketchDimensionKind::SweepDegrees
        && !(MIN_ARC_SWEEP_DEGREES..=MAX_ARC_SWEEP_DEGREES).contains(&value)
    {
        return Err(DimensionInputError::SweepOutOfRange);
    }
    if matches!(kind, SketchDimensionKind::U | SketchDimensionKind::V)
        && value.abs() > MAX_ABS_SKETCH_COORDINATE
    {
        return Err(DimensionInputError::CoordinateOutOfRange);
    }
    Ok(value)
}

fn format_input_value(value: f64) -> String {
    let mut formatted = format!("{value:.6}");
    while formatted.contains('.') && formatted.ends_with('0') {
        formatted.pop();
    }
    if formatted.ends_with('.') {
        formatted.pop();
    }
    formatted
}

fn normalized_angle_degrees(value: f64) -> f64 {
    normalized_zero((value + 180.0).rem_euclid(360.0) - 180.0)
}

fn nonzero_sign(value: f64) -> Option<f64> {
    (value.abs() > MIN_ENTITY_LENGTH).then(|| value.signum())
}

fn unit_direction(start: SketchPoint, end: SketchPoint) -> Option<(f64, f64)> {
    let du = end.u - start.u;
    let dv = end.v - start.v;
    let length = du.hypot(dv);
    (length.is_finite() && length > MIN_ENTITY_LENGTH).then(|| (du / length, dv / length))
}

fn polyline_points_coincident(first: SketchPoint, second: SketchPoint) -> bool {
    let tolerance = PrecisionPolicy::default().min_feature_size;
    first.distance_squared(second) <= tolerance * tolerance
}

fn midpoint(first: SketchPoint, second: SketchPoint) -> SketchPoint {
    SketchPoint::new((first.u + second.u) * 0.5, (first.v + second.v) * 0.5)
}

fn rotate_direction(direction: (f64, f64), angle: f64) -> (f64, f64) {
    let (sin, cos) = angle.sin_cos();
    (
        direction.0.mul_add(cos, -direction.1 * sin),
        direction.0.mul_add(sin, direction.1 * cos),
    )
}

fn sweep_degrees_toward(center: SketchPoint, start: SketchPoint, point: SketchPoint) -> f64 {
    let start_angle = (start.v - center.v).atan2(start.u - center.u);
    let point_angle = (point.v - center.v).atan2(point.u - center.u);
    normalized_zero(
        (point_angle - start_angle)
            .rem_euclid(std::f64::consts::TAU)
            .to_degrees(),
    )
}

fn geometry_coordinates_supported(geometry: SketchGeometry) -> bool {
    geometry.control_points().iter().all(|point| {
        point.is_finite()
            && point.u.abs() <= MAX_ABS_SKETCH_COORDINATE
            && point.v.abs() <= MAX_ABS_SKETCH_COORDINATE
    })
}

#[derive(Clone, Copy, Debug)]
struct GeometryPoints {
    points: [SketchPoint; 9],
    len: usize,
}

impl GeometryPoints {
    const fn one(first: SketchPoint) -> Self {
        Self {
            points: [first; 9],
            len: 1,
        }
    }

    const fn two(first: SketchPoint, second: SketchPoint) -> Self {
        Self {
            points: [
                first, second, first, first, first, first, first, first, first,
            ],
            len: 2,
        }
    }

    const fn three(first: SketchPoint, second: SketchPoint, third: SketchPoint) -> Self {
        Self {
            points: [
                first, second, third, first, first, first, first, first, first,
            ],
            len: 3,
        }
    }

    #[allow(dead_code)]
    const fn four(points: [SketchPoint; 4]) -> Self {
        Self {
            points: [
                points[0], points[1], points[2], points[3], points[0], points[0], points[0],
                points[0], points[0],
            ],
            len: 4,
        }
    }

    #[allow(dead_code)]
    const fn five(
        p0: SketchPoint,
        p1: SketchPoint,
        p2: SketchPoint,
        p3: SketchPoint,
        p4: SketchPoint,
    ) -> Self {
        Self {
            points: [p0, p1, p2, p3, p4, p0, p0, p0, p0],
            len: 5,
        }
    }

    const fn nine(points: [SketchPoint; 9]) -> Self {
        Self { points, len: 9 }
    }

    fn iter(self) -> impl Iterator<Item = SketchPoint> {
        self.points.into_iter().take(self.len)
    }
}

/// One committed or pending sketch entity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SketchEntity {
    pub id: SketchEntityId,
    pub geometry: SketchGeometry,
    pub role: SketchEntityRole,
}

/// Whether an authored curve contributes material-region boundaries.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SketchEntityRole {
    /// Ordinary sketch geometry considered by exact profile compilation.
    #[default]
    Profile,
    /// Dashed layout geometry available to selection, snapping, and dimensions.
    Construction,
    /// Read-only projected/support geometry; never a material boundary.
    Reference,
}

fn sketch_insert_label(entity: SketchEntity) -> &'static str {
    if entity.role == SketchEntityRole::Construction {
        return "Add sketch construction geometry";
    }
    match entity.geometry {
        SketchGeometry::Point(_) => "Add sketch point",
        SketchGeometry::Segment { .. } => "Add sketch line",
        SketchGeometry::Rectangle { .. } => "Add sketch rectangle",
        SketchGeometry::Circle { .. } => "Add sketch circle",
        SketchGeometry::Arc { .. } => "Add sketch arc",
    }
}

const fn core_point(point: SketchPoint) -> CorePoint2 {
    CorePoint2::new(point.u, point.v)
}

fn core_recipe_for_entity(entity: SketchEntity) -> Option<CoreRecipe> {
    let point_input = |point| CorePointInput::Position(core_point(point));
    match entity.geometry {
        SketchGeometry::Point(position) => Some(CoreRecipe::Point {
            position: core_point(position),
        }),
        SketchGeometry::Segment { start, end } => {
            let start = point_input(start);
            let end = point_input(end);
            Some(if entity.role == SketchEntityRole::Construction {
                CoreRecipe::CentreLine { start, end }
            } else {
                CoreRecipe::Line { start, end }
            })
        }
        SketchGeometry::Rectangle { first, opposite } => Some(CoreRecipe::TwoPointRectangle {
            first_corner: point_input(first),
            width: CoreValue::Literal(CoreSignedLength::new(opposite.u - first.u).ok()?),
            height: CoreValue::Literal(CoreSignedLength::new(opposite.v - first.v).ok()?),
        }),
        SketchGeometry::Circle { center, rim } => {
            let radius = center.distance_squared(rim).sqrt();
            Some(CoreRecipe::CentrePointCircle {
                center: point_input(center),
                radius: CoreValue::Literal(CoreLength::new(radius).ok()?),
                radial_angle: CoreValue::Literal(
                    CoreAngle::radians((rim.v - center.v).atan2(rim.u - center.u)).ok()?,
                ),
            })
        }
        SketchGeometry::Arc { center, start, end } => Some(CoreRecipe::CentreStartEndArc {
            center: point_input(center),
            start: point_input(start),
            end: point_input(end),
            direction: CoreCurveDirection::CounterClockwise,
        }),
    }
}

fn core_point_input(point: SketchPoint) -> CorePointInput {
    CorePointInput::Position(core_point(point))
}

fn centre_point_rectangle_recipe(
    center: SketchPoint,
    width: f64,
    height: f64,
) -> Option<CoreRecipe> {
    Some(CoreRecipe::CentrePointRectangle {
        center: core_point_input(center),
        width: CoreValue::Literal(CoreLength::new(width).ok()?),
        height: CoreValue::Literal(CoreLength::new(height).ok()?),
    })
}

fn centre_point_rectangle_recipe_from_geometry(geometry: SketchGeometry) -> Option<CoreRecipe> {
    let SketchGeometry::Rectangle { first, opposite } = geometry else {
        return None;
    };
    centre_point_rectangle_recipe(
        midpoint(first, opposite),
        (opposite.u - first.u).abs(),
        (opposite.v - first.v).abs(),
    )
}

fn two_point_circle_recipe(first: SketchPoint, second: SketchPoint) -> CoreRecipe {
    CoreRecipe::TwoPointCircle {
        first_diameter_point: core_point_input(first),
        second_diameter_point: core_point_input(second),
        direction: CoreCurveDirection::CounterClockwise,
    }
}

fn two_point_circle_endpoints(geometry: SketchGeometry) -> Option<(SketchPoint, SketchPoint)> {
    let SketchGeometry::Circle { center, rim } = geometry else {
        return None;
    };
    Some((
        SketchPoint::new(center.u.mul_add(2.0, -rim.u), center.v.mul_add(2.0, -rim.v)),
        rim,
    ))
}

fn regular_polygon_recipe(
    variant: ToolVariant,
    center: SketchPoint,
    reference: SketchPoint,
    sides: u16,
) -> Option<CoreRecipe> {
    let radius = center.distance_squared(reference).sqrt();
    let direction = (reference.v - center.v).atan2(reference.u - center.u);
    let sides_value = CoreValue::Literal(CoreInteger::new(sides));
    Some(match variant {
        ToolVariant::InnerDiameterPolygon => CoreRecipe::InnerDiameterPolygon {
            center: core_point_input(center),
            inner_diameter: CoreValue::Literal(CoreLength::new(radius * 2.0).ok()?),
            sides: sides_value,
            // The click defines a side-midpoint direction while the core
            // recipe stores the first vertex direction.
            rotation: CoreValue::Literal(
                CoreAngle::radians(direction - std::f64::consts::PI / f64::from(sides)).ok()?,
            ),
        },
        ToolVariant::OuterDiameterPolygon => CoreRecipe::OuterDiameterPolygon {
            center: core_point_input(center),
            outer_diameter: CoreValue::Literal(CoreLength::new(radius * 2.0).ok()?),
            sides: sides_value,
            rotation: CoreValue::Literal(CoreAngle::radians(direction).ok()?),
        },
        _ => return None,
    })
}

fn slot_width_from_point(
    axis_start: SketchPoint,
    axis_end: SketchPoint,
    width_point: SketchPoint,
) -> Option<f64> {
    let du = axis_end.u - axis_start.u;
    let dv = axis_end.v - axis_start.v;
    let length = du.hypot(dv);
    if !length.is_finite() || length <= MIN_ENTITY_LENGTH {
        return None;
    }
    let offset_u = width_point.u - axis_start.u;
    let offset_v = width_point.v - axis_start.v;
    Some((du.mul_add(offset_v, -(dv * offset_u))).abs() * 2.0 / length)
}

fn two_point_slot_recipe(
    first_cap_center: SketchPoint,
    second_cap_center: SketchPoint,
    width: f64,
) -> Option<CoreRecipe> {
    Some(CoreRecipe::TwoPointSlot {
        first_cap_center: core_point_input(first_cap_center),
        second_cap_center: core_point_input(second_cap_center),
        width: CoreValue::Literal(CoreLength::new(width).ok()?),
    })
}

fn centre_outer_point_slot_recipe(
    center: SketchPoint,
    outer_tip: SketchPoint,
    width: f64,
) -> Option<CoreRecipe> {
    let overall_length = center.distance_squared(outer_tip).sqrt() * 2.0;
    Some(CoreRecipe::CentreOuterPointSlot {
        center: core_point_input(center),
        overall_length: CoreValue::Literal(CoreLength::new(overall_length).ok()?),
        width: CoreValue::Literal(CoreLength::new(width).ok()?),
        angle: CoreValue::Literal(
            CoreAngle::radians((outer_tip.v - center.v).atan2(outer_tip.u - center.u)).ok()?,
        ),
    })
}

#[derive(Clone, Copy, Debug)]
struct ThreePointArcSolution {
    center: SketchPoint,
    direction: CoreCurveDirection,
}

fn three_point_arc_geometry(
    first: SketchPoint,
    second: SketchPoint,
    solution: ThreePointArcSolution,
) -> SketchGeometry {
    match solution.direction {
        CoreCurveDirection::CounterClockwise => SketchGeometry::arc(solution.center, first, second),
        CoreCurveDirection::Clockwise => SketchGeometry::arc(solution.center, second, first),
    }
}

fn directed_three_point_arc_sweep(
    first: SketchPoint,
    second: SketchPoint,
    solution: ThreePointArcSolution,
) -> f64 {
    let first_angle = (first.v - solution.center.v).atan2(first.u - solution.center.u);
    let second_angle = (second.v - solution.center.v).atan2(second.u - solution.center.u);
    match solution.direction {
        CoreCurveDirection::CounterClockwise => {
            (second_angle - first_angle).rem_euclid(std::f64::consts::TAU)
        }
        CoreCurveDirection::Clockwise => {
            (first_angle - second_angle).rem_euclid(std::f64::consts::TAU)
        }
    }
}

/// Reconstructs the unique directed circular arc for two endpoints and one
/// sweep. The chord fixes the radius, so exposing radius as independently
/// editable here would be mathematically dishonest.
fn three_point_arc_geometry_for_sweep(
    constraint: ThreePointArcConstraint,
    sweep: f64,
) -> Option<SketchGeometry> {
    if !sweep.is_finite()
        || !(MIN_ARC_SWEEP_DEGREES.to_radians()..=MAX_ARC_SWEEP_DEGREES.to_radians())
            .contains(&sweep)
    {
        return None;
    }
    let chord_u = constraint.second.u - constraint.first.u;
    let chord_v = constraint.second.v - constraint.first.v;
    let chord = chord_u.hypot(chord_v);
    if !chord.is_finite() || chord <= MIN_ENTITY_LENGTH {
        return None;
    }
    let half_sweep_sine = (sweep * 0.5).sin();
    if !half_sweep_sine.is_finite() || half_sweep_sine <= 0.0 {
        return None;
    }
    let radius = chord / (2.0 * half_sweep_sine);
    let half_chord = chord * 0.5;
    let height_squared = radius.mul_add(radius, -(half_chord * half_chord));
    if height_squared < -MIN_ENTITY_LENGTH || !height_squared.is_finite() {
        return None;
    }
    let height = height_squared.max(0.0).sqrt();
    let left_u = -chord_v / chord;
    let left_v = chord_u / chord;
    let minor_sign = if sweep <= std::f64::consts::PI {
        1.0
    } else {
        -1.0
    };
    let direction_sign = match constraint.direction {
        CoreCurveDirection::CounterClockwise => 1.0,
        CoreCurveDirection::Clockwise => -1.0,
    };
    let signed_height = height * minor_sign * direction_sign;
    let midpoint = midpoint(constraint.first, constraint.second);
    let center = SketchPoint::new(
        signed_height.mul_add(left_u, midpoint.u),
        signed_height.mul_add(left_v, midpoint.v),
    );
    if !center.is_finite() {
        return None;
    }
    Some(match constraint.direction {
        CoreCurveDirection::CounterClockwise => {
            SketchGeometry::arc(center, constraint.first, constraint.second)
        }
        CoreCurveDirection::Clockwise => {
            SketchGeometry::arc(center, constraint.second, constraint.first)
        }
    })
}

fn three_point_arc_solution(
    start: SketchPoint,
    end: SketchPoint,
    through: SketchPoint,
) -> Option<ThreePointArcSolution> {
    let determinant = 2.0
        * (start
            .u
            .mul_add(end.v - through.v, end.u * (through.v - start.v))
            + through.u * (start.v - end.v));
    let scale = start
        .u
        .abs()
        .max(start.v.abs())
        .max(end.u.abs())
        .max(end.v.abs())
        .max(through.u.abs())
        .max(through.v.abs())
        .max(1.0);
    if !determinant.is_finite() || determinant.abs() <= MIN_ENTITY_LENGTH * scale * scale {
        return None;
    }
    let start_norm = start.u.mul_add(start.u, start.v * start.v);
    let end_norm = end.u.mul_add(end.u, end.v * end.v);
    let through_norm = through.u.mul_add(through.u, through.v * through.v);
    let center = SketchPoint::new(
        (start_norm.mul_add(
            end.v - through.v,
            end_norm.mul_add(through.v - start.v, through_norm * (start.v - end.v)),
        )) / determinant,
        (start_norm.mul_add(
            through.u - end.u,
            end_norm.mul_add(start.u - through.u, through_norm * (end.u - start.u)),
        )) / determinant,
    );
    if !center.is_finite() {
        return None;
    }
    let start_angle = (start.v - center.v).atan2(start.u - center.u);
    let end_angle = (end.v - center.v).atan2(end.u - center.u);
    let through_angle = (through.v - center.v).atan2(through.u - center.u);
    let ccw_end = (end_angle - start_angle).rem_euclid(std::f64::consts::TAU);
    let ccw_through = (through_angle - start_angle).rem_euclid(std::f64::consts::TAU);
    let direction = if ccw_through <= ccw_end {
        CoreCurveDirection::CounterClockwise
    } else {
        CoreCurveDirection::Clockwise
    };
    Some(ThreePointArcSolution { center, direction })
}

fn legacy_geometry_from_core(curve: CoreEvaluatedCurve2) -> SketchGeometry {
    let point = |point: CorePoint2| SketchPoint::new(point.u, point.v);
    match curve {
        CoreEvaluatedCurve2::Line { start, end } => {
            SketchGeometry::segment(point(start), point(end))
        }
        CoreEvaluatedCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } => {
            let (start, end) = if direction == CoreCurveDirection::CounterClockwise {
                (start, end)
            } else {
                (end, start)
            };
            SketchGeometry::arc(point(center), point(start), point(end))
        }
        CoreEvaluatedCurve2::Circle { center, radius, .. } => {
            let center = point(center);
            SketchGeometry::circle(center, SketchPoint::new(center.u + radius, center.v))
        }
        CoreEvaluatedCurve2::Bspline { control_points, .. } => {
            let start = control_points
                .first()
                .map(|p| point(*p))
                .unwrap_or_default();
            let end = control_points.last().map(|p| point(*p)).unwrap_or_default();
            SketchGeometry::segment(start, end)
        }
    }
}

fn regular_polygon_preview(
    variant: ToolVariant,
    center: SketchPoint,
    reference: SketchPoint,
    sides: u16,
) -> Option<Vec<SketchGeometry>> {
    if !(CORE_MIN_POLYGON_SIDES..=CORE_MAX_POLYGON_SIDES).contains(&sides) {
        return None;
    }
    let reference_radius = center.distance_squared(reference).sqrt();
    if !reference_radius.is_finite() || reference_radius <= MIN_ENTITY_LENGTH {
        return None;
    }
    let reference_angle = (reference.v - center.v).atan2(reference.u - center.u);
    let half_step = std::f64::consts::PI / f64::from(sides);
    let (circumradius, rotation) = match variant {
        ToolVariant::InnerDiameterPolygon => (
            reference_radius / half_step.cos(),
            reference_angle - half_step,
        ),
        ToolVariant::OuterDiameterPolygon => (reference_radius, reference_angle),
        _ => return None,
    };
    let step = std::f64::consts::TAU / f64::from(sides);
    let vertices = (0..sides)
        .map(|index| {
            let angle = f64::from(index).mul_add(step, rotation);
            SketchPoint::new(
                circumradius.mul_add(angle.cos(), center.u),
                circumradius.mul_add(angle.sin(), center.v),
            )
        })
        .collect::<Vec<_>>();
    Some(
        (0..vertices.len())
            .map(|index| {
                SketchGeometry::segment(vertices[index], vertices[(index + 1) % vertices.len()])
            })
            .collect(),
    )
}

fn slot_preview(
    first_cap_center: SketchPoint,
    second_cap_center: SketchPoint,
    width: f64,
) -> Option<Vec<SketchGeometry>> {
    let (direction_u, direction_v) = unit_direction(first_cap_center, second_cap_center)?;
    if !width.is_finite() || width <= MIN_ENTITY_LENGTH {
        return None;
    }
    let radius = width * 0.5;
    let normal_u = -direction_v * radius;
    let normal_v = direction_u * radius;
    let first_left = SketchPoint::new(first_cap_center.u + normal_u, first_cap_center.v + normal_v);
    let first_right =
        SketchPoint::new(first_cap_center.u - normal_u, first_cap_center.v - normal_v);
    let second_right = SketchPoint::new(
        second_cap_center.u - normal_u,
        second_cap_center.v - normal_v,
    );
    let second_left = SketchPoint::new(
        second_cap_center.u + normal_u,
        second_cap_center.v + normal_v,
    );
    Some(vec![
        SketchGeometry::arc(first_cap_center, first_left, first_right),
        SketchGeometry::segment(first_right, second_right),
        SketchGeometry::arc(second_cap_center, second_right, second_left),
        SketchGeometry::segment(second_left, first_left),
    ])
}

fn centre_outer_slot_cap_centers(
    center: SketchPoint,
    outer_tip: SketchPoint,
    width: f64,
) -> Option<(SketchPoint, SketchPoint)> {
    let overall_length = center.distance_squared(outer_tip).sqrt() * 2.0;
    if !overall_length.is_finite()
        || !width.is_finite()
        || width <= MIN_ENTITY_LENGTH
        || overall_length <= width
    {
        return None;
    }
    let (direction_u, direction_v) = unit_direction(center, outer_tip)?;
    let half_separation = (overall_length - width) * 0.5;
    Some((
        SketchPoint::new(
            (-direction_u).mul_add(half_separation, center.u),
            (-direction_v).mul_add(half_separation, center.v),
        ),
        SketchPoint::new(
            direction_u.mul_add(half_separation, center.u),
            direction_v.mul_add(half_separation, center.v),
        ),
    ))
}

fn exact_creation_preview_geometries(state: &SketchCanvasState) -> Option<Vec<SketchGeometry>> {
    let pointer = state.pointer_preview?.point;
    match state.exact_tool {
        ToolVariant::InnerDiameterPolygon | ToolVariant::OuterDiameterPolygon => {
            let center = state.creation_anchor?;
            let reference = state.polygon_reference_from_inputs(center, pointer);
            regular_polygon_preview(state.exact_tool, center, reference, state.polygon_sides)
        }
        ToolVariant::Text => {
            let content = state.active_tool_text("content")?;
            let height = state.active_tool_number("height")?;
            let angle = state.active_tool_number("angle")?.to_radians();
            Some(text_preview_geometries(pointer, &content, height, angle))
        }
        ToolVariant::TwoPointSlot | ToolVariant::CentreToOuterPointSlot => {
            let axis_start = state.creation_anchor?;
            let axis_end = state.slot_axis_from_inputs(axis_start, state.arc_start?);
            let width = state
                .active_tool_number("width")
                .or_else(|| {
                    state
                        .dimension_session
                        .as_ref()
                        .map(|session| session.value(SketchDimensionKind::Width))
                })
                .or_else(|| slot_width_from_point(axis_start, axis_end, pointer))?;
            let (first_cap_center, second_cap_center) = match state.exact_tool {
                ToolVariant::TwoPointSlot => (axis_start, axis_end),
                ToolVariant::CentreToOuterPointSlot => {
                    centre_outer_slot_cap_centers(axis_start, axis_end, width)?
                }
                _ => unreachable!("the outer match limits slot variants"),
            };
            slot_preview(first_cap_center, second_cap_center, width)
        }
        ToolVariant::ThreePointArc => {
            let start = state.creation_anchor?;
            let end = state.arc_start?;
            let solution = three_point_arc_solution(start, end, pointer)?;
            let geometry = match solution.direction {
                CoreCurveDirection::CounterClockwise => {
                    SketchGeometry::arc(solution.center, start, end)
                }
                CoreCurveDirection::Clockwise => SketchGeometry::arc(solution.center, end, start),
            };
            Some(vec![geometry])
        }
        _ => None,
    }
}

fn exact_live_measurement(
    state: &SketchCanvasState,
) -> Option<(SketchGeometry, Vec<DimensionField>)> {
    let geometries = exact_creation_preview_geometries(state)?;
    let [geometry] = geometries.as_slice() else {
        return None;
    };
    let geometry = *geometry;
    let mut fields = dimension_fields_for_geometry(DimensionPhase::ArcSweep, geometry);
    for field in &mut fields {
        field.readout.editable = false;
    }
    Some((geometry, fields))
}

/// Read-only dimensions for a staged exact transaction that produced one
/// presentation curve. Core transactions are immutable previews, so this
/// deliberately never creates an editable [`DimensionSession`].
fn pending_single_curve_measurement(
    state: &SketchCanvasState,
) -> Option<(SketchGeometry, Vec<DimensionField>)> {
    let pending = state.pending.as_ref()?;
    let [entity] = pending.entities.as_slice() else {
        return None;
    };
    let geometry = entity.geometry;
    let phase = dimension_phase_for_geometry(geometry);
    let mut fields = dimension_fields_for_geometry(phase, geometry);
    for field in &mut fields {
        field.readout.editable = false;
        field.readout.locked = false;
    }
    (!fields.is_empty()).then_some((geometry, fields))
}

/// Traversal direction of an exact circular profile primitive.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SketchCurveDirection {
    CounterClockwise,
    Clockwise,
}

impl SketchCurveDirection {
    const fn reversed(self) -> Self {
        match self {
            Self::CounterClockwise => Self::Clockwise,
            Self::Clockwise => Self::CounterClockwise,
        }
    }

    const fn signed_sweep(self, counter_clockwise_sweep: f64) -> f64 {
        match self {
            Self::CounterClockwise => counter_clockwise_sweep,
            Self::Clockwise => -(std::f64::consts::TAU - counter_clockwise_sweep),
        }
    }
}

/// One exact, traversal-oriented curve use in a certified sketch loop.
///
/// This is the renderer/application payload boundary for future native curve
/// topology. Circular entities remain analytic; display tessellation is never
/// reused as modeling input.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CertifiedSketchCurve {
    Line {
        start: SketchPoint,
        end: SketchPoint,
    },
    CircularArc {
        center: SketchPoint,
        start: SketchPoint,
        end: SketchPoint,
        direction: SketchCurveDirection,
    },
    /// A complete analytic circle is a loop by itself and has no graph seam.
    Circle {
        center: SketchPoint,
        rim: SketchPoint,
        direction: SketchCurveDirection,
    },
}

impl CertifiedSketchCurve {
    #[must_use]
    pub const fn is_linear(self) -> bool {
        matches!(self, Self::Line { .. })
    }

    #[must_use]
    pub const fn start(self) -> Option<SketchPoint> {
        match self {
            Self::Line { start, .. } | Self::CircularArc { start, .. } => Some(start),
            Self::Circle { .. } => None,
        }
    }

    #[must_use]
    pub const fn end(self) -> Option<SketchPoint> {
        match self {
            Self::Line { end, .. } | Self::CircularArc { end, .. } => Some(end),
            Self::Circle { .. } => None,
        }
    }

    #[must_use]
    pub const fn reversed(self) -> Self {
        match self {
            Self::Line { start, end } => Self::Line {
                start: end,
                end: start,
            },
            Self::CircularArc {
                center,
                start,
                end,
                direction,
            } => Self::CircularArc {
                center,
                start: end,
                end: start,
                direction: direction.reversed(),
            },
            Self::Circle {
                center,
                rim,
                direction,
            } => Self::Circle {
                center,
                rim,
                direction: direction.reversed(),
            },
        }
    }
}

/// One exact closed wire after deterministic ordering and orientation.
#[derive(Clone, Debug, PartialEq)]
pub struct CertifiedSketchLoop {
    /// Outer loops are normalized counter-clockwise and hole loops clockwise.
    pub winding: ProfileWinding,
    /// Even depths are material islands; odd depths are void boundaries.
    pub nesting_depth: usize,
    pub curves: Vec<CertifiedSketchCurve>,
}

impl CertifiedSketchLoop {
    #[must_use]
    pub fn has_analytic_curves(&self) -> bool {
        self.curves.iter().any(|curve| !curve.is_linear())
    }

    /// Returns the exact polygon vertices when this wire is entirely linear.
    /// The repeated closing vertex is intentionally omitted.
    #[must_use]
    pub fn linear_vertices(&self) -> Option<Vec<SketchPoint>> {
        let vertices = self
            .curves
            .iter()
            .copied()
            .map(CertifiedSketchCurve::start)
            .collect::<Option<Vec<_>>>()?;
        (vertices.len() >= 3).then_some(vertices)
    }
}

/// One material region: a single outer boundary and its directly nested holes.
#[derive(Clone, Debug, PartialEq)]
pub struct CertifiedSketchRegion {
    pub outer: CertifiedSketchLoop,
    pub holes: Vec<CertifiedSketchLoop>,
}

/// A protocol-neutral linear region adapter.
///
/// Kernel/protocol code can map this losslessly to its native point type. If
/// [`CertifiedSketchProfile::linear_regions`] returns `None`, the profile owns
/// analytic curves and must use an exact curve-wire command instead.
#[derive(Clone, Debug, PartialEq)]
pub struct CertifiedLinearRegion {
    pub outer: Vec<SketchPoint>,
    pub holes: Vec<Vec<SketchPoint>>,
}

/// Exact planar material selected by all committed closed sketch wires.
#[derive(Clone, Debug, PartialEq)]
pub struct CertifiedSketchProfile {
    pub regions: Vec<CertifiedSketchRegion>,
}

impl CertifiedSketchProfile {
    #[must_use]
    pub fn loop_count(&self) -> usize {
        self.regions
            .iter()
            .map(|region| 1 + region.holes.len())
            .sum()
    }

    #[must_use]
    pub fn hole_count(&self) -> usize {
        self.regions.iter().map(|region| region.holes.len()).sum()
    }

    #[must_use]
    pub fn curve_count(&self) -> usize {
        self.regions
            .iter()
            .map(|region| {
                region.outer.curves.len()
                    + region
                        .holes
                        .iter()
                        .map(|profile_loop| profile_loop.curves.len())
                        .sum::<usize>()
            })
            .sum()
    }

    #[must_use]
    pub fn has_analytic_curves(&self) -> bool {
        self.regions.iter().any(|region| {
            region.outer.has_analytic_curves()
                || region
                    .holes
                    .iter()
                    .any(CertifiedSketchLoop::has_analytic_curves)
        })
    }

    /// Lossless adapter for kernels that currently accept linear regions.
    #[must_use]
    pub fn linear_regions(&self) -> Option<Vec<CertifiedLinearRegion>> {
        self.regions
            .iter()
            .map(|region| {
                Some(CertifiedLinearRegion {
                    outer: region.outer.linear_vertices()?,
                    holes: region
                        .holes
                        .iter()
                        .map(CertifiedSketchLoop::linear_vertices)
                        .collect::<Option<Vec<_>>>()?,
                })
            })
            .collect()
    }
}

/// A model edit waiting for the workbench-wide tick/Enter confirmation.
///
/// One pending action may own several atomic curves (rectangle, polygon, slot,
/// pattern, or modifier), but it still publishes as one confirmation and one
/// sketch revision.
#[derive(Clone, Debug)]
pub struct PendingSketchEdit {
    /// Stable presentation identity used by the workbench confirmation gate.
    /// Insertions use their first provisional entity; retirement-only edits
    /// use the selected committed entity even though they insert no geometry.
    subject: SketchEntityId,
    label: &'static str,
    entities: Vec<SketchEntity>,
    core_transaction: Option<CoreTransaction>,
    /// Core curve IDs aligned with `entities`. Programmatic point entities do
    /// not have a core curve and therefore carry `None`.
    core_entities: Vec<Option<CoreEntityId>>,
    /// Committed presentation entities superseded when this transaction is
    /// confirmed. They remain visible as a red retirement preview until then.
    retired_entities: Vec<SketchEntityId>,
    /// This edit re-authors existing geometry rather than adding to it: the
    /// canvas must show the subject at its new value, not a replacement
    /// beside a red original, because accepting the typed value is the
    /// commit. Only the selected-feature parameter editor stages one.
    in_place: bool,
}

struct PendingCorePresentation {
    entities: Vec<SketchEntity>,
    core_entities: Vec<Option<CoreEntityId>>,
    retired_entities: Vec<SketchEntityId>,
}

impl PendingSketchEdit {
    #[must_use]
    pub const fn subject(&self) -> SketchEntityId {
        self.subject
    }

    #[must_use]
    pub fn entity(&self) -> Option<SketchEntity> {
        self.entities.first().copied()
    }

    #[must_use]
    pub const fn label(&self) -> &'static str {
        self.label
    }

    #[must_use]
    pub fn entities(&self) -> &[SketchEntity] {
        &self.entities
    }

    #[must_use]
    pub fn retired_entities(&self) -> &[SketchEntityId] {
        &self.retired_entities
    }

    #[must_use]
    pub const fn is_in_place(&self) -> bool {
        self.in_place
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SketchEditError {
    PendingEditAlreadyExists,
    NoPendingEdit,
    NonFiniteGeometry,
    DegenerateGeometry,
    AuthoringRejected,
}

/// Conservative visual diagnostics only; kernel/profile certification owns truth.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProfileDiagnostics {
    pub isolated_points: usize,
    pub open_segments: usize,
    pub closed_rectangles: usize,
    pub closed_circles: usize,
    pub open_arcs: usize,
    pub degenerate_entities: usize,
    pub pending_entities: usize,
    /// Closed wires found after order- and direction-independent extraction.
    pub certified_loops: usize,
    pub material_regions: usize,
    pub profile_holes: usize,
    pub analytic_curves: usize,
    pub open_wire_components: usize,
    pub branched_vertices: usize,
    pub intersecting_wires: usize,
}

impl ProfileDiagnostics {
    #[must_use]
    pub const fn has_closed_profile_candidate(self) -> bool {
        self.certified_loops > 0
            && self.open_wire_components == 0
            && self.branched_vertices == 0
            && self.intersecting_wires == 0
            && self.degenerate_entities == 0
    }

    #[must_use]
    pub const fn status(self) -> LocalProfileStatus {
        if self.degenerate_entities > 0 {
            LocalProfileStatus::Degenerate
        } else if self.open_wire_components > 0 || self.branched_vertices > 0 {
            LocalProfileStatus::Open
        } else if self.has_closed_profile_candidate() {
            LocalProfileStatus::ClosedCandidate
        } else if self.isolated_points == 0
            && self.open_segments == 0
            && self.closed_rectangles == 0
            && self.closed_circles == 0
        {
            LocalProfileStatus::Empty
        } else {
            LocalProfileStatus::Open
        }
    }
}

/// UI-level profile feedback. This never substitutes for kernel certification.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum LocalProfileStatus {
    #[default]
    Empty,
    Open,
    ClosedCandidate,
    IndeterminateCurves,
    Degenerate,
}

/// Certified status of the currently visible profile geometry.
///
/// Polyline closure, winding, and self-intersection come from
/// `artificer-geometry`'s conservative predicate pipeline. Circles have an
/// analytically closed candidate status, while arcs and mixed/multiple loops
/// remain explicit rather than being guessed valid.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CertifiedProfileStatus {
    #[default]
    Empty,
    Open,
    Closed {
        winding: ProfileWinding,
    },
    SelfIntersecting,
    Invalid,
    Indeterminate,
    ClosedAnalyticCircle,
    ClosedAnalyticCurves,
    ClosedRegions {
        regions: usize,
        loops: usize,
        holes: usize,
        analytic: bool,
    },
    TooManyCurves {
        count: usize,
    },
    TooManyLoops {
        count: usize,
    },
    TooManyRegions {
        count: usize,
    },
    LinearLoopTooLarge {
        count: usize,
    },
    CurvesNeedCertification,
    MultipleProfiles,
}

impl CertifiedProfileStatus {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Empty => "PROFILE EMPTY",
            Self::Open => "PROFILE OPEN",
            Self::Closed {
                winding: ProfileWinding::CounterClockwise,
            } => "PROFILE CLOSED · COUNTER-CLOCKWISE",
            Self::Closed {
                winding: ProfileWinding::Clockwise,
            } => "PROFILE CLOSED · CLOCKWISE (REVERSED)",
            Self::SelfIntersecting => "PROFILE SELF-INTERSECTING",
            Self::Invalid => "PROFILE sketch_colours().invalid",
            Self::Indeterminate => "PROFILE INDETERMINATE",
            Self::ClosedAnalyticCircle => "PROFILE CLOSED · ANALYTIC CIRCLE",
            Self::ClosedAnalyticCurves => "PROFILE CLOSED · ANALYTIC CURVES",
            Self::ClosedRegions { analytic: true, .. } => "PROFILE REGIONS CLOSED · EXACT CURVES",
            Self::ClosedRegions {
                analytic: false, ..
            } => "PROFILE REGIONS CLOSED",
            Self::TooManyCurves { .. } => "PROFILE CURVE LIMIT EXCEEDED",
            Self::TooManyLoops { .. } => "PROFILE LOOP LIMIT EXCEEDED",
            Self::TooManyRegions { .. } => "PROFILE REGION LIMIT EXCEEDED",
            Self::LinearLoopTooLarge { .. } => "LINEAR LOOP LIMIT EXCEEDED",
            Self::CurvesNeedCertification => {
                "PROFILE CURVES · CERTIFICATION sketch_colours().pending"
            }
            Self::MultipleProfiles => "PROFILE MULTI-LOOP · AMBIGUOUS",
        }
    }

    #[must_use]
    pub const fn can_finish(self) -> bool {
        matches!(
            self,
            Self::Closed { .. }
                | Self::ClosedAnalyticCircle
                | Self::ClosedAnalyticCurves
                | Self::ClosedRegions { .. }
        )
    }
}

impl LocalProfileStatus {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Empty => "Empty",
            Self::Open => "Open profile",
            Self::ClosedCandidate => "Closed candidate",
            Self::IndeterminateCurves => "Curves need certification",
            Self::Degenerate => "Degenerate geometry",
        }
    }
}

/// Describes why a pointer location snapped to its returned model coordinate.
///
/// The `Support*` variants come from the sketch support's own analytic
/// geometry — the edges of the face being sketched on — rather than from an
/// authored sketch entity, so they carry no [`SketchEntityId`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SnapKind {
    #[default]
    None,
    Grid,
    Endpoint(SketchEntityId),
    Intersection(SketchEntityId, SketchEntityId),
    Center(SketchEntityId),
    Midpoint(SketchEntityId),
    Quadrant(SketchEntityId, u8),
    /// Nearest point along an authored curve's interior.
    OnCurve(SketchEntityId),
    SupportEndpoint,
    SupportCenter,
    SupportMidpoint,
    SupportQuadrant,
    SupportEdge,
}

impl SnapKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "Snap: off",
            Self::Grid => "Snap: grid",
            Self::Endpoint(_) => "Snap: endpoint",
            Self::Intersection(_, _) => "Snap: intersection",
            Self::Center(_) => "Snap: centre",
            Self::Midpoint(_) => "Snap: midpoint",
            Self::Quadrant(_, _) => "Snap: quadrant",
            Self::OnCurve(_) => "Snap: on curve",
            Self::SupportEndpoint => "Snap: edge vertex",
            Self::SupportCenter => "Snap: edge centre",
            Self::SupportMidpoint => "Snap: edge midpoint",
            Self::SupportQuadrant => "Snap: edge quadrant",
            Self::SupportEdge => "Snap: on edge",
        }
    }

    /// Whether the snap referenced support geometry instead of a sketch entity.
    #[must_use]
    pub const fn is_support_reference(self) -> bool {
        matches!(
            self,
            Self::SupportEndpoint
                | Self::SupportCenter
                | Self::SupportMidpoint
                | Self::SupportQuadrant
                | Self::SupportEdge
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SnapResult {
    pub point: SketchPoint,
    pub kind: SnapKind,
}

/// User-configurable sketch snapping behavior.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnapSettings {
    pub enabled: bool,
    pub grid_step: f64,
    pub endpoint_radius_points: f32,
}

impl Default for SnapSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            grid_step: 0.25,
            endpoint_radius_points: 10.0,
        }
    }
}

impl SnapSettings {
    #[must_use]
    pub fn snap_to_grid(self, point: SketchPoint) -> SketchPoint {
        self.snap_to_step(point, self.grid_step)
    }

    /// Snaps to the grid the user can actually see at this zoom.
    ///
    /// `grid_step` is the finest lattice the sketch admits, not the spacing
    /// drawn on screen: the display already coarsens to readable 1/2/5
    /// multiples as the camera pulls back. Snapping to the raw lattice while
    /// the visible grid coarsens is what makes a zoomed-out sketch feel
    /// unsnapped — the step lands far below one screen pixel, so every
    /// pointer position is already "on" it.
    #[must_use]
    pub fn snap_to_visible_grid(self, point: SketchPoint, points_per_unit: f64) -> SketchPoint {
        let step =
            resolvable_grid_spacing(points_per_unit, self.grid_step, TARGET_SNAP_SPACING_POINTS)
                .map_or(self.grid_step, VisibleGridSpacing::minor_world_step);
        self.snap_to_step(point, step)
    }

    fn snap_to_step(self, point: SketchPoint, step: f64) -> SketchPoint {
        if !self.enabled || !step.is_finite() || step <= 0.0 {
            return point;
        }
        let snapped = SketchPoint::new(
            normalized_zero((point.u / step).round() * step),
            normalized_zero((point.v / step).round() * step),
        );
        if snapped.is_finite() { snapped } else { point }
    }
}

/// Orthographic, planar camera with a model-space point at screen center.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SketchView {
    pub center: SketchPoint,
    pub points_per_unit: f64,
    /// Presentation-only quarter turns applied after canonical sketch
    /// coordinates are evaluated. Geometry and kernel profiles remain in the
    /// authoritative plane frame while a face sketch can retain the model
    /// camera's nearest upright orientation.
    pub quarter_turns: u8,
}

impl Default for SketchView {
    fn default() -> Self {
        Self {
            center: SketchPoint::default(),
            points_per_unit: DEFAULT_POINTS_PER_UNIT,
            quarter_turns: 0,
        }
    }
}

impl SketchView {
    #[must_use]
    pub fn sketch_to_screen(self, rect: Rect, point: SketchPoint) -> Pos2 {
        let (horizontal, vertical) =
            self.rotate_offset(point.u - self.center.u, point.v - self.center.v);
        Pos2::new(
            rect.center().x + (horizontal * self.points_per_unit) as f32,
            rect.center().y - (vertical * self.points_per_unit) as f32,
        )
    }

    #[must_use]
    pub fn screen_to_sketch(self, rect: Rect, position: Pos2) -> SketchPoint {
        let horizontal = f64::from(position.x - rect.center().x) / self.points_per_unit;
        let vertical = f64::from(rect.center().y - position.y) / self.points_per_unit;
        let (u, v) = self.unrotate_offset(horizontal, vertical);
        SketchPoint::new(self.center.u + u, self.center.v + v)
    }

    pub fn pan_by_screen_delta(&mut self, delta: Vec2) {
        let horizontal = f64::from(delta.x) / self.points_per_unit;
        let vertical = -f64::from(delta.y) / self.points_per_unit;
        let (u, v) = self.unrotate_offset(horizontal, vertical);
        self.center.u -= u;
        self.center.v -= v;
    }

    pub fn zoom_about(&mut self, rect: Rect, position: Pos2, factor: f64) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let before = self.screen_to_sketch(rect, position);
        self.points_per_unit =
            (self.points_per_unit * factor).clamp(MIN_POINTS_PER_UNIT, MAX_POINTS_PER_UNIT);
        let after = self.screen_to_sketch(rect, position);
        self.center.u += before.u - after.u;
        self.center.v += before.v - after.v;
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn set_quarter_turns(&mut self, quarter_turns: u8) {
        self.quarter_turns = quarter_turns % 4;
    }

    fn rotate_offset(self, u: f64, v: f64) -> (f64, f64) {
        match self.quarter_turns % 4 {
            0 => (u, v),
            1 => (-v, u),
            2 => (-u, -v),
            3 => (v, -u),
            _ => unreachable!("quarter turns are reduced modulo four"),
        }
    }

    fn unrotate_offset(self, horizontal: f64, vertical: f64) -> (f64, f64) {
        match self.quarter_turns % 4 {
            0 => (horizontal, vertical),
            1 => (vertical, -horizontal),
            2 => (-horizontal, -vertical),
            3 => (-vertical, horizontal),
            _ => unreachable!("quarter turns are reduced modulo four"),
        }
    }
}

/// UI-facing point-acquisition progress for the active exact sketch tool.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SketchGestureProgress {
    /// Number of point-acquisition clicks already accepted.
    pub completed_points: u8,
    /// Number of point-acquisition clicks required by this tool.
    pub required_points: u8,
    /// The geometry is complete and owned by the universal tick/cross gate.
    pub awaiting_confirmation: bool,
}

/// Revision-keyed analytic arrangement and user profile-cell selection.
///
/// The cache owns no modeling approximation: signatures, hit testing, and
/// profile compilation all refer to exact arrangement curves. Sampling occurs
/// only when the selected cells are painted.
#[derive(Debug, Default)]
struct AnalyticRegionSelection {
    revision: Option<CoreSketchRevision>,
    arrangement: Option<CoreSketchArrangement>,
    selected: BTreeSet<CoreRegionSignature>,
    selection_anchors: BTreeMap<CoreRegionSignature, CorePoint2>,
    hovered: Option<CoreRegionSignature>,
    /// Whether the selection came from a deliberate pick rather than the
    /// lone-cell fallback that keeps Extrude working on single-profile
    /// sketches. Only deliberate picks earn a selection fill in the model
    /// viewport; the fallback would tint every committed sketch forever.
    explicit: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PatternManipulatorKind {
    RectangularDirection,
    CircularCenter,
}

#[derive(Clone, Copy, Debug)]
struct PatternManipulator {
    kind: PatternManipulatorKind,
    position: SketchPoint,
    dragging: bool,
}

/// Persistent state owned by the workbench's sketch document or session.
#[derive(Debug)]
pub struct SketchCanvasState {
    plane: SketchPlane,
    tool: SketchTool,
    exact_tool: ToolVariant,
    view: SketchView,
    snap: SnapSettings,
    authoring: CoreSketchDefinition,
    undo_journal: CoreUndoJournal,
    entities: Vec<SketchEntity>,
    core_by_ui: BTreeMap<SketchEntityId, Vec<CoreEntityId>>,
    ui_by_core: BTreeMap<CoreEntityId, SketchEntityId>,
    operation_by_ui: BTreeMap<SketchEntityId, CoreOperationId>,
    selected: Option<SketchEntityId>,
    /// Retained text/value state for editing the selected operation recipe.
    /// It is presentation state only; exact candidates always come from
    /// `CoreSketchDefinition::stage_replace`.
    selected_recipe_editor: Option<SelectedRecipeEditor>,
    /// Stable seed/source acquisition for compound modifiers and patterns.
    modifier_sources: Vec<CoreEntityId>,
    /// Operands picked so far for the active relation tool, in click order.
    relation_operands: Vec<RelationOperand>,
    /// What the dimension tool has been pointed at so far, when it is taking a
    /// distance between two things rather than editing one curve's own
    /// dimensions. Empty is the ordinary per-curve behaviour.
    dimension_operands: Vec<RelationOperand>,
    /// Why the last relation was refused, in the solver's own words. Cleared
    /// when a relation succeeds or the tool changes.
    relation_diagnostic: Option<String>,
    /// The points of the relation the panel's pointer is over, ringed on the
    /// canvas so a row in the list can be found in the drawing. Presentation
    /// only: set each frame by whoever draws the list, never persisted.
    relation_highlight: Vec<SketchPoint>,
    /// The drag in progress on a dimension's value, and where inside the chip
    /// it was grabbed, so the label does not jump to the pointer.
    dimension_drag: DragHandleState,
    dimension_drag_target: Option<(CoreConstraintId, (f64, f64))>,
    /// Exact model-space picks keyed by the carrier selected for a corner tool.
    modifier_picks: BTreeMap<CoreEntityId, SketchPoint>,
    hovered: Option<SketchEntityId>,
    /// Exact removable fragment under the Trim pointer. This intentionally
    /// stores the selected subcurve rather than the carrier entity.
    trim_hover_fragment: Option<CoreEvaluatedCurve2>,
    /// The chain Offset would take if the pointer clicked where it is. What a
    /// click will select is worth seeing before the click, because "every
    /// connected curve" is a claim about geometry the user cannot check by
    /// looking at one of them.
    offset_hover: Vec<CoreEvaluatedCurve2>,
    /// Retained primary-pointer handle for pattern direction/centre editing.
    pattern_manipulator: Option<PatternManipulator>,
    pattern_drag: DragHandleState,
    active_drag_handle: Option<SketchDragHandle>,
    creation_anchor: Option<SketchPoint>,
    /// Uncommitted vertices owned by one atomic chained-polyline gesture.
    ///
    /// These points deliberately have no core/UI identity until the complete
    /// chain is staged behind the universal confirmation gate.
    polyline_vertices: Vec<SketchPoint>,
    /// Whether the pointer currently owns the provisional segment extending
    /// from the last accepted polyline vertex. Escape clears this layer before
    /// a subsequent Escape clears the accepted chain.
    polyline_current_segment_active: bool,
    arc_start: Option<SketchPoint>,
    polygon_sides: u16,
    tool_inputs: ActiveToolInputs,
    pointer_preview: Option<SnapResult>,
    pending: Option<PendingSketchEdit>,
    next_entity_id: u64,
    certified_profile: CertifiedProfileStatus,
    profile_analysis: ProfileAnalysis,
    analytic_regions: AnalyticRegionSelection,
    dimension_session: Option<DimensionSession>,
    /// One-shot caret request for the box the Dimension tool just armed. The
    /// pick and the boxes render in the same frame, so this never survives a
    /// frame boundary: it is a focus request, not editor state.
    focus_dimension_box: Option<SketchDimensionKind>,
    /// Where the Dimension tool's pick landed, in sketch coordinates. On a
    /// compound target — a rectangle reassembled from its exploded sides —
    /// this is what remembers *which side* got clicked, so the armed box and
    /// its annotation sit on that side rather than always on Width. It lives
    /// until the selection or tool changes; unlike the focus request it has
    /// to survive frames so the box stays put while the value is typed.
    dimension_pick: Option<SketchPoint>,
    next_dimension_serial: u64,
    last_context_fit_key: Option<SketchContextFitKey>,
    /// The active support's analytic curves, mirrored here because snapping
    /// runs before the frame's context borrow is available to `snap_point`.
    support_curves: Vec<SketchContextCurve>,
    /// Named document variables, by name, evaluated to canonical magnitudes
    /// (millimetres for lengths). Dimension and recipe fields accept these
    /// names in arithmetic entries: `width`, `width / 2 + 5`. The workbench
    /// refreshes the map from the document's parameter table.
    named_values: BTreeMap<String, f64>,
}

impl Default for SketchCanvasState {
    fn default() -> Self {
        Self {
            plane: SketchPlane::default(),
            tool: SketchTool::default(),
            exact_tool: ToolVariant::Select,
            view: SketchView::default(),
            snap: SnapSettings::default(),
            authoring: CoreSketchDefinition::new(),
            undo_journal: CoreUndoJournal::new(128),
            entities: Vec::new(),
            core_by_ui: BTreeMap::new(),
            ui_by_core: BTreeMap::new(),
            operation_by_ui: BTreeMap::new(),
            selected: None,
            selected_recipe_editor: None,
            modifier_sources: Vec::new(),
            relation_operands: Vec::new(),
            dimension_operands: Vec::new(),
            relation_diagnostic: None,
            relation_highlight: Vec::new(),
            dimension_drag: DragHandleState::default(),
            dimension_drag_target: None,
            modifier_picks: BTreeMap::new(),
            hovered: None,
            trim_hover_fragment: None,
            offset_hover: Vec::new(),
            pattern_manipulator: None,
            pattern_drag: DragHandleState::default(),
            active_drag_handle: None,
            creation_anchor: None,
            polyline_vertices: Vec::new(),
            polyline_current_segment_active: false,
            arc_start: None,
            polygon_sides: DEFAULT_POLYGON_SIDES,
            tool_inputs: ActiveToolInputs::default(),
            pointer_preview: None,
            pending: None,
            next_entity_id: 1,
            certified_profile: CertifiedProfileStatus::Empty,
            profile_analysis: ProfileAnalysis::status(
                CertifiedProfileStatus::Empty,
                RegionAnalysisDiagnostics::default(),
            ),
            analytic_regions: AnalyticRegionSelection::default(),
            dimension_session: None,
            focus_dimension_box: None,
            dimension_pick: None,
            next_dimension_serial: 1,
            last_context_fit_key: None,
            support_curves: Vec::new(),
            named_values: BTreeMap::new(),
        }
    }
}

impl SketchCanvasState {
    #[must_use]
    pub fn new(plane: SketchPlane) -> Self {
        Self {
            plane,
            ..Self::default()
        }
    }

    /// Hydrates an exact persisted v6 authoring graph into a directly editable
    /// canvas. Core identities remain authoritative; presentation identities
    /// are rebuilt monotonically and local undo starts at the loaded state.
    pub fn from_authoring(
        plane: SketchPlane,
        authoring: CoreSketchDefinition,
    ) -> Result<Self, SketchEditError> {
        let mut state = Self::new(plane);
        state.replace_authoring(authoring)?;
        Ok(state)
    }

    /// Hydrates a persisted authoring graph and restores an explicit set of
    /// stable analytic region signatures. The arrangement is always rebuilt
    /// from current exact curves; no cached profile geometry is trusted.
    pub fn from_authoring_with_regions(
        plane: SketchPlane,
        authoring: CoreSketchDefinition,
        selected_regions: &[CoreRegionSignature],
    ) -> Result<Self, SketchEditError> {
        let mut state = Self::new(plane);
        state.replace_authoring(authoring)?;
        state.restore_selected_regions(selected_regions)?;
        Ok(state)
    }

    /// Replaces canvas truth from a checked persistent graph without carrying
    /// undo entries across document loads or sketch switches.
    pub fn replace_authoring(
        &mut self,
        authoring: CoreSketchDefinition,
    ) -> Result<(), SketchEditError> {
        authoring
            .validate(PrecisionPolicy::default())
            .map_err(|_| SketchEditError::AuthoringRejected)?;
        self.authoring = authoring;
        self.undo_journal = CoreUndoJournal::new(128);
        self.pending = None;
        self.analytic_regions = AnalyticRegionSelection::default();
        self.clear_creation_draft();
        self.rebuild_presentation_from_authoring()?;
        Ok(())
    }

    fn restore_selected_regions(
        &mut self,
        selected_regions: &[CoreRegionSignature],
    ) -> Result<(), SketchEditError> {
        self.refresh_analytic_regions();
        let arrangement = self
            .analytic_regions
            .arrangement
            .as_ref()
            .ok_or(SketchEditError::AuthoringRejected)?;
        let precision = PrecisionPolicy::default();
        let mut selected = BTreeSet::new();
        let mut anchors = BTreeMap::new();
        for signature in selected_regions {
            let cell = arrangement
                .cell(signature)
                .ok_or(SketchEditError::AuthoringRejected)?;
            let anchor = arrangement
                .cell_interior_sample(cell, &precision)
                .ok_or(SketchEditError::AuthoringRejected)?;
            selected.insert(signature.clone());
            anchors.insert(signature.clone(), anchor);
        }
        self.analytic_regions.selected = selected;
        self.analytic_regions.selection_anchors = anchors;
        self.analytic_regions.hovered = None;
        if !selected_regions.is_empty() && self.selected_planar_profile().is_none() {
            return Err(SketchEditError::AuthoringRejected);
        }
        Ok(())
    }

    #[must_use]
    pub fn can_undo_local(&self) -> bool {
        self.pending.is_none() && self.undo_journal.can_undo()
    }

    #[must_use]
    pub fn can_redo_local(&self) -> bool {
        self.pending.is_none() && self.undo_journal.can_redo()
    }

    /// Restores the previous confirmed authoring graph. Provisional edits are
    /// excluded, and UI identities continue above their existing high-water
    /// mark when presentation adapters are rebuilt.
    pub fn undo_local(&mut self) -> bool {
        if self.pending.is_some() || !self.undo_journal.undo(&mut self.authoring) {
            return false;
        }
        if self.rebuild_presentation_from_authoring().is_err() {
            let restored = self.undo_journal.redo(&mut self.authoring);
            debug_assert!(restored, "failed undo must be recoverable");
            let _ = self.rebuild_presentation_from_authoring();
            return false;
        }
        true
    }

    /// Reapplies the next confirmed local graph through the same deterministic
    /// presentation repair as undo.
    pub fn redo_local(&mut self) -> bool {
        if self.pending.is_some() || !self.undo_journal.redo(&mut self.authoring) {
            return false;
        }
        if self.rebuild_presentation_from_authoring().is_err() {
            let restored = self.undo_journal.undo(&mut self.authoring);
            debug_assert!(restored, "failed redo must be recoverable");
            let _ = self.rebuild_presentation_from_authoring();
            return false;
        }
        true
    }

    #[must_use]
    pub const fn plane(&self) -> SketchPlane {
        self.plane
    }

    /// Plane changes are rejected after geometry exists or while an edit is
    /// awaiting confirmation. Existing planar coordinates must never be
    /// silently reinterpreted against a different world plane.
    pub fn set_plane(&mut self, plane: SketchPlane) -> bool {
        if self.pending.is_some() {
            return false;
        }
        if self.plane != plane {
            if !self.entities.is_empty() {
                return false;
            }
            self.plane = plane;
            self.selected = None;
            self.selected_recipe_editor = None;
            self.clear_creation_draft();
            self.view.reset();
            self.last_context_fit_key = None;
        }
        true
    }

    #[must_use]
    pub const fn tool(&self) -> SketchTool {
        self.tool
    }

    #[must_use]
    pub const fn exact_tool(&self) -> ToolVariant {
        self.exact_tool
    }

    /// Current point-acquisition phase for the active-tool palette.
    #[must_use]
    pub fn gesture_progress(&self) -> SketchGestureProgress {
        let required_points =
            u8::try_from(self.exact_tool.descriptor().acquisition_phases.len()).unwrap_or(u8::MAX);
        let completed_points = if self.pending.is_some() {
            required_points
        } else if self.exact_tool == ToolVariant::ChainedPolyline {
            // A polyline's second acquisition phase is intentionally
            // repeatable. Keep the palette on that phase until the complete
            // local chain is staged as one operation.
            u8::from(!self.polyline_vertices.is_empty())
        } else if matches!(
            self.exact_tool,
            ToolVariant::Fillet | ToolVariant::Chamfer | ToolVariant::TwoDistanceChamfer
        ) {
            u8::try_from(self.modifier_sources.len()).unwrap_or(u8::MAX)
        } else {
            u8::from(self.creation_anchor.is_some()) + u8::from(self.arc_start.is_some())
        };
        SketchGestureProgress {
            completed_points: completed_points.min(required_points),
            required_points,
            // A typed parameter preview is not waiting for anything: it
            // applies when it is accepted. Only a staged gesture is.
            awaiting_confirmation: self
                .pending
                .as_ref()
                .is_some_and(|pending| !pending.in_place),
        }
    }

    /// Side count used by both regular-polygon variants.
    #[must_use]
    pub const fn polygon_sides(&self) -> u16 {
        self.polygon_sides
    }

    /// Updates the live polygon parameter. A staged operation remains immutable
    /// until it is confirmed or cancelled.
    pub fn set_polygon_sides(&mut self, sides: u16) -> bool {
        if self.pending.is_some()
            || !(CORE_MIN_POLYGON_SIDES..=CORE_MAX_POLYGON_SIDES).contains(&sides)
        {
            return false;
        }
        self.polygon_sides = sides;
        if matches!(
            self.exact_tool,
            ToolVariant::InnerDiameterPolygon | ToolVariant::OuterDiameterPolygon
        ) {
            self.sync_active_tool_number("sides", f64::from(sides));
        }
        true
    }

    /// Current retained text for an inspector-owned active-tool field.
    /// Canvas-owned primitive dimensions deliberately return `None` and keep
    /// using their in-canvas dimensional editor.
    #[must_use]
    pub fn active_tool_input_text(&self, stable_key: &'static str) -> Option<String> {
        if let Some(text) = self.active_tool_text(stable_key) {
            return Some(text);
        }
        let (default, _) = tool_number_spec(self.exact_tool, stable_key)?;
        Some(
            self.tool_inputs
                .numbers
                .get(&(self.exact_tool, stable_key))
                .map_or_else(|| format_tool_number(default), |input| input.text.clone()),
        )
    }

    /// The value of one free-text tool field, when the active tool has one.
    #[must_use]
    pub fn active_tool_text(&self, stable_key: &'static str) -> Option<String> {
        let default = tool_text_default(self.exact_tool, stable_key)?;
        Some(
            self.tool_inputs
                .texts
                .get(&(self.exact_tool, stable_key))
                .cloned()
                .unwrap_or_else(|| default.to_owned()),
        )
    }

    /// Applies one typed edit while retaining the previous valid preview
    /// value. Returns false only when the field does not belong to this tool
    /// or while an immutable operation is awaiting confirmation.
    pub fn set_active_tool_input_text(&mut self, stable_key: &'static str, text: String) -> bool {
        if self.pending.is_some() {
            return false;
        }
        if tool_text_default(self.exact_tool, stable_key).is_some() {
            self.tool_inputs
                .texts
                .insert((self.exact_tool, stable_key), text);
            return true;
        }
        let Some((default, domain)) = tool_number_spec(self.exact_tool, stable_key) else {
            return false;
        };
        self.tool_inputs
            .numbers
            .entry((self.exact_tool, stable_key))
            .or_insert_with(|| RetainedToolNumber::new(default))
            .edit(text, domain);
        if stable_key == "sides"
            && self.active_tool_input_error(stable_key).is_none()
            && let Some(sides) = self.active_tool_number(stable_key)
        {
            self.polygon_sides = sides as u16;
        }
        if self.exact_tool == ToolVariant::RectangularPattern
            && stable_key == "spacing_u"
            && self.active_tool_input_error(stable_key).is_none()
        {
            self.sync_pattern_manipulator_from_spacing();
        }
        true
    }

    /// Escape from a focused inspector editor restores the retained valid
    /// value without cancelling a draft or pending model operation.
    pub fn restore_active_tool_input(&mut self, stable_key: &'static str) -> bool {
        let Some(input) = self
            .tool_inputs
            .numbers
            .get_mut(&(self.exact_tool, stable_key))
        else {
            return tool_number_spec(self.exact_tool, stable_key).is_some()
                || tool_text_default(self.exact_tool, stable_key).is_some();
        };
        input.restore_last_valid();
        true
    }

    #[must_use]
    pub fn active_tool_input_error(&self, stable_key: &'static str) -> Option<ToolInputError> {
        self.tool_inputs
            .numbers
            .get(&(self.exact_tool, stable_key))
            .and_then(|input| input.error)
    }

    #[must_use]
    pub fn active_tool_flag(&self, stable_key: &'static str) -> Option<bool> {
        let default = tool_flag_default(self.exact_tool, stable_key)?;
        Some(
            self.tool_inputs
                .flags
                .get(&(self.exact_tool, stable_key))
                .copied()
                .unwrap_or(default),
        )
    }

    pub fn set_active_tool_flag(&mut self, stable_key: &'static str, value: bool) -> bool {
        if self.pending.is_some() || tool_flag_default(self.exact_tool, stable_key).is_none() {
            return false;
        }
        self.tool_inputs
            .flags
            .insert((self.exact_tool, stable_key), value);
        true
    }

    fn active_tool_number(&self, stable_key: &'static str) -> Option<f64> {
        let (default, _) = tool_number_spec(self.exact_tool, stable_key)?;
        Some(
            self.tool_inputs
                .numbers
                .get(&(self.exact_tool, stable_key))
                .map_or(default, |input| input.value),
        )
    }

    fn sync_active_tool_number(&mut self, stable_key: &'static str, value: f64) {
        let Some((default, domain)) = tool_number_spec(self.exact_tool, stable_key) else {
            return;
        };
        self.tool_inputs
            .numbers
            .entry((self.exact_tool, stable_key))
            .or_insert_with(|| RetainedToolNumber::new(default))
            .sync_live_value(value, domain);
    }

    fn set_active_tool_number_from_manipulator(&mut self, stable_key: &'static str, value: f64) {
        let Some((default, domain)) = tool_number_spec(self.exact_tool, stable_key) else {
            return;
        };
        self.tool_inputs
            .numbers
            .entry((self.exact_tool, stable_key))
            .or_insert_with(|| RetainedToolNumber::new(default))
            .set_manipulator_value(value, domain);
    }

    /// First validation issue which blocks staging for the active recipe.
    #[must_use]
    pub fn active_tool_parameter_issue(&self) -> Option<ToolInputError> {
        let key_is_relevant = |key: &'static str| {
            !matches!(
                (self.exact_tool, key),
                (ToolVariant::RectangularPattern, "count_v" | "spacing_v")
                    if !self.active_tool_flag("second_direction").unwrap_or(false)
            ) && !matches!(
                (self.exact_tool, key),
                (ToolVariant::CircularPattern, "extent")
                    if self.active_tool_flag("full_circle").unwrap_or(true)
            )
        };
        if let Some(error) = self
            .exact_tool
            .descriptor()
            .inputs
            .iter()
            .filter(|field| key_is_relevant(field.stable_key))
            .find_map(|field| self.active_tool_input_error(field.stable_key))
        {
            return Some(error);
        }
        match self.exact_tool {
            ToolVariant::RectangularPattern => {
                let columns = self.active_tool_number("count_u")? as u16;
                let rows = if self.active_tool_flag("second_direction")? {
                    self.active_tool_number("count_v")? as u16
                } else {
                    1
                };
                let total = u32::from(columns) * u32::from(rows);
                (!(2..=256).contains(&total)).then_some(ToolInputError::PatternLimit)
            }
            ToolVariant::CentreToOuterPointSlot => {
                let length = self.active_tool_number("overall_length")?;
                let width = self.active_tool_number("width")?;
                (length <= width).then_some(ToolInputError::SlotLengthNotGreaterThanWidth)
            }
            _ => None,
        }
    }

    /// Tool changes clear an incomplete first click but never a pending edit.
    pub fn set_tool(&mut self, tool: SketchTool) -> bool {
        if self.pending.is_some() {
            return false;
        }
        if self.tool != tool {
            self.tool = tool;
            self.exact_tool = match tool {
                SketchTool::Select => ToolVariant::Select,
                SketchTool::Point => ToolVariant::Point,
                SketchTool::Line => ToolVariant::ChainedPolyline,
                SketchTool::CentreLine => ToolVariant::Centreline,
                SketchTool::Rectangle => ToolVariant::TwoPointRectangle,
                SketchTool::Circle => ToolVariant::CentrePointCircle,
                SketchTool::Arc => ToolVariant::CentreStartEndArc,
            };
            self.modifier_sources.clear();
            self.modifier_picks.clear();
            self.trim_hover_fragment = None;
            self.pattern_manipulator = None;
            self.clear_creation_draft();
        }
        true
    }

    /// Selects one exact registry tool while retaining the legacy canvas mode
    /// as a compatibility adapter for existing tests and gestures.
    pub fn set_exact_tool(&mut self, variant: ToolVariant) -> bool {
        if self.pending.is_some() {
            return false;
        }
        let tool = match variant {
            ToolVariant::Select
            | ToolVariant::Dimension
            | ToolVariant::Trim
            | ToolVariant::Fillet
            | ToolVariant::Chamfer
            | ToolVariant::TwoDistanceChamfer
            | ToolVariant::Offset
            | ToolVariant::RectangularPattern
            | ToolVariant::CircularPattern
            | ToolVariant::FixedRelation
            | ToolVariant::CoincidentRelation
            | ToolVariant::HorizontalRelation
            | ToolVariant::VerticalRelation
            | ToolVariant::DistanceRelation
            | ToolVariant::ParallelRelation
            | ToolVariant::PerpendicularRelation
            | ToolVariant::EqualLengthRelation
            | ToolVariant::TangentRelation
            | ToolVariant::CollinearRelation => SketchTool::Select,
            ToolVariant::Point => SketchTool::Point,
            ToolVariant::SingleLine
            | ToolVariant::ChainedPolyline
            | ToolVariant::FitPointSpline
            | ToolVariant::ControlVertexSpline => SketchTool::Line,
            ToolVariant::Centreline => SketchTool::CentreLine,
            ToolVariant::TwoPointRectangle | ToolVariant::CentrePointRectangle => {
                SketchTool::Rectangle
            }
            ToolVariant::CentrePointCircle | ToolVariant::TwoPointCircle => SketchTool::Circle,
            ToolVariant::CentreStartEndArc | ToolVariant::ThreePointArc => SketchTool::Arc,
            ToolVariant::InnerDiameterPolygon
            | ToolVariant::OuterDiameterPolygon
            | ToolVariant::Text
            | ToolVariant::TwoPointSlot
            | ToolVariant::CentreToOuterPointSlot => SketchTool::Point,
        };
        if self.exact_tool != variant {
            self.exact_tool = variant;
            self.tool = tool;
            self.modifier_sources.clear();
            self.modifier_picks.clear();
            // An operand was picked for the relation that was armed when it
            // was clicked. Carrying it into the next relation would complete
            // that one from a pick the user made for something else, and
            // carrying the refusal would explain a pick two tools ago.
            self.clear_relation_acquisition();
            self.dimension_operands.clear();
            self.relation_diagnostic = None;
            self.trim_hover_fragment = None;
            self.pattern_manipulator = None;
            self.dimension_pick = None;
            if matches!(
                variant,
                ToolVariant::RectangularPattern | ToolVariant::CircularPattern
            ) && let Some(selected) = self.selected
                && self
                    .core_by_ui
                    .get(&selected)
                    .is_some_and(|sources| !sources.is_empty())
            {
                self.modifier_sources
                    .extend(self.core_by_ui[&selected].iter().copied());
            }
            self.ensure_pattern_manipulator();
            self.clear_creation_draft();
        }
        true
    }

    #[must_use]
    pub const fn view(&self) -> SketchView {
        self.view
    }

    pub const fn view_mut(&mut self) -> &mut SketchView {
        &mut self.view
    }

    #[must_use]
    pub const fn snap_settings(&self) -> SnapSettings {
        self.snap
    }

    pub fn set_snap_settings(&mut self, settings: SnapSettings) {
        self.snap = settings;
    }

    /// Publishes the document's evaluated variables for numeric entries: a
    /// dimension box or recipe field can then name them in arithmetic, so a
    /// rectangle's width can be `plate_width / 2`. Values are canonical
    /// magnitudes; lengths arrive in millimetres.
    pub fn set_named_values(&mut self, values: BTreeMap<String, f64>) {
        if self.named_values != values {
            self.named_values = values;
        }
    }

    /// Evaluates one numeric entry over the published document variables —
    /// the same arithmetic the dimension boxes accept. Lengths come back in
    /// millimetres; a plain number passes straight through.
    #[must_use]
    pub fn evaluate_value_entry(&self, text: &str) -> Option<f64> {
        let trimmed = text.trim();
        trimmed
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .or_else(|| evaluate_named_expression(trimmed, &self.named_values))
    }

    /// Publishes the sketch support's analytic curves as snap references.
    ///
    /// Non-finite curves are dropped so a degenerate support can never move
    /// the pointer somewhere the user cannot see. Passing an empty slice
    /// restores plain sketch-only snapping.
    pub fn set_support_curves(&mut self, curves: &[SketchContextCurve]) {
        if self.support_curves.len() == curves.len()
            && self
                .support_curves
                .iter()
                .zip(curves)
                .all(|(current, next)| current == next)
        {
            return;
        }
        self.support_curves = curves
            .iter()
            .copied()
            .filter(|curve| curve.is_finite())
            .collect();
    }

    #[must_use]
    pub fn support_curves(&self) -> &[SketchContextCurve] {
        &self.support_curves
    }

    /// Requests that the next contextual sketch frame refit its projected body.
    ///
    /// This is presentation-only and has no effect when [`show`] is used
    /// without a [`SketchViewportContext`].
    pub fn request_context_fit(&mut self) {
        self.last_context_fit_key = None;
    }

    pub fn apply_prepared_context_view(&mut self, view: SketchView, key: SketchContextFitKey) {
        self.view = view;
        self.last_context_fit_key = Some(key);
    }

    #[must_use]
    pub fn entities(&self) -> &[SketchEntity] {
        &self.entities
    }

    #[must_use]
    pub fn entity_geometry(&self, id: SketchEntityId) -> Option<SketchGeometry> {
        self.entities
            .iter()
            .find(|entity| entity.id == id)
            .map(|entity| entity.geometry)
    }

    /// Authoritative persistent recipe graph mirrored by the interactive
    /// canvas. Evaluated canvas entities remain a presentation adapter.
    #[must_use]
    pub const fn authoring(&self) -> &CoreSketchDefinition {
        &self.authoring
    }

    /// Number of exact bounded arrangement cells selected for a subsequent
    /// profile operation.
    #[must_use]
    pub fn selected_region_count(&self) -> usize {
        self.analytic_regions.selected.len()
    }

    /// The analytic arrangement for the current authoring revision, rebuilt
    /// first if an edit has landed since it was last needed.
    ///
    /// Exposed so the model viewport can offer the very same cells this canvas
    /// selects. It takes `&mut self` deliberately: reading the cached field
    /// alone would hand out a one-edit-stale arrangement whenever this is
    /// called before the canvas next draws.
    pub fn refreshed_arrangement(&mut self) -> Option<&CoreSketchArrangement> {
        self.refresh_analytic_regions();
        self.analytic_regions.arrangement.as_ref()
    }

    /// Number of analytically certified bounded cells currently available for
    /// explicit profile selection.
    #[must_use]
    pub fn available_region_count(&self) -> usize {
        self.analytic_regions
            .arrangement
            .as_ref()
            .map_or(0, |arrangement| arrangement.cells.len())
    }

    /// Stable, deterministic signatures of the selected exact arrangement
    /// cells. These are suitable for late-bound document history.
    #[must_use]
    pub fn selected_region_signatures(&self) -> Vec<CoreRegionSignature> {
        self.analytic_regions.selected.iter().cloned().collect()
    }

    /// Canonical interior anchors of the deliberately selected cells, in
    /// sketch-plane coordinates. These are the very samples the
    /// model-viewport overlay regions anchor to, so a selection made on
    /// either canvas highlights on both regardless of where inside the cell
    /// the pick landed. The lone-cell fallback selection is excluded: it is
    /// a convenience for Extrude, not something the user picked.
    #[must_use]
    pub fn selected_region_canonical_anchors(&mut self) -> Vec<[f64; 2]> {
        self.refresh_analytic_regions();
        if !self.analytic_regions.explicit {
            return Vec::new();
        }
        let Some(arrangement) = self.analytic_regions.arrangement.as_ref() else {
            return Vec::new();
        };
        let precision = PrecisionPolicy::default();
        arrangement
            .cells
            .iter()
            .filter(|cell| self.analytic_regions.selected.contains(&cell.signature))
            .filter_map(|cell| arrangement.cell_interior_sample(cell, &precision))
            .map(|anchor| [anchor.u, anchor.v])
            .collect()
    }

    /// Clears the region selection, as a click on empty space does. The next
    /// authoring edit still restores the lone cell of a single-cell sketch,
    /// so Extrude keeps working without an explicit pick.
    pub fn clear_region_selection(&mut self) -> bool {
        self.clear_selected_regions()
    }

    #[must_use]
    pub fn hovered_region_signature(&self) -> Option<&CoreRegionSignature> {
        self.analytic_regions.hovered.as_ref()
    }

    /// Compiles the selected cell union directly from the exact analytic
    /// arrangement. Pending UI geometry and display sampling are excluded.
    #[must_use]
    pub fn selected_planar_profile(&self) -> Option<PlanarProfile2> {
        self.compile_selected_planar_profile()?
            .ok()
            .map(|selection| selection.profile)
    }

    /// Why the selected cell union does not compile, when it does not.
    #[must_use]
    pub fn selected_planar_profile_error(&self) -> Option<CoreProfileCompileError> {
        self.compile_selected_planar_profile()?.err()
    }

    fn compile_selected_planar_profile(
        &self,
    ) -> Option<Result<artificer_sketch::CompiledProfileSelection, CoreProfileCompileError>> {
        if self.pending.is_some() {
            return None;
        }
        let arrangement = self.analytic_regions.arrangement.as_ref()?;
        let signatures = self.selected_region_signatures();
        Some(compile_selected_profile(
            arrangement,
            &signatures,
            &PrecisionPolicy::default(),
        ))
    }

    /// Whether selected analytic material cells should be painted. A
    /// retirement-only candidate deliberately suppresses the live graph's
    /// stale cell fill while its retiring boundaries are shown in red.
    fn selected_region_fill_visible(&self) -> bool {
        !self.pending.as_ref().is_some_and(|pending| {
            pending.entities.is_empty() && !pending.retired_entities.is_empty()
        })
    }

    fn refresh_analytic_regions(&mut self) {
        let revision = self.authoring.revision();
        if self.analytic_regions.revision == Some(revision) {
            return;
        }
        let old_selected = std::mem::take(&mut self.analytic_regions.selected);
        let old_anchors = std::mem::take(&mut self.analytic_regions.selection_anchors);
        let precision = PrecisionPolicy::default();
        let arrangement = self
            .authoring
            .arrangement_inputs()
            .ok()
            .map(|inputs| build_arrangement(&inputs, &precision, CoreArrangementLimits::default()))
            .unwrap_or_else(|| {
                build_arrangement(&[], &precision, CoreArrangementLimits::default())
            });
        let boundary_tolerance = precision
            .linear_agreement
            .max(precision.modeling_resolution);
        let mut selected = BTreeSet::new();
        let mut anchors = BTreeMap::new();
        for signature in old_selected {
            if arrangement.cell(&signature).is_some() {
                if let Some(anchor) = old_anchors.get(&signature).copied() {
                    anchors.insert(signature.clone(), anchor);
                }
                selected.insert(signature);
                continue;
            }
            let Some(anchor) = old_anchors.get(&signature).copied() else {
                continue;
            };
            if arrangement.point_near_boundary(anchor, boundary_tolerance) {
                continue;
            }
            if let Some(repaired) = arrangement.cell_at_point(anchor, &precision) {
                selected.insert(repaired.signature.clone());
                anchors.insert(repaired.signature.clone(), anchor);
            }
        }
        let mut explicit = self.analytic_regions.explicit && !selected.is_empty();
        if selected.is_empty()
            && arrangement.cells.len() == 1
            && let Some(cell) = arrangement.cells.first()
        {
            selected.insert(cell.signature.clone());
            if let Some(anchor) = arrangement.cell_interior_sample(cell, &precision) {
                anchors.insert(cell.signature.clone(), anchor);
            }
            explicit = false;
        }
        self.analytic_regions = AnalyticRegionSelection {
            revision: Some(revision),
            arrangement: Some(arrangement),
            selected,
            selection_anchors: anchors,
            hovered: None,
            explicit,
        };
    }

    fn clear_selected_regions(&mut self) -> bool {
        let changed = !self.analytic_regions.selected.is_empty();
        self.analytic_regions.selected.clear();
        self.analytic_regions.selection_anchors.clear();
        self.analytic_regions.explicit = false;
        changed
    }

    /// Selects the exact bounded cell containing `point`. Additive selection
    /// toggles that cell; replacement selection clears every other cell.
    /// This renderer-independent seam is shared by pointer UI, automation,
    /// and headless modeling tests.
    pub fn select_region_at_point(&mut self, point: SketchPoint, additive: bool) -> bool {
        self.refresh_analytic_regions();
        let point = core_point(point);
        let signature = self
            .analytic_regions
            .arrangement
            .as_ref()
            .and_then(|arrangement| {
                arrangement
                    .cell_at_point(point, &PrecisionPolicy::default())
                    .map(|cell| cell.signature.clone())
            });
        let Some(signature) = signature else {
            return !additive && self.clear_selected_regions();
        };
        if additive {
            if self.analytic_regions.selected.remove(&signature) {
                self.analytic_regions.selection_anchors.remove(&signature);
            } else {
                self.analytic_regions.selected.insert(signature.clone());
                self.analytic_regions
                    .selection_anchors
                    .insert(signature, point);
            }
            self.analytic_regions.explicit = !self.analytic_regions.selected.is_empty();
            true
        } else {
            let unchanged = self.analytic_regions.selected.len() == 1
                && self.analytic_regions.selected.contains(&signature);
            self.analytic_regions.selected.clear();
            self.analytic_regions.selection_anchors.clear();
            self.analytic_regions.selected.insert(signature.clone());
            self.analytic_regions
                .selection_anchors
                .insert(signature, point);
            self.analytic_regions.explicit = true;
            !unchanged
        }
    }

    fn update_region_hover(&mut self, point: Option<SketchPoint>) -> bool {
        self.refresh_analytic_regions();
        let hovered = if self.pending.is_none() {
            point.and_then(|point| {
                self.analytic_regions
                    .arrangement
                    .as_ref()
                    .and_then(|arrangement| {
                        arrangement
                            .cell_at_point(core_point(point), &PrecisionPolicy::default())
                            .map(|cell| cell.signature.clone())
                    })
            })
        } else {
            None
        };
        let changed = self.analytic_regions.hovered != hovered;
        self.analytic_regions.hovered = hovered;
        changed
    }

    #[must_use]
    pub const fn selected(&self) -> Option<SketchEntityId> {
        self.selected
    }

    pub fn set_selected(&mut self, selected: Option<SketchEntityId>) -> bool {
        if selected.is_some_and(|id| !self.entities.iter().any(|entity| entity.id == id)) {
            return false;
        }
        let changed = self.selected != selected;
        self.selected = selected;
        if changed {
            // A new subject invalidates the previous pick's side identity;
            // the caller re-records it when the pick is a Dimension gesture.
            self.dimension_pick = None;
        }
        if changed && self.pending.is_none() {
            self.rebuild_selected_recipe_editor();
        }
        if matches!(
            self.exact_tool,
            ToolVariant::RectangularPattern | ToolVariant::CircularPattern
        ) {
            self.modifier_sources.clear();
            self.modifier_picks.clear();
            if let Some(selected) = selected
                && self
                    .core_by_ui
                    .get(&selected)
                    .is_some_and(|sources| !sources.is_empty())
            {
                self.modifier_sources
                    .extend(self.core_by_ui[&selected].iter().copied());
            }
        }
        changed
    }

    fn rebuild_selected_recipe_editor(&mut self) {
        self.selected_recipe_editor = self.selected.and_then(|subject| {
            let operation = self.operation_by_ui.get(&subject).copied().or_else(|| {
                self.core_by_ui
                    .get(&subject)
                    .and_then(|entities| entities.first())
                    .and_then(|entity| self.authoring.entity(*entity))
                    .map(|record| record.provenance.operation)
            })?;
            let recipe = self.authoring.operation(operation)?.recipe.clone();
            Some(selected_recipe_editor_for(subject, operation, recipe))
        });
    }

    /// Inspector projection for the selected committed authored feature.
    #[must_use]
    pub fn selected_recipe_editor(&self) -> Option<SelectedRecipeEditorView> {
        self.selected_recipe_editor
            .as_ref()
            .map(|editor| SelectedRecipeEditorView {
                title: editor.title,
                parameters: editor
                    .parameters
                    .iter()
                    .map(RetainedRecipeParameter::view)
                    .collect(),
                reference_note: editor.reference_note,
            })
    }

    /// The stable keys of the selected recipe's parameters that the Dimension
    /// tool can drive from the canvas, so a host knows which ones no dimension
    /// box will ever show. The tool arms a box only for the kinds the clicked
    /// geometry carries (`committed_dimension_parameter`): a rectangle's width
    /// and height, a circle's diameter, a line's length and angle, a fillet's
    /// radius, and a polygon edge's side. A slot's width, a chamfer's two
    /// distances, a polygon's diameter, side count and rotation, and a
    /// pattern's counts have no such box, and are reachable only elsewhere.
    #[must_use]
    pub fn selected_recipe_canvas_dimensionable_keys(&self) -> &'static [&'static str] {
        self.selected_recipe_editor.as_ref().map_or(&[], |editor| {
            canvas_dimensionable_keys(&editor.original_recipe)
        })
    }

    /// Stable core curve-output IDs owned by the selected recipe operation.
    /// This is intentionally model identity, not a canvas presentation ID.
    #[must_use]
    pub fn selected_recipe_output_ids(&self) -> Vec<u64> {
        let Some(operation) = self
            .selected_recipe_editor
            .as_ref()
            .and_then(|editor| self.authoring.operation(editor.operation))
        else {
            return Vec::new();
        };
        operation
            .outputs
            .values()
            .filter_map(|output| match output {
                CoreOutputRef::Curve(entity) => Some(entity.get()),
                CoreOutputRef::Point(_) => None,
            })
            .collect()
    }

    /// First retained problem in the selected feature editor, used by the
    /// universal confirmation rail to disable its green tick.
    #[must_use]
    pub fn selected_recipe_parameter_issue(&self) -> Option<RecipeParameterError> {
        self.selected_recipe_editor.as_ref().and_then(|editor| {
            editor
                .parameters
                .iter()
                .find_map(|parameter| parameter.error)
        })
    }

    /// Retains an inspector field's text and, when valid, rebuilds an exact
    /// replacement transaction from the live authoring graph. The returned
    /// subject is what the workbench binds to its universal tick/cross rail.
    /// Invalid text never replaces the last valid candidate.
    pub fn set_selected_recipe_parameter_text(
        &mut self,
        stable_key: &'static str,
        text: String,
    ) -> Option<SketchEntityId> {
        let (subject, operation, old_value, domain) = {
            let editor = self.selected_recipe_editor.as_mut()?;
            if self
                .pending
                .as_ref()
                .is_some_and(|pending| pending.subject != editor.subject)
            {
                return None;
            }
            let parameter = editor
                .parameters
                .iter_mut()
                .find(|parameter| parameter.stable_key == stable_key)?;
            if !parameter.is_text() {
                parameter.value?;
            }
            let old_value = parameter.value;
            parameter.text = text;
            parameter.error = None;
            (
                editor.subject,
                editor.operation,
                old_value,
                parameter.domain,
            )
        };

        // Free text is kept as typed; the recipe replay below decides
        // whether it can be set.
        let parsed = if matches!(domain, ToolNumberDomain::Text) {
            None
        } else {
            let named_values = &self.named_values;
            let editor = self.selected_recipe_editor.as_mut()?;
            let parameter = editor
                .parameters
                .iter_mut()
                .find(|parameter| parameter.stable_key == stable_key)?;
            // Plain numbers stay the fast path; anything else may name a
            // document variable — `plate_width / 2` — which evaluates first
            // and then faces the same domain rules a typed number would.
            let evaluated = validate_tool_number(&parameter.text, domain).or_else(|error| {
                evaluate_named_expression(&parameter.text, named_values)
                    .ok_or(error)
                    .and_then(|value| validate_tool_number(&value.to_string(), domain))
            });
            match evaluated {
                Ok(value) => {
                    parameter.value = Some(value);
                    Some(value)
                }
                Err(error) => {
                    parameter.error = Some(RecipeParameterError::Numeric(error));
                    return self.pending.as_ref().map(|pending| pending.subject);
                }
            }
        };

        let recipe = rebuilt_selected_recipe(self.selected_recipe_editor.as_ref()?).ok()?;
        let transaction = self
            .authoring
            .stage_replace(
                operation,
                recipe,
                "Edit sketch parameters",
                &Default::default(),
                PrecisionPolicy::default(),
            )
            .ok();
        let Some(transaction) = transaction else {
            let editor = self.selected_recipe_editor.as_mut()?;
            let parameter = editor
                .parameters
                .iter_mut()
                .find(|parameter| parameter.stable_key == stable_key)?;
            debug_assert_eq!(parameter.value, parsed);
            parameter.value = old_value;
            parameter.error = Some(RecipeParameterError::ReplayRejected);
            return self.pending.as_ref().map(|pending| pending.subject);
        };
        let presentation = self.core_transaction_presentation(&transaction).ok()?;

        if let Some(pending) = self.pending.as_mut() {
            if pending.subject != subject || pending.label != "Edit sketch parameters" {
                return None;
            }
            pending.entities = presentation.entities;
            pending.core_entities = presentation.core_entities;
            pending.retired_entities = presentation.retired_entities;
            pending.core_transaction = Some(transaction);
            pending.in_place = true;
            self.dimension_session = None;
            self.refresh_profile_analysis();
            return Some(subject);
        }

        self.stage_core_transaction_for_subject(
            transaction,
            "Edit sketch parameters",
            Some(subject),
            true,
        )
        .ok()
    }

    /// Whether a typed parameter value is previewing on the canvas. It is
    /// presentation only: no revision, identity, or undo entry exists until
    /// the value is accepted.
    #[must_use]
    pub fn selected_recipe_edit_pending(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.in_place)
    }

    /// Drops a typed parameter preview and puts every retained buffer back to
    /// what the committed recipe says. Escape means "as you were", including
    /// the text, whether or not the text ever parsed.
    pub fn revert_selected_recipe_edit(&mut self) -> bool {
        let reverted = self.selected_recipe_edit_pending();
        if reverted {
            self.cancel_pending();
        }
        self.rebuild_selected_recipe_editor();
        reverted
    }

    #[must_use]
    pub const fn hovered(&self) -> Option<SketchEntityId> {
        self.hovered
    }

    #[must_use]
    pub const fn creation_anchor(&self) -> Option<SketchPoint> {
        self.creation_anchor
    }

    /// Whether the user has authored an incomplete creation gesture.
    #[must_use]
    pub const fn creation_draft_blocks_modeling(&self) -> bool {
        self.arc_start.is_some()
            || self.creation_anchor.is_some()
            || !self.polyline_vertices.is_empty()
    }

    #[must_use]
    pub const fn arc_start(&self) -> Option<SketchPoint> {
        self.arc_start
    }

    #[must_use]
    pub const fn pointer_preview(&self) -> Option<SnapResult> {
        self.pointer_preview
    }

    #[must_use]
    pub fn pending(&self) -> Option<&PendingSketchEdit> {
        self.pending.as_ref()
    }

    #[must_use]
    pub const fn has_pending_edit(&self) -> bool {
        self.pending.is_some()
    }

    /// Live values for a draft/pending entity, or read-only values for the
    /// selected committed entity when no creation is active.
    #[must_use]
    pub fn dimension_readouts(&self) -> Vec<DimensionReadout> {
        if let Some(session) = &self.dimension_session {
            return session.readouts().collect();
        }
        if let Some((_, fields)) = exact_live_measurement(self) {
            return fields.into_iter().map(|field| field.readout).collect();
        }
        if let Some((_, fields)) = pending_single_curve_measurement(self) {
            return fields.into_iter().map(|field| field.readout).collect();
        }
        dimension_target(self).map_or_else(Vec::new, |(geometry, _)| {
            let phase = dimension_phase_for_geometry(geometry);
            dimension_fields_for_geometry(phase, geometry)
                .into_iter()
                .map(|mut field| {
                    // The canvas box and this card describe the same thing, so
                    // they take editability from the same predicate.
                    field.readout.editable =
                        committed_dimension_parameter(self, field.readout.kind).is_some();
                    field.readout.locked = false;
                    field.readout
                })
                .collect()
        })
    }

    #[must_use]
    pub fn active_dimension(&self) -> Option<SketchDimensionKind> {
        self.dimension_session
            .as_ref()
            .and_then(DimensionSession::active_kind)
    }

    #[must_use]
    pub fn dimension_editor_active(&self) -> bool {
        self.active_dimension().is_some()
    }

    #[must_use]
    pub fn dimension_error(&self) -> Option<DimensionInputError> {
        self.dimension_session
            .as_ref()
            .and_then(|session| session.error)
    }

    #[must_use]
    pub fn pending_geometry(&self) -> Option<SketchGeometry> {
        self.pending
            .as_ref()
            .and_then(|pending| pending.entities.first())
            .map(|entity| entity.geometry)
    }

    #[must_use]
    pub fn pending_entity_count(&self) -> usize {
        self.pending
            .as_ref()
            .map_or(0, |pending| pending.entities.len())
    }

    /// Translates the currently selected sketch entity by (delta_u, delta_v).
    pub fn translate_selected(&mut self, delta_u: f64, delta_v: f64) -> bool {
        self.reshape_selected(SketchDragHandle::Translate, delta_u, delta_v)
    }

    /// Reshapes or translates the currently selected sketch entity using the given drag handle and delta.
    pub fn reshape_selected(
        &mut self,
        handle: SketchDragHandle,
        delta_u: f64,
        delta_v: f64,
    ) -> bool {
        let Some(selected) = self.selected else {
            return false;
        };
        if !delta_u.is_finite() || !delta_v.is_finite() || (delta_u == 0.0 && delta_v == 0.0) {
            return false;
        }
        let mut any_reshaped = false;
        let op_id = self.operation_by_ui.get(&selected).copied().or_else(|| {
            self.core_by_ui
                .get(&selected)
                .and_then(|entities| entities.first())
                .and_then(|entity| self.authoring.entity(*entity))
                .map(|record| record.provenance.operation)
        });
        if let Some(op_id) = op_id
            && let Some(op) = self.authoring.operation(op_id)
        {
            let mut new_recipe = op.recipe.clone();
            reshape_core_recipe(&mut new_recipe, handle, delta_u, delta_v);
            if let Ok(tx) = self.authoring.stage_replace(
                op_id,
                new_recipe,
                "Reshape sketch entity",
                &Default::default(),
                PrecisionPolicy::default(),
            ) && self
                .authoring
                .commit(tx, CoreConfirmationSource::GreenTick)
                .is_ok()
            {
                any_reshaped = true;
            }
        }
        for entity in &mut self.entities {
            let matches_op =
                op_id.is_some() && self.operation_by_ui.get(&entity.id).copied() == op_id;
            if entity.id == selected || matches_op {
                entity.geometry = entity.geometry.reshape(handle, delta_u, delta_v);
                any_reshaped = true;
            }
        }
        if any_reshaped {
            self.refresh_profile_analysis();
            self.rebuild_selected_recipe_editor();
        }
        any_reshaped
    }

    /// Stages programmatically supplied geometry through the same UI gate.
    pub fn stage_geometry(
        &mut self,
        geometry: SketchGeometry,
    ) -> Result<SketchEntityId, SketchEditError> {
        self.stage_geometry_with_role(geometry, SketchEntityRole::Profile)
    }

    /// Stages geometry with an explicit material/construction role through the
    /// same global confirmation gate.
    pub fn stage_geometry_with_role(
        &mut self,
        geometry: SketchGeometry,
        role: SketchEntityRole,
    ) -> Result<SketchEntityId, SketchEditError> {
        if self.pending.is_some() {
            return Err(SketchEditError::PendingEditAlreadyExists);
        }
        if !geometry.is_finite() || !geometry_coordinates_supported(geometry) {
            return Err(SketchEditError::NonFiniteGeometry);
        }
        let id = SketchEntityId(self.next_entity_id);
        let entity = SketchEntity { id, geometry, role };
        self.pending = Some(PendingSketchEdit {
            subject: id,
            label: sketch_insert_label(entity),
            entities: vec![entity],
            core_transaction: None,
            core_entities: vec![None],
            retired_entities: Vec::new(),
            in_place: false,
        });
        let existing_draft = self.dimension_session.take().filter(|session| {
            session.target == DimensionTarget::Draft
                && dimension_phase_accepts_geometry(session.phase, geometry)
        });
        self.dimension_session = Some(if let Some(mut session) = existing_draft {
            session.target = DimensionTarget::Pending(id);
            session.geometry = geometry;
            session.fields = dimension_fields_for_geometry(session.phase, geometry);
            session.error = None;
            session
        } else {
            let serial = self.take_dimension_serial();
            DimensionSession::from_geometry(DimensionTarget::Pending(id), geometry, serial)
        });
        self.refresh_profile_analysis();
        Ok(id)
    }

    /// Stages one typed primitive recipe as an atomic multi-curve edit.
    pub fn stage_recipe(
        &mut self,
        recipe: CoreRecipe,
        label: &'static str,
    ) -> Result<SketchEntityId, SketchEditError> {
        if self.pending.is_some() {
            return Err(SketchEditError::PendingEditAlreadyExists);
        }
        let transaction = self
            .authoring
            .stage(recipe, label)
            .map_err(|_| SketchEditError::AuthoringRejected)?;
        self.stage_core_transaction(transaction, label)
    }

    /// Stages retirement of the selected authored operation. Compound UI
    /// adapters (rectangles, polygons, slots, patterns) carry one owner, so a
    /// single Delete retires their complete semantic output. Dependents are
    /// cascaded atomically by the core transaction instead of leaving dangling
    /// point or curve references.
    pub fn stage_delete_selected(&mut self) -> Result<SketchEntityId, SketchEditError> {
        if self.pending.is_some() {
            return Err(SketchEditError::PendingEditAlreadyExists);
        }
        let selected = self.selected.ok_or(SketchEditError::NoPendingEdit)?;
        let operation = self
            .operation_by_ui
            .get(&selected)
            .copied()
            .or_else(|| {
                self.core_by_ui
                    .get(&selected)
                    .and_then(|entities| entities.first())
                    .and_then(|entity| self.authoring.entity(*entity))
                    .map(|record| record.provenance.operation)
            })
            .ok_or(SketchEditError::AuthoringRejected)?;
        let transaction = self
            .authoring
            .stage_retire_operation(
                operation,
                CoreRetirementPolicy::CascadeDependents,
                "Delete sketch geometry",
                PrecisionPolicy::default(),
            )
            .map_err(|_| SketchEditError::AuthoringRejected)?;
        self.clear_creation_draft();
        self.stage_core_transaction_for_subject(
            transaction,
            "Delete sketch geometry",
            Some(selected),
            false,
        )
    }

    /// Converts an exact core candidate into one atomic presentation preview.
    /// Stable core/UI IDs are carried explicitly so reference-only points can
    /// never desynchronise later modifier selections.
    fn stage_core_transaction(
        &mut self,
        transaction: CoreTransaction,
        label: &'static str,
    ) -> Result<SketchEntityId, SketchEditError> {
        self.stage_core_transaction_for_subject(transaction, label, None, false)
    }

    /// Stages either an insertion/modifier or a retirement-only transaction.
    /// A retirement has no provisional output, so its selected presentation
    /// identity is carried separately through the universal confirmation gate.
    fn stage_core_transaction_for_subject(
        &mut self,
        transaction: CoreTransaction,
        label: &'static str,
        subject: Option<SketchEntityId>,
        in_place: bool,
    ) -> Result<SketchEntityId, SketchEditError> {
        if self.pending.is_some() {
            return Err(SketchEditError::PendingEditAlreadyExists);
        }
        let presentation = self.core_transaction_presentation(&transaction)?;
        let PendingCorePresentation {
            entities,
            core_entities,
            retired_entities,
        } = presentation;
        let subject = subject
            .or_else(|| entities.first().map(|entity| entity.id))
            .ok_or(SketchEditError::AuthoringRejected)?;
        if entities.is_empty() && !retired_entities.contains(&subject) {
            return Err(SketchEditError::AuthoringRejected);
        }
        self.pending = Some(PendingSketchEdit {
            subject,
            label,
            entities,
            core_transaction: Some(transaction),
            core_entities,
            retired_entities,
            in_place,
        });
        self.dimension_session = None;
        self.refresh_profile_analysis();
        Ok(subject)
    }

    fn core_transaction_presentation(
        &self,
        transaction: &CoreTransaction,
    ) -> Result<PendingCorePresentation, SketchEditError> {
        let impacted = transaction
            .impact()
            .changed_entities
            .iter()
            .chain(&transaction.impact().inserted_entities)
            .copied()
            .collect::<BTreeSet<_>>();
        let mut entities = Vec::with_capacity(impacted.len());
        let mut core_entities = Vec::with_capacity(impacted.len());
        let mut used_ui_entities = BTreeSet::new();
        let mut next_provisional_id = self.next_entity_id;
        for core_id in impacted {
            let record = transaction
                .preview()
                .entity(core_id)
                .ok_or(SketchEditError::AuthoringRejected)?;
            // Modifier impact reports include a consumed source as changed
            // because its active/tombstone state changed. It belongs in the
            // red retirement overlay below, but there is no active evaluated
            // curve to add to the green candidate overlay.
            if !record.active {
                continue;
            }
            let curve = transaction
                .preview()
                .evaluated_curve(core_id)
                .map_err(|_| SketchEditError::AuthoringRejected)?;
            let geometry = legacy_geometry_from_core(curve);
            if !geometry.is_finite() || geometry.is_degenerate() {
                return Err(SketchEditError::DegenerateGeometry);
            }
            // A changed semantic output keeps its presentation identity. A
            // legacy composite can map several core outputs to one UI entity;
            // the first keeps that identity and later curves receive fresh
            // IDs, intentionally exploding the adapter into exact outputs.
            let raw_id = self
                .ui_by_core
                .get(&core_id)
                .copied()
                .filter(|id| used_ui_entities.insert(*id))
                .map_or_else(
                    || {
                        while self
                            .entities
                            .iter()
                            .any(|entity| entity.id.get() == next_provisional_id)
                            || used_ui_entities
                                .iter()
                                .any(|entity| entity.get() == next_provisional_id)
                        {
                            next_provisional_id = next_provisional_id
                                .checked_add(1)
                                .ok_or(SketchEditError::AuthoringRejected)?;
                        }
                        let allocated = next_provisional_id;
                        next_provisional_id = next_provisional_id
                            .checked_add(1)
                            .ok_or(SketchEditError::AuthoringRejected)?;
                        used_ui_entities.insert(SketchEntityId(allocated));
                        Ok(allocated)
                    },
                    |id| Ok(id.get()),
                )?;
            entities.push(SketchEntity {
                id: SketchEntityId(raw_id),
                geometry,
                role: match record.role {
                    CoreEntityRole::Profile => SketchEntityRole::Profile,
                    CoreEntityRole::Construction => SketchEntityRole::Construction,
                    CoreEntityRole::Reference => SketchEntityRole::Reference,
                },
            });
            core_entities.push(Some(core_id));
        }
        let mut retired_entities = Vec::with_capacity(
            transaction.impact().changed_entities.len()
                + transaction.impact().retired_entities.len()
                + transaction.impact().superseded_entities.len(),
        );
        for core in &transaction.impact().changed_entities {
            if let Some(ui_entity) = self.ui_by_core.get(core).copied() {
                retired_entities.push(ui_entity);
            }
        }
        retired_entities.reserve(
            transaction.impact().retired_entities.len()
                + transaction.impact().superseded_entities.len(),
        );
        for core in transaction
            .impact()
            .retired_entities
            .iter()
            .chain(&transaction.impact().superseded_entities)
        {
            if let Some(ui_entity) = self.ui_by_core.get(core).copied() {
                retired_entities.push(ui_entity);
            }
        }
        retired_entities.sort_unstable();
        retired_entities.dedup();
        Ok(PendingCorePresentation {
            entities,
            core_entities,
            retired_entities,
        })
    }

    /// Replaces only the geometry carried by the current pending insertion.
    /// The document-local identity and outer confirmation operation remain
    /// stable while a dimension is edited.
    pub fn replace_pending_geometry(
        &mut self,
        id: SketchEntityId,
        geometry: SketchGeometry,
    ) -> Result<(), SketchEditError> {
        if !geometry.is_finite() || !geometry_coordinates_supported(geometry) {
            return Err(SketchEditError::NonFiniteGeometry);
        }
        let Some(pending) = self.pending.as_mut() else {
            return Err(SketchEditError::NoPendingEdit);
        };
        if pending.entities.len() != 1 || pending.core_transaction.is_some() {
            return Err(SketchEditError::NoPendingEdit);
        }
        let entity = &mut pending.entities[0];
        if entity.id != id {
            return Err(SketchEditError::NoPendingEdit);
        }
        entity.geometry = geometry;
        self.refresh_profile_analysis();
        Ok(())
    }

    /// Publishes a locally valid pending edit. Failure retains the pending edit.
    pub fn commit_pending(&mut self) -> Result<SketchEntityId, SketchEditError> {
        let pending = self
            .pending
            .as_ref()
            .ok_or(SketchEditError::NoPendingEdit)?;
        for entity in pending.entities() {
            if !entity.geometry.is_finite() {
                return Err(SketchEditError::NonFiniteGeometry);
            }
            if entity.geometry.is_degenerate() {
                return Err(SketchEditError::DegenerateGeometry);
            }
        }
        let mut pending = self
            .pending
            .take()
            .expect("the pending edit was checked above");
        let subject = pending.subject;
        let prepared_core_transaction = pending.core_transaction.is_some();
        let transaction = if let Some(transaction) = pending.core_transaction.take() {
            transaction
        } else {
            let Some(entity) = pending.entity() else {
                self.pending = Some(pending);
                return Err(SketchEditError::AuthoringRejected);
            };
            let Some(recipe) = core_recipe_for_entity(entity) else {
                self.pending = Some(pending);
                return Err(SketchEditError::AuthoringRejected);
            };
            match self.authoring.stage(recipe, pending.label) {
                Ok(transaction) => transaction,
                Err(_) => {
                    self.pending = Some(pending);
                    return Err(SketchEditError::AuthoringRejected);
                }
            }
        };
        let inserted_core_entities = transaction
            .impact()
            .inserted_entities
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let inserted_core_operations = transaction
            .impact()
            .inserted_operations
            .iter()
            .copied()
            .collect::<Vec<_>>();
        if self
            .undo_journal
            .confirm(
                &mut self.authoring,
                transaction,
                CoreConfirmationSource::BareEnter,
                PrecisionPolicy::default(),
            )
            .is_err()
        {
            self.pending = Some(pending);
            return Err(SketchEditError::AuthoringRejected);
        }
        let last = pending.entities.last().copied();

        // A modifier may retire only part of a legacy composite (for example
        // one rectangle side). Remove that presentation adapter and rebuild
        // any still-active sibling curves below, keeping core identities exact.
        for retired in &pending.retired_entities {
            self.entities.retain(|entity| entity.id != *retired);
            if let Some(core_ids) = self.core_by_ui.remove(retired) {
                for core_id in core_ids {
                    self.ui_by_core.remove(&core_id);
                }
            }
            self.operation_by_ui.remove(retired);
        }
        if let Some(maximum_inserted_id) =
            pending.entities.iter().map(|entity| entity.id.get()).max()
        {
            self.next_entity_id = self
                .next_entity_id
                .max(maximum_inserted_id.saturating_add(1));
        }
        self.entities.extend(pending.entities.iter().copied());
        if prepared_core_transaction {
            for (entity, core_id) in pending
                .entities
                .iter()
                .zip(pending.core_entities.iter().copied())
            {
                if let Some(core_id) = core_id {
                    self.core_by_ui.insert(entity.id, vec![core_id]);
                    self.ui_by_core.insert(core_id, entity.id);
                    if let Some(record) = self.authoring.entity(core_id) {
                        self.operation_by_ui
                            .insert(entity.id, record.provenance.operation);
                    }
                }
            }
        } else if pending.entities.len() == 1 {
            // The legacy rectangle adapter intentionally presents four core
            // sides as one selectable/dimensioned UI entity until an exact
            // modifier needs to split it.
            if !inserted_core_entities.is_empty() {
                self.core_by_ui
                    .insert(subject, inserted_core_entities.clone());
                for core_id in inserted_core_entities {
                    self.ui_by_core.insert(core_id, subject);
                }
            }
            if let Some(operation) = inserted_core_operations.first().copied() {
                self.operation_by_ui.insert(subject, operation);
            }
        } else if pending.entities.len() == inserted_core_entities.len() {
            for (entity, core_id) in pending.entities.iter().zip(inserted_core_entities) {
                self.core_by_ui.insert(entity.id, vec![core_id]);
                self.ui_by_core.insert(core_id, entity.id);
                if let Some(record) = self.authoring.entity(core_id) {
                    self.operation_by_ui
                        .insert(entity.id, record.provenance.operation);
                }
            }
        }
        self.reconcile_active_core_entities();
        // A relation moves points that existing curves already own, so their
        // cached presentation geometry is stale until it is re-read.
        self.refresh_presentation_geometry();
        self.selected = self
            .entities
            .iter()
            .any(|entity| entity.id == subject)
            .then_some(subject)
            .or_else(|| last.map(|entity| entity.id));
        self.rebuild_selected_recipe_editor();
        self.dimension_session = None;
        self.pointer_preview = None;
        self.creation_anchor = None;
        self.polyline_vertices.clear();
        self.polyline_current_segment_active = false;
        self.arc_start = None;
        self.trim_hover_fragment = None;
        self.pattern_manipulator = None;
        self.modifier_sources.clear();
        self.clear_relation_acquisition();
        self.modifier_picks.clear();
        self.refresh_profile_analysis();
        Ok(subject)
    }

    /// Ensures every active exact curve has one presentation entity after a
    /// modifier explodes a legacy composite. Existing one-to-one mappings and
    /// user-facing IDs remain stable.
    fn reconcile_active_core_entities(&mut self) {
        let missing = self
            .authoring
            .active_entities()
            .filter(|record| !self.ui_by_core.contains_key(&record.id))
            .map(|record| record.id)
            .collect::<Vec<_>>();
        for core_id in missing {
            let Some(record) = self.authoring.entity(core_id) else {
                continue;
            };
            let Ok(curve) = self.authoring.evaluated_curve(core_id) else {
                continue;
            };
            let id = SketchEntityId(self.next_entity_id);
            self.next_entity_id = self.next_entity_id.saturating_add(1);
            let entity = SketchEntity {
                id,
                geometry: legacy_geometry_from_core(curve),
                role: match record.role {
                    CoreEntityRole::Profile => SketchEntityRole::Profile,
                    CoreEntityRole::Construction => SketchEntityRole::Construction,
                    CoreEntityRole::Reference => SketchEntityRole::Reference,
                },
            };
            self.entities.push(entity);
            self.core_by_ui.insert(id, vec![core_id]);
            self.ui_by_core.insert(core_id, id);
            self.operation_by_ui.insert(id, record.provenance.operation);
        }
    }

    /// Rebuilds the presentation adapter from exact active graph records.
    /// This is used after persisted hydration and journal restores, where the
    /// graph snapshot—not a stale canvas cache—is authoritative.
    fn rebuild_presentation_from_authoring(&mut self) -> Result<(), SketchEditError> {
        let curves = self
            .authoring
            .active_entities()
            .map(|record| {
                let curve = self
                    .authoring
                    .evaluated_curve(record.id)
                    .map_err(|_| SketchEditError::AuthoringRejected)?;
                Ok((record.id, record.role, record.provenance.operation, curve))
            })
            .collect::<Result<Vec<_>, SketchEditError>>()?;
        let standalone_points = self
            .authoring
            .active_operations()
            .filter(|operation| matches!(operation.recipe, CoreRecipe::Point { .. }))
            .filter_map(|operation| {
                operation.outputs.values().find_map(|output| match output {
                    CoreOutputRef::Point(point) => self
                        .authoring
                        .point(*point)
                        .filter(|record| record.active)
                        .map(|record| (operation.id, record.evaluated_position)),
                    CoreOutputRef::Curve(_) => None,
                })
            })
            .collect::<Vec<_>>();

        self.entities.clear();
        self.core_by_ui.clear();
        self.ui_by_core.clear();
        self.operation_by_ui.clear();
        self.selected = None;
        self.selected_recipe_editor = None;
        self.hovered = None;
        self.modifier_sources.clear();
        self.clear_relation_acquisition();
        self.modifier_picks.clear();
        self.trim_hover_fragment = None;
        self.pattern_manipulator = None;
        self.clear_creation_draft();

        for (core_id, role, operation, curve) in curves {
            let geometry = legacy_geometry_from_core(curve);
            if !geometry.is_finite() || geometry.is_degenerate() {
                return Err(SketchEditError::AuthoringRejected);
            }
            let id = SketchEntityId(self.next_entity_id);
            self.next_entity_id = self.next_entity_id.saturating_add(1);
            self.entities.push(SketchEntity {
                id,
                geometry,
                role: match role {
                    CoreEntityRole::Profile => SketchEntityRole::Profile,
                    CoreEntityRole::Construction => SketchEntityRole::Construction,
                    CoreEntityRole::Reference => SketchEntityRole::Reference,
                },
            });
            self.core_by_ui.insert(id, vec![core_id]);
            self.ui_by_core.insert(core_id, id);
            self.operation_by_ui.insert(id, operation);
        }
        for (operation, position) in standalone_points {
            let id = SketchEntityId(self.next_entity_id);
            self.next_entity_id = self.next_entity_id.saturating_add(1);
            self.entities.push(SketchEntity {
                id,
                geometry: SketchGeometry::point(SketchPoint::new(position.u, position.v)),
                role: SketchEntityRole::Profile,
            });
            self.core_by_ui.insert(id, Vec::new());
            self.operation_by_ui.insert(id, operation);
        }
        self.refresh_profile_analysis();
        Ok(())
    }

    /// Discards only the staged edit. It never removes committed entities.
    pub fn cancel_pending(&mut self) -> Option<PendingSketchEdit> {
        let cancelled = self.pending.take();
        if cancelled.is_some() {
            self.trim_hover_fragment = None;
            self.pattern_manipulator = None;
            self.modifier_sources.clear();
            self.modifier_picks.clear();
            self.clear_creation_draft();
            self.rebuild_selected_recipe_editor();
            self.refresh_profile_analysis();
        }
        cancelled
    }

    pub fn clear_creation_draft(&mut self) {
        self.creation_anchor = None;
        self.polyline_vertices.clear();
        self.polyline_current_segment_active = false;
        self.arc_start = None;
        self.pointer_preview = None;
        self.dimension_session = None;
    }

    /// Whether an inspector can offer its explicit "Finish chain" action.
    /// Provisional vertices remain identity- and revision-neutral here.
    #[must_use]
    pub fn polyline_draft_can_finish(&self) -> bool {
        self.exact_tool == ToolVariant::ChainedPolyline
            && self.pending.is_none()
            && self.polyline_vertices.len() >= 2
    }

    /// Stages the accepted vertices of an open chained polyline as one exact
    /// recipe. The caller still owns the universal tick/Enter confirmation.
    /// No IDs, revisions, or undo entries are consumed before this call.
    pub fn finish_polyline_draft(&mut self) -> Result<SketchEntityId, SketchEditError> {
        self.stage_polyline_draft(false)
    }

    fn stage_polyline_draft(&mut self, closed: bool) -> Result<SketchEntityId, SketchEditError> {
        if self.exact_tool != ToolVariant::ChainedPolyline {
            return Err(SketchEditError::NoPendingEdit);
        }
        let minimum_vertices = if closed { 3 } else { 2 };
        if self.polyline_vertices.len() < minimum_vertices {
            return Err(SketchEditError::DegenerateGeometry);
        }
        let vertices = self
            .polyline_vertices
            .iter()
            .copied()
            .map(|point| CorePointInput::Position(core_point(point)))
            .collect();
        let subject = self.stage_recipe(
            CoreRecipe::Polyline {
                vertices,
                closed,
                construction: false,
            },
            "Add chained polyline",
        )?;
        self.clear_creation_draft();
        Ok(subject)
    }

    fn begin_polyline_segment(&mut self, start: SketchPoint) {
        self.creation_anchor = Some(start);
        self.polyline_current_segment_active = true;
        let serial = self.take_dimension_serial();
        self.dimension_session = Some(DimensionSession::new(
            DimensionTarget::Draft,
            DimensionPhase::Line,
            SketchGeometry::segment(start, start),
            serial,
        ));
    }

    fn accept_polyline_vertex(&mut self, pointer: SketchPoint) -> Option<SketchEntityId> {
        if self.pending.is_some() || self.exact_tool != ToolVariant::ChainedPolyline {
            return None;
        }
        if self.polyline_vertices.is_empty() {
            self.polyline_vertices.push(pointer);
            self.begin_polyline_segment(pointer);
            return None;
        }

        if self.polyline_vertices.len() >= 3
            && polyline_points_coincident(pointer, self.polyline_vertices[0])
        {
            return self.stage_polyline_draft(true).ok();
        }

        let start = *self
            .polyline_vertices
            .last()
            .expect("a non-empty polyline has a final vertex");
        if self.polyline_current_segment_active {
            self.update_dimension_pointer(pointer);
        }
        let end = self
            .dimension_session
            .as_ref()
            .filter(|_| self.polyline_current_segment_active)
            .and_then(|session| match session.geometry {
                SketchGeometry::Segment { end, .. } => Some(end),
                _ => None,
            })
            .unwrap_or(pointer);
        if polyline_points_coincident(start, end)
            || self
                .polyline_vertices
                .iter()
                .any(|existing| polyline_points_coincident(*existing, end))
        {
            return None;
        }
        self.polyline_vertices.push(end);
        self.begin_polyline_segment(end);
        None
    }

    fn finish_polyline_at_pointer(&mut self, point: SketchPoint) -> Option<SketchEntityId> {
        // egui reports a double-click on its second click; the first click has
        // already accepted this vertex on the preceding frame. Only acquire a
        // point here when the chain still lacks the two vertices required for
        // an open recipe, preventing a duplicate or tiny terminal segment.
        if self.polyline_vertices.len() < 2
            && let Some(subject) = self.accept_polyline_vertex(point)
        {
            return Some(subject);
        }
        self.finish_polyline_draft().ok()
    }

    /// Consumes one Escape layer from an atomic polyline gesture. A live
    /// numeric editor is handled before this method by the dimension widget;
    /// the next Escape removes only the pointer-owned segment and the
    /// following Escape discards the accepted local chain.
    fn cancel_polyline_layer(&mut self) -> bool {
        if self.pending.is_some()
            || self.exact_tool != ToolVariant::ChainedPolyline
            || self.polyline_vertices.is_empty()
        {
            return false;
        }
        if self.polyline_current_segment_active {
            self.polyline_current_segment_active = false;
            self.dimension_session = None;
            self.pointer_preview = None;
        } else {
            self.clear_creation_draft();
        }
        true
    }

    /// Removes exactly one accepted provisional segment without mutating the
    /// authoring graph. The first vertex is retained because it is an anchor,
    /// not a segment.
    fn backspace_polyline_segment(&mut self) -> bool {
        if self.pending.is_some()
            || self.exact_tool != ToolVariant::ChainedPolyline
            || self.polyline_vertices.is_empty()
        {
            return false;
        }
        if self.polyline_vertices.len() > 1 {
            self.polyline_vertices.pop();
        }
        let anchor = *self
            .polyline_vertices
            .last()
            .expect("the first polyline anchor is retained");
        self.pointer_preview = None;
        self.begin_polyline_segment(anchor);
        true
    }

    fn take_dimension_serial(&mut self) -> u64 {
        let serial = self.next_dimension_serial;
        self.next_dimension_serial = self.next_dimension_serial.saturating_add(1);
        serial
    }

    fn sync_dimension_pending(&mut self) {
        let pending_update = self.dimension_session.as_ref().and_then(|session| {
            let DimensionTarget::Pending(id) = session.target else {
                return None;
            };
            Some((id, session.geometry))
        });
        if let Some((id, geometry)) = pending_update
            && self.replace_pending_geometry(id, geometry).is_err()
            && let Some(session) = self.dimension_session.as_mut()
        {
            session.error = Some(DimensionInputError::DegenerateGeometry);
        }
    }

    fn update_dimension_pointer(&mut self, point: SketchPoint) {
        if self.exact_tool == ToolVariant::ChainedPolyline && !self.polyline_current_segment_active
        {
            return;
        }
        if self.exact_tool == ToolVariant::ThreePointArc
            && let (Some(first), Some(second)) = (self.creation_anchor, self.arc_start)
        {
            if self
                .dimension_session
                .as_ref()
                .is_some_and(|session| session.phase == DimensionPhase::ThreePointArc)
            {
                if let Some(session) = self.dimension_session.as_mut() {
                    session.update_pointer(point);
                }
            } else if let Some(session) = DimensionSession::three_point_arc(
                first,
                second,
                point,
                self.take_dimension_serial(),
            ) {
                self.dimension_session = Some(session);
            }
            return;
        }
        if self
            .dimension_session
            .as_ref()
            .is_some_and(|session| session.phase == DimensionPhase::SlotWidth)
            && let (Some(axis_start), Some(axis_end)) = (self.creation_anchor, self.arc_start)
        {
            let (direction_u, direction_v) =
                unit_direction(axis_start, axis_end).unwrap_or((1.0, 0.0));
            let relative_u = point.u - axis_start.u;
            let relative_v = point.v - axis_start.v;
            let along = relative_u.mul_add(direction_u, relative_v * direction_v);
            let projection = SketchPoint::new(
                direction_u.mul_add(along, axis_start.u),
                direction_v.mul_add(along, axis_start.v),
            );
            if let Some(session) = self.dimension_session.as_mut() {
                let width_locked = session
                    .field_index(SketchDimensionKind::Width)
                    .is_some_and(|index| session.fields[index].readout.locked);
                let raw_half_width = projection.distance_squared(point).sqrt();
                session.set_pointer_value(SketchDimensionKind::Width, raw_half_width * 2.0);
                let normal =
                    unit_direction(projection, point).unwrap_or((-direction_v, direction_u));
                let half_width = session.value(SketchDimensionKind::Width) * 0.5;
                session.geometry = SketchGeometry::segment(
                    projection,
                    if width_locked {
                        SketchPoint::new(
                            half_width.mul_add(normal.0, projection.u),
                            half_width.mul_add(normal.1, projection.v),
                        )
                    } else {
                        point
                    },
                );
            }
            self.sync_exact_tool_pointer_inputs(point);
            return;
        }
        if let Some(session) = self.dimension_session.as_mut() {
            session.update_pointer(point);
        }
        self.sync_exact_tool_pointer_inputs(point);
        self.sync_dimension_pending();
    }

    fn sync_exact_tool_pointer_inputs(&mut self, point: SketchPoint) {
        match self.exact_tool {
            ToolVariant::InnerDiameterPolygon | ToolVariant::OuterDiameterPolygon => {
                let Some(center) = self.creation_anchor else {
                    return;
                };
                let reference = self
                    .dimension_session
                    .as_ref()
                    .and_then(|session| match session.geometry {
                        SketchGeometry::Circle { rim, .. } => Some(rim),
                        _ => None,
                    })
                    .unwrap_or(point);
                let diameter = center.distance_squared(reference).sqrt() * 2.0;
                let mut rotation = (reference.v - center.v).atan2(reference.u - center.u);
                if self.exact_tool == ToolVariant::InnerDiameterPolygon {
                    rotation -= std::f64::consts::PI / f64::from(self.polygon_sides);
                }
                self.sync_active_tool_number(
                    if self.exact_tool == ToolVariant::InnerDiameterPolygon {
                        "inner_diameter"
                    } else {
                        "outer_diameter"
                    },
                    diameter,
                );
                self.sync_active_tool_number("rotation", rotation.to_degrees());
            }
            ToolVariant::TwoPointSlot | ToolVariant::CentreToOuterPointSlot => {
                let Some(axis_start) = self.creation_anchor else {
                    return;
                };
                let axis_end = self.arc_start.or_else(|| {
                    self.dimension_session
                        .as_ref()
                        .and_then(|session| match session.geometry {
                            SketchGeometry::Segment { end, .. } => Some(end),
                            _ => None,
                        })
                });
                let Some(axis_end) = axis_end else {
                    return;
                };
                let axis_length = axis_start.distance_squared(axis_end).sqrt();
                let angle = (axis_end.v - axis_start.v)
                    .atan2(axis_end.u - axis_start.u)
                    .to_degrees();
                if self.exact_tool == ToolVariant::TwoPointSlot {
                    self.sync_active_tool_number("centre_distance", axis_length);
                } else {
                    self.sync_active_tool_number("overall_length", axis_length * 2.0);
                }
                self.sync_active_tool_number("angle", angle);
                if let Some(width) = self
                    .dimension_session
                    .as_ref()
                    .filter(|session| session.phase == DimensionPhase::SlotWidth)
                    .map(|session| session.value(SketchDimensionKind::Width))
                {
                    self.sync_active_tool_number("width", width);
                }
            }
            ToolVariant::RectangularPattern => {
                if let Some(anchor) = self.pattern_anchor() {
                    self.sync_active_tool_number(
                        "spacing_u",
                        anchor.distance_squared(point).sqrt(),
                    );
                }
            }
            _ => {}
        }
    }

    fn polygon_reference_from_inputs(
        &self,
        center: SketchPoint,
        fallback: SketchPoint,
    ) -> SketchPoint {
        let diameter_key = if self.exact_tool == ToolVariant::InnerDiameterPolygon {
            "inner_diameter"
        } else {
            "outer_diameter"
        };
        let diameter = self
            .active_tool_number(diameter_key)
            .unwrap_or_else(|| center.distance_squared(fallback).sqrt() * 2.0);
        let fallback_angle = (fallback.v - center.v).atan2(fallback.u - center.u)
            - if self.exact_tool == ToolVariant::InnerDiameterPolygon {
                std::f64::consts::PI / f64::from(self.polygon_sides)
            } else {
                0.0
            };
        let rotation = self
            .active_tool_number("rotation")
            .map_or(fallback_angle, f64::to_radians);
        let reference_angle = rotation
            + if self.exact_tool == ToolVariant::InnerDiameterPolygon {
                std::f64::consts::PI / f64::from(self.polygon_sides)
            } else {
                0.0
            };
        let radius = diameter * 0.5;
        SketchPoint::new(
            radius.mul_add(reference_angle.cos(), center.u),
            radius.mul_add(reference_angle.sin(), center.v),
        )
    }

    fn slot_axis_from_inputs(
        &self,
        axis_start: SketchPoint,
        fallback_end: SketchPoint,
    ) -> SketchPoint {
        let fallback_length = axis_start.distance_squared(fallback_end).sqrt();
        let fallback_angle = (fallback_end.v - axis_start.v).atan2(fallback_end.u - axis_start.u);
        let axis_length = if self.exact_tool == ToolVariant::TwoPointSlot {
            self.active_tool_number("centre_distance")
                .unwrap_or(fallback_length)
        } else {
            self.active_tool_number("overall_length")
                .map_or(fallback_length, |overall| overall * 0.5)
        };
        let angle = self
            .active_tool_number("angle")
            .map_or(fallback_angle, f64::to_radians);
        SketchPoint::new(
            axis_length.mul_add(angle.cos(), axis_start.u),
            axis_length.mul_add(angle.sin(), axis_start.v),
        )
    }

    #[must_use]
    pub fn diagnostics(&self) -> ProfileDiagnostics {
        let mut result = ProfileDiagnostics::default();
        let visible_entities = self
            .entities
            .iter()
            .copied()
            .chain(
                self.pending
                    .iter()
                    .flat_map(|pending| pending.entities.iter().copied()),
            )
            .collect::<Vec<_>>();
        for entity in &visible_entities {
            if self.pending.as_ref().is_some_and(|pending| {
                pending
                    .entities
                    .iter()
                    .any(|pending| pending.id == entity.id)
            }) {
                result.pending_entities += 1;
            }
            if entity.geometry.is_degenerate() {
                result.degenerate_entities += 1;
            }
            match entity.geometry {
                SketchGeometry::Point(_) => result.isolated_points += 1,
                SketchGeometry::Segment { .. } => result.open_segments += 1,
                SketchGeometry::Rectangle { .. } => result.closed_rectangles += 1,
                SketchGeometry::Circle { .. } => result.closed_circles += 1,
                SketchGeometry::Arc { .. } => result.open_arcs += 1,
            }
        }
        let diagnostics = self.profile_analysis.diagnostics;
        result.certified_loops = diagnostics.closed_loops;
        result.material_regions = diagnostics.material_regions;
        result.profile_holes = diagnostics.holes;
        result.analytic_curves = diagnostics.analytic_curves;
        result.open_wire_components = diagnostics.open_components;
        result.branched_vertices = diagnostics.branched_vertices;
        result.intersecting_wires = diagnostics.intersections;
        result
    }

    /// Returns a conservative profile classification for committed geometry
    /// plus the visible pending edit, if any.
    #[must_use]
    pub const fn certified_profile_status(&self) -> CertifiedProfileStatus {
        self.certified_profile
    }

    /// Exports the exact committed polyline behind a certified closed profile.
    ///
    /// The repeated closing point is omitted for declarative kernel commands.
    /// Analytic circles, arcs, mixed geometry, pending edits, and unsupported
    /// multi-loop profiles are never approximated here.
    #[must_use]
    pub fn certified_polyline_vertices(&self) -> Option<Vec<SketchPoint>> {
        let profile = self.certified_sketch_profile()?;
        let linear_regions = profile.linear_regions()?;
        let [region] = linear_regions.as_slice() else {
            return None;
        };
        if !region.holes.is_empty() {
            return None;
        }
        Some(region.outer.clone())
    }

    /// Exports every exact committed material region in deterministic order.
    ///
    /// Pending entities participate in visible diagnostics but are never
    /// exportable modeling input. Analytic curves remain analytic in this API;
    /// callers must not substitute [`SketchGeometry::display_polyline`].
    #[must_use]
    pub fn certified_sketch_profile(&self) -> Option<CertifiedSketchProfile> {
        if self.pending.is_some() {
            return None;
        }
        self.profile_analysis.profile.clone()
    }

    /// The single line of prose the canvas shows for the active tool. It is a
    /// method rather than a local so a test can assert what the user is told
    /// without reading pixels.
    #[must_use]
    pub fn canvas_instruction(&self) -> &'static str {
        let state = self;
        let descriptor = state.exact_tool.descriptor();
        let progress = state.gesture_progress();
        if progress.awaiting_confirmation
            && matches!(
                state.exact_tool,
                ToolVariant::Fillet | ToolVariant::Chamfer | ToolVariant::TwoDistanceChamfer
            )
            && state.modifier_sources.is_empty()
        {
            "Corner preview · select another corner or press Enter / ✓"
        } else if progress.awaiting_confirmation
            && matches!(
                state.exact_tool,
                ToolVariant::Fillet | ToolVariant::Chamfer | ToolVariant::TwoDistanceChamfer
            )
        {
            "Corner preview · select the second curve or press Enter / ✓"
        } else if progress.awaiting_confirmation {
            "Pending sketch edit · Enter or ✓ to confirm · Esc to cancel"
        } else if state.exact_tool == ToolVariant::Dimension
            && first_armed_dimension_kind(state).is_some()
        {
            "Click a dimension and type an exact value · Enter applies · Esc reverts"
        } else if state.exact_tool == ToolVariant::Dimension && state.selected.is_some() {
            // The honest negative: a plain line, a point, or a free arc measures
            // itself, so nothing here drives a literal. Say so on the canvas
            // rather than leaving the tool looking broken.
            "This feature has no driving dimension · see the SELECTED FEATURE card"
        } else if matches!(
            state.exact_tool,
            ToolVariant::RectangularPattern | ToolVariant::CircularPattern
        ) && state.modifier_sources.is_empty()
        {
            "Select seed geometry · Shift-click adds or removes seed curves"
        } else if matches!(
            state.exact_tool,
            ToolVariant::RectangularPattern | ToolVariant::CircularPattern
        ) {
            "Drag square direction/centre handle · release to stage · Shift-click edits seeds"
        } else if state.exact_tool == ToolVariant::Select
            && state.pending.is_none()
            && (state.available_region_count() > 1 || state.selected_region_count() == 0)
            && state.available_region_count() > 0
        {
            // Several bounded cells, or none picked yet: the profile choice
            // is the thing this tool is for right now, and the fill only
            // follows a click, so say where to click.
            "Click inside a profile · Shift-click adds more"
        } else if descriptor.acquisition_phases.is_empty() {
            descriptor.prompt
        } else {
            descriptor
                .acquisition_phases
                .get(usize::from(progress.completed_points))
                .or_else(|| descriptor.acquisition_phases.last())
                .map_or(descriptor.prompt, |phase| phase.prompt)
        }
    }

    /// The geometry the canvas is showing for one entity. While a typed value
    /// previews, that is the candidate rather than the committed original, so
    /// a measured readout can never disagree with what is drawn.
    fn presented_entity(&self, id: SketchEntityId) -> Option<SketchEntity> {
        self.pending
            .as_ref()
            .filter(|pending| pending.in_place)
            .and_then(|pending| pending.entities.iter().find(|entity| entity.id == id))
            .or_else(|| self.entities.iter().find(|entity| entity.id == id))
            .copied()
    }

    /// Committed entities that a live in-place edit has already replaced.
    /// They must not paint under the candidate.
    fn superseded_by_in_place_edit(&self) -> &[SketchEntityId] {
        match self.pending.as_ref() {
            Some(pending) if pending.in_place => &pending.retired_entities,
            _ => &[],
        }
    }

    /// The entity the canvas should highlight as hovered. Trim owns its own
    /// hover presentation, so every entity pass has to suppress the ordinary
    /// highlight the same way.
    const fn hovered_for_paint(&self) -> Option<SketchEntityId> {
        match self.exact_tool {
            ToolVariant::Trim => None,
            _ => self.hovered,
        }
    }

    fn refresh_profile_analysis(&mut self) {
        let retired = self
            .pending
            .as_ref()
            .map(|pending| {
                pending
                    .retired_entities
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let entities = self
            .entities
            .iter()
            .copied()
            .filter(|entity| !retired.contains(&entity.id))
            .chain(
                self.pending
                    .iter()
                    .flat_map(|pending| pending.entities.iter().copied()),
            )
            .collect::<Vec<_>>();
        let analysis = analyze_profile_entities(&entities);
        self.certified_profile = analysis.status;
        self.profile_analysis = analysis;
        self.refresh_analytic_regions();
    }

    #[must_use]
    pub fn snap_point(&self, rect: Rect, screen_position: Pos2) -> SnapResult {
        let raw = self.view.screen_to_sketch(rect, screen_position);
        if !self.snap.enabled {
            return SnapResult {
                point: raw,
                kind: SnapKind::None,
            };
        }

        let model_radius = f64::from(self.snap.endpoint_radius_points) / self.view.points_per_unit;
        if self.exact_tool == ToolVariant::ChainedPolyline
            && self.polyline_vertices.len() >= 3
            && let Some(first) = self.polyline_vertices.first().copied()
            && self
                .view
                .sketch_to_screen(rect, first)
                .distance_sq(screen_position)
                <= self.snap.endpoint_radius_points * self.snap.endpoint_radius_points
        {
            // Provisional vertices have no stable ID by design. `None` keeps
            // that identity-neutral contract while the marker still makes the
            // close target visible.
            return SnapResult {
                point: first,
                kind: SnapKind::None,
            };
        }
        if let Some(candidate) = query_snap_candidates(
            &self.authoring,
            CorePoint2::new(raw.u, raw.v),
            model_radius,
            &PrecisionPolicy::default(),
            32,
        )
        .into_iter()
        .next()
        {
            let entity = |id: artificer_sketch::SketchEntityId| SketchEntityId(id.get());
            let kind = match candidate.key {
                CoreSnapKey::Endpoint { entity: id, .. } => SnapKind::Endpoint(entity(id)),
                CoreSnapKey::Intersection {
                    first_entity,
                    second_entity,
                    ..
                } => SnapKind::Intersection(entity(first_entity), entity(second_entity)),
                CoreSnapKey::Center { entity: id } => SnapKind::Center(entity(id)),
                CoreSnapKey::Midpoint { entity: id } => SnapKind::Midpoint(entity(id)),
                CoreSnapKey::Quadrant { entity: id, index } => {
                    SnapKind::Quadrant(entity(id), index)
                }
                CoreSnapKey::OnCurve { entity: id } => SnapKind::OnCurve(entity(id)),
            };
            return SnapResult {
                point: SketchPoint::new(candidate.point.u, candidate.point.v),
                kind,
            };
        }

        let radius_squared = self.snap.endpoint_radius_points * self.snap.endpoint_radius_points;
        let mut closest_endpoint = None::<(f32, SketchEntityId, SketchPoint)>;
        for entity in &self.entities {
            for point in entity.geometry.control_points().iter() {
                let screen = self.view.sketch_to_screen(rect, point);
                let distance_squared = screen.distance_sq(screen_position);
                let is_closer = closest_endpoint
                    .is_none_or(|(best_distance, _, _)| distance_squared < best_distance);
                if distance_squared <= radius_squared && is_closer {
                    closest_endpoint = Some((distance_squared, entity.id, point));
                }
            }
        }
        if let Some((_, entity, point)) = closest_endpoint {
            return SnapResult {
                point,
                kind: SnapKind::Endpoint(entity),
            };
        }

        // Authored geometry outranks the support it is drawn on, so reference
        // snapping only runs once no sketch entity claimed the pointer.
        if let Some(support) = self.support_snap(rect, screen_position) {
            return support;
        }

        SnapResult {
            point: self
                .snap
                .snap_to_visible_grid(raw, self.view.points_per_unit),
            kind: SnapKind::Grid,
        }
    }

    /// Nearest snap on the sketch support's own edges, if the pointer is close.
    ///
    /// Distances are compared in screen points so the capture radius stays
    /// constant while zooming. A named point always beats merely lying on an
    /// edge, however close the edge is; otherwise a boundary would shadow its
    /// own corners. Lying on an edge also uses a tighter radius so tracing an
    /// outline never blocks free placement beside it.
    fn support_snap(&self, rect: Rect, screen_position: Pos2) -> Option<SnapResult> {
        if self.support_curves.is_empty() {
            return None;
        }
        /// Screen-distance slack, squared, below which two candidates are the
        /// same location. The kernel splits a closed hole at a seam, so its
        /// quadrants land exactly on arc endpoints and midpoints; naming that
        /// shared point is a ranking question, not a distance one.
        const COINCIDENT_TOLERANCE: f32 = 0.01;

        let point_radius_squared =
            self.snap.endpoint_radius_points * self.snap.endpoint_radius_points;
        let mut best = None::<(u8, f32, SnapResult)>;
        let mut consider = |rank: u8, kind: SnapKind, point: SketchPoint| {
            if !point.is_finite() {
                return;
            }
            let distance_squared = self
                .view
                .sketch_to_screen(rect, point)
                .distance_sq(screen_position);
            if distance_squared > point_radius_squared {
                return;
            }
            let is_better = best.as_ref().is_none_or(|(best_rank, best_distance, _)| {
                if (distance_squared - best_distance).abs() <= COINCIDENT_TOLERANCE {
                    rank < *best_rank
                } else {
                    distance_squared < *best_distance
                }
            });
            if is_better {
                best = Some((rank, distance_squared, SnapResult { point, kind }));
            }
        };

        for curve in &self.support_curves {
            // A circle's centre is unambiguous; its quadrants describe a seam
            // vertex better than "endpoint" does; a midpoint is the weakest
            // name a point on a curve can carry.
            if let Some(center) = curve.center() {
                consider(0, SnapKind::SupportCenter, center);
            }
            for quadrant in curve.quadrants() {
                consider(1, SnapKind::SupportQuadrant, quadrant);
            }
            if let Some(endpoints) = curve.endpoints() {
                for endpoint in endpoints {
                    consider(2, SnapKind::SupportEndpoint, endpoint);
                }
            }
            if let Some(midpoint) = curve.midpoint() {
                consider(3, SnapKind::SupportMidpoint, midpoint);
            }
        }
        if let Some((_, _, result)) = best {
            return Some(result);
        }

        let edge_radius = self.snap.endpoint_radius_points * SUPPORT_EDGE_RADIUS_RATIO;
        let edge_radius_squared = edge_radius * edge_radius;
        let pointer = self.view.screen_to_sketch(rect, screen_position);
        self.support_curves
            .iter()
            .filter_map(|curve| curve.closest_point(pointer))
            .filter(|point| point.is_finite())
            .map(|point| {
                let distance_squared = self
                    .view
                    .sketch_to_screen(rect, point)
                    .distance_sq(screen_position);
                (distance_squared, point)
            })
            .filter(|(distance_squared, _)| *distance_squared <= edge_radius_squared)
            .min_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, point)| SnapResult {
                point,
                kind: SnapKind::SupportEdge,
            })
    }

    fn handle_creation_click(&mut self, point: SketchPoint) -> Option<SketchEntityId> {
        if self.pending.is_some() {
            return None;
        }
        match self.exact_tool {
            ToolVariant::Select
            | ToolVariant::Dimension
            | ToolVariant::Trim
            | ToolVariant::Fillet
            | ToolVariant::Chamfer
            | ToolVariant::TwoDistanceChamfer
            | ToolVariant::Offset
            | ToolVariant::RectangularPattern
            | ToolVariant::CircularPattern
            | ToolVariant::FixedRelation
            | ToolVariant::CoincidentRelation
            | ToolVariant::HorizontalRelation
            | ToolVariant::VerticalRelation
            | ToolVariant::DistanceRelation
            | ToolVariant::ParallelRelation
            | ToolVariant::PerpendicularRelation
            | ToolVariant::EqualLengthRelation
            | ToolVariant::TangentRelation
            | ToolVariant::CollinearRelation
            | ToolVariant::FitPointSpline
            | ToolVariant::ControlVertexSpline => None,
            ToolVariant::Point => self.stage_geometry(SketchGeometry::point(point)).ok(),
            ToolVariant::Text => {
                if self.active_tool_parameter_issue().is_some() {
                    return None;
                }
                let content = self.active_tool_text("content")?;
                let height = self.active_tool_number("height")?;
                let angle = self.active_tool_number("angle")?.to_radians();
                let recipe = text_recipe(point, &content, height, angle)?;
                self.stage_recipe(recipe, "Add sketch text").ok()
            }
            ToolVariant::SingleLine => {
                if let Some(start) = self.creation_anchor.take() {
                    self.update_dimension_pointer(point);
                    let geometry = self.dimension_session.as_ref().map_or_else(
                        || SketchGeometry::segment(start, point),
                        |session| session.geometry,
                    );
                    self.stage_geometry(geometry).ok()
                } else {
                    self.creation_anchor = Some(point);
                    let serial = self.take_dimension_serial();
                    self.dimension_session = Some(DimensionSession::new(
                        DimensionTarget::Draft,
                        DimensionPhase::Line,
                        SketchGeometry::segment(point, point),
                        serial,
                    ));
                    None
                }
            }
            ToolVariant::ChainedPolyline => self.accept_polyline_vertex(point),
            ToolVariant::Centreline => {
                if let Some(start) = self.creation_anchor.take() {
                    self.update_dimension_pointer(point);
                    let geometry = self.dimension_session.as_ref().map_or_else(
                        || SketchGeometry::segment(start, point),
                        |session| session.geometry,
                    );
                    self.stage_geometry_with_role(geometry, SketchEntityRole::Construction)
                        .ok()
                } else {
                    self.creation_anchor = Some(point);
                    let serial = self.take_dimension_serial();
                    self.dimension_session = Some(DimensionSession::new(
                        DimensionTarget::Draft,
                        DimensionPhase::Line,
                        SketchGeometry::segment(point, point),
                        serial,
                    ));
                    None
                }
            }
            ToolVariant::TwoPointRectangle => {
                if let Some(first) = self.creation_anchor.take() {
                    self.update_dimension_pointer(point);
                    let geometry = self.dimension_session.as_ref().map_or_else(
                        || SketchGeometry::rectangle(first, point),
                        |session| session.geometry,
                    );
                    self.stage_geometry(geometry).ok()
                } else {
                    self.creation_anchor = Some(point);
                    let serial = self.take_dimension_serial();
                    self.dimension_session = Some(DimensionSession::new(
                        DimensionTarget::Draft,
                        DimensionPhase::Rectangle,
                        SketchGeometry::rectangle(point, point),
                        serial,
                    ));
                    None
                }
            }
            ToolVariant::CentrePointRectangle => {
                if self.creation_anchor.is_some() {
                    self.update_dimension_pointer(point);
                    let geometry = self.dimension_session.as_ref()?.geometry;
                    let recipe = centre_point_rectangle_recipe_from_geometry(geometry)?;
                    let staged = self.stage_recipe(recipe, "Add centre-point rectangle").ok();
                    if staged.is_some() {
                        self.creation_anchor = None;
                    }
                    staged
                } else {
                    self.creation_anchor = Some(point);
                    let serial = self.take_dimension_serial();
                    self.dimension_session = Some(DimensionSession::new(
                        DimensionTarget::Draft,
                        DimensionPhase::CentreRectangle,
                        SketchGeometry::rectangle(point, point),
                        serial,
                    ));
                    None
                }
            }
            ToolVariant::CentrePointCircle => {
                if let Some(center) = self.creation_anchor.take() {
                    self.update_dimension_pointer(point);
                    let geometry = self.dimension_session.as_ref().map_or_else(
                        || SketchGeometry::circle(center, point),
                        |session| session.geometry,
                    );
                    self.stage_geometry(geometry).ok()
                } else {
                    self.creation_anchor = Some(point);
                    let serial = self.take_dimension_serial();
                    self.dimension_session = Some(DimensionSession::new(
                        DimensionTarget::Draft,
                        DimensionPhase::Circle,
                        SketchGeometry::circle(point, point),
                        serial,
                    ));
                    None
                }
            }
            ToolVariant::TwoPointCircle => {
                if self.creation_anchor.is_some() {
                    self.update_dimension_pointer(point);
                    let geometry = self.dimension_session.as_ref()?.geometry;
                    let (first, second) = two_point_circle_endpoints(geometry)?;
                    let staged = self
                        .stage_recipe(
                            two_point_circle_recipe(first, second),
                            "Add two-point circle",
                        )
                        .ok();
                    if staged.is_some() {
                        self.creation_anchor = None;
                    }
                    staged
                } else {
                    self.creation_anchor = Some(point);
                    let serial = self.take_dimension_serial();
                    self.dimension_session = Some(DimensionSession::new(
                        DimensionTarget::Draft,
                        DimensionPhase::TwoPointCircle,
                        SketchGeometry::circle(point, point),
                        serial,
                    ));
                    None
                }
            }
            ToolVariant::CentreStartEndArc => {
                if let (Some(center), Some(start)) = (self.creation_anchor, self.arc_start) {
                    self.update_dimension_pointer(point);
                    self.creation_anchor = None;
                    self.arc_start = None;
                    let geometry = self.dimension_session.as_ref().map_or_else(
                        || {
                            let end = arc_endpoint(center, start, point);
                            SketchGeometry::arc(center, start, end)
                        },
                        |session| session.geometry,
                    );
                    self.stage_geometry(geometry).ok()
                } else if self.creation_anchor.is_some() {
                    self.update_dimension_pointer(point);
                    let (center, canonical_start, radius, radius_locked) = self
                        .dimension_session
                        .as_ref()
                        .and_then(|session| {
                            let SketchGeometry::Segment { start, end } = session.geometry else {
                                return None;
                            };
                            let radius_field = session
                                .field_index(SketchDimensionKind::Radius)
                                .map(|index| session.fields[index].readout);
                            Some((
                                start,
                                end,
                                radius_field.map_or(0.0, |field| field.value),
                                radius_field.is_some_and(|field| field.locked),
                            ))
                        })
                        .unwrap_or((
                            self.creation_anchor
                                .expect("the arc center was checked above"),
                            point,
                            self.creation_anchor
                                .expect("the arc center was checked above")
                                .distance_squared(point)
                                .sqrt(),
                            false,
                        ));
                    self.arc_start = Some(canonical_start);
                    let serial = self.take_dimension_serial();
                    let mut session = DimensionSession::new(
                        DimensionTarget::Draft,
                        DimensionPhase::ArcSweep,
                        SketchGeometry::arc(center, canonical_start, canonical_start),
                        serial,
                    );
                    if let Some(index) = session.field_index(SketchDimensionKind::Radius) {
                        session.fields[index].readout.value = radius;
                        session.fields[index].readout.locked = radius_locked;
                    }
                    self.dimension_session = Some(session);
                    None
                } else {
                    self.creation_anchor = Some(point);
                    let serial = self.take_dimension_serial();
                    self.dimension_session = Some(DimensionSession::arc_radius(point, serial));
                    None
                }
            }
            ToolVariant::ThreePointArc => {
                if self.creation_anchor.is_some() && self.arc_start.is_some() {
                    self.update_dimension_pointer(point);
                    let recipe = self
                        .dimension_session
                        .as_ref()
                        .and_then(DimensionSession::three_point_arc_recipe)?;
                    let staged = self.stage_recipe(recipe, "Add three-point arc").ok();
                    if staged.is_some() {
                        self.creation_anchor = None;
                        self.arc_start = None;
                    }
                    staged
                } else if self.creation_anchor.is_some() {
                    self.arc_start = Some(point);
                    self.dimension_session = None;
                    None
                } else {
                    self.creation_anchor = Some(point);
                    self.dimension_session = None;
                    None
                }
            }
            ToolVariant::InnerDiameterPolygon | ToolVariant::OuterDiameterPolygon => {
                if let Some(center) = self.creation_anchor {
                    self.update_dimension_pointer(point);
                    if self.active_tool_parameter_issue().is_some() {
                        return None;
                    }
                    let fallback_reference = self
                        .dimension_session
                        .as_ref()
                        .and_then(|session| match session.geometry {
                            SketchGeometry::Circle { rim, .. } => Some(rim),
                            _ => None,
                        })
                        .unwrap_or(point);
                    let reference = self.polygon_reference_from_inputs(center, fallback_reference);
                    let recipe = regular_polygon_recipe(
                        self.exact_tool,
                        center,
                        reference,
                        self.polygon_sides,
                    )?;
                    let staged = self.stage_recipe(recipe, "Add regular polygon").ok();
                    if staged.is_some() {
                        self.creation_anchor = None;
                    }
                    staged
                } else {
                    self.creation_anchor = Some(point);
                    let serial = self.take_dimension_serial();
                    self.dimension_session = Some(DimensionSession::new(
                        DimensionTarget::Draft,
                        DimensionPhase::Circle,
                        SketchGeometry::circle(point, point),
                        serial,
                    ));
                    None
                }
            }
            ToolVariant::TwoPointSlot | ToolVariant::CentreToOuterPointSlot => {
                if let (Some(axis_start), Some(axis_end)) = (self.creation_anchor, self.arc_start) {
                    self.update_dimension_pointer(point);
                    if self.active_tool_parameter_issue().is_some() {
                        return None;
                    }
                    let axis_end = self.slot_axis_from_inputs(axis_start, axis_end);
                    let width = self.active_tool_number("width").or_else(|| {
                        self.dimension_session
                            .as_ref()
                            .map(|session| session.value(SketchDimensionKind::Width))
                    })?;
                    let recipe = match self.exact_tool {
                        ToolVariant::TwoPointSlot => {
                            two_point_slot_recipe(axis_start, axis_end, width)?
                        }
                        ToolVariant::CentreToOuterPointSlot => {
                            centre_outer_point_slot_recipe(axis_start, axis_end, width)?
                        }
                        _ => unreachable!("the outer match limits slot variants"),
                    };
                    let staged = self.stage_recipe(recipe, "Add sketch slot").ok();
                    if staged.is_some() {
                        self.creation_anchor = None;
                        self.arc_start = None;
                    }
                    staged
                } else if let Some(axis_start) = self.creation_anchor {
                    self.update_dimension_pointer(point);
                    let fallback_axis_end = self
                        .dimension_session
                        .as_ref()
                        .and_then(|session| match session.geometry {
                            SketchGeometry::Segment { end, .. } => Some(end),
                            _ => None,
                        })
                        .unwrap_or(point);
                    let axis_end = self.slot_axis_from_inputs(axis_start, fallback_axis_end);
                    self.arc_start = Some(axis_end);
                    let serial = self.take_dimension_serial();
                    self.dimension_session = Some(DimensionSession::new(
                        DimensionTarget::Draft,
                        DimensionPhase::SlotWidth,
                        SketchGeometry::segment(axis_start, axis_start),
                        serial,
                    ));
                    None
                } else {
                    self.creation_anchor = Some(point);
                    let serial = self.take_dimension_serial();
                    self.dimension_session = Some(DimensionSession::new(
                        DimensionTarget::Draft,
                        DimensionPhase::Line,
                        SketchGeometry::segment(point, point),
                        serial,
                    ));
                    None
                }
            }
        }
    }

    fn exact_curve_hit_in(
        definition: &CoreSketchDefinition,
        point: SketchPoint,
        radius: f64,
    ) -> Option<CoreEntityId> {
        hit_test_curves(
            definition,
            core_point(point),
            radius.max(PrecisionPolicy::default().modeling_resolution),
        )
        .into_iter()
        .next()
        .map(|hit| hit.entity)
    }

    fn exact_curve_hit(&self, point: SketchPoint, radius: f64) -> Option<CoreEntityId> {
        Self::exact_curve_hit_in(&self.authoring, point, radius)
    }

    /// How many curves the highlighted chain holds, for tests and diagnostics.
    #[must_use]
    pub fn offset_hover_count(&self) -> usize {
        self.offset_hover.len()
    }

    /// Highlights the chain a click would take, and reports whether it moved.
    fn update_offset_hover(&mut self, point: Option<SketchPoint>, pick_radius: f64) -> bool {
        let next = point
            .filter(|_| self.exact_tool == ToolVariant::Offset && self.pending.is_none())
            .and_then(|point| {
                let picked = self.exact_curve_hit(point, pick_radius)?;
                let chain = self.offset_chain_for(picked)?;
                Some(chain_geometry(&self.authoring, &chain).ok()?.curves)
            })
            .unwrap_or_default();
        let changed = next != self.offset_hover;
        self.offset_hover = next;
        changed
    }

    fn update_trim_hover(&mut self, point: Option<SketchPoint>, pick_radius: f64) -> bool {
        let previous = self.trim_hover_fragment.clone();
        let next = point.and_then(|point| {
            if self.exact_tool != ToolVariant::Trim {
                return None;
            }
            let definition = self
                .pending
                .as_ref()
                .and_then(|pending| pending.core_transaction.as_ref())
                .filter(|transaction| {
                    transaction
                        .preview()
                        .operations()
                        .last()
                        .is_some_and(|operation| {
                            matches!(&operation.recipe, CoreRecipe::Trim { .. })
                        })
                })
                .map_or(&self.authoring, CoreTransaction::preview);
            let target = Self::exact_curve_hit_in(definition, point, pick_radius)?;
            let target_record = definition.entity(target)?;
            let target_curve = definition.evaluated_curve(target).ok()?;
            let limits = definition
                .active_entities()
                .filter(|record| {
                    record.id != target && record.visible && record.role == target_record.role
                })
                .map(|record| {
                    definition
                        .evaluated_curve(record.id)
                        .ok()
                        .map(|curve| CoreTrimCurve {
                            entity: record.id,
                            curve,
                        })
                })
                .collect::<Option<Vec<_>>>()?;
            select_trim_span(
                CoreTrimCurve {
                    entity: target,
                    curve: target_curve,
                },
                &limits,
                core_point(point),
                &PrecisionPolicy::default(),
                MAX_CURVE_EDITS_PER_TRANSACTION,
            )
            .ok()
            .map(|selection| selection.removed.curve)
        });
        self.trim_hover_fragment = next.clone();
        previous != next
    }

    fn pattern_manipulator_kind(&self) -> Option<PatternManipulatorKind> {
        match self.exact_tool {
            ToolVariant::RectangularPattern => Some(PatternManipulatorKind::RectangularDirection),
            ToolVariant::CircularPattern => Some(PatternManipulatorKind::CircularCenter),
            _ => None,
        }
    }

    fn default_pattern_manipulator_position(&self) -> Option<SketchPoint> {
        let anchor = self.pattern_anchor()?;
        match self.pattern_manipulator_kind()? {
            PatternManipulatorKind::RectangularDirection => {
                let spacing = self
                    .active_tool_number("spacing_u")
                    .unwrap_or(DEFAULT_TOOL_LENGTH)
                    .abs()
                    .max(PrecisionPolicy::default().min_feature_size * 2.0);
                Some(SketchPoint::new(anchor.u + spacing, anchor.v))
            }
            PatternManipulatorKind::CircularCenter => {
                let origin = SketchPoint::new(0.0, 0.0);
                if anchor.distance_squared(origin).sqrt()
                    > PrecisionPolicy::default().min_feature_size
                {
                    Some(origin)
                } else {
                    Some(SketchPoint::new(anchor.u - DEFAULT_TOOL_LENGTH, anchor.v))
                }
            }
        }
    }

    fn ensure_pattern_manipulator(&mut self) {
        let Some(kind) = self.pattern_manipulator_kind() else {
            self.pattern_manipulator = None;
            return;
        };
        if self.modifier_sources.is_empty() || self.pending.is_some() {
            self.pattern_manipulator = None;
            return;
        }
        if self
            .pattern_manipulator
            .is_some_and(|manipulator| manipulator.kind == kind)
        {
            return;
        }
        self.pattern_manipulator =
            self.default_pattern_manipulator_position()
                .map(|position| PatternManipulator {
                    kind,
                    position,
                    dragging: false,
                });
    }

    fn sync_pattern_manipulator_from_spacing(&mut self) {
        self.ensure_pattern_manipulator();
        let Some(anchor) = self.pattern_anchor() else {
            return;
        };
        let Some(spacing) = self.active_tool_number("spacing_u") else {
            return;
        };
        let Some(manipulator) = self.pattern_manipulator.as_mut() else {
            return;
        };
        if manipulator.kind != PatternManipulatorKind::RectangularDirection || manipulator.dragging
        {
            return;
        }
        let direction = unit_direction(anchor, manipulator.position).unwrap_or((1.0, 0.0));
        let magnitude = spacing
            .abs()
            .max(PrecisionPolicy::default().min_feature_size * 2.0);
        manipulator.position = SketchPoint::new(
            magnitude.mul_add(direction.0, anchor.u),
            magnitude.mul_add(direction.1, anchor.v),
        );
    }

    fn begin_pattern_manipulator_drag(&mut self, canvas_rect: Rect, pointer: Pos2) -> bool {
        self.ensure_pattern_manipulator();
        let view = self.view;
        let Some(manipulator) = self.pattern_manipulator.as_mut() else {
            return false;
        };
        let handle = view.sketch_to_screen(canvas_rect, manipulator.position);
        if handle.distance(pointer) > PATTERN_MANIPULATOR_HIT_RADIUS_POINTS {
            return false;
        }
        manipulator.dragging = true;
        true
    }

    fn update_pattern_manipulator_drag(&mut self, position: SketchPoint) -> bool {
        let Some(manipulator) = self.pattern_manipulator.as_mut() else {
            return false;
        };
        if !manipulator.dragging || !position.is_finite() {
            return false;
        }
        manipulator.position = position;
        if manipulator.kind == PatternManipulatorKind::RectangularDirection
            && let Some(anchor) = self.pattern_anchor()
        {
            self.set_active_tool_number_from_manipulator(
                "spacing_u",
                anchor.distance_squared(position).sqrt(),
            );
        }
        true
    }

    fn release_pattern_manipulator_drag(&mut self) -> Option<SketchEntityId> {
        let manipulator = self.pattern_manipulator.as_mut()?;
        if !manipulator.dragging {
            return None;
        }
        manipulator.dragging = false;
        let position = manipulator.position;
        let staged = match manipulator.kind {
            PatternManipulatorKind::RectangularDirection => {
                self.stage_rectangular_pattern(position).ok()
            }
            PatternManipulatorKind::CircularCenter => self.stage_circular_pattern(position).ok(),
        };
        if staged.is_some() {
            self.modifier_sources.clear();
            self.pattern_manipulator = None;
        }
        staged
    }

    /// Adds another exact Trim to the transaction already owned by the outer
    /// confirmation gate. Hit testing and limit collection use the evolving
    /// candidate graph, whose active-curve iterator excludes every source
    /// superseded by an earlier click in this batch.
    fn append_pending_trim(
        &mut self,
        point: SketchPoint,
        pick_radius: f64,
    ) -> Option<SketchEntityId> {
        let (subject, label, mut transaction) = {
            let pending = self.pending.as_ref()?;
            let transaction = pending.core_transaction.as_ref()?;
            let is_trim_batch = transaction
                .preview()
                .operations()
                .last()
                .is_some_and(|operation| matches!(&operation.recipe, CoreRecipe::Trim { .. }));
            if !is_trim_batch {
                return None;
            }
            (pending.subject, pending.label, transaction.clone())
        };
        let candidate = transaction.preview();
        let target = Self::exact_curve_hit_in(candidate, point, pick_radius)?;
        let target_record = candidate.entity(target)?;
        let limits = candidate
            .active_entities()
            .filter(|record| {
                record.id != target && record.visible && record.role == target_record.role
            })
            .map(|record| record.id)
            .collect::<Vec<_>>();
        transaction
            .append_trim(target, limits, core_point(point))
            .ok()?;
        let presentation = self.core_transaction_presentation(&transaction).ok()?;
        let pending = self.pending.as_mut()?;
        if pending.subject != subject || pending.label != label {
            return None;
        }
        pending.entities = presentation.entities;
        pending.core_entities = presentation.core_entities;
        pending.retired_entities = presentation.retired_entities;
        pending.core_transaction = Some(transaction);
        self.dimension_session = None;
        self.refresh_profile_analysis();
        Some(subject)
    }

    /// The nearest active point within the pick radius, in exact evaluated
    /// coordinates. Display tessellation is never consulted.
    fn exact_point_hit(&self, point: SketchPoint, radius: f64) -> Option<CorePointId> {
        let target = core_point(point);
        let radius = radius.max(PrecisionPolicy::default().modeling_resolution);
        self.authoring
            .active_points()
            .filter_map(|record| {
                let distance = (record.evaluated_position.u - target.u)
                    .hypot(record.evaluated_position.v - target.v);
                (distance <= radius).then_some((distance, record.id))
            })
            .min_by(|left, right| left.0.total_cmp(&right.0))
            .map(|(_, id)| id)
    }

    fn relation_operand_hit(&self, point: SketchPoint, radius: f64) -> Option<RelationOperand> {
        self.exact_point_hit(point, radius)
            .map(RelationOperand::Point)
            .or_else(|| {
                self.exact_curve_hit(point, radius)
                    .map(RelationOperand::Curve)
            })
    }

    /// Whether the dimension tool should take this click as an operand of a
    /// distance between two things.
    ///
    /// The first pick decides: a click that lands on a point starts a distance
    /// dimension, and a click on a curve keeps the tool's older behaviour of
    /// arming that curve's own dimensions. Once a first operand is held every
    /// click belongs to the distance, which is what lets the second one be an
    /// edge.
    #[must_use]
    fn dimension_takes_the_click(&self, point: SketchPoint, radius: f64) -> bool {
        self.exact_tool == ToolVariant::Dimension
            && self.pending.is_none()
            && (!self.dimension_operands.is_empty()
                || self.exact_point_hit(point, radius).is_some())
    }

    /// How many operands the dimension tool is holding.
    #[must_use]
    pub fn dimension_operand_count(&self) -> usize {
        self.dimension_operands.len()
    }

    /// What the dimension tool is waiting for, when it is measuring between
    /// two things rather than editing one curve's own dimensions.
    #[must_use]
    pub fn dimension_step(&self) -> Option<&'static str> {
        if self.exact_tool != ToolVariant::Dimension {
            return None;
        }
        match self.dimension_operands.len() {
            0 => None,
            _ => Some("Picked one · click the point or edge to measure to"),
        }
    }

    /// The two endpoints of a straight curve used as a dimension's reference.
    ///
    /// Unlike a relation operand this accepts a curve a recipe owns: a
    /// rectangle's side is exactly what a drawer measures from, and the solver
    /// moves that rectangle as a body, so using its edge as a reference cannot
    /// deform it.
    fn dimension_line_points(
        &self,
        entity: CoreEntityId,
    ) -> Result<(CorePointId, CorePointId), String> {
        let record = self
            .authoring
            .entity(entity)
            .ok_or_else(|| "That curve is no longer part of the sketch.".to_owned())?;
        match record.geometry {
            CoreCurve2::Line { start, end } => Ok((start, end)),
            CoreCurve2::CircularArc { .. }
            | CoreCurve2::Circle { .. }
            | CoreCurve2::Bspline { .. } => {
                Err("A distance dimension measures from a straight edge or a point.".to_owned())
            }
        }
    }

    /// Accumulates one dimension operand and stages the dimension once it has
    /// both.
    fn append_dimension_operand(
        &mut self,
        point: SketchPoint,
        pick_radius: f64,
    ) -> Option<SketchEntityId> {
        let Some(operand) = self.relation_operand_hit(point, pick_radius) else {
            self.relation_diagnostic = Some(
                "Nothing to measure here. Click a point, or an edge to measure from.".to_owned(),
            );
            return None;
        };
        if self.dimension_operands.contains(&operand) {
            self.relation_diagnostic =
                Some("A dimension needs two different things to measure between.".to_owned());
            return None;
        }
        if let RelationOperand::Curve(curve) = operand {
            self.select_core_entity_for_modifier(curve);
        }
        self.dimension_operands.push(operand);
        if self.dimension_operands.len() < 2 {
            self.relation_diagnostic = None;
            return None;
        }
        let staged = self.stage_dimension();
        self.dimension_operands.clear();
        staged
    }

    /// Turns the two picked operands into the dimension they describe: the
    /// separation of two points, or the perpendicular offset of a point from
    /// an edge.
    fn stage_dimension(&mut self) -> Option<SketchEntityId> {
        let kind = match self.dimension_constraint() {
            Ok(kind) => kind,
            Err(reason) => {
                self.relation_diagnostic = Some(reason);
                return None;
            }
        };
        let label = "Sketch dimension";
        let transaction =
            match self
                .authoring
                .stage_constraint(kind, label, PrecisionPolicy::default())
            {
                Ok(transaction) => transaction,
                Err(error) => {
                    self.relation_diagnostic = Some(error.to_string());
                    return None;
                }
            };
        let subject = self.relation_subject(&transaction)?;
        match self.stage_core_relation(transaction, label, subject) {
            Ok(subject) => {
                self.relation_diagnostic = None;
                Some(subject)
            }
            Err(_) => {
                self.relation_diagnostic =
                    Some("The dimension could not be staged for confirmation.".to_owned());
                None
            }
        }
    }

    /// The dimension the picked pair describes, measured where it stands.
    ///
    /// The value is what the sketch already shows, which the user then retypes
    /// to drive it: a dimension arrives holding the truth and becomes an
    /// instruction the moment it is edited.
    fn dimension_constraint(&self) -> Result<CoreConstraintKind, String> {
        let (first, second) = match self.dimension_operands.as_slice() {
            [first, second] => (*first, *second),
            _ => return Err("A dimension needs two picks.".to_owned()),
        };
        let position = |point: CorePointId| {
            self.relation_point_position(point)
                .ok_or_else(|| "That point is no longer part of the sketch.".to_owned())
        };
        match (first, second) {
            (RelationOperand::Point(first), RelationOperand::Point(second)) => {
                let (from, to) = (position(first)?, position(second)?);
                let distance = (to.u - from.u).hypot(to.v - from.v);
                if distance <= PrecisionPolicy::default().min_feature_size {
                    return Err("Those points are already in the same place.".to_owned());
                }
                Ok(CoreConstraintKind::Distance {
                    first,
                    second,
                    distance,
                })
            }
            // Either order of picks means the same dimension: the edge is the
            // reference and the point is what it holds.
            (RelationOperand::Curve(curve), RelationOperand::Point(subject))
            | (RelationOperand::Point(subject), RelationOperand::Curve(curve)) => {
                let (line_start, line_end) = self.dimension_line_points(curve)?;
                let (a, b, c) = (position(line_start)?, position(line_end)?, position(subject)?);
                let offset = signed_offset_from_line(a, b, c)
                    .ok_or_else(|| "That edge is too short to measure from.".to_owned())?;
                if offset.abs() <= PrecisionPolicy::default().min_feature_size {
                    return Err("That point is already on the edge.".to_owned());
                }
                Ok(CoreConstraintKind::PointToLineDistance {
                    point: subject,
                    line_start,
                    line_end,
                    distance: offset,
                })
            }
            (RelationOperand::Curve(_), RelationOperand::Curve(_)) => Err(
                "A distance between two edges is not a dimension yet; measure from an edge to a point."
                    .to_owned(),
            ),
        }
    }

    /// Stages a new value for a dimension the sketch already holds.
    ///
    /// The panel hands over a magnitude. An offset taken from the far side of
    /// an edge is stored negative, and it keeps that side: retyping the number
    /// changes how far, never which way.
    pub fn stage_constraint_value(
        &mut self,
        id: CoreConstraintId,
        value: f64,
    ) -> Option<SketchEntityId> {
        let held = self
            .authoring
            .constraints()
            .get(&id)
            .and_then(|record| record.kind.value());
        let value = match held {
            Some(held) if held < 0.0 => -value.abs(),
            Some(_) => value.abs(),
            None => value,
        };
        let transaction = match self.authoring.stage_constraint_value(
            id,
            value,
            "Sketch dimension",
            PrecisionPolicy::default(),
        ) {
            Ok(transaction) => transaction,
            Err(error) => {
                self.relation_diagnostic = Some(error.to_string());
                return None;
            }
        };
        let subject = transaction
            .impact()
            .changed_entities
            .iter()
            .find_map(|entity| self.ui_by_core.get(entity).copied())
            .or(self.selected)
            .or_else(|| self.entities.first().map(|entity| entity.id))?;
        match self.stage_core_relation(transaction, "Sketch dimension", subject) {
            Ok(subject) => {
                self.relation_diagnostic = None;
                Some(subject)
            }
            Err(_) => {
                self.relation_diagnostic =
                    Some("The dimension could not be staged for confirmation.".to_owned());
                None
            }
        }
    }

    /// The sketch's single centreline, as the axis a revolve can turn about
    /// (ADR 0026, F3).
    ///
    /// One centreline is an axis; several are an ambiguity, and the user has
    /// to say which. Returning nothing in that case is what lets the ribbon
    /// explain itself rather than guess.
    #[must_use]
    pub fn centreline_axis(&self) -> Option<(SketchPoint, SketchPoint)> {
        let mut axis = None;
        for entity in &self.entities {
            if entity.role != SketchEntityRole::Construction {
                continue;
            }
            let SketchGeometry::Segment { start, end } = entity.geometry else {
                continue;
            };
            if axis.is_some() {
                return None;
            }
            axis = Some((start, end));
        }
        axis
    }

    #[must_use]
    pub fn relation_operand_count(&self) -> usize {
        self.relation_operands.len()
    }

    /// How many operands the active relation wants, if a relation tool owns
    /// the pointer.
    ///
    /// Horizontal and vertical take either one line or two endpoints, so the
    /// answer depends on what has been picked already: nothing yet means one
    /// pick may be enough, a lone point means a second is needed.
    #[must_use]
    pub fn relation_operands_required(&self) -> Option<usize> {
        let arity = relation_arity(self.exact_tool)?;
        Some(match (self.exact_tool, self.relation_operands.as_slice()) {
            (
                ToolVariant::HorizontalRelation | ToolVariant::VerticalRelation,
                [RelationOperand::Point(_)],
            ) => 2,
            _ => arity,
        })
    }

    /// What the armed relation is waiting for, in one line, or nothing when no
    /// relation is armed.
    ///
    /// The step is what the panel says while a pick is half-made; the
    /// descriptor's own prompt covers the first pick.
    #[must_use]
    pub fn relation_step(&self) -> Option<String> {
        let required = self.relation_operands_required()?;
        let picked = self.relation_operands.len();
        Some(if picked == 0 {
            self.exact_tool.descriptor().prompt.to_owned()
        } else {
            format!("Picked {picked} of {required} · click the next one")
        })
    }

    /// The last refusal, in the solver's own words.
    #[must_use]
    pub fn relation_diagnostic(&self) -> Option<&str> {
        self.relation_diagnostic.as_deref()
    }

    /// Rings these points on the canvas until told otherwise.
    ///
    /// Set from the relation list each frame: an empty slice is how the
    /// highlight goes away, so nothing has to remember to clear it.
    pub fn set_relation_highlight(&mut self, points: &[SketchPoint]) {
        if self.relation_highlight != points {
            self.relation_highlight.clear();
            self.relation_highlight.extend_from_slice(points);
        }
    }

    /// How many relations the sketch is holding.
    #[must_use]
    pub fn constraint_count(&self) -> usize {
        self.authoring
            .constraints()
            .values()
            .filter(|record| record.enabled)
            .count()
    }

    /// Every relation the sketch is holding, oldest first.
    ///
    /// This is what makes constraints a thing the user can see rather than
    /// infer: a sketch that will not move the way they expect is a sketch with
    /// a relation in it, and until now nothing on screen said so.
    #[must_use]
    pub fn constraint_summaries(&self) -> Vec<SketchConstraintSummary> {
        self.authoring
            .constraints()
            .values()
            .filter(|record| record.enabled)
            .map(|record| SketchConstraintSummary {
                id: record.id,
                label: constraint_kind_label(&record.kind),
                detail: constraint_detail(&record.kind),
                // A point-to-line offset is signed by the side it was taken
                // from; the panel shows and takes the magnitude, and the sign
                // is restored when it is written back.
                value: record.kind.value().map(f64::abs),
                leader: self.constraint_leader(&record.kind),
                points: record
                    .kind
                    .referenced_points()
                    .into_iter()
                    .filter_map(|point| self.relation_point_position(point))
                    .map(|position| SketchPoint::new(position.u, position.v))
                    .collect(),
            })
            .collect()
    }

    /// Where a dimension's value is drawn, in sketch coordinates: the middle
    /// of what it measures, plus wherever the user has dragged it.
    fn dimension_chip_anchor(
        &self,
        id: CoreConstraintId,
        kind: &CoreConstraintKind,
    ) -> Option<SketchPoint> {
        let (from, to) = self.constraint_leader(kind)?;
        let placed = self
            .authoring
            .constraints()
            .get(&id)
            .and_then(|record| record.label_offset)
            .unwrap_or(CorePoint2::new(0.0, 0.0));
        Some(SketchPoint::new(
            (from.u + to.u) * 0.5 + placed.u,
            (from.v + to.v) * 0.5 + placed.v,
        ))
    }

    /// Where every dimension's value is drawn, in sketch coordinates, in the
    /// order the panel lists them.
    #[must_use]
    pub fn dimension_label_positions(&self) -> Vec<SketchPoint> {
        self.authoring
            .constraints()
            .values()
            .filter(|record| record.enabled)
            .filter_map(|record| self.dimension_chip_anchor(record.id, &record.kind))
            .collect()
    }

    /// The rectangle a dimension's value occupies on screen.
    ///
    /// One answer for the painter and the pointer alike: a chip you can see
    /// but not grab, or grab but not see, is worse than no chip.
    fn dimension_chip_rect(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        id: CoreConstraintId,
        kind: &CoreConstraintKind,
    ) -> Option<Rect> {
        let value = kind.value()?.abs();
        let centre = self
            .view
            .sketch_to_screen(rect, self.dimension_chip_anchor(id, kind)?);
        let galley = painter.layout_no_wrap(
            dimension_chip_text(value),
            FontId::monospace(DIMENSION_CHIP_TEXT_SIZE),
            Color32::WHITE,
        );
        Some(Rect::from_center_size(
            centre,
            galley.rect.size() + DIMENSION_CHIP_PADDING,
        ))
    }

    /// The dimension whose value the pointer is over, nearest first.
    fn dimension_chip_at(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        position: Pos2,
    ) -> Option<CoreConstraintId> {
        self.authoring
            .constraints()
            .values()
            .filter(|record| record.enabled)
            .filter_map(|record| {
                let chip = self.dimension_chip_rect(painter, rect, record.id, &record.kind)?;
                chip.contains(position).then_some(record.id)
            })
            .next_back()
    }

    /// Takes hold of a dimension's value at `position`, remembering where
    /// inside the chip it was grabbed.
    fn begin_dimension_drag(
        &mut self,
        painter: &egui::Painter,
        rect: Rect,
        position: Pos2,
    ) -> bool {
        let Some(id) = self.dimension_chip_at(painter, rect, position) else {
            return false;
        };
        let Some(kind) = self
            .authoring
            .constraints()
            .get(&id)
            .map(|record| record.kind.clone())
        else {
            return false;
        };
        let Some(anchor) = self.dimension_chip_anchor(id, &kind) else {
            return false;
        };
        let grabbed = self.view.screen_to_sketch(rect, position);
        self.dimension_drag_target = Some((id, (grabbed.u - anchor.u, grabbed.v - anchor.v)));
        true
    }

    /// Moves the dragged value to follow the pointer.
    fn update_dimension_drag(&mut self, rect: Rect, position: Pos2) -> bool {
        let Some((id, grab)) = self.dimension_drag_target else {
            return false;
        };
        let Some(kind) = self
            .authoring
            .constraints()
            .get(&id)
            .map(|record| record.kind.clone())
        else {
            return false;
        };
        let Some((from, to)) = self.constraint_leader(&kind) else {
            return false;
        };
        let pointer = self.view.screen_to_sketch(rect, position);
        let placement = CorePoint2::new(
            pointer.u - grab.0 - (from.u + to.u) * 0.5,
            pointer.v - grab.1 - (from.v + to.v) * 0.5,
        );
        self.authoring
            .set_constraint_label_offset(id, Some(placement))
    }

    /// Where a dimension's leader runs: from the thing it is measured from to
    /// the thing it holds.
    ///
    /// For an offset that is the foot of the perpendicular on the reference
    /// edge and the point itself, so the leader lies along the very distance
    /// the number names.
    fn constraint_leader(&self, kind: &CoreConstraintKind) -> Option<(SketchPoint, SketchPoint)> {
        let at = |point: CorePointId| {
            self.relation_point_position(point)
                .map(|position| SketchPoint::new(position.u, position.v))
        };
        match *kind {
            CoreConstraintKind::Distance { first, second, .. } => Some((at(first)?, at(second)?)),
            CoreConstraintKind::PointToLineDistance {
                point,
                line_start,
                line_end,
                ..
            } => {
                let (a, b, subject) = (at(line_start)?, at(line_end)?, at(point)?);
                let (du, dv) = (b.u - a.u, b.v - a.v);
                let length_squared = du.mul_add(du, dv * dv);
                if length_squared <= f64::EPSILON {
                    return None;
                }
                let along = ((subject.u - a.u) * du + (subject.v - a.v) * dv) / length_squared;
                let foot = SketchPoint::new(du.mul_add(along, a.u), dv.mul_add(along, a.v));
                Some((foot, subject))
            }
            _ => None,
        }
    }

    /// Stages the removal of one relation behind the usual confirmation gate.
    ///
    /// Releasing an equation cannot conflict — what is left solved before — but
    /// it frees the points the solver was holding, so it travels the same
    /// staged path as making one: preview, tick, undo.
    pub fn stage_constraint_removal(&mut self, id: CoreConstraintId) -> Option<SketchEntityId> {
        let transaction = match self.authoring.stage_constraint_removal(
            id,
            "Remove relation",
            PrecisionPolicy::default(),
        ) {
            Ok(transaction) => transaction,
            Err(error) => {
                self.relation_diagnostic = Some(error.to_string());
                return None;
            }
        };
        let subject = transaction
            .impact()
            .changed_entities
            .iter()
            .find_map(|entity| self.ui_by_core.get(entity).copied())
            .or(self.selected)
            .or_else(|| self.entities.first().map(|entity| entity.id))?;
        match self.stage_core_relation(transaction, "Remove relation", subject) {
            Ok(subject) => {
                self.relation_diagnostic = None;
                Some(subject)
            }
            Err(_) => {
                self.relation_diagnostic =
                    Some("The removal could not be staged for confirmation.".to_owned());
                None
            }
        }
    }

    fn clear_relation_acquisition(&mut self) {
        self.relation_operands.clear();
    }

    /// The two endpoints of a straight operand, or a refusal naming why not.
    ///
    /// The recipe boundary of ADR 0026 lives here: a curve that a pattern,
    /// slot, or polygon owns exposes no constrainable endpoints in v1, because
    /// its shape is the recipe's to decide, not the solver's.
    fn relation_line_points(
        &self,
        entity: CoreEntityId,
    ) -> Result<(CorePointId, CorePointId), String> {
        let record = self
            .authoring
            .entity(entity)
            .ok_or_else(|| "That curve is no longer part of the sketch.".to_owned())?;
        let owner = self.authoring.operation(record.provenance.operation);
        if let Some(owner) = owner
            && !matches!(
                owner.recipe,
                CoreRecipe::Line { .. }
                    | CoreRecipe::CentreLine { .. }
                    | CoreRecipe::Polyline { .. }
            )
        {
            return Err(
                "That curve belongs to a recipe feature, which owns its own shape. Relate its anchor points instead."
                    .to_owned(),
            );
        }
        match record.geometry {
            CoreCurve2::Line { start, end } => Ok((start, end)),
            CoreCurve2::CircularArc { .. }
            | CoreCurve2::Circle { .. }
            | CoreCurve2::Bspline { .. } => Err(
                "This relation applies to straight curves; pick a line or two endpoints."
                    .to_owned(),
            ),
        }
    }

    fn relation_point_position(&self, point: CorePointId) -> Option<CorePoint2> {
        self.authoring
            .point(point)
            .filter(|record| record.active)
            .map(|record| record.evaluated_position)
    }

    /// Turns the picked operands into the equations the solver will hold.
    fn relation_constraints(
        &self,
        variant: ToolVariant,
    ) -> Result<Vec<CoreConstraintKind>, String> {
        let operands = self.relation_operands.as_slice();
        let pair_points = |first: RelationOperand, second: RelationOperand| match (first, second) {
            (RelationOperand::Point(first), RelationOperand::Point(second)) => Ok((first, second)),
            _ => Err("This relation applies to two endpoints.".to_owned()),
        };
        let pair_lines = |first: RelationOperand, second: RelationOperand| match (first, second) {
            (RelationOperand::Curve(first), RelationOperand::Curve(second)) => {
                let first = self.relation_line_points(first)?;
                let second = self.relation_line_points(second)?;
                Ok((first, second))
            }
            _ => Err("This relation applies to two lines.".to_owned()),
        };
        match (variant, operands) {
            (ToolVariant::FixedRelation, [RelationOperand::Point(point)]) => {
                let position = self
                    .relation_point_position(*point)
                    .ok_or_else(|| "That point is no longer part of the sketch.".to_owned())?;
                Ok(vec![CoreConstraintKind::Fixed {
                    point: *point,
                    position,
                }])
            }
            (ToolVariant::FixedRelation, [RelationOperand::Curve(curve)]) => {
                let (start, end) = self.relation_line_points(*curve)?;
                [start, end]
                    .into_iter()
                    .map(|point| {
                        let position = self.relation_point_position(point).ok_or_else(|| {
                            "That point is no longer part of the sketch.".to_owned()
                        })?;
                        Ok(CoreConstraintKind::Fixed { point, position })
                    })
                    .collect()
            }
            (
                ToolVariant::HorizontalRelation | ToolVariant::VerticalRelation,
                [RelationOperand::Curve(curve)],
            ) => {
                let (first, second) = self.relation_line_points(*curve)?;
                Ok(vec![if variant == ToolVariant::HorizontalRelation {
                    CoreConstraintKind::Horizontal { first, second }
                } else {
                    CoreConstraintKind::Vertical { first, second }
                }])
            }
            (
                ToolVariant::HorizontalRelation | ToolVariant::VerticalRelation,
                [
                    RelationOperand::Point(first),
                    RelationOperand::Point(second),
                ],
            ) => Ok(vec![if variant == ToolVariant::HorizontalRelation {
                CoreConstraintKind::Horizontal {
                    first: *first,
                    second: *second,
                }
            } else {
                CoreConstraintKind::Vertical {
                    first: *first,
                    second: *second,
                }
            }]),
            (ToolVariant::CoincidentRelation, [first, second]) => {
                let (first, second) = pair_points(*first, *second)?;
                Ok(vec![CoreConstraintKind::Coincident { first, second }])
            }
            (ToolVariant::DistanceRelation, [first, second]) => {
                let (first, second) = pair_points(*first, *second)?;
                let (from, to) = (
                    self.relation_point_position(first),
                    self.relation_point_position(second),
                );
                let (Some(from), Some(to)) = (from, to) else {
                    return Err("Those points are no longer part of the sketch.".to_owned());
                };
                // The present separation becomes the held value: the relation
                // locks what the user already sees, and the dimension tool
                // edits it afterwards.
                let distance = (to.u - from.u).hypot(to.v - from.v);
                if distance <= PrecisionPolicy::default().min_feature_size {
                    return Err(
                        "Those points are already together; use a coincident relation.".to_owned(),
                    );
                }
                Ok(vec![CoreConstraintKind::Distance {
                    first,
                    second,
                    distance,
                }])
            }
            (
                ToolVariant::ParallelRelation
                | ToolVariant::PerpendicularRelation
                | ToolVariant::EqualLengthRelation,
                [first, second],
            ) => {
                let ((first_start, first_end), (second_start, second_end)) =
                    pair_lines(*first, *second)?;
                Ok(vec![match variant {
                    ToolVariant::ParallelRelation => CoreConstraintKind::Parallel {
                        first_start,
                        first_end,
                        second_start,
                        second_end,
                    },
                    ToolVariant::PerpendicularRelation => CoreConstraintKind::Perpendicular {
                        first_start,
                        first_end,
                        second_start,
                        second_end,
                    },
                    _ => CoreConstraintKind::EqualLength {
                        first_start,
                        first_end,
                        second_start,
                        second_end,
                    },
                }])
            }
            (ToolVariant::CollinearRelation, [first, second]) => {
                let ((first_start, first_end), (second_start, second_end)) =
                    pair_lines(*first, *second)?;
                // Both ends of the second line onto the first line's carrier:
                // that is two equations, the same count as making the lines
                // parallel and then sharing a point.
                Ok(vec![
                    CoreConstraintKind::Collinear {
                        first: first_start,
                        second: second_start,
                        third: first_end,
                    },
                    CoreConstraintKind::Collinear {
                        first: first_start,
                        second: second_end,
                        third: first_end,
                    },
                ])
            }
            (ToolVariant::TangentRelation, [first, second]) => {
                let (line, round) = match (first, second) {
                    (RelationOperand::Curve(first), RelationOperand::Curve(second)) => {
                        if self.relation_line_points(*first).is_ok() {
                            (*first, *second)
                        } else {
                            (*second, *first)
                        }
                    }
                    _ => {
                        return Err(
                            "This relation applies to a line and a circle or arc.".to_owned()
                        );
                    }
                };
                let (start, end) = self.relation_line_points(line).map_err(|_| {
                    "This relation applies to a line and a circle or arc.".to_owned()
                })?;
                Ok(vec![match self.relation_round_carrier(round)? {
                    RoundCarrier::Circle { center, radius } => {
                        CoreConstraintKind::LineTangentToCircle {
                            start,
                            end,
                            center,
                            radius,
                        }
                    }
                    RoundCarrier::Arc { center, rim } => CoreConstraintKind::LineTangentToArc {
                        start,
                        end,
                        center,
                        rim,
                    },
                }])
            }
            _ => Err("This relation needs different operands.".to_owned()),
        }
    }

    /// The centre and radius of a circular operand, as the solver holds them.
    fn relation_round_carrier(&self, entity: CoreEntityId) -> Result<RoundCarrier, String> {
        let record = self
            .authoring
            .entity(entity)
            .ok_or_else(|| "That curve is no longer part of the sketch.".to_owned())?;
        match record.geometry {
            CoreCurve2::Circle { center, radius, .. } => {
                Ok(RoundCarrier::Circle { center, radius })
            }
            CoreCurve2::CircularArc { center, start, .. } => {
                Ok(RoundCarrier::Arc { center, rim: start })
            }
            CoreCurve2::Line { .. } | CoreCurve2::Bspline { .. } => {
                Err("This relation applies to a line and a circle or arc.".to_owned())
            }
        }
    }

    /// Accumulates one relation operand and stages the relation once the
    /// active kind has everything it needs.
    fn append_relation_operand(
        &mut self,
        point: SketchPoint,
        pick_radius: f64,
    ) -> Option<SketchEntityId> {
        let variant = self.exact_tool;
        let arity = relation_arity(variant)?;
        let Some(operand) = self.relation_operand_hit(point, pick_radius) else {
            // A relation tool owns the click, so a miss cannot fall through to
            // selection and must say so itself. Silence here is what made the
            // relation tools look inert: the pointer was simply not on
            // anything a relation can name.
            self.relation_diagnostic =
                Some("Nothing to relate here. Click a line or one of its endpoints.".to_owned());
            return None;
        };
        if self.relation_operands.contains(&operand) {
            self.relation_diagnostic = Some("A relation needs two different operands.".to_owned());
            return None;
        }
        self.relation_operands.push(operand);
        if let RelationOperand::Curve(curve) = operand {
            self.select_core_entity_for_modifier(curve);
        }

        // Horizontal and vertical accept either one line or two points, so a
        // single point is a complete pick only when a second one follows.
        let complete = match (variant, self.relation_operands.as_slice()) {
            (
                ToolVariant::HorizontalRelation | ToolVariant::VerticalRelation,
                [RelationOperand::Point(_)],
            ) => false,
            _ => self.relation_operands.len() >= arity,
        };
        if !complete {
            self.relation_diagnostic = None;
            return None;
        }
        let staged = self.stage_relation(variant);
        self.clear_relation_acquisition();
        staged
    }

    fn stage_relation(&mut self, variant: ToolVariant) -> Option<SketchEntityId> {
        let kinds = match self.relation_constraints(variant) {
            Ok(kinds) => kinds,
            Err(reason) => {
                self.relation_diagnostic = Some(reason);
                return None;
            }
        };
        let label = relation_label(variant);
        let transaction =
            match self
                .authoring
                .stage_constraints(kinds, label, PrecisionPolicy::default())
            {
                Ok(transaction) => transaction,
                Err(error) => {
                    self.relation_diagnostic = Some(error.to_string());
                    return None;
                }
            };
        // A relation inserts no geometry, so the confirmation gate needs a
        // presentation subject: the entity the user last named.
        let subject = self.relation_subject(&transaction)?;
        match self.stage_core_relation(transaction, label, subject) {
            Ok(subject) => {
                self.relation_diagnostic = None;
                Some(subject)
            }
            Err(_) => {
                self.relation_diagnostic =
                    Some("The relation could not be staged for confirmation.".to_owned());
                None
            }
        }
    }

    /// The presentation entity a staged relation hangs from: a curve operand
    /// if one was picked, otherwise any curve the solver moved.
    fn relation_subject(&self, transaction: &CoreTransaction) -> Option<SketchEntityId> {
        self.relation_operands
            .iter()
            .rev()
            .find_map(|operand| match operand {
                RelationOperand::Curve(curve) => self.ui_by_core.get(curve).copied(),
                RelationOperand::Point(_) => None,
            })
            .or_else(|| {
                transaction
                    .impact()
                    .changed_entities
                    .iter()
                    .find_map(|entity| self.ui_by_core.get(entity).copied())
            })
            .or(self.selected)
            .or_else(|| self.entities.first().map(|entity| entity.id))
    }

    /// Stages a transaction that changes only existing geometry.
    ///
    /// The insertion and retirement paths both key their preview on entities
    /// that appear or disappear. A relation does neither: it moves points that
    /// are already there, so it carries no provisional geometry and the
    /// preview is the moved sketch itself.
    fn stage_core_relation(
        &mut self,
        transaction: CoreTransaction,
        label: &'static str,
        subject: SketchEntityId,
    ) -> Result<SketchEntityId, SketchEditError> {
        if self.pending.is_some() {
            return Err(SketchEditError::PendingEditAlreadyExists);
        }
        self.pending = Some(PendingSketchEdit {
            subject,
            label,
            entities: Vec::new(),
            core_transaction: Some(transaction),
            core_entities: Vec::new(),
            retired_entities: Vec::new(),
            in_place: false,
        });
        self.refresh_profile_analysis();
        Ok(subject)
    }

    /// Re-reads every presentation curve from the exact definition, keeping
    /// user-facing identity. A relation moves points rather than replacing
    /// curves, so the adapter must follow the geometry without renumbering it.
    fn refresh_presentation_geometry(&mut self) {
        let updates = self
            .ui_by_core
            .iter()
            .filter(|(_, ui_id)| {
                // A legacy composite presents several core curves as one
                // entity; its geometry is the composite's, not any one side's,
                // so only one-to-one adapters are re-read here.
                self.core_by_ui
                    .get(*ui_id)
                    .is_none_or(|sources| sources.len() == 1)
            })
            .filter_map(|(core_id, ui_id)| {
                let curve = self.authoring.evaluated_curve(*core_id).ok()?;
                Some((*ui_id, legacy_geometry_from_core(curve)))
            })
            .collect::<Vec<_>>();
        for (ui_id, geometry) in updates {
            if let Some(entity) = self.entities.iter_mut().find(|entity| entity.id == ui_id) {
                entity.geometry = geometry;
            }
        }
        self.refresh_profile_analysis();
    }

    fn select_core_entity_for_modifier(&mut self, core_id: CoreEntityId) {
        if let Some(ui_id) = self.ui_by_core.get(&core_id).copied() {
            let _ = self.set_selected(Some(ui_id));
        }
    }

    fn toggle_pattern_source(&mut self, point: SketchPoint, radius: f64) -> bool {
        let Some(source) = self.exact_curve_hit(point, radius) else {
            return false;
        };
        if let Some(index) = self
            .modifier_sources
            .iter()
            .position(|existing| *existing == source)
        {
            self.modifier_sources.remove(index);
        } else {
            self.modifier_sources.push(source);
            self.modifier_sources.sort_unstable();
            self.modifier_sources.dedup();
            self.select_core_entity_for_modifier(source);
        }
        self.pattern_manipulator = None;
        self.ensure_pattern_manipulator();
        true
    }

    fn pattern_anchor(&self) -> Option<SketchPoint> {
        let mut sum_u = 0.0;
        let mut sum_v = 0.0;
        let mut count = 0_u32;
        let mut include = |point: CorePoint2| {
            sum_u += point.u;
            sum_v += point.v;
            count = count.saturating_add(1);
        };
        for source in &self.modifier_sources {
            match self.authoring.evaluated_curve(*source).ok()? {
                CoreEvaluatedCurve2::Line { start, end } => {
                    include(start);
                    include(end);
                }
                CoreEvaluatedCurve2::CircularArc {
                    center, start, end, ..
                } => {
                    include(center);
                    include(start);
                    include(end);
                }
                CoreEvaluatedCurve2::Circle { center, .. } => include(center),
                CoreEvaluatedCurve2::Bspline { control_points, .. } => {
                    for cp in control_points {
                        include(cp);
                    }
                }
            }
        }
        (count > 0).then(|| SketchPoint::new(sum_u / f64::from(count), sum_v / f64::from(count)))
    }

    fn corner_modifier_recipe(
        &self,
        definition: &CoreSketchDefinition,
        first: CoreEntityId,
        second: CoreEntityId,
    ) -> Result<(CoreRecipe, &'static str), SketchEditError> {
        if self.active_tool_parameter_issue().is_some() {
            return Err(SketchEditError::AuthoringRejected);
        }
        let first_curve = definition
            .evaluated_curve(first)
            .map_err(|_| SketchEditError::AuthoringRejected)?;
        let second_curve = definition
            .evaluated_curve(second)
            .map_err(|_| SketchEditError::AuthoringRejected)?;
        let first_pick = self
            .modifier_picks
            .get(&first)
            .copied()
            .ok_or(SketchEditError::AuthoringRejected)?;
        let second_pick = self
            .modifier_picks
            .get(&second)
            .copied()
            .ok_or(SketchEditError::AuthoringRejected)?;
        Ok(match self.exact_tool {
            ToolVariant::Fillet => {
                let radius = CoreLength::new(
                    self.active_tool_number("radius")
                        .ok_or(SketchEditError::AuthoringRejected)?,
                )
                .map_err(|_| SketchEditError::AuthoringRejected)?;
                let intersections =
                    intersect_curves(first_curve, second_curve, &PrecisionPolicy::default());
                let CoreCurveIntersections::Points { intersections } = intersections else {
                    return Err(SketchEditError::AuthoringRejected);
                };
                let corner = intersections
                    .iter()
                    .min_by(|left, right| {
                        let score = |point: CorePoint2| {
                            let point = SketchPoint::new(point.u, point.v);
                            point.distance_squared(first_pick) + point.distance_squared(second_pick)
                        };
                        score(left.point)
                            .total_cmp(&score(right.point))
                            .then_with(|| left.point.total_cmp(&right.point))
                    })
                    .ok_or(SketchEditError::AuthoringRejected)?
                    .point;
                (
                    CoreRecipe::FilletWithHints {
                        first,
                        second,
                        radius: CoreValue::Literal(radius),
                        hints: CoreFilletBranchHints {
                            first_pick: core_point(first_pick),
                            second_pick: core_point(second_pick),
                            corner_hint: corner,
                        },
                    },
                    "Fillet sketch corner",
                )
            }
            ToolVariant::Chamfer | ToolVariant::TwoDistanceChamfer => {
                let first_distance = CoreLength::new(
                    self.active_tool_number("distance_1")
                        .ok_or(SketchEditError::AuthoringRejected)?,
                )
                .map_err(|_| SketchEditError::AuthoringRejected)?;
                let second_distance = if self.exact_tool == ToolVariant::TwoDistanceChamfer {
                    CoreLength::new(
                        self.active_tool_number("distance_2")
                            .ok_or(SketchEditError::AuthoringRejected)?,
                    )
                    .map_err(|_| SketchEditError::AuthoringRejected)?
                } else {
                    first_distance
                };
                (
                    CoreRecipe::Chamfer {
                        first,
                        second,
                        first_distance: CoreValue::Literal(first_distance),
                        second_distance: CoreValue::Literal(second_distance),
                    },
                    "Chamfer sketch corner",
                )
            }
            _ => return Err(SketchEditError::AuthoringRejected),
        })
    }

    fn stage_corner_modifier(
        &mut self,
        first: CoreEntityId,
        second: CoreEntityId,
    ) -> Result<SketchEntityId, SketchEditError> {
        let (recipe, label) = self.corner_modifier_recipe(&self.authoring, first, second)?;
        let transaction = self
            .authoring
            .stage_modifier(recipe, label)
            .map_err(|_| SketchEditError::AuthoringRejected)?;
        self.stage_core_transaction(transaction, label)
    }

    /// Adds another fillet/chamfer corner to the exact transaction already
    /// owned by the universal confirmation gate. A first click merely selects
    /// one carrier in the evolving candidate; the second click appends the
    /// complete corner. Invalid pairs leave the preceding preview untouched.
    fn append_pending_corner(
        &mut self,
        point: SketchPoint,
        pick_radius: f64,
    ) -> Option<SketchEntityId> {
        let (subject, label, mut transaction) = {
            let pending = self.pending.as_ref()?;
            let transaction = pending.core_transaction.as_ref()?;
            let is_corner_batch =
                transaction
                    .preview()
                    .operations()
                    .last()
                    .is_some_and(|operation| {
                        matches!(
                            &operation.recipe,
                            CoreRecipe::Fillet { .. }
                                | CoreRecipe::FilletWithHints { .. }
                                | CoreRecipe::Chamfer { .. }
                        )
                    });
            if !is_corner_batch {
                return None;
            }
            (pending.subject, pending.label, transaction.clone())
        };
        let picked = Self::exact_curve_hit_in(transaction.preview(), point, pick_radius)?;
        if self.modifier_sources.is_empty() {
            self.modifier_sources.push(picked);
            self.modifier_picks.insert(picked, point);
            return None;
        }
        let first = self.modifier_sources[0];
        if picked == first {
            return None;
        }
        self.modifier_picks.insert(picked, point);
        let (recipe, _) = self
            .corner_modifier_recipe(transaction.preview(), first, picked)
            .ok()?;
        transaction.append_modifier(recipe).ok()?;
        let presentation = self.core_transaction_presentation(&transaction).ok()?;
        let pending = self.pending.as_mut()?;
        if pending.subject != subject || pending.label != label {
            return None;
        }
        pending.entities = presentation.entities;
        pending.core_entities = presentation.core_entities;
        pending.retired_entities = presentation.retired_entities;
        pending.core_transaction = Some(transaction);
        self.modifier_sources.clear();
        self.clear_relation_acquisition();
        self.modifier_picks.clear();
        self.dimension_session = None;
        self.refresh_profile_analysis();
        Some(subject)
    }

    /// Offsets the chain under the pointer, on the side the click fell.
    ///
    /// One click does the whole thing: the chain is what the curve is joined
    /// to, the side is which way the click sits from it, and the magnitude is
    /// the palette's `distance`, which `Tab` retypes and which the recipe then
    /// keeps. That is the smallest gesture that is still the tool — the live
    /// drag handle is the next stage, not a different command.
    fn stage_offset(
        &mut self,
        point: SketchPoint,
        pick_radius: f64,
    ) -> Result<SketchEntityId, SketchEditError> {
        if self.active_tool_parameter_issue().is_some() {
            return Err(SketchEditError::AuthoringRejected);
        }
        let picked = self
            .exact_curve_hit(point, pick_radius)
            .ok_or(SketchEditError::AuthoringRejected)?;
        let magnitude = self
            .active_tool_number("distance")
            .ok_or(SketchEditError::AuthoringRejected)?;
        let chain = self
            .offset_chain_for(picked)
            .ok_or(SketchEditError::AuthoringRejected)?;
        let side = self.offset_side(&chain, picked, point).unwrap_or(1.0);
        let distance = CoreSignedLength::new(magnitude.abs() * side)
            .map_err(|_| SketchEditError::AuthoringRejected)?;
        let recipe = CoreRecipe::Offset {
            sources: chain.members,
            closed: chain.closed,
            distance: CoreValue::Literal(distance),
        };
        let transaction = self
            .authoring
            .stage(recipe, "Offset sketch geometry")
            .map_err(|_| SketchEditError::AuthoringRejected)?;
        self.stage_core_transaction(transaction, "Offset sketch geometry")
    }

    /// The chain a click on `picked` means: the whole connected run when chain
    /// selection is on, that one curve when it is off.
    fn offset_chain_for(&self, picked: CoreEntityId) -> Option<CoreSketchChain> {
        let precision = PrecisionPolicy::default();
        let chain = connected_chain(&self.authoring, picked, &precision).ok()?;
        if self.active_tool_flag("chain_selection").unwrap_or(true) {
            return Some(chain);
        }
        Some(CoreSketchChain {
            members: chain
                .members
                .into_iter()
                .filter(|member| member.entity == picked)
                .collect(),
            closed: false,
        })
    }

    /// Which side of the chain the pointer is on, as +1 (left of travel) or -1.
    ///
    /// Read from the curve the click actually landed on, oriented the way the
    /// chain walks it. A click exactly on the curve has no side, and the caller
    /// keeps the sign it already had.
    fn offset_side(
        &self,
        chain: &CoreSketchChain,
        picked: CoreEntityId,
        point: SketchPoint,
    ) -> Option<f64> {
        let index = chain
            .members
            .iter()
            .position(|member| member.entity == picked)?;
        let geometry = chain_geometry(&self.authoring, chain).ok()?;
        let subject = core_point(point);
        let slack = PrecisionPolicy::default().min_feature_size;
        match geometry.curves.get(index)? {
            CoreEvaluatedCurve2::Line { start, end } => {
                let offset = signed_offset_from_line(*start, *end, subject)?;
                (offset.abs() > slack).then(|| if offset > 0.0 { 1.0 } else { -1.0 })
            }
            // The left of a counter-clockwise arc is its inside, so a click
            // outside one asks for the larger radius and a click inside for the
            // smaller. A chord would answer for a semicircle and lie for
            // anything longer.
            CoreEvaluatedCurve2::CircularArc {
                center,
                start,
                direction,
                ..
            } => {
                let radius = (*start - *center).length();
                let reach = (subject - *center).length();
                let inward = match direction {
                    CoreCurveDirection::CounterClockwise => 1.0,
                    CoreCurveDirection::Clockwise => -1.0,
                };
                ((reach - radius).abs() > slack)
                    .then(|| if reach > radius { -inward } else { inward })
            }
            CoreEvaluatedCurve2::Circle {
                center,
                radius,
                direction,
            } => {
                let reach = (subject - *center).length();
                let inward = match direction {
                    CoreCurveDirection::CounterClockwise => 1.0,
                    CoreCurveDirection::Clockwise => -1.0,
                };
                ((reach - radius).abs() > slack)
                    .then(|| if reach > *radius { -inward } else { inward })
            }
            CoreEvaluatedCurve2::Bspline { .. } => None,
        }
    }

    fn stage_rectangular_pattern(
        &mut self,
        direction_point: SketchPoint,
    ) -> Result<SketchEntityId, SketchEditError> {
        let anchor = self
            .pattern_anchor()
            .ok_or(SketchEditError::AuthoringRejected)?;
        let delta_u = direction_point.u - anchor.u;
        let delta_v = direction_point.v - anchor.v;
        self.sync_active_tool_number("spacing_u", delta_u.hypot(delta_v));
        if self.active_tool_parameter_issue().is_some() {
            return Err(SketchEditError::AuthoringRejected);
        }
        if !delta_u.is_finite()
            || !delta_v.is_finite()
            || delta_u.hypot(delta_v) <= PrecisionPolicy::default().min_feature_size
        {
            return Err(SketchEditError::AuthoringRejected);
        }
        let columns = self
            .active_tool_number("count_u")
            .ok_or(SketchEditError::AuthoringRejected)? as u16;
        let second_direction = self.active_tool_flag("second_direction").unwrap_or(false);
        let rows = if second_direction {
            self.active_tool_number("count_v")
                .ok_or(SketchEditError::AuthoringRejected)? as u16
        } else {
            1
        };
        let column_spacing = CoreSignedLength::new(
            self.active_tool_number("spacing_u")
                .ok_or(SketchEditError::AuthoringRejected)?,
        )
        .map_err(|_| SketchEditError::AuthoringRejected)?;
        let row_spacing = CoreSignedLength::new(if second_direction {
            self.active_tool_number("spacing_v")
                .ok_or(SketchEditError::AuthoringRejected)?
        } else {
            0.0
        })
        .map_err(|_| SketchEditError::AuthoringRejected)?;
        let direction = CoreAngle::radians(delta_v.atan2(delta_u))
            .map_err(|_| SketchEditError::AuthoringRejected)?;
        let recipe = CoreRecipe::RectangularPattern {
            sources: self.modifier_sources.clone(),
            columns: CoreValue::Literal(CoreInteger::new(columns)),
            rows: CoreValue::Literal(CoreInteger::new(rows)),
            column_spacing: CoreValue::Literal(column_spacing),
            row_spacing: CoreValue::Literal(row_spacing),
            direction: CoreValue::Literal(direction),
        };
        let transaction = self
            .authoring
            .stage(recipe, "Pattern sketch geometry")
            .map_err(|_| SketchEditError::AuthoringRejected)?;
        self.stage_core_transaction(transaction, "Pattern sketch geometry")
    }

    fn stage_circular_pattern(
        &mut self,
        center: SketchPoint,
    ) -> Result<SketchEntityId, SketchEditError> {
        if self.active_tool_parameter_issue().is_some() {
            return Err(SketchEditError::AuthoringRejected);
        }
        let anchor = self
            .pattern_anchor()
            .ok_or(SketchEditError::AuthoringRejected)?;
        if anchor.distance_squared(center).sqrt() <= PrecisionPolicy::default().min_feature_size {
            return Err(SketchEditError::AuthoringRejected);
        }
        let count = self
            .active_tool_number("count")
            .ok_or(SketchEditError::AuthoringRejected)? as u16;
        let complete = self.active_tool_flag("full_circle").unwrap_or(true);
        let angle_degrees = self
            .active_tool_number("extent")
            .ok_or(SketchEditError::AuthoringRejected)?;
        let recipe = CoreRecipe::CircularPattern {
            sources: self.modifier_sources.clone(),
            center: core_point_input(center),
            count: CoreValue::Literal(CoreInteger::new(count)),
            total_angle: CoreValue::Literal(
                CoreAngle::radians(if complete {
                    std::f64::consts::TAU
                } else {
                    angle_degrees.to_radians()
                })
                .map_err(|_| SketchEditError::AuthoringRejected)?,
            ),
            distribution: if complete {
                CoreCircularPatternDistribution::Complete
            } else {
                CoreCircularPatternDistribution::Extent
            },
            rotate_instances: self.active_tool_flag("rotate_instances").unwrap_or(true),
        };
        let transaction = self
            .authoring
            .stage(recipe, "Pattern sketch geometry")
            .map_err(|_| SketchEditError::AuthoringRejected)?;
        self.stage_core_transaction(transaction, "Pattern sketch geometry")
    }

    /// Handles exact modifier/pattern acquisition. Every completed operation
    /// is staged behind the universal tick/Enter gate; a failed pick leaves the
    /// live definition and stable-ID high-water marks untouched.
    fn handle_modifier_click(
        &mut self,
        point: SketchPoint,
        pick_radius: f64,
    ) -> Option<SketchEntityId> {
        if self.pending.is_some() {
            return match self.exact_tool {
                ToolVariant::Trim => self.append_pending_trim(point, pick_radius),
                ToolVariant::Fillet | ToolVariant::Chamfer | ToolVariant::TwoDistanceChamfer => {
                    self.append_pending_corner(point, pick_radius)
                }
                _ => None,
            };
        }
        match self.exact_tool {
            ToolVariant::Trim => {
                let target = self.exact_curve_hit(point, pick_radius)?;
                let target_record = self.authoring.entity(target)?;
                let limits = self
                    .authoring
                    .active_entities()
                    .filter(|record| {
                        record.id != target && record.visible && record.role == target_record.role
                    })
                    .map(|record| record.id)
                    .collect::<Vec<_>>();
                let transaction = self
                    .authoring
                    .stage_trim(
                        target,
                        limits,
                        core_point(point),
                        "Trim sketch curve",
                        PrecisionPolicy::default(),
                    )
                    .ok()?;
                self.select_core_entity_for_modifier(target);
                self.stage_core_transaction(transaction, "Trim sketch curve")
                    .ok()
            }
            ToolVariant::Fillet | ToolVariant::Chamfer | ToolVariant::TwoDistanceChamfer => {
                let picked = self.exact_curve_hit(point, pick_radius)?;
                if self.modifier_sources.is_empty() {
                    self.modifier_sources.push(picked);
                    self.modifier_picks.insert(picked, point);
                    self.select_core_entity_for_modifier(picked);
                    return None;
                }
                let first = self.modifier_sources[0];
                if picked == first {
                    return None;
                }
                self.modifier_picks.insert(picked, point);
                let staged = self.stage_corner_modifier(first, picked).ok();
                if staged.is_some() {
                    self.modifier_sources.clear();
                    self.modifier_picks.clear();
                }
                staged
            }
            ToolVariant::FixedRelation
            | ToolVariant::CoincidentRelation
            | ToolVariant::HorizontalRelation
            | ToolVariant::VerticalRelation
            | ToolVariant::DistanceRelation
            | ToolVariant::ParallelRelation
            | ToolVariant::PerpendicularRelation
            | ToolVariant::EqualLengthRelation
            | ToolVariant::TangentRelation
            | ToolVariant::CollinearRelation => self.append_relation_operand(point, pick_radius),
            ToolVariant::Offset => self.stage_offset(point, pick_radius).ok(),
            ToolVariant::RectangularPattern | ToolVariant::CircularPattern => {
                if self.modifier_sources.is_empty() {
                    let picked = self.exact_curve_hit(point, pick_radius)?;
                    self.modifier_sources.push(picked);
                    self.select_core_entity_for_modifier(picked);
                    self.ensure_pattern_manipulator();
                    return None;
                }
                let staged = if self.exact_tool == ToolVariant::RectangularPattern {
                    self.stage_rectangular_pattern(point).ok()
                } else {
                    self.stage_circular_pattern(point).ok()
                };
                if staged.is_some() {
                    self.modifier_sources.clear();
                }
                staged
            }
            _ => None,
        }
    }
}

/// One thing a relation can name.
///
/// Relations are equations over points, but a user picks what they see: a
/// whole line reads as "this line", an endpoint as "this corner". An endpoint
/// within the pick radius wins, exactly as it does for snapping, so the finer
/// intent is always reachable without a modifier key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RelationOperand {
    Point(CorePointId),
    Curve(CoreEntityId),
}

/// A circular operand's centre and the way it carries its radius.
#[derive(Clone, Copy, Debug)]
enum RoundCarrier {
    Circle {
        center: CorePointId,
        radius: f64,
    },
    Arc {
        center: CorePointId,
        rim: CorePointId,
    },
}

/// How many operands a relation needs before it can be staged.
const fn relation_arity(variant: ToolVariant) -> Option<usize> {
    match variant {
        ToolVariant::FixedRelation => Some(1),
        ToolVariant::HorizontalRelation | ToolVariant::VerticalRelation => Some(1),
        ToolVariant::CoincidentRelation
        | ToolVariant::DistanceRelation
        | ToolVariant::ParallelRelation
        | ToolVariant::PerpendicularRelation
        | ToolVariant::EqualLengthRelation
        | ToolVariant::TangentRelation
        | ToolVariant::CollinearRelation => Some(2),
        _ => None,
    }
}

/// The signed perpendicular offset of `subject` from the line through `a` and
/// `b`, along that line's left normal, or nothing when the line is too short
/// to name a direction.
fn signed_offset_from_line(a: CorePoint2, b: CorePoint2, subject: CorePoint2) -> Option<f64> {
    let (du, dv) = (b.u - a.u, b.v - a.v);
    let length = du.hypot(dv);
    if length <= PrecisionPolicy::default().min_feature_size {
        return None;
    }
    let normal = (-dv / length, du / length);
    Some((subject.u - a.u) * normal.0 + (subject.v - a.v) * normal.1)
}

/// One held relation, as the constraint panel lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct SketchConstraintSummary {
    pub id: CoreConstraintId,
    /// What the relation holds, in one or two words.
    pub label: &'static str,
    /// What it holds it on, in the user's terms rather than point ids.
    pub detail: String,
    /// The value it holds, for the relations that hold one. This is what makes
    /// a dimension retypable: the number is the relation, not a caption on it.
    pub value: Option<f64>,
    /// The constrained points, in sketch coordinates, for highlighting the
    /// relation on the canvas.
    pub points: Vec<SketchPoint>,
    /// The two ends of the dimension's leader, for the relations that measure
    /// something: from where it is measured to what it holds. A dimension the
    /// drawing does not show is a dimension nobody can check.
    pub leader: Option<(SketchPoint, SketchPoint)>,
}

/// What one held relation holds, named the way the user picked it rather than
/// by point identifier.
fn constraint_detail(kind: &CoreConstraintKind) -> String {
    let millimetres = |value: f64| format!("{value:.3} mm");
    match kind {
        CoreConstraintKind::Fixed { .. } => "pinned point".to_owned(),
        CoreConstraintKind::Coincident { .. } => "two points held together".to_owned(),
        CoreConstraintKind::Horizontal { .. } => "two points level".to_owned(),
        CoreConstraintKind::Vertical { .. } => "two points plumb".to_owned(),
        CoreConstraintKind::Distance { distance, .. } => millimetres(*distance),
        CoreConstraintKind::PointToLineDistance { distance, .. } => {
            format!("{} from an edge", millimetres(distance.abs()))
        }
        CoreConstraintKind::Parallel { .. } => "two lines parallel".to_owned(),
        CoreConstraintKind::Perpendicular { .. } => "two lines square".to_owned(),
        CoreConstraintKind::EqualLength { .. } => "two lines the same length".to_owned(),
        CoreConstraintKind::Tangent { .. } | CoreConstraintKind::LineTangentToArc { .. } => {
            "line touching an arc".to_owned()
        }
        CoreConstraintKind::Collinear { .. } => "three points in line".to_owned(),
        CoreConstraintKind::LineTangentToCircle { radius, .. } => {
            format!("line touching a circle of radius {}", millimetres(*radius))
        }
    }
}

/// The name a held relation goes by in the panel.
///
/// Shorter than the tool that made it: the panel column is narrow and the row
/// already says it is a relation.
const fn constraint_kind_label(kind: &CoreConstraintKind) -> &'static str {
    match kind {
        CoreConstraintKind::Fixed { .. } => "Fixed",
        CoreConstraintKind::Coincident { .. } => "Coincident",
        CoreConstraintKind::Horizontal { .. } => "Horizontal",
        CoreConstraintKind::Vertical { .. } => "Vertical",
        CoreConstraintKind::Distance { .. } => "Distance",
        CoreConstraintKind::PointToLineDistance { .. } => "Offset",
        CoreConstraintKind::Parallel { .. } => "Parallel",
        CoreConstraintKind::Perpendicular { .. } => "Perpendicular",
        CoreConstraintKind::EqualLength { .. } => "Equal length",
        CoreConstraintKind::Tangent { .. }
        | CoreConstraintKind::LineTangentToCircle { .. }
        | CoreConstraintKind::LineTangentToArc { .. } => "Tangent",
        CoreConstraintKind::Collinear { .. } => "Collinear",
    }
}

/// The label a staged relation carries into the confirmation gate and undo.
const fn relation_label(variant: ToolVariant) -> &'static str {
    match variant {
        ToolVariant::FixedRelation => "Fixed relation",
        ToolVariant::CoincidentRelation => "Coincident relation",
        ToolVariant::HorizontalRelation => "Horizontal relation",
        ToolVariant::VerticalRelation => "Vertical relation",
        ToolVariant::DistanceRelation => "Distance relation",
        ToolVariant::ParallelRelation => "Parallel relation",
        ToolVariant::PerpendicularRelation => "Perpendicular relation",
        ToolVariant::EqualLengthRelation => "Equal-length relation",
        ToolVariant::TangentRelation => "Tangent relation",
        ToolVariant::CollinearRelation => "Collinear relation",
        _ => "Sketch relation",
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct RegionAnalysisDiagnostics {
    closed_loops: usize,
    material_regions: usize,
    holes: usize,
    analytic_curves: usize,
    open_components: usize,
    branched_vertices: usize,
    intersections: usize,
}

#[derive(Clone, Debug)]
struct ProfileAnalysis {
    status: CertifiedProfileStatus,
    profile: Option<CertifiedSketchProfile>,
    diagnostics: RegionAnalysisDiagnostics,
}

impl ProfileAnalysis {
    fn status(status: CertifiedProfileStatus, diagnostics: RegionAnalysisDiagnostics) -> Self {
        Self {
            status,
            profile: None,
            diagnostics,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ProfileCurveSeed {
    source: SketchEntityId,
    subindex: u8,
    curve: CertifiedSketchCurve,
}

impl ProfileCurveSeed {
    fn endpoints(self) -> Option<[SketchPoint; 2]> {
        Some([self.curve.start()?, self.curve.end()?])
    }

    fn oriented_from(self, start: PointKey) -> Option<CertifiedSketchCurve> {
        let endpoints = self.endpoints()?;
        if PointKey::new(endpoints[0]) == start {
            Some(self.curve)
        } else if PointKey::new(endpoints[1]) == start {
            Some(self.curve.reversed())
        } else {
            None
        }
    }
}

/// An exact endpoint key. Interactive snapping writes identical coordinates;
/// profile extraction deliberately adds no secondary geometric tolerance.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PointKey {
    u: u64,
    v: u64,
}

impl PointKey {
    fn new(point: SketchPoint) -> Self {
        Self {
            u: ordered_f64_key(point.u),
            v: ordered_f64_key(point.v),
        }
    }
}

fn ordered_f64_key(value: f64) -> u64 {
    let bits = if value == 0.0 { 0 } else { value.to_bits() };
    if bits >> 63 == 0 {
        bits | (1_u64 << 63)
    } else {
        !bits
    }
}

fn analyze_profile_entities(entities: &[SketchEntity]) -> ProfileAnalysis {
    let mut diagnostics = RegionAnalysisDiagnostics::default();
    let profile_entities = entities
        .iter()
        .copied()
        .filter(|entity| entity.role == SketchEntityRole::Profile)
        .collect::<Vec<_>>();
    let entities = profile_entities.as_slice();
    if entities.is_empty() {
        return ProfileAnalysis::status(CertifiedProfileStatus::Empty, diagnostics);
    }
    if entities.iter().any(|entity| {
        !entity.geometry.is_finite()
            || entity.geometry.is_degenerate()
            || !analytic_geometry_is_certifiable(entity.geometry)
    }) {
        return ProfileAnalysis::status(CertifiedProfileStatus::Invalid, diagnostics);
    }

    // Reject bounded-command overflows before graph extraction or any
    // pairwise curve work. This keeps an oversized interactive sketch from
    // turning its resource-limit diagnostic into an unbounded UI pause.
    let authored_curve_count = entities.iter().fold(0_usize, |count, entity| {
        count.saturating_add(match entity.geometry {
            SketchGeometry::Point(_) => 0,
            SketchGeometry::Rectangle { .. } => 4,
            SketchGeometry::Segment { .. }
            | SketchGeometry::Circle { .. }
            | SketchGeometry::Arc { .. } => 1,
        })
    });
    if authored_curve_count > MAX_PLANAR_PROFILE_CURVES {
        return ProfileAnalysis::status(
            CertifiedProfileStatus::TooManyCurves {
                count: authored_curve_count,
            },
            diagnostics,
        );
    }

    let mut seeds = Vec::new();
    let mut loops = Vec::<CertifiedSketchLoop>::new();
    for entity in entities {
        match entity.geometry {
            // Standalone points are not profile edges. They remain visible
            // sketch geometry without changing the closed-region selection.
            SketchGeometry::Point(_) => {}
            SketchGeometry::Segment { start, end } => seeds.push(ProfileCurveSeed {
                source: entity.id,
                subindex: 0,
                curve: CertifiedSketchCurve::Line { start, end },
            }),
            SketchGeometry::Rectangle { .. } => {
                let corners = entity
                    .geometry
                    .rectangle_corners()
                    .expect("a validated rectangle has canonical corners");
                for index in 0..corners.len() {
                    seeds.push(ProfileCurveSeed {
                        source: entity.id,
                        subindex: index as u8,
                        curve: CertifiedSketchCurve::Line {
                            start: corners[index],
                            end: corners[(index + 1) % corners.len()],
                        },
                    });
                }
            }
            SketchGeometry::Circle { center, rim } => {
                diagnostics.analytic_curves += 1;
                loops.push(CertifiedSketchLoop {
                    winding: ProfileWinding::CounterClockwise,
                    nesting_depth: 0,
                    curves: vec![CertifiedSketchCurve::Circle {
                        center,
                        rim,
                        direction: SketchCurveDirection::CounterClockwise,
                    }],
                });
            }
            SketchGeometry::Arc { center, start, end } => {
                diagnostics.analytic_curves += 1;
                seeds.push(ProfileCurveSeed {
                    source: entity.id,
                    subindex: 0,
                    curve: CertifiedSketchCurve::CircularArc {
                        center,
                        start,
                        // Radius agreement was certified above. Preserve the
                        // exact authored endpoint for graph connectivity.
                        end,
                        direction: SketchCurveDirection::CounterClockwise,
                    },
                });
            }
        }
    }

    let extracted = extract_closed_wire_components(&seeds);
    diagnostics.open_components = extracted.open_components;
    diagnostics.branched_vertices = extracted.branched_vertices;
    loops.extend(
        extracted
            .loops
            .into_iter()
            .map(|curves| CertifiedSketchLoop {
                winding: ProfileWinding::CounterClockwise,
                nesting_depth: 0,
                curves,
            }),
    );
    diagnostics.closed_loops = loops.len();

    if loops.len() > MAX_PLANAR_PROFILE_LOOPS {
        return ProfileAnalysis::status(
            CertifiedProfileStatus::TooManyLoops { count: loops.len() },
            diagnostics,
        );
    }
    if let Some(count) = loops
        .iter()
        .filter(|profile_loop| !profile_loop.has_analytic_curves())
        .map(|profile_loop| profile_loop.curves.len())
        .find(|count| *count > MAX_EXTRUSION_PROFILE_VERTICES)
    {
        return ProfileAnalysis::status(
            CertifiedProfileStatus::LinearLoopTooLarge { count },
            diagnostics,
        );
    }

    if loops.is_empty() {
        return ProfileAnalysis::status(CertifiedProfileStatus::Open, diagnostics);
    }
    if diagnostics.open_components > 0 || diagnostics.branched_vertices > 0 {
        return ProfileAnalysis::status(CertifiedProfileStatus::Open, diagnostics);
    }

    let mut source_windings = Vec::with_capacity(loops.len());
    for sketch_loop in &mut loops {
        match certify_loop_winding(sketch_loop) {
            Ok(winding) => {
                sketch_loop.winding = winding;
                source_windings.push(winding);
            }
            Err(CertifiedProfileStatus::SelfIntersecting) => {
                diagnostics.intersections += 1;
                return ProfileAnalysis::status(
                    CertifiedProfileStatus::SelfIntersecting,
                    diagnostics,
                );
            }
            Err(status) => return ProfileAnalysis::status(status, diagnostics),
        }
    }

    for first in 0..loops.len() {
        // Linear self-intersection was already certified by
        // `classify_profile`; do not repeat its quadratic predicate pass.
        if loops[first].has_analytic_curves() && loop_has_self_intersection(&loops[first]) {
            diagnostics.intersections += 1;
            return ProfileAnalysis::status(CertifiedProfileStatus::SelfIntersecting, diagnostics);
        }
        for second in first + 1..loops.len() {
            if loops_intersect(&loops[first], &loops[second]) {
                diagnostics.intersections += 1;
                return ProfileAnalysis::status(
                    CertifiedProfileStatus::SelfIntersecting,
                    diagnostics,
                );
            }
        }
    }

    let Some(mut profile) = nest_and_normalize_loops(loops) else {
        return ProfileAnalysis::status(CertifiedProfileStatus::Indeterminate, diagnostics);
    };
    if profile.regions.len() > MAX_PLANAR_PROFILE_REGIONS {
        return ProfileAnalysis::status(
            CertifiedProfileStatus::TooManyRegions {
                count: profile.regions.len(),
            },
            diagnostics,
        );
    }
    diagnostics.material_regions = profile.regions.len();
    diagnostics.holes = profile.hole_count();
    diagnostics.closed_loops = profile.loop_count();
    let analytic = profile.has_analytic_curves();

    let status = if profile.loop_count() == 1 && profile.regions.len() == 1 {
        let only = &profile.regions[0].outer;
        if matches!(
            only.curves.as_slice(),
            [CertifiedSketchCurve::Circle { .. }]
        ) {
            CertifiedProfileStatus::ClosedAnalyticCircle
        } else if analytic {
            CertifiedProfileStatus::ClosedAnalyticCurves
        } else {
            CertifiedProfileStatus::Closed {
                winding: legacy_authored_linear_winding(entities).unwrap_or(source_windings[0]),
            }
        }
    } else {
        CertifiedProfileStatus::ClosedRegions {
            regions: profile.regions.len(),
            loops: profile.loop_count(),
            holes: profile.hole_count(),
            analytic,
        }
    };
    // Avoid retaining spare capacity in the stable payload; this also makes
    // allocation behavior deterministic in semantic tests.
    profile.regions.shrink_to_fit();
    ProfileAnalysis {
        status,
        profile: Some(profile),
        diagnostics,
    }
}

fn analytic_geometry_is_certifiable(geometry: SketchGeometry) -> bool {
    let SketchGeometry::Arc { center, start, end } = geometry else {
        return true;
    };
    let start_radius = center.distance_squared(start).sqrt();
    let end_radius = center.distance_squared(end).sqrt();
    let scale = start_radius.max(end_radius).max(1.0);
    start_radius.is_finite()
        && end_radius.is_finite()
        && (start_radius - end_radius).abs() <= ARC_RADIUS_RELATIVE_TOLERANCE * scale
        && arc_sweep(center, start, end) > MIN_ARC_SWEEP_DEGREES.to_radians()
        && arc_sweep(center, start, end) < MAX_ARC_SWEEP_DEGREES.to_radians()
}

#[derive(Default)]
struct ExtractedWireComponents {
    loops: Vec<Vec<CertifiedSketchCurve>>,
    open_components: usize,
    branched_vertices: usize,
}

fn extract_closed_wire_components(seeds: &[ProfileCurveSeed]) -> ExtractedWireComponents {
    let mut result = ExtractedWireComponents::default();
    if seeds.is_empty() {
        return result;
    }
    let mut adjacency = BTreeMap::<PointKey, Vec<usize>>::new();
    for (index, seed) in seeds.iter().copied().enumerate() {
        let Some([start, end]) = seed.endpoints() else {
            continue;
        };
        adjacency
            .entry(PointKey::new(start))
            .or_default()
            .push(index);
        adjacency.entry(PointKey::new(end)).or_default().push(index);
    }
    for incident in adjacency.values_mut() {
        incident.sort_by_key(|index| seed_key(seeds[*index]));
    }

    let mut remaining = (0..seeds.len()).collect::<BTreeSet<_>>();
    while let Some(first) = remaining.first().copied() {
        let mut component = BTreeSet::new();
        let mut stack = vec![first];
        while let Some(index) = stack.pop() {
            if !component.insert(index) {
                continue;
            }
            if let Some([start, end]) = seeds[index].endpoints() {
                for endpoint in [PointKey::new(start), PointKey::new(end)] {
                    if let Some(incident) = adjacency.get(&endpoint) {
                        stack.extend(incident.iter().copied());
                    }
                }
            }
        }
        for index in &component {
            remaining.remove(index);
        }

        let vertices = component
            .iter()
            .flat_map(|index| {
                seeds[*index]
                    .endpoints()
                    .into_iter()
                    .flatten()
                    .map(PointKey::new)
            })
            .collect::<BTreeSet<_>>();
        let mut closed = true;
        for vertex in &vertices {
            let degree = adjacency.get(vertex).map_or(0, |incident| {
                incident
                    .iter()
                    .filter(|index| component.contains(index))
                    .count()
            });
            if degree != 2 {
                closed = false;
            }
            if degree > 2 {
                result.branched_vertices += 1;
            }
        }
        if !closed {
            result.open_components += 1;
            continue;
        }

        let start_vertex = *vertices
            .first()
            .expect("a non-empty component has vertices");
        let first_edge = adjacency[&start_vertex]
            .iter()
            .copied()
            .filter(|index| component.contains(index))
            .min_by_key(|index| traversal_seed_key(seeds[*index], start_vertex))
            .expect("a degree-two vertex has an incident edge");
        let mut current_vertex = start_vertex;
        let mut current_edge = first_edge;
        let mut walked = BTreeSet::new();
        let mut curves = Vec::with_capacity(component.len());
        loop {
            if !walked.insert(current_edge) {
                break;
            }
            let curve = seeds[current_edge]
                .oriented_from(current_vertex)
                .expect("the walk vertex belongs to its edge");
            let next_vertex = PointKey::new(curve.end().expect("wire curves have endpoints"));
            curves.push(curve);
            if next_vertex == start_vertex {
                break;
            }
            let Some(next_edge) = adjacency[&next_vertex]
                .iter()
                .copied()
                .filter(|index| component.contains(index) && *index != current_edge)
                .min_by_key(|index| traversal_seed_key(seeds[*index], next_vertex))
            else {
                break;
            };
            current_vertex = next_vertex;
            current_edge = next_edge;
        }
        if walked == component
            && curves
                .last()
                .and_then(|curve| curve.end())
                .is_some_and(|point| PointKey::new(point) == start_vertex)
        {
            result.loops.push(curves);
        } else {
            result.open_components += 1;
        }
    }
    result
}

fn seed_key(seed: ProfileCurveSeed) -> (PointKey, PointKey, u64, u8) {
    let [start, end] = seed
        .endpoints()
        .expect("only endpoint-owned curves enter the wire graph");
    let first = PointKey::new(start).min(PointKey::new(end));
    let second = PointKey::new(start).max(PointKey::new(end));
    (first, second, seed.source.get(), seed.subindex)
}

fn traversal_seed_key(seed: ProfileCurveSeed, from: PointKey) -> (PointKey, u64, u8) {
    let [start, end] = seed
        .endpoints()
        .expect("only endpoint-owned curves enter the wire graph");
    let other = if PointKey::new(start) == from {
        PointKey::new(end)
    } else {
        PointKey::new(start)
    };
    (other, seed.source.get(), seed.subindex)
}

fn legacy_authored_linear_winding(entities: &[SketchEntity]) -> Option<ProfileWinding> {
    if let [
        SketchEntity {
            geometry: SketchGeometry::Rectangle { .. },
            ..
        },
    ] = entities
    {
        return Some(ProfileWinding::CounterClockwise);
    }
    let first = entities.first()?;
    let SketchGeometry::Segment { start, end } = first.geometry else {
        return None;
    };
    let mut points = vec![start, end];
    for entity in &entities[1..] {
        let SketchGeometry::Segment { start, end } = entity.geometry else {
            return None;
        };
        if points.last().copied() != Some(start) {
            return None;
        }
        points.push(end);
    }
    match classify_sketch_polyline(&points) {
        CertifiedProfileStatus::Closed { winding } => Some(winding),
        _ => None,
    }
}

fn certify_loop_winding(
    sketch_loop: &CertifiedSketchLoop,
) -> Result<ProfileWinding, CertifiedProfileStatus> {
    if sketch_loop.curves.is_empty() {
        return Err(CertifiedProfileStatus::Invalid);
    }
    if let Some(vertices) = sketch_loop.linear_vertices() {
        let mut closed = vertices;
        closed.push(closed[0]);
        return match classify_sketch_polyline(&closed) {
            CertifiedProfileStatus::Closed { winding } => Ok(winding),
            status => Err(status),
        };
    }
    if !loop_connectivity_is_exact(sketch_loop) {
        return Err(CertifiedProfileStatus::Open);
    }
    let signed_area = loop_signed_area(sketch_loop).ok_or(CertifiedProfileStatus::Indeterminate)?;
    let scale = loop_coordinate_scale(sketch_loop);
    let area_tolerance = 256.0 * f64::EPSILON * scale * scale;
    if signed_area > area_tolerance {
        Ok(ProfileWinding::CounterClockwise)
    } else if signed_area < -area_tolerance {
        Ok(ProfileWinding::Clockwise)
    } else {
        Err(CertifiedProfileStatus::Indeterminate)
    }
}

fn loop_connectivity_is_exact(sketch_loop: &CertifiedSketchLoop) -> bool {
    if matches!(
        sketch_loop.curves.as_slice(),
        [CertifiedSketchCurve::Circle { .. }]
    ) {
        return true;
    }
    sketch_loop.curves.iter().enumerate().all(|(index, curve)| {
        let next = sketch_loop.curves[(index + 1) % sketch_loop.curves.len()];
        curve.end().is_some() && curve.end() == next.start()
    })
}

fn loop_coordinate_scale(sketch_loop: &CertifiedSketchLoop) -> f64 {
    sketch_loop
        .curves
        .iter()
        .flat_map(|curve| curve_control_points(*curve))
        .fold(1.0_f64, |scale, point| {
            scale.max(point.u.abs()).max(point.v.abs())
        })
}

fn curve_control_points(curve: CertifiedSketchCurve) -> Vec<SketchPoint> {
    match curve {
        CertifiedSketchCurve::Line { start, end } => vec![start, end],
        CertifiedSketchCurve::CircularArc {
            center, start, end, ..
        } => vec![center, start, end],
        CertifiedSketchCurve::Circle { center, rim, .. } => vec![center, rim],
    }
}

fn loop_signed_area(sketch_loop: &CertifiedSketchLoop) -> Option<f64> {
    let mut twice_area = 0.0;
    for curve in &sketch_loop.curves {
        let contribution = match *curve {
            CertifiedSketchCurve::Line { start, end } => start.u.mul_add(end.v, -start.v * end.u),
            CertifiedSketchCurve::CircularArc {
                center,
                start,
                end,
                direction,
            } => {
                let radius = center.distance_squared(start).sqrt();
                let counter_clockwise_sweep = arc_sweep(center, start, end);
                let signed_sweep = direction.signed_sweep(counter_clockwise_sweep);
                center.u.mul_add(
                    end.v - start.v,
                    -center.v * (end.u - start.u) + radius * radius * signed_sweep,
                )
            }
            CertifiedSketchCurve::Circle {
                center,
                rim,
                direction,
            } => {
                let radius_squared = center.distance_squared(rim);
                let sign = match direction {
                    SketchCurveDirection::CounterClockwise => 1.0,
                    SketchCurveDirection::Clockwise => -1.0,
                };
                return Some(sign * std::f64::consts::PI * radius_squared);
            }
        };
        twice_area += contribution;
        if !twice_area.is_finite() {
            return None;
        }
    }
    Some(0.5 * twice_area)
}

fn nest_and_normalize_loops(mut loops: Vec<CertifiedSketchLoop>) -> Option<CertifiedSketchProfile> {
    let samples = loops
        .iter()
        .map(loop_containment_sample)
        .collect::<Option<Vec<_>>>()?;
    let mut depths = vec![0_usize; loops.len()];
    for inner in 0..loops.len() {
        for (outer, outer_loop) in loops.iter().enumerate() {
            if inner != outer && point_inside_loop(samples[inner], outer_loop)? {
                depths[inner] += 1;
            }
        }
    }
    for (sketch_loop, depth) in loops.iter_mut().zip(depths.iter().copied()) {
        sketch_loop.nesting_depth = depth;
        let desired = if depth.is_multiple_of(2) {
            ProfileWinding::CounterClockwise
        } else {
            ProfileWinding::Clockwise
        };
        normalize_loop(sketch_loop, desired);
    }

    let mut regions = Vec::new();
    for outer_index in 0..loops.len() {
        if !depths[outer_index].is_multiple_of(2) {
            continue;
        }
        let mut holes = (0..loops.len())
            .filter(|hole_index| {
                depths[*hole_index] == depths[outer_index] + 1
                    && point_inside_loop(samples[*hole_index], &loops[outer_index]) == Some(true)
            })
            .map(|hole_index| loops[hole_index].clone())
            .collect::<Vec<_>>();
        holes.sort_by_key(loop_sort_key);
        regions.push(CertifiedSketchRegion {
            outer: loops[outer_index].clone(),
            holes,
        });
    }
    regions.sort_by_key(|region| loop_sort_key(&region.outer));
    (!regions.is_empty()).then_some(CertifiedSketchProfile { regions })
}

fn normalize_loop(sketch_loop: &mut CertifiedSketchLoop, desired: ProfileWinding) {
    if sketch_loop.winding != desired {
        sketch_loop.curves = sketch_loop
            .curves
            .iter()
            .rev()
            .copied()
            .map(CertifiedSketchCurve::reversed)
            .collect();
        sketch_loop.winding = desired;
    }
    if sketch_loop.curves.len() <= 1 {
        return;
    }
    let start = (0..sketch_loop.curves.len())
        .min_by_key(|index| certified_curve_sort_key(sketch_loop.curves[*index]))
        .expect("a non-empty loop has a canonical start");
    sketch_loop.curves.rotate_left(start);
}

fn loop_sort_key(sketch_loop: &CertifiedSketchLoop) -> [u64; 8] {
    sketch_loop
        .curves
        .first()
        .copied()
        .map_or([0; 8], certified_curve_sort_key)
}

fn certified_curve_sort_key(curve: CertifiedSketchCurve) -> [u64; 8] {
    match curve {
        CertifiedSketchCurve::Line { start, end } => [
            0,
            ordered_f64_key(start.u),
            ordered_f64_key(start.v),
            ordered_f64_key(end.u),
            ordered_f64_key(end.v),
            0,
            0,
            0,
        ],
        CertifiedSketchCurve::CircularArc {
            center,
            start,
            end,
            direction,
        } => [
            1,
            ordered_f64_key(start.u),
            ordered_f64_key(start.v),
            ordered_f64_key(end.u),
            ordered_f64_key(end.v),
            ordered_f64_key(center.u),
            ordered_f64_key(center.v),
            u64::from(direction == SketchCurveDirection::Clockwise),
        ],
        CertifiedSketchCurve::Circle {
            center,
            rim,
            direction,
        } => [
            2,
            ordered_f64_key(center.u),
            ordered_f64_key(center.v),
            ordered_f64_key(rim.u),
            ordered_f64_key(rim.v),
            u64::from(direction == SketchCurveDirection::Clockwise),
            0,
            0,
        ],
    }
}

fn loop_containment_sample(sketch_loop: &CertifiedSketchLoop) -> Option<SketchPoint> {
    match sketch_loop.curves.first().copied()? {
        // A circle center is also inside every concentric child/parent and
        // therefore cannot distinguish nesting depth. Its rim is on this
        // loop but, after intersection rejection, strictly inside or outside
        // every other loop being tested.
        CertifiedSketchCurve::Circle { rim, .. } => Some(rim),
        curve => curve.start(),
    }
}

fn point_inside_loop(point: SketchPoint, sketch_loop: &CertifiedSketchLoop) -> Option<bool> {
    let mut query_y = point.v;
    if sketch_loop
        .curves
        .iter()
        .flat_map(|curve| curve_endpoints(*curve))
        .any(|endpoint| endpoint.v == query_y)
    {
        query_y = query_y.next_up();
    }
    let query = SketchPoint::new(point.u, query_y);
    let mut crossings = 0_usize;
    for curve in &sketch_loop.curves {
        crossings += curve_ray_crossings(query, *curve)?;
    }
    Some(crossings % 2 == 1)
}

fn curve_endpoints(curve: CertifiedSketchCurve) -> Vec<SketchPoint> {
    match curve {
        CertifiedSketchCurve::Line { start, end }
        | CertifiedSketchCurve::CircularArc { start, end, .. } => vec![start, end],
        CertifiedSketchCurve::Circle { .. } => Vec::new(),
    }
}

fn curve_ray_crossings(point: SketchPoint, curve: CertifiedSketchCurve) -> Option<usize> {
    match curve {
        CertifiedSketchCurve::Line { start, end } => {
            if (start.v > point.v) == (end.v > point.v) {
                return Some(0);
            }
            let crossing =
                (end.u - start.u).mul_add((point.v - start.v) / (end.v - start.v), start.u);
            Some(usize::from(crossing > point.u))
        }
        CertifiedSketchCurve::Circle { center, rim, .. } => {
            let radius = center.distance_squared(rim).sqrt();
            let dy = point.v - center.v;
            if dy.abs() >= radius {
                return Some(0);
            }
            let offset = (radius * radius - dy * dy).sqrt();
            Some(
                usize::from(center.u - offset > point.u) + usize::from(center.u + offset > point.u),
            )
        }
        CertifiedSketchCurve::CircularArc {
            center,
            start,
            end,
            direction,
        } => {
            let radius = center.distance_squared(start).sqrt();
            let dy = point.v - center.v;
            if dy.abs() >= radius {
                return Some(0);
            }
            let offset = (radius * radius - dy * dy).sqrt();
            let mut count = 0;
            for x in [center.u - offset, center.u + offset] {
                let candidate = SketchPoint::new(x, point.v);
                if x > point.u && point_on_arc(candidate, center, start, end, direction) {
                    count += 1;
                }
            }
            Some(count)
        }
    }
}

fn point_on_arc(
    point: SketchPoint,
    center: SketchPoint,
    start: SketchPoint,
    end: SketchPoint,
    direction: SketchCurveDirection,
) -> bool {
    let start_angle = (start.v - center.v).atan2(start.u - center.u);
    let end_angle = (end.v - center.v).atan2(end.u - center.u);
    let point_angle = (point.v - center.v).atan2(point.u - center.u);
    match direction {
        SketchCurveDirection::CounterClockwise => {
            (point_angle - start_angle).rem_euclid(std::f64::consts::TAU)
                <= (end_angle - start_angle).rem_euclid(std::f64::consts::TAU)
        }
        SketchCurveDirection::Clockwise => {
            (start_angle - point_angle).rem_euclid(std::f64::consts::TAU)
                <= (start_angle - end_angle).rem_euclid(std::f64::consts::TAU)
        }
    }
}

fn loop_has_self_intersection(sketch_loop: &CertifiedSketchLoop) -> bool {
    if sketch_loop.curves.len() <= 1 {
        return false;
    }
    for first in 0..sketch_loop.curves.len() {
        for second in first + 1..sketch_loop.curves.len() {
            if curves_intersect_beyond_shared_endpoints(
                sketch_loop.curves[first],
                sketch_loop.curves[second],
            ) {
                return true;
            }
        }
    }
    false
}

fn loops_intersect(first: &CertifiedSketchLoop, second: &CertifiedSketchLoop) -> bool {
    first.curves.iter().any(|first_curve| {
        second.curves.iter().any(|second_curve| {
            curves_intersect_beyond_shared_endpoints(*first_curve, *second_curve)
        })
    })
}

fn curves_intersect_beyond_shared_endpoints(
    first: CertifiedSketchCurve,
    second: CertifiedSketchCurve,
) -> bool {
    let allowed = curve_endpoints(first)
        .into_iter()
        .filter(|point| curve_endpoints(second).contains(point))
        .collect::<Vec<_>>();
    let scale = curve_intersection_scale(first, second);
    let tolerance_squared = (256.0 * f64::EPSILON * scale).powi(2);
    match exact_curve_intersections(first, second) {
        CurveIntersections::Coincident => coincident_curves_overlap(first, second),
        // A circle resting on a side is two regions meeting at a point, not
        // a self-intersecting profile; the arrangement splits both carriers
        // there and the cells stay selectable.
        CurveIntersections::Tangent => false,
        CurveIntersections::Points(points) => points.into_iter().any(|point| {
            !allowed
                .iter()
                .any(|shared| shared.distance_squared(point) <= tolerance_squared)
        }),
    }
}

fn curve_intersection_scale(first: CertifiedSketchCurve, second: CertifiedSketchCurve) -> f64 {
    curve_control_points(first)
        .into_iter()
        .chain(curve_control_points(second))
        .fold(1.0_f64, |scale, point| {
            scale.max(point.u.abs()).max(point.v.abs())
        })
}

enum CurveIntersections {
    Points(Vec<SketchPoint>),
    /// One coalesced contact where the carriers share a tangent. The curves
    /// touch without crossing, so the loops on either side share no area.
    Tangent,
    Coincident,
}

fn exact_curve_intersections(
    first: CertifiedSketchCurve,
    second: CertifiedSketchCurve,
) -> CurveIntersections {
    match (first, second) {
        (
            CertifiedSketchCurve::Line {
                start: first_start,
                end: first_end,
            },
            CertifiedSketchCurve::Line {
                start: second_start,
                end: second_end,
            },
        ) => line_line_intersections(first_start, first_end, second_start, second_end),
        (CertifiedSketchCurve::Line { start, end }, circle_curve) => {
            line_circular_curve_intersections(start, end, circle_curve)
        }
        (circle_curve, CertifiedSketchCurve::Line { start, end }) => {
            line_circular_curve_intersections(start, end, circle_curve)
        }
        (first_circle, second_circle) => circular_curve_intersections(first_circle, second_circle),
    }
}

fn line_line_intersections(
    first_start: SketchPoint,
    first_end: SketchPoint,
    second_start: SketchPoint,
    second_end: SketchPoint,
) -> CurveIntersections {
    let p = Point2::new(first_start.u, first_start.v);
    let p2 = Point2::new(first_end.u, first_end.v);
    let q = Point2::new(second_start.u, second_start.v);
    let q2 = Point2::new(second_end.u, second_end.v);
    let orientations = [
        orient2d(p, p2, q),
        orient2d(p, p2, q2),
        orient2d(q, q2, p),
        orient2d(q, q2, p2),
    ];
    if orientations
        .iter()
        .all(|orientation| *orientation == Orientation2::Collinear)
    {
        let mut contacts = [first_start, first_end, second_start, second_end]
            .into_iter()
            .filter(|point| {
                point_in_segment_bounds(*point, first_start, first_end)
                    && point_in_segment_bounds(*point, second_start, second_end)
            })
            .collect::<Vec<_>>();
        contacts.sort_by_key(|point| PointKey::new(*point));
        contacts.dedup();
        return if contacts.len() > 1 {
            CurveIntersections::Coincident
        } else {
            CurveIntersections::Points(contacts)
        };
    }
    let first_dx = first_end.u - first_start.u;
    let first_dy = first_end.v - first_start.v;
    let second_dx = second_end.u - second_start.u;
    let second_dy = second_end.v - second_start.v;
    let denominator = first_dx.mul_add(second_dy, -first_dy * second_dx);
    if denominator == 0.0 || !denominator.is_finite() {
        return CurveIntersections::Points(Vec::new());
    }
    let offset_u = second_start.u - first_start.u;
    let offset_v = second_start.v - first_start.v;
    let first_t = offset_u.mul_add(second_dy, -offset_v * second_dx) / denominator;
    let second_t = offset_u.mul_add(first_dy, -offset_v * first_dx) / denominator;
    if (0.0..=1.0).contains(&first_t) && (0.0..=1.0).contains(&second_t) {
        CurveIntersections::Points(vec![SketchPoint::new(
            first_dx.mul_add(first_t, first_start.u),
            first_dy.mul_add(first_t, first_start.v),
        )])
    } else {
        CurveIntersections::Points(Vec::new())
    }
}

fn point_in_segment_bounds(point: SketchPoint, start: SketchPoint, end: SketchPoint) -> bool {
    (start.u.min(end.u)..=start.u.max(end.u)).contains(&point.u)
        && (start.v.min(end.v)..=start.v.max(end.v)).contains(&point.v)
}

fn line_circular_curve_intersections(
    start: SketchPoint,
    end: SketchPoint,
    circular: CertifiedSketchCurve,
) -> CurveIntersections {
    let Some((center, radius)) = curve_circle(circular) else {
        return CurveIntersections::Points(Vec::new());
    };
    let du = end.u - start.u;
    let dv = end.v - start.v;
    let offset_u = start.u - center.u;
    let offset_v = start.v - center.v;
    let a = du.mul_add(du, dv * dv);
    let b = 2.0 * offset_u.mul_add(du, offset_v * dv);
    let c = offset_u.mul_add(offset_u, offset_v * offset_v) - radius * radius;
    let discriminant = b.mul_add(b, -4.0 * a * c);
    if discriminant < 0.0 || !discriminant.is_finite() {
        return CurveIntersections::Points(Vec::new());
    }
    let root = discriminant.sqrt();
    let mut points = [(-b - root) / (2.0 * a), (-b + root) / (2.0 * a)]
        .into_iter()
        .filter(|parameter| (0.0..=1.0).contains(parameter))
        .map(|parameter| {
            SketchPoint::new(
                du.mul_add(parameter, start.u),
                dv.mul_add(parameter, start.v),
            )
        })
        .filter(|point| point_belongs_to_circular_curve(*point, circular))
        .collect::<Vec<_>>();
    points.sort_by_key(|point| PointKey::new(*point));
    points.dedup_by(|left, right| left.distance_squared(*right) <= f64::EPSILON.powi(2));
    if discriminant == 0.0 && points.len() == 1 {
        return CurveIntersections::Tangent;
    }
    CurveIntersections::Points(points)
}

fn circular_curve_intersections(
    first: CertifiedSketchCurve,
    second: CertifiedSketchCurve,
) -> CurveIntersections {
    let Some((first_center, first_radius)) = curve_circle(first) else {
        return CurveIntersections::Points(Vec::new());
    };
    let Some((second_center, second_radius)) = curve_circle(second) else {
        return CurveIntersections::Points(Vec::new());
    };
    let center_distance = first_center.distance_squared(second_center).sqrt();
    if center_distance == 0.0 && first_radius == second_radius {
        return CurveIntersections::Coincident;
    }
    if center_distance == 0.0
        || center_distance > first_radius + second_radius
        || center_distance < (first_radius - second_radius).abs()
    {
        return CurveIntersections::Points(Vec::new());
    }
    let along = (first_radius * first_radius - second_radius * second_radius
        + center_distance * center_distance)
        / (2.0 * center_distance);
    let height_squared = first_radius * first_radius - along * along;
    if height_squared < 0.0 {
        return CurveIntersections::Points(Vec::new());
    }
    let unit_u = (second_center.u - first_center.u) / center_distance;
    let unit_v = (second_center.v - first_center.v) / center_distance;
    let base = SketchPoint::new(
        unit_u.mul_add(along, first_center.u),
        unit_v.mul_add(along, first_center.v),
    );
    let height = height_squared.sqrt();
    let mut points = [
        SketchPoint::new(
            (-unit_v).mul_add(height, base.u),
            unit_u.mul_add(height, base.v),
        ),
        SketchPoint::new(
            unit_v.mul_add(height, base.u),
            (-unit_u).mul_add(height, base.v),
        ),
    ]
    .into_iter()
    .filter(|point| {
        point_belongs_to_circular_curve(*point, first)
            && point_belongs_to_circular_curve(*point, second)
    })
    .collect::<Vec<_>>();
    points.sort_by_key(|point| PointKey::new(*point));
    points.dedup_by(|left, right| left.distance_squared(*right) <= f64::EPSILON.powi(2));
    if height == 0.0 && points.len() == 1 {
        return CurveIntersections::Tangent;
    }
    CurveIntersections::Points(points)
}

fn curve_circle(curve: CertifiedSketchCurve) -> Option<(SketchPoint, f64)> {
    match curve {
        CertifiedSketchCurve::CircularArc { center, start, .. } => {
            Some((center, center.distance_squared(start).sqrt()))
        }
        CertifiedSketchCurve::Circle { center, rim, .. } => {
            Some((center, center.distance_squared(rim).sqrt()))
        }
        CertifiedSketchCurve::Line { .. } => None,
    }
}

fn point_belongs_to_circular_curve(point: SketchPoint, curve: CertifiedSketchCurve) -> bool {
    match curve {
        CertifiedSketchCurve::CircularArc {
            center,
            start,
            end,
            direction,
        } => point_on_arc(point, center, start, end, direction),
        CertifiedSketchCurve::Circle { .. } => true,
        CertifiedSketchCurve::Line { .. } => false,
    }
}

fn coincident_curves_overlap(first: CertifiedSketchCurve, second: CertifiedSketchCurve) -> bool {
    match (first, second) {
        (CertifiedSketchCurve::Circle { .. }, _) | (_, CertifiedSketchCurve::Circle { .. }) => true,
        (
            CertifiedSketchCurve::CircularArc {
                center,
                start,
                end,
                direction,
            },
            second,
        ) => {
            let midpoint = arc_midpoint(center, start, end, direction);
            if point_belongs_to_circular_curve(midpoint, second) {
                return true;
            }
            let CertifiedSketchCurve::CircularArc {
                center,
                start,
                end,
                direction,
            } = second
            else {
                return false;
            };
            point_belongs_to_circular_curve(arc_midpoint(center, start, end, direction), first)
        }
        _ => true,
    }
}

fn arc_midpoint(
    center: SketchPoint,
    start: SketchPoint,
    end: SketchPoint,
    direction: SketchCurveDirection,
) -> SketchPoint {
    let start_angle = (start.v - center.v).atan2(start.u - center.u);
    let counter_clockwise_sweep = arc_sweep(center, start, end);
    let signed_sweep = direction.signed_sweep(counter_clockwise_sweep);
    let angle = 0.5_f64.mul_add(signed_sweep, start_angle);
    let radius = center.distance_squared(start).sqrt();
    SketchPoint::new(
        radius.mul_add(angle.cos(), center.u),
        radius.mul_add(angle.sin(), center.v),
    )
}

fn classify_sketch_polyline(points: &[SketchPoint]) -> CertifiedProfileStatus {
    let owned = points
        .iter()
        .map(|point| Point2::new(point.u, point.v))
        .collect::<Vec<_>>();
    match classify_profile(&owned) {
        ProfileClassification::Open => CertifiedProfileStatus::Open,
        ProfileClassification::Closed { winding } => CertifiedProfileStatus::Closed { winding },
        ProfileClassification::SelfIntersecting => CertifiedProfileStatus::SelfIntersecting,
        ProfileClassification::Invalid(_) => CertifiedProfileStatus::Invalid,
        ProfileClassification::Indeterminate => CertifiedProfileStatus::Indeterminate,
    }
}

/// Per-frame observations returned to the workbench integration layer.
#[must_use]
pub struct SketchCanvasOutput {
    pub response: Response,
    pub hovered: Option<SketchEntityId>,
    pub selection_changed: bool,
    pub navigation_changed: bool,
    pub draft_changed: bool,
    pub pending_created: Option<SketchEntityId>,
    pub dimension_keys: DimensionKeyClaims,
    /// The on-canvas recipe field that owns the keyboard, with the rectangle it
    /// occupies, so the workbench can settle acceptance from the same place it
    /// settles the Properties field.
    pub recipe_dimension_field: Option<(Id, Rect)>,
    /// An on-canvas recipe field consumed `Enter` this frame and the value
    /// should be published now.
    pub recipe_dimension_accepted: bool,
}

/// Size of the text in a dimension's value chip.
const DIMENSION_CHIP_TEXT_SIZE: f32 = 10.0;
/// Breathing room around that text inside the chip.
const DIMENSION_CHIP_PADDING: Vec2 = egui::vec2(8.0, 4.0);

/// What a dimension's value reads as on the drawing.
fn dimension_chip_text(value: f64) -> String {
    format!("{value:.2} mm")
}

#[derive(Clone, Copy, Debug, Default)]
struct DimensionDragInteraction {
    consumes_primary: bool,
    changed: bool,
}

/// Drags a dimension's value clear of the geometry it measures.
///
/// A dimension whose number sits on top of the drawing is a dimension a drawer
/// moves, so the chip is a handle. It takes the primary button while it is held
/// — the same way the pattern manipulator does — so grabbing a label never also
/// picks the geometry underneath it.
fn handle_dimension_drag_input(
    ui: &Ui,
    canvas: &Response,
    state: &mut SketchCanvasState,
) -> DimensionDragInteraction {
    if state.pending.is_some() {
        state.dimension_drag.cancel();
        state.dimension_drag_target = None;
        return DimensionDragInteraction::default();
    }
    let painter = ui.painter();
    let pointer = PointerSample::primary(ui, canvas.rect);
    let initial_hit = pointer.position.is_some_and(|position| {
        state
            .dimension_chip_at(painter, canvas.rect, position)
            .is_some()
    });
    let handle = state.dimension_drag.update(pointer, initial_hit);
    let mut interaction = DimensionDragInteraction {
        consumes_primary: handle.consumes_primary,
        ..DimensionDragInteraction::default()
    };
    // The pointer says the chip is a handle before the user finds out by
    // trying it.
    if initial_hit || state.dimension_drag_target.is_some() {
        ui.ctx()
            .set_cursor_icon(if state.dimension_drag_target.is_some() {
                CursorIcon::Grabbing
            } else {
                CursorIcon::Grab
            });
    }
    let Some(event) = handle.event else {
        return interaction;
    };
    if event.phase == DragHandlePhase::Started
        && state.begin_dimension_drag(painter, canvas.rect, event.position)
    {
        canvas.request_focus();
        interaction.changed = true;
    }
    if matches!(
        event.phase,
        DragHandlePhase::Dragging | DragHandlePhase::Finished
    ) {
        interaction.changed |= state.update_dimension_drag(canvas.rect, event.position);
    }
    if event.phase == DragHandlePhase::Finished {
        state.dimension_drag_target = None;
    }
    interaction
}

#[derive(Clone, Copy, Debug, Default)]
struct PatternManipulatorInteraction {
    consumes_primary: bool,
    changed: bool,
    pending_created: Option<SketchEntityId>,
}

fn handle_pattern_manipulator_input(
    ui: &Ui,
    canvas: &Response,
    state: &mut SketchCanvasState,
) -> PatternManipulatorInteraction {
    state.ensure_pattern_manipulator();
    if state.pattern_manipulator.is_none() || state.pending.is_some() {
        state.pattern_drag.cancel();
        return PatternManipulatorInteraction::default();
    }
    let pointer = PointerSample::primary(ui, canvas.rect);
    let initial_hit = pointer.position.is_some_and(|position| {
        state.pattern_manipulator.is_some_and(|manipulator| {
            state
                .view
                .sketch_to_screen(canvas.rect, manipulator.position)
                .distance(position)
                <= PATTERN_MANIPULATOR_HIT_RADIUS_POINTS
        })
    });
    let handle = state.pattern_drag.update(pointer, initial_hit);
    let mut interaction = PatternManipulatorInteraction {
        consumes_primary: handle.consumes_primary,
        ..PatternManipulatorInteraction::default()
    };
    if let Some(event) = handle.event {
        if event.phase == DragHandlePhase::Started
            && state.begin_pattern_manipulator_drag(canvas.rect, event.position)
        {
            canvas.request_focus();
            interaction.changed = true;
        }
        if matches!(
            event.phase,
            DragHandlePhase::Dragging | DragHandlePhase::Finished
        ) {
            let position = state.view.screen_to_sketch(canvas.rect, event.position);
            interaction.changed |= state.update_pattern_manipulator_drag(position);
        }
        if event.phase == DragHandlePhase::Finished {
            interaction.pending_created = state.release_pattern_manipulator_drag();
            interaction.changed = true;
        }
    }
    interaction
}

/// Renders and interacts with a sketch state without executing a kernel operation.
pub fn show(ui: &mut Ui, state: &mut SketchCanvasState) -> SketchCanvasOutput {
    show_with_context(ui, state, None)
}

/// Renders a sketch with an optional read-only projected-body context.
///
/// The context is painted beneath the grid and all sketch geometry. Passing
/// `None` is exactly the legacy [`show`] path.
pub fn show_with_context(
    ui: &mut Ui,
    state: &mut SketchCanvasState,
    context: Option<&SketchViewportContext<'_>>,
) -> SketchCanvasOutput {
    // Never force the surrounding workbench beyond its available rectangle;
    // compact-window layout owns the minimum size policy.
    let size = ui.available_size().max(Vec2::splat(1.0));
    let (response, painter) = ui.allocate_painter(size, Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Image, true, "Sketch viewport"));
    response.ctx.accesskit_node_builder(response.id, |node| {
        let plane_description = if context.is_some() {
            "face-aligned".to_owned()
        } else {
            state.plane.label().to_owned()
        };
        node.set_description(format!(
            "Orthographic {} sketch canvas. {} tool active. Middle-drag or right-drag to pan and use the mouse wheel to zoom.",
            plane_description,
            state.tool.label()
        ));
    });
    let hover_cursor = if state.tool == SketchTool::Select && state.pending.is_none() {
        if let Some(handle) = state.active_drag_handle {
            handle.cursor()
        } else if let Some(hovered_id) = state.hovered
            && let Some(pos) = response.hover_pos()
            && let Some(geom) = state.entity_geometry(hovered_id)
        {
            hit_test_drag_handle(geom, state.view, response.rect, pos, 12.0).cursor()
        } else {
            state.tool.cursor()
        }
    } else {
        state.tool.cursor()
    };
    let response = response.on_hover_cursor(hover_cursor);
    // Focus traversal and Escape preprocessing can move focus before widgets
    // inspect this frame. `lost_focus` therefore represents ownership at the
    // start of the key event just as importantly as `has_focus` does.
    let canvas_owned_keyboard = response.has_focus() || response.lost_focus();

    painter.rect_filled(response.rect, 0.0, sketch_colours().background);
    // Snapping runs below against state, so the frame's support curves are
    // mirrored first. A plane sketch has none and falls back to sketch-only
    // snapping.
    state.set_support_curves(context.map_or(&[], |context| context.snap_curves));
    let context_fit_changed =
        context.is_some_and(|context| auto_fit_context_if_needed(state, response.rect, context));
    let navigation_changed =
        handle_navigation(ui, &response, &mut state.view) || context_fit_changed;
    state.refresh_analytic_regions();
    let pattern_interaction = handle_pattern_manipulator_input(ui, &response, state);
    // Before anything reads the primary button: a label being dragged owns it,
    // so a grab never doubles as a pick of whatever lies under the chip.
    let dimension_interaction = handle_dimension_drag_input(ui, &response, state);

    let prior_dimension_layouts = dimension_widget_layouts(state, response.rect);
    let pointer_over_dimension = response.hover_pos().is_some_and(|position| {
        prior_dimension_layouts
            .iter()
            .any(|layout| layout.readout.editable && layout.rect.contains(position))
    });
    if matches!(
        state.exact_tool,
        ToolVariant::Select | ToolVariant::Dimension
    ) || state.pending.is_some()
    {
        state.pointer_preview = None;
    } else if let Some(position) = response.hover_pos().filter(|_| !pointer_over_dimension) {
        let snap = state.snap_point(response.rect, position);
        state.pointer_preview = Some(snap);
        state.update_dimension_pointer(snap.point);
    }
    let entity_pick_radius = if state.exact_tool == ToolVariant::Dimension {
        14.0
    } else {
        8.0
    };
    state.hovered = response.hover_pos().and_then(|position| {
        hit_test_entities(
            &state.entities,
            state.view,
            response.rect,
            position,
            entity_pick_radius,
        )
    });
    let trim_hover_point = (state.exact_tool == ToolVariant::Trim && !pointer_over_dimension)
        .then(|| {
            response
                .hover_pos()
                .map(|position| state.view.screen_to_sketch(response.rect, position))
        })
        .flatten();
    state.update_trim_hover(trim_hover_point, 8.0 / state.view.points_per_unit);
    let offset_hover_point = (state.exact_tool == ToolVariant::Offset && !pointer_over_dimension)
        .then(|| {
            response
                .hover_pos()
                .map(|position| state.view.screen_to_sketch(response.rect, position))
        })
        .flatten();
    state.update_offset_hover(offset_hover_point, 8.0 / state.view.points_per_unit);
    let region_hover_point = (state.exact_tool == ToolVariant::Select && !pointer_over_dimension)
        .then(|| {
            response
                .hover_pos()
                .map(|position| state.view.screen_to_sketch(response.rect, position))
        })
        .flatten();
    state.update_region_hover(region_hover_point);

    let mut selection_changed = false;
    let mut draft_changed = pattern_interaction.changed || dimension_interaction.changed;
    let mut pending_created = pattern_interaction.pending_created;
    let primary_finish_click = response.double_clicked_by(PointerButton::Primary)
        || response.triple_clicked_by(PointerButton::Primary);
    if (response.clicked_by(PointerButton::Primary) || primary_finish_click)
        && !pattern_interaction.consumes_primary
        && !dimension_interaction.consumes_primary
        && !pointer_over_dimension
        && state.dimension_error().is_none()
        && let Some(position) = response.interact_pointer_pos()
    {
        response.request_focus();
        // A dimension between two things is decided before the click is
        // routed, because the same tool still edits one curve's own
        // dimensions when the click lands on a curve rather than a point.
        let raw_pick = state.view.screen_to_sketch(response.rect, position);
        let dimension_pick_radius = 8.0 / state.view.points_per_unit;
        let dimensioning = state.dimension_takes_the_click(raw_pick, dimension_pick_radius);
        if matches!(
            state.exact_tool,
            ToolVariant::Trim
                | ToolVariant::Fillet
                | ToolVariant::Chamfer
                | ToolVariant::TwoDistanceChamfer
                | ToolVariant::Offset
                | ToolVariant::RectangularPattern
                | ToolVariant::CircularPattern
        ) || relation_arity(state.exact_tool).is_some()
            || dimensioning
        {
            // Span and corner modifiers retain the actual model-space pick.
            // Snapping to a junction would erase the finite carrier branch the
            // user intended to keep. Pattern centres remain snap-aware. A
            // relation picks what is under the pointer, endpoint first, so it
            // takes the raw point too: a click with a relation tool used to
            // fall through to plain selection here, which is why relations
            // never staged from the canvas.
            let raw_point = state.view.screen_to_sketch(response.rect, position);
            // Offset takes the raw point too, and for a reason of its own:
            // which side of the chain the click fell on is the whole of the
            // direction, and a snap onto the curve has no side.
            let point = if matches!(
                state.exact_tool,
                ToolVariant::Trim
                    | ToolVariant::Fillet
                    | ToolVariant::Chamfer
                    | ToolVariant::TwoDistanceChamfer
                    | ToolVariant::Offset
            ) || relation_arity(state.exact_tool).is_some()
                || dimensioning
            {
                raw_point
            } else {
                state.snap_point(response.rect, position).point
            };
            let pick_radius = 8.0 / state.view.points_per_unit;
            let additive_pattern_pick = matches!(
                state.exact_tool,
                ToolVariant::RectangularPattern | ToolVariant::CircularPattern
            ) && ui.input(|input| input.modifiers.shift);
            if additive_pattern_pick {
                state.toggle_pattern_source(point, pick_radius);
            } else if dimensioning {
                pending_created = state.append_dimension_operand(point, pick_radius);
            } else {
                pending_created = state.handle_modifier_click(point, pick_radius);
            }
            draft_changed = true;
        } else {
            match state.tool {
                SketchTool::Select => {
                    let sketch_pt = state.view.screen_to_sketch(response.rect, position);
                    let additive = ui.input(|input| input.modifiers.shift);
                    let selected = hit_test_entities(
                        &state.entities,
                        state.view,
                        response.rect,
                        position,
                        entity_pick_radius,
                    );
                    if let Some(selected) = selected {
                        selection_changed = state.set_selected(Some(selected));
                        if !additive {
                            selection_changed |= state.clear_selected_regions();
                        }
                        // Picking is the whole Dimension gesture: the first
                        // driving box takes the caret so "click the curve, type
                        // 3, Enter" never leaves the canvas. This deliberately
                        // does not read `selection_changed`, so re-picking a
                        // curve that is already selected re-arms it. A staged
                        // candidate owns the boxes, so it is left alone.
                        if state.exact_tool == ToolVariant::Dimension && state.pending.is_none() {
                            state.dimension_pick = Some(sketch_pt);
                            state.focus_dimension_box = first_armed_dimension_kind(state);
                        }
                    } else {
                        selection_changed = state.set_selected(None);
                        selection_changed |= state.select_region_at_point(sketch_pt, additive);
                    }
                }
                SketchTool::Point
                | SketchTool::Line
                | SketchTool::CentreLine
                | SketchTool::Rectangle
                | SketchTool::Circle
                | SketchTool::Arc => {
                    let point = state.snap_point(response.rect, position).point;
                    pending_created = if state.exact_tool == ToolVariant::ChainedPolyline
                        && primary_finish_click
                    {
                        state.finish_polyline_at_pointer(point)
                    } else {
                        state.handle_creation_click(point)
                    };
                    draft_changed = true;
                }
            }
        }
    }

    if response.drag_started_by(PointerButton::Primary)
        && !pattern_interaction.consumes_primary
        && !dimension_interaction.consumes_primary
        && !pointer_over_dimension
        && state.tool == SketchTool::Select
        && state.pending.is_none()
        && let Some(pos) = response.interact_pointer_pos()
    {
        if state.selected.is_none() && state.hovered.is_some() {
            state.set_selected(state.hovered);
            selection_changed = true;
        }
        if let Some(selected_id) = state.selected
            && let Some(geom) = state.entity_geometry(selected_id)
        {
            state.active_drag_handle = Some(hit_test_drag_handle(
                geom,
                state.view,
                response.rect,
                pos,
                12.0,
            ));
        }
    }

    let primary_drag_delta = ui.input(|input| input.pointer.delta());
    if response.dragged_by(PointerButton::Primary)
        && !pattern_interaction.consumes_primary
        && !dimension_interaction.consumes_primary
        && !pointer_over_dimension
        && state.tool == SketchTool::Select
        && state.pending.is_none()
        && primary_drag_delta != Vec2::ZERO
    {
        if state.selected.is_none() && state.hovered.is_some() {
            state.set_selected(state.hovered);
            selection_changed = true;
        }
        let handle = state.active_drag_handle.unwrap_or_else(|| {
            if let Some(pos) = response.interact_pointer_pos()
                && let Some(selected_id) = state.selected
                && let Some(geom) = state.entity_geometry(selected_id)
            {
                let h = hit_test_drag_handle(geom, state.view, response.rect, pos, 12.0);
                state.active_drag_handle = Some(h);
                h
            } else {
                SketchDragHandle::Translate
            }
        });
        let horizontal = f64::from(primary_drag_delta.x) / state.view.points_per_unit;
        let vertical = -f64::from(primary_drag_delta.y) / state.view.points_per_unit;
        let (delta_u, delta_v) = state.view.unrotate_offset(horizontal, vertical);
        if state.reshape_selected(handle, delta_u, delta_v) {
            draft_changed = true;
            ui.ctx().request_repaint();
        }
    }

    if response.drag_stopped() {
        state.active_drag_handle = None;
    }

    if let Some(context) = context {
        paint_viewport_context(&painter, response.rect, state.view, context);
    }
    let axis_labels = context
        .and_then(|context| context.axis_labels)
        .unwrap_or_else(|| state.plane.axis_labels());
    paint_grid(&painter, response.rect, state.view, axis_labels, state.snap);
    paint_profile_fill(&painter, response.rect, state);
    // A typed dimension replaces its subject in place. Painting the retired
    // original underneath it is what made one edit look like two rectangles;
    // `refresh_profile_analysis` has always excluded them the same way.
    let superseded = state.superseded_by_in_place_edit();
    paint_entities(
        &painter,
        response.rect,
        state.view,
        state
            .entities
            .iter()
            .copied()
            .filter(|entity| !superseded.contains(&entity.id)),
        state.hovered_for_paint(),
        state.selected,
    );
    paint_modifier_sources(&painter, response.rect, state);
    paint_constraint_dimensions(&painter, response.rect, state);
    paint_relation_highlight(&painter, response.rect, state);
    let semantic_selected = semantic_selection_targets(ui, response.rect, state);
    if let Some(selected) = semantic_selected {
        selection_changed |= state.set_selected(Some(selected));
        if !ui.input(|input| input.modifiers.shift) {
            selection_changed |= state.clear_selected_regions();
        }
        // The semantic chip sits over the canvas and takes the click outright,
        // so a pick that lands on it never reaches the branch above. Arming
        // here keeps both routes equivalent, and it still runs before the
        // boxes lay out below. The chip carries no side identity, so the
        // armed dimension falls back to the field order.
        if state.exact_tool == ToolVariant::Dimension && state.pending.is_none() {
            state.dimension_pick = None;
            state.focus_dimension_box = first_armed_dimension_kind(state);
        }
    }
    paint_pending(&painter, response.rect, state);
    paint_trim_hover(&painter, response.rect, state);
    paint_offset_hover(&painter, response.rect, state);
    paint_creation_preview(&painter, response.rect, state);
    paint_overlay(&painter, response.rect, state, context.is_some());
    let committed_annotations = committed_dimension_annotation_layouts(state, response.rect);
    paint_committed_dimension_annotations(&painter, &committed_annotations);
    let dimension_layouts = dimension_widget_layouts(state, response.rect);
    paint_dimension_leaders(&painter, &dimension_layouts);
    let dimensions = show_dimension_widgets(ui, state, &dimension_layouts, canvas_owned_keyboard);
    pending_created = pending_created.or(dimensions.pending_created);

    SketchCanvasOutput {
        response,
        hovered: state.hovered,
        selection_changed,
        navigation_changed,
        draft_changed,
        pending_created,
        dimension_keys: dimensions.claims,
        recipe_dimension_field: dimensions.recipe_field,
        recipe_dimension_accepted: dimensions.recipe_accepted,
    }
}

fn auto_fit_context_if_needed(
    state: &mut SketchCanvasState,
    rect: Rect,
    context: &SketchViewportContext<'_>,
) -> bool {
    let Some(key) = context.auto_fit_key else {
        return false;
    };
    if state.last_context_fit_key == Some(key) {
        return false;
    }
    // Claim the key even when its input is invalid so malformed presentation
    // data does not trigger the same fit scan on every frame.
    state.last_context_fit_key = Some(key);
    fit_context_view(&mut state.view, rect, context)
}

fn fit_context_view(
    view: &mut SketchView,
    rect: Rect,
    context: &SketchViewportContext<'_>,
) -> bool {
    let Some(body_bounds) = sketch_point_bounds(context_points(context)) else {
        return false;
    };
    let focus =
        sketch_point_bounds(context.selected_face_boundary.iter().copied()).unwrap_or(body_bounds);
    let center = SketchPoint::new((focus[0] + focus[1]) * 0.5, (focus[2] + focus[3]) * 0.5);
    let half_u = (center.u - body_bounds[0])
        .abs()
        .max((body_bounds[1] - center.u).abs())
        .max(MIN_ENTITY_LENGTH);
    let half_v = (center.v - body_bounds[2])
        .abs()
        .max((body_bounds[3] - center.v).abs())
        .max(MIN_ENTITY_LENGTH);
    let (screen_half_width, screen_half_height) = if view.quarter_turns.is_multiple_of(2) {
        (half_u, half_v)
    } else {
        (half_v, half_u)
    };
    let available_half_width =
        (f64::from(rect.width()) * 0.5 - f64::from(CONTEXT_FIT_PADDING_POINTS)).max(1.0);
    let available_half_height =
        (f64::from(rect.height()) * 0.5 - f64::from(CONTEXT_FIT_PADDING_POINTS)).max(1.0);
    let points_per_unit = (available_half_width / screen_half_width)
        .min(available_half_height / screen_half_height)
        .clamp(MIN_POINTS_PER_UNIT, MAX_POINTS_PER_UNIT);
    if !center.is_finite() || !points_per_unit.is_finite() {
        return false;
    }
    let changed = view.center != center || view.points_per_unit != points_per_unit;
    view.center = center;
    view.points_per_unit = points_per_unit;
    changed
}

pub fn fitted_context_view_with_quarter_turn(
    size: Vec2,
    context: &SketchViewportContext<'_>,
    quarter_turns: u8,
) -> Option<SketchView> {
    if !size.is_finite() || size.x <= 1.0 || size.y <= 1.0 {
        return None;
    }
    // Validate the projected context independently from whether fitting would
    // happen to change the default view.  A perfectly centred, unit-scale
    // context is still a valid prepared hand-off and must consume the first
    // sketch frame's auto-fit key; otherwise that first frame can visibly
    // jump after an otherwise continuous camera transition.
    sketch_point_bounds(context_points(context))?;
    let mut view = SketchView::default();
    view.set_quarter_turns(quarter_turns);
    let rect = Rect::from_min_size(Pos2::ZERO, size);
    let _ = fit_context_view(&mut view, rect, context);
    Some(view)
}

fn context_points<'a>(
    context: &'a SketchViewportContext<'a>,
) -> impl Iterator<Item = SketchPoint> + 'a {
    context
        .triangles
        .iter()
        .flat_map(|triangle| triangle.vertices)
        .chain(context.edges.iter().flat_map(|edge| edge.endpoints))
        .chain(context.selected_face_boundary.iter().copied())
}

fn sketch_point_bounds(points: impl IntoIterator<Item = SketchPoint>) -> Option<[f64; 4]> {
    let mut bounds = [
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ];
    let mut found = false;
    for point in points {
        if !point.is_finite() {
            continue;
        }
        found = true;
        bounds[0] = bounds[0].min(point.u);
        bounds[1] = bounds[1].max(point.u);
        bounds[2] = bounds[2].min(point.v);
        bounds[3] = bounds[3].max(point.v);
    }
    found.then_some(bounds)
}

fn paint_viewport_context(
    painter: &egui::Painter,
    rect: Rect,
    view: SketchView,
    context: &SketchViewportContext<'_>,
) {
    // The x-ray of what lies below the surface goes down first, so the body
    // that is really there paints over it wherever the two overlap.
    let mesh = projected_context_mesh(rect, view, context.triangles);
    if !mesh.indices.is_empty() {
        painter.add(egui::Shape::mesh(mesh));
    }

    let body_stroke = Stroke::new(1.0, sketch_colours().context_edge);
    let below_stroke = Stroke::new(1.2, sketch_colours().context_below_edge);
    for edge in context.edges {
        let [first, second] = edge.endpoints;
        if !first.is_finite() || !second.is_finite() {
            continue;
        }
        let points = [
            view.sketch_to_screen(rect, first),
            view.sketch_to_screen(rect, second),
        ];
        if !screen_points_are_finite(&points) {
            continue;
        }
        match edge.layer {
            SketchContextLayer::Body => {
                painter.line_segment(points, body_stroke);
            }
            SketchContextLayer::Below => {
                // Hidden-line convention: what is under the surface dashes.
                painter.add(egui::Shape::dashed_line(&points, below_stroke, 6.0, 4.0));
            }
        }
    }

    let boundary = context.selected_face_boundary;
    if boundary.len() >= 2 {
        let boundary_stroke = Stroke::new(1.8, sketch_colours().context_selected_boundary);
        for index in 0..boundary.len() {
            let first = boundary[index];
            let second = boundary[(index + 1) % boundary.len()];
            if !first.is_finite() || !second.is_finite() {
                continue;
            }
            let points = [
                view.sketch_to_screen(rect, first),
                view.sketch_to_screen(rect, second),
            ];
            if screen_points_are_finite(&points) {
                painter.line_segment(points, boundary_stroke);
            }
        }
    }
    let boundary_stroke = Stroke::new(1.8, sketch_colours().context_selected_boundary);
    for inner in context.selected_face_inner_boundaries {
        if inner.len() < 2 {
            continue;
        }
        for index in 0..inner.len() {
            let first = inner[index];
            let second = inner[(index + 1) % inner.len()];
            if !first.is_finite() || !second.is_finite() {
                continue;
            }
            let points = [
                view.sketch_to_screen(rect, first),
                view.sketch_to_screen(rect, second),
            ];
            if screen_points_are_finite(&points) {
                painter.line_segment(points, boundary_stroke);
            }
        }
    }
}

fn projected_context_mesh(
    rect: Rect,
    view: SketchView,
    triangles: &[SketchContextTriangle],
) -> egui::Mesh {
    let colours = sketch_colours();
    let mut mesh = egui::Mesh::default();
    mesh.reserve_vertices(triangles.len() * 3);
    mesh.reserve_triangles(triangles.len());
    // Below-surface faces first, then the body, so painter order inside each
    // layer (far to near) is preserved and the body always wins overlaps.
    let layered = triangles
        .iter()
        .filter(|triangle| triangle.layer == SketchContextLayer::Below)
        .chain(
            triangles
                .iter()
                .filter(|triangle| triangle.layer == SketchContextLayer::Body),
        );
    for triangle in layered {
        if triangle.vertices.iter().any(|point| !point.is_finite()) {
            continue;
        }
        let points = triangle
            .vertices
            .map(|point| view.sketch_to_screen(rect, point));
        if !screen_points_are_finite(&points) {
            continue;
        }
        let base = match triangle.layer {
            SketchContextLayer::Body => colours.context_face,
            SketchContextLayer::Below => colours.context_below_face,
        };
        let colour = shaded_context_colour(base, triangle.shade, colours.background);
        let first = mesh.vertices.len() as u32;
        for point in points {
            mesh.colored_vertex(point, colour);
        }
        mesh.add_triangle(first, first + 1, first + 2);
    }
    mesh
}

/// Darkens or lightens a context colour toward the canvas ground by the
/// triangle's shade, so a sloping or deep face reads as one even when it is
/// the same carrier as its neighbour. Alpha is kept: the x-ray stays an
/// x-ray.
fn shaded_context_colour(base: Color32, shade: f32, ground: Color32) -> Color32 {
    let shade = shade.clamp(0.0, 1.0);
    // Shade 1.0 is the palette colour; shade 0.0 is one third of the way to
    // the ground, which is visibly darker (or lighter, on a dark theme)
    // without dissolving the face into the background.
    let weight = 0.35 * (1.0 - shade);
    let mix = |from: u8, to: u8| -> u8 {
        let value = f32::from(from) + (f32::from(to) - f32::from(from)) * weight;
        value.round().clamp(0.0, 255.0) as u8
    };
    Color32::from_rgba_unmultiplied(
        mix(base.r(), ground.r()),
        mix(base.g(), ground.g()),
        mix(base.b(), ground.b()),
        base.a(),
    )
}

fn screen_points_are_finite(points: &[Pos2]) -> bool {
    points
        .iter()
        .all(|point| point.x.is_finite() && point.y.is_finite())
}

fn handle_navigation(ui: &Ui, response: &Response, view: &mut SketchView) -> bool {
    let mut changed = false;
    if response.dragged_by(PointerButton::Middle) || response.dragged_by(PointerButton::Secondary) {
        let delta = ui.input(|input| input.pointer.delta());
        if delta != Vec2::ZERO {
            view.pan_by_screen_delta(delta);
            changed = true;
        }
    }
    if response.hovered() {
        let scroll = ui.input(|input| input.smooth_scroll_delta.y);
        if scroll.abs() > f32::EPSILON {
            let position = response.hover_pos().unwrap_or(response.rect.center());
            view.zoom_about(response.rect, position, (f64::from(scroll) * 0.0025).exp());
            changed = true;
        }
    }
    if changed {
        ui.ctx().request_repaint();
    }
    changed
}

fn paint_grid(
    painter: &egui::Painter,
    rect: Rect,
    view: SketchView,
    axis_labels: [&str; 2],
    snap: SnapSettings,
) {
    if let Some(spacing) = visible_grid_spacing(view.points_per_unit, snap.grid_step) {
        paint_grid_family(
            painter,
            rect,
            view,
            spacing.minor_world_step(),
            sketch_colours().grid_minor,
            1.0,
        );
        paint_grid_family(
            painter,
            rect,
            view,
            spacing.major_world_step(),
            sketch_colours().grid_major,
            1.15,
        );
    }

    let [min_u, max_u, min_v, max_v] = sketch_view_bounds(view, rect);
    painter.line_segment(
        [
            view.sketch_to_screen(rect, SketchPoint::new(min_u, 0.0)),
            view.sketch_to_screen(rect, SketchPoint::new(max_u, 0.0)),
        ],
        Stroke::new(1.35, sketch_colours().axis_first),
    );
    painter.line_segment(
        [
            view.sketch_to_screen(rect, SketchPoint::new(0.0, min_v)),
            view.sketch_to_screen(rect, SketchPoint::new(0.0, max_v)),
        ],
        Stroke::new(1.35, sketch_colours().axis_second),
    );

    let origin = view.sketch_to_screen(rect, SketchPoint::default());
    let u_direction = view.sketch_to_screen(rect, SketchPoint::new(1.0, 0.0)) - origin;
    let v_direction = view.sketch_to_screen(rect, SketchPoint::new(0.0, 1.0)) - origin;

    painter.text(
        axis_label_position(rect, u_direction, true),
        Align2::CENTER_CENTER,
        axis_labels[0],
        FontId::monospace(10.0),
        sketch_colours().axis_first,
    );
    painter.text(
        axis_label_position(rect, v_direction, false),
        Align2::CENTER_CENTER,
        axis_labels[1],
        FontId::monospace(10.0),
        sketch_colours().axis_second,
    );
}

fn paint_grid_family(
    painter: &egui::Painter,
    rect: Rect,
    view: SketchView,
    step: f64,
    color: Color32,
    width: f32,
) {
    if !step.is_finite() || step <= 0.0 {
        return;
    }
    let [min_u, max_u, min_v, max_v] = sketch_view_bounds(view, rect);
    let first_u = (min_u / step).ceil() as i64;
    let last_u = (max_u / step).floor() as i64;
    for index in bounded_grid_indices(first_u, last_u) {
        let u = index as f64 * step;
        painter.line_segment(
            [
                view.sketch_to_screen(rect, SketchPoint::new(u, min_v - step)),
                view.sketch_to_screen(rect, SketchPoint::new(u, max_v + step)),
            ],
            Stroke::new(width, color),
        );
    }
    let first_v = (min_v / step).ceil() as i64;
    let last_v = (max_v / step).floor() as i64;
    for index in bounded_grid_indices(first_v, last_v) {
        let v = index as f64 * step;
        painter.line_segment(
            [
                view.sketch_to_screen(rect, SketchPoint::new(min_u - step, v)),
                view.sketch_to_screen(rect, SketchPoint::new(max_u + step, v)),
            ],
            Stroke::new(width, color),
        );
    }
}

fn sketch_view_bounds(view: SketchView, rect: Rect) -> [f64; 4] {
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.left_bottom(),
        rect.right_bottom(),
    ]
    .map(|corner| view.screen_to_sketch(rect, corner));
    corners.iter().fold(
        [
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ],
        |mut bounds, point| {
            bounds[0] = bounds[0].min(point.u);
            bounds[1] = bounds[1].max(point.u);
            bounds[2] = bounds[2].min(point.v);
            bounds[3] = bounds[3].max(point.v);
            bounds
        },
    )
}

fn axis_label_position(rect: Rect, direction: Vec2, primary: bool) -> Pos2 {
    if direction.x.abs() >= direction.y.abs() {
        Pos2::new(
            if direction.x.is_sign_negative() {
                rect.left() + 14.0
            } else {
                rect.right() - 14.0
            },
            if primary {
                rect.bottom() - 12.0
            } else {
                rect.top() + 14.0
            },
        )
    } else {
        Pos2::new(
            if primary {
                rect.right() - 16.0
            } else {
                rect.left() + 12.0
            },
            if direction.y.is_sign_negative() {
                rect.top() + 14.0
            } else {
                rect.bottom() - 12.0
            },
        )
    }
}

fn bounded_grid_indices(first: i64, last: i64) -> impl Iterator<Item = i64> {
    let last = last.min(first.saturating_add(MAX_GRID_LINES_PER_AXIS as i64));
    first..=last
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct VisibleGridSpacing {
    lattice_step: f64,
    minor_multiple: u64,
    major_multiple: u64,
}

impl VisibleGridSpacing {
    fn new(lattice_step: f64, minor_multiple: u64) -> Option<Self> {
        let major_multiple = minor_multiple.checked_mul(MAJOR_GRID_INTERVAL)?;
        let result = Self {
            lattice_step,
            minor_multiple,
            major_multiple,
        };
        (result.minor_world_step().is_finite() && result.major_world_step().is_finite())
            .then_some(result)
    }

    fn lattice_step(self) -> f64 {
        self.lattice_step
    }

    fn minor_world_step(self) -> f64 {
        self.lattice_step() * self.minor_multiple as f64
    }

    fn major_world_step(self) -> f64 {
        self.lattice_step() * self.major_multiple as f64
    }
}

fn visible_grid_spacing(points_per_unit: f64, lattice_step: f64) -> Option<VisibleGridSpacing> {
    resolvable_grid_spacing(points_per_unit, lattice_step, TARGET_GRID_SPACING_POINTS)
}

/// The coarsest 1/2/5 multiple of the lattice that still sits at least
/// `target_points` apart on screen, or the lattice itself when it already does.
fn resolvable_grid_spacing(
    points_per_unit: f64,
    lattice_step: f64,
    target_points: f64,
) -> Option<VisibleGridSpacing> {
    if !points_per_unit.is_finite()
        || points_per_unit <= 0.0
        || !lattice_step.is_finite()
        || lattice_step <= 0.0
    {
        return None;
    }
    let lattice_spacing_points = lattice_step * points_per_unit;
    if !lattice_spacing_points.is_finite() || lattice_spacing_points <= 0.0 {
        return None;
    }
    let required_multiple = (target_points / lattice_spacing_points).ceil().max(1.0);
    let minor_multiple = readable_integer_multiple(required_multiple)?;
    VisibleGridSpacing::new(lattice_step, minor_multiple)
}

fn readable_integer_multiple(required: f64) -> Option<u64> {
    if !required.is_finite() || required <= 0.0 || required > u64::MAX as f64 {
        return None;
    }
    let required = required as u64;
    let mut magnitude = 1_u64;
    while magnitude <= required / 10 {
        magnitude = magnitude.checked_mul(10)?;
    }
    [1_u64, 2, 5, 10]
        .into_iter()
        .filter_map(|factor| magnitude.checked_mul(factor))
        .find(|candidate| *candidate >= required)
}

fn paint_profile_fill(painter: &egui::Painter, rect: Rect, state: &SketchCanvasState) {
    // A retirement-only transaction previews removed edges in red. Keeping
    // the old selected-cell fill visible would incorrectly imply that the
    // material region survives confirmation, even though the exact candidate
    // graph contains no such region.
    if !state.selected_region_fill_visible() {
        return;
    }
    let Some(arrangement) = state.analytic_regions.arrangement.as_ref() else {
        return;
    };
    // Every bounded cell wears a faint standing tint: that the sketch has
    // found a closed profile is visible before anything is hovered or picked,
    // and each separately selectable region reads as its own patch. A pending
    // edit shows the live arrangement, so the standing tint stays out of it.
    if state.pending.is_none() {
        let standing_fill = translucent(sketch_colours().region_fill, 12);
        for cell in &arrangement.cells {
            if state.analytic_regions.selected.contains(&cell.signature)
                || state.analytic_regions.hovered.as_ref() == Some(&cell.signature)
            {
                continue;
            }
            paint_analytic_cell_fill(painter, rect, state.view, cell, standing_fill);
        }
    }
    let selected_fill = if state.pending.is_some() {
        translucent(sketch_colours().pending, 34)
    } else {
        translucent(sketch_colours().region_fill, 44)
    };
    for signature in &state.analytic_regions.selected {
        if let Some(cell) = arrangement.cell(signature) {
            paint_analytic_cell_fill(painter, rect, state.view, cell, selected_fill);
        }
    }
    if let Some(hovered) = state.analytic_regions.hovered.as_ref()
        && !state.analytic_regions.selected.contains(hovered)
        && let Some(cell) = arrangement.cell(hovered)
    {
        paint_analytic_cell_fill(
            painter,
            rect,
            state.view,
            cell,
            translucent(sketch_colours().region_hover, 42),
        );
    }
}

fn paint_analytic_cell_fill(
    painter: &egui::Painter,
    rect: Rect,
    view: SketchView,
    cell: &CoreArrangementCell,
    color: Color32,
) {
    let mut contours = Vec::with_capacity(cell.holes.len().saturating_add(1));
    contours.push(sample_arrangement_loop(&cell.outer, view, rect));
    contours.extend(
        cell.holes
            .iter()
            .map(|hole| sample_arrangement_loop(hole, view, rect)),
    );
    let mesh = even_odd_scanline_mesh(&contours, rect, color);
    if !mesh.indices.is_empty() {
        painter.add(egui::Shape::mesh(mesh));
    }
}

fn sample_arrangement_loop(
    profile_loop: &CoreArrangementLoop,
    view: SketchView,
    rect: Rect,
) -> Vec<Pos2> {
    let mut points = Vec::new();
    for curve in &profile_loop.curves {
        let subdivisions = if matches!(curve, CoreEvaluatedCurve2::Line { .. }) {
            1
        } else {
            ((curve.arc_length() * view.points_per_unit / 8.0).ceil() as usize).clamp(8, 256)
        };
        for step in 0..subdivisions {
            let parameter = step as f64 / subdivisions as f64;
            if let Ok(point) = curve.evaluate(parameter) {
                let screen = view.sketch_to_screen(rect, SketchPoint::new(point.u, point.v));
                if points
                    .last()
                    .is_none_or(|last: &Pos2| last.distance(screen) > 1.0e-4)
                {
                    points.push(screen);
                }
            }
        }
    }
    points
}

/// Presentation-only even/odd scan conversion. Every contour, including
/// holes, participates in parity, so a failed or odd crossing row is omitted
/// rather than displaying material in an analytic void.
fn even_odd_scanline_mesh(contours: &[Vec<Pos2>], clip: Rect, color: Color32) -> egui::Mesh {
    const STRIP_HEIGHT: f32 = 2.0;
    let mut mesh = egui::Mesh::default();
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for point in contours.iter().flatten() {
        min_y = min_y.min(point.y);
        max_y = max_y.max(point.y);
    }
    if !min_y.is_finite() || !max_y.is_finite() || max_y <= min_y {
        return mesh;
    }
    let mut top = (min_y / STRIP_HEIGHT).floor() * STRIP_HEIGHT;
    top = top.max(clip.top());
    let bottom_limit = max_y.min(clip.bottom());
    while top < bottom_limit {
        let bottom = (top + STRIP_HEIGHT).min(bottom_limit);
        let sample_y = (top + bottom) * 0.5;
        let mut intersections = Vec::new();
        for contour in contours.iter().filter(|contour| contour.len() >= 3) {
            for (first, second) in contour
                .iter()
                .copied()
                .zip(contour.iter().copied().cycle().skip(1))
                .take(contour.len())
            {
                if (first.y <= sample_y && second.y > sample_y)
                    || (second.y <= sample_y && first.y > sample_y)
                {
                    let fraction = (sample_y - first.y) / (second.y - first.y);
                    intersections.push((second.x - first.x).mul_add(fraction, first.x));
                }
            }
        }
        intersections.sort_by(f32::total_cmp);
        if intersections.len() % 2 == 0 {
            for pair in intersections.chunks_exact(2) {
                let left = pair[0].max(clip.left());
                let right = pair[1].min(clip.right());
                if right <= left {
                    continue;
                }
                let first = mesh.vertices.len() as u32;
                for point in [
                    Pos2::new(left, top),
                    Pos2::new(right, top),
                    Pos2::new(right, bottom),
                    Pos2::new(left, bottom),
                ] {
                    mesh.colored_vertex(point, color);
                }
                mesh.add_triangle(first, first + 1, first + 2);
                mesh.add_triangle(first, first + 2, first + 3);
            }
        }
        top = bottom;
    }
    mesh
}

/// Returns only regions whose fill can be represented without lying about a
/// void. Hole-aware display triangulation is intentionally separate from the
/// exact modeling payload; until it is available, the boundary remains visible
/// and the ambiguous fill is omitted completely.
#[cfg(test)]
fn fillable_linear_profile_polygons(profile: &CertifiedSketchProfile) -> Vec<Vec<SketchPoint>> {
    profile.linear_regions().map_or_else(Vec::new, |regions| {
        regions
            .into_iter()
            .filter(|region| region.holes.is_empty())
            .map(|region| region.outer)
            .collect()
    })
}

#[cfg(test)]
fn triangulate_simple_polygon(points: &[Pos2]) -> Vec<[Pos2; 3]> {
    let mut triangles = Vec::new();
    if points.len() < 3 {
        return triangles;
    }
    let signed_area = points
        .iter()
        .copied()
        .zip(points.iter().copied().cycle().skip(1))
        .take(points.len())
        .map(|(first, second)| first.x * second.y - second.x * first.y)
        .sum::<f32>();
    if signed_area == 0.0 || !signed_area.is_finite() {
        return triangles;
    }
    let winding = signed_area.signum();
    let mut remaining = (0..points.len()).collect::<Vec<_>>();
    let mut guard = points.len() * points.len();
    while remaining.len() > 2 && guard > 0 {
        guard -= 1;
        let mut clipped = false;
        for current in 0..remaining.len() {
            let previous = remaining[(current + remaining.len() - 1) % remaining.len()];
            let vertex = remaining[current];
            let next = remaining[(current + 1) % remaining.len()];
            if triangle_cross(points[previous], points[vertex], points[next]) * winding <= 0.0 {
                continue;
            }
            if remaining.iter().copied().any(|candidate| {
                candidate != previous
                    && candidate != vertex
                    && candidate != next
                    && point_in_triangle(
                        points[candidate],
                        points[previous],
                        points[vertex],
                        points[next],
                        winding,
                    )
            }) {
                continue;
            }
            triangles.push([points[previous], points[vertex], points[next]]);
            remaining.remove(current);
            clipped = true;
            break;
        }
        if !clipped {
            break;
        }
    }
    triangles
}

#[cfg(test)]
fn triangle_cross(first: Pos2, second: Pos2, third: Pos2) -> f32 {
    (second.x - first.x) * (third.y - first.y) - (second.y - first.y) * (third.x - first.x)
}

#[cfg(test)]
fn point_in_triangle(point: Pos2, first: Pos2, second: Pos2, third: Pos2, winding: f32) -> bool {
    triangle_cross(first, second, point) * winding >= 0.0
        && triangle_cross(second, third, point) * winding >= 0.0
        && triangle_cross(third, first, point) * winding >= 0.0
}

fn paint_entities(
    painter: &egui::Painter,
    rect: Rect,
    view: SketchView,
    entities: impl IntoIterator<Item = SketchEntity>,
    hovered: Option<SketchEntityId>,
    selected: Option<SketchEntityId>,
) {
    let entities_vec = entities.into_iter().collect::<Vec<_>>();
    for entity in &entities_vec {
        let (color, width) = if Some(entity.id) == selected {
            (sketch_colours().selected, 2.5)
        } else if Some(entity.id) == hovered {
            (sketch_colours().hovered, 2.2)
        } else if entity.role == SketchEntityRole::Construction {
            (sketch_colours().construction, 1.45)
        } else if matches!(entity.geometry, SketchGeometry::Point(_)) {
            (sketch_colours().entity.gamma_multiply(0.4), 1.7)
        } else {
            (sketch_colours().entity, 1.7)
        };
        let stroke = Stroke::new(width, color);
        if entity.role == SketchEntityRole::Construction {
            paint_dashed_geometry(painter, rect, view, entity.geometry, stroke);
        } else {
            paint_geometry(painter, rect, view, entity.geometry, stroke);
        }
        if Some(entity.id) == selected || Some(entity.id) == hovered {
            for pt in entity.geometry.control_points().iter() {
                let pt_screen = view.sketch_to_screen(rect, pt);
                painter.circle_filled(pt_screen, 3.5, color);
                painter.circle_stroke(
                    pt_screen,
                    4.5,
                    Stroke::new(1.0, sketch_colours().background),
                );
            }
        }
    }
}

fn paint_modifier_sources(painter: &egui::Painter, rect: Rect, state: &SketchCanvasState) {
    if state.modifier_sources.is_empty()
        || !matches!(
            state.exact_tool,
            ToolVariant::Fillet
                | ToolVariant::Chamfer
                | ToolVariant::TwoDistanceChamfer
                | ToolVariant::RectangularPattern
                | ToolVariant::CircularPattern
        )
    {
        return;
    }
    let definition = state
        .pending
        .as_ref()
        .and_then(|pending| pending.core_transaction.as_ref())
        .map_or(&state.authoring, CoreTransaction::preview);
    for source in &state.modifier_sources {
        if let Ok(curve) = definition.evaluated_curve(*source) {
            paint_geometry(
                painter,
                rect,
                state.view,
                legacy_geometry_from_core(curve),
                Stroke::new(3.1, sketch_colours().selected),
            );
        }
    }
}

/// Draws every dimension the sketch holds, on the geometry it holds.
///
/// A leader along the distance itself and the value at its middle: the same
/// chip a recipe's own dimensions use, so a driving number reads the same
/// whether it came from a curve's parameters or from a relation between two
/// things.
fn paint_constraint_dimensions(painter: &egui::Painter, rect: Rect, state: &SketchCanvasState) {
    let colours = sketch_colours();
    let stroke = Stroke::new(1.0, colours.dimension.gamma_multiply(0.45));
    // Straight from the records rather than through the panel's summaries:
    // this runs every frame, and the summaries build a sentence per relation.
    for record in state.authoring.constraints().values() {
        if !record.enabled {
            continue;
        }
        let (Some((from, to)), Some(value)) =
            (state.constraint_leader(&record.kind), record.kind.value())
        else {
            continue;
        };
        let value = value.abs();
        let start = state.view.sketch_to_screen(rect, from);
        let end = state.view.sketch_to_screen(rect, to);
        painter.line_segment([start, end], stroke);
        let text = dimension_chip_text(value);
        let measured = start + (end - start) * 0.5;
        let Some(chip) = state.dimension_chip_rect(painter, rect, record.id, &record.kind) else {
            continue;
        };
        // A value dragged clear of the geometry keeps a thread back to what it
        // measures, which is what stops a loose number meaning nothing.
        if chip.center().distance(measured) > 1.0 {
            painter.line_segment(
                [measured, chip.center()],
                Stroke::new(1.0, colours.dimension.gamma_multiply(0.3)),
            );
        }
        painter.rect(
            chip,
            4.0,
            colours.dimension_background.gamma_multiply(0.85),
            Stroke::new(1.0, colours.dimension.gamma_multiply(0.55)),
            egui::StrokeKind::Inside,
        );
        painter.text(
            chip.center(),
            Align2::CENTER_CENTER,
            text,
            FontId::monospace(DIMENSION_CHIP_TEXT_SIZE),
            colours.dimension.gamma_multiply(0.85),
        );
    }
}

/// Rings the points one listed relation holds.
///
/// A relation names points, and a list of words cannot say which ones. The
/// panel row sets this while the pointer is over it, so "Coincident" in the
/// list and the corner it holds are the same thing on screen.
fn paint_relation_highlight(painter: &egui::Painter, rect: Rect, state: &SketchCanvasState) {
    for point in &state.relation_highlight {
        let centre = state.view.sketch_to_screen(rect, *point);
        painter.circle_stroke(centre, 6.0, Stroke::new(1.6, sketch_colours().selected));
    }
}

/// The chain Offset would take, drawn heavier in the selection colour.
///
/// It is the answer to the only question the gesture leaves open before the
/// click: how far the connection runs.
fn paint_offset_hover(painter: &egui::Painter, rect: Rect, state: &SketchCanvasState) {
    for curve in &state.offset_hover {
        paint_geometry(
            painter,
            rect,
            state.view,
            legacy_geometry_from_core(curve.clone()),
            Stroke::new(3.0, sketch_colours().selected),
        );
    }
}

fn paint_trim_hover(painter: &egui::Painter, rect: Rect, state: &SketchCanvasState) {
    let Some(ref fragment) = state.trim_hover_fragment else {
        return;
    };
    paint_geometry(
        painter,
        rect,
        state.view,
        legacy_geometry_from_core(fragment.clone()),
        Stroke::new(3.4, sketch_colours().trim_hover),
    );
}

fn paint_pending(painter: &egui::Painter, rect: Rect, state: &SketchCanvasState) {
    let Some(pending) = state.pending.as_ref() else {
        return;
    };
    if pending.is_in_place() {
        // The value applies the moment it is accepted, so the candidate is
        // painted as the entity it will be — same stroke rules, same
        // selection highlight — and no red ghost is drawn at all.
        paint_entities(
            painter,
            rect,
            state.view,
            pending.entities().iter().copied(),
            state.hovered_for_paint(),
            state.selected,
        );
        return;
    }
    for retired in pending.retired_entities() {
        if let Some(source) = state.entities.iter().find(|entity| entity.id == *retired) {
            paint_geometry(
                painter,
                rect,
                state.view,
                source.geometry,
                Stroke::new(3.0, sketch_colours().invalid.gamma_multiply(0.72)),
            );
        }
    }
    for entity in pending.entities() {
        let geometry = entity.geometry;
        let color = if geometry.is_degenerate() {
            sketch_colours().invalid
        } else {
            sketch_colours().pending
        };
        if entity.role == SketchEntityRole::Construction {
            paint_dashed_geometry(painter, rect, state.view, geometry, Stroke::new(2.2, color));
        } else {
            paint_geometry(painter, rect, state.view, geometry, Stroke::new(2.4, color));
        }
    }
}

fn paint_creation_preview(painter: &egui::Painter, rect: Rect, state: &SketchCanvasState) {
    if matches!(
        state.exact_tool,
        ToolVariant::RectangularPattern | ToolVariant::CircularPattern
    ) && !state.modifier_sources.is_empty()
        && let (Some(anchor), Some(manipulator)) =
            (state.pattern_anchor(), state.pattern_manipulator)
    {
        let anchor_screen = state.view.sketch_to_screen(rect, anchor);
        let pointer = manipulator.position;
        let pointer_screen = state.view.sketch_to_screen(rect, pointer);
        if state.exact_tool == ToolVariant::RectangularPattern {
            painter.line_segment(
                [anchor_screen, pointer_screen],
                Stroke::new(1.6, sketch_colours().pending.gamma_multiply(0.82)),
            );
            let delta_u = pointer.u - anchor.u;
            let delta_v = pointer.v - anchor.v;
            let magnitude = delta_u.hypot(delta_v);
            if magnitude > PrecisionPolicy::default().min_feature_size {
                let direction_u = delta_u / magnitude;
                let direction_v = delta_v / magnitude;
                let columns = state.active_tool_number("count_u").unwrap_or(3.0) as u16;
                let second_direction = state.active_tool_flag("second_direction").unwrap_or(false);
                let rows = if second_direction {
                    state.active_tool_number("count_v").unwrap_or(2.0) as u16
                } else {
                    1
                };
                let column_spacing = state
                    .active_tool_number("spacing_u")
                    .unwrap_or(DEFAULT_TOOL_LENGTH);
                let row_spacing = if second_direction {
                    state
                        .active_tool_number("spacing_v")
                        .unwrap_or(DEFAULT_TOOL_LENGTH)
                } else {
                    0.0
                };
                let mut remaining_markers = 255_usize;
                'pattern_rows: for row in 0..rows {
                    for column in 0..columns {
                        if row == 0 && column == 0 {
                            continue;
                        }
                        if remaining_markers == 0 {
                            break 'pattern_rows;
                        }
                        remaining_markers -= 1;
                        let column_offset = f64::from(column) * column_spacing;
                        let row_offset = f64::from(row) * row_spacing;
                        let marker = SketchPoint::new(
                            direction_u.mul_add(
                                column_offset,
                                (-direction_v).mul_add(row_offset, anchor.u),
                            ),
                            direction_v
                                .mul_add(column_offset, direction_u.mul_add(row_offset, anchor.v)),
                        );
                        painter.circle_filled(
                            state.view.sketch_to_screen(rect, marker),
                            3.0,
                            sketch_colours().pending,
                        );
                    }
                }
            }
        } else {
            let radius = anchor_screen.distance(pointer_screen);
            if radius.is_finite() && radius > 0.0 {
                painter.circle_stroke(
                    pointer_screen,
                    radius,
                    Stroke::new(1.4, sketch_colours().pending.gamma_multiply(0.72)),
                );
                painter.line_segment(
                    [pointer_screen, anchor_screen],
                    Stroke::new(1.2, sketch_colours().pending.gamma_multiply(0.72)),
                );
                let count = state.active_tool_number("count").unwrap_or(4.0) as u16;
                let complete = state.active_tool_flag("full_circle").unwrap_or(true);
                let total_angle = if complete {
                    std::f64::consts::TAU
                } else {
                    state
                        .active_tool_number("extent")
                        .unwrap_or(360.0)
                        .to_radians()
                };
                let divisor = if complete {
                    f64::from(count)
                } else {
                    f64::from(count.saturating_sub(1).max(1))
                };
                let seed_u = anchor.u - pointer.u;
                let seed_v = anchor.v - pointer.v;
                for instance in 1..count {
                    let angle = f64::from(instance) * total_angle / divisor;
                    let marker = SketchPoint::new(
                        seed_u.mul_add(angle.cos(), (-seed_v).mul_add(angle.sin(), pointer.u)),
                        seed_u.mul_add(angle.sin(), seed_v.mul_add(angle.cos(), pointer.v)),
                    );
                    painter.circle_filled(
                        state.view.sketch_to_screen(rect, marker),
                        3.0,
                        sketch_colours().pending,
                    );
                }
            }
        }
        let handle_fill = if manipulator.dragging {
            sketch_colours().selected
        } else {
            sketch_colours().pending
        };
        painter.rect_filled(
            Rect::from_center_size(pointer_screen, Vec2::splat(9.0)),
            1.5,
            handle_fill,
        );
        painter.rect_stroke(
            Rect::from_center_size(pointer_screen, Vec2::splat(13.0)),
            2.0,
            Stroke::new(1.4, translucent(sketch_colours().overlay_text, 210)),
            egui::StrokeKind::Outside,
        );
        paint_snap_marker(painter, rect, state);
        return;
    }
    if state.exact_tool == ToolVariant::ChainedPolyline && !state.polyline_vertices.is_empty() {
        for vertices in state.polyline_vertices.windows(2) {
            paint_geometry(
                painter,
                rect,
                state.view,
                SketchGeometry::segment(vertices[0], vertices[1]),
                Stroke::new(2.1, sketch_colours().pending),
            );
        }
        if state.polyline_current_segment_active
            && let Some(session) = state
                .dimension_session
                .as_ref()
                .filter(|session| session.target == DimensionTarget::Draft)
            && !session.geometry.is_degenerate()
        {
            paint_geometry(
                painter,
                rect,
                state.view,
                session.geometry,
                Stroke::new(1.5, sketch_colours().pending.gamma_multiply(0.72)),
            );
        }
        if let Some(first) = state.polyline_vertices.first().copied() {
            painter.circle_stroke(
                state.view.sketch_to_screen(rect, first),
                5.0,
                Stroke::new(1.5, sketch_colours().pending),
            );
        }
        if let Some(last) = state.polyline_vertices.last().copied() {
            painter.circle_filled(
                state.view.sketch_to_screen(rect, last),
                3.5,
                sketch_colours().pending,
            );
        }
        paint_snap_marker(painter, rect, state);
        return;
    }
    if let Some(geometries) = exact_creation_preview_geometries(state) {
        for geometry in geometries {
            let color = if geometry.is_degenerate() {
                sketch_colours().invalid.gamma_multiply(0.82)
            } else {
                sketch_colours().pending.gamma_multiply(0.72)
            };
            paint_geometry(painter, rect, state.view, geometry, Stroke::new(1.5, color));
        }
        if let Some(anchor) = state.creation_anchor {
            painter.circle_filled(
                state.view.sketch_to_screen(rect, anchor),
                3.5,
                sketch_colours().pending,
            );
        }
        if let Some(second) = state.arc_start {
            painter.circle_filled(
                state.view.sketch_to_screen(rect, second),
                3.5,
                sketch_colours().pending,
            );
        }
        paint_snap_marker(painter, rect, state);
        return;
    }
    if let Some(session) = state
        .dimension_session
        .as_ref()
        .filter(|session| session.target == DimensionTarget::Draft)
    {
        let color = if session.geometry.is_degenerate() {
            sketch_colours().invalid.gamma_multiply(0.82)
        } else {
            sketch_colours().pending.gamma_multiply(0.72)
        };
        let stroke = Stroke::new(1.5, color);
        if state.tool == SketchTool::CentreLine {
            paint_dashed_geometry(painter, rect, state.view, session.geometry, stroke);
        } else {
            paint_geometry(painter, rect, state.view, session.geometry, stroke);
        }
        if let Some(anchor) = state.creation_anchor {
            painter.circle_filled(
                state.view.sketch_to_screen(rect, anchor),
                3.5,
                sketch_colours().pending,
            );
        }
        paint_snap_marker(painter, rect, state);
        return;
    }
    let (Some(anchor), Some(pointer)) = (state.creation_anchor, state.pointer_preview) else {
        paint_snap_marker(painter, rect, state);
        return;
    };
    let geometry = match state.tool {
        SketchTool::Select => return,
        SketchTool::Point => SketchGeometry::point(pointer.point),
        SketchTool::Line | SketchTool::CentreLine => SketchGeometry::segment(anchor, pointer.point),
        SketchTool::Rectangle => SketchGeometry::rectangle(anchor, pointer.point),
        SketchTool::Circle => SketchGeometry::circle(anchor, pointer.point),
        SketchTool::Arc => {
            if let Some(start) = state.arc_start {
                SketchGeometry::arc(anchor, start, pointer.point)
            } else {
                // The center-to-start radius is construction-only until the
                // third click supplies an arc end direction.
                SketchGeometry::segment(anchor, pointer.point)
            }
        }
    };
    let color = if geometry.is_degenerate() {
        sketch_colours().invalid.gamma_multiply(0.82)
    } else {
        sketch_colours().pending.gamma_multiply(0.72)
    };
    let stroke = Stroke::new(1.5, color);
    if state.tool == SketchTool::CentreLine {
        paint_dashed_geometry(painter, rect, state.view, geometry, stroke);
    } else {
        paint_geometry(painter, rect, state.view, geometry, stroke);
    }
    let anchor_screen = state.view.sketch_to_screen(rect, anchor);
    painter.circle_filled(anchor_screen, 3.5, sketch_colours().pending);
    paint_snap_marker(painter, rect, state);
}

fn paint_snap_marker(painter: &egui::Painter, rect: Rect, state: &SketchCanvasState) {
    let Some(snap) = state.pointer_preview else {
        return;
    };
    let position = state.view.sketch_to_screen(rect, snap.point);
    let color = if snap.kind.is_support_reference() {
        sketch_colours().snap_support
    } else {
        sketch_colours().snap
    };
    let stroke = Stroke::new(1.3, color);
    match snap.kind {
        SnapKind::Endpoint(_) | SnapKind::SupportEndpoint => {
            let radius = 5.5;
            for [first, second] in [
                [egui::pos2(-radius, -radius), egui::pos2(radius, -radius)],
                [egui::pos2(radius, -radius), egui::pos2(radius, radius)],
                [egui::pos2(radius, radius), egui::pos2(-radius, radius)],
                [egui::pos2(-radius, radius), egui::pos2(-radius, -radius)],
            ] {
                painter.line_segment(
                    [position + first.to_vec2(), position + second.to_vec2()],
                    stroke,
                );
            }
        }
        SnapKind::Intersection(_, _) => {
            painter.line_segment(
                [
                    position + egui::vec2(-5.0, -5.0),
                    position + egui::vec2(5.0, 5.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    position + egui::vec2(-5.0, 5.0),
                    position + egui::vec2(5.0, -5.0),
                ],
                stroke,
            );
        }
        SnapKind::Center(_) | SnapKind::SupportCenter => {
            painter.circle_stroke(position, 5.0, stroke);
            painter.circle_filled(position, 1.7, color);
        }
        SnapKind::Midpoint(_) | SnapKind::SupportMidpoint => {
            for [first, second] in [
                [egui::vec2(0.0, -5.5), egui::vec2(5.5, 0.0)],
                [egui::vec2(5.5, 0.0), egui::vec2(0.0, 5.5)],
                [egui::vec2(0.0, 5.5), egui::vec2(-5.5, 0.0)],
                [egui::vec2(-5.5, 0.0), egui::vec2(0.0, -5.5)],
            ] {
                painter.line_segment([position + first, position + second], stroke);
            }
        }
        SnapKind::Quadrant(_, _) | SnapKind::SupportQuadrant => {
            painter.circle_stroke(position, 4.5, stroke);
        }
        SnapKind::OnCurve(_) | SnapKind::SupportEdge => {
            // The hourglass reads as "somewhere along this edge" rather than
            // naming a distinguished point, matching the usual nearest glyph.
            for [first, second] in [
                [egui::vec2(-5.0, -5.0), egui::vec2(5.0, -5.0)],
                [egui::vec2(-5.0, 5.0), egui::vec2(5.0, 5.0)],
                [egui::vec2(-5.0, -5.0), egui::vec2(5.0, 5.0)],
                [egui::vec2(5.0, -5.0), egui::vec2(-5.0, 5.0)],
            ] {
                painter.line_segment([position + first, position + second], stroke);
            }
        }
        SnapKind::Grid | SnapKind::None => {
            painter.circle_stroke(position, 3.5, stroke);
        }
    }
}

fn paint_geometry(
    painter: &egui::Painter,
    rect: Rect,
    view: SketchView,
    geometry: SketchGeometry,
    stroke: Stroke,
) {
    match geometry {
        SketchGeometry::Point(point) => {
            painter.circle_filled(view.sketch_to_screen(rect, point), 3.5, stroke.color);
        }
        SketchGeometry::Segment { start, end } => {
            painter.line_segment(
                [
                    view.sketch_to_screen(rect, start),
                    view.sketch_to_screen(rect, end),
                ],
                stroke,
            );
        }
        SketchGeometry::Rectangle { .. } => {
            let corners = geometry
                .rectangle_corners()
                .expect("the rectangle branch always supplies corners")
                .map(|point| view.sketch_to_screen(rect, point));
            for index in 0..4 {
                painter.line_segment([corners[index], corners[(index + 1) % 4]], stroke);
            }
            if let Some(center) = geometry.center() {
                let center_screen = view.sketch_to_screen(rect, center);
                painter.circle_filled(center_screen, 2.5, stroke.color.gamma_multiply(0.75));
            }
        }
        SketchGeometry::Circle { center, rim } => {
            let center_screen = view.sketch_to_screen(rect, center);
            let rim_screen = view.sketch_to_screen(rect, rim);
            painter.circle_stroke(center_screen, center_screen.distance(rim_screen), stroke);
            painter.circle_filled(center_screen, 2.5, stroke.color.gamma_multiply(0.85));
        }
        SketchGeometry::Arc { center, start, end } => {
            paint_arc(painter, rect, view, center, start, end, stroke);
            let center_screen = view.sketch_to_screen(rect, center);
            painter.circle_filled(center_screen, 2.5, stroke.color.gamma_multiply(0.75));
        }
    }
}

fn paint_dashed_geometry(
    painter: &egui::Painter,
    rect: Rect,
    view: SketchView,
    geometry: SketchGeometry,
    stroke: Stroke,
) {
    let Some(polyline) = geometry.display_polyline() else {
        paint_geometry(painter, rect, view, geometry, stroke);
        return;
    };
    for [start, end] in polyline.segments() {
        let start = view.sketch_to_screen(rect, start);
        let end = view.sketch_to_screen(rect, end);
        let delta = end - start;
        let length = delta.length();
        if !length.is_finite() || length <= f32::EPSILON {
            continue;
        }
        let direction = delta / length;
        let mut offset = 0.0_f32;
        while offset < length {
            let dash_end = (offset + 7.0).min(length);
            painter.line_segment(
                [start + direction * offset, start + direction * dash_end],
                stroke,
            );
            offset += 11.0;
        }
    }
}

fn paint_overlay(
    painter: &egui::Painter,
    rect: Rect,
    state: &SketchCanvasState,
    face_aligned: bool,
) {
    let plane_label = if face_aligned {
        "Face plane"
    } else {
        match state.plane {
            SketchPlane::XY => "XY plane",
            SketchPlane::YZ => "YZ plane",
            SketchPlane::XZ => "XZ plane",
        }
    };
    let instruction = state.canvas_instruction();
    painter.text(
        rect.left_top() + Vec2::new(13.0, 12.0),
        Align2::LEFT_TOP,
        format!("{plane_label} · {instruction}"),
        FontId::monospace(10.0),
        sketch_colours().overlay_text,
    );

    let snap_label = state
        .pointer_preview
        .map_or("Snap: inactive", |snap| snap.kind.label());
    let profile_label = state.certified_profile_status().label();
    // Second line under the instruction, not the bottom edge. Three things want
    // the bottom-left corner — the projection chip, this line, and the floating
    // confirmation chip that became the sketch's only tick and cross when the
    // right-hand panel went away — and this line lost. `PROFILE EMPTY` read as
    // `FILE EMP`, which is the one status that says whether Extrude can run.
    painter.text(
        rect.left_top() + Vec2::new(13.0, 28.0),
        Align2::LEFT_TOP,
        format!("{snap_label} · {profile_label}"),
        FontId::monospace(10.0),
        sketch_colours().overlay_text,
    );
}

#[derive(Clone, Copy, Debug)]
struct DimensionWidgetLayout {
    readout: DimensionReadout,
    rect: Rect,
    leader_start: Pos2,
    /// The two feature points this dimension spans, in screen space.
    ///
    /// A leader to a midpoint says "this number is about that thing"; a drafted
    /// dimension says *what it measures* — witness lines standing off the two
    /// ends, a dimension line between them, arrowheads closing on both. Kinds
    /// that measure a span carry it; kinds that measure a position do not.
    span: Option<(Pos2, Pos2)>,
    id: Id,
}

/// The geometry the dimension boxes measure for the selected feature.
///
/// A rectangle authors one recipe but does not stay one presentation entity:
/// the first replay of a legacy composite explodes it into one entity per
/// exact curve (see `core_transaction_presentation`), and reloading a document
/// rebuilds it the same way. Measuring whichever side happened to be picked
/// would then show a line's length where the recipe says Width — so the
/// composite is reassembled from every sibling the same authored operation
/// owns, which is what the SELECTED FEATURE card is already describing. The
/// picked side is not lost: `dimension_pick` remembers where the click
/// landed, and the armed box dresses that side.
fn dimension_target(state: &SketchCanvasState) -> Option<(SketchGeometry, u64)> {
    let selected = state.selected?;
    let entity = state.presented_entity(selected)?;
    let composite = state
        .selected_recipe_editor
        .as_ref()
        .filter(|editor| {
            editor.subject == selected
                && matches!(
                    editor.original_recipe,
                    CoreRecipe::TwoPointRectangle { .. } | CoreRecipe::CentrePointRectangle { .. }
                )
        })
        .and_then(|_| rectangle_from_operation_siblings(state, selected));
    // The picked entity keeps the widget serial: its identity survives the
    // explode, so a `TextEdit` under the caret is not rebuilt underneath it.
    Some((composite.unwrap_or(entity.geometry), selected.get()))
}

/// Reassembles an axis-aligned rectangle from the curves of one authored
/// operation. Both rectangle recipes are axis-aligned, so their bounds are the
/// rectangle exactly rather than an approximation of it.
fn rectangle_from_operation_siblings(
    state: &SketchCanvasState,
    selected: SketchEntityId,
) -> Option<SketchGeometry> {
    let siblings: Vec<SketchEntity> = match state.pending.as_ref() {
        // A live in-place candidate is the whole feature, exploded.
        Some(pending) if pending.in_place => pending.entities.clone(),
        _ => {
            let operation = state.operation_by_ui.get(&selected).copied()?;
            state
                .entities
                .iter()
                .filter(|entity| state.operation_by_ui.get(&entity.id) == Some(&operation))
                .copied()
                .collect()
        }
    };
    let bounds = sketch_point_bounds(
        siblings
            .iter()
            .flat_map(|entity| entity.geometry.control_points().iter()),
    )?;
    let [min_u, max_u, min_v, max_v] = bounds;
    (max_u > min_u && max_v > min_v).then(|| {
        SketchGeometry::rectangle(
            SketchPoint::new(min_u, min_v),
            SketchPoint::new(max_u, max_v),
        )
    })
}

/// The recipe keys `committed_dimension_parameter` can ever resolve for one
/// recipe: the kinds its geometry offers, mapped to the keys it carries. A
/// rectangle's chip yields width and height; a circle, its diameter; a line,
/// length and angle; a fillet is an arc, so radius; a polygon edge is a line
/// whose length falls through to `"side"`. A slot's edges and arcs resolve to
/// keys the slot does not have, so nothing on a slot is drivable from a box.
const fn canvas_dimensionable_keys(recipe: &CoreRecipe) -> &'static [&'static str] {
    match recipe {
        CoreRecipe::TwoPointRectangle { .. } | CoreRecipe::CentrePointRectangle { .. } => {
            &["width", "height"]
        }
        CoreRecipe::CentrePointCircle { .. } => &["diameter"],
        CoreRecipe::Line { .. } | CoreRecipe::CentreLine { .. } => &["length", "angle"],
        CoreRecipe::InnerDiameterPolygon { .. } | CoreRecipe::OuterDiameterPolygon { .. } => {
            &["side"]
        }
        CoreRecipe::Fillet { .. } | CoreRecipe::FilletWithHints { .. } => &["radius"],
        _ => &[],
    }
}

/// The recipe literal a committed dimension box drives, if the Dimension tool
/// has armed one.
fn committed_dimension_parameter(
    state: &SketchCanvasState,
    kind: SketchDimensionKind,
) -> Option<&RetainedRecipeParameter> {
    if state.exact_tool != ToolVariant::Dimension {
        return None;
    }
    let stable_key = match kind {
        SketchDimensionKind::Width => "width",
        SketchDimensionKind::Height => "height",
        SketchDimensionKind::Diameter => "diameter",
        SketchDimensionKind::Radius => "radius",
        // A line calls it "length"; a polygon edge is one of n equal "side"s;
        // a slot calls it "overall_length" or "centre_distance".
        SketchDimensionKind::Length => "length",
        SketchDimensionKind::AngleDegrees => "angle",
        _ => return None,
    };
    let editor = state.selected_recipe_editor.as_ref()?;
    let stable_key = if stable_key == "length"
        && !editor
            .parameters
            .iter()
            .any(|parameter| parameter.stable_key == "length")
    {
        if editor
            .parameters
            .iter()
            .any(|parameter| parameter.stable_key == "overall_length")
        {
            "overall_length"
        } else if editor
            .parameters
            .iter()
            .any(|parameter| parameter.stable_key == "centre_distance")
        {
            "centre_distance"
        } else {
            "side"
        }
    } else if (stable_key == "radius" || stable_key == "diameter")
        && !editor
            .parameters
            .iter()
            .any(|parameter| parameter.stable_key == stable_key)
        && editor
            .parameters
            .iter()
            .any(|parameter| parameter.stable_key == "width")
    {
        "width"
    } else {
        stable_key
    };
    if state
        .pending
        .as_ref()
        .is_some_and(|pending| pending.subject != editor.subject)
    {
        return None;
    }
    editor
        .parameters
        .iter()
        .find(|parameter| parameter.stable_key == stable_key && parameter.value.is_some())
}

/// The dimension the Dimension tool arms for the selected feature.
///
/// On a rectangle the picked side names it: a horizontal side is a Width
/// question and a vertical side a Height question, so clicking the left wall
/// of a centre rectangle edits its height rather than whatever field came
/// first. Without a side identity — the semantic chip, or a non-rectangle —
/// the first drivable field keeps the caret.
fn first_armed_dimension_kind(state: &SketchCanvasState) -> Option<SketchDimensionKind> {
    let (geometry, _) = dimension_target(state)?;
    if let Some(pick) = state.dimension_pick
        && let Some(kind) = rectangle_picked_dimension_kind(geometry, pick)
        && committed_dimension_parameter(state, kind).is_some()
    {
        return Some(kind);
    }
    dimension_fields_for_geometry(dimension_phase_for_geometry(geometry), geometry)
        .into_iter()
        .map(|field| field.readout.kind)
        .find(|kind| committed_dimension_parameter(state, *kind).is_some())
}

/// The dimension the picked side of an axis-aligned rectangle asks for: a
/// horizontal side spans the width, a vertical side the height.
fn rectangle_picked_dimension_kind(
    geometry: SketchGeometry,
    pick: SketchPoint,
) -> Option<SketchDimensionKind> {
    let corners = geometry.rectangle_corners()?;
    let (min_u, min_v) = (corners[0].u, corners[0].v);
    let (max_u, max_v) = (corners[2].u, corners[2].v);
    let to_horizontal = (pick.v - min_v).abs().min((pick.v - max_v).abs());
    let to_vertical = (pick.u - min_u).abs().min((pick.u - max_u).abs());
    Some(if to_horizontal <= to_vertical {
        SketchDimensionKind::Width
    } else {
        SketchDimensionKind::Height
    })
}

/// Which of a rectangle's parallel sides carry the Width and Height
/// annotations. The picked side wins, so the box and its witness lines sit
/// on the edge that was clicked; without a pick the bottom and right sides
/// keep their traditional spots.
#[derive(Clone, Copy, Default)]
struct RectangleAnnotationSides {
    /// `true` when Width dresses the top (max v) side.
    width_on_top: bool,
    /// `true` when Height dresses the left (min u) side.
    height_on_left: bool,
}

fn rectangle_annotation_sides(
    geometry: SketchGeometry,
    pick: Option<SketchPoint>,
) -> RectangleAnnotationSides {
    let (Some(pick), Some(corners)) = (pick, geometry.rectangle_corners()) else {
        return RectangleAnnotationSides::default();
    };
    let (min_u, min_v) = (corners[0].u, corners[0].v);
    let (max_u, max_v) = (corners[2].u, corners[2].v);
    RectangleAnnotationSides {
        width_on_top: (pick.v - max_v).abs() < (pick.v - min_v).abs(),
        height_on_left: (pick.u - min_u).abs() < (pick.u - max_u).abs(),
    }
}

fn dimension_widget_layouts(
    state: &SketchCanvasState,
    canvas_rect: Rect,
) -> Vec<DimensionWidgetLayout> {
    // A typed parameter is the selected feature's own candidate. Its read-only
    // one-curve measurement must never evict the box whose keystroke staged
    // it: on a circle that would replace the focused field with a label and
    // drop the caret on the first character.
    let editing_selected_recipe = state
        .pending
        .as_ref()
        .is_some_and(|pending| pending.in_place);
    let source = if let Some(session) = &state.dimension_session {
        Some((
            session.geometry,
            session.readouts().collect::<Vec<_>>(),
            session.serial,
            true,
        ))
    } else if let Some((geometry, fields)) = exact_live_measurement(state) {
        Some((
            geometry,
            fields
                .into_iter()
                .map(|field| field.readout)
                .collect::<Vec<_>>(),
            state.next_dimension_serial,
            true,
        ))
    } else if !editing_selected_recipe
        && let Some((geometry, fields)) = pending_single_curve_measurement(state)
    {
        Some((
            geometry,
            fields
                .into_iter()
                .map(|field| field.readout)
                .collect::<Vec<_>>(),
            state
                .pending
                .as_ref()
                .map_or(state.next_dimension_serial, |pending| pending.subject.get()),
            false,
        ))
    } else {
        dimension_target(state).map(|(geometry, serial)| {
            let phase = dimension_phase_for_geometry(geometry);
            let readouts = dimension_fields_for_geometry(phase, geometry)
                .into_iter()
                .map(|mut field| {
                    field.readout.editable =
                        committed_dimension_parameter(state, field.readout.kind).is_some();
                    field.readout
                })
                .collect::<Vec<_>>();
            (geometry, readouts, serial, false)
        })
    };
    let Some((geometry, readouts, serial, live)) = source else {
        return Vec::new();
    };
    // A live draft has no pick; the committed target dresses the side the
    // Dimension tool's click landed on.
    let sides =
        rectangle_annotation_sides(geometry, (!live).then_some(state.dimension_pick).flatten());

    readouts
        .into_iter()
        .filter(|readout| readout.kind.shows_on_canvas())
        .filter_map(|readout| {
            dimension_widget_position(geometry, readout.kind, sides, state.view, canvas_rect)
                .and_then(|(center, leader_start)| {
                    let rect = clamp_dimension_rect(
                        Rect::from_center_size(center, DIMENSION_WIDGET_SIZE),
                        canvas_rect,
                    );
                    (leader_start.is_finite() && rect.is_finite()).then_some(
                        DimensionWidgetLayout {
                            readout,
                            rect,
                            leader_start,
                            span: dimension_span(
                                geometry,
                                readout.kind,
                                sides,
                                state.view,
                                canvas_rect,
                            ),
                            id: Id::new(("sketch-dimension", live, serial, readout.kind)),
                        },
                    )
                })
        })
        .collect()
}

/// Read-only annotations for every committed feature whose recipe carries
/// driving dimensions, shown while the Dimension tool is active: the
/// engineering-drawing record that a width, height, or diameter has been
/// assigned, kept on the canvas after the pick moves on instead of vanishing
/// the moment the entity deselects. The selected feature is excluded — its
/// interactive boxes already dress it.
fn committed_dimension_annotation_layouts(
    state: &SketchCanvasState,
    canvas_rect: Rect,
) -> Vec<DimensionWidgetLayout> {
    if state.exact_tool != ToolVariant::Dimension {
        return Vec::new();
    }
    let selected_operation = state
        .selected
        .and_then(|selected| state.operation_by_ui.get(&selected).copied());
    let superseded = state.superseded_by_in_place_edit();
    let mut layouts = Vec::new();
    for (index, operation) in state
        .authoring
        .operations()
        .iter()
        .filter(|operation| operation.active)
        .enumerate()
    {
        if selected_operation == Some(operation.id) {
            continue;
        }
        let entities = state
            .entities
            .iter()
            .filter(|entity| {
                !superseded.contains(&entity.id)
                    && state.operation_by_ui.get(&entity.id) == Some(&operation.id)
            })
            .copied()
            .collect::<Vec<_>>();
        if entities.is_empty() {
            continue;
        }
        let mut dressed: Vec<(SketchDimensionKind, SketchGeometry, f64)> = Vec::new();
        match operation.recipe {
            CoreRecipe::TwoPointRectangle { .. } | CoreRecipe::CentrePointRectangle { .. } => {
                if let Some([min_u, max_u, min_v, max_v]) = sketch_point_bounds(
                    entities
                        .iter()
                        .flat_map(|entity| entity.geometry.control_points().iter()),
                ) && max_u > min_u
                    && max_v > min_v
                {
                    let geometry = SketchGeometry::rectangle(
                        SketchPoint::new(min_u, min_v),
                        SketchPoint::new(max_u, max_v),
                    );
                    dressed.push((SketchDimensionKind::Width, geometry, max_u - min_u));
                    dressed.push((SketchDimensionKind::Height, geometry, max_v - min_v));
                }
            }
            CoreRecipe::CentrePointCircle { .. } => {
                if let Some(geometry @ SketchGeometry::Circle { center, rim }) = entities
                    .iter()
                    .map(|entity| entity.geometry)
                    .find(|geometry| matches!(geometry, SketchGeometry::Circle { .. }))
                {
                    let radius = (rim.u - center.u).hypot(rim.v - center.v);
                    if radius.is_finite() && radius > 0.0 {
                        dressed.push((SketchDimensionKind::Diameter, geometry, radius * 2.0));
                    }
                }
            }
            CoreRecipe::TwoPointSlot {
                width: CoreValue::Literal(w),
                ..
            } => {
                if let Some(arc) = entities
                    .iter()
                    .find(|e| matches!(e.geometry, SketchGeometry::Arc { .. }))
                {
                    dressed.push((SketchDimensionKind::Radius, arc.geometry, w.get() * 0.5));
                }
            }
            CoreRecipe::CentreOuterPointSlot {
                overall_length,
                width,
                ..
            } => {
                if let CoreValue::Literal(w) = width
                    && let Some(arc) = entities
                        .iter()
                        .find(|e| matches!(e.geometry, SketchGeometry::Arc { .. }))
                {
                    dressed.push((SketchDimensionKind::Radius, arc.geometry, w.get() * 0.5));
                }
                if let CoreValue::Literal(l) = overall_length
                    && let Some(seg) = entities
                        .iter()
                        .find(|e| matches!(e.geometry, SketchGeometry::Segment { .. }))
                {
                    dressed.push((SketchDimensionKind::Length, seg.geometry, l.get()));
                }
            }
            _ => {}
        }
        for (kind, geometry, value) in dressed {
            let Some((center, leader_start)) = dimension_widget_position(
                geometry,
                kind,
                RectangleAnnotationSides::default(),
                state.view,
                canvas_rect,
            ) else {
                continue;
            };
            let rect = clamp_dimension_rect(
                Rect::from_center_size(center, DIMENSION_WIDGET_SIZE),
                canvas_rect,
            );
            if !leader_start.is_finite() || !rect.is_finite() {
                continue;
            }
            layouts.push(DimensionWidgetLayout {
                readout: DimensionReadout {
                    kind,
                    value,
                    locked: false,
                    editable: false,
                },
                rect,
                leader_start,
                span: dimension_span(
                    geometry,
                    kind,
                    RectangleAnnotationSides::default(),
                    state.view,
                    canvas_rect,
                ),
                id: Id::new(("sketch-committed-dimension", index, kind)),
            });
        }
    }
    layouts
}

/// Paints the committed dimension records: the drafted annotation with a
/// quiet read-only value plate. These are canvas ink rather than widgets —
/// they never take a click or the caret.
fn paint_committed_dimension_annotations(
    painter: &egui::Painter,
    layouts: &[DimensionWidgetLayout],
) {
    let colours = sketch_colours();
    let stroke = Stroke::new(1.0, colours.dimension.gamma_multiply(0.45));
    for layout in layouts {
        if !paint_dimension_annotation(painter, layout, stroke) {
            painter.line_segment([layout.leader_start, layout.rect.center()], stroke);
        }
        painter.rect(
            layout.rect,
            4.0,
            colours.dimension_background.gamma_multiply(0.85),
            Stroke::new(1.0, colours.dimension.gamma_multiply(0.55)),
            egui::StrokeKind::Inside,
        );
        painter.text(
            layout.rect.center(),
            Align2::CENTER_CENTER,
            format_dimension_readout(layout.readout),
            FontId::monospace(10.0),
            colours.dimension.gamma_multiply(0.85),
        );
    }
}

fn dimension_widget_position(
    geometry: SketchGeometry,
    kind: SketchDimensionKind,
    sides: RectangleAnnotationSides,
    view: SketchView,
    canvas_rect: Rect,
) -> Option<(Pos2, Pos2)> {
    let screen = |point| view.sketch_to_screen(canvas_rect, point);
    match (kind, geometry) {
        (SketchDimensionKind::U, SketchGeometry::Point(point)) => {
            let point = screen(point);
            Some((point + Vec2::new(66.0, -14.0), point))
        }
        (SketchDimensionKind::V, SketchGeometry::Point(point)) => {
            let point = screen(point);
            Some((point + Vec2::new(66.0, 14.0), point))
        }
        (
            SketchDimensionKind::Length
            | SketchDimensionKind::AngleDegrees
            | SketchDimensionKind::DeltaU
            | SketchDimensionKind::DeltaV,
            SketchGeometry::Segment { start, end },
        ) => {
            let start = screen(start);
            let end = screen(end);
            let midpoint = start + (end - start) * 0.5;
            let direction = (end - start).normalized();
            let normal = if direction == Vec2::ZERO {
                Vec2::new(0.0, -1.0)
            } else {
                Vec2::new(-direction.y, direction.x)
            };
            // Straddle the line rather than cascade down one side of it. The
            // box is 24 px tall, so an offset of 20 left it overlapping the
            // geometry by all but eight pixels — close enough to read as
            // covering the line you are drawing. The deltas no longer appear
            // here at all, which is what collapses the old four-box stack.
            let offset = match kind {
                SketchDimensionKind::Length => 46.0,
                SketchDimensionKind::AngleDegrees => -46.0,
                SketchDimensionKind::DeltaU | SketchDimensionKind::DeltaV => {
                    unreachable!("deltas are panel-only; shows_on_canvas filters them")
                }
                _ => unreachable!("the match arm filters line dimension kinds"),
            };
            Some((midpoint + normal * offset, midpoint))
        }
        (SketchDimensionKind::Width, geometry @ SketchGeometry::Rectangle { .. }) => {
            let corners = geometry.rectangle_corners()?;
            let (first, second, outward) = if sides.width_on_top {
                (screen(corners[3]), screen(corners[2]), -18.0)
            } else {
                (screen(corners[0]), screen(corners[1]), 18.0)
            };
            let midpoint = first + (second - first) * 0.5;
            Some((midpoint + Vec2::new(0.0, outward), midpoint))
        }
        (SketchDimensionKind::Height, geometry @ SketchGeometry::Rectangle { .. }) => {
            let corners = geometry.rectangle_corners()?;
            let (first, second, outward) = if sides.height_on_left {
                (screen(corners[0]), screen(corners[3]), -64.0)
            } else {
                (screen(corners[1]), screen(corners[2]), 64.0)
            };
            let midpoint = first + (second - first) * 0.5;
            Some((midpoint + Vec2::new(outward, 0.0), midpoint))
        }
        (SketchDimensionKind::Width, SketchGeometry::Segment { start, end }) => {
            let start = screen(start);
            let end = screen(end);
            let midpoint = start + (end - start) * 0.5;
            let direction = (end - start).normalized();
            let normal = if direction == Vec2::ZERO {
                Vec2::new(1.0, 0.0)
            } else {
                Vec2::new(-direction.y, direction.x)
            };
            Some((midpoint + normal * 24.0, midpoint))
        }
        (SketchDimensionKind::Diameter, SketchGeometry::Circle { center, rim }) => {
            let center = screen(center);
            let radius = center.distance(screen(rim));
            let leader = center + Vec2::new(0.0, -radius);
            Some((leader + Vec2::new(0.0, -18.0), leader))
        }
        (SketchDimensionKind::Radius, SketchGeometry::Segment { start, end }) => {
            let start = screen(start);
            let end = screen(end);
            let midpoint = start + (end - start) * 0.5;
            let direction = (end - start).normalized();
            let normal = if direction == Vec2::ZERO {
                Vec2::new(0.0, -1.0)
            } else {
                Vec2::new(-direction.y, direction.x)
            };
            Some((midpoint + normal * 20.0, midpoint))
        }
        (
            SketchDimensionKind::Radius,
            SketchGeometry::Arc {
                center,
                start,
                end: _,
            },
        ) => {
            let center = screen(center);
            let start = screen(start);
            let midpoint = center + (start - center) * 0.5;
            Some((midpoint + Vec2::new(0.0, -20.0), midpoint))
        }
        (SketchDimensionKind::SweepDegrees, SketchGeometry::Arc { center, start, end }) => {
            let radius = center.distance_squared(start).sqrt();
            let start_angle = (start.v - center.v).atan2(start.u - center.u);
            let middle_angle = arc_sweep(center, start, end).mul_add(0.5, start_angle);
            let middle = SketchPoint::new(
                radius.mul_add(middle_angle.cos(), center.u),
                radius.mul_add(middle_angle.sin(), center.v),
            );
            let center_screen = screen(center);
            let middle_screen = screen(middle);
            let outward = (middle_screen - center_screen).normalized();
            let outward = if outward == Vec2::ZERO {
                Vec2::new(0.0, -1.0)
            } else {
                outward
            };
            Some((middle_screen + outward * 24.0, middle_screen))
        }
        _ => None,
    }
}

fn clamp_dimension_rect(rect: Rect, canvas_rect: Rect) -> Rect {
    let bounds = canvas_rect.shrink(4.0);
    let mut delta = Vec2::ZERO;
    if rect.left() < bounds.left() {
        delta.x += bounds.left() - rect.left();
    } else if rect.right() > bounds.right() {
        delta.x -= rect.right() - bounds.right();
    }
    if rect.top() < bounds.top() {
        delta.y += bounds.top() - rect.top();
    } else if rect.bottom() > bounds.bottom() {
        delta.y -= rect.bottom() - bounds.bottom();
    }
    rect.translate(delta)
}

/// The two points a dimension of this kind measures between, in screen space.
///
/// Only span kinds have one. An angle is measured about a vertex and a
/// coordinate is measured from an axis, so neither gets witness lines.
fn dimension_span(
    geometry: SketchGeometry,
    kind: SketchDimensionKind,
    sides: RectangleAnnotationSides,
    view: SketchView,
    canvas_rect: Rect,
) -> Option<(Pos2, Pos2)> {
    let screen = |point| view.sketch_to_screen(canvas_rect, point);
    match (kind, geometry) {
        (SketchDimensionKind::Length, SketchGeometry::Segment { start, end }) => {
            Some((screen(start), screen(end)))
        }
        (SketchDimensionKind::Diameter, SketchGeometry::Circle { center, rim }) => {
            let centre = screen(center);
            let edge = screen(rim);
            let radius = (edge - centre).length();
            (radius.is_finite() && radius > 0.0).then(|| {
                let offset = Vec2::new(radius, 0.0);
                (centre - offset, centre + offset)
            })
        }
        (SketchDimensionKind::Width, geometry @ SketchGeometry::Rectangle { .. }) => {
            let corners = geometry.rectangle_corners()?;
            Some(if sides.width_on_top {
                (screen(corners[3]), screen(corners[2]))
            } else {
                (screen(corners[0]), screen(corners[1]))
            })
        }
        (SketchDimensionKind::Height, geometry @ SketchGeometry::Rectangle { .. }) => {
            let corners = geometry.rectangle_corners()?;
            Some(if sides.height_on_left {
                (screen(corners[0]), screen(corners[3]))
            } else {
                (screen(corners[1]), screen(corners[2]))
            })
        }
        _ => None,
    }
}

/// Draws a dimension the way a drawing does: witness lines standing off the
/// measured ends, a dimension line between them offset to where the value sits,
/// and arrowheads closing inward on both.
///
/// The whole annotation is built in the feature's own frame — the dimension
/// line runs parallel to what it measures and the witness lines run
/// perpendicular — so it rotates with the geometry instead of staying
/// stubbornly axis-aligned.
fn paint_dimension_annotation(
    painter: &egui::Painter,
    layout: &DimensionWidgetLayout,
    stroke: Stroke,
) -> bool {
    let Some((start, end)) = layout.span else {
        return false;
    };
    let along = end - start;
    let length = along.length();
    if !length.is_finite() || length < 1.0 {
        return false;
    }
    let along = along / length;
    let normal = Vec2::new(-along.y, along.x);

    // Project the value box onto the feature normal: the dimension line passes
    // through the box, so the annotation follows the box wherever it is placed
    // rather than assuming a fixed side.
    let offset = (layout.rect.center() - start).dot(normal);
    let dimension_start = start + normal * offset;
    let dimension_end = end + normal * offset;

    // Witness lines stop short of the geometry and overrun the dimension line,
    // which is what keeps them readable where they meet the feature.
    const WITNESS_GAP: f32 = 3.0;
    const WITNESS_OVERRUN: f32 = 5.0;
    let sign = if offset >= 0.0 { 1.0 } else { -1.0 };
    for (foot, head) in [(start, dimension_start), (end, dimension_end)] {
        let span = head - foot;
        let reach = span.length();
        if reach <= WITNESS_GAP + WITNESS_OVERRUN {
            continue;
        }
        let direction = span / reach;
        painter.line_segment(
            [
                foot + direction * WITNESS_GAP,
                head + normal * (WITNESS_OVERRUN * sign),
            ],
            stroke,
        );
    }

    // The dimension line breaks either side of the value rather than running
    // under it: text sitting on a line is the thing that makes a drawing hard
    // to read.
    let gap = layout.rect.width() * 0.5 + 4.0;
    let centre = dimension_start + (dimension_end - dimension_start) * 0.5;
    for direction in [-1.0_f32, 1.0] {
        let inner = centre + along * (gap * direction);
        let outer = if direction < 0.0 {
            dimension_start
        } else {
            dimension_end
        };
        if (outer - inner).dot(along) * direction > 0.0 {
            painter.line_segment([inner, outer], stroke);
        }
    }

    for (tip, direction) in [(dimension_start, along), (dimension_end, -along)] {
        paint_dimension_arrowhead(painter, tip, direction, stroke.color);
    }
    true
}

fn paint_dimension_arrowhead(painter: &egui::Painter, tip: Pos2, direction: Vec2, color: Color32) {
    const LENGTH: f32 = 9.0;
    const HALF_WIDTH: f32 = 3.0;
    let normal = Vec2::new(-direction.y, direction.x);
    let base = tip + direction * LENGTH;
    painter.add(egui::Shape::convex_polygon(
        vec![tip, base + normal * HALF_WIDTH, base - normal * HALF_WIDTH],
        color,
        Stroke::NONE,
    ));
}

fn paint_dimension_leaders(painter: &egui::Painter, layouts: &[DimensionWidgetLayout]) {
    for layout in layouts {
        let stroke = Stroke::new(1.0, sketch_colours().dimension.gamma_multiply(0.72));
        // A span kind gets the drafted annotation. Everything else — an angle
        // about a vertex, a coordinate from an axis — keeps the plain leader,
        // because witness lines would be claiming a span that is not there.
        if !paint_dimension_annotation(painter, layout, stroke) {
            painter.line_segment([layout.leader_start, layout.rect.center()], stroke);
        }
    }
}

/// What one frame of dimension boxes hands back to the canvas.
struct DimensionWidgetOutcome {
    claims: DimensionKeyClaims,
    pending_created: Option<SketchEntityId>,
    /// The on-canvas recipe field that owns the keyboard, with the rectangle it
    /// occupies. The workbench settles acceptance from this exactly as it does
    /// for the Properties field.
    recipe_field: Option<(Id, Rect)>,
    /// The focused on-canvas recipe field consumed `Enter` this frame.
    recipe_accepted: bool,
}

fn show_dimension_widgets(
    ui: &mut Ui,
    state: &mut SketchCanvasState,
    layouts: &[DimensionWidgetLayout],
    canvas_owned_keyboard: bool,
) -> DimensionWidgetOutcome {
    let active_at_start = state.active_dimension();
    let enter_pressed = raw_key_pressed(ui, egui::Key::Enter, egui::Modifiers::NONE);
    let escape_pressed = raw_key_pressed(ui, egui::Key::Escape, egui::Modifiers::NONE);
    let backspace_pressed = raw_key_pressed(ui, egui::Key::Backspace, egui::Modifiers::NONE);
    let tab_forward = raw_key_pressed(ui, egui::Key::Tab, egui::Modifiers::NONE);
    let tab_backward = raw_key_pressed(ui, egui::Key::Tab, egui::Modifiers::SHIFT);

    let mut clicked_kind = None;
    let mut live_changed = false;
    let mut active_editor_owned_keyboard = false;
    let mut claims = DimensionKeyClaims::default();
    let mut recipe_field = None;
    let mut recipe_accepted = false;
    for layout in layouts {
        // The Dimension tool edits committed geometry through the selected
        // feature's authoritative recipe rather than through a second editor.
        // The text is the literal that will be replayed, so what is typed is
        // exactly what the kernel receives. Bind it first: the shared borrow
        // of `state` has to end before the edit below takes it mutably.
        let committed =
            committed_dimension_parameter(state, layout.readout.kind).map(|parameter| {
                (
                    parameter.stable_key,
                    parameter.text.clone(),
                    parameter.error.map(RecipeParameterError::label),
                )
            });
        if let Some((stable_key, mut text, error)) = committed {
            // An armed box still has to read as a dimension, so it keeps the
            // plate and leader the read-only boxes use and the field is drawn
            // transparent on top of it. Focus is read before the widget so the
            // plate can carry the highlight the text alone would not.
            let focused = ui.memory(|memory| memory.focused()) == Some(layout.id)
                || state.focus_dimension_box == Some(layout.readout.kind);
            ui.painter().rect(
                layout.rect,
                4.0,
                sketch_colours().dimension_background,
                Stroke::new(
                    if focused { 1.8 } else { 1.0 },
                    if error.is_some() {
                        sketch_colours().invalid
                    } else if focused {
                        sketch_colours().selected
                    } else {
                        sketch_colours().dimension
                    },
                ),
                egui::StrokeKind::Inside,
            );
            let response = ui.put(
                layout.rect,
                egui::TextEdit::singleline(&mut text)
                    .id(layout.id)
                    .desired_width(layout.rect.width())
                    .horizontal_align(egui::Align::Center)
                    .background_color(Color32::TRANSPARENT)
                    .font(FontId::monospace(11.0))
                    .text_color(if error.is_some() {
                        sketch_colours().invalid
                    } else {
                        sketch_colours().dimension_locked
                    }),
            );
            response.ctx.accesskit_node_builder(response.id, |node| {
                node.set_label(layout.readout.kind.label());
                node.set_description(format!(
                    "{} in {}. Enter applies the value; Escape reverts it.",
                    layout.readout.kind.label(),
                    dimension_unit_label(layout.readout.kind)
                ));
            });
            let char_count = text.chars().count();
            if state.focus_dimension_box == Some(layout.readout.kind) {
                response.request_focus();
                select_all_dimension_text(ui, layout.id, &response, char_count);
            }
            if let Some(error) = error {
                ui.painter().text(
                    layout.rect.left_bottom() + Vec2::new(0.0, 3.0),
                    Align2::LEFT_TOP,
                    error,
                    FontId::monospace(9.0),
                    sketch_colours().invalid,
                );
            }
            if response.has_focus() {
                recipe_field = Some((response.id, response.rect));
            }
            let owns_keyboard = response.has_focus() || response.lost_focus();
            if response.changed() {
                state.set_selected_recipe_parameter_text(stable_key, text);
            }
            // Enter and Escape mean here exactly what they mean in the
            // Properties field for this parameter (ADR 0027): Enter applies,
            // Escape reverts. Escape is not claimed here because egui clears
            // focus before any widget renders, so the workbench has already
            // settled it by the time this box is drawn.
            if owns_keyboard && enter_pressed {
                claims.enter = true;
                if state.selected_recipe_parameter_issue().is_none() {
                    recipe_accepted = true;
                    response.surrender_focus();
                }
            }
            continue;
        }
        let is_active = active_at_start == Some(layout.readout.kind);
        if is_active {
            let (mut buffer, request_focus) = state
                .dimension_session
                .as_mut()
                .map(|session| {
                    let request = session.focus_next_frame;
                    session.focus_next_frame = false;
                    (std::mem::take(&mut session.buffer), request)
                })
                .unwrap_or_default();
            let response = ui.put(
                layout.rect,
                egui::TextEdit::singleline(&mut buffer)
                    .id(layout.id)
                    .desired_width(layout.rect.width())
                    .font(FontId::monospace(11.0))
                    .text_color(if state.dimension_error().is_some() {
                        sketch_colours().invalid
                    } else {
                        sketch_colours().dimension_locked
                    }),
            );
            response.ctx.accesskit_node_builder(response.id, |node| {
                node.set_label(layout.readout.kind.label());
                node.set_description(format!(
                    "{} in {}. Tab selects the next dimension; Enter accepts this value.",
                    layout.readout.kind.label(),
                    dimension_unit_label(layout.readout.kind)
                ));
            });
            let char_count = buffer.chars().count();
            if let Some(session) = state.dimension_session.as_mut() {
                session.buffer = buffer;
            }
            if request_focus {
                response.request_focus();
                select_all_dimension_text(ui, layout.id, &response, char_count);
            }
            active_editor_owned_keyboard |= response.has_focus() || response.lost_focus();
            if response.changed() {
                live_changed = true;
            }
        } else {
            let response = ui.interact(
                layout.rect,
                layout.id,
                if layout.readout.editable {
                    Sense::click()
                } else {
                    Sense::hover()
                },
            );
            response.widget_info(|| {
                WidgetInfo::labeled(
                    if layout.readout.editable {
                        WidgetType::Button
                    } else {
                        WidgetType::Label
                    },
                    layout.readout.editable,
                    layout.readout.kind.label(),
                )
            });
            let color = if layout.readout.locked {
                sketch_colours().dimension_locked
            } else {
                sketch_colours().dimension
            };
            ui.painter().rect(
                layout.rect,
                4.0,
                sketch_colours().dimension_background,
                Stroke::new(1.0, color),
                egui::StrokeKind::Inside,
            );
            ui.painter().text(
                layout.rect.center(),
                Align2::CENTER_CENTER,
                format_dimension_readout(layout.readout),
                FontId::monospace(10.5),
                color,
            );
            if layout.readout.editable && response.clicked() {
                clicked_kind = Some(layout.readout.kind);
            }
        }
    }

    if live_changed {
        let named_values = &state.named_values;
        let result = state
            .dimension_session
            .as_mut()
            .map_or(Ok(()), |session| session.apply_buffer_live(named_values));
        if let Some(session) = state.dimension_session.as_mut() {
            session.error = result.err();
        }
        if result.is_ok() {
            state.sync_dimension_pending();
        }
    }

    let mut pending_created = None;
    let polyline_gesture_owned = canvas_owned_keyboard
        || (state.exact_tool == ToolVariant::ChainedPolyline
            && !state.polyline_vertices.is_empty()
            && !ui.ctx().egui_wants_keyboard_input());
    let tab_owned = if active_at_start.is_some() {
        active_editor_owned_keyboard
    } else {
        canvas_owned_keyboard
    };
    if tab_owned && tab_forward {
        ui.input_mut(|input| {
            input.consume_key(egui::Modifiers::NONE, egui::Key::Tab);
        });
    }
    if tab_owned && tab_backward {
        ui.input_mut(|input| {
            input.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab);
        });
    }

    if active_at_start.is_some() && active_editor_owned_keyboard && escape_pressed {
        claims.escape = true;
        if let Some(session) = state.dimension_session.as_mut() {
            session.cancel_edit();
        }
        state.sync_dimension_pending();
    } else if tab_owned && (tab_forward || tab_backward) {
        let named_values = &state.named_values;
        if let Some(session) = state.dimension_session.as_mut()
            && let Err(error) = session.cycle(tab_backward, named_values)
        {
            session.error = Some(error);
            session.focus_next_frame = true;
        }
        state.sync_dimension_pending();
    } else if active_at_start.is_some() && active_editor_owned_keyboard && enter_pressed {
        claims.enter = true;
        let named_values = &state.named_values;
        let accepted = if let Some(session) = state.dimension_session.as_mut() {
            match session.accept(named_values) {
                Ok(()) => true,
                Err(error) => {
                    session.error = Some(error);
                    session.focus_next_frame = true;
                    false
                }
            }
        } else {
            false
        };
        if accepted {
            state.sync_dimension_pending();
            pending_created = stage_complete_dimension_draft(state);
        }
    } else if active_at_start.is_none()
        && polyline_gesture_owned
        && backspace_pressed
        && state.backspace_polyline_segment()
    {
        ui.input_mut(|input| {
            input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace);
        });
    } else if active_at_start.is_none()
        && polyline_gesture_owned
        && escape_pressed
        && state.cancel_polyline_layer()
    {
        claims.escape = true;
    } else if active_at_start.is_none()
        && polyline_gesture_owned
        && enter_pressed
        && state.exact_tool == ToolVariant::ChainedPolyline
        && !state.polyline_vertices.is_empty()
    {
        claims.enter = true;
        pending_created = state.finish_polyline_draft().ok();
    } else if let Some(kind) = clicked_kind {
        let can_switch = if state.dimension_editor_active() {
            let named_values = &state.named_values;
            if let Some(session) = state.dimension_session.as_mut() {
                match session.accept(named_values) {
                    Ok(()) => true,
                    Err(error) => {
                        session.error = Some(error);
                        session.focus_next_frame = true;
                        false
                    }
                }
            } else {
                false
            }
        } else {
            true
        };
        if can_switch {
            state.sync_dimension_pending();
            if let Some(session) = state.dimension_session.as_mut() {
                session.begin_kind(kind);
            }
        }
    }

    if let Some(session) = &state.dimension_session {
        claims.confirmation_blocked = session.error.is_some();
        if let Some(error) = session.error
            && let Some(active) = session.active_kind()
            && let Some(layout) = layouts.iter().find(|layout| layout.readout.kind == active)
        {
            ui.painter().text(
                layout.rect.left_bottom() + Vec2::new(0.0, 3.0),
                Align2::LEFT_TOP,
                error.label(),
                FontId::monospace(9.0),
                sketch_colours().invalid,
            );
        }
    }
    // The workbench still observes these raw events through `dimension_keys`,
    // but removing their filtered counterparts prevents its later sketch-tool
    // shortcut pass from treating the same first Escape as "clear draft".
    if claims.enter {
        ui.input_mut(|input| {
            input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
        });
    }
    if claims.escape {
        ui.input_mut(|input| {
            input.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
        });
    }
    // The request lives for exactly the frame that armed it, whether or not
    // the armed box actually laid out.
    state.focus_dimension_box = None;
    DimensionWidgetOutcome {
        claims,
        pending_created,
        recipe_field,
        recipe_accepted,
    }
}

fn stage_complete_dimension_draft(state: &mut SketchCanvasState) -> Option<SketchEntityId> {
    let (phase, geometry) = state.dimension_session.as_ref().and_then(|session| {
        (session.target == DimensionTarget::Draft).then_some((session.phase, session.geometry))
    })?;
    if !phase.can_stage() {
        return None;
    }
    if geometry.is_degenerate() {
        if let Some(session) = state.dimension_session.as_mut() {
            session.error = Some(DimensionInputError::DegenerateGeometry);
            session.focus_next_frame = true;
        }
        return None;
    }
    if state.exact_tool == ToolVariant::ChainedPolyline {
        let SketchGeometry::Segment { end, .. } = geometry else {
            return None;
        };
        return state.accept_polyline_vertex(end);
    }
    let exact_recipe = match state.exact_tool {
        ToolVariant::CentrePointRectangle => centre_point_rectangle_recipe_from_geometry(geometry),
        ToolVariant::TwoPointCircle => {
            let (first, second) = two_point_circle_endpoints(geometry)?;
            Some(two_point_circle_recipe(first, second))
        }
        ToolVariant::InnerDiameterPolygon | ToolVariant::OuterDiameterPolygon => {
            let SketchGeometry::Circle { center, rim } = geometry else {
                return None;
            };
            regular_polygon_recipe(state.exact_tool, center, rim, state.polygon_sides)
        }
        ToolVariant::TwoPointSlot | ToolVariant::CentreToOuterPointSlot => {
            let (Some(axis_start), Some(axis_end)) = (state.creation_anchor, state.arc_start)
            else {
                return None;
            };
            let width = state
                .dimension_session
                .as_ref()?
                .value(SketchDimensionKind::Width);
            Some(match state.exact_tool {
                ToolVariant::TwoPointSlot => two_point_slot_recipe(axis_start, axis_end, width)?,
                ToolVariant::CentreToOuterPointSlot => {
                    centre_outer_point_slot_recipe(axis_start, axis_end, width)?
                }
                _ => unreachable!("the outer match limits slot variants"),
            })
        }
        ToolVariant::ThreePointArc => state
            .dimension_session
            .as_ref()
            .and_then(DimensionSession::three_point_arc_recipe),
        _ => None,
    };
    if let Some(recipe) = exact_recipe {
        let staged = state.stage_recipe(recipe, "Add sketch primitive").ok();
        if staged.is_some() {
            state.creation_anchor = None;
            state.arc_start = None;
        }
        return staged;
    }
    state.creation_anchor = None;
    state.arc_start = None;
    if state.tool == SketchTool::CentreLine {
        state
            .stage_geometry_with_role(geometry, SketchEntityRole::Construction)
            .ok()
    } else {
        state.stage_geometry(geometry).ok()
    }
}

fn raw_key_pressed(ui: &Ui, wanted: egui::Key, wanted_modifiers: egui::Modifiers) -> bool {
    ui.input(|input| {
        input.raw.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } if *key == wanted && *modifiers == wanted_modifiers
            )
        })
    })
}

fn select_all_dimension_text(ui: &Ui, id: Id, response: &Response, char_count: usize) {
    let mut state = egui::TextEdit::load_state(ui.ctx(), id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::default(),
            egui::text::CCursor::new(char_count),
        )));
    state.store(ui.ctx(), response.id);
}

fn format_dimension_readout(readout: DimensionReadout) -> String {
    let unit = if readout.kind.is_angle() { "deg" } else { "mm" };
    format!(
        "{} {:.2} {unit}",
        readout.kind.short_label(),
        normalized_zero(readout.value)
    )
}

fn dimension_unit_label(kind: SketchDimensionKind) -> &'static str {
    if kind.is_angle() {
        "degrees"
    } else {
        "millimetres"
    }
}

fn hit_test_entities(
    entities: &[SketchEntity],
    view: SketchView,
    rect: Rect,
    position: Pos2,
    radius: f32,
) -> Option<SketchEntityId> {
    let mut closest = None::<(f32, SketchEntityId)>;
    for entity in entities.iter().rev() {
        let distance = geometry_screen_distance(entity.geometry, view, rect, position);
        if distance <= radius && closest.is_none_or(|(best, _)| distance < best) {
            closest = Some((distance, entity.id));
        }
    }
    closest.map(|(_, id)| id)
}

fn geometry_screen_distance(
    geometry: SketchGeometry,
    view: SketchView,
    rect: Rect,
    position: Pos2,
) -> f32 {
    match geometry {
        SketchGeometry::Point(point) => view.sketch_to_screen(rect, point).distance(position),
        SketchGeometry::Segment { start, end } => point_segment_distance(
            position,
            view.sketch_to_screen(rect, start),
            view.sketch_to_screen(rect, end),
        ),
        SketchGeometry::Rectangle { .. } => {
            let corners = geometry
                .rectangle_corners()
                .expect("the rectangle branch always supplies corners")
                .map(|point| view.sketch_to_screen(rect, point));
            (0..4)
                .map(|index| {
                    point_segment_distance(position, corners[index], corners[(index + 1) % 4])
                })
                .fold(f32::INFINITY, f32::min)
        }
        SketchGeometry::Circle { center, rim } => {
            let center_screen = view.sketch_to_screen(rect, center);
            let radius = center_screen.distance(view.sketch_to_screen(rect, rim));
            (center_screen.distance(position) - radius).abs()
        }
        SketchGeometry::Arc { center, start, end } => {
            arc_screen_distance(position, rect, view, center, start, end)
        }
    }
}

fn semantic_selection_targets(
    ui: &mut Ui,
    canvas_rect: Rect,
    state: &SketchCanvasState,
) -> Option<SketchEntityId> {
    if !matches!(
        state.exact_tool,
        ToolVariant::Select | ToolVariant::Dimension
    ) {
        return None;
    }
    let mut selected = None;
    for entity in &state.entities {
        let Some((position, kind)) = semantic_target(entity.geometry, state.view, canvas_rect)
        else {
            continue;
        };
        let hit_rect = Rect::from_center_size(position, Vec2::splat(22.0)).intersect(canvas_rect);
        if !hit_rect.is_positive() || !hit_rect.is_finite() {
            continue;
        }
        let response = ui.interact(
            hit_rect,
            ui.id().with(("sketch-entity", entity.id.get())),
            Sense::click(),
        );
        let label = format!("Sketch {kind} {}", entity.id.get());
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &label));
        if response.clicked() {
            selected = Some(entity.id);
        }
    }
    selected
}

fn semantic_target(
    geometry: SketchGeometry,
    view: SketchView,
    canvas_rect: Rect,
) -> Option<(Pos2, &'static str)> {
    let (point, kind) = match geometry {
        SketchGeometry::Point(point) => (point, "point"),
        SketchGeometry::Segment { start, end } => (
            SketchPoint::new(
                start.u.mul_add(0.5, end.u * 0.5),
                start.v.mul_add(0.5, end.v * 0.5),
            ),
            "segment",
        ),
        geometry @ SketchGeometry::Rectangle { .. } => {
            let corners = geometry
                .rectangle_corners()
                .expect("the matched geometry is a rectangle");
            (
                SketchPoint::new(corners[0].u.mul_add(0.5, corners[1].u * 0.5), corners[0].v),
                "rectangle",
            )
        }
        SketchGeometry::Circle { rim, .. } => (rim, "circle"),
        SketchGeometry::Arc { center, start, end } => {
            let radius = center.distance_squared(start).sqrt();
            let start_angle = (start.v - center.v).atan2(start.u - center.u);
            let angle = arc_sweep(center, start, end).mul_add(0.5, start_angle);
            (
                SketchPoint::new(
                    radius.mul_add(angle.cos(), center.u),
                    radius.mul_add(angle.sin(), center.v),
                ),
                "arc",
            )
        }
    };
    let position = view.sketch_to_screen(canvas_rect, point);
    (point.is_finite() && position.is_finite()).then_some((position, kind))
}

fn paint_arc(
    painter: &egui::Painter,
    rect: Rect,
    view: SketchView,
    center: SketchPoint,
    start: SketchPoint,
    end: SketchPoint,
    stroke: Stroke,
) {
    let radius = center.distance_squared(start).sqrt();
    if !radius.is_finite() || radius <= MIN_ENTITY_LENGTH {
        return;
    }
    let start_angle = (start.v - center.v).atan2(start.u - center.u);
    let sweep = arc_sweep(center, start, end);
    let segment_count = ((sweep / std::f64::consts::TAU) * 64.0)
        .ceil()
        .clamp(2.0, 64.0) as usize;
    let mut previous = view.sketch_to_screen(rect, start);
    for index in 1..=segment_count {
        let parameter = index as f64 / segment_count as f64;
        let angle = sweep.mul_add(parameter, start_angle);
        let point = SketchPoint::new(
            radius.mul_add(angle.cos(), center.u),
            radius.mul_add(angle.sin(), center.v),
        );
        let current = view.sketch_to_screen(rect, point);
        painter.line_segment([previous, current], stroke);
        previous = current;
    }
}

fn arc_screen_distance(
    position: Pos2,
    rect: Rect,
    view: SketchView,
    center: SketchPoint,
    start: SketchPoint,
    end: SketchPoint,
) -> f32 {
    let center_screen = view.sketch_to_screen(rect, center);
    let radius = center_screen.distance(view.sketch_to_screen(rect, start));
    if radius <= f32::EPSILON {
        return center_screen.distance(position);
    }
    let point = view.screen_to_sketch(rect, position);
    let start_angle = (start.v - center.v).atan2(start.u - center.u);
    let point_angle = (point.v - center.v).atan2(point.u - center.u);
    let point_sweep = (point_angle - start_angle).rem_euclid(std::f64::consts::TAU);
    let sweep = arc_sweep(center, start, end);
    if point_sweep <= sweep {
        (center_screen.distance(position) - radius).abs()
    } else {
        let projected_end = arc_endpoint(center, start, end);
        view.sketch_to_screen(rect, start).distance(position).min(
            view.sketch_to_screen(rect, projected_end)
                .distance(position),
        )
    }
}

fn arc_endpoint(center: SketchPoint, start: SketchPoint, end: SketchPoint) -> SketchPoint {
    let radius_squared = center.distance_squared(start);
    let end_radius_squared = center.distance_squared(end);
    let radial_scale = radius_squared.abs().max(end_radius_squared.abs()).max(1.0);
    if (end_radius_squared - radius_squared).abs() <= 64.0 * f64::EPSILON * radial_scale {
        // Preserve an exact snapped endpoint when it already lies on the
        // construction circle. Reconstructing `sin(PI)` would otherwise turn
        // (−r, 0) into (−r, 2.4e−16), leaving an exact endpoint graph open.
        return SketchPoint::new(normalized_zero(end.u), normalized_zero(end.v));
    }
    let radius = radius_squared.sqrt();
    let end_angle = (end.v - center.v).atan2(end.u - center.u);
    SketchPoint::new(
        radius.mul_add(end_angle.cos(), center.u),
        radius.mul_add(end_angle.sin(), center.v),
    )
}

fn arc_sweep(center: SketchPoint, start: SketchPoint, end: SketchPoint) -> f64 {
    let start_angle = (start.v - center.v).atan2(start.u - center.u);
    let end_angle = (end.v - center.v).atan2(end.u - center.u);
    (end_angle - start_angle).rem_euclid(std::f64::consts::TAU)
}

fn point_segment_distance(point: Pos2, start: Pos2, end: Pos2) -> f32 {
    let segment = end - start;
    let length_squared = segment.length_sq();
    if length_squared <= f32::EPSILON {
        return point.distance(start);
    }
    let parameter = ((point - start).dot(segment) / length_squared).clamp(0.0, 1.0);
    point.distance(start + parameter * segment)
}

fn normalized_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::accesskit::Role;
    use egui_kittest::{Harness, kittest::Queryable as _};

    const EPSILON: f64 = 1.0e-10;

    #[test]
    fn the_picked_rectangle_side_names_its_dimension() {
        let geometry =
            SketchGeometry::rectangle(SketchPoint::new(-2.0, -1.0), SketchPoint::new(2.0, 1.0));
        // Horizontal sides are Width questions, vertical sides Height ones.
        for (pick, expected) in [
            (SketchPoint::new(0.3, 1.0), SketchDimensionKind::Width),
            (SketchPoint::new(-0.4, -1.0), SketchDimensionKind::Width),
            (SketchPoint::new(2.0, 0.2), SketchDimensionKind::Height),
            (SketchPoint::new(-2.0, -0.5), SketchDimensionKind::Height),
        ] {
            assert_eq!(
                rectangle_picked_dimension_kind(geometry, pick),
                Some(expected),
                "{pick:?}"
            );
        }
        // The picked side also carries the annotation.
        let top = rectangle_annotation_sides(geometry, Some(SketchPoint::new(0.0, 1.0)));
        assert!(top.width_on_top);
        let left = rectangle_annotation_sides(geometry, Some(SketchPoint::new(-2.0, 0.0)));
        assert!(left.height_on_left);
        let default = rectangle_annotation_sides(geometry, None);
        assert!(!default.width_on_top && !default.height_on_left);
    }

    #[test]
    fn committed_recipes_keep_their_annotations_while_the_dimension_tool_is_active() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_tool(SketchTool::Rectangle));
        state
            .stage_geometry(SketchGeometry::rectangle(
                SketchPoint::new(-2.0, -1.0),
                SketchPoint::new(2.0, 1.0),
            ))
            .expect("rectangle should stage");
        state.commit_pending().expect("rectangle should commit");
        // Committing auto-selects the new entity; the record annotations are
        // for everything the pick has moved on from.
        state.set_selected(None);
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));

        // Outside the Dimension tool the canvas stays clean.
        assert!(committed_dimension_annotation_layouts(&state, canvas).is_empty());

        assert!(state.set_exact_tool(ToolVariant::Dimension));
        let layouts = committed_dimension_annotation_layouts(&state, canvas);
        assert_eq!(
            layouts.len(),
            2,
            "an unselected committed rectangle wears its width and height"
        );
        for layout in &layouts {
            assert!(
                layout.span.is_some(),
                "{:?} carries witness lines and arrows",
                layout.readout.kind
            );
            assert!(!layout.readout.editable);
        }
        let width = layouts
            .iter()
            .find(|layout| layout.readout.kind == SketchDimensionKind::Width)
            .expect("width annotation");
        assert!((width.readout.value - 4.0).abs() <= EPSILON);

        // Selecting the rectangle hands it to the interactive boxes instead
        // of double-drawing it.
        let subject = state.entities[0].id;
        assert!(state.set_selected(Some(subject)));
        assert!(committed_dimension_annotation_layouts(&state, canvas).is_empty());
    }

    #[test]
    fn rectangle_spans_dress_the_picked_side() {
        let geometry =
            SketchGeometry::rectangle(SketchPoint::new(-2.0, -1.0), SketchPoint::new(2.0, 1.0));
        let view = SketchView::default();
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));
        let top = rectangle_annotation_sides(geometry, Some(SketchPoint::new(0.0, 1.0)));
        let span = dimension_span(geometry, SketchDimensionKind::Width, top, view, canvas)
            .expect("a rectangle width is a span");
        let top_screen = view.sketch_to_screen(canvas, SketchPoint::new(0.0, 1.0));
        assert!(
            (span.0.y - top_screen.y).abs() <= 0.5 && (span.1.y - top_screen.y).abs() <= 0.5,
            "the span runs along the picked top side: {span:?}"
        );
    }

    #[test]
    fn arc_endpoint_preserves_an_exact_snapped_antipode() {
        let center = SketchPoint::new(0.0, 0.0);
        let start = SketchPoint::new(2.0, 0.0);
        let end = SketchPoint::new(-2.0, 0.0);

        assert_eq!(arc_endpoint(center, start, end), end);
    }

    fn assert_point_near(actual: SketchPoint, expected: SketchPoint) {
        assert!((actual.u - expected.u).abs() <= EPSILON);
        assert!((actual.v - expected.v).abs() <= EPSILON);
    }

    #[test]
    fn coordinate_mapping_round_trips_with_pan_and_zoom() {
        let rect = Rect::from_min_size(Pos2::new(40.0, 20.0), Vec2::new(800.0, 500.0));
        let view = SketchView {
            center: SketchPoint::new(2.5, -1.25),
            points_per_unit: 80.0,
            quarter_turns: 0,
        };
        let point = SketchPoint::new(-3.75, 4.5);

        assert_point_near(
            view.screen_to_sketch(rect, view.sketch_to_screen(rect, point)),
            point,
        );
    }

    #[test]
    fn quarter_turned_sketch_view_rotates_only_presentation_and_round_trips() {
        let rect = Rect::from_min_size(Pos2::new(20.0, 30.0), Vec2::new(600.0, 400.0));
        let point = SketchPoint::new(3.0, 1.0);
        let expected_u_directions = [
            Vec2::new(50.0, 0.0),
            Vec2::new(0.0, -50.0),
            Vec2::new(-50.0, 0.0),
            Vec2::new(0.0, 50.0),
        ];
        for (quarter_turns, expected) in expected_u_directions.into_iter().enumerate() {
            let mut view = SketchView {
                center: SketchPoint::new(3.0, 1.0),
                points_per_unit: 50.0,
                quarter_turns: 0,
            };
            view.set_quarter_turns(quarter_turns as u8);
            let origin = view.sketch_to_screen(rect, point);
            let u = view.sketch_to_screen(rect, SketchPoint::new(point.u + 1.0, point.v));
            assert!((u - origin - expected).length() <= f32::EPSILON);

            let sample = SketchPoint::new(-2.75, 4.125);
            assert_point_near(
                view.screen_to_sketch(rect, view.sketch_to_screen(rect, sample)),
                sample,
            );
        }
    }

    #[test]
    fn every_sketch_geometry_exports_a_bounded_renderer_neutral_outline() {
        let point = SketchGeometry::point(SketchPoint::new(1.0, 2.0))
            .display_polyline()
            .unwrap();
        assert_eq!(point.points, vec![SketchPoint::new(1.0, 2.0)]);
        assert!(!point.closed);
        assert_eq!(point.segments().count(), 0);

        let segment =
            SketchGeometry::segment(SketchPoint::new(-1.0, 0.0), SketchPoint::new(2.0, 3.0))
                .display_polyline()
                .unwrap();
        assert_eq!(segment.points.len(), 2);
        assert!(!segment.closed);
        assert_eq!(segment.segments().count(), 1);

        let rectangle =
            SketchGeometry::rectangle(SketchPoint::new(2.0, 3.0), SketchPoint::new(-2.0, -1.0))
                .display_polyline()
                .unwrap();
        assert_eq!(
            rectangle.points,
            vec![
                SketchPoint::new(-2.0, -1.0),
                SketchPoint::new(2.0, -1.0),
                SketchPoint::new(2.0, 3.0),
                SketchPoint::new(-2.0, 3.0),
            ]
        );
        assert!(rectangle.closed);
        assert_eq!(rectangle.segments().count(), 4);

        let center = SketchPoint::new(4.0, -2.0);
        let rim = SketchPoint::new(4.0, 1.0);
        let circle = SketchGeometry::circle(center, rim)
            .display_polyline()
            .unwrap();
        assert_eq!(circle.points.len(), MAX_DISPLAY_CURVE_SEGMENTS);
        assert_point_near(circle.points[0], rim);
        assert!(circle.closed);
        assert_eq!(circle.segments().count(), MAX_DISPLAY_CURVE_SEGMENTS);

        let start = SketchPoint::new(6.0, -2.0);
        let end = SketchPoint::new(4.0, 0.0);
        let arc = SketchGeometry::arc(center, start, end)
            .display_polyline()
            .unwrap();
        assert!((3..=MAX_DISPLAY_CURVE_SEGMENTS + 1).contains(&arc.points.len()));
        assert_point_near(arc.points[0], start);
        assert_point_near(
            *arc.points.last().unwrap(),
            arc_endpoint(center, start, end),
        );
        assert!(!arc.closed);
        assert_eq!(arc.segments().count(), arc.points.len() - 1);
    }

    #[test]
    fn display_outline_rejects_degenerate_and_non_finite_geometry() {
        assert!(
            SketchGeometry::circle(SketchPoint::default(), SketchPoint::default())
                .display_polyline()
                .is_none()
        );
        assert!(
            SketchGeometry::segment(SketchPoint::default(), SketchPoint::new(f64::NAN, 1.0),)
                .display_polyline()
                .is_none()
        );
    }

    fn context_fixture<'a>(
        triangles: &'a [SketchContextTriangle],
        edges: &'a [SketchContextEdge],
        boundary: &'a [SketchPoint],
        key: u64,
    ) -> SketchViewportContext<'a> {
        SketchViewportContext::new(triangles, edges)
            .with_selected_face(boundary, SketchContextFitKey::new([key as u8; 32], key))
    }

    #[test]
    fn projected_context_fit_contains_the_body_while_centering_the_selected_face() {
        let triangles = [SketchContextTriangle::new([
            SketchPoint::new(-10.0, -4.0),
            SketchPoint::new(12.0, -4.0),
            SketchPoint::new(12.0, 6.0),
        ])];
        let edges = [SketchContextEdge::new([
            SketchPoint::new(-10.0, 6.0),
            SketchPoint::new(12.0, 6.0),
        ])];
        let boundary = [
            SketchPoint::new(1.0, -1.0),
            SketchPoint::new(3.0, -1.0),
            SketchPoint::new(3.0, 1.0),
            SketchPoint::new(1.0, 1.0),
        ];
        let context = context_fixture(&triangles, &edges, &boundary, 7);
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1_000.0, 600.0));
        let mut view = SketchView::default();

        assert!(fit_context_view(&mut view, rect, &context));
        assert_point_near(view.center, SketchPoint::new(2.0, 0.0));
        let padded = rect.shrink(CONTEXT_FIT_PADDING_POINTS - 0.5);
        for point in context_points(&context) {
            let screen = view.sketch_to_screen(rect, point);
            assert!(
                padded.contains(screen),
                "projected body point {point:?} escaped auto-fit at {screen:?}"
            );
        }
    }

    #[test]
    fn context_auto_fit_runs_once_per_key_and_can_be_requested_again() {
        let triangles = [SketchContextTriangle::new([
            SketchPoint::new(-4.0, -2.0),
            SketchPoint::new(4.0, -2.0),
            SketchPoint::new(4.0, 2.0),
        ])];
        let boundary = [
            SketchPoint::new(-1.0, -1.0),
            SketchPoint::new(1.0, -1.0),
            SketchPoint::new(1.0, 1.0),
            SketchPoint::new(-1.0, 1.0),
        ];
        let context = context_fixture(&triangles, &[], &boundary, 91);
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0));
        let mut state = SketchCanvasState::default();

        assert!(auto_fit_context_if_needed(&mut state, rect, &context));
        let fitted = state.view;
        state.view.pan_by_screen_delta(Vec2::new(40.0, -20.0));
        let user_view = state.view;
        assert!(!auto_fit_context_if_needed(&mut state, rect, &context));
        assert_eq!(
            state.view, user_view,
            "a stable key must preserve user navigation"
        );

        state.request_context_fit();
        assert!(auto_fit_context_if_needed(&mut state, rect, &context));
        assert_eq!(state.view, fitted);
    }

    #[test]
    fn projected_context_mesh_batches_valid_triangles_and_skips_invalid_input() {
        let triangles = [
            SketchContextTriangle::new([
                SketchPoint::new(-1.0, -1.0),
                SketchPoint::new(1.0, -1.0),
                SketchPoint::new(0.0, 1.0),
            ]),
            SketchContextTriangle::new([
                SketchPoint::new(f64::NAN, 0.0),
                SketchPoint::new(1.0, 0.0),
                SketchPoint::new(0.0, 1.0),
            ]),
        ];
        let mesh = projected_context_mesh(
            Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0)),
            SketchView::default(),
            &triangles,
        );

        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.indices, vec![0, 1, 2]);
        assert!(
            mesh.vertices
                .iter()
                .all(|vertex| vertex.color == sketch_colours().context_face)
        );
        assert_eq!(
            sketch_colours().context_face.a(),
            255,
            "a committed body must remain an opaque solid in face-sketch mode"
        );
    }

    #[test]
    fn absent_projected_context_preserves_the_default_sketch_view() {
        let mut harness = Harness::builder()
            .with_size(Vec2::new(600.0, 400.0))
            .build_ui_state(
                |ui, state| {
                    let _ = show_with_context(ui, state, None);
                },
                SketchCanvasState::default(),
            );
        harness.run();

        assert_eq!(harness.state().view(), SketchView::default());
    }

    #[test]
    fn zoom_preserves_coordinate_beneath_pointer() {
        let rect = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(640.0, 480.0));
        let pointer = Pos2::new(513.0, 117.0);
        let mut view = SketchView::default();
        let before = view.screen_to_sketch(rect, pointer);

        view.zoom_about(rect, pointer, 1.7);

        assert_point_near(view.screen_to_sketch(rect, pointer), before);
    }

    #[test]
    fn pan_tracks_screen_delta_in_plane_coordinates() {
        let mut view = SketchView {
            center: SketchPoint::new(0.0, 0.0),
            points_per_unit: 50.0,
            quarter_turns: 0,
        };

        view.pan_by_screen_delta(Vec2::new(100.0, -25.0));

        assert_point_near(view.center, SketchPoint::new(-2.0, -0.5));
    }

    #[test]
    fn grid_snapping_follows_the_grid_the_camera_actually_draws() {
        let snap = SnapSettings {
            enabled: true,
            grid_step: 0.25,
            endpoint_radius_points: 10.0,
        };
        // At the default camera the lattice must survive untouched: a user
        // placing a point on a 0.25 gridline expects it to stay there.
        assert_point_near(
            snap.snap_to_visible_grid(SketchPoint::new(1.31, -0.62), DEFAULT_POINTS_PER_UNIT),
            SketchPoint::new(1.25, -0.5),
        );
        assert_point_near(
            snap.snap_to_visible_grid(SketchPoint::new(-1.75, 3.0), DEFAULT_POINTS_PER_UNIT),
            SketchPoint::new(-1.75, 3.0),
        );

        // Zoomed in further it stays the lattice.
        let close = 200.0;
        assert_point_near(
            snap.snap_to_visible_grid(SketchPoint::new(1.31, -0.62), close),
            SketchPoint::new(1.25, -0.5),
        );

        // Pulled back until the lattice is far below a pixel, snapping must
        // coarsen with the drawn grid instead of quietly becoming a no-op.
        let far = 1.0;
        let raw = SketchPoint::new(63.4, -128.9);
        let step = resolvable_grid_spacing(far, snap.grid_step, TARGET_SNAP_SPACING_POINTS)
            .expect("a reachable grid exists")
            .minor_world_step();
        assert!(step * far >= TARGET_SNAP_SPACING_POINTS);
        assert!(step > snap.grid_step, "the visible step must coarsen");
        let snapped = snap.snap_to_visible_grid(raw, far);
        for value in [snapped.u, snapped.v] {
            let multiple = value / step;
            assert!(
                (multiple - multiple.round()).abs() <= 1.0e-9,
                "{value} is not a multiple of the visible step {step}"
            );
        }
        assert!(
            (snapped.u - raw.u).abs() <= step && (snapped.v - raw.v).abs() <= step,
            "snapping must stay within one visible cell of the pointer"
        );
    }

    #[test]
    fn grid_snapping_is_symmetric_and_normalizes_zero() {
        let snap = SnapSettings {
            grid_step: 0.25,
            ..SnapSettings::default()
        };

        assert_point_near(
            snap.snap_to_grid(SketchPoint::new(0.37, -0.38)),
            SketchPoint::new(0.25, -0.5),
        );
        let zero = snap.snap_to_grid(SketchPoint::new(-0.01, 0.01));
        assert!(zero.u.is_sign_positive());
        assert!(zero.v.is_sign_positive());
    }

    #[test]
    fn endpoint_snap_takes_precedence_over_grid() {
        let mut state = SketchCanvasState::default();
        let id = state
            .stage_geometry(SketchGeometry::point(SketchPoint::new(0.13, 0.13)))
            .expect("point should stage");
        state.commit_pending().expect("point should commit");
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0));
        let screen = state
            .view
            .sketch_to_screen(rect, SketchPoint::new(0.14, 0.14));

        let snapped = state.snap_point(rect, screen);

        assert_eq!(snapped.kind, SnapKind::Endpoint(id));
        assert_point_near(snapped.point, SketchPoint::new(0.13, 0.13));
    }

    #[test]
    fn provisional_polyline_start_is_an_identity_neutral_close_snap() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::ChainedPolyline));
        let authoring_before = state.authoring.clone();
        for point in [
            SketchPoint::new(-2.0, -1.0),
            SketchPoint::new(2.0, -1.0),
            SketchPoint::new(2.0, 2.0),
        ] {
            state.handle_creation_click(point);
        }
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0));
        let near_start = state.view.sketch_to_screen(
            rect,
            SketchPoint::new(-2.0 + 2.0 / state.view.points_per_unit, -1.0),
        );

        let snapped = state.snap_point(rect, near_start);

        assert_eq!(snapped.kind, SnapKind::None);
        assert_eq!(snapped.point, SketchPoint::new(-2.0, -1.0));
        assert_eq!(state.next_entity_id, 1);
        assert_eq!(state.authoring, authoring_before);
    }

    #[test]
    fn analytic_intersection_snap_takes_precedence_over_midpoints_and_grid() {
        let mut state = SketchCanvasState::default();
        for geometry in [
            SketchGeometry::segment(SketchPoint::new(-2.0, 0.0), SketchPoint::new(2.0, 0.0)),
            SketchGeometry::segment(SketchPoint::new(0.0, -2.0), SketchPoint::new(0.0, 2.0)),
        ] {
            state.stage_geometry(geometry).expect("line should stage");
            state.commit_pending().expect("line should commit");
        }
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0));
        let screen = state
            .view
            .sketch_to_screen(rect, SketchPoint::new(0.03, -0.02));

        let snapped = state.snap_point(rect, screen);

        assert!(matches!(snapped.kind, SnapKind::Intersection(_, _)));
        assert_point_near(snapped.point, SketchPoint::new(0.0, 0.0));
    }

    #[test]
    fn circle_center_and_quadrant_have_distinct_semantic_snaps() {
        let mut state = SketchCanvasState::default();
        state
            .stage_geometry(SketchGeometry::circle(
                SketchPoint::new(1.0, -1.0),
                SketchPoint::new(3.0, -1.0),
            ))
            .expect("circle should stage");
        state.commit_pending().expect("circle should commit");
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0));

        let center = state.snap_point(
            rect,
            state
                .view
                .sketch_to_screen(rect, SketchPoint::new(1.01, -1.01)),
        );
        assert!(matches!(center.kind, SnapKind::Center(_)));
        assert_point_near(center.point, SketchPoint::new(1.0, -1.0));

        let quadrant = state.snap_point(
            rect,
            state
                .view
                .sketch_to_screen(rect, SketchPoint::new(3.01, -1.0)),
        );
        assert!(matches!(quadrant.kind, SnapKind::Quadrant(_, 0)));
        assert_point_near(quadrant.point, SketchPoint::new(3.0, -1.0));
    }

    #[test]
    fn a_stroke_ending_beside_an_edge_snaps_onto_the_edge_itself() {
        let mut state = SketchCanvasState::default();
        state
            .stage_geometry(SketchGeometry::rectangle(
                SketchPoint::new(-2.0, -2.0),
                SketchPoint::new(2.0, 2.0),
            ))
            .expect("rectangle should stage");
        state.commit_pending().expect("rectangle should commit");
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0));

        // A hair inside the top edge, far from every named point: the
        // pointer lands exactly on the edge so the line forms a T-junction.
        let near_edge = state.snap_point(
            rect,
            state
                .view
                .sketch_to_screen(rect, SketchPoint::new(0.3, 1.98)),
        );
        assert!(matches!(near_edge.kind, SnapKind::OnCurve(_)));
        // The pointer round-trips through screen space in single precision,
        // so only the coordinate the snap decides is exact.
        assert!((near_edge.point.v - 2.0).abs() <= EPSILON);
        assert!((near_edge.point.u - 0.3).abs() <= 1.0e-4);

        // Well clear of the edge the grid still wins.
        let clear = state.snap_point(
            rect,
            state
                .view
                .sketch_to_screen(rect, SketchPoint::new(0.3, 1.0)),
        );
        assert_eq!(clear.kind, SnapKind::Grid);
    }

    /// A unit circle's support curve, split into two half turns the way a
    /// drilled hole's loop arrives from the kernel.
    fn support_hole(center: SketchPoint, radius: f64) -> Vec<SketchContextCurve> {
        [
            (0.0, std::f64::consts::PI),
            (std::f64::consts::PI, std::f64::consts::TAU),
        ]
        .into_iter()
        .map(|(start, end)| SketchContextCurve::Arc {
            center,
            u: [1.0, 0.0],
            v: [0.0, 1.0],
            radius,
            start,
            end,
        })
        .collect()
    }

    #[test]
    fn support_edge_offers_endpoint_midpoint_and_on_edge_snaps() {
        let mut state = SketchCanvasState::default();
        state.set_support_curves(&[SketchContextCurve::segment(
            SketchPoint::new(-2.0, 1.0),
            SketchPoint::new(2.0, 1.0),
        )]);
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0));
        let at = |point| state.snap_point(rect, state.view.sketch_to_screen(rect, point));

        let midpoint = at(SketchPoint::new(0.02, 1.02));
        assert_eq!(midpoint.kind, SnapKind::SupportMidpoint);
        assert_point_near(midpoint.point, SketchPoint::new(0.0, 1.0));

        let endpoint = at(SketchPoint::new(1.98, 1.01));
        assert_eq!(endpoint.kind, SnapKind::SupportEndpoint);
        assert_point_near(endpoint.point, SketchPoint::new(2.0, 1.0));

        // Between the named points the pointer still lands on the edge itself,
        // keeping only the coordinate it did not aim at.
        let on_edge = at(SketchPoint::new(1.0, 1.02));
        assert_eq!(on_edge.kind, SnapKind::SupportEdge);
        assert_point_near(on_edge.point, SketchPoint::new(1.0, 1.0));

        // Well clear of the edge, ordinary grid snapping resumes.
        let free = at(SketchPoint::new(1.0, 3.02));
        assert_eq!(free.kind, SnapKind::Grid);
    }

    #[test]
    fn support_hole_publishes_its_analytic_centre_and_quadrants() {
        let mut state = SketchCanvasState::default();
        let center = SketchPoint::new(1.0, -1.0);
        state.set_support_curves(&support_hole(center, 0.75));
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0));
        let at = |point| state.snap_point(rect, state.view.sketch_to_screen(rect, point));

        let centre = at(SketchPoint::new(1.01, -1.02));
        assert_eq!(centre.kind, SnapKind::SupportCenter);
        assert_point_near(centre.point, center);

        // The split into two half turns is the kernel's seam, not the user's
        // model. All four quadrants are reachable and named as quadrants even
        // though two of them are also arc endpoints and two are arc midpoints.
        for expected in [
            SketchPoint::new(1.75, -1.0),
            SketchPoint::new(1.0, -0.25),
            SketchPoint::new(0.25, -1.0),
            SketchPoint::new(1.0, -1.75),
        ] {
            let snapped = at(SketchPoint::new(expected.u + 0.01, expected.v + 0.01));
            assert_eq!(snapped.kind, SnapKind::SupportQuadrant);
            assert_point_near(snapped.point, expected);
        }
    }

    #[test]
    fn an_arc_offers_only_the_quadrants_its_own_span_reaches() {
        let mut state = SketchCanvasState::default();
        // The upper half turn alone: the two side quadrants are its endpoints,
        // and the bottom quadrant is not on this curve at all.
        state.set_support_curves(&[SketchContextCurve::Arc {
            center: SketchPoint::default(),
            u: [1.0, 0.0],
            v: [0.0, 1.0],
            radius: 2.0,
            start: 0.0,
            end: std::f64::consts::PI,
        }]);
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0));
        let at = |point| state.snap_point(rect, state.view.sketch_to_screen(rect, point));

        let top = at(SketchPoint::new(0.01, 1.99));
        assert_eq!(top.kind, SnapKind::SupportQuadrant);
        assert_point_near(top.point, SketchPoint::new(0.0, 2.0));

        let bottom = at(SketchPoint::new(0.01, -1.99));
        assert_eq!(
            bottom.kind,
            SnapKind::Grid,
            "a half arc must not advertise the quadrant on its missing half"
        );
    }

    #[test]
    fn authored_geometry_outranks_the_support_it_is_drawn_on() {
        let mut state = SketchCanvasState::default();
        state.set_support_curves(&[SketchContextCurve::segment(
            SketchPoint::new(-2.0, 0.0),
            SketchPoint::new(2.0, 0.0),
        )]);
        state
            .stage_geometry(SketchGeometry::segment(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(0.0, 2.0),
            ))
            .expect("line should stage");
        state.commit_pending().expect("line should commit");
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0));

        // The sketch line's endpoint and the support edge's midpoint coincide.
        // The authored entity must own the snap so the report names it.
        let snapped = state.snap_point(
            rect,
            state
                .view
                .sketch_to_screen(rect, SketchPoint::new(0.01, 0.01)),
        );
        assert!(
            matches!(snapped.kind, SnapKind::Endpoint(_)),
            "expected an authored endpoint, received {:?}",
            snapped.kind
        );
        assert_point_near(snapped.point, SketchPoint::new(0.0, 0.0));
    }

    #[test]
    fn the_point_tool_places_an_entity_on_a_snapped_support_centre() {
        let mut state = SketchCanvasState::default();
        let center = SketchPoint::new(1.0, -1.0);
        state.set_support_curves(&support_hole(center, 0.75));
        assert!(state.set_tool(SketchTool::Point));
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0));

        let snapped = state.snap_point(
            rect,
            state
                .view
                .sketch_to_screen(rect, SketchPoint::new(1.02, -1.01)),
        );
        assert_eq!(snapped.kind, SnapKind::SupportCenter);
        let id = state
            .handle_creation_click(snapped.point)
            .expect("one click stages a point");

        assert_eq!(state.commit_pending(), Ok(id));
        let entity = state
            .entities()
            .iter()
            .find(|entity| entity.id == id)
            .expect("the committed point");
        let SketchGeometry::Point(position) = entity.geometry else {
            panic!(
                "the point tool authors a point, received {:?}",
                entity.geometry
            );
        };
        assert_point_near(position, center);
    }

    #[test]
    fn disabled_snapping_ignores_support_curves_entirely() {
        let mut state = SketchCanvasState::default();
        state.set_support_curves(&support_hole(SketchPoint::default(), 1.0));
        let mut settings = state.snap_settings();
        settings.enabled = false;
        state.set_snap_settings(settings);
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0));
        let raw = SketchPoint::new(0.01, 0.01);

        let snapped = state.snap_point(rect, state.view.sketch_to_screen(rect, raw));

        assert_eq!(snapped.kind, SnapKind::None);
        // The pointer keeps its own position: only the screen round trip's
        // single-precision step separates it from the requested coordinate.
        assert!(snapped.point.distance_squared(raw) < 1.0e-8);
        assert!(snapped.point.distance_squared(SketchPoint::default()) > 1.0e-6);
    }

    #[test]
    fn non_finite_support_curves_are_refused_rather_than_snapped_to() {
        let mut state = SketchCanvasState::default();
        state.set_support_curves(&[
            SketchContextCurve::segment(
                SketchPoint::new(f64::NAN, 0.0),
                SketchPoint::new(2.0, 0.0),
            ),
            SketchContextCurve::Arc {
                center: SketchPoint::default(),
                u: [1.0, 0.0],
                v: [0.0, 1.0],
                radius: 0.0,
                start: 0.0,
                end: std::f64::consts::TAU,
            },
        ]);

        assert!(state.support_curves().is_empty());
    }

    #[test]
    fn default_line_tool_finishes_then_commits_without_skipping_confirmation() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_tool(SketchTool::Line));

        assert_eq!(
            state.handle_creation_click(SketchPoint::new(0.0, 0.0)),
            None
        );
        assert_eq!(
            state.handle_creation_click(SketchPoint::new(2.0, 1.0)),
            None,
            "polyline vertices stay local until the gesture is finished"
        );

        assert!(state.entities().is_empty());
        assert!(!state.has_pending_edit());
        let id = state
            .finish_polyline_draft()
            .expect("finishing should stage the atomic line chain");
        assert!(state.has_pending_edit());
        assert_eq!(state.commit_pending(), Ok(id));
        assert_eq!(state.entities().len(), 1);
        assert_eq!(state.creation_anchor(), None);
    }

    #[test]
    fn rejected_degenerate_edit_remains_pending_until_cancelled() {
        let mut state = SketchCanvasState::default();
        let point = SketchPoint::new(1.0, 1.0);
        state
            .stage_geometry(SketchGeometry::segment(point, point))
            .expect("finite line should stage");

        assert_eq!(
            state.commit_pending(),
            Err(SketchEditError::DegenerateGeometry)
        );
        assert!(state.entities().is_empty());
        assert!(state.has_pending_edit());
        assert!(state.cancel_pending().is_some());
        assert!(!state.has_pending_edit());
    }

    #[test]
    fn staging_rejects_finite_geometry_outside_the_supported_coordinate_envelope() {
        let mut state = SketchCanvasState::default();
        let outside = MAX_ABS_SKETCH_COORDINATE + 1.0;

        assert_eq!(
            state.stage_geometry(SketchGeometry::rectangle(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(outside, 1.0),
            )),
            Err(SketchEditError::NonFiniteGeometry)
        );
        assert_eq!(
            state.stage_geometry(SketchGeometry::segment(
                SketchPoint::new(-f64::MAX, 0.0),
                SketchPoint::new(f64::MAX, 0.0),
            )),
            Err(SketchEditError::NonFiniteGeometry)
        );
        assert!(!state.has_pending_edit());
        assert!(state.dimension_session.is_none());
    }

    #[test]
    fn pending_edit_blocks_plane_and_tool_changes() {
        let mut state = SketchCanvasState::default();
        state
            .stage_geometry(SketchGeometry::point(SketchPoint::new(1.0, 2.0)))
            .expect("point should stage");

        assert!(!state.set_plane(SketchPlane::YZ));
        assert!(!state.set_tool(SketchTool::Rectangle));
        assert_eq!(state.plane(), SketchPlane::XY);
        assert_eq!(state.tool(), SketchTool::Select);
    }

    #[test]
    fn committed_geometry_locks_the_sketch_plane() {
        let mut state = SketchCanvasState::default();
        state
            .stage_geometry(SketchGeometry::point(SketchPoint::new(1.0, 2.0)))
            .expect("point should stage");
        state.commit_pending().expect("point should commit");

        assert!(!state.set_plane(SketchPlane::YZ));
        assert_eq!(state.plane(), SketchPlane::XY);
        assert!(state.set_plane(SketchPlane::XY));
    }

    #[test]
    fn rectangle_diagnostics_are_conservative_profile_candidates() {
        let mut state = SketchCanvasState::default();
        state
            .stage_geometry(SketchGeometry::rectangle(
                SketchPoint::new(3.0, 4.0),
                SketchPoint::new(-1.0, -2.0),
            ))
            .expect("rectangle should stage");

        let pending = state.diagnostics();
        assert_eq!(pending.closed_rectangles, 1);
        assert_eq!(pending.pending_entities, 1);
        assert!(pending.has_closed_profile_candidate());
        assert_eq!(state.certified_polyline_vertices(), None);

        state.commit_pending().expect("rectangle should commit");
        let committed = state.diagnostics();
        assert_eq!(committed.closed_rectangles, 1);
        assert_eq!(committed.pending_entities, 0);
        assert_eq!(
            state.certified_polyline_vertices(),
            Some(vec![
                SketchPoint::new(-1.0, -2.0),
                SketchPoint::new(3.0, -2.0),
                SketchPoint::new(3.0, 4.0),
                SketchPoint::new(-1.0, 4.0),
            ])
        );
    }

    #[test]
    fn circle_tool_stages_on_radius_click_and_reports_closed_candidate() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_tool(SketchTool::Circle));

        assert_eq!(
            state.handle_creation_click(SketchPoint::new(1.0, 2.0)),
            None
        );
        let id = state
            .handle_creation_click(SketchPoint::new(4.0, 2.0))
            .expect("radius click should stage a circle");

        assert_eq!(
            state.pending().map(PendingSketchEdit::label),
            Some("Add sketch circle")
        );
        assert_eq!(
            state.diagnostics().status(),
            LocalProfileStatus::ClosedCandidate
        );
        assert_eq!(state.commit_pending(), Ok(id));
        assert!(matches!(
            state.entities()[0].geometry,
            SketchGeometry::Circle { .. }
        ));
        assert_eq!(state.certified_polyline_vertices(), None);
    }

    #[test]
    fn arc_tool_uses_center_start_end_and_defers_profile_certification() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_tool(SketchTool::Arc));
        let center = SketchPoint::new(0.0, 0.0);
        let start = SketchPoint::new(2.0, 0.0);
        let end = SketchPoint::new(0.0, 3.0);

        assert_eq!(state.handle_creation_click(center), None);
        assert_eq!(state.handle_creation_click(start), None);
        let id = state
            .handle_creation_click(end)
            .expect("third click should stage an arc");

        assert_eq!(
            state.pending().map(PendingSketchEdit::label),
            Some("Add sketch arc")
        );
        assert_eq!(state.diagnostics().status(), LocalProfileStatus::Open);
        let stored_end = match state
            .pending()
            .expect("arc remains pending")
            .entity()
            .expect("arc transaction inserts geometry")
            .geometry
        {
            SketchGeometry::Arc { end, .. } => end,
            geometry => panic!("expected a pending arc, got {geometry:?}"),
        };
        assert_point_near(stored_end, SketchPoint::new(0.0, 2.0));
        assert_eq!(state.commit_pending(), Ok(id));

        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(500.0));
        let raw_click = state.view.sketch_to_screen(rect, end);
        assert_eq!(state.snap_point(rect, raw_click).kind, SnapKind::Grid);
        let canonical_end = state
            .view
            .sketch_to_screen(rect, SketchPoint::new(0.0, 2.0));
        assert_eq!(
            state.snap_point(rect, canonical_end).kind,
            SnapKind::Endpoint(id)
        );
    }

    #[test]
    fn arc_tool_preserves_non_axis_snapped_endpoints_on_the_construction_circle() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_tool(SketchTool::Arc));
        let center = SketchPoint::new(0.0, 0.0);
        let start = SketchPoint::new(3.0, 4.0);
        let end = SketchPoint::new(-3.0, -4.0);

        assert_eq!(state.handle_creation_click(center), None);
        assert_eq!(state.handle_creation_click(start), None);
        state
            .handle_creation_click(end)
            .expect("third click should stage an exact semicircle");

        let SketchGeometry::Arc {
            center: stored_center,
            start: stored_start,
            end: stored_end,
        } = state
            .pending()
            .expect("pending arc")
            .entity()
            .expect("arc transaction inserts geometry")
            .geometry
        else {
            panic!("expected an arc");
        };
        assert_eq!(stored_center, center);
        assert_eq!(stored_start, start);
        assert_eq!(stored_end, end);
    }

    #[test]
    fn zero_sweep_arc_is_rejected_and_retained() {
        let mut state = SketchCanvasState::default();
        state
            .stage_geometry(SketchGeometry::arc(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(1.0, 0.0),
                SketchPoint::new(2.0, 0.0),
            ))
            .expect("finite arc should stage");

        assert_eq!(
            state.commit_pending(),
            Err(SketchEditError::DegenerateGeometry)
        );
        assert!(state.has_pending_edit());
        assert_eq!(state.diagnostics().status(), LocalProfileStatus::Degenerate);
    }

    #[test]
    fn construction_geometry_is_committed_and_selectable_but_never_forms_a_profile() {
        let mut state = SketchCanvasState::default();
        let id = state
            .stage_geometry_with_role(
                SketchGeometry::segment(SketchPoint::new(-2.0, 0.0), SketchPoint::new(2.0, 0.0)),
                SketchEntityRole::Construction,
            )
            .expect("construction line should stage");
        assert_eq!(state.commit_pending(), Ok(id));
        assert_eq!(state.selected(), Some(id));
        assert_eq!(state.entities()[0].role, SketchEntityRole::Construction);
        assert_eq!(
            state.certified_profile_status(),
            CertifiedProfileStatus::Empty
        );
        assert!(state.certified_sketch_profile().is_none());
    }

    #[test]
    fn centreline_tool_stages_one_construction_curve_behind_confirmation() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_tool(SketchTool::CentreLine));
        assert_eq!(
            state.handle_creation_click(SketchPoint::new(-1.0, 0.0)),
            None
        );
        let id = state
            .handle_creation_click(SketchPoint::new(1.0, 0.0))
            .expect("second click should stage the centreline");
        assert!(state.entities().is_empty());
        assert_eq!(
            state
                .pending()
                .expect("pending centreline")
                .entity()
                .expect("centreline transaction inserts geometry")
                .role,
            SketchEntityRole::Construction
        );
        assert_eq!(state.commit_pending(), Ok(id));
        assert_eq!(
            state.certified_profile_status(),
            CertifiedProfileStatus::Empty
        );
    }

    #[test]
    fn centre_point_rectangle_uses_full_live_dimensions_and_one_atomic_recipe() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::CentrePointRectangle));
        let center = SketchPoint::new(1.0, -1.0);
        let corner = SketchPoint::new(3.0, 0.5);

        assert_eq!(state.handle_creation_click(center), None);
        state.update_dimension_pointer(corner);
        assert_eq!(
            state.dimension_readouts(),
            vec![
                DimensionReadout {
                    kind: SketchDimensionKind::Width,
                    value: 4.0,
                    locked: false,
                    editable: true,
                },
                DimensionReadout {
                    kind: SketchDimensionKind::Height,
                    value: 3.0,
                    locked: false,
                    editable: true,
                },
            ]
        );
        let id = state
            .handle_creation_click(corner)
            .expect("corner click should stage the complete rectangle recipe");
        assert_eq!(
            state.pending().expect("pending rectangle").entities().len(),
            4
        );
        assert_eq!(state.gesture_progress().completed_points, 2);
        assert!(state.gesture_progress().awaiting_confirmation);

        assert_eq!(state.commit_pending(), Ok(id));
        assert_eq!(state.entities().len(), 4);
        assert_eq!(state.authoring().active_operations().count(), 1);
        assert!(state.certified_sketch_profile().is_some());
    }

    #[test]
    fn two_point_circle_reports_diameter_and_persists_an_analytic_circle() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::TwoPointCircle));
        let first = SketchPoint::new(-2.0, 1.0);
        let second = SketchPoint::new(4.0, 1.0);

        state.handle_creation_click(first);
        state.update_dimension_pointer(second);
        assert_eq!(
            state.dimension_readouts()[0].kind,
            SketchDimensionKind::Diameter
        );
        assert!((state.dimension_readouts()[0].value - 6.0).abs() <= EPSILON);
        let id = state
            .handle_creation_click(second)
            .expect("diameter endpoint should stage the circle");
        let SketchGeometry::Circle { center, rim } = state
            .pending()
            .expect("pending circle")
            .entity()
            .expect("circle transaction inserts geometry")
            .geometry
        else {
            panic!("two-point circle must remain analytic");
        };
        assert_point_near(center, SketchPoint::new(1.0, 1.0));
        assert!((center.distance_squared(rim).sqrt() - 3.0).abs() <= EPSILON);
        assert_eq!(state.commit_pending(), Ok(id));
        assert_eq!(state.authoring().active_entities().count(), 1);
    }

    #[test]
    fn polygon_variants_stage_closed_atomic_edges_with_configurable_side_count() {
        for variant in [
            ToolVariant::InnerDiameterPolygon,
            ToolVariant::OuterDiameterPolygon,
        ] {
            let mut state = SketchCanvasState::default();
            assert!(state.set_exact_tool(variant));
            assert_eq!(state.polygon_sides(), DEFAULT_POLYGON_SIDES);
            assert!(state.set_polygon_sides(8));
            assert!(!state.set_polygon_sides(CORE_MIN_POLYGON_SIDES - 1));
            assert_eq!(state.gesture_progress().required_points, 2);

            state.handle_creation_click(SketchPoint::new(0.0, 0.0));
            state.update_dimension_pointer(SketchPoint::new(0.0, 2.0));
            assert_eq!(
                state.dimension_readouts()[0].kind,
                SketchDimensionKind::Diameter
            );
            assert!((state.dimension_readouts()[0].value - 4.0).abs() <= EPSILON);
            let id = state
                .handle_creation_click(SketchPoint::new(0.0, 2.0))
                .expect("reference click should stage the polygon");
            assert_eq!(
                state.pending().expect("pending polygon").entities().len(),
                8
            );
            assert_eq!(state.commit_pending(), Ok(id));
            assert_eq!(state.authoring().active_operations().count(), 1);
            assert_eq!(state.authoring().active_entities().count(), 8);
            assert!(state.certified_sketch_profile().is_some());
        }
    }

    #[test]
    fn the_text_tool_stages_glyph_outlines_from_one_anchor_click() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::Text));
        assert_eq!(state.gesture_progress().required_points, 1);
        assert_eq!(
            state.active_tool_input_text("content").as_deref(),
            Some(DEFAULT_TEXT_CONTENT)
        );
        assert!(state.set_active_tool_input_text("content", "O".to_owned()));
        assert!(state.set_active_tool_input_text("height", "12".to_owned()));
        assert!(state.set_active_tool_input_text("angle", "90".to_owned()));
        assert!(state.active_tool_input_error("height").is_none());

        let id = state
            .handle_creation_click(SketchPoint::new(5.0, 5.0))
            .expect("the anchor click stages the text");
        let CoreRecipe::Text {
            content,
            height,
            angle,
            ..
        } = pending_recipe(&state)
        else {
            panic!("the text tool stages the text recipe")
        };
        assert_eq!(content, "O");
        assert!((literal_length(*height) - 12.0).abs() <= EPSILON);
        assert!((literal_angle(*angle) - std::f64::consts::FRAC_PI_2).abs() <= EPSILON);
        let entities = state.pending().expect("pending text").entities().len();
        assert!(
            entities >= 16,
            "an O is two loops of chords, got {entities}"
        );

        assert_eq!(state.commit_pending(), Ok(id));
        assert_eq!(state.authoring().active_operations().count(), 1);
        assert_eq!(state.authoring().active_entities().count(), entities);
        assert!(
            state.certified_sketch_profile().is_some(),
            "the letter's stroke is a closed profile"
        );

        // The committed text is one operation whose text can be retyped:
        // selecting any of its chords exposes the whole recipe.
        let chord = state.entities().first().expect("committed chord").id;
        state.set_selected(None);
        assert!(state.set_selected(Some(chord)));
        let view = state
            .selected_recipe_editor()
            .expect("a selected text exposes its recipe");
        assert_eq!(view.title, "Text");
        let content = view
            .parameters
            .iter()
            .find(|parameter| parameter.stable_key == "content")
            .expect("content parameter");
        assert_eq!(content.text, "O");
        assert!(content.editable);
        assert!(
            view.parameters
                .iter()
                .any(|parameter| parameter.stable_key == "height")
        );
    }

    #[test]
    fn both_slot_gestures_stage_two_rails_and_two_analytic_caps_atomically() {
        for variant in [
            ToolVariant::TwoPointSlot,
            ToolVariant::CentreToOuterPointSlot,
        ] {
            let mut state = SketchCanvasState::default();
            assert!(state.set_exact_tool(variant));
            assert_eq!(state.gesture_progress().required_points, 3);
            assert_eq!(
                state.handle_creation_click(SketchPoint::new(0.0, 0.0)),
                None
            );
            assert_eq!(
                state.handle_creation_click(SketchPoint::new(4.0, 0.0)),
                None
            );
            state.update_dimension_pointer(SketchPoint::new(2.0, 1.0));
            assert_eq!(
                state.dimension_readouts()[0].kind,
                SketchDimensionKind::Width
            );
            assert!((state.dimension_readouts()[0].value - 2.0).abs() <= EPSILON);
            let id = state
                .handle_creation_click(SketchPoint::new(2.0, 1.0))
                .expect("width click should stage the slot");
            let pending = state.pending().expect("pending slot");
            assert_eq!(pending.entities().len(), 4);
            assert_eq!(
                pending
                    .entities()
                    .iter()
                    .filter(|entity| matches!(entity.geometry, SketchGeometry::Arc { .. }))
                    .count(),
                2
            );
            assert_eq!(state.commit_pending(), Ok(id));
            assert_eq!(state.authoring().active_operations().count(), 1);
            assert!(state.certified_sketch_profile().is_some());
        }
    }

    #[test]
    fn three_point_arc_passes_through_the_third_point_on_either_orientation() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::ThreePointArc));
        let start = SketchPoint::new(-1.0, 0.0);
        let end = SketchPoint::new(1.0, 0.0);
        let through = SketchPoint::new(0.0, 1.0);

        assert_eq!(state.handle_creation_click(start), None);
        assert_eq!(state.handle_creation_click(end), None);
        let id = state
            .handle_creation_click(through)
            .expect("third point should stage an analytic arc");
        let SketchGeometry::Arc {
            center,
            start: stored_start,
            end: stored_end,
        } = state
            .pending()
            .expect("pending arc")
            .entity()
            .expect("arc transaction inserts geometry")
            .geometry
        else {
            panic!("three-point arc must remain analytic");
        };
        assert_point_near(center, SketchPoint::new(0.0, 0.0));
        for point in [stored_start, stored_end, through] {
            assert!((center.distance_squared(point).sqrt() - 1.0).abs() <= EPSILON);
        }
        assert_eq!(state.commit_pending(), Ok(id));
    }

    #[test]
    fn three_point_arc_exposes_editable_sweep_and_a_truthfully_derived_radius() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::ThreePointArc));
        let start = SketchPoint::new(-2.0, 0.0);
        let end = SketchPoint::new(2.0, 0.0);
        let through = SketchPoint::new(0.0, 2.0);
        state.handle_creation_click(start);
        state.handle_creation_click(end);
        state.update_dimension_pointer(through);

        let live = state.dimension_readouts();
        assert_eq!(
            live.iter().map(|readout| readout.kind).collect::<Vec<_>>(),
            vec![
                SketchDimensionKind::Radius,
                SketchDimensionKind::SweepDegrees
            ]
        );
        assert!(!live[0].editable, "the fixed endpoint chord derives radius");
        assert!(
            live[1].editable,
            "directed sweep is the independent control"
        );

        state
            .handle_creation_click(through)
            .expect("third point should stage the exact arc");
        let staged = state.dimension_readouts();
        assert_eq!(staged.len(), 2);
        assert!(staged.iter().all(|readout| !readout.editable));
        assert!((staged[0].value - live[0].value).abs() <= EPSILON);
        assert!((staged[1].value - live[1].value).abs() <= EPSILON);
        assert!(state.dimension_session.is_none());
        assert!(state.pending().is_some_and(|pending| {
            pending.core_transaction.is_some() && pending.entities().len() == 1
        }));
    }

    #[test]
    fn typed_three_point_arc_sweep_preserves_endpoints_branch_and_exact_recipe() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::ThreePointArc));
        let first = SketchPoint::new(-2.0, 0.0);
        let second = SketchPoint::new(2.0, 0.0);
        let through = SketchPoint::new(0.0, 2.0);
        state.handle_creation_click(first);
        state.handle_creation_click(second);
        state.update_dimension_pointer(through);

        let session = state.dimension_session.as_mut().expect("live arc session");
        let direction = session
            .three_point_arc
            .expect("fixed endpoint constraint")
            .direction;
        assert!(session.begin_kind(SketchDimensionKind::SweepDegrees));
        session.buffer = "120".to_owned();
        session
            .apply_buffer_live(&BTreeMap::new())
            .expect("valid directed sweep");
        session
            .accept(&BTreeMap::new())
            .expect("typed sweep accepts");
        let radius = 4.0 / (2.0 * 60_f64.to_radians().sin());
        assert!((session.value(SketchDimensionKind::Radius) - radius).abs() <= EPSILON);
        let SketchGeometry::Arc { center, start, end } = session.geometry else {
            panic!("typed three-point arc remains analytic")
        };
        assert!((center.distance_squared(start).sqrt() - radius).abs() <= EPSILON);
        assert!((center.distance_squared(end).sqrt() - radius).abs() <= EPSILON);
        assert!(
            [start, end]
                .iter()
                .any(|point| point.distance_squared(first) <= EPSILON)
        );
        assert!(
            [start, end]
                .iter()
                .any(|point| point.distance_squared(second) <= EPSILON)
        );

        state
            .handle_creation_click(SketchPoint::new(0.0, 9.0))
            .expect("locked sweep stages instead of following the final pointer");
        let CoreRecipe::CentreStartEndArc {
            center: CorePointInput::Position(recipe_center),
            start: CorePointInput::Position(recipe_start),
            end: CorePointInput::Position(recipe_end),
            direction: recipe_direction,
        } = pending_recipe(&state)
        else {
            panic!("three-point arc must stage one exact analytic recipe")
        };
        assert_eq!(*recipe_start, core_point(first));
        assert_eq!(*recipe_end, core_point(second));
        assert_eq!(*recipe_direction, direction);
        assert_point_near(SketchPoint::new(recipe_center.u, recipe_center.v), center);
    }

    #[test]
    fn invalid_three_point_arc_sweep_retains_last_valid_preview_and_blocks_staging() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::ThreePointArc));
        state.handle_creation_click(SketchPoint::new(-2.0, 0.0));
        state.handle_creation_click(SketchPoint::new(2.0, 0.0));
        let through = SketchPoint::new(0.0, 2.0);
        state.update_dimension_pointer(through);
        let session = state.dimension_session.as_mut().expect("live arc session");
        let valid_geometry = session.geometry;
        assert!(session.begin_kind(SketchDimensionKind::SweepDegrees));
        session.buffer = "400".to_owned();
        let error = session
            .apply_buffer_live(&BTreeMap::new())
            .expect_err("sweep beyond 360 is invalid");
        session.error = Some(error);
        assert_eq!(session.geometry, valid_geometry);

        assert_eq!(state.handle_creation_click(through), None);
        assert!(!state.has_pending_edit());
        assert_eq!(
            state
                .dimension_session
                .as_ref()
                .expect("correctable editor remains")
                .geometry,
            valid_geometry
        );
        assert_eq!(
            state.dimension_error(),
            Some(DimensionInputError::SweepOutOfRange)
        );
    }

    #[test]
    fn three_point_arc_tab_editor_stages_the_typed_sweep_before_global_confirmation() {
        let mut sketch = SketchCanvasState::default();
        assert!(sketch.set_exact_tool(ToolVariant::ThreePointArc));
        sketch.handle_creation_click(SketchPoint::new(-2.0, 0.0));
        sketch.handle_creation_click(SketchPoint::new(2.0, 0.0));
        sketch.update_dimension_pointer(SketchPoint::new(0.0, 2.0));
        let mut harness = dimension_harness(sketch);
        harness.run();

        assert!(
            harness
                .query_by_role_and_label(Role::Label, "Arc radius")
                .is_some(),
            "radius is visible as a derived dimension"
        );
        assert!(
            harness
                .query_by_role_and_label(Role::Button, "Arc sweep")
                .is_some(),
            "sweep is the truthful independent arc dimension"
        );
        harness.key_press(egui::Key::Tab);
        harness.step();
        harness.step();
        assert_eq!(
            harness.state().0.active_dimension(),
            Some(SketchDimensionKind::SweepDegrees)
        );
        harness
            .get_by_role_and_label(Role::TextInput, "Arc sweep")
            .type_text("120");
        harness.step();
        harness.key_press(egui::Key::Enter);
        harness.step();

        assert!(harness.state().1.enter);
        assert!(harness.state().0.has_pending_edit());
        let CoreRecipe::CentreStartEndArc { .. } = pending_recipe(&harness.state().0) else {
            panic!("typed sweep stages the exact analytic arc recipe")
        };
        assert!(
            harness
                .state()
                .0
                .dimension_readouts()
                .iter()
                .all(|readout| !readout.editable)
        );
    }

    #[test]
    fn invalid_final_points_keep_exact_gestures_live_for_correction() {
        let mut arc = SketchCanvasState::default();
        assert!(arc.set_exact_tool(ToolVariant::ThreePointArc));
        arc.handle_creation_click(SketchPoint::new(-1.0, 0.0));
        arc.handle_creation_click(SketchPoint::new(1.0, 0.0));
        arc.update_dimension_pointer(SketchPoint::new(0.0, 1.0));
        assert_eq!(
            arc.handle_creation_click(SketchPoint::new(0.0, 0.0)),
            None,
            "a collinear click must not stage the last valid hover arc"
        );
        assert_eq!(arc.gesture_progress().completed_points, 2);
        assert!(
            arc.handle_creation_click(SketchPoint::new(0.0, 1.0))
                .is_some(),
            "the retained gesture should accept a corrected third point"
        );

        let mut slot = SketchCanvasState::default();
        assert!(slot.set_exact_tool(ToolVariant::CentreToOuterPointSlot));
        slot.handle_creation_click(SketchPoint::new(0.0, 0.0));
        slot.handle_creation_click(SketchPoint::new(2.0, 0.0));
        assert_eq!(
            slot.handle_creation_click(SketchPoint::new(1.0, 3.0)),
            None,
            "slot width cannot exceed its overall length"
        );
        assert_eq!(slot.gesture_progress().completed_points, 2);
        assert!(
            slot.handle_creation_click(SketchPoint::new(1.0, 1.0))
                .is_some(),
            "the retained slot should accept a corrected width"
        );
    }

    #[test]
    fn single_line_stages_immediately_while_chained_polyline_stays_local_until_finish() {
        let mut single = SketchCanvasState::default();
        assert!(single.set_exact_tool(ToolVariant::SingleLine));
        single.handle_creation_click(SketchPoint::new(0.0, 0.0));
        let id = single
            .handle_creation_click(SketchPoint::new(1.0, 0.0))
            .expect("single line should stage");
        assert_eq!(single.commit_pending(), Ok(id));
        assert_eq!(single.creation_anchor(), None);

        let mut chained = SketchCanvasState::default();
        assert!(chained.set_exact_tool(ToolVariant::ChainedPolyline));
        let authoring_before = chained.authoring.clone();
        let next_id_before = chained.next_entity_id;
        chained.handle_creation_click(SketchPoint::new(0.0, 0.0));
        assert_eq!(
            chained.handle_creation_click(SketchPoint::new(1.0, 0.0)),
            None
        );
        assert_eq!(chained.creation_anchor(), Some(SketchPoint::new(1.0, 0.0)));
        assert_eq!(chained.gesture_progress().completed_points, 1);
        assert_eq!(chained.authoring, authoring_before);
        assert_eq!(chained.next_entity_id, next_id_before);
        assert!(chained.pending().is_none());
        assert!(chained.entities().is_empty());

        let id = chained
            .finish_polyline_draft()
            .expect("finishing should stage the complete open chain");
        assert_eq!(chained.pending_entity_count(), 1);
        assert_eq!(chained.commit_pending(), Ok(id));
        assert_eq!(chained.creation_anchor(), None);
    }

    #[test]
    fn polyline_finish_query_requires_two_local_vertices_and_no_pending_edit() {
        let mut state = SketchCanvasState::default();
        assert!(!state.polyline_draft_can_finish());
        assert!(state.set_exact_tool(ToolVariant::ChainedPolyline));
        state.handle_creation_click(SketchPoint::new(0.0, 0.0));
        assert!(!state.polyline_draft_can_finish());
        state.handle_creation_click(SketchPoint::new(2.0, 0.0));
        assert!(state.polyline_draft_can_finish());

        state.finish_polyline_draft().expect("stage open chain");
        assert!(!state.polyline_draft_can_finish());
    }

    #[test]
    fn clicking_first_polyline_vertex_stages_one_closed_recipe() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::ChainedPolyline));
        for point in [
            SketchPoint::new(0.0, 0.0),
            SketchPoint::new(3.0, 0.0),
            SketchPoint::new(3.0, 2.0),
        ] {
            assert_eq!(state.handle_creation_click(point), None);
        }
        let subject = state
            .handle_creation_click(SketchPoint::new(0.0, 0.0))
            .expect("the first vertex closes and stages the chain");
        let pending = state.pending().expect("closed polyline preview");
        assert_eq!(pending.subject(), subject);
        assert_eq!(pending.entities().len(), 3);
        let transaction = pending
            .core_transaction
            .as_ref()
            .expect("exact transaction");
        let operation = transaction
            .impact()
            .inserted_operations
            .iter()
            .next()
            .and_then(|id| transaction.preview().operation(*id))
            .expect("polyline operation");
        assert!(matches!(
            operation.recipe,
            CoreRecipe::Polyline {
                ref vertices,
                closed: true,
                construction: false,
            } if vertices.len() == 3
        ));
        assert!(state.polyline_vertices.is_empty());
        assert!(state.gesture_progress().awaiting_confirmation);
    }

    #[test]
    fn open_polyline_confirmation_is_one_revision_and_one_undo_entry() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::ChainedPolyline));
        let pristine = state.authoring.clone();
        for point in [
            SketchPoint::new(-2.0, 0.0),
            SketchPoint::new(0.0, 2.0),
            SketchPoint::new(2.0, 0.0),
        ] {
            state.handle_creation_click(point);
        }
        assert_eq!(state.authoring, pristine);
        assert!(!state.can_undo_local());

        let subject = state
            .finish_polyline_draft()
            .expect("bare Enter seam stages an open chain");
        assert_eq!(state.authoring, pristine, "staging is revision-neutral");
        assert_eq!(state.pending_entity_count(), 2);
        assert_eq!(state.commit_pending(), Ok(subject));
        assert_eq!(state.authoring.active_operations().count(), 1);
        assert_eq!(state.authoring.active_entities().count(), 2);
        let committed = state.authoring.clone();

        assert!(state.undo_local());
        assert_eq!(state.authoring.revision(), pristine.revision());
        assert_eq!(state.authoring.active_operations().count(), 0);
        assert_eq!(state.authoring.active_entities().count(), 0);
        assert!(!state.can_undo_local(), "the full chain is one undo entry");
        assert!(state.redo_local());
        assert_eq!(state.authoring, committed);
        assert!(!state.can_redo_local());
    }

    #[test]
    fn polyline_backspace_and_escape_only_mutate_local_gesture_layers() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::ChainedPolyline));
        let authoring_before = state.authoring.clone();
        let next_id_before = state.next_entity_id;
        for point in [
            SketchPoint::new(0.0, 0.0),
            SketchPoint::new(2.0, 0.0),
            SketchPoint::new(2.0, 2.0),
        ] {
            state.handle_creation_click(point);
        }

        assert!(state.backspace_polyline_segment());
        assert_eq!(
            state.polyline_vertices,
            vec![SketchPoint::new(0.0, 0.0), SketchPoint::new(2.0, 0.0)]
        );
        assert_eq!(state.creation_anchor(), Some(SketchPoint::new(2.0, 0.0)));
        assert!(state.polyline_current_segment_active);

        assert!(state.cancel_polyline_layer());
        assert!(!state.polyline_current_segment_active);
        assert_eq!(state.polyline_vertices.len(), 2);
        assert!(state.cancel_polyline_layer());
        assert!(state.polyline_vertices.is_empty());
        assert_eq!(state.creation_anchor(), None);
        assert_eq!(state.authoring, authoring_before);
        assert_eq!(state.next_entity_id, next_id_before);
        assert!(!state.can_undo_local());
    }

    #[test]
    fn cancelling_staged_polyline_is_revision_and_identity_neutral() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::ChainedPolyline));
        let authoring_before = state.authoring.clone();
        let next_id_before = state.next_entity_id;
        state.handle_creation_click(SketchPoint::new(0.0, 0.0));
        state.handle_creation_click(SketchPoint::new(2.0, 0.0));
        state.handle_creation_click(SketchPoint::new(2.0, 2.0));
        state.finish_polyline_draft().expect("stage polyline");
        assert!(state.cancel_pending().is_some());

        assert_eq!(state.authoring, authoring_before);
        assert_eq!(state.next_entity_id, next_id_before);
        assert!(state.entities().is_empty());
        assert!(!state.can_undo_local());
    }

    #[test]
    fn committed_polyline_recipe_survives_json_replay() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::ChainedPolyline));
        for point in [
            SketchPoint::new(-1.0, -1.0),
            SketchPoint::new(1.0, -1.0),
            SketchPoint::new(1.0, 1.0),
        ] {
            state.handle_creation_click(point);
        }
        let subject = state.finish_polyline_draft().expect("stage polyline");
        assert_eq!(state.commit_pending(), Ok(subject));

        let json = serde_json::to_string(state.authoring()).expect("serialize exact recipe graph");
        let replayed: CoreSketchDefinition =
            serde_json::from_str(&json).expect("deserialize exact recipe graph");
        let operation = replayed
            .active_operations()
            .next()
            .expect("one replayed operation");
        assert!(matches!(
            operation.recipe,
            CoreRecipe::Polyline {
                ref vertices,
                closed: false,
                construction: false,
            } if vertices.len() == 3
        ));
        let hydrated = SketchCanvasState::from_authoring(SketchPlane::XY, replayed)
            .expect("replay exact polyline into the canvas");
        assert_eq!(hydrated.entities().len(), 2);
        assert!(!hydrated.can_undo_local());
    }

    #[test]
    fn plane_embedding_preserves_canvas_axes() {
        let point = SketchPoint::new(2.0, 3.0);
        assert_eq!(SketchPlane::XY.to_world(point), [2.0, 3.0, 0.0]);
        assert_eq!(SketchPlane::YZ.to_world(point), [0.0, 2.0, 3.0]);
        assert_eq!(SketchPlane::XZ.to_world(point), [2.0, 0.0, 3.0]);
        for plane in SketchPlane::ALL {
            assert_point_near(plane.from_world(plane.to_world(point)), point);
        }
    }

    #[test]
    fn visible_grid_is_an_adaptive_integer_sub_lattice_of_snap_grid() {
        for points_per_unit in [4.0, 10.0, 56.0, 320.0, 4_000.0] {
            for lattice_step in [0.01, 0.2, 0.3, 1.0] {
                let spacing = visible_grid_spacing(points_per_unit, lattice_step)
                    .expect("valid snap lattice should produce a visible grid");
                assert_eq!(
                    spacing.major_multiple,
                    spacing.minor_multiple * MAJOR_GRID_INTERVAL
                );
                assert!(spacing.minor_multiple >= 1);
                assert!(spacing.minor_world_step() * points_per_unit >= TARGET_GRID_SPACING_POINTS);
                for visible_index in -4_i64..=4 {
                    let visible_coordinate = visible_index as f64 * spacing.minor_world_step();
                    let lattice_index = visible_index * spacing.minor_multiple as i64;
                    let lattice_coordinate = lattice_index as f64 * lattice_step;
                    assert!((visible_coordinate - lattice_coordinate).abs() <= EPSILON);
                }
            }
        }

        let coarse = visible_grid_spacing(56.0, 0.2).expect("grid should be visible");
        let changed = visible_grid_spacing(56.0, 0.3).expect("grid should be visible");
        assert_eq!(coarse.lattice_step(), 0.2);
        assert_eq!(changed.lattice_step(), 0.3);
        assert_ne!(coarse.minor_world_step(), changed.minor_world_step());
    }

    #[test]
    fn overflowing_analytic_circle_radius_fails_closed() {
        let geometry = SketchGeometry::circle(
            SketchPoint::new(f64::MAX, 0.0),
            SketchPoint::new(-f64::MAX, 0.0),
        );
        let mut state = SketchCanvasState::default();

        assert!(!geometry.is_finite());
        assert_eq!(
            state.stage_geometry(geometry),
            Err(SketchEditError::NonFiniteGeometry)
        );
        assert_eq!(
            state.certified_profile_status(),
            CertifiedProfileStatus::Empty
        );
        assert!(!state.certified_profile_status().can_finish());
    }

    #[test]
    fn point_tool_stages_on_one_click() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_tool(SketchTool::Point));

        let id = state
            .handle_creation_click(SketchPoint::new(1.25, -2.5))
            .expect("a point should stage on its first click");

        assert_eq!(
            state.pending().map(PendingSketchEdit::label),
            Some("Add sketch point")
        );
        assert_eq!(state.commit_pending(), Ok(id));
        assert_eq!(
            state.certified_profile_status(),
            CertifiedProfileStatus::Open
        );
    }

    #[test]
    fn connected_lines_certify_closure_winding_and_crossings() {
        let counter_clockwise = [
            SketchPoint::new(0.0, 0.0),
            SketchPoint::new(4.0, 0.0),
            SketchPoint::new(4.0, 3.0),
            SketchPoint::new(0.0, 3.0),
            SketchPoint::new(0.0, 0.0),
        ];
        let clockwise = [
            SketchPoint::new(0.0, 0.0),
            SketchPoint::new(0.0, 3.0),
            SketchPoint::new(4.0, 3.0),
            SketchPoint::new(4.0, 0.0),
            SketchPoint::new(0.0, 0.0),
        ];
        let bow_tie = [
            SketchPoint::new(0.0, 0.0),
            SketchPoint::new(4.0, 4.0),
            SketchPoint::new(0.0, 4.0),
            SketchPoint::new(4.0, 0.0),
            SketchPoint::new(0.0, 0.0),
        ];

        for (points, expected) in [
            (
                counter_clockwise.as_slice(),
                CertifiedProfileStatus::Closed {
                    winding: ProfileWinding::CounterClockwise,
                },
            ),
            (
                clockwise.as_slice(),
                CertifiedProfileStatus::Closed {
                    winding: ProfileWinding::Clockwise,
                },
            ),
            (bow_tie.as_slice(), CertifiedProfileStatus::SelfIntersecting),
        ] {
            let mut state = SketchCanvasState::default();
            for edge in points.windows(2) {
                state
                    .stage_geometry(SketchGeometry::segment(edge[0], edge[1]))
                    .expect("line should stage");
                state.commit_pending().expect("line should commit");
            }
            assert_eq!(state.certified_profile_status(), expected);
            assert_eq!(
                state.certified_polyline_vertices(),
                matches!(expected, CertifiedProfileStatus::Closed { .. })
                    .then(|| counter_clockwise[..counter_clockwise.len() - 1].to_vec())
            );
        }
    }

    #[test]
    fn unordered_and_reversed_multiline_square_exports_one_canonical_loop() {
        let a = SketchPoint::new(0.0, 0.0);
        let b = SketchPoint::new(4.0, 0.0);
        let c = SketchPoint::new(4.0, 3.0);
        let d = SketchPoint::new(0.0, 3.0);
        let mut state = SketchCanvasState::default();
        for (start, end) in [(c, b), (a, d), (c, d), (a, b)] {
            state
                .stage_geometry(SketchGeometry::segment(start, end))
                .expect("line should stage");
            state.commit_pending().expect("line should commit");
        }

        assert!(matches!(
            state.certified_profile_status(),
            CertifiedProfileStatus::Closed { .. }
        ));
        assert_eq!(state.certified_polyline_vertices(), Some(vec![a, b, c, d]));
        let profile = state
            .certified_sketch_profile()
            .expect("the endpoint graph is one closed region");
        assert_eq!(profile.regions.len(), 1);
        assert_eq!(profile.loop_count(), 1);
        assert!(!profile.has_analytic_curves());
        assert_eq!(state.diagnostics().open_wire_components, 0);
    }

    #[test]
    fn oversized_profile_resources_reject_before_pairwise_curve_analysis() {
        use std::time::{Duration, Instant};

        let circles = (0..=MAX_PLANAR_PROFILE_CURVES)
            .map(|index| SketchEntity {
                id: SketchEntityId(index as u64 + 1),
                geometry: SketchGeometry::circle(
                    SketchPoint::new(index as f64 * 3.0, 0.0),
                    SketchPoint::new(index as f64 * 3.0 + 1.0, 0.0),
                ),
                role: SketchEntityRole::Profile,
            })
            .collect::<Vec<_>>();
        let start = Instant::now();
        let analysis = analyze_profile_entities(&circles);
        let elapsed = start.elapsed();
        assert_eq!(
            analysis.status,
            CertifiedProfileStatus::TooManyCurves {
                count: MAX_PLANAR_PROFILE_CURVES + 1,
            }
        );
        assert!(
            elapsed < Duration::from_micros(16_667),
            "curve-limit preflight took {elapsed:?}"
        );

        let circles = (0..=MAX_PLANAR_PROFILE_LOOPS)
            .map(|index| SketchEntity {
                id: SketchEntityId(index as u64 + 1),
                geometry: SketchGeometry::circle(
                    SketchPoint::new(index as f64 * 3.0, 0.0),
                    SketchPoint::new(index as f64 * 3.0 + 1.0, 0.0),
                ),
                role: SketchEntityRole::Profile,
            })
            .collect::<Vec<_>>();
        let start = Instant::now();
        let analysis = analyze_profile_entities(&circles);
        let elapsed = start.elapsed();
        assert_eq!(
            analysis.status,
            CertifiedProfileStatus::TooManyLoops {
                count: MAX_PLANAR_PROFILE_LOOPS + 1,
            }
        );
        assert!(
            elapsed < Duration::from_micros(16_667),
            "loop-limit preflight took {elapsed:?}"
        );

        let vertex_count = MAX_EXTRUSION_PROFILE_VERTICES + 1;
        let vertices = (0..vertex_count)
            .map(|index| {
                let angle = std::f64::consts::TAU * index as f64 / vertex_count as f64;
                SketchPoint::new(angle.cos() * 100.0, angle.sin() * 100.0)
            })
            .collect::<Vec<_>>();
        let lines = (0..vertex_count)
            .map(|index| SketchEntity {
                id: SketchEntityId(index as u64 + 1),
                geometry: SketchGeometry::segment(
                    vertices[index],
                    vertices[(index + 1) % vertex_count],
                ),
                role: SketchEntityRole::Profile,
            })
            .collect::<Vec<_>>();
        let start = Instant::now();
        let analysis = analyze_profile_entities(&lines);
        let elapsed = start.elapsed();
        assert_eq!(
            analysis.status,
            CertifiedProfileStatus::LinearLoopTooLarge {
                count: vertex_count,
            }
        );
        assert!(
            elapsed < Duration::from_micros(16_667),
            "linear-loop preflight took {elapsed:?}"
        );
    }

    #[test]
    fn nested_rectangles_are_one_region_with_a_clockwise_hole() {
        let mut state = SketchCanvasState::default();
        for geometry in [
            SketchGeometry::rectangle(SketchPoint::new(-5.0, -4.0), SketchPoint::new(5.0, 4.0)),
            SketchGeometry::rectangle(SketchPoint::new(-2.0, -1.0), SketchPoint::new(2.0, 1.0)),
        ] {
            state
                .stage_geometry(geometry)
                .expect("rectangle should stage");
            state.commit_pending().expect("rectangle should commit");
        }

        assert_eq!(
            state.certified_profile_status(),
            CertifiedProfileStatus::ClosedRegions {
                regions: 1,
                loops: 2,
                holes: 1,
                analytic: false,
            }
        );
        let profile = state
            .certified_sketch_profile()
            .expect("nested loops should produce one material region");
        assert_eq!(profile.regions.len(), 1);
        assert_eq!(
            profile.regions[0].outer.winding,
            ProfileWinding::CounterClockwise
        );
        assert_eq!(profile.regions[0].holes.len(), 1);
        assert_eq!(
            profile.regions[0].holes[0].winding,
            ProfileWinding::Clockwise
        );
        let linear = profile
            .linear_regions()
            .expect("both loops are exactly linear");
        assert_eq!(linear.len(), 1);
        assert_eq!(linear[0].outer.len(), 4);
        assert_eq!(
            linear[0].holes,
            vec![vec![
                SketchPoint::new(-2.0, -1.0),
                SketchPoint::new(-2.0, 1.0),
                SketchPoint::new(2.0, 1.0),
                SketchPoint::new(2.0, -1.0),
            ]]
        );
        let fill_triangles = fillable_linear_profile_polygons(&profile)
            .into_iter()
            .flat_map(|polygon| {
                let points = polygon
                    .into_iter()
                    .map(|point| Pos2::new(point.u as f32, point.v as f32))
                    .collect::<Vec<_>>();
                triangulate_simple_polygon(&points)
            })
            .collect::<Vec<_>>();
        let hole_center = Pos2::new(0.0, 0.0);
        assert!(
            fill_triangles.iter().all(|triangle| {
                let winding = triangle_cross(triangle[0], triangle[1], triangle[2]).signum();
                !point_in_triangle(hole_center, triangle[0], triangle[1], triangle[2], winding)
            }),
            "a hole center must never receive a profile-fill triangle"
        );
    }

    #[test]
    fn a_circle_resting_on_a_square_side_is_two_cells_not_a_self_intersection() {
        // Grid snap makes this the common way to draw "a circle inside a
        // square". The touch is not a crossing: the headline stays closed,
        // and the arrangement offers the disc and the pinched surround.
        let mut state = SketchCanvasState::default();
        for geometry in [
            SketchGeometry::rectangle(SketchPoint::new(-2.0, -2.0), SketchPoint::new(2.0, 2.0)),
            SketchGeometry::circle(SketchPoint::new(-1.0, 0.0), SketchPoint::new(-2.0, 0.0)),
        ] {
            state
                .stage_geometry(geometry)
                .expect("geometry should stage");
            state.commit_pending().expect("geometry should commit");
        }
        assert_eq!(
            state.certified_profile_status(),
            CertifiedProfileStatus::ClosedRegions {
                regions: 1,
                loops: 2,
                holes: 1,
                analytic: true,
            }
        );
        assert_eq!(state.available_region_count(), 2);
        assert!(state.select_region_at_point(SketchPoint::new(-1.0, 0.0), false));
        let disc = state
            .selected_planar_profile()
            .expect("the disc compiles on its own");
        assert_eq!(disc.regions.len(), 1);
        assert!(disc.regions[0].holes.is_empty());
        // The surround's boundary returns to the touch point, which no face
        // loop may do; the refusal names that rather than a generic failure.
        assert!(state.select_region_at_point(SketchPoint::new(1.5, 1.5), false));
        assert!(state.selected_planar_profile().is_none());
        assert_eq!(
            state.selected_planar_profile_error(),
            Some(CoreProfileCompileError::PinchedBoundary)
        );
    }

    #[test]
    fn concentric_and_offset_circles_classify_as_exact_annular_regions() {
        for inner_center in [SketchPoint::new(0.0, 0.0), SketchPoint::new(1.0, 0.5)] {
            let mut state = SketchCanvasState::default();
            for geometry in [
                SketchGeometry::circle(SketchPoint::new(0.0, 0.0), SketchPoint::new(6.0, 0.0)),
                SketchGeometry::circle(
                    inner_center,
                    SketchPoint::new(inner_center.u + 2.0, inner_center.v),
                ),
            ] {
                state.stage_geometry(geometry).expect("circle should stage");
                state.commit_pending().expect("circle should commit");
            }
            let profile = state
                .certified_sketch_profile()
                .expect("nested circles form an annulus");
            assert_eq!(profile.regions.len(), 1);
            assert_eq!(profile.hole_count(), 1);
            assert!(profile.has_analytic_curves());
            assert_eq!(
                profile.regions[0].outer.winding,
                ProfileWinding::CounterClockwise
            );
            assert_eq!(
                profile.regions[0].holes[0].winding,
                ProfileWinding::Clockwise
            );
            assert!(profile.linear_regions().is_none());
        }
    }

    #[test]
    fn island_inside_a_hole_becomes_a_second_depth_two_material_region() {
        let mut state = SketchCanvasState::default();
        for (first, opposite) in [
            ((-8.0, -8.0), (8.0, 8.0)),
            ((-5.0, -5.0), (5.0, 5.0)),
            ((-2.0, -2.0), (2.0, 2.0)),
        ] {
            state
                .stage_geometry(SketchGeometry::rectangle(
                    SketchPoint::new(first.0, first.1),
                    SketchPoint::new(opposite.0, opposite.1),
                ))
                .expect("rectangle should stage");
            state.commit_pending().expect("rectangle should commit");
        }

        let profile = state
            .certified_sketch_profile()
            .expect("alternating nesting produces material, void, material");
        assert_eq!(profile.regions.len(), 2);
        assert_eq!(profile.loop_count(), 3);
        assert_eq!(profile.hole_count(), 1);
        assert_eq!(profile.regions[0].outer.nesting_depth, 0);
        assert_eq!(profile.regions[0].holes[0].nesting_depth, 1);
        assert_eq!(profile.regions[1].outer.nesting_depth, 2);
    }

    #[test]
    fn analytic_circle_and_line_arc_loop_remain_exact_curve_payloads() {
        let mut circle = SketchCanvasState::default();
        circle
            .stage_geometry(SketchGeometry::circle(
                SketchPoint::new(2.0, 3.0),
                SketchPoint::new(5.0, 3.0),
            ))
            .expect("circle should stage");
        assert!(
            circle.certified_sketch_profile().is_none(),
            "pending geometry is diagnostic only"
        );
        circle.commit_pending().expect("circle should commit");
        let profile = circle
            .certified_sketch_profile()
            .expect("circle is an exact closed profile");
        assert!(profile.has_analytic_curves());
        assert!(
            profile.linear_regions().is_none(),
            "a circle must never be faceted"
        );
        assert!(matches!(
            profile.regions[0].outer.curves.as_slice(),
            [CertifiedSketchCurve::Circle { .. }]
        ));

        let left = SketchPoint::new(-2.0, 0.0);
        let right = SketchPoint::new(2.0, 0.0);
        let mut semicircle = SketchCanvasState::default();
        for geometry in [
            SketchGeometry::segment(left, right),
            SketchGeometry::arc(SketchPoint::new(0.0, 0.0), right, left),
        ] {
            semicircle
                .stage_geometry(geometry)
                .expect("curve should stage");
            semicircle.commit_pending().expect("curve should commit");
        }
        assert_eq!(
            semicircle.certified_profile_status(),
            CertifiedProfileStatus::ClosedAnalyticCurves
        );
        let profile = semicircle
            .certified_sketch_profile()
            .expect("line and semicircle form one exact loop");
        assert_eq!(profile.loop_count(), 1);
        assert_eq!(profile.regions[0].outer.curves.len(), 2);
        assert!(profile.linear_regions().is_none());
    }

    #[test]
    fn malformed_arc_radius_is_rejected_instead_of_moving_its_endpoint() {
        let mut state = SketchCanvasState::default();
        state
            .stage_geometry(SketchGeometry::arc(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(2.0, 0.0),
                SketchPoint::new(0.0, 3.0),
            ))
            .expect("finite geometry can be staged for visible correction");
        assert_eq!(
            state.certified_profile_status(),
            CertifiedProfileStatus::Invalid
        );
        assert!(state.certified_sketch_profile().is_none());
    }

    #[test]
    fn disconnected_and_mixed_curve_profiles_are_never_guessed_closed() {
        let mut disconnected = SketchCanvasState::default();
        for (start, end) in [
            (SketchPoint::new(0.0, 0.0), SketchPoint::new(1.0, 0.0)),
            (SketchPoint::new(2.0, 0.0), SketchPoint::new(2.0, 1.0)),
        ] {
            disconnected
                .stage_geometry(SketchGeometry::segment(start, end))
                .expect("line should stage");
            disconnected.commit_pending().expect("line should commit");
        }
        assert_eq!(
            disconnected.certified_profile_status(),
            CertifiedProfileStatus::Open
        );

        let mut curved = SketchCanvasState::default();
        curved
            .stage_geometry(SketchGeometry::arc(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(1.0, 0.0),
                SketchPoint::new(0.0, 1.0),
            ))
            .expect("arc should stage");
        assert_eq!(
            curved.certified_profile_status(),
            CertifiedProfileStatus::Open
        );
    }

    #[test]
    fn every_geometry_kind_has_an_on_geometry_semantic_target() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0));
        let view = SketchView::default();
        let geometries = [
            SketchGeometry::point(SketchPoint::new(-2.0, 0.0)),
            SketchGeometry::segment(SketchPoint::new(-1.0, -1.0), SketchPoint::new(1.0, -1.0)),
            SketchGeometry::rectangle(SketchPoint::new(-2.0, -2.0), SketchPoint::new(0.0, 0.0)),
            SketchGeometry::circle(SketchPoint::new(2.0, 0.0), SketchPoint::new(3.0, 0.0)),
            SketchGeometry::arc(
                SketchPoint::new(0.0, 2.0),
                SketchPoint::new(1.0, 2.0),
                SketchPoint::new(0.0, 3.0),
            ),
        ];

        for geometry in geometries {
            let (position, _) = semantic_target(geometry, view, rect)
                .expect("finite visible geometry should have a semantic target");
            let distance = geometry_screen_distance(geometry, view, rect, position);
            assert!(
                distance <= 1.0e-4,
                "semantic target for {geometry:?} is {distance} points off geometry"
            );
        }
    }

    #[test]
    fn closed_geometry_semantic_targets_are_clipped_accessible_buttons() {
        let mut state = SketchCanvasState::default();
        for geometry in [
            SketchGeometry::rectangle(SketchPoint::new(-2.0, -1.0), SketchPoint::new(0.0, 1.0)),
            SketchGeometry::circle(SketchPoint::new(4.1, 0.0), SketchPoint::new(5.1, 0.0)),
            SketchGeometry::arc(
                SketchPoint::new(0.0, 2.0),
                SketchPoint::new(1.0, 2.0),
                SketchPoint::new(0.0, 3.0),
            ),
        ] {
            state
                .stage_geometry(geometry)
                .expect("geometry should stage");
            state.commit_pending().expect("geometry should commit");
        }

        let mut harness = Harness::builder()
            .with_size(Vec2::new(600.0, 400.0))
            .build_ui_state(
                |ui, state| {
                    let _ = show(ui, state);
                },
                state,
            );
        harness.run();

        let viewport = harness.get_by_label("Sketch viewport").rect();
        for label in ["Sketch rectangle 1", "Sketch circle 2", "Sketch arc 3"] {
            let target = harness.get_by_role_and_label(Role::Button, label).rect();
            assert!(viewport.contains(target.min));
            assert!(viewport.contains(target.max));
        }
        assert!(
            harness
                .get_by_role_and_label(Role::Button, "Sketch circle 2")
                .rect()
                .width()
                < 22.0
        );
    }

    fn readout_value(session: &DimensionSession, kind: SketchDimensionKind) -> f64 {
        session
            .readouts()
            .find(|readout| readout.kind == kind)
            .unwrap_or_else(|| panic!("missing {kind:?} readout"))
            .value
    }

    #[test]
    fn rectangle_dimensions_lock_independently_while_pointer_selects_quadrant() {
        let first = SketchPoint::new(1.0, 2.0);
        let mut session = DimensionSession::new(
            DimensionTarget::Draft,
            DimensionPhase::Rectangle,
            SketchGeometry::rectangle(first, first),
            1,
        );
        session.update_pointer(SketchPoint::new(5.0, -1.0));
        assert!((readout_value(&session, SketchDimensionKind::Width) - 4.0).abs() <= EPSILON);
        assert!((readout_value(&session, SketchDimensionKind::Height) - 3.0).abs() <= EPSILON);

        assert!(session.begin_kind(SketchDimensionKind::Width));
        session.buffer = "10".to_owned();
        session
            .apply_buffer_live(&BTreeMap::new())
            .expect("valid width");
        session
            .accept(&BTreeMap::new())
            .expect("width should accept");
        session.update_pointer(SketchPoint::new(-2.0, 9.0));

        assert!((readout_value(&session, SketchDimensionKind::Width) - 10.0).abs() <= EPSILON);
        assert!((readout_value(&session, SketchDimensionKind::Height) - 7.0).abs() <= EPSILON);
        assert_eq!(
            session.geometry,
            SketchGeometry::rectangle(first, SketchPoint::new(-9.0, 9.0))
        );
    }

    #[test]
    fn line_length_angle_and_deltas_remain_consistent() {
        let start = SketchPoint::new(0.0, 0.0);
        let mut session = DimensionSession::new(
            DimensionTarget::Draft,
            DimensionPhase::Line,
            SketchGeometry::segment(start, start),
            1,
        );
        session.update_pointer(SketchPoint::new(3.0, 4.0));
        assert!((readout_value(&session, SketchDimensionKind::Length) - 5.0).abs() <= EPSILON);

        assert!(session.begin_kind(SketchDimensionKind::Length));
        session.buffer = "10".to_owned();
        session
            .accept(&BTreeMap::new())
            .expect("length should accept");
        session.update_pointer(SketchPoint::new(0.0, 6.0));

        let SketchGeometry::Segment { end, .. } = session.geometry else {
            panic!("expected line geometry");
        };
        assert_point_near(end, SketchPoint::new(0.0, 10.0));
        assert!(
            (readout_value(&session, SketchDimensionKind::AngleDegrees) - 90.0).abs() <= EPSILON
        );
        assert!(readout_value(&session, SketchDimensionKind::DeltaU).abs() <= EPSILON);
        assert!((readout_value(&session, SketchDimensionKind::DeltaV) - 10.0).abs() <= EPSILON);
    }

    #[test]
    fn unconstrained_line_preserves_the_exact_pointer_endpoint() {
        let start = SketchPoint::new(-1.75, 2.25);
        let end = SketchPoint::new(0.375, -4.625);
        let mut session = DimensionSession::new(
            DimensionTarget::Draft,
            DimensionPhase::Line,
            SketchGeometry::segment(start, start),
            1,
        );

        session.update_pointer(end);

        assert_eq!(session.geometry, SketchGeometry::segment(start, end));
    }

    #[test]
    fn circle_uses_diameter_while_preserving_rim_direction() {
        let center = SketchPoint::new(0.0, 0.0);
        let mut session = DimensionSession::new(
            DimensionTarget::Draft,
            DimensionPhase::Circle,
            SketchGeometry::circle(center, center),
            1,
        );
        session.update_pointer(SketchPoint::new(3.0, 4.0));
        assert!((readout_value(&session, SketchDimensionKind::Diameter) - 10.0).abs() <= EPSILON);

        assert!(session.begin_kind(SketchDimensionKind::Diameter));
        session.buffer = "20".to_owned();
        session
            .accept(&BTreeMap::new())
            .expect("diameter should accept");
        let SketchGeometry::Circle { rim, .. } = session.geometry else {
            panic!("expected circle geometry");
        };
        assert_point_near(rim, SketchPoint::new(6.0, 8.0));
    }

    #[test]
    fn arc_radius_and_sweep_rebuild_one_canonical_arc() {
        let center = SketchPoint::new(0.0, 0.0);
        let mut session = DimensionSession::new(
            DimensionTarget::Draft,
            DimensionPhase::ArcSweep,
            SketchGeometry::arc(
                center,
                SketchPoint::new(2.0, 0.0),
                SketchPoint::new(0.0, 2.0),
            ),
            1,
        );
        assert!(session.begin_kind(SketchDimensionKind::Radius));
        session.buffer = "3".to_owned();
        session
            .accept(&BTreeMap::new())
            .expect("radius should accept");
        assert!(session.begin_kind(SketchDimensionKind::SweepDegrees));
        session.buffer = "180".to_owned();
        session
            .accept(&BTreeMap::new())
            .expect("sweep should accept");

        let SketchGeometry::Arc { start, end, .. } = session.geometry else {
            panic!("expected arc geometry");
        };
        assert_point_near(start, SketchPoint::new(3.0, 0.0));
        assert_point_near(end, SketchPoint::new(-3.0, 0.0));
        assert!((readout_value(&session, SketchDimensionKind::Radius) - 3.0).abs() <= EPSILON);
        assert!(
            (readout_value(&session, SketchDimensionKind::SweepDegrees) - 180.0).abs() <= EPSILON
        );
    }

    #[test]
    fn invalid_dimension_text_reverts_without_moving_geometry() {
        let geometry =
            SketchGeometry::rectangle(SketchPoint::new(0.0, 0.0), SketchPoint::new(4.0, 3.0));
        let mut session = DimensionSession::new(
            DimensionTarget::Draft,
            DimensionPhase::Rectangle,
            geometry,
            1,
        );
        assert!(session.begin_kind(SketchDimensionKind::Width));
        session.buffer = "not a number".to_owned();
        assert_eq!(
            session.apply_buffer_live(&BTreeMap::new()),
            Err(DimensionInputError::NotANumber)
        );
        assert_eq!(session.geometry, geometry);
        assert!(session.cancel_edit());
        assert_eq!(session.geometry, geometry);
        assert_eq!(session.active_kind(), None);
    }

    #[test]
    fn pending_dimension_updates_preserve_entity_identity_and_profile_cache() {
        let mut state = SketchCanvasState::default();
        let id = state
            .stage_geometry(SketchGeometry::circle(
                SketchPoint::new(1.0, 1.0),
                SketchPoint::new(3.0, 1.0),
            ))
            .expect("circle should stage");
        let session = state
            .dimension_session
            .as_mut()
            .expect("pending circle should expose dimensions");
        assert!(session.begin_kind(SketchDimensionKind::Diameter));
        session.buffer = "10".to_owned();
        session
            .accept(&BTreeMap::new())
            .expect("diameter should accept");
        state.sync_dimension_pending();

        assert_eq!(
            state
                .pending()
                .and_then(PendingSketchEdit::entity)
                .map(|entity| entity.id),
            Some(id)
        );
        let SketchGeometry::Circle { center, rim } =
            state.pending_geometry().expect("pending circle")
        else {
            panic!("expected circle geometry");
        };
        assert!((center.distance_squared(rim).sqrt() - 5.0).abs() <= EPSILON);
        assert_eq!(
            state.certified_profile_status(),
            CertifiedProfileStatus::ClosedAnalyticCircle
        );
        assert_eq!(state.commit_pending(), Ok(id));
        assert!(!state.dimension_editor_active());
    }

    #[test]
    fn circle_creation_clicks_produce_accurate_diameter_readout() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::CentrePointCircle));
        state.handle_creation_click(SketchPoint::new(0.0, 0.0));
        let id = state
            .handle_creation_click(SketchPoint::new(8.0, 0.0))
            .expect("second click should stage circle");
        let session = state
            .dimension_session
            .as_ref()
            .expect("staged circle should have dimension session");
        assert_eq!(session.target, DimensionTarget::Pending(id));
        let dia = readout_value(session, SketchDimensionKind::Diameter);
        assert!(
            (dia - 16.0).abs() <= EPSILON,
            "expected diameter 16.0, got {dia}"
        );
        assert!(session.error.is_none());
        assert_eq!(
            state.certified_profile_status(),
            CertifiedProfileStatus::ClosedAnalyticCircle
        );
    }

    #[test]
    fn accepted_polyline_vertex_starts_a_fresh_unlocked_dimension_session() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_tool(SketchTool::Line));
        state.handle_creation_click(SketchPoint::new(0.0, 0.0));
        assert_eq!(
            state.handle_creation_click(SketchPoint::new(2.0, 0.0)),
            None,
            "accepted vertices remain an identity-free local gesture"
        );

        assert_eq!(state.creation_anchor(), Some(SketchPoint::new(2.0, 0.0)));
        let session = state
            .dimension_session
            .as_ref()
            .expect("continuing line should own a fresh session");
        assert_eq!(session.target, DimensionTarget::Draft);
        assert_eq!(session.phase, DimensionPhase::Line);
        assert_eq!(readout_value(session, SketchDimensionKind::Length), 0.0);
        assert!(session.readouts().all(|readout| !readout.locked));
        assert!(
            state.creation_draft_blocks_modeling(),
            "the unfinished atomic chain is a user-authored draft"
        );
        assert!(state.entities().is_empty());
        assert!(state.pending().is_none());
    }

    #[test]
    fn typed_polyline_length_accepts_a_vertex_without_staging_the_chain() {
        let mut state = SketchCanvasState::default();
        assert!(state.set_exact_tool(ToolVariant::ChainedPolyline));
        state.handle_creation_click(SketchPoint::new(1.0, 2.0));
        state.update_dimension_pointer(SketchPoint::new(4.0, 2.0));
        let session = state
            .dimension_session
            .as_mut()
            .expect("live segment dimensions");
        assert!(session.begin_kind(SketchDimensionKind::Length));
        session.buffer = "5".to_owned();
        session.accept(&BTreeMap::new()).expect("valid length");

        assert_eq!(stage_complete_dimension_draft(&mut state), None);
        assert_eq!(
            state.polyline_vertices,
            vec![SketchPoint::new(1.0, 2.0), SketchPoint::new(6.0, 2.0)]
        );
        assert!(state.pending().is_none());
        assert_eq!(state.authoring.active_operations().count(), 0);
    }

    #[test]
    fn every_creation_phase_has_visible_clipped_dimension_widgets() {
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(720.0, 500.0));
        let cases = [
            (
                DimensionPhase::Point,
                SketchGeometry::point(SketchPoint::new(0.0, 0.0)),
                vec![SketchDimensionKind::U, SketchDimensionKind::V],
            ),
            (
                DimensionPhase::Line,
                SketchGeometry::segment(SketchPoint::new(-2.0, -1.0), SketchPoint::new(2.0, 1.0)),
                // Length and angle only. The deltas describe the same line a
                // second way, so the canvas would be drawing it twice; they
                // stay in the readouts, asserted below.
                vec![
                    SketchDimensionKind::Length,
                    SketchDimensionKind::AngleDegrees,
                ],
            ),
            (
                DimensionPhase::Rectangle,
                SketchGeometry::rectangle(SketchPoint::new(-2.0, -1.0), SketchPoint::new(2.0, 1.0)),
                vec![SketchDimensionKind::Width, SketchDimensionKind::Height],
            ),
            (
                DimensionPhase::Circle,
                SketchGeometry::circle(SketchPoint::new(0.0, 0.0), SketchPoint::new(2.0, 0.0)),
                vec![SketchDimensionKind::Diameter],
            ),
            (
                DimensionPhase::ArcRadius,
                SketchGeometry::segment(SketchPoint::new(0.0, 0.0), SketchPoint::new(2.0, 0.0)),
                vec![SketchDimensionKind::Radius],
            ),
            (
                DimensionPhase::ArcSweep,
                SketchGeometry::arc(
                    SketchPoint::new(0.0, 0.0),
                    SketchPoint::new(2.0, 0.0),
                    SketchPoint::new(0.0, 2.0),
                ),
                vec![
                    SketchDimensionKind::Radius,
                    SketchDimensionKind::SweepDegrees,
                ],
            ),
        ];

        // Suppressing a box must never mean losing the number. A kind kept off
        // the canvas still has to be readable and editable somewhere, and the
        // session readouts are what the sketch panel and the dimension tool
        // both draw from.
        let line = DimensionSession::new(
            DimensionTarget::Draft,
            DimensionPhase::Line,
            SketchGeometry::segment(SketchPoint::new(-2.0, -1.0), SketchPoint::new(2.0, 1.0)),
            1,
        );
        let offered = line
            .readouts()
            .map(|readout| readout.kind)
            .collect::<Vec<_>>();
        for kind in [SketchDimensionKind::DeltaU, SketchDimensionKind::DeltaV] {
            assert!(!kind.shows_on_canvas(), "{kind:?} should be panel-only");
            assert!(
                offered.contains(&kind),
                "{kind:?} vanished from the readouts"
            );
        }

        for (phase, geometry, expected) in cases {
            let state = SketchCanvasState {
                dimension_session: Some(DimensionSession::new(
                    DimensionTarget::Draft,
                    phase,
                    geometry,
                    1,
                )),
                ..SketchCanvasState::default()
            };
            let layouts = dimension_widget_layouts(&state, canvas);
            assert_eq!(
                layouts
                    .iter()
                    .map(|layout| layout.readout.kind)
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(layouts.iter().all(|layout| {
                canvas.contains(layout.rect.min)
                    && canvas.contains(layout.rect.max)
                    && layout.rect.is_positive()
            }));
        }
    }

    type DimensionHarnessState = (
        SketchCanvasState,
        DimensionKeyClaims,
        Option<SketchEntityId>,
    );

    fn dimension_harness(state: SketchCanvasState) -> Harness<'static, DimensionHarnessState> {
        Harness::builder()
            .with_size(Vec2::new(720.0, 500.0))
            .build_ui_state(
                |ui, state: &mut DimensionHarnessState| {
                    let output = show(ui, &mut state.0);
                    state.1.enter |= output.dimension_keys.enter;
                    state.1.escape |= output.dimension_keys.escape;
                    state.1.confirmation_blocked = output.dimension_keys.confirmation_blocked;
                    state.2 = state.2.or(output.pending_created);
                },
                (state, DimensionKeyClaims::default(), None),
            )
    }

    #[test]
    fn bare_enter_claims_the_key_and_stages_one_open_polyline() {
        let mut sketch = SketchCanvasState::default();
        assert!(sketch.set_exact_tool(ToolVariant::ChainedPolyline));
        sketch.handle_creation_click(SketchPoint::new(0.0, 0.0));
        sketch.handle_creation_click(SketchPoint::new(2.0, 0.0));
        let mut harness = dimension_harness(sketch);
        harness.run();
        let viewport = harness.get_by_label("Sketch viewport").rect();
        let third = harness
            .state()
            .0
            .view
            .sketch_to_screen(viewport, SketchPoint::new(2.0, 2.0));
        harness.event(egui::Event::PointerMoved(third));
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton {
                pos: third,
                button: PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        harness.step();

        harness.key_press(egui::Key::Enter);
        harness.step();
        assert!(harness.state().1.enter, "the raw Enter is claimed");
        assert!(harness.state().0.pending().is_some());
        assert_eq!(harness.state().0.pending_entity_count(), 2);
        assert!(harness.state().0.polyline_vertices.is_empty());
    }

    #[test]
    fn backspace_and_escape_are_canvas_owned_polyline_gesture_keys() {
        let mut sketch = SketchCanvasState::default();
        assert!(sketch.set_exact_tool(ToolVariant::ChainedPolyline));
        sketch.handle_creation_click(SketchPoint::new(0.0, 0.0));
        sketch.handle_creation_click(SketchPoint::new(2.0, 0.0));
        let mut harness = dimension_harness(sketch);
        harness.run();
        let viewport = harness.get_by_label("Sketch viewport").rect();
        let third = harness
            .state()
            .0
            .view
            .sketch_to_screen(viewport, SketchPoint::new(2.0, 2.0));
        harness.event(egui::Event::PointerMoved(third));
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton {
                pos: third,
                button: PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        harness.step();
        assert_eq!(harness.state().0.polyline_vertices.len(), 3);

        harness.key_press(egui::Key::Backspace);
        harness.step();
        assert_eq!(harness.state().0.polyline_vertices.len(), 2);

        harness.key_press(egui::Key::Escape);
        harness.step();
        assert!(harness.state().1.escape, "the raw Escape is claimed");
        assert!(!harness.state().0.polyline_current_segment_active);
        assert_eq!(harness.state().0.polyline_vertices.len(), 2);

        harness.state_mut().1.escape = false;
        harness.key_press(egui::Key::Escape);
        harness.step();
        assert!(harness.state().1.escape);
        assert!(harness.state().0.polyline_vertices.is_empty());
    }

    #[test]
    fn pointer_double_click_finishes_without_a_duplicate_terminal_vertex() {
        let mut sketch = SketchCanvasState::default();
        assert!(sketch.set_exact_tool(ToolVariant::ChainedPolyline));
        let mut harness = Harness::builder()
            .with_size(Vec2::new(600.0, 400.0))
            .with_step_dt(0.01)
            .build_ui_state(
                |ui, state| {
                    let _ = show(ui, state);
                },
                sketch,
            );
        harness.run();
        let viewport = harness.get_by_label("Sketch viewport").rect();
        let first = harness
            .state()
            .view
            .sketch_to_screen(viewport, SketchPoint::new(-2.0, 0.0));
        let second = harness
            .state()
            .view
            .sketch_to_screen(viewport, SketchPoint::new(2.0, 0.0));
        click_harness_at(&mut harness, first, egui::Modifiers::NONE);
        assert_eq!(harness.state().polyline_vertices.len(), 1);
        click_harness_at(&mut harness, second, egui::Modifiers::NONE);
        assert_eq!(harness.state().polyline_vertices.len(), 2);
        click_harness_at(&mut harness, second, egui::Modifiers::NONE);

        let pending = harness.state().pending().expect("double-click stages");
        assert_eq!(pending.entities().len(), 1);
        let SketchGeometry::Segment { start, end } = pending.entities()[0].geometry else {
            panic!("two unique polyline vertices produce one segment");
        };
        assert_point_near(start, SketchPoint::new(-2.0, 0.0));
        assert_point_near(end, SketchPoint::new(2.0, 0.0));
        assert!(harness.state().polyline_vertices.is_empty());
    }

    #[test]
    fn tab_exposes_accessible_editor_and_enter_stages_before_global_confirmation() {
        let mut sketch = SketchCanvasState::default();
        assert!(sketch.set_tool(SketchTool::Rectangle));
        sketch.handle_creation_click(SketchPoint::new(-2.0, -1.0));
        sketch.update_dimension_pointer(SketchPoint::new(2.0, 1.0));
        let mut harness = dimension_harness(sketch);
        harness.run();

        assert!(
            harness
                .query_by_role_and_label(Role::Button, "Rectangle width")
                .is_some()
        );
        assert!(
            harness
                .query_by_role_and_label(Role::Button, "Rectangle height")
                .is_some()
        );

        harness.key_press(egui::Key::Tab);
        harness.step();
        harness.step();
        assert_eq!(
            harness.state().0.active_dimension(),
            Some(SketchDimensionKind::Width)
        );
        harness
            .get_by_role_and_label(Role::TextInput, "Rectangle width")
            .type_text("4");
        harness.step();

        harness.key_press(egui::Key::Tab);
        harness.step();
        harness.step();
        assert_eq!(
            harness.state().0.active_dimension(),
            Some(SketchDimensionKind::Height)
        );
        harness
            .get_by_role_and_label(Role::TextInput, "Rectangle height")
            .type_text("2");
        harness.step();
        harness.key_press(egui::Key::Enter);
        harness.step();

        assert!(harness.state().1.enter);
        assert!(harness.state().0.has_pending_edit());
        assert!(harness.state().0.entities().is_empty());
        assert!(harness.state().2.is_some());
        let readouts = harness.state().0.dimension_readouts();
        assert!(readouts.iter().any(|readout| {
            readout.kind == SketchDimensionKind::Width && (readout.value - 4.0).abs() <= EPSILON
        }));
        assert!(readouts.iter().any(|readout| {
            readout.kind == SketchDimensionKind::Height && (readout.value - 2.0).abs() <= EPSILON
        }));

        harness.state_mut().1 = DimensionKeyClaims::default();
        harness.key_press(egui::Key::Enter);
        harness.step();
        assert!(!harness.state().1.enter);
        assert!(harness.state().0.has_pending_edit());
    }

    #[test]
    fn first_escape_reverts_active_field_and_claims_only_that_key() {
        let mut sketch = SketchCanvasState::default();
        assert!(sketch.set_tool(SketchTool::Circle));
        sketch.handle_creation_click(SketchPoint::new(0.0, 0.0));
        sketch.update_dimension_pointer(SketchPoint::new(2.0, 0.0));
        let original = sketch
            .dimension_session
            .as_ref()
            .expect("circle dimensions")
            .geometry;
        let mut harness = dimension_harness(sketch);
        harness.run();
        harness.key_press(egui::Key::Tab);
        harness.step();
        harness.step();
        harness
            .get_by_role_and_label(Role::TextInput, "Circle diameter")
            .type_text("bad");
        harness.step();
        assert!(harness.state().0.dimension_error().is_some());

        harness.key_press(egui::Key::Escape);
        harness.step();
        assert!(harness.state().1.escape);
        assert!(!harness.state().0.dimension_editor_active());
        assert_eq!(
            harness
                .state()
                .0
                .dimension_session
                .as_ref()
                .expect("draft remains")
                .geometry,
            original
        );
        harness.state_mut().1 = DimensionKeyClaims::default();
        harness.key_press(egui::Key::Escape);
        harness.step();
        assert!(!harness.state().1.escape);
    }

    #[test]
    fn selected_committed_geometry_exposes_read_only_accessible_dimensions() {
        let mut sketch = SketchCanvasState::default();
        sketch
            .stage_geometry(SketchGeometry::circle(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(2.0, 0.0),
            ))
            .expect("circle should stage");
        sketch.commit_pending().expect("circle should commit");
        let mut harness = dimension_harness(sketch);
        harness.run();

        assert!(
            harness
                .query_by_role_and_label(Role::Label, "Circle diameter")
                .is_some()
        );
        assert_eq!(
            harness.state().0.dimension_readouts(),
            vec![DimensionReadout {
                kind: SketchDimensionKind::Diameter,
                value: 4.0,
                locked: false,
                editable: false,
            }]
        );
    }

    #[test]
    fn dimension_tool_selects_a_driving_value_and_stages_its_exact_edit() {
        let mut sketch = SketchCanvasState::default();
        let circle = sketch
            .stage_geometry(SketchGeometry::circle(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(2.0, 0.0),
            ))
            .expect("circle should stage");
        sketch.commit_pending().expect("circle should commit");
        assert!(sketch.set_exact_tool(ToolVariant::Dimension));
        assert!(sketch.set_selected(Some(circle)) || sketch.selected() == Some(circle));
        // Editable: the Dimension tool arms this readout because the circle's
        // recipe carries a `"diameter"` literal to drive.
        assert_eq!(
            sketch.dimension_readouts(),
            vec![DimensionReadout {
                kind: SketchDimensionKind::Diameter,
                value: 4.0,
                locked: false,
                editable: true,
            }]
        );
        assert_eq!(
            first_armed_dimension_kind(&sketch),
            Some(SketchDimensionKind::Diameter)
        );
        let editor = sketch
            .selected_recipe_editor()
            .expect("dimension selection exposes its driving recipe");
        assert_eq!(editor.title, "Centre-point circle");
        assert_eq!(editor.parameters[0].stable_key, "diameter");
        assert_eq!(editor.parameters[0].text, "4");

        assert_eq!(
            sketch.set_selected_recipe_parameter_text("diameter", "6".to_owned()),
            Some(circle)
        );
        assert_eq!(
            sketch.pending().map(|pending| pending.label),
            Some("Edit sketch parameters")
        );
        assert_eq!(
            sketch.pending_geometry(),
            Some(SketchGeometry::circle(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(3.0, 0.0),
            ))
        );
        let active_driver_count = sketch.authoring().active_operations().count();
        assert_eq!(sketch.commit_pending(), Ok(circle));
        // Editing a dimension replaces its one authoritative recipe. It does
        // not accumulate a second competing driver (the minimal
        // over-constraint guarantee for the recipe-based sketch solver).
        assert_eq!(
            sketch.authoring().active_operations().count(),
            active_driver_count
        );

        let persisted = sketch.authoring().clone();
        let hydrated = SketchCanvasState::from_authoring(SketchPlane::XY, persisted)
            .expect("dimensioned sketch should rehydrate");
        assert!(hydrated.entities().iter().any(|entity| {
            entity.geometry
                == SketchGeometry::circle(SketchPoint::new(0.0, 0.0), SketchPoint::new(3.0, 0.0))
        }));
    }

    /// A typed value re-authors its subject, so the canvas must stop painting
    /// the original underneath it. Every other staged edit keeps the red
    /// retirement overlay, because there the removal is the point.
    #[test]
    fn in_place_parameter_preview_supersedes_only_its_own_original() {
        let mut sketch = SketchCanvasState::default();
        let circle = sketch
            .stage_geometry(SketchGeometry::circle(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(2.0, 0.0),
            ))
            .expect("circle should stage");
        sketch.commit_pending().expect("circle should commit");
        assert!(sketch.set_selected(Some(circle)) || sketch.selected() == Some(circle));

        assert_eq!(
            sketch.set_selected_recipe_parameter_text("diameter", "6".to_owned()),
            Some(circle)
        );
        assert!(sketch.pending().is_some_and(PendingSketchEdit::is_in_place));
        assert!(sketch.selected_recipe_edit_pending());
        assert_eq!(sketch.superseded_by_in_place_edit(), &[circle]);

        assert!(sketch.revert_selected_recipe_edit());
        assert!(sketch.pending().is_none());
        assert_eq!(
            sketch
                .selected_recipe_editor()
                .expect("the circle stays selected")
                .parameters[0]
                .text,
            "4"
        );

        sketch.stage_delete_selected().expect("delete should stage");
        assert!(!sketch.pending().expect("delete is staged").is_in_place());
        assert!(!sketch.selected_recipe_edit_pending());
        assert!(sketch.superseded_by_in_place_edit().is_empty());
    }

    fn commit_test_line(
        state: &mut SketchCanvasState,
        start: (f64, f64),
        end: (f64, f64),
    ) -> SketchEntityId {
        let id = state
            .stage_geometry(SketchGeometry::segment(
                SketchPoint::new(start.0, start.1),
                SketchPoint::new(end.0, end.1),
            ))
            .expect("stage fixture line");
        assert_eq!(state.commit_pending(), Ok(id));
        id
    }

    fn crossing_trim_fixture() -> (SketchCanvasState, SketchEntityId) {
        let mut state = SketchCanvasState::default();
        let target = commit_test_line(&mut state, (-4.0, 0.0), (4.0, 0.0));
        commit_test_line(&mut state, (-2.0, -2.0), (-2.0, 2.0));
        commit_test_line(&mut state, (0.0, -2.0), (0.0, 2.0));
        commit_test_line(&mut state, (2.0, -2.0), (2.0, 2.0));
        (state, target)
    }

    fn horizontal_segment_ranges(entities: &[SketchEntity]) -> Vec<(f64, f64)> {
        let mut ranges = entities
            .iter()
            .filter_map(|entity| match entity.geometry {
                SketchGeometry::Segment { start, end } if start.v == 0.0 && end.v == 0.0 => {
                    Some((start.u.min(end.u), start.u.max(end.u)))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        ranges.sort_by(|left, right| left.0.total_cmp(&right.0));
        ranges
    }

    #[test]
    fn a_relation_tool_levels_the_line_it_is_clicked_on() {
        let mut state = SketchCanvasState::default();
        let line = commit_test_line(&mut state, (0.0, 0.0), (8.0, 3.0));
        assert!(state.set_exact_tool(ToolVariant::HorizontalRelation));

        // Click the middle of the span, clear of both endpoints, so the curve
        // is the operand rather than a point.
        let subject = state
            .handle_modifier_click(SketchPoint::new(4.0, 1.5), 0.2)
            .expect("one line completes a horizontal relation");
        assert_eq!(subject, line);
        assert!(state.pending().is_some(), "the relation must stage");
        assert_eq!(state.relation_diagnostic(), None);

        assert_eq!(state.commit_pending(), Ok(subject));
        let SketchGeometry::Segment { start, end } = state
            .entities
            .iter()
            .find(|entity| entity.id == line)
            .expect("the line survives")
            .geometry
        else {
            panic!("a line should present as a segment");
        };
        assert!(
            (start.v - end.v).abs() <= 1.0e-9,
            "the presented curve must follow the solved points, got {start:?} {end:?}"
        );
        assert_eq!(state.authoring.constraints().len(), 1);
    }

    #[test]
    fn a_perpendicular_relation_needs_two_lines_and_squares_them() {
        let mut state = SketchCanvasState::default();
        commit_test_line(&mut state, (0.0, 0.0), (8.0, 0.0));
        commit_test_line(&mut state, (0.0, 4.0), (6.0, 7.0));
        assert!(state.set_exact_tool(ToolVariant::PerpendicularRelation));

        assert!(
            state
                .handle_modifier_click(SketchPoint::new(4.0, 0.0), 0.2)
                .is_none(),
            "one line is not yet a perpendicular relation"
        );
        assert_eq!(state.relation_operand_count(), 1);
        let subject = state
            .handle_modifier_click(SketchPoint::new(3.0, 5.5), 0.2)
            .expect("the second line completes the relation");
        assert_eq!(state.commit_pending(), Ok(subject));

        let directions = state
            .entities
            .iter()
            .filter_map(|entity| match entity.geometry {
                SketchGeometry::Segment { start, end } => Some((end.u - start.u, end.v - start.v)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(directions.len(), 2);
        let dot = directions[0]
            .0
            .mul_add(directions[1].0, directions[0].1 * directions[1].1);
        assert!(
            dot.abs() <= 1.0e-7,
            "the lines should be square, dot = {dot}"
        );
    }

    #[test]
    fn a_relation_the_solver_refuses_stages_nothing_and_says_why() {
        let mut state = SketchCanvasState::default();
        commit_test_line(&mut state, (0.0, 0.0), (8.0, 0.0));
        assert!(state.set_exact_tool(ToolVariant::FixedRelation));
        let first = state
            .handle_modifier_click(SketchPoint::new(4.0, 0.0), 0.2)
            .expect("pinning a line stages");
        assert_eq!(state.commit_pending(), Ok(first));

        // The same line pinned twice repeats each point in one system, which
        // the solver names as a duplicate rather than silently absorbing.
        let authoring_before = state.authoring.clone();
        assert!(state.set_exact_tool(ToolVariant::CoincidentRelation));
        assert!(
            state
                .handle_modifier_click(SketchPoint::new(0.0, 0.0), 0.2)
                .is_none()
        );
        assert!(
            state
                .handle_modifier_click(SketchPoint::new(0.0, 0.0), 0.2)
                .is_none(),
            "the same endpoint twice is not a relation"
        );
        assert!(
            state
                .relation_diagnostic()
                .is_some_and(|reason| reason.contains("two different operands")),
            "unexpected diagnostic: {:?}",
            state.relation_diagnostic()
        );
        assert!(state.pending().is_none());
        assert_eq!(state.authoring, authoring_before);
    }

    #[test]
    fn a_tangent_relation_slides_a_line_onto_a_circle() {
        let mut state = SketchCanvasState::default();
        let line = commit_test_line(&mut state, (-4.0, 3.0), (4.0, 3.0));
        let circle = commit_test_circle(&mut state, (0.0, 0.0), 2.0);
        assert!(state.set_exact_tool(ToolVariant::TangentRelation));
        assert!(
            state
                .handle_modifier_click(SketchPoint::new(1.0, 3.0), 0.2)
                .is_none(),
            "one operand is not yet a relation"
        );
        let subject = state
            .handle_modifier_click(SketchPoint::new(0.0, 2.0), 0.2)
            .expect("a line and a circle complete a tangent relation");
        assert_eq!(state.relation_diagnostic(), None);
        assert_eq!(state.commit_pending(), Ok(subject));
        let SketchGeometry::Segment { start, end } = state
            .entities
            .iter()
            .find(|entity| entity.id == line)
            .expect("the line survives")
            .geometry
        else {
            panic!("a line should present as a segment");
        };
        let SketchGeometry::Circle { center, rim } = state
            .entities
            .iter()
            .find(|entity| entity.id == circle)
            .expect("the circle survives")
            .geometry
        else {
            panic!("a circle should present as a circle");
        };
        // Both are free to move, so they meet half way: the line comes down
        // and the circle comes up until the centre is one radius from the
        // line. The presented circle follows its solved centre.
        let radius = center.distance_squared(rim).sqrt();
        let offset = (center.v - start.v).abs();
        assert!((end.v - start.v).abs() <= 1.0e-9, "{start:?} {end:?}");
        assert!(
            (offset - radius).abs() <= 1.0e-9,
            "the line should touch the circle: centre {center:?}, radius {radius}, line at v = {}",
            start.v
        );
        assert!(start.v < 3.0 && center.v > 0.0);
    }

    #[test]
    fn a_collinear_relation_lays_a_line_along_another_without_collapsing_it() {
        let mut state = SketchCanvasState::default();
        commit_test_line(&mut state, (0.0, 0.0), (4.0, 0.0));
        let second = commit_test_line(&mut state, (6.0, 1.0), (10.0, 1.5));
        assert!(state.set_exact_tool(ToolVariant::CollinearRelation));
        assert!(
            state
                .handle_modifier_click(SketchPoint::new(2.0, 0.0), 0.2)
                .is_none()
        );
        let subject = state
            .handle_modifier_click(SketchPoint::new(8.0, 1.25), 0.2)
            .expect("two lines complete a collinear relation");
        assert_eq!(state.relation_diagnostic(), None);
        assert_eq!(state.commit_pending(), Ok(subject));
        let SketchGeometry::Segment { start, end } = state
            .entities
            .iter()
            .find(|entity| entity.id == second)
            .expect("the second line survives")
            .geometry
        else {
            panic!("a line should present as a segment");
        };
        assert!(
            start.v.abs() <= 1.0e-9 && end.v.abs() <= 1.0e-9,
            "{start:?} {end:?}"
        );
        assert!(
            start.u > 4.0 && end.u > start.u,
            "the line stays beyond the first, end to end: {start:?} {end:?}"
        );
    }

    #[test]
    fn a_recipe_owned_curve_refuses_a_relation_by_name() {
        let mut state = SketchCanvasState::default();
        state
            .stage_recipe(
                CoreRecipe::TwoPointRectangle {
                    first_corner: CorePointInput::Position(CorePoint2::new(0.0, 0.0)),
                    width: CoreValue::Literal(CoreSignedLength::new(6.0).expect("finite width")),
                    height: CoreValue::Literal(CoreSignedLength::new(4.0).expect("finite height")),
                },
                "Rectangle",
            )
            .expect("stage a rectangle");
        state.commit_pending().expect("commit the rectangle");
        commit_test_line(&mut state, (-4.0, -4.0), (-1.0, -2.0));

        assert!(state.set_exact_tool(ToolVariant::ParallelRelation));
        assert!(
            state
                .handle_modifier_click(SketchPoint::new(-2.5, -3.0), 0.2)
                .is_none()
        );
        assert!(
            state
                .handle_modifier_click(SketchPoint::new(3.0, 0.0), 0.2)
                .is_none(),
            "a recipe-owned side must refuse"
        );
        assert!(
            state
                .relation_diagnostic()
                .is_some_and(|reason| reason.contains("recipe feature")),
            "unexpected diagnostic: {:?}",
            state.relation_diagnostic()
        );
        assert!(state.pending().is_none());
    }

    #[test]
    fn trim_hover_selects_only_the_exact_line_span_in_the_current_candidate() {
        let (mut state, _) = crossing_trim_fixture();
        assert!(state.set_exact_tool(ToolVariant::Trim));
        assert!(state.update_trim_hover(Some(SketchPoint::new(-1.0, 0.0)), 0.25));
        let CoreEvaluatedCurve2::Line { start, end } = state
            .trim_hover_fragment
            .clone()
            .expect("middle line fragment")
        else {
            panic!("line trim hover must remain an exact line subcurve")
        };
        assert_eq!((start.u, end.u), (-2.0, 0.0));

        state
            .handle_modifier_click(SketchPoint::new(-1.0, 0.0), 0.25)
            .expect("first exact span stages");
        assert!(state.update_trim_hover(Some(SketchPoint::new(3.0, 0.0)), 0.25));
        let CoreEvaluatedCurve2::Line { start, end } = state
            .trim_hover_fragment
            .clone()
            .expect("retained candidate's outer fragment")
        else {
            panic!("repeated trim hover must query the evolving candidate")
        };
        assert_eq!((start.u, end.u), (2.0, 4.0));

        assert!(state.update_trim_hover(None, 0.25));
        assert!(state.trim_hover_fragment.is_none());
    }

    #[test]
    fn trim_hover_keeps_circle_and_arc_fragments_analytic() {
        let mut circle = SketchCanvasState::default();
        commit_test_circle(&mut circle, (0.0, 0.0), 2.0);
        commit_test_line(&mut circle, (0.0, -3.0), (0.0, 3.0));
        assert!(circle.set_exact_tool(ToolVariant::Trim));
        circle.update_trim_hover(Some(SketchPoint::new(2.0, 0.0)), 0.25);
        assert!(matches!(
            circle.trim_hover_fragment,
            Some(CoreEvaluatedCurve2::CircularArc { .. })
        ));

        let mut arc = SketchCanvasState::default();
        let staged = arc
            .stage_geometry(SketchGeometry::arc(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(2.0, 0.0),
                SketchPoint::new(-2.0, 0.0),
            ))
            .expect("upper semicircle stages");
        arc.commit_pending().expect("upper semicircle commits");
        assert_eq!(arc.selected(), Some(staged));
        commit_test_line(&mut arc, (-1.0, -3.0), (-1.0, 3.0));
        commit_test_line(&mut arc, (1.0, -3.0), (1.0, 3.0));
        assert!(arc.set_exact_tool(ToolVariant::Trim));
        arc.update_trim_hover(Some(SketchPoint::new(0.0, 2.0)), 0.25);
        let Some(CoreEvaluatedCurve2::CircularArc { start, end, .. }) = arc.trim_hover_fragment
        else {
            panic!("arc trim hover must remain an exact analytic arc fragment")
        };
        assert!((start.u.abs() - 1.0).abs() <= EPSILON);
        assert!((end.u.abs() - 1.0).abs() <= EPSILON);
        assert!((start.v - 3_f64.sqrt()).abs() <= EPSILON);
        assert!((end.v - 3_f64.sqrt()).abs() <= EPSILON);
    }

    #[test]
    fn trim_click_removes_only_the_crossing_shapes_middle_span_atomically() {
        let mut state = SketchCanvasState::default();
        let target = commit_test_line(&mut state, (-4.0, 0.0), (4.0, 0.0));
        commit_test_line(&mut state, (-1.0, -2.0), (-1.0, 2.0));
        commit_test_line(&mut state, (1.0, -2.0), (1.0, 2.0));
        let committed_before = state.entities.clone();
        let authoring_before = state.authoring.clone();

        assert!(state.set_exact_tool(ToolVariant::Trim));
        let staged = state
            .handle_modifier_click(SketchPoint::new(0.0, 0.0), 0.25)
            .expect("middle span should stage");
        let pending = state.pending().expect("trim preview");
        assert_eq!(pending.entities().len(), 2);
        assert_eq!(pending.retired_entities(), &[target]);
        assert_eq!(state.entities, committed_before);
        assert_eq!(state.authoring, authoring_before);

        assert_eq!(state.commit_pending(), Ok(staged));
        assert_eq!(state.authoring.active_entities().count(), 4);
        assert_eq!(state.entities().len(), 4);
        assert!(!state.entities().iter().any(|entity| entity.id == target));
        let retained = state
            .entities()
            .iter()
            .filter_map(|entity| match entity.geometry {
                SketchGeometry::Segment { start, end }
                    if start.v.abs() < 1.0e-9 && end.v.abs() < 1.0e-9 =>
                {
                    Some((start.u.min(end.u), start.u.max(end.u)))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(retained.contains(&(-4.0, -1.0)));
        assert!(retained.contains(&(1.0, 4.0)));
    }

    #[test]
    fn repeated_trim_clicks_replace_the_preview_and_commit_once() {
        let (mut state, target) = crossing_trim_fixture();
        let revision_before = state.authoring.revision();
        let authoring_before = state.authoring.clone();
        let next_id_before = state.next_entity_id;

        assert!(state.set_exact_tool(ToolVariant::Trim));
        let subject = state
            .handle_modifier_click(SketchPoint::new(-1.0, 0.0), 0.25)
            .expect("first span stages the batch");
        assert_eq!(
            horizontal_segment_ranges(state.pending().expect("first preview").entities()),
            vec![(-4.0, -2.0), (0.0, 2.0), (2.0, 4.0)]
        );
        let same_subject = state
            .handle_modifier_click(SketchPoint::new(3.0, 0.0), 0.25)
            .expect("second click trims the retained candidate span");
        assert_eq!(same_subject, subject);
        let pending = state.pending().expect("batched preview");
        assert_eq!(pending.subject(), subject);
        assert_eq!(pending.retired_entities(), &[target]);
        assert_eq!(
            horizontal_segment_ranges(pending.entities()),
            vec![(-4.0, -2.0), (0.0, 2.0)]
        );
        assert_eq!(state.authoring, authoring_before);
        assert_eq!(state.next_entity_id, next_id_before);

        assert_eq!(state.commit_pending(), Ok(subject));
        assert_eq!(
            state.authoring.revision(),
            CoreSketchRevision::new(revision_before.get() + 1)
        );
        assert_eq!(
            horizontal_segment_ranges(state.entities()),
            vec![(-4.0, -2.0), (0.0, 2.0)]
        );
    }

    #[test]
    fn cancelling_repeated_trim_is_revision_and_id_neutral() {
        let (mut state, _) = crossing_trim_fixture();
        let authoring_before = state.authoring.clone();
        let entities_before = state.entities.clone();
        let next_id_before = state.next_entity_id;
        assert!(state.set_exact_tool(ToolVariant::Trim));
        state
            .handle_modifier_click(SketchPoint::new(-1.0, 0.0), 0.25)
            .expect("first span");
        state
            .handle_modifier_click(SketchPoint::new(3.0, 0.0), 0.25)
            .expect("second span");

        assert!(state.cancel_pending().is_some());
        assert_eq!(state.authoring, authoring_before);
        assert_eq!(state.entities, entities_before);
        assert_eq!(state.next_entity_id, next_id_before);
    }

    #[test]
    fn rejected_second_trim_click_preserves_the_first_preview() {
        let (mut state, _) = crossing_trim_fixture();
        assert!(state.set_exact_tool(ToolVariant::Trim));
        let subject = state
            .handle_modifier_click(SketchPoint::new(-1.0, 0.0), 0.25)
            .expect("first span");
        let (preview_before, impact_before, entities_before, retired_before) = {
            let pending = state.pending().expect("first preview");
            let transaction = pending.core_transaction.as_ref().expect("core trim");
            (
                transaction.preview().clone(),
                transaction.impact().clone(),
                pending.entities.clone(),
                pending.retired_entities.clone(),
            )
        };

        assert_eq!(
            state.handle_modifier_click(SketchPoint::new(-2.0, 0.0), 0.1),
            None,
            "a junction pick has no unique adjacent span"
        );
        let pending = state.pending().expect("first preview retained");
        let transaction = pending.core_transaction.as_ref().expect("core trim");
        assert_eq!(pending.subject(), subject);
        assert_eq!(pending.entities, entities_before);
        assert_eq!(pending.retired_entities, retired_before);
        assert_eq!(transaction.preview(), &preview_before);
        assert_eq!(transaction.impact(), &impact_before);
    }

    #[test]
    fn repeated_trim_is_one_local_undo_redo_entry() {
        let (mut state, _) = crossing_trim_fixture();
        state.undo_journal = CoreUndoJournal::new(128);
        let revision_before = state.authoring.revision();
        assert!(state.set_exact_tool(ToolVariant::Trim));
        state
            .handle_modifier_click(SketchPoint::new(-1.0, 0.0), 0.25)
            .expect("first span");
        let subject = state
            .handle_modifier_click(SketchPoint::new(3.0, 0.0), 0.25)
            .expect("second span");
        assert_eq!(state.commit_pending(), Ok(subject));
        let committed_revision = state.authoring.revision();
        assert_eq!(
            horizontal_segment_ranges(state.entities()),
            vec![(-4.0, -2.0), (0.0, 2.0)]
        );

        assert!(state.undo_local());
        assert_eq!(state.authoring.revision(), revision_before);
        assert_eq!(
            horizontal_segment_ranges(state.entities()),
            vec![(-4.0, 4.0)]
        );
        assert!(
            !state.can_undo_local(),
            "the whole batch is one journal entry"
        );

        assert!(state.redo_local());
        assert_eq!(state.authoring.revision(), committed_revision);
        assert_eq!(
            horizontal_segment_ranges(state.entities()),
            vec![(-4.0, -2.0), (0.0, 2.0)]
        );
        assert!(!state.can_redo_local());
    }

    #[test]
    fn cancelling_trim_or_pattern_is_revision_and_id_neutral() {
        let mut state = SketchCanvasState::default();
        commit_test_line(&mut state, (-4.0, 0.0), (4.0, 0.0));
        commit_test_line(&mut state, (-1.0, -2.0), (-1.0, 2.0));
        commit_test_line(&mut state, (1.0, -2.0), (1.0, 2.0));
        let authoring_before = state.authoring.clone();
        let entities_before = state.entities.clone();
        let next_id_before = state.next_entity_id;

        assert!(state.set_exact_tool(ToolVariant::Trim));
        state
            .handle_modifier_click(SketchPoint::new(0.0, 0.0), 0.25)
            .expect("trim preview");
        assert!(state.cancel_pending().is_some());
        assert_eq!(state.authoring, authoring_before);
        assert_eq!(state.entities, entities_before);
        assert_eq!(state.next_entity_id, next_id_before);

        assert!(state.set_selected(Some(SketchEntityId(2))));
        assert!(state.set_exact_tool(ToolVariant::RectangularPattern));
        let anchor = state.pattern_anchor().expect("selected pattern seed");
        state
            .handle_modifier_click(SketchPoint::new(anchor.u + 2.0, anchor.v), 0.25)
            .expect("pattern preview");
        assert!(state.cancel_pending().is_some());
        assert_eq!(state.authoring, authoring_before);
        assert_eq!(state.entities, entities_before);
        assert_eq!(state.next_entity_id, next_id_before);
    }

    #[test]
    fn connected_line_line_fillet_and_chamfer_have_exact_atomic_previews() {
        for (tool, expects_arc) in [(ToolVariant::Fillet, true), (ToolVariant::Chamfer, false)] {
            let mut state = SketchCanvasState::default();
            commit_test_line(&mut state, (0.0, 0.0), (5.0, 0.0));
            commit_test_line(&mut state, (5.0, 0.0), (5.0, 5.0));
            let before = state.authoring.clone();
            assert!(state.set_exact_tool(tool));
            assert_eq!(
                state.handle_modifier_click(SketchPoint::new(2.0, 0.0), 0.3),
                None
            );
            let staged = state
                .handle_modifier_click(SketchPoint::new(5.0, 2.0), 0.3)
                .expect("connected corner should stage");
            let pending = state.pending().expect("corner preview");
            assert_eq!(pending.retired_entities().len(), 2);
            assert_eq!(pending.entities().len(), 3);
            assert_eq!(
                pending
                    .entities()
                    .iter()
                    .any(|entity| matches!(entity.geometry, SketchGeometry::Arc { .. })),
                expects_arc
            );
            assert_eq!(state.authoring, before, "preview must not mutate truth");
            assert_eq!(state.commit_pending(), Ok(staged));
            assert_eq!(state.entities().len(), 3);
            assert_eq!(state.authoring.active_entities().count(), 3);
        }
    }

    #[test]
    fn multiple_fillet_and_chamfer_targets_share_one_visible_confirmation() {
        for tool in [ToolVariant::Fillet, ToolVariant::Chamfer] {
            let mut state = SketchCanvasState::default();
            commit_test_line(&mut state, (0.0, 0.0), (10.0, 0.0));
            commit_test_line(&mut state, (10.0, 0.0), (10.0, 10.0));
            commit_test_line(&mut state, (10.0, 10.0), (0.0, 10.0));
            commit_test_line(&mut state, (0.0, 10.0), (0.0, 0.0));
            let revision_before = state.authoring.revision();
            let authoring_before = state.authoring.clone();

            assert!(state.set_exact_tool(tool));
            assert_eq!(
                state.handle_modifier_click(SketchPoint::new(8.0, 10.0), 0.3),
                None
            );
            let subject = state
                .handle_modifier_click(SketchPoint::new(10.0, 8.0), 0.3)
                .expect("first corner preview");
            assert!(state.has_pending_edit());

            assert_eq!(
                state.handle_modifier_click(SketchPoint::new(2.0, 0.0), 0.3),
                None,
                "the first carrier of the next corner remains visibly selected"
            );
            assert_eq!(state.modifier_sources.len(), 1);
            assert_eq!(
                state
                    .handle_modifier_click(SketchPoint::new(0.0, 2.0), 0.3)
                    .expect("second corner appends"),
                subject
            );
            assert!(state.modifier_sources.is_empty());
            assert_eq!(state.authoring, authoring_before, "batch remains a preview");

            assert_eq!(state.commit_pending(), Ok(subject));
            assert_eq!(
                state.authoring.revision(),
                CoreSketchRevision::new(revision_before.get() + 1),
                "all corners publish as one sketch revision"
            );
            assert_eq!(state.authoring.active_entities().count(), 6);
        }
    }

    fn pending_recipe(state: &SketchCanvasState) -> &CoreRecipe {
        &state
            .pending
            .as_ref()
            .expect("pending edit")
            .core_transaction
            .as_ref()
            .expect("exact transaction")
            .preview()
            .operations()
            .last()
            .expect("staged operation")
            .recipe
    }

    fn literal_length(value: CoreValue<CoreLength>) -> f64 {
        let CoreValue::Literal(value) = value else {
            panic!("test recipe should retain a literal length")
        };
        value.get()
    }

    fn literal_signed_length(value: CoreValue<CoreSignedLength>) -> f64 {
        let CoreValue::Literal(value) = value else {
            panic!("test recipe should retain a literal signed length")
        };
        value.get()
    }

    fn literal_integer(value: CoreValue<CoreInteger>) -> u16 {
        let CoreValue::Literal(value) = value else {
            panic!("test recipe should retain a literal integer")
        };
        value.get()
    }

    fn literal_angle(value: CoreValue<CoreAngle>) -> f64 {
        let CoreValue::Literal(value) = value else {
            panic!("test recipe should retain a literal angle")
        };
        value.get()
    }

    #[test]
    fn analytic_fillet_retains_radius_model_picks_and_deterministic_corner_hint() {
        let mut state = SketchCanvasState::default();
        commit_test_line(&mut state, (0.0, 0.0), (12.0, 0.0));
        commit_test_circle(&mut state, (5.0, 3.0), 5.0);
        assert!(state.set_exact_tool(ToolVariant::Fillet));
        assert!(state.set_active_tool_input_text("radius", "0.75".to_owned()));
        let first_pick = SketchPoint::new(11.0, 0.0);
        let second_pick = SketchPoint::new(5.0, 8.0);
        assert_eq!(state.handle_modifier_click(first_pick, 0.3), None);
        state
            .handle_modifier_click(second_pick, 0.3)
            .expect("line-circle fillet should stage");

        let CoreRecipe::FilletWithHints { radius, hints, .. } = pending_recipe(&state) else {
            panic!("the app must stage the branch-explicit analytic fillet recipe")
        };
        assert!((literal_length(*radius) - 0.75).abs() <= EPSILON);
        assert_eq!(hints.first_pick, core_point(first_pick));
        assert_eq!(hints.second_pick, core_point(second_pick));
        assert!((hints.corner_hint.u - 9.0).abs() <= EPSILON);
        assert!(hints.corner_hint.v.abs() <= EPSILON);
    }

    #[test]
    fn equal_and_two_distance_chamfers_persist_the_exact_visible_setbacks() {
        for (tool, first_value, second_value) in [
            (ToolVariant::Chamfer, 0.75, 0.75),
            (ToolVariant::TwoDistanceChamfer, 0.5, 1.25),
        ] {
            let mut state = SketchCanvasState::default();
            commit_test_line(&mut state, (0.0, 0.0), (5.0, 0.0));
            commit_test_line(&mut state, (5.0, 0.0), (5.0, 5.0));
            assert!(state.set_exact_tool(tool));
            assert!(state.set_active_tool_input_text("distance_1", first_value.to_string(),));
            if tool == ToolVariant::TwoDistanceChamfer {
                assert!(state.set_active_tool_input_text("distance_2", second_value.to_string(),));
            }
            state.handle_modifier_click(SketchPoint::new(2.0, 0.0), 0.3);
            state
                .handle_modifier_click(SketchPoint::new(5.0, 2.0), 0.3)
                .expect("chamfer should stage");
            let CoreRecipe::Chamfer {
                first_distance,
                second_distance,
                ..
            } = pending_recipe(&state)
            else {
                panic!("chamfer recipe")
            };
            assert!((literal_length(*first_distance) - first_value).abs() <= EPSILON);
            assert!((literal_length(*second_distance) - second_value).abs() <= EPSILON);
        }
    }

    #[test]
    fn non_default_pattern_controls_reach_the_exact_staged_recipes() {
        let mut rectangular = SketchCanvasState::default();
        commit_test_line(&mut rectangular, (0.0, 0.0), (2.0, 0.0));
        assert!(rectangular.set_exact_tool(ToolVariant::RectangularPattern));
        for (key, value) in [
            ("count_u", "4"),
            ("spacing_u", "-3"),
            ("count_v", "3"),
            ("spacing_v", "7"),
        ] {
            assert!(rectangular.set_active_tool_input_text(key, value.to_owned()));
        }
        assert!(rectangular.set_active_tool_flag("second_direction", true));
        let anchor = rectangular.pattern_anchor().expect("pattern anchor");
        rectangular
            .handle_modifier_click(SketchPoint::new(anchor.u, anchor.v + 2.0), 0.2)
            .expect("two-direction rectangular pattern should stage");
        let CoreRecipe::RectangularPattern {
            columns,
            rows,
            column_spacing,
            row_spacing,
            direction,
            ..
        } = pending_recipe(&rectangular)
        else {
            panic!("rectangular pattern recipe")
        };
        assert_eq!(literal_integer(*columns), 4);
        assert_eq!(literal_integer(*rows), 3);
        assert_eq!(literal_signed_length(*column_spacing), -3.0);
        assert_eq!(literal_signed_length(*row_spacing), 7.0);
        assert!((literal_angle(*direction) - std::f64::consts::FRAC_PI_2).abs() <= EPSILON);
        assert_eq!(rectangular.pending().expect("preview").entities().len(), 11);

        let mut circular = SketchCanvasState::default();
        commit_test_line(&mut circular, (2.0, 0.0), (4.0, 0.0));
        assert!(circular.set_exact_tool(ToolVariant::CircularPattern));
        assert!(circular.set_active_tool_input_text("count", "5".to_owned()));
        assert!(circular.set_active_tool_flag("full_circle", false));
        assert!(circular.set_active_tool_input_text("extent", "180".to_owned()));
        assert!(circular.set_active_tool_flag("rotate_instances", false));
        circular
            .handle_modifier_click(SketchPoint::new(0.0, 0.0), 0.2)
            .expect("extent circular pattern should stage");
        let CoreRecipe::CircularPattern {
            count,
            total_angle,
            distribution,
            rotate_instances,
            ..
        } = pending_recipe(&circular)
        else {
            panic!("circular pattern recipe")
        };
        assert_eq!(literal_integer(*count), 5);
        assert!((literal_angle(*total_angle) - std::f64::consts::PI).abs() <= EPSILON);
        assert_eq!(*distribution, CoreCircularPatternDistribution::Extent);
        assert!(!*rotate_instances);
    }

    #[test]
    fn polygon_and_slot_typed_controls_are_truthful_recipe_parameters() {
        let mut polygon = SketchCanvasState::default();
        assert!(polygon.set_exact_tool(ToolVariant::InnerDiameterPolygon));
        for (key, value) in [("sides", "7"), ("inner_diameter", "12"), ("rotation", "30")] {
            assert!(polygon.set_active_tool_input_text(key, value.to_owned()));
        }
        polygon.handle_creation_click(SketchPoint::new(1.0, 2.0));
        polygon
            .handle_creation_click(SketchPoint::new(3.0, 2.0))
            .expect("typed polygon should stage");
        let CoreRecipe::InnerDiameterPolygon {
            inner_diameter,
            sides,
            rotation,
            ..
        } = pending_recipe(&polygon)
        else {
            panic!("inner polygon recipe")
        };
        assert_eq!(literal_integer(*sides), 7);
        assert!((literal_length(*inner_diameter) - 12.0).abs() <= EPSILON);
        assert!((literal_angle(*rotation) - 30_f64.to_radians()).abs() <= EPSILON);

        let mut slot = SketchCanvasState::default();
        assert!(slot.set_exact_tool(ToolVariant::TwoPointSlot));
        for (key, value) in [("centre_distance", "6"), ("width", "2"), ("angle", "30")] {
            assert!(slot.set_active_tool_input_text(key, value.to_owned()));
        }
        slot.handle_creation_click(SketchPoint::new(0.0, 0.0));
        slot.handle_creation_click(SketchPoint::new(1.0, 0.0));
        slot.handle_creation_click(SketchPoint::new(0.0, 1.0))
            .expect("typed slot should stage");
        let CoreRecipe::TwoPointSlot {
            first_cap_center,
            second_cap_center,
            width,
        } = pending_recipe(&slot)
        else {
            panic!("two-point slot recipe")
        };
        let (CorePointInput::Position(first), CorePointInput::Position(second)) =
            (first_cap_center, second_cap_center)
        else {
            panic!("slot gesture should retain literal cap centres")
        };
        assert_eq!(*first, CorePoint2::new(0.0, 0.0));
        assert!((first.distance(*second) - 6.0).abs() <= EPSILON);
        assert!((literal_length(*width) - 2.0).abs() <= EPSILON);
        assert!((second.v.atan2(second.u) - 30_f64.to_radians()).abs() <= EPSILON);

        let mut centre_slot = SketchCanvasState::default();
        assert!(centre_slot.set_exact_tool(ToolVariant::CentreToOuterPointSlot));
        for (key, value) in [("overall_length", "8"), ("width", "2"), ("angle", "-45")] {
            assert!(centre_slot.set_active_tool_input_text(key, value.to_owned()));
        }
        centre_slot.handle_creation_click(SketchPoint::new(1.0, 1.0));
        centre_slot.handle_creation_click(SketchPoint::new(2.0, 1.0));
        centre_slot
            .handle_creation_click(SketchPoint::new(1.0, 2.0))
            .expect("typed centre slot should stage");
        let CoreRecipe::CentreOuterPointSlot {
            overall_length,
            width,
            angle,
            ..
        } = pending_recipe(&centre_slot)
        else {
            panic!("centre-outer slot recipe")
        };
        assert!((literal_length(*overall_length) - 8.0).abs() <= EPSILON);
        assert!((literal_length(*width) - 2.0).abs() <= EPSILON);
        assert!((literal_angle(*angle) - (-45_f64).to_radians()).abs() <= EPSILON);
    }

    #[test]
    fn invalid_typed_pattern_text_retains_preview_value_and_blocks_staging() {
        let mut state = SketchCanvasState::default();
        commit_test_line(&mut state, (0.0, 0.0), (2.0, 0.0));
        assert!(state.set_exact_tool(ToolVariant::RectangularPattern));
        assert!(state.set_active_tool_input_text("spacing_u", "9".to_owned()));
        assert!(state.set_active_tool_input_text("spacing_u", "invalid".to_owned()));
        assert_eq!(
            state.active_tool_input_error("spacing_u"),
            Some(ToolInputError::NotANumber)
        );
        assert_eq!(state.active_tool_number("spacing_u"), Some(9.0));
        let anchor = state.pattern_anchor().expect("pattern anchor");
        assert!(
            (state
                .pattern_manipulator
                .expect("typed spacing keeps a visible handle")
                .position
                .distance_squared(anchor)
                .sqrt()
                - 9.0)
                .abs()
                <= EPSILON,
            "invalid correction retains the last valid typed handle distance"
        );
        assert_eq!(
            state.handle_modifier_click(SketchPoint::new(anchor.u + 4.0, anchor.v), 0.2),
            None
        );
        assert!(!state.has_pending_edit());
        assert!(state.restore_active_tool_input("spacing_u"));
        state
            .handle_modifier_click(SketchPoint::new(anchor.u + 4.0, anchor.v), 0.2)
            .expect("restored last-valid spacing should stage");
        let CoreRecipe::RectangularPattern { column_spacing, .. } = pending_recipe(&state) else {
            panic!("rectangular pattern recipe")
        };
        assert_eq!(literal_signed_length(*column_spacing), 9.0);
    }

    #[test]
    fn inactive_pattern_fields_neither_poison_preview_nor_block_staging() {
        let mut rectangular = SketchCanvasState::default();
        commit_test_line(&mut rectangular, (0.0, 0.0), (2.0, 0.0));
        assert!(rectangular.set_exact_tool(ToolVariant::RectangularPattern));
        assert!(rectangular.set_active_tool_input_text("count_v", "invalid".to_owned()));
        assert_eq!(rectangular.active_tool_parameter_issue(), None);
        let anchor = rectangular.pattern_anchor().expect("pattern anchor");
        rectangular
            .handle_modifier_click(SketchPoint::new(anchor.u + 3.0, anchor.v), 0.2)
            .expect("disabled second direction must ignore its retained invalid field");

        let mut circular = SketchCanvasState::default();
        commit_test_line(&mut circular, (2.0, 0.0), (4.0, 0.0));
        assert!(circular.set_exact_tool(ToolVariant::CircularPattern));
        assert!(circular.set_active_tool_input_text("extent", "0".to_owned()));
        assert_eq!(circular.active_tool_parameter_issue(), None);
        circular
            .handle_modifier_click(SketchPoint::new(0.0, 0.0), 0.2)
            .expect("complete distribution must ignore its inactive extent field");
    }

    #[test]
    fn rectangular_and_circular_patterns_stage_bounded_atomic_instances() {
        let mut rectangular = SketchCanvasState::default();
        let seed = commit_test_line(&mut rectangular, (0.0, 0.0), (2.0, 0.0));
        assert_eq!(rectangular.selected(), Some(seed));
        assert!(rectangular.set_exact_tool(ToolVariant::RectangularPattern));
        let anchor = rectangular.pattern_anchor().expect("pattern anchor");
        let staged = rectangular
            .handle_modifier_click(SketchPoint::new(anchor.u + 3.0, anchor.v), 0.2)
            .expect("rectangular pattern preview");
        assert_eq!(
            rectangular
                .pending()
                .expect("pattern pending")
                .entities()
                .len(),
            2
        );
        assert_eq!(rectangular.commit_pending(), Ok(staged));
        assert_eq!(rectangular.entities().len(), 3);

        let mut circular = SketchCanvasState::default();
        let seed = commit_test_line(&mut circular, (2.0, 0.0), (4.0, 0.0));
        assert_eq!(circular.selected(), Some(seed));
        assert!(circular.set_exact_tool(ToolVariant::CircularPattern));
        let staged = circular
            .handle_modifier_click(SketchPoint::new(0.0, 0.0), 0.2)
            .expect("circular pattern preview");
        assert_eq!(
            circular
                .pending()
                .expect("pattern pending")
                .entities()
                .len(),
            3
        );
        assert_eq!(circular.commit_pending(), Ok(staged));
        assert_eq!(circular.entities().len(), 4);
    }

    fn pattern_harness(state: SketchCanvasState) -> Harness<'static, SketchCanvasState> {
        Harness::builder()
            .with_size(Vec2::new(720.0, 500.0))
            .build_ui_state(
                |ui, state| {
                    let _ = show(ui, state);
                },
                state,
            )
    }

    fn pattern_pointer_button(
        harness: &mut Harness<'_, SketchCanvasState>,
        position: Pos2,
        pressed: bool,
    ) {
        harness.event(egui::Event::PointerMoved(position));
        harness.event(egui::Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();
    }

    #[test]
    fn rectangular_pattern_square_handle_drags_continuously_then_stages_on_release() {
        let mut state = SketchCanvasState::default();
        commit_test_line(&mut state, (0.0, 0.0), (2.0, 0.0));
        assert!(state.set_exact_tool(ToolVariant::RectangularPattern));
        let anchor = state.pattern_anchor().expect("selected seed anchor");
        let mut harness = pattern_harness(state);
        harness.run();
        let viewport = harness.get_by_label("Sketch viewport").rect();
        let handle = harness
            .state()
            .pattern_manipulator
            .expect("retained direction handle")
            .position;
        let handle_screen = harness.state().view.sketch_to_screen(viewport, handle);
        let target = SketchPoint::new(anchor.u, anchor.v + 4.0);
        let target_screen = harness.state().view.sketch_to_screen(viewport, target);

        pattern_pointer_button(&mut harness, handle_screen, true);
        assert!(
            harness
                .state()
                .pattern_manipulator
                .is_some_and(|manipulator| manipulator.dragging)
        );
        assert!(!harness.state().has_pending_edit());

        harness.event(egui::Event::PointerMoved(target_screen));
        harness.step();
        assert_eq!(harness.state().active_tool_number("spacing_u"), Some(4.0));
        assert_point_near(
            harness
                .state()
                .pattern_manipulator
                .expect("drag remains retained")
                .position,
            target,
        );
        assert!(!harness.state().has_pending_edit());

        pattern_pointer_button(&mut harness, target_screen, false);
        let CoreRecipe::RectangularPattern {
            column_spacing,
            direction,
            ..
        } = pending_recipe(harness.state())
        else {
            panic!("release stages one exact rectangular pattern")
        };
        assert!((literal_signed_length(*column_spacing) - 4.0).abs() <= EPSILON);
        assert!((literal_angle(*direction) - std::f64::consts::FRAC_PI_2).abs() <= EPSILON);
    }

    #[test]
    fn circular_pattern_centre_handle_drags_then_stages_the_released_centre() {
        let mut state = SketchCanvasState::default();
        commit_test_line(&mut state, (2.0, 0.0), (4.0, 0.0));
        assert!(state.set_exact_tool(ToolVariant::CircularPattern));
        let mut harness = pattern_harness(state);
        harness.run();
        let viewport = harness.get_by_label("Sketch viewport").rect();
        let handle = harness
            .state()
            .pattern_manipulator
            .expect("retained centre handle")
            .position;
        let handle_screen = harness.state().view.sketch_to_screen(viewport, handle);
        let target = SketchPoint::new(0.0, 2.0);
        let target_screen = harness.state().view.sketch_to_screen(viewport, target);

        pattern_pointer_button(&mut harness, handle_screen, true);
        harness.event(egui::Event::PointerMoved(target_screen));
        harness.step();
        assert_point_near(
            harness
                .state()
                .pattern_manipulator
                .expect("centre drag remains live")
                .position,
            target,
        );
        assert!(!harness.state().has_pending_edit());

        pattern_pointer_button(&mut harness, target_screen, false);
        let CoreRecipe::CircularPattern {
            center: CorePointInput::Position(center),
            ..
        } = pending_recipe(harness.state())
        else {
            panic!("release stages one exact circular pattern")
        };
        assert_eq!(*center, core_point(target));
    }

    #[test]
    fn over_limit_pattern_drag_is_revision_and_identity_neutral() {
        let mut state = SketchCanvasState::default();
        commit_test_line(&mut state, (0.0, 0.0), (2.0, 0.0));
        assert!(state.set_exact_tool(ToolVariant::RectangularPattern));
        assert!(state.set_active_tool_input_text("count_u", "256".to_owned()));
        assert!(state.set_active_tool_input_text("count_v", "2".to_owned()));
        assert!(state.set_active_tool_flag("second_direction", true));
        assert_eq!(
            state.active_tool_parameter_issue(),
            Some(ToolInputError::PatternLimit)
        );
        let authoring_before = state.authoring.clone();
        let next_id_before = state.next_entity_id;
        let mut harness = pattern_harness(state);
        harness.run();
        let viewport = harness.get_by_label("Sketch viewport").rect();
        let handle = harness
            .state()
            .pattern_manipulator
            .expect("retained direction handle")
            .position;
        let handle_screen = harness.state().view.sketch_to_screen(viewport, handle);
        let target_screen = harness
            .state()
            .view
            .sketch_to_screen(viewport, SketchPoint::new(1.0, 4.0));

        pattern_pointer_button(&mut harness, handle_screen, true);
        harness.event(egui::Event::PointerMoved(target_screen));
        harness.step();
        pattern_pointer_button(&mut harness, target_screen, false);

        assert!(!harness.state().has_pending_edit());
        assert_eq!(harness.state().authoring, authoring_before);
        assert_eq!(harness.state().next_entity_id, next_id_before);
        assert!(
            harness
                .state()
                .pattern_manipulator
                .is_some_and(|manipulator| {
                    !manipulator.dragging
                        && manipulator.kind == PatternManipulatorKind::RectangularDirection
                })
        );
    }

    #[test]
    fn invalid_modifier_inputs_never_create_a_pending_operation() {
        let mut disconnected = SketchCanvasState::default();
        commit_test_line(&mut disconnected, (0.0, 0.0), (2.0, 0.0));
        commit_test_line(&mut disconnected, (4.0, 0.0), (4.0, 2.0));
        let before = disconnected.authoring.clone();
        assert!(disconnected.set_exact_tool(ToolVariant::Fillet));
        disconnected.handle_modifier_click(SketchPoint::new(1.0, 0.0), 0.2);
        assert_eq!(
            disconnected.handle_modifier_click(SketchPoint::new(4.0, 1.0), 0.2),
            None
        );
        assert!(!disconnected.has_pending_edit());
        assert_eq!(disconnected.authoring, before);

        let mut no_intersection = SketchCanvasState::default();
        commit_test_line(&mut no_intersection, (0.0, 0.0), (2.0, 0.0));
        commit_test_line(&mut no_intersection, (4.0, -1.0), (4.0, 1.0));
        let before = no_intersection.authoring.clone();
        assert!(no_intersection.set_exact_tool(ToolVariant::Trim));
        assert_eq!(
            no_intersection.handle_modifier_click(SketchPoint::new(1.0, 0.0), 0.2),
            None
        );
        assert!(!no_intersection.has_pending_edit());
        assert_eq!(no_intersection.authoring, before);

        assert!(no_intersection.set_selected(Some(SketchEntityId(1))));
        assert!(no_intersection.set_exact_tool(ToolVariant::RectangularPattern));
        let anchor = no_intersection.pattern_anchor().expect("pattern anchor");
        assert_eq!(
            no_intersection.handle_modifier_click(anchor, 0.2),
            None,
            "zero spacing is invalid"
        );
        assert!(!no_intersection.has_pending_edit());
    }

    fn commit_test_rectangle(
        state: &mut SketchCanvasState,
        first: (f64, f64),
        opposite: (f64, f64),
    ) -> SketchEntityId {
        let id = state
            .stage_geometry(SketchGeometry::rectangle(
                SketchPoint::new(first.0, first.1),
                SketchPoint::new(opposite.0, opposite.1),
            ))
            .expect("stage fixture rectangle");
        assert_eq!(state.commit_pending(), Ok(id));
        id
    }

    fn commit_test_circle(
        state: &mut SketchCanvasState,
        center: (f64, f64),
        radius: f64,
    ) -> SketchEntityId {
        let center = SketchPoint::new(center.0, center.1);
        let id = state
            .stage_geometry(SketchGeometry::circle(
                center,
                SketchPoint::new(center.u + radius, center.v),
            ))
            .expect("stage fixture circle");
        assert_eq!(state.commit_pending(), Ok(id));
        id
    }

    #[test]
    fn sole_analytic_cell_auto_selects_and_compiles_without_display_geometry() {
        let mut state = SketchCanvasState::default();
        commit_test_rectangle(&mut state, (-2.0, -1.0), (2.0, 1.0));

        assert_eq!(state.available_region_count(), 1);
        assert_eq!(state.selected_region_count(), 1);
        assert_eq!(state.selected_region_signatures().len(), 1);
        let profile = state
            .selected_planar_profile()
            .expect("sole cell should compile exactly");
        assert_eq!(profile.regions.len(), 1);
        assert_eq!(profile.regions[0].outer.curves.len(), 4);
        assert!(profile.regions[0].holes.is_empty());
    }

    #[test]
    fn crossing_profiles_support_blank_space_pick_and_additive_cell_union() {
        let mut state = SketchCanvasState::default();
        commit_test_rectangle(&mut state, (-4.0, -2.0), (1.0, 1.0));
        commit_test_rectangle(&mut state, (-1.0, -1.0), (4.0, 2.0));
        assert_eq!(state.available_region_count(), 3);

        let _ = state.select_region_at_point(SketchPoint::new(-3.0, 0.0), false);
        assert_eq!(state.selected_region_count(), 1);
        assert!(state.select_region_at_point(SketchPoint::new(3.0, 0.0), true));
        assert_eq!(state.selected_region_count(), 2);
        assert!(state.select_region_at_point(SketchPoint::new(0.0, 0.0), true));
        assert_eq!(state.selected_region_count(), 3);

        let profile = state
            .selected_planar_profile()
            .expect("all adjacent cells should compile as one exact union");
        assert_eq!(profile.regions.len(), 1);
        assert!(profile.regions[0].holes.is_empty());
    }

    #[test]
    fn annular_cell_selection_compiles_and_paints_without_silently_filling_its_hole() {
        let mut state = SketchCanvasState::default();
        commit_test_circle(&mut state, (0.0, 0.0), 4.0);
        commit_test_circle(&mut state, (0.0, 0.0), 1.5);
        assert_eq!(state.available_region_count(), 2);
        let _ = state.select_region_at_point(SketchPoint::new(3.0, 0.0), false);

        let profile = state
            .selected_planar_profile()
            .expect("annulus should compile exactly");
        assert_eq!(profile.regions.len(), 1);
        assert_eq!(profile.regions[0].holes.len(), 1);

        let signature = state.selected_region_signatures().remove(0);
        let cell = state
            .analytic_regions
            .arrangement
            .as_ref()
            .and_then(|arrangement| arrangement.cell(&signature))
            .expect("selected annular cell");
        assert_eq!(cell.holes.len(), 1);
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0));
        let contours = std::iter::once(&cell.outer)
            .chain(cell.holes.iter())
            .map(|profile_loop| sample_arrangement_loop(profile_loop, state.view, rect))
            .collect::<Vec<_>>();
        let mesh = even_odd_scanline_mesh(&contours, rect, Color32::WHITE);
        let hole_center = state
            .view
            .sketch_to_screen(rect, SketchPoint::new(0.0, 0.0));
        assert!(mesh.indices.chunks_exact(3).all(|triangle| {
            let points = [triangle[0], triangle[1], triangle[2]]
                .map(|index| mesh.vertices[index as usize].pos);
            let winding = triangle_cross(points[0], points[1], points[2]).signum();
            !point_in_triangle(hole_center, points[0], points[1], points[2], winding)
        }));

        assert!(state.select_region_at_point(SketchPoint::new(0.0, 0.0), true));
        let filled_disk = state
            .selected_planar_profile()
            .expect("annulus plus disk should cancel the shared circle");
        assert_eq!(filled_disk.regions.len(), 1);
        assert!(filled_disk.regions[0].holes.is_empty());
    }

    #[test]
    fn cancelled_edits_preserve_region_selection_and_committed_splits_clear_stale_cells() {
        let mut state = SketchCanvasState::default();
        commit_test_rectangle(&mut state, (-2.0, -2.0), (2.0, 2.0));
        assert_eq!(state.selected_region_count(), 1);
        let anchor = *state
            .analytic_regions
            .selection_anchors
            .values()
            .next()
            .expect("auto-selected cell has a repair anchor");
        let selected_before = state.selected_region_signatures();

        state
            .stage_geometry(SketchGeometry::circle(
                SketchPoint::new(8.0, 0.0),
                SketchPoint::new(9.0, 0.0),
            ))
            .expect("stage unrelated pending edit");
        assert_eq!(state.selected_region_signatures(), selected_before);
        assert!(state.selected_planar_profile().is_none());
        assert!(state.cancel_pending().is_some());
        assert_eq!(state.selected_region_signatures(), selected_before);
        assert!(state.selected_planar_profile().is_some());

        commit_test_line(&mut state, (anchor.u, -3.0), (anchor.u, 3.0));
        assert_eq!(state.available_region_count(), 2);
        assert_eq!(
            state.selected_region_count(),
            0,
            "an anchor on the new split boundary must clear, not guess, a replacement cell"
        );
        assert!(state.selected_region_signatures().iter().all(|signature| {
            state
                .analytic_regions
                .arrangement
                .as_ref()
                .is_some_and(|arrangement| arrangement.cell(signature).is_some())
        }));
    }

    fn click_harness_at(
        harness: &mut Harness<'_, SketchCanvasState>,
        position: Pos2,
        modifiers: egui::Modifiers,
    ) {
        harness.input_mut().modifiers = modifiers;
        harness.event(egui::Event::PointerMoved(position));
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton {
                pos: position,
                button: PointerButton::Primary,
                pressed,
                modifiers,
            });
        }
        harness.step();
        harness.input_mut().modifiers = egui::Modifiers::NONE;
    }

    #[test]
    fn select_mode_ui_prioritizes_curves_and_shift_toggles_blank_profile_cells() {
        let mut state = SketchCanvasState::default();
        commit_test_rectangle(&mut state, (-4.0, -2.0), (1.0, 1.0));
        commit_test_rectangle(&mut state, (-1.0, -1.0), (4.0, 2.0));
        state.clear_selected_regions();
        state.set_selected(None);
        let mut harness = Harness::builder()
            .with_size(Vec2::new(600.0, 400.0))
            .build_ui_state(
                |ui, state| {
                    let _ = show(ui, state);
                },
                state,
            );
        harness.run();
        let viewport = harness.get_by_label("Sketch viewport").rect();
        let model_to_screen =
            |state: &SketchCanvasState, point| state.view.sketch_to_screen(viewport, point);

        let left = model_to_screen(harness.state(), SketchPoint::new(-3.0, 0.0));
        click_harness_at(&mut harness, left, egui::Modifiers::NONE);
        assert_eq!(harness.state().selected_region_count(), 1);
        assert_eq!(harness.state().selected(), None);

        let right = model_to_screen(harness.state(), SketchPoint::new(3.0, 0.0));
        click_harness_at(&mut harness, right, egui::Modifiers::SHIFT);
        assert_eq!(harness.state().selected_region_count(), 2);

        let curve = model_to_screen(harness.state(), SketchPoint::new(-4.0, 0.0));
        click_harness_at(&mut harness, curve, egui::Modifiers::NONE);
        assert_eq!(harness.state().selected_region_count(), 0);
        assert_eq!(harness.state().selected(), Some(SketchEntityId(1)));
    }

    #[test]
    fn staged_delete_of_compound_output_is_cancel_neutral_and_locally_undoable() {
        let mut state = SketchCanvasState::default();
        let rectangle = state
            .stage_geometry(SketchGeometry::rectangle(
                SketchPoint::new(-2.0, -1.0),
                SketchPoint::new(2.0, 1.0),
            ))
            .expect("stage rectangle");
        state.commit_pending().expect("commit rectangle");
        let committed = state.authoring().clone();
        let ui_high_water = state.next_entity_id;
        assert_eq!(state.selected(), Some(rectangle));

        let subject = state
            .stage_delete_selected()
            .expect("stage compound deletion");
        assert_eq!(subject, rectangle);
        let pending = state.pending().expect("retirement preview");
        assert_eq!(pending.subject(), rectangle);
        assert!(pending.entities().is_empty());
        assert_eq!(pending.retired_entities(), &[rectangle]);
        assert!(
            !state.selected_region_fill_visible(),
            "retirement preview must not imply the old material region survives"
        );
        assert_eq!(
            state.certified_profile_status(),
            CertifiedProfileStatus::Empty
        );
        assert_eq!(state.authoring(), &committed);
        assert_eq!(state.entities().len(), 1);

        state.cancel_pending().expect("cancel deletion");
        assert_eq!(state.authoring(), &committed);
        assert_eq!(state.entities().len(), 1);
        assert_eq!(state.selected(), Some(rectangle));
        assert!(
            state.can_undo_local(),
            "the prior rectangle confirmation remains undoable"
        );
        assert!(!state.can_redo_local(), "cancel creates no redo entry");

        state.stage_delete_selected().expect("restage deletion");
        state.commit_pending().expect("confirm deletion");
        assert!(state.entities().is_empty());
        assert_eq!(state.authoring().active_operations().count(), 0);
        assert_eq!(state.authoring().active_entities().count(), 0);
        assert!(!state.authoring().operations()[0].active);
        assert!(state.can_undo_local());

        assert!(state.undo_local());
        assert_eq!(state.authoring().active_operations().count(), 1);
        assert_eq!(state.entities().len(), 4);
        assert_eq!(state.selected(), None);
        assert!(state.next_entity_id > ui_high_water);
        assert!(matches!(
            state.certified_profile_status(),
            CertifiedProfileStatus::Closed { .. }
        ));

        assert!(state.redo_local());
        assert!(state.entities().is_empty());
        assert_eq!(state.authoring().active_operations().count(), 0);
        assert_eq!(state.selected(), None);
    }

    #[test]
    fn persisted_authoring_hydrates_with_exact_delete_mappings_and_empty_local_history() {
        let mut source = SketchCanvasState::default();
        source
            .stage_recipe(
                CoreRecipe::CentrePointCircle {
                    center: CorePointInput::Position(CorePoint2::new(1.0, 2.0)),
                    radius: CoreValue::Literal(CoreLength::new(3.0).expect("radius")),
                    radial_angle: CoreValue::Literal(
                        CoreAngle::radians(0.0).expect("radial angle"),
                    ),
                },
                "Add circle",
            )
            .expect("stage exact circle");
        source.commit_pending().expect("commit exact circle");
        let persisted = source.authoring().clone();

        let mut hydrated = SketchCanvasState::from_authoring(SketchPlane::XY, persisted.clone())
            .expect("hydrate checked v6 authoring");
        assert_eq!(hydrated.authoring(), &persisted);
        assert_eq!(hydrated.entities().len(), 1);
        assert!(!hydrated.can_undo_local());
        let selected = hydrated.entities()[0].id;
        assert!(hydrated.set_selected(Some(selected)));
        assert_eq!(
            hydrated
                .stage_delete_selected()
                .expect("stage hydrated delete"),
            selected
        );
        hydrated.commit_pending().expect("commit hydrated delete");
        assert!(hydrated.entities().is_empty());
        assert!(hydrated.undo_local());
        assert_eq!(hydrated.entities().len(), 1);
    }

    #[test]
    fn persisted_region_signatures_rehydrate_the_exact_selected_union() {
        let mut source = SketchCanvasState::default();
        commit_test_rectangle(&mut source, (-4.0, -2.0), (1.0, 1.0));
        commit_test_rectangle(&mut source, (-1.0, -1.0), (4.0, 2.0));
        source.clear_selected_regions();
        let _ = source.select_region_at_point(SketchPoint::new(-3.0, 0.0), false);
        assert!(source.select_region_at_point(SketchPoint::new(0.0, 0.0), true));
        let signatures = source.selected_region_signatures();
        assert_eq!(source.available_region_count(), 3);
        assert_eq!(source.selected_region_count(), 2);
        let expected = compile_selected_profile(
            source
                .analytic_regions
                .arrangement
                .as_ref()
                .expect("arrangement"),
            &signatures,
            &PrecisionPolicy::default(),
        )
        .expect("two selected arrangement cells should compile")
        .profile;

        let hydrated = SketchCanvasState::from_authoring_with_regions(
            SketchPlane::XY,
            source.authoring().clone(),
            &signatures,
        )
        .expect("rehydrate selected regions");
        assert_eq!(hydrated.available_region_count(), 3);
        assert_eq!(hydrated.selected_region_count(), 2);
        assert_eq!(hydrated.selected_region_signatures(), signatures);
        assert_eq!(hydrated.selected_planar_profile(), Some(expected));
        assert!(!hydrated.can_undo_local());
    }

    #[test]
    fn sketch_objects_expose_visible_and_snappable_centre_points() {
        // Circle center point
        let circle =
            SketchGeometry::circle(SketchPoint::new(10.0, 20.0), SketchPoint::new(15.0, 20.0));
        assert_eq!(circle.center(), Some(SketchPoint::new(10.0, 20.0)));
        let circle_points = circle.control_points().iter().collect::<Vec<_>>();
        assert!(circle_points.contains(&SketchPoint::new(10.0, 20.0)));

        // Rectangle center point
        let rect =
            SketchGeometry::rectangle(SketchPoint::new(0.0, 0.0), SketchPoint::new(40.0, 30.0));
        assert_eq!(rect.center(), Some(SketchPoint::new(20.0, 15.0)));
        let rect_points = rect.control_points().iter().collect::<Vec<_>>();
        assert_eq!(rect_points.len(), 9);
        assert!(rect_points.contains(&SketchPoint::new(20.0, 15.0))); // center
        assert!(rect_points.contains(&SketchPoint::new(20.0, 0.0))); // bottom midpoint
        assert!(rect_points.contains(&SketchPoint::new(40.0, 15.0))); // right midpoint
        assert!(rect_points.contains(&SketchPoint::new(20.0, 30.0))); // top midpoint
        assert!(rect_points.contains(&SketchPoint::new(0.0, 15.0))); // left midpoint

        // Arc center point
        let arc = SketchGeometry::arc(
            SketchPoint::new(5.0, 5.0),
            SketchPoint::new(10.0, 5.0),
            SketchPoint::new(5.0, 10.0),
        );
        assert_eq!(arc.center(), Some(SketchPoint::new(5.0, 5.0)));
        let arc_points = arc.control_points().iter().collect::<Vec<_>>();
        assert!(arc_points.contains(&SketchPoint::new(5.0, 5.0)));
    }

    #[test]
    fn sketch_objects_can_be_grabbed_and_translated_via_drag() {
        let mut state = SketchCanvasState::default();
        let circle_id = state
            .stage_geometry(SketchGeometry::circle(
                SketchPoint::new(10.0, 20.0),
                SketchPoint::new(15.0, 20.0),
            ))
            .expect("stage circle");
        state.commit_pending().expect("commit circle");
        state.set_selected(Some(circle_id));

        // Drag by (5.0, -10.0)
        assert!(state.translate_selected(5.0, -10.0));

        let translated = state.entity_geometry(circle_id).expect("circle exists");
        assert_eq!(translated.center(), Some(SketchPoint::new(15.0, 10.0)));

        // Stage and drag a rectangle
        let rect_id = state
            .stage_geometry(SketchGeometry::rectangle(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(20.0, 10.0),
            ))
            .expect("stage rect");
        state.commit_pending().expect("commit rect");
        state.set_selected(Some(rect_id));

        assert!(state.translate_selected(10.0, 20.0));
        let translated_rect = state.entity_geometry(rect_id).expect("rect exists");
        assert_eq!(translated_rect.center(), Some(SketchPoint::new(20.0, 25.0)));
    }

    #[test]
    fn sketch_objects_can_be_reshaped_via_corner_side_and_rim_dragging() {
        let mut state = SketchCanvasState::default();

        // 1. Rectangle corner reshaping: dragging top-right corner (index 2)
        let rect_id = state
            .stage_geometry(SketchGeometry::rectangle(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(10.0, 20.0),
            ))
            .expect("stage rect");
        state.commit_pending().expect("commit rect");
        state.set_selected(Some(rect_id));

        // Drag Top-Right corner (index 2) by (+5.0, +10.0) -> rectangle should become [0..15] x [0..30]
        assert!(state.reshape_selected(SketchDragHandle::RectangleCorner(2), 5.0, 10.0));
        let reshaped_rect = state.entity_geometry(rect_id).expect("rect exists");
        let corners = reshaped_rect.rectangle_corners().expect("corners");
        assert_eq!(corners[0], SketchPoint::new(0.0, 0.0)); // bottom-left stays fixed
        assert_eq!(corners[2], SketchPoint::new(15.0, 30.0)); // top-right moved

        // 2. Rectangle side reshaping: dragging Right side (index 1) by (+10.0, 0.0)
        assert!(state.reshape_selected(SketchDragHandle::RectangleSide(1), 10.0, 0.0));
        let reshaped_rect2 = state.entity_geometry(rect_id).expect("rect exists");
        let corners2 = reshaped_rect2.rectangle_corners().expect("corners");
        assert_eq!(corners2[0], SketchPoint::new(0.0, 0.0));
        assert_eq!(corners2[1], SketchPoint::new(25.0, 0.0)); // width increased from 15 to 25
        assert_eq!(corners2[2], SketchPoint::new(25.0, 30.0));
        assert_eq!(corners2[3], SketchPoint::new(0.0, 30.0)); // height stays 30

        // 3. Rectangle side reshaping: dragging Top side (index 2) by (0.0, +5.0)
        assert!(state.reshape_selected(SketchDragHandle::RectangleSide(2), 0.0, 5.0));
        let reshaped_rect3 = state.entity_geometry(rect_id).expect("rect exists");
        let corners3 = reshaped_rect3.rectangle_corners().expect("corners");
        assert_eq!(corners3[0], SketchPoint::new(0.0, 0.0));
        assert_eq!(corners3[2], SketchPoint::new(25.0, 35.0)); // height increased from 30 to 35

        // 4. Circle rim reshaping: dragging rim changes radius
        let circle_id = state
            .stage_geometry(SketchGeometry::circle(
                SketchPoint::new(50.0, 50.0),
                SketchPoint::new(60.0, 50.0),
            ))
            .expect("stage circle");
        state.commit_pending().expect("commit circle");
        state.set_selected(Some(circle_id));

        // Drag rim by (+5.0, 0.0) -> radius should expand from 10 to 15
        assert!(state.reshape_selected(SketchDragHandle::CircleRim, 5.0, 0.0));
        let reshaped_circle = state.entity_geometry(circle_id).expect("circle exists");
        assert_eq!(reshaped_circle.center(), Some(SketchPoint::new(50.0, 50.0))); // center fixed
        match reshaped_circle {
            SketchGeometry::Circle { center, rim } => {
                let r = center.distance_squared(rim).sqrt();
                assert!((r - 15.0).abs() < 1e-6);
            }
            _ => panic!("expected circle"),
        }

        // 5. Line endpoint reshaping: dragging end moves end, keeping start fixed
        let line_id = state
            .stage_geometry(SketchGeometry::segment(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(10.0, 0.0),
            ))
            .expect("stage line");
        state.commit_pending().expect("commit line");
        state.set_selected(Some(line_id));

        assert!(state.reshape_selected(SketchDragHandle::EndPoint, 0.0, 15.0));
        let reshaped_line = state.entity_geometry(line_id).expect("line exists");
        match reshaped_line {
            SketchGeometry::Segment { start, end } => {
                assert_eq!(start, SketchPoint::new(0.0, 0.0)); // start fixed
                assert_eq!(end, SketchPoint::new(10.0, 15.0)); // end moved
            }
            _ => panic!("expected segment"),
        }
    }

    #[test]
    fn midpoints_appear_on_hover_and_selection_across_all_straight_lines() {
        let mut state = SketchCanvasState::default();
        let canvas_rect = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(800.0, 600.0));

        // 1. Line segment midpoint
        let line_id = state
            .stage_geometry(SketchGeometry::segment(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(20.0, 0.0),
            ))
            .expect("stage line");
        state.commit_pending().expect("commit line");

        let geom = state.entity_geometry(line_id).expect("line geometry");
        let control_pts = geom.control_points().iter().collect::<Vec<_>>();
        assert_eq!(control_pts.len(), 3);
        assert_eq!(control_pts[0], SketchPoint::new(0.0, 0.0)); // start
        assert_eq!(control_pts[1], SketchPoint::new(20.0, 0.0)); // end
        assert_eq!(control_pts[2], SketchPoint::new(10.0, 0.0)); // midpoint!

        // Hover over the line
        let screen_pos = state
            .view
            .sketch_to_screen(canvas_rect, SketchPoint::new(10.0, 0.0));
        let hovered = hit_test_entities(&state.entities, state.view, canvas_rect, screen_pos, 8.0);
        assert_eq!(hovered, Some(line_id));

        // Snapping near the midpoint returns SnapKind::Endpoint / Midpoint
        let snap = state.snap_point(canvas_rect, screen_pos);
        assert_eq!(snap.point, SketchPoint::new(10.0, 0.0));

        // 2. Rectangle side midpoints
        let rect_id = state
            .stage_geometry(SketchGeometry::rectangle(
                SketchPoint::new(0.0, 0.0),
                SketchPoint::new(40.0, 20.0),
            ))
            .expect("stage rect");
        state.commit_pending().expect("commit rect");

        let rect_geom = state.entity_geometry(rect_id).expect("rect geometry");
        let rect_pts = rect_geom.control_points().iter().collect::<Vec<_>>();
        assert_eq!(rect_pts.len(), 9); // 4 corners + 1 center + 4 side midpoints
        assert!(rect_pts.contains(&SketchPoint::new(20.0, 0.0))); // bottom side midpoint
        assert!(rect_pts.contains(&SketchPoint::new(40.0, 10.0))); // right side midpoint
        assert!(rect_pts.contains(&SketchPoint::new(20.0, 20.0))); // top side midpoint
        assert!(rect_pts.contains(&SketchPoint::new(0.0, 10.0))); // left side midpoint
    }

    #[test]
    fn rotated_sketch_view_drag_moves_in_screen_direction() {
        let mut state = SketchCanvasState::default();
        state.view.set_quarter_turns(1); // 90 deg CCW rotation

        let pt_id = state
            .stage_geometry(SketchGeometry::point(SketchPoint::new(10.0, 20.0)))
            .expect("stage point");
        state.commit_pending().expect("commit");
        state.set_selected(Some(pt_id));

        // Dragging UP on screen: horizontal = 0, vertical = 5.0
        let (delta_u, delta_v) = state.view.unrotate_offset(0.0, 5.0);
        // With quarter_turns = 1, (0, 5) unrotates to (5, 0) in sketch coords (which is +U)
        assert_eq!(delta_u, 5.0);
        assert_eq!(delta_v, 0.0);

        assert!(state.reshape_selected(SketchDragHandle::Translate, delta_u, delta_v));
        let moved_geom = state.entity_geometry(pt_id).expect("point geom");
        assert_eq!(
            moved_geom,
            SketchGeometry::point(SketchPoint::new(15.0, 20.0))
        );

        // In the rotated view, (15.0, 20.0) is drawn 5 units higher on screen than (10.0, 20.0)
        let canvas_rect = Rect::from_min_size(Pos2::ZERO, Vec2::splat(500.0));
        let screen_before = state
            .view
            .sketch_to_screen(canvas_rect, SketchPoint::new(10.0, 20.0));
        let screen_after = state
            .view
            .sketch_to_screen(canvas_rect, SketchPoint::new(15.0, 20.0));
        assert!((screen_after.x - screen_before.x).abs() < 1e-4);
        assert!(screen_after.y < screen_before.y); // Y decreases upwards on screen!
    }
}
