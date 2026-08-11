//! Registry-driven compact sketch toolbar.
//!
//! This module owns presentation metadata and toolbar interaction only.  It
//! deliberately does not know about document revisions, sketch geometry, or
//! gesture-controller state.  A controller receives the exact [`ToolVariant`]
//! returned by [`render_sketch_toolbar`] and remains responsible for staging a
//! candidate behind the application's universal confirmation gate.

use std::array;
use std::f32::consts::{PI, TAU};

use egui::{
    Align, Button, Color32, Direction, Key, Layout, Modifiers, Painter, Pos2, Rect, Response,
    Sense, Stroke, StrokeKind, Ui, UiBuilder, WidgetInfo, WidgetType, pos2, vec2,
};

/// Side length of every persistent primary icon cell.
pub const PRIMARY_CELL_SIZE: f32 = 30.0;
/// Side length of the separately focusable chooser contained by a family tile.
pub const CHEVRON_CELL_WIDTH: f32 = 12.0;
/// Inset that keeps the chooser visibly inside its family tile.
pub const CHEVRON_CELL_INSET: f32 = 1.0;
/// Horizontal space between tool families.
pub const FAMILY_GAP: f32 = 4.0;
/// Vertical space between the two persistent toolbar rows.
pub const ROW_GAP: f32 = 2.0;
/// Padding between the group caption and the first row of tool tiles.
pub const TOOLBAR_TOP_PADDING: f32 = 4.0;
/// Reserved clearance below the second row before the ribbon divider.
pub const TOOLBAR_BOTTOM_PADDING: f32 = 8.0;
/// Width of the uniform six-column toolbar grid.
pub const SKETCH_TOOLBAR_WIDTH: f32 = PRIMARY_CELL_SIZE * 7.0 + FAMILY_GAP * 6.0;
/// Height required by the padded two-row icon grid.
pub const SKETCH_TOOLBAR_HEIGHT: f32 =
    PRIMARY_CELL_SIZE * 2.0 + ROW_GAP + TOOLBAR_TOP_PADDING + TOOLBAR_BOTTOM_PADDING;

const _: () = {
    assert!(CHEVRON_CELL_WIDTH + CHEVRON_CELL_INSET < PRIMARY_CELL_SIZE);
    assert!(TOOLBAR_TOP_PADDING >= 4.0);
    assert!(TOOLBAR_BOTTOM_PADDING >= TOOLBAR_TOP_PADDING);
};

/// The reason used when the universal tick/cross rail owns keyboard input.
pub const PENDING_CONFIRMATION_DISABLED_REASON: &str = "Confirm the current operation with the green tick or Enter, or cancel it with the red cross or Escape.";

/// Stable top-level families shown in the compact ribbon.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum ToolFamily {
    Select,
    Point,
    Line,
    Rectangle,
    Circle,
    Arc,
    Polygon,
    Slot,
    Trim,
    Fillet,
    Chamfer,
    Pattern,
    Dimension,
}

impl ToolFamily {
    pub const COUNT: usize = 13;
    pub const ALL: [Self; Self::COUNT] = [
        Self::Select,
        Self::Point,
        Self::Line,
        Self::Rectangle,
        Self::Circle,
        Self::Arc,
        Self::Polygon,
        Self::Slot,
        Self::Trim,
        Self::Fillet,
        Self::Chamfer,
        Self::Pattern,
        Self::Dimension,
    ];

    /// The only authority used to populate a family dropdown.
    #[must_use]
    pub fn variants(self) -> &'static [ToolVariant] {
        self.descriptor().variants
    }

    #[must_use]
    pub fn descriptor(self) -> &'static ToolFamilyDescriptor {
        &TOOL_FAMILIES[self as usize]
    }

    #[must_use]
    pub fn default_variant(self) -> ToolVariant {
        self.descriptor().default_variant
    }
}

/// Every executable sketch command has its own stable typed variant.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum ToolVariant {
    Select,
    Point,
    SingleLine,
    ChainedPolyline,
    Centreline,
    TwoPointRectangle,
    CentrePointRectangle,
    CentrePointCircle,
    TwoPointCircle,
    CentreStartEndArc,
    ThreePointArc,
    InnerDiameterPolygon,
    OuterDiameterPolygon,
    TwoPointSlot,
    CentreToOuterPointSlot,
    Trim,
    Fillet,
    Chamfer,
    TwoDistanceChamfer,
    RectangularPattern,
    CircularPattern,
    Dimension,
}

impl ToolVariant {
    pub const COUNT: usize = 22;
    pub const ALL: [Self; Self::COUNT] = [
        Self::Select,
        Self::Point,
        Self::SingleLine,
        Self::ChainedPolyline,
        Self::Centreline,
        Self::TwoPointRectangle,
        Self::CentrePointRectangle,
        Self::CentrePointCircle,
        Self::TwoPointCircle,
        Self::CentreStartEndArc,
        Self::ThreePointArc,
        Self::InnerDiameterPolygon,
        Self::OuterDiameterPolygon,
        Self::TwoPointSlot,
        Self::CentreToOuterPointSlot,
        Self::Trim,
        Self::Fillet,
        Self::Chamfer,
        Self::TwoDistanceChamfer,
        Self::RectangularPattern,
        Self::CircularPattern,
        Self::Dimension,
    ];

    #[must_use]
    pub fn descriptor(self) -> &'static ToolDescriptor {
        &TOOL_DESCRIPTORS[self as usize]
    }

    #[must_use]
    pub fn family(self) -> ToolFamily {
        self.descriptor().family
    }
}

/// Keyboard shortcut scoped to the sketch workbench.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolShortcut {
    pub key: Key,
    pub label: &'static str,
}

/// Cursor semantic requested by a gesture controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolCursor {
    Arrow,
    Crosshair,
    PrecisionPick,
    Trim,
}

/// Typed live-input categories shared with the active-tool palette.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolInputKind {
    Length,
    SignedLength,
    Angle,
    Integer,
    Choice,
    Boolean,
}

/// One deterministic field in a tool's Tab order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolInputField {
    pub stable_key: &'static str,
    pub label: &'static str,
    pub kind: ToolInputKind,
    pub domain: &'static str,
}

/// One click/selection phase advertised by a creation controller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PointAcquisitionPhase {
    pub stable_key: &'static str,
    pub prompt: &'static str,
}

/// Selection prerequisite used by the controller capability check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectionRequirement {
    None,
    CurveSpanUnderPointer,
    OneOrMoreEditableEntities,
    TwoConnectedProfileCurves,
    TwoConnectedProfileLines,
}

/// Semantic result generated by the command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolOutputRole {
    SessionOnly,
    ProfileGeometry,
    ConstructionGeometry,
    Modification,
    GeneratedGeometry,
}

/// Capability class evaluated by the sketch session before enabling a tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityRequirement {
    EditableSketch,
    TrimCandidate,
    FilletPair,
    ChamferLinePair,
    PatternSeedSelection,
}

/// Model-changing commands must stage, then use the permanent tick/Enter gate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitContract {
    SessionOnly,
    StageThenUniversalTickOrEnter,
}

/// Vector icon authored in normalized coordinates and painted by this module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolIcon {
    Select,
    Point,
    SingleLine,
    Polyline,
    Centreline,
    CornerRectangle,
    CentreRectangle,
    CentreCircle,
    DiameterCircle,
    CentreArc,
    ThreePointArc,
    InnerPolygon,
    OuterPolygon,
    TwoPointSlot,
    CentreSlot,
    Trim,
    Fillet,
    Chamfer,
    RectangularPattern,
    CircularPattern,
    Dimension,
}

/// Presentation and interaction metadata for one exact tool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolDescriptor {
    pub variant: ToolVariant,
    pub stable_key: &'static str,
    pub family: ToolFamily,
    pub accessible_name: &'static str,
    pub short_tooltip: &'static str,
    pub extended_tooltip: &'static str,
    pub prompt: &'static str,
    pub chooser_accessible_name: &'static str,
    pub shortcut: Option<ToolShortcut>,
    pub icon: ToolIcon,
    pub cursor: ToolCursor,
    pub acquisition_phases: &'static [PointAcquisitionPhase],
    pub inputs: &'static [ToolInputField],
    pub selection: SelectionRequirement,
    pub output_role: ToolOutputRole,
    pub capability: CapabilityRequirement,
    pub disabled_reason: &'static str,
    pub commit_contract: CommitContract,
}

/// Static metadata for a visible split-button family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolFamilyDescriptor {
    pub family: ToolFamily,
    pub stable_key: &'static str,
    pub accessible_name: &'static str,
    pub shortcut: Option<ToolShortcut>,
    pub variants: &'static [ToolVariant],
    pub default_variant: ToolVariant,
}

const SHORTCUT_V: ToolShortcut = ToolShortcut {
    key: Key::V,
    label: "V",
};
const SHORTCUT_P: ToolShortcut = ToolShortcut {
    key: Key::P,
    label: "P",
};
const SHORTCUT_L: ToolShortcut = ToolShortcut {
    key: Key::L,
    label: "L",
};
const SHORTCUT_R: ToolShortcut = ToolShortcut {
    key: Key::R,
    label: "R",
};
const SHORTCUT_C: ToolShortcut = ToolShortcut {
    key: Key::C,
    label: "C",
};
const SHORTCUT_A: ToolShortcut = ToolShortcut {
    key: Key::A,
    label: "A",
};
const SHORTCUT_T: ToolShortcut = ToolShortcut {
    key: Key::T,
    label: "T",
};
const SHORTCUT_D: ToolShortcut = ToolShortcut {
    key: Key::D,
    label: "D",
};

