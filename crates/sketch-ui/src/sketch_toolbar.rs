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
    Align, Align2, Button, Color32, Direction, FontId, Key, Layout, Modifiers, Painter, Pos2, Rect,
    Response, Sense, Stroke, StrokeKind, TextStyle, Ui, UiBuilder, WidgetInfo, WidgetType, pos2,
    vec2,
};

/// Height of every tile.
///
/// Three rows of this plus the gaps come to exactly the ribbon's content
/// height, which is what puts the `SKETCH` caption on the ribbon's bottom edge.
/// A shorter tile leaves the caption floating in dead space with the icons
/// bunched above it.
///
/// Three shorter rows rather than two taller ones is what buys the width: a
/// named tile each for twelve drawing families and eleven constraints is
/// twenty-three tiles, and twelve columns of them do not fit the 1040 px
/// minimum window at any legible size. Eight columns do, with room to spare.
/// 24 px is also the floor a tile may not go under and stay a hit target, and
/// the height the View group's stacked commands already use.
pub const PRIMARY_CELL_SIZE: f32 = 24.0;
/// Width of every tile: icon column, label, and — where a family has variants —
/// the chooser.
///
/// One width for drawing tools, generators and constraints alike, because they
/// are the same kind of control and a block of ragged tiles reads as a mistake.
/// Sized so the longest label on either side clears its columns: `Rectangle` at
/// 40 px plus the chooser, and `Perpendicular` at 56 px without one, both
/// measured at [`TILE_LABEL_TEXT_SIZE`]. Nine columns of 84 px did not fit the
/// minimum window once the generators took a block of their own.
pub const PRIMARY_CELL_WIDTH: f32 = 80.0;
/// Left column of a tile, holding the icon.
pub const TILE_ICON_COLUMN: f32 = 20.0;
/// Side length of the icon painted inside that column.
pub const TILE_ICON_SIZE: f32 = 18.0;
/// Gap between the icon column and the start of the label.
pub const TILE_LABEL_GAP: f32 = 2.0;
/// Size of the text drawn on a tile.
///
/// Two points under `TextStyle::Small`, which is 11 px here. The tile label is a
/// caption on a control the icon has already named, not body text, and what the
/// smaller text gives back per tile is what lets every constraint carry its own
/// name at the minimum window width.
pub const TILE_LABEL_TEXT_SIZE: f32 = 9.0;
/// Side length of the separately focusable chooser contained by a family tile.
///
/// A full-height column at the tile's right edge. It used to be a 12 px square
/// tucked into the bottom-right corner *over* the icon, which is what made the
/// chooser and the glyph collide.
pub const CHEVRON_CELL_WIDTH: f32 = 13.0;
/// Room a tile leaves its label once the icon column and the chooser are out.
///
/// The narrower of the two bounds, because a family with variants spends the
/// chooser column and a constraint does not.
pub const TILE_LABEL_ROOM: f32 =
    PRIMARY_CELL_WIDTH - TILE_ICON_COLUMN - TILE_LABEL_GAP - CHEVRON_CELL_WIDTH;
/// Room a tile without a chooser leaves its label — every constraint tile.
pub const CONSTRAINT_LABEL_ROOM: f32 = PRIMARY_CELL_WIDTH - TILE_ICON_COLUMN - TILE_LABEL_GAP;
// The label has to clear the columns beside it: `Rectangle` is the longest of
// the drawing labels at 40 px and `Perpendicular` the longest constraint at
// 56 px, both at `TILE_LABEL_TEXT_SIZE`. A geometry that breaks either does
// not compile, and a test measures the text itself against these bounds.
const _: () = {
    assert!(TILE_LABEL_ROOM >= 42.0);
    assert!(CONSTRAINT_LABEL_ROOM >= 58.0);
};
/// Inset that keeps the chooser visibly inside its family tile.
pub const CHEVRON_CELL_INSET: f32 = 1.0;
/// Horizontal space between tool families: the same gap that separates the
/// rows, so the block reads as one grid of tiles rather than a row of pairs.
/// At 4 px the seven-family strip was 12 px wider than the 1040 px minimum
/// window could give it.
pub const FAMILY_GAP: f32 = 2.0;
/// Vertical space between the three persistent toolbar rows.
pub const ROW_GAP: f32 = 2.0;
/// Padding above the first row of tiles.
///
/// None: three rows of tiles fill the ribbon's content box exactly, the way
/// the View group's three stacked commands do. The padding existed to make up
/// what two rows left over.
pub const TOOLBAR_TOP_PADDING: f32 = 0.0;
/// Padding below the last row of tiles, for the same reason.
pub const TOOLBAR_BOTTOM_PADDING: f32 = 0.0;
/// Width of a strip that parts one block of tiles from the next: a hairline
/// with breathing room either side. It replaces one family gap, and says in the
/// layout what the grouping means — what puts geometry on the canvas, what
/// copies geometry that is already there, and what tells the solver how all of
/// it has to behave.
pub const BLOCK_DIVIDER_WIDTH: f32 = 8.0;
/// The divider before the constraints, under the name it has always had.
pub const CONSTRAINT_DIVIDER_WIDTH: f32 = BLOCK_DIVIDER_WIDTH;
/// Rows in every block. The grid is as tall as the ribbon allows and as
/// narrow as that makes it.
pub const TOOLBAR_ROWS: usize = 3;
/// Drawing-tool columns: twelve families over three rows.
pub const DRAWING_COLUMNS: usize = 4;
/// Generator columns: one, holding what repeats geometry that already exists.
pub const GENERATOR_COLUMNS: usize = 1;
/// Constraint columns: eleven constraints over the same three rows.
pub const CONSTRAINT_COLUMNS: usize = 4;
/// Width of one block of tiles, in columns of [`PRIMARY_CELL_WIDTH`].
const fn block_width(columns: usize) -> f32 {
    PRIMARY_CELL_WIDTH * columns as f32 + FAMILY_GAP * (columns as f32 - 1.0)
}
/// Width of the drawing block, this side of the first divider.
pub const DRAWING_BLOCK_WIDTH: f32 = block_width(DRAWING_COLUMNS);
/// Width of the generator block, between the two dividers.
pub const GENERATOR_BLOCK_WIDTH: f32 = block_width(GENERATOR_COLUMNS);
/// Width of the constraint block, beyond the second.
pub const CONSTRAINT_BLOCK_WIDTH: f32 = block_width(CONSTRAINT_COLUMNS);
/// Left edge of the generator block, measured from the toolbar's own.
pub const GENERATOR_BLOCK_ORIGIN: f32 = DRAWING_BLOCK_WIDTH + BLOCK_DIVIDER_WIDTH;
/// Left edge of the constraint block, measured from the same origin.
pub const CONSTRAINT_BLOCK_ORIGIN: f32 =
    GENERATOR_BLOCK_ORIGIN + GENERATOR_BLOCK_WIDTH + BLOCK_DIVIDER_WIDTH;
