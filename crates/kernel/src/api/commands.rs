//! High-level command types for Artificer

use artificer_protocol::{PlanarFrame3, Point2, Point3, Vector3};
use serde::{Deserialize, Serialize};

use crate::api::selectors::EntitySelector;

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

/// A reference to a prior operation's step.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StepLabel(pub String);

impl From<&str> for StepLabel {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for StepLabel {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for StepLabel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Extrude boolean operation type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtrudeOp {
    New,
    Add,
    Cut,
}

/// Defines a plane for a sketch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SketchPlane {
    XY,
    XZ,
    YZ,
    /// A face of the current body. The selector sits under its own key:
    /// it carries a `type` tag of its own, which the plane's tag would
    /// otherwise collide with on the wire.
    OnFace {
        face: EntitySelector,
    },
    /// Any plane already resolved to a frame: a world plane given by its
    /// origin and axes, or offset from one of the three above. The frame's
    /// axes are directions; the kernel normalizes them, and `u × v` is the
    /// side a sketch on it faces.
    Frame {
        frame: PlanarFrame3,
    },
    /// A planar face of the current body, its own frame moved `offset` along
    /// its outward normal (ADR 0048).
    OffsetFace {
        face: Box<EntitySelector>,
        #[serde(default)]
        offset: f64,
        #[serde(default)]
        flip: bool,
    },
    /// Halfway between two parallel planar faces of the current body, facing
    /// as the first does, then moved `offset` along that normal.
    Midplane {
        first: Box<EntitySelector>,
        second: Box<EntitySelector>,
        #[serde(default)]
        offset: f64,
        #[serde(default)]
        flip: bool,
    },
    /// Through a straight edge of the current body, hinged on it and turned
    /// `angle_degrees` from the planar face it starts on: 0 lies on the face,
    /// 90 stands square to it. `face` names which face when the edge bounds
    /// two planar faces.
    ThroughEdge {
        edge: Box<EntitySelector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        face: Option<Box<EntitySelector>>,
        #[serde(default)]
        angle_degrees: f64,
        #[serde(default)]
        offset: f64,
        #[serde(default)]
        flip: bool,
    },
}

/// A 2D geometric entity in a sketch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SketchEntity {
    Line {
        start: Point2,
        end: Point2,
    },
    Circle {
        center: Point2,
        radius: f64,
    },
    Arc {
        center: Point2,
        radius: f64,
        start_angle: f64,
        end_angle: f64,
    },
    Rectangle {
        origin: Point2,
        width: f64,
        height: f64,
    },
    /// A spline through fit points (ADR 0050), drawn as the sketch's
    /// fit-point tool draws it: cubic when there are four points or more,
    /// and back to the first point, smooth there, when `closed`.
    Spline {
        points: Vec<Point2>,
        #[serde(default, skip_serializing_if = "is_false")]
        closed: bool,
    },
    /// A spline by its control points (ADR 0050): clamped and uniform, of
    /// `degree`, and ending on its first control point when `closed`.
    ControlSpline {
        control_points: Vec<Point2>,
        degree: usize,
        #[serde(default, skip_serializing_if = "is_false")]
        closed: bool,
    },
}

/// A geometric constraint applied to sketch entities.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SketchConstraint {
    Coincident,
    Horizontal,
    Vertical,
    Distance { distance: f64 },
    Parallel,
    Perpendicular,
    EqualLength,
    Tangent,
    Fixed,
}

/// Where a feature pattern puts its instances. `count` is the total
/// including the original.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PatternPlacement {
    /// Instances every `spacing` along `direction`, which must lie in the
    /// feature's face.
    Linear {
        direction: Vector3,
        spacing: f64,
        count: u16,
    },
    /// Instances turned about an axis normal to the feature's face, every
    /// `angle_step_degrees` (a full turn shared equally when zero).
    Circular {
        axis_origin: Point3,
        axis_direction: Vector3,
        count: u16,
        #[serde(default, skip_serializing_if = "is_zero")]
        angle_step_degrees: f64,
    },
}