const SELECT_VARIANTS: &[ToolVariant] = &[ToolVariant::Select];
const POINT_VARIANTS: &[ToolVariant] = &[ToolVariant::Point];
const LINE_VARIANTS: &[ToolVariant] = &[
    ToolVariant::SingleLine,
    ToolVariant::ChainedPolyline,
    ToolVariant::Centreline,
];
const RECTANGLE_VARIANTS: &[ToolVariant] = &[
    ToolVariant::TwoPointRectangle,
    ToolVariant::CentrePointRectangle,
];
const CIRCLE_VARIANTS: &[ToolVariant] =
    &[ToolVariant::CentrePointCircle, ToolVariant::TwoPointCircle];
const ARC_VARIANTS: &[ToolVariant] = &[ToolVariant::CentreStartEndArc, ToolVariant::ThreePointArc];
const POLYGON_VARIANTS: &[ToolVariant] = &[
    ToolVariant::InnerDiameterPolygon,
    ToolVariant::OuterDiameterPolygon,
];
const SLOT_VARIANTS: &[ToolVariant] = &[
    ToolVariant::TwoPointSlot,
    ToolVariant::CentreToOuterPointSlot,
];
const TRIM_VARIANTS: &[ToolVariant] = &[ToolVariant::Trim];
const FILLET_VARIANTS: &[ToolVariant] = &[ToolVariant::Fillet];
const CHAMFER_VARIANTS: &[ToolVariant] = &[ToolVariant::Chamfer, ToolVariant::TwoDistanceChamfer];
const PATTERN_VARIANTS: &[ToolVariant] = &[
    ToolVariant::RectangularPattern,
    ToolVariant::CircularPattern,
];
const DIMENSION_VARIANTS: &[ToolVariant] = &[ToolVariant::Dimension];

const TOOL_FAMILIES: [ToolFamilyDescriptor; ToolFamily::COUNT] = [
    family(
        ToolFamily::Select,
        "select",
        "Select",
        Some(SHORTCUT_V),
        SELECT_VARIANTS,
        ToolVariant::Select,
    ),
    family(
        ToolFamily::Point,
        "point",
        "Point",
        Some(SHORTCUT_P),
        POINT_VARIANTS,
        ToolVariant::Point,
    ),
    family(
        ToolFamily::Line,
        "line",
        "Line",
        Some(SHORTCUT_L),
        LINE_VARIANTS,
        ToolVariant::SingleLine,
    ),
    family(
        ToolFamily::Rectangle,
        "rectangle",
        "Rectangle",
        Some(SHORTCUT_R),
        RECTANGLE_VARIANTS,
        ToolVariant::TwoPointRectangle,
    ),
    family(
        ToolFamily::Circle,
        "circle",
        "Circle",
        Some(SHORTCUT_C),
        CIRCLE_VARIANTS,
        ToolVariant::CentrePointCircle,
    ),
    family(
        ToolFamily::Arc,
        "arc",
        "Arc",
        Some(SHORTCUT_A),
        ARC_VARIANTS,
        ToolVariant::CentreStartEndArc,
    ),
    family(
        ToolFamily::Polygon,
        "polygon",
        "Polygon",
        None,
        POLYGON_VARIANTS,
        ToolVariant::OuterDiameterPolygon,
    ),
    family(
        ToolFamily::Slot,
        "slot",
        "Slot",
        None,
        SLOT_VARIANTS,
        ToolVariant::TwoPointSlot,
    ),
    family(
        ToolFamily::Trim,
        "trim",
        "Trim",
        Some(SHORTCUT_T),
        TRIM_VARIANTS,
        ToolVariant::Trim,
    ),
    family(
        ToolFamily::Fillet,
        "fillet",
        "Fillet",
        None,
        FILLET_VARIANTS,
        ToolVariant::Fillet,
    ),
    family(
        ToolFamily::Chamfer,
        "chamfer",
        "Chamfer",
        None,
        CHAMFER_VARIANTS,
        ToolVariant::Chamfer,
    ),
    family(
        ToolFamily::Pattern,
        "pattern",
        "Pattern",
        None,
        PATTERN_VARIANTS,
        ToolVariant::RectangularPattern,
    ),
    family(
        ToolFamily::Dimension,
        "dimension",
        "Sketch dimension",
        Some(SHORTCUT_D),
        DIMENSION_VARIANTS,
        ToolVariant::Dimension,
    ),
];

const fn family(
    family: ToolFamily,
    stable_key: &'static str,
    accessible_name: &'static str,
    shortcut: Option<ToolShortcut>,
    variants: &'static [ToolVariant],
    default_variant: ToolVariant,
) -> ToolFamilyDescriptor {
    ToolFamilyDescriptor {
        family,
        stable_key,
        accessible_name,
        shortcut,
        variants,
        default_variant,
    }
}

const NO_PHASES: &[PointAcquisitionPhase] = &[];
const POINT_PHASES: &[PointAcquisitionPhase] =
    &[phase("point", "Click to place the sketch point.")];
const LINE_PHASES: &[PointAcquisitionPhase] = &[
    phase("start", "Click the line start point."),
    phase("end", "Click the line end point."),
];
const POLYLINE_PHASES: &[PointAcquisitionPhase] = &[
    phase("start", "Click the first polyline point."),
    phase(
        "next",
        "Click consecutive vertices; Enter, double-click, close to the first point, or Finish chain stages it.",
    ),
];
const CORNER_RECTANGLE_PHASES: &[PointAcquisitionPhase] = &[
    phase("corner", "Click the first rectangle corner."),
    phase("opposite", "Click the opposite rectangle corner."),
];
const CENTRE_RECTANGLE_PHASES: &[PointAcquisitionPhase] = &[
    phase("centre", "Click the rectangle centre."),
    phase("corner", "Click any outer corner."),
];
const CENTRE_CIRCLE_PHASES: &[PointAcquisitionPhase] = &[
    phase("centre", "Click the circle centre."),
    phase("circumference", "Click a point on the circumference."),
];
const TWO_POINT_CIRCLE_PHASES: &[PointAcquisitionPhase] = &[
    phase("diameter_start", "Click the first end of the diameter."),
    phase("diameter_end", "Click the opposite end of the diameter."),
];
const CENTRE_ARC_PHASES: &[PointAcquisitionPhase] = &[
    phase("centre", "Click the arc centre."),
    phase("start", "Click the arc start point."),
    phase("end", "Click the directed arc end point."),
];
const THREE_POINT_ARC_PHASES: &[PointAcquisitionPhase] = &[
    phase("start", "Click the arc start point."),
    phase("end", "Click the arc end point."),
    phase("through", "Click a point that the arc passes through."),
];
const INNER_POLYGON_PHASES: &[PointAcquisitionPhase] = &[
    phase("centre", "Click the polygon centre."),
    phase("side", "Click the midpoint direction of one polygon side."),
];
const OUTER_POLYGON_PHASES: &[PointAcquisitionPhase] = &[
    phase("centre", "Click the polygon centre."),
    phase("vertex", "Click one outer polygon vertex."),
];
const TWO_POINT_SLOT_PHASES: &[PointAcquisitionPhase] = &[
    phase("cap_start", "Click the first cap centre."),
    phase("cap_end", "Click the second cap centre."),
    phase("width", "Click to set the full slot width."),
];
const CENTRE_SLOT_PHASES: &[PointAcquisitionPhase] = &[
    phase("centre", "Click the overall slot centre."),
    phase("outer_tip", "Click one outer slot tip."),
    phase("width", "Click to set the full slot width."),
];
const TRIM_PHASES: &[PointAcquisitionPhase] = &[phase(
    "span",
    "Hover a curve span bounded by the nearest junctions, then click to stage its removal.",
)];
const FILLET_PHASES: &[PointAcquisitionPhase] = &[
    phase("first_curve", "Select the first connected profile curve."),
    phase("second_curve", "Select the second connected profile curve."),
];
const CHAMFER_PHASES: &[PointAcquisitionPhase] = &[
    phase("first_line", "Select the first connected profile line."),
    phase("second_line", "Select the second connected profile line."),
];
const RECTANGULAR_PATTERN_PHASES: &[PointAcquisitionPhase] = &[phase(
    "direction",
    "Drag the first pattern direction; the palette controls counts and spacing.",
)];
const CIRCULAR_PATTERN_PHASES: &[PointAcquisitionPhase] = &[phase(
    "centre",
    "Click or select the circular pattern centre.",
)];

const fn phase(stable_key: &'static str, prompt: &'static str) -> PointAcquisitionPhase {
    PointAcquisitionPhase { stable_key, prompt }
}