/// Width of the toolbar: three blocks and the two dividers between them.
pub const SKETCH_TOOLBAR_WIDTH: f32 = CONSTRAINT_BLOCK_ORIGIN + CONSTRAINT_BLOCK_WIDTH;
/// Height required by the padded three-row tile grid.
pub const SKETCH_TOOLBAR_HEIGHT: f32 = PRIMARY_CELL_SIZE * TOOLBAR_ROWS as f32
    + ROW_GAP * (TOOLBAR_ROWS as f32 - 1.0)
    + TOOLBAR_TOP_PADDING
    + TOOLBAR_BOTTOM_PADDING;

const _: () = {
    // The chooser is a column beside the label now, not an overlay on the icon,
    // so it has to leave the icon column and a readable label behind it.
    assert!(TILE_ICON_COLUMN + TILE_LABEL_GAP + CHEVRON_CELL_WIDTH < PRIMARY_CELL_WIDTH);
    assert!(TILE_ICON_SIZE < TILE_ICON_COLUMN);
    assert!(TILE_ICON_SIZE < PRIMARY_CELL_SIZE);
    // A tile under 24 px stops being a hit target anyone can rely on, and a
    // test at the minimum window holds every tile to it.
    assert!(PRIMARY_CELL_SIZE >= 24.0);
    // The grid must fill the ribbon's content height exactly: short leaves the
    // caption floating, tall clips the bottom row.
    assert!(SKETCH_TOOLBAR_HEIGHT == 76.0);
    // The 1040 px minimum window has this much room for the grid, and no more.
    // The extra over the two-block layout is what Extrude gave back by living
    // on the Model tab alone, which a sketch can now reach without leaving.
    assert!(SKETCH_TOOLBAR_WIDTH <= 764.0);
    // Every tool has a tile of its own; none may fall off its block.
    assert!(
        ToolFamily::COUNT - CONSTRAINT_FAMILIES.len() - GENERATOR_FAMILIES.len()
            <= DRAWING_COLUMNS * TOOLBAR_ROWS
    );
    assert!(GENERATOR_FAMILIES.len() <= GENERATOR_COLUMNS * TOOLBAR_ROWS);
    assert!(CONSTRAINT_TOOLS.len() <= CONSTRAINT_COLUMNS * TOOLBAR_ROWS);
};

/// Left column of a variant-menu row, holding the icon.
pub const MENU_ICON_COLUMN: f32 = 30.0;
/// Side length of the icon painted inside that column.
pub const MENU_ICON_SIZE: f32 = 18.0;
/// Gap between the menu icon column and the start of the row label.
pub const MENU_LABEL_GAP: f32 = 4.0;

const _: () = {
    assert!(MENU_ICON_SIZE < MENU_ICON_COLUMN);
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
    Offset,
    Pattern,
    Relation,
    Dimension,
}

impl ToolFamily {
    pub const COUNT: usize = 15;
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
        Self::Offset,
        Self::Pattern,
        Self::Relation,
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
    FitPointSpline,
    ControlVertexSpline,
    TwoPointRectangle,
    CentrePointRectangle,
    CentrePointCircle,
    TwoPointCircle,
    CentreStartEndArc,
    ThreePointArc,
    InnerDiameterPolygon,
    OuterDiameterPolygon,
    Text,
    TwoPointSlot,
    CentreToOuterPointSlot,
    Trim,
    Fillet,
    Chamfer,
    TwoDistanceChamfer,
    Offset,
    RectangularPattern,
    CircularPattern,
    FixedRelation,
    CoincidentRelation,
    HorizontalRelation,
    VerticalRelation,
    DistanceRelation,
    ParallelRelation,
    PerpendicularRelation,
    EqualLengthRelation,
    TangentRelation,
    CollinearRelation,
    Dimension,
}

impl ToolVariant {
    pub const COUNT: usize = 36;
    pub const ALL: [Self; Self::COUNT] = [
        Self::Select,
        Self::Point,
        Self::SingleLine,
        Self::ChainedPolyline,
        Self::Centreline,
        Self::FitPointSpline,
        Self::ControlVertexSpline,
        Self::TwoPointRectangle,
        Self::CentrePointRectangle,
        Self::CentrePointCircle,
        Self::TwoPointCircle,
        Self::CentreStartEndArc,
        Self::ThreePointArc,
        Self::InnerDiameterPolygon,
        Self::OuterDiameterPolygon,
        Self::Text,
        Self::TwoPointSlot,
        Self::CentreToOuterPointSlot,
        Self::Trim,
        Self::Fillet,
        Self::Chamfer,
        Self::TwoDistanceChamfer,
        Self::Offset,
        Self::RectangularPattern,
        Self::CircularPattern,
        Self::FixedRelation,
        Self::CoincidentRelation,
        Self::HorizontalRelation,
        Self::VerticalRelation,
        Self::DistanceRelation,
        Self::ParallelRelation,
        Self::PerpendicularRelation,
        Self::EqualLengthRelation,
        Self::TangentRelation,
        Self::CollinearRelation,
        Self::Dimension,
    ];

    #[must_use]
    pub fn descriptor(self) -> &'static ToolDescriptor {
        &TOOL_DESCRIPTORS[self as usize]
    }

    /// The label on the tile while this variant is its current choice.
    ///
    /// Families of one kind of shape keep their own name; a variant that is a
    /// different thing altogether, like text under the closed-shapes tile,
    /// names itself so the tile never lies about what a click will draw. Every
    /// constraint names itself too — each has a tile rather than a share of
    /// one, and "Relation" on ten of them would say nothing.
    #[must_use]
    pub fn tile_label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::FixedRelation => "Fixed",
            Self::CoincidentRelation => "Coincident",
            Self::HorizontalRelation => "Horizontal",
            Self::VerticalRelation => "Vertical",
            Self::DistanceRelation => "Distance",
            Self::ParallelRelation => "Parallel",
            Self::PerpendicularRelation => "Perpendicular",
            // The relation holds two lines at the same length; the tile has
            // room for the half of that which distinguishes it.
            Self::EqualLengthRelation => "Equal",
            Self::TangentRelation => "Tangent",
            Self::CollinearRelation => "Collinear",
            _ => self.family().descriptor().tile_label,
        }
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
    /// Free text, such as the characters a text tool sets.
    Text,
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
    /// Every curve reachable from the one under the pointer by shared
    /// endpoints — the loop, where the chain closes.
    ConnectedCurveChain,
    /// The operands a relation names: endpoints, whole curves, or a mix,
    /// picked in the canvas while the relation tool is active.
    RelationOperands,
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
    OffsetChain,
    PatternSeedSelection,
    RelationOperands,
}

/// Model-changing commands must stage, then use the permanent tick/Enter gate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommitContract {
    SessionOnly,
    StageThenUniversalTickOrEnter,
    /// The tool stages a private candidate that previews live and publishes it
    /// the moment the value is accepted — bare `Enter`, or the field losing the
    /// keyboard — with `Escape` reverting. It never shows the shared rail.
    /// [ADR 0027](../../../docs/architecture/adr/0027-sketch-edits-commit-on-acceptance.md).
    CommitsOnAcceptance,
}

