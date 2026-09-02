//! High-level command types for Artificer

use artificer_protocol::{Point2, Point3, Vector3};
use serde::{Deserialize, Serialize};

use crate::api::selectors::EntitySelector;

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
    Revolve {
        label: String,
        sketch: StepLabel,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        regions: Vec<u32>,
        axis_origin: Point3,
        axis_direction: Vector3,
        angle_degrees: f64,
        operation: ExtrudeOp,
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
            Self::Revolve { .. } => "revolve",
            Self::PushPull { .. } => "push_pull",
            Self::DrillHole { .. } => "drill_hole",
            Self::Fillet { .. } => "fillet",
            Self::Chamfer { .. } => "chamfer",
            Self::Mirror { .. } => "mirror",
            Self::LinearPattern { .. } => "linear_pattern",
            Self::FeaturePattern { .. } => "feature_pattern",
            Self::BooleanUnion { .. } => "boolean_union",
            Self::BooleanDifference { .. } => "boolean_difference",
            Self::BooleanIntersection { .. } => "boolean_intersection",
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::MakeBox { label, .. }
            | Self::MakeCylinder { label, .. }
            | Self::Sketch { label, .. }
            | Self::Extrude { label, .. }
            | Self::Revolve { label, .. }
            | Self::PushPull { label, .. }
            | Self::DrillHole { label, .. }
            | Self::Fillet { label, .. }
            | Self::Chamfer { label, .. }
            | Self::Mirror { label, .. }
            | Self::LinearPattern { label, .. }
            | Self::FeaturePattern { label, .. }
            | Self::BooleanUnion { label, .. }
            | Self::BooleanDifference { label, .. }
            | Self::BooleanIntersection { label, .. } => label,
        }
    }
}