const NO_INPUTS: &[ToolInputField] = &[];
const POSITION_INPUTS: &[ToolInputField] = &[
    input(
        "u",
        "U position",
        ToolInputKind::SignedLength,
        "finite coordinate",
    ),
    input(
        "v",
        "V position",
        ToolInputKind::SignedLength,
        "finite coordinate",
    ),
];
const LINE_INPUTS: &[ToolInputField] = &[
    input(
        "length",
        "Length",
        ToolInputKind::Length,
        "greater than modeling resolution",
    ),
    input(
        "angle",
        "Angle",
        ToolInputKind::Angle,
        "finite directed angle",
    ),
];
const RECTANGLE_INPUTS: &[ToolInputField] = &[
    input(
        "width",
        "Width",
        ToolInputKind::Length,
        "positive finite length",
    ),
    input(
        "height",
        "Height",
        ToolInputKind::Length,
        "positive finite length",
    ),
];
const DIAMETER_INPUTS: &[ToolInputField] = &[input(
    "diameter",
    "Diameter",
    ToolInputKind::Length,
    "greater than twice modeling resolution",
)];
const ARC_INPUTS: &[ToolInputField] = &[
    input(
        "radius",
        "Radius",
        ToolInputKind::Length,
        "positive finite radius",
    ),
    input(
        "sweep",
        "Sweep angle",
        ToolInputKind::Angle,
        "non-zero and less than one full turn",
    ),
];
const INNER_POLYGON_INPUTS: &[ToolInputField] = &[
    input(
        "sides",
        "Sides",
        ToolInputKind::Integer,
        "integer from 3 to 256",
    ),
    input(
        "inner_diameter",
        "Inner diameter",
        ToolInputKind::Length,
        "positive finite across-flats diameter",
    ),
    input(
        "rotation",
        "Rotation",
        ToolInputKind::Angle,
        "finite directed angle",
    ),
];
const OUTER_POLYGON_INPUTS: &[ToolInputField] = &[
    input(
        "sides",
        "Sides",
        ToolInputKind::Integer,
        "integer from 3 to 256",
    ),
    input(
        "outer_diameter",
        "Outer diameter",
        ToolInputKind::Length,
        "positive finite across-corners diameter",
    ),
    input(
        "rotation",
        "Rotation",
        ToolInputKind::Angle,
        "finite directed angle",
    ),
];
const TWO_POINT_SLOT_INPUTS: &[ToolInputField] = &[
    input(
        "centre_distance",
        "Centre distance",
        ToolInputKind::Length,
        "positive finite cap-centre distance",
    ),
    input(
        "width",
        "Width",
        ToolInputKind::Length,
        "positive finite full width",
    ),
    input(
        "angle",
        "Angle",
        ToolInputKind::Angle,
        "finite directed angle",
    ),
];
const CENTRE_SLOT_INPUTS: &[ToolInputField] = &[
    input(
        "overall_length",
        "Overall length",
        ToolInputKind::Length,
        "greater than the full width",
    ),
    input(
        "width",
        "Width",
        ToolInputKind::Length,
        "positive finite full width",
    ),
    input(
        "angle",
        "Angle",
        ToolInputKind::Angle,
        "finite directed angle",
    ),
];
const FILLET_INPUTS: &[ToolInputField] = &[input(
    "radius",
    "Fillet radius",
    ToolInputKind::Length,
    "positive radius that leaves both carriers non-degenerate",
)];
const EQUAL_CHAMFER_INPUTS: &[ToolInputField] = &[input(
    "distance_1",
    "Chamfer distance",
    ToolInputKind::Length,
    "positive equal setback on both lines",
)];
const TWO_DISTANCE_CHAMFER_INPUTS: &[ToolInputField] = &[
    input(
        "distance_1",
        "First distance",
        ToolInputKind::Length,
        "positive setback on the first line",
    ),
    input(
        "distance_2",
        "Second distance",
        ToolInputKind::Length,
        "positive setback on the second line",
    ),
];
const RECTANGULAR_PATTERN_INPUTS: &[ToolInputField] = &[
    input(
        "count_u",
        "First count",
        ToolInputKind::Integer,
        "integer from 1 to 256 expanded instances",
    ),
    input(
        "spacing_u",
        "First spacing",
        ToolInputKind::SignedLength,
        "finite non-zero spacing",
    ),
    input(
        "second_direction",
        "Second direction",
        ToolInputKind::Boolean,
        "enabled or disabled",
    ),
    input(
        "count_v",
        "Second count",
        ToolInputKind::Integer,
        "integer preserving the 256-instance limit",
    ),
    input(
        "spacing_v",
        "Second spacing",
        ToolInputKind::SignedLength,
        "finite non-zero spacing",
    ),
];
const CIRCULAR_PATTERN_INPUTS: &[ToolInputField] = &[
    input(
        "count",
        "Count",
        ToolInputKind::Integer,
        "integer from 1 to 256 expanded instances",
    ),
    input(
        "full_circle",
        "Full circle",
        ToolInputKind::Boolean,
        "enabled or disabled",
    ),
    input(
        "extent",
        "Angular extent",
        ToolInputKind::Angle,
        "non-zero directed angle",
    ),
    input(
        "rotate_instances",
        "Rotate instances",
        ToolInputKind::Boolean,
        "rotate with the pattern or preserve source orientation",
    ),
];

const fn input(
    stable_key: &'static str,
    label: &'static str,
    kind: ToolInputKind,
    domain: &'static str,
) -> ToolInputField {
    ToolInputField {
        stable_key,
        label,
        kind,
        domain,
    }
}