/// Vector icon authored in normalized coordinates and painted by this module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolIcon {
    Select,
    Point,
    SingleLine,
    Polyline,
    Centreline,
    FitPointSpline,
    ControlVertexSpline,
    CornerRectangle,
    CentreRectangle,
    CentreCircle,
    DiameterCircle,
    CentreArc,
    ThreePointArc,
    InnerPolygon,
    OuterPolygon,
    Text,
    TwoPointSlot,
    CentreSlot,
    Trim,
    Fillet,
    Chamfer,
    Offset,
    RectangularPattern,
    CircularPattern,
    FixedRelation,
    CoincidentRelation,
    HorizontalRelation,
    VerticalRelation,
    DistanceRelation,
    ParallelRelation,
    PerpendicularRelation,
    EqualLengthRelation,
    TangentRelation,
    CollinearRelation,
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
    /// The name drawn on the tile, beside the icon.
    ///
    /// Separate from `accessible_name` for the same reason the model ribbon
    /// separates them: the tile is width-constrained and the accessible name is
    /// not, so shortening what is drawn must never rename the control a user has
    /// learned or a test drives. Where the two agree the label simply repeats it.
    pub tile_label: &'static str,
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
const SHORTCUT_G: ToolShortcut = ToolShortcut {
    key: Key::G,
    label: "G",
};
/// The key Fusion binds Offset to, free here: the model workspace's O is
/// Orbit, and single-letter shortcuts are read per workspace.
const SHORTCUT_O: ToolShortcut = ToolShortcut {
    key: Key::O,
    label: "O",
};

const SELECT_VARIANTS: &[ToolVariant] = &[ToolVariant::Select];
const POINT_VARIANTS: &[ToolVariant] = &[ToolVariant::Point];
const LINE_VARIANTS: &[ToolVariant] = &[
    ToolVariant::SingleLine,
    ToolVariant::ChainedPolyline,
    ToolVariant::Centreline,
    ToolVariant::FitPointSpline,
    ToolVariant::ControlVertexSpline,
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
    ToolVariant::Text,
];
const SLOT_VARIANTS: &[ToolVariant] = &[
    ToolVariant::TwoPointSlot,
    ToolVariant::CentreToOuterPointSlot,
];
const TRIM_VARIANTS: &[ToolVariant] = &[ToolVariant::Trim];
const FILLET_VARIANTS: &[ToolVariant] = &[ToolVariant::Fillet];
const CHAMFER_VARIANTS: &[ToolVariant] = &[ToolVariant::Chamfer, ToolVariant::TwoDistanceChamfer];
const OFFSET_VARIANTS: &[ToolVariant] = &[ToolVariant::Offset];
const PATTERN_VARIANTS: &[ToolVariant] = &[
    ToolVariant::RectangularPattern,
    ToolVariant::CircularPattern,
];
const RELATION_VARIANTS: &[ToolVariant] = &[
    ToolVariant::HorizontalRelation,
    ToolVariant::VerticalRelation,
    ToolVariant::CoincidentRelation,
    ToolVariant::ParallelRelation,
    ToolVariant::PerpendicularRelation,
    ToolVariant::EqualLengthRelation,
    ToolVariant::DistanceRelation,
    ToolVariant::FixedRelation,
    ToolVariant::TangentRelation,
    ToolVariant::CollinearRelation,
];
const DIMENSION_VARIANTS: &[ToolVariant] = &[ToolVariant::Dimension];

const TOOL_FAMILIES: [ToolFamilyDescriptor; ToolFamily::COUNT] = [
    family(
        ToolFamily::Select,
        "select",
        "Select",
        "Select",
        Some(SHORTCUT_V),
        SELECT_VARIANTS,
        ToolVariant::Select,
    ),
    family(
        ToolFamily::Point,
        "point",
        "Point",
        "Point",
        Some(SHORTCUT_P),
        POINT_VARIANTS,
        ToolVariant::Point,
    ),
    family(
        ToolFamily::Line,
        "line",
        "Line",
        "Line",
        Some(SHORTCUT_L),
        LINE_VARIANTS,
        ToolVariant::SingleLine,
    ),
    family(
        ToolFamily::Rectangle,
        "rectangle",
        "Rectangle",
        "Rectangle",
        Some(SHORTCUT_R),
        RECTANGLE_VARIANTS,
        ToolVariant::TwoPointRectangle,
    ),
    family(
        ToolFamily::Circle,
        "circle",
        "Circle",
        "Circle",
        Some(SHORTCUT_C),
        CIRCLE_VARIANTS,
        ToolVariant::CentrePointCircle,
    ),
    family(
        ToolFamily::Arc,
        "arc",
        "Arc",
        "Arc",
        Some(SHORTCUT_A),
        ARC_VARIANTS,
        ToolVariant::CentreStartEndArc,
    ),
    family(
        ToolFamily::Polygon,
        "polygon",
        "Polygon",
        "Polygon",
        None,
        POLYGON_VARIANTS,
        ToolVariant::OuterDiameterPolygon,
    ),
    family(
        ToolFamily::Slot,
        "slot",
        "Slot",
        "Slot",
        None,
        SLOT_VARIANTS,
        ToolVariant::TwoPointSlot,
    ),
    family(
        ToolFamily::Trim,
        "trim",
        "Trim",
        "Trim",
        Some(SHORTCUT_T),
        TRIM_VARIANTS,
        ToolVariant::Trim,
    ),
    family(
        ToolFamily::Fillet,
        "fillet",
        "Fillet",
        "Fillet",
        None,
        FILLET_VARIANTS,
        ToolVariant::Fillet,
    ),
    family(
        ToolFamily::Chamfer,
        "chamfer",
        "Chamfer",
        "Chamfer",
        None,
        CHAMFER_VARIANTS,
        ToolVariant::Chamfer,
    ),
    family(
        ToolFamily::Offset,
        "offset",
        "Offset",
        "Offset",
        Some(SHORTCUT_O),
        OFFSET_VARIANTS,
        ToolVariant::Offset,
    ),
    family(
        ToolFamily::Pattern,
        "pattern",
        "Pattern",
        "Pattern",
        None,
        PATTERN_VARIANTS,
        ToolVariant::RectangularPattern,
    ),
    family(
        ToolFamily::Relation,
        "relation",
        "Sketch relation",
        "Relation",
        Some(SHORTCUT_G),
        RELATION_VARIANTS,
        ToolVariant::HorizontalRelation,
    ),
    family(
        ToolFamily::Dimension,
        "dimension",
        "Sketch dimension",
        "Dimension",
        Some(SHORTCUT_D),
        DIMENSION_VARIANTS,
        ToolVariant::Dimension,
    ),
];