impl PatternPlacement {
    /// The total number of instances, the original included.
    #[must_use]
    pub const fn count(&self) -> u16 {
        match self {
            Self::Linear { count, .. } | Self::Circular { count, .. } => *count,
        }
    }
}

/// A construction axis the body places, as `axis(...)` names it: found
/// again against the body as it stands when the step that uses it runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AxisPlacement {
    /// Along a straight edge, from its start to its end.
    Along {
        edge: Box<EntitySelector>,
        #[serde(default, skip_serializing_if = "is_false")]
        flip: bool,
    },
    /// Through a curved face: the axis of its cylinder, cone, sphere or
    /// torus.
    Through {
        face: Box<EntitySelector>,
        #[serde(default, skip_serializing_if = "is_false")]
        flip: bool,
    },
    /// Where two flat faces meet, running along the first's normal crossed
    /// with the second's.
    Between {
        first: Box<EntitySelector>,
        second: Box<EntitySelector>,
        #[serde(default, skip_serializing_if = "is_false")]
        flip: bool,
    },
}

/// Commands for geometry operations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApiCommand {
    MakeBox {
        label: String,
        origin: Point3,
        size: [f64; 3],
    },
    MakeCylinder {
        label: String,
        center: Point3,
        axis: Vector3,
        radius: f64,
        height: f64,
    },
    Sketch {
        label: String,
        on: SketchPlane,
        entities: Vec<SketchEntity>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        constraints: Vec<SketchConstraint>,
    },
    Extrude {
        label: String,
        sketch: StepLabel,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        regions: Vec<u32>,
        distance: f64,
        operation: ExtrudeOp,
        /// Draft angle in degrees for a new body: positive leans the walls
        /// outward, negative inward. Replays as an exact loft to the
        /// profile's offset section. Add and cut extrusions do not draft.
        #[serde(default, skip_serializing_if = "is_zero")]
        draft_degrees: f64,
    },
    /// A loft between the regions of two sketches, each on its own plane
    /// (ADR 0049). A new body, or an add or cut against the current one.
    Loft {
        label: String,
        sections: Vec<StepLabel>,
        operation: ExtrudeOp,
    },
    Revolve {
        label: String,
        sketch: StepLabel,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        regions: Vec<u32>,
        axis_origin: Point3,
        axis_direction: Vector3,
        angle_degrees: f64,
        operation: ExtrudeOp,
        /// An axis the body places, found again when the revolve runs; it
        /// stands in for `axis_origin` and `axis_direction`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        axis_placement: Option<AxisPlacement>,
    },
    PushPull {
        label: String,
        face: EntitySelector,
        distance: f64,
    },
    DrillHole {
        label: String,
        face: EntitySelector,
        center: Point2,
        diameter: f64,
        depth: f64,
    },
    Fillet {
        label: String,
        edges: Vec<EntitySelector>,
        radius: f64,
    },
    Chamfer {
        label: String,
        edges: Vec<EntitySelector>,
        distance: f64,
    },
    Mirror {
        label: String,
        plane_origin: Point3,
        plane_normal: Vector3,
    },
    LinearPattern {
        label: String,
        direction: Vector3,
        spacing: f64,
        count: u16,
    },
    /// Repeats an earlier feature, a drilled hole or an extrusion from a
    /// sketch on a face, at rigid placements on the same face: a row or a
    /// circular array. Each instance is the same exact feature replayed,
    /// committed as the step `<label>/<n>`.
    FeaturePattern {
        label: String,
        /// The step to repeat.
        step: StepLabel,
        placement: PatternPlacement,
    },
    /// Hollows the current body to one uniform wall, open at the given
    /// faces: one cap, two opposite caps, or none for a closed hollow.
    Shell {
        label: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        open: Vec<EntitySelector>,
        wall: f64,
    },
    BooleanUnion {
        label: String,
        target: StepLabel,
        tool: StepLabel,
    },
    BooleanDifference {
        label: String,
        target: StepLabel,
        tool: StepLabel,
    },
    BooleanIntersection {
        label: String,
        target: StepLabel,
        tool: StepLabel,
    },
    /// A sheet body (ADR 0056, Track S): the walls a sketch's open or closed
    /// chain of lines and arcs sweeps along the sketch plane's normal, with
    /// no caps. A new body of its own.
    SurfaceExtrude {
        label: String,
        sketch: StepLabel,
        distance: f64,
    },
    /// A sheet body: the bands a sketch's chain sweeps about an axis in
    /// the sketch plane, with no wedge faces closing a partial turn. A new
    /// body of its own.
    SurfaceRevolve {
        label: String,
        sketch: StepLabel,
        axis_origin: Point3,
        axis_direction: Vector3,
        angle_degrees: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        axis_placement: Option<AxisPlacement>,
    },
    /// A sheet body of one planar face per region of a sketch, holes
    /// included. A new body of its own.
    Patch {
        label: String,
        sketch: StepLabel,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        regions: Vec<u32>,
    },
    /// Several sheet bodies welded along their boundaries into one body,
    /// a solid when they close (ADR 0056, S3).
    Stitch {
        label: String,
        sheets: Vec<StepLabel>,
    },
    /// The current sheet body thickened into a solid (ADR 0056, S4):
    /// positive along the sheet's normal, negative against it.
    Thicken { label: String, thickness: f64 },
    /// The current sheet body trimmed by a plane, keeping the side the
    /// plane faces (ADR 0056, S2).
    Trim { label: String, plane: SketchPlane },
    /// Reads a STEP file into a new body (ADR 0056, Track I). The file is
    /// read from `path` when the command runs, relative to the working
    /// directory, unless its `text` travels with the command.
    ImportStep {
        label: String,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
}