const TOOL_DESCRIPTORS: [ToolDescriptor; ToolVariant::COUNT] = [
    descriptor(
        ToolVariant::Select,
        "sketch.select",
        ToolFamily::Select,
        "Select sketch geometry",
        "Select points, curves, operations, or profile regions.",
        "Click an item to select it. Shift-click extends the selection; Delete stages removal of editable geometry.",
        "Select sketch geometry.",
        "Select has no variants.",
        Some(SHORTCUT_V),
        ToolIcon::Select,
        ToolCursor::Arrow,
        NO_PHASES,
        NO_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::SessionOnly,
        CapabilityRequirement::EditableSketch,
        "Selection requires an editable sketch.",
        CommitContract::SessionOnly,
    ),
    descriptor(
        ToolVariant::Point,
        "sketch.point",
        ToolFamily::Point,
        "Sketch point",
        "Place an exact authored sketch point.",
        "Click to place the point. Tab edits U and V. Enter stages; the green tick commits.",
        "Click to place a sketch point.",
        "Point has no variants.",
        Some(SHORTCUT_P),
        ToolIcon::Point,
        ToolCursor::Crosshair,
        POINT_PHASES,
        POSITION_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Point creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::SingleLine,
        "sketch.line.single",
        ToolFamily::Line,
        "Single line",
        "Create one exact line segment.",
        "Click the start, then the end. Tab edits length and angle. Enter stages; the green tick commits.",
        "Click the line start point.",
        "Choose line type; current default: Single line.",
        Some(SHORTCUT_L),
        ToolIcon::SingleLine,
        ToolCursor::Crosshair,
        LINE_PHASES,
        LINE_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Line creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::ChainedPolyline,
        "sketch.line.polyline",
        ToolFamily::Line,
        "Chained polyline",
        "Create a connected chain of exact line segments.",
        "Click consecutive vertices. Tab edits the live segment. Enter, double-click, closing to the first point, or Finish chain stages the complete atomic chain; the green tick then commits it.",
        "Click the first polyline point.",
        "Choose line type; current default: Chained polyline.",
        Some(SHORTCUT_L),
        ToolIcon::Polyline,
        ToolCursor::Crosshair,
        POLYLINE_PHASES,
        LINE_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Polyline creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::Centreline,
        "sketch.line.centreline",
        ToolFamily::Line,
        "Centreline",
        "Create a construction line excluded from material profiles.",
        "Click two points. Tab edits length and angle. The centreline remains available for snapping and patterns but creates no profile region.",
        "Click the centreline start point.",
        "Choose line type; current default: Centreline.",
        Some(SHORTCUT_L),
        ToolIcon::Centreline,
        ToolCursor::Crosshair,
        LINE_PHASES,
        LINE_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ConstructionGeometry,
        CapabilityRequirement::EditableSketch,
        "Centreline creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::TwoPointRectangle,
        "sketch.rectangle.two_point",
        ToolFamily::Rectangle,
        "Two-point rectangle",
        "Create a rectangle from two opposite corners.",
        "Click one corner, then its opposite. Tab switches between width and height. Enter stages; the green tick commits all four edges.",
        "Click the first rectangle corner.",
        "Choose rectangle type; current default: Two-point rectangle.",
        Some(SHORTCUT_R),
        ToolIcon::CornerRectangle,
        ToolCursor::Crosshair,
        CORNER_RECTANGLE_PHASES,
        RECTANGLE_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Rectangle creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::CentrePointRectangle,
        "sketch.rectangle.centre_point",
        ToolFamily::Rectangle,
        "Centre-point rectangle",
        "Create a symmetric rectangle from its centre and a corner.",
        "Click the centre, then any corner. Tab switches between full width and full height. Enter stages; the green tick commits all four edges.",
        "Click the rectangle centre.",
        "Choose rectangle type; current default: Centre-point rectangle.",
        Some(SHORTCUT_R),
        ToolIcon::CentreRectangle,
        ToolCursor::Crosshair,
        CENTRE_RECTANGLE_PHASES,
        RECTANGLE_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Rectangle creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::CentrePointCircle,
        "sketch.circle.centre_point",
        ToolFamily::Circle,
        "Centre-point circle",
        "Create a circle from its centre and circumference.",
        "Click the centre, then a point on the circumference. Tab edits the diameter. Enter stages; the green tick commits.",
        "Click the circle centre.",
        "Choose circle type; current default: Centre-point circle.",
        Some(SHORTCUT_C),
        ToolIcon::CentreCircle,
        ToolCursor::Crosshair,
        CENTRE_CIRCLE_PHASES,
        DIAMETER_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Circle creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::TwoPointCircle,
        "sketch.circle.two_point",
        ToolFamily::Circle,
        "Two-point diameter circle",
        "Create a circle from the two ends of a diameter.",
        "Click both diameter endpoints. Tab edits the diameter while preserving the first endpoint and direction. Enter stages; the green tick commits.",
        "Click the first end of the circle diameter.",
        "Choose circle type; current default: Two-point diameter circle.",
        Some(SHORTCUT_C),
        ToolIcon::DiameterCircle,
        ToolCursor::Crosshair,
        TWO_POINT_CIRCLE_PHASES,
        DIAMETER_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Circle creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::CentreStartEndArc,
        "sketch.arc.centre_start_end",
        ToolFamily::Arc,
        "Centre-start-end arc",
        "Create a directed circular arc from its centre, start, and end.",
        "Click the centre, start, then directed end. Tab edits radius and sweep. Enter stages; the green tick commits.",
        "Click the arc centre.",
        "Choose arc type; current default: Centre-start-end arc.",
        Some(SHORTCUT_A),
        ToolIcon::CentreArc,
        ToolCursor::Crosshair,
        CENTRE_ARC_PHASES,
        ARC_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Arc creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::ThreePointArc,
        "sketch.arc.three_point",
        ToolFamily::Arc,
        "Three-point arc",
        "Create the unique supported arc through three points.",
        "Click the start, end, then a point on the arc. Collinear or under-resolved input remains editable and cannot be committed.",
        "Click the arc start point.",
        "Choose arc type; current default: Three-point arc.",
        Some(SHORTCUT_A),
        ToolIcon::ThreePointArc,
        ToolCursor::Crosshair,
        THREE_POINT_ARC_PHASES,
        ARC_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Arc creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::InnerDiameterPolygon,
        "sketch.polygon.inner_diameter",
        ToolFamily::Polygon,
        "Inner-diameter polygon",
        "Create a regular polygon sized across flats.",
        "Click the centre, then the midpoint direction of a side. Inner diameter is the circle tangent to every side; Tab edits sides, diameter, and rotation.",
        "Click the polygon centre.",
        "Choose polygon type; current default: Inner-diameter polygon.",
        None,
        ToolIcon::InnerPolygon,
        ToolCursor::Crosshair,
        INNER_POLYGON_PHASES,
        INNER_POLYGON_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Polygon creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::OuterDiameterPolygon,
        "sketch.polygon.outer_diameter",
        ToolFamily::Polygon,
        "Outer-diameter polygon",
        "Create a regular polygon sized across corners.",
        "Click the centre, then a vertex. Outer diameter is the circle through every vertex; Tab edits sides, diameter, and rotation.",
        "Click the polygon centre.",
        "Choose polygon type; current default: Outer-diameter polygon.",
        None,
        ToolIcon::OuterPolygon,
        ToolCursor::Crosshair,
        OUTER_POLYGON_PHASES,
        OUTER_POLYGON_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Polygon creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::TwoPointSlot,
        "sketch.slot.two_point",
        ToolFamily::Slot,
        "Two-point centre-to-centre slot",
        "Create a slot from its two cap centres and full width.",
        "Click both cap centres, then set the full width. Tab edits centre distance, width, and angle. Both rails and semicircular caps commit atomically.",
        "Click the first slot cap centre.",
        "Choose slot type; current default: Two-point centre-to-centre slot.",
        None,
        ToolIcon::TwoPointSlot,
        ToolCursor::Crosshair,
        TWO_POINT_SLOT_PHASES,
        TWO_POINT_SLOT_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Slot creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::CentreToOuterPointSlot,
        "sketch.slot.centre_outer",
        ToolFamily::Slot,
        "Centre-to-outer-point slot",
        "Create a symmetric slot from its overall centre and one outer tip.",
        "Click the overall centre, one outer tip, then set width. The opposite tip is reflected through the centre; Tab edits overall length, width, and angle.",
        "Click the overall slot centre.",
        "Choose slot type; current default: Centre-to-outer-point slot.",
        None,
        ToolIcon::CentreSlot,
        ToolCursor::Crosshair,
        CENTRE_SLOT_PHASES,
        CENTRE_SLOT_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Slot creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::Trim,
        "sketch.modify.trim",
        ToolFamily::Trim,
        "Trim curve span",
        "Remove only the span between the nearest certified junctions.",
        "Hover the span to preview it, then click to stage the atomic split and removal. Enter stages; the green tick commits.",
        "Hover a bounded curve span to trim it.",
        "Trim has no variants.",
        Some(SHORTCUT_T),
        ToolIcon::Trim,
        ToolCursor::Trim,
        TRIM_PHASES,
        NO_INPUTS,
        SelectionRequirement::CurveSpanUnderPointer,
        ToolOutputRole::Modification,
        CapabilityRequirement::TrimCandidate,
        "Trim requires an editable profile span bounded by certified junctions.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::Fillet,
        "sketch.modify.fillet",
        ToolFamily::Fillet,
        "2D fillet",
        "Replace a supported connected curve pair with a tangent circular arc.",
        "Select two connected profile curves. Tab edits radius. A valid tangent preview stages both carrier edits and the new arc as one operation.",
        "Select the first curve to fillet.",
        "Fillet has no variants.",
        None,
        ToolIcon::Fillet,
        ToolCursor::PrecisionPick,
        FILLET_PHASES,
        FILLET_INPUTS,
        SelectionRequirement::TwoConnectedProfileCurves,
        ToolOutputRole::Modification,
        CapabilityRequirement::FilletPair,
        "Fillet requires two connected supported profile curves and enough carrier length for the radius.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::Chamfer,
        "sketch.modify.chamfer.equal",
        ToolFamily::Chamfer,
        "Equal-distance chamfer",
        "Replace a connected line-line corner using one equal setback.",
        "Enter one positive distance, then select two connected profile lines. Both carrier edits and the chamfer commit atomically.",
        "Select the first line to chamfer.",
        "Choose chamfer type; current default: Equal-distance chamfer.",
        None,
        ToolIcon::Chamfer,
        ToolCursor::PrecisionPick,
        CHAMFER_PHASES,
        EQUAL_CHAMFER_INPUTS,
        SelectionRequirement::TwoConnectedProfileLines,
        ToolOutputRole::Modification,
        CapabilityRequirement::ChamferLinePair,
        "Chamfer requires two connected non-collinear profile lines and valid positive setbacks.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::TwoDistanceChamfer,
        "sketch.modify.chamfer.two_distances",
        ToolFamily::Chamfer,
        "Two-distance chamfer",
        "Replace a connected line-line corner using independent setbacks.",
        "Enter two positive distances, then select two connected profile lines. Source order follows the first and second picks and persists exactly.",
        "Select the first line to chamfer.",
        "Choose chamfer type; current default: Equal-distance chamfer.",
        None,
        ToolIcon::Chamfer,
        ToolCursor::PrecisionPick,
        CHAMFER_PHASES,
        TWO_DISTANCE_CHAMFER_INPUTS,
        SelectionRequirement::TwoConnectedProfileLines,
        ToolOutputRole::Modification,
        CapabilityRequirement::ChamferLinePair,
        "Chamfer requires two connected non-collinear profile lines and valid positive setbacks.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::RectangularPattern,
        "sketch.pattern.rectangular",
        ToolFamily::Pattern,
        "Rectangular sketch pattern",
        "Repeat selected sketch geometry in one or two linear directions.",
        "Select editable seed geometry, then drag the first direction. Tab edits counts, signed spacing, and the optional second direction. Generated instances commit atomically.",
        "Select pattern seeds, then set the first direction.",
        "Choose pattern type; current default: Rectangular sketch pattern.",
        None,
        ToolIcon::RectangularPattern,
        ToolCursor::PrecisionPick,
        RECTANGULAR_PATTERN_PHASES,
        RECTANGULAR_PATTERN_INPUTS,
        SelectionRequirement::OneOrMoreEditableEntities,
        ToolOutputRole::GeneratedGeometry,
        CapabilityRequirement::PatternSeedSelection,
        "A rectangular pattern requires one or more editable seed entities.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::CircularPattern,
        "sketch.pattern.circular",
        ToolFamily::Pattern,
        "Circular sketch pattern",
        "Repeat selected sketch geometry around a chosen centre.",
        "Select editable seed geometry, then choose the centre. Tab edits count, angular extent, and full-circle mode. Generated instances commit atomically.",
        "Select pattern seeds, then choose the centre.",
        "Choose pattern type; current default: Circular sketch pattern.",
        None,
        ToolIcon::CircularPattern,
        ToolCursor::PrecisionPick,
        CIRCULAR_PATTERN_PHASES,
        CIRCULAR_PATTERN_INPUTS,
        SelectionRequirement::OneOrMoreEditableEntities,
        ToolOutputRole::GeneratedGeometry,
        CapabilityRequirement::PatternSeedSelection,
        "A circular pattern requires one or more editable seed entities and a valid centre.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::Dimension,
        "sketch.dimension",
        ToolFamily::Dimension,
        "Sketch dimension",
        "Display and edit a driving dimension on selected sketch geometry.",
        "Click a sketch curve to display its dimensions. Edit the driving value in the palette; the change previews immediately and commits through the green tick.",
        "Select sketch geometry to dimension.",
        "Dimension has no variants.",
        Some(SHORTCUT_D),
        ToolIcon::Dimension,
        ToolCursor::PrecisionPick,
        NO_PHASES,
        NO_INPUTS,
        SelectionRequirement::OneOrMoreEditableEntities,
        ToolOutputRole::SessionOnly,
        CapabilityRequirement::EditableSketch,
        "Dimensions require an editable sketch entity.",
        CommitContract::SessionOnly,
    ),
];

