//! CAM from the history (ADR 0057).
//!
//! From a snapshot this crate decides whether a part is turned or milled,
//! chooses stock, tools and operations, generates toolpaths and tool changes,
//! posts G-code, and simulates the G-code against an exact lathe stock or a
//! heightmap mill stock. It never draws, and everything it does is testable
//! headlessly.
//!
//! The kernel is read through five public queries (`turned_section`,
//! `prism_profile`, `point_in_solid`, `offset_loop`, `profile_boolean`); the
//! algorithms are the literature's, named where they are used: contour
//! parallel pocketing by repeated offsetting, the heightmap stock model,
//! feeds and speeds from the handbooks, and LinuxCNC's G-code dialect.

pub mod geom;
pub mod plan;
pub mod recognise;
pub mod simulate;
pub mod space;
pub mod stock;
pub mod tools;
pub mod turning;

use std::fmt;

use artificer_kernel::Snapshot;

pub use plan::{FeedRate, Machine, Move, Operation, OperationKind, Plan, Spindle};
pub use recognise::{
    BarStock, BoxStock, FaceIssue, Level, MillAllowances, MillTurnSetup, MilledSetup, Setup,
    TurnAllowances, TurnAxis, TurnedSetup, WorkOrigin, recognise, recognise_with,
};
pub use tools::{Material, Tool, ToolKind, ToolLibrary};

/// Plans a recognised setup: the operations, tools and toolpaths.
pub fn plan_setup(
    setup: &Setup,
    library: &ToolLibrary,
    material: Material,
) -> Result<Plan, CamRefusal> {
    match setup {
        Setup::Turned(turned) => turning::plan_turning(turned, library, material),
        Setup::Milled(_) => Err(CamRefusal::Pocket {
            detail: "milling is planned in the next slice".to_owned(),
        }),
        Setup::MillTurn(mill_turn) => Err(CamRefusal::MillTurnNotPlanned {
            faces: mill_turn.milled_faces.clone(),
        }),
        Setup::Unsupported { faces } => Err(CamRefusal::Unsupported {
            faces: faces.clone(),
        }),
    }
}

/// The whole button: recognise a body and plan it.
pub fn plan_part(
    snapshot: &Snapshot,
    library: &ToolLibrary,
    material: Material,
) -> Result<(Setup, Plan), CamRefusal> {
    let setup = recognise(snapshot);
    let plan = plan_setup(&setup, library, material)?;
    Ok((setup, plan))
}

/// Why CAM could not plan a part, by name.
#[derive(Clone, Debug, PartialEq)]
pub enum CamRefusal {
    /// Recognition found no way to hold the part.
    Unsupported { faces: Vec<FaceIssue> },
    /// A turned body with radial features needs a second setup on a mill.
    MillTurnNotPlanned { faces: Vec<FaceIssue> },
    /// The section has a face turned towards the chuck that is not a
    /// groove's wall: it cannot be reached from the tailstock end.
    TurnedUndercut { detail: String },
    /// A groove the parting blade cannot make.
    GrooveUnsupported { detail: String },
    /// A bore at both ends needs a second setup.
    BoreBothEnds,
    /// A bore that gets wider as it goes deeper.
    BoreUndercut { detail: String },
    /// The stock model refused a pass.
    StockModel { detail: String },
    /// No tool in the library fits.
    NoToolFits { detail: String },
    /// A milled level's outline could not be read.
    Outline { detail: String },
    /// A pocket could not be offset.
    Pocket { detail: String },
}

impl fmt::Display for CamRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { faces } => write!(
                formatter,
                "the part is neither a solid of revolution nor a 2.5D prism: {}",
                faces_in_words(faces)
            ),
            Self::MillTurnNotPlanned { faces } => write!(
                formatter,
                "the part is turned but carries features a lathe cannot make; mill-turn is a later slice: {}",
                faces_in_words(faces)
            ),
            Self::TurnedUndercut { detail } => write!(formatter, "undercut: {detail}"),
            Self::GrooveUnsupported { detail } => write!(formatter, "groove: {detail}"),
            Self::BoreBothEnds => {
                formatter.write_str("the part is bored from both ends, which needs a second setup")
            }
            Self::BoreUndercut { detail } => write!(formatter, "bore undercut: {detail}"),
            Self::StockModel { detail } => {
                write!(formatter, "the stock model refused a pass: {detail}")
            }
            Self::NoToolFits { detail } => write!(formatter, "no tool fits: {detail}"),
            Self::Outline { detail } => write!(formatter, "outline: {detail}"),
            Self::Pocket { detail } => write!(formatter, "pocket: {detail}"),
        }
    }
}

impl std::error::Error for CamRefusal {}

fn faces_in_words(faces: &[FaceIssue]) -> String {
    if faces.is_empty() {
        return "no faces".to_owned();
    }
    faces
        .iter()
        .map(|issue| format!("face {} ({}) {}", issue.face, issue.surface, issue.reason))
        .collect::<Vec<_>>()
        .join("; ")
}
