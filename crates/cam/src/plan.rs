//! What CAM decides: operations, each with a tool, a spindle setting, a
//! feed and a toolpath, in the order they run.
//!
//! Toolpaths are written in the machine's work coordinates — the ones the
//! G-code carries. On the lathe a point is `(r, 0, z)`: radius, never
//! diameter, with `z = 0` at the front face. On the mill it is `(x, y, z)`
//! from the work origin.

use artificer_protocol::{Point2, Point3};
use serde::{Deserialize, Serialize};

use crate::tools::{Material, Tool};

/// Which machine a plan runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Machine {
    Lathe,
    Mill,
}

impl Machine {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lathe => "lathe",
            Self::Mill => "mill",
        }
    }
}

/// What an operation does, for its name and its ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Face,
    Rough,
    CentreDrill,
    Drill,
    BoreRough,
    BoreFinish,
    Finish,
    Groove,
    PartOff,
    Pocket,
    Profile,
    HelicalBore,
}

impl OperationKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Face => "Face",
            Self::Rough => "Rough",
            Self::CentreDrill => "Centre drill",
            Self::Drill => "Drill",
            Self::BoreRough => "Rough bore",
            Self::BoreFinish => "Finish bore",
            Self::Finish => "Finish",
            Self::Groove => "Groove",
            Self::PartOff => "Part off",
            Self::Pocket => "Pocket",
            Self::Profile => "Profile",
            Self::HelicalBore => "Helical bore",
        }
    }
}

/// The spindle as the post writes it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Spindle {
    /// `G97 S`: a fixed speed in rpm.
    Rpm(f64),
    /// `G96 S… D…`: constant surface speed in m/min under a cap in rpm.
    SurfaceSpeed {
        metres_per_minute: f64,
        max_rpm: f64,
    },
}

/// The feed as the post writes it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum FeedRate {
    /// `G94 F`: millimetres per minute.
    PerMinute(f64),
    /// `G95 F`: millimetres per revolution.
    PerRevolution(f64),
}

/// One programmed move.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Move {
    Rapid {
        to: Point3,
    },
    Feed {
        to: Point3,
    },
    /// An arc in the working plane (`XY` on the mill, `XZ` on the lathe)
    /// about `center`, given in the plane's two coordinates; `to.z` on the
    /// mill may differ from the start for a helix.
    Arc {
        to: Point3,
        center: Point2,
        clockwise: bool,
    },
    /// A canned drilling cycle at `(x, y)`: `G81` when `peck` is `None`,
    /// `G83` otherwise. The tool rapids to `retract`, feeds to `depth` and
    /// rapids back to `retract`.
    Drill {
        x: f64,
        y: f64,
        depth: f64,
        retract: f64,
        peck: Option<f64>,
    },
}

impl Move {
    /// Where the move ends, for cycles the retract plane over the hole.
    #[must_use]
    pub fn end(&self) -> Point3 {
        match *self {
            Self::Rapid { to } | Self::Feed { to } | Self::Arc { to, .. } => to,
            Self::Drill { x, y, retract, .. } => Point3::new(x, y, retract),
        }
    }
}

/// One operation of the plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub kind: OperationKind,
    pub name: String,
    pub tool: u32,
    pub spindle: Spindle,
    pub feed: FeedRate,
    pub moves: Vec<Move>,
    /// Things the user should know: approximations, choices made.
    pub notes: Vec<String>,
}

impl Operation {
    /// Where the tool is safe: the plane a tool change retracts to.
    #[must_use]
    pub fn is_lathe_inside(&self) -> bool {
        matches!(
            self.kind,
            OperationKind::CentreDrill
                | OperationKind::Drill
                | OperationKind::BoreRough
                | OperationKind::BoreFinish
        )
    }
}

/// The whole plan for one setup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub machine: Machine,
    pub material: Material,
    /// A name for the header.
    pub setup_name: String,
    pub operations: Vec<Operation>,
    /// Every tool the plan uses, by number.
    pub tools: Vec<Tool>,
    /// The `z` (mill) or `x` (lathe) a tool change retracts to, in work
    /// coordinates.
    pub safe_height: f64,
    /// Rapid traverse in mm/min, for the time estimate.
    pub rapid_rate: f64,
    pub tool_change_seconds: f64,
    /// What the plan approximated, for the card and the report.
    pub notes: Vec<String>,
}

impl Plan {
    #[must_use]
    pub fn tool(&self, number: u32) -> Option<&Tool> {
        self.tools.iter().find(|tool| tool.number == number)
    }

    /// How many `M6` the program needs: one per change of tool between
    /// consecutive operations, plus the first.
    #[must_use]
    pub fn tool_changes(&self) -> usize {
        let mut changes = 0;
        let mut current = None;
        for operation in &self.operations {
            if current != Some(operation.tool) {
                changes += 1;
                current = Some(operation.tool);
            }
        }
        changes
    }

    /// The sequence of tool numbers, one per change.
    #[must_use]
    pub fn tool_sequence(&self) -> Vec<u32> {
        let mut sequence: Vec<u32> = Vec::new();
        for operation in &self.operations {
            if sequence.last() != Some(&operation.tool) {
                sequence.push(operation.tool);
            }
        }
        sequence
    }
}