#[allow(clippy::too_many_arguments)]
const fn descriptor(
    variant: ToolVariant,
    stable_key: &'static str,
    family: ToolFamily,
    accessible_name: &'static str,
    short_tooltip: &'static str,
    extended_tooltip: &'static str,
    prompt: &'static str,
    chooser_accessible_name: &'static str,
    shortcut: Option<ToolShortcut>,
    icon: ToolIcon,
    cursor: ToolCursor,
    acquisition_phases: &'static [PointAcquisitionPhase],
    inputs: &'static [ToolInputField],
    selection: SelectionRequirement,
    output_role: ToolOutputRole,
    capability: CapabilityRequirement,
    disabled_reason: &'static str,
    commit_contract: CommitContract,
) -> ToolDescriptor {
    ToolDescriptor {
        variant,
        stable_key,
        family,
        accessible_name,
        short_tooltip,
        extended_tooltip,
        prompt,
        chooser_accessible_name,
        shortcut,
        icon,
        cursor,
        acquisition_phases,
        inputs,
        selection,
        output_role,
        capability,
        disabled_reason,
        commit_contract,
    }
}

/// Stateless authority used by toolbar, shortcuts, palette, and semantic tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct SketchToolRegistry;

impl SketchToolRegistry {
    #[must_use]
    pub const fn families() -> &'static [ToolFamilyDescriptor] {
        &TOOL_FAMILIES
    }

    #[must_use]
    pub const fn tools() -> &'static [ToolDescriptor] {
        &TOOL_DESCRIPTORS
    }

    #[must_use]
    pub fn family_for_shortcut(key: Key) -> Option<ToolFamily> {
        TOOL_FAMILIES
            .iter()
            .find(|descriptor| {
                descriptor
                    .shortcut
                    .is_some_and(|shortcut| shortcut.key == key)
            })
            .map(|descriptor| descriptor.family)
    }
}

/// Session-local last-used family defaults.  This is intentionally not model data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SketchToolPreferences {
    last_used: [ToolVariant; ToolFamily::COUNT],
}

impl Default for SketchToolPreferences {
    fn default() -> Self {
        Self {
            last_used: array::from_fn(|index| ToolFamily::ALL[index].default_variant()),
        }
    }
}

impl SketchToolPreferences {
    #[must_use]
    pub fn last_used(&self, family: ToolFamily) -> ToolVariant {
        self.last_used[family as usize]
    }

    pub fn remember(&mut self, variant: ToolVariant) {
        self.last_used[variant.family() as usize] = variant;
    }

    /// Resolve a family shortcut to its current exact default.
    #[must_use]
    pub fn variant_for_shortcut(&self, key: Key) -> Option<ToolVariant> {
        SketchToolRegistry::family_for_shortcut(key).map(|family| self.last_used(family))
    }
}

/// Dynamic per-tool availability supplied by the active sketch session.
#[derive(Clone, Debug)]
pub struct SketchToolCapabilities {
    disabled_reasons: [Option<String>; ToolVariant::COUNT],
}

impl Default for SketchToolCapabilities {
    fn default() -> Self {
        Self {
            disabled_reasons: array::from_fn(|_| None),
        }
    }
}

impl SketchToolCapabilities {
    #[must_use]
    pub fn is_enabled(&self, variant: ToolVariant) -> bool {
        self.disabled_reasons[variant as usize].is_none()
    }

    #[must_use]
    pub fn disabled_reason(&self, variant: ToolVariant) -> Option<&str> {
        self.disabled_reasons[variant as usize].as_deref()
    }

    /// Disable a tool with its registry-owned supported-domain explanation.
    pub fn disable(&mut self, variant: ToolVariant) {
        self.disabled_reasons[variant as usize] = Some(variant.descriptor().disabled_reason.into());
    }

    /// Disable a tool with a more specific live-session explanation.
    ///
    /// Empty explanations are replaced by the registry default so every
    /// disabled control remains understandable and accessible.
    pub fn disable_with_reason(&mut self, variant: ToolVariant, reason: impl Into<String>) {
        let reason = reason.into();
        self.disabled_reasons[variant as usize] = Some(if reason.trim().is_empty() {
            variant.descriptor().disabled_reason.into()
        } else {
            reason
        });
    }

    pub fn enable(&mut self, variant: ToolVariant) {
        self.disabled_reasons[variant as usize] = None;
    }
}

/// State of the application's universal model-operation gate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SketchOperationGate {
    #[default]
    Ready,
    AwaitingConfirmation,
}

impl SketchOperationGate {
    #[must_use]
    pub const fn disabled_reason(self) -> Option<&'static str> {
        match self {
            Self::Ready => None,
            Self::AwaitingConfirmation => Some(PENDING_CONFIRMATION_DISABLED_REASON),
        }
    }
}

/// Persistent UI state for the toolbar.  Popup open state remains in egui memory.
#[derive(Clone, Debug, Default)]
pub struct SketchToolbarState {
    preferences: SketchToolPreferences,
}

impl SketchToolbarState {
    #[must_use]
    pub fn preferences(&self) -> &SketchToolPreferences {
        &self.preferences
    }

    pub fn preferences_mut(&mut self) -> &mut SketchToolPreferences {
        &mut self.preferences
    }

    /// Resolve a shortcut while respecting both capability and confirmation gates.
    #[must_use]
    pub fn enabled_variant_for_shortcut(
        &self,
        key: Key,
        gate: SketchOperationGate,
        capabilities: &SketchToolCapabilities,
    ) -> Option<ToolVariant> {
        if gate == SketchOperationGate::AwaitingConfirmation {
            return None;
        }
        self.preferences
            .variant_for_shortcut(key)
            .filter(|variant| capabilities.is_enabled(*variant))
    }
}

/// Screen-space rectangles for a family primary and optional chooser.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToolControlLayout {
    pub primary: Rect,
    pub chooser: Option<Rect>,
}

/// Result of one toolbar frame.
#[derive(Clone, Debug)]
pub struct SketchToolbarOutput {
    /// Exact variant selected by a primary cell, family menu, or keyboard route.
    pub chosen: Option<ToolVariant>,
    /// Tight union of persistent toolbar controls; popup menus are excluded.
    pub bounds: Option<Rect>,
    /// Per-family geometry, indexed by [`ToolFamily`] discriminant.
    pub controls: [Option<ToolControlLayout>; ToolFamily::COUNT],
    /// True while any family chooser is open after this frame.
    pub menu_open: bool,
    /// True when this module consumed Escape to close a chooser.
    pub escape_consumed: bool,
}

impl Default for SketchToolbarOutput {
    fn default() -> Self {
        Self {
            chosen: None,
            bounds: None,
            controls: array::from_fn(|_| None),
            menu_open: false,
            escape_consumed: false,
        }
    }
}

const FIRST_ROW: &[ToolFamily] = &[
    ToolFamily::Select,
    ToolFamily::Point,
    ToolFamily::Line,
    ToolFamily::Rectangle,
    ToolFamily::Circle,
    ToolFamily::Arc,
];
const SECOND_ROW: &[ToolFamily] = &[
    ToolFamily::Polygon,
    ToolFamily::Slot,
    ToolFamily::Trim,
    ToolFamily::Fillet,
    ToolFamily::Chamfer,
    ToolFamily::Pattern,
    ToolFamily::Dimension,
];

/// Paint the compact two-row sketch tool grid and return an exact chosen tool.
///
/// When `gate` is [`SketchOperationGate::AwaitingConfirmation`], the renderer
/// unconditionally disables every primary and chooser control.  This prevents
/// per-tool capability plumbing from bypassing the permanent green-tick/red-
/// cross operation rail.
pub fn render_sketch_toolbar(
    ui: &mut Ui,
    state: &mut SketchToolbarState,
    active: ToolVariant,
    gate: SketchOperationGate,
    capabilities: &SketchToolCapabilities,
) -> SketchToolbarOutput {
    let mut output = SketchToolbarOutput::default();
    let mut escaped_anchor = None;

    ui.allocate_ui_with_layout(
        vec2(SKETCH_TOOLBAR_WIDTH, SKETCH_TOOLBAR_HEIGHT),
        Layout::top_down(Align::Min),
        |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(TOOLBAR_TOP_PADDING);
            render_toolbar_row(
                ui,
                FIRST_ROW,
                &mut state.preferences,
                active,
                gate,
                capabilities,
                &mut output,
                &mut escaped_anchor,
            );
            ui.add_space(ROW_GAP);
            render_toolbar_row(
                ui,
                SECOND_ROW,
                &mut state.preferences,
                active,
                gate,
                capabilities,
                &mut output,
                &mut escaped_anchor,
            );
            ui.add_space(TOOLBAR_BOTTOM_PADDING);
        },
    );

    if let Some(anchor) = escaped_anchor {
        // Popup handling may already have consumed the raw Escape event. The
        // open-to-closed transition above is the semantic authority, so still
        // report it and return focus to the family chooser deterministically.
        let _ = ui
            .ctx()
            .input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape));
        output.escape_consumed = true;
        ui.ctx().memory_mut(|memory| memory.request_focus(anchor));
    }

    output
}