const fn family(
    family: ToolFamily,
    stable_key: &'static str,
    accessible_name: &'static str,
    tile_label: &'static str,
    shortcut: Option<ToolShortcut>,
    variants: &'static [ToolVariant],
    default_variant: ToolVariant,
) -> ToolFamilyDescriptor {
    ToolFamilyDescriptor {
        family,
        stable_key,
        accessible_name,
        tile_label,
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
const SPLINE_PHASES: &[PointAcquisitionPhase] = &[
    phase("start", "Click the first spline point."),
    phase(
        "next",
        "Click consecutive points; Enter, double-click, or Finish stages the spline.",
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
const TEXT_PHASES: &[PointAcquisitionPhase] = &[phase(
    "anchor",
    "Click where the text baseline starts; the palette sets the text, height, and angle.",
)];
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
const OFFSET_PHASES: &[PointAcquisitionPhase] = &[
    phase(
        "chain",
        "Hover a curve to highlight every curve connected to it, then click to take the chain.",
    ),
    phase(
        "distance",
        "Move to either side of the chain to set which way it offsets, then click; Tab types the distance.",
    ),
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

/// One relation operand: an endpoint if the pointer is on one, otherwise the
/// curve under it. The same phase text serves every arity because the operand
/// rule is the same at every step.
const RELATION_ONE_OPERAND_PHASES: &[PointAcquisitionPhase] = &[phase(
    "operand",
    "Click a sketch curve, or one of its endpoints, to relate.",
)];
const RELATION_TWO_OPERAND_PHASES: &[PointAcquisitionPhase] = &[
    phase("first", "Click the first curve or endpoint to relate."),
    phase("second", "Click the second curve or endpoint to relate."),
];

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
const TEXT_INPUTS: &[ToolInputField] = &[
    input(
        "content",
        "Text",
        ToolInputKind::Text,
        "one line of characters the bundled typeface can set",
    ),
    input(
        "height",
        "Height",
        ToolInputKind::Length,
        "positive finite capital-letter height",
    ),
    input(
        "angle",
        "Angle",
        ToolInputKind::Angle,
        "finite baseline direction",
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
const OFFSET_INPUTS: &[ToolInputField] = &[
    input(
        "distance",
        "Offset distance",
        ToolInputKind::SignedLength,
        "non-zero finite distance; the sign chooses the side",
    ),
    input(
        "chain_selection",
        "Chain selection",
        ToolInputKind::Boolean,
        "take the whole connected chain, or only the curve under the pointer",
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
        ToolVariant::FitPointSpline,
        "sketch.spline.fit_points",
        ToolFamily::Line,
        "Fit-point spline",
        "Create an exact B-spline interpolated through clicked points.",
        "Click consecutive fit points. Enter, double-click, or Finish stages the fitted curve; the green tick commits.",
        "Click the first fit point.",
        "Choose line or spline type; current default: Fit-point spline.",
        Some(SHORTCUT_L),
        ToolIcon::FitPointSpline,
        ToolCursor::Crosshair,
        SPLINE_PHASES,
        NO_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Spline creation requires an editable sketch.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::ControlVertexSpline,
        "sketch.spline.control_vertices",
        ToolFamily::Line,
        "Control-vertex spline",
        "Create an exact B-spline defined by its control polygon.",
        "Click consecutive control vertices. Enter, double-click, or Finish stages the curve; the green tick commits.",
        "Click the first control vertex.",
        "Choose line or spline type; current default: Control-vertex spline.",
        Some(SHORTCUT_L),
        ToolIcon::ControlVertexSpline,
        ToolCursor::Crosshair,
        SPLINE_PHASES,
        NO_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Spline creation requires an editable sketch.",
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
        ToolVariant::Text,
        "sketch.text",
        ToolFamily::Polygon,
        "Sketch text",
        "Set a line of text as exact outline loops.",
        "Click where the baseline starts. Every letter becomes closed loops of exact lines that extrude like any profile; Tab edits the text, its capital height, and the baseline angle.",
        "Click where the text baseline starts.",
        "Choose polygon type; current default: Text.",
        None,
        ToolIcon::Text,
        ToolCursor::Crosshair,
        TEXT_PHASES,
        TEXT_INPUTS,
        SelectionRequirement::None,
        ToolOutputRole::ProfileGeometry,
        CapabilityRequirement::EditableSketch,
        "Text creation requires an editable sketch.",
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
        ToolVariant::Offset,
        "sketch.modify.offset",
        ToolFamily::Offset,
        "Offset chain",
        "Copy a connected chain of curves a set distance to one side.",
        "Hover a curve and the whole connected chain highlights; click to take it, then drag to either side or Tab a signed distance. The copy is new sketch geometry that keeps its distance from the chain it came from.",
        "Hover a curve to take its connected chain.",
        "Offset has no variants.",
        Some(SHORTCUT_O),
        ToolIcon::Offset,
        ToolCursor::PrecisionPick,
        OFFSET_PHASES,
        OFFSET_INPUTS,
        SelectionRequirement::ConnectedCurveChain,
        ToolOutputRole::GeneratedGeometry,
        CapabilityRequirement::OffsetChain,
        "Offset requires a connected chain of sketch curves, or a body edge projected into the sketch.",
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
        ToolVariant::FixedRelation,
        "sketch.relation.fixed",
        ToolFamily::Relation,
        "Fixed relation",
        "Pin geometry where it is.",
        "Click a line or an endpoint. Its present position becomes the held value, and the solver moves everything else around it.",
        "Click a line or endpoint to pin.",
        "Choose relation; current default: Horizontal.",
        Some(SHORTCUT_G),
        ToolIcon::FixedRelation,
        ToolCursor::PrecisionPick,
        RELATION_ONE_OPERAND_PHASES,
        NO_INPUTS,
        SelectionRequirement::RelationOperands,
        ToolOutputRole::Modification,
        CapabilityRequirement::RelationOperands,
        "A fixed relation needs one line or endpoint.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::CoincidentRelation,
        "sketch.relation.coincident",
        ToolFamily::Relation,
        "Coincident relation",
        "Hold two points together.",
        "Click two endpoints. The solver brings them together and keeps them together, which is what closes a profile and keeps it closed.",
        "Click the first endpoint to make coincident.",
        "Choose relation; current default: Horizontal.",
        Some(SHORTCUT_G),
        ToolIcon::CoincidentRelation,
        ToolCursor::PrecisionPick,
        RELATION_TWO_OPERAND_PHASES,
        NO_INPUTS,
        SelectionRequirement::RelationOperands,
        ToolOutputRole::Modification,
        CapabilityRequirement::RelationOperands,
        "A coincident relation needs two endpoints.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::HorizontalRelation,
        "sketch.relation.horizontal",
        ToolFamily::Relation,
        "Horizontal relation",
        "Hold two points at the same height.",
        "Click a line, or two endpoints. The solver levels them and keeps them level; a system it cannot satisfy is refused whole.",
        "Click a line or two endpoints to hold level.",
        "Choose relation; current default: Horizontal.",
        Some(SHORTCUT_G),
        ToolIcon::HorizontalRelation,
        ToolCursor::PrecisionPick,
        RELATION_ONE_OPERAND_PHASES,
        NO_INPUTS,
        SelectionRequirement::RelationOperands,
        ToolOutputRole::Modification,
        CapabilityRequirement::RelationOperands,
        "A horizontal relation needs one line or two endpoints.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::VerticalRelation,
        "sketch.relation.vertical",
        ToolFamily::Relation,
        "Vertical relation",
        "Hold two points at the same horizontal position.",
        "Click a line, or two endpoints. The solver plumbs them and keeps them plumb; a system it cannot satisfy is refused whole.",
        "Click a line or two endpoints to hold plumb.",
        "Choose relation; current default: Horizontal.",
        Some(SHORTCUT_G),
        ToolIcon::VerticalRelation,
        ToolCursor::PrecisionPick,
        RELATION_ONE_OPERAND_PHASES,
        NO_INPUTS,
        SelectionRequirement::RelationOperands,
        ToolOutputRole::Modification,
        CapabilityRequirement::RelationOperands,
        "A vertical relation needs one line or two endpoints.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::DistanceRelation,
        "sketch.relation.distance",
        ToolFamily::Relation,
        "Distance relation",
        "Lock the current separation of two points.",
        "Click two endpoints. Their present separation becomes the held value; edit it afterwards with the dimension tool.",
        "Click the first endpoint to lock a separation.",
        "Choose relation; current default: Horizontal.",
        Some(SHORTCUT_G),
        ToolIcon::DistanceRelation,
        ToolCursor::PrecisionPick,
        RELATION_TWO_OPERAND_PHASES,
        NO_INPUTS,
        SelectionRequirement::RelationOperands,
        ToolOutputRole::Modification,
        CapabilityRequirement::RelationOperands,
        "A distance relation needs two endpoints a positive distance apart.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::ParallelRelation,
        "sketch.relation.parallel",
        ToolFamily::Relation,
        "Parallel relation",
        "Hold two lines parallel.",
        "Click two lines. The solver turns the second onto the first's direction and holds it there.",
        "Click the first line to hold parallel.",
        "Choose relation; current default: Horizontal.",
        Some(SHORTCUT_G),
        ToolIcon::ParallelRelation,
        ToolCursor::PrecisionPick,
        RELATION_TWO_OPERAND_PHASES,
        NO_INPUTS,
        SelectionRequirement::RelationOperands,
        ToolOutputRole::Modification,
        CapabilityRequirement::RelationOperands,
        "A parallel relation needs two lines.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::PerpendicularRelation,
        "sketch.relation.perpendicular",
        ToolFamily::Relation,
        "Perpendicular relation",
        "Hold two lines square.",
        "Click two lines. The solver squares them and holds them square.",
        "Click the first line to hold square.",
        "Choose relation; current default: Horizontal.",
        Some(SHORTCUT_G),
        ToolIcon::PerpendicularRelation,
        ToolCursor::PrecisionPick,
        RELATION_TWO_OPERAND_PHASES,
        NO_INPUTS,
        SelectionRequirement::RelationOperands,
        ToolOutputRole::Modification,
        CapabilityRequirement::RelationOperands,
        "A perpendicular relation needs two lines.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::EqualLengthRelation,
        "sketch.relation.equal_length",
        ToolFamily::Relation,
        "Equal-length relation",
        "Hold two lines the same length.",
        "Click two lines. The solver matches their lengths and keeps them matched as either one is edited.",
        "Click the first line to hold equal.",
        "Choose relation; current default: Horizontal.",
        Some(SHORTCUT_G),
        ToolIcon::EqualLengthRelation,
        ToolCursor::PrecisionPick,
        RELATION_TWO_OPERAND_PHASES,
        NO_INPUTS,
        SelectionRequirement::RelationOperands,
        ToolOutputRole::Modification,
        CapabilityRequirement::RelationOperands,
        "An equal-length relation needs two lines.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::TangentRelation,
        "sketch.relation.tangent",
        ToolFamily::Relation,
        "Tangent relation",
        "Hold two curves tangent at their junction.",
        "Click two connected curves. The solver aligns their tangent vectors and holds them tangent.",
        "Click the first curve to hold tangent.",
        "Choose relation; current default: Horizontal.",
        Some(SHORTCUT_G),
        ToolIcon::TangentRelation,
        ToolCursor::PrecisionPick,
        RELATION_TWO_OPERAND_PHASES,
        NO_INPUTS,
        SelectionRequirement::RelationOperands,
        ToolOutputRole::Modification,
        CapabilityRequirement::RelationOperands,
        "A tangent relation needs two curves.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::CollinearRelation,
        "sketch.relation.collinear",
        ToolFamily::Relation,
        "Collinear relation",
        "Hold points or lines along the same infinite line.",
        "Click lines or points. The solver brings them onto one common line and holds them collinear.",
        "Click the first entity to hold collinear.",
        "Choose relation; current default: Horizontal.",
        Some(SHORTCUT_G),
        ToolIcon::CollinearRelation,
        ToolCursor::PrecisionPick,
        RELATION_TWO_OPERAND_PHASES,
        NO_INPUTS,
        SelectionRequirement::RelationOperands,
        ToolOutputRole::Modification,
        CapabilityRequirement::RelationOperands,
        "A collinear relation needs two lines or three points.",
        CommitContract::StageThenUniversalTickOrEnter,
    ),
    descriptor(
        ToolVariant::Dimension,
        "sketch.dimension",
        ToolFamily::Dimension,
        "Sketch dimension",
        "Display and edit a driving dimension on selected sketch geometry.",
        "Click a sketch curve to arm its driving dimensions. Type the exact value in the box on the curve or in the palette; the change previews as you type and applies on Enter or when you click away.",
        "Select sketch geometry to dimension.",
        "Dimension has no variants.",
        Some(SHORTCUT_D),
        ToolIcon::Dimension,
        ToolCursor::PrecisionPick,
        NO_PHASES,
        NO_INPUTS,
        SelectionRequirement::OneOrMoreEditableEntities,
        ToolOutputRole::Modification,
        CapabilityRequirement::EditableSketch,
        "Dimensions require an editable sketch entity.",
        CommitContract::CommitsOnAcceptance,
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
    ///
    /// A constraint family has no tile of its own any more, so its entry is the
    /// union of the cells its variants occupy in the constraint block.
    pub controls: [Option<ToolControlLayout>; ToolFamily::COUNT],
    /// Per-constraint geometry, indexed as in [`CONSTRAINT_TOOLS`].
    pub constraints: [Option<Rect>; CONSTRAINT_TOOLS.len()],
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
            constraints: array::from_fn(|_| None),
            menu_open: false,
            escape_consumed: false,
        }
    }
}

/// The drawing tools: what puts geometry on the canvas and what reshapes it.
///
/// Three rows of [`DRAWING_COLUMNS`], reading in the order the work goes:
/// picking and placing, then the closed shapes, then what reshapes what is
/// already drawn. Offset closes that last row, where Pattern used to: it
/// reshapes an existing chain into another one beside it, which is the same
/// kind of work as trimming, filleting and chamfering, and not the same kind
/// as repeating a shape wholesale.
pub const DRAWING_FAMILIES: &[ToolFamily] = &[
    ToolFamily::Select,
    ToolFamily::Point,
    ToolFamily::Line,
    ToolFamily::Rectangle,
    ToolFamily::Circle,
    ToolFamily::Arc,
    ToolFamily::Polygon,
    ToolFamily::Slot,
    ToolFamily::Trim,
    ToolFamily::Fillet,
    ToolFamily::Chamfer,
    ToolFamily::Offset,
];
/// The generators: what takes geometry that already exists and makes more of
/// it, unchanged, somewhere else.
///
/// Its own divided column, because that is a different promise from every tile
/// to its left. One family today, and room for the two that belong beside it —
/// mirror and circular arrays are the same idea.
pub const GENERATOR_FAMILIES: &[ToolFamily] = &[ToolFamily::Pattern];
/// The families whose variants are drawn as constraint tiles instead.
pub const CONSTRAINT_FAMILIES: &[ToolFamily] = &[ToolFamily::Relation, ToolFamily::Dimension];
/// The constraints: what tells the solver how the geometry has to behave.
///
/// They stand apart from the drawing tools, beyond a divider, and every one of
/// them is its own named tile — the same tile a drawing tool gets, because they
/// are the same kind of control. A relation the user cannot see is a relation
/// they do not know they have: nine of these used to live inside one tile's
/// dropdown, under the name of whichever was picked last, which is why the
/// sketch read as having no constraints at all.
///
/// Three rows of [`CONSTRAINT_COLUMNS`], in the order a drawer reaches for
/// them: the axis relations first, then the pairwise ones, then those that
/// hold a measurement.
pub const CONSTRAINT_TOOLS: &[ToolVariant] = &[
    ToolVariant::HorizontalRelation,
    ToolVariant::VerticalRelation,
    ToolVariant::CoincidentRelation,
    ToolVariant::CollinearRelation,
    ToolVariant::ParallelRelation,
    ToolVariant::PerpendicularRelation,
    ToolVariant::TangentRelation,
    ToolVariant::EqualLengthRelation,
    ToolVariant::DistanceRelation,
    ToolVariant::FixedRelation,
    ToolVariant::Dimension,
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
            // A row of widgets starts out as tall as the interact size of the
            // `Ui` that opens it, and the ribbon group sets that to 26 px. The
            // grid's row is 22, and a row that quietly grows four pixels puts
            // every row under it four pixels low. Set once here: every row and
            // every tile below inherits it.
            ui.spacing_mut().interact_size.y = PRIMARY_CELL_SIZE;
            ui.add_space(TOOLBAR_TOP_PADDING);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                render_family_block(
                    ui,
                    DRAWING_FAMILIES,
                    DRAWING_COLUMNS,
                    &mut state.preferences,
                    active,
                    gate,
                    capabilities,
                    &mut output,
                    &mut escaped_anchor,
                );
                render_block_divider(ui);
                render_family_block(
                    ui,
                    GENERATOR_FAMILIES,
                    GENERATOR_COLUMNS,
                    &mut state.preferences,
                    active,
                    gate,
                    capabilities,
                    &mut output,
                    &mut escaped_anchor,
                );
                render_block_divider(ui);
                render_constraint_block(
                    ui,
                    &mut state.preferences,
                    active,
                    gate,
                    capabilities,
                    &mut output,
                );
            });
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

/// Every constraint as its own named tile, in three rows beyond the divider.
///
/// The same tile a drawing family gets, minus the chooser: a constraint has no
/// variants to choose between, and the point of the block is that the whole set
/// is on screen at once under its own name. Picking one still records it as the
/// family's last-used variant, so the keyboard shortcut re-arms whichever
/// relation the user reached for most recently.
fn render_constraint_block(
    ui: &mut Ui,
    preferences: &mut SketchToolPreferences,
    active: ToolVariant,
    gate: SketchOperationGate,
    capabilities: &SketchToolCapabilities,
    output: &mut SketchToolbarOutput,
) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (row_index, row) in CONSTRAINT_TOOLS.chunks(CONSTRAINT_COLUMNS).enumerate() {
            if row_index > 0 {
                ui.add_space(ROW_GAP);
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = FAMILY_GAP;
                for (column, variant) in row.iter().enumerate() {
                    let index = row_index * CONSTRAINT_COLUMNS + column;
                    let (_, cell_rect) =
                        ui.allocate_space(vec2(PRIMARY_CELL_WIDTH, PRIMARY_CELL_SIZE));
                    let reason = gate
                        .disabled_reason()
                        .or_else(|| capabilities.disabled_reason(*variant));
                    let response = icon_button(
                        ui,
                        cell_rect,
                        variant.descriptor(),
                        variant.tile_label(),
                        active == *variant,
                        reason,
                        TileIdentity::Constraint(*variant),
                    );
                    // The same cell boundary a drawing family draws. Both
                    // blocks are one set of controls, and a tile without the
                    // outline reads as a label rather than a button.
                    paint_family_cell_boundary(
                        ui,
                        cell_rect,
                        active == *variant,
                        response.hovered(),
                    );
                    if response.clicked() {
                        preferences.remember(*variant);
                        output.chosen = Some(*variant);
                    }
                    output.constraints[index] = Some(response.rect);
                    // A constraint family owns no tile, so its layout is the
                    // union of the cells its variants sit in.
                    let family = variant.family() as usize;
                    output.controls[family] = Some(match output.controls[family] {
                        Some(layout) => ToolControlLayout {
                            primary: layout.primary.union(response.rect),
                            chooser: None,
                        },
                        None => ToolControlLayout {
                            primary: response.rect,
                            chooser: None,
                        },
                    });
                    output.bounds = Some(match output.bounds {
                        Some(bounds) => bounds.union(response.rect),
                        None => response.rect,
                    });
                }
            });
        }
    });
}

/// One block of family tiles, laid out in rows of `columns`.
///
/// A block shorter than its grid — the generators are one tile in a column of
/// three — simply leaves the remaining cells unallocated, so the tiles it does
/// have stay on the same rows as every other block's.
#[allow(clippy::too_many_arguments)]
fn render_family_block(
    ui: &mut Ui,
    families: &[ToolFamily],
    columns: usize,
    preferences: &mut SketchToolPreferences,
    active: ToolVariant,
    gate: SketchOperationGate,
    capabilities: &SketchToolCapabilities,
    output: &mut SketchToolbarOutput,
    escaped_anchor: &mut Option<egui::Id>,
) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (index, row) in families.chunks(columns).enumerate() {
            if index > 0 {
                ui.add_space(ROW_GAP);
            }
            render_toolbar_row(
                ui,
                row,
                preferences,
                active,
                gate,
                capabilities,
                output,
                escaped_anchor,
            );
        }
    });
}