impl ApiCommand {
    /// The command's kind as it appears on the wire: `make_box`,
    /// `drill_hole`, `boolean_union`, and so on.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::MakeBox { .. } => "make_box",
            Self::MakeCylinder { .. } => "make_cylinder",
            Self::Sketch { .. } => "sketch",
            Self::Extrude { .. } => "extrude",
            Self::Loft { .. } => "loft",
            Self::Revolve { .. } => "revolve",
            Self::PushPull { .. } => "push_pull",
            Self::DrillHole { .. } => "drill_hole",
            Self::Fillet { .. } => "fillet",
            Self::Chamfer { .. } => "chamfer",
            Self::Mirror { .. } => "mirror",
            Self::LinearPattern { .. } => "linear_pattern",
            Self::FeaturePattern { .. } => "feature_pattern",
            Self::Shell { .. } => "shell",
            Self::BooleanUnion { .. } => "boolean_union",
            Self::BooleanDifference { .. } => "boolean_difference",
            Self::BooleanIntersection { .. } => "boolean_intersection",
            Self::SurfaceExtrude { .. } => "surface_extrude",
            Self::SurfaceRevolve { .. } => "surface_revolve",
            Self::Patch { .. } => "patch",
            Self::Stitch { .. } => "stitch",
            Self::Thicken { .. } => "thicken",
            Self::Trim { .. } => "trim",
            Self::ImportStep { .. } => "import_step",
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::MakeBox { label, .. }
            | Self::MakeCylinder { label, .. }
            | Self::Sketch { label, .. }
            | Self::Extrude { label, .. }
            | Self::Loft { label, .. }
            | Self::Revolve { label, .. }
            | Self::PushPull { label, .. }
            | Self::DrillHole { label, .. }
            | Self::Fillet { label, .. }
            | Self::Chamfer { label, .. }
            | Self::Mirror { label, .. }
            | Self::LinearPattern { label, .. }
            | Self::FeaturePattern { label, .. }
            | Self::Shell { label, .. }
            | Self::BooleanUnion { label, .. }
            | Self::BooleanDifference { label, .. }
            | Self::BooleanIntersection { label, .. }
            | Self::SurfaceExtrude { label, .. }
            | Self::SurfaceRevolve { label, .. }
            | Self::Patch { label, .. }
            | Self::Stitch { label, .. }
            | Self::Thicken { label, .. }
            | Self::Trim { label, .. }
            | Self::ImportStep { label, .. } => label,
        }
    }
}