#[allow(clippy::too_many_arguments)]
fn render_toolbar_row(
    ui: &mut Ui,
    families: &[ToolFamily],
    preferences: &mut SketchToolPreferences,
    active: ToolVariant,
    gate: SketchOperationGate,
    capabilities: &SketchToolCapabilities,
    output: &mut SketchToolbarOutput,
    escaped_anchor: &mut Option<egui::Id>,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = FAMILY_GAP;
        for family in families {
            let current = preferences.last_used(*family);
            let result = render_family(ui, *family, current, active, gate, capabilities);
            if let Some(chosen) = result.chosen {
                preferences.remember(chosen);
                output.chosen = Some(chosen);
            }
            output.menu_open |= result.menu_open;
            if result.escape_was_pressed {
                *escaped_anchor = Some(result.focus_anchor);
            }
            output.controls[*family as usize] = Some(result.layout);
            output.bounds = Some(match output.bounds {
                Some(bounds) => bounds
                    .union(result.layout.primary)
                    .union_opt(result.layout.chooser),
                None => result.layout.primary.union_opt(result.layout.chooser),
            });
        }
    });
}

trait RectOptionUnion {
    fn union_opt(self, other: Option<Rect>) -> Self;
}

impl RectOptionUnion for Rect {
    fn union_opt(self, other: Option<Rect>) -> Self {
        other.map_or(self, |rect| self.union(rect))
    }
}

struct FamilyRenderResult {
    chosen: Option<ToolVariant>,
    layout: ToolControlLayout,
    menu_open: bool,
    escape_was_pressed: bool,
    focus_anchor: egui::Id,
}

fn render_family(
    ui: &mut Ui,
    family: ToolFamily,
    current: ToolVariant,
    active: ToolVariant,
    gate: SketchOperationGate,
    capabilities: &SketchToolCapabilities,
) -> FamilyRenderResult {
    let split = family.variants().len() > 1;
    let (_, cell_rect) = ui.allocate_space(vec2(PRIMARY_CELL_SIZE, PRIMARY_CELL_SIZE));
    let mut family_ui = ui.new_child(
        UiBuilder::new()
            .id_salt(("sketch_tool_family", family))
            .max_rect(cell_rect),
    );
    let ui = &mut family_ui;
    let primary_reason = gate
        .disabled_reason()
        .or_else(|| capabilities.disabled_reason(current));
    let primary = icon_button(
        ui,
        cell_rect,
        current.descriptor(),
        active == current,
        primary_reason,
    );
    let mut chosen = primary.clicked().then_some(current);
    let mut chooser_rect = None;
    let mut chooser_hovered = false;
    let mut menu_open = false;
    let mut escape_was_pressed = false;
    let mut focus_anchor = primary.id;

    if split {
        let family_has_enabled_variant = family
            .variants()
            .iter()
            .any(|variant| capabilities.is_enabled(*variant));
        let chooser_reason = gate.disabled_reason().or_else(|| {
            (!family_has_enabled_variant).then_some(current.descriptor().disabled_reason)
        });
        let chooser_max = cell_rect.max - vec2(CHEVRON_CELL_INSET, CHEVRON_CELL_INSET);
        let contained_chooser_rect = Rect::from_min_max(
            chooser_max - vec2(CHEVRON_CELL_WIDTH, CHEVRON_CELL_WIDTH),
            chooser_max,
        );
        let chooser = chooser_button(
            ui,
            contained_chooser_rect,
            current.descriptor(),
            chooser_reason,
        );
        chooser_rect = Some(chooser.rect);
        chooser_hovered = chooser.hovered();
        focus_anchor = chooser.id;
        let popup_id = egui::Popup::default_response_id(&chooser);
        let was_open = egui::Popup::is_id_open(ui.ctx(), popup_id);
        let escape_was_requested =
            was_open && ui.ctx().input(|input| input.key_pressed(Key::Escape));

        // A newly staged operation owns keyboard and pointer input. Close a
        // chooser that was opened before the operation became pending so its
        // menu rows cannot outlive their disabled owner.
        if gate == SketchOperationGate::AwaitingConfirmation {
            egui::Popup::close_id(ui.ctx(), popup_id);
        }

        let keyboard_open = chooser_reason.is_none()
            && (chooser.has_focus() || primary.has_focus())
            && ui.ctx().input_mut(|input| {
                input.consume_key(Modifiers::NONE, Key::ArrowDown)
                    || input.consume_key(Modifiers::ALT, Key::ArrowDown)
            });
        if keyboard_open {
            egui::Popup::open_id(ui.ctx(), popup_id);
        }

        let popup_result = egui::Popup::menu(&chooser)
            .width(246.0)
            .show(|menu_ui| render_variant_menu(menu_ui, family, current, capabilities));
        if let Some(menu_result) = popup_result.and_then(|inner| inner.inner) {
            chosen = Some(menu_result);
            egui::Popup::close_id(ui.ctx(), popup_id);
            ui.ctx()
                .memory_mut(|memory| memory.request_focus(primary.id));
            focus_anchor = primary.id;
        }
        menu_open = egui::Popup::is_id_open(ui.ctx(), popup_id);
        escape_was_pressed = escape_was_requested;
    }

    paint_family_cell_boundary(
        ui,
        cell_rect,
        active == current,
        primary.hovered() || chooser_hovered,
    );

    FamilyRenderResult {
        chosen,
        layout: ToolControlLayout {
            primary: cell_rect,
            chooser: chooser_rect,
        },
        menu_open,
        escape_was_pressed,
        focus_anchor,
    }
}

fn icon_button(
    ui: &mut Ui,
    rect: Rect,
    descriptor: &'static ToolDescriptor,
    selected: bool,
    disabled_reason: Option<&str>,
) -> Response {
    let enabled = disabled_reason.is_none();
    let button = Button::new(())
        .min_size(rect.size())
        .selected(selected)
        .corner_radius(4.0);
    let mut button_ui = ui.new_child(
        UiBuilder::new()
            .id_salt((descriptor.family, "primary"))
            .max_rect(rect)
            .layout(Layout::centered_and_justified(Direction::TopDown)),
    );
    if !enabled {
        button_ui.disable();
    }
    let mut response = button_ui.add_sized(rect.size(), button);
    let icon_color = if enabled {
        ui.style()
            .interact_selectable(&response, selected)
            .fg_stroke
            .color
    } else {
        ui.visuals().weak_text_color()
    };
    paint_tool_icon(
        ui.painter(),
        Rect::from_center_size(response.rect.center(), vec2(21.0, 21.0)),
        descriptor.icon,
        icon_color,
    );
    if selected {
        ui.painter().rect_stroke(
            response.rect.shrink(1.0),
            3.0,
            Stroke::new(1.4, ui.visuals().selection.stroke.color),
            StrokeKind::Inside,
        );
    }
    if let Some(reason) = disabled_reason {
        response = response.on_disabled_hover_text(reason);
    } else {
        response = response.on_hover_ui(|tooltip| descriptor_tooltip(tooltip, descriptor));
    }
    response.widget_info(|| {
        WidgetInfo::selected(
            WidgetType::Button,
            enabled,
            selected,
            descriptor.accessible_name,
        )
    });
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_label(descriptor.accessible_name);
        node.set_description(descriptor.extended_tooltip);
    });
    response
}

fn chooser_button(
    ui: &mut Ui,
    rect: Rect,
    descriptor: &'static ToolDescriptor,
    disabled_reason: Option<&str>,
) -> Response {
    let enabled = disabled_reason.is_none();
    let mut button_ui = ui.new_child(
        UiBuilder::new()
            .id_salt((descriptor.family, "chooser"))
            .max_rect(rect)
            .layout(Layout::centered_and_justified(Direction::TopDown)),
    );
    if !enabled {
        button_ui.disable();
    }
    let chooser_id = button_ui.make_persistent_id("button");
    let mut response = button_ui.interact(rect, chooser_id, Sense::click());
    let visuals = button_ui.style().interact(&response);
    button_ui.painter().rect(
        rect,
        2.0,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
        StrokeKind::Inside,
    );
    let color = if enabled {
        visuals.fg_stroke.color
    } else {
        button_ui.visuals().weak_text_color()
    };
    paint_chevron(button_ui.painter(), response.rect, color);
    if let Some(reason) = disabled_reason {
        response = response.on_disabled_hover_text(reason);
    } else {
        response = response.on_hover_text(descriptor.chooser_accessible_name);
    }
    response.widget_info(|| {
        WidgetInfo::labeled(
            WidgetType::Button,
            enabled,
            descriptor.chooser_accessible_name,
        )
    });
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_label(descriptor.chooser_accessible_name);
        node.set_description("Open the exact tool variants in this sketch family.");
    });
    response
}