/// The hairline between two blocks. It spans every row, so each block reads as
/// one set of tiles set apart from its neighbours rather than as the tail of
/// each row.
fn render_block_divider(ui: &mut Ui) {
    let (_, rect) = ui.allocate_space(vec2(
        BLOCK_DIVIDER_WIDTH,
        PRIMARY_CELL_SIZE * TOOLBAR_ROWS as f32 + ROW_GAP * (TOOLBAR_ROWS as f32 - 1.0),
    ));
    let x = rect.center().x.round() + 0.5;
    ui.painter().line_segment(
        [pos2(x, rect.top() + 3.0), pos2(x, rect.bottom() - 3.0)],
        Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
    );
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
    let (_, cell_rect) = ui.allocate_space(vec2(PRIMARY_CELL_WIDTH, PRIMARY_CELL_SIZE));
    let mut family_ui = ui.new_child(
        UiBuilder::new()
            .id_salt(("sketch_tool_family", family))
            .max_rect(cell_rect),
    );
    let ui = &mut family_ui;
    // The chooser owns a column at the right edge, so the primary stops short of
    // it. Nothing overlaps: the icon, the label and the chevron each have their
    // own horizontal band.
    let primary_rect = if split {
        Rect::from_min_max(
            cell_rect.min,
            pos2(
                cell_rect.max.x - CHEVRON_CELL_WIDTH - CHEVRON_CELL_INSET,
                cell_rect.max.y,
            ),
        )
    } else {
        cell_rect
    };
    let primary_reason = gate
        .disabled_reason()
        .or_else(|| capabilities.disabled_reason(current));
    let primary = icon_button(
        ui,
        primary_rect,
        current.descriptor(),
        current.tile_label(),
        active == current,
        primary_reason,
        TileIdentity::Family(family),
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
        let contained_chooser_rect = Rect::from_min_max(
            pos2(
                cell_rect.max.x - CHEVRON_CELL_WIDTH - CHEVRON_CELL_INSET,
                cell_rect.min.y + CHEVRON_CELL_INSET,
            ),
            cell_rect.max - vec2(CHEVRON_CELL_INSET, CHEVRON_CELL_INSET),
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

/// What a tile is keyed on, which is not the same question on both sides of the
/// divider.
///
/// A drawing tile is one control that shows whichever variant is current, so it
/// keeps the family's identity across a variant change and keeps its focus with
/// it. A constraint tile is one control per variant — ten of them share the
/// relation family, so the family cannot tell them apart.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum TileIdentity {
    Family(ToolFamily),
    Constraint(ToolVariant),
}

fn icon_button(
    ui: &mut Ui,
    rect: Rect,
    descriptor: &'static ToolDescriptor,
    label: &'static str,
    selected: bool,
    disabled_reason: Option<&str>,
    identity: TileIdentity,
) -> Response {
    let enabled = disabled_reason.is_none();
    let button = Button::new(())
        .min_size(rect.size())
        .selected(selected)
        .corner_radius(4.0);
    let mut button_ui = ui.new_child(
        UiBuilder::new()
            .id_salt((identity, "primary"))
            .max_rect(rect)
            .layout(Layout::centered_and_justified(Direction::TopDown)),
    );
    // A button is no shorter than the interact size of the `Ui` it is added
    // to, and the ribbon group sets that to 26 px. The grid has already
    // decided how tall a tile is; without this the button grows past its cell
    // and the rows overlap.
    button_ui.spacing_mut().interact_size.y = rect.height();
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
    // The icon sits centred in its own left column and the label starts where
    // that column ends, so neither can drift into the other however long the
    // label or wide the glyph.
    let icon_centre = pos2(
        response.rect.left() + TILE_ICON_COLUMN / 2.0,
        response.rect.center().y,
    );
    paint_tool_icon(
        ui.painter(),
        Rect::from_center_size(icon_centre, vec2(TILE_ICON_SIZE, TILE_ICON_SIZE)),
        descriptor.icon,
        icon_color,
    );
    ui.painter().text(
        pos2(
            response.rect.left() + TILE_ICON_COLUMN + TILE_LABEL_GAP,
            response.rect.center().y,
        ),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(TILE_LABEL_TEXT_SIZE),
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
        // The row carries no button text at all: the icon and the label are both
        // painted at fixed offsets below, so neither can move relative to the
        // other. The previous row indented its label with six ordinary spaces,
        // which are proportional — the reserved width changed with the font and
        // theme, and wherever it fell short of the icon the two overlapped.
        let mut button = Button::new("")
            .selected(*variant == current)
            .min_size(vec2(236.0, 30.0));
        if let Some(shortcut) = descriptor.shortcut {
            button = button.shortcut_text(shortcut.label);
        }
        let mut response = ui.add_enabled(enabled, button);
        let icon_rect = Rect::from_center_size(
            pos2(
                response.rect.left() + MENU_ICON_COLUMN / 2.0,
                response.rect.center().y,
            ),
            vec2(MENU_ICON_SIZE, MENU_ICON_SIZE),
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
        ui.painter().text(
            pos2(
                response.rect.left() + MENU_ICON_COLUMN + MENU_LABEL_GAP,
                response.rect.center().y,
            ),
            Align2::LEFT_CENTER,
            descriptor.accessible_name,
            TextStyle::Button.resolve(ui.style()),
            icon_color,
        );
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
            ToolIcon::FitPointSpline => {
                self.line((0.15, 0.75), (0.35, 0.35));
                self.line((0.35, 0.35), (0.65, 0.65));
                self.line((0.65, 0.65), (0.85, 0.25));
                self.dot((0.15, 0.75), 0.05);
                self.dot((0.35, 0.35), 0.05);
                self.dot((0.65, 0.65), 0.05);
                self.dot((0.85, 0.25), 0.05);
            }
            ToolIcon::ControlVertexSpline => {
                self.line((0.15, 0.75), (0.85, 0.25));
                self.dot((0.15, 0.75), 0.05);
                self.dot((0.40, 0.15), 0.05);
                self.dot((0.60, 0.85), 0.05);
                self.dot((0.85, 0.25), 0.05);
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
            ToolIcon::Text => {
                // A capital A with its crossbar and a baseline: lettering.
                self.line((0.22, 0.76), (0.50, 0.16));
                self.line((0.50, 0.16), (0.78, 0.76));
                self.line((0.33, 0.55), (0.67, 0.55));
                self.line((0.14, 0.86), (0.86, 0.86));
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
            // An open corner and the same corner copied outside it: the shape
            // the tool makes, at the distance it holds.
            ToolIcon::Offset => {
                self.line((0.20, 0.20), (0.20, 0.66));
                self.line((0.20, 0.66), (0.66, 0.66));
                self.line((0.38, 0.38), (0.38, 0.84));
                self.line((0.38, 0.84), (0.84, 0.84));
                self.line((0.20, 0.20), (0.38, 0.38));
                self.line((0.66, 0.66), (0.84, 0.84));
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
            // The relation glyphs are the conventional CAD marks: two bars for
            // parallel, a corner square for perpendicular, matched ticks for
            // equal, and so on. A user coming from any mainstream package
            // reads them without a tooltip.
            ToolIcon::FixedRelation => {
                self.line((0.20, 0.80), (0.80, 0.80));
                self.line((0.20, 0.80), (0.32, 0.62));
                self.line((0.44, 0.80), (0.56, 0.62));
                self.line((0.68, 0.80), (0.80, 0.62));
                self.dot((0.50, 0.34), 0.09);
            }
            ToolIcon::CoincidentRelation => {
                self.circle((0.50, 0.50), 0.28);
                self.dot((0.50, 0.50), 0.10);
            }
            ToolIcon::HorizontalRelation => {
                self.line((0.14, 0.50), (0.86, 0.50));
                self.dot((0.22, 0.50), 0.07);
                self.dot((0.78, 0.50), 0.07);
            }
            ToolIcon::VerticalRelation => {
                self.line((0.50, 0.14), (0.50, 0.86));
                self.dot((0.50, 0.22), 0.07);
                self.dot((0.50, 0.78), 0.07);
            }
            ToolIcon::DistanceRelation => {
                self.line((0.18, 0.30), (0.18, 0.70));
                self.line((0.82, 0.30), (0.82, 0.70));
                self.line((0.18, 0.50), (0.82, 0.50));
                self.line((0.18, 0.50), (0.30, 0.42));
                self.line((0.18, 0.50), (0.30, 0.58));
                self.line((0.82, 0.50), (0.70, 0.42));
                self.line((0.82, 0.50), (0.70, 0.58));
            }
            ToolIcon::ParallelRelation => {
                self.line((0.30, 0.82), (0.52, 0.18));
                self.line((0.52, 0.82), (0.74, 0.18));
            }
            ToolIcon::PerpendicularRelation => {
                self.line((0.22, 0.82), (0.82, 0.82));
                self.line((0.38, 0.82), (0.38, 0.20));
                self.line((0.38, 0.68), (0.52, 0.68));
                self.line((0.52, 0.68), (0.52, 0.82));
            }
            ToolIcon::EqualLengthRelation => {
                self.line((0.20, 0.38), (0.80, 0.38));
                self.line((0.20, 0.62), (0.80, 0.62));
            }
            ToolIcon::TangentRelation => {
                self.circle((0.50, 0.58), 0.26);
                self.line((0.15, 0.32), (0.85, 0.32));
                self.dot((0.50, 0.32), 0.045);
            }
            ToolIcon::CollinearRelation => {
                self.line((0.15, 0.50), (0.45, 0.50));
                self.line((0.55, 0.50), (0.85, 0.50));
                self.dot((0.15, 0.50), 0.045);
                self.dot((0.45, 0.50), 0.045);
                self.dot((0.55, 0.50), 0.045);
                self.dot((0.85, 0.50), 0.045);
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
            let expected = match descriptor.variant {
                ToolVariant::Select => CommitContract::SessionOnly,
                // Dimension re-authors an existing recipe, so it publishes on
                // acceptance rather than through the shared rail (ADR 0027).
                ToolVariant::Dimension => CommitContract::CommitsOnAcceptance,
                _ => CommitContract::StageThenUniversalTickOrEnter,
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
    fn fixed_layout_is_three_rows_and_fits_the_supported_ribbon() {
        assert_eq!(DRAWING_FAMILIES.len(), DRAWING_COLUMNS * TOOLBAR_ROWS);
        assert_eq!(
            DRAWING_BLOCK_WIDTH
                + BLOCK_DIVIDER_WIDTH
                + GENERATOR_BLOCK_WIDTH
                + BLOCK_DIVIDER_WIDTH
                + CONSTRAINT_BLOCK_WIDTH,
            SKETCH_TOOLBAR_WIDTH
        );
        // All three blocks are built from the same tile, which is what makes a
        // constraint read as the same kind of control as a drawing tool.
        assert_eq!(
            DRAWING_BLOCK_WIDTH - CONSTRAINT_BLOCK_WIDTH,
            (DRAWING_COLUMNS as f32 - CONSTRAINT_COLUMNS as f32)
                * (PRIMARY_CELL_WIDTH + FAMILY_GAP)
        );
        assert!((20.0..=26.0).contains(&PRIMARY_CELL_SIZE));
        // Three rows fill the ribbon's content box exactly, which is what puts
        // the group caption on its bottom edge rather than above a gap.
        assert_eq!(SKETCH_TOOLBAR_HEIGHT, 76.0);
        // The label-clears-its-columns bounds are compile-time assertions
        // beside the constants; a geometry that breaks one does not build.

        // Every family is drawn, and every family is drawn once: the drawing
        // and generator families as tiles, the constraint families as their
        // variants' tiles.
        let mut all = DRAWING_FAMILIES
            .iter()
            .chain(GENERATOR_FAMILIES)
            .copied()
            .chain(CONSTRAINT_TOOLS.iter().map(|variant| variant.family()))
            .collect::<Vec<_>>();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all, ToolFamily::ALL);
        // The three lists partition the families: nothing drawn twice, nothing
        // left to a block that does not know how to draw it.
        for family in DRAWING_FAMILIES {
            assert!(!CONSTRAINT_FAMILIES.contains(family), "{family:?}");
            assert!(!GENERATOR_FAMILIES.contains(family), "{family:?}");
        }
        for family in GENERATOR_FAMILIES {
            assert!(!CONSTRAINT_FAMILIES.contains(family), "{family:?}");
        }
    }

    /// The block is the whole of both constraint families and nothing else:
    /// a relation missing from it is a relation the user cannot reach.
    #[test]
    fn every_constraint_variant_has_a_tile_of_its_own() {
        let mut listed = CONSTRAINT_TOOLS.to_vec();
        listed.sort_unstable();
        listed.dedup();
        assert_eq!(listed.len(), CONSTRAINT_TOOLS.len(), "a tile is repeated");

        let mut expected = CONSTRAINT_FAMILIES
            .iter()
            .flat_map(|family| family.variants())
            .copied()
            .collect::<Vec<_>>();
        expected.sort_unstable();
        assert_eq!(listed, expected);
        assert!(CONSTRAINT_TOOLS.len() <= CONSTRAINT_COLUMNS * TOOLBAR_ROWS);
    }

    /// Every tile says which tool it is. A constraint that fell back to its
    /// family name would put "Relation" on ten different tiles.
    #[test]
    fn every_constraint_tile_carries_its_own_name() {
        let mut labels = Vec::new();
        for variant in CONSTRAINT_TOOLS {
            let label = variant.tile_label();
            assert!(
                variant.descriptor().accessible_name.starts_with(label)
                    || label == "Dimension" && variant == &ToolVariant::Dimension
                    || label == "Equal",
                "{label} does not name {}",
                variant.descriptor().accessible_name
            );
            labels.push(label);
        }
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), CONSTRAINT_TOOLS.len(), "a name is repeated");
    }
}