fn paint_family_cell_boundary(ui: &Ui, rect: Rect, selected: bool, hovered: bool) {
    let color = if selected {
        ui.visuals().selection.stroke.color
    } else if hovered {
        ui.visuals().widgets.hovered.bg_stroke.color
    } else {
        ui.visuals()
            .widgets
            .noninteractive
            .bg_stroke
            .color
            .gamma_multiply(0.7)
    };
    let width = if selected { 1.4 } else { 1.0 };
    ui.painter().rect_stroke(
        rect.shrink(0.5),
        4.0,
        Stroke::new(width, color),
        StrokeKind::Inside,
    );
}

fn descriptor_tooltip(ui: &mut Ui, descriptor: &ToolDescriptor) {
    ui.set_max_width(330.0);
    ui.strong(descriptor.accessible_name);
    if let Some(shortcut) = descriptor.shortcut {
        ui.weak(format!("Shortcut: {}", shortcut.label));
    }
    ui.label(descriptor.short_tooltip);
    ui.label(descriptor.extended_tooltip);
}

fn render_variant_menu(
    ui: &mut Ui,
    family: ToolFamily,
    current: ToolVariant,
    capabilities: &SketchToolCapabilities,
) -> Option<ToolVariant> {
    let mut chosen = None;
    for variant in family.variants() {
        let descriptor = variant.descriptor();
        let enabled = capabilities.is_enabled(*variant);
        let text = format!("      {}", descriptor.accessible_name);
        let mut button = Button::new(text)
            .selected(*variant == current)
            .min_size(vec2(236.0, 30.0));
        if let Some(shortcut) = descriptor.shortcut {
            button = button.shortcut_text(shortcut.label);
        }
        let mut response = ui.add_enabled(enabled, button);
        let icon_rect = Rect::from_center_size(
            pos2(response.rect.left() + 16.0, response.rect.center().y),
            vec2(18.0, 18.0),
        );
        let icon_color = if enabled {
            ui.style()
                .interact_selectable(&response, *variant == current)
                .fg_stroke
                .color
        } else {
            ui.visuals().weak_text_color()
        };
        paint_tool_icon(ui.painter(), icon_rect, descriptor.icon, icon_color);
        if enabled {
            response = response.on_hover_ui(|tooltip| descriptor_tooltip(tooltip, descriptor));
        } else if let Some(reason) = capabilities.disabled_reason(*variant) {
            response = response.on_disabled_hover_text(reason);
        }
        response.widget_info(|| {
            WidgetInfo::selected(
                WidgetType::Button,
                enabled,
                *variant == current,
                descriptor.accessible_name,
            )
        });
        if response.clicked() {
            chosen = Some(*variant);
            ui.close();
        }
    }
    chosen
}

fn paint_chevron(painter: &Painter, rect: Rect, color: Color32) {
    let centre = rect.center();
    painter.line_segment(
        [centre + vec2(-2.5, -1.5), centre + vec2(0.0, 1.5)],
        Stroke::new(1.2, color),
    );
    painter.line_segment(
        [centre + vec2(0.0, 1.5), centre + vec2(2.5, -1.5)],
        Stroke::new(1.2, color),
    );
}

/// Paint one deterministic code-authored vector icon.
pub fn paint_tool_icon(painter: &Painter, rect: Rect, icon: ToolIcon, color: Color32) {
    IconPainter::new(painter, rect, color).paint(icon);
}

struct IconPainter<'a> {
    painter: &'a Painter,
    rect: Rect,
    color: Color32,
}

impl<'a> IconPainter<'a> {
    fn new(painter: &'a Painter, rect: Rect, color: Color32) -> Self {
        Self {
            painter,
            rect,
            color,
        }
    }

    fn p(&self, x: f32, y: f32) -> Pos2 {
        pos2(
            self.rect.left() + self.rect.width() * x,
            self.rect.top() + self.rect.height() * y,
        )
    }

    fn stroke(&self) -> Stroke {
        Stroke::new((self.rect.width() / 15.0).clamp(1.1, 1.7), self.color)
    }

    fn line(&self, a: (f32, f32), b: (f32, f32)) {
        self.painter
            .line_segment([self.p(a.0, a.1), self.p(b.0, b.1)], self.stroke());
    }

    fn dot(&self, point: (f32, f32), radius: f32) {
        self.painter.circle_filled(
            self.p(point.0, point.1),
            self.rect.width() * radius,
            self.color,
        );
    }

    fn circle(&self, centre: (f32, f32), radius: f32) {
        self.painter.circle_stroke(
            self.p(centre.0, centre.1),
            self.rect.width() * radius,
            self.stroke(),
        );
    }

    fn arc(&self, centre: (f32, f32), radius: f32, start: f32, sweep: f32) {
        let segments = 12;
        for index in 0..segments {
            let a = start + sweep * index as f32 / segments as f32;
            let b = start + sweep * (index + 1) as f32 / segments as f32;
            let from = self.p(centre.0 + radius * a.cos(), centre.1 + radius * a.sin());
            let to = self.p(centre.0 + radius * b.cos(), centre.1 + radius * b.sin());
            self.painter.line_segment([from, to], self.stroke());
        }
    }

    fn polygon(&self, centre: (f32, f32), radius: f32, sides: usize, rotation: f32) {
        for index in 0..sides {
            let a = rotation + TAU * index as f32 / sides as f32;
            let b = rotation + TAU * (index + 1) as f32 / sides as f32;
            self.line(
                (centre.0 + radius * a.cos(), centre.1 + radius * a.sin()),
                (centre.0 + radius * b.cos(), centre.1 + radius * b.sin()),
            );
        }
    }

    fn paint(&self, kind: ToolIcon) {
        match kind {
            ToolIcon::Select => {
                let points = [
                    (0.22, 0.12),
                    (0.28, 0.78),
                    (0.45, 0.61),
                    (0.58, 0.88),
                    (0.72, 0.81),
                    (0.58, 0.55),
                    (0.82, 0.52),
                ];
                for index in 0..points.len() {
                    self.line(points[index], points[(index + 1) % points.len()]);
                }
            }
            ToolIcon::Point => {
                self.line((0.18, 0.50), (0.82, 0.50));
                self.line((0.50, 0.18), (0.50, 0.82));
                self.dot((0.50, 0.50), 0.095);
            }
            ToolIcon::SingleLine => {
                self.line((0.18, 0.78), (0.82, 0.22));
                self.dot((0.18, 0.78), 0.07);
                self.dot((0.82, 0.22), 0.07);
            }
            ToolIcon::Polyline => {
                self.line((0.10, 0.72), (0.35, 0.34));
                self.line((0.35, 0.34), (0.61, 0.66));
                self.line((0.61, 0.66), (0.89, 0.23));
                self.dot((0.10, 0.72), 0.045);
                self.dot((0.35, 0.34), 0.045);
                self.dot((0.61, 0.66), 0.045);
                self.dot((0.89, 0.23), 0.045);
            }
            ToolIcon::Centreline => {
                for index in 0..4 {
                    let start = 0.10 + index as f32 * 0.22;
                    self.line(
                        (start, 0.70 - start * 0.45),
                        (start + 0.13, 0.64 - start * 0.45),
                    );
                }
                self.dot((0.50, 0.50), 0.055);
            }
            ToolIcon::CornerRectangle => {
                self.line((0.16, 0.22), (0.84, 0.22));
                self.line((0.84, 0.22), (0.84, 0.78));
                self.line((0.84, 0.78), (0.16, 0.78));
                self.line((0.16, 0.78), (0.16, 0.22));
                self.dot((0.16, 0.78), 0.055);
                self.dot((0.84, 0.22), 0.055);
            }
            ToolIcon::CentreRectangle => {
                self.line((0.14, 0.23), (0.86, 0.23));
                self.line((0.86, 0.23), (0.86, 0.77));
                self.line((0.86, 0.77), (0.14, 0.77));
                self.line((0.14, 0.77), (0.14, 0.23));
                self.line((0.38, 0.50), (0.62, 0.50));
                self.line((0.50, 0.38), (0.50, 0.62));
            }
            ToolIcon::CentreCircle => {
                self.circle((0.50, 0.50), 0.34);
                self.line((0.50, 0.50), (0.78, 0.31));
                self.dot((0.50, 0.50), 0.06);
            }
            ToolIcon::DiameterCircle => {
                self.circle((0.50, 0.50), 0.34);
                self.line((0.19, 0.63), (0.81, 0.37));
                self.dot((0.19, 0.63), 0.045);
                self.dot((0.81, 0.37), 0.045);
            }
            ToolIcon::CentreArc => {
                self.arc((0.50, 0.55), 0.34, PI * 1.05, PI * 1.35);
                self.line((0.50, 0.55), (0.17, 0.50));
                self.line((0.50, 0.55), (0.72, 0.29));
                self.dot((0.50, 0.55), 0.05);
            }
            ToolIcon::ThreePointArc => {
                self.arc((0.50, 0.63), 0.37, PI * 1.08, PI * 0.84);
                self.dot((0.14, 0.54), 0.05);
                self.dot((0.50, 0.26), 0.05);
                self.dot((0.86, 0.54), 0.05);
            }
            ToolIcon::InnerPolygon => {
                self.polygon((0.50, 0.50), 0.37, 6, 0.0);
                self.circle((0.50, 0.50), 0.31);
                self.line((0.50, 0.50), (0.50, 0.19));
            }
            ToolIcon::OuterPolygon => {
                self.circle((0.50, 0.50), 0.38);
                self.polygon((0.50, 0.50), 0.38, 6, 0.0);
                self.line((0.50, 0.50), (0.83, 0.50));
            }
            ToolIcon::TwoPointSlot => {
                self.line((0.28, 0.25), (0.72, 0.25));
                self.line((0.28, 0.75), (0.72, 0.75));
                self.arc((0.28, 0.50), 0.25, PI * 0.5, PI);
                self.arc((0.72, 0.50), 0.25, -PI * 0.5, PI);
                self.dot((0.28, 0.50), 0.045);
                self.dot((0.72, 0.50), 0.045);
            }
            ToolIcon::CentreSlot => {
                self.line((0.28, 0.25), (0.72, 0.25));
                self.line((0.28, 0.75), (0.72, 0.75));
                self.arc((0.28, 0.50), 0.25, PI * 0.5, PI);
                self.arc((0.72, 0.50), 0.25, -PI * 0.5, PI);
                self.line((0.42, 0.50), (0.58, 0.50));
                self.line((0.50, 0.42), (0.50, 0.58));
                self.dot((0.97, 0.50), 0.04);
            }
            ToolIcon::Trim => {
                self.line((0.10, 0.28), (0.90, 0.72));
                self.line((0.10, 0.72), (0.38, 0.57));
                self.line((0.62, 0.43), (0.90, 0.28));
                self.circle((0.42, 0.73), 0.10);
                self.circle((0.58, 0.73), 0.10);
            }
            ToolIcon::Fillet => {
                self.line((0.18, 0.82), (0.18, 0.48));
                self.line((0.52, 0.18), (0.86, 0.18));
                self.arc((0.52, 0.48), 0.34, PI, PI * 0.5);
                self.dot((0.18, 0.48), 0.045);
                self.dot((0.52, 0.18), 0.045);
            }
            ToolIcon::Chamfer => {
                self.line((0.18, 0.82), (0.18, 0.49));
                self.line((0.49, 0.18), (0.86, 0.18));
                self.line((0.18, 0.49), (0.49, 0.18));
                self.dot((0.18, 0.49), 0.045);
                self.dot((0.49, 0.18), 0.045);
            }
            ToolIcon::RectangularPattern => {
                for row in 0..2 {
                    for column in 0..3 {
                        let x = 0.18 + column as f32 * 0.28;
                        let y = 0.26 + row as f32 * 0.40;
                        self.painter.rect_stroke(
                            Rect::from_center_size(
                                self.p(x, y),
                                vec2(self.rect.width() * 0.14, self.rect.height() * 0.14),
                            ),
                            1.0,
                            self.stroke(),
                            StrokeKind::Inside,
                        );
                    }
                }
            }
            ToolIcon::CircularPattern => {
                self.dot((0.50, 0.50), 0.045);
                for index in 0..6 {
                    let angle = TAU * index as f32 / 6.0;
                    self.circle(
                        (0.50 + 0.31 * angle.cos(), 0.50 + 0.31 * angle.sin()),
                        0.065,
                    );
                }
                self.arc((0.50, 0.50), 0.31, 0.25, PI * 1.45);
            }
            ToolIcon::Dimension => {
                self.line((0.18, 0.70), (0.82, 0.70));
                self.line((0.18, 0.58), (0.18, 0.82));
                self.line((0.82, 0.58), (0.82, 0.82));
                self.line((0.18, 0.70), (0.30, 0.62));
                self.line((0.18, 0.70), (0.30, 0.78));
                self.line((0.82, 0.70), (0.70, 0.62));
                self.line((0.82, 0.70), (0.70, 0.78));
                self.line((0.36, 0.28), (0.36, 0.50));
                self.arc((0.50, 0.39), 0.14, -PI * 0.5, PI);
                self.line((0.64, 0.28), (0.64, 0.50));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    #[test]
    fn registry_is_total_unique_and_family_owned() {
        assert_eq!(SketchToolRegistry::families().len(), ToolFamily::COUNT);
        assert_eq!(SketchToolRegistry::tools().len(), ToolVariant::COUNT);
        let mut keys = BTreeSet::new();
        let mut names = BTreeSet::new();
        let mut ownership = BTreeMap::<ToolVariant, usize>::new();

        for descriptor in SketchToolRegistry::tools() {
            assert_eq!(descriptor.variant.descriptor(), descriptor);
            assert!(!descriptor.stable_key.trim().is_empty());
            assert!(!descriptor.accessible_name.trim().is_empty());
            assert!(!descriptor.short_tooltip.trim().is_empty());
            assert!(!descriptor.extended_tooltip.trim().is_empty());
            assert!(!descriptor.prompt.trim().is_empty());
            assert!(!descriptor.disabled_reason.trim().is_empty());
            assert!(keys.insert(descriptor.stable_key));
            assert!(names.insert(descriptor.accessible_name));
            for phase in descriptor.acquisition_phases {
                assert!(!phase.stable_key.trim().is_empty());
                assert!(!phase.prompt.trim().is_empty());
            }
            for field in descriptor.inputs {
                assert!(!field.stable_key.trim().is_empty());
                assert!(!field.label.trim().is_empty());
                assert!(!field.domain.trim().is_empty());
            }
        }

        for family in ToolFamily::ALL {
            let descriptor = family.descriptor();
            assert_eq!(descriptor.family, family);
            assert!(!descriptor.stable_key.trim().is_empty());
            assert!(!descriptor.accessible_name.trim().is_empty());
            assert!(!descriptor.variants.is_empty());
            assert!(descriptor.variants.contains(&descriptor.default_variant));
            for variant in descriptor.variants {
                assert_eq!(variant.family(), family);
                *ownership.entry(*variant).or_default() += 1;
            }
        }
        assert_eq!(ownership.len(), ToolVariant::COUNT);
        assert!(ownership.values().all(|count| *count == 1));
    }

    #[test]
    fn family_shortcuts_are_unique_and_follow_last_used_preferences() {
        let mut shortcut_keys = BTreeSet::new();
        for family in SketchToolRegistry::families() {
            if let Some(shortcut) = family.shortcut {
                assert!(shortcut_keys.insert(shortcut.key));
                assert!(!shortcut.label.is_empty());
                assert!(
                    family
                        .variants
                        .iter()
                        .all(|variant| variant.descriptor().shortcut == Some(shortcut))
                );
            }
        }

        let mut preferences = SketchToolPreferences::default();
        assert_eq!(
            preferences.variant_for_shortcut(Key::L),
            Some(ToolVariant::SingleLine)
        );
        preferences.remember(ToolVariant::Centreline);
        assert_eq!(
            preferences.variant_for_shortcut(Key::L),
            Some(ToolVariant::Centreline)
        );
        assert_eq!(preferences.variant_for_shortcut(Key::F), None);
    }

    #[test]
    fn every_model_changing_tool_uses_the_universal_confirmation_contract() {
        for descriptor in SketchToolRegistry::tools() {
            let expected = if matches!(
                descriptor.variant,
                ToolVariant::Select | ToolVariant::Dimension
            ) {
                CommitContract::SessionOnly
            } else {
                CommitContract::StageThenUniversalTickOrEnter
            };
            assert_eq!(
                descriptor.commit_contract, expected,
                "{}",
                descriptor.stable_key
            );
        }
    }

    #[test]
    fn capability_reasons_never_become_empty_and_pending_gate_wins() {
        let mut capabilities = SketchToolCapabilities::default();
        capabilities.disable_with_reason(ToolVariant::Fillet, "   ");
        assert_eq!(
            capabilities.disabled_reason(ToolVariant::Fillet),
            Some(ToolVariant::Fillet.descriptor().disabled_reason)
        );
        capabilities.enable(ToolVariant::Fillet);
        assert!(capabilities.is_enabled(ToolVariant::Fillet));
        assert_eq!(
            SketchOperationGate::AwaitingConfirmation.disabled_reason(),
            Some(PENDING_CONFIRMATION_DISABLED_REASON)
        );
    }

    #[test]
    fn fixed_layout_is_two_rows_and_fits_the_supported_ribbon() {
        let row_width = |row: &[ToolFamily]| {
            PRIMARY_CELL_SIZE * row.len() as f32 + FAMILY_GAP * row.len().saturating_sub(1) as f32
        };
        assert_eq!(FIRST_ROW.len(), 6);
        assert_eq!(SECOND_ROW.len(), 7);
        assert!(row_width(FIRST_ROW) <= SKETCH_TOOLBAR_WIDTH);
        assert_eq!(row_width(SECOND_ROW), SKETCH_TOOLBAR_WIDTH);
        assert!((28.0..=34.0).contains(&PRIMARY_CELL_SIZE));
        assert_eq!(SKETCH_TOOLBAR_HEIGHT, 74.0);
        assert_eq!(CHEVRON_CELL_WIDTH, 12.0);

        let mut all = FIRST_ROW
            .iter()
            .chain(SECOND_ROW)
            .copied()
            .collect::<Vec<_>>();
        all.sort_unstable();
        assert_eq!(all, ToolFamily::ALL);
    }
}
