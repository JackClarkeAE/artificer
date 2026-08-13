//! A compact, native-only visual lab for the first Artificer kernel slice.
//!
//! The app deliberately consumes only the public transactional kernel facade.
//! It never constructs topology, validation results, or display source maps on
//! its own.

pub use artificer_sketch_ui as sketch;
pub use artificer_sketch_ui::sketch_toolbar;
pub use artificer_ui_core::{drag_handle, navigation, presentation, theme};
pub use artificer_viewport as viewport;

pub mod assembly;
mod development_log;
pub mod document_replay;
mod export;
pub mod library_catalog;
pub mod material;
pub mod part_library;
mod ribbon;
pub mod shell;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::hash::{DefaultHasher, Hash as _, Hasher as _};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use artificer_catalog::CatalogStore;
use artificer_compute::{
    ComputePool, ExecutionMode, JobError, JobHandle, JobPriority, JobScheduler,
};
use artificer_geometry::{Orientation2, Point2 as GeometryPoint2, orient2d};
use artificer_kernel::{
    CancellationToken, DebugScene, ExecutionOutcome, FaceBoundaryCurve2, FaceRole, NativeKernel,
    PlanarFaceSupport, Snapshot, SnapshotMeasures,
};
use artificer_model::persistent::{
    FeatureOperationReport, PersistentRef, PersistentResolution, TargetedKernel,
    resolve_persistent_ref,
};
use artificer_model::{
    BodyId, BooleanFeatureRecipe, ComponentContentDigest, ComponentDefinitionRef,
    ComponentDefinitionRevision, ComponentInstanceDraft, ComponentInstanceId,
    ComponentInstanceRecord, FeatureDraft, FeatureId, FeatureInput, FeatureKind, FeatureOutput,
    JointAxis, JointDraft, JointKind, JointOrigin, JointParent, ModelDocument, OutputDraft,
    ParameterBinding, ParameterExposure, ParameterId, ParameterMetadata, ParameterOverrides,
    ParameterSpec, ParameterType, ParameterUnit, ParameterValue, QuantityKind, RebuildState,
    ReplayAction, ReplayDisposition, RigidComponentPose, SketchId, SketchPayload,
    SketchRegionExtrusion, SketchSupportRecipe, SnapshotAssociation,
};
use artificer_protocol::{
    Aabb3, ArcDirection, BooleanOperation, BooleanRequest, CURRENT_PROTOCOL_VERSION,
    EdgeFinishKind, EntityKind, EntityRef, ExecuteRequest, FaceExtrusionOperation, HistoryRelation,
    KernelCommand, KernelError, KernelErrorCode, KernelStage, MAX_EXTRUSION_PROFILE_VERTICES,
    MAX_PLANAR_PROFILE_CURVES, MAX_PLANAR_PROFILE_LOOPS, MAX_PLANAR_PROFILE_REGIONS,
    OperationReport, PlanarAxis2, PlanarCurve2, PlanarFrame3, PlanarLoop2, PlanarProfile2,
    PlanarRegion2, Point2 as ProtocolPoint2, Point3, PrecisionPolicy, RequestId, RevolveAngle,
    RotationQuaternion, SemanticDigest, SnapshotId, TopologyCounts, Vector3,
};
use artificer_sketch::{
    ArrangementCell, ArrangementLimits, CurveDirection as AuthoringCurveDirection,
    CurveIntersections, EvaluatedCurve2 as AuthoringCurve2, RegionSignature, SketchArrangement,
    SketchDefinition, SketchPoint2 as AuthoringPoint2, build_arrangement, compile_selected_profile,
    intersect_curves,
};
use eframe::egui;
use egui::{Color32, CornerRadius, FontId, Frame, Margin, RichText, Stroke};
use serde::{Deserialize, Serialize};

use crate::development_log::{DevelopmentRecorder, UiTraceState};
use crate::document_replay::{HydratedDocument, HydrationOptions, hydrate_model_document};
use crate::export::{ExportTriangle, write_ascii_stl, write_faceted_step};
use crate::library_catalog::{
    builtin_aluminium_extrusion_package, resolve_builtin_insertion, resolve_store_insertion,
};
use crate::part_library::{PartInsertionEligibility, PartInsertionIntent, PartLibraryState};
use crate::presentation::{
    ActiveTool, CameraTransition, DisplayTransform, MotionState, StandardView, ViewState,
    bounds_center,
};
use crate::shell::{WorkbenchShellState, WorkbenchShellVisibility};
use crate::sketch::{
    CertifiedProfileStatus, CertifiedSketchCurve, CertifiedSketchLoop, CertifiedSketchProfile,
    DimensionInputError, DimensionKeyClaims, DimensionReadout, SelectedRecipeEditorView,
    SketchCanvasState, SketchContextCurve, SketchContextEdge, SketchContextFitKey,
    SketchContextTriangle, SketchCurveDirection, SketchDimensionKind, SketchEditError,
    SketchEntity, SketchEntityId, SketchGeometry, SketchPlane, SketchPoint, SketchView,
    SketchViewportContext,
};
use crate::sketch_toolbar::{
    SelectionRequirement, SketchToolbarState, ToolInputKind, ToolVariant, paint_tool_icon,
};

use crate::theme::{
    ACCENT, BAD, BG, BORDER, CARD, GOOD, MUTED, PANEL, RIBBON_FILL, SELECTED_FILL, TEXT,
    TIMELINE_FILL, WARN, install_style,
};
const ORIGIN_PLANE_HALF_EXTENT_MM: f64 = 25.0;
const MAX_NATIVE_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;
static DOCUMENT_SAVE_COUNTER: AtomicU64 = AtomicU64::new(1);
/// Shades a body that is currently picked as a Boolean tool. It is deliberately
/// unlike any material colour so a tinted body cannot be mistaken for a pick.
const BOOLEAN_TOOL_TINT: egui::Color32 = egui::Color32::from_rgb(222, 104, 30);
const ARTIFICER_WORKSPACE_FORMAT: &str = "artificer.workspace";
const ARTIFICER_WORKSPACE_VERSION: u32 = 1;

/// User-facing document length unit. Kernel and persisted geometry remain in
/// canonical millimetres; this setting controls entry/readout conversion.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayLengthUnit {
    Micrometre,
    #[default]
    Millimetre,
    Centimetre,
    Metre,
    Inch,
    Foot,
}

impl DisplayLengthUnit {
    const ALL: [Self; 6] = [
        Self::Micrometre,
        Self::Millimetre,
        Self::Centimetre,
        Self::Metre,
        Self::Inch,
        Self::Foot,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Micrometre => "Micrometres (µm)",
            Self::Millimetre => "Millimetres (mm)",
            Self::Centimetre => "Centimetres (cm)",
            Self::Metre => "Metres (m)",
            Self::Inch => "Inches (in)",
            Self::Foot => "Feet (ft)",
        }
    }

    const fn symbol(self) -> &'static str {
        match self {
            Self::Micrometre => "µm",
            Self::Millimetre => "mm",
            Self::Centimetre => "cm",
            Self::Metre => "m",
            Self::Inch => "in",
            Self::Foot => "ft",
        }
    }

    const fn millimetres_per_unit(self) -> f64 {
        match self {
            Self::Micrometre => 0.001,
            Self::Millimetre => 1.0,
            Self::Centimetre => 10.0,
            Self::Metre => 1_000.0,
            Self::Inch => 25.4,
            Self::Foot => 304.8,
        }
    }

    fn convert_from_millimetres(self, value: f64) -> f64 {
        value / self.millimetres_per_unit()
    }

    fn format_length(self, value_mm: f64) -> String {
        let value = self.convert_from_millimetres(value_mm);
        let precision = if value.abs() >= 1_000.0 { 2 } else { 3 };
        format!("{value:.precision$} {}", self.symbol())
    }

    fn format_area(self, value_mm2: f64) -> String {
        let scale = self.millimetres_per_unit();
        let value = value_mm2 / (scale * scale);
        format!("{value:.3} {}²", self.symbol())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DocumentSettings {
    pub length_unit: DisplayLengthUnit,
    #[serde(default)]
    pub navigation: navigation::NavigationPreset,
}

#[derive(Deserialize, Serialize)]
struct ArtificerWorkspaceFile {
    format: String,
    version: u32,
    settings: DocumentSettings,
    #[serde(default)]
    construction_planes: Vec<ConstructionPlane>,
    #[serde(default)]
    materials: Vec<BodyMaterial>,
    document: ModelDocument,
}

/// One body's material assignment, persisted by stable key so the library can
/// grow without invalidating saved documents.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct BodyMaterial {
    body: u64,
    material: String,
}

/// One document-owned datum plane. Geometry is stored explicitly so the plane
/// remains available even while its source face is absent at a history stop;
/// `source` retains the creation intent for future associative remapping.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct ConstructionPlane {
    id: u64,
    name: String,
    #[serde(default)]
    feature: Option<FeatureId>,
    frame: PlanarFrame3,
    half_u: f64,
    half_v: f64,
    visible: bool,
    source: ConstructionPlaneSource,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ConstructionPlaneSource {
    OnFace {
        body: BodyId,
        face: EntityRef,
    },
    BetweenFaces {
        first_body: BodyId,
        first_face: EntityRef,
        second_body: BodyId,
        second_face: EntityRef,
    },
}

/// Top-level presentation mode of the Artificer workbench.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorkbenchMode {
    #[default]
    Model,
    Sketch,
}

impl WorkbenchMode {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Model => "Model",
            Self::Sketch => "Sketch",
        }
    }
}

const fn active_tool_trace_name(tool: ActiveTool) -> &'static str {
    match tool {
        ActiveTool::Select => "select",
        ActiveTool::Measure => "measure",
        ActiveTool::Orbit => "orbit",
        ActiveTool::Move => "move",
        ActiveTool::Rotate => "rotate",
        ActiveTool::Scale => "scale",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabCase {
    CanonicalCuboid,
    ZeroWidth,
    NonFiniteDepth,
    StaleSnapshot,
}

impl LabCase {
    const ALL: [Self; 4] = [
        Self::CanonicalCuboid,
        Self::ZeroWidth,
        Self::NonFiniteDepth,
        Self::StaleSnapshot,
    ];

    const fn title(self) -> &'static str {
        match self {
            Self::CanonicalCuboid => "Valid 2 × 3 × 4",
            Self::ZeroWidth => "Zero width",
            Self::NonFiniteDepth => "Non-finite depth",
            Self::StaleSnapshot => "Stale snapshot",
        }
    }

    const fn detail(self) -> &'static str {
        match self {
            Self::CanonicalCuboid => "Accepted reference body",
            Self::ZeroWidth => "Reject a degenerate extent",
            Self::NonFiniteDepth => "Reject NaN at the boundary",
            Self::StaleSnapshot => "Reject an obsolete edit target",
        }
    }

    const fn slug(self) -> &'static str {
        match self {
            Self::CanonicalCuboid => "canonical",
            Self::ZeroWidth => "zero-width",
            Self::NonFiniteDepth => "non-finite-depth",
            Self::StaleSnapshot => "stale-snapshot",
        }
    }
}

/// Exhaustive queue of interactive model mutations.
///
/// Widgets may create or edit this state, but only
/// `confirm_pending_operation` may execute it through the kernel.
/// A revolve captured at staging time: the region, the axis it turns about,
/// and the frame both live in.
#[derive(Clone, Debug, PartialEq)]
struct StagedRevolve {
    frame: PlanarFrame3,
    profile: PlanarProfile2,
    axis: PlanarAxis2,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PendingOperation {
    Transform {
        base_snapshot: SnapshotId,
    },
    ComponentPlacement {
        component: ComponentInstanceId,
        base_pose: RigidComponentPose,
    },
    SetComponentGrounded {
        component: ComponentInstanceId,
        base_grounded: bool,
        grounded: bool,
    },
    CreateRevoluteJoint {
        component: ComponentInstanceId,
    },
    RunCase {
        case: LabCase,
        base_snapshot: SnapshotId,
    },
    LibraryInsertion {
        staging_id: u64,
    },
    LoadDefaultDocument,
    SetParameterLiteral {
        parameter: ParameterId,
        base: ParameterLiteralDraft,
        value: ParameterLiteralDraft,
    },
    AddUserLengthParameter {
        ordinal: u32,
        value_mm: f64,
    },
    CreateConstructionPlane {
        frame: PlanarFrame3,
        half_u: f64,
        half_v: f64,
        source: ConstructionPlaneSource,
    },
    /// The tool bodies are picked interactively while this is staged and live
    /// in `boolean_tools`, the same way an edge finish collects `selected_edges`.
    /// Keeping them out of the pending value lets the operation stay `Copy`.
    BooleanBodies {
        target: BodyId,
        operation: BooleanOperation,
        keep_tools: bool,
    },
    PresetFeature {
        preset: SolidFeaturePreset,
        base_snapshot: SnapshotId,
        body: Option<BodyId>,
        target_face: Option<EntityRef>,
        frame: Option<PlanarFrame3>,
    },
    SketchEdit {
        entity: SketchEntityId,
        label: &'static str,
    },
    FinishSketch {
        plane: SketchPlane,
        revision: u64,
    },
    ExtrudeSketch {
        base_snapshot: SnapshotId,
        support_body: Option<BodyId>,
        plane: SketchPlane,
        revision: u64,
        cancel_mode: WorkbenchMode,
        finish_sketch_on_commit: bool,
        distance: f64,
        frame: PlanarFrame3,
        target_face: Option<EntityRef>,
        support_digest: Option<SemanticDigest>,
        mode: ExtrusionMode,
    },
    PushPullFace {
        base_snapshot: SnapshotId,
        support_body: BodyId,
        target_face: EntityRef,
        distance: f64,
    },
}

#[derive(Clone, Debug, PartialEq)]
struct AsyncFeaturePreviewIntent {
    input_snapshot: Option<SnapshotId>,
    frame: PlanarFrame3,
    profile: PlanarProfile2,
    target_face: Option<EntityRef>,
    distance: f64,
    mode: ExtrusionMode,
}

#[derive(Clone, Debug, PartialEq)]
struct AsyncEdgeFinishPreviewIntent {
    input_snapshot: SnapshotId,
    body: viewport::BodyInstanceKey,
    target_edges: Vec<EntityRef>,
    kind: EdgeFinishKind,
    distance: f64,
}

struct AsyncSketchExtrusionCommit {
    pending: PendingOperation,
    replay_command: KernelCommand,
    operation_id: String,
    started: Instant,
    kernel_cancellation: CancellationToken,
    job: JobHandle<TimedSketchExtrusionExecution>,
}

struct TimedSketchExtrusionExecution {
    queue_wait: Duration,
    execution: Duration,
    result: Result<ExecutionOutcome, KernelError>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct DevelopmentTraceFingerprint {
    workbench: WorkbenchMode,
    pending_operation: Option<&'static str>,
    model_tool: &'static str,
    sketch_tool: &'static str,
    history_position: usize,
    snapshot: Option<SnapshotId>,
    selection_digest: u64,
    drag_active: bool,
}

impl PendingOperation {
    const fn title(self) -> &'static str {
        match self {
            Self::Transform { .. } => "Transform whole body/group",
            Self::ComponentPlacement { .. } => "Place component",
            Self::SetComponentGrounded { grounded: true, .. } => "Ground component",
            Self::SetComponentGrounded {
                grounded: false, ..
            } => "Release component",
            Self::CreateRevoluteJoint { .. } => "Create revolute joint",
            Self::RunCase { case, .. } => case.title(),
            Self::LibraryInsertion { .. } => "Insert library component",
            Self::LoadDefaultDocument => "Open saved document",
            Self::SetParameterLiteral { .. } => "Update document parameter",
            Self::AddUserLengthParameter { .. } => "Add document parameter",
            Self::CreateConstructionPlane { source, .. } => match source {
                ConstructionPlaneSource::OnFace { .. } => "Create plane on face",
                ConstructionPlaneSource::BetweenFaces { .. } => "Create midplane",
            },
            Self::BooleanBodies { operation, .. } => match operation {
                BooleanOperation::Union => "Combine bodies",
                BooleanOperation::Difference => "Subtract bodies",
                BooleanOperation::Intersection => "Intersect bodies",
            },
            Self::PresetFeature { preset, .. } => preset.label(),
            Self::SketchEdit { label, .. } => label,
            Self::FinishSketch { .. } => "Finish sketch",
            Self::ExtrudeSketch {
                finish_sketch_on_commit: true,
                ..
            } => "Extrude active sketch",
            Self::ExtrudeSketch {
                finish_sketch_on_commit: false,
                ..
            } => "Extrude finished sketch",
            Self::PushPullFace { .. } => "Push/pull selected face",
        }
    }

    const fn detail(self) -> &'static str {
        match self {
            Self::Transform { .. } => "Validate and publish the transform as model truth",
            Self::ComponentPlacement { .. } => {
                "Commit a rigid occurrence pose without rewriting the component B-rep"
            }
            Self::SetComponentGrounded { grounded: true, .. } => {
                "Lock this occurrence at its committed assembly pose"
            }
            Self::SetComponentGrounded {
                grounded: false, ..
            } => "Allow this occurrence to be placed again",
            Self::CreateRevoluteJoint { .. } => {
                "Add a named world-parented rotation axis to the assembly graph"
            }
            Self::RunCase { .. } => "Execute this diagnostic constructor through the native kernel",
            Self::LibraryInsertion { .. } => {
                "Commit one independently identified component insertion with its resolved parameters"
            }
            Self::LoadDefaultDocument => {
                "Replace the current workspace from the verified native document file"
            }
            Self::SetParameterLiteral { .. } => {
                "Publish the new typed value and rebuild every consuming feature"
            }
            Self::AddUserLengthParameter { .. } => {
                "Create one named, reusable length parameter in this document"
            }
            Self::CreateConstructionPlane { source, .. } => match source {
                ConstructionPlaneSource::OnFace { .. } => {
                    "Commit a datum plane coincident with the selected planar face"
                }
                ConstructionPlaneSource::BetweenFaces { .. } => {
                    "Commit a datum plane halfway between two parallel planar faces"
                }
            },
            Self::BooleanBodies { .. } => {
                "Click the tool bodies to combine with the target, then confirm to publish a validated successor"
            }
            Self::PresetFeature { preset, .. } => preset.detail(),
            Self::SketchEdit { .. } => {
                "Validate and publish this planar entity in the sketch document"
            }
            Self::FinishSketch { .. } => {
                "Accept only a certified closed profile and return to Model mode"
            }
            Self::ExtrudeSketch {
                finish_sketch_on_commit: true,
                ..
            } => "Finish the active sketch and publish its native solid atomically",
            Self::ExtrudeSketch {
                finish_sketch_on_commit: false,
                ..
            } => "Build, validate, and publish a native solid from the finished profile",
            Self::PushPullFace { .. } => {
                "Move the complete selected face and its adjacent walls as one exact solid edit"
            }
        }
    }

    fn trace_payload(self) -> serde_json::Value {
        let mut payload = serde_json::json!({"operation": self.title()});
        let object = payload
            .as_object_mut()
            .expect("the trace payload starts as an object");
        match self {
            Self::Transform { base_snapshot } | Self::RunCase { base_snapshot, .. } => {
                object.insert(
                    "base_snapshot".to_owned(),
                    serde_json::json!(base_snapshot.to_string()),
                );
            }
            Self::ComponentPlacement { component, .. }
            | Self::SetComponentGrounded { component, .. }
            | Self::CreateRevoluteJoint { component } => {
                object.insert(
                    "component".to_owned(),
                    serde_json::json!(component.to_string()),
                );
            }
            Self::LibraryInsertion { staging_id } => {
                object.insert("staging_id".to_owned(), serde_json::json!(staging_id));
            }
            Self::SetParameterLiteral { parameter, .. } => {
                object.insert(
                    "parameter_id".to_owned(),
                    serde_json::json!(parameter.to_string()),
                );
            }
            Self::AddUserLengthParameter { ordinal, value_mm } => {
                object.insert("ordinal".to_owned(), serde_json::json!(ordinal));
                object.insert("value_mm".to_owned(), serde_json::json!(value_mm));
            }
            Self::CreateConstructionPlane { source, .. } => {
                object.insert(
                    "plane_source".to_owned(),
                    serde_json::json!(format!("{source:?}")),
                );
            }
            Self::BooleanBodies {
                target,
                operation,
                keep_tools,
            } => {
                object.insert(
                    "target_body".to_owned(),
                    serde_json::json!(target.to_string()),
                );
                object.insert(
                    "boolean".to_owned(),
                    serde_json::json!(format!("{operation:?}")),
                );
                object.insert("keep_tools".to_owned(), serde_json::json!(keep_tools));
            }
            Self::PresetFeature {
                preset,
                base_snapshot,
                body,
                target_face,
                ..
            } => {
                object.insert("preset".to_owned(), serde_json::json!(preset.label()));
                object.insert(
                    "base_snapshot".to_owned(),
                    serde_json::json!(base_snapshot.to_string()),
                );
                object.insert(
                    "body".to_owned(),
                    serde_json::json!(body.map(|body| body.to_string())),
                );
                object.insert(
                    "target_face".to_owned(),
                    serde_json::json!(target_face.map(|face| face.to_string())),
                );
            }
            Self::SketchEdit { entity, .. } => {
                object.insert(
                    "entity".to_owned(),
                    serde_json::json!(format!("{entity:?}")),
                );
            }
            Self::FinishSketch { plane, revision } => {
                object.insert("plane".to_owned(), serde_json::json!(format!("{plane:?}")));
                object.insert("revision".to_owned(), serde_json::json!(revision));
            }
            Self::ExtrudeSketch {
                base_snapshot,
                revision,
                distance,
                mode,
                target_face,
                ..
            } => {
                object.insert(
                    "base_snapshot".to_owned(),
                    serde_json::json!(base_snapshot.to_string()),
                );
                object.insert("revision".to_owned(), serde_json::json!(revision));
                object.insert("distance_mm".to_owned(), serde_json::json!(distance));
                object.insert("mode".to_owned(), serde_json::json!(format!("{mode:?}")));
                object.insert(
                    "target_face".to_owned(),
                    serde_json::json!(target_face.map(|face| face.to_string())),
                );
            }
            Self::PushPullFace {
                base_snapshot,
                target_face,
                distance,
                ..
            } => {
                object.insert(
                    "base_snapshot".to_owned(),
                    serde_json::json!(base_snapshot.to_string()),
                );
                object.insert(
                    "target_face".to_owned(),
                    serde_json::json!(target_face.to_string()),
                );
                object.insert("distance_mm".to_owned(), serde_json::json!(distance));
            }
            Self::LoadDefaultDocument => {}
        }
        payload
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SolidFeaturePreset {
    Revolve,
    Hole,
    Rib,
    Mirror,
    LinearPattern,
    Chamfer,
    Fillet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EdgeFinishSelectionSupport {
    ExactParallelSet,
    ExactRimBlend,
    RegularizedBlendSet,
    Empty,
    MixedBodies,
    CurvedOrPartialEdge,
}

impl EdgeFinishSelectionSupport {
    const fn can_commit(self) -> bool {
        matches!(
            self,
            Self::ExactParallelSet | Self::ExactRimBlend | Self::RegularizedBlendSet
        )
    }

    const fn headline(self) -> &'static str {
        match self {
            Self::ExactParallelSet => "EXACT PARALLEL SET",
            Self::ExactRimBlend => "EXACT RIM BLEND",
            Self::RegularizedBlendSet => "REGULARIZED CORNER BLEND",
            Self::Empty => "SELECT EDGES",
            Self::MixedBodies => "ONE BODY REQUIRED",
            Self::CurvedOrPartialEdge => "EDGE SET UNSUPPORTED",
        }
    }

    const fn detail(self) -> &'static str {
        match self {
            Self::ExactParallelSet => {
                "The selected complete parallel prism edges will be finished as one exact feature."
            }
            Self::ExactRimBlend => {
                "The complete cap rim finishes exactly: a fillet sweeps cylinder, torus and sphere patches around the loop, a chamfer cuts planar and conical slants, both with analytic measures and no faceted reconstruction."
            }
            Self::RegularizedBlendSet => {
                "Interacting and successor edge neighbourhoods will be regularized together. Chamfers remain planar; stacked fillets use the document approximation budget."
            }
            Self::Empty => "Select one or more edges on the active body.",
            Self::MixedBodies => "Every edge in one finish feature must belong to the active body.",
            Self::CurvedOrPartialEdge => {
                "Only complete cap rims and straight prism edges support finishes; partial arcs and other curved carriers remain gated."
            }
        }
    }
}

/// A plane sketch deferred until its camera flight lands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PendingPlaneSketch {
    Origin(SketchPlane),
    Construction(u64),
}

impl SolidFeaturePreset {
    const fn label(self) -> &'static str {
        match self {
            Self::Revolve => "Revolve radial section",
            Self::Hole => "Drill hole",
            Self::Rib => "Add rib",
            Self::Mirror => "Mirror body",
            Self::LinearPattern => "Linear body pattern",
            Self::Chamfer => "Chamfer edge",
            Self::Fillet => "Fillet edge",
        }
    }

    const fn detail(self) -> &'static str {
        match self {
            Self::Revolve => "Create an exact full-turn annular revolve as a new body",
            Self::Hole => "Cut an exact cylindrical hole normal to the selected planar face",
            Self::Rib => "Add a straight rectangular rib to the selected planar face",
            Self::Mirror => "Mirror the active all-planar body across the world YZ plane",
            Self::LinearPattern => "Create three separated +X copies as one multi-solid body group",
            Self::Chamfer => {
                "Finish one or more compatible cuboid edges with exact planar chamfers"
            }
            Self::Fillet => {
                "Finish one or more compatible cuboid edges with exact cylindrical fillets"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum ParameterLiteralDraft {
    Quantity { magnitude: f64, unit: ParameterUnit },
    Integer(i64),
    Boolean(bool),
}

impl ParameterLiteralDraft {
    fn from_value(value: &ParameterValue) -> Option<Self> {
        match value {
            ParameterValue::Quantity { value } => Some(Self::Quantity {
                magnitude: value.magnitude,
                unit: value.unit,
            }),
            ParameterValue::Integer { value } => Some(Self::Integer(*value)),
            ParameterValue::Boolean { value } => Some(Self::Boolean(*value)),
            ParameterValue::Choice { .. } => None,
        }
    }

    fn into_value(self) -> ParameterValue {
        match self {
            Self::Quantity { magnitude, unit } => ParameterValue::quantity(magnitude, unit),
            Self::Integer(value) => ParameterValue::integer(value),
            Self::Boolean(value) => ParameterValue::boolean(value),
        }
    }
}

/// User-visible material intent for the extrusion command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExtrusionMode {
    #[default]
    NewBody,
    Add,
    Cut,
}

impl ExtrusionMode {
    const fn label(self) -> &'static str {
        match self {
            Self::NewBody => "New body",
            Self::Add => "Add",
            Self::Cut => "Cut",
        }
    }

    const fn feature_operation(self) -> Option<FaceExtrusionOperation> {
        match self {
            Self::NewBody => None,
            Self::Add => Some(FaceExtrusionOperation::Add),
            Self::Cut => Some(FaceExtrusionOperation::Cut),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum SketchSupport {
    Origin {
        plane: SketchPlane,
    },
    ConstructionPlane {
        id: Option<u64>,
        frame: Box<PlanarFrame3>,
    },
    PlanarFace {
        body: BodyId,
        snapshot: SnapshotId,
        face: EntityRef,
        frame: Box<PlanarFrame3>,
        boundary: Vec<ProtocolPoint2>,
        inner_boundaries: Vec<Vec<ProtocolPoint2>>,
        support_digest: SemanticDigest,
    },
}

impl Default for SketchSupport {
    fn default() -> Self {
        Self::Origin {
            plane: SketchPlane::XY,
        }
    }
}

impl SketchSupport {
    fn frame(&self) -> PlanarFrame3 {
        match self {
            Self::Origin { plane } => sketch_plane_frame(*plane),
            Self::ConstructionPlane { frame, .. } => **frame,
            Self::PlanarFace { frame, .. } => **frame,
        }
    }

    const fn target_face(&self) -> Option<EntityRef> {
        match self {
            Self::Origin { .. } | Self::ConstructionPlane { .. } => None,
            Self::PlanarFace { face, .. } => Some(*face),
        }
    }

    const fn support_digest(&self) -> Option<SemanticDigest> {
        match self {
            Self::Origin { .. } | Self::ConstructionPlane { .. } => None,
            Self::PlanarFace { support_digest, .. } => Some(*support_digest),
        }
    }

    const fn body(&self) -> Option<BodyId> {
        match self {
            Self::Origin { .. } | Self::ConstructionPlane { .. } => None,
            Self::PlanarFace { body, .. } => Some(*body),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Origin { plane } => origin_plane_label(*plane).to_owned(),
            Self::ConstructionPlane { id: Some(id), .. } => format!("Plane {id}"),
            Self::ConstructionPlane { id: None, .. } => "Construction plane".to_owned(),
            Self::PlanarFace { face, .. } => format!("Face #{}", face.entity),
        }
    }

    fn axis_labels(&self) -> [&'static str; 2] {
        match self {
            Self::Origin { plane } => plane.axis_labels(),
            Self::ConstructionPlane { frame, .. } | Self::PlanarFace { frame, .. } => [
                dominant_axis_label(frame.u).unwrap_or("U"),
                dominant_axis_label(frame.v).unwrap_or("V"),
            ],
        }
    }

    fn display_normal(&self) -> Vector3 {
        frame_normal(self.frame()).unwrap_or_else(|| Vector3::new(0.0, 0.0, 0.0))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ModelBodyKind {
    #[default]
    Cuboid,
    SketchExtrusion,
    AddedBoss,
    CutPocket,
    PushedPulled,
    Boolean,
}

impl ModelBodyKind {
    const fn browser_label(self) -> &'static str {
        match self {
            Self::Cuboid => "native cuboid",
            Self::SketchExtrusion => "native sketch extrusion",
            Self::AddedBoss => "native added boss",
            Self::CutPocket => "native cut pocket",
            Self::PushedPulled => "native pushed/pulled solid",
            Self::Boolean => "native Boolean result",
        }
    }
}

fn browser_body_object_name(ordinal: u32, solid_count: u64) -> String {
    if solid_count > 1 {
        format!("Body group {ordinal} · {solid_count} solids")
    } else {
        format!("Body {ordinal}")
    }
}

fn model_segment_length(segment: [Point3; 2]) -> f64 {
    (segment[1].x - segment[0].x)
        .hypot(segment[1].y - segment[0].y)
        .hypot(segment[1].z - segment[0].z)
}

/// Certified finite-segment distance used by the model Measure tool.
/// Parallel, crossing, skew, point-edge, and point-point cases all share this
/// clamped model-space calculation; no screen projection enters the readout.
fn model_segment_distance(first: [Point3; 2], second: [Point3; 2]) -> f64 {
    let vector = |from: Point3, to: Point3| [to.x - from.x, to.y - from.y, to.z - from.z];
    let dot = |left: [f64; 3], right: [f64; 3]| {
        left[0].mul_add(right[0], left[1].mul_add(right[1], left[2] * right[2]))
    };
    let subtract = |left: [f64; 3], right: [f64; 3]| {
        [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
    };
    let u = vector(first[0], first[1]);
    let v = vector(second[0], second[1]);
    let w = vector(second[0], first[0]);
    let a = dot(u, u);
    let b = dot(u, v);
    let c = dot(v, v);
    let d = dot(u, w);
    let e = dot(v, w);
    let epsilon = 128.0 * f64::EPSILON * a.max(c).max(1.0);

    let (mut numerator_s, mut denominator_s) = (b * e - c * d, a * c - b * b);
    let (mut numerator_t, mut denominator_t) = (a * e - b * d, denominator_s);
    if a <= epsilon && c <= epsilon {
        return model_segment_length([first[0], second[0]]);
    }
    if a <= epsilon {
        numerator_s = 0.0;
        denominator_s = 1.0;
        numerator_t = e;
        denominator_t = c;
    } else if c <= epsilon {
        numerator_t = 0.0;
        denominator_t = 1.0;
        numerator_s = -d;
        denominator_s = a;
    } else {
        if denominator_s.abs() <= epsilon {
            numerator_s = 0.0;
            denominator_s = 1.0;
            numerator_t = e;
            denominator_t = c;
        } else if numerator_s < 0.0 {
            numerator_s = 0.0;
            numerator_t = e;
            denominator_t = c;
        } else if numerator_s > denominator_s {
            numerator_s = denominator_s;
            numerator_t = e + b;
            denominator_t = c;
        }
        if numerator_t < 0.0 {
            numerator_t = 0.0;
            numerator_s = (-d).clamp(0.0, a);
            denominator_s = a;
        } else if numerator_t > denominator_t {
            numerator_t = denominator_t;
            numerator_s = (b - d).clamp(0.0, a);
            denominator_s = a;
        }
    }
    let s = if numerator_s.abs() <= epsilon {
        0.0
    } else {
        numerator_s / denominator_s
    };
    let t = if numerator_t.abs() <= epsilon {
        0.0
    } else {
        numerator_t / denominator_t
    };
    let separation = subtract(
        w,
        subtract(
            [v[0] * t, v[1] * t, v[2] * t],
            [u[0] * s, u[1] * s, u[2] * s],
        ),
    );
    dot(separation, separation).max(0.0).sqrt()
}

fn model_segments_share_tangent_endpoint(first: [Point3; 2], second: [Point3; 2]) -> bool {
    let near = |left: Point3, right: Point3| model_segment_length([left, right]) <= 1.0e-8;
    if !first
        .into_iter()
        .any(|left| second.into_iter().any(|right| near(left, right)))
    {
        return false;
    }
    let direction = |segment: [Point3; 2]| {
        [
            segment[1].x - segment[0].x,
            segment[1].y - segment[0].y,
            segment[1].z - segment[0].z,
        ]
    };
    let first = direction(first);
    let second = direction(second);
    let length = |vector: [f64; 3]| {
        vector[0]
            .mul_add(
                vector[0],
                vector[1].mul_add(vector[1], vector[2] * vector[2]),
            )
            .sqrt()
    };
    let denominator = length(first) * length(second);
    denominator > f64::EPSILON
        && (first[0].mul_add(second[0], first[1].mul_add(second[1], first[2] * second[2]))
            / denominator)
            .abs()
            >= 1.0 - 1.0e-8
}

/// Compact presentation projection of the authoritative `ModelDocument`.
///
/// Replay payloads, stable identity, dependencies, and rebuild state remain in
/// `artificer-model`; this type carries only labels and active-chip state needed
/// by the workbench timeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FeaturePreviewKind {
    Origin,
    BaseBody,
    Component,
    Sketch,
    Extrude,
    Add,
    Cut,
    Transform,
    Boolean,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FeaturePreviewEntry {
    kind: FeaturePreviewKind,
    ordinal: u32,
    revision: u64,
    finished: bool,
    /// Stable lineage colour/group for related sketch and solid features.
    group: u64,
}

impl FeaturePreviewEntry {
    const fn fixed(kind: FeaturePreviewKind) -> Self {
        Self {
            kind,
            ordinal: 0,
            revision: 0,
            finished: true,
            group: 0,
        }
    }

    fn label(&self) -> String {
        match self.kind {
            FeaturePreviewKind::Origin => "Origin".to_owned(),
            FeaturePreviewKind::BaseBody => "Base body".to_owned(),
            FeaturePreviewKind::Component => format!("Component {}", self.ordinal),
            FeaturePreviewKind::Sketch => {
                format!("Sketch {} · r{}", self.ordinal, self.revision)
            }
            FeaturePreviewKind::Extrude => format!("Extrude {}", self.ordinal),
            FeaturePreviewKind::Add => format!("Add {}", self.ordinal),
            FeaturePreviewKind::Cut => format!("Cut {}", self.ordinal),
            FeaturePreviewKind::Transform => format!("Transform {}", self.ordinal),
            FeaturePreviewKind::Boolean => format!("Boolean {}", self.ordinal),
        }
    }

    fn accessible_label(&self) -> String {
        match self.kind {
            FeaturePreviewKind::Origin => "Origin feature".to_owned(),
            FeaturePreviewKind::BaseBody => "Base cuboid feature".to_owned(),
            _ => format!(
                "{} feature",
                self.label().split(" · ").next().unwrap_or("Feature")
            ),
        }
    }
}

#[derive(Clone, Debug)]
struct FeaturePreviewState {
    entries: Vec<FeaturePreviewEntry>,
    active_sketch: Option<usize>,
}

impl Default for FeaturePreviewState {
    fn default() -> Self {
        Self {
            entries: vec![
                FeaturePreviewEntry::fixed(FeaturePreviewKind::Origin),
                FeaturePreviewEntry::fixed(FeaturePreviewKind::BaseBody),
            ],
            active_sketch: None,
        }
    }
}

impl FeaturePreviewState {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn labels(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(FeaturePreviewEntry::label)
            .collect()
    }

    fn begin_new_sketch(&mut self) {
        self.active_sketch = None;
    }

    fn current_sketch_ordinal(&self) -> u32 {
        self.active_sketch
            .and_then(|index| self.entries.get(index))
            .filter(|entry| entry.kind == FeaturePreviewKind::Sketch)
            .map_or_else(
                || self.next_ordinal(FeaturePreviewKind::Sketch),
                |entry| entry.ordinal,
            )
    }

    fn commit_sketch_revision(&mut self, revision: u64) {
        if let Some(index) = self.active_sketch
            && let Some(entry) = self.entries.get_mut(index)
        {
            entry.revision = revision;
            entry.finished = false;
            return;
        }

        let ordinal = self.next_ordinal(FeaturePreviewKind::Sketch);
        self.entries.push(FeaturePreviewEntry {
            kind: FeaturePreviewKind::Sketch,
            ordinal,
            revision,
            finished: false,
            group: self
                .entries
                .iter()
                .map(|entry| entry.group)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
        });
        self.active_sketch = Some(self.entries.len() - 1);
    }

    /// Repairs the transient timeline after local sketch undo/redo. Undoing
    /// the first authored operation removes the provisional sketch chip;
    /// every other restore updates the existing chip in place.
    fn restore_sketch_revision(&mut self, revision: u64) {
        if revision == 0 {
            if let Some(index) = self.active_sketch.take()
                && self
                    .entries
                    .get(index)
                    .is_some_and(|entry| entry.kind == FeaturePreviewKind::Sketch)
            {
                self.entries.remove(index);
            }
        } else {
            self.commit_sketch_revision(revision);
        }
    }

    fn finish_active_sketch(&mut self) {
        if let Some(index) = self.active_sketch
            && let Some(entry) = self.entries.get_mut(index)
        {
            entry.finished = true;
        }
    }

    fn append(&mut self, kind: FeaturePreviewKind) {
        debug_assert!(matches!(
            kind,
            FeaturePreviewKind::Component
                | FeaturePreviewKind::Extrude
                | FeaturePreviewKind::Add
                | FeaturePreviewKind::Cut
                | FeaturePreviewKind::Transform
        ));
        self.entries.push(FeaturePreviewEntry {
            kind,
            ordinal: self.next_ordinal(kind),
            revision: 0,
            finished: true,
            group: self
                .active_sketch
                .and_then(|index| self.entries.get(index))
                .map_or_else(
                    || self.entries.last().map_or(0, |entry| entry.group),
                    |entry| entry.group,
                ),
        });
    }

    fn next_ordinal(&self, kind: FeaturePreviewKind) -> u32 {
        self.entries
            .iter()
            .filter(|entry| entry.kind == kind)
            .count() as u32
            + 1
    }
}

/// Conservative workbench classification for the first extrusion slice.
///
/// This is an early UX preflight only. The kernel independently repeats all
/// profile, resource, precision, and topology validation before publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SketchExtrusionEligibility {
    Ready,
    SketchNotFinished,
    RegionSelectionRequired { available: usize },
    InactiveHistorySketch,
    StaleFaceSupport,
    UnsupportedProfile,
    TooManyVertices { count: usize },
    TooManyRegions { count: usize },
    TooManyLoops { count: usize },
    TooManyCurves { count: usize },
    Concave,
    CollinearTurn,
    NumericallyIndeterminate,
    FaceRectangleRequired,
    ProfileOutsideSupport,
    BooleanUnionRequired,
}

impl SketchExtrusionEligibility {
    const fn can_stage(self) -> bool {
        matches!(self, Self::Ready)
    }

    fn visible_reason(self) -> Option<String> {
        match self {
            Self::Ready => None,
            Self::SketchNotFinished => {
                Some("Create and confirm one closed profile before starting an extrusion.".to_owned())
            }
            Self::RegionSelectionRequired { available } => Some(format!(
                "Extrusion requires an explicit profile selection · click inside one of the {available} bounded regions; Shift-click adds more."
            )),
            Self::InactiveHistorySketch => Some(
                "This sketch is suppressed or unavailable in the active history. Restore it before extruding."
                    .to_owned(),
            ),
            Self::StaleFaceSupport => Some(
                "This face sketch belongs to an earlier body state. Select a current face to create the next sketch; direct editing of historical sketches is not enabled yet."
                    .to_owned(),
            ),
            Self::UnsupportedProfile => Some(
                "Extrusion requires closed, non-intersecting regions whose exact line, arc, and circle wires can be certified."
                    .to_owned(),
            ),
            Self::TooManyVertices { count } => Some(format!(
                "Extrusion unavailable · this profile has {count} vertices; v0 supports at most {MAX_EXTRUSION_PROFILE_VERTICES}."
            )),
            Self::TooManyRegions { count } => Some(format!(
                "Extrusion unavailable · this sketch contains {count} material regions; the bounded request supports at most {MAX_PLANAR_PROFILE_REGIONS}."
            )),
            Self::TooManyLoops { count } => Some(format!(
                "Extrusion unavailable · this sketch contains {count} closed loops; the bounded request supports at most {MAX_PLANAR_PROFILE_LOOPS}."
            )),
            Self::TooManyCurves { count } => Some(format!(
                "Extrusion unavailable · this sketch contains {count} exact curves; the bounded request supports at most {MAX_PLANAR_PROFILE_CURVES}."
            )),
            Self::Concave => Some(
                "Extrusion unavailable · this profile has a concave turn; v0 requires strict convexity."
                    .to_owned(),
            ),
            Self::CollinearTurn => Some(
                "Extrusion unavailable · this profile has a collinear turn; remove intermediate straight-edge points."
                    .to_owned(),
            ),
            Self::NumericallyIndeterminate => Some(
                "Extrusion unavailable · the current numerical filters cannot certify every profile turn."
                    .to_owned(),
            ),
            Self::FaceRectangleRequired => Some(
                "The selected face boundary could not be certified as a supported simple planar region."
                    .to_owned(),
            ),
            Self::ProfileOutsideSupport => Some(
                "The feature loop must stay inside the selected face material and outside every face hole."
                    .to_owned(),
            ),
            Self::BooleanUnionRequired => Some(
                "The selected regions cross a face edge or hole. Rejoining those material islands is a same-body Boolean union, not a regular face extrusion; that exact merge is not implemented yet."
                    .to_owned(),
            ),
        }
    }

    const fn rejection_code(self) -> KernelErrorCode {
        match self {
            Self::TooManyVertices { .. }
            | Self::TooManyRegions { .. }
            | Self::TooManyLoops { .. }
            | Self::TooManyCurves { .. } => KernelErrorCode::ResourceLimitExceeded,
            Self::NumericallyIndeterminate => KernelErrorCode::NumericallyIndeterminate,
            Self::StaleFaceSupport => KernelErrorCode::StaleSnapshot,
            Self::Ready
            | Self::SketchNotFinished
            | Self::RegionSelectionRequired { .. }
            | Self::InactiveHistorySketch
            | Self::UnsupportedProfile
            | Self::Concave
            | Self::CollinearTurn
            | Self::FaceRectangleRequired
            | Self::ProfileOutsideSupport
            | Self::BooleanUnionRequired => KernelErrorCode::InvalidInput,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfirmationAction {
    Confirm,
    Cancel,
    /// One-click sketch completion from the idle sketch-mode rail.
    FinishSketch,
    /// Leave sketch mode without appending to the document.
    ExitSketch,
}

#[derive(Clone, Debug)]
enum Attempt {
    NotRun,
    Accepted {
        operation: &'static str,
    },
    Rejected {
        operation: &'static str,
        error: KernelError,
    },
}

#[derive(Clone)]
struct DisplayedBody {
    snapshot: Snapshot,
    report: OperationReport,
    scene: DebugScene,
}

#[derive(Clone)]
struct ArchivedBody {
    body: DisplayedBody,
    kind: ModelBodyKind,
}

#[derive(Clone)]
struct ArchivedFeatureReport {
    feature: FeatureId,
    association: SnapshotAssociation,
    report: OperationReport,
}

#[derive(Clone)]
struct WorkbenchBody {
    id: BodyId,
    last_feature: FeatureId,
    ordinal: u32,
    body: DisplayedBody,
    kind: ModelBodyKind,
    visible: bool,
    /// The assigned material's stable key. Bodies start unassigned so mass is
    /// never quoted from a default nobody chose.
    material: Option<String>,
}

#[derive(Clone, Debug)]
struct WorkbenchSketch {
    id: Option<SketchId>,
    feature: Option<FeatureId>,
    body: Option<BodyId>,
    ordinal: u32,
    support: SketchSupport,
    entities: Vec<SketchEntity>,
    /// Exact document-owned geometry used when this sketch came from a fresh
    /// process and has no editable canvas cache yet.
    portable_payload: Option<SketchPayload>,
    revision: u64,
    finished: bool,
    visible: bool,
    consumed: bool,
}

/// Complete unpublished UI projection built from a successfully replayed
/// native document. Publishing this value is one infallible state swap.
struct HydratedWorkbenchRuntime {
    document: ModelDocument,
    feature_reports: Vec<(FeatureId, OperationReport)>,
    feature_report_archive: Vec<ArchivedFeatureReport>,
    body_archive: Vec<ArchivedBody>,
    bodies: Vec<WorkbenchBody>,
    sketches: Vec<WorkbenchSketch>,
    bootstrap_body: Option<BodyId>,
}

/// Cached, face-local projection of the committed body shown behind a sketch.
///
/// This is presentation data only. The profile and every kernel command remain
/// bound to `SketchSupport` and the immutable committed snapshot.
#[derive(Clone, Debug)]
struct FaceSketchDisplayContext {
    fit_key: SketchContextFitKey,
    axis_labels: [&'static str; 2],
    triangles: Vec<SketchContextTriangle>,
    edges: Vec<SketchContextEdge>,
    boundary: Vec<SketchPoint>,
    inner_boundaries: Vec<Vec<SketchPoint>>,
    /// The support face's exact boundary curves, offered to sketch snapping.
    /// Unlike `edges`, these are analytic and never a chord approximation.
    snap_curves: Vec<SketchContextCurve>,
}

#[derive(Clone, Debug)]
struct PendingFaceSketch {
    support: PlanarFaceSupport,
    body: BodyId,
    snapshot: SnapshotId,
    context: Option<FaceSketchDisplayContext>,
    fitted_view: Option<SketchView>,
}

impl FaceSketchDisplayContext {
    fn viewport_context(&self) -> SketchViewportContext<'_> {
        SketchViewportContext::new(&self.triangles, &self.edges)
            .with_selected_face(&self.boundary, self.fit_key)
            .with_selected_face_inner_boundaries(&self.inner_boundaries)
            .with_snap_curves(&self.snap_curves)
            .with_axis_labels(self.axis_labels)
    }
}

/// State for the native Artificer workbench and kernel lab.
pub struct KernelLabApp {
    document: ModelDocument,
    document_path: PathBuf,
    document_path_text: String,
    document_settings: DocumentSettings,
    construction_planes: Vec<ConstructionPlane>,
    next_construction_plane_id: u64,
    selected_construction_plane: Option<u64>,
    document_properties_open: bool,
    stl_export_path_text: String,
    step_export_path_text: String,
    feature_reports: Vec<(FeatureId, OperationReport)>,
    feature_report_archive: Vec<ArchivedFeatureReport>,
    body_archive: Vec<ArchivedBody>,
    bootstrap_body: Option<BodyId>,
    selected_history_feature: Option<FeatureId>,
    history_scrub_position: usize,
    document_status: Option<String>,
    displayed: Option<DisplayedBody>,
    bodies: Vec<WorkbenchBody>,
    active_body_ordinal: u32,
    next_body_ordinal: u32,
    empty_snapshot: Snapshot,
    last_case: LabCase,
    last_attempt: Attempt,
    request_serial: u64,
    edge_overlay: bool,
    model_display_mode: viewport::ModelDisplayMode,
    selected_face: Option<EntityRef>,
    selected_edge: Option<viewport::DocumentEdgeSelection>,
    selected_vertex: Option<viewport::DocumentVertexSelection>,
    selected_faces: Vec<viewport::DocumentFaceSelection>,
    selected_edges: Vec<viewport::DocumentEdgeSelection>,
    selected_vertices: Vec<viewport::DocumentVertexSelection>,
    measured_edges: Vec<viewport::DocumentEdgeSelection>,
    measured_face: Option<viewport::DocumentFaceSelection>,
    /// Tool bodies picked while a Boolean is staged, in click order. Empty
    /// outside a staged Boolean; the target is never a member.
    boolean_tools: Vec<BodyId>,
    /// The sketch region and axis captured when a revolve was staged. It lives
    /// beside the pending operation rather than inside it because a profile is
    /// not `Copy`, exactly as the Boolean tool list does.
    staged_revolve: Option<StagedRevolve>,
    active_tool: ActiveTool,
    display_transform: DisplayTransform,
    pending_operation: Option<PendingOperation>,
    body_pivot: Option<Point3>,
    view: ViewState,
    motion: MotionState,
    last_motion_time: Option<f64>,
    face_camera_transition: Option<CameraTransition>,
    pending_face_sketch: Option<PendingFaceSketch>,
    last_face_camera_time: Option<f64>,
    animate_face_camera_transitions: bool,
    last_model_viewport_size: Option<egui::Vec2>,
    last_focused_editor: Option<egui::Id>,
    workbench_mode: WorkbenchMode,
    selected_origin_plane: SketchPlane,
    sketch_support: SketchSupport,
    face_sketch_context: Option<FaceSketchDisplayContext>,
    sketches: Vec<WorkbenchSketch>,
    active_sketch_index: Option<usize>,
    sketch: SketchCanvasState,
    sketch_toolbar: SketchToolbarState,
    active_sketch_tool: ToolVariant,
    sketch_revision: u64,
    sketch_finished: bool,
    sketch_last_error: Option<SketchEditError>,
    sketch_finish_issue: Option<CertifiedProfileStatus>,
    extrusion_distance: f64,
    extrusion_mode: ExtrusionMode,
    /// When false, signed face distance retains the convenient Add/Cut
    /// inference. Clicking an operation in Properties turns this on so the
    /// Boolean intent can change without moving or reversing the arrow.
    extrusion_mode_explicit: bool,
    extruded_sketch_revision: Option<u64>,
    sketch_extrusion_issue: Option<KernelError>,
    model_body_kind: ModelBodyKind,
    sketch_dimension_keys: DimensionKeyClaims,
    shell: WorkbenchShellState,
    catalog_store: Option<CatalogStore>,
    part_library: PartLibraryState,
    feature_preview: FeaturePreviewState,
    feature_preview_drag: viewport::FeaturePreviewDragState,
    model_edge_frame_memo: Option<viewport::EdgeFrameMemo>,
    display_detail_buckets: BTreeMap<u64, u8>,
    feature_preview_scheduler: Option<JobScheduler>,
    async_feature_preview_intent: Option<AsyncFeaturePreviewIntent>,
    async_feature_preview_job: Option<JobHandle<Option<viewport::FeaturePreview>>>,
    async_feature_preview_cache: Option<viewport::FeaturePreview>,
    async_edge_finish_preview_intent: Option<AsyncEdgeFinishPreviewIntent>,
    async_edge_finish_preview_job: Option<JobHandle<Option<viewport::EdgeFinishCandidatePreview>>>,
    async_edge_finish_preview_cache: Option<Arc<viewport::EdgeFinishCandidatePreview>>,
    async_sketch_extrusion_commit: Option<AsyncSketchExtrusionCommit>,
    development_recorder: Option<DevelopmentRecorder>,
    last_development_trace_fingerprint: Option<DevelopmentTraceFingerprint>,
    edge_finish_distance: f64,
    edge_finish_distance_text: String,
    edge_finish_tangent_chain: bool,
    /// Whether the floating context inspector is showing. It carries the
    /// active tool's options, so it opens with a command and stays until the
    /// user dismisses it.
    inspector_open: bool,
    /// The model camera as it stood before a plane sketch reframed it, so
    /// leaving the sketch hands the three-dimensional view back.
    camera_before_plane_sketch: Option<ViewState>,
    /// A plane sketch waiting for its camera flight to land. Opening the 2D
    /// canvas immediately would replace the very viewport the animation
    /// plays in, which reads as a snap even though the camera is flying.
    pending_plane_sketch: Option<PendingPlaneSketch>,
    /// While sketching, holding the right mouse button swaps the 2D canvas
    /// for the live 3D viewport so the sketch is visible in relation to the
    /// rest of the part; releasing flies the camera back onto the plane.
    sketch_orbit_peek: bool,
    /// The camera exactly as the sketch had it when the peek began, so the
    /// return flight restores the drawing view rather than recomputing an
    /// approximation of it.
    sketch_orbit_return_view: Option<ViewState>,
    /// The peek's return flight is in progress: the 3D view stays up until
    /// the camera lands back on the sketch plane, then the canvas returns.
    sketch_orbit_returning: bool,
}

impl Default for KernelLabApp {
    fn default() -> Self {
        let empty_snapshot = NativeKernel::empty();
        let document_path = default_document_path();
        let document_path_text = document_path.display().to_string();
        let stl_export_path_text = document_path.with_extension("stl").display().to_string();
        let step_export_path_text = document_path.with_extension("step").display().to_string();
        let mut part_library = PartLibraryState::default();
        if let Ok(package) = builtin_aluminium_extrusion_package() {
            part_library.set_definition_digest(package.content_digest().to_hex());
        }
        let mut app = Self {
            document: ModelDocument::default(),
            document_path,
            document_path_text,
            document_settings: DocumentSettings::default(),
            construction_planes: Vec::new(),
            next_construction_plane_id: 1,
            selected_construction_plane: None,
            document_properties_open: false,
            stl_export_path_text,
            step_export_path_text,
            feature_reports: Vec::new(),
            feature_report_archive: Vec::new(),
            body_archive: Vec::new(),
            bootstrap_body: None,
            selected_history_feature: None,
            history_scrub_position: 0,
            document_status: None,
            displayed: None,
            bodies: Vec::new(),
            active_body_ordinal: 1,
            next_body_ordinal: 2,
            empty_snapshot,
            last_case: LabCase::CanonicalCuboid,
            last_attempt: Attempt::NotRun,
            request_serial: 0,
            edge_overlay: true,
            model_display_mode: viewport::ModelDisplayMode::ShadedEdges,
            selected_face: None,
            selected_edge: None,
            selected_vertex: None,
            selected_faces: Vec::new(),
            selected_edges: Vec::new(),
            selected_vertices: Vec::new(),
            measured_edges: Vec::new(),
            measured_face: None,
            boolean_tools: Vec::new(),
            staged_revolve: None,
            active_tool: ActiveTool::Select,
            display_transform: DisplayTransform::default(),
            pending_operation: None,
            body_pivot: None,
            view: ViewState::default(),
            motion: MotionState::default(),
            last_motion_time: None,
            face_camera_transition: None,
            pending_face_sketch: None,
            last_face_camera_time: None,
            animate_face_camera_transitions: false,
            last_model_viewport_size: None,
            last_focused_editor: None,
            workbench_mode: WorkbenchMode::Model,
            selected_origin_plane: SketchPlane::XY,
            sketch_support: SketchSupport::default(),
            face_sketch_context: None,
            sketches: Vec::new(),
            active_sketch_index: None,
            sketch: SketchCanvasState::default(),
            sketch_toolbar: SketchToolbarState::default(),
            active_sketch_tool: ToolVariant::Select,
            sketch_revision: 0,
            sketch_finished: false,
            sketch_last_error: None,
            sketch_finish_issue: None,
            extrusion_distance: 4.0,
            extrusion_mode: ExtrusionMode::NewBody,
            extrusion_mode_explicit: false,
            extruded_sketch_revision: None,
            sketch_extrusion_issue: None,
            model_body_kind: ModelBodyKind::default(),
            sketch_dimension_keys: DimensionKeyClaims::default(),
            shell: WorkbenchShellState::default(),
            catalog_store: None,
            part_library,
            feature_preview: FeaturePreviewState::default(),
            feature_preview_drag: viewport::FeaturePreviewDragState::default(),
            model_edge_frame_memo: None,
            display_detail_buckets: BTreeMap::new(),
            feature_preview_scheduler: None,
            async_feature_preview_intent: None,
            async_feature_preview_job: None,
            async_feature_preview_cache: None,
            async_edge_finish_preview_intent: None,
            async_edge_finish_preview_job: None,
            async_edge_finish_preview_cache: None,
            async_sketch_extrusion_commit: None,
            development_recorder: None,
            last_development_trace_fingerprint: None,
            edge_finish_distance: 0.4,
            edge_finish_distance_text: "0.400".to_owned(),
            edge_finish_tangent_chain: false,
            inspector_open: true,
            camera_before_plane_sketch: None,
            pending_plane_sketch: None,
            sketch_orbit_peek: false,
            sketch_orbit_return_view: None,
            sketch_orbit_returning: false,
        };
        // Internal bootstrap is the sole non-interactive construction path.
        // Once the UI is live, every model mutation is staged first.
        app.execute_case(LabCase::CanonicalCuboid, None);
        app.initialize_document_from_displayed();
        app.history_scrub_position = app.document.history_position();
        app
    }
}

impl KernelLabApp {
    #[must_use]
    pub fn new(creation_context: &eframe::CreationContext<'_>) -> Self {
        install_style(&creation_context.egui_ctx);
        let mut app = Self {
            animate_face_camera_transitions: true,
            feature_preview_scheduler: Some(JobScheduler::new(2)),
            ..Self::default()
        };
        app.reset_to_blank_workspace();
        if let Err(error) = app.open_catalog_store(default_catalog_root()) {
            app.document_status = Some(format!(
                "Local Part Library is using its verified built-in fallback: {error}"
            ));
        }
        match DevelopmentRecorder::start_default() {
            Ok(recorder) => {
                eprintln!(
                    "Artificer development session log: {}",
                    recorder.session_path().display()
                );
                app.development_recorder = Some(recorder);
            }
            Err(error) => {
                eprintln!("Artificer could not start its local development log: {error}");
            }
        }
        app
    }

    /// Deterministic constructor for semantic and pixel tests.
    #[must_use]
    pub fn new_paused(creation_context: &eframe::CreationContext<'_>) -> Self {
        install_style(&creation_context.egui_ctx);
        Self::default()
    }

    /// Deterministic constructor that exercises the real persistent catalog
    /// without writing outside a caller-owned test or application directory.
    #[must_use]
    pub fn new_paused_with_catalog_root(
        creation_context: &eframe::CreationContext<'_>,
        root: impl AsRef<Path>,
    ) -> Self {
        install_style(&creation_context.egui_ctx);
        let mut app = Self::default();
        if let Err(error) = app.open_catalog_store(root) {
            app.document_status = Some(format!("Local Part Library failed to open: {error}"));
        }
        app
    }

    fn open_catalog_store(&mut self, root: impl AsRef<Path>) -> Result<(), String> {
        let package = builtin_aluminium_extrusion_package().map_err(|error| error.to_string())?;
        let digest = package.content_digest();
        let store =
            CatalogStore::open(root.as_ref().to_path_buf()).map_err(|error| error.to_string())?;
        store.publish(&package).map_err(|error| error.to_string())?;
        let rebuilt = store.rebuild_index().map_err(|error| error.to_string())?;
        if rebuilt.accepted() == 0 {
            return Err("the catalog contains no accepted definitions".into());
        }
        self.part_library.set_definition_digest(digest.to_hex());
        self.catalog_store = Some(store);
        self.document_status = Some(format!(
            "Local Part Library ready · {} verified definition(s)",
            rebuilt.accepted()
        ));
        Ok(())
    }

    #[must_use]
    pub const fn persistent_catalog_active(&self) -> bool {
        self.catalog_store.is_some()
    }

    #[must_use]
    pub fn catalog_entry_count(&self) -> usize {
        self.catalog_store
            .as_ref()
            .and_then(|store| store.index_snapshot().ok())
            .map_or(0, |index| index.len())
    }

    #[must_use]
    pub fn displayed_snapshot_id(&self) -> Option<SnapshotId> {
        self.displayed.as_ref().map(|body| body.snapshot.id())
    }

    /// Snapshot identity used by operations that may legitimately begin in a
    /// body-less document. A blank document is the canonical empty snapshot,
    /// not a missing or stale modeling state.
    fn active_snapshot_id_or_empty(&self) -> SnapshotId {
        self.displayed_snapshot_id()
            .unwrap_or_else(|| self.empty_snapshot.id())
    }

    #[must_use]
    pub fn body_count(&self) -> usize {
        self.bodies.len()
    }

    #[must_use]
    pub fn body_visible(&self, index: usize) -> bool {
        self.bodies.get(index).is_some_and(|body| body.visible)
    }

    #[must_use]
    pub fn sketch_count(&self) -> usize {
        self.sketches
            .iter()
            .filter(|sketch| {
                sketch
                    .id
                    .is_none_or(|id| self.document.sketch(id).is_some())
            })
            .count()
    }

    #[must_use]
    pub fn sketch_visible(&self, index: usize) -> bool {
        self.sketches
            .iter()
            .filter(|sketch| {
                sketch
                    .id
                    .is_none_or(|id| self.document.sketch(id).is_some())
            })
            .nth(index)
            .is_some_and(|sketch| sketch.visible)
    }

    #[must_use]
    pub fn visible_model_sketch_overlay_count(&self) -> usize {
        self.sketches
            .iter()
            .filter(|sketch| {
                sketch.visible
                    && workbench_sketch_has_overlay_geometry(sketch)
                    && sketch
                        .id
                        .is_none_or(|id| self.document.sketch(id).is_some())
            })
            .count()
    }

    #[must_use]
    pub fn document_feature_count(&self) -> usize {
        self.document.features().len()
    }

    #[must_use]
    pub fn component_instance_count(&self) -> usize {
        self.document.component_instances().len()
    }

    /// Stable occurrence IDs paired with canonical resolved-variant digests.
    #[must_use]
    pub fn component_variant_bindings(&self) -> Vec<(u64, String)> {
        self.document
            .component_instances()
            .iter()
            .map(|component| (component.id.get(), component.binding_digest.to_string()))
            .collect()
    }

    /// Stable component occurrence IDs and their committed rigid poses.
    #[must_use]
    pub fn component_poses(&self) -> Vec<(u64, [f64; 3], [f64; 4])> {
        self.document
            .component_instances()
            .iter()
            .map(|component| {
                let translation = component.pose.translation;
                let rotation = component.pose.rotation;
                (
                    component.id.get(),
                    [translation.x(), translation.y(), translation.z()],
                    [rotation.w(), rotation.x(), rotation.y(), rotation.z()],
                )
            })
            .collect()
    }

    #[must_use]
    pub fn active_component_instance_id(&self) -> Option<u64> {
        self.active_component_instance()
            .map(|component| component.id.get())
    }

    #[must_use]
    pub fn assembly_joint_count(&self) -> usize {
        self.document.joints().len()
    }

    #[must_use]
    pub fn assembly_joint_summaries(&self) -> Vec<(u64, String, u64, &'static str, bool)> {
        self.document
            .joints()
            .iter()
            .map(|joint| {
                (
                    joint.id.get(),
                    joint.name.clone(),
                    joint.child.get(),
                    match joint.kind {
                        JointKind::Fixed => "Fixed",
                        JointKind::Revolute { .. } => "Revolute",
                    },
                    joint.enabled,
                )
            })
            .collect()
    }

    #[must_use]
    pub const fn document_revision(&self) -> u64 {
        self.document.revision()
    }

    #[must_use]
    pub fn document_dirty_feature_count(&self) -> usize {
        self.document
            .features()
            .iter()
            .filter(|feature| feature.state.rebuild == RebuildState::Dirty)
            .count()
    }

    #[must_use]
    pub fn document_can_undo(&self) -> bool {
        self.document.can_undo()
    }

    #[must_use]
    pub fn document_can_redo(&self) -> bool {
        self.document.can_redo()
    }

    pub fn native_document_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&self.document).map_err(|error| error.to_string())
    }

    #[must_use]
    pub fn document_path(&self) -> &Path {
        &self.document_path
    }

    pub fn set_document_path(&mut self, path: impl Into<PathBuf>) {
        self.document_path = path.into();
        self.document_path_text = self.document_path.display().to_string();
    }

    #[must_use]
    pub const fn document_settings(&self) -> DocumentSettings {
        self.document_settings
    }

    pub fn set_display_length_unit(&mut self, unit: DisplayLengthUnit) {
        self.document_settings.length_unit = unit;
    }

    /// Portable Artificer workspace envelope. Unlike the raw model archive used
    /// by replay tests, this owns document presentation settings and is the
    /// public `.artificer` save format.
    pub fn workspace_document_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(&ArtificerWorkspaceFile {
            materials: self
                .bodies
                .iter()
                .filter_map(|body| {
                    body.material.as_ref().map(|material| BodyMaterial {
                        body: body.id.get(),
                        material: material.clone(),
                    })
                })
                .collect(),
            format: ARTIFICER_WORKSPACE_FORMAT.to_owned(),
            version: ARTIFICER_WORKSPACE_VERSION,
            settings: self.document_settings,
            construction_planes: self.construction_planes.clone(),
            document: self.document.clone(),
        })
        .map_err(|error| error.to_string())
    }

    /// The material assigned to a body, if any.
    #[must_use]
    pub fn body_material(&self, body: BodyId) -> Option<&'static material::Material> {
        self.bodies
            .iter()
            .find(|entry| entry.id == body)
            .and_then(|entry| entry.material.as_deref())
            .and_then(material::by_key)
    }

    /// Assigns or clears a body's material. Passing an unknown key clears it,
    /// so a stale workspace cannot leave a body quoting someone else's mass.
    pub fn set_body_material(&mut self, body: BodyId, key: Option<&str>) {
        let resolved = key
            .and_then(material::by_key)
            .map(|found| found.key.to_owned());
        if let Some(entry) = self.bodies.iter_mut().find(|entry| entry.id == body) {
            entry.material = resolved;
        }
    }

    /// Mass properties of every visible body, combining volume, mass, and the
    /// mass-weighted centre.
    #[must_use]
    pub fn mass_properties(&self) -> material::MassProperties {
        let mut accumulator = material::MassAccumulator::new();
        for body in self.bodies.iter().filter(|body| body.visible) {
            let measures = body.body.snapshot.measures();
            let placement = self.occurrence_transform_for_body(body.id);
            let centroid = measures.centroid.map(|centroid| {
                let placed = placement.transform_point(centroid);
                [placed.x, placed.y, placed.z]
            });
            let density = body
                .material
                .as_deref()
                .and_then(material::by_key)
                .map(|found| found.density);
            accumulator.add(measures.volume, centroid, density);
        }
        accumulator.finish()
    }

    pub fn save_workspace_to_path(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let json = self.workspace_document_json()?;
        save_bounded_json(path.as_ref(), &json, "Artificer workspace")
    }

    pub fn load_workspace_from_path(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let path = path.as_ref();
        let json = read_bounded_document(path)?;
        self.load_workspace_json(&json)?;
        self.set_document_path(path.to_path_buf());
        Ok(())
    }

    /// Loads the versioned workspace envelope and accepts legacy raw document
    /// JSON as a one-way migration path. Settings publish only after model
    /// replay succeeds, so a malformed file cannot partially mutate the app.
    pub fn load_workspace_json(&mut self, json: &str) -> Result<(), String> {
        let value = serde_json::from_str::<serde_json::Value>(json)
            .map_err(|error| format!("Artificer workspace is invalid: {error}"))?;
        if value.get("format").and_then(serde_json::Value::as_str)
            != Some(ARTIFICER_WORKSPACE_FORMAT)
        {
            return self.load_native_document_json(json);
        }
        let workspace = serde_json::from_value::<ArtificerWorkspaceFile>(value)
            .map_err(|error| format!("Artificer workspace is invalid: {error}"))?;
        debug_assert_eq!(workspace.format, ARTIFICER_WORKSPACE_FORMAT);
        if workspace.version != ARTIFICER_WORKSPACE_VERSION {
            return Err(format!(
                "unsupported Artificer workspace version {}; this build supports {}",
                workspace.version, ARTIFICER_WORKSPACE_VERSION
            ));
        }
        validate_construction_planes(&workspace.construction_planes)?;
        let document_json =
            serde_json::to_string(&workspace.document).map_err(|error| error.to_string())?;
        self.load_native_document_json(&document_json)?;
        self.document_settings = workspace.settings;
        self.construction_planes = workspace.construction_planes;
        self.next_construction_plane_id = self
            .construction_planes
            .iter()
            .map(|plane| plane.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        self.selected_construction_plane = None;
        // Restore material assignments by stable key. A key this build does
        // not carry leaves that body unassigned rather than substituting a
        // material the document never named.
        for assignment in &workspace.materials {
            if let Some(found) = material::by_key(&assignment.material)
                && let Some(body) = self
                    .bodies
                    .iter_mut()
                    .find(|body| body.id.get() == assignment.body)
            {
                body.material = Some(found.key.to_owned());
            }
        }
        self.rebind_construction_plane_sketch_supports();
        Ok(())
    }

    /// Writes one complete native document using a same-directory temporary
    /// file and atomic rename, so a failed write never truncates the last save.
    pub fn save_native_document_to_path(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let json = self.native_document_json()?;
        save_bounded_json(path.as_ref(), &json, "native document")
    }

    /// Reads and atomically hydrates one bounded, regular native document file.
    pub fn load_native_document_from_path(&mut self, path: impl AsRef<Path>) -> Result<(), String> {
        let json = read_bounded_document(path.as_ref())?;
        self.load_native_document_json(&json)
    }

    /// Atomically replaces the workspace from a native document archive.
    /// Kernel snapshots and reports are regenerated in a private stage; any
    /// parse, replay, persistent-reference, or provenance error leaves the
    /// current document and viewport untouched.
    pub fn load_native_document_json(&mut self, json: &str) -> Result<(), String> {
        if self.pending_operation.is_some() {
            return Err("confirm or cancel the pending operation before loading a document".into());
        }
        let original = serde_json::from_str::<ModelDocument>(json)
            .map_err(|error| format!("native document is invalid: {error}"))?;
        let saved_history_position = original.history_position();
        let mut replay_document = original.clone();
        if replay_document.history_position() != replay_document.features().len() {
            replay_document
                .set_history_position(replay_document.features().len())
                .map_err(|error| format!("history could not be prepared for replay: {error}"))?;
            replay_document.clear_undo_history();
        }
        let hydrated = hydrate_model_document(replay_document, HydrationOptions::default())
            .map_err(|error| error.to_string())?;
        let mut runtime = Self::project_hydrated_runtime(hydrated)?;
        runtime.document = original;
        self.publish_hydrated_runtime(runtime);
        if saved_history_position != self.document.features().len() {
            self.restore_runtime_from_document();
            self.history_scrub_position = saved_history_position;
            self.document_status = Some(format!(
                "Loaded native document at history position {saved_history_position} of {}",
                self.document.features().len()
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn displayed_semantic_digest(&self) -> Option<SemanticDigest> {
        self.displayed
            .as_ref()
            .map(|body| body.report.semantic_digest)
    }

    #[must_use]
    pub const fn edge_overlay_enabled(&self) -> bool {
        self.edge_overlay
    }

    #[must_use]
    pub const fn shaded_display_enabled(&self) -> bool {
        self.model_display_mode.is_shaded()
    }

    #[must_use]
    pub const fn selected_face(&self) -> Option<EntityRef> {
        self.selected_face
    }

    #[must_use]
    pub fn selected_face_role(&self) -> Option<FaceRole> {
        let selected = self.selected_face?;
        self.displayed
            .as_ref()?
            .scene
            .triangles
            .iter()
            .find(|triangle| triangle.source_face == selected)
            .map(|triangle| triangle.role)
    }

    #[must_use]
    pub fn last_error_code(&self) -> Option<KernelErrorCode> {
        match &self.last_attempt {
            Attempt::Rejected { error, .. } => Some(error.code),
            Attempt::NotRun | Attempt::Accepted { .. } => None,
        }
    }

    #[must_use]
    pub fn active_tool_label(&self) -> &'static str {
        self.active_tool.label()
    }

    #[must_use]
    pub const fn animation_playing(&self) -> bool {
        self.motion.playing
    }

    #[must_use]
    pub const fn animation_phase(&self) -> f64 {
        self.motion.phase
    }

    #[must_use]
    pub const fn reported_fps(&self) -> Option<f64> {
        self.motion.smoothed_fps
    }

    #[must_use]
    pub const fn transaction_attempt_count(&self) -> u64 {
        self.request_serial
    }

    #[must_use]
    pub const fn workbench_mode(&self) -> WorkbenchMode {
        self.workbench_mode
    }

    #[must_use]
    pub const fn shell_visibility(&self) -> WorkbenchShellVisibility {
        self.shell.visibility()
    }

    #[must_use]
    pub const fn part_library_open(&self) -> bool {
        self.part_library.is_open()
    }

    #[must_use]
    pub fn part_library_eligibility(&self) -> PartInsertionEligibility {
        self.part_library.eligibility()
    }

    #[must_use]
    pub fn staged_part_insertion(&self) -> Option<&PartInsertionIntent> {
        self.part_library.staged_intent()
    }

    #[must_use]
    pub fn committed_part_insertions(&self) -> &[PartInsertionIntent] {
        self.part_library.committed_intents()
    }

    /// Drains presentation-confirmed insertion requests for the component
    /// model adapter. The library shell itself never publishes geometry.
    pub fn drain_committed_part_insertions(&mut self) -> Vec<PartInsertionIntent> {
        self.part_library.drain_committed_intents()
    }

    #[must_use]
    pub fn feature_timeline_entries(&self) -> Vec<String> {
        self.feature_preview.labels()
    }

    #[must_use]
    pub fn history_position(&self) -> usize {
        self.document.history_position()
    }

    #[must_use]
    pub fn history_position_count(&self) -> usize {
        self.document.history_position_count()
    }

    #[must_use]
    pub const fn selected_origin_plane(&self) -> SketchPlane {
        self.selected_origin_plane
    }

    #[must_use]
    pub const fn sketch_plane(&self) -> SketchPlane {
        self.sketch.plane()
    }

    #[must_use]
    pub fn sketch_tool_label(&self) -> &'static str {
        self.sketch.tool().label()
    }

    #[must_use]
    pub fn sketch_entity_count(&self) -> usize {
        self.sketch.entities().len()
    }

    #[must_use]
    pub fn selected_sketch_region_count(&self) -> usize {
        self.sketch.selected_region_count()
    }

    #[must_use]
    pub fn available_sketch_region_count(&self) -> usize {
        self.sketch.available_region_count()
    }

    #[must_use]
    pub const fn sketch_revision(&self) -> u64 {
        self.sketch_revision
    }

    #[must_use]
    pub const fn sketch_finished(&self) -> bool {
        self.sketch_finished
    }

    #[must_use]
    pub fn sketch_profile_status(&self) -> CertifiedProfileStatus {
        self.sketch.certified_profile_status()
    }

    #[must_use]
    pub fn sketch_view_parameters(&self) -> (f64, f64, f64) {
        let view = self.sketch.view();
        (view.center.u, view.center.v, view.points_per_unit)
    }

    #[must_use]
    pub const fn sketch_view_quarter_turns(&self) -> u8 {
        self.sketch.view().quarter_turns
    }

    #[must_use]
    pub fn sketch_screen_axis_labels(&self) -> [&'static str; 2] {
        let axes = self.sketch_support.axis_labels();
        if self.sketch.view().quarter_turns.is_multiple_of(2) {
            axes
        } else {
            [axes[1], axes[0]]
        }
    }

    #[must_use]
    pub fn sketch_point_screen_position(
        &self,
        viewport: egui::Rect,
        point: SketchPoint,
    ) -> egui::Pos2 {
        self.sketch.view().sketch_to_screen(viewport, point)
    }

    #[must_use]
    pub const fn sketch_creation_draft_active(&self) -> bool {
        self.sketch.creation_draft_blocks_modeling()
    }

    #[must_use]
    pub fn sketch_dimension_readouts(&self) -> Vec<DimensionReadout> {
        self.sketch.dimension_readouts()
    }

    #[must_use]
    pub fn sketch_dimension_error(&self) -> Option<DimensionInputError> {
        self.sketch.dimension_error()
    }

    #[must_use]
    pub fn selected_sketch_recipe_editor(&self) -> Option<SelectedRecipeEditorView> {
        self.sketch.selected_recipe_editor()
    }

    #[must_use]
    pub fn selected_sketch_recipe_output_ids(&self) -> Vec<u64> {
        self.sketch.selected_recipe_output_ids()
    }

    #[must_use]
    pub fn sketch_pending_geometry(&self) -> Option<SketchGeometry> {
        self.sketch.pending_geometry()
    }

    #[must_use]
    pub fn sketch_pending_entity_count(&self) -> usize {
        self.sketch.pending_entity_count()
    }

    #[must_use]
    pub const fn extrusion_distance(&self) -> f64 {
        self.extrusion_distance
    }

    #[must_use]
    pub const fn extrusion_mode(&self) -> ExtrusionMode {
        self.extrusion_mode
    }

    #[must_use]
    pub const fn extrusion_mode_is_automatic(&self) -> bool {
        !self.extrusion_mode_explicit
    }

    fn signed_face_distance_context(&self) -> bool {
        matches!(self.sketch_support, SketchSupport::PlanarFace { .. })
            || matches!(
                self.pending_operation,
                Some(PendingOperation::PushPullFace { .. })
            )
            || (self.workbench_mode == WorkbenchMode::Model
                && self.selected_face_push_pull_support().is_some())
    }

    fn extrusion_distance_is_valid(&self) -> bool {
        self.extrusion_distance.is_finite()
            && if self.signed_face_distance_context() {
                self.extrusion_distance.abs() > f64::EPSILON
            } else {
                self.extrusion_distance > 0.0
            }
    }

    fn set_extrusion_distance_intent(&mut self, distance: f64) {
        self.extrusion_distance = distance;
        if self.signed_face_distance_context() {
            if !self.extrusion_mode_explicit {
                if distance > 0.0 {
                    self.extrusion_mode = ExtrusionMode::Add;
                } else if distance < 0.0 {
                    self.extrusion_mode = ExtrusionMode::Cut;
                }
            }
        } else {
            self.extrusion_mode_explicit = false;
            self.extrusion_mode = ExtrusionMode::NewBody;
        }
    }

    fn select_extrusion_mode(&mut self, mode: ExtrusionMode) {
        self.extrusion_mode = mode;
        self.extrusion_mode_explicit = matches!(mode, ExtrusionMode::Add | ExtrusionMode::Cut)
            && self.signed_face_distance_context();
        if mode == ExtrusionMode::NewBody && self.extrusion_distance <= 0.0 {
            self.extrusion_distance = self.extrusion_distance.abs().max(1.0);
        }
    }

    fn select_automatic_extrusion_mode(&mut self) {
        self.extrusion_mode_explicit = false;
        self.set_extrusion_distance_intent(self.extrusion_distance);
    }

    #[must_use]
    pub fn sketch_support_label(&self) -> String {
        self.sketch_support.label()
    }

    #[must_use]
    pub const fn sketch_is_face_supported(&self) -> bool {
        matches!(self.sketch_support, SketchSupport::PlanarFace { .. })
    }

    fn sketch_support_is_current(&self) -> bool {
        match &self.sketch_support {
            SketchSupport::Origin { .. } => true,
            SketchSupport::ConstructionPlane { id, frame } => id.map_or_else(
                || {
                    self.construction_planes
                        .iter()
                        .any(|plane| plane.frame == **frame)
                },
                |id| {
                    self.construction_planes
                        .iter()
                        .any(|plane| plane.id == id && plane.frame == **frame)
                },
            ),
            SketchSupport::PlanarFace { body, snapshot, .. } => {
                Some(*body) == self.active_body_id()
                    && Some(*snapshot) == self.displayed_snapshot_id()
            }
        }
    }

    fn rebind_construction_plane_sketch_supports(&mut self) {
        let resolve = |frame: PlanarFrame3| {
            self.construction_planes
                .iter()
                .find(|plane| plane.frame == frame)
                .map(|plane| plane.id)
        };
        for sketch in &mut self.sketches {
            if let SketchSupport::ConstructionPlane { id, frame } = &mut sketch.support {
                *id = resolve(**frame);
            }
        }
        if let SketchSupport::ConstructionPlane { id, frame } = &mut self.sketch_support {
            *id = resolve(**frame);
        }
    }

    fn active_document_sketch_is_available(&self) -> bool {
        let Some(index) = self.active_sketch_index else {
            return true;
        };
        let Some(record) = self.sketches.get(index) else {
            return false;
        };
        match (record.id, record.feature) {
            (None, None) => true,
            (Some(sketch), Some(feature)) => {
                self.document.sketch(sketch).is_some()
                    && self.document.feature_is_active(feature).unwrap_or(false)
                    && self.document.feature(feature).is_some_and(|node| {
                        node.committed.is_some()
                            && !node.state.suppressed
                            && node.state.rebuild == RebuildState::Clean
                    })
            }
            _ => false,
        }
    }

    /// Number of committed body primitives projected behind the active face sketch.
    #[must_use]
    pub fn face_sketch_context_counts(&self) -> Option<(usize, usize)> {
        self.face_sketch_context
            .as_ref()
            .map(|context| (context.triangles.len(), context.edges.len()))
    }

    /// Analytic support curves the active face sketch offers to snapping.
    #[must_use]
    pub fn face_sketch_snap_curves(&self) -> &[SketchContextCurve] {
        self.face_sketch_context
            .as_ref()
            .map_or(&[], |context| context.snap_curves.as_slice())
    }

    #[must_use]
    pub fn sketch_extrusion_eligibility(&self) -> SketchExtrusionEligibility {
        if !self.sketch_support_is_current() {
            return SketchExtrusionEligibility::StaleFaceSupport;
        }
        if !self.active_document_sketch_is_available() {
            return SketchExtrusionEligibility::InactiveHistorySketch;
        }
        if let Some(payload) = self
            .active_sketch_index
            .and_then(|index| self.sketches.get(index))
            .and_then(|sketch| sketch.portable_payload.as_ref())
            .filter(|_| self.sketch.authoring().operations().is_empty())
        {
            if payload.profile.regions.is_empty() {
                return SketchExtrusionEligibility::SketchNotFinished;
            }
            if payload.profile.regions.len() > MAX_PLANAR_PROFILE_REGIONS {
                return SketchExtrusionEligibility::TooManyRegions {
                    count: payload.profile.regions.len(),
                };
            }
            if payload.profile.loop_count() > MAX_PLANAR_PROFILE_LOOPS {
                return SketchExtrusionEligibility::TooManyLoops {
                    count: payload.profile.loop_count(),
                };
            }
            if payload.profile.curve_count() > MAX_PLANAR_PROFILE_CURVES {
                return SketchExtrusionEligibility::TooManyCurves {
                    count: payload.profile.curve_count(),
                };
            }
            // Loaded exact profiles have already crossed the document's v4
            // validation boundary. Preserve only the cheap exact containment
            // checks for one simple loop; every complex profile proceeds to
            // the native kernel's authoritative topology certification.
            return classify_selected_planar_profile(&payload.profile, &self.sketch_support);
        }

        // Exact bounded cells are the primary modeling contract for editable
        // v6 sketches. A multi-cell arrangement must never silently fall back
        // to the old whole-sketch profile heuristic: the user-selected union
        // is the extrusion payload.
        let available_regions = self.sketch.available_region_count();
        if available_regions > 0 {
            if self.sketch.selected_region_count() == 0 {
                return SketchExtrusionEligibility::RegionSelectionRequired {
                    available: available_regions,
                };
            }
            let Some(profile) = self.sketch.selected_planar_profile() else {
                return SketchExtrusionEligibility::UnsupportedProfile;
            };
            return classify_selected_planar_profile(&profile, &self.sketch_support);
        }

        let profile = self.sketch.certified_profile_status();
        if !profile.can_finish() {
            if let Some(compiled) = compile_single_authoring_region(self.sketch.authoring()) {
                if compiled.regions.len() > MAX_PLANAR_PROFILE_REGIONS {
                    return SketchExtrusionEligibility::TooManyRegions {
                        count: compiled.regions.len(),
                    };
                }
                if compiled.loop_count() > MAX_PLANAR_PROFILE_LOOPS {
                    return SketchExtrusionEligibility::TooManyLoops {
                        count: compiled.loop_count(),
                    };
                }
                if compiled.curve_count() > MAX_PLANAR_PROFILE_CURVES {
                    return SketchExtrusionEligibility::TooManyCurves {
                        count: compiled.curve_count(),
                    };
                }
                // A closed arrangement cell remains extrudable even when the
                // same sketch also carries unrelated open or construction
                // geometry. The kernel performs final face-domain checks.
                return classify_selected_planar_profile(&compiled, &self.sketch_support);
            }
            return match profile {
                CertifiedProfileStatus::Empty | CertifiedProfileStatus::Open => {
                    SketchExtrusionEligibility::SketchNotFinished
                }
                CertifiedProfileStatus::Indeterminate => {
                    SketchExtrusionEligibility::NumericallyIndeterminate
                }
                CertifiedProfileStatus::TooManyCurves { count } => {
                    SketchExtrusionEligibility::TooManyCurves { count }
                }
                CertifiedProfileStatus::TooManyLoops { count } => {
                    SketchExtrusionEligibility::TooManyLoops { count }
                }
                CertifiedProfileStatus::TooManyRegions { count } => {
                    SketchExtrusionEligibility::TooManyRegions { count }
                }
                CertifiedProfileStatus::LinearLoopTooLarge { count } => {
                    SketchExtrusionEligibility::TooManyVertices { count }
                }
                _ => SketchExtrusionEligibility::UnsupportedProfile,
            };
        }
        let Some(profile) = self.sketch.certified_sketch_profile() else {
            return SketchExtrusionEligibility::UnsupportedProfile;
        };
        if profile.regions.len() > MAX_PLANAR_PROFILE_REGIONS {
            return SketchExtrusionEligibility::TooManyRegions {
                count: profile.regions.len(),
            };
        }
        if profile.loop_count() > MAX_PLANAR_PROFILE_LOOPS {
            return SketchExtrusionEligibility::TooManyLoops {
                count: profile.loop_count(),
            };
        }
        if profile.curve_count() > MAX_PLANAR_PROFILE_CURVES {
            return SketchExtrusionEligibility::TooManyCurves {
                count: profile.curve_count(),
            };
        }
        let SketchSupport::PlanarFace {
            boundary,
            inner_boundaries,
            ..
        } = &self.sketch_support
        else {
            return SketchExtrusionEligibility::Ready;
        };
        if let [region] = profile.regions.as_slice() {
            if region.holes.is_empty()
                && let [CertifiedSketchCurve::Circle { center, rim, .. }] =
                    region.outer.curves.as_slice()
            {
                return classify_face_circle_domain(*center, *rim, boundary, inner_boundaries);
            }
            // The fast UI preflight remains useful for the common unholed
            // linear loop. Mixed curves, holes, and multi-region profiles are
            // deliberately delegated to the unified kernel implementation.
            if !profile.has_analytic_curves()
                && let Some(linear) = profile.linear_regions()
                && let [region] = linear.as_slice()
                && region.holes.is_empty()
            {
                return classify_face_profile_domain(&region.outer, boundary, inner_boundaries);
            }
        }
        SketchExtrusionEligibility::Ready
    }

    #[must_use]
    pub const fn extruded_sketch_revision(&self) -> Option<u64> {
        self.extruded_sketch_revision
    }

    #[must_use]
    pub fn displayed_measures(&self) -> Option<SnapshotMeasures> {
        self.displayed.as_ref().map(|body| body.snapshot.measures())
    }

    /// Exact committed B-rep counts exposed for semantic workbench checks.
    #[must_use]
    pub fn displayed_topology_counts(&self) -> Option<TopologyCounts> {
        self.displayed.as_ref().map(|body| body.snapshot.counts())
    }

    #[must_use]
    pub const fn displayed_transform(&self) -> ([f64; 3], [f64; 3], f64) {
        (
            self.display_transform.translation,
            self.display_transform.rotation,
            self.display_transform.scale,
        )
    }

    #[must_use]
    pub fn transform_preview_pending(&self) -> bool {
        matches!(
            self.pending_operation,
            Some(PendingOperation::Transform { .. } | PendingOperation::ComponentPlacement { .. })
        )
    }

    #[must_use]
    pub fn transform_preview_base(&self) -> Option<SnapshotId> {
        match self.pending_operation {
            Some(PendingOperation::Transform { base_snapshot }) => Some(base_snapshot),
            Some(
                PendingOperation::ComponentPlacement { .. }
                | PendingOperation::SetComponentGrounded { .. }
                | PendingOperation::CreateRevoluteJoint { .. }
                | PendingOperation::RunCase { .. }
                | PendingOperation::LibraryInsertion { .. }
                | PendingOperation::LoadDefaultDocument
                | PendingOperation::SetParameterLiteral { .. }
                | PendingOperation::AddUserLengthParameter { .. }
                | PendingOperation::CreateConstructionPlane { .. }
                | PendingOperation::BooleanBodies { .. }
                | PendingOperation::PresetFeature { .. }
                | PendingOperation::SketchEdit { .. }
                | PendingOperation::FinishSketch { .. }
                | PendingOperation::ExtrudeSketch { .. }
                | PendingOperation::PushPullFace { .. },
            )
            | None => None,
        }
    }

    #[must_use]
    pub const fn operation_confirmation_pending(&self) -> bool {
        self.pending_operation.is_some()
    }

    #[must_use]
    pub fn pending_operation_label(&self) -> Option<&'static str> {
        self.pending_operation.map(PendingOperation::title)
    }

    fn history_is_at_end(&self) -> bool {
        self.document.history_position() == self.document.features().len()
    }

    fn transform_tools_available(&self) -> bool {
        let compatible_pending = matches!(
            self.pending_operation,
            None | Some(
                PendingOperation::Transform { .. } | PendingOperation::ComponentPlacement { .. }
            )
        );
        let active_component_movable = self.active_component_instance().is_none_or(|component| {
            !component.grounded && self.document.joint_for_child(component.id).is_none()
        });
        self.history_is_at_end() && compatible_pending && active_component_movable
    }

    fn scale_tool_available(&self) -> bool {
        self.transform_tools_available() && self.active_component_instance().is_none()
    }

    #[must_use]
    pub const fn view_parameters(&self) -> (f64, f64, f64) {
        (self.view.yaw, self.view.pitch, self.view.zoom)
    }

    #[must_use]
    pub const fn view_frame(&self) -> (Point3, f64) {
        (self.view.target(), self.view.fit_radius())
    }

    #[must_use]
    pub const fn face_camera_transition_active(&self) -> bool {
        self.face_camera_transition.is_some()
    }

    pub fn set_animation_phase(&mut self, phase: f64) {
        if phase.is_finite() {
            self.motion.phase = phase.rem_euclid(std::f64::consts::TAU);
        }
    }

    pub fn set_animation_playing(&mut self, playing: bool) {
        if playing {
            self.motion.play();
        } else {
            self.motion.pause();
        }
        self.last_motion_time = None;
    }

    /// Enables or disables the timed face-focus camera move. Deterministic
    /// tests default to instant transitions; timing-sensitive tests opt back
    /// into the production 340 ms animation explicitly.
    pub fn set_face_camera_animation(&mut self, animate: bool) {
        self.animate_face_camera_transitions = animate;
    }

    /// Production documents begin with reference planes and no unsolicited
    /// solid. `Default` deliberately retains the canonical cuboid fixture used
    /// by deterministic kernel/UI tests.
    fn reset_to_blank_workspace(&mut self) {
        let empty = SnapshotAssociation::new(
            self.empty_snapshot.id(),
            self.empty_snapshot.id(),
            self.empty_snapshot.semantic_digest(),
        );
        let mut document = ModelDocument::default();
        document
            .append_feature(
                FeatureDraft::new(FeatureKind::Origin, "Origin", ReplayAction::Marker)
                    .with_commit(empty)
                    .read_only(true),
            )
            .expect("the built-in origin document node is valid");
        document.clear_undo_history();
        self.document = document;
        self.feature_reports.clear();
        self.feature_report_archive.clear();
        self.body_archive.clear();
        self.bootstrap_body = None;
        self.construction_planes.clear();
        self.next_construction_plane_id = 1;
        self.selected_construction_plane = None;
        self.displayed = None;
        self.bodies.clear();
        self.sketches.clear();
        self.active_body_ordinal = 1;
        self.next_body_ordinal = 1;
        self.selected_history_feature = self.document.features().first().map(|node| node.id);
        self.history_scrub_position = self.document.history_position();
        self.clear_model_entity_selection();
        self.body_pivot = None;
        self.sketch = SketchCanvasState::default();
        self.sketch_support = SketchSupport::default();
        self.active_sketch_index = None;
        self.sketch_revision = 0;
        self.sketch_finished = false;
        self.pending_operation = None;
        self.request_serial = 0;
        self.last_attempt = Attempt::NotRun;
        self.feature_preview = FeaturePreviewState::default();
        self.sync_feature_preview_from_document();
        self.document_status =
            Some("Blank document ready · choose a plane or create a datum plane".into());
        self.frame_visible_document();
    }

    fn initialize_document_from_displayed(&mut self) {
        let Some(displayed) = self.displayed.clone() else {
            return;
        };
        let empty = SnapshotAssociation::new(
            self.empty_snapshot.id(),
            self.empty_snapshot.id(),
            self.empty_snapshot.semantic_digest(),
        );
        let mut document = ModelDocument::default();
        let origin = document
            .append_feature(
                FeatureDraft::new(FeatureKind::Origin, "Origin", ReplayAction::Marker)
                    .with_commit(empty)
                    .read_only(true),
            )
            .expect("the built-in origin document node is valid")
            .feature;
        let base_commit = SnapshotAssociation::new(
            displayed.report.input_snapshot,
            displayed.snapshot.id(),
            displayed.snapshot.semantic_digest(),
        );
        let base = document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Base body",
                    ReplayAction::Kernel(KernelCommand::MakeCuboid {
                        origin: Point3::new(0.0, 0.0, 0.0),
                        size_x: 2.0,
                        size_y: 3.0,
                        size_z: 4.0,
                    }),
                )
                .with_input(FeatureInput::Feature(origin))
                .with_output(OutputDraft::CreateBody {
                    label: "Body 1".to_owned(),
                })
                .with_commit(base_commit),
            )
            .expect("the bootstrapped cuboid document node is valid");
        let body_id = *base
            .created_bodies
            .first()
            .expect("the base-body feature creates one body");
        document.clear_undo_history();
        self.document = document;
        self.history_scrub_position = self.document.history_position();
        self.feature_reports.clear();
        self.feature_report_archive.clear();
        self.archive_feature_report(base.feature, displayed.report.clone());
        self.body_archive = vec![ArchivedBody {
            body: displayed.clone(),
            kind: ModelBodyKind::Cuboid,
        }];
        self.bootstrap_body = Some(body_id);
        self.selected_history_feature = Some(base.feature);
        self.document_status = Some("Parametric document ready".to_owned());
        self.bodies.clear();
        self.active_body_ordinal = 1;
        self.next_body_ordinal = 2;
        self.bodies.push(WorkbenchBody {
            material: None,
            id: body_id,
            last_feature: base.feature,
            ordinal: 1,
            body: displayed,
            kind: self.model_body_kind,
            visible: true,
        });
    }

    fn project_hydrated_runtime(
        hydrated: HydratedDocument,
    ) -> Result<HydratedWorkbenchRuntime, String> {
        let document = hydrated.document;
        let mut feature_reports = Vec::new();
        let mut feature_report_archive = Vec::new();
        let mut body_archive = Vec::new();
        let mut body_kinds = std::collections::BTreeMap::<BodyId, ModelBodyKind>::new();

        for result in &hydrated.features {
            let Some(report) = result.report.as_ref() else {
                continue;
            };
            let feature = document
                .feature(result.feature)
                .ok_or_else(|| format!("hydration returned unknown feature {}", result.feature))?;
            let previous_kind = result
                .branches
                .first()
                .and_then(|body| body_kinds.get(body))
                .copied()
                .unwrap_or(ModelBodyKind::Cuboid);
            let push_pull = matches!(
                &feature.action,
                ReplayAction::TargetedKernel(targeted)
                    if matches!(targeted.command_template(), KernelCommand::PushPullFace { .. })
            );
            let kind = match feature.kind {
                FeatureKind::BaseBody if feature.component_instance.is_some() => {
                    ModelBodyKind::SketchExtrusion
                }
                FeatureKind::BaseBody => ModelBodyKind::Cuboid,
                FeatureKind::Extrude => ModelBodyKind::SketchExtrusion,
                FeatureKind::Add if push_pull => ModelBodyKind::PushedPulled,
                FeatureKind::Cut if push_pull => ModelBodyKind::PushedPulled,
                FeatureKind::Add => ModelBodyKind::AddedBoss,
                FeatureKind::Cut => ModelBodyKind::CutPocket,
                FeatureKind::Transform => previous_kind,
                FeatureKind::Boolean => ModelBodyKind::Boolean,
                FeatureKind::Origin | FeatureKind::DatumPlane | FeatureKind::Sketch => {
                    previous_kind
                }
            };
            for body in &result.branches {
                body_kinds.insert(*body, kind);
            }
            let snapshot = hydrated
                .snapshots
                .get(&result.association.output)
                .ok_or_else(|| {
                    format!(
                        "hydration omitted feature {} snapshot {}",
                        result.feature, result.association.output
                    )
                })?
                .clone();
            let displayed = DisplayedBody {
                scene: NativeKernel::debug_scene(&snapshot),
                snapshot,
                report: report.clone(),
            };
            feature_reports.push((result.feature, report.clone()));
            feature_report_archive.push(ArchivedFeatureReport {
                feature: result.feature,
                association: result.association,
                report: report.clone(),
            });
            body_archive.push(ArchivedBody {
                body: displayed,
                kind,
            });
        }

        let bootstrap_body = document.features().iter().find_map(|feature| {
            (feature.kind == FeatureKind::BaseBody && feature.component_instance.is_none())
                .then(|| {
                    feature.outputs.iter().find_map(|output| match output {
                        FeatureOutput::Body(body) => Some(*body),
                        FeatureOutput::Sketch { .. } => None,
                    })
                })
                .flatten()
        });
        let bootstrap_replaced = bootstrap_body.is_some()
            && document.features().iter().any(|feature| {
                feature.kind == FeatureKind::Extrude
                    && feature.outputs.iter().any(|output| {
                        matches!(output, FeatureOutput::Body(body) if Some(*body) != bootstrap_body)
                    })
            });

        let mut bodies = Vec::new();
        for record in document.bodies() {
            if bootstrap_replaced && Some(record.id) == bootstrap_body {
                continue;
            }
            let Some(head) = hydrated.branch_heads.get(&record.id).copied() else {
                continue;
            };
            let snapshot = hydrated
                .snapshots
                .get(&head)
                .ok_or_else(|| format!("body {} snapshot {head} is unavailable", record.id))?
                .clone();
            let result = hydrated
                .features
                .iter()
                .rev()
                .find(|result| {
                    result.branches.contains(&record.id)
                        && result.report.is_some()
                        && result.association.output == head
                })
                .ok_or_else(|| format!("body {} has no regenerated operation report", record.id))?;
            let report = result
                .report
                .as_ref()
                .expect("the hydrated result was filtered to operation reports")
                .clone();
            let component_visible = document
                .component_instances()
                .iter()
                .find(|component| component.bodies.contains(&record.id))
                .is_none_or(|component| component.visible && !component.suppressed);
            let ordinal = bodies.len() as u32 + 1;
            bodies.push(WorkbenchBody {
                material: None,
                id: record.id,
                last_feature: record.last_feature,
                ordinal,
                body: DisplayedBody {
                    scene: NativeKernel::debug_scene(&snapshot),
                    snapshot,
                    report,
                },
                kind: body_kinds
                    .get(&record.id)
                    .copied()
                    .unwrap_or(ModelBodyKind::Cuboid),
                visible: record.visible && component_visible,
            });
        }

        let ordered_reports = feature_reports
            .iter()
            .map(|(feature, report)| FeatureOperationReport::new(*feature, report))
            .collect::<Vec<_>>();
        let skipped = hydrated
            .skipped
            .iter()
            .map(|skip| skip.feature)
            .collect::<std::collections::BTreeSet<_>>();
        let mut sketches = Vec::new();
        for record in document.sketches() {
            let feature_active = document
                .feature_is_active(record.created_by)
                .unwrap_or(false)
                && !skipped.contains(&record.created_by)
                && document.feature(record.created_by).is_some_and(|feature| {
                    !feature.state.suppressed && feature.state.rebuild == RebuildState::Clean
                });
            if !feature_active {
                continue;
            }
            let payload = document
                .sketch_payload(record.id, record.geometry_revision)
                .cloned();
            let support = match payload.as_ref().map(|payload| &payload.support) {
                Some(SketchSupportRecipe::Origin) => {
                    let plane = sketch_plane_for_frame(
                        payload
                            .as_ref()
                            .expect("the support came from a payload")
                            .frame,
                    );
                    if sketch_plane_frame(plane)
                        != payload
                            .as_ref()
                            .expect("the support came from a payload")
                            .frame
                    {
                        SketchSupport::ConstructionPlane {
                            id: None,
                            frame: Box::new(
                                payload
                                    .as_ref()
                                    .expect("the support came from a payload")
                                    .frame,
                            ),
                        }
                    } else {
                        SketchSupport::Origin { plane }
                    }
                }
                Some(SketchSupportRecipe::PlanarFace { body, face }) => {
                    let head = hydrated.branch_heads.get(body).copied().ok_or_else(|| {
                        format!("sketch {} support body {} is unavailable", record.id, body)
                    })?;
                    let resolved = match resolve_persistent_ref(face, &ordered_reports, head) {
                        PersistentResolution::Resolved(face) => face,
                        PersistentResolution::Missing(missing) => {
                            return Err(format!(
                                "sketch {} support face is missing: {:?}",
                                record.id, missing.reason
                            ));
                        }
                        PersistentResolution::Ambiguous(ambiguity) => {
                            return Err(format!(
                                "sketch {} support face is ambiguous: {:?}",
                                record.id, ambiguity.reason
                            ));
                        }
                    };
                    let snapshot = hydrated.snapshots.get(&head).ok_or_else(|| {
                        format!("sketch {} support snapshot is unavailable", record.id)
                    })?;
                    let native = NativeKernel::planar_face_support(snapshot, resolved)
                        .map_err(|error| format!("sketch {} support failed: {error}", record.id))?;
                    if native.frame
                        != payload
                            .as_ref()
                            .expect("the support came from a payload")
                            .frame
                    {
                        return Err(format!(
                            "sketch {} support frame changed during replay",
                            record.id
                        ));
                    }
                    SketchSupport::PlanarFace {
                        body: *body,
                        snapshot: head,
                        face: native.face,
                        frame: Box::new(native.frame),
                        boundary: native.boundary,
                        inner_boundaries: native.inner_boundaries,
                        support_digest: native.support_digest,
                    }
                }
                None if record.support_body.is_none() => SketchSupport::Origin {
                    plane: SketchPlane::XY,
                },
                None => continue,
            };
            let consumed = document.features().iter().any(|feature| {
                document.feature_is_active(feature.id).unwrap_or(false)
                    && feature.committed.is_some()
                    && !feature.state.suppressed
                    && matches!(
                        feature.kind,
                        FeatureKind::Extrude | FeatureKind::Add | FeatureKind::Cut
                    )
                    && feature.inputs.contains(&FeatureInput::Sketch(record.id))
            });
            let auto_hidden_consumer_active = record.auto_hidden_by.is_some_and(|consumer| {
                document.feature_is_active(consumer).unwrap_or(false)
                    && document.feature(consumer).is_some_and(|feature| {
                        feature.committed.is_some() && !feature.state.suppressed
                    })
            });
            sketches.push(WorkbenchSketch {
                id: Some(record.id),
                feature: Some(record.last_feature),
                body: record.support_body,
                ordinal: sketches.len() as u32 + 1,
                support,
                entities: Vec::new(),
                portable_payload: payload,
                revision: record.geometry_revision,
                finished: true,
                visible: record.visible
                    || record.auto_hidden_by.is_some() && !auto_hidden_consumer_active,
                consumed,
            });
        }

        Ok(HydratedWorkbenchRuntime {
            document,
            feature_reports,
            feature_report_archive,
            body_archive,
            bodies,
            sketches,
            bootstrap_body,
        })
    }

    fn hydrate_sketch_canvas(
        plane: SketchPlane,
        payload: &SketchPayload,
    ) -> Result<Option<SketchCanvasState>, SketchEditError> {
        let Some(authoring) = payload.authoring().cloned() else {
            return Ok(None);
        };
        if payload.profile.regions.is_empty() {
            return SketchCanvasState::from_authoring(plane, authoring).map(Some);
        }
        let selected_regions =
            authoring_region_signatures_for_profile(&authoring, &payload.profile)
                .ok_or(SketchEditError::AuthoringRejected)?;
        SketchCanvasState::from_authoring_with_regions(plane, authoring, &selected_regions)
            .map(Some)
    }

    fn publish_hydrated_runtime(&mut self, runtime: HydratedWorkbenchRuntime) {
        self.document = runtime.document;
        self.feature_reports = runtime.feature_reports;
        self.feature_report_archive = runtime.feature_report_archive;
        self.body_archive = runtime.body_archive;
        self.bodies = runtime.bodies;
        self.sketches = runtime.sketches;
        self.bootstrap_body = runtime.bootstrap_body;
        self.next_body_ordinal = self.bodies.len() as u32 + 1;
        self.active_sketch_index = self
            .sketches
            .iter()
            .rposition(|sketch| !sketch.consumed && sketch.portable_payload.is_some());

        let preferred_body = self
            .active_sketch_index
            .and_then(|index| self.sketches.get(index))
            .and_then(|sketch| sketch.body);
        let active_body_index = preferred_body
            .and_then(|body| {
                self.bodies
                    .iter()
                    .position(|candidate| candidate.id == body)
            })
            .or_else(|| self.bodies.len().checked_sub(1));
        if let Some(index) = active_body_index {
            let body = self.bodies[index].clone();
            self.active_body_ordinal = body.ordinal;
            self.displayed = Some(body.body.clone());
            self.model_body_kind = body.kind;
            self.body_pivot = self.committed_world_pivot_for_body(&body);
        } else {
            self.active_body_ordinal = 1;
            self.displayed = None;
            self.body_pivot = None;
            self.model_body_kind = ModelBodyKind::Cuboid;
        }

        if let Some(index) = self.active_sketch_index {
            let sketch = self.sketches[index].clone();
            self.sketch_support = sketch.support;
            self.sketch_revision = sketch.revision;
            self.sketch_finished = true;
            self.selected_origin_plane = sketch_plane_for_frame(self.sketch_support.frame());
            self.sketch = sketch
                .portable_payload
                .as_ref()
                .map(|payload| Self::hydrate_sketch_canvas(self.selected_origin_plane, payload))
                .transpose()
                .expect("hydrated v6 sketch authoring was validated before publication")
                .flatten()
                .unwrap_or_else(|| SketchCanvasState::new(self.selected_origin_plane));
            self.active_sketch_tool = ToolVariant::Select;
            self.extrusion_mode = if self.sketch_support.body().is_some() {
                ExtrusionMode::Add
            } else {
                ExtrusionMode::NewBody
            };
            self.extrusion_mode_explicit = false;
            self.extrusion_distance = if self.sketch_support.body().is_some() {
                1.0
            } else {
                4.0
            };
        } else {
            self.sketch_support = SketchSupport::default();
            self.sketch = SketchCanvasState::default();
            self.active_sketch_tool = ToolVariant::Select;
            self.sketch_revision = 0;
            self.sketch_finished = false;
            self.extrusion_mode = ExtrusionMode::NewBody;
            self.extrusion_mode_explicit = false;
            self.extrusion_distance = 4.0;
        }
        self.face_sketch_context = None;
        self.pending_face_sketch = None;
        self.pending_operation = None;
        self.selected_face = None;
        self.leave_sketch_mode();
        self.active_tool = ActiveTool::Select;
        self.clear_transform_preview();
        self.sketch_last_error = None;
        self.sketch_finish_issue = None;
        self.sketch_extrusion_issue = None;
        self.extruded_sketch_revision = None;
        self.history_scrub_position = self.document.history_position();
        self.selected_history_feature = self
            .document
            .active_features()
            .last()
            .map(|feature| feature.id);
        self.sync_feature_preview_from_document();
        self.frame_visible_document();
        self.last_attempt = Attempt::Accepted {
            operation: "Native document loaded",
        };
        self.document_status = Some(format!(
            "Loaded native document · {} bodies · {} components · {} sketches",
            self.bodies.len(),
            self.document.component_instances().len(),
            self.sketches.len()
        ));
    }

    fn active_body_index(&self) -> Option<usize> {
        self.bodies
            .iter()
            .position(|body| body.ordinal == self.active_body_ordinal)
    }

    fn active_body_id(&self) -> Option<BodyId> {
        self.active_body_index()
            .and_then(|index| self.bodies.get(index))
            .map(|body| body.id)
    }

    fn component_for_body(&self, body: BodyId) -> Option<&ComponentInstanceRecord> {
        self.document
            .component_instances()
            .iter()
            .find(|component| component.bodies.contains(&body))
    }

    fn active_component_instance(&self) -> Option<&ComponentInstanceRecord> {
        self.active_body_id()
            .and_then(|body| self.component_for_body(body))
    }

    fn occurrence_transform_for_body(&self, body: BodyId) -> viewport::RigidOccurrenceTransform {
        let Some(component) = self.component_for_body(body) else {
            return viewport::RigidOccurrenceTransform::identity();
        };
        let translation = component.pose.translation;
        let rotation = component.pose.rotation;
        viewport::RigidOccurrenceTransform::new(
            Vector3::new(translation.x(), translation.y(), translation.z()),
            RotationQuaternion::new(rotation.w(), rotation.x(), rotation.y(), rotation.z()),
        )
        .expect("validated document component poses are finite and canonical")
    }

    fn measured_edge_geometry(
        &self,
    ) -> Vec<(viewport::DocumentEdgeSelection, Vec<[Point3; 2]>, f64)> {
        self.measured_edges
            .iter()
            .filter_map(|selection| {
                let body = self
                    .bodies
                    .iter()
                    .find(|body| body.id.get() == selection.body.get())?;
                let placement = self.occurrence_transform_for_body(body.id);
                let segments = body
                    .body
                    .scene
                    .edges
                    .iter()
                    .filter(|edge| edge.source_edge == selection.edge)
                    .map(|edge| edge.endpoints.map(|point| placement.transform_point(point)))
                    .collect::<Vec<_>>();
                if segments.is_empty() {
                    return None;
                }
                let length = NativeKernel::edge_length(&body.body.snapshot, selection.edge).ok()?;
                Some((*selection, segments, length))
            })
            .collect()
    }

    fn measured_face_area(&self) -> Option<(viewport::DocumentFaceSelection, f64)> {
        let selection = self.measured_face?;
        let body = self
            .bodies
            .iter()
            .find(|body| body.id.get() == selection.body.get())?;
        let area = NativeKernel::face_area(&body.body.snapshot, selection.face).ok()?;
        Some((selection, area))
    }

    fn measured_edge_angle_degrees(&self) -> Option<f64> {
        let [first, second] = self.measured_edges.as_slice() else {
            return None;
        };
        if first.body != second.body {
            return None;
        }
        let body = self
            .bodies
            .iter()
            .find(|body| body.id.get() == first.body.get())?;
        let segment_for = |selection: viewport::DocumentEdgeSelection| {
            let segments = body
                .body
                .scene
                .edges
                .iter()
                .filter(|edge| edge.source_edge == selection.edge)
                .map(|edge| edge.endpoints)
                .collect::<Vec<_>>();
            (segments.len() == 1).then(|| segments[0])
        };
        let first_segment = segment_for(*first)?;
        let second_segment = segment_for(*second)?;
        let common_planar_face = body
            .body
            .scene
            .triangles
            .iter()
            .map(|triangle| triangle.source_face)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|face| NativeKernel::planar_face_support(&body.body.snapshot, face).ok())
            .any(|support| {
                let normal = [
                    support.frame.u.y * support.frame.v.z - support.frame.u.z * support.frame.v.y,
                    support.frame.u.z * support.frame.v.x - support.frame.u.x * support.frame.v.z,
                    support.frame.u.x * support.frame.v.y - support.frame.u.y * support.frame.v.x,
                ];
                [first_segment, second_segment]
                    .into_iter()
                    .flatten()
                    .all(|point| {
                        let offset = [
                            point.x - support.frame.origin.x,
                            point.y - support.frame.origin.y,
                            point.z - support.frame.origin.z,
                        ];
                        offset[0]
                            .mul_add(
                                normal[0],
                                offset[1].mul_add(normal[1], offset[2] * normal[2]),
                            )
                            .abs()
                            <= body
                                .body
                                .snapshot
                                .precision_policy()
                                .unwrap_or_default()
                                .linear_agreement
                    })
            });
        if !common_planar_face {
            return None;
        }
        let direction = |segment: [Point3; 2]| {
            [
                segment[1].x - segment[0].x,
                segment[1].y - segment[0].y,
                segment[1].z - segment[0].z,
            ]
        };
        let first = direction(first_segment);
        let second = direction(second_segment);
        let length = |vector: [f64; 3]| {
            vector[0]
                .mul_add(
                    vector[0],
                    vector[1].mul_add(vector[1], vector[2] * vector[2]),
                )
                .sqrt()
        };
        let denominator = length(first) * length(second);
        if denominator <= f64::EPSILON {
            return None;
        }
        let cosine = first[0]
            .mul_add(second[0], first[1].mul_add(second[1], first[2] * second[2]))
            .abs()
            / denominator;
        let angle = cosine.clamp(-1.0, 1.0).acos().to_degrees();
        (angle > 1.0e-7).then_some(angle)
    }

    fn current_measurement_annotation(&self) -> Option<viewport::DocumentMeasurement> {
        let unit = self.document_settings.length_unit;
        if let Some((selection, area)) = self.measured_face_area() {
            return Some(viewport::DocumentMeasurement::Face {
                selection,
                label: format!("A {}", unit.format_area(area)),
            });
        }
        let geometry = self.measured_edge_geometry();
        match geometry.as_slice() {
            [(selection, _, length)] => Some(viewport::DocumentMeasurement::Edge {
                selection: *selection,
                label: format!("L {}", unit.format_length(*length)),
            }),
            [(first, first_segments, _), (second, second_segments, _)] => {
                let distance = first_segments
                    .iter()
                    .flat_map(|first| {
                        second_segments
                            .iter()
                            .map(move |second| model_segment_distance(*first, *second))
                    })
                    .fold(f64::INFINITY, f64::min);
                let angle = self.measured_edge_angle_degrees();
                distance
                    .is_finite()
                    .then(|| viewport::DocumentMeasurement::EdgeDistance {
                        first: *first,
                        second: *second,
                        label: angle.map_or_else(
                            || format!("D {}", unit.format_length(distance)),
                            |angle| format!("D {} · ∠ {angle:.3}°", unit.format_length(distance)),
                        ),
                    })
            }
            _ => None,
        }
    }

    fn prune_stale_measured_edges(&mut self) {
        let current_edges = self
            .bodies
            .iter()
            .flat_map(|body| {
                body.body
                    .scene
                    .edges
                    .iter()
                    .map(move |edge| viewport::DocumentEdgeSelection {
                        body: viewport::BodyInstanceKey::new(body.id.get()),
                        edge: edge.source_edge,
                    })
            })
            .collect::<Vec<_>>();
        self.measured_edges
            .retain(|selection| current_edges.contains(selection));
        self.selected_edges
            .retain(|selection| current_edges.contains(selection));
        if self.measured_face.is_some_and(|selection| {
            !self.bodies.iter().any(|body| {
                body.id.get() == selection.body.get()
                    && body
                        .body
                        .scene
                        .triangles
                        .iter()
                        .any(|triangle| triangle.source_face == selection.face)
            })
        }) {
            self.measured_face = None;
        }
        self.selected_faces.retain(|selection| {
            self.bodies.iter().any(|body| {
                body.id.get() == selection.body.get()
                    && body
                        .body
                        .scene
                        .triangles
                        .iter()
                        .any(|triangle| triangle.source_face == selection.face)
            })
        });
        if let Some(selection) = self.selected_faces.last() {
            self.selected_face = Some(selection.face);
        }
        if self
            .selected_edge
            .is_some_and(|selection| !current_edges.contains(&selection))
        {
            self.selected_edge = None;
        }
        self.selected_edge = self.selected_edges.last().copied();
        if self.selected_vertex.is_some_and(|selection| {
            !self.bodies.iter().any(|body| {
                body.id.get() == selection.body.get()
                    && body
                        .body
                        .scene
                        .vertices
                        .iter()
                        .any(|vertex| vertex.source_vertex == selection.vertex)
            })
        }) {
            self.selected_vertex = None;
        }
        self.selected_vertices.retain(|selection| {
            self.bodies.iter().any(|body| {
                body.id.get() == selection.body.get()
                    && body
                        .body
                        .scene
                        .vertices
                        .iter()
                        .any(|vertex| vertex.source_vertex == selection.vertex)
            })
        });
        self.selected_vertex = self.selected_vertices.last().copied();
    }

    fn committed_world_bounds_for_body(&self, body: &WorkbenchBody) -> Option<Aabb3> {
        let local_bounds = body.body.report.bounds?;
        self.component_for_body(body.id)
            .map_or(Some(local_bounds), |component| {
                assembly::component_world_bounds(local_bounds, component.pose).ok()
            })
    }

    fn committed_world_pivot_for_body(&self, body: &WorkbenchBody) -> Option<Point3> {
        self.committed_world_bounds_for_body(body)
            .map(bounds_center)
    }

    fn active_motion_name(&self) -> String {
        let joint_name = (|| {
            let pivot = self.body_pivot?;
            let component = self.active_component_instance()?;
            let joint = self.document.joint_for_child(component.id)?;
            let JointKind::Revolute { origin, axis, .. } = joint.kind else {
                return None;
            };
            (joint.enabled
                && axis.x() == 0.0
                && axis.y() == 0.0
                && axis.z() == 1.0
                && origin.x() == pivot.x
                && origin.y() == pivot.y
                && origin.z() == pivot.z)
                .then(|| joint.name.clone())
        })();
        joint_name.unwrap_or_else(|| "Turntable".to_owned())
    }

    fn next_document_feature_label(document: &ModelDocument, kind: FeatureKind) -> String {
        let ordinal = document
            .features()
            .iter()
            .filter(|feature| feature.kind == kind)
            .count()
            + 1;
        match kind {
            FeatureKind::Origin => "Origin".to_owned(),
            FeatureKind::DatumPlane => format!("Plane {ordinal}"),
            FeatureKind::BaseBody => "Base body".to_owned(),
            FeatureKind::Sketch => format!("Sketch {ordinal}"),
            FeatureKind::Extrude => format!("Extrude {ordinal}"),
            FeatureKind::Add => format!("Add {ordinal}"),
            FeatureKind::Cut => format!("Cut {ordinal}"),
            FeatureKind::Transform => format!("Transform {ordinal}"),
            FeatureKind::Boolean => format!("Boolean {ordinal}"),
        }
    }

    fn archive_feature_report(&mut self, feature: FeatureId, report: OperationReport) {
        let association = SnapshotAssociation::new(
            report.input_snapshot,
            report.output_snapshot,
            report.semantic_digest,
        );
        if !self
            .feature_report_archive
            .iter()
            .any(|archived| archived.feature == feature && archived.association == association)
        {
            self.feature_report_archive.push(ArchivedFeatureReport {
                feature,
                association,
                report: report.clone(),
            });
        }
        self.feature_reports
            .retain(|(existing, _)| *existing != feature);
        self.feature_reports.push((feature, report));
    }

    fn sync_feature_reports_from_document(&mut self) {
        let mut active = Vec::new();
        let mut missing = Vec::new();
        for node in self.document.features() {
            if node.action == ReplayAction::Marker {
                continue;
            }
            let Some(association) = node.committed else {
                continue;
            };
            if let Some(archived) =
                self.feature_report_archive.iter().rev().find(|archived| {
                    archived.feature == node.id && archived.association == association
                })
            {
                active.push((node.id, archived.report.clone()));
            } else {
                missing.push(node.label.clone());
            }
        }
        self.feature_reports = active;
        if !missing.is_empty() {
            self.document_status = Some(format!(
                "History needs replay evidence for {}",
                missing.join(", ")
            ));
        }
    }

    fn persistent_ref_for_current_entity(&self, entity: EntityRef) -> Option<PersistentRef> {
        if !matches!(entity.kind, EntityKind::Face | EntityKind::Edge) {
            return None;
        }
        let active_body = self.active_body_id()?;
        let kind = entity.kind;
        let mut candidate = entity;
        for (feature, report) in self.feature_reports.iter().rev() {
            if !self.document.feature(*feature).is_some_and(|node| {
                node.outputs.contains(&FeatureOutput::Body(active_body))
                    && node.committed.is_some_and(|commit| {
                        commit.input == report.input_snapshot
                            && commit.output == report.output_snapshot
                            && commit.semantic_digest == report.semantic_digest
                    })
            }) {
                continue;
            }
            let Some(history) = report
                .history
                .iter()
                .find(|record| record.outputs.contains(&candidate))
            else {
                continue;
            };
            if let Some(role) = history.role.clone() {
                return Some(PersistentRef::new(*feature, role, kind));
            }
            let mut inputs = history
                .inputs
                .iter()
                .copied()
                .filter(|input| input.kind == kind);
            let input = inputs.next()?;
            if inputs.next().is_some() {
                return None;
            }
            candidate = input;
        }
        None
    }

    fn persistent_ref_for_current_face(&self, face: EntityRef) -> Option<PersistentRef> {
        (face.kind == EntityKind::Face)
            .then(|| self.persistent_ref_for_current_entity(face))
            .flatten()
    }

    fn persistent_ref_for_current_edge(&self, edge: EntityRef) -> Option<PersistentRef> {
        (edge.kind == EntityKind::Edge)
            .then(|| self.persistent_ref_for_current_entity(edge))
            .flatten()
    }

    fn sync_active_body_record(&mut self) {
        let Some(displayed) = self.displayed.clone() else {
            return;
        };
        let Some(index) = self.active_body_index() else {
            return;
        };
        self.bodies[index].body = displayed;
        self.bodies[index].kind = self.model_body_kind;
        if let Some(record) = self.document.body(self.bodies[index].id) {
            self.bodies[index].last_feature = record.last_feature;
        }
    }

    fn archive_displayed_body(&mut self) {
        let Some(body) = self.displayed.clone() else {
            return;
        };
        if self
            .body_archive
            .iter()
            .any(|entry| entry.body.snapshot.id() == body.snapshot.id())
        {
            return;
        }
        self.body_archive.push(ArchivedBody {
            body,
            kind: self.model_body_kind,
        });
    }

    fn sync_feature_preview_from_document(&mut self) {
        let mut kind_ordinals = std::collections::BTreeMap::<u8, u32>::new();
        let mut feature_groups = BTreeMap::<FeatureId, u64>::new();
        let mut sketch_groups = BTreeMap::<SketchId, u64>::new();
        let mut body_groups = BTreeMap::<BodyId, u64>::new();
        let active_feature = self
            .active_sketch_index
            .and_then(|index| self.sketches.get(index))
            .and_then(|sketch| sketch.feature);
        let mut active_sketch = None;
        let mut entries = Vec::with_capacity(self.document.features().len());
        for feature in self.document.features() {
            let kind = match feature.kind {
                FeatureKind::Origin => FeaturePreviewKind::Origin,
                FeatureKind::DatumPlane => FeaturePreviewKind::Origin,
                FeatureKind::BaseBody if feature.component_instance.is_some() => {
                    FeaturePreviewKind::Component
                }
                FeatureKind::BaseBody => FeaturePreviewKind::BaseBody,
                FeatureKind::Sketch => FeaturePreviewKind::Sketch,
                FeatureKind::Extrude => FeaturePreviewKind::Extrude,
                FeatureKind::Add => FeaturePreviewKind::Add,
                FeatureKind::Cut => FeaturePreviewKind::Cut,
                FeatureKind::Transform => FeaturePreviewKind::Transform,
                FeatureKind::Boolean => FeaturePreviewKind::Boolean,
            };
            let key = kind as u8;
            let ordinal = if matches!(
                kind,
                FeaturePreviewKind::Origin | FeaturePreviewKind::BaseBody
            ) {
                0
            } else {
                let next = kind_ordinals.entry(key).or_insert(0);
                *next = next.saturating_add(1);
                *next
            };
            let revision = if kind == FeaturePreviewKind::Sketch {
                feature
                    .outputs
                    .iter()
                    .find_map(|output| match output {
                        FeatureOutput::Sketch {
                            geometry_revision, ..
                        } => Some(*geometry_revision),
                        FeatureOutput::Body(_) => None,
                    })
                    .unwrap_or(0)
            } else {
                0
            };
            if kind == FeaturePreviewKind::Sketch && active_feature == Some(feature.id) {
                active_sketch = Some(entries.len());
            }
            // A sketch is always a new design-intent branch, even when it is
            // hosted by the same face as an earlier sketch. Its consuming
            // extrusion inherits that branch; later targeted edge/face
            // features inherit the producer of their persistent target before
            // falling back to the current body branch.
            let targeted_producer = match &feature.action {
                ReplayAction::TargetedKernel(targeted) => {
                    targeted.targets().next().map(|target| target.producer)
                }
                _ => None,
            };
            let group = if matches!(kind, FeaturePreviewKind::Origin) {
                0
            } else if matches!(kind, FeaturePreviewKind::Sketch) {
                feature.id.get()
            } else {
                targeted_producer
                    .and_then(|producer| feature_groups.get(&producer).copied())
                    .or_else(|| {
                        feature.inputs.iter().find_map(|input| match input {
                            FeatureInput::Feature(id) => feature_groups.get(id).copied(),
                            FeatureInput::Sketch(id) => sketch_groups.get(id).copied(),
                            FeatureInput::Body(id) => body_groups.get(id).copied(),
                        })
                    })
                    .unwrap_or(feature.id.get())
            };
            feature_groups.insert(feature.id, group);
            for output in &feature.outputs {
                match output {
                    FeatureOutput::Sketch { sketch, .. } => {
                        sketch_groups.insert(*sketch, group);
                    }
                    FeatureOutput::Body(body) => {
                        body_groups.insert(*body, group);
                    }
                }
            }
            entries.push(FeaturePreviewEntry {
                kind,
                ordinal,
                revision,
                finished: kind == FeaturePreviewKind::Sketch,
                group,
            });
        }
        self.feature_preview = FeaturePreviewState {
            entries,
            active_sketch,
        };
    }

    fn restore_runtime_from_document(&mut self) {
        self.sync_feature_reports_from_document();
        let previous_active = self.active_body_id();
        let bootstrap_replaced = self.bootstrap_body.is_some()
            && self.document.features().iter().any(|feature| {
                feature.kind == FeatureKind::Extrude
                    && self.document.feature_is_active(feature.id).unwrap_or(false)
                    && feature.committed.is_some()
                    && !feature.state.suppressed
                    && feature.outputs.iter().any(|output| {
                        matches!(
                            output,
                            FeatureOutput::Body(body) if Some(*body) != self.bootstrap_body
                        )
                    })
            });
        let mut bodies = Vec::new();
        for record in self.document.bodies() {
            if bootstrap_replaced && Some(record.id) == self.bootstrap_body {
                continue;
            }
            let Some(snapshot) = record.committed_snapshot else {
                continue;
            };
            let Some(archived) = self
                .body_archive
                .iter()
                .rev()
                .find(|entry| entry.body.snapshot.id() == snapshot)
            else {
                self.document_status = Some(format!(
                    "History needs rebuild data for snapshot {snapshot}"
                ));
                continue;
            };
            let ordinal = record
                .label
                .split_whitespace()
                .last()
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or_else(|| bodies.len() as u32 + 1);
            bodies.push(WorkbenchBody {
                material: None,
                id: record.id,
                last_feature: record.last_feature,
                ordinal,
                body: archived.body.clone(),
                kind: archived.kind,
                visible: record.visible
                    && self
                        .component_for_body(record.id)
                        .is_none_or(|component| component.visible && !component.suppressed),
            });
        }
        self.bodies = bodies;
        self.next_body_ordinal = self
            .bodies
            .iter()
            .map(|body| body.ordinal)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let active_index = previous_active
            .and_then(|id| self.bodies.iter().position(|body| body.id == id))
            .or_else(|| self.bodies.len().checked_sub(1));
        if let Some(index) = active_index {
            let body = self.bodies[index].clone();
            self.active_body_ordinal = body.ordinal;
            self.body_pivot = self.committed_world_pivot_for_body(&body);
            self.displayed = Some(body.body);
            self.model_body_kind = body.kind;
        } else {
            self.displayed = None;
            self.body_pivot = None;
        }
        for sketch in &mut self.sketches {
            let Some(id) = sketch.id else {
                continue;
            };
            let Some(record) = self.document.sketch(id) else {
                // Keep the identity and geometry only as a runtime redo cache.
                // Clearing these IDs would disguise the undone profile as a
                // new uncommitted sketch; live projections already reject an
                // identity that is absent from the authoritative document.
                sketch.visible = false;
                sketch.finished = false;
                sketch.consumed = false;
                continue;
            };
            sketch.feature = Some(record.created_by);
            sketch.body = record.support_body;
            let feature_active = self
                .document
                .feature_is_active(record.created_by)
                .unwrap_or(false)
                && self
                    .document
                    .feature(record.created_by)
                    .is_some_and(|feature| {
                        feature.committed.is_some()
                            && !feature.state.suppressed
                            && feature.state.rebuild == RebuildState::Clean
                    });
            let consumed = self.document.features().iter().any(|feature| {
                self.document.feature_is_active(feature.id).unwrap_or(false)
                    && feature.committed.is_some()
                    && !feature.state.suppressed
                    && matches!(
                        feature.kind,
                        FeatureKind::Extrude | FeatureKind::Add | FeatureKind::Cut
                    )
                    && feature.inputs.contains(&FeatureInput::Sketch(id))
            });
            let auto_hidden_consumer_active = record.auto_hidden_by.is_some_and(|consumer| {
                self.document.feature_is_active(consumer).unwrap_or(false)
                    && self.document.feature(consumer).is_some_and(|feature| {
                        feature.committed.is_some() && !feature.state.suppressed
                    })
            });
            sketch.revision = record.geometry_revision;
            sketch.finished = feature_active;
            sketch.consumed = consumed;
            sketch.visible = feature_active
                && (record.visible
                    || record.auto_hidden_by.is_some() && !auto_hidden_consumer_active);
        }
        self.extruded_sketch_revision = self
            .active_sketch_index
            .and_then(|index| self.sketches.get(index))
            .and_then(|sketch| sketch.consumed.then_some(sketch.revision));
        self.selected_face = None;
        self.history_scrub_position = self.document.history_position();
        self.sync_feature_preview_from_document();
    }

    fn archived_snapshot(&self, id: SnapshotId) -> Option<Snapshot> {
        if self.empty_snapshot.id() == id {
            return Some(self.empty_snapshot.clone());
        }
        self.body_archive
            .iter()
            .rev()
            .find(|entry| entry.body.snapshot.id() == id)
            .map(|entry| entry.body.snapshot.clone())
    }

    fn rebuild_document_from(&mut self, from: FeatureId) -> bool {
        let Ok(mut transaction) = self.document.begin_rebuild(from) else {
            self.document_status = Some("The selected history branch cannot be rebuilt".to_owned());
            return false;
        };
        let evaluated_parameters = if transaction
            .plan()
            .steps
            .iter()
            .any(|step| matches!(step.action, ReplayAction::ParameterizedKernel(_)))
        {
            match self
                .document
                .evaluate_parameters(&ParameterOverrides::default())
            {
                Ok(parameters) => Some(parameters),
                Err(error) => {
                    let message = format!("document parameter evaluation failed: {error}");
                    let _ = self.document.rollback_rebuild(transaction);
                    self.document_status = Some(format!("Rebuild rolled back: {message}"));
                    return false;
                }
            }
        } else {
            None
        };
        let impacted = transaction
            .plan()
            .steps
            .iter()
            .map(|step| step.feature)
            .collect::<std::collections::BTreeSet<_>>();
        let mut reports = self
            .feature_reports
            .iter()
            .filter(|(feature, report)| {
                !impacted.contains(feature)
                    && self.document.feature(*feature).is_some_and(|node| {
                        node.committed
                            .is_some_and(|commit| commit.output == report.output_snapshot)
                    })
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut cursors = transaction
            .plan()
            .branch_bases
            .iter()
            .filter_map(|branch| branch.replay_input.map(|snapshot| (branch.body, snapshot)))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut rebuilt_bodies = Vec::<ArchivedBody>::new();

        while let Some(step) = transaction.next_executable_step().cloned() {
            debug_assert_eq!(step.disposition, ReplayDisposition::Execute);
            let feature = step.feature;
            let branch = step.branches.first().copied();
            let input_id = branch
                .and_then(|body| cursors.get(&body).copied())
                .or_else(|| {
                    self.document
                        .feature(feature)
                        .and_then(|node| node.committed)
                        .map(|commit| commit.input)
                })
                .unwrap_or_else(|| self.empty_snapshot.id());
            let input = rebuilt_bodies
                .iter()
                .rev()
                .find(|entry| entry.body.snapshot.id() == input_id)
                .map(|entry| entry.body.snapshot.clone())
                .or_else(|| self.archived_snapshot(input_id));
            let Some(input) = input else {
                let message = format!("snapshot {input_id} is unavailable for replay");
                let _ = transaction.record_failure(feature, message.clone());
                let _ = self.document.rollback_rebuild(transaction);
                self.document_status = Some(format!("Rebuild rolled back: {message}"));
                return false;
            };
            let action = match step.action {
                parameterized @ ReplayAction::ParameterizedKernel(_) => {
                    let parameters = evaluated_parameters
                        .as_ref()
                        .expect("parameterized rebuild steps require evaluated parameters");
                    match parameterized.resolve_parameters(parameters) {
                        Ok(action) => action,
                        Err(error) => {
                            let message = format!("parameter binding failed: {error}");
                            let _ = transaction.record_failure(feature, message.clone());
                            let _ = self.document.rollback_rebuild(transaction);
                            self.document_status = Some(format!("Rebuild rolled back: {message}"));
                            return false;
                        }
                    }
                }
                action => action,
            };
            let action = match action.resolve_sketch_regions(
                &self.document,
                input.precision_policy().unwrap_or_default(),
            ) {
                Ok(action) => action,
                Err(error) => {
                    let message = format!("sketch region needs repair: {error}");
                    let _ = transaction.record_failure(feature, message.clone());
                    let _ = self.document.rollback_rebuild(transaction);
                    self.document_status = Some(format!("Rebuild needs repair: {message}"));
                    return false;
                }
            };
            enum RebuildDispatch {
                Marker,
                Command(KernelCommand),
                Boolean(artificer_model::BooleanFeatureRecipe),
            }
            let dispatch = match action {
                ReplayAction::Marker => RebuildDispatch::Marker,
                ReplayAction::Kernel(command) => RebuildDispatch::Command(command),
                ReplayAction::TargetedKernel(targeted) => {
                    let ordered = self
                        .document
                        .features()
                        .iter()
                        .filter_map(|node| {
                            reports.iter().find(|(feature, _)| *feature == node.id).map(
                                |(feature, report)| FeatureOperationReport::new(*feature, report),
                            )
                        })
                        .collect::<Vec<_>>();
                    match targeted.rebind(&ordered, input.id()) {
                        PersistentResolution::Resolved(command) => {
                            RebuildDispatch::Command(command)
                        }
                        PersistentResolution::Missing(missing) => {
                            let message =
                                format!("persistent face is missing: {:?}", missing.reason);
                            let _ = transaction.record_failure(feature, message.clone());
                            let _ = self.document.rollback_rebuild(transaction);
                            self.document_status = Some(format!("Rebuild needs repair: {message}"));
                            return false;
                        }
                        PersistentResolution::Ambiguous(ambiguity) => {
                            let message =
                                format!("persistent face is ambiguous: {:?}", ambiguity.reason);
                            let _ = transaction.record_failure(feature, message.clone());
                            let _ = self.document.rollback_rebuild(transaction);
                            self.document_status = Some(format!("Rebuild needs repair: {message}"));
                            return false;
                        }
                    }
                }
                ReplayAction::ParameterizedKernel(_) => {
                    unreachable!("parameterized replay actions are resolved before kernel dispatch")
                }
                ReplayAction::SketchRegionExtrusion(_) => {
                    unreachable!("sketch-region replay actions are resolved before kernel dispatch")
                }
                ReplayAction::Boolean(recipe) => RebuildDispatch::Boolean(recipe),
            };
            let association = if let RebuildDispatch::Command(command) = &dispatch {
                self.request_serial = self.request_serial.saturating_add(1);
                let request = ExecuteRequest {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    request_id: RequestId::new(format!(
                        "workbench-{}-rebuild-{}",
                        self.request_serial,
                        feature.get()
                    )),
                    expected_snapshot: input.id(),
                    precision: input.precision_policy().unwrap_or_default(),
                    command: command.clone(),
                };
                let outcome =
                    match NativeKernel::execute(&input, &request, &CancellationToken::new()) {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            let message = format!("kernel replay failed: {error}");
                            let _ = transaction.record_failure(feature, message.clone());
                            let _ = self.document.rollback_rebuild(transaction);
                            self.document_status = Some(format!("Rebuild rolled back: {message}"));
                            return false;
                        }
                    };
                let feature_node = self.document.feature(feature);
                let is_push_pull = feature_node.is_some_and(|node| {
                    matches!(
                        &node.action,
                        ReplayAction::TargetedKernel(targeted)
                            if matches!(targeted.command_template(), KernelCommand::PushPullFace { .. })
                    )
                });
                let kind = match feature_node.map(|node| node.kind) {
                    Some(FeatureKind::Extrude) => ModelBodyKind::SketchExtrusion,
                    Some(FeatureKind::Add) if is_push_pull => ModelBodyKind::PushedPulled,
                    Some(FeatureKind::Cut) if is_push_pull => ModelBodyKind::PushedPulled,
                    Some(FeatureKind::Add) => ModelBodyKind::AddedBoss,
                    Some(FeatureKind::Cut) => ModelBodyKind::CutPocket,
                    Some(FeatureKind::BaseBody) => ModelBodyKind::Cuboid,
                    Some(FeatureKind::Transform) => branch
                        .and_then(|body| {
                            self.bodies
                                .iter()
                                .find(|candidate| candidate.id == body)
                                .map(|candidate| candidate.kind)
                        })
                        .unwrap_or(ModelBodyKind::Cuboid),
                    Some(FeatureKind::Boolean) => ModelBodyKind::Boolean,
                    Some(FeatureKind::Origin | FeatureKind::DatumPlane | FeatureKind::Sketch)
                    | None => ModelBodyKind::Cuboid,
                };
                let archived = ArchivedBody {
                    body: DisplayedBody {
                        scene: NativeKernel::debug_scene(&outcome.snapshot),
                        snapshot: outcome.snapshot,
                        report: outcome.report.clone(),
                    },
                    kind,
                };
                reports.retain(|(existing, _)| *existing != feature);
                reports.push((feature, outcome.report.clone()));
                rebuilt_bodies.push(archived);
                SnapshotAssociation::new(
                    outcome.report.input_snapshot,
                    outcome.report.output_snapshot,
                    outcome.report.semantic_digest,
                )
            } else if let RebuildDispatch::Boolean(recipe) = &dispatch {
                let tool_id = cursors.get(&recipe.tool).copied();
                let tool = tool_id.and_then(|id| {
                    rebuilt_bodies
                        .iter()
                        .rev()
                        .find(|entry| entry.body.snapshot.id() == id)
                        .map(|entry| entry.body.snapshot.clone())
                        .or_else(|| self.archived_snapshot(id))
                });
                let Some(tool) = tool else {
                    let message =
                        format!("Boolean tool body {} has no replay snapshot", recipe.tool);
                    let _ = transaction.record_failure(feature, message.clone());
                    let _ = self.document.rollback_rebuild(transaction);
                    self.document_status = Some(format!("Rebuild rolled back: {message}"));
                    return false;
                };
                self.request_serial = self.request_serial.saturating_add(1);
                let request = BooleanRequest {
                    protocol_version: CURRENT_PROTOCOL_VERSION,
                    request_id: RequestId::new(format!(
                        "workbench-{}-boolean-rebuild-{}",
                        self.request_serial,
                        feature.get()
                    )),
                    expected_target_snapshot: input.id(),
                    expected_tool_snapshot: tool.id(),
                    precision: input.precision_policy().unwrap_or_default(),
                    operation: recipe.operation,
                };
                let outcome = match NativeKernel::execute_boolean(
                    &input,
                    &tool,
                    &request,
                    &CancellationToken::new(),
                ) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        let message = format!("Boolean replay failed: {error}");
                        let _ = transaction.record_failure(feature, message.clone());
                        let _ = self.document.rollback_rebuild(transaction);
                        self.document_status = Some(format!("Rebuild rolled back: {message}"));
                        return false;
                    }
                };
                let archived = ArchivedBody {
                    body: DisplayedBody {
                        scene: NativeKernel::debug_scene(&outcome.snapshot),
                        snapshot: outcome.snapshot,
                        report: outcome.report.clone(),
                    },
                    kind: ModelBodyKind::Boolean,
                };
                reports.retain(|(existing, _)| *existing != feature);
                reports.push((feature, outcome.report.clone()));
                rebuilt_bodies.push(archived);
                SnapshotAssociation::new(
                    outcome.report.input_snapshot,
                    outcome.report.output_snapshot,
                    outcome.report.semantic_digest,
                )
            } else {
                SnapshotAssociation::new(input.id(), input.id(), input.semantic_digest())
            };
            if let Err(error) = transaction.record_success(feature, association) {
                let message = format!("rebuild journal rejected a result: {error}");
                let _ = self.document.rollback_rebuild(transaction);
                self.document_status = Some(format!("Rebuild rolled back: {message}"));
                return false;
            }
            if let Some(body) = branch {
                cursors.insert(body, association.output);
            }
        }
        let completed = transaction.plan().executable_count();
        if let Err(error) = self.document.commit_rebuild(transaction) {
            self.document_status = Some(format!("Rebuild commit rejected: {error}"));
            return false;
        }
        for rebuilt in rebuilt_bodies {
            if !self
                .body_archive
                .iter()
                .any(|entry| entry.body.snapshot.id() == rebuilt.body.snapshot.id())
            {
                self.body_archive.push(rebuilt);
            }
        }
        for (feature, report) in reports {
            self.archive_feature_report(feature, report);
        }
        self.restore_runtime_from_document();
        self.document_status = Some(format!("Rebuilt {completed} feature(s) atomically"));
        true
    }

    fn undo_document(&mut self) -> bool {
        if self.pending_operation.is_some() || !self.document.undo() {
            return false;
        }
        self.restore_runtime_from_document();
        self.document_status = Some("Undo restored the previous document state".to_owned());
        true
    }

    fn redo_document(&mut self) -> bool {
        if self.pending_operation.is_some() || !self.document.redo() {
            return false;
        }
        self.restore_runtime_from_document();
        self.document_status = Some("Redo restored the next document state".to_owned());
        true
    }

    fn move_history_cursor(&mut self, position: usize) -> bool {
        if self.pending_operation.is_some() {
            self.history_scrub_position = self.document.history_position();
            return false;
        }
        match self.document.set_history_position(position) {
            Ok(true) => {
                self.restore_runtime_from_document();
                self.sketch.clear_creation_draft();
                self.leave_sketch_mode();
                self.active_tool = ActiveTool::Select;
                self.feature_preview_drag.cancel();
                self.selected_history_feature = position
                    .checked_sub(1)
                    .and_then(|index| self.document.features().get(index))
                    .map(|feature| feature.id);
                self.document_status = Some(format!(
                    "History restored to feature {position} of {}",
                    self.document.features().len()
                ));
                true
            }
            Ok(false) => {
                self.history_scrub_position = self.document.history_position();
                false
            }
            Err(error) => {
                self.history_scrub_position = self.document.history_position();
                self.document_status = Some(format!("History rollback rejected: {error}"));
                false
            }
        }
    }

    fn toggle_feature_suppression(&mut self, feature: FeatureId) -> bool {
        if self.pending_operation.is_some() {
            return false;
        }
        let Some(node) = self.document.feature(feature) else {
            return false;
        };
        if node.state.read_only || matches!(node.kind, FeatureKind::Origin | FeatureKind::BaseBody)
        {
            self.document_status =
                Some("That foundational feature cannot be suppressed".to_owned());
            return false;
        }
        let suppressed = !node.state.suppressed;
        if let Err(error) = self.document.set_feature_suppressed(feature, suppressed) {
            self.document_status = Some(format!("Suppression rejected: {error}"));
            return false;
        }
        self.selected_history_feature = Some(feature);
        self.rebuild_document_from(feature)
    }

    fn publish_new_body_record(&mut self) {
        let Some(displayed) = self.displayed.clone() else {
            return;
        };
        let replaces_bootstrap = self.bodies.len() == 1
            && self.bodies[0].kind == ModelBodyKind::Cuboid
            && !self
                .feature_preview
                .entries
                .iter()
                .any(|entry| entry.kind == FeaturePreviewKind::Extrude);
        if replaces_bootstrap {
            let body_record = self
                .document
                .bodies()
                .last()
                .expect("a committed NewBody feature publishes a document body");
            self.bodies[0].id = body_record.id;
            self.bodies[0].last_feature = body_record.last_feature;
            self.bodies[0].body = displayed;
            self.bodies[0].kind = self.model_body_kind;
            self.bodies[0].visible = true;
            self.active_body_ordinal = self.bodies[0].ordinal;
            return;
        }

        let ordinal = self.next_body_ordinal;
        self.next_body_ordinal = self.next_body_ordinal.saturating_add(1);
        self.bodies.push(WorkbenchBody {
            material: None,
            id: self
                .document
                .bodies()
                .last()
                .expect("a committed NewBody feature publishes a document body")
                .id,
            last_feature: self
                .document
                .features()
                .last()
                .expect("a committed NewBody feature publishes a document feature")
                .id,
            ordinal,
            body: displayed,
            kind: self.model_body_kind,
            visible: true,
        });
        self.active_body_ordinal = ordinal;
    }

    fn activate_body(&mut self, index: usize) {
        let Some(body) = self.bodies.get(index).cloned() else {
            return;
        };
        self.active_body_ordinal = body.ordinal;
        self.displayed = Some(body.body.clone());
        self.model_body_kind = body.kind;
        self.body_pivot = self.committed_world_pivot_for_body(&body);
        self.clear_transform_preview();
    }

    fn clear_model_entity_selection(&mut self) {
        self.selected_face = None;
        self.selected_edge = None;
        self.selected_vertex = None;
        self.selected_faces.clear();
        self.selected_edges.clear();
        self.selected_vertices.clear();
    }

    fn select_model_face(&mut self, selection: viewport::DocumentFaceSelection, additive: bool) {
        if !additive {
            self.clear_model_entity_selection();
        }
        if additive && self.selected_faces.contains(&selection) {
            self.selected_faces
                .retain(|candidate| *candidate != selection);
        } else {
            self.selected_faces.push(selection);
        }
        self.selected_face = self.selected_faces.last().map(|selection| selection.face);
        self.selected_edge = self.selected_edges.last().copied();
        self.selected_vertex = self.selected_vertices.last().copied();
    }

    fn select_model_edge(&mut self, selection: viewport::DocumentEdgeSelection, additive: bool) {
        // A full circle is two exact semicircle edges around one carrier;
        // selection, highlighting, and edge-set features treat it as one
        // logical rim. The carrier decision is topological (kernel), with the
        // sampled-scene chain heuristic retained for straight logical edges.
        let logical_group = self
            .bodies
            .iter()
            .find(|body| body.id.get() == selection.body.get())
            .map(|body| {
                let carrier_group =
                    NativeKernel::carrier_edge_group(&body.body.snapshot, selection.edge)
                        .unwrap_or_default();
                if carrier_group.len() > 1 {
                    carrier_group.into_iter().collect::<BTreeSet<_>>()
                } else {
                    viewport::logical_edge_group(&body.body.scene, selection.edge)
                }
            })
            .unwrap_or_else(|| BTreeSet::from([selection.edge]));
        let selections = logical_group
            .into_iter()
            .map(|edge| viewport::DocumentEdgeSelection {
                body: selection.body,
                edge,
            })
            .collect::<Vec<_>>();
        if !additive {
            self.clear_model_entity_selection();
        }
        if additive
            && selections
                .iter()
                .all(|candidate| self.selected_edges.contains(candidate))
        {
            self.selected_edges
                .retain(|candidate| !selections.contains(candidate));
        } else {
            for candidate in selections {
                if !self.selected_edges.contains(&candidate) {
                    self.selected_edges.push(candidate);
                }
            }
        }
        self.selected_edge = self.selected_edges.last().copied();
        self.selected_face = self.selected_faces.last().map(|selection| selection.face);
        self.selected_vertex = self.selected_vertices.last().copied();
    }

    fn select_model_vertex(
        &mut self,
        selection: viewport::DocumentVertexSelection,
        additive: bool,
    ) {
        if !additive {
            self.clear_model_entity_selection();
        }
        if additive && self.selected_vertices.contains(&selection) {
            self.selected_vertices
                .retain(|candidate| *candidate != selection);
        } else {
            self.selected_vertices.push(selection);
        }
        self.selected_vertex = self.selected_vertices.last().copied();
        self.selected_face = self.selected_faces.last().map(|selection| selection.face);
        self.selected_edge = self.selected_edges.last().copied();
    }

    fn set_body_visibility(&mut self, index: usize, visible: bool) {
        let Some(body_id) = self.bodies.get(index).map(|body| body.id) else {
            return;
        };
        if let Err(error) = self.document.set_body_visible(body_id, visible) {
            self.document_status = Some(format!("Visibility change rejected: {error}"));
            return;
        }
        let body = &mut self.bodies[index];
        body.visible = visible;
        if !visible {
            self.selected_faces
                .retain(|selection| selection.body.get() != body_id.get());
            self.selected_edges
                .retain(|selection| selection.body.get() != body_id.get());
            self.selected_vertices
                .retain(|selection| selection.body.get() != body_id.get());
            self.selected_face = self.selected_faces.last().map(|selection| selection.face);
            self.selected_edge = self.selected_edges.last().copied();
            self.selected_vertex = self.selected_vertices.last().copied();
        }
    }

    fn visible_sketch_overlays(&self) -> Vec<viewport::ModelSketchOverlay> {
        self.sketches
            .iter()
            .enumerate()
            .filter(|(_, sketch)| {
                sketch.visible
                    && workbench_sketch_has_overlay_geometry(sketch)
                    && sketch
                        .id
                        .is_none_or(|id| self.document.sketch(id).is_some())
            })
            .filter_map(|(sketch_index, sketch)| {
                let frame = sketch
                    .portable_payload
                    .as_ref()
                    .map_or_else(|| sketch.support.frame(), |payload| payload.frame);
                let mut points = Vec::new();
                let mut segments = Vec::new();
                let mut selectable_regions = Vec::new();
                if let Some(payload) = &sketch.portable_payload {
                    for region in &payload.profile.regions {
                        let outer_local = sample_planar_loop(&region.outer)?;
                        let holes_local = region
                            .holes
                            .iter()
                            .map(sample_planar_loop)
                            .collect::<Option<Vec<_>>>()?;
                        let anchor = profile_region_anchor(&outer_local, &holes_local)?;
                        selectable_regions.push(viewport::ModelSketchRegion::new(
                            outer_local
                                .iter()
                                .copied()
                                .map(|point| frame_point(frame, point))
                                .collect(),
                            holes_local
                                .iter()
                                .map(|hole| {
                                    hole.iter()
                                        .copied()
                                        .map(|point| frame_point(frame, point))
                                        .collect()
                                })
                                .collect(),
                            [anchor.x, anchor.y],
                        ));
                    }
                    if let Some(authoring) = payload.authoring() {
                        for entity in authoring.active_entities().filter(|entity| entity.visible) {
                            let Ok(curve) = authoring.evaluated_curve(entity.id) else {
                                continue;
                            };
                            let Some(sampled) = preview_authoring_curve(frame, curve) else {
                                continue;
                            };
                            segments.extend(sampled.windows(2).map(|pair| [pair[0], pair[1]]));
                            if curve.is_periodic()
                                && let (Some(first), Some(last)) =
                                    (sampled.first().copied(), sampled.last().copied())
                            {
                                segments.push([last, first]);
                            }
                        }
                    } else {
                        for profile_loop in payload.profile.regions.iter().flat_map(|region| {
                            std::iter::once(&region.outer).chain(region.holes.iter())
                        }) {
                            let Some(sampled) = preview_planar_loop(frame, profile_loop) else {
                                continue;
                            };
                            segments.extend(sampled.windows(2).map(|pair| [pair[0], pair[1]]));
                            if let (Some(first), Some(last)) =
                                (sampled.first().copied(), sampled.last().copied())
                            {
                                segments.push([last, first]);
                            }
                        }
                    }
                } else {
                    for entity in &sketch.entities {
                        let Some(polyline) = entity.geometry.display_polyline() else {
                            continue;
                        };
                        if polyline.segments().next().is_none() {
                            points.extend(polyline.points.iter().copied().map(|point| {
                                frame_point(frame, ProtocolPoint2::new(point.u, point.v))
                            }));
                        }
                        segments.extend(polyline.segments().map(|segment| {
                            segment.map(|point| {
                                frame_point(frame, ProtocolPoint2::new(point.u, point.v))
                            })
                        }));
                    }
                }
                (!points.is_empty() || !segments.is_empty()).then(|| {
                    let overlay =
                        viewport::ModelSketchOverlay::new(points, segments, sketch.consumed)
                            .on_frame(frame)
                            .selectable(sketch_index, selectable_regions);
                    match sketch.body {
                        Some(body) => overlay.for_body(viewport::BodyInstanceKey::new(body.get())),
                        None => overlay,
                    }
                })
            })
            .collect()
    }

    fn visible_reference_plane_overlays(&self) -> Vec<viewport::ModelSketchOverlay> {
        let mut planes = self
            .construction_planes
            .iter()
            .filter(|plane| plane.visible && self.construction_plane_is_active(plane))
            .map(|plane| {
                reference_plane_overlay(
                    plane.frame,
                    plane.half_u,
                    plane.half_v,
                    self.selected_construction_plane != Some(plane.id),
                    Some(viewport::ReferencePlaneSelection::Construction(plane.id)),
                    &plane.name,
                )
            })
            .collect::<Vec<_>>();
        if let Some(PendingOperation::CreateConstructionPlane {
            frame,
            half_u,
            half_v,
            ..
        }) = self.pending_operation
        {
            planes.push(reference_plane_overlay(
                frame,
                half_u,
                half_v,
                false,
                None,
                "Plane preview",
            ));
        }
        // A genuinely blank document still presents its three usable origin
        // planes instead of replacing them with an arbitrary starter solid.
        if self.origin_reference_planes_visible() {
            planes.extend(
                SketchPlane::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(index, plane)| {
                        reference_plane_overlay(
                            sketch_plane_frame(plane),
                            ORIGIN_PLANE_HALF_EXTENT_MM,
                            ORIGIN_PLANE_HALF_EXTENT_MM,
                            self.selected_origin_plane != plane,
                            Some(viewport::ReferencePlaneSelection::Origin(index as u8)),
                            origin_plane_label(plane),
                        )
                    }),
            );
        }
        planes
    }

    fn visible_reference_plane_bounds(&self) -> Option<Aabb3> {
        let mut points = Vec::new();
        for plane in self
            .construction_planes
            .iter()
            .filter(|plane| plane.visible && self.construction_plane_is_active(plane))
        {
            points.extend(reference_plane_corners(
                plane.frame,
                plane.half_u,
                plane.half_v,
            ));
        }
        if let Some(PendingOperation::CreateConstructionPlane {
            frame,
            half_u,
            half_v,
            ..
        }) = self.pending_operation
        {
            points.extend(reference_plane_corners(frame, half_u, half_v));
        }
        if self.origin_reference_planes_visible() {
            for plane in SketchPlane::ALL {
                points.extend(reference_plane_corners(
                    sketch_plane_frame(plane),
                    ORIGIN_PLANE_HALF_EXTENT_MM,
                    ORIGIN_PLANE_HALF_EXTENT_MM,
                ));
            }
        }
        bounds_for_points(&points)
    }

    fn origin_reference_planes_visible(&self) -> bool {
        // Entering a sketch is the moment the plane choice stops being a
        // question, so the standard planes retire then rather than lingering
        // until the first solid feature commits. They are hidden, never
        // deleted: leaving the sketch on a still-blank document brings them
        // back so the next plane is pickable.
        if self.workbench_mode == WorkbenchMode::Sketch {
            return false;
        }
        // A committed sketch is not a solid feature: it answers "where", not
        // "what". Retiring the datum planes for it left a finished sketch on a
        // bodiless document with nothing on screen at all, and the viewport
        // fell through to its "No committed body" placeholder — which reads as
        // a failed commit when the commit in fact succeeded.
        self.document
            .active_features()
            .iter()
            .all(|feature| matches!(feature.kind, FeatureKind::Origin | FeatureKind::Sketch))
    }

    fn construction_plane_is_active(&self, plane: &ConstructionPlane) -> bool {
        plane
            .feature
            .is_none_or(|feature| self.document.feature_is_active(feature).unwrap_or(false))
    }

    fn sync_active_sketch_record(&mut self) {
        // A brand-new empty canvas has no Browser record to create. An
        // existing sketch must still be synchronised when its final authored
        // operation is retired, otherwise stale overlay geometry survives in
        // the Browser until the next document rebuild.
        if self.sketch.entities().is_empty() && self.active_sketch_index.is_none() {
            return;
        }
        let index = match self.active_sketch_index {
            Some(index) if index < self.sketches.len() => index,
            _ => {
                let ordinal = self.feature_preview.current_sketch_ordinal();
                let body = self.sketch_support.body();
                self.sketches.push(WorkbenchSketch {
                    id: None,
                    feature: None,
                    body,
                    ordinal,
                    support: self.sketch_support.clone(),
                    entities: Vec::new(),
                    portable_payload: None,
                    revision: 0,
                    finished: false,
                    visible: true,
                    consumed: false,
                });
                let index = self.sketches.len() - 1;
                self.active_sketch_index = Some(index);
                index
            }
        };
        let record = &mut self.sketches[index];
        record.body = self.sketch_support.body();
        record.support = self.sketch_support.clone();
        record.entities = self.sketch.entities().to_vec();
        record.revision = self.sketch_revision;
        record.finished = self.sketch_finished;
    }

    fn append_active_sketch_to_document(
        &self,
        document: &mut ModelDocument,
    ) -> Result<(SketchId, FeatureId), String> {
        let existing = self
            .active_sketch_index
            .and_then(|index| self.sketches.get(index))
            .and_then(|record| match (record.id, record.feature) {
                (Some(sketch), Some(feature)) => Some((sketch, feature)),
                _ => None,
            });
        let authoring = self.sketch.authoring().clone();
        if authoring.operations().is_empty() {
            if let Some((sketch, feature)) = existing
                && document.sketch(sketch).is_some()
                && document.feature(feature).is_some_and(|node| {
                    node.committed.is_some()
                        && !node.state.suppressed
                        && node.state.rebuild == RebuildState::Clean
                })
            {
                return Ok((sketch, feature));
            }
            return Err("a sketch with no authored operations cannot enter history".to_owned());
        }
        authoring
            .validate(PrecisionPolicy::default())
            .map_err(|error| format!("the editable sketch graph is invalid: {error}"))?;
        let profile = self.current_canvas_profile_payload();
        let support = match &self.sketch_support {
            SketchSupport::Origin { .. } | SketchSupport::ConstructionPlane { .. } => {
                // The exact frame is carried by the payload. The workspace
                // envelope owns the datum-plane identity and provenance.
                SketchSupportRecipe::Origin
            }
            SketchSupport::PlanarFace { body, face, .. } => {
                if Some(*body) != self.active_body_id() {
                    return Err("a face sketch cannot move between document bodies".to_owned());
                }
                let persistent_face =
                    self.persistent_ref_for_current_face(*face).ok_or_else(|| {
                        "the sketch face has no unique persistent-history recipe".to_owned()
                    })?;
                SketchSupportRecipe::PlanarFace {
                    body: *body,
                    face: persistent_face,
                }
            }
        };
        let payload =
            SketchPayload::from_authoring(self.sketch_support.frame(), authoring, profile, support)
                .map_err(|error| format!("the exact sketch payload is invalid: {error}"))?;

        if let Some((sketch, feature)) = existing
            && let Some(record) = document.sketch(sketch)
            && document
                .sketch_payload(sketch, record.geometry_revision)
                .is_some_and(|existing| existing == &payload)
        {
            return Ok((sketch, feature));
        }
        if let Some((sketch, _)) = existing {
            // Editing a CAD feature changes that logical feature in place; it
            // does not append a second Sketch chip after consumers which can
            // no longer participate in forward dependency propagation.
            // `replace_sketch_payload` owns one document undo snapshot,
            // advances the logical geometry revision once, and marks the
            // original Sketch node plus every dependent feature dirty.
            document
                .replace_sketch_payload(sketch, payload)
                .map_err(|error| error.to_string())?;
            let record = document
                .sketch(sketch)
                .ok_or_else(|| "the edited sketch identity disappeared".to_owned())?;
            return Ok((sketch, record.last_feature));
        }

        // A first sketch in a blank document is committed against the
        // canonical empty snapshot. `displayed == None` and `empty_snapshot`
        // are two presentations of the same valid document state, not a
        // missing snapshot.
        let sketch_commit = self.displayed.as_ref().map_or_else(
            || {
                SnapshotAssociation::new(
                    self.empty_snapshot.id(),
                    self.empty_snapshot.id(),
                    self.empty_snapshot.semantic_digest(),
                )
            },
            |displayed| {
                SnapshotAssociation::new(
                    displayed.snapshot.id(),
                    displayed.snapshot.id(),
                    displayed.snapshot.semantic_digest(),
                )
            },
        );
        let mut draft = FeatureDraft::new(
            FeatureKind::Sketch,
            Self::next_document_feature_label(document, FeatureKind::Sketch),
            ReplayAction::Marker,
        )
        .with_sketch_payload(payload)
        .with_commit(sketch_commit);
        draft = if let Some(body) = self.sketch_support.body() {
            draft
                .with_output(OutputDraft::CreateSketch {
                    label: format!("Sketch {}", self.feature_preview.current_sketch_ordinal()),
                    geometry_revision: self.sketch_revision,
                })
                .with_input(FeatureInput::Body(body))
        } else {
            draft.with_output(OutputDraft::CreateSketch {
                label: format!("Sketch {}", self.feature_preview.current_sketch_ordinal()),
                geometry_revision: self.sketch_revision,
            })
        };
        let appended = document
            .append_feature(draft)
            .map_err(|error| error.to_string())?;
        let sketch = appended
            .created_sketches
            .first()
            .copied()
            .ok_or_else(|| "the sketch feature did not publish a sketch identity".to_owned())?;
        Ok((sketch, appended.feature))
    }

    fn bind_active_sketch_document_ids(&mut self, sketch: SketchId, feature: FeatureId) {
        self.sync_active_sketch_record();
        if let Some(index) = self.active_sketch_index
            && let Some(record) = self.sketches.get_mut(index)
        {
            record.id = Some(sketch);
            record.feature = Some(feature);
            record.portable_payload = self
                .document
                .sketch(sketch)
                .and_then(|record| {
                    self.document
                        .sketch_payload(record.id, record.geometry_revision)
                })
                .cloned();
        }
    }

    fn append_extrusion_to_document(
        &self,
        document: &mut ModelDocument,
        sketch: SketchId,
        command: KernelCommand,
        report: &OperationReport,
        mode: ExtrusionMode,
    ) -> Result<(FeatureId, Option<BodyId>), String> {
        let kind = match mode {
            ExtrusionMode::NewBody => FeatureKind::Extrude,
            ExtrusionMode::Add => FeatureKind::Add,
            ExtrusionMode::Cut => FeatureKind::Cut,
        };
        let authoring = document
            .sketch(sketch)
            .and_then(|record| document.sketch_payload(sketch, record.geometry_revision))
            .and_then(SketchPayload::authoring)
            .ok_or_else(|| {
                "the extrusion source has no editable sketch authoring graph".to_owned()
            })?;
        let action = match command {
            KernelCommand::ExtrudePlanarProfile {
                profile, distance, ..
            } => {
                let selected_regions = authoring_region_signatures_for_profile(authoring, &profile)
                    .ok_or_else(|| {
                        "the extrusion profile has no stable sketch-region selection".to_owned()
                    })?;
                ReplayAction::SketchRegionExtrusion(
                    SketchRegionExtrusion::new_body(sketch, selected_regions, distance.abs())
                        .map_err(|error| format!("invalid sketch-region extrusion: {error}"))?,
                )
            }
            KernelCommand::ExtrudeFaceProfile {
                target_face,
                vertices,
                distance,
                operation,
                ..
            } => {
                let profile = PlanarProfile2::from_polygon(&vertices);
                let selected_regions = authoring_region_signatures_for_profile(authoring, &profile)
                    .ok_or_else(|| {
                        "the face extrusion profile has no stable sketch-region selection"
                            .to_owned()
                    })?;
                let target = self
                    .persistent_ref_for_current_face(target_face)
                    .ok_or_else(|| {
                        "the selected face has no unique persistent-history recipe".to_owned()
                    })?;
                ReplayAction::SketchRegionExtrusion(
                    SketchRegionExtrusion::on_face(
                        sketch,
                        selected_regions,
                        target,
                        operation,
                        distance.abs(),
                    )
                    .map_err(|error| format!("invalid face sketch-region extrusion: {error}"))?,
                )
            }
            KernelCommand::ExtrudeFacePlanarProfile {
                target_face,
                profile,
                distance,
                operation,
                ..
            } => {
                let selected_regions = authoring_region_signatures_for_profile(authoring, &profile)
                    .ok_or_else(|| {
                        "the face extrusion profile has no stable sketch-region selection"
                            .to_owned()
                    })?;
                let target = self
                    .persistent_ref_for_current_face(target_face)
                    .ok_or_else(|| {
                        "the selected face has no unique persistent-history recipe".to_owned()
                    })?;
                ReplayAction::SketchRegionExtrusion(
                    SketchRegionExtrusion::on_face(
                        sketch,
                        selected_regions,
                        target,
                        operation,
                        distance.abs(),
                    )
                    .map_err(|error| format!("invalid face sketch-region extrusion: {error}"))?,
                )
            }
            _ => ReplayAction::Kernel(command),
        };
        let mut draft = FeatureDraft::new(
            kind,
            Self::next_document_feature_label(document, kind),
            action,
        )
        .with_input(FeatureInput::Sketch(sketch))
        .with_commit(SnapshotAssociation::new(
            report.input_snapshot,
            report.output_snapshot,
            report.semantic_digest,
        ));
        match mode {
            ExtrusionMode::NewBody => {
                let ordinal = if self.bodies.len() == 1
                    && self.bodies[0].kind == ModelBodyKind::Cuboid
                    && !self
                        .feature_preview
                        .entries
                        .iter()
                        .any(|entry| entry.kind == FeaturePreviewKind::Extrude)
                {
                    1
                } else {
                    self.next_body_ordinal
                };
                draft = draft.with_output(OutputDraft::CreateBody {
                    label: format!("Body {ordinal}"),
                });
            }
            ExtrusionMode::Add | ExtrusionMode::Cut => {
                let body = self
                    .sketch_support
                    .body()
                    .ok_or_else(|| "a face feature requires a body-bound sketch".to_owned())?;
                if Some(body) != self.active_body_id() {
                    return Err(
                        "a face feature cannot consume a sketch from another body".to_owned()
                    );
                }
                draft = draft
                    .with_input(FeatureInput::Body(body))
                    .with_output(OutputDraft::ModifyBody(body));
            }
        }
        let appended = document
            .append_feature(draft)
            .map_err(|error| error.to_string())?;
        Ok((appended.feature, appended.created_bodies.first().copied()))
    }

    fn append_push_pull_to_document(
        &self,
        document: &mut ModelDocument,
        body: BodyId,
        target_face: EntityRef,
        command: KernelCommand,
        report: &OperationReport,
        distance: f64,
    ) -> Result<FeatureId, String> {
        if Some(body) != self.active_body_id() {
            return Err("push/pull cannot modify an inactive body".to_owned());
        }
        let target = self
            .persistent_ref_for_current_face(target_face)
            .ok_or_else(|| {
                "the selected face has no unique persistent-history recipe".to_owned()
            })?;
        let action = ReplayAction::TargetedKernel(
            TargetedKernel::new(command, target)
                .map_err(|error| format!("invalid persistent push/pull command: {error:?}"))?,
        );
        let kind = if distance < 0.0 {
            FeatureKind::Cut
        } else {
            FeatureKind::Add
        };
        document
            .append_feature(
                FeatureDraft::new(
                    kind,
                    Self::next_document_feature_label(document, kind),
                    action,
                )
                .with_input(FeatureInput::Body(body))
                .with_output(OutputDraft::ModifyBody(body))
                .with_commit(SnapshotAssociation::new(
                    report.input_snapshot,
                    report.output_snapshot,
                    report.semantic_digest,
                )),
            )
            .map(|appended| appended.feature)
            .map_err(|error| error.to_string())
    }

    fn set_sketch_visibility(&mut self, index: usize, visible: bool) {
        let Some(sketch_id) = self.sketches.get(index).and_then(|sketch| sketch.id) else {
            if let Some(sketch) = self.sketches.get_mut(index) {
                sketch.visible = visible;
            }
            return;
        };
        if let Err(error) = self.document.set_sketch_visible(sketch_id, visible) {
            self.document_status = Some(format!("Visibility change rejected: {error}"));
            return;
        }
        if let Some(sketch) = self.sketches.get_mut(index) {
            sketch.visible = visible;
        }
    }

    /// Makes one committed Browser sketch the modeling selection. Geometry is
    /// sourced from its document payload, so selection works after a fresh
    /// process load and does not depend on a surviving UI-canvas cache.
    fn activate_committed_sketch(&mut self, index: usize) -> bool {
        if self.pending_operation.is_some() || !self.history_is_at_end() {
            return false;
        }
        let Some(record) = self.sketches.get(index).cloned() else {
            return false;
        };
        let Some(sketch_id) = record.id else {
            return false;
        };
        if self.document.sketch(sketch_id).is_none()
            || record
                .feature
                .is_some_and(|feature| !self.document.feature_is_active(feature).unwrap_or(false))
        {
            return false;
        }
        if let Some(body) = record.body
            && let Some(body_index) = self
                .bodies
                .iter()
                .position(|candidate| candidate.id == body)
        {
            self.activate_body(body_index);
        }

        let already_active = self.active_sketch_index == Some(index);
        self.active_sketch_index = Some(index);
        self.sketch_support = record.support;
        self.sketch_revision = record.revision;
        self.sketch_finished = true;
        self.selected_origin_plane = sketch_plane_for_frame(self.sketch_support.frame());
        if !already_active {
            self.sketch = match record.portable_payload.as_ref() {
                Some(payload) => {
                    match Self::hydrate_sketch_canvas(self.selected_origin_plane, payload) {
                        Ok(Some(canvas)) => canvas,
                        Ok(None) => SketchCanvasState::new(self.selected_origin_plane),
                        Err(_) => return false,
                    }
                }
                None => SketchCanvasState::new(self.selected_origin_plane),
            };
            self.active_sketch_tool = ToolVariant::Select;
        }
        self.extrusion_mode = if self.sketch_support.body().is_some() {
            ExtrusionMode::Add
        } else {
            ExtrusionMode::NewBody
        };
        self.extrusion_mode_explicit = false;
        self.extrusion_distance = if self.sketch_support.body().is_some() {
            1.0
        } else {
            4.0
        };
        self.extruded_sketch_revision = None;
        self.selected_face = None;
        self.face_sketch_context = None;
        self.leave_sketch_mode();
        self.sketch_finish_issue = None;
        self.sketch_extrusion_issue = None;
        self.selected_history_feature = record.feature;
        self.sync_feature_preview_from_document();
        true
    }

    fn consume_active_sketch(&mut self) {
        self.sync_active_sketch_record();
        if let Some(index) = self.active_sketch_index
            && let Some(sketch) = self.sketches.get_mut(index)
        {
            sketch.finished = true;
            sketch.consumed = true;
            sketch.visible = false;
        }
    }

    fn begin_new_origin_sketch(&mut self) {
        if self.pending_operation.is_some() || !self.history_is_at_end() {
            return;
        }
        self.selected_face = None;
        self.sketch = SketchCanvasState::new(self.selected_origin_plane);
        self.active_sketch_tool = ToolVariant::Select;
        self.sketch_support = SketchSupport::Origin {
            plane: self.selected_origin_plane,
        };
        self.face_sketch_context = None;
        self.active_sketch_index = None;
        self.sketch_revision = 0;
        self.sketch_finished = false;
        self.sketch_last_error = None;
        self.sketch_finish_issue = None;
        self.sketch_extrusion_issue = None;
        self.extruded_sketch_revision = None;
        self.extrusion_mode = ExtrusionMode::NewBody;
        self.extrusion_mode_explicit = false;
        self.extrusion_distance = 4.0;
        self.feature_preview.begin_new_sketch();
        self.workbench_mode = WorkbenchMode::Sketch;
    }

    /// Resolves and inserts one immutable catalog variant through the same
    /// transactional kernel/document boundary as every other modeling action.
    /// Nothing is published until the package, kernel result, component
    /// occurrence, and feature-history record have all been accepted.
    fn execute_library_insertion(&mut self, staging_id: u64) {
        let Some(intent) = self
            .part_library
            .staged_intent()
            .filter(|intent| intent.staging_id == staging_id)
            .cloned()
        else {
            self.document_status =
                Some("Library insertion rejected: the staged placement no longer exists".into());
            return;
        };
        if !self.history_is_at_end() {
            self.document_status =
                Some("Library insertion is unavailable while design history is rolled back".into());
            return;
        }

        let resolution = self.catalog_store.as_ref().map_or_else(
            || resolve_builtin_insertion(&intent),
            |store| resolve_store_insertion(store, &intent),
        );
        let resolved = match resolution {
            Ok(resolved) if resolved.staging_id() == staging_id => resolved,
            Ok(_) => {
                self.document_status = Some(
                    "Library insertion rejected: the resolved placement identity changed".into(),
                );
                return;
            }
            Err(error) => {
                self.document_status = Some(format!("Library insertion rejected: {error}"));
                return;
            }
        };
        let definition = match ComponentDefinitionRef::new(
            intent.definition_key.clone(),
            ComponentDefinitionRevision::new(intent.definition_revision, 0, 0),
            ComponentContentDigest::from_bytes(*resolved.evidence().definition_digest().as_bytes()),
        ) {
            Ok(definition) => definition,
            Err(error) => {
                self.document_status = Some(format!(
                    "Library insertion rejected: invalid component definition: {error}"
                ));
                return;
            }
        };
        self.request_serial = self.request_serial.saturating_add(1);
        let input = NativeKernel::empty();
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new(format!(
                "workbench-{}-insert-library-component",
                self.request_serial
            )),
            expected_snapshot: input.id(),
            precision: PrecisionPolicy::default(),
            command: resolved.command().clone(),
        };
        let replay_command = request.command.clone();
        let outcome = match NativeKernel::execute(&input, &request, &CancellationToken::new()) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.last_attempt = Attempt::Rejected {
                    operation: "Library component rejected",
                    error,
                };
                self.document_status =
                    Some("Library insertion rejected by the geometry kernel".into());
                return;
            }
        };
        let Some(local_bounds) = outcome.report.bounds else {
            self.document_status =
                Some("Library insertion rejected: the accepted part has no finite bounds".into());
            return;
        };
        let occupied_world_bounds = self
            .bodies
            .iter()
            .filter_map(|body| self.committed_world_bounds_for_body(body))
            .collect::<Vec<_>>();
        let initial_pose = match assembly::deterministic_initial_pose(
            local_bounds,
            &occupied_world_bounds,
            assembly::DEFAULT_COMPONENT_INSERTION_CLEARANCE_MM,
        ) {
            Ok(pose) => pose,
            Err(error) => {
                self.document_status =
                    Some(format!("Library insertion placement rejected: {error}"));
                return;
            }
        };
        let component = ComponentInstanceDraft::new(
            intent.display_name.clone(),
            definition,
            resolved.evaluated_parameters().clone(),
            initial_pose,
        );

        let mut next_document = self.document.clone();
        let association = SnapshotAssociation::new(
            outcome.report.input_snapshot,
            outcome.report.output_snapshot,
            outcome.report.semantic_digest,
        );
        let appended = match next_document.append_feature(
            FeatureDraft::new(
                FeatureKind::BaseBody,
                format!("Insert {}", intent.display_name),
                ReplayAction::Kernel(replay_command),
            )
            .with_component_instance(component)
            .with_output(OutputDraft::CreateBody {
                label: intent.display_name.clone(),
            })
            .with_commit(association),
        ) {
            Ok(appended)
                if appended.created_bodies.len() == 1
                    && appended.created_component_instance.is_some() =>
            {
                appended
            }
            Ok(_) => {
                self.document_status = Some(
                    "Library insertion rejected: the feature did not create one component body"
                        .into(),
                );
                return;
            }
            Err(error) => {
                self.document_status = Some(format!(
                    "Library insertion rejected by design history: {error}"
                ));
                return;
            }
        };

        // This call cannot fail after the matching staged intent was cloned,
        // but it remains the final gate before either staged result is made
        // visible in the workspace.
        if !self.part_library.commit_staged(staging_id) {
            self.document_status =
                Some("Library insertion rejected: confirmation state changed".into());
            return;
        }

        let feature_id = appended.feature;
        let body_id = appended.created_bodies[0];
        let scene = NativeKernel::debug_scene(&outcome.snapshot);
        let displayed = DisplayedBody {
            snapshot: outcome.snapshot,
            report: outcome.report,
            scene,
        };

        self.document = next_document;
        self.history_scrub_position = self.document.history_position();
        self.archive_feature_report(feature_id, displayed.report.clone());
        self.displayed = Some(displayed.clone());
        self.face_sketch_context = None;
        self.pending_face_sketch = None;
        self.clear_transform_preview();
        self.pending_operation = None;
        self.selected_face = None;
        self.leave_sketch_mode();
        self.active_tool = ActiveTool::Select;
        self.model_body_kind = ModelBodyKind::SketchExtrusion;

        let ordinal = self.next_body_ordinal;
        self.next_body_ordinal = self.next_body_ordinal.saturating_add(1);
        self.bodies.push(WorkbenchBody {
            material: None,
            id: body_id,
            last_feature: feature_id,
            ordinal,
            body: displayed,
            kind: self.model_body_kind,
            visible: true,
        });
        self.active_body_ordinal = ordinal;
        self.archive_displayed_body();
        self.body_pivot = self
            .bodies
            .last()
            .and_then(|body| self.committed_world_pivot_for_body(body));
        self.frame_visible_document();
        self.feature_preview.append(FeaturePreviewKind::Component);
        self.last_attempt = Attempt::Accepted {
            operation: "Library component committed",
        };
        self.selected_history_feature = Some(feature_id);
        self.document_status = Some(format!(
            "Inserted {} as component instance {}",
            intent.display_name,
            appended
                .created_component_instance
                .expect("the component occurrence was validated")
                .get()
        ));
    }

    fn execute_case(&mut self, case: LabCase, staged_base: Option<SnapshotId>) {
        self.last_case = case;
        self.request_serial += 1;
        let interactive_replacement = staged_base.is_some();

        if let Some(staged_base) = staged_base {
            let actual = self
                .displayed_snapshot_id()
                .unwrap_or_else(|| self.empty_snapshot.id());
            if actual != staged_base {
                self.last_attempt = Attempt::Rejected {
                    operation: "Cuboid rejected",
                    error: workbench_extrusion_error(
                        KernelErrorCode::StaleSnapshot,
                        actual,
                        "the staged diagnostic replacement targets a body that is no longer displayed",
                    ),
                };
                return;
            }
        }

        // MakeCuboid is a root constructor. A diagnostic replacement discards
        // the previous document only after this new root succeeds, so its
        // operation report and document commit must both start from ZERO.
        let input = &self.empty_snapshot;
        let base_snapshot = input.id();
        let expected_snapshot = match case {
            LabCase::StaleSnapshot => deliberately_stale_id(base_snapshot),
            LabCase::CanonicalCuboid | LabCase::ZeroWidth | LabCase::NonFiniteDepth => {
                base_snapshot
            }
        };
        let (size_x, size_y, size_z) = match case {
            LabCase::CanonicalCuboid | LabCase::StaleSnapshot => (2.0, 3.0, 4.0),
            LabCase::ZeroWidth => (0.0, 3.0, 4.0),
            LabCase::NonFiniteDepth => (2.0, 3.0, f64::NAN),
        };
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new(format!(
                "workbench-{}-{}",
                self.request_serial,
                case.slug()
            )),
            expected_snapshot,
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeCuboid {
                origin: Point3::new(0.0, 0.0, 0.0),
                size_x,
                size_y,
                size_z,
            },
        };

        // This is intentionally the exact public path used by headless callers.
        // A rejected operation never replaces `displayed`.
        match NativeKernel::execute(input, &request, &CancellationToken::new()) {
            Ok(outcome) => {
                let scene = NativeKernel::debug_scene(&outcome.snapshot);
                let bounds = outcome.report.bounds;
                self.displayed = Some(DisplayedBody {
                    snapshot: outcome.snapshot,
                    report: outcome.report,
                    scene,
                });
                self.face_sketch_context = None;
                if let Some(bounds) = bounds {
                    self.body_pivot = Some(bounds_center(bounds));
                    self.view.frame(bounds);
                }
                self.clear_transform_preview();
                if matches!(
                    self.pending_operation,
                    Some(PendingOperation::RunCase { .. })
                ) {
                    self.pending_operation = None;
                }
                self.selected_face = None;
                self.last_attempt = Attempt::Accepted {
                    operation: "Cuboid committed",
                };
                self.model_body_kind = ModelBodyKind::Cuboid;
                self.extruded_sketch_revision = None;
                if interactive_replacement {
                    self.sketch = SketchCanvasState::default();
                    self.active_sketch_tool = ToolVariant::Select;
                    self.sketch_support = SketchSupport::default();
                    self.face_sketch_context = None;
                    self.selected_origin_plane = SketchPlane::XY;
                    self.sketch_revision = 0;
                    self.sketch_finished = false;
                    self.sketch_last_error = None;
                    self.sketch_finish_issue = None;
                    self.sketch_extrusion_issue = None;
                    self.extrusion_mode = ExtrusionMode::NewBody;
                    self.extrusion_mode_explicit = false;
                    self.extrusion_distance = 4.0;
                    self.sketches.clear();
                    self.active_sketch_index = None;
                    self.feature_preview.reset();
                    self.initialize_document_from_displayed();
                }
            }
            Err(error) => {
                self.last_attempt = Attempt::Rejected {
                    operation: "Cuboid rejected",
                    error,
                };
            }
        }
    }

    fn sync_transform_preview(&mut self) {
        if !self.history_is_at_end() {
            self.clear_transform_preview();
            return;
        }
        if self.display_transform.is_identity() {
            if matches!(
                self.pending_operation,
                Some(
                    PendingOperation::Transform { .. }
                        | PendingOperation::ComponentPlacement { .. }
                )
            ) {
                self.pending_operation = None;
            }
            return;
        }
        if self.pending_operation.is_some() {
            return;
        }
        if let Some((component, base_pose, grounded, label, joint_name)) =
            self.active_component_instance().map(|component| {
                (
                    component.id,
                    component.pose,
                    component.grounded,
                    component.label.clone(),
                    self.document
                        .joint_for_child(component.id)
                        .map(|joint| joint.name.clone()),
                )
            })
        {
            if grounded {
                self.display_transform.reset();
                self.document_status = Some(format!(
                    "{} is grounded; release it before changing its placement",
                    label
                ));
                return;
            }
            if let Some(joint_name) = joint_name {
                self.display_transform.reset();
                self.document_status = Some(format!(
                    "{label} is constrained by {joint_name}; joint-coordinate editing is the next assembly slice"
                ));
                return;
            }
            if (self.display_transform.scale - 1.0).abs() > f64::EPSILON {
                self.display_transform.set_scale(1.0);
                self.document_status =
                    Some("Component occurrences can move and rotate, but cannot scale".into());
            }
            self.pending_operation = Some(PendingOperation::ComponentPlacement {
                component,
                base_pose,
            });
        } else if let Some(base_snapshot) = self.displayed_snapshot_id() {
            self.pending_operation = Some(PendingOperation::Transform { base_snapshot });
        }
    }

    fn clear_transform_preview(&mut self) {
        self.display_transform.reset();
        if matches!(
            self.pending_operation,
            Some(PendingOperation::Transform { .. } | PendingOperation::ComponentPlacement { .. })
        ) {
            self.pending_operation = None;
        }
    }

    fn stage_case(&mut self, case: LabCase) {
        if self.pending_operation.is_none() && self.history_is_at_end() {
            self.active_tool = ActiveTool::Select;
            let base_snapshot = self
                .displayed_snapshot_id()
                .unwrap_or_else(|| self.empty_snapshot.id());
            self.pending_operation = Some(PendingOperation::RunCase {
                case,
                base_snapshot,
            });
        }
    }

    fn stage_construction_plane(&mut self) {
        if self.pending_operation.is_some() || !self.history_is_at_end() {
            return;
        }
        if !(1..=2).contains(&self.selected_faces.len()) {
            self.document_status = Some(
                "Select one planar face for a coincident plane, or two parallel planar faces for a midplane"
                    .into(),
            );
            return;
        }
        let supports = self
            .selected_faces
            .iter()
            .map(|selection| {
                let body = self
                    .bodies
                    .iter()
                    .find(|body| body.id.get() == selection.body.get())
                    .ok_or_else(|| "the selected face body is no longer available".to_owned())?;
                let support =
                    NativeKernel::planar_face_support(&body.body.snapshot, selection.face)
                        .map_err(|error| format!("the selected face is not planar: {error}"))?;
                let (frame, half_u, half_v) = centered_plane_frame(&support)?;
                Ok((body.id, selection.face, frame, half_u, half_v))
            })
            .collect::<Result<Vec<_>, String>>();
        let supports = match supports {
            Ok(supports) => supports,
            Err(error) => {
                self.document_status = Some(format!("Plane creation rejected: {error}"));
                return;
            }
        };
        let (frame, half_u, half_v, source) = match supports.as_slice() {
            [(body, face, frame, half_u, half_v)] => (
                *frame,
                *half_u,
                *half_v,
                ConstructionPlaneSource::OnFace {
                    body: *body,
                    face: *face,
                },
            ),
            [first, second] => {
                let Some(first_normal) = frame_normal(first.2) else {
                    self.document_status =
                        Some("Plane creation rejected: invalid first face frame".into());
                    return;
                };
                let Some(second_normal) = frame_normal(second.2) else {
                    self.document_status =
                        Some("Plane creation rejected: invalid second face frame".into());
                    return;
                };
                if dot_vector(first_normal, second_normal).abs() < 1.0 - 1.0e-8 {
                    self.document_status = Some(
                        "Plane creation rejected: the two selected faces must be parallel".into(),
                    );
                    return;
                }
                let separation = dot_vector(
                    Vector3::new(
                        second.2.origin.x - first.2.origin.x,
                        second.2.origin.y - first.2.origin.y,
                        second.2.origin.z - first.2.origin.z,
                    ),
                    first_normal,
                );
                let origin = Point3::new(
                    first.2.origin.x + 0.5 * separation * first_normal.x,
                    first.2.origin.y + 0.5 * separation * first_normal.y,
                    first.2.origin.z + 0.5 * separation * first_normal.z,
                );
                (
                    PlanarFrame3::new(origin, first.2.u, first.2.v),
                    first.3.max(second.3),
                    first.4.max(second.4),
                    ConstructionPlaneSource::BetweenFaces {
                        first_body: first.0,
                        first_face: first.1,
                        second_body: second.0,
                        second_face: second.1,
                    },
                )
            }
            _ => unreachable!("the selection count was bounded above"),
        };
        self.pending_operation = Some(PendingOperation::CreateConstructionPlane {
            frame,
            half_u,
            half_v,
            source,
        });
        self.document_status = Some(if supports.len() == 1 {
            "Coincident plane staged · confirm with Enter or the green tick".into()
        } else {
            "Midplane staged · confirm with Enter or the green tick".into()
        });
    }

    fn commit_construction_plane(
        &mut self,
        frame: PlanarFrame3,
        half_u: f64,
        half_v: f64,
        source: ConstructionPlaneSource,
    ) {
        let id = self.next_construction_plane_id;
        self.next_construction_plane_id = id.saturating_add(1);
        self.construction_planes.push(ConstructionPlane {
            id,
            name: format!("Plane {id}"),
            feature: None,
            frame,
            half_u,
            half_v,
            visible: true,
            source,
        });
        let association = self.displayed.as_ref().map_or_else(
            || {
                SnapshotAssociation::new(
                    self.empty_snapshot.id(),
                    self.empty_snapshot.id(),
                    self.empty_snapshot.semantic_digest(),
                )
            },
            |displayed| {
                SnapshotAssociation::new(
                    displayed.snapshot.id(),
                    displayed.snapshot.id(),
                    displayed.snapshot.semantic_digest(),
                )
            },
        );
        if let Ok(appended) = self.document.append_feature(
            FeatureDraft::new(
                FeatureKind::DatumPlane,
                format!("Plane {id}"),
                ReplayAction::Marker,
            )
            .with_commit(association),
        ) {
            if let Some(plane) = self.construction_planes.last_mut() {
                plane.feature = Some(appended.feature);
            }
            self.selected_history_feature = Some(appended.feature);
            self.history_scrub_position = self.document.history_position();
            self.sync_feature_preview_from_document();
        }
        self.selected_construction_plane = Some(id);
        self.clear_model_entity_selection();
        self.pending_operation = None;
        self.document_status = Some(format!("Plane {id} committed and ready for sketching"));
    }

    fn begin_construction_plane_sketch(&mut self, id: u64) {
        let Some(plane) = self
            .construction_planes
            .iter()
            .find(|plane| plane.id == id)
            .cloned()
        else {
            return;
        };
        let half_extent = plane.half_u.max(plane.half_v);
        if self.start_plane_sketch_camera_transition(plane.frame, half_extent) {
            self.pending_plane_sketch = Some(PendingPlaneSketch::Construction(id));
            return;
        }
        self.open_construction_plane_sketch(id);
    }

    /// Opens a sketch on a construction plane, once the camera is looking at
    /// it.
    fn open_construction_plane_sketch(&mut self, id: u64) {
        let Some(plane) = self
            .construction_planes
            .iter()
            .find(|plane| plane.id == id)
            .cloned()
        else {
            return;
        };
        let display_plane = sketch_plane_for_frame(plane.frame);
        self.sketch = SketchCanvasState::new(display_plane);
        self.active_sketch_tool = ToolVariant::Select;
        self.sketch_support = SketchSupport::ConstructionPlane {
            id: Some(id),
            frame: Box::new(plane.frame),
        };
        self.face_sketch_context = None;
        self.active_sketch_index = None;
        self.sketch_revision = 0;
        self.sketch_finished = false;
        self.sketch_last_error = None;
        self.sketch_finish_issue = None;
        self.sketch_extrusion_issue = None;
        self.extruded_sketch_revision = None;
        self.extrusion_mode = ExtrusionMode::NewBody;
        self.extrusion_mode_explicit = false;
        self.extrusion_distance = 4.0;
        self.feature_preview.begin_new_sketch();
        self.workbench_mode = WorkbenchMode::Sketch;
        self.show_properties_tab();
    }

    fn enter_sketch_mode(&mut self) {
        if self.pending_operation.is_some() || !self.history_is_at_end() {
            return;
        }
        if self.selected_face.is_some() && self.active_component_instance().is_some() {
            self.document_status = Some(
                "Library component faces are read-only in this workspace; edit the source part or start an origin-plane sketch"
                    .into(),
            );
            return;
        }
        // An undo may move the document before the active sketch while its
        // runtime geometry remains cached for a possible redo. That cache is
        // not a fresh sketch and must not be reopened through the workspace
        // switch or offered to Extrude.
        if !self.active_document_sketch_is_available() {
            return;
        }
        if let Some(selected_face) = self.selected_face {
            let already_on_selected_face = matches!(
                &self.sketch_support,
                SketchSupport::PlanarFace { face, .. } if *face == selected_face
            ) && self.sketch_support_is_current();
            if !already_on_selected_face {
                let Some(body) = &self.displayed else {
                    return;
                };
                match NativeKernel::planar_face_support(&body.snapshot, selected_face) {
                    Ok(support) => {
                        if self.start_face_sketch_camera_transition(support) {
                            return;
                        }
                    }
                    Err(error) => {
                        self.sketch_extrusion_issue = Some(error.clone());
                        self.last_attempt = Attempt::Rejected {
                            operation: "Face sketch rejected",
                            error,
                        };
                        return;
                    }
                }
            }
        } else if let Some(id) = self.selected_construction_plane {
            let already_on_plane = matches!(
                self.sketch_support,
                SketchSupport::ConstructionPlane { id: Some(active), .. } if active == id
            ) && self.sketch_support_is_current();
            if !already_on_plane || self.sketch.entities().is_empty() {
                self.begin_construction_plane_sketch(id);
                return;
            }
        } else if self.sketch.entities().is_empty() {
            let plane = self.selected_origin_plane;
            let flying = self.start_plane_sketch_camera_transition(
                sketch_plane_frame(plane),
                ORIGIN_PLANE_HALF_EXTENT_MM,
            );
            if flying {
                self.pending_plane_sketch = Some(PendingPlaneSketch::Origin(plane));
                return;
            }
            self.open_origin_plane_sketch(plane);
        } else if matches!(&self.sketch_support, SketchSupport::Origin { .. }) {
            self.selected_origin_plane = self.sketch.plane();
        } else if !self.sketch_support_is_current() {
            // Face-backed sketches belong to the exact committed snapshot
            // whose support digest certified them. Reopening one after the
            // body changes would make an old face look editable against a new
            // B-rep. M5a can remap the feature during replay, but direct edits
            // to historical sketch geometry are not exposed in this slice.
            return;
        }
        self.workbench_mode = WorkbenchMode::Sketch;
        self.show_properties_tab();
    }

    /// Frames the camera onto a sketch plane, the way starting a sketch on a
    /// face already frames the camera onto that face.
    ///
    /// Unlike a face sketch this needs no deferral: a plane is not backed by a
    /// body snapshot, so there is nothing that could go stale while the camera
    /// flies. The sketch opens immediately and only the view animates.
    /// Returns `true` when a flight was scheduled and the caller should defer
    /// opening the sketch until the camera lands.
    fn start_plane_sketch_camera_transition(
        &mut self,
        frame: PlanarFrame3,
        half_extent: f64,
    ) -> bool {
        // Framing the plane is for drawing on it. A solid extruded off that
        // plane is invisible edge-on, so the view the user had is restored
        // when they leave, rather than leaving the model looking flat.
        self.camera_before_plane_sketch.get_or_insert(self.view);
        self.motion.pause();
        self.last_motion_time = None;
        let focus = frame.origin;
        if self.animate_face_camera_transitions {
            if let Some(transition) =
                CameraTransition::face_aligned(self.view, frame, focus, half_extent)
            {
                self.face_camera_transition = Some(transition);
                self.last_face_camera_time = None;
                return true;
            }
        } else if let Some(target) = self.view.face_aligned_target(frame, focus, half_extent) {
            // Instant transitions are the test default and the accessibility
            // fallback; the camera must still arrive, just without the flight.
            self.view = target;
        }
        false
    }

    /// Opens a sketch on an origin plane, once the camera is looking at it.
    fn open_origin_plane_sketch(&mut self, plane: SketchPlane) {
        let _ = self.sketch.set_plane(plane);
        self.sketch_support = SketchSupport::Origin { plane };
        self.face_sketch_context = None;
        self.extrusion_mode = ExtrusionMode::NewBody;
        self.extrusion_mode_explicit = false;
        self.workbench_mode = WorkbenchMode::Sketch;
        self.show_properties_tab();
    }

    fn start_face_sketch_camera_transition(&mut self, support: PlanarFaceSupport) -> bool {
        let Some(body) = self.active_body_id() else {
            return true;
        };
        let Some(snapshot) = self.displayed_snapshot_id() else {
            return true;
        };
        // Face sketching always starts from authored geometry. Animation is a
        // temporary inspection transform and must not leak into either the
        // focus target or the sketch-plane presentation.
        self.motion.pause();
        self.last_motion_time = None;
        let quarter_turns = self
            .view
            .face_sketch_quarter_turn(support.frame)
            .unwrap_or(0);
        let context = self
            .displayed
            .as_ref()
            .and_then(|displayed| project_face_sketch_context(&displayed.scene, &support));
        let fitted_view = context.as_ref().and_then(|context| {
            self.last_model_viewport_size.and_then(|size| {
                sketch::fitted_context_view_with_quarter_turn(
                    size,
                    &context.viewport_context(),
                    quarter_turns,
                )
            })
        });
        let pending = PendingFaceSketch {
            support,
            body,
            snapshot,
            context,
            fitted_view,
        };
        let transition_target = self.face_camera_target(&pending);
        if self.animate_face_camera_transitions
            && let Some((frame, focus, fit_radius)) = transition_target
            && let Some(transition) =
                CameraTransition::face_aligned(self.view, frame, focus, fit_radius)
        {
            self.face_camera_transition = Some(transition);
            self.pending_face_sketch = Some(pending);
            self.last_face_camera_time = None;
            self.active_tool = ActiveTool::Select;
            return true;
        }
        self.begin_face_sketch(pending);
        false
    }

    fn face_camera_target(
        &self,
        pending: &PendingFaceSketch,
    ) -> Option<(PlanarFrame3, Point3, f64)> {
        let pivot = self.body_pivot?;
        let presented_frame = presented_planar_frame(
            pending.support.frame,
            pivot,
            self.display_transform,
            self.motion.phase,
        )?;
        let unit_scale = vector_length(presented_frame.u)?;
        if let (Some(view), Some(size)) = (pending.fitted_view, self.last_model_viewport_size) {
            let raw_focus = frame_point(
                pending.support.frame,
                ProtocolPoint2::new(view.center.u, view.center.v),
            );
            let focus = self
                .display_transform
                .present_point(raw_focus, pivot, self.motion.phase);
            let fit_radius =
                f64::from(size.x.min(size.y)) * 0.34 / view.points_per_unit * unit_scale;
            return (fit_radius.is_finite() && fit_radius > f64::EPSILON).then_some((
                presented_frame,
                focus,
                fit_radius,
            ));
        }
        let (raw_focus, raw_radius) = face_support_focus(&pending.support)?;
        let focus = self
            .display_transform
            .present_point(raw_focus, pivot, self.motion.phase);
        Some((presented_frame, focus, raw_radius * unit_scale))
    }

    fn advance_face_camera_transition(&mut self, context: &egui::Context) -> bool {
        let Some(transition) = self.face_camera_transition.as_mut() else {
            self.last_face_camera_time = None;
            return false;
        };
        let now = context.input(|input| input.time);
        let delta = self
            .last_face_camera_time
            .map_or(0.0, |previous| (now - previous).max(0.0));
        self.last_face_camera_time = Some(now);
        self.view = transition.advance(delta);
        let complete = transition.is_complete();
        context.request_repaint();
        if complete {
            self.face_camera_transition = None;
            self.last_face_camera_time = None;
            if let Some(pending) = self.pending_plane_sketch.take() {
                match pending {
                    PendingPlaneSketch::Origin(plane) => self.open_origin_plane_sketch(plane),
                    PendingPlaneSketch::Construction(id) => {
                        self.open_construction_plane_sketch(id);
                    }
                }
            }
            if let Some(pending) = self.pending_face_sketch.take() {
                if Some(pending.body) == self.active_body_id()
                    && Some(pending.snapshot) == self.displayed_snapshot_id()
                {
                    self.begin_face_sketch(pending);
                    self.workbench_mode = WorkbenchMode::Sketch;
                } else {
                    let actual = self
                        .displayed_snapshot_id()
                        .unwrap_or_else(|| self.empty_snapshot.id());
                    let error = workbench_extrusion_error(
                        KernelErrorCode::StaleSnapshot,
                        actual,
                        "the selected face changed bodies while the sketch camera was moving",
                    );
                    self.sketch_extrusion_issue = Some(error.clone());
                    self.last_attempt = Attempt::Rejected {
                        operation: "Face sketch rejected",
                        error,
                    };
                }
            }
        }
        true
    }

    fn begin_face_sketch(&mut self, pending: PendingFaceSketch) {
        let PendingFaceSketch {
            support,
            body,
            snapshot,
            context,
            fitted_view,
        } = pending;
        self.face_sketch_context = context;
        let display_plane = sketch_plane_for_frame(support.frame);
        self.sketch = SketchCanvasState::new(display_plane);
        self.active_sketch_tool = ToolVariant::Select;
        if let (Some(context), Some(view)) = (&self.face_sketch_context, fitted_view) {
            self.sketch
                .apply_prepared_context_view(view, context.fit_key);
        }
        self.sketch_support = SketchSupport::PlanarFace {
            body,
            snapshot,
            face: support.face,
            frame: Box::new(support.frame),
            boundary: support.boundary,
            inner_boundaries: support.inner_boundaries,
            support_digest: support.support_digest,
        };
        self.active_sketch_index = None;
        self.sketch_revision = 0;
        self.sketch_finished = false;
        self.sketch_last_error = None;
        self.sketch_finish_issue = None;
        self.sketch_extrusion_issue = None;
        self.extruded_sketch_revision = None;
        self.extrusion_mode = ExtrusionMode::Add;
        self.extrusion_mode_explicit = false;
        self.extrusion_distance = 1.0;
        self.feature_preview.begin_new_sketch();
        self.show_properties_tab();
    }

    fn enter_model_mode(&mut self) {
        if self.pending_operation.is_some() {
            return;
        }
        // A first/second creation click is presentation-only and therefore
        // needs no confirmation. It must not survive a mode round trip and
        // become geometry when the user next clicks the sketch canvas.
        self.sketch.clear_creation_draft();
        self.leave_sketch_mode();
    }

    /// Leaves the sketch workspace, handing back any camera it borrowed.
    fn leave_sketch_mode(&mut self) {
        self.workbench_mode = WorkbenchMode::Model;
        self.restore_camera_after_plane_sketch();
    }

    /// Hands back the model camera a plane sketch borrowed.
    fn restore_camera_after_plane_sketch(&mut self) {
        let Some(view) = self.camera_before_plane_sketch.take() else {
            return;
        };
        if self.animate_face_camera_transitions
            && let Some(transition) = CameraTransition::to_view(self.view, view)
        {
            self.face_camera_transition = Some(transition);
            self.last_face_camera_time = None;
            return;
        }
        self.view = view;
    }

    fn frame_active_sketch(&mut self) {
        if self.face_sketch_context.is_some() {
            self.sketch.request_context_fit();
        } else {
            self.sketch.view_mut().reset();
        }
    }

    /// Commits a freshly drawn sketch stroke immediately. Mainstream
    /// sketching flows one stroke into the next with undo as the safety
    /// net, so only typed dimension edits keep an explicit confirmation;
    /// solid operations keep theirs untouched.
    fn commit_sketch_stroke(&mut self, entity: SketchEntityId) {
        if self.pending_operation.is_some() || !self.history_is_at_end() {
            return;
        }
        if self.sketch.pending().map(|edit| edit.subject()) != Some(entity) {
            return;
        }
        // An invalid live dimension keeps the stroke staged behind the
        // explicit gate until it is corrected: committing a degenerate
        // stroke would be worse than one extra confirmation.
        if self.sketch.dimension_error().is_some() {
            self.stage_sketch_edit(entity);
            return;
        }
        match self.sketch.commit_pending() {
            Ok(_) => {
                self.sketch_revision = self.sketch_revision.saturating_add(1);
                self.feature_preview
                    .commit_sketch_revision(self.sketch_revision);
                self.sketch_finished = false;
                self.sync_active_sketch_record();
                self.sketch_last_error = None;
            }
            Err(error) => self.sketch_last_error = Some(error),
        }
    }

    /// Finishes the sketch in one action: stage the document append and
    /// confirm it in the same frame, the way the confirmation-corner tick
    /// works in mainstream sketchers.
    fn finish_sketch_now(&mut self) -> bool {
        self.stage_finish_sketch();
        if matches!(
            self.pending_operation,
            Some(PendingOperation::FinishSketch { .. })
        ) {
            self.confirm_pending_operation()
        } else {
            false
        }
    }

    fn stage_sketch_edit(&mut self, entity: SketchEntityId) {
        if self.pending_operation.is_some() || !self.history_is_at_end() {
            return;
        }
        let Some(pending) = self.sketch.pending() else {
            return;
        };
        if pending.subject() != entity {
            return;
        }
        self.pending_operation = Some(PendingOperation::SketchEdit {
            entity,
            label: pending.label(),
        });
        self.sketch_last_error = None;
        self.sketch_finish_issue = None;
    }

    fn stage_delete_selected_sketch(&mut self) -> bool {
        if self.pending_operation.is_some()
            || !self.history_is_at_end()
            || self.sketch.dimension_editor_active()
        {
            return false;
        }
        let Ok(subject) = self.sketch.stage_delete_selected() else {
            return false;
        };
        self.commit_sketch_stroke(subject);
        !self.sketch.has_pending_edit()
    }

    fn restore_local_sketch_journal(&mut self, redo: bool) -> bool {
        if self.workbench_mode != WorkbenchMode::Sketch || self.pending_operation.is_some() {
            return false;
        }
        let restored = if redo {
            self.sketch.redo_local()
        } else {
            self.sketch.undo_local()
        };
        if !restored {
            return false;
        }
        // `sketch_revision` is the document geometry revision, not the core
        // authoring transaction counter. A local undo/redo is a new editable
        // document state and therefore advances monotonically even when the
        // restored core snapshot carries an older internal revision.
        self.sketch_revision = self.sketch_revision.saturating_add(1);
        self.feature_preview.restore_sketch_revision(
            if self.sketch.authoring().operations().is_empty() {
                0
            } else {
                self.sketch_revision
            },
        );
        self.sketch_finished = false;
        self.extruded_sketch_revision = None;
        self.sync_active_sketch_record();
        self.sketch_last_error = None;
        self.sketch_finish_issue = None;
        self.sketch_extrusion_issue = None;
        true
    }

    fn stage_finish_sketch(&mut self) {
        if self.pending_operation.is_none()
            && self.history_is_at_end()
            && !self.sketch.authoring().operations().is_empty()
        {
            self.pending_operation = Some(PendingOperation::FinishSketch {
                plane: self.sketch.plane(),
                revision: self.sketch_revision,
            });
            self.sketch_finish_issue = None;
        }
    }

    fn stage_sketch_extrusion(&mut self) -> bool {
        if self.pending_operation.is_some()
            || !self.history_is_at_end()
            || self.sketch.has_pending_edit()
            || self.sketch_creation_draft_active()
            || !self.sketch_extrusion_eligibility().can_stage()
            || !self.extrusion_distance_is_valid()
        {
            return false;
        }
        let base_snapshot = self.active_snapshot_id_or_empty();
        let target_face = self.sketch_support.target_face();
        let mode = if target_face.is_some() {
            self.extrusion_mode
        } else {
            ExtrusionMode::NewBody
        };
        // Extrusion owns pointer interaction until it is confirmed or
        // cancelled. Do not leave a transform drag tool armed behind the
        // feature preview, where its presentation-only change would be lost
        // when the B-rep publishes.
        self.active_tool = ActiveTool::Select;
        // No provisional creation gesture may survive into an extrusion
        // preview, even though a certified committed profile is exportable.
        self.sketch.clear_creation_draft();
        let finish_sketch_on_commit = !self.sketch_finished;
        // Cancelling an unfinished sketch's compound preview must reopen that
        // sketch even if the user briefly switched to Model before Extrude.
        let cancel_mode = if finish_sketch_on_commit {
            WorkbenchMode::Sketch
        } else {
            self.workbench_mode
        };
        self.pending_operation = Some(PendingOperation::ExtrudeSketch {
            base_snapshot,
            support_body: self.sketch_support.body(),
            plane: self.sketch.plane(),
            revision: self.sketch_revision,
            cancel_mode,
            finish_sketch_on_commit,
            distance: self.extrusion_distance,
            frame: self.sketch_support.frame(),
            target_face,
            support_digest: self.sketch_support.support_digest(),
            mode,
        });
        self.sketch_extrusion_issue = None;
        self.leave_sketch_mode();
        true
    }

    fn selected_face_push_pull_support(&self) -> Option<PlanarFaceSupport> {
        let face = self.selected_face?;
        let body = self.displayed.as_ref()?;
        let support = NativeKernel::planar_face_support(&body.snapshot, face).ok()?;
        (support.inner_boundaries.is_empty() && support.boundary.len() >= 3).then_some(support)
    }

    fn stage_face_push_pull(&mut self) -> bool {
        if self.pending_operation.is_some()
            || !self.history_is_at_end()
            || self.workbench_mode != WorkbenchMode::Model
            || !self.extrusion_distance.is_finite()
            || self.extrusion_distance.abs() <= f64::EPSILON
        {
            return false;
        }
        let Some(support) = self.selected_face_push_pull_support() else {
            return false;
        };
        let Some(support_body) = self.active_body_id() else {
            return false;
        };
        self.active_tool = ActiveTool::Select;
        self.extrusion_mode = if self.extrusion_distance > 0.0 {
            ExtrusionMode::Add
        } else {
            ExtrusionMode::Cut
        };
        self.extrusion_mode_explicit = false;
        self.pending_operation = Some(PendingOperation::PushPullFace {
            base_snapshot: support.face.snapshot,
            support_body,
            target_face: support.face,
            distance: self.extrusion_distance,
        });
        self.sketch_extrusion_issue = None;
        true
    }

    fn sync_pending_sketch_extrusion_inputs(&mut self) {
        match self.pending_operation.as_mut() {
            Some(PendingOperation::ExtrudeSketch {
                distance,
                mode,
                target_face,
                ..
            }) => {
                *distance = self.extrusion_distance;
                *mode = if target_face.is_some() {
                    self.extrusion_mode
                } else {
                    ExtrusionMode::NewBody
                };
            }
            Some(PendingOperation::PushPullFace { distance, .. }) => {
                *distance = self.extrusion_distance;
            }
            _ => {}
        }
    }

    /// Exact, renderer-independent modeling payload for the committed sketch.
    fn current_canvas_profile_payload(&self) -> Option<PlanarProfile2> {
        if self.sketch.available_region_count() > 0 {
            return self.sketch.selected_planar_profile();
        }
        self.sketch
            .certified_sketch_profile()
            .as_ref()
            .and_then(protocol_planar_profile)
            .or_else(|| compile_single_authoring_region(self.sketch.authoring()))
    }

    /// Exact, renderer-independent modeling payload for the committed sketch.
    #[must_use]
    pub fn sketch_planar_profile_payload(&self) -> Option<PlanarProfile2> {
        let current = self.current_canvas_profile_payload();
        if current.is_some() || !self.sketch.authoring().operations().is_empty() {
            current
        } else {
            self.active_sketch_index
                .and_then(|index| self.sketches.get(index))
                .and_then(|sketch| sketch.portable_payload.as_ref())
                .and_then(|payload| {
                    (!payload.profile.regions.is_empty()).then(|| payload.profile.clone())
                })
        }
    }

    /// Declarative command represented by the current extrusion preview.
    ///
    /// This is intentionally inspectable by semantic UI tests. It does not
    /// execute or publish anything; Enter/the green tick remains the sole
    /// commit path.
    #[must_use]
    pub fn pending_sketch_extrusion_command(&self) -> Option<KernelCommand> {
        let PendingOperation::ExtrudeSketch {
            frame,
            target_face,
            distance,
            mode,
            ..
        } = self.pending_operation?
        else {
            return None;
        };
        build_planar_profile_extrusion_command(
            frame,
            self.sketch_planar_profile_payload()?,
            target_face,
            distance,
            mode,
        )
    }

    fn current_feature_preview(&self) -> Option<viewport::FeaturePreview> {
        let (regions, direction, distance, mode) = match self.pending_operation? {
            PendingOperation::ExtrudeSketch {
                frame,
                distance,
                mode,
                ..
            } => {
                let profile = self.sketch_planar_profile_payload()?;
                (
                    preview_planar_profile_regions(frame, &profile)?,
                    frame_normal(frame)?,
                    distance,
                    mode,
                )
            }
            PendingOperation::PushPullFace {
                target_face,
                distance,
                ..
            } => {
                let body = self.displayed.as_ref()?;
                let support =
                    NativeKernel::planar_face_support(&body.snapshot, target_face).ok()?;
                let profile = support
                    .boundary
                    .into_iter()
                    .map(|point| frame_point(support.frame, point))
                    .collect::<Vec<_>>();
                let mode = if distance < 0.0 {
                    ExtrusionMode::Cut
                } else {
                    ExtrusionMode::Add
                };
                (
                    vec![viewport::FeaturePreviewRegion::new(profile, Vec::new())],
                    frame_normal(support.frame)?,
                    distance,
                    mode,
                )
            }
            _ => return None,
        };
        let style = match mode {
            ExtrusionMode::NewBody => viewport::FeaturePreviewStyle::Neutral,
            ExtrusionMode::Add => viewport::FeaturePreviewStyle::Add,
            ExtrusionMode::Cut => viewport::FeaturePreviewStyle::Cut,
        };
        Some(viewport::FeaturePreview::planar_regions(
            regions, direction, distance, style,
        ))
    }

    fn feature_preview_for_frame(
        &mut self,
        context: &egui::Context,
    ) -> Option<viewport::FeaturePreview> {
        let scheduler = self.feature_preview_scheduler.clone();
        let Some(PendingOperation::ExtrudeSketch {
            base_snapshot,
            support_body,
            frame,
            target_face,
            distance,
            mode,
            ..
        }) = self.pending_operation
        else {
            self.async_feature_preview_intent = None;
            self.async_feature_preview_job = None;
            self.async_feature_preview_cache = None;
            return self.current_feature_preview();
        };
        let profile = self.sketch_planar_profile_payload()?;
        let source = support_body.and_then(|body_id| {
            self.bodies
                .iter()
                .find(|body| body.id == body_id && body.body.snapshot.id() == base_snapshot)
                .map(|body| (body.body.snapshot.clone(), body.body.scene.clone()))
        });
        let intent = AsyncFeaturePreviewIntent {
            input_snapshot: source.as_ref().map(|(snapshot, _)| snapshot.id()),
            frame,
            profile,
            target_face,
            distance,
            mode,
        };

        if self.async_feature_preview_intent.as_ref() != Some(&intent) {
            if let Some(scheduler) = scheduler {
                let job_intent = intent.clone();
                let job_source = source.clone();
                self.async_feature_preview_job = Some(scheduler.submit(
                    JobPriority::InteractivePreview,
                    Some("model.feature-preview"),
                    move |cancellation| {
                        build_async_feature_preview(
                            &job_intent,
                            job_source.as_ref().map(|(snapshot, _)| snapshot),
                            job_source.as_ref().map(|(_, scene)| scene),
                            Some(&cancellation),
                        )
                    },
                ));
            } else {
                self.async_feature_preview_cache = build_async_feature_preview(
                    &intent,
                    source.as_ref().map(|(snapshot, _)| snapshot),
                    source.as_ref().map(|(_, scene)| scene),
                    None,
                );
            }
            self.async_feature_preview_intent = Some(intent);
        }

        if let Some(result) = self
            .async_feature_preview_job
            .as_ref()
            .and_then(JobHandle::try_take)
        {
            self.async_feature_preview_job = None;
            self.async_feature_preview_cache = result.ok().flatten();
        }
        if self.async_feature_preview_job.is_some() {
            // Poll background preview completion at the display cadence. A
            // four-millisecond timer forced needless 250 Hz viewport work
            // while the Boolean worker was already busy, which made orbiting
            // a staged complex cut visibly contend with its own preview.
            context.request_repaint_after(std::time::Duration::from_millis(16));
        }
        let style = match mode {
            ExtrusionMode::NewBody => viewport::FeaturePreviewStyle::Neutral,
            ExtrusionMode::Add => viewport::FeaturePreviewStyle::Add,
            ExtrusionMode::Cut => viewport::FeaturePreviewStyle::Cut,
        };
        // Keep the last valid sampled profile visible while its replacement
        // is calculated. Most drag frames change only distance/style, which
        // can be applied immediately and must never tear down pointer capture.
        self.async_feature_preview_cache
            .clone()
            .map(|preview| preview.with_presentation(distance, style))
            .or_else(|| self.current_feature_preview())
    }

    #[cfg(test)]
    fn current_edge_finish_preview(&mut self) -> Option<viewport::EdgeFinishPreview> {
        self.edge_finish_preview_for_frame(None)
    }

    fn edge_finish_preview_for_frame(
        &mut self,
        context: Option<&egui::Context>,
    ) -> Option<viewport::EdgeFinishPreview> {
        let PendingOperation::PresetFeature {
            preset,
            base_snapshot,
            body: Some(body_id),
            ..
        } = self.pending_operation?
        else {
            self.async_edge_finish_preview_intent = None;
            self.async_edge_finish_preview_job = None;
            self.async_edge_finish_preview_cache = None;
            return None;
        };
        let (label, kind) = match preset {
            SolidFeaturePreset::Chamfer => ("CHAMFER", EdgeFinishKind::Chamfer),
            SolidFeaturePreset::Fillet => ("RADIUS", EdgeFinishKind::Fillet),
            _ => {
                self.async_edge_finish_preview_intent = None;
                self.async_edge_finish_preview_job = None;
                self.async_edge_finish_preview_cache = None;
                return None;
            }
        };
        let body_key = viewport::BodyInstanceKey::new(body_id.get());
        let (source_segments, live_frames) = {
            let body = self.bodies.iter().find(|body| body.id == body_id)?;
            if body.body.snapshot.id() != base_snapshot {
                return None;
            }
            let segments = self
                .selected_edges
                .iter()
                .filter(|selection| selection.body == body_key)
                .flat_map(|selection| {
                    body.body
                        .scene
                        .edges
                        .iter()
                        .filter(|edge| edge.source_edge == selection.edge && !edge.is_smooth)
                        .map(|edge| edge.endpoints)
                })
                .collect::<Vec<_>>();
            if segments.is_empty() {
                return None;
            }
            let frames = segments
                .iter()
                .copied()
                .filter_map(|segment| viewport::edge_finish_live_frame(&body.body.scene, segment))
                .collect::<Vec<_>>();
            (segments, frames)
        };
        let target_edges = self
            .selected_edges
            .iter()
            .filter(|selection| selection.body == body_key)
            .map(|selection| selection.edge)
            .collect::<Vec<_>>();
        if target_edges.is_empty() || target_edges.len() != self.selected_edges.len() {
            return None;
        }
        let intent = AsyncEdgeFinishPreviewIntent {
            input_snapshot: base_snapshot,
            body: body_key,
            target_edges,
            kind,
            distance: self.edge_finish_distance,
        };

        if let Some(scheduler) = self.feature_preview_scheduler.clone() {
            if self.async_edge_finish_preview_intent.as_ref() != Some(&intent) {
                let structurally_compatible = self
                    .async_edge_finish_preview_intent
                    .as_ref()
                    .is_some_and(|previous| {
                        previous.input_snapshot == intent.input_snapshot
                            && previous.body == intent.body
                            && previous.target_edges == intent.target_edges
                            && previous.kind == intent.kind
                    });
                if !structurally_compatible {
                    self.async_edge_finish_preview_cache = None;
                }
                let (input, source_scene) = {
                    let body = self.bodies.iter().find(|body| body.id == body_id)?;
                    (body.body.snapshot.clone(), body.body.scene.clone())
                };
                let job_intent = intent.clone();
                self.async_edge_finish_preview_job = Some(scheduler.submit(
                    JobPriority::InteractivePreview,
                    Some("model.edge-finish-preview"),
                    move |cancellation| {
                        if cancellation.is_cancelled() {
                            return None;
                        }
                        build_exact_edge_finish_preview(
                            &input,
                            &source_scene,
                            &job_intent,
                            Some(&cancellation),
                        )
                    },
                ));
                self.async_edge_finish_preview_intent = Some(intent);
            }
            if let Some(result) = self
                .async_edge_finish_preview_job
                .as_ref()
                .and_then(JobHandle::try_take)
            {
                self.async_edge_finish_preview_job = None;
                self.async_edge_finish_preview_cache = result.ok().flatten().map(Arc::new);
            }
            if self.async_edge_finish_preview_job.is_some()
                && let Some(context) = context
            {
                context.request_repaint_after(std::time::Duration::from_millis(4));
            }
        } else if self.async_edge_finish_preview_intent.as_ref() != Some(&intent) {
            let (input, source_scene) = {
                let body = self.bodies.iter().find(|body| body.id == body_id)?;
                (body.body.snapshot.clone(), body.body.scene.clone())
            };
            self.async_edge_finish_preview_cache =
                build_exact_edge_finish_preview(&input, &source_scene, &intent, None).map(Arc::new);
            self.async_edge_finish_preview_intent = Some(intent);
        }

        Some(viewport::EdgeFinishPreview {
            body: body_key,
            edges: self.selected_edges.clone(),
            source_segments,
            live_frames,
            distance: self.edge_finish_distance,
            label,
            kind,
            candidate: self.async_edge_finish_preview_cache.clone(),
        })
    }

    fn edge_finish_selection_support(&self) -> EdgeFinishSelectionSupport {
        if self.selected_edges.is_empty() {
            return EdgeFinishSelectionSupport::Empty;
        }
        let Some(body_id) = self.active_body_id() else {
            return EdgeFinishSelectionSupport::MixedBodies;
        };
        let Some(body) = self.bodies.iter().find(|body| body.id == body_id) else {
            return EdgeFinishSelectionSupport::MixedBodies;
        };
        if self
            .selected_edges
            .iter()
            .any(|selection| selection.body.get() != body_id.get())
        {
            return EdgeFinishSelectionSupport::MixedBodies;
        }
        let counts = body.body.snapshot.counts();
        let pristine_prism = counts.solids == 1 && counts.faces == 6;

        let tolerance = body
            .body
            .snapshot
            .precision_policy()
            .unwrap_or_default()
            .linear_agreement;
        // A whole cap rim — of any profile, straight or arced — enters the
        // exact rim-loop path, so recognise it before the per-edge analysis.
        if let Some(loop_edges) = self
            .selected_edges
            .first()
            .and_then(|seed| NativeKernel::rim_loop_group(&body.body.snapshot, seed.edge).ok())
        {
            let members = loop_edges
                .iter()
                .map(|member| member.entity.0)
                .collect::<BTreeSet<_>>();
            let chosen = self
                .selected_edges
                .iter()
                .map(|selection| selection.edge.entity.0)
                .collect::<BTreeSet<_>>();
            if members.len() > 1 && members == chosen {
                return EdgeFinishSelectionSupport::ExactRimBlend;
            }
        }
        // A selection consisting entirely of complete circular rims (each
        // carrier group fully selected) enters the exact torus rim-blend
        // path; the decision is topological, never sampled.
        let mut rim_members = BTreeSet::new();
        let mut carriers_complete = true;
        let mut any_circle = false;
        for selection in &self.selected_edges {
            let group = NativeKernel::carrier_edge_group(&body.body.snapshot, selection.edge)
                .unwrap_or_default();
            if group.len() > 1 {
                any_circle = true;
                let all_selected = group.iter().all(|member| {
                    self.selected_edges
                        .iter()
                        .any(|candidate| candidate.edge == *member)
                });
                if !all_selected {
                    carriers_complete = false;
                }
                rim_members.extend(group.into_iter().map(|member| member.entity.0));
            } else {
                carriers_complete = false;
            }
        }
        if any_circle {
            let only_rims = self
                .selected_edges
                .iter()
                .all(|selection| rim_members.contains(&selection.edge.entity.0));
            return if carriers_complete && only_rims {
                EdgeFinishSelectionSupport::ExactRimBlend
            } else {
                EdgeFinishSelectionSupport::CurvedOrPartialEdge
            };
        }

        let mut axes = BTreeSet::new();
        let mut all_world_axis_aligned = true;
        for selection in &self.selected_edges {
            let segments = body
                .body
                .scene
                .edges
                .iter()
                .filter(|edge| edge.source_edge == selection.edge && !edge.is_smooth)
                .collect::<Vec<_>>();
            if segments.is_empty() {
                return EdgeFinishSelectionSupport::CurvedOrPartialEdge;
            }
            for segment in segments {
                let delta = [
                    (segment.endpoints[1].x - segment.endpoints[0].x).abs(),
                    (segment.endpoints[1].y - segment.endpoints[0].y).abs(),
                    (segment.endpoints[1].z - segment.endpoints[0].z).abs(),
                ];
                let varying = delta
                    .iter()
                    .enumerate()
                    .filter_map(|(axis, length)| (*length > tolerance).then_some(axis))
                    .collect::<Vec<_>>();
                let length = delta[0].hypot(delta[1]).hypot(delta[2]);
                if !length.is_finite() || length <= tolerance {
                    return EdgeFinishSelectionSupport::CurvedOrPartialEdge;
                }
                if let [axis] = varying.as_slice() {
                    axes.insert(*axis);
                } else {
                    all_world_axis_aligned = false;
                }
            }
        }
        if pristine_prism && all_world_axis_aligned && axes.len() == 1 {
            EdgeFinishSelectionSupport::ExactParallelSet
        } else {
            EdgeFinishSelectionSupport::RegularizedBlendSet
        }
    }

    fn confirm_pending_operation(&mut self) -> bool {
        if self.async_sketch_extrusion_commit.is_some() {
            return true;
        }
        let Some(pending) = self.pending_operation else {
            return false;
        };
        if let Some(recorder) = self.development_recorder.as_ref() {
            recorder.log("operation.intent", pending.trace_payload());
        }
        match pending {
            PendingOperation::Transform { .. } => self.apply_transform_preview(),
            PendingOperation::ComponentPlacement { .. } => {
                self.apply_component_placement_preview();
            }
            PendingOperation::SetComponentGrounded {
                component,
                base_grounded,
                grounded,
            } => self.apply_component_grounding(component, base_grounded, grounded),
            PendingOperation::CreateRevoluteJoint { component } => {
                self.apply_revolute_joint(component);
            }
            PendingOperation::RunCase {
                case,
                base_snapshot,
            } => {
                self.execute_case(case, Some(base_snapshot));
            }
            PendingOperation::LibraryInsertion { staging_id } => {
                self.execute_library_insertion(staging_id);
            }
            PendingOperation::LoadDefaultDocument => {
                let path = self.document_path.clone();
                self.pending_operation = None;
                if let Err(error) = self.load_workspace_from_path(&path) {
                    self.pending_operation = Some(PendingOperation::LoadDefaultDocument);
                    self.document_status = Some(format!("Open failed: {error}"));
                }
            }
            PendingOperation::SetParameterLiteral {
                parameter, value, ..
            } => {
                match self
                    .document
                    .set_parameter_binding(parameter, ParameterBinding::literal(value.into_value()))
                {
                    Ok(_) => {
                        self.pending_operation = None;
                        self.rebuild_after_parameter_change();
                    }
                    Err(error) => {
                        self.document_status = Some(format!("Parameter update rejected: {error}"));
                    }
                }
            }
            PendingOperation::AddUserLengthParameter { ordinal, value_mm } => {
                let key = format!("UserLength{ordinal}");
                let metadata = ParameterMetadata {
                    exposure: ParameterExposure::UserInput,
                    description: Some("Reusable document length".to_owned()),
                    ..ParameterMetadata::default()
                };
                let spec = ParameterSpec::new(
                    key.clone(),
                    format!("User length {ordinal}"),
                    ParameterType::Quantity(QuantityKind::Length),
                )
                .with_display_unit(ParameterUnit::Millimeter)
                .with_metadata(metadata);
                match self.document.add_parameter(
                    spec,
                    ParameterBinding::literal(ParameterValue::quantity(
                        value_mm,
                        ParameterUnit::Millimeter,
                    )),
                ) {
                    Ok(_) => {
                        self.pending_operation = None;
                        self.history_scrub_position = self.document.history_position();
                        self.document_status = Some(format!("Parameter {key} added"));
                    }
                    Err(error) => {
                        self.document_status =
                            Some(format!("Parameter creation rejected: {error}"));
                    }
                }
            }
            PendingOperation::CreateConstructionPlane {
                frame,
                half_u,
                half_v,
                source,
            } => self.commit_construction_plane(frame, half_u, half_v, source),
            PendingOperation::BooleanBodies {
                target,
                operation,
                keep_tools,
            } => {
                let tools = std::mem::take(&mut self.boolean_tools);
                self.execute_body_boolean(target, &tools, operation, keep_tools);
                if self.pending_operation.is_some() {
                    // The kernel or history refused: keep the picks so the
                    // user can adjust an operand instead of starting over.
                    self.boolean_tools = tools;
                }
            }
            PendingOperation::PresetFeature {
                preset,
                base_snapshot,
                body,
                target_face,
                frame,
            } => {
                if matches!(
                    preset,
                    SolidFeaturePreset::Chamfer | SolidFeaturePreset::Fillet
                ) {
                    let support = self.edge_finish_selection_support();
                    if !support.can_commit() {
                        self.document_status = Some(support.detail().to_owned());
                        return true;
                    }
                }
                self.execute_preset_feature(preset, base_snapshot, body, target_face, frame);
            }
            PendingOperation::SketchEdit { entity, .. } => {
                let staged_entity = self.sketch.pending().map(|edit| edit.subject());
                if staged_entity != Some(entity) {
                    self.sketch_last_error = Some(SketchEditError::NoPendingEdit);
                } else {
                    match self.sketch.commit_pending() {
                        Ok(_) => {
                            // This is the document-facing geometry revision.
                            // Keep it monotonic across edits of a hydrated
                            // sketch; the authoring graph owns its own revision
                            // counter inside the portable payload.
                            self.sketch_revision = self.sketch_revision.saturating_add(1);
                            self.feature_preview
                                .commit_sketch_revision(self.sketch_revision);
                            self.sketch_finished = false;
                            self.sync_active_sketch_record();
                            self.sketch_last_error = None;
                            self.pending_operation = None;
                        }
                        Err(error) => self.sketch_last_error = Some(error),
                    }
                }
            }
            PendingOperation::FinishSketch { plane, revision } => {
                let profile = self.sketch.certified_profile_status();
                let authoring_valid = !self.sketch.authoring().operations().is_empty()
                    && self
                        .sketch
                        .authoring()
                        .validate(PrecisionPolicy::default())
                        .is_ok();
                if plane == self.sketch.plane()
                    && revision == self.sketch_revision
                    && authoring_valid
                {
                    self.sync_active_sketch_record();
                    let mut next_document = self.document.clone();
                    let document_binding =
                        self.append_active_sketch_to_document(&mut next_document);
                    let Ok((sketch_id, sketch_feature)) = document_binding else {
                        self.document_status = Some(format!(
                            "Sketch history rejected: {}",
                            document_binding.expect_err("the result is known to be an error")
                        ));
                        return true;
                    };
                    self.document = next_document;
                    self.sketch_revision = self
                        .document
                        .sketch(sketch_id)
                        .map_or(self.sketch_revision, |record| record.geometry_revision);
                    self.history_scrub_position = self.document.history_position();
                    self.sketch_finished = true;
                    self.feature_preview
                        .commit_sketch_revision(self.sketch_revision);
                    self.feature_preview.finish_active_sketch();
                    self.sync_active_sketch_record();
                    self.bind_active_sketch_document_ids(sketch_id, sketch_feature);
                    self.selected_history_feature = Some(sketch_feature);
                    self.document_status = Some("Sketch committed to history".to_owned());
                    self.sketch_finish_issue = None;
                    self.pending_operation = None;
                    self.leave_sketch_mode();
                } else {
                    self.sketch_finish_issue = Some(if authoring_valid {
                        profile
                    } else {
                        CertifiedProfileStatus::Indeterminate
                    });
                }
            }
            pending @ PendingOperation::ExtrudeSketch { .. } => {
                self.execute_sketch_extrusion(pending);
            }
            pending @ PendingOperation::PushPullFace { .. } => {
                self.execute_face_push_pull(pending);
            }
        }
        true
    }

    fn cancel_pending_operation(&mut self) -> bool {
        let Some(pending) = self.pending_operation else {
            return false;
        };
        if let Some(recorder) = self.development_recorder.as_ref() {
            recorder.log_critical("operation.cancel", pending.trace_payload());
        }
        match pending {
            PendingOperation::Transform { .. } => self.clear_transform_preview(),
            PendingOperation::ComponentPlacement { .. } => self.clear_transform_preview(),
            PendingOperation::SetComponentGrounded { .. }
            | PendingOperation::CreateRevoluteJoint { .. } => self.pending_operation = None,
            PendingOperation::RunCase { .. } => self.pending_operation = None,
            PendingOperation::LibraryInsertion { staging_id } => {
                if self.part_library.cancel_staged(staging_id) {
                    self.pending_operation = None;
                }
            }
            PendingOperation::LoadDefaultDocument => self.pending_operation = None,
            PendingOperation::BooleanBodies { .. } => {
                self.boolean_tools.clear();
                self.pending_operation = None;
            }
            PendingOperation::SetParameterLiteral { .. }
            | PendingOperation::AddUserLengthParameter { .. }
            | PendingOperation::CreateConstructionPlane { .. } => self.pending_operation = None,
            PendingOperation::PresetFeature { .. } => {
                self.staged_revolve = None;
                self.pending_operation = None;
            }
            PendingOperation::SketchEdit { .. } => {
                self.sketch.cancel_pending();
                self.sketch_last_error = None;
                self.pending_operation = None;
            }
            PendingOperation::FinishSketch { .. } => {
                self.sketch_finish_issue = None;
                self.pending_operation = None;
            }
            PendingOperation::ExtrudeSketch { cancel_mode, .. } => {
                self.cancel_async_sketch_extrusion_commit();
                self.sketch_extrusion_issue = None;
                self.pending_operation = None;
                self.workbench_mode = cancel_mode;
            }
            PendingOperation::PushPullFace { .. } => {
                self.sketch_extrusion_issue = None;
                self.pending_operation = None;
            }
        }
        true
    }

    fn execute_sketch_extrusion(&mut self, pending: PendingOperation) {
        let PendingOperation::ExtrudeSketch {
            base_snapshot,
            support_body,
            plane,
            revision,
            cancel_mode: _,
            finish_sketch_on_commit,
            distance,
            frame,
            target_face,
            support_digest,
            mode,
        } = pending
        else {
            unreachable!("only a staged sketch extrusion can reach extrusion execution")
        };
        let displayed_snapshot = self.displayed_snapshot_id();
        let actual_snapshot = self.active_snapshot_id_or_empty();
        if actual_snapshot != base_snapshot
            || support_body != self.sketch_support.body()
            || support_body.is_some() && support_body != self.active_body_id()
        {
            let mut error = workbench_extrusion_error(
                KernelErrorCode::StaleSnapshot,
                actual_snapshot,
                "the staged extrusion targets a body or snapshot that is no longer active",
            );
            error
                .details
                .insert("expected_snapshot".to_owned(), base_snapshot.to_string());
            error
                .details
                .insert("actual_snapshot".to_owned(), actual_snapshot.to_string());
            error.details.insert(
                "displayed_snapshot".to_owned(),
                displayed_snapshot.map_or_else(|| "empty".to_owned(), |id| id.to_string()),
            );
            error.details.insert(
                "expected_body".to_owned(),
                support_body.map_or_else(|| "new body".to_owned(), |body| body.to_string()),
            );
            error.details.insert(
                "actual_body".to_owned(),
                self.active_body_id()
                    .map_or_else(|| "none".to_owned(), |body| body.to_string()),
            );
            self.reject_staged_sketch_extrusion(error);
            return;
        }

        let expected_finished_state = !finish_sketch_on_commit;
        if self.sketch_finished != expected_finished_state
            || self.sketch.plane() != plane
            || self.sketch_revision != revision
        {
            let mut error = workbench_extrusion_error(
                KernelErrorCode::StaleSnapshot,
                base_snapshot,
                "the staged extrusion no longer matches the finished sketch revision or plane",
            );
            error
                .details
                .insert("expected_revision".to_owned(), revision.to_string());
            error.details.insert(
                "actual_revision".to_owned(),
                self.sketch_revision.to_string(),
            );
            error
                .details
                .insert("expected_plane".to_owned(), format!("{plane:?}"));
            error.details.insert(
                "actual_plane".to_owned(),
                format!("{:?}", self.sketch.plane()),
            );
            self.reject_staged_sketch_extrusion(error);
            return;
        }

        if self.sketch_support.frame() != frame
            || self.sketch_support.target_face() != target_face
            || self.sketch_support.support_digest() != support_digest
            || self.sketch_support.body() != support_body
        {
            self.reject_staged_sketch_extrusion(workbench_extrusion_error(
                KernelErrorCode::StaleSnapshot,
                base_snapshot,
                "the staged extrusion no longer matches its sketch support",
            ));
            return;
        }

        let support_mode_valid = matches!(
            (target_face, mode),
            (None, ExtrusionMode::NewBody) | (Some(_), ExtrusionMode::Add | ExtrusionMode::Cut)
        );
        if !support_mode_valid {
            self.reject_staged_sketch_extrusion(workbench_extrusion_error(
                KernelErrorCode::InvalidInput,
                base_snapshot,
                "the staged extrusion mode is incompatible with its sketch support",
            ));
            return;
        }

        let distance_valid = distance.is_finite()
            && match target_face {
                Some(_) => distance.abs() > f64::EPSILON,
                None => distance > 0.0,
            };
        if !distance_valid {
            self.reject_staged_sketch_extrusion(workbench_extrusion_error(
                KernelErrorCode::InvalidInput,
                base_snapshot,
                "the staged extrusion distance must be finite and non-zero",
            ));
            return;
        }

        let eligibility = self.sketch_extrusion_eligibility();
        if !eligibility.can_stage() {
            self.reject_staged_sketch_extrusion(workbench_extrusion_error(
                eligibility.rejection_code(),
                base_snapshot,
                eligibility.visible_reason().unwrap_or_else(|| {
                    "the staged profile is not eligible for extrusion".to_owned()
                }),
            ));
            return;
        }

        let Some(profile) = self.sketch_planar_profile_payload() else {
            self.reject_staged_sketch_extrusion(workbench_extrusion_error(
                KernelErrorCode::InvalidInput,
                base_snapshot,
                "the staged extrusion profile is no longer available",
            ));
            return;
        };

        self.request_serial += 1;
        let empty_input = NativeKernel::empty();
        let input = match target_face {
            Some(_) => self
                .displayed
                .as_ref()
                .map(|body| &body.snapshot)
                .expect("a face-supported extrusion has a displayed body"),
            None => &empty_input,
        };
        let Some(command) =
            build_planar_profile_extrusion_command(frame, profile, target_face, distance, mode)
        else {
            self.reject_staged_sketch_extrusion(workbench_extrusion_error(
                KernelErrorCode::InvalidInput,
                base_snapshot,
                "the staged extrusion mode is incompatible with its planar profile support",
            ));
            return;
        };
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new(format!("workbench-{}-extrude-sketch", self.request_serial)),
            expected_snapshot: input.id(),
            precision: self
                .displayed
                .as_ref()
                .and_then(|body| body.snapshot.precision_policy())
                .unwrap_or_default(),
            command,
        };
        let replay_command = request.command.clone();

        if let Some(scheduler) = self.feature_preview_scheduler.clone() {
            if self.async_sketch_extrusion_commit.is_some() {
                return;
            }
            let input = input.clone();
            let operation_id = request.request_id.to_string();
            let submitted = Instant::now();
            let kernel_cancellation = CancellationToken::new();
            let job_cancellation = kernel_cancellation.clone();
            let job = scheduler.submit(JobPriority::Commit, None, move |_| {
                let execution_started = Instant::now();
                let result = NativeKernel::execute(&input, &request, &job_cancellation);
                TimedSketchExtrusionExecution {
                    queue_wait: execution_started.duration_since(submitted),
                    execution: execution_started.elapsed(),
                    result,
                }
            });
            if let Some(recorder) = self.development_recorder.as_ref() {
                recorder.log(
                    "operation.start",
                    serde_json::json!({
                        "operation_id": &operation_id,
                        "command": "planar_profile_extrusion",
                        "input_snapshot": base_snapshot.to_string(),
                        "distance_mm": distance,
                        "mode": format!("{mode:?}"),
                        "sketch_revision": revision
                    }),
                );
            }
            self.async_sketch_extrusion_commit = Some(AsyncSketchExtrusionCommit {
                pending,
                replay_command,
                operation_id,
                started: Instant::now(),
                kernel_cancellation,
                job,
            });
            self.document_status = Some("Computing extrusion…".to_owned());
            return;
        }

        let result = NativeKernel::execute(input, &request, &CancellationToken::new());
        self.apply_sketch_extrusion_result(pending, replay_command, result);
    }

    fn apply_sketch_extrusion_result(
        &mut self,
        pending: PendingOperation,
        replay_command: KernelCommand,
        result: Result<ExecutionOutcome, KernelError>,
    ) {
        let PendingOperation::ExtrudeSketch {
            base_snapshot,
            revision,
            finish_sketch_on_commit,
            mode,
            ..
        } = pending
        else {
            return;
        };
        match result {
            Ok(outcome) => {
                let mut next_document = self.document.clone();
                let sketch_binding = self.append_active_sketch_to_document(&mut next_document);
                let Ok((sketch_id, sketch_feature)) = sketch_binding else {
                    self.reject_staged_sketch_extrusion(workbench_extrusion_error(
                        KernelErrorCode::InternalFailure,
                        base_snapshot,
                        format!(
                            "the sketch could not enter parametric history: {}",
                            sketch_binding.expect_err("the result is known to be an error")
                        ),
                    ));
                    return;
                };
                let feature_binding = self.append_extrusion_to_document(
                    &mut next_document,
                    sketch_id,
                    replay_command,
                    &outcome.report,
                    mode,
                );
                let Ok((feature_id, _created_body)) = feature_binding else {
                    self.reject_staged_sketch_extrusion(workbench_extrusion_error(
                        KernelErrorCode::InternalFailure,
                        base_snapshot,
                        format!(
                            "the extrusion could not enter parametric history: {}",
                            feature_binding.expect_err("the result is known to be an error")
                        ),
                    ));
                    return;
                };
                if let Err(error) =
                    next_document.auto_hide_sketch_consumed_by(sketch_id, feature_id)
                {
                    self.reject_staged_sketch_extrusion(workbench_extrusion_error(
                        KernelErrorCode::InternalFailure,
                        base_snapshot,
                        format!("the consumed sketch could not be hidden atomically: {error}"),
                    ));
                    return;
                }
                let scene = NativeKernel::debug_scene(&outcome.snapshot);
                let bounds = outcome.report.bounds;
                self.document = next_document;
                self.history_scrub_position = self.document.history_position();
                self.bind_active_sketch_document_ids(sketch_id, sketch_feature);
                self.archive_feature_report(feature_id, outcome.report.clone());
                self.displayed = Some(DisplayedBody {
                    snapshot: outcome.snapshot,
                    report: outcome.report,
                    scene,
                });
                self.face_sketch_context = None;
                if let Some(bounds) = bounds {
                    self.body_pivot = Some(bounds_center(bounds));
                    if mode != ExtrusionMode::NewBody {
                        self.view.frame(bounds);
                    }
                }
                self.clear_transform_preview();
                self.pending_operation = None;
                self.selected_face = None;
                self.extruded_sketch_revision = Some(revision);
                self.sketch_extrusion_issue = None;
                if finish_sketch_on_commit {
                    self.sketch_finished = true;
                    self.feature_preview.finish_active_sketch();
                }
                self.model_body_kind = match mode {
                    ExtrusionMode::NewBody => ModelBodyKind::SketchExtrusion,
                    ExtrusionMode::Add => ModelBodyKind::AddedBoss,
                    ExtrusionMode::Cut => ModelBodyKind::CutPocket,
                };
                match mode {
                    ExtrusionMode::NewBody => self.publish_new_body_record(),
                    ExtrusionMode::Add | ExtrusionMode::Cut => self.sync_active_body_record(),
                }
                if mode == ExtrusionMode::NewBody {
                    self.frame_visible_document();
                }
                let active_body = self.active_body_id();
                if let Some(index) = self.active_sketch_index
                    && let Some(sketch) = self.sketches.get_mut(index)
                {
                    sketch.body = active_body;
                }
                self.archive_displayed_body();
                self.consume_active_sketch();
                self.feature_preview.append(match mode {
                    ExtrusionMode::NewBody => FeaturePreviewKind::Extrude,
                    ExtrusionMode::Add => FeaturePreviewKind::Add,
                    ExtrusionMode::Cut => FeaturePreviewKind::Cut,
                });
                self.last_attempt = Attempt::Accepted {
                    operation: match mode {
                        ExtrusionMode::NewBody => "Sketch extrusion committed",
                        ExtrusionMode::Add => "Added extrusion committed",
                        ExtrusionMode::Cut => "Cut extrusion committed",
                    },
                };
                self.selected_history_feature = Some(feature_id);
                self.document_status = Some("Feature regenerated cleanly".to_owned());
            }
            Err(error) => {
                self.sketch_extrusion_issue = Some(error.clone());
                self.last_attempt = Attempt::Rejected {
                    operation: "Sketch extrusion rejected",
                    error,
                };
            }
        }
    }

    fn poll_async_sketch_extrusion_commit(&mut self, context: &egui::Context) {
        let completed = self
            .async_sketch_extrusion_commit
            .as_ref()
            .and_then(|commit| commit.job.try_take());
        if let Some(completed) = completed {
            let commit = self
                .async_sketch_extrusion_commit
                .take()
                .expect("a completed extrusion job remains staged");
            match completed {
                Ok(execution) => {
                    if let Some(recorder) = self.development_recorder.as_ref() {
                        match execution.result.as_ref() {
                            Ok(outcome) => recorder.log(
                                "operation.finish",
                                serde_json::json!({
                                    "operation_id": &commit.operation_id,
                                    "result": "accepted",
                                    "queue_wait_ms": execution.queue_wait.as_secs_f64() * 1_000.0,
                                    "kernel_ms": execution.execution.as_secs_f64() * 1_000.0,
                                    "total_ms": commit.started.elapsed().as_secs_f64() * 1_000.0,
                                    "output_snapshot": outcome.report.output_snapshot.to_string(),
                                    "semantic_digest": outcome.report.semantic_digest.to_string(),
                                    "topology": outcome.report.topology
                                }),
                            ),
                            Err(error) => recorder.log_critical(
                                "operation.finish",
                                serde_json::json!({
                                    "operation_id": &commit.operation_id,
                                    "result": "rejected",
                                    "queue_wait_ms": execution.queue_wait.as_secs_f64() * 1_000.0,
                                    "kernel_ms": execution.execution.as_secs_f64() * 1_000.0,
                                    "total_ms": commit.started.elapsed().as_secs_f64() * 1_000.0,
                                    "error_code": error.code.to_string(),
                                    "stage": format!("{:?}", error.stage),
                                    "diagnostic_count": error.diagnostics.len(),
                                    "diagnostics": error.diagnostics.iter().map(|diagnostic| {
                                        serde_json::json!({
                                            "code": diagnostic.code.to_string(),
                                            "path": &diagnostic.path
                                        })
                                    }).collect::<Vec<_>>()
                                }),
                            ),
                        }
                    }
                    self.apply_sketch_extrusion_result(
                        commit.pending,
                        commit.replay_command,
                        execution.result,
                    );
                }
                Err(JobError::Cancelled) => {
                    if let Some(recorder) = self.development_recorder.as_ref() {
                        recorder.log_critical(
                            "operation.finish",
                            serde_json::json!({
                                "operation_id": &commit.operation_id,
                                "result": "cancelled",
                                "total_ms": commit.started.elapsed().as_secs_f64() * 1_000.0
                            }),
                        );
                    }
                    self.document_status = Some("Extrusion cancelled safely".to_owned());
                }
                Err(JobError::Panicked | JobError::SchedulerStopped) => {
                    if let Some(recorder) = self.development_recorder.as_ref() {
                        recorder.log_critical(
                            "operation.finish",
                            serde_json::json!({
                                "operation_id": &commit.operation_id,
                                "result": "worker_failure",
                                "total_ms": commit.started.elapsed().as_secs_f64() * 1_000.0
                            }),
                        );
                    }
                    let base_snapshot = match commit.pending {
                        PendingOperation::ExtrudeSketch { base_snapshot, .. } => base_snapshot,
                        _ => self.empty_snapshot.id(),
                    };
                    self.reject_staged_sketch_extrusion(workbench_extrusion_error(
                        KernelErrorCode::InternalFailure,
                        base_snapshot,
                        "the modeling worker failed safely; the previous body was retained",
                    ));
                    self.document_status = Some(
                        "Extrusion failed safely · previous body retained · incident details retained"
                            .to_owned(),
                    );
                }
            }
            return;
        }

        if let Some(commit) = self.async_sketch_extrusion_commit.as_ref() {
            let elapsed = commit.started.elapsed();
            self.document_status = Some(if elapsed >= Duration::from_secs(2) {
                format!(
                    "Computing complex extrusion… {:.1}s · the UI remains responsive",
                    elapsed.as_secs_f64()
                )
            } else {
                "Computing extrusion…".to_owned()
            });
            context.request_repaint_after(Duration::from_millis(16));
        }
    }

    fn cancel_async_sketch_extrusion_commit(&mut self) {
        if let Some(commit) = self.async_sketch_extrusion_commit.take() {
            if let Some(recorder) = self.development_recorder.as_ref() {
                recorder.log_critical(
                    "operation.cancel_requested",
                    serde_json::json!({
                        "operation_id": &commit.operation_id,
                        "elapsed_ms": commit.started.elapsed().as_secs_f64() * 1_000.0
                    }),
                );
            }
            commit.kernel_cancellation.cancel();
            commit.job.cancel();
        }
    }

    fn execute_face_push_pull(&mut self, pending: PendingOperation) {
        let PendingOperation::PushPullFace {
            base_snapshot,
            support_body,
            target_face,
            distance,
        } = pending
        else {
            unreachable!("only a staged face push/pull can reach this execution path")
        };
        if self.displayed_snapshot_id() != Some(base_snapshot)
            || self.active_body_id() != Some(support_body)
        {
            self.reject_face_push_pull(workbench_extrusion_error(
                KernelErrorCode::StaleSnapshot,
                self.displayed_snapshot_id()
                    .unwrap_or_else(|| self.empty_snapshot.id()),
                "the staged push/pull face or body is no longer active",
            ));
            return;
        }
        if !distance.is_finite() || distance.abs() <= f64::EPSILON {
            self.reject_face_push_pull(workbench_extrusion_error(
                KernelErrorCode::InvalidInput,
                base_snapshot,
                "the staged push/pull distance must be finite and non-zero",
            ));
            return;
        }
        let Some(input) = self.displayed.as_ref() else {
            return;
        };
        self.request_serial += 1;
        let command = KernelCommand::PushPullFace {
            target_face,
            distance,
        };
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new(format!("workbench-{}-push-pull-face", self.request_serial)),
            expected_snapshot: input.snapshot.id(),
            precision: input.snapshot.precision_policy().unwrap_or_default(),
            command: command.clone(),
        };

        match NativeKernel::execute(&input.snapshot, &request, &CancellationToken::new()) {
            Ok(outcome) => {
                let mut next_document = self.document.clone();
                let feature_binding = self.append_push_pull_to_document(
                    &mut next_document,
                    support_body,
                    target_face,
                    command,
                    &outcome.report,
                    distance,
                );
                let Ok(feature_id) = feature_binding else {
                    self.reject_face_push_pull(workbench_extrusion_error(
                        KernelErrorCode::InternalFailure,
                        base_snapshot,
                        format!(
                            "push/pull could not enter parametric history: {}",
                            feature_binding.expect_err("the result is known to be an error")
                        ),
                    ));
                    return;
                };
                let remapped_selection = outcome
                    .report
                    .history
                    .iter()
                    .find(|record| {
                        record.relation != HistoryRelation::Deleted
                            && record.inputs.as_slice() == [target_face]
                    })
                    .and_then(|record| {
                        record
                            .outputs
                            .iter()
                            .copied()
                            .find(|output| output.kind == EntityKind::Face)
                    });
                let scene = NativeKernel::debug_scene(&outcome.snapshot);
                let bounds = outcome.report.bounds;
                self.document = next_document;
                self.history_scrub_position = self.document.history_position();
                self.archive_feature_report(feature_id, outcome.report.clone());
                self.displayed = Some(DisplayedBody {
                    snapshot: outcome.snapshot,
                    report: outcome.report,
                    scene,
                });
                self.face_sketch_context = None;
                if let Some(bounds) = bounds {
                    self.body_pivot = Some(bounds_center(bounds));
                }
                self.clear_transform_preview();
                self.pending_operation = None;
                self.selected_face = remapped_selection;
                self.sketch_extrusion_issue = None;
                self.model_body_kind = ModelBodyKind::PushedPulled;
                self.sync_active_body_record();
                self.archive_displayed_body();
                let kind = if distance < 0.0 {
                    FeaturePreviewKind::Cut
                } else {
                    FeaturePreviewKind::Add
                };
                self.feature_preview.append(kind);
                self.last_attempt = Attempt::Accepted {
                    operation: "Face push/pull committed",
                };
                self.selected_history_feature = Some(feature_id);
                self.document_status = Some("Feature regenerated cleanly".to_owned());
            }
            Err(error) => self.reject_face_push_pull(error),
        }
    }

    fn reject_face_push_pull(&mut self, error: KernelError) {
        self.sketch_extrusion_issue = Some(error.clone());
        self.last_attempt = Attempt::Rejected {
            operation: "Face push/pull rejected",
            error,
        };
    }

    fn reject_staged_sketch_extrusion(&mut self, error: KernelError) {
        if let Some(recorder) = self.development_recorder.as_ref() {
            recorder.log_critical(
                "operation.reject",
                extrusion_rejection_trace_payload(&error),
            );
        }
        self.sketch_extrusion_issue = Some(error.clone());
        self.last_attempt = Attempt::Rejected {
            operation: "Sketch extrusion rejected",
            error,
        };
    }

    fn clear_numeric_editor_after_model_action(&mut self, context: &egui::Context) {
        let focused = self
            .last_focused_editor
            .take()
            .or_else(|| context.memory(|memory| memory.focused()));
        let Some(focused) = focused else {
            return;
        };
        // DragValue stores its editable string under the widget ID. Removing
        // it before/after focus is surrendered prevents the next frame's
        // lost-focus path from writing a pre-commit value into a reset preview.
        context.data_mut(|data| data.remove_temp::<String>(focused));
        context.memory_mut(|memory| memory.surrender_focus(focused));
    }

    fn apply_transform_preview(&mut self) {
        self.sync_transform_preview();
        if !self.transform_preview_pending() {
            return;
        }
        let Some(body) = self.displayed.as_ref() else {
            return;
        };
        let Some(pivot) = self.body_pivot else {
            return;
        };

        self.request_serial += 1;
        let input_snapshot = body.snapshot.id();
        let expected_snapshot = self.transform_preview_base().unwrap_or(input_snapshot);
        let preview = self.display_transform;
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new(format!(
                "workbench-{}-transform-snapshot",
                self.request_serial
            )),
            expected_snapshot,
            precision: body.snapshot.precision_policy().unwrap_or_default(),
            command: KernelCommand::TransformSnapshot {
                transform: preview.kernel_similarity(pivot),
            },
        };
        let replay_command = request.command.clone();

        match NativeKernel::execute(&body.snapshot, &request, &CancellationToken::new()) {
            Ok(outcome) => {
                let Some(body_id) = self.active_body_id() else {
                    self.document_status =
                        Some("Transform rejected: no active parametric body".to_owned());
                    return;
                };
                let mut next_document = self.document.clone();
                let feature_kind = FeatureKind::Transform;
                let appended = next_document.append_feature(
                    FeatureDraft::new(
                        feature_kind,
                        Self::next_document_feature_label(&next_document, feature_kind),
                        ReplayAction::Kernel(replay_command),
                    )
                    .with_input(FeatureInput::Body(body_id))
                    .with_output(OutputDraft::ModifyBody(body_id))
                    .with_commit(SnapshotAssociation::new(
                        outcome.report.input_snapshot,
                        outcome.report.output_snapshot,
                        outcome.report.semantic_digest,
                    )),
                );
                let Ok(appended) = appended else {
                    self.document_status = Some(format!(
                        "Transform history rejected: {}",
                        appended.expect_err("the result is known to be an error")
                    ));
                    return;
                };
                let feature_id = appended.feature;
                let remapped_selection = self.selected_face.and_then(|selected| {
                    outcome
                        .report
                        .history
                        .iter()
                        .find(|record| {
                            record.relation != HistoryRelation::Deleted
                                && record.inputs.as_slice() == [selected]
                        })
                        .and_then(|record| {
                            record
                                .outputs
                                .iter()
                                .copied()
                                .find(|output| output.kind == selected.kind)
                        })
                });
                let next_pivot = preview.transform_point_about(pivot, pivot);
                let scene = NativeKernel::debug_scene(&outcome.snapshot);
                self.document = next_document;
                self.history_scrub_position = self.document.history_position();
                self.archive_feature_report(feature_id, outcome.report.clone());
                self.displayed = Some(DisplayedBody {
                    snapshot: outcome.snapshot,
                    report: outcome.report,
                    scene,
                });
                self.face_sketch_context = None;
                self.body_pivot = Some(next_pivot);
                self.selected_face = remapped_selection;
                self.clear_transform_preview();
                self.sync_active_body_record();
                self.archive_displayed_body();
                self.feature_preview.append(FeaturePreviewKind::Transform);
                self.last_attempt = Attempt::Accepted {
                    operation: "Transform committed",
                };
                self.selected_history_feature = Some(feature_id);
                self.document_status = Some("Transform committed to history".to_owned());
            }
            Err(error) => {
                self.last_attempt = Attempt::Rejected {
                    operation: "Transform rejected",
                    error,
                };
            }
        }
    }

    fn apply_component_placement_preview(&mut self) {
        let Some(PendingOperation::ComponentPlacement {
            component,
            base_pose,
        }) = self.pending_operation
        else {
            return;
        };
        let Some(current) = self.document.component_instance(component) else {
            self.document_status =
                Some("Component placement rejected: the occurrence no longer exists".into());
            return;
        };
        if current.pose != base_pose {
            self.document_status = Some(
                "Component placement rejected: its committed pose changed after preview began"
                    .into(),
            );
            return;
        }
        let Some(world_pivot) = self.body_pivot else {
            self.document_status = Some(
                "Component placement rejected: no stable occurrence pivot is available".into(),
            );
            return;
        };
        let preview = match assembly::MoveRotatePreview::from_display_parts(
            self.display_transform.translation,
            self.display_transform.rotation,
            self.display_transform.scale,
        ) {
            Ok(preview) => preview,
            Err(error) => {
                self.document_status = Some(format!("Component placement rejected: {error}"));
                return;
            }
        };
        let pose = match assembly::compose_move_rotate_preview(
            current.pose,
            preview,
            world_pivot,
            current.grounded,
        ) {
            Ok(pose) => pose,
            Err(error) => {
                self.document_status = Some(format!("Component placement rejected: {error}"));
                return;
            }
        };
        let mut next_document = self.document.clone();
        if let Err(error) = next_document.set_component_pose(component, pose) {
            self.document_status = Some(format!("Component placement rejected: {error}"));
            return;
        }
        self.document = next_document;
        self.clear_transform_preview();
        if let Some(index) = self.active_body_index() {
            self.body_pivot = self.committed_world_pivot_for_body(&self.bodies[index]);
        }
        self.last_attempt = Attempt::Accepted {
            operation: "Component placement committed",
        };
        self.document_status =
            Some("Rigid component placement committed · component B-rep remained unchanged".into());
    }

    fn stage_component_grounding(&mut self, grounded: bool) {
        if self.pending_operation.is_some() || !self.history_is_at_end() {
            return;
        }
        let Some(component) = self.active_component_instance() else {
            return;
        };
        if component.grounded == grounded {
            return;
        }
        self.pending_operation = Some(PendingOperation::SetComponentGrounded {
            component: component.id,
            base_grounded: component.grounded,
            grounded,
        });
    }

    fn apply_component_grounding(
        &mut self,
        component: ComponentInstanceId,
        base_grounded: bool,
        grounded: bool,
    ) {
        let Some(current) = self.document.component_instance(component) else {
            self.document_status =
                Some("Grounding rejected: the component occurrence no longer exists".into());
            return;
        };
        if current.grounded != base_grounded {
            self.document_status =
                Some("Grounding rejected: the occurrence state changed after staging".into());
            return;
        }
        if grounded && self.document.joint_for_child(component).is_some() {
            self.document_status = Some(
                "Grounding rejected: remove or replace the component's parent joint first".into(),
            );
            return;
        }
        let mut next_document = self.document.clone();
        if let Err(error) = next_document.set_component_grounded(component, grounded) {
            self.document_status = Some(format!("Grounding rejected: {error}"));
            return;
        }
        self.document = next_document;
        self.pending_operation = None;
        if grounded {
            self.active_tool = ActiveTool::Select;
        }
        self.last_attempt = Attempt::Accepted {
            operation: if grounded {
                "Component grounded"
            } else {
                "Component released"
            },
        };
        self.document_status = Some(if grounded {
            "Component grounded at its committed assembly pose".into()
        } else {
            "Component released for rigid placement".into()
        });
    }

    fn stage_revolute_joint(&mut self) {
        if self.pending_operation.is_some() || !self.history_is_at_end() {
            return;
        }
        let Some(component) = self.active_component_instance() else {
            return;
        };
        if component.grounded || self.document.joint_for_child(component.id).is_some() {
            return;
        }
        self.pending_operation = Some(PendingOperation::CreateRevoluteJoint {
            component: component.id,
        });
    }

    fn apply_revolute_joint(&mut self, component: ComponentInstanceId) {
        let Some(instance) = self.document.component_instance(component) else {
            self.document_status =
                Some("Joint rejected: the component occurrence no longer exists".into());
            return;
        };
        if instance.grounded {
            self.document_status =
                Some("Joint rejected: release the grounded component first".into());
            return;
        }
        if self.document.joint_for_child(component).is_some() {
            self.document_status =
                Some("Joint rejected: this component already has a parent joint".into());
            return;
        }
        let Some(origin) = self.body_pivot else {
            self.document_status = Some("Joint rejected: no stable component pivot".into());
            return;
        };
        let joint_origin = match JointOrigin::new(origin.x, origin.y, origin.z) {
            Ok(origin) => origin,
            Err(error) => {
                self.document_status = Some(format!("Joint rejected: {error}"));
                return;
            }
        };
        let axis = JointAxis::new(0.0, 0.0, 1.0)
            .expect("the world Z unit vector is a valid revolute axis");
        let label = instance.label.clone();
        let draft = JointDraft::new(
            format!("{label} Rotation"),
            JointParent::World,
            component,
            JointKind::Revolute {
                origin: joint_origin,
                axis,
                limits: None,
            },
        );
        let mut next_document = self.document.clone();
        let joint = match next_document.add_joint(draft) {
            Ok(joint) => joint,
            Err(error) => {
                self.document_status = Some(format!("Joint rejected: {error}"));
                return;
            }
        };
        self.document = next_document;
        self.pending_operation = None;
        self.active_tool = ActiveTool::Select;
        self.last_attempt = Attempt::Accepted {
            operation: "Revolute joint committed",
        };
        self.document_status = Some(format!(
            "Created {label} Rotation ({joint}) · world Z axis · animation ready"
        ));
    }

    fn stage_body_boolean(&mut self, operation: BooleanOperation) {
        if self.pending_operation.is_some() || !self.history_is_at_end() {
            return;
        }
        let Some(target) = self.active_body_id() else {
            return;
        };
        if !self
            .bodies
            .iter()
            .any(|body| body.id != target && body.visible)
        {
            self.document_status =
                Some("Boolean needs a second visible body in this workspace".to_owned());
            return;
        }
        // Tools start empty on purpose. Guessing an operand was the old
        // behaviour and it silently picked the wrong body once a third
        // existed; the user now names every operand before confirming.
        self.boolean_tools.clear();
        self.pending_operation = Some(PendingOperation::BooleanBodies {
            target,
            operation,
            keep_tools: false,
        });
        self.document_status =
            Some("Boolean staged · click the tool bodies to combine with the target".to_owned());
    }

    /// Adds or removes one tool body from the staged Boolean.
    ///
    /// Returns whether the pick was accepted, so a click that lands on the
    /// target or on a hidden body can fall through to ordinary selection.
    fn toggle_boolean_tool(&mut self, body: BodyId) -> bool {
        let Some(PendingOperation::BooleanBodies { target, .. }) = self.pending_operation else {
            return false;
        };
        if body == target {
            self.document_status =
                Some("The target body cannot also be a tool · pick a different body".to_owned());
            return true;
        }
        if !self
            .bodies
            .iter()
            .any(|candidate| candidate.id == body && candidate.visible)
        {
            return false;
        }
        if let Some(index) = self.boolean_tools.iter().position(|held| *held == body) {
            self.boolean_tools.remove(index);
        } else {
            self.boolean_tools.push(body);
        }
        self.document_status = Some(self.boolean_operand_summary());
        true
    }

    fn set_boolean_keep_tools(&mut self, keep: bool) {
        if let Some(PendingOperation::BooleanBodies { keep_tools, .. }) =
            self.pending_operation.as_mut()
        {
            *keep_tools = keep;
        }
    }

    /// Human-readable operand state for the staged Boolean.
    fn boolean_operand_summary(&self) -> String {
        let Some(PendingOperation::BooleanBodies { target, .. }) = self.pending_operation else {
            return String::new();
        };
        let name = |id: BodyId| {
            self.bodies.iter().find(|body| body.id == id).map_or_else(
                || "Body".to_owned(),
                |body| format!("Body {}", body.ordinal),
            )
        };
        if self.boolean_tools.is_empty() {
            return format!(
                "{} is the target · click one or more tool bodies",
                name(target)
            );
        }
        let tools = self
            .boolean_tools
            .iter()
            .map(|id| name(*id))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{} ← {tools}", name(target))
    }

    /// Whether the staged Boolean still lacks an operand the user must supply.
    fn boolean_confirmation_blocked(&self) -> bool {
        matches!(
            self.pending_operation,
            Some(PendingOperation::BooleanBodies { .. })
        ) && self.boolean_tools.is_empty()
    }

    /// Applies every picked tool to the target as one atomic user action.
    ///
    /// Each tool is a separate kernel Boolean and a separate history feature,
    /// because [`BooleanFeatureRecipe`] is a two-body intent and replay must
    /// stay able to reproduce each step exactly. Nothing is published until
    /// every step has succeeded, so a rejection midway leaves the workspace
    /// untouched rather than half-combined.
    fn execute_body_boolean(
        &mut self,
        target: BodyId,
        tools: &[BodyId],
        operation: BooleanOperation,
        keep_tools: bool,
    ) {
        if tools.is_empty() {
            self.document_status = Some("Boolean needs at least one tool body".to_owned());
            return;
        }
        let Some(original_target) = self.bodies.iter().find(|body| body.id == target).cloned()
        else {
            self.document_status = Some("Boolean operands changed after staging".to_owned());
            return;
        };

        let mut next_document = self.document.clone();
        let mut current = original_target.body.clone();
        let mut committed = Vec::with_capacity(tools.len());
        for tool in tools.iter().copied() {
            let Some(tool_body) = self.bodies.iter().find(|body| body.id == tool).cloned() else {
                self.document_status = Some("Boolean operands changed after staging".to_owned());
                return;
            };
            self.request_serial = self.request_serial.saturating_add(1);
            let request = BooleanRequest {
                protocol_version: CURRENT_PROTOCOL_VERSION,
                request_id: RequestId::new(format!("workbench-{}-boolean", self.request_serial)),
                expected_target_snapshot: current.snapshot.id(),
                expected_tool_snapshot: tool_body.body.snapshot.id(),
                precision: current.snapshot.precision_policy().unwrap_or_default(),
                operation,
            };
            let outcome = match NativeKernel::execute_boolean(
                &current.snapshot,
                &tool_body.body.snapshot,
                &request,
                &CancellationToken::new(),
            ) {
                Ok(outcome) => outcome,
                Err(error) => {
                    self.last_attempt = Attempt::Rejected {
                        operation: "Body Boolean",
                        error: error.clone(),
                    };
                    self.document_status = Some(format!("Boolean rejected: {error}"));
                    return;
                }
            };
            let association = SnapshotAssociation::new(
                outcome.report.input_snapshot,
                outcome.report.output_snapshot,
                outcome.report.semantic_digest,
            );
            let recipe = BooleanFeatureRecipe {
                target,
                tool,
                operation,
                keep_tool: keep_tools,
            };
            let label = Self::next_document_feature_label(&next_document, FeatureKind::Boolean);
            let appended = next_document.append_feature(
                FeatureDraft::new(FeatureKind::Boolean, label, ReplayAction::Boolean(recipe))
                    .with_input(FeatureInput::Body(target))
                    .with_input(FeatureInput::Body(tool))
                    .with_output(OutputDraft::ModifyBody(target))
                    .with_commit(association),
            );
            let Ok(appended) = appended else {
                self.document_status = Some(format!(
                    "Boolean history rejected: {}",
                    appended.expect_err("known error")
                ));
                return;
            };
            current = DisplayedBody {
                scene: NativeKernel::debug_scene(&outcome.snapshot),
                snapshot: outcome.snapshot,
                report: outcome.report.clone(),
            };
            committed.push((appended.feature, outcome.report));
        }

        // Every step succeeded: publish once.
        if !self
            .body_archive
            .iter()
            .any(|entry| entry.body.snapshot.id() == original_target.body.snapshot.id())
        {
            self.body_archive.push(ArchivedBody {
                body: original_target.body.clone(),
                kind: original_target.kind,
            });
        }
        let solid_count = current.snapshot.counts().solids;
        let last_feature = committed
            .last()
            .map(|(feature, _)| *feature)
            .expect("a non-empty tool list commits at least one feature");
        self.document = next_document;
        for (feature, report) in committed {
            self.archive_feature_report(feature, report);
        }
        if let Some(body) = self.bodies.iter_mut().find(|body| body.id == target) {
            body.body = current.clone();
            body.last_feature = last_feature;
            body.kind = ModelBodyKind::Boolean;
        }
        if !keep_tools {
            for tool in tools {
                if let Some(body) = self.bodies.iter_mut().find(|body| body.id == *tool) {
                    body.visible = false;
                }
            }
        }
        if self.active_body_id() == Some(target) {
            self.displayed = Some(current);
            self.model_body_kind = ModelBodyKind::Boolean;
            self.body_pivot = self
                .displayed
                .as_ref()
                .and_then(|body| body.report.bounds.map(bounds_center));
        }
        self.boolean_tools.clear();
        self.pending_operation = None;
        self.history_scrub_position = self.document.history_position();
        self.selected_history_feature = Some(last_feature);
        self.sync_feature_preview_from_document();
        self.last_attempt = Attempt::Accepted {
            operation: match operation {
                BooleanOperation::Union => "Bodies combined",
                BooleanOperation::Difference => "Body subtracted",
                BooleanOperation::Intersection => "Body intersection committed",
            },
        };
        let tool_count = tools.len();
        self.document_status = Some(format!(
            "Boolean committed · {tool_count} tool body(s) · {solid_count} solid component(s)"
        ));
    }

    /// The active sketch's closed profile and centreline, if it has both.
    ///
    /// A revolve needs a region and an axis in the same frame, which is
    /// exactly what a sketch with one centreline already is.
    fn staged_sketch_revolve(&self) -> Option<StagedRevolve> {
        let (start, end) = self.sketch.centreline_axis()?;
        let profile = self.sketch_planar_profile_payload()?;
        Some(StagedRevolve {
            frame: self.sketch_support.frame(),
            profile,
            axis: PlanarAxis2::new(
                ProtocolPoint2::new(start.u, start.v),
                ProtocolPoint2::new(end.u, end.v),
            ),
        })
    }

    fn stage_preset_feature(&mut self, preset: SolidFeaturePreset) {
        if self.pending_operation.is_some() || !self.history_is_at_end() {
            return;
        }
        if preset == SolidFeaturePreset::Revolve {
            // A sketched region turning about its own centreline is the real
            // command; the fixed tube remains only for an empty document.
            self.staged_revolve = self.staged_sketch_revolve();
            self.document_status = Some(if self.staged_revolve.is_some() {
                "Revolve staged from the active sketch profile and centreline".to_owned()
            } else {
                "Revolve staged · draw a closed profile and one centreline to revolve your own"
                    .to_owned()
            });
            self.pending_operation = Some(PendingOperation::PresetFeature {
                preset,
                base_snapshot: SnapshotId::ZERO,
                body: None,
                target_face: None,
                frame: None,
            });
            return;
        }
        let Some(index) = self.active_body_index() else {
            return;
        };
        let body = self.bodies[index].id;
        let base_snapshot = self.bodies[index].body.snapshot.id();
        let (target_face, frame) =
            if matches!(preset, SolidFeaturePreset::Hole | SolidFeaturePreset::Rib) {
                let Some(face) = self.selected_face else {
                    self.document_status = Some("Select a planar face for Hole or Rib".to_owned());
                    return;
                };
                let support = match NativeKernel::planar_face_support(
                    &self.bodies[index].body.snapshot,
                    face,
                ) {
                    Ok(support) => support,
                    Err(error) => {
                        self.document_status =
                            Some(format!("Selected face is unsupported: {error}"));
                        return;
                    }
                };
                (Some(face), Some(support.frame))
            } else if matches!(
                preset,
                SolidFeaturePreset::Chamfer | SolidFeaturePreset::Fillet
            ) {
                self.apply_tangent_edge_chain();
                let selected = self
                    .selected_edges
                    .iter()
                    .copied()
                    .filter(|selection| selection.body.get() == body.get())
                    .collect::<Vec<_>>();
                if selected.is_empty() || selected.len() != self.selected_edges.len() {
                    self.document_status =
                        Some("Select one or more edges on the active body".to_owned());
                    return;
                }
                let support = self.edge_finish_selection_support();
                self.document_status = Some(if support.can_commit() {
                    "Edge-finish preview staged · confirm with Enter or the green tick".to_owned()
                } else {
                    format!("Preview staged · {}", support.detail())
                });
                (Some(selected[0].edge), None)
            } else {
                (None, None)
            };
        self.pending_operation = Some(PendingOperation::PresetFeature {
            preset,
            base_snapshot,
            body: Some(body),
            target_face,
            frame,
        });
        if matches!(
            preset,
            SolidFeaturePreset::Chamfer | SolidFeaturePreset::Fillet
        ) {
            self.edge_finish_distance_text = format!("{:.3}", self.edge_finish_distance);
        }
    }

    fn apply_tangent_edge_chain(&mut self) {
        if !self.edge_finish_tangent_chain || self.selected_edges.is_empty() {
            return;
        }
        let body_key = self.selected_edges[0].body;
        let Some(body) = self
            .bodies
            .iter()
            .find(|body| body.id.get() == body_key.get())
        else {
            return;
        };
        let candidates = body
            .body
            .scene
            .edges
            .iter()
            .filter(|edge| !edge.is_smooth)
            .map(|edge| viewport::DocumentEdgeSelection {
                body: body_key,
                edge: edge.source_edge,
            })
            .collect::<BTreeSet<_>>();
        let mut changed = true;
        while changed {
            changed = false;
            for candidate in &candidates {
                if self.selected_edges.contains(candidate) {
                    continue;
                }
                let candidate_segments = body
                    .body
                    .scene
                    .edges
                    .iter()
                    .filter(|edge| edge.source_edge == candidate.edge && !edge.is_smooth)
                    .map(|edge| edge.endpoints)
                    .collect::<Vec<_>>();
                if candidate_segments.is_empty() {
                    continue;
                }
                let tangent = self.selected_edges.iter().any(|selected| {
                    let selected_segments = body
                        .body
                        .scene
                        .edges
                        .iter()
                        .filter(|edge| edge.source_edge == selected.edge && !edge.is_smooth)
                        .map(|edge| edge.endpoints)
                        .collect::<Vec<_>>();
                    selected_segments.iter().any(|selected_segment| {
                        candidate_segments.iter().any(|candidate_segment| {
                            model_segments_share_tangent_endpoint(
                                *selected_segment,
                                *candidate_segment,
                            )
                        })
                    })
                });
                if tangent {
                    self.selected_edges.push(*candidate);
                    changed = true;
                }
            }
        }
        self.selected_edge = self.selected_edges.last().copied();
    }

    fn execute_preset_feature(
        &mut self,
        preset: SolidFeaturePreset,
        base_snapshot: SnapshotId,
        body: Option<BodyId>,
        target_face: Option<EntityRef>,
        frame: Option<PlanarFrame3>,
    ) {
        let input = if preset == SolidFeaturePreset::Revolve {
            NativeKernel::empty()
        } else {
            let Some(body) = body else {
                return;
            };
            let Some(workbench) = self.bodies.iter().find(|candidate| candidate.id == body) else {
                self.document_status = Some("Feature body changed after staging".to_owned());
                return;
            };
            if workbench.body.snapshot.id() != base_snapshot {
                self.document_status = Some("Feature preview is stale".to_owned());
                return;
            }
            workbench.body.snapshot.clone()
        };
        let command = match preset {
            // The preset is still a fixed tube, but it now travels the
            // general revolve: a section rectangle beside an axis in its own
            // frame, exactly as a sketched profile will once region-and-axis
            // staging lands. `MakeRevolvedAnnulus` has no consumer left in the
            // product.
            SolidFeaturePreset::Revolve => {
                let staged = self.staged_revolve.clone();
                staged.map_or_else(
                    // No sketch to turn: the preset still builds the tube it
                    // always did, so an empty document has something to show.
                    || KernelCommand::RevolvePlanarProfile {
                        frame: PlanarFrame3::new(
                            Point3::new(0.0, 0.0, 0.0),
                            Vector3::new(1.0, 0.0, 0.0),
                            Vector3::new(0.0, 0.0, 1.0),
                        ),
                        profile: PlanarProfile2 {
                            regions: vec![PlanarRegion2 {
                                outer: PlanarLoop2::from_polygon(&[
                                    ProtocolPoint2::new(1.0, 0.0),
                                    ProtocolPoint2::new(2.0, 0.0),
                                    ProtocolPoint2::new(2.0, 3.0),
                                    ProtocolPoint2::new(1.0, 3.0),
                                ]),
                                holes: Vec::new(),
                            }],
                        },
                        axis: PlanarAxis2::new(
                            ProtocolPoint2::new(0.0, 0.0),
                            ProtocolPoint2::new(0.0, 1.0),
                        ),
                        angle: RevolveAngle::FullTurn,
                    },
                    |staged| KernelCommand::RevolvePlanarProfile {
                        frame: staged.frame,
                        profile: staged.profile,
                        axis: staged.axis,
                        angle: RevolveAngle::FullTurn,
                    },
                )
            }
            SolidFeaturePreset::Hole => KernelCommand::DrillHole {
                target_face: target_face.expect("staged hole face"),
                frame: frame.expect("staged hole frame"),
                center: ProtocolPoint2::new(0.0, 0.0),
                diameter: 1.0,
                depth: 1_000.0,
            },
            SolidFeaturePreset::Rib => KernelCommand::AddRib {
                target_face: target_face.expect("staged rib face"),
                frame: frame.expect("staged rib frame"),
                start: ProtocolPoint2::new(-0.75, 0.0),
                end: ProtocolPoint2::new(0.75, 0.0),
                thickness: 0.5,
                height: 1.0,
            },
            SolidFeaturePreset::Mirror => KernelCommand::MirrorSnapshot {
                plane_origin: Point3::new(0.0, 0.0, 0.0),
                plane_normal: Vector3::new(1.0, 0.0, 0.0),
            },
            SolidFeaturePreset::LinearPattern => {
                let spacing = input
                    .measures()
                    .bounds
                    .map_or(5.0, |bounds| (bounds.max.x - bounds.min.x).abs() + 2.0);
                KernelCommand::LinearPatternSnapshot {
                    direction: Vector3::new(1.0, 0.0, 0.0),
                    spacing,
                    count: 3,
                }
            }
            SolidFeaturePreset::Chamfer | SolidFeaturePreset::Fillet => {
                let kind = if preset == SolidFeaturePreset::Chamfer {
                    EdgeFinishKind::Chamfer
                } else {
                    EdgeFinishKind::Fillet
                };
                let targets = self
                    .selected_edges
                    .iter()
                    .filter(|selection| body.is_some_and(|body| selection.body.get() == body.get()))
                    .map(|selection| selection.edge)
                    .collect::<Vec<_>>();
                if targets.len() <= 1 {
                    KernelCommand::FinishEdge {
                        target_edge: targets
                            .first()
                            .copied()
                            .or(target_face)
                            .expect("staged edge finish target"),
                        kind,
                        distance: self.edge_finish_distance,
                    }
                } else {
                    KernelCommand::FinishEdges {
                        target_edges: targets,
                        kind,
                        distance: self.edge_finish_distance,
                    }
                }
            }
        };
        self.request_serial = self.request_serial.saturating_add(1);
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new(format!("workbench-{}-preset", self.request_serial)),
            expected_snapshot: input.id(),
            precision: input.precision_policy().unwrap_or_default(),
            command: command.clone(),
        };
        let outcome = match NativeKernel::execute(&input, &request, &CancellationToken::new()) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.last_attempt = Attempt::Rejected {
                    operation: preset.label(),
                    error: error.clone(),
                };
                self.document_status = Some(format!("{} rejected: {error}", preset.label()));
                return;
            }
        };
        // The captured region and axis have been spent.
        self.staged_revolve = None;
        let association = SnapshotAssociation::new(
            outcome.report.input_snapshot,
            outcome.report.output_snapshot,
            outcome.report.semantic_digest,
        );
        let kind = match preset {
            SolidFeaturePreset::Revolve => FeatureKind::BaseBody,
            SolidFeaturePreset::Hole => FeatureKind::Cut,
            SolidFeaturePreset::Rib => FeatureKind::Add,
            SolidFeaturePreset::Mirror
            | SolidFeaturePreset::LinearPattern
            | SolidFeaturePreset::Chamfer
            | SolidFeaturePreset::Fillet => FeatureKind::Transform,
        };
        let replay_target = match &command {
            KernelCommand::FinishEdge { target_edge, .. } => Some(*target_edge),
            _ => target_face,
        };
        let action = if matches!(&command, KernelCommand::FinishEdges { .. }) {
            let targets = self
                .selected_edges
                .iter()
                .map(|selection| self.persistent_ref_for_current_edge(selection.edge))
                .collect::<Option<Vec<_>>>();
            let Some(targets) = targets else {
                self.document_status = Some(
                    "One or more selected edges have no persistent history identity".to_owned(),
                );
                return;
            };
            match TargetedKernel::new_many(command, targets) {
                Ok(targeted) => ReplayAction::TargetedKernel(targeted),
                Err(error) => {
                    self.document_status = Some(format!("Feature targets rejected: {error:?}"));
                    return;
                }
            }
        } else if let Some(entity) = replay_target {
            let target = if entity.kind == EntityKind::Edge {
                self.persistent_ref_for_current_edge(entity)
            } else {
                self.persistent_ref_for_current_face(entity)
            };
            let Some(target) = target else {
                self.document_status =
                    Some("Feature target has no persistent history identity".to_owned());
                return;
            };
            match TargetedKernel::new(command, target) {
                Ok(targeted) => ReplayAction::TargetedKernel(targeted),
                Err(error) => {
                    self.document_status = Some(format!("Feature target rejected: {error:?}"));
                    return;
                }
            }
        } else {
            ReplayAction::Kernel(command)
        };
        let mut next_document = self.document.clone();
        let mut draft = FeatureDraft::new(kind, preset.label(), action).with_commit(association);
        if let Some(body) = body {
            draft = draft
                .with_input(FeatureInput::Body(body))
                .with_output(OutputDraft::ModifyBody(body));
        } else {
            draft = draft.with_output(OutputDraft::CreateBody {
                label: "Revolved body".to_owned(),
            });
        }
        let appended = match next_document.append_feature(draft) {
            Ok(appended) => appended,
            Err(error) => {
                self.document_status = Some(format!("Feature history rejected: {error}"));
                return;
            }
        };
        let displayed = DisplayedBody {
            scene: NativeKernel::debug_scene(&outcome.snapshot),
            snapshot: outcome.snapshot,
            report: outcome.report.clone(),
        };
        self.document = next_document;
        self.archive_feature_report(appended.feature, outcome.report);
        if let Some(body_id) = body {
            if let Some(existing) = self.bodies.iter_mut().find(|entry| entry.id == body_id) {
                existing.body = displayed.clone();
                existing.last_feature = appended.feature;
                existing.kind = match preset {
                    SolidFeaturePreset::Hole => ModelBodyKind::CutPocket,
                    SolidFeaturePreset::Rib => ModelBodyKind::AddedBoss,
                    _ => ModelBodyKind::Boolean,
                };
            }
        } else if let Some(body_id) = appended.created_bodies.first().copied() {
            let ordinal = self.next_body_ordinal;
            self.next_body_ordinal = self.next_body_ordinal.saturating_add(1);
            self.active_body_ordinal = ordinal;
            self.bodies.push(WorkbenchBody {
                material: None,
                id: body_id,
                last_feature: appended.feature,
                ordinal,
                body: displayed.clone(),
                kind: ModelBodyKind::SketchExtrusion,
                visible: true,
            });
        }
        self.displayed = Some(displayed);
        self.model_body_kind = match preset {
            SolidFeaturePreset::Hole => ModelBodyKind::CutPocket,
            SolidFeaturePreset::Rib => ModelBodyKind::AddedBoss,
            SolidFeaturePreset::Revolve => ModelBodyKind::SketchExtrusion,
            _ => ModelBodyKind::Boolean,
        };
        // History replay restores immutable snapshots from this archive. Edge
        // finishes and other preset features used to update only the live
        // body, leaving every cursor position after the base body black.
        self.archive_displayed_body();
        self.body_pivot = self
            .displayed
            .as_ref()
            .and_then(|body| body.report.bounds.map(bounds_center));
        self.pending_operation = None;
        self.history_scrub_position = self.document.history_position();
        self.selected_history_feature = Some(appended.feature);
        self.selected_face = None;
        if matches!(
            preset,
            SolidFeaturePreset::Chamfer | SolidFeaturePreset::Fillet
        ) {
            self.clear_model_entity_selection();
        }
        self.sync_feature_preview_from_document();
        self.last_attempt = Attempt::Accepted {
            operation: preset.label(),
        };
        self.document_status = Some(format!("{} committed", preset.label()));
    }

    fn advance_motion(&mut self, context: &egui::Context) {
        if !self.motion.playing {
            self.last_motion_time = None;
            return;
        }

        let now = context.input(|input| input.time);
        if let Some(previous) = self.last_motion_time {
            self.motion.advance(now - previous);
        }
        self.last_motion_time = Some(now);
        // Let the native backend/vsync pace continuous frames. A fixed delayed
        // repaint is not portable across refresh rates because egui subtracts
        // its predicted frame time from requested delays. Motion remains
        // time-based, while 60 FPS is the measured minimum performance goal.
        context.request_repaint();
    }

    fn toggle_animation(&mut self, context: &egui::Context) {
        self.motion.toggle();
        self.last_motion_time = None;
        context.request_repaint();
    }

    fn reset_view(&mut self, context: &egui::Context) {
        self.view.reset_orientation();
        context.request_repaint();
    }

    fn frame_visible_document(&mut self) {
        let active = self.active_body_id();
        let bounds = self
            .bodies
            .iter()
            .filter(|body| body.visible)
            .filter_map(|body| {
                let bounds = self.committed_world_bounds_for_body(body)?;
                if Some(body.id) == active {
                    let pivot = bounds_center(bounds);
                    Some(self.display_transform.transformed_bounds(bounds, pivot))
                } else {
                    Some(bounds)
                }
            })
            .reduce(union_aabb);
        // A blank production document still has real construction geometry.
        // Frame its visible reference planes so increasing their physical size
        // also produces a matching initial camera scale instead of clipping a
        // 50 mm datum against the old one-unit camera radius.
        let bounds = bounds.or_else(|| self.visible_reference_plane_bounds());
        if let Some(bounds) = bounds {
            self.view.frame(bounds);
        }
    }

    fn frame_visible_body(&mut self, context: &egui::Context) {
        // Frame what the user is looking at (ADR 0026, F10). With a face or
        // edge selected, F means "show me this"; with nothing selected it
        // keeps its old meaning of framing the document.
        if self.frame_selection() {
            context.request_repaint();
            return;
        }
        self.frame_visible_document();
        context.request_repaint();
    }

    /// The world bounds of the current selection, if anything is selected.
    fn selection_world_bounds(&self) -> Option<Aabb3> {
        let body = self.displayed.as_ref()?;
        let scene = &body.scene;
        let mut bounds: Option<Aabb3> = None;
        let mut include = |point: Point3| {
            bounds = Some(match bounds {
                None => Aabb3::new(point, point),
                Some(existing) => union_aabb(existing, Aabb3::new(point, point)),
            });
        };
        let faces = self
            .selected_face
            .map(|face| viewport::tangent_face_group(scene, face))
            .unwrap_or_default();
        for triangle in &scene.triangles {
            if faces.contains(&triangle.source_face) {
                for vertex in triangle.vertices {
                    include(vertex);
                }
            }
        }
        let edges = self
            .selected_edges
            .iter()
            .map(|selection| selection.edge)
            .collect::<Vec<_>>();
        for edge in &scene.edges {
            if edges.contains(&edge.source_edge) {
                for endpoint in edge.endpoints {
                    include(endpoint);
                }
            }
        }
        for vertex in &scene.vertices {
            if self
                .selected_vertices
                .iter()
                .any(|selection| selection.vertex == vertex.source_vertex)
            {
                include(vertex.point);
            }
        }
        bounds
    }

    fn frame_selection(&mut self) -> bool {
        let Some(bounds) = self.selection_world_bounds() else {
            return false;
        };
        self.view.frame(bounds);
        true
    }

    fn handle_shortcuts(
        &mut self,
        context: &egui::Context,
        cancel_pending: bool,
        confirm_pending: bool,
    ) {
        let completed_action = if cancel_pending {
            self.cancel_pending_operation()
        } else if confirm_pending {
            self.confirm_pending_operation()
        } else {
            false
        };
        if completed_action {
            self.clear_numeric_editor_after_model_action(context);
            context.request_repaint();
            // One input event may complete at most one action. In particular,
            // Space on a focused tick/cross must not also toggle animation
            // after the confirmation control disappears.
            return;
        }

        if !context.egui_wants_keyboard_input() {
            let (undo, redo) = context.input(|input| {
                let command = input.modifiers.command;
                (
                    command && input.key_pressed(egui::Key::Z) && !input.modifiers.shift,
                    command
                        && (input.key_pressed(egui::Key::Y)
                            || (input.modifiers.shift && input.key_pressed(egui::Key::Z))),
                )
            });
            if self.workbench_mode == WorkbenchMode::Sketch
                && !self.sketch.dimension_editor_active()
                && ((undo && self.restore_local_sketch_journal(false))
                    || (redo && self.restore_local_sketch_journal(true)))
            {
                context.request_repaint();
                return;
            }
            if (undo && self.undo_document()) || (redo && self.redo_document()) {
                context.request_repaint();
                return;
            }
        }

        // Confirm and Cancel are global model actions, including while a
        // numeric editor owns focus. Single-letter view/tool shortcuts remain
        // suppressed so editing a value cannot unexpectedly change modes.
        if context.egui_wants_keyboard_input() {
            return;
        }
        match self.workbench_mode {
            WorkbenchMode::Model => {
                // Single-letter tool shortcuts require bare keys: a modified
                // press (Cmd+A select-all, Cmd+S save, …) must never switch
                // tools even when no editor currently owns the keyboard.
                let pressed = context.input(|input| {
                    let plain = input.modifiers.is_none();
                    (
                        plain && input.key_pressed(egui::Key::V),
                        plain && input.key_pressed(egui::Key::I),
                        plain && input.key_pressed(egui::Key::O),
                        plain && input.key_pressed(egui::Key::M),
                        plain && input.key_pressed(egui::Key::R),
                        plain && input.key_pressed(egui::Key::S),
                        plain && input.key_pressed(egui::Key::Space),
                        plain && input.key_pressed(egui::Key::Home),
                        plain && input.key_pressed(egui::Key::F),
                    )
                });
                let transform_tools_available = self.transform_tools_available();
                self.active_tool = if pressed.0 {
                    ActiveTool::Select
                } else if pressed.1 {
                    ActiveTool::Measure
                } else if pressed.2 {
                    ActiveTool::Orbit
                } else if pressed.3 && transform_tools_available {
                    ActiveTool::Move
                } else if pressed.4 && transform_tools_available {
                    ActiveTool::Rotate
                } else if pressed.5 && self.scale_tool_available() {
                    ActiveTool::Scale
                } else {
                    self.active_tool
                };
                if pressed.6 {
                    self.toggle_animation(context);
                }
                if pressed.7 {
                    self.reset_view(context);
                }
                if pressed.8 {
                    self.frame_visible_body(context);
                }
            }
            WorkbenchMode::Sketch => {
                // Same bare-key requirement as Model mode: Cmd+A must select
                // text in a palette editor, not activate the Arc tool.
                let pressed = context.input(|input| {
                    let plain = input.modifiers.is_none();
                    (
                        plain && input.key_pressed(egui::Key::V),
                        plain && input.key_pressed(egui::Key::P),
                        plain && input.key_pressed(egui::Key::L),
                        plain && input.key_pressed(egui::Key::R),
                        plain && input.key_pressed(egui::Key::C),
                        plain && input.key_pressed(egui::Key::A),
                        plain && input.key_pressed(egui::Key::T),
                        plain
                            && (input.key_pressed(egui::Key::Home)
                                || input.key_pressed(egui::Key::F)),
                        plain && input.key_pressed(egui::Key::Escape),
                        plain && input.key_pressed(egui::Key::Delete),
                    )
                });
                let requested_tool = if pressed.0 {
                    Some(ToolVariant::Select)
                } else if pressed.1 {
                    Some(ToolVariant::Point)
                } else if pressed.2 {
                    Some(
                        self.sketch_toolbar
                            .preferences()
                            .last_used(crate::sketch_toolbar::ToolFamily::Line),
                    )
                } else if pressed.3 {
                    Some(
                        self.sketch_toolbar
                            .preferences()
                            .last_used(crate::sketch_toolbar::ToolFamily::Rectangle),
                    )
                } else if pressed.4 {
                    Some(
                        self.sketch_toolbar
                            .preferences()
                            .last_used(crate::sketch_toolbar::ToolFamily::Circle),
                    )
                } else if pressed.5 {
                    Some(
                        self.sketch_toolbar
                            .preferences()
                            .last_used(crate::sketch_toolbar::ToolFamily::Arc),
                    )
                } else if pressed.6 {
                    Some(ToolVariant::Trim)
                } else {
                    None
                };
                if let Some(tool) = requested_tool {
                    self.activate_sketch_tool_variant(tool);
                }
                if pressed.7 {
                    self.frame_active_sketch();
                }
                if pressed.8 && self.pending_operation.is_none() {
                    self.sketch.clear_creation_draft();
                }
                if pressed.9
                    && self.pending_operation.is_none()
                    && !self.sketch.dimension_editor_active()
                {
                    self.stage_delete_selected_sketch();
                }
            }
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let operation_pending = self.operation_confirmation_pending();
        ui.horizontal_centered(|ui| {
            let product_mark = ui.label(
                RichText::new("ARTIFICER")
                    .font(FontId::proportional(10.0))
                    .color(ACCENT)
                    .strong(),
            );
            product_mark.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Label, true, "Artificer Workbench")
            });
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(
                RichText::new("Document 1")
                    .font(FontId::proportional(14.0))
                    .color(TEXT)
                    .strong(),
            );
            let can_save = !operation_pending;
            let save = ui
                .add_enabled(can_save, egui::Button::new("Save").small())
                .on_hover_text(format!(
                    "Save Artificer workspace to {}",
                    self.document_path.display()
                ));
            save.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, can_save, "Save document")
            });
            let can_open = !operation_pending && self.document_path.is_file();
            let open = ui
                .add_enabled(can_open, egui::Button::new("Open").small())
                .on_hover_text(if self.document_path.is_file() {
                    format!("Stage opening {}", self.document_path.display())
                } else {
                    format!(
                        "No saved document exists at {}",
                        self.document_path.display()
                    )
                });
            open.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, can_open, "Open saved document")
            });
            if save.clicked() {
                let path = self.document_path.clone();
                self.document_status = Some(match self.save_workspace_to_path(&path) {
                    Ok(()) => format!("Saved Artificer workspace to {}", path.display()),
                    Err(error) => format!("Save failed: {error}"),
                });
            } else if open.clicked() {
                self.pending_operation = Some(PendingOperation::LoadDefaultDocument);
            }
            shell_toggle_button(
                ui,
                self.part_library.open_mut(),
                "Library",
                "part library",
                operation_pending,
            );
            ui.add_space(8.0);
            for mode in [WorkbenchMode::Model, WorkbenchMode::Sketch] {
                let enabled = self.pending_operation.is_none();
                let response = workspace_tab(
                    ui,
                    &format!("{} mode", mode.label()),
                    self.workbench_mode == mode,
                    enabled,
                );
                if response.clicked() {
                    match mode {
                        WorkbenchMode::Model => self.enter_model_mode(),
                        WorkbenchMode::Sketch => self.enter_sketch_mode(),
                    }
                }
                if !enabled {
                    response.on_disabled_hover_text(
                        "Confirm or cancel the pending operation before changing workspaces.",
                    );
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let properties =
                    ui.add_enabled(!operation_pending, egui::Button::new("Properties"));
                if properties.clicked() {
                    self.show_properties_tab();
                    // The document popout is a Model-workspace affordance; in
                    // Sketch mode it would cover the canvas at the minimum
                    // window size, so Properties only raises the side palette.
                    if self.workbench_mode == WorkbenchMode::Model {
                        self.document_properties_open = true;
                    }
                }
                let browser = ui.add_enabled(!operation_pending, egui::Button::new("Browser"));
                if browser.clicked() {
                    self.shell.set_model_browser(true);
                }
                shell_toggle_button(
                    ui,
                    self.shell.feature_timeline_mut(),
                    "History",
                    "design-history preview",
                    operation_pending,
                );
            });
        });
    }

    /// Mouse-navigation scheme. Orbit and pan bindings are pure habit, so the
    /// workbench asks which package the user already knows rather than making
    /// them relearn one.
    fn navigation_card(&mut self, ui: &mut egui::Ui) {
        let mut preset = self.document_settings.navigation;
        let changed = egui::ComboBox::from_id_salt("navigation_preset_picker")
            .selected_text(preset.label())
            .width(ui.available_width() - 8.0)
            .show_ui(ui, |ui| {
                let mut changed = false;
                for option in navigation::NavigationPreset::ALL {
                    if ui
                        .selectable_label(option == preset, option.label())
                        .on_hover_text(option.summary())
                        .clicked()
                    {
                        preset = option;
                        changed = true;
                    }
                }
                changed
            })
            .inner
            .unwrap_or(false);
        if changed {
            self.document_settings.navigation = preset;
        }
        ui.label(RichText::new(preset.summary()).small().color(MUTED));
    }

    /// Material assignment for the active body, and the mass properties that
    /// follow from it. Anything the kernel could not certify is named as
    /// unavailable rather than filled in with a plausible number.
    fn material_card(&mut self, ui: &mut egui::Ui) {
        let Some(active) = self.active_body_id() else {
            ui.label(RichText::new("No body is active.").small().color(MUTED));
            return;
        };
        let assigned = self.body_material(active);
        let operation_pending = self.operation_confirmation_pending();

        let selected_label = assigned.map_or("Unassigned", |material| material.name);
        let mut chosen: Option<Option<&'static str>> = None;
        ui.add_enabled_ui(!operation_pending, |ui| {
            egui::ComboBox::from_id_salt("body_material_picker")
                .selected_text(selected_label)
                .width(ui.available_width() - 8.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(assigned.is_none(), "Unassigned")
                        .clicked()
                    {
                        chosen = Some(None);
                    }
                    let mut category = "";
                    for entry in material::LIBRARY {
                        if entry.category != category {
                            category = entry.category;
                            ui.label(RichText::new(category).small().color(MUTED));
                        }
                        let selected = assigned.is_some_and(|current| current.key == entry.key);
                        if ui.selectable_label(selected, entry.name).clicked() {
                            chosen = Some(Some(entry.key));
                        }
                    }
                });
        });
        if let Some(choice) = chosen {
            self.set_body_material(active, choice);
        }

        if let Some(material) = assigned {
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 2, material.colour);
                ui.painter().rect_stroke(
                    rect,
                    2,
                    Stroke::new(1.0, BORDER),
                    egui::StrokeKind::Inside,
                );
                ui.label(
                    RichText::new(format!("{:.0} kg/m³", material.density))
                        .small()
                        .color(MUTED),
                );
            });
        }

        let properties = self.mass_properties();
        let unit = self.document_settings.length_unit;
        let per_unit = unit.millimetres_per_unit();
        ui.label(
            RichText::new(format!(
                "Volume {:.3} {}³",
                properties.volume / per_unit.powi(3),
                unit.symbol()
            ))
            .small()
            .color(TEXT),
        );
        match properties.mass_grams {
            Some(grams) if grams >= 1000.0 => {
                ui.label(
                    RichText::new(format!("Mass {:.3} kg", grams / 1000.0))
                        .small()
                        .color(ACCENT),
                );
            }
            Some(grams) => {
                ui.label(
                    RichText::new(format!("Mass {grams:.3} g"))
                        .small()
                        .color(ACCENT),
                );
            }
            None => {
                ui.label(
                    RichText::new("Mass needs a material on every visible body")
                        .small()
                        .color(MUTED),
                );
            }
        }
        match properties.centre {
            Some(centre) => {
                ui.label(
                    RichText::new(format!(
                        "Centre of mass [{:.3}, {:.3}, {:.3}] {}",
                        Self::display_coordinate(centre[0] / per_unit),
                        Self::display_coordinate(centre[1] / per_unit),
                        Self::display_coordinate(centre[2] / per_unit),
                        unit.symbol()
                    ))
                    .small()
                    .color(TEXT),
                );
            }
            None => {
                ui.label(
                    RichText::new("Centre of mass unavailable for this body")
                        .small()
                        .color(MUTED),
                );
            }
        }
    }

    fn model_browser(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().interact_size.y = 24.0;
                egui::CollapsingHeader::new(
                    RichText::new("Document 1 · Root")
                        .font(FontId::proportional(12.5))
                        .color(TEXT)
                        .strong(),
                )
                .id_salt("browser_document")
                .default_open(true)
                .show(ui, |ui| {
                    egui::CollapsingHeader::new(
                        RichText::new("Origin")
                            .font(FontId::proportional(12.0))
                            .color(TEXT)
                            .strong(),
                    )
                    .id_salt("browser_origin")
                    .default_open(true)
                    .show(ui, |ui| {
                        for plane in SketchPlane::ALL {
                            let has_other_plane_sketch =
                                !self.sketch.entities().is_empty() && self.sketch.plane() != plane;
                            let enabled =
                                self.pending_operation.is_none() && !has_other_plane_sketch;
                            let selected = self.selected_origin_plane == plane;
                            let response = ui.add_enabled(
                                enabled,
                                egui::Button::new(origin_plane_label(plane))
                                    .frame(false)
                                    .selected(selected)
                                    .corner_radius(2)
                                    .min_size(egui::vec2(ui.available_width(), 24.0)),
                            );
                            if response.clicked() {
                                self.selected_origin_plane = plane;
                                self.selected_construction_plane = None;
                                if self.sketch.entities().is_empty() {
                                    let _ = self.sketch.set_plane(plane);
                                }
                            }
                            if has_other_plane_sketch {
                                response.on_disabled_hover_text(
                                    "This first profile slice owns one plane per document.",
                                );
                            }
                        }
                    });
                    if !self.construction_planes.is_empty() {
                        let rows = self
                            .construction_planes
                            .iter()
                            .filter(|plane| self.construction_plane_is_active(plane))
                            .map(|plane| {
                                (
                                    plane.id,
                                    plane.name.clone(),
                                    plane.visible,
                                    self.selected_construction_plane == Some(plane.id),
                                )
                            })
                            .collect::<Vec<_>>();
                        let mut visibility_change = None;
                        let mut selected_plane = None;
                        egui::CollapsingHeader::new(
                            RichText::new(format!("Construction ({})", rows.len()))
                                .font(FontId::proportional(12.0))
                                .color(TEXT)
                                .strong(),
                        )
                        .id_salt("browser_construction_planes")
                        .default_open(true)
                        .show(ui, |ui| {
                            for (id, name, visible, selected) in rows {
                                ui.horizontal(|ui| {
                                    if ui
                                        .add_sized(
                                            [22.0, 22.0],
                                            egui::Button::new(if visible { "●" } else { "○" })
                                                .frame(false),
                                        )
                                        .on_hover_text(if visible {
                                            format!("Hide {name}")
                                        } else {
                                            format!("Show {name}")
                                        })
                                        .clicked()
                                    {
                                        visibility_change = Some((id, !visible));
                                    }
                                    if ui
                                        .add_sized(
                                            [(ui.available_width() - 2.0).max(24.0), 22.0],
                                            egui::Button::new(format!("▱  {name}"))
                                                .frame(false)
                                                .selected(selected)
                                                .truncate(),
                                        )
                                        .on_hover_text(format!(
                                            "Select {name} as a sketch support plane"
                                        ))
                                        .clicked()
                                    {
                                        selected_plane = Some(id);
                                    }
                                });
                            }
                        });
                        if let Some((id, visible)) = visibility_change
                            && let Some(plane) = self
                                .construction_planes
                                .iter_mut()
                                .find(|plane| plane.id == id)
                        {
                            plane.visible = visible;
                        }
                        if let Some(id) = selected_plane {
                            self.selected_construction_plane = Some(id);
                            self.clear_model_entity_selection();
                        }
                    }
                    let body_rows = self
                        .bodies
                        .iter()
                        .enumerate()
                        .map(|(index, body)| {
                            let component = self
                                .document
                                .component_instances()
                                .iter()
                                .find(|component| component.bodies.contains(&body.id))
                                .map(|component| (component.id.get(), component.label.clone()));
                            (
                                index,
                                body.ordinal,
                                body.kind,
                                body.body.report.topology.solids,
                                body.visible,
                                body.ordinal == self.active_body_ordinal,
                                component,
                            )
                        })
                        .collect::<Vec<_>>();
                    let mut body_visibility_change = None;
                    let mut activate_body = None;
                    for (index, ordinal, kind, solid_count, visible, active, component) in body_rows
                    {
                        ui.horizontal(|ui| {
                            let object_name = browser_body_object_name(ordinal, solid_count);
                            let visibility_label = if visible {
                                format!("Hide {object_name}")
                            } else {
                                format!("Show {object_name}")
                            };
                            let eye = ui.add_sized(
                                [22.0, 22.0],
                                egui::Button::new(if visible { "●" } else { "○" })
                                    .frame(false)
                                    .corner_radius(2),
                            );
                            eye.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    true,
                                    &visibility_label,
                                )
                            });
                            if eye.clicked() {
                                body_visibility_change = Some((index, !visible));
                            }
                            let (body_icon, body_label, visible_body_label) = component
                                .map_or_else(
                                    || {
                                        let label =
                                            format!("{object_name} · {}", kind.browser_label());
                                        ("◆", label.clone(), label)
                                    },
                                    |(instance, label)| {
                                        (
                                            "◇",
                                            format!("{label} · component {instance}"),
                                            format!("C{instance} · {label}"),
                                        )
                                    },
                                );
                            let accessible_label = format!("{body_icon}  {body_label}");
                            let label_width = (ui.available_width() - 6.0).max(24.0);
                            let response = ui.add_sized(
                                [label_width, 22.0],
                                egui::Button::new(format!("{body_icon}  {visible_body_label}"))
                                    .frame(false)
                                    .selected(active)
                                    .corner_radius(2)
                                    .truncate(),
                            );
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    true,
                                    &accessible_label,
                                )
                            });
                            let response = response.on_hover_text(&body_label);
                            if response.clicked() {
                                activate_body = Some(index);
                            }
                        });
                    }
                    if let Some((index, visible)) = body_visibility_change {
                        self.set_body_visibility(index, visible);
                    }
                    if let Some(index) = activate_body {
                        self.activate_body(index);
                        self.clear_model_entity_selection();
                    }

                    let sketch_rows = self
                        .sketches
                        .iter()
                        .enumerate()
                        .filter(|(_, sketch)| {
                            sketch
                                .id
                                .is_none_or(|id| self.document.sketch(id).is_some())
                        })
                        .map(|(index, sketch)| {
                            (
                                index,
                                sketch.ordinal,
                                sketch.support.label(),
                                sketch.finished,
                                sketch.visible,
                                sketch.consumed,
                                self.active_sketch_index == Some(index),
                            )
                        })
                        .collect::<Vec<_>>();
                    let mut sketch_visibility_change = None;
                    let mut activate_sketch = None;
                    for (index, ordinal, support, finished, visible, consumed, active) in
                        sketch_rows
                    {
                        ui.horizontal(|ui| {
                            let visibility_label = if visible {
                                format!("Hide Sketch {ordinal}")
                            } else {
                                format!("Show Sketch {ordinal}")
                            };
                            let eye = ui.add_sized(
                                [22.0, 22.0],
                                egui::Button::new(if visible { "●" } else { "○" })
                                    .frame(false)
                                    .corner_radius(2),
                            );
                            eye.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    true,
                                    &visibility_label,
                                )
                            });
                            if eye.clicked() {
                                sketch_visibility_change = Some((index, !visible));
                            }
                            let state = if finished || consumed {
                                "finished"
                            } else {
                                "editing"
                            };
                            let label = format!("└  Sketch {ordinal} · {support} · {state}");
                            let response = ui.add_sized(
                                [(ui.available_width() - 2.0).max(24.0), 22.0],
                                egui::Button::new(RichText::new(&label).color(if visible {
                                    ACCENT
                                } else {
                                    MUTED
                                }))
                                .frame(false)
                                .selected(active)
                                .corner_radius(2)
                                .truncate(),
                            );
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    true,
                                    format!("Select Sketch {ordinal}"),
                                )
                            });
                            if response.clicked() {
                                activate_sketch = Some(index);
                            }
                        });
                    }
                    if self.workbench_mode == WorkbenchMode::Sketch
                        && self.active_sketch_index.is_none()
                        && self.sketch.entities().is_empty()
                    {
                        browser_text_row(
                            ui,
                            &format!(
                                "└  Sketch {} · {} · empty",
                                self.feature_preview.current_sketch_ordinal(),
                                self.sketch_support.label()
                            ),
                            ACCENT,
                        );
                    }
                    if let Some((index, visible)) = sketch_visibility_change {
                        self.set_sketch_visibility(index, visible);
                    }
                    if let Some(index) = activate_sketch {
                        self.activate_committed_sketch(index);
                    }

                    let joint_rows = self
                        .document
                        .joints()
                        .iter()
                        .map(|joint| {
                            (
                                joint.id,
                                joint.name.clone(),
                                joint.child,
                                joint.kind,
                                joint.enabled,
                            )
                        })
                        .collect::<Vec<_>>();
                    if !joint_rows.is_empty() {
                        egui::CollapsingHeader::new(
                            RichText::new(format!("Joints ({})", joint_rows.len()))
                                .font(FontId::proportional(12.0))
                                .color(TEXT)
                                .strong(),
                        )
                        .id_salt("browser_joints")
                        .default_open(true)
                        .show(ui, |ui| {
                            for (id, name, child, kind, enabled) in joint_rows {
                                let kind = match kind {
                                    JointKind::Fixed => "Fixed",
                                    JointKind::Revolute { .. } => "Revolute",
                                };
                                browser_text_row(
                                    ui,
                                    &format!(
                                        "{}  {name} · {kind} · C{} · {id}",
                                        if enabled { "↻" } else { "○" },
                                        child.get()
                                    ),
                                    if enabled { GOOD } else { MUTED },
                                );
                            }
                        });
                    }
                });
            });
    }

    /// The left dock holds the document tree and nothing else.
    ///
    /// Tool options and feature editors float over the viewport instead. A
    /// docked inspector that swapped itself in during a command kept moving
    /// the tree out from under the pointer, and the tree is the one thing that
    /// has to stay where the user left it.
    fn left_workspace_panel(&mut self, ui: &mut egui::Ui) {
        let operation_pending = self.operation_confirmation_pending();
        ui.horizontal(|ui| {
            ui.label(RichText::new("BROWSER").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = ui.small_button("−").on_hover_text("Collapse browser panel");
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        "Collapse browser panel",
                    )
                });
                if shell_button_activated(ui, &response, operation_pending) {
                    self.shell.set_model_browser(false);
                }
            });
        });
        ui.separator();
        self.model_browser(ui);
    }

    fn show_properties_tab(&mut self) {
        self.inspector_open = true;
    }

    fn committed_export_triangles(&self) -> Vec<ExportTriangle> {
        self.bodies
            .iter()
            .filter(|body| body.visible)
            .flat_map(|body| {
                let placement = self.occurrence_transform_for_body(body.id);
                // Interchange facets are regenerated at the kernel
                // approximation budget: the retained display scene spends a
                // coarser presentation chord budget that must never define
                // exported geometry quality.
                NativeKernel::authoritative_scene(&body.body.snapshot)
                    .triangles
                    .into_iter()
                    .map(move |triangle| ExportTriangle {
                        body: body.id.get(),
                        vertices: triangle
                            .vertices
                            .map(|point| placement.transform_point(point)),
                    })
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    fn export_stl_to_path(&self, path: &Path) -> Result<(), String> {
        if self.pending_operation.is_some() {
            return Err("confirm or cancel the pending operation before exporting".into());
        }
        write_ascii_stl(path, &self.committed_export_triangles())
    }

    fn export_step_to_path(&self, path: &Path) -> Result<(), String> {
        if self.pending_operation.is_some() {
            return Err("confirm or cancel the pending operation before exporting".into());
        }
        write_faceted_step(path, &self.committed_export_triangles())
    }

    /// Workspace settings and diagnostics.
    ///
    /// These are things you open on purpose, not things you watch while
    /// modelling, so they live in the document popout rather than the
    /// floating inspector. Keeping them out also keeps the inspector short
    /// enough that its tail never scrolls out of reach.
    fn workspace_settings_cards(&mut self, ui: &mut egui::Ui) {
        self.document_parameter_controls(ui);
        ui.add_space(5.0);
        collapsible_card(ui, "navigation_scheme", "NAVIGATION", false, |ui| {
            self.navigation_card(ui);
        });
        ui.add_space(5.0);
        collapsible_card(ui, "lab_diagnostics", "LAB / DIAGNOSTICS", false, |ui| {
            if let Some(recorder) = self.development_recorder.as_ref() {
                collapsible_card(ui, "development_trace", "DEVELOPMENT TRACE", true, |ui| {
                    status_line(ui, "Session log active", GOOD);
                    ui.label(
                        RichText::new(
                            recorder
                                .session_path()
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("session.jsonl"),
                        )
                        .small()
                        .color(MUTED),
                    );
                    ui.label(
                                    RichText::new(
                                        "Local only · gestures are coalesced · text and clipboard data are excluded",
                                    )
                                    .small()
                                    .color(MUTED),
                                );
                });
                ui.add_space(5.0);
            }
            collapsible_card(ui, "compute_activity", "COMPUTE ACTIVITY", true, |ui| {
                let compute = ComputePool::global();
                let config = compute.config();
                ui.label(
                    RichText::new(format!(
                        "{} worker{} · parallel from {} work items",
                        config.threads,
                        if config.threads == 1 { "" } else { "s" },
                        config.parallel_min_items
                    ))
                    .color(ACCENT),
                );
                let metrics = compute.recent_metrics();
                if metrics.is_empty() {
                    ui.label(RichText::new("No measured compute batches yet").color(MUTED));
                } else {
                    for metric in metrics.iter().rev().take(6) {
                        let mode = match metric.mode {
                            ExecutionMode::Serial => "1 thread",
                            ExecutionMode::Parallel => "parallel",
                        };
                        ui.label(
                            RichText::new(format!(
                                "{} · {} · {} items · {:.2} ms",
                                metric.task,
                                mode,
                                metric.items,
                                metric.elapsed.as_secs_f64() * 1_000.0
                            ))
                            .small()
                            .color(MUTED),
                        );
                    }
                }
            });
            ui.add_space(5.0);
            collapsible_card(ui, "diagnostic_cases", "DIAGNOSTIC CASES", true, |ui| {
                let operation_pending = self.operation_confirmation_pending();
                for case in LabCase::ALL {
                    let staged = matches!(
                        self.pending_operation,
                        Some(PendingOperation::RunCase {
                            case: pending_case,
                            ..
                        }) if pending_case == case
                    );
                    let selected = self.last_case == case || staged;
                    let button = egui::Button::new(
                        RichText::new(case.title())
                            .color(if staged {
                                WARN
                            } else if selected {
                                TEXT
                            } else {
                                MUTED
                            })
                            .strong(),
                    )
                    .selected(selected)
                    .corner_radius(3)
                    .fill(if selected { SELECTED_FILL } else { CARD });
                    let response = ui.add_enabled(
                        !operation_pending,
                        button.min_size(egui::vec2(ui.available_width(), 30.0)),
                    );
                    if response.clicked() {
                        self.stage_case(case);
                    }
                    if operation_pending {
                        response.on_disabled_hover_text(
                            "Confirm or cancel the pending operation first.",
                        );
                    }
                    ui.label(RichText::new(case.detail()).small().color(MUTED));
                    ui.add_space(5.0);
                }
            });

            ui.add_space(5.0);
            collapsible_card(ui, "motion", "MOTION", false, |ui| {
                self.motion_controls(ui);
            });

            ui.add_space(5.0);
            collapsible_card(ui, "transform_preview", "TRANSFORM PREVIEW", false, |ui| {
                self.transform_controls(ui);
            });

            ui.add_space(5.0);
            collapsible_card(ui, "last_transaction", "LAST TRANSACTION", true, |ui| {
                self.attempt_card(ui);
            });

            ui.add_space(5.0);
            collapsible_card(ui, "display", "DISPLAY", false, |ui| {
                ui.checkbox(&mut self.edge_overlay, "Source edge overlay");
                ui.add_space(3.0);
                ui.label(
                            RichText::new(
                                "Every model change stays pending until Enter or the green tick confirms it.",
                            )
                            .small()
                            .color(MUTED),
                        );
            });
        });
    }
    fn document_properties_window(&mut self, context: &egui::Context) {
        if !self.document_properties_open {
            return;
        }
        let mut open = self.document_properties_open;
        egui::Window::new("DOCUMENT PROPERTIES")
            .id(egui::Id::new("document_properties_window"))
            // Keep the persistent navigation cube unobstructed.
            // Kept off the centre of the viewport: a window there intercepts
            // the drags that orbit and move the model. The inspector is docked
            // now, so this no longer has anything to collide with.
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-18.0, 252.0))
            .default_width(340.0)
            .max_height(510.0)
            .resizable(false)
            .open(&mut open)
            .frame(
                Frame::new()
                    .fill(PANEL.gamma_multiply(0.98))
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(6)
                    .inner_margin(Margin::same(10)),
            )
            .show(context, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.workspace_settings_cards(ui);
                    ui.add_space(6.0);
                    ui.label(RichText::new("UNITS").small().color(MUTED));
                    let previous = self.document_settings.length_unit;
                    egui::ComboBox::new("document_length_unit", "Length unit")
                        .selected_text(previous.label())
                        .width(220.0)
                        .show_ui(ui, |ui| {
                            for unit in DisplayLengthUnit::ALL {
                                ui.selectable_value(
                                    &mut self.document_settings.length_unit,
                                    unit,
                                    unit.label(),
                                );
                            }
                        });
                    if previous != self.document_settings.length_unit {
                        self.document_status = Some(format!(
                            "Display units changed to {}. Kernel geometry remains canonical millimetres.",
                            self.document_settings.length_unit.label()
                        ));
                    }
                    ui.label(
                        RichText::new(
                            "Measurement readouts use this unit. Kernel geometry, current feature-entry fields, and interchange authority remain millimetres.",
                        )
                        .small()
                        .color(MUTED),
                    );

                    ui.separator();
                    ui.label(RichText::new("ARTIFICER DOCUMENT").small().color(MUTED));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.document_path_text)
                            .desired_width(f32::INFINITY)
                            .hint_text("/path/to/design.artificer"),
                    )
                    .on_hover_text("Portable feature/history workspace path");
                    let requested_document_path = PathBuf::from(self.document_path_text.trim());
                    let operation_pending = self.pending_operation.is_some();
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!operation_pending, egui::Button::new("Save .ARTIFICER"))
                            .clicked()
                        {
                            self.set_document_path(requested_document_path.clone());
                            self.document_status = Some(
                                self.save_workspace_to_path(&requested_document_path).map_or_else(
                                    |error| format!("Save failed: {error}"),
                                    |()| {
                                        format!(
                                            "Saved Artificer workspace to {}",
                                            requested_document_path.display()
                                        )
                                    },
                                ),
                            );
                        }
                        if ui
                            .add_enabled(
                                !operation_pending && requested_document_path.is_file(),
                                egui::Button::new("Open .ARTIFICER"),
                            )
                            .clicked()
                        {
                            self.set_document_path(requested_document_path.clone());
                            self.pending_operation = Some(PendingOperation::LoadDefaultDocument);
                        }
                    });
                    ui.label(
                        RichText::new(format!(
                            "Workspace envelope v{ARTIFICER_WORKSPACE_VERSION} · feature history + settings"
                        ))
                        .small()
                        .color(GOOD),
                    );

                    ui.separator();
                    ui.label(RichText::new("INTERCHANGE EXPORT").small().color(MUTED));
                    ui.label(RichText::new("STL path").small().color(MUTED));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.stl_export_path_text)
                            .desired_width(f32::INFINITY),
                    );
                    let stl_path = PathBuf::from(self.stl_export_path_text.trim());
                    if ui
                        .add_enabled(!operation_pending, egui::Button::new("Export STL"))
                        .clicked()
                    {
                        self.document_status = Some(
                            self.export_stl_to_path(&stl_path).map_or_else(
                                |error| format!("STL export failed: {error}"),
                                |()| format!("Exported STL to {}", stl_path.display()),
                            ),
                        );
                    }
                    ui.label(RichText::new("STEP path").small().color(MUTED));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.step_export_path_text)
                            .desired_width(f32::INFINITY),
                    );
                    let step_path = PathBuf::from(self.step_export_path_text.trim());
                    if ui
                        .add_enabled(!operation_pending, egui::Button::new("Export faceted STEP"))
                        .clicked()
                    {
                        self.document_status = Some(
                            self.export_step_to_path(&step_path).map_or_else(
                                |error| format!("STEP export failed: {error}"),
                                |()| format!("Exported faceted STEP to {}", step_path.display()),
                            ),
                        );
                    }
                    ui.label(
                        RichText::new(
                            "STL and this first STEP exporter use the committed visible tessellation in millimetres; previews are never exported.",
                        )
                        .small()
                        .color(MUTED),
                    );
                });
            });
        self.document_properties_open = open;
    }

    /// The context inspector, docked to the right of the viewport.
    ///
    /// This is where the active tool's options live. It is docked rather than
    /// floating because it is always on: a permanent window over the canvas
    /// hides a quarter of the drawing area and silently swallows every click
    /// that lands on it. Transient command editors still float, because they
    /// come and go with one operation.
    fn contextual_inspector_panel(&mut self, ui: &mut egui::Ui) {
        let title = match self.workbench_mode {
            WorkbenchMode::Model => "PROPERTIES",
            WorkbenchMode::Sketch => "SKETCH",
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = ui.small_button("−").on_hover_text("Collapse inspector");
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Collapse inspector")
                });
                if response.clicked() {
                    self.inspector_open = false;
                }
            });
        });
        ui.separator();
        // Reserve the exact contents rectangle in the parent, then render into
        // a detached clipped child. egui clamps oversized panel content after
        // layout, and without this isolation one long diagnostic line moves the
        // panel's inner edge — which shifts the viewport beside it and takes
        // every model coordinate under the pointer with it.
        let content_rect = ui.available_rect_before_wrap();
        let _ = ui.allocate_rect(content_rect, egui::Sense::hover());
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("contextual_inspector_contents")
                .max_rect(content_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        ui.set_clip_rect(content_rect);
        let ui = &mut ui;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            // Reserve the scrollbar always. Letting it appear and disappear
            // with the content changes the panel's width, which shifts the
            // viewport beside it by a fraction of a point — enough to move
            // every model coordinate under the pointer.
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show(ui, |ui| match self.workbench_mode {
                WorkbenchMode::Model => self.controls(ui),
                WorkbenchMode::Sketch => self.sketch_inspector(ui),
            });
    }

    fn edge_finish_editor(&mut self, context: &egui::Context) {
        let Some(PendingOperation::PresetFeature { preset, .. }) = self.pending_operation else {
            return;
        };
        if !matches!(
            preset,
            SolidFeaturePreset::Chamfer | SolidFeaturePreset::Fillet
        ) {
            return;
        }
        let title = if preset == SolidFeaturePreset::Chamfer {
            "Chamfer"
        } else {
            "Fillet"
        };
        egui::Window::new(title)
            .id(egui::Id::new("edge_finish_feature_editor"))
            .anchor(egui::Align2::RIGHT_CENTER, egui::vec2(-18.0, 0.0))
            .collapsible(true)
            .resizable(false)
            .default_width(248.0)
            .frame(
                Frame::new()
                    .fill(PANEL.gamma_multiply(0.98))
                    .stroke(Stroke::new(1.0, BORDER))
                    .corner_radius(6)
                    .inner_margin(Margin::same(10)),
            )
            .show(context, |ui| {
                let support = self.edge_finish_selection_support();
                status_line(
                    ui,
                    &format!("{} EDGES", self.selected_edges.len()),
                    if self.selected_edges.is_empty() { BAD } else { ACCENT },
                );
                ui.label(
                    RichText::new("Shift-click adds or removes edges from this feature.")
                        .small()
                        .color(MUTED),
                );
                status_line(
                    ui,
                    support.headline(),
                    if support.can_commit() { GOOD } else { BAD },
                );
                ui.label(
                    RichText::new(support.detail())
                        .small()
                        .color(if support.can_commit() { MUTED } else { BAD }),
                );
                ui.separator();
                ui.label(RichText::new(if preset == SolidFeaturePreset::Chamfer {
                    "Setback distance"
                } else {
                    "Radius"
                }).small().color(MUTED));
                let slider = ui.add(
                    egui::Slider::new(&mut self.edge_finish_distance, 0.01..=10.0)
                        .logarithmic(true)
                        .show_value(false)
                        .text("Distance"),
                );
                if slider.changed() {
                    self.edge_finish_distance_text = format!("{:.3}", self.edge_finish_distance);
                }
                let editor = ui.add(
                    egui::TextEdit::singleline(&mut self.edge_finish_distance_text)
                        .id(egui::Id::new("edge_finish_dimension"))
                        .desired_width(112.0)
                        .font(FontId::monospace(12.0))
                        .hint_text("Distance mm"),
                );
                if editor.changed()
                    && let Ok(value) = self.edge_finish_distance_text.trim().parse::<f64>()
                    && value.is_finite()
                    && value > 0.0
                {
                    self.edge_finish_distance = value;
                }
                editor.on_hover_text("Type the exact value; Tab moves to the next feature option.");
                ui.label(
                    RichText::new(format!("{:.3} mm", self.edge_finish_distance))
                        .monospace()
                        .color(GOOD),
                );
                ui.separator();
                if ui
                    .checkbox(&mut self.edge_finish_tangent_chain, "Tangent chain")
                    .changed()
                    && self.edge_finish_tangent_chain
                {
                    self.apply_tangent_edge_chain();
                }
                ui.label(
                    RichText::new(
                        "Manual Shift selections form one chain. Tangent chain adds connected collinear continuations.",
                    )
                    .small()
                    .color(MUTED),
                );
                ui.separator();
                ui.label(
                    RichText::new("Drag the in-canvas diamond to adjust this value visually.")
                        .small()
                        .color(ACCENT),
                );
            });
    }

    fn extrusion_feature_editor(&mut self, context: &egui::Context) {
        let active_sketch_consumed = self
            .active_sketch_index
            .and_then(|index| self.sketches.get(index))
            .is_some_and(|sketch| sketch.consumed);
        // The inspector already carries the extrusion controls for a ready
        // sketch. Showing this editor at the same time would put two live
        // Distance fields on screen, which is confusing to use and ambiguous
        // to drive.
        if self.document_properties_open
            || self.inspector_open
            || self.workbench_mode != WorkbenchMode::Model
            || self.sketch_extrusion_eligibility() != SketchExtrusionEligibility::Ready
            || self.pending_operation.is_some()
            || active_sketch_consumed
            || self.extruded_sketch_revision == Some(self.sketch_revision)
        {
            return;
        }
        let face_supported = self.sketch_support.body().is_some();
        egui::Window::new("EXTRUSION")
            .id(egui::Id::new("extrusion_feature_editor"))
            // Command editors share the centre-right slot below the inspector.
            // Extrude and the edge finishes cannot both be live: one requires
            // no pending operation, the other requires one.
            .anchor(egui::Align2::RIGHT_CENTER, egui::vec2(-18.0, 0.0))
            .default_width(230.0)
            .resizable(false)
            .collapsible(true)
            .show(context, |ui| {
                ui.label(RichText::new("OPERATION").small().color(MUTED));
                ui.horizontal(|ui| {
                    let modes: &[_] = if face_supported {
                        &[ExtrusionMode::Add, ExtrusionMode::Cut]
                    } else {
                        &[ExtrusionMode::NewBody]
                    };
                    for &mode in modes {
                        if ui
                            .add(
                                egui::Button::new(mode.label())
                                    .selected(self.extrusion_mode == mode)
                                    .corner_radius(3),
                            )
                            .on_hover_text(format!(
                                "{} this face sketch without changing its direction",
                                mode.label()
                            ))
                            .clicked()
                        {
                            self.select_extrusion_mode(mode);
                        }
                    }
                });
                ui.add(
                    egui::DragValue::new(&mut self.extrusion_distance)
                        .speed(0.1)
                        .range(if face_supported {
                            -1_000.0..=1_000.0
                        } else {
                            0.01..=1_000.0
                        })
                        .max_decimals(3)
                        .prefix("Distance ")
                        .suffix(" mm"),
                );
            });
    }

    fn sketch_inspector(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .scroll_bar_visibility(
                egui::containers::scroll_area::ScrollBarVisibility::AlwaysVisible,
            )
            .show(ui, |ui| {
                collapsible_card(ui, "sketch_plane", "SKETCH PLANE", true, |ui| {
                    ui.label(
                        RichText::new(self.sketch_support.label())
                            .color(ACCENT)
                            .strong(),
                    );
                    if matches!(&self.sketch_support, SketchSupport::PlanarFace { .. }) {
                        ui.label(
                            RichText::new("Authoritative face-local frame · reference boundary")
                                .small()
                                .color(GOOD),
                        );
                    }
                    let screen_axes = self.sketch_screen_axis_labels();
                    let normal = self.sketch_support.display_normal();
                    ui.label(
                        RichText::new(format!(
                            "Horizontal {} · vertical {}",
                            screen_axes[0], screen_axes[1]
                        ))
                        .small()
                        .color(MUTED),
                    );
                    ui.label(
                        RichText::new(format!(
                            "Normal [{:.0}, {:.0}, {:.0}]",
                            normal.x, normal.y, normal.z
                        ))
                        .small()
                        .color(MUTED),
                    );
                });

                ui.add_space(7.0);
                collapsible_card(ui, "active_sketch_tool", "ACTIVE TOOL", true, |ui| {
                    let descriptor = self.active_sketch_tool.descriptor();
                    ui.horizontal(|ui| {
                        let (icon_rect, _) =
                            ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
                        paint_tool_icon(
                            ui.painter(),
                            icon_rect.shrink(3.0),
                            descriptor.icon,
                            ACCENT,
                        );
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(descriptor.accessible_name)
                                    .color(ACCENT)
                                    .strong(),
                            );
                            ui.add(
                                egui::Label::new(
                                    RichText::new(descriptor.short_tooltip)
                                        .small()
                                        .color(MUTED),
                                )
                                .wrap(),
                            );
                        });
                    });

                    let progress = self.sketch.gesture_progress();
                    let step = if progress.awaiting_confirmation {
                        "Geometry staged · awaiting confirmation".to_owned()
                    } else if progress.required_points == 0 {
                        descriptor.prompt.to_owned()
                    } else if let Some(phase) = descriptor
                        .acquisition_phases
                        .get(usize::from(progress.completed_points))
                    {
                        format!(
                            "Step {} of {} · {}",
                            progress.completed_points + 1,
                            progress.required_points,
                            phase.prompt
                        )
                    } else {
                        "Gesture complete · Enter stages".to_owned()
                    };
                    ui.label(RichText::new(step).small().color(if progress.awaiting_confirmation {
                        GOOD
                    } else {
                        TEXT
                    }));

                    let selection = match descriptor.selection {
                        SelectionRequirement::None => None,
                        SelectionRequirement::CurveSpanUnderPointer => {
                            Some("Point to one exact curve span")
                        }
                        SelectionRequirement::OneOrMoreEditableEntities => {
                            Some("Select one or more editable entities")
                        }
                        SelectionRequirement::TwoConnectedProfileCurves => {
                            Some("Select two connected profile curves")
                        }
                        SelectionRequirement::TwoConnectedProfileLines => {
                            Some("Select two connected profile lines")
                        }
                        SelectionRequirement::RelationOperands => {
                            Some("Pick the curves or endpoints to relate")
                        }
                    };
                    if let Some(requirement) = selection {
                        ui.label(
                            RichText::new(format!(
                                "{requirement} · {} selected",
                                usize::from(self.sketch.selected().is_some())
                            ))
                            .small()
                            .color(if self.sketch.selected().is_some() {
                                GOOD
                            } else {
                                WARN
                            }),
                        );
                    }

                    if self.active_sketch_tool == ToolVariant::ChainedPolyline
                        && !progress.awaiting_confirmation
                    {
                        let can_finish = self.sketch.polyline_draft_can_finish();
                        let response = ui.add_enabled(
                            can_finish,
                            egui::Button::new("Finish chain").corner_radius(5),
                        );
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                can_finish,
                                "Finish chained polyline",
                            )
                        });
                        let response = if can_finish {
                            response.on_hover_text(
                                "Stage all accepted segments as one operation; the green tick or Enter then commits it.",
                            )
                        } else {
                            response.on_disabled_hover_text(
                                "Accept at least two distinct vertices before finishing the chain.",
                            )
                        };
                        if response.clicked()
                            && let Ok(subject) = self.sketch.finish_polyline_draft()
                        {
                            self.commit_sketch_stroke(subject);
                        }
                    }

                    let operation_pending =
                        self.pending_operation.is_some() || self.sketch.has_pending_edit();
                    let mut individual_input_error_visible = false;
                    for input in descriptor.inputs {
                        let conditionally_enabled = match (
                            self.active_sketch_tool,
                            input.stable_key,
                        ) {
                            (ToolVariant::RectangularPattern, "count_v" | "spacing_v") => self
                                .sketch
                                .active_tool_flag("second_direction")
                                .unwrap_or(false),
                            (ToolVariant::CircularPattern, "extent") => !self
                                .sketch
                                .active_tool_flag("full_circle")
                                .unwrap_or(true),
                            _ => true,
                        };
                        let enabled = !operation_pending && conditionally_enabled;
                        if input.kind == ToolInputKind::Boolean {
                            let mut value = self
                                .sketch
                                .active_tool_flag(input.stable_key)
                                .unwrap_or(false);
                            let response = ui.add_enabled(
                                !operation_pending,
                                egui::Checkbox::new(&mut value, input.label),
                            );
                            let response = response.on_hover_text(input.domain);
                            response.ctx.accesskit_node_builder(response.id, |node| {
                                node.set_label(input.label);
                                node.set_description(input.domain);
                            });
                            if response.changed() {
                                self.sketch
                                    .set_active_tool_flag(input.stable_key, value);
                            }
                            continue;
                        }

                        if let Some(mut text) =
                            self.sketch.active_tool_input_text(input.stable_key)
                        {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(input.label).small().color(if enabled {
                                    MUTED
                                } else {
                                    MUTED.gamma_multiply(0.58)
                                }));
                                let response = ui.add_enabled(
                                    enabled,
                                    egui::TextEdit::singleline(&mut text)
                                        .id(egui::Id::new((
                                            "active_sketch_tool_input",
                                            descriptor.stable_key,
                                            input.stable_key,
                                        )))
                                        .desired_width(86.0)
                                        .font(FontId::monospace(11.0)),
                                );
                                let response = response.on_hover_text(if conditionally_enabled {
                                    input.domain
                                } else {
                                    "Inactive for the current distribution mode"
                                });
                                response.ctx.accesskit_node_builder(response.id, |node| {
                                    node.set_label(input.label);
                                    node.set_description(format!(
                                        "{}. Invalid text keeps the last valid preview and blocks staging.",
                                        input.domain
                                    ));
                                });
                                if response.changed() {
                                    self.sketch.set_active_tool_input_text(
                                        input.stable_key,
                                        text.clone(),
                                    );
                                }
                                let owns_keyboard = response.has_focus() || response.lost_focus();
                                let (enter, escape) = ui.ctx().input(|state| {
                                    let pressed = |wanted: egui::Key| {
                                        state.raw.events.iter().any(|event| {
                                            matches!(
                                                event,
                                                egui::Event::Key {
                                                    key,
                                                    pressed: true,
                                                    repeat: false,
                                                    modifiers,
                                                    ..
                                                } if *key == wanted
                                                    && *modifiers == egui::Modifiers::NONE
                                            )
                                        })
                                    };
                                    (pressed(egui::Key::Enter), pressed(egui::Key::Escape))
                                });
                                if owns_keyboard && escape {
                                    self.sketch.restore_active_tool_input(input.stable_key);
                                    self.sketch_dimension_keys.escape = true;
                                    response.surrender_focus();
                                }
                                if owns_keyboard && enter {
                                    self.sketch_dimension_keys.enter = true;
                                    if self
                                        .sketch
                                        .active_tool_input_error(input.stable_key)
                                        .is_none()
                                    {
                                        response.surrender_focus();
                                    }
                                }
                                let unit = match input.kind {
                                    ToolInputKind::Length | ToolInputKind::SignedLength => "mm",
                                    ToolInputKind::Angle => "°",
                                    ToolInputKind::Integer
                                    | ToolInputKind::Choice
                                    | ToolInputKind::Boolean => "",
                                };
                                if !unit.is_empty() {
                                    ui.label(RichText::new(unit).small().color(MUTED));
                                }
                            });
                            if let Some(error) =
                                self.sketch.active_tool_input_error(input.stable_key)
                            {
                                individual_input_error_visible = true;
                                ui.label(RichText::new(error.label()).small().color(BAD));
                            } else if !conditionally_enabled {
                                ui.label(
                                    RichText::new("Inactive in current mode")
                                        .small()
                                        .color(MUTED),
                                );
                            }
                        } else {
                            ui.label(
                                RichText::new(format!("{} · live on canvas", input.label))
                                    .small()
                                    .color(MUTED),
                            )
                            .on_hover_text(input.domain);
                        }
                    }
                    if let Some(issue) = self.sketch.active_tool_parameter_issue() {
                        self.sketch_dimension_keys.confirmation_blocked = true;
                        if !individual_input_error_visible {
                            ui.label(RichText::new(issue.label()).small().color(BAD));
                        }
                    }
                    ui.label(
                        RichText::new("Tab / Shift-Tab moves fields · Enter accepts · tick commits")
                            .small()
                            .color(GOOD),
                    );
                });

                ui.add_space(7.0);
                let recipe_edit_pending = matches!(
                    self.pending_operation,
                    Some(PendingOperation::SketchEdit {
                        label: "Edit sketch parameters",
                        ..
                    })
                );
                if (matches!(
                    self.active_sketch_tool,
                    ToolVariant::Select | ToolVariant::Dimension
                ) || recipe_edit_pending)
                    && let Some(editor) = self.sketch.selected_recipe_editor()
                {
                    collapsible_card(
                        ui,
                        "selected_sketch_feature",
                        "SELECTED FEATURE",
                        true,
                        |ui| {
                            ui.label(RichText::new(editor.title).color(ACCENT).strong());
                            ui.label(
                                RichText::new(if editor.parameters.is_empty() {
                                    "READ-ONLY RECIPE"
                                } else {
                                    "EDIT PARAMETERS"
                                })
                                .small()
                                .color(MUTED),
                            );
                            if editor.parameters.is_empty() {
                                ui.label(
                                    RichText::new("No editable literal dimensions")
                                        .small()
                                        .color(MUTED),
                                );
                            }
                            for parameter in editor.parameters {
                                let enabled = parameter.editable
                                    && (self.pending_operation.is_none() || recipe_edit_pending);
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(parameter.label).small().color(if enabled {
                                            MUTED
                                        } else {
                                            MUTED.gamma_multiply(0.62)
                                        }),
                                    );
                                    let mut text = parameter.text.clone();
                                    let response = ui.add_enabled(
                                        enabled,
                                        egui::TextEdit::singleline(&mut text)
                                            .id(egui::Id::new((
                                                "selected_sketch_recipe_parameter",
                                                parameter.stable_key,
                                            )))
                                            .desired_width(82.0)
                                            .font(FontId::monospace(11.0)),
                                    );
                                    let hover = parameter.read_only_reason.unwrap_or(
                                        "Edits rebuild this recipe and every dependent curve exactly.",
                                    );
                                    let response = if enabled {
                                        response.on_hover_text(hover)
                                    } else {
                                        response.on_disabled_hover_text(hover)
                                    };
                                    response.ctx.accesskit_node_builder(response.id, |node| {
                                        node.set_label(parameter.label);
                                        node.set_description(hover);
                                    });
                                    if response.changed() {
                                        let staged = self.sketch.set_selected_recipe_parameter_text(
                                            parameter.stable_key,
                                            text,
                                        );
                                        if self.pending_operation.is_none()
                                            && let Some(subject) = staged
                                        {
                                            self.stage_sketch_edit(subject);
                                        }
                                    }
                                    let owns_keyboard =
                                        response.has_focus() || response.lost_focus();
                                    let (enter, escape) = ui.ctx().input(|state| {
                                        let pressed = |wanted: egui::Key| {
                                            state.raw.events.iter().any(|event| {
                                                matches!(
                                                    event,
                                                    egui::Event::Key {
                                                        key,
                                                        pressed: true,
                                                        repeat: false,
                                                        modifiers,
                                                        ..
                                                    } if *key == wanted
                                                        && *modifiers == egui::Modifiers::NONE
                                                )
                                            })
                                        };
                                        (pressed(egui::Key::Enter), pressed(egui::Key::Escape))
                                    });
                                    if owns_keyboard && enter {
                                        self.sketch_dimension_keys.enter = true;
                                        if self.sketch.selected_recipe_parameter_issue().is_none() {
                                            response.surrender_focus();
                                        }
                                    }
                                    if owns_keyboard && escape && !recipe_edit_pending {
                                        self.sketch.restore_selected_recipe_parameter(
                                            parameter.stable_key,
                                        );
                                        self.sketch_dimension_keys.escape = true;
                                        response.surrender_focus();
                                    }
                                    if !parameter.unit.is_empty() {
                                        ui.label(
                                            RichText::new(parameter.unit).small().color(MUTED),
                                        );
                                    }
                                });
                                if let Some(error) = parameter.error {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(error.label()).small().color(BAD),
                                        )
                                        .wrap(),
                                    );
                                } else if let Some(reason) = parameter.read_only_reason {
                                    ui.label(RichText::new(reason).small().color(MUTED));
                                }
                            }
                            if self.sketch.selected_recipe_parameter_issue().is_some() {
                                self.sketch_dimension_keys.confirmation_blocked = true;
                            }
                            ui.add(
                                egui::Label::new(
                                    RichText::new(editor.reference_note).small().color(MUTED),
                                )
                                .wrap(),
                            );
                            if recipe_edit_pending {
                                ui.label(
                                    RichText::new("Live exact preview · tick/Enter commits")
                                        .small()
                                        .color(GOOD),
                                );
                            }
                        },
                    );
                    ui.add_space(7.0);
                }

                collapsible_card(ui, "live_dimensions", "LIVE DIMENSIONS", true, |ui| {
                    let readouts = self.sketch.dimension_readouts();
                    if readouts.is_empty() {
                        ui.label(
                            RichText::new("Start drawing or select an entity")
                                .small()
                                .color(MUTED),
                        );
                    }
                    for readout in readouts {
                        let unit = if matches!(
                            readout.kind,
                            SketchDimensionKind::AngleDegrees
                                | SketchDimensionKind::SweepDegrees
                        ) {
                            "°"
                        } else {
                            " mm"
                        };
                        let active = self.sketch.active_dimension() == Some(readout.kind);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(readout.kind.label())
                                    .small()
                                    .color(if active { ACCENT } else { MUTED }),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!("{:.3}{unit}", readout.value))
                                            .monospace()
                                            .color(if readout.locked { GOOD } else { TEXT }),
                                    );
                                },
                            );
                        });
                    }
                    if let Some(error) = self.sketch.dimension_error() {
                        ui.label(RichText::new(error.label()).small().color(BAD));
                    } else if self.sketch.dimension_editor_active() {
                        ui.label(
                            RichText::new("Type a value · Tab cycles · Enter stages")
                                .small()
                                .color(GOOD),
                        );
                    } else if !self.sketch.dimension_readouts().is_empty() {
                        ui.label(
                            RichText::new("Tab cycles editable values")
                                .small()
                                .color(MUTED),
                        );
                    }
                });

                ui.add_space(7.0);
                collapsible_card(
                    ui,
                    "profile_diagnostics",
                    "PROFILE DIAGNOSTICS",
                    true,
                    |ui| {
                    let profile = self.sketch.certified_profile_status();
                    status_line(ui, profile.label(), profile_status_color(profile));
                    let diagnostics = self.sketch.diagnostics();
                    let selected_regions = self.sketch.selected_region_count();
                    let available_regions = self.sketch.available_region_count();
                    if available_regions > 0 {
                        ui.label(
                            RichText::new(format!(
                                "{selected_regions} selected · {available_regions} available"
                            ))
                            .small()
                            .color(if selected_regions > 0 { GOOD } else { WARN }),
                        );
                        ui.add(
                            egui::Label::new(
                                RichText::new("Click inside a bounded profile cell · Shift-click adds")
                                    .small()
                                    .color(MUTED),
                            )
                            .wrap(),
                        );
                    }
                    ui.label(
                        RichText::new(format!(
                            "{} entities · {} pending",
                            self.sketch.entities().len(),
                            diagnostics.pending_entities
                        ))
                        .small()
                        .color(MUTED),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{} regions · {} closed loops · {} holes",
                            diagnostics.material_regions,
                            diagnostics.certified_loops,
                            diagnostics.profile_holes
                        ))
                        .small()
                        .color(if diagnostics.certified_loops > 0 {
                            GOOD
                        } else {
                            MUTED
                        }),
                    );
                    if diagnostics.analytic_curves > 0 {
                        ui.label(
                            RichText::new(format!(
                                "{} exact analytic curve{}",
                                diagnostics.analytic_curves,
                                if diagnostics.analytic_curves == 1 { "" } else { "s" }
                            ))
                            .small()
                            .color(ACCENT),
                        );
                    }
                    if diagnostics.open_wire_components > 0
                        || diagnostics.branched_vertices > 0
                    {
                        ui.label(
                            RichText::new(format!(
                                "{} open wire components · {} branched vertices",
                                diagnostics.open_wire_components,
                                diagnostics.branched_vertices
                            ))
                            .small()
                            .color(WARN),
                        );
                    }
                    ui.add(
                        egui::Label::new(
                            RichText::new(
                                "Entity order and direction do not matter. Exact circles and connected arc chains remain analytic.",
                            )
                            .small()
                            .color(MUTED),
                        )
                        .wrap(),
                    );
                    if let Some(error) = self.sketch_last_error {
                        ui.label(
                            RichText::new(format!("Edit rejected · {error:?}"))
                                .small()
                                .color(BAD),
                        );
                    }
                    if let Some(issue) = self.sketch_finish_issue {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("Finish rejected · {}", issue.label()))
                                    .small()
                                    .color(BAD),
                            )
                            .wrap(),
                        );
                    }
                    },
                );

                ui.add_space(12.0);
                ui.label(
                    RichText::new(
                        "Strokes commit as you draw; undo steps back. Finish from the rail below or the ribbon; only certified closed regions can extrude.",
                    )
                    .small()
                    .color(MUTED),
                );

                ui.add_space(7.0);
                collapsible_card(ui, "snapping_view", "SNAPPING AND VIEW", true, |ui| {
                    let mut settings = self.sketch.snap_settings();
                    let mut changed = ui.checkbox(&mut settings.enabled, "Enable snapping").changed();
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut settings.grid_step, 0.05..=2.0)
                                .logarithmic(true)
                                .text("Grid step")
                                .suffix(" mm"),
                        )
                        .changed();
                    if changed {
                        self.sketch.set_snap_settings(settings);
                    }
                    let frame_label = if self.face_sketch_context.is_some() {
                        "Frame body and face"
                    } else {
                        "Frame sketch origin"
                    };
                    if ui.button(frame_label).clicked() {
                        self.frame_active_sketch();
                    }
                });
            });
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .scroll_bar_visibility(
                egui::containers::scroll_area::ScrollBarVisibility::AlwaysVisible,
            )
            .show(ui, |ui| {
                ui.add_space(5.0);
                collapsible_card(ui, "material_and_mass", "MATERIAL", true, |ui| {
                    self.material_card(ui);
                });
                ui.add_space(5.0);
                let measured_geometry = self.measured_edge_geometry();
                let measured_face = self.measured_face_area();
                let active_component = self.active_component_instance().map(|component| {
                    let joint = self.document.joint_for_child(component.id).map(|joint| {
                        (joint.id, joint.name.clone(), joint.kind, joint.enabled)
                    });
                    (
                        component.id,
                        component.label.clone(),
                        component.definition.definition_key().to_owned(),
                        component.definition.revision(),
                        component.binding_digest.to_string(),
                        component.pose,
                        component.grounded,
                        joint,
                    )
                });
                if let Some((
                    component_id,
                    label,
                    definition_key,
                    definition_revision,
                    binding_digest,
                    pose,
                    grounded,
                    joint,
                )) = active_component
                {
                    collapsible_card(ui, "component_occurrence", "COMPONENT", true, |ui| {
                        status_line(
                            ui,
                            &format!("{label} · C{}", component_id.get()),
                            ACCENT,
                        );
                        ui.label(
                            RichText::new(format!(
                                "{definition_key} · revision {definition_revision}"
                            ))
                            .small()
                            .color(MUTED),
                        );
                        ui.label(
                            RichText::new(format!("Variant {}…", &binding_digest[..12]))
                                .small()
                                .monospace()
                                .color(MUTED),
                        );
                        let fields = assembly::ComponentPoseFields::from_pose(pose);
                        ui.separator();
                        ui.label(RichText::new("COMMITTED PLACEMENT").small().color(MUTED));
                        ui.label(
                            RichText::new(format!(
                                "X {:.2}  Y {:.2}  Z {:.2} mm",
                                fields.translation_mm.x,
                                fields.translation_mm.y,
                                fields.translation_mm.z
                            ))
                            .monospace()
                            .color(TEXT),
                        );
                        ui.label(
                            RichText::new(format!(
                                "RX {:.1}°  RY {:.1}°  RZ {:.1}°",
                                fields.rotation_degrees.x,
                                fields.rotation_degrees.y,
                                fields.rotation_degrees.z
                            ))
                            .monospace()
                            .color(TEXT),
                        );
                        let can_stage = self.pending_operation.is_none()
                            && self.history_is_at_end();
                        let has_joint = joint.is_some();
                        let ground_label = if grounded {
                            "Release component"
                        } else {
                            "Ground component"
                        };
                        let ground_response = ui.add_enabled(
                                can_stage && (!has_joint || grounded),
                                egui::Button::new(ground_label)
                                    .min_size(egui::vec2(ui.available_width(), 30.0)),
                            );
                        if ground_response.clicked() {
                            self.stage_component_grounding(!grounded);
                        }
                        if has_joint && !grounded {
                            ground_response.on_disabled_hover_text(
                                "This component already has a parent joint; grounding would conflict with it.",
                            );
                        }
                        ui.separator();
                        match joint {
                            Some((joint_id, name, kind, enabled)) => {
                                status_line(
                                    ui,
                                    &format!(
                                        "{} · {}",
                                        match kind {
                                            JointKind::Fixed => "FIXED JOINT",
                                            JointKind::Revolute { .. } => "REVOLUTE JOINT",
                                        },
                                        if enabled { "ENABLED" } else { "DISABLED" }
                                    ),
                                    GOOD,
                                );
                                ui.label(
                                    RichText::new(format!("{name} · {joint_id}"))
                                        .small()
                                        .color(TEXT),
                                );
                                if let JointKind::Revolute { axis, limits, .. } = kind {
                                    ui.label(
                                        RichText::new(format!(
                                            "Axis [{:.2}, {:.2}, {:.2}]{}",
                                            axis.x(),
                                            axis.y(),
                                            axis.z(),
                                            if limits.is_some() { " · limited" } else { "" }
                                        ))
                                        .small()
                                        .color(MUTED),
                                    );
                                }
                            }
                            None => {
                                let enabled = can_stage && !grounded;
                                let response = ui.add_enabled(
                                    enabled,
                                    egui::Button::new("Add revolute joint")
                                        .min_size(egui::vec2(ui.available_width(), 30.0)),
                                );
                                if response.clicked() {
                                    self.stage_revolute_joint();
                                }
                                if grounded {
                                    response.on_disabled_hover_text(
                                        "Release the grounded component before adding a movable joint.",
                                    );
                                }
                                ui.label(
                                    RichText::new(
                                        "Creates a named world-Z rotation at the component pivot.",
                                    )
                                    .small()
                                    .color(MUTED),
                                );
                            }
                        }
                    });
                    ui.add_space(5.0);
                }

                if self.active_tool == ActiveTool::Measure
                    || !measured_geometry.is_empty()
                    || measured_face.is_some()
                {
                    collapsible_card(ui, "edge_measurement", "MEASURE", true, |ui| {
                        status_line(
                            ui,
                            match (measured_face, measured_geometry.len()) {
                                (Some(_), _) => "FACE AREA RESULT",
                                (None, 0) => "SELECT FACE OR EDGE",
                                (None, 1) => "EDGE LENGTH RESULT",
                                _ => "EDGE-TO-EDGE RESULT",
                            },
                            if measured_geometry.is_empty() && measured_face.is_none() {
                                MUTED
                            } else {
                                ACCENT
                            },
                        );
                        if let Some((selection, area)) = measured_face {
                            ui.label(
                                RichText::new(format!(
                                    "Face #{} · area {:.3} mm²",
                                    selection.face.entity, area
                                ))
                                .color(GOOD)
                                .strong(),
                            );
                        }
                        for (index, (selection, _, length)) in
                            measured_geometry.iter().enumerate()
                        {
                            ui.label(
                                RichText::new(format!(
                                    "Edge {} · #{} · length {:.3} mm",
                                    index + 1,
                                    selection.edge.entity,
                                    length
                                ))
                                .color(TEXT),
                            );
                        }
                        if let [(_, first_segments, _), (_, second_segments, _)] =
                            measured_geometry.as_slice()
                        {
                            let distance = first_segments
                                .iter()
                                .flat_map(|first| {
                                    second_segments.iter().map(move |second| {
                                        model_segment_distance(*first, *second)
                                    })
                                })
                                .fold(f64::INFINITY, f64::min);
                            ui.separator();
                            ui.label(
                                RichText::new(format!("Minimum distance  {distance:.3} mm"))
                                    .color(GOOD)
                                    .strong(),
                            );
                            if let Some(angle) = self.measured_edge_angle_degrees() {
                                ui.label(
                                    RichText::new(format!("Included angle  {angle:.3}°"))
                                        .color(ACCENT)
                                        .strong(),
                                );
                            }
                        } else if measured_face.is_none() {
                            ui.label(
                                RichText::new(if measured_geometry.is_empty() {
                                    "Click a face for area or an edge for length."
                                } else {
                                    "Click a second edge for the model-space minimum distance."
                                })
                                .small()
                                .color(MUTED),
                            );
                        }
                        if (!self.measured_edges.is_empty() || self.measured_face.is_some())
                            && ui.button("Clear measurement").clicked()
                        {
                            self.measured_edges.clear();
                            self.measured_face = None;
                        }
                    });
                    ui.add_space(5.0);
                }

                if self.selected_face.is_some()
                    || self.selected_edge.is_some()
                    || self.selected_vertex.is_some()
                {
                    collapsible_card(ui, "selection_properties", "SELECTION", true, |ui| {
                        let selection_count = self.selected_faces.len()
                            + self.selected_edges.len()
                            + self.selected_vertices.len();
                        if selection_count > 1 {
                            status_line(ui, &format!("{selection_count} ITEMS SELECTED"), ACCENT);
                            ui.label(
                                RichText::new(format!(
                                    "{} faces · {} edges · {} vertices",
                                    self.selected_faces.len(),
                                    self.selected_edges.len(),
                                    self.selected_vertices.len()
                                ))
                                .small()
                                .color(TEXT),
                            );
                        }
                        if let Some(face) = self.selected_face {
                            status_line(ui, &format!("Face #{}", face.entity), ACCENT);
                            ui.label(
                                RichText::new(
                                    "Faces can host sketches, push/pull operations, and face-based features.",
                                )
                                .small()
                                .color(MUTED),
                            );
                        } else if let Some(edge) = self.selected_edge {
                            status_line(ui, &format!("Edge #{}", edge.edge.entity), ACCENT);
                            ui.label(
                                RichText::new("This edge can be used by Chamfer or Fillet.")
                                    .small()
                                    .color(MUTED),
                            );
                        } else if let Some(vertex) = self.selected_vertex {
                            status_line(ui, &format!("Vertex #{}", vertex.vertex.entity), ACCENT);
                            ui.label(
                                RichText::new("Authoritative B-rep point selected.")
                                    .small()
                                    .color(MUTED),
                            );
                        }
                    });
                    ui.add_space(5.0);
                }

                if (self.sketch.entities().is_empty() && self.selected_face.is_some())
                    || matches!(
                        self.pending_operation,
                        Some(PendingOperation::PushPullFace { .. })
                    )
                {
                    collapsible_card(ui, "face_feature", "FACE FEATURE", true, |ui| {
                        self.push_pull_controls(ui);
                    });
                    ui.add_space(5.0);
                }

                if !self.sketch.entities().is_empty() || self.sketch_finished {
                    collapsible_card(ui, "sketch_feature", "SKETCH FEATURE", true, |ui| {
                        self.extrusion_controls(ui);
                    });
                    ui.add_space(5.0);
                }

                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("NATIVE RUST ONLY")
                            .font(FontId::proportional(10.0))
                            .strong()
                            .color(ACCENT),
                    );
                    ui.separator();
                    ui.label(
                        RichText::new("60 FPS GOAL")
                            .font(FontId::proportional(10.0))
                            .color(GOOD),
                    );
                });
                self.compact_attempt_status(ui);
            });
    }

    /// A coordinate as displayed at three decimals, with the sign of a value
    /// that rounds to zero normalized away: the exact measure engines leave
    /// residuals like -1e-16 on symmetric solids, and "-0.000" is not a number
    /// anyone asked to see.
    fn display_coordinate(value: f64) -> f64 {
        let rounded = (value * 1000.0).round() / 1000.0;
        if rounded == 0.0 { 0.0 } else { rounded }
    }

    fn document_parameter_controls(&mut self, ui: &mut egui::Ui) {
        let records = self.document.parameters().records().to_vec();
        collapsible_card(ui, "document_parameters", "PARAMETERS", false, |ui| {
            if records.is_empty() {
                ui.label(
                    RichText::new("No document parameters yet")
                        .small()
                        .color(MUTED),
                );
            }
            for record in records {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&record.spec.label).color(TEXT));
                        ui.label(
                            RichText::new(&record.spec.key)
                                .small()
                                .monospace()
                                .color(MUTED),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let staged = match self.pending_operation {
                            Some(PendingOperation::SetParameterLiteral {
                                parameter,
                                value,
                                ..
                            }) if parameter == record.id => Some(value),
                            _ => None,
                        };
                        match &record.binding {
                            ParameterBinding::Literal { value } => {
                                let Some(base) = ParameterLiteralDraft::from_value(value) else {
                                    ui.label(RichText::new("choice").small().color(MUTED));
                                    return;
                                };
                                let mut edited = staged.unwrap_or(base);
                                let enabled = self.pending_operation.is_none() || staged.is_some();
                                let changed = match &mut edited {
                                    ParameterLiteralDraft::Quantity { magnitude, unit } => {
                                        let suffix = match unit {
                                            ParameterUnit::Micrometer => " µm",
                                            ParameterUnit::Millimeter => " mm",
                                            ParameterUnit::Centimeter => " cm",
                                            ParameterUnit::Meter => " m",
                                            ParameterUnit::Inch => " in",
                                            ParameterUnit::Foot => " ft",
                                            ParameterUnit::Radian => " rad",
                                            ParameterUnit::Degree => "°",
                                            ParameterUnit::Scalar => "",
                                        };
                                        ui.add_enabled(
                                            enabled,
                                            egui::DragValue::new(magnitude)
                                                .speed(0.1)
                                                .max_decimals(4)
                                                .suffix(suffix),
                                        )
                                        .changed()
                                    }
                                    ParameterLiteralDraft::Integer(value) => ui
                                        .add_enabled(enabled, egui::DragValue::new(value))
                                        .changed(),
                                    ParameterLiteralDraft::Boolean(value) => ui
                                        .add_enabled(enabled, egui::Checkbox::without_text(value))
                                        .changed(),
                                };
                                if changed {
                                    self.pending_operation =
                                        Some(PendingOperation::SetParameterLiteral {
                                            parameter: record.id,
                                            base,
                                            value: edited,
                                        });
                                }
                            }
                            ParameterBinding::Expression { .. } => {
                                ui.label(RichText::new("expression").small().color(ACCENT));
                            }
                            ParameterBinding::Unresolved => {
                                ui.label(RichText::new("required").small().color(WARN));
                            }
                        }
                    });
                });
                ui.separator();
            }
            let can_add = self.pending_operation.is_none() && self.history_is_at_end();
            let add = ui.add_enabled(
                can_add,
                egui::Button::new("+ Length parameter")
                    .min_size(egui::vec2(ui.available_width(), 28.0)),
            );
            if add.clicked() {
                let mut ordinal = self.document.parameters().len() as u32 + 1;
                while self
                    .document
                    .parameters()
                    .get_by_key(&format!("UserLength{ordinal}"))
                    .is_some()
                {
                    ordinal = ordinal.saturating_add(1);
                }
                self.pending_operation = Some(PendingOperation::AddUserLengthParameter {
                    ordinal,
                    value_mm: 10.0,
                });
            }
            ui.label(
                RichText::new("Changes remain staged until Enter or the green tick")
                    .small()
                    .color(GOOD),
            );
        });
    }

    fn rebuild_after_parameter_change(&mut self) {
        self.history_scrub_position = self.document.history_position();
        let dirty = self
            .document
            .features()
            .iter()
            .find(|feature| feature.state.rebuild == RebuildState::Dirty)
            .map(|feature| feature.id);
        if let Some(feature) = dirty {
            if self.rebuild_document_from(feature) {
                self.document_status =
                    Some("Parameter committed · dependent features rebuilt".to_owned());
            }
        } else {
            self.document_status = Some("Parameter committed".to_owned());
        }
    }

    fn extrusion_controls(&mut self, ui: &mut egui::Ui) {
        let profile = self.sketch.certified_profile_status();
        let eligibility = self.sketch_extrusion_eligibility();
        let extrusion_pending = matches!(
            self.pending_operation,
            Some(PendingOperation::ExtrudeSketch { .. })
        );
        let already_extruded = self.extruded_sketch_revision == Some(self.sketch_revision);
        status_line(
            ui,
            match (extrusion_pending, already_extruded, eligibility) {
                (true, _, _) => "EXTRUSION PREVIEW",
                (false, true, _) => "EXTRUSION COMMITTED",
                (false, false, SketchExtrusionEligibility::Ready) => "EXTRUSION READY",
                (false, false, SketchExtrusionEligibility::SketchNotFinished) => {
                    "NO CLOSED PROFILE"
                }
                (false, false, SketchExtrusionEligibility::RegionSelectionRequired { .. }) => {
                    "SELECT PROFILE"
                }
                (false, false, _) => "EXTRUSION UNAVAILABLE",
            },
            match (extrusion_pending, already_extruded, eligibility) {
                (true, _, _) => WARN,
                (false, true, _) | (false, false, SketchExtrusionEligibility::Ready) => GOOD,
                (false, false, SketchExtrusionEligibility::SketchNotFinished) => MUTED,
                (false, false, SketchExtrusionEligibility::RegionSelectionRequired { .. }) => WARN,
                (false, false, _) => WARN,
            },
        );
        ui.label(
            RichText::new(format!(
                "{} · revision {}",
                self.sketch_support.label(),
                self.sketch_revision
            ))
            .small()
            .color(MUTED),
        );

        let editable = self.pending_operation.is_none() || extrusion_pending;
        let face_supported = matches!(&self.sketch_support, SketchSupport::PlanarFace { .. });
        let mut intent_changed = false;
        ui.horizontal(|ui| {
            if face_supported {
                let response = ui.add_enabled(
                    editable,
                    egui::Button::new("Auto")
                        .selected(!self.extrusion_mode_explicit)
                        .corner_radius(4),
                );
                if response.clicked() {
                    self.select_automatic_extrusion_mode();
                    intent_changed = true;
                }
            }
            for mode in [
                ExtrusionMode::NewBody,
                ExtrusionMode::Add,
                ExtrusionMode::Cut,
            ] {
                let supported = match mode {
                    ExtrusionMode::NewBody => !face_supported,
                    ExtrusionMode::Add | ExtrusionMode::Cut => face_supported,
                };
                let response = ui.add_enabled(
                    editable && supported,
                    egui::Button::new(mode.label())
                        .selected(self.extrusion_mode == mode)
                        .corner_radius(4),
                );
                if response.clicked() {
                    self.select_extrusion_mode(mode);
                    intent_changed = true;
                }
            }
        });
        ui.add_enabled_ui(editable, |ui| {
            intent_changed |= ui
                .add(
                    egui::DragValue::new(&mut self.extrusion_distance)
                        .speed(0.1)
                        .range(if face_supported {
                            -1_000.0..=1_000.0
                        } else {
                            0.01..=1_000.0
                        })
                        .max_decimals(3)
                        .prefix("Distance ")
                        .suffix(" mm"),
                )
                .changed();
        });
        if intent_changed {
            self.set_extrusion_distance_intent(self.extrusion_distance);
        }
        if face_supported {
            ui.label(
                RichText::new(if self.extrusion_mode_explicit {
                    "Operation locked · signed distance controls direction only · Auto restores sign-based Add/Cut"
                } else {
                    "Auto · positive adds, negative cuts · choose Add or Cut to override without reversing direction"
                })
                    .small()
                    .color(MUTED),
            );
        }
        if extrusion_pending {
            self.sync_pending_sketch_extrusion_inputs();
            if intent_changed {
                self.sketch_extrusion_issue = None;
            }
        }

        let distance_valid = self.extrusion_distance_is_valid();
        if self.extruded_sketch_revision == Some(self.sketch_revision)
            && let Some(measures) = self.displayed_measures()
        {
            ui.separator();
            ui.label(
                RichText::new(format!(
                    "Volume {:.3} mm³ · area {:.3} mm²",
                    measures.volume, measures.surface_area
                ))
                .small()
                .color(ACCENT),
            );
            if let Some(centroid) = measures.centroid {
                ui.label(
                    RichText::new(format!(
                        "Centroid [{:.3}, {:.3}, {:.3}] mm",
                        Self::display_coordinate(centroid.x),
                        Self::display_coordinate(centroid.y),
                        Self::display_coordinate(centroid.z)
                    ))
                    .small()
                    .color(MUTED),
                );
            }
        }

        if extrusion_pending {
            if let Some(error) = &self.sketch_extrusion_issue {
                status_line(ui, "EXTRUSION REJECTED · INTENT RETAINED", BAD);
                ui.label(
                    RichText::new(format!("Rejected · {}", error.code))
                        .small()
                        .color(BAD),
                );
                ui.add(egui::Label::new(RichText::new(&error.message).small().color(MUTED)).wrap());
                ui.label(
                    RichText::new(
                        "Previous body retained · adjust the inputs or cancel this preview",
                    )
                    .small()
                    .color(GOOD),
                );
            } else {
                ui.label(
                    RichText::new(match self.extrusion_mode {
                        ExtrusionMode::NewBody => {
                            "Extrusion staged · confirm with Enter or the green tick".to_owned()
                        }
                        ExtrusionMode::Add => {
                            "Add preview staged · confirm with Enter or the green tick".to_owned()
                        }
                        ExtrusionMode::Cut => {
                            "Cut preview staged · confirm with Enter or the green tick".to_owned()
                        }
                    })
                    .small()
                    .color(WARN),
                );
            }
        } else if already_extruded {
            // The committed-state summary above is complete. Do not surface
            // the deliberately stale support of a consumed historical sketch
            // as a warning; the Create command already guides the user to a
            // current face for the next feature.
        } else if eligibility == SketchExtrusionEligibility::Ready && distance_valid {
            ui.label(
                RichText::new("Click Extrude in the ribbon to start a live preview.")
                    .small()
                    .color(GOOD),
            );
        } else if !distance_valid {
            ui.add(
                egui::Label::new(
                    RichText::new("Enter a finite, non-zero extrusion distance.")
                        .small()
                        .color(WARN),
                )
                .wrap(),
            );
        } else if eligibility == SketchExtrusionEligibility::SketchNotFinished {
            ui.label(
                RichText::new(format!(
                    "{} · create and confirm one closed profile",
                    profile.label()
                ))
                .small()
                .color(MUTED),
            );
        } else if let Some(reason) = eligibility.visible_reason() {
            ui.add(egui::Label::new(RichText::new(reason).small().color(WARN)).wrap());
        }
    }

    fn push_pull_controls(&mut self, ui: &mut egui::Ui) {
        let pending = matches!(
            self.pending_operation,
            Some(PendingOperation::PushPullFace { .. })
        );
        let support = self.selected_face_push_pull_support();
        if support.is_some() && self.extrusion_mode == ExtrusionMode::NewBody {
            self.extrusion_mode = if self.extrusion_distance < 0.0 {
                ExtrusionMode::Cut
            } else {
                ExtrusionMode::Add
            };
            self.extrusion_mode_explicit = false;
        }
        status_line(
            ui,
            if pending {
                "PUSH/PULL PREVIEW"
            } else if support.is_some() {
                "PUSH/PULL READY"
            } else {
                "PUSH/PULL UNAVAILABLE"
            },
            if pending {
                WARN
            } else if support.is_some() {
                GOOD
            } else {
                MUTED
            },
        );
        if let Some(face) = self.selected_face {
            ui.label(
                RichText::new(format!("Face #{} · exact boundary", face.entity))
                    .small()
                    .color(MUTED),
            );
        }

        let editable = self.pending_operation.is_none() || pending;
        let mut intent_changed = false;
        ui.horizontal(|ui| {
            for mode in [ExtrusionMode::Add, ExtrusionMode::Cut] {
                let response = ui.add_enabled(
                    editable && support.is_some(),
                    egui::Button::new(mode.label())
                        .selected(self.extrusion_mode == mode)
                        .corner_radius(4),
                );
                if response.clicked() {
                    self.extrusion_mode_explicit = false;
                    let magnitude = self.extrusion_distance.abs().max(1.0);
                    self.set_extrusion_distance_intent(match mode {
                        ExtrusionMode::Add => magnitude,
                        ExtrusionMode::Cut => -magnitude,
                        ExtrusionMode::NewBody => unreachable!("push/pull has no new-body mode"),
                    });
                    intent_changed = true;
                }
            }
        });
        ui.add_enabled_ui(editable && support.is_some(), |ui| {
            intent_changed |= ui
                .add(
                    egui::DragValue::new(&mut self.extrusion_distance)
                        .speed(0.1)
                        .range(-1_000.0..=1_000.0)
                        .max_decimals(3)
                        .prefix("Distance ")
                        .suffix(" mm"),
                )
                .changed();
        });
        if intent_changed {
            self.set_extrusion_distance_intent(self.extrusion_distance);
            if pending {
                self.sync_pending_sketch_extrusion_inputs();
                self.sketch_extrusion_issue = None;
            }
        }
        ui.label(
            RichText::new("Pull the viewport arrow · positive adds, negative cuts")
                .small()
                .color(MUTED),
        );

        if pending {
            if let Some(error) = &self.sketch_extrusion_issue {
                status_line(ui, "PUSH/PULL REJECTED · INTENT RETAINED", BAD);
                ui.label(
                    RichText::new(format!("Rejected · {}", error.code))
                        .small()
                        .color(BAD),
                );
                ui.add(egui::Label::new(RichText::new(&error.message).small().color(MUTED)).wrap());
                ui.label(
                    RichText::new("Previous body retained · adjust the distance or cancel")
                        .small()
                        .color(GOOD),
                );
            } else {
                ui.label(
                    RichText::new(if self.extrusion_distance < 0.0 {
                        "Cut preview staged · confirm with Enter or the green tick"
                    } else {
                        "Add preview staged · confirm with Enter or the green tick"
                    })
                    .small()
                    .color(WARN),
                );
            }
        } else if support.is_some() {
            ui.label(
                RichText::new("Click Extrude in the ribbon to move the complete selected face.")
                    .small()
                    .color(GOOD),
            );
        } else {
            ui.add(
                egui::Label::new(
                    RichText::new(
                        "Direct push/pull currently requires one unholed planar extrusion cap.",
                    )
                    .small()
                    .color(WARN),
                )
                .wrap(),
            );
        }
    }

    fn motion_controls(&mut self, ui: &mut egui::Ui) {
        let motion_name = self.active_motion_name();
        ui.label(
            RichText::new(format!("Playback · {motion_name}"))
                .small()
                .color(ACCENT),
        );
        let button_label = if self.motion.playing {
            "Stop animation"
        } else {
            "Play animation"
        };
        if ui
            .add_sized(
                [ui.available_width(), 32.0],
                egui::Button::new(button_label)
                    .selected(self.motion.playing)
                    .corner_radius(7),
            )
            .clicked()
        {
            self.toggle_animation(ui.ctx());
        }

        let mut speed = self.motion.speed_rpm;
        if ui
            .add(
                egui::Slider::new(&mut speed, -30.0..=30.0)
                    .text(&motion_name)
                    .suffix(" rpm"),
            )
            .changed()
        {
            self.motion.set_speed_rpm(speed);
        }

        let phase_fraction = (self.motion.phase / std::f64::consts::TAU) as f32;
        ui.add(
            egui::ProgressBar::new(phase_fraction)
                .text(format!("Phase {:>5.1}°", self.motion.phase.to_degrees())),
        );
        ui.horizontal(|ui| {
            let (cadence, cadence_color) = self.cadence_status();
            ui.label(
                RichText::new(cadence)
                    .color(cadence_color)
                    .strong(),
            )
            .on_hover_text(
                "Measured UI repaint-start cadence; GPU presentation telemetry is not yet available.",
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Reset phase").clicked() {
                    self.motion.phase = 0.0;
                    ui.ctx().request_repaint();
                }
            });
        });
    }

    fn transform_controls(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let editable = self.transform_tools_available();
        let component_target = self.active_component_instance().map(|component| {
            (
                component.label.clone(),
                component.grounded,
                self.document
                    .joint_for_child(component.id)
                    .map(|joint| joint.name.clone()),
            )
        });
        let scale_editable = self.scale_tool_available();
        ui.label(
            RichText::new(
                component_target
                    .as_ref()
                    .map_or("WHOLE BODY / GROUP", |_| "RIGID COMPONENT OCCURRENCE"),
            )
            .small()
            .color(if component_target.is_some() {
                ACCENT
            } else {
                MUTED
            }),
        );
        ui.label(RichText::new("Offset · mm").small().color(MUTED));
        ui.horizontal(|ui| {
            let field_width =
                ((ui.available_width() - 2.0 * ui.spacing().item_spacing.x - 12.0) / 3.0).max(44.0);
            for (axis, value) in ["X", "Y", "Z"]
                .into_iter()
                .zip(&mut self.display_transform.translation)
            {
                changed |= ui
                    .add_enabled_ui(editable, |ui| {
                        ui.add_sized(
                            [field_width, ui.spacing().interact_size.y],
                            egui::DragValue::new(value)
                                .speed(0.05)
                                .range(-20.0..=20.0)
                                .max_decimals(2)
                                .prefix(format!("{axis} ")),
                        )
                    })
                    .inner
                    .changed();
            }
        });

        ui.label(RichText::new("Rotation · degrees").small().color(MUTED));
        ui.horizontal(|ui| {
            let field_width =
                ((ui.available_width() - 2.0 * ui.spacing().item_spacing.x - 12.0) / 3.0).max(44.0);
            for (index, axis) in ["X", "Y", "Z"].into_iter().enumerate() {
                let mut degrees = self.display_transform.rotation[index].to_degrees();
                if ui
                    .add_enabled_ui(editable, |ui| {
                        ui.add_sized(
                            [field_width, ui.spacing().interact_size.y],
                            egui::DragValue::new(&mut degrees)
                                .speed(0.5)
                                .range(-180.0..=180.0)
                                .max_decimals(2)
                                .prefix(format!("{axis} "))
                                .suffix("°"),
                        )
                    })
                    .inner
                    .changed()
                {
                    self.display_transform.rotation[index] = degrees.to_radians();
                    changed = true;
                }
            }
        });

        let mut scale = self.display_transform.scale;
        if ui
            .add_enabled_ui(scale_editable, |ui| {
                ui.add_sized(
                    [
                        (ui.available_width() - 8.0).max(80.0),
                        ui.spacing().interact_size.y,
                    ],
                    egui::Slider::new(&mut scale, 0.35..=1.75)
                        .max_decimals(3)
                        .custom_formatter(|value, _| format!("{value:04.2}"))
                        .text("Display scale"),
                )
            })
            .inner
            .changed()
        {
            self.display_transform.set_scale(scale);
            changed = true;
        }
        if component_target.is_some() {
            ui.label(
                RichText::new("Scale is defined by the component's authored parameters.")
                    .small()
                    .color(MUTED),
            );
        }
        let mut zoom = self.view.zoom;
        if ui
            .add_sized(
                [
                    (ui.available_width() - 8.0).max(80.0),
                    ui.spacing().interact_size.y,
                ],
                egui::Slider::new(&mut zoom, 0.45..=2.0)
                    .max_decimals(3)
                    .custom_formatter(|value, _| format!("{value:04.2}"))
                    .text("Camera zoom"),
            )
            .changed()
        {
            self.view.set_zoom(zoom);
        }

        ui.horizontal(|ui| {
            if ui.button("Reset view").clicked() {
                self.reset_view(ui.ctx());
            }
            if ui.button("Frame all visible").clicked() {
                self.frame_visible_body(ui.ctx());
                ui.ctx().request_repaint();
            }
        });
        if changed {
            self.sync_transform_preview();
        }
        if matches!(
            self.pending_operation,
            Some(PendingOperation::RunCase { .. })
        ) {
            status_line(ui, "ANOTHER OPERATION IS PENDING", WARN);
            ui.add(
                egui::Label::new(
                    RichText::new("Confirm or cancel it before editing a transform")
                        .small()
                        .color(MUTED),
                )
                .wrap(),
            );
        } else if self.transform_preview_pending() {
            status_line(ui, "PREVIEW — NOT COMMITTED", WARN);
            ui.add(
                egui::Label::new(
                    RichText::new(if component_target.is_some() {
                        "Target: component placement · B-rep and snapshot unchanged"
                    } else {
                        "Target: whole body/group · snapshot unchanged"
                    })
                    .small()
                    .color(MUTED),
                )
                .wrap(),
            );
        } else {
            let grounded = component_target
                .as_ref()
                .is_some_and(|(_, grounded, _)| *grounded);
            let joint_name = component_target
                .as_ref()
                .and_then(|(_, _, joint_name)| joint_name.as_deref());
            status_line(
                ui,
                if joint_name.is_some() {
                    "Component constrained by joint"
                } else if grounded {
                    "Component grounded"
                } else {
                    "Model and view are in sync"
                },
                if grounded || joint_name.is_some() {
                    WARN
                } else {
                    GOOD
                },
            );
            ui.label(
                RichText::new(component_target.map_or_else(
                    || "Target: whole body/group".to_owned(),
                    |(label, _, joint_name)| match joint_name {
                        Some(joint_name) => format!("Target: {label} · {joint_name}"),
                        None => format!("Target: {label} rigid occurrence"),
                    },
                ))
                .small()
                .color(MUTED),
            );
        }
    }

    fn cadence_status(&self) -> (String, Color32) {
        if !self.motion.playing {
            return ("Animation stopped".to_owned(), MUTED);
        }
        let Some(fps) = self.motion.smoothed_fps else {
            return ("Measuring UI cadence…".to_owned(), MUTED);
        };
        let color = if fps >= f64::from(self.motion.target_hz) * 0.995 {
            GOOD
        } else {
            BAD
        };
        (format!("{fps:.0} FPS UI"), color)
    }

    fn attempt_card(&self, ui: &mut egui::Ui) {
        match &self.last_attempt {
            Attempt::NotRun => {
                status_line(ui, "Not run", MUTED);
            }
            Attempt::Accepted { operation } => {
                status_line(ui, operation, GOOD);
                ui.label(
                    RichText::new("Accepted · validated before publication")
                        .small()
                        .color(MUTED),
                );
            }
            Attempt::Rejected { operation, error } => {
                status_line(ui, operation, BAD);
                ui.label(
                    RichText::new(format!("Rejected · {}", error.code))
                        .small()
                        .color(BAD),
                );
                ui.label(RichText::new(&error.message).small().color(MUTED));
                ui.add_space(4.0);
                ui.label(
                    RichText::new("Last valid snapshot retained")
                        .small()
                        .color(GOOD),
                );
            }
        }
    }

    fn compact_attempt_status(&self, ui: &mut egui::Ui) {
        match &self.last_attempt {
            Attempt::NotRun => {}
            Attempt::Accepted { operation } => status_line(ui, operation, GOOD),
            Attempt::Rejected { operation, error } => {
                status_line(ui, operation, BAD);
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(format!("Rejected · {}", error.code))
                            .small()
                            .color(BAD),
                    );
                    ui.label(
                        RichText::new("Last valid snapshot retained")
                            .small()
                            .color(GOOD),
                    );
                });
            }
        }
    }

    /// The live overlay of the sketch being drawn, projected onto its plane,
    /// so the orbit peek shows the work in progress and not only committed
    /// geometry.
    fn active_sketch_peek_overlay(&self) -> Option<viewport::ModelSketchOverlay> {
        let frame = self.sketch_support.frame();
        let mut points = Vec::new();
        let mut segments = Vec::new();
        for entity in self.sketch.entities() {
            let Some(polyline) = entity.geometry.display_polyline() else {
                continue;
            };
            if polyline.segments().next().is_none() {
                points.extend(
                    polyline
                        .points
                        .iter()
                        .copied()
                        .map(|point| frame_point(frame, ProtocolPoint2::new(point.u, point.v))),
                );
            }
            segments.extend(polyline.segments().map(|segment| {
                segment.map(|point| frame_point(frame, ProtocolPoint2::new(point.u, point.v)))
            }));
        }
        (!points.is_empty() || !segments.is_empty())
            .then(|| viewport::ModelSketchOverlay::new(points, segments, false))
    }

    /// The 3D viewport shown while the preset's orbit button holds a
    /// sketch-mode peek. Interactions other than the camera are ignored: the peek
    /// is for looking, and a stray click must not select bodies or activate
    /// another sketch while one is being drawn.
    fn sketch_orbit_peek_viewport(&mut self, ui: &mut egui::Ui) {
        let orbit_button = match self.document_settings.navigation.bindings().orbit {
            navigation::Gesture::Right => egui::PointerButton::Secondary,
            _ => egui::PointerButton::Middle,
        };
        let released = !ui.input(|input| input.pointer.button_down(orbit_button));
        if released && !self.sketch_orbit_returning {
            self.sketch_orbit_returning = true;
            let mut landed = true;
            if let Some(view) = self.sketch_orbit_return_view.take() {
                if self.animate_face_camera_transitions
                    && let Some(transition) = CameraTransition::to_view(self.view, view)
                {
                    self.face_camera_transition = Some(transition);
                    self.last_face_camera_time = None;
                    landed = false;
                } else {
                    self.view = view;
                }
            }
            if landed {
                // Instant mode: the camera has already landed.
                self.sketch_orbit_peek = false;
                self.sketch_orbit_returning = false;
            }
        }
        if self.sketch_orbit_returning && self.face_camera_transition.is_none() {
            self.sketch_orbit_peek = false;
            self.sketch_orbit_returning = false;
            self.sketch_viewport(ui);
            return;
        }
        self.model_viewport(ui);
    }

    fn sketch_viewport(&mut self, ui: &mut egui::Ui) {
        let available = ui.available_size();
        let canvas_size = egui::vec2((available.x - 14.0).max(1.0), (available.y - 14.0).max(1.0));
        let output = {
            let viewport_context = self
                .face_sketch_context
                .as_ref()
                .map(FaceSketchDisplayContext::viewport_context);
            Frame::new()
                .fill(CARD)
                .stroke(Stroke::new(1.0, BORDER))
                .corner_radius(4)
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    ui.set_min_size(canvas_size);
                    ui.set_max_size(canvas_size);
                    sketch::show_with_context(ui, &mut self.sketch, viewport_context.as_ref())
                })
                .inner
        };
        // Trying to orbit while sketching peeks at the model in 3D: the
        // canvas swaps for the model viewport while the button is held, and
        // the camera flies back onto the sketch when it is released. Keyed
        // to the active preset's orbit gesture so it reads as "orbit" under
        // any binding; the 2D canvas pans with both middle and right drags,
        // so whichever button the preset claims, the other still pans.
        let orbit = self.document_settings.navigation.bindings().orbit;
        if output.response.hovered()
            && ui.input(|input| {
                orbit.matches(navigation::GestureState {
                    right: input.pointer.button_pressed(egui::PointerButton::Secondary),
                    middle: input.pointer.button_pressed(egui::PointerButton::Middle),
                    shift: input.modifiers.shift,
                    ctrl: input.modifiers.command || input.modifiers.ctrl,
                })
            })
        {
            self.sketch_orbit_peek = true;
            self.sketch_orbit_returning = false;
            self.sketch_orbit_return_view = Some(self.view);
            self.motion.pause();
            self.last_motion_time = None;
        }
        self.sketch_canvas_overlay(ui, output.response.rect);
        self.sketch_dimension_keys.enter |= output.dimension_keys.enter;
        self.sketch_dimension_keys.escape |= output.dimension_keys.escape;
        self.sketch_dimension_keys.confirmation_blocked |=
            output.dimension_keys.confirmation_blocked;
        if output.selection_changed
            && self.active_sketch_tool == ToolVariant::Dimension
            && let Some(parameter) = self.sketch.selected_recipe_editor().and_then(|editor| {
                editor
                    .parameters
                    .into_iter()
                    .find(|parameter| parameter.editable)
            })
        {
            // Dimension selection is a select-and-type workflow: the next
            // keystroke replaces the authoritative driving value instead of
            // requiring a second trip to the Properties panel.
            ui.ctx().memory_mut(|memory| {
                memory.request_focus(egui::Id::new((
                    "selected_sketch_recipe_parameter",
                    parameter.stable_key,
                )));
            });
        }
        if let Some(entity) = output.pending_created {
            self.commit_sketch_stroke(entity);
        }
    }

    fn sketch_canvas_overlay(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let overlay = rect.shrink(7.0);
        let title_rect = egui::Rect::from_center_size(
            egui::pos2(overlay.center().x, overlay.top() + 13.0),
            egui::vec2(176.0, 24.0),
        );
        let breadcrumb = canvas_overlay_label(
            ui,
            "sketch_canvas_breadcrumb",
            title_rect,
            &format!("Sketch · {}", self.sketch_support.label()),
            ACCENT,
        );
        let accessible_plane = if self.sketch_is_face_supported() {
            format!(
                "{} · face-aligned orthographic sketch",
                self.sketch_support.label()
            )
        } else {
            format!(
                "{} · orthographic sketch",
                origin_plane_label(self.sketch.plane())
            )
        };
        breadcrumb.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &accessible_plane)
        });

        let plane_status_rect = egui::Rect::from_min_size(
            egui::pos2(overlay.left(), overlay.bottom() - 24.0),
            egui::vec2(150.0, 24.0),
        );
        canvas_overlay_label(
            ui,
            "sketch_plane_status",
            plane_status_rect,
            if self.sketch_is_face_supported() {
                "FACE · ORTHOGRAPHIC"
            } else {
                match self.sketch.plane() {
                    SketchPlane::XY => "XY · ORTHOGRAPHIC",
                    SketchPlane::YZ => "YZ · ORTHOGRAPHIC",
                    SketchPlane::XZ => "XZ · ORTHOGRAPHIC",
                }
            },
            MUTED,
        );

        if let Some(entity) = self.sketch.selected() {
            let selection_rect = egui::Rect::from_min_size(
                egui::pos2(overlay.right() - 132.0, overlay.bottom() - 24.0),
                egui::vec2(132.0, 24.0),
            );
            canvas_overlay_label(
                ui,
                "sketch_selection_status",
                selection_rect,
                &format!("Sketch entity #{}", entity.get()),
                ACCENT,
            );
        }

        let cube_rect = egui::Rect::from_min_size(
            egui::pos2(overlay.right() - 84.0, overlay.top()),
            egui::vec2(84.0, 78.0),
        );
        if let Some(ViewCubeCommand::Face(_)) = model_view_cube(ui, cube_rect, self.view, false) {
            self.frame_active_sketch();
        }

        let hud_rect = egui::Rect::from_center_size(
            egui::pos2(overlay.center().x, overlay.bottom() - 17.0),
            egui::vec2(112.0, 34.0),
        );
        let mut hud_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("sketch_navigation_hud")
                .max_rect(hud_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        hud_ui.set_clip_rect(rect);
        Frame::new()
            .fill(PANEL.gamma_multiply(0.94))
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(3)
            .inner_margin(Margin::symmetric(3, 2))
            .show(&mut hud_ui, |ui| {
                let frame = hud_button(ui, "F", "Frame sketch view", false);
                if frame.clicked() {
                    self.frame_active_sketch();
                }
                let mut settings = self.sketch.snap_settings();
                let snap = hud_button(ui, "Snap", "Toggle sketch snapping", settings.enabled);
                if snap.clicked() {
                    settings.enabled = !settings.enabled;
                    self.sketch.set_snap_settings(settings);
                }
            });
    }

    fn confirmation_slot(&self, ui: &mut egui::Ui) -> Option<ConfirmationAction> {
        let pending = self.pending_operation;
        let mut confirm_clicked = false;
        let mut cancel_clicked = false;
        // The rail's children render inside a detached, clipped UI after the
        // parent reserves an exact rectangle. This makes even unusually long
        // operation names incapable of widening the root UI and moving either
        // workbench viewport.
        let rail_size = ui.available_size();
        let (rail_rect, _) = ui.allocate_exact_size(rail_size, egui::Sense::click_and_drag());
        let rail_fill = if pending.is_some() {
            theme::GOOD_FILL
        } else {
            PANEL
        };
        ui.painter()
            .rect_filled(rail_rect, CornerRadius::ZERO, rail_fill);

        let inner_rect = rail_rect.shrink2(egui::vec2(4.0, 1.0));
        let mut rail_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("confirmation_rail_contents")
                .max_rect(inner_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        rail_ui.set_clip_rect(rail_rect);
        rail_ui.set_min_size(inner_rect.size());
        rail_ui.set_max_size(inner_rect.size());

        if let Some(pending) = pending {
            rail_ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let direct_manipulation_active = self.feature_preview_drag.is_active();
                let execution_active = self.async_sketch_extrusion_commit.is_some();
                let cancel_response = ui
                    .add_enabled(
                        !direct_manipulation_active,
                        egui::Button::new("")
                            .fill(BAD)
                            .stroke(Stroke::NONE)
                            .corner_radius(2)
                            .min_size(egui::vec2(30.0, 30.0)),
                    )
                    .on_hover_text(if direct_manipulation_active {
                        "Release the live feature handle before cancelling."
                    } else {
                        "Cancel the pending operation (Escape)"
                    });
                cancel_response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        !direct_manipulation_active,
                        "Cancel operation",
                    )
                });
                paint_confirmation_cross(
                    ui,
                    cancel_response.rect,
                    if direct_manipulation_active {
                        MUTED
                    } else {
                        Color32::WHITE
                    },
                );
                cancel_clicked = confirmation_button_activated(ui, &cancel_response);

                let dimension_confirmation_blocked = self.workbench_mode == WorkbenchMode::Sketch
                    && (self.sketch.dimension_error().is_some()
                        || self.sketch.selected_recipe_parameter_issue().is_some());
                let extrusion_confirmation_blocked = matches!(
                    self.pending_operation,
                    Some(PendingOperation::ExtrudeSketch {
                        distance,
                        target_face,
                        ..
                    }) if !distance.is_finite()
                        || target_face.is_some() && distance.abs() <= f64::EPSILON
                        || target_face.is_none() && distance <= 0.0
                ) || matches!(
                    self.pending_operation,
                    Some(PendingOperation::PushPullFace { distance, .. })
                        if !distance.is_finite() || distance.abs() <= f64::EPSILON
                );
                let edge_finish_confirmation_blocked = matches!(
                    self.pending_operation,
                    Some(PendingOperation::PresetFeature {
                        preset: SolidFeaturePreset::Chamfer | SolidFeaturePreset::Fillet,
                        ..
                    }) if !self.edge_finish_distance.is_finite()
                        || self.edge_finish_distance <= 0.0
                        || self.selected_edges.is_empty()
                        || !self.edge_finish_selection_support().can_commit()
                );
                let boolean_confirmation_blocked = self.boolean_confirmation_blocked();
                let confirmation_blocked =
                    dimension_confirmation_blocked
                        || extrusion_confirmation_blocked
                        || edge_finish_confirmation_blocked
                        || boolean_confirmation_blocked
                        || direct_manipulation_active
                        || execution_active;
                let confirm_response = ui.add_enabled(
                    !confirmation_blocked,
                    egui::Button::new("")
                        .fill(GOOD)
                        .stroke(Stroke::NONE)
                        .corner_radius(2)
                        .min_size(egui::vec2(30.0, 30.0)),
                );
                confirm_response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        !confirmation_blocked,
                        "Confirm operation",
                    )
                });
                paint_confirmation_tick(
                    ui,
                    confirm_response.rect,
                    if confirmation_blocked {
                        MUTED
                    } else {
                        Color32::WHITE
                    },
                );
                let confirm_response = if confirmation_blocked {
                    confirm_response.on_disabled_hover_text(if execution_active {
                        "The kernel is computing this extrusion. Cancel it or wait for completion."
                    } else if direct_manipulation_active {
                        "Release the live feature handle before confirming."
                    } else if extrusion_confirmation_blocked {
                        "Drag away from zero or enter a non-zero feature distance before confirming."
                    } else if edge_finish_confirmation_blocked {
                        self.edge_finish_selection_support().detail()
                    } else if boolean_confirmation_blocked {
                        "Click at least one tool body in the viewport before confirming."
                    } else if self.sketch.selected_recipe_parameter_issue().is_some() {
                        "Correct or cancel the invalid selected-feature parameter before confirming."
                    } else {
                        "Correct or cancel the invalid active dimension before confirming."
                    })
                } else {
                    confirm_response
                        .on_hover_text("Validate and commit the pending operation (Enter)")
                };
                confirm_clicked = confirmation_button_activated(ui, &confirm_response);

                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    if execution_active {
                        ui.spinner();
                    }
                    ui.add(
                        egui::Label::new(RichText::new(pending.title()).color(TEXT).strong())
                            .truncate(),
                    )
                    .on_hover_text(pending.detail());
                });
            });
        } else if self.workbench_mode == WorkbenchMode::Sketch {
            // The idle sketch rail: strokes commit as they are drawn, so the
            // persistent actions are the mainstream pair — finish the sketch
            // into the document, or step back out of sketch mode.
            let mut finish_clicked = false;
            let mut exit_clicked = false;
            rail_ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let exit_response = ui
                    .add(
                        egui::Button::new("")
                            .fill(BAD)
                            .stroke(Stroke::NONE)
                            .corner_radius(2)
                            .min_size(egui::vec2(30.0, 30.0)),
                    )
                    .on_hover_text("Exit sketch without saving it to the document");
                exit_response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Exit sketch")
                });
                paint_confirmation_cross(ui, exit_response.rect, Color32::WHITE);
                exit_clicked = confirmation_button_activated(ui, &exit_response);

                let finish_enabled = !self.sketch.authoring().operations().is_empty()
                    && !self.sketch_creation_draft_active()
                    && self.history_is_at_end();
                let finish_response = ui.add_enabled(
                    finish_enabled,
                    egui::Button::new("")
                        .fill(GOOD)
                        .stroke(Stroke::NONE)
                        .corner_radius(2)
                        .min_size(egui::vec2(30.0, 30.0)),
                );
                finish_response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        finish_enabled,
                        "Finish sketch",
                    )
                });
                paint_confirmation_tick(
                    ui,
                    finish_response.rect,
                    if finish_enabled { Color32::WHITE } else { MUTED },
                );
                let finish_response = if finish_enabled {
                    finish_response.on_hover_text("Finish the sketch and save it to the document")
                } else {
                    finish_response
                        .on_disabled_hover_text("Draw at least one stroke before finishing.")
                };
                finish_clicked = confirmation_button_activated(ui, &finish_response);

                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(RichText::new("Sketch").color(TEXT).strong()).truncate(),
                    )
                    .on_hover_text(
                        "Strokes commit as you draw. Finish saves the sketch; exit leaves it as a draft.",
                    );
                });
            });
            if exit_clicked {
                return Some(ConfirmationAction::ExitSketch);
            }
            if finish_clicked {
                return Some(ConfirmationAction::FinishSketch);
            }
        }

        if cancel_clicked {
            Some(ConfirmationAction::Cancel)
        } else if confirm_clicked {
            Some(ConfirmationAction::Confirm)
        } else {
            None
        }
    }

    /// Re-samples committed display scenes to a coarser presentation chord
    /// budget while a body projects small on screen, and back to full display
    /// density when it grows again. Buckets are deliberately coarse so a
    /// steady zoom does not oscillate between densities; the swap only ever
    /// touches presentation scenes, never snapshots, measures, or exports.
    fn refresh_display_detail_buckets(&mut self, viewport_size: egui::Vec2) {
        let (_, fit_radius) = self.view_frame();
        let points_per_unit =
            f64::from(viewport_size.x.min(viewport_size.y)) * 0.34 * self.view.zoom
                / fit_radius.max(1.0e-9);
        if !points_per_unit.is_finite() || points_per_unit <= 0.0 {
            return;
        }
        for body in &mut self.bodies {
            if !body.visible {
                continue;
            }
            let Some(bounds) = body.body.report.bounds else {
                continue;
            };
            let radius = 0.5
                * ((bounds.max.x - bounds.min.x).powi(2)
                    + (bounds.max.y - bounds.min.y).powi(2)
                    + (bounds.max.z - bounds.min.z).powi(2))
                .sqrt();
            let screen_radius = radius * points_per_unit;
            let bucket: u8 = if screen_radius >= 160.0 {
                0
            } else if screen_radius >= 48.0 {
                1
            } else {
                2
            };
            let entry = self
                .display_detail_buckets
                .entry(body.id.get())
                .or_insert(0);
            if *entry != bucket {
                *entry = bucket;
                let scale = match bucket {
                    0 => 1.0,
                    1 => 3.0,
                    _ => 9.0,
                };
                body.body.scene = NativeKernel::display_scene_scaled(&body.body.snapshot, scale);
            }
        }
    }

    fn model_viewport(&mut self, ui: &mut egui::Ui) {
        self.prune_stale_measured_edges();
        let feature_preview = self.feature_preview_for_frame(ui.ctx());
        let edge_finish_preview = self.edge_finish_preview_for_frame(Some(ui.ctx()));
        let mut sketch_overlays = self.visible_sketch_overlays();
        if self.sketch_orbit_peek
            && let Some(overlay) = self.active_sketch_peek_overlay()
        {
            sketch_overlays.push(overlay);
        }
        sketch_overlays.extend(self.visible_reference_plane_overlays());
        let reference_plane_bounds = self.visible_reference_plane_bounds();
        let active_body = self
            .active_body_id()
            .map(|body| viewport::BodyInstanceKey::new(body.get()));
        let selected = self.selected_face.and_then(|face| {
            active_body.map(|body| viewport::DocumentFaceSelection { body, face })
        });
        let measurement = self.current_measurement_annotation();
        let available = ui.available_size();
        let viewport_size =
            egui::vec2((available.x - 14.0).max(1.0), (available.y - 14.0).max(1.0));
        self.last_model_viewport_size = Some(viewport_size);
        self.refresh_display_detail_buckets(viewport_size);

        let frame_output = Frame::new()
            .fill(theme::VIEWPORT_BOTTOM)
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(4)
            .inner_margin(Margin::same(6))
            .show(ui, |ui| {
                ui.set_min_size(viewport_size);
                ui.set_max_size(viewport_size);
                let body_instances = self
                    .bodies
                    .iter()
                    .filter(|body| body.visible)
                    .filter_map(|body| {
                        let source_bounds = body.body.report.bounds?;
                        let body_key = viewport::BodyInstanceKey::new(body.id.get());
                        let feature_candidate = (active_body == Some(body_key))
                            .then(|| {
                                feature_preview
                                    .as_ref()
                                    .and_then(|preview| preview.candidate())
                            })
                            .flatten();
                        let edge_candidate = edge_finish_preview
                            .as_ref()
                            .filter(|preview| preview.body.get() == body.id.get())
                            .and_then(|preview| preview.candidate.as_deref());
                        let scene = feature_candidate.map_or_else(
                            || {
                                edge_candidate
                                    .map_or(&body.body.scene, |candidate| &candidate.scene)
                            },
                            |candidate| &candidate.scene,
                        );
                        let bounds = feature_candidate.map_or_else(
                            || edge_candidate.map_or(source_bounds, |candidate| candidate.bounds),
                            |candidate| candidate.bounds,
                        );
                        Some(
                            viewport::DocumentBodyInstance::new(
                                body_key,
                                scene,
                                Some(bounds),
                                bounds_center(source_bounds),
                            )
                            .with_tint(
                                // A picked Boolean tool overrides its material
                                // for the duration of the pick, so the operand
                                // set is legible on the model itself and not
                                // only in the ribbon readout.
                                if self.boolean_tools.contains(&body.id) {
                                    Some(BOOLEAN_TOOL_TINT)
                                } else {
                                    body.material
                                        .as_deref()
                                        .and_then(material::by_key)
                                        .map(|found| found.colour)
                                },
                            )
                            .with_base_transform(self.occurrence_transform_for_body(body.id)),
                        )
                    })
                    .collect::<Vec<_>>();
                // A finished sketch is content even when no body exists yet:
                // the placeholder must not replace the viewport while there is
                // still something to look at.
                let sketch_overlay_visible = !sketch_overlays.is_empty();
                if body_instances.is_empty()
                    && reference_plane_bounds.is_none()
                    && !sketch_overlay_visible
                {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new(if self.bodies.is_empty() {
                                "No committed body"
                            } else {
                                "All bodies hidden"
                            })
                            .color(MUTED),
                        );
                    });
                } else {
                    let output = viewport::show_document_with_feature_drag(
                        ui,
                        &body_instances,
                        reference_plane_bounds,
                        self.edge_overlay,
                        self.model_display_mode,
                        selected,
                        self.selected_edge,
                        self.selected_vertex,
                        &self.selected_faces,
                        &self.selected_edges,
                        &self.selected_vertices,
                        active_body,
                        self.active_tool,
                        &mut self.display_transform,
                        &mut self.view,
                        self.motion.phase,
                        feature_preview.as_ref(),
                        &sketch_overlays,
                        &self.measured_edges,
                        measurement.as_ref(),
                        edge_finish_preview.as_ref(),
                        &mut self.feature_preview_drag,
                        &mut self.model_edge_frame_memo,
                        self.document_settings.navigation,
                    );
                    if self.sketch_orbit_peek {
                        // The orbit peek is look-only: the camera responds,
                        // but a stray click must not select bodies or
                        // activate another sketch while one is being drawn.
                        return;
                    }
                    if let Some(feature_drag) = output.feature_drag {
                        self.set_extrusion_distance_intent(feature_drag.signed_extent);
                        self.sync_pending_sketch_extrusion_inputs();
                        self.sketch_extrusion_issue = None;
                    }
                    if let Some(delta) = output.edge_finish_distance_delta {
                        self.edge_finish_distance =
                            (self.edge_finish_distance + delta).clamp(0.001, 10_000.0);
                        self.edge_finish_distance_text =
                            format!("{:.3}", self.edge_finish_distance);
                    }
                    let edge_feature_pending = matches!(
                        self.pending_operation,
                        Some(PendingOperation::PresetFeature {
                            preset: SolidFeaturePreset::Chamfer | SolidFeaturePreset::Fillet,
                            ..
                        })
                    );
                    let boolean_pending = matches!(
                        self.pending_operation,
                        Some(PendingOperation::BooleanBodies { .. })
                    );
                    if boolean_pending {
                        // Any pick on a body names that body as a tool. Which
                        // face or edge was hit is irrelevant to a Boolean, so
                        // the whole body is the operand.
                        if let Some(key) = output
                            .selected_face
                            .map(|selection| selection.body)
                            .or_else(|| output.selected_edge.map(|selection| selection.body))
                            .or_else(|| output.selected_vertex.map(|selection| selection.body))
                            && let Some(body) = self
                                .bodies
                                .iter()
                                .find(|body| body.id.get() == key.get())
                                .map(|body| body.id)
                        {
                            self.toggle_boolean_tool(body);
                        }
                    } else if edge_feature_pending && output.edge_finish_distance_delta.is_none() {
                        if let Some(edge) = output.selected_edge {
                            let additive = ui.input(|input| input.modifiers.shift);
                            self.select_model_edge(edge, additive);
                            self.apply_tangent_edge_chain();
                        }
                    } else if self.pending_operation.is_some() {
                        // A live feature handle owns model-selection clicks
                        // until the universal confirmation gate is resolved.
                    } else if let Some(plane) = output.selected_reference_plane {
                        self.clear_model_entity_selection();
                        match plane {
                            viewport::ReferencePlaneSelection::Origin(index) => {
                                self.selected_origin_plane = match index {
                                    0 => SketchPlane::XY,
                                    1 => SketchPlane::YZ,
                                    _ => SketchPlane::XZ,
                                };
                                self.selected_construction_plane = None;
                                self.document_status = Some(format!(
                                    "{} selected",
                                    origin_plane_label(self.selected_origin_plane)
                                ));
                            }
                            viewport::ReferencePlaneSelection::Construction(id) => {
                                if let Some(plane) = self.construction_planes.iter().find(|plane| {
                                    plane.id == id
                                        && plane.visible
                                        && self.construction_plane_is_active(plane)
                                }) {
                                    self.selected_construction_plane = Some(id);
                                    self.document_status = Some(format!("{} selected", plane.name));
                                }
                            }
                        }
                    } else if self.active_tool == ActiveTool::Measure {
                        if let Some(edge) = output.selected_edge {
                            self.measured_face = None;
                            if self.measured_edges.len() >= 2
                                || self.measured_edges.first().copied() == Some(edge)
                            {
                                self.measured_edges.clear();
                            }
                            self.measured_edges.push(edge);
                            self.show_properties_tab();
                        } else if let Some(face) = output.selected_face {
                            self.measured_edges.clear();
                            self.measured_face = Some(face);
                            self.show_properties_tab();
                        }
                    } else if let Some(region) = output.selected_sketch_region {
                        if self.activate_committed_sketch(region.sketch_index) {
                            self.clear_model_entity_selection();
                            let selected = self.sketch.select_region_at_point(
                                SketchPoint::new(region.anchor[0], region.anchor[1]),
                                false,
                            );
                            if selected || self.sketch.selected_region_count() > 0 {
                                self.document_status = Some(
                                    "Committed sketch region selected · choose Extrude, Add, or Cut"
                                        .to_owned(),
                                );
                                self.show_properties_tab();
                            }
                        }
                    } else if let Some(vertex) = output.selected_vertex
                        && let Some(index) = self
                            .bodies
                            .iter()
                            .position(|body| body.id.get() == vertex.body.get())
                    {
                        let additive = ui.input(|input| input.modifiers.shift);
                        self.activate_body(index);
                        self.select_model_vertex(vertex, additive);
                    } else if let Some(edge) = output.selected_edge
                        && let Some(index) = self
                            .bodies
                            .iter()
                            .position(|body| body.id.get() == edge.body.get())
                    {
                        let additive = ui.input(|input| input.modifiers.shift);
                        self.activate_body(index);
                        self.select_model_edge(edge, additive);
                        self.show_properties_tab();
                    } else if let Some(selection) = output.selected_face
                        && !matches!(
                            self.pending_operation,
                            Some(
                                PendingOperation::ExtrudeSketch { .. }
                                    | PendingOperation::PushPullFace { .. }
                            )
                        )
                        && let Some(index) = self
                            .bodies
                            .iter()
                            .position(|body| body.id.get() == selection.body.get())
                    {
                        let additive = ui.input(|input| input.modifiers.shift);
                        self.activate_body(index);
                        self.select_model_face(selection, additive);
                    }
                }
            });
        self.model_canvas_overlay(ui, frame_output.response.rect.shrink(7.0));
        self.sync_transform_preview();
    }

    fn model_canvas_overlay(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let selection_count =
            self.selected_faces.len() + self.selected_edges.len() + self.selected_vertices.len();
        let selected = if selection_count > 1 {
            format!("Model · {selection_count} selected")
        } else if let Some(vertex) = self.selected_vertex {
            format!("Model · Vertex #{}", vertex.vertex.entity)
        } else if let Some(edge) = self.selected_edge {
            format!("Model · Edge #{}", edge.edge.entity)
        } else {
            self.selected_face.map_or_else(
                || {
                    let solid_count = self
                        .active_body_index()
                        .and_then(|index| self.bodies.get(index))
                        .map_or(1, |body| body.body.report.topology.solids);
                    format!(
                        "Model · {}",
                        browser_body_object_name(self.active_body_ordinal, solid_count)
                    )
                },
                |face| format!("Model · Face #{}", face.entity),
            )
        };
        // Keep the breadcrumb beside the view cube. Centering it over the
        // canvas instruction line causes the two labels to collide at the
        // supported compact window size.
        let title_width = 150.0_f32.min((rect.width() - 118.0).max(1.0));
        let title_rect = egui::Rect::from_min_size(
            egui::pos2(
                (rect.right() - 118.0 - title_width).max(rect.left()),
                rect.top(),
            ),
            egui::vec2(title_width, 24.0),
        );
        canvas_overlay_label(
            ui,
            "model_canvas_breadcrumb",
            title_rect,
            &selected,
            if selection_count > 0 || self.selected_face.is_some() {
                ACCENT
            } else {
                TEXT
            },
        );

        let cube_rect = egui::Rect::from_min_size(
            egui::pos2(rect.right() - 112.0, rect.top()),
            egui::vec2(112.0, 108.0),
        );
        if let Some(command) = model_view_cube(ui, cube_rect, self.view, true) {
            match command {
                ViewCubeCommand::Face(face) => self.view.set_standard_view(face),
                ViewCubeCommand::Roll { clockwise } => {
                    self.view.rotate_in_plane_quarter_turn(clockwise);
                }
                ViewCubeCommand::Isometric => self.reset_view(ui.ctx()),
            }
            ui.ctx().request_repaint();
        }

        let hud_rect = egui::Rect::from_center_size(
            egui::pos2(rect.center().x, rect.bottom() - 17.0),
            egui::vec2(254.0, 34.0),
        );
        let mut hud_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("model_navigation_hud")
                .max_rect(hud_rect)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        hud_ui.set_clip_rect(rect);
        Frame::new()
            .fill(PANEL.gamma_multiply(0.94))
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(3)
            .inner_margin(Margin::symmetric(3, 2))
            .show(&mut hud_ui, |ui| {
                let select = hud_button(
                    ui,
                    "V",
                    "Select model entities",
                    self.active_tool == ActiveTool::Select,
                );
                if select.clicked() {
                    self.active_tool = ActiveTool::Select;
                }
                let measure = hud_button(
                    ui,
                    "I",
                    "Measure an edge or the minimum distance between two edges",
                    self.active_tool == ActiveTool::Measure,
                );
                if measure.clicked() {
                    self.active_tool = ActiveTool::Measure;
                }
                let orbit = hud_button(
                    ui,
                    "O",
                    "Orbit model view",
                    self.active_tool == ActiveTool::Orbit,
                );
                if orbit.clicked() {
                    self.active_tool = ActiveTool::Orbit;
                }
                let frame = hud_button(ui, "F", "Frame all visible bodies", false);
                if frame.clicked() {
                    self.frame_visible_body(ui.ctx());
                }
                let edges = hud_button(ui, "E", "Toggle source edge overlay", self.edge_overlay);
                if edges.clicked() {
                    self.edge_overlay = !self.edge_overlay;
                }
                let motion = hud_button(
                    ui,
                    if self.motion.playing { "II" } else { ">" },
                    if self.motion.playing {
                        "Stop model motion and restore authored pose"
                    } else {
                        "Play temporary model inspection motion"
                    },
                    self.motion.playing,
                );
                if motion.clicked() {
                    self.toggle_animation(ui.ctx());
                }
                let home = hud_button(ui, "H", "Reset model view", false);
                if home.clicked() {
                    self.reset_view(ui.ctx());
                }
            });

        let Some(body) = &self.displayed else {
            return;
        };
        let report = &body.report;
        let (cadence, cadence_color) = self.cadence_status();
        let status_size = egui::vec2(230.0_f32.min(rect.width() - 16.0), 42.0);
        let status_pos = if rect.width() >= 720.0 {
            egui::pos2(rect.right() - status_size.x, rect.bottom() - status_size.y)
        } else {
            egui::pos2(rect.right() - status_size.x, rect.top() + 114.0)
        };
        let status_rect = egui::Rect::from_min_size(status_pos, status_size);
        let mut status_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt("model_canvas_status")
                .max_rect(status_rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        status_ui.set_clip_rect(rect);
        Frame::new()
            .fill(PANEL.gamma_multiply(0.94))
            .stroke(Stroke::new(1.0, BORDER))
            .corner_radius(3)
            .inner_margin(Margin::symmetric(6, 3))
            .show(&mut status_ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{} faces", report.topology.faces))
                            .small()
                            .color(TEXT),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{} edges · {} vertices",
                            report.topology.edges, report.topology.vertices
                        ))
                        .small()
                        .color(MUTED),
                    );
                    ui.label(
                        RichText::new(if report.validation.valid {
                            "Solid · valid"
                        } else {
                            "Invalid"
                        })
                        .small()
                        .color(if report.validation.valid { GOOD } else { BAD }),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(cadence.to_uppercase())
                            .small()
                            .color(cadence_color),
                    );
                    if self.transform_preview_pending() {
                        ui.label(RichText::new("PREVIEW — NOT COMMITTED").small().color(WARN));
                    }
                });
            });
    }

    fn collapsed_browser_rail(&mut self, ui: &mut egui::Ui) {
        let operation_pending = self.operation_confirmation_pending();
        ui.vertical_centered(|ui| {
            let response = ui
                .add_sized([24.0, 24.0], egui::Button::new("›").frame(false))
                .on_hover_text("Expand model browser");
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Expand model browser")
            });
            if shell_button_activated(ui, &response, operation_pending) {
                self.shell.set_model_browser(true);
            }
        });
    }

    fn feature_timeline(&mut self, ui: &mut egui::Ui) {
        let operation_pending = self.operation_confirmation_pending();
        if !self.shell.visibility().feature_timeline {
            ui.horizontal_centered(|ui| {
                let response = ui.add_sized([24.0, 22.0], egui::Button::new("+").frame(false));
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        "Expand design-history preview",
                    )
                });
                if shell_button_activated(ui, &response, operation_pending) {
                    self.shell.set_feature_timeline(true);
                }
                ui.label(RichText::new("Parametric history").small().color(MUTED))
                    .on_hover_text(
                        "Authoritative feature history with undo, suppression, and rebuild.",
                    );
            });
            return;
        }

        ui.horizontal_centered(|ui| {
            let response = ui
                .add_sized([24.0, 24.0], egui::Button::new("−").frame(false))
                .on_hover_text("Collapse design-history preview");
            response.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    true,
                    "Collapse design-history preview",
                )
            });
            if shell_button_activated(ui, &response, operation_pending) {
                self.shell.set_feature_timeline(false);
            }
            ui.label(
                RichText::new("PARAMETRIC HISTORY")
                    .font(FontId::proportional(10.5))
                    .strong()
                    .color(MUTED),
            )
            .on_hover_text("Stable feature identities and deterministic branch-local regeneration.");
            ui.separator();
            let history_position = self.document.history_position();
            let history_end = self.document.features().len();
            let back_enabled = !operation_pending && history_position > 0;
            let back = ui
                .add_enabled(back_enabled, egui::Button::new("◀").small())
                .on_hover_text("Move the history marker back one feature");
            back.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    back_enabled,
                    "Step history backward",
                )
            });
            let forward_enabled = !operation_pending && history_position < history_end;
            let forward = ui
                .add_enabled(forward_enabled, egui::Button::new("▶").small())
                .on_hover_text("Move the history marker forward one feature");
            forward.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    forward_enabled,
                    "Step history forward",
                )
            });
            let slider = ui.add_enabled(
                !operation_pending && history_end > 0,
                egui::Slider::new(&mut self.history_scrub_position, 0..=history_end)
                    .show_value(false)
                    .trailing_fill(true),
            );
            slider.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Slider,
                    !operation_pending && history_end > 0,
                    "History rollback marker",
                )
            });
            slider
                .clone()
                .on_hover_text(format!(
                    "Rollback marker · {} of {history_end}",
                    self.history_scrub_position
                ));
            let requested_history_position = if back.clicked() {
                history_position.checked_sub(1)
            } else if forward.clicked() {
                Some((history_position + 1).min(history_end))
            } else if slider.drag_stopped()
                || slider.changed() && !slider.dragged()
            {
                Some(self.history_scrub_position)
            } else {
                None
            };
            ui.label(
                RichText::new(format!("{history_position}/{history_end}"))
                    .monospace()
                    .small()
                    .color(ACCENT),
            );
            if let Some(position) = requested_history_position {
                self.move_history_cursor(position);
            }
            ui.separator();
            let undo_enabled = !operation_pending && self.document.can_undo();
            let undo = ui.add_enabled(undo_enabled, egui::Button::new("Undo").small());
            undo.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    undo_enabled,
                    "Undo history change",
                )
            });
            let redo_enabled = !operation_pending && self.document.can_redo();
            let redo = ui.add_enabled(redo_enabled, egui::Button::new("Redo").small());
            redo.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    redo_enabled,
                    "Redo history change",
                )
            });
            let selected_node = self
                .selected_history_feature
                .and_then(|feature| self.document.feature(feature));
            let selected_suppressed = selected_node.is_some_and(|feature| feature.state.suppressed);
            let suppression_enabled = !operation_pending
                && self
                    .selected_history_feature
                    .is_some_and(|feature| self.document.feature_is_active(feature).unwrap_or(false))
                && selected_node.is_some_and(|feature| {
                    !feature.state.read_only
                        && !matches!(feature.kind, FeatureKind::Origin | FeatureKind::BaseBody)
                });
            let suppress = ui.add_enabled(
                suppression_enabled,
                egui::Button::new(if selected_suppressed { "Restore" } else { "Suppress" })
                    .small(),
            );
            suppress.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    suppression_enabled,
                    if selected_suppressed {
                        "Restore selected feature"
                    } else {
                        "Suppress selected feature"
                    },
                )
            });
            let selected_dirty = selected_node
                .is_some_and(|feature| feature.state.rebuild == RebuildState::Dirty);
            let rebuild = ui.add_enabled(
                !operation_pending && selected_dirty,
                egui::Button::new("Rebuild").small(),
            );
            rebuild.widget_info(|| {
                egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    selected_dirty,
                    "Rebuild selected branch",
                )
            });
            if undo.clicked() {
                self.undo_document();
            } else if redo.clicked() {
                self.redo_document();
            } else if suppress.clicked()
                && let Some(feature) = self.selected_history_feature
            {
                self.toggle_feature_suppression(feature);
            } else if rebuild.clicked()
                && let Some(feature) = self.selected_history_feature
            {
                self.rebuild_document_from(feature);
            }
            let dirty_count = self.document_dirty_feature_count();
            ui.label(
                RichText::new(if dirty_count == 0 {
                    "CLEAN".to_owned()
                } else {
                    format!("{dirty_count} DIRTY")
                })
                .small()
                .color(if dirty_count == 0 { GOOD } else { WARN }),
            )
            .on_hover_text(
                self.document_status
                    .as_deref()
                    .unwrap_or("Parametric document is current"),
            );
            ui.separator();
            let entries = &self.feature_preview.entries;
            let document_features = self.document.features();
            let history_position = self.document.history_position();
            let active_sketch = self.feature_preview.active_sketch;
            let active_sketch_support_current = self.sketch_support_is_current();
            let pending_operation_clear = self.pending_operation.is_none();
            let workbench_mode = self.workbench_mode;
            let selected_model_entry = entries.iter().rposition(|entry| {
                !matches!(
                    entry.kind,
                    FeaturePreviewKind::Origin | FeaturePreviewKind::Sketch
                )
            });
            let mut requested_mode = None;
            let mut selected_feature = None;
            egui::ScrollArea::horizontal()
                .id_salt("feature_timeline_scroll")
                .auto_shrink([false, true])
                .stick_to_right(true)
                .show(ui, |ui| {
                    // Compact chip metrics keep even a group-framed entry within
                    // the fixed timeline strip: an overflowing bottom panel
                    // mis-reports its consumed height to the parent and lets
                    // the central viewport shift across confirmation.
                    ui.spacing_mut().interact_size.y = 20.0;
                    ui.spacing_mut().button_padding = egui::vec2(7.0, 2.0);
                    ui.horizontal(|ui| {
                        let mut render_entry = |ui: &mut egui::Ui, index: usize| {
                            let entry = &entries[index];
                            if entry.kind == FeaturePreviewKind::Origin {
                                timeline_chip(
                                    ui,
                                    &entry.label(),
                                    false,
                                    if index < history_position { GOOD } else { MUTED },
                                );
                                return;
                            }

                            let sketch_entry = entry.kind == FeaturePreviewKind::Sketch;
                            let feature_active = index < history_position;
                            let enabled = pending_operation_clear
                                && feature_active
                                && (!sketch_entry
                                    || (active_sketch == Some(index)
                                        && active_sketch_support_current));
                            let selected = if sketch_entry {
                                workbench_mode == WorkbenchMode::Sketch
                                    && active_sketch == Some(index)
                            } else {
                                workbench_mode == WorkbenchMode::Model
                                    && selected_model_entry == Some(index)
                            };
                            let response = ui.add_enabled(
                                enabled,
                                egui::Button::new(entry.label())
                                    .selected(selected)
                                    .fill(if selected {
                                        translucent(timeline_group_color(entry.group), 44)
                                    } else {
                                        Color32::TRANSPARENT
                                    })
                                    .stroke(Stroke::new(
                                        1.0,
                                        translucent(
                                            timeline_group_color(entry.group),
                                            if selected { 220 } else { 96 },
                                        ),
                                    ))
                                    .corner_radius(3),
                            );
                            let accessible_label = entry.accessible_label();
                            response.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    enabled,
                                    &accessible_label,
                                )
                            });
                            let response = if sketch_entry {
                                response.on_hover_text(if !active_sketch_support_current {
                                    "Historical face sketch; replay-safe, but direct geometry editing is not enabled yet."
                                } else if entry.finished {
                                    "Finished sketch entry; select a solid feature to suppress or restore its branch."
                                } else {
                                    "Active committed sketch entry in the parametric document."
                                })
                            } else {
                                response
                            };
                            if response.clicked() {
                                selected_feature = document_features
                                    .get(index)
                                    .map(|feature| feature.id);
                                requested_mode = Some(if sketch_entry {
                                    WorkbenchMode::Sketch
                                } else {
                                    WorkbenchMode::Model
                                });
                            }
                        };

                        let mut index = 0;
                        while index < entries.len() {
                            if index > 0 {
                                timeline_connector(ui, false, BORDER);
                            }
                            let group = entries[index].group;
                            if group == 0 {
                                render_entry(ui, index);
                                index += 1;
                                continue;
                            }
                            let mut end = index + 1;
                            while end < entries.len() && entries[end].group == group {
                                end += 1;
                            }
                            let group_color = timeline_group_color(group);
                            let group_active = (index..end).any(|entry| entry < history_position);
                            Frame::new()
                                .fill(translucent(
                                    group_color,
                                    if group_active { 34 } else { 16 },
                                ))
                                .stroke(Stroke::new(1.0, translucent(group_color, 150)))
                                .corner_radius(5)
                                .inner_margin(Margin::symmetric(4, 3))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        for entry_index in index..end {
                                            if entry_index > index {
                                                timeline_connector(ui, true, group_color);
                                            }
                                            render_entry(ui, entry_index);
                                        }
                                    });
                                });
                            index = end;
                        }
                    });
                });
            if let Some(feature) = selected_feature {
                self.selected_history_feature = Some(feature);
                self.show_properties_tab();
            }
            match requested_mode {
                // A History chip navigates to the already-active sketch. It
                // must not run the new-sketch entry path, which may bind a
                // selected model face and replace the current sketch canvas.
                Some(WorkbenchMode::Sketch) if active_sketch_support_current => {
                    self.workbench_mode = WorkbenchMode::Sketch;
                }
                Some(WorkbenchMode::Sketch) => {}
                Some(WorkbenchMode::Model) => self.enter_model_mode(),
                None => {}
            }
        });
    }
}

impl eframe::App for KernelLabApp {
    fn logic(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_async_sketch_extrusion_commit(context);
        if !self.advance_face_camera_transition(context) {
            self.advance_motion(context);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.sketch_dimension_keys = DimensionKeyClaims::default();
        let operation_at_frame_start = self.pending_operation;
        if let Some(focused) = ui.ctx().memory(|memory| memory.focused()) {
            self.last_focused_editor = Some(focused);
        }
        // Capture global model shortcuts before a focused editor can consume
        // them, but execute them only after that editor has rendered.
        let (mut cancel_pending, mut confirm_pending) = ui.ctx().input(|input| {
            let key_pressed = |wanted: egui::Key| {
                input.raw.events.iter().any(|event| {
                    matches!(
                        event,
                        egui::Event::Key {
                            key,
                            pressed: true,
                            repeat: false,
                            modifiers,
                            ..
                        } if *key == wanted && *modifiers == egui::Modifiers::NONE
                    )
                })
            };
            (
                key_pressed(egui::Key::Escape),
                key_pressed(egui::Key::Enter),
            )
        });
        egui::Panel::top("lab_header")
            .exact_size(38.0)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(10, 4))
                    .stroke(Stroke::new(0.0, Color32::TRANSPARENT)),
            )
            .show(ui, |ui| self.header(ui));

        let ribbon_height = if self.shell.visibility().command_ribbon {
            if self.workbench_mode == WorkbenchMode::Sketch {
                // Two 32 px icon rows plus the group caption underneath need
                // their own vertical breathing room. The larger reservation
                // prevents the second row from overlapping the viewport at
                // 1040×700.
                112.0
            } else {
                72.0
            }
        } else {
            30.0
        };
        egui::Panel::top("command_ribbon")
            .exact_size(ribbon_height)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(RIBBON_FILL)
                    .inner_margin(Margin::symmetric(8, 4))
                    .stroke(Stroke::new(1.0, BORDER)),
            )
            .show(ui, |ui| self.command_ribbon(ui));

        let timeline_height = if self.shell.visibility().feature_timeline {
            38.0
        } else {
            28.0
        };
        // The confirmation controls only exist while they have something to
        // say. Idle in model mode the old rail rendered as a bare strip
        // under the timeline, indistinguishable from a layout bug. The
        // sketch workspace keeps a rail for its persistent Finish/Exit pair
        // (entering a sketch re-lays the whole screen out anyway), but a
        // pending model operation floats its tick/cross over the canvas
        // instead: staging an operation must never move or resize the
        // viewport under a live drag.
        let confirmation_action = if self.workbench_mode == WorkbenchMode::Sketch {
            egui::Panel::bottom("operation_confirmation")
                .exact_size(38.0)
                .resizable(false)
                .show_separator_line(false)
                .frame(
                    Frame::new()
                        .fill(PANEL)
                        .inner_margin(Margin::symmetric(6, 3))
                        .stroke(Stroke::new(1.0, BORDER)),
                )
                .show(ui, |ui| self.confirmation_slot(ui))
                .inner
        } else if self.pending_operation.is_some() {
            // Positioned from the screen rectangle rather than `anchor`:
            // an anchored area only knows its own size one frame after it
            // first appears, and a chip that shifts on its second frame
            // breaks clicks aimed at where it stood on its first.
            let screen = ui.ctx().content_rect();
            let chip_size = egui::vec2(442.0, 38.0);
            egui::Area::new(egui::Id::new("operation_confirmation_overlay"))
                .fixed_pos(egui::pos2(
                    screen.center().x - chip_size.x / 2.0,
                    screen.bottom() - timeline_height - 10.0 - chip_size.y,
                ))
                // Without a size hint, the area's first frame runs egui's
                // constrain pass against an unknown size and lands the chip
                // mid-screen; the very first click aimed at it then misses.
                .default_size(chip_size)
                .constrain(false)
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |area_ui| {
                    Frame::new()
                        .fill(PANEL)
                        .stroke(Stroke::new(1.0, BORDER))
                        .corner_radius(6)
                        .inner_margin(Margin::symmetric(6, 3))
                        .show(area_ui, |ui| {
                            ui.set_min_size(egui::vec2(430.0, 32.0));
                            ui.set_max_size(egui::vec2(430.0, 32.0));
                            self.confirmation_slot(ui)
                        })
                        .inner
                })
                .inner
        } else {
            None
        };
        egui::Panel::bottom("feature_timeline")
            .exact_size(timeline_height)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(TIMELINE_FILL)
                    .inner_margin(Margin::symmetric(8, 3))
                    .stroke(Stroke::new(1.0, BORDER)),
            )
            .show(ui, |ui| self.feature_timeline(ui));

        // The settings dialog takes the inspector's place while it is open.
        // Both anchor to the right edge, and letting them stack put the
        // dialog on top of the dock — the inspector's controls stayed in the
        // accessibility tree but a pointer could no longer reach them.
        if self.inspector_open && !self.document_properties_open {
            egui::Panel::right("contextual_inspector")
                // Fixed width, deliberately not resizable. A width the content
                // can influence lets one long diagnostic line shift the
                // viewport beside it, which moves every model coordinate under
                // the pointer mid-operation.
                .exact_size(268.0)
                .resizable(false)
                .show_separator_line(false)
                .frame(
                    Frame::new()
                        .fill(PANEL)
                        .inner_margin(Margin::symmetric(6, 6))
                        .stroke(Stroke::new(1.0, BORDER)),
                )
                .show(ui, |ui| self.contextual_inspector_panel(ui));
        }

        if self.shell.visibility().model_browser {
            egui::Panel::left("model_browser_expanded")
                .default_size(232.0)
                .size_range(190.0..=320.0)
                .resizable(true)
                .show_separator_line(false)
                .frame(
                    Frame::new()
                        .fill(PANEL)
                        .inner_margin(Margin::symmetric(6, 6))
                        .stroke(Stroke::new(1.0, BORDER)),
                )
                .show(ui, |ui| self.left_workspace_panel(ui));
        } else {
            egui::Panel::left("model_browser_collapsed")
                .exact_size(28.0)
                .resizable(false)
                .show_separator_line(false)
                .frame(
                    Frame::new()
                        .fill(PANEL)
                        .inner_margin(Margin::symmetric(2, 4))
                        .stroke(Stroke::new(1.0, BORDER)),
                )
                .show(ui, |ui| self.collapsed_browser_rail(ui));
        }

        let central_panel = egui::CentralPanel::default()
            .frame(Frame::new().fill(BG).inner_margin(Margin::ZERO))
            .show(ui, |ui| match self.workbench_mode {
                WorkbenchMode::Model => self.model_viewport(ui),
                WorkbenchMode::Sketch => {
                    if self.sketch_orbit_peek {
                        self.sketch_orbit_peek_viewport(ui);
                    } else {
                        self.sketch_viewport(ui);
                    }
                }
            });

        self.edge_finish_editor(ui.ctx());
        self.extrusion_feature_editor(ui.ctx());
        self.document_properties_window(ui.ctx());

        if let Some(staging_id) = self
            .part_library
            .show(ui.ctx(), self.pending_operation.is_some())
        {
            self.pending_operation = Some(PendingOperation::LibraryInsertion { staging_id });
        }

        match confirmation_action {
            Some(ConfirmationAction::FinishSketch) => {
                self.finish_sketch_now();
            }
            Some(ConfirmationAction::ExitSketch) => {
                self.enter_model_mode();
            }
            _ => {}
        }
        cancel_pending = (cancel_pending
            && operation_at_frame_start.is_some()
            && self.pending_operation == operation_at_frame_start)
            || confirmation_action == Some(ConfirmationAction::Cancel);
        confirm_pending = (confirm_pending
            && operation_at_frame_start.is_some()
            && self.pending_operation == operation_at_frame_start)
            || confirmation_action == Some(ConfirmationAction::Confirm);

        if self.workbench_mode == WorkbenchMode::Sketch {
            if self.sketch_dimension_keys.escape {
                cancel_pending = false;
            }
            if self.sketch_dimension_keys.enter || self.sketch_dimension_keys.confirmation_blocked {
                confirm_pending = false;
            }
        }

        // A direct-manipulation gesture is modal until pointer release. This
        // prevents Enter/Escape or a coincident action from committing or
        // cancelling a half-sampled extrusion.
        if self.feature_preview_drag.is_active() {
            cancel_pending = false;
            confirm_pending = false;
        }

        // Focused numeric editors must finalize this frame's value before a
        // keyboard confirmation resets preview state. Otherwise their retained text
        // buffer can restore the old preview when focus later changes.
        if (cancel_pending || confirm_pending)
            && let Some(recorder) = self.development_recorder.as_ref()
        {
            recorder.log(
                "command.activate",
                serde_json::json!({
                    "command": if confirm_pending { "Confirm" } else { "Cancel" },
                    "source": match confirmation_action {
                        Some(_) => "confirmation_button",
                        None => "keyboard"
                    },
                    "pending_operation": operation_at_frame_start.map(PendingOperation::title)
                }),
            );
        }
        self.handle_shortcuts(ui.ctx(), cancel_pending, confirm_pending);

        let mut selection_hasher = DefaultHasher::new();
        self.selected_faces.hash(&mut selection_hasher);
        self.selected_edges.hash(&mut selection_hasher);
        self.selected_vertices.hash(&mut selection_hasher);
        let trace_fingerprint = DevelopmentTraceFingerprint {
            workbench: self.workbench_mode,
            pending_operation: self.pending_operation.map(PendingOperation::title),
            model_tool: active_tool_trace_name(self.active_tool),
            sketch_tool: self.active_sketch_tool.descriptor().stable_key,
            history_position: self.history_scrub_position,
            snapshot: self.displayed_snapshot_id(),
            selection_digest: selection_hasher.finish(),
            drag_active: self.feature_preview_drag.is_active(),
        };
        let trace_state_changed =
            self.last_development_trace_fingerprint != Some(trace_fingerprint);
        self.last_development_trace_fingerprint = Some(trace_fingerprint);
        if let Some(recorder) = self.development_recorder.as_mut() {
            recorder.capture_egui_input(ui.ctx(), central_panel.response.rect);
            if trace_state_changed {
                let mut selected_targets = BTreeSet::new();
                for selection in &self.selected_faces {
                    selected_targets.insert(format!(
                        "body:{}:face:{}",
                        selection.body.get(),
                        selection.face
                    ));
                }
                for selection in &self.selected_edges {
                    selected_targets.insert(format!(
                        "body:{}:edge:{}",
                        selection.body.get(),
                        selection.edge
                    ));
                }
                for selection in &self.selected_vertices {
                    selected_targets.insert(format!(
                        "body:{}:vertex:{}",
                        selection.body.get(),
                        selection.vertex
                    ));
                }
                recorder.observe_ui_state(UiTraceState {
                    workbench: trace_fingerprint.workbench.label(),
                    pending_operation: trace_fingerprint.pending_operation,
                    model_tool: trace_fingerprint.model_tool.to_owned(),
                    sketch_tool: trace_fingerprint.sketch_tool.to_owned(),
                    history_position: trace_fingerprint.history_position,
                    snapshot: trace_fingerprint
                        .snapshot
                        .map(|snapshot| snapshot.to_string()),
                    selected_targets: selected_targets.into_iter().collect(),
                    drag_active: trace_fingerprint.drag_active,
                });
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ViewCubeCommand {
    Face(StandardView),
    Roll { clockwise: bool },
    Isometric,
}

fn model_view_cube(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    view: ViewState,
    show_controls: bool,
) -> Option<ViewCubeCommand> {
    ui.painter().rect(
        rect,
        4.0,
        translucent(PANEL, 92),
        Stroke::new(1.0, translucent(BORDER, 132)),
        egui::StrokeKind::Inside,
    );
    let cube_center = egui::pos2(
        rect.center().x,
        if show_controls {
            rect.top() + 43.0
        } else {
            rect.center().y
        },
    );
    let cube_scale = 22.0_f32;
    let mut visible_faces = view_cube_faces()
        .into_iter()
        .filter_map(|(face, vertices)| {
            let facing = view.project_direction(face.outward_normal()).depth;
            (facing > 1.0e-6).then(|| {
                let projected = vertices.map(|vertex| {
                    let point = view.project_direction(vertex);
                    egui::pos2(
                        cube_center.x + point.coordinates[0] as f32 * cube_scale,
                        cube_center.y + point.coordinates[1] as f32 * cube_scale,
                    )
                });
                (face, vertices, projected, facing)
            })
        })
        .collect::<Vec<_>>();
    visible_faces.sort_by(|left, right| left.3.total_cmp(&right.3));

    let nearest = view.nearest_standard_view();
    let mut command = None;
    for (face, _, points, facing) in visible_faces {
        let center = points
            .iter()
            .fold(egui::Vec2::ZERO, |sum, point| sum + point.to_vec2())
            / 4.0;
        let center = egui::pos2(center.x, center.y);
        let hit_rect = egui::Rect::from_center_size(center, egui::vec2(34.0, 18.0))
            .intersect(rect.shrink(3.0));
        let response = ui.interact(
            hit_rect,
            ui.id().with(("view-cube-face", face.label())),
            egui::Sense::click(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("View cube {}", face.label().to_ascii_lowercase()),
            )
        });
        let base = if face == nearest {
            Color32::from_rgb(214, 222, 231)
        } else {
            Color32::from_rgb(188, 197, 208)
        };
        let light = (0.86 + facing as f32 * 0.14).clamp(0.86, 1.0);
        let mut fill = translucent(base.gamma_multiply(light), 235);
        if response.hovered() {
            fill = blend_color(fill, ACCENT, 0.30);
        }
        ui.painter().add(egui::Shape::convex_polygon(
            points.to_vec(),
            fill,
            Stroke::new(1.0, Color32::from_rgb(148, 158, 170)),
        ));
        ui.painter().text(
            center,
            egui::Align2::CENTER_CENTER,
            face.label(),
            FontId::monospace(7.0),
            Color32::from_rgb(58, 68, 79),
        );
        if response.clicked() {
            command = Some(ViewCubeCommand::Face(face));
        }
    }

    if !show_controls {
        return command;
    }

    let controls_y = rect.bottom() - 25.0;
    for (x, width, text, label, next) in [
        (
            rect.left() + 8.0,
            24.0,
            "<",
            "Rotate view counter-clockwise",
            ViewCubeCommand::Roll { clockwise: false },
        ),
        (
            rect.center().x - 17.0,
            34.0,
            "ISO",
            "Reset to isometric view",
            ViewCubeCommand::Isometric,
        ),
        (
            rect.right() - 32.0,
            24.0,
            ">",
            "Rotate view clockwise",
            ViewCubeCommand::Roll { clockwise: true },
        ),
    ] {
        let button_rect =
            egui::Rect::from_min_size(egui::pos2(x, controls_y), egui::vec2(width, 20.0));
        let response = ui.put(
            button_rect,
            egui::Button::new(RichText::new(text).small())
                .corner_radius(3)
                .fill(translucent(CARD, 188)),
        );
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        if response.clicked() {
            command = Some(next);
        }
    }
    command
}

fn view_cube_faces() -> [(StandardView, [Vector3; 4]); 6] {
    let point = Vector3::new;
    [
        (
            StandardView::Right,
            [
                point(1.0, -1.0, -1.0),
                point(1.0, 1.0, -1.0),
                point(1.0, 1.0, 1.0),
                point(1.0, -1.0, 1.0),
            ],
        ),
        (
            StandardView::Left,
            [
                point(-1.0, 1.0, -1.0),
                point(-1.0, -1.0, -1.0),
                point(-1.0, -1.0, 1.0),
                point(-1.0, 1.0, 1.0),
            ],
        ),
        (
            StandardView::Back,
            [
                point(1.0, 1.0, -1.0),
                point(-1.0, 1.0, -1.0),
                point(-1.0, 1.0, 1.0),
                point(1.0, 1.0, 1.0),
            ],
        ),
        (
            StandardView::Front,
            [
                point(-1.0, -1.0, -1.0),
                point(1.0, -1.0, -1.0),
                point(1.0, -1.0, 1.0),
                point(-1.0, -1.0, 1.0),
            ],
        ),
        (
            StandardView::Top,
            [
                point(-1.0, -1.0, 1.0),
                point(1.0, -1.0, 1.0),
                point(1.0, 1.0, 1.0),
                point(-1.0, 1.0, 1.0),
            ],
        ),
        (
            StandardView::Bottom,
            [
                point(-1.0, 1.0, -1.0),
                point(1.0, 1.0, -1.0),
                point(1.0, -1.0, -1.0),
                point(-1.0, -1.0, -1.0),
            ],
        ),
    ]
}

fn blend_color(left: Color32, right: Color32, amount: f32) -> Color32 {
    let amount = amount.clamp(0.0, 1.0);
    let channel = |left: u8, right: u8| {
        (f32::from(left) * (1.0 - amount) + f32::from(right) * amount).round() as u8
    };
    Color32::from_rgb(
        channel(left.r(), right.r()),
        channel(left.g(), right.g()),
        channel(left.b(), right.b()),
    )
}

fn translucent(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

fn default_catalog_root() -> PathBuf {
    if let Some(root) = std::env::var_os("ARTIFICER_CATALOG_DIR").filter(|value| !value.is_empty())
    {
        return PathBuf::from(root);
    }
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("Artificer")
            .join("catalog");
    }
    #[cfg(target_os = "windows")]
    if let Some(local_data) = std::env::var_os("LOCALAPPDATA").filter(|value| !value.is_empty()) {
        return PathBuf::from(local_data).join("Artificer").join("catalog");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty())
        {
            return PathBuf::from(data_home).join("artificer").join("catalog");
        }
        if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("artificer")
                .join("catalog");
        }
    }
    std::env::temp_dir().join("artificer-catalog")
}

fn default_document_path() -> PathBuf {
    if let Some(path) =
        std::env::var_os("ARTIFICER_DOCUMENT_PATH").filter(|value| !value.is_empty())
    {
        return PathBuf::from(path);
    }
    let catalog = default_catalog_root();
    catalog
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("current.artificer")
}

fn save_bounded_json(path: &Path, json: &str, kind: &str) -> Result<(), String> {
    if json.len() as u64 > MAX_NATIVE_DOCUMENT_BYTES {
        return Err(format!(
            "{kind} exceeds the {MAX_NATIVE_DOCUMENT_BYTES} byte save limit"
        ));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| format!("the {kind} path has no valid file name"))?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let unique = DOCUMENT_SAVE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".{file_name}.{}.{unique}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| error.to_string())?;
        file.write_all(json.as_bytes())
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        fs::rename(&temporary, path).map_err(|error| error.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn read_bounded_document(path: &Path) -> Result<String, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Err("the Artificer document path must be a regular non-symlink file".into());
    }
    if metadata.len() > MAX_NATIVE_DOCUMENT_BYTES {
        return Err(format!(
            "Artificer document exceeds the {MAX_NATIVE_DOCUMENT_BYTES} byte load limit"
        ));
    }
    fs::read_to_string(path).map_err(|error| error.to_string())
}

fn deliberately_stale_id(current: SnapshotId) -> SnapshotId {
    let candidate = SnapshotId::new([0xA5; 16]);
    if candidate == current {
        SnapshotId::new([0x5A; 16])
    } else {
        candidate
    }
}

fn protocol_planar_profile(profile: &CertifiedSketchProfile) -> Option<PlanarProfile2> {
    let regions = profile
        .regions
        .iter()
        .map(|region| {
            Some(PlanarRegion2 {
                outer: protocol_planar_loop(&region.outer)?,
                holes: region
                    .holes
                    .iter()
                    .map(protocol_planar_loop)
                    .collect::<Option<Vec<_>>>()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let profile = PlanarProfile2 { regions };
    (profile.regions.len() <= MAX_PLANAR_PROFILE_REGIONS
        && profile.loop_count() <= MAX_PLANAR_PROFILE_LOOPS
        && profile.curve_count() <= MAX_PLANAR_PROFILE_CURVES)
        .then_some(profile)
}

fn protocol_planar_loop(profile_loop: &CertifiedSketchLoop) -> Option<PlanarLoop2> {
    let curves = profile_loop
        .curves
        .iter()
        .copied()
        .map(|curve| {
            Some(match curve {
                CertifiedSketchCurve::Line { start, end } => PlanarCurve2::Line {
                    start: protocol_sketch_point(start),
                    end: protocol_sketch_point(end),
                },
                CertifiedSketchCurve::CircularArc {
                    center,
                    start,
                    end,
                    direction,
                } => PlanarCurve2::CircularArc {
                    center: protocol_sketch_point(center),
                    start: protocol_sketch_point(start),
                    end: protocol_sketch_point(end),
                    direction: protocol_arc_direction(direction),
                },
                CertifiedSketchCurve::Circle {
                    center,
                    rim,
                    direction,
                } => {
                    let radius = center.distance_squared(rim).sqrt();
                    if !radius.is_finite() || radius <= 0.0 {
                        return None;
                    }
                    PlanarCurve2::Circle {
                        center: protocol_sketch_point(center),
                        radius,
                        direction: protocol_arc_direction(direction),
                    }
                }
            })
        })
        .collect::<Option<Vec<_>>>()?;
    (!curves.is_empty()).then_some(PlanarLoop2 { curves })
}

/// Samples exact profile curves only for the live display mesh. The modeling
/// request remains the untouched [`PlanarProfile2`], so changing this visual
/// density can never change topology, measures, history, or semantic hashes.
fn build_async_feature_preview(
    intent: &AsyncFeaturePreviewIntent,
    input: Option<&Snapshot>,
    source_scene: Option<&DebugScene>,
    cancellation: Option<&artificer_compute::CancellationToken>,
) -> Option<viewport::FeaturePreview> {
    let regions = preview_planar_profile_regions(intent.frame, &intent.profile)?;
    let direction = frame_normal(intent.frame)?;
    let style = match intent.mode {
        ExtrusionMode::NewBody => viewport::FeaturePreviewStyle::Neutral,
        ExtrusionMode::Add => viewport::FeaturePreviewStyle::Add,
        ExtrusionMode::Cut => viewport::FeaturePreviewStyle::Cut,
    };
    let preview =
        viewport::FeaturePreview::planar_regions(regions, direction, intent.distance, style);
    if intent.mode != ExtrusionMode::Cut
        || cancellation.is_some_and(artificer_compute::CancellationToken::is_cancelled)
    {
        return Some(preview);
    }
    let input = input.filter(|input| Some(input.id()) == intent.input_snapshot)?;
    let command = build_planar_profile_extrusion_command(
        intent.frame,
        intent.profile.clone(),
        intent.target_face,
        intent.distance,
        intent.mode,
    )?;
    let precision = input.precision_policy().unwrap_or_default();
    let outcome = NativeKernel::execute(
        input,
        &ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("workbench-cut-preview"),
            expected_snapshot: input.id(),
            precision,
            command,
        },
        &CancellationToken::new(),
    )
    .ok()?;
    if cancellation.is_some_and(artificer_compute::CancellationToken::is_cancelled) {
        return None;
    }
    let bounds = outcome.report.bounds?;
    let owned_source_scene;
    let source_scene = if let Some(source_scene) = source_scene {
        source_scene
    } else {
        owned_source_scene = NativeKernel::debug_scene(input);
        &owned_source_scene
    };
    let scene = NativeKernel::debug_scene(&outcome.snapshot);
    let changed_faces = new_surface_faces(source_scene, &scene, precision);
    Some(preview.with_candidate(viewport::FeatureCandidatePreview {
        scene,
        bounds,
        changed_faces,
        distance: intent.distance,
    }))
}

/// Evaluates the exact command behind a staged chamfer or fillet without
/// publishing it.  Confirmation executes the same command again against the
/// same immutable snapshot, so the preview cannot diverge from the committed
/// body through a separate display-only construction path.
fn build_exact_edge_finish_preview(
    input: &Snapshot,
    source_scene: &DebugScene,
    intent: &AsyncEdgeFinishPreviewIntent,
    cancellation: Option<&artificer_compute::CancellationToken>,
) -> Option<viewport::EdgeFinishCandidatePreview> {
    if input.id() != intent.input_snapshot
        || intent.target_edges.is_empty()
        || !intent.distance.is_finite()
        || intent.distance <= 0.0
        || cancellation.is_some_and(artificer_compute::CancellationToken::is_cancelled)
    {
        return None;
    }
    let command = if intent.target_edges.len() == 1 {
        KernelCommand::FinishEdge {
            target_edge: intent.target_edges[0],
            kind: intent.kind,
            distance: intent.distance,
        }
    } else {
        KernelCommand::FinishEdges {
            target_edges: intent.target_edges.clone(),
            kind: intent.kind,
            distance: intent.distance,
        }
    };
    let precision = input.precision_policy().unwrap_or_default();
    let request = ExecuteRequest {
        protocol_version: CURRENT_PROTOCOL_VERSION,
        request_id: RequestId::new("workbench-edge-finish-preview"),
        expected_snapshot: input.id(),
        precision,
        command,
    };
    let outcome = NativeKernel::execute(input, &request, &CancellationToken::new()).ok()?;
    if cancellation.is_some_and(artificer_compute::CancellationToken::is_cancelled) {
        return None;
    }
    let bounds = outcome.report.bounds?;
    let scene = NativeKernel::debug_scene(&outcome.snapshot);
    if cancellation.is_some_and(artificer_compute::CancellationToken::is_cancelled) {
        return None;
    }
    let changed_faces = new_surface_faces(source_scene, &scene, precision);
    Some(viewport::EdgeFinishCandidatePreview {
        scene,
        bounds,
        changed_faces,
        distance: intent.distance,
    })
}

fn new_surface_faces(
    source: &DebugScene,
    candidate: &DebugScene,
    precision: PrecisionPolicy,
) -> BTreeSet<EntityRef> {
    let source_planes = source
        .triangles
        .iter()
        .filter_map(|triangle| presentation_triangle_plane(triangle.vertices))
        .collect::<Vec<_>>();
    let mut candidate_planes = BTreeMap::<EntityRef, Vec<([f64; 3], f64)>>::new();
    for triangle in &candidate.triangles {
        if let Some(plane) = presentation_triangle_plane(triangle.vertices) {
            candidate_planes
                .entry(triangle.source_face)
                .or_default()
                .push(plane);
        }
    }
    let distance_tolerance = precision
        .linear_agreement
        .max(precision.modeling_resolution)
        .max(1.0e-9)
        * 64.0;
    candidate_planes
        .into_iter()
        .filter_map(|(face, planes)| {
            let lies_on_source_surface = planes.iter().all(|(normal, offset)| {
                source_planes.iter().any(|(source_normal, source_offset)| {
                    let dot = normal[0].mul_add(
                        source_normal[0],
                        normal[1].mul_add(source_normal[1], normal[2] * source_normal[2]),
                    );
                    dot >= 1.0 - 1.0e-10 && (offset - source_offset).abs() <= distance_tolerance
                })
            });
            (!lies_on_source_surface).then_some(face)
        })
        .collect()
}

fn presentation_triangle_plane(vertices: [Point3; 3]) -> Option<([f64; 3], f64)> {
    let first = [
        vertices[1].x - vertices[0].x,
        vertices[1].y - vertices[0].y,
        vertices[1].z - vertices[0].z,
    ];
    let second = [
        vertices[2].x - vertices[0].x,
        vertices[2].y - vertices[0].y,
        vertices[2].z - vertices[0].z,
    ];
    let mut normal = [
        first[1].mul_add(second[2], -first[2] * second[1]),
        first[2].mul_add(second[0], -first[0] * second[2]),
        first[0].mul_add(second[1], -first[1] * second[0]),
    ];
    let length = normal[0]
        .mul_add(
            normal[0],
            normal[1].mul_add(normal[1], normal[2] * normal[2]),
        )
        .sqrt();
    if !length.is_finite() || length <= f64::EPSILON {
        return None;
    }
    for component in &mut normal {
        *component /= length;
    }
    let first_significant = normal
        .iter()
        .copied()
        .find(|component| component.abs() > 1.0e-12)
        .unwrap_or(1.0);
    if first_significant < 0.0 {
        for component in &mut normal {
            *component = -*component;
        }
    }
    let offset = normal[0].mul_add(
        vertices[0].x,
        normal[1].mul_add(vertices[0].y, normal[2] * vertices[0].z),
    );
    offset.is_finite().then_some((normal, offset))
}

fn preview_planar_profile_regions(
    frame: PlanarFrame3,
    profile: &PlanarProfile2,
) -> Option<Vec<viewport::FeaturePreviewRegion>> {
    let regions = profile
        .regions
        .iter()
        .map(|region| {
            Some(viewport::FeaturePreviewRegion::new(
                preview_planar_loop(frame, &region.outer)?,
                region
                    .holes
                    .iter()
                    .map(|profile_loop| preview_planar_loop(frame, profile_loop))
                    .collect::<Option<Vec<_>>>()?,
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    (!regions.is_empty()).then_some(regions)
}

fn preview_planar_loop(frame: PlanarFrame3, profile_loop: &PlanarLoop2) -> Option<Vec<Point3>> {
    Some(
        sample_planar_loop(profile_loop)?
            .into_iter()
            .map(|point| frame_point(frame, point))
            .collect(),
    )
}

fn sample_planar_loop(profile_loop: &PlanarLoop2) -> Option<Vec<ProtocolPoint2>> {
    const SEGMENTS_PER_REVOLUTION: usize = 96;
    const MIN_ARC_SEGMENTS: usize = 2;

    let mut sampled = Vec::new();
    for curve in &profile_loop.curves {
        match *curve {
            PlanarCurve2::Line { start, .. } => sampled.push(start),
            PlanarCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            } => {
                let radius = (start.x - center.x).hypot(start.y - center.y);
                let end_radius = (end.x - center.x).hypot(end.y - center.y);
                let scale = radius.max(end_radius).max(1.0);
                if !radius.is_finite()
                    || radius <= f64::EPSILON
                    || (radius - end_radius).abs() > 1.0e-9 * scale
                {
                    return None;
                }
                let start_angle = (start.y - center.y).atan2(start.x - center.x);
                let end_angle = (end.y - center.y).atan2(end.x - center.x);
                let (signed_sweep, magnitude) = match direction {
                    ArcDirection::CounterClockwise => {
                        let sweep = (end_angle - start_angle).rem_euclid(std::f64::consts::TAU);
                        (sweep, sweep)
                    }
                    ArcDirection::Clockwise => {
                        let sweep = (start_angle - end_angle).rem_euclid(std::f64::consts::TAU);
                        (-sweep, sweep)
                    }
                };
                if !magnitude.is_finite() || magnitude <= f64::EPSILON {
                    return None;
                }
                let segments =
                    ((magnitude / std::f64::consts::TAU * SEGMENTS_PER_REVOLUTION as f64).ceil()
                        as usize)
                        .max(MIN_ARC_SEGMENTS);
                for step in 0..segments {
                    let progress = step as f64 / segments as f64;
                    let angle = signed_sweep.mul_add(progress, start_angle);
                    sampled.push(ProtocolPoint2::new(
                        radius.mul_add(angle.cos(), center.x),
                        radius.mul_add(angle.sin(), center.y),
                    ));
                }
            }
            PlanarCurve2::Circle {
                center,
                radius,
                direction,
            } => {
                if !radius.is_finite() || radius <= f64::EPSILON {
                    return None;
                }
                let sign = match direction {
                    ArcDirection::CounterClockwise => 1.0,
                    ArcDirection::Clockwise => -1.0,
                };
                for step in 0..SEGMENTS_PER_REVOLUTION {
                    let angle =
                        sign * std::f64::consts::TAU * step as f64 / SEGMENTS_PER_REVOLUTION as f64;
                    sampled.push(ProtocolPoint2::new(
                        radius.mul_add(angle.cos(), center.x),
                        radius.mul_add(angle.sin(), center.y),
                    ));
                }
            }
        }
    }
    if sampled.len() < 3 || sampled.iter().any(|point| !point.is_finite()) {
        return None;
    }
    Some(sampled)
}

fn profile_region_anchor(
    outer: &[ProtocolPoint2],
    holes: &[Vec<ProtocolPoint2>],
) -> Option<ProtocolPoint2> {
    if outer.len() < 3 {
        return None;
    }
    let centroid = ProtocolPoint2::new(
        outer.iter().map(|point| point.x).sum::<f64>() / outer.len() as f64,
        outer.iter().map(|point| point.y).sum::<f64>() / outer.len() as f64,
    );
    let valid = |candidate: ProtocolPoint2| {
        point_in_planar_polygon(candidate, outer)
            && !holes
                .iter()
                .any(|hole| point_in_planar_polygon(candidate, hole))
    };
    if valid(centroid) {
        return Some(centroid);
    }
    for index in 0..outer.len() {
        let next = outer[(index + 1) % outer.len()];
        let midpoint = ProtocolPoint2::new(
            (outer[index].x + next.x) * 0.5,
            (outer[index].y + next.y) * 0.5,
        );
        for blend in [0.08, 0.2, 0.4, 0.65] {
            let candidate = ProtocolPoint2::new(
                midpoint.x.mul_add(1.0 - blend, centroid.x * blend),
                midpoint.y.mul_add(1.0 - blend, centroid.y * blend),
            );
            if valid(candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

fn point_in_planar_polygon(point: ProtocolPoint2, polygon: &[ProtocolPoint2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut previous = polygon.len() - 1;
    for current in 0..polygon.len() {
        let a = polygon[current];
        let b = polygon[previous];
        if (a.y > point.y) != (b.y > point.y)
            && point.x < (b.x - a.x).mul_add((point.y - a.y) / (b.y - a.y), a.x)
        {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

fn workbench_sketch_has_overlay_geometry(sketch: &WorkbenchSketch) -> bool {
    // While an existing sketch is being edited, the canvas adapter is the
    // current Browser/overlay projection and `portable_payload` is still the
    // last finished document revision. Falling through to that stale payload
    // would resurrect geometry after Delete until Finish publishes it.
    if !sketch.finished {
        return !sketch.entities.is_empty();
    }
    if !sketch.entities.is_empty() {
        return true;
    }
    let Some(payload) = &sketch.portable_payload else {
        return false;
    };
    payload.authoring().map_or_else(
        || !payload.profile.regions.is_empty(),
        |authoring| authoring.active_entities().any(|entity| entity.visible),
    )
}

fn preview_authoring_curve(frame: PlanarFrame3, curve: AuthoringCurve2) -> Option<Vec<Point3>> {
    let subdivisions = match curve {
        AuthoringCurve2::Line { .. } => 1,
        AuthoringCurve2::CircularArc { .. } => 32,
        AuthoringCurve2::Circle { .. } => 64,
    };
    let sample_count = if curve.is_periodic() {
        subdivisions
    } else {
        subdivisions + 1
    };
    let points = (0..sample_count)
        .map(|step| {
            let parameter = step as f64 / subdivisions as f64;
            curve
                .evaluate(parameter)
                .ok()
                .map(|point| frame_point(frame, ProtocolPoint2::new(point.u, point.v)))
        })
        .collect::<Option<Vec<_>>>()?;
    (points.len() >= 2).then_some(points)
}

/// Automatically compiles the only bounded arrangement cell. Multi-cell
/// sketches intentionally require a future explicit region-selection session;
/// choosing all cells here would silently fill intended holes.
fn compile_single_authoring_region(authoring: &SketchDefinition) -> Option<PlanarProfile2> {
    let precision = PrecisionPolicy::default();
    let inputs = authoring.arrangement_inputs().ok()?;
    let arrangement = build_arrangement(&inputs, &precision, ArrangementLimits::default());
    let [cell] = arrangement.cells.as_slice() else {
        return None;
    };
    compile_selected_profile(
        &arrangement,
        std::slice::from_ref(&cell.signature),
        &precision,
    )
    .ok()
    .map(|compiled| compiled.profile)
}

fn authoring_region_signatures_for_profile(
    authoring: &SketchDefinition,
    profile: &PlanarProfile2,
) -> Option<Vec<RegionSignature>> {
    let precision = PrecisionPolicy::default();
    let inputs = authoring.arrangement_inputs().ok()?;
    let arrangement = build_arrangement(&inputs, &precision, ArrangementLimits::default());
    let selected = arrangement
        .cells
        .iter()
        .filter_map(|cell| {
            let sample = arrangement_cell_interior_sample(cell, &arrangement, &precision)?;
            planar_profile_contains_authoring_point(profile, sample, &precision)
                .then(|| cell.signature.clone())
        })
        .collect::<Vec<_>>();
    if selected.is_empty() {
        return None;
    }
    // Prove the chosen cells still form a valid exact profile before storing
    // their identities. The regenerated profile, not the current cache, is
    // what future rebuilds will submit to the kernel.
    compile_selected_profile(&arrangement, &selected, &precision)
        .ok()
        .map(|compiled| compiled.selected_regions)
}

fn arrangement_cell_interior_sample(
    cell: &ArrangementCell,
    arrangement: &SketchArrangement,
    precision: &PrecisionPolicy,
) -> Option<AuthoringPoint2> {
    let bounds = cell
        .outer
        .curves
        .iter()
        .map(|curve| curve.bounds())
        .reduce(|mut first, second| {
            first.include(second.min);
            first.include(second.max);
            first
        })?;
    let scale = (bounds.max.u - bounds.min.u)
        .hypot(bounds.max.v - bounds.min.v)
        .max(precision.min_feature_size * 8.0)
        .max(1.0);
    for curve in &cell.outer.curves {
        for parameter in [0.125, 0.375, 0.625, 0.875] {
            let point = curve.evaluate(parameter).ok()?;
            let inward = curve.tangent(parameter).ok()?.left_normal().normalized()?;
            for fraction in [1.0e-6, 1.0e-5, 1.0e-4, 1.0e-3, 1.0e-2, 5.0e-2] {
                let offset = (scale * fraction).max(precision.min_feature_size * 4.0);
                let candidate = point + inward * offset;
                if arrangement
                    .cell_at_point(candidate, precision)
                    .is_some_and(|resolved| resolved.signature == cell.signature)
                {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn planar_profile_contains_authoring_point(
    profile: &PlanarProfile2,
    point: AuthoringPoint2,
    precision: &PrecisionPolicy,
) -> bool {
    profile.regions.iter().any(|region| {
        planar_loop_contains_authoring_point(&region.outer, point, precision)
            && !region
                .holes
                .iter()
                .any(|hole| planar_loop_contains_authoring_point(hole, point, precision))
    })
}

fn planar_loop_contains_authoring_point(
    profile_loop: &PlanarLoop2,
    point: AuthoringPoint2,
    precision: &PrecisionPolicy,
) -> bool {
    let curves = profile_loop
        .curves
        .iter()
        .map(protocol_curve_as_authoring)
        .collect::<Option<Vec<_>>>();
    let Some(curves) = curves else {
        return false;
    };
    let max_u = curves
        .iter()
        .map(|curve| curve.bounds().max.u)
        .fold(point.u + 1.0, f64::max);
    let ray = AuthoringCurve2::Line {
        start: point,
        end: AuthoringPoint2::new(max_u + (max_u - point.u).abs().max(1.0) * 2.0, point.v),
    };
    let mut parameters = Vec::new();
    for curve in curves {
        if let CurveIntersections::Points { intersections } =
            intersect_curves(ray, curve, precision)
        {
            parameters.extend(
                intersections
                    .into_iter()
                    .filter(|intersection| {
                        intersection.first_parameter > precision.parameter_resolution
                    })
                    .map(|intersection| intersection.first_parameter),
            );
        }
    }
    parameters.sort_by(f64::total_cmp);
    parameters.dedup_by(|second, first| {
        (*first - *second).abs() <= precision.parameter_resolution.max(f64::EPSILON * 64.0)
    });
    parameters.len() % 2 == 1
}

fn protocol_curve_as_authoring(curve: &PlanarCurve2) -> Option<AuthoringCurve2> {
    Some(match *curve {
        PlanarCurve2::Line { start, end } => AuthoringCurve2::Line {
            start: start.into(),
            end: end.into(),
        },
        PlanarCurve2::CircularArc {
            center,
            start,
            end,
            direction,
        } => AuthoringCurve2::CircularArc {
            center: center.into(),
            start: start.into(),
            end: end.into(),
            direction: match direction {
                ArcDirection::CounterClockwise => AuthoringCurveDirection::CounterClockwise,
                ArcDirection::Clockwise => AuthoringCurveDirection::Clockwise,
            },
        },
        PlanarCurve2::Circle {
            center,
            radius,
            direction,
        } if radius.is_finite() && radius > 0.0 => AuthoringCurve2::Circle {
            center: center.into(),
            radius,
            direction: match direction {
                ArcDirection::CounterClockwise => AuthoringCurveDirection::CounterClockwise,
                ArcDirection::Clockwise => AuthoringCurveDirection::Clockwise,
            },
        },
        PlanarCurve2::Circle { .. } => return None,
    })
}

const fn protocol_sketch_point(point: SketchPoint) -> ProtocolPoint2 {
    ProtocolPoint2::new(point.u, point.v)
}

const fn protocol_arc_direction(direction: SketchCurveDirection) -> ArcDirection {
    match direction {
        SketchCurveDirection::CounterClockwise => ArcDirection::CounterClockwise,
        SketchCurveDirection::Clockwise => ArcDirection::Clockwise,
    }
}

fn build_planar_profile_extrusion_command(
    frame: PlanarFrame3,
    profile: PlanarProfile2,
    target_face: Option<EntityRef>,
    distance: f64,
    mode: ExtrusionMode,
) -> Option<KernelCommand> {
    match (target_face, mode.feature_operation()) {
        (Some(target_face), Some(operation)) => {
            let reverse_frame = matches!(
                (mode, distance.is_sign_negative()),
                (ExtrusionMode::Add, true) | (ExtrusionMode::Cut, false)
            );
            let (frame, profile) = if reverse_frame {
                reflect_face_extrusion_direction(frame, profile)
            } else {
                (frame, profile)
            };
            Some(KernelCommand::ExtrudeFacePlanarProfile {
                target_face,
                frame,
                profile,
                distance: distance.abs(),
                operation,
            })
        }
        (None, None) => Some(KernelCommand::ExtrudePlanarProfile {
            frame,
            profile,
            distance: distance.abs(),
        }),
        _ => None,
    }
}

/// Reverses the frame normal while reflecting profile coordinates so the
/// physical sketch wires remain exactly where the user drew them. This keeps
/// the protocol's positive depth invariant while making Boolean operation and
/// signed arrow direction independent.
fn reflect_face_extrusion_direction(
    mut frame: PlanarFrame3,
    mut profile: PlanarProfile2,
) -> (PlanarFrame3, PlanarProfile2) {
    frame.v = Vector3::new(-frame.v.x, -frame.v.y, -frame.v.z);
    for curve in profile
        .regions
        .iter_mut()
        .flat_map(|region| std::iter::once(&mut region.outer).chain(&mut region.holes))
        .flat_map(|profile_loop| &mut profile_loop.curves)
    {
        *curve = match *curve {
            PlanarCurve2::Line { start, end } => PlanarCurve2::Line {
                start: reflect_profile_point(start),
                end: reflect_profile_point(end),
            },
            PlanarCurve2::CircularArc {
                center,
                start,
                end,
                direction,
            } => PlanarCurve2::CircularArc {
                center: reflect_profile_point(center),
                start: reflect_profile_point(start),
                end: reflect_profile_point(end),
                direction: reverse_arc_direction(direction),
            },
            PlanarCurve2::Circle {
                center,
                radius,
                direction,
            } => PlanarCurve2::Circle {
                center: reflect_profile_point(center),
                radius,
                direction: reverse_arc_direction(direction),
            },
        };
    }
    (frame, profile)
}

const fn reflect_profile_point(point: ProtocolPoint2) -> ProtocolPoint2 {
    ProtocolPoint2::new(point.x, -point.y)
}

const fn reverse_arc_direction(direction: ArcDirection) -> ArcDirection {
    match direction {
        ArcDirection::CounterClockwise => ArcDirection::Clockwise,
        ArcDirection::Clockwise => ArcDirection::CounterClockwise,
    }
}

#[cfg(test)]
fn classify_sketch_extrusion_vertices(
    vertices: &[SketchPoint],
    _winding: artificer_geometry::ProfileWinding,
) -> SketchExtrusionEligibility {
    if vertices.len() > MAX_EXTRUSION_PROFILE_VERTICES {
        return SketchExtrusionEligibility::TooManyVertices {
            count: vertices.len(),
        };
    }
    if vertices.len() < 3 {
        return SketchExtrusionEligibility::UnsupportedProfile;
    }

    if vertices.iter().any(|point| !point.is_finite()) {
        return SketchExtrusionEligibility::NumericallyIndeterminate;
    }
    SketchExtrusionEligibility::Ready
}

/// Applies the workbench's conservative face-domain preflight to the exact
/// union chosen from the analytic arrangement. The native kernel repeats all
/// topology and containment checks authoritatively at confirmation time.
fn classify_selected_planar_profile(
    profile: &PlanarProfile2,
    support: &SketchSupport,
) -> SketchExtrusionEligibility {
    if profile.regions.is_empty() {
        return SketchExtrusionEligibility::SketchNotFinished;
    }
    if profile.regions.len() > MAX_PLANAR_PROFILE_REGIONS {
        return SketchExtrusionEligibility::TooManyRegions {
            count: profile.regions.len(),
        };
    }
    if profile.loop_count() > MAX_PLANAR_PROFILE_LOOPS {
        return SketchExtrusionEligibility::TooManyLoops {
            count: profile.loop_count(),
        };
    }
    if profile.curve_count() > MAX_PLANAR_PROFILE_CURVES {
        return SketchExtrusionEligibility::TooManyCurves {
            count: profile.curve_count(),
        };
    }

    let SketchSupport::PlanarFace {
        boundary,
        inner_boundaries,
        ..
    } = support
    else {
        return SketchExtrusionEligibility::Ready;
    };
    if let [region] = profile.regions.as_slice()
        && region.holes.is_empty()
        && let [PlanarCurve2::Circle { center, radius, .. }] = region.outer.curves.as_slice()
    {
        let center = SketchPoint::new(center.x, center.y);
        let rim = SketchPoint::new(center.u + radius, center.v);
        return classify_face_circle_domain(center, rim, boundary, inner_boundaries);
    }
    let all_unholed_linear = profile.regions.iter().all(|region| {
        region.holes.is_empty()
            && region
                .outer
                .curves
                .iter()
                .all(|curve| matches!(curve, PlanarCurve2::Line { .. }))
    });
    if all_unholed_linear {
        for region in &profile.regions {
            let vertices = region
                .outer
                .curves
                .iter()
                .filter_map(|curve| match curve {
                    PlanarCurve2::Line { start, .. } => Some(SketchPoint::new(start.x, start.y)),
                    PlanarCurve2::CircularArc { .. } | PlanarCurve2::Circle { .. } => None,
                })
                .collect::<Vec<_>>();
            let eligibility = classify_face_profile_domain(&vertices, boundary, inner_boundaries);
            if eligibility != SketchExtrusionEligibility::Ready {
                return eligibility;
            }
        }
    }
    // The UI has no sound, cheaper proof for mixed curves, holes, or multiple
    // regions. These are valid unified-kernel inputs and must reach kernel
    // authority instead of being rejected by obsolete presentation gates.
    SketchExtrusionEligibility::Ready
}

fn classify_face_profile_domain(
    vertices: &[SketchPoint],
    boundary: &[ProtocolPoint2],
    inner_boundaries: &[Vec<ProtocolPoint2>],
) -> SketchExtrusionEligibility {
    if boundary.len() < 3
        || boundary
            .iter()
            .chain(inner_boundaries.iter().flatten())
            .any(|point| !point.is_finite())
        || inner_boundaries.iter().any(|inner| inner.len() < 3)
    {
        return SketchExtrusionEligibility::FaceRectangleRequired;
    }
    const MARGIN: f64 = 1.0e-5;
    let profile = vertices
        .iter()
        .map(|point| ProtocolPoint2::new(point.u, point.v))
        .collect::<Vec<_>>();
    let intersects_material = profile
        .iter()
        .copied()
        .any(|point| point_in_face_material_2d(point, boundary, inner_boundaries))
        || (0..profile.len()).any(|index| {
            let start = profile[index];
            let end = profile[(index + 1) % profile.len()];
            point_in_face_material_2d(
                ProtocolPoint2::new((start.x + end.x) * 0.5, (start.y + end.y) * 0.5),
                boundary,
                inner_boundaries,
            )
        });
    if profile.iter().any(|point| {
        !point_strictly_inside_loop(*point, boundary, MARGIN)
            || inner_boundaries.iter().any(|inner| {
                point_in_loop(*point, inner) || point_loop_distance(*point, inner) <= MARGIN
            })
    }) {
        return if intersects_material {
            SketchExtrusionEligibility::BooleanUnionRequired
        } else {
            SketchExtrusionEligibility::ProfileOutsideSupport
        };
    }
    for index in 0..profile.len() {
        let edge = [profile[index], profile[(index + 1) % profile.len()]];
        if segment_loop_distance(edge, boundary) <= MARGIN
            || inner_boundaries
                .iter()
                .any(|inner| segment_loop_distance(edge, inner) <= MARGIN)
        {
            return if intersects_material {
                SketchExtrusionEligibility::BooleanUnionRequired
            } else {
                SketchExtrusionEligibility::ProfileOutsideSupport
            };
        }
    }
    if inner_boundaries.iter().any(|inner| {
        inner
            .first()
            .is_some_and(|point| point_in_loop(*point, &profile))
    }) {
        return if intersects_material {
            SketchExtrusionEligibility::BooleanUnionRequired
        } else {
            SketchExtrusionEligibility::ProfileOutsideSupport
        };
    }
    SketchExtrusionEligibility::Ready
}

fn point_in_face_material_2d(
    point: ProtocolPoint2,
    boundary: &[ProtocolPoint2],
    inner_boundaries: &[Vec<ProtocolPoint2>],
) -> bool {
    point_in_loop(point, boundary)
        && inner_boundaries
            .iter()
            .all(|inner| !point_in_loop(point, inner))
}

fn classify_face_circle_domain(
    center: SketchPoint,
    rim: SketchPoint,
    boundary: &[ProtocolPoint2],
    inner_boundaries: &[Vec<ProtocolPoint2>],
) -> SketchExtrusionEligibility {
    if boundary.len() < 3
        || boundary
            .iter()
            .chain(inner_boundaries.iter().flatten())
            .any(|point| !point.is_finite())
        || inner_boundaries.iter().any(|inner| inner.len() < 3)
    {
        return SketchExtrusionEligibility::FaceRectangleRequired;
    }
    const MARGIN: f64 = 1.0e-5;
    let center = ProtocolPoint2::new(center.u, center.v);
    let radius = rim
        .distance_squared(SketchPoint::new(center.x, center.y))
        .sqrt();
    if !radius.is_finite()
        || radius <= MARGIN
        || !point_in_loop(center, boundary)
        || point_loop_distance(center, boundary) <= radius + MARGIN
        || inner_boundaries.iter().any(|inner| {
            point_in_loop(center, inner) || point_loop_distance(center, inner) <= radius + MARGIN
        })
    {
        return SketchExtrusionEligibility::ProfileOutsideSupport;
    }
    SketchExtrusionEligibility::Ready
}

fn point_strictly_inside_loop(
    point: ProtocolPoint2,
    loop_points: &[ProtocolPoint2],
    margin: f64,
) -> bool {
    point_in_loop(point, loop_points) && point_loop_distance(point, loop_points) > margin
}

fn point_in_loop(point: ProtocolPoint2, loop_points: &[ProtocolPoint2]) -> bool {
    let mut inside = false;
    for index in 0..loop_points.len() {
        let start = loop_points[index];
        let end = loop_points[(index + 1) % loop_points.len()];
        if (start.y > point.y) != (end.y > point.y) {
            let crossing = (end.x - start.x) * (point.y - start.y) / (end.y - start.y) + start.x;
            if point.x < crossing {
                inside = !inside;
            }
        }
    }
    inside
}

fn point_loop_distance(point: ProtocolPoint2, loop_points: &[ProtocolPoint2]) -> f64 {
    (0..loop_points.len())
        .map(|index| {
            point_segment_distance_2d(
                point,
                loop_points[index],
                loop_points[(index + 1) % loop_points.len()],
            )
        })
        .fold(f64::INFINITY, f64::min)
}

fn segment_loop_distance(segment: [ProtocolPoint2; 2], loop_points: &[ProtocolPoint2]) -> f64 {
    (0..loop_points.len())
        .map(|index| {
            segment_distance_2d(
                segment,
                [
                    loop_points[index],
                    loop_points[(index + 1) % loop_points.len()],
                ],
            )
        })
        .fold(f64::INFINITY, f64::min)
}

fn segment_distance_2d(first: [ProtocolPoint2; 2], second: [ProtocolPoint2; 2]) -> f64 {
    let orientations = [
        orient2d(
            GeometryPoint2::new(first[0].x, first[0].y),
            GeometryPoint2::new(first[1].x, first[1].y),
            GeometryPoint2::new(second[0].x, second[0].y),
        ),
        orient2d(
            GeometryPoint2::new(first[0].x, first[0].y),
            GeometryPoint2::new(first[1].x, first[1].y),
            GeometryPoint2::new(second[1].x, second[1].y),
        ),
        orient2d(
            GeometryPoint2::new(second[0].x, second[0].y),
            GeometryPoint2::new(second[1].x, second[1].y),
            GeometryPoint2::new(first[0].x, first[0].y),
        ),
        orient2d(
            GeometryPoint2::new(second[0].x, second[0].y),
            GeometryPoint2::new(second[1].x, second[1].y),
            GeometryPoint2::new(first[1].x, first[1].y),
        ),
    ];
    let opposite = |left: Orientation2, right: Orientation2| {
        matches!(
            (left, right),
            (Orientation2::Clockwise, Orientation2::CounterClockwise)
                | (Orientation2::CounterClockwise, Orientation2::Clockwise)
        )
    };
    if opposite(orientations[0], orientations[1]) && opposite(orientations[2], orientations[3]) {
        return 0.0;
    }
    [
        point_segment_distance_2d(first[0], second[0], second[1]),
        point_segment_distance_2d(first[1], second[0], second[1]),
        point_segment_distance_2d(second[0], first[0], first[1]),
        point_segment_distance_2d(second[1], first[0], first[1]),
    ]
    .into_iter()
    .fold(f64::INFINITY, f64::min)
}

fn point_segment_distance_2d(
    point: ProtocolPoint2,
    start: ProtocolPoint2,
    end: ProtocolPoint2,
) -> f64 {
    let delta = ProtocolPoint2::new(end.x - start.x, end.y - start.y);
    let length_squared = delta.x.mul_add(delta.x, delta.y * delta.y);
    if !length_squared.is_finite() || length_squared <= 0.0 {
        return f64::INFINITY;
    }
    let projection =
        ((point.x - start.x) * delta.x + (point.y - start.y) * delta.y) / length_squared;
    let parameter = projection.clamp(0.0, 1.0);
    (point.x - (start.x + parameter * delta.x)).hypot(point.y - (start.y + parameter * delta.y))
}

fn workbench_extrusion_error(
    code: KernelErrorCode,
    input_snapshot: SnapshotId,
    message: impl Into<String>,
) -> KernelError {
    KernelError {
        code,
        stage: KernelStage::Preflight,
        input_snapshot,
        message: message.into(),
        diagnostics: Vec::new(),
        details: Default::default(),
    }
}

fn extrusion_rejection_trace_payload(error: &KernelError) -> serde_json::Value {
    // Keep the rejection useful for deterministic diagnosis without turning
    // the session trace into a general dump of document/user metadata.
    const TRACEABLE_DETAIL_KEYS: &[&str] = &[
        "expected_snapshot",
        "actual_snapshot",
        "displayed_snapshot",
        "expected_body",
        "actual_body",
        "expected_revision",
        "actual_revision",
        "expected_plane",
        "actual_plane",
    ];
    let details = error
        .details
        .iter()
        .filter(|(key, _)| TRACEABLE_DETAIL_KEYS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    serde_json::json!({
        "operation": "Extrude active sketch",
        "result": "rejected",
        "error_code": error.code.to_string(),
        "stage": format!("{:?}", error.stage),
        "input_snapshot": error.input_snapshot.to_string(),
        "message": &error.message,
        "details": details,
        "diagnostic_count": error.diagnostics.len(),
        "diagnostics": error.diagnostics.iter().map(|diagnostic| {
            serde_json::json!({
                "code": diagnostic.code.to_string(),
                "path": &diagnostic.path
            })
        }).collect::<Vec<_>>()
    })
}

const fn sketch_plane_frame(plane: SketchPlane) -> PlanarFrame3 {
    let (axis_u, axis_v) = match plane {
        SketchPlane::XY => (Vector3::new(1.0, 0.0, 0.0), Vector3::new(0.0, 1.0, 0.0)),
        SketchPlane::YZ => (Vector3::new(0.0, 1.0, 0.0), Vector3::new(0.0, 0.0, 1.0)),
        SketchPlane::XZ => (Vector3::new(1.0, 0.0, 0.0), Vector3::new(0.0, 0.0, 1.0)),
    };
    PlanarFrame3::new(Point3::new(0.0, 0.0, 0.0), axis_u, axis_v)
}

fn sketch_plane_for_frame(frame: PlanarFrame3) -> SketchPlane {
    let normal = Vector3::new(
        frame.u.y * frame.v.z - frame.u.z * frame.v.y,
        frame.u.z * frame.v.x - frame.u.x * frame.v.z,
        frame.u.x * frame.v.y - frame.u.y * frame.v.x,
    );
    let components = [normal.x.abs(), normal.y.abs(), normal.z.abs()];
    let axis = components
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(right.1))
        .map_or(2, |(axis, _)| axis);
    match axis {
        0 => SketchPlane::YZ,
        1 => SketchPlane::XZ,
        _ => SketchPlane::XY,
    }
}

fn face_support_focus(support: &PlanarFaceSupport) -> Option<(Point3, f64)> {
    let mut points = support
        .boundary
        .iter()
        .copied()
        .map(|point| frame_point(support.frame, point))
        .filter(|point| point.is_finite());
    let first = points.next()?;
    let mut min = first;
    let mut max = first;
    let mut collected = vec![first];
    for point in points {
        min.x = min.x.min(point.x);
        min.y = min.y.min(point.y);
        min.z = min.z.min(point.z);
        max.x = max.x.max(point.x);
        max.y = max.y.max(point.y);
        max.z = max.z.max(point.z);
        collected.push(point);
    }
    let focus = bounds_center(Aabb3::new(min, max));
    let radius = collected
        .into_iter()
        .map(|point| {
            (point.x - focus.x)
                .hypot(point.y - focus.y)
                .hypot(point.z - focus.z)
        })
        .fold(0.0_f64, f64::max)
        * 1.18;
    (radius.is_finite() && radius > f64::EPSILON).then_some((focus, radius))
}

fn union_aabb(left: Aabb3, right: Aabb3) -> Aabb3 {
    Aabb3::new(
        Point3::new(
            left.min.x.min(right.min.x),
            left.min.y.min(right.min.y),
            left.min.z.min(right.min.z),
        ),
        Point3::new(
            left.max.x.max(right.max.x),
            left.max.y.max(right.max.y),
            left.max.z.max(right.max.z),
        ),
    )
}

fn presented_planar_frame(
    frame: PlanarFrame3,
    pivot: Point3,
    transform: DisplayTransform,
    phase: f64,
) -> Option<PlanarFrame3> {
    let origin = transform.present_point(frame.origin, pivot, phase);
    let u_end = transform.present_point(
        Point3::new(
            frame.origin.x + frame.u.x,
            frame.origin.y + frame.u.y,
            frame.origin.z + frame.u.z,
        ),
        pivot,
        phase,
    );
    let v_end = transform.present_point(
        Point3::new(
            frame.origin.x + frame.v.x,
            frame.origin.y + frame.v.y,
            frame.origin.z + frame.v.z,
        ),
        pivot,
        phase,
    );
    let presented = PlanarFrame3::new(
        origin,
        Vector3::new(u_end.x - origin.x, u_end.y - origin.y, u_end.z - origin.z),
        Vector3::new(v_end.x - origin.x, v_end.y - origin.y, v_end.z - origin.z),
    );
    (presented.origin.is_finite() && presented.u.is_finite() && presented.v.is_finite())
        .then_some(presented)
}

fn vector_length(vector: Vector3) -> Option<f64> {
    let length = vector
        .x
        .mul_add(vector.x, vector.y.mul_add(vector.y, vector.z * vector.z))
        .sqrt();
    (length.is_finite() && length > f64::EPSILON).then_some(length)
}

#[derive(Clone, Copy, Debug)]
struct FaceSketchProjection {
    origin: Point3,
    u: Vector3,
    v: Vector3,
    normal: Vector3,
}

impl FaceSketchProjection {
    fn from_frame(frame: PlanarFrame3) -> Option<Self> {
        let u = normalized_vector(frame.u)?;
        let normal = normalized_vector(cross_vector(u, frame.v))?;
        let v = normalized_vector(cross_vector(normal, u))?;
        Some(Self {
            origin: frame.origin,
            u,
            v,
            normal,
        })
    }

    fn project(self, point: Point3) -> Option<(SketchPoint, f64)> {
        let offset = Vector3::new(
            point.x - self.origin.x,
            point.y - self.origin.y,
            point.z - self.origin.z,
        );
        let projected = SketchPoint::new(dot_vector(offset, self.u), dot_vector(offset, self.v));
        let depth = dot_vector(offset, self.normal);
        (projected.is_finite() && depth.is_finite()).then_some((projected, depth))
    }
}

fn project_face_sketch_context(
    scene: &DebugScene,
    support: &PlanarFaceSupport,
) -> Option<FaceSketchDisplayContext> {
    let projection = FaceSketchProjection::from_frame(support.frame)?;
    let mut projected_triangles = scene
        .triangles
        .iter()
        .filter_map(|triangle| {
            let projected = triangle
                .vertices
                .map(|point| projection.project(point))
                .into_iter()
                .collect::<Option<Vec<_>>>()?;
            let vertices: [SketchPoint; 3] = projected
                .iter()
                .map(|(point, _)| *point)
                .collect::<Vec<_>>()
                .try_into()
                .ok()?;
            let signed_area = sketch_triangle_signed_area(vertices);
            if !signed_area.is_finite() || signed_area <= 0.0 {
                return None;
            }
            let depth = projected.iter().map(|(_, depth)| depth).sum::<f64>() / 3.0;
            let vertex_depths: [f64; 3] = projected
                .iter()
                .map(|(_, depth)| *depth)
                .collect::<Vec<_>>()
                .try_into()
                .ok()?;
            Some((depth, vertex_depths, SketchContextTriangle::new(vertices)))
        })
        .collect::<Vec<_>>();
    projected_triangles.sort_by(|left, right| left.0.total_cmp(&right.0));

    let edges = scene
        .edges
        .iter()
        .filter_map(|edge| {
            let endpoints = edge.endpoints.map(|point| projection.project(point));
            let endpoints = [endpoints[0]?, endpoints[1]?];
            projected_triangles
                .iter()
                .any(|(_, depths, triangle)| {
                    projected_edge_matches_triangle(endpoints, triangle.vertices, *depths)
                })
                .then_some(SketchContextEdge::new([endpoints[0].0, endpoints[1].0]))
        })
        .collect::<Vec<_>>();
    let boundary = support
        .boundary
        .iter()
        .map(|point| SketchPoint::new(point.x, point.y))
        .collect::<Vec<_>>();
    let inner_boundaries = support
        .inner_boundaries
        .iter()
        .map(|boundary| {
            boundary
                .iter()
                .map(|point| SketchPoint::new(point.x, point.y))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    // The support publishes its boundary in the same frame the sketch draws in,
    // so the analytic curves need no projection — only a change of vocabulary.
    let snap_curves = support
        .boundary_curves
        .iter()
        .chain(support.inner_boundary_curves.iter().flatten())
        .map(sketch_context_curve)
        .collect::<Vec<_>>();

    (!boundary.is_empty()).then_some(FaceSketchDisplayContext {
        fit_key: SketchContextFitKey::new(
            *support.support_digest.as_bytes(),
            support.face.entity.0,
        ),
        axis_labels: [
            dominant_axis_label(support.frame.u).unwrap_or("U"),
            dominant_axis_label(support.frame.v).unwrap_or("V"),
        ],
        triangles: projected_triangles
            .into_iter()
            .map(|(_, _, triangle)| triangle)
            .collect(),
        edges,
        boundary,
        inner_boundaries,
        snap_curves,
    })
}

/// Restates one exact face-boundary curve in the sketch canvas's vocabulary.
///
/// The face frame and the sketch `(u, v)` frame are the same frame, so this is
/// a rename rather than a transform and stays exact.
fn sketch_context_curve(curve: &FaceBoundaryCurve2) -> SketchContextCurve {
    match *curve {
        FaceBoundaryCurve2::Segment { endpoints } => SketchContextCurve::Segment {
            start: SketchPoint::new(endpoints[0].x, endpoints[0].y),
            end: SketchPoint::new(endpoints[1].x, endpoints[1].y),
        },
        FaceBoundaryCurve2::Arc {
            center,
            u,
            v,
            radius,
            start,
            end,
        } => SketchContextCurve::Arc {
            center: SketchPoint::new(center.x, center.y),
            u,
            v,
            radius,
            start,
            end,
        },
    }
}

fn projected_edge_matches_triangle(
    edge: [(SketchPoint, f64); 2],
    vertices: [SketchPoint; 3],
    depths: [f64; 3],
) -> bool {
    const TOLERANCE: f64 = 1.0e-8;
    [(0, 1), (1, 2), (2, 0)].into_iter().any(|(first, second)| {
        projected_vertex_matches(edge[0], vertices[first], depths[first], TOLERANCE)
            && projected_vertex_matches(edge[1], vertices[second], depths[second], TOLERANCE)
            || projected_vertex_matches(edge[0], vertices[second], depths[second], TOLERANCE)
                && projected_vertex_matches(edge[1], vertices[first], depths[first], TOLERANCE)
    })
}

fn projected_vertex_matches(
    projected: (SketchPoint, f64),
    point: SketchPoint,
    depth: f64,
    tolerance: f64,
) -> bool {
    projected.0.distance_squared(point) <= tolerance * tolerance
        && (projected.1 - depth).abs() <= tolerance
}

fn sketch_triangle_signed_area(vertices: [SketchPoint; 3]) -> f64 {
    let [first, second, third] = vertices;
    (second.u - first.u) * (third.v - first.v) - (second.v - first.v) * (third.u - first.u)
}

fn cross_vector(left: Vector3, right: Vector3) -> Vector3 {
    Vector3::new(
        left.y * right.z - left.z * right.y,
        left.z * right.x - left.x * right.z,
        left.x * right.y - left.y * right.x,
    )
}

fn dot_vector(left: Vector3, right: Vector3) -> f64 {
    left.x * right.x + left.y * right.y + left.z * right.z
}

fn normalized_vector(vector: Vector3) -> Option<Vector3> {
    let length = dot_vector(vector, vector).sqrt();
    if !length.is_finite() || length <= f64::EPSILON {
        return None;
    }
    Some(Vector3::new(
        vector.x / length,
        vector.y / length,
        vector.z / length,
    ))
}

fn dominant_axis_label(vector: Vector3) -> Option<&'static str> {
    let vector = normalized_vector(vector)?;
    let components = [vector.x.abs(), vector.y.abs(), vector.z.abs()];
    let (axis, dominant) = components
        .iter()
        .copied()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(&right.1))?;
    let tolerance = 1.0e-9;
    if (1.0 - dominant).abs() > tolerance
        || components
            .iter()
            .enumerate()
            .any(|(index, component)| index != axis && *component > tolerance)
    {
        return None;
    }
    Some(match axis {
        0 => "X",
        1 => "Y",
        _ => "Z",
    })
}

fn frame_point(frame: PlanarFrame3, point: ProtocolPoint2) -> Point3 {
    Point3::new(
        frame.origin.x + frame.u.x * point.x + frame.v.x * point.y,
        frame.origin.y + frame.u.y * point.x + frame.v.y * point.y,
        frame.origin.z + frame.u.z * point.x + frame.v.z * point.y,
    )
}

fn frame_normal(frame: PlanarFrame3) -> Option<Vector3> {
    let normal = Vector3::new(
        frame.u.y * frame.v.z - frame.u.z * frame.v.y,
        frame.u.z * frame.v.x - frame.u.x * frame.v.z,
        frame.u.x * frame.v.y - frame.u.y * frame.v.x,
    );
    let length = (normal.x * normal.x + normal.y * normal.y + normal.z * normal.z).sqrt();
    if !length.is_finite() || length <= f64::EPSILON {
        return None;
    }
    Some(Vector3::new(
        normal.x / length,
        normal.y / length,
        normal.z / length,
    ))
}

fn centered_plane_frame(support: &PlanarFaceSupport) -> Result<(PlanarFrame3, f64, f64), String> {
    let u = normalized_vector(support.frame.u)
        .ok_or_else(|| "the face has an invalid local U axis".to_owned())?;
    let normal = frame_normal(support.frame)
        .ok_or_else(|| "the face has an invalid local normal".to_owned())?;
    let v = normalized_vector(cross_vector(normal, u))
        .ok_or_else(|| "the face has an invalid local V axis".to_owned())?;
    let Some(first) = support.boundary.first() else {
        return Err("the face boundary is empty".to_owned());
    };
    let (mut min_u, mut max_u, mut min_v, mut max_v) = (first.x, first.x, first.y, first.y);
    for point in support.boundary.iter().skip(1) {
        min_u = min_u.min(point.x);
        max_u = max_u.max(point.x);
        min_v = min_v.min(point.y);
        max_v = max_v.max(point.y);
    }
    let center_u = 0.5 * (min_u + max_u);
    let center_v = 0.5 * (min_v + max_v);
    let origin = Point3::new(
        support.frame.origin.x + support.frame.u.x * center_u + support.frame.v.x * center_v,
        support.frame.origin.y + support.frame.u.y * center_u + support.frame.v.y * center_v,
        support.frame.origin.z + support.frame.u.z * center_u + support.frame.v.z * center_v,
    );
    let u_scale = vector_length(support.frame.u).unwrap_or(1.0);
    let v_scale = vector_length(support.frame.v).unwrap_or(1.0);
    let half_u = (0.575 * (max_u - min_u).abs() * u_scale).max(0.5);
    let half_v = (0.575 * (max_v - min_v).abs() * v_scale).max(0.5);
    Ok((PlanarFrame3::new(origin, u, v), half_u, half_v))
}

fn reference_plane_corners(frame: PlanarFrame3, half_u: f64, half_v: f64) -> [Point3; 4] {
    [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(su, sv)| {
        Point3::new(
            frame.origin.x + su * half_u * frame.u.x + sv * half_v * frame.v.x,
            frame.origin.y + su * half_u * frame.u.y + sv * half_v * frame.v.y,
            frame.origin.z + su * half_u * frame.u.z + sv * half_v * frame.v.z,
        )
    })
}

fn reference_plane_overlay(
    frame: PlanarFrame3,
    half_u: f64,
    half_v: f64,
    subdued: bool,
    selection: Option<viewport::ReferencePlaneSelection>,
    label: &str,
) -> viewport::ModelSketchOverlay {
    let corners = reference_plane_corners(frame, half_u, half_v);
    let segments = vec![
        [corners[0], corners[1]],
        [corners[1], corners[2]],
        [corners[2], corners[3]],
        [corners[3], corners[0]],
    ];
    viewport::ModelSketchOverlay::new(Vec::new(), segments, subdued)
        .reference_plane(selection, label, corners)
}

fn bounds_for_points(points: &[Point3]) -> Option<Aabb3> {
    let first = *points.first()?;
    let (mut min, mut max) = (first, first);
    for point in points.iter().copied().skip(1) {
        min.x = min.x.min(point.x);
        min.y = min.y.min(point.y);
        min.z = min.z.min(point.z);
        max.x = max.x.max(point.x);
        max.y = max.y.max(point.y);
        max.z = max.z.max(point.z);
    }
    Some(Aabb3::new(min, max))
}

fn validate_construction_planes(planes: &[ConstructionPlane]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for plane in planes {
        if plane.id == 0 || !ids.insert(plane.id) {
            return Err("Artificer workspace contains an invalid or duplicate plane id".into());
        }
        if plane.name.trim().is_empty()
            || !plane.frame.is_finite()
            || frame_normal(plane.frame).is_none()
            || !plane.half_u.is_finite()
            || !plane.half_v.is_finite()
            || plane.half_u <= 0.0
            || plane.half_v <= 0.0
        {
            return Err(format!("Artificer workspace plane {} is invalid", plane.id));
        }
    }
    Ok(())
}

const fn origin_plane_label(plane: SketchPlane) -> &'static str {
    match plane {
        SketchPlane::XY => "XY Plane",
        SketchPlane::YZ => "YZ Plane",
        SketchPlane::XZ => "XZ Plane",
    }
}

const fn profile_status_color(status: CertifiedProfileStatus) -> Color32 {
    match status {
        CertifiedProfileStatus::Closed { .. }
        | CertifiedProfileStatus::ClosedAnalyticCircle
        | CertifiedProfileStatus::ClosedAnalyticCurves
        | CertifiedProfileStatus::ClosedRegions { .. } => GOOD,
        CertifiedProfileStatus::SelfIntersecting
        | CertifiedProfileStatus::Invalid
        | CertifiedProfileStatus::TooManyCurves { .. }
        | CertifiedProfileStatus::TooManyLoops { .. }
        | CertifiedProfileStatus::TooManyRegions { .. }
        | CertifiedProfileStatus::LinearLoopTooLarge { .. } => BAD,
        CertifiedProfileStatus::Open
        | CertifiedProfileStatus::Indeterminate
        | CertifiedProfileStatus::CurvesNeedCertification
        | CertifiedProfileStatus::MultipleProfiles => WARN,
        CertifiedProfileStatus::Empty => MUTED,
    }
}

fn paint_confirmation_tick(ui: &egui::Ui, button_rect: egui::Rect, color: Color32) {
    // Paint this as geometry because the bundled font does not contain a
    // reliable check-mark glyph on every supported platform.
    let center = button_rect.center();
    let stroke = Stroke::new(2.0, color);
    ui.painter().line_segment(
        [
            center + egui::vec2(-4.0, 0.0),
            center + egui::vec2(-1.0, 3.5),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            center + egui::vec2(-1.0, 3.5),
            center + egui::vec2(5.0, -4.0),
        ],
        stroke,
    );
}

fn paint_confirmation_cross(ui: &egui::Ui, button_rect: egui::Rect, color: Color32) {
    let center = button_rect.center();
    let stroke = Stroke::new(2.0, color);
    ui.painter().line_segment(
        [
            center + egui::vec2(-4.0, -4.0),
            center + egui::vec2(4.0, 4.0),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            center + egui::vec2(-4.0, 4.0),
            center + egui::vec2(4.0, -4.0),
        ],
        stroke,
    );
}

fn confirmation_button_activated(ui: &egui::Ui, response: &egui::Response) -> bool {
    if response.clicked_by(egui::PointerButton::Primary) {
        return true;
    }

    // Bare Enter belongs to the global operation contract, independent of
    // which confirmation button happens to own focus. Space retains standard
    // focused-button activation for keyboard and accessibility users.
    let enter_activation = ui.input(|input| {
        input.raw.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    ..
                }
            )
        })
    });
    response.clicked() && !enter_activation
}

fn shell_button_activated(
    ui: &egui::Ui,
    response: &egui::Response,
    operation_pending: bool,
) -> bool {
    if !operation_pending || response.clicked_by(egui::PointerButton::Primary) {
        return response.clicked();
    }

    let bare_enter = ui.input(|input| {
        input.raw.events.iter().any(|event| {
            matches!(
                event,
                egui::Event::Key {
                    key: egui::Key::Enter,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } if *modifiers == egui::Modifiers::NONE
            )
        })
    });
    response.clicked() && !bare_enter
}

fn shell_toggle_button(
    ui: &mut egui::Ui,
    expanded: &mut bool,
    label: &str,
    noun: &str,
    operation_pending: bool,
) {
    let response = ui.add(
        egui::Button::new(RichText::new(label).font(FontId::proportional(11.5)))
            .frame(false)
            .selected(*expanded)
            .corner_radius(2)
            .min_size(egui::vec2(56.0, 26.0)),
    );
    if shell_button_activated(ui, &response, operation_pending) {
        *expanded = !*expanded;
    }
    response.on_hover_text(if *expanded {
        format!("Hide the {noun}")
    } else {
        format!("Show the {noun}")
    });
}

fn workspace_tab(ui: &mut egui::Ui, label: &str, selected: bool, enabled: bool) -> egui::Response {
    let response = ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).font(FontId::proportional(12.0)))
            .frame(false)
            .corner_radius(2)
            .min_size(egui::vec2(66.0, 28.0)),
    );
    if selected {
        ui.painter().line_segment(
            [
                egui::pos2(response.rect.left() + 7.0, response.rect.bottom() - 1.0),
                egui::pos2(response.rect.right() - 7.0, response.rect.bottom() - 1.0),
            ],
            Stroke::new(2.0, ACCENT),
        );
    }
    response
}

fn browser_text_row(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.add_sized(
        [ui.available_width(), 24.0],
        egui::Label::new(
            RichText::new(text)
                .font(FontId::proportional(11.5))
                .color(color),
        )
        .truncate(),
    )
    .on_hover_text(text);
}

fn collapsible_card<R>(
    ui: &mut egui::Ui,
    id: &'static str,
    title: &'static str,
    default_open: bool,
    contents: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    egui::CollapsingHeader::new(RichText::new(title).small().strong().color(MUTED))
        .id_salt(id)
        .default_open(default_open)
        .show_background(false)
        .show(ui, |ui| {
            Frame::new()
                .inner_margin(Margin::symmetric(5, 4))
                .show(ui, contents)
                .inner
        })
        .body_returned
}

fn timeline_chip(ui: &mut egui::Ui, label: &str, selected: bool, color: Color32) {
    Frame::new()
        .fill(if selected {
            translucent(color, 40)
        } else {
            Color32::TRANSPARENT
        })
        .stroke(Stroke::new(1.0, if selected { color } else { BORDER }))
        .corner_radius(3)
        .inner_margin(Margin::symmetric(7, 2))
        .show(ui, |ui| {
            ui.label(RichText::new(label).small().color(color));
        });
}

fn timeline_group_color(group: u64) -> Color32 {
    const HUES: [Color32; 6] = [
        Color32::from_rgb(24, 112, 172),
        Color32::from_rgb(52, 122, 60),
        Color32::from_rgb(128, 68, 140),
        Color32::from_rgb(172, 102, 22),
        Color32::from_rgb(20, 126, 108),
        Color32::from_rgb(164, 62, 86),
    ];
    if group == 0 {
        BORDER
    } else {
        HUES[(group as usize) % HUES.len()]
    }
}

fn timeline_connector(ui: &mut egui::Ui, related: bool, color: Color32) {
    ui.label(
        RichText::new(if related { "•" } else { "›" })
            .color(if related {
                color.gamma_multiply(0.72)
            } else {
                BORDER
            })
            .strong(),
    );
}

fn status_line(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.horizontal_top(|ui| {
        let (dot, _) = ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.0, color);
        ui.add(egui::Label::new(RichText::new(text).color(TEXT).strong()).wrap());
    });
}

fn canvas_overlay_label(
    ui: &mut egui::Ui,
    id: &'static str,
    rect: egui::Rect,
    text: &str,
    color: Color32,
) -> egui::Response {
    let mut overlay_ui = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id)
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    Frame::new()
        .fill(PANEL.gamma_multiply(0.94))
        .stroke(Stroke::new(1.0, BORDER))
        .corner_radius(3)
        .inner_margin(Margin::symmetric(6, 2))
        .show(&mut overlay_ui, |ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(text)
                        .font(FontId::proportional(11.0))
                        .strong()
                        .color(color),
                )
                .truncate(),
            )
        })
        .inner
}

fn hud_button(
    ui: &mut egui::Ui,
    visual_label: &str,
    accessible_label: &str,
    selected: bool,
) -> egui::Response {
    let response = ui.add(
        egui::Button::new(RichText::new(visual_label).font(FontId::proportional(11.0)))
            .selected(selected)
            .corner_radius(3)
            .min_size(egui::vec2(28.0, 28.0)),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, accessible_label)
    });
    response.on_hover_text(accessible_label)
}

#[cfg(test)]
mod extrusion_workbench_tests {
    use artificer_geometry::ProfileWinding;

    use super::*;

    fn point(u: f64, v: f64) -> SketchPoint {
        SketchPoint::new(u, v)
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() <= 1.0e-9,
            "actual={actual:.15}, expected={expected:.15}"
        );
    }

    #[test]
    fn artificer_workspace_file_round_trips_document_and_units() {
        let root = std::env::temp_dir().join(format!(
            "artificer-workspace-roundtrip-{}-{}",
            std::process::id(),
            DOCUMENT_SAVE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let path = root.join("fixture.artificer");
        let mut source = KernelLabApp::default();
        source.set_display_length_unit(DisplayLengthUnit::Inch);
        let expected_snapshot = source.displayed_snapshot_id();
        source
            .save_workspace_to_path(&path)
            .expect("save workspace");

        let mut restored = KernelLabApp::default();
        restored
            .load_workspace_from_path(&path)
            .expect("load workspace");
        assert_eq!(restored.displayed_snapshot_id(), expected_snapshot);
        assert_eq!(
            restored.document_settings().length_unit,
            DisplayLengthUnit::Inch
        );
        assert_eq!(restored.document_path(), path);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_workspace_version_is_atomic_and_legacy_documents_still_migrate() {
        let source = KernelLabApp::default();
        let legacy = source.native_document_json().expect("legacy archive");
        let mut restored = KernelLabApp::default();
        restored.set_display_length_unit(DisplayLengthUnit::Foot);
        restored
            .load_workspace_json(&legacy)
            .expect("legacy model document migration");
        assert_eq!(
            restored.document_settings().length_unit,
            DisplayLengthUnit::Foot
        );

        let snapshot = restored.displayed_snapshot_id();
        let revision = restored.document_revision();
        let mut invalid = serde_json::from_str::<serde_json::Value>(
            &source.workspace_document_json().expect("workspace archive"),
        )
        .unwrap();
        invalid["version"] = serde_json::json!(ARTIFICER_WORKSPACE_VERSION + 1);
        assert!(restored.load_workspace_json(&invalid.to_string()).is_err());
        assert_eq!(restored.displayed_snapshot_id(), snapshot);
        assert_eq!(restored.document_revision(), revision);
        assert_eq!(
            restored.document_settings().length_unit,
            DisplayLengthUnit::Foot
        );
    }

    #[test]
    fn face_plane_and_parallel_face_midplane_commit_through_the_transaction_gate() {
        let mut app = KernelLabApp::default();
        let body = app.active_body_id().expect("bootstrap body");
        let body_key = viewport::BodyInstanceKey::new(body.get());
        let face_for = |role| {
            app.displayed
                .as_ref()
                .unwrap()
                .scene
                .triangles
                .iter()
                .find(|triangle| triangle.role == role)
                .unwrap()
                .source_face
        };
        let bottom = viewport::DocumentFaceSelection {
            body: body_key,
            face: face_for(FaceRole::NegativeZ),
        };
        let top = viewport::DocumentFaceSelection {
            body: body_key,
            face: face_for(FaceRole::PositiveZ),
        };

        app.select_model_face(top, false);
        app.stage_construction_plane();
        assert!(matches!(
            app.pending_operation,
            Some(PendingOperation::CreateConstructionPlane {
                source: ConstructionPlaneSource::OnFace { .. },
                ..
            })
        ));
        assert!(app.confirm_pending_operation());
        assert_eq!(app.construction_planes.len(), 1);
        assert_eq!(
            app.document.features().last().unwrap().kind,
            FeatureKind::DatumPlane
        );

        app.select_model_face(bottom, false);
        app.select_model_face(top, true);
        app.stage_construction_plane();
        assert!(matches!(
            app.pending_operation,
            Some(PendingOperation::CreateConstructionPlane {
                source: ConstructionPlaneSource::BetweenFaces { .. },
                ..
            })
        ));
        assert!(app.confirm_pending_operation());
        let midplane = app.construction_planes.last().unwrap();
        assert_close(midplane.frame.origin.z, 2.0);
        assert_eq!(app.selected_construction_plane, Some(midplane.id));
    }

    #[test]
    fn construction_plane_round_trips_and_supports_an_offset_sketch() {
        let mut source = KernelLabApp::default();
        let body = source.active_body_id().unwrap();
        let face = source
            .displayed
            .as_ref()
            .unwrap()
            .scene
            .triangles
            .iter()
            .find(|triangle| triangle.role == FaceRole::PositiveZ)
            .unwrap()
            .source_face;
        source.select_model_face(
            viewport::DocumentFaceSelection {
                body: viewport::BodyInstanceKey::new(body.get()),
                face,
            },
            false,
        );
        source.stage_construction_plane();
        source.confirm_pending_operation();
        let plane_id = source.construction_planes[0].id;
        source.begin_construction_plane_sketch(plane_id);
        assert!(matches!(
            source.sketch_support,
            SketchSupport::ConstructionPlane {
                id: Some(id),
                ..
            } if id == plane_id
        ));

        let json = source.workspace_document_json().unwrap();
        let mut restored = KernelLabApp::default();
        restored.load_workspace_json(&json).unwrap();
        assert_eq!(restored.construction_planes, source.construction_planes);
    }

    #[test]
    fn blank_workspace_keeps_origin_history_without_a_starting_solid() {
        let mut app = KernelLabApp::default();
        app.reset_to_blank_workspace();
        assert!(app.bodies.is_empty());
        assert!(app.displayed.is_none());
        assert_eq!(app.document.features().len(), 1);
        assert_eq!(app.document.features()[0].kind, FeatureKind::Origin);
        let overlays = app.visible_reference_plane_overlays();
        assert_eq!(overlays.len(), 3);
        assert!(overlays.iter().all(|overlay| overlay.segment_count() == 4));
        let bounds = app
            .visible_reference_plane_bounds()
            .expect("the three origin planes have finite bounds");
        assert_eq!(bounds.min, Point3::new(-25.0, -25.0, -25.0));
        assert_eq!(bounds.max, Point3::new(25.0, 25.0, 25.0));
        assert_close(app.view.fit_radius(), 25.0 * 3.0_f64.sqrt());
        assert_eq!(app.view.target(), Point3::new(0.0, 0.0, 0.0));

        let modeled = KernelLabApp::default();
        assert!(
            modeled.visible_reference_plane_overlays().is_empty(),
            "the first committed modeling feature hides, but does not delete, origin planes"
        );
    }

    #[test]
    fn starting_a_plane_sketch_frames_the_camera_on_that_plane() {
        // A face sketch flies the camera onto the face; a plane sketch must do
        // the same, so the two entry points feel like one gesture.
        for plane in SketchPlane::ALL {
            let mut app = KernelLabApp::default();
            app.reset_to_blank_workspace();
            app.selected_origin_plane = plane;
            let before = app.view;
            app.enter_sketch_mode();
            assert_eq!(app.workbench_mode, WorkbenchMode::Sketch);
            // The sketch itself opens straight away: a plane is not backed by
            // a snapshot, so nothing can go stale while the camera moves.
            assert!(matches!(app.sketch_support, SketchSupport::Origin { .. }));
            // Instant mode is the test default, so the camera has arrived.
            assert_ne!(app.view, before, "{plane:?} should reframe the camera");
            let expected = before
                .face_aligned_target(
                    sketch_plane_frame(plane),
                    sketch_plane_frame(plane).origin,
                    ORIGIN_PLANE_HALF_EXTENT_MM,
                )
                .expect("a plane always has a camera target");
            assert_eq!(app.view, expected, "{plane:?} should look straight at it");

            // Leaving the sketch hands the model camera back, so a solid
            // extruded off the plane is not left being viewed edge-on.
            app.enter_model_mode();
            assert_eq!(app.workbench_mode, WorkbenchMode::Model);
            assert_eq!(
                app.view, before,
                "{plane:?} should restore the camera the sketch borrowed"
            );

            // With animation on, the flight is scheduled and the sketch
            // waits for it: opening the 2D canvas immediately would replace
            // the very viewport the animation plays in.
            let mut animated = KernelLabApp::default();
            animated.reset_to_blank_workspace();
            animated.set_face_camera_animation(true);
            animated.selected_origin_plane = plane;
            let start = animated.view;
            animated.enter_sketch_mode();
            assert!(
                animated.face_camera_transition.is_some(),
                "{plane:?} should fly the camera when animating"
            );
            assert_eq!(animated.view, start, "an animated transition does not jump");
            assert_eq!(
                animated.workbench_mode,
                WorkbenchMode::Model,
                "the sketch waits for the flight to land"
            );
            assert_eq!(
                animated.pending_plane_sketch,
                Some(PendingPlaneSketch::Origin(plane))
            );
            // Landing opens the sketch, exactly as the completion handler does.
            match animated.pending_plane_sketch.take().expect("pending") {
                PendingPlaneSketch::Origin(plane) => animated.open_origin_plane_sketch(plane),
                PendingPlaneSketch::Construction(id) => {
                    animated.open_construction_plane_sketch(id);
                }
            }
            assert_eq!(animated.workbench_mode, WorkbenchMode::Sketch);
            assert!(matches!(
                animated.sketch_support,
                SketchSupport::Origin { .. }
            ));
        }
    }

    #[test]
    fn starting_a_sketch_retires_the_standard_planes_without_deleting_them() {
        let mut app = KernelLabApp::default();
        app.reset_to_blank_workspace();
        assert_eq!(app.visible_reference_plane_overlays().len(), 3);

        // Entering the sketch is when the plane choice is settled, so the
        // planes go then — not when the first solid feature commits.
        app.workbench_mode = WorkbenchMode::Sketch;
        assert!(
            app.visible_reference_plane_overlays().is_empty(),
            "the standard planes must retire as the sketch opens"
        );

        // Leaving a still-blank sketch brings them back: nothing was deleted.
        app.workbench_mode = WorkbenchMode::Model;
        assert_eq!(app.visible_reference_plane_overlays().len(), 3);
    }

    #[test]
    fn edge_measurement_handles_single_parallel_skew_and_crossing_segments() {
        let edge = [Point3::new(0.0, 0.0, 0.0), Point3::new(3.0, 4.0, 0.0)];
        assert_close(model_segment_length(edge), 5.0);
        assert_close(
            model_segment_distance(
                [Point3::new(0.0, 0.0, 0.0), Point3::new(4.0, 0.0, 0.0)],
                [Point3::new(0.0, 2.0, 0.0), Point3::new(4.0, 2.0, 0.0)],
            ),
            2.0,
        );
        assert_close(
            model_segment_distance(
                [Point3::new(-1.0, 0.0, 0.0), Point3::new(1.0, 0.0, 0.0)],
                [Point3::new(0.0, -1.0, 0.0), Point3::new(0.0, 1.0, 0.0)],
            ),
            0.0,
        );
        assert_close(
            model_segment_distance(
                [Point3::new(0.0, 0.0, 0.0), Point3::new(1.0, 0.0, 0.0)],
                [Point3::new(0.5, -1.0, 3.0), Point3::new(0.5, 1.0, 3.0)],
            ),
            3.0,
        );
    }

    #[test]
    fn first_closed_sketch_in_a_blank_document_publishes_the_first_body() {
        let mut app = KernelLabApp::default();
        app.reset_to_blank_workspace();
        app.workbench_mode = WorkbenchMode::Sketch;
        app.sketch
            .stage_geometry(SketchGeometry::rectangle(
                point(-10.0, -5.0),
                point(10.0, 5.0),
            ))
            .expect("blank-document rectangle should stage");
        app.sketch
            .commit_pending()
            .expect("blank-document rectangle should commit");
        app.sketch_revision = 1;
        app.feature_preview.commit_sketch_revision(1);

        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        assert!(app.stage_sketch_extrusion());
        assert!(app.confirm_pending_operation());

        assert!(app.pending_operation.is_none());
        assert_eq!(app.last_error_code(), None);
        assert_eq!(app.body_count(), 1);
        assert!(app.displayed.is_some());
        assert_ne!(
            app.displayed_snapshot_id(),
            Some(app.empty_snapshot.id()),
            "the first extrusion must replace the canonical empty snapshot"
        );
        assert_eq!(
            app.document
                .features()
                .iter()
                .map(|feature| feature.kind)
                .collect::<Vec<_>>(),
            vec![
                FeatureKind::Origin,
                FeatureKind::Sketch,
                FeatureKind::Extrude
            ]
        );
        assert!(
            app.visible_reference_plane_overlays().is_empty(),
            "the first real feature hides the standard planes"
        );
    }

    #[test]
    fn extrusion_rejection_trace_is_actionable_and_privacy_filtered() {
        let app = KernelLabApp::default();
        let mut error = workbench_extrusion_error(
            KernelErrorCode::StaleSnapshot,
            app.empty_snapshot.id(),
            "the staged extrusion targets an inactive snapshot",
        );
        error
            .details
            .insert("expected_snapshot".to_owned(), "expected".to_owned());
        error
            .details
            .insert("actual_snapshot".to_owned(), "actual".to_owned());
        error
            .details
            .insert("parameter_name".to_owned(), "private label".to_owned());

        let payload = extrusion_rejection_trace_payload(&error);
        assert_eq!(payload["result"], "rejected");
        assert_eq!(payload["error_code"], "stale_snapshot");
        assert_eq!(payload["details"]["expected_snapshot"], "expected");
        assert_eq!(payload["details"]["actual_snapshot"], "actual");
        assert!(payload["details"].get("parameter_name").is_none());
        assert!(!payload.to_string().contains("private label"));
    }

    fn finished_rectangle_app() -> KernelLabApp {
        let mut app = KernelLabApp::default();
        app.sketch
            .stage_geometry(SketchGeometry::rectangle(point(0.0, 0.0), point(4.0, 2.0)))
            .expect("rectangle should stage");
        app.sketch
            .commit_pending()
            .expect("rectangle should commit");
        app.sketch_revision = 1;
        app.sketch_finished = true;
        app.workbench_mode = WorkbenchMode::Model;
        app
    }

    fn active_rectangle_app() -> KernelLabApp {
        let mut app = KernelLabApp::default();
        app.sketch
            .stage_geometry(SketchGeometry::rectangle(point(0.0, 0.0), point(4.0, 2.0)))
            .expect("rectangle should stage");
        app.sketch
            .commit_pending()
            .expect("rectangle should commit");
        app.sketch_revision = 1;
        app.feature_preview.commit_sketch_revision(1);
        app.sketch_finished = false;
        app.workbench_mode = WorkbenchMode::Sketch;
        app
    }

    fn active_polygon_app(vertices: &[SketchPoint]) -> KernelLabApp {
        let mut app = KernelLabApp::default();
        for (first, second) in vertices
            .iter()
            .copied()
            .zip(vertices.iter().copied().cycle().skip(1))
            .take(vertices.len())
        {
            app.sketch
                .stage_geometry(SketchGeometry::segment(first, second))
                .expect("polygon edge should stage");
            app.sketch
                .commit_pending()
                .expect("polygon edge should commit");
            app.sketch_revision = app.sketch_revision.saturating_add(1);
        }
        app.feature_preview
            .commit_sketch_revision(app.sketch_revision);
        app.sketch_finished = false;
        app.workbench_mode = WorkbenchMode::Sketch;
        app
    }

    fn active_geometry_app(geometries: impl IntoIterator<Item = SketchGeometry>) -> KernelLabApp {
        let mut app = KernelLabApp::default();
        for geometry in geometries {
            app.sketch
                .stage_geometry(geometry)
                .expect("profile geometry should stage");
            app.sketch
                .commit_pending()
                .expect("profile geometry should commit");
            app.sketch_revision = app.sketch_revision.saturating_add(1);
        }
        app.feature_preview
            .commit_sketch_revision(app.sketch_revision);
        app.sketch_finished = false;
        app.workbench_mode = WorkbenchMode::Sketch;
        app
    }

    fn finished_face_rectangle_app(mode: ExtrusionMode) -> KernelLabApp {
        let mut app = KernelLabApp::default();
        let body = app.displayed.as_ref().expect("bootstrap body");
        let face = body
            .scene
            .triangles
            .iter()
            .find(|triangle| triangle.role == FaceRole::PositiveZ)
            .expect("positive Z face")
            .source_face;
        let support = NativeKernel::planar_face_support(&body.snapshot, face)
            .expect("selected face supports a sketch");
        let bounds = support.boundary.iter().fold(
            [
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ],
            |[u0, u1, v0, v1], point| {
                [
                    u0.min(point.x),
                    u1.max(point.x),
                    v0.min(point.y),
                    v1.max(point.y),
                ]
            },
        );
        app.selected_face = Some(face);
        assert!(!app.start_face_sketch_camera_transition(support));
        app.sketch
            .stage_geometry(SketchGeometry::rectangle(
                point(bounds[0] * 0.5, bounds[2] * 0.5),
                point(bounds[1] * 0.5, bounds[3] * 0.5),
            ))
            .expect("face rectangle should stage");
        app.sketch
            .commit_pending()
            .expect("face rectangle should commit");
        app.sketch_revision = 1;
        app.sketch_finished = true;
        app.workbench_mode = WorkbenchMode::Model;
        app.select_extrusion_mode(mode);
        if mode == ExtrusionMode::Cut {
            app.set_extrusion_distance_intent(-app.extrusion_distance.abs());
        }
        app
    }

    fn replace_finished_face_geometry(
        app: &mut KernelLabApp,
        geometries: impl IntoIterator<Item = SketchGeometry>,
        mode: ExtrusionMode,
        distance: f64,
    ) {
        app.sketch = SketchCanvasState::default();
        app.sketch_revision = 0;
        for geometry in geometries {
            app.sketch
                .stage_geometry(geometry)
                .expect("face profile geometry should stage");
            app.sketch
                .commit_pending()
                .expect("face profile geometry should commit");
            app.sketch_revision = app.sketch_revision.saturating_add(1);
        }
        app.feature_preview
            .commit_sketch_revision(app.sketch_revision);
        app.sketch_finished = true;
        app.workbench_mode = WorkbenchMode::Model;
        app.select_extrusion_mode(mode);
        app.set_extrusion_distance_intent(distance);
    }

    fn finished_face_circle_app(mode: ExtrusionMode, distance: f64) -> KernelLabApp {
        let mut app = finished_face_rectangle_app(mode);
        let boundary = match &app.sketch_support {
            SketchSupport::PlanarFace { boundary, .. } => boundary,
            SketchSupport::Origin { .. } | SketchSupport::ConstructionPlane { .. } => {
                panic!("expected face support")
            }
        };
        let bounds = boundary.iter().fold(
            [
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ],
            |[u0, u1, v0, v1], point| {
                [
                    u0.min(point.x),
                    u1.max(point.x),
                    v0.min(point.y),
                    v1.max(point.y),
                ]
            },
        );
        let center = point((bounds[0] + bounds[1]) * 0.5, (bounds[2] + bounds[3]) * 0.5);
        let radius = ((bounds[1] - bounds[0]).min(bounds[3] - bounds[2])) * 0.25;
        replace_finished_face_geometry(
            &mut app,
            [SketchGeometry::circle(
                center,
                point(center.u + radius, center.v),
            )],
            mode,
            distance,
        );
        app
    }

    fn replace_with_finished_face_rectangle(
        app: &mut KernelLabApp,
        role: FaceRole,
        fraction: f64,
        mode: ExtrusionMode,
        distance: f64,
    ) {
        let body = app.displayed.as_ref().expect("committed body");
        let face = body
            .scene
            .triangles
            .iter()
            .find(|triangle| triangle.role == role)
            .unwrap_or_else(|| panic!("missing {role:?} face"))
            .source_face;
        let support = NativeKernel::planar_face_support(&body.snapshot, face)
            .expect("selected face supports a sketch");
        let bounds = support.boundary.iter().fold(
            [
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ],
            |[u0, u1, v0, v1], point| {
                [
                    u0.min(point.x),
                    u1.max(point.x),
                    v0.min(point.y),
                    v1.max(point.y),
                ]
            },
        );
        app.selected_face = Some(face);
        assert!(!app.start_face_sketch_camera_transition(support));
        app.sketch
            .stage_geometry(SketchGeometry::rectangle(
                point(bounds[0] * fraction, bounds[2] * fraction),
                point(bounds[1] * fraction, bounds[3] * fraction),
            ))
            .expect("face rectangle should stage");
        app.sketch
            .commit_pending()
            .expect("face rectangle should commit");
        app.sketch_revision = 1;
        app.sketch_finished = true;
        app.workbench_mode = WorkbenchMode::Model;
        app.select_extrusion_mode(mode);
        app.set_extrusion_distance_intent(distance);
    }

    #[test]
    fn workbench_extrusion_classifier_accepts_simple_linear_loops_and_fails_closed() {
        let rectangle = [
            point(0.0, 0.0),
            point(4.0, 0.0),
            point(4.0, 2.0),
            point(0.0, 2.0),
        ];
        assert_eq!(
            classify_sketch_extrusion_vertices(&rectangle, ProfileWinding::CounterClockwise),
            SketchExtrusionEligibility::Ready
        );

        let concave = [
            point(0.0, 0.0),
            point(3.0, 0.0),
            point(1.5, 1.0),
            point(3.0, 2.0),
            point(0.0, 2.0),
        ];
        assert_eq!(
            classify_sketch_extrusion_vertices(&concave, ProfileWinding::CounterClockwise),
            SketchExtrusionEligibility::Ready
        );

        let collinear = [
            point(0.0, 0.0),
            point(1.0, 0.0),
            point(2.0, 0.0),
            point(2.0, 2.0),
            point(0.0, 2.0),
        ];
        assert_eq!(
            classify_sketch_extrusion_vertices(&collinear, ProfileWinding::CounterClockwise),
            SketchExtrusionEligibility::Ready
        );

        let unresolved = [point(0.0, 0.0), point(f64::NAN, 1.0), point(0.0, 3.0)];
        assert_eq!(
            classify_sketch_extrusion_vertices(&unresolved, ProfileWinding::CounterClockwise),
            SketchExtrusionEligibility::NumericallyIndeterminate
        );

        let oversized = vec![point(0.0, 0.0); MAX_EXTRUSION_PROFILE_VERTICES + 1];
        assert_eq!(
            classify_sketch_extrusion_vertices(&oversized, ProfileWinding::CounterClockwise),
            SketchExtrusionEligibility::TooManyVertices {
                count: MAX_EXTRUSION_PROFILE_VERTICES + 1,
            }
        );
    }

    #[test]
    fn face_profile_preflight_respects_concave_material_and_exact_holes() {
        let outer = vec![
            ProtocolPoint2::new(-3.0, -3.0),
            ProtocolPoint2::new(3.0, -3.0),
            ProtocolPoint2::new(3.0, 3.0),
            ProtocolPoint2::new(-3.0, 3.0),
        ];
        let hole = vec![
            ProtocolPoint2::new(-0.5, -0.5),
            ProtocolPoint2::new(-0.5, 0.5),
            ProtocolPoint2::new(0.5, 0.5),
            ProtocolPoint2::new(0.5, -0.5),
        ];
        let material_profile = [
            point(1.0, -0.5),
            point(2.0, -0.5),
            point(2.0, 0.5),
            point(1.0, 0.5),
        ];
        assert_eq!(
            classify_face_profile_domain(&material_profile, &outer, std::slice::from_ref(&hole)),
            SketchExtrusionEligibility::Ready
        );

        let inside_hole = [
            point(-0.25, -0.25),
            point(0.25, -0.25),
            point(0.25, 0.25),
            point(-0.25, 0.25),
        ];
        assert_eq!(
            classify_face_profile_domain(&inside_hole, &outer, std::slice::from_ref(&hole)),
            SketchExtrusionEligibility::ProfileOutsideSupport
        );

        let encloses_hole = [
            point(-1.0, -1.0),
            point(1.0, -1.0),
            point(1.0, 1.0),
            point(-1.0, 1.0),
        ];
        assert_eq!(
            classify_face_profile_domain(&encloses_hole, &outer, std::slice::from_ref(&hole),),
            SketchExtrusionEligibility::BooleanUnionRequired
        );

        let spoke_crossing_the_void = [
            point(0.0, -0.2),
            point(2.0, -0.2),
            point(2.0, 0.2),
            point(0.0, 0.2),
        ];
        assert_eq!(
            classify_face_profile_domain(
                &spoke_crossing_the_void,
                &outer,
                std::slice::from_ref(&hole),
            ),
            SketchExtrusionEligibility::BooleanUnionRequired,
            "a spoke that reaches from a face void into material is a union bridge, not an invalid closed profile"
        );
    }

    #[test]
    fn every_cuboid_face_context_uses_its_exact_right_handed_local_frame() {
        let app = KernelLabApp::default();
        let body = app.displayed.as_ref().expect("bootstrap body");
        for role in [
            FaceRole::NegativeX,
            FaceRole::PositiveX,
            FaceRole::NegativeY,
            FaceRole::PositiveY,
            FaceRole::NegativeZ,
            FaceRole::PositiveZ,
        ] {
            let face = body
                .scene
                .triangles
                .iter()
                .find(|triangle| triangle.role == role)
                .unwrap_or_else(|| panic!("missing {role:?} face"))
                .source_face;
            let support = NativeKernel::planar_face_support(&body.snapshot, face)
                .unwrap_or_else(|error| panic!("{role:?} support failed: {error:?}"));
            let projection = FaceSketchProjection::from_frame(support.frame)
                .unwrap_or_else(|| panic!("{role:?} frame should prepare"));
            let local = ProtocolPoint2::new(0.375, -0.625);
            let world = frame_point(support.frame, local);
            let (round_trip, depth) = projection
                .project(world)
                .unwrap_or_else(|| panic!("{role:?} point should project"));
            assert_close(round_trip.u, local.x);
            assert_close(round_trip.v, local.y);
            assert_close(depth, 0.0);

            let camera_side = Point3::new(
                support.frame.origin.x + projection.normal.x,
                support.frame.origin.y + projection.normal.y,
                support.frame.origin.z + projection.normal.z,
            );
            let (camera_local, camera_depth) = projection
                .project(camera_side)
                .unwrap_or_else(|| panic!("{role:?} camera-side point should project"));
            assert_close(camera_local.u, 0.0);
            assert_close(camera_local.v, 0.0);
            assert_close(camera_depth, 1.0);

            let context = project_face_sketch_context(&body.scene, &support)
                .unwrap_or_else(|| panic!("{role:?} body context should project"));
            assert!(!context.triangles.is_empty());
            assert_eq!(context.edges.len(), 4);
            assert!(context.edges.len() < body.scene.edges.len());
            assert_eq!(context.boundary.len(), support.boundary.len());
        }
    }

    #[test]
    fn rotated_face_projection_round_trips_without_axis_plane_approximation() {
        let diagonal = 0.5_f64.sqrt();
        let frame = PlanarFrame3::new(
            Point3::new(10.0, -7.0, 3.5),
            Vector3::new(diagonal, diagonal, 0.0),
            Vector3::new(0.0, 0.0, 1.0),
        );
        let projection = FaceSketchProjection::from_frame(frame).expect("rotated frame");
        let local = ProtocolPoint2::new(2.75, -1.25);
        let world = frame_point(frame, local);
        let (round_trip, depth) = projection.project(world).expect("projected point");

        assert_close(round_trip.u, local.x);
        assert_close(round_trip.v, local.y);
        assert_close(depth, 0.0);
    }

    #[test]
    fn active_closed_sketch_can_stage_and_cancel_extrusion_without_being_finished() {
        let mut app = active_rectangle_app();
        let snapshot = app.displayed_snapshot_id();
        let attempts = app.transaction_attempt_count();
        let timeline = app.feature_timeline_entries();

        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        app.stage_sketch_extrusion();
        assert!(app.current_feature_preview().is_some());
        assert!(matches!(
            app.pending_operation,
            Some(PendingOperation::ExtrudeSketch {
                finish_sketch_on_commit: true,
                ..
            })
        ));
        let active_history = app
            .feature_preview
            .active_sketch
            .expect("active sketch history");
        assert!(!app.feature_preview.entries[active_history].finished);
        assert!(app.cancel_pending_operation());

        assert_eq!(app.workbench_mode, WorkbenchMode::Sketch);
        assert!(!app.sketch_finished);
        assert_eq!(app.displayed_snapshot_id(), snapshot);
        assert_eq!(app.transaction_attempt_count(), attempts);
        assert_eq!(app.feature_timeline_entries(), timeline);
        assert!(!app.feature_preview.entries[active_history].finished);
        assert!(app.pending_operation.is_none());
    }

    #[test]
    fn production_preview_queue_supersedes_stale_extrusion_generations() {
        let mut app = active_rectangle_app();
        app.feature_preview_scheduler = Some(JobScheduler::new(1));
        assert!(app.stage_sketch_extrusion());
        let context = egui::Context::default();

        let _ = app.feature_preview_for_frame(&context);
        let first_job = app
            .async_feature_preview_job
            .as_ref()
            .expect("first preview job")
            .id();
        app.set_extrusion_distance_intent(7.25);
        app.sync_pending_sketch_extrusion_inputs();
        let _ = app.feature_preview_for_frame(&context);
        let replacement_job = app
            .async_feature_preview_job
            .as_ref()
            .expect("replacement preview job")
            .id();
        assert!(replacement_job > first_job);

        for _ in 0..100 {
            if app.feature_preview_for_frame(&context).is_some() {
                return;
            }
            std::thread::yield_now();
        }
        panic!("the latest preview generation should be published");
    }

    #[test]
    fn production_extrusion_commit_runs_off_the_ui_thread_and_publishes_atomically() {
        let mut app = active_rectangle_app();
        app.feature_preview_scheduler = Some(JobScheduler::new(1));
        assert!(app.stage_sketch_extrusion());
        let before = app.displayed_snapshot_id();

        assert!(app.confirm_pending_operation());
        assert!(app.async_sketch_extrusion_commit.is_some());
        assert_eq!(app.displayed_snapshot_id(), before);
        assert!(app.pending_operation.is_some());

        let context = egui::Context::default();
        for _ in 0..2_000 {
            app.poll_async_sketch_extrusion_commit(&context);
            if app.async_sketch_extrusion_commit.is_none() {
                assert_ne!(app.displayed_snapshot_id(), before);
                assert!(app.pending_operation.is_none());
                assert_eq!(app.last_error_code(), None);
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the background extrusion commit should complete without blocking the UI");
    }

    #[test]
    fn unresolved_region_rebuild_keeps_the_last_valid_displayed_body() {
        use artificer_sketch::{
            Angle, ConfirmationSource, Length, PointInput, SketchPoint2, SketchRecipe, SketchValue,
        };

        let mut app = active_rectangle_app();
        assert!(app.stage_sketch_extrusion());
        assert!(app.confirm_pending_operation());
        let retained_snapshot = app.displayed_snapshot_id();
        let retained_volume = app.displayed_measures().unwrap().volume;
        let sketch = app.sketches[app.active_sketch_index.unwrap()]
            .id
            .expect("committed sketch identity");
        let extrusion = app.selected_history_feature.expect("extrusion feature");
        assert!(matches!(
            app.document.feature(extrusion).unwrap().action,
            ReplayAction::SketchRegionExtrusion(_)
        ));

        let existing_payload = app
            .document
            .sketch(sketch)
            .and_then(|record| {
                app.document
                    .sketch_payload(sketch, record.geometry_revision)
            })
            .cloned()
            .expect("current editable payload");
        let mut replacement = SketchDefinition::new();
        let transaction = replacement
            .stage(
                SketchRecipe::CentrePointCircle {
                    center: PointInput::Position(SketchPoint2::new(0.0, 0.0)),
                    radius: SketchValue::Literal(Length::new(1.0).unwrap()),
                    radial_angle: SketchValue::Literal(Angle::radians(0.0).unwrap()),
                },
                "Replace rectangle with circle",
            )
            .unwrap();
        replacement
            .commit(transaction, ConfirmationSource::GreenTick)
            .unwrap();
        let profile = compile_single_authoring_region(&replacement).unwrap();
        app.document
            .replace_sketch_payload(
                sketch,
                SketchPayload::from_authoring(
                    existing_payload.frame,
                    replacement,
                    Some(profile),
                    existing_payload.support,
                )
                .unwrap(),
            )
            .unwrap();
        let sketch_feature = app.document.sketch(sketch).unwrap().last_feature;

        assert!(!app.rebuild_document_from(sketch_feature));
        assert_eq!(app.displayed_snapshot_id(), retained_snapshot);
        assert_eq!(app.displayed_measures().unwrap().volume, retained_volume);
        assert_eq!(
            app.document.feature(extrusion).unwrap().state.rebuild,
            RebuildState::Dirty
        );
        assert!(
            app.document_status
                .as_deref()
                .is_some_and(|status| status.contains("needs repair"))
        );
    }

    #[test]
    fn staged_unordered_multiline_uses_one_exact_planar_region_command() {
        let a = point(0.0, 0.0);
        let b = point(4.0, 0.0);
        let c = point(4.0, 3.0);
        let d = point(0.0, 3.0);
        let mut app = active_geometry_app(
            [(c, b), (a, d), (c, d), (a, b)]
                .map(|(start, end)| SketchGeometry::segment(start, end)),
        );

        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        assert!(app.stage_sketch_extrusion());
        assert!(
            app.current_feature_preview().is_some(),
            "unordered multiline must have a live preview"
        );
        let KernelCommand::ExtrudePlanarProfile { profile, .. } = app
            .pending_sketch_extrusion_command()
            .expect("staged command payload")
        else {
            panic!("unordered multiline must use the planar-profile command");
        };
        assert_eq!(profile.regions.len(), 1);
        assert!(profile.regions[0].holes.is_empty());
        assert_eq!(profile.regions[0].outer.curves.len(), 4);
        assert!(
            profile.regions[0]
                .outer
                .curves
                .iter()
                .all(|curve| matches!(curve, PlanarCurve2::Line { .. }))
        );
        assert!(app.confirm_pending_operation());
        assert_eq!(app.last_error_code(), None);
        assert_close(
            app.displayed_measures().expect("extruded square").volume,
            48.0,
        );
    }

    #[test]
    fn closed_multiline_can_extrude_while_the_line_tool_remains_active() {
        let mut app = KernelLabApp::default();
        assert!(app.sketch.set_tool(crate::sketch::SketchTool::Line));
        let vertices = [
            point(0.0, 0.0),
            point(4.0, 0.0),
            point(4.0, 3.0),
            point(0.0, 3.0),
            point(0.0, 0.0),
        ];
        for edge in vertices.windows(2) {
            app.sketch
                .stage_geometry(SketchGeometry::segment(edge[0], edge[1]))
                .expect("polyline edge should stage");
            app.sketch
                .commit_pending()
                .expect("polyline edge should commit");
            app.sketch_revision = app.sketch_revision.saturating_add(1);
        }
        app.feature_preview
            .commit_sketch_revision(app.sketch_revision);
        app.workbench_mode = WorkbenchMode::Sketch;

        assert!(app.sketch.creation_anchor().is_none());
        assert!(!app.sketch_creation_draft_active());
        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        assert!(app.stage_sketch_extrusion());
        assert!(app.sketch.creation_anchor().is_none());
        assert!(matches!(
            app.pending_sketch_extrusion_command(),
            Some(KernelCommand::ExtrudePlanarProfile { .. })
        ));
    }

    #[test]
    fn staged_linear_profile_hole_remains_a_hole_in_one_region() {
        let mut app = active_geometry_app([
            SketchGeometry::rectangle(point(-5.0, -4.0), point(5.0, 4.0)),
            SketchGeometry::rectangle(point(-2.0, -1.0), point(2.0, 1.0)),
        ]);

        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        assert!(app.stage_sketch_extrusion());
        assert!(
            app.current_feature_preview().is_some(),
            "profile holes must have an honest live preview"
        );
        let KernelCommand::ExtrudePlanarProfile { profile, .. } = app
            .pending_sketch_extrusion_command()
            .expect("staged holed payload")
        else {
            panic!("profile holes must use the planar-profile command");
        };
        assert_eq!(profile.regions.len(), 1);
        assert_eq!(profile.regions[0].outer.curves.len(), 4);
        assert_eq!(profile.regions[0].holes.len(), 1);
        assert_eq!(profile.regions[0].holes[0].curves.len(), 4);
        assert!(app.confirm_pending_operation());
        assert_eq!(app.last_error_code(), None);
        assert_close(
            app.displayed_measures().expect("extruded frame").volume,
            288.0,
        );
    }

    #[test]
    fn disjoint_new_body_regions_are_an_explicit_hideable_body_group() {
        let mut app = active_geometry_app([
            SketchGeometry::rectangle(point(-4.0, -1.0), point(-2.0, 1.0)),
            SketchGeometry::rectangle(point(2.0, -1.0), point(4.0, 1.0)),
        ]);
        let _ = app.sketch.select_region_at_point(point(-3.0, 0.0), false);
        assert!(app.sketch.select_region_at_point(point(3.0, 0.0), true));

        assert!(app.stage_sketch_extrusion());
        assert!(app.confirm_pending_operation());
        assert_eq!(app.displayed_topology_counts().unwrap().solids, 2);
        assert_eq!(
            browser_body_object_name(app.active_body_ordinal, 2),
            "Body group 1 · 2 solids"
        );

        let active = app.active_body_index().expect("active body group");
        app.set_body_visibility(active, false);
        assert!(!app.bodies[active].visible);
        let end = app.document.history_position();
        assert!(app.move_history_cursor(2));
        assert!(app.move_history_cursor(end));
        let restored = app.active_body_index().expect("restored body group");
        assert_eq!(app.bodies[restored].body.report.topology.solids, 2);
        assert!(!app.bodies[restored].visible);
    }

    #[test]
    fn a_transform_commits_every_solid_in_the_active_body_group() {
        let mut app = active_geometry_app([
            SketchGeometry::rectangle(point(-4.0, -1.0), point(-2.0, 1.0)),
            SketchGeometry::rectangle(point(2.0, -1.0), point(4.0, 1.0)),
        ]);
        let _ = app.sketch.select_region_at_point(point(-3.0, 0.0), false);
        assert!(app.sketch.select_region_at_point(point(3.0, 0.0), true));
        assert!(app.stage_sketch_extrusion());
        assert!(app.confirm_pending_operation());
        assert_eq!(app.displayed_topology_counts().unwrap().solids, 2);
        let before = app.displayed_snapshot_id();
        let before_centroid = app
            .displayed_measures()
            .and_then(|measures| measures.centroid)
            .expect("body-group centroid");

        app.display_transform.translation = [1.0, 2.0, 3.0];
        app.apply_transform_preview();

        assert_ne!(app.displayed_snapshot_id(), before);
        assert_eq!(app.displayed_topology_counts().unwrap().solids, 2);
        let after_centroid = app
            .displayed_measures()
            .and_then(|measures| measures.centroid)
            .expect("transformed body-group centroid");
        assert_close(after_centroid.x, before_centroid.x + 1.0);
        assert_close(after_centroid.y, before_centroid.y + 2.0);
        assert_close(after_centroid.z, before_centroid.z + 3.0);
        assert!(!app.transform_preview_pending());
        assert!(app.pending_operation.is_none());
    }

    #[test]
    fn a_through_cut_split_restores_as_one_explicit_body_group() {
        let mut app = finished_face_rectangle_app(ExtrusionMode::Cut);
        let boundary = match &app.sketch_support {
            SketchSupport::PlanarFace { boundary, .. } => boundary,
            SketchSupport::Origin { .. } | SketchSupport::ConstructionPlane { .. } => {
                panic!("expected face support")
            }
        };
        let bounds = boundary.iter().fold(
            [
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ],
            |[u0, u1, v0, v1], point| {
                [
                    u0.min(point.x),
                    u1.max(point.x),
                    v0.min(point.y),
                    v1.max(point.y),
                ]
            },
        );
        replace_finished_face_geometry(
            &mut app,
            [
                SketchGeometry::rectangle(
                    point(bounds[0] * 0.75, bounds[2] * 0.75),
                    point(bounds[1] * 0.75, bounds[3] * 0.75),
                ),
                SketchGeometry::rectangle(
                    point(bounds[0] * 0.25, bounds[2] * 0.25),
                    point(bounds[1] * 0.25, bounds[3] * 0.25),
                ),
            ],
            ExtrusionMode::Cut,
            -4.0,
        );

        assert!(app.stage_sketch_extrusion());
        assert!(app.confirm_pending_operation());
        assert_eq!(app.last_error_code(), None);
        assert_eq!(app.displayed_topology_counts().unwrap().solids, 2);
        assert_eq!(
            browser_body_object_name(app.active_body_ordinal, 2),
            "Body group 1 · 2 solids"
        );

        let active = app.active_body_index().expect("active split body group");
        app.set_body_visibility(active, false);
        let end = app.document.history_position();
        assert!(app.move_history_cursor(2));
        assert!(app.move_history_cursor(end));
        let restored = app.active_body_index().expect("restored split group");
        assert_eq!(app.bodies[restored].body.report.topology.solids, 2);
        assert!(!app.bodies[restored].visible);
    }

    #[test]
    fn staged_circle_and_mixed_arc_commands_preserve_analytic_curves() {
        let mut circle =
            active_geometry_app([SketchGeometry::circle(point(2.0, 3.0), point(5.0, 3.0))]);
        assert_eq!(
            circle.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        assert!(circle.stage_sketch_extrusion());
        assert!(circle.current_feature_preview().is_some());
        let KernelCommand::ExtrudePlanarProfile { profile, .. } = circle
            .pending_sketch_extrusion_command()
            .expect("staged circle payload")
        else {
            panic!("circle must use the planar-profile command");
        };
        let [
            PlanarCurve2::Circle {
                center,
                radius,
                direction,
            },
        ] = profile.regions[0].outer.curves.as_slice()
        else {
            panic!("circle payload must remain one analytic curve");
        };
        assert_eq!(*center, ProtocolPoint2::new(2.0, 3.0));
        assert_eq!(*radius, 3.0);
        assert_eq!(*direction, ArcDirection::CounterClockwise);
        assert!(circle.confirm_pending_operation());
        assert_eq!(circle.last_error_code(), None);
        assert_close(
            circle.displayed_measures().expect("analytic disk").volume,
            36.0 * std::f64::consts::PI,
        );

        let mut annulus = active_geometry_app([
            SketchGeometry::circle(point(0.0, 0.0), point(5.0, 0.0)),
            SketchGeometry::circle(point(0.0, 0.0), point(2.0, 0.0)),
        ]);
        assert_eq!(
            annulus.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        assert!(annulus.stage_sketch_extrusion());
        assert!(annulus.current_feature_preview().is_some());
        let KernelCommand::ExtrudePlanarProfile { profile, .. } = annulus
            .pending_sketch_extrusion_command()
            .expect("staged annulus payload")
        else {
            panic!("analytic holes must use the planar-profile command");
        };
        assert_eq!(profile.regions.len(), 1);
        let [
            PlanarCurve2::Circle {
                radius: outer_radius,
                direction: outer_direction,
                ..
            },
        ] = profile.regions[0].outer.curves.as_slice()
        else {
            panic!("annulus outer must remain one analytic circle");
        };
        let [PlanarLoop2 { curves }] = profile.regions[0].holes.as_slice() else {
            panic!("annulus must retain one hole loop");
        };
        let [
            PlanarCurve2::Circle {
                radius: hole_radius,
                direction: hole_direction,
                ..
            },
        ] = curves.as_slice()
        else {
            panic!("annulus hole must remain one analytic circle");
        };
        assert_eq!(*outer_radius, 5.0);
        assert_eq!(*outer_direction, ArcDirection::CounterClockwise);
        assert_eq!(*hole_radius, 2.0);
        assert_eq!(*hole_direction, ArcDirection::Clockwise);
        assert!(annulus.confirm_pending_operation());
        assert_eq!(annulus.last_error_code(), None);
        assert_close(
            annulus
                .displayed_measures()
                .expect("analytic annulus")
                .volume,
            84.0 * std::f64::consts::PI,
        );

        let left = point(-2.0, 0.0);
        let right = point(2.0, 0.0);
        let mut mixed = active_geometry_app([
            SketchGeometry::segment(left, right),
            SketchGeometry::arc(point(0.0, 0.0), right, left),
        ]);
        assert_eq!(
            mixed.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        assert!(mixed.stage_sketch_extrusion());
        assert!(mixed.current_feature_preview().is_some());
        let KernelCommand::ExtrudePlanarProfile { profile, .. } = mixed
            .pending_sketch_extrusion_command()
            .expect("staged line/arc payload")
        else {
            panic!("line/arc loop must use the planar-profile command");
        };
        assert_eq!(profile.regions[0].outer.curves.len(), 2);
        assert!(
            profile.regions[0]
                .outer
                .curves
                .iter()
                .any(|curve| matches!(curve, PlanarCurve2::CircularArc { .. }))
        );
        assert!(
            profile.regions[0]
                .outer
                .curves
                .iter()
                .any(|curve| matches!(curve, PlanarCurve2::Line { .. }))
        );
        assert!(mixed.confirm_pending_operation());
        assert_eq!(mixed.last_error_code(), None);
        assert_close(
            mixed
                .displayed_measures()
                .expect("mixed line-and-arc prism")
                .volume,
            8.0 * std::f64::consts::PI,
        );
    }

    #[test]
    fn analytic_outer_with_a_multiline_hole_stages_and_commits_as_one_region() {
        let mut app = active_geometry_app([
            SketchGeometry::circle(point(0.0, 0.0), point(5.0, 0.0)),
            SketchGeometry::rectangle(point(-1.0, -1.0), point(1.0, 1.0)),
        ]);

        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        assert!(app.stage_sketch_extrusion());
        assert!(app.current_feature_preview().is_some());
        let KernelCommand::ExtrudePlanarProfile { profile, .. } = app
            .pending_sketch_extrusion_command()
            .expect("staged mixed-boundary profile")
        else {
            panic!("mixed-boundary hole must use the exact planar-profile command");
        };
        assert_eq!(profile.regions.len(), 1);
        assert!(matches!(
            profile.regions[0].outer.curves.as_slice(),
            [PlanarCurve2::Circle { .. }]
        ));
        assert_eq!(profile.regions[0].holes.len(), 1);
        assert_eq!(profile.regions[0].holes[0].curves.len(), 4);
        assert!(
            profile.regions[0].holes[0]
                .curves
                .iter()
                .all(|curve| matches!(curve, PlanarCurve2::Line { .. }))
        );

        assert!(app.confirm_pending_operation());
        assert_eq!(app.last_error_code(), None);
        assert_close(
            app.displayed_measures()
                .expect("circular prism with a rectangular hole")
                .volume,
            100.0 * std::f64::consts::PI - 16.0,
        );
    }

    #[test]
    fn staged_face_sketch_uses_snapshot_bound_planar_profile_command() {
        let mut app = finished_face_rectangle_app(ExtrusionMode::Add);
        assert!(app.stage_sketch_extrusion());
        let KernelCommand::ExtrudeFacePlanarProfile {
            target_face,
            profile,
            operation,
            ..
        } = app
            .pending_sketch_extrusion_command()
            .expect("staged face-profile payload")
        else {
            panic!("face sketches must use the snapshot-bound planar-profile command");
        };
        assert_eq!(Some(target_face), app.sketch_support.target_face());
        assert_eq!(operation, FaceExtrusionOperation::Add);
        assert_eq!(profile.regions.len(), 1);
        assert!(profile.regions[0].holes.is_empty());
        assert_eq!(profile.regions[0].outer.curves.len(), 4);
    }

    #[test]
    fn explicit_face_operation_preserves_signed_direction_and_auto_can_be_restored() {
        let mut app = finished_face_rectangle_app(ExtrusionMode::Add);
        app.select_automatic_extrusion_mode();
        app.set_extrusion_distance_intent(-2.0);
        assert_eq!(app.extrusion_mode(), ExtrusionMode::Cut);
        assert!(app.extrusion_mode_is_automatic());

        app.select_extrusion_mode(ExtrusionMode::Add);
        assert_eq!(app.extrusion_distance(), -2.0);
        assert_eq!(app.extrusion_mode(), ExtrusionMode::Add);
        assert!(!app.extrusion_mode_is_automatic());
        app.set_extrusion_distance_intent(-3.0);
        assert_eq!(app.extrusion_mode(), ExtrusionMode::Add);

        assert!(app.stage_sketch_extrusion());
        let original_normal = frame_normal(app.sketch_support.frame()).expect("support normal");
        let KernelCommand::ExtrudeFacePlanarProfile {
            frame,
            distance,
            operation,
            ..
        } = app
            .pending_sketch_extrusion_command()
            .expect("explicit inward Add command")
        else {
            panic!("face extrusion command")
        };
        let directed_normal = frame_normal(frame).expect("directed frame normal");
        assert_eq!(operation, FaceExtrusionOperation::Add);
        assert_eq!(distance, 3.0);
        assert!(
            original_normal.x * directed_normal.x
                + original_normal.y * directed_normal.y
                + original_normal.z * directed_normal.z
                < -0.999_999,
            "negative direction is encoded without changing Add intent"
        );

        assert!(app.cancel_pending_operation());
        app.select_automatic_extrusion_mode();
        assert_eq!(app.extrusion_mode(), ExtrusionMode::Cut);
        assert!(app.extrusion_mode_is_automatic());
    }

    #[test]
    fn cancelling_an_unfinished_extrusion_reopens_sketch_even_if_staged_from_model() {
        let mut app = active_rectangle_app();
        app.workbench_mode = WorkbenchMode::Model;

        assert!(app.stage_sketch_extrusion());
        assert!(app.cancel_pending_operation());

        assert_eq!(app.workbench_mode, WorkbenchMode::Sketch);
        assert!(!app.sketch_finished);
        assert!(app.pending_operation.is_none());
    }

    #[test]
    fn every_eligible_active_convex_polygon_has_a_live_extrusion_preview() {
        let fixtures = [
            vec![point(-1.0, -1.0), point(1.0, -1.0), point(0.0, 1.0)],
            vec![
                point(0.0, -1.5),
                point(1.5, -0.5),
                point(1.0, 1.25),
                point(-1.0, 1.25),
                point(-1.5, -0.5),
            ],
        ];
        for vertices in fixtures {
            let mut app = active_polygon_app(&vertices);
            assert_eq!(
                app.sketch_extrusion_eligibility(),
                SketchExtrusionEligibility::Ready
            );
            assert!(app.stage_sketch_extrusion());
            assert!(
                app.current_feature_preview().is_some(),
                "{}-vertex eligible profile staged without a live preview",
                vertices.len()
            );
        }
    }

    #[test]
    fn active_closed_sketch_is_finished_only_when_extrusion_publishes() {
        let mut app = active_rectangle_app();
        let original = app.displayed_snapshot_id();
        let attempts = app.transaction_attempt_count();

        app.stage_sketch_extrusion();
        assert!(app.current_feature_preview().is_some());
        let active_history = app
            .feature_preview
            .active_sketch
            .expect("active sketch history");
        assert!(!app.feature_preview.entries[active_history].finished);
        assert!(!app.sketch_finished);
        assert_eq!(app.pending_operation_label(), Some("Extrude active sketch"));
        assert!(app.confirm_pending_operation());

        assert!(app.sketch_finished);
        assert!(app.feature_preview.entries[active_history].finished);
        assert_ne!(app.displayed_snapshot_id(), original);
        assert_eq!(app.transaction_attempt_count(), attempts + 1);
        assert_eq!(
            app.feature_timeline_entries(),
            vec![
                "Origin".to_owned(),
                "Base body".to_owned(),
                "Sketch 1 · r1".to_owned(),
                "Extrude 1".to_owned(),
            ]
        );
        assert!(app.pending_operation.is_none());
    }

    #[test]
    fn active_face_through_cut_finishes_and_publishes_atomically() {
        let mut app = finished_face_rectangle_app(ExtrusionMode::Cut);
        app.sketch_finished = false;
        app.workbench_mode = WorkbenchMode::Sketch;
        app.feature_preview
            .commit_sketch_revision(app.sketch_revision);
        app.set_extrusion_distance_intent(-4.0);
        let body = app.displayed_snapshot_id();
        let attempts = app.transaction_attempt_count();
        let timeline = app.feature_timeline_entries();
        let active_history = app
            .feature_preview
            .active_sketch
            .expect("active sketch history");

        assert!(app.stage_sketch_extrusion());
        assert!(app.current_feature_preview().is_some());
        assert!(app.confirm_pending_operation());

        assert_eq!(app.last_error_code(), None);
        assert_ne!(app.displayed_snapshot_id(), body);
        assert_eq!(app.transaction_attempt_count(), attempts + 1);
        assert_eq!(app.feature_timeline_entries().len(), timeline.len() + 1);
        assert!(app.sketch_finished);
        assert!(app.feature_preview.entries[active_history].finished);
        assert_close(app.displayed_measures().unwrap().volume, 18.0);
        assert!(app.pending_operation.is_none());
    }

    #[test]
    fn stale_staged_extrusion_is_rejected_without_losing_body_or_intent() {
        let mut app = finished_rectangle_app();
        let body = app.displayed_snapshot_id();
        let attempts = app.transaction_attempt_count();
        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        app.stage_sketch_extrusion();
        let pending = app.pending_operation.expect("extrusion should stage");

        app.sketch_revision += 1;
        assert!(app.confirm_pending_operation());

        assert_eq!(app.displayed_snapshot_id(), body);
        assert_eq!(app.transaction_attempt_count(), attempts);
        assert_eq!(app.pending_operation, Some(pending));
        assert_eq!(app.last_error_code(), Some(KernelErrorCode::StaleSnapshot));
        assert_eq!(
            app.sketch_extrusion_issue.as_ref().map(|error| error.code),
            Some(KernelErrorCode::StaleSnapshot)
        );
    }

    #[test]
    fn invalid_staged_extrusion_is_rejected_without_losing_body_or_intent() {
        let mut app = finished_rectangle_app();
        let body = app.displayed_snapshot_id();
        let attempts = app.transaction_attempt_count();
        app.stage_sketch_extrusion();
        let Some(PendingOperation::ExtrudeSketch {
            base_snapshot,
            support_body,
            plane,
            revision,
            cancel_mode,
            finish_sketch_on_commit,
            frame,
            target_face,
            support_digest,
            mode,
            ..
        }) = app.pending_operation
        else {
            panic!("extrusion should stage");
        };
        let invalid = PendingOperation::ExtrudeSketch {
            base_snapshot,
            support_body,
            plane,
            revision,
            cancel_mode,
            finish_sketch_on_commit,
            distance: 0.0,
            frame,
            target_face,
            support_digest,
            mode,
        };
        app.pending_operation = Some(invalid);

        assert!(app.confirm_pending_operation());

        assert_eq!(app.displayed_snapshot_id(), body);
        assert_eq!(app.transaction_attempt_count(), attempts);
        assert_eq!(app.pending_operation, Some(invalid));
        assert_eq!(app.last_error_code(), Some(KernelErrorCode::InvalidInput));
        assert_eq!(
            app.sketch_extrusion_issue.as_ref().map(|error| error.code),
            Some(KernelErrorCode::InvalidInput)
        );
    }

    #[test]
    fn selected_face_add_and_cut_publish_exact_native_features() {
        for (mode, expected_volume, body_kind) in [
            (ExtrusionMode::Add, 25.5, ModelBodyKind::AddedBoss),
            (ExtrusionMode::Cut, 22.5, ModelBodyKind::CutPocket),
        ] {
            let mut app = finished_face_rectangle_app(mode);
            let original = app.displayed_snapshot_id();
            let attempts = app.transaction_attempt_count();
            assert!(app.sketch_is_face_supported());
            assert_eq!(
                app.sketch_extrusion_eligibility(),
                SketchExtrusionEligibility::Ready
            );

            app.stage_sketch_extrusion();
            assert!(app.current_feature_preview().is_some());
            assert_eq!(app.transaction_attempt_count(), attempts);
            assert_eq!(app.displayed_snapshot_id(), original);
            assert!(app.confirm_pending_operation());

            let committed = app.displayed_snapshot_id();
            assert_ne!(committed, original);
            assert_eq!(app.transaction_attempt_count(), attempts + 1);
            assert_eq!(app.model_body_kind, body_kind);
            assert_eq!(app.displayed.as_ref().unwrap().report.topology.faces, 11);
            assert_close(app.displayed_measures().unwrap().volume, expected_volume);
            assert_close(app.displayed_measures().unwrap().surface_area, 57.0);
            assert!(app.pending_operation.is_none());
        }
    }

    #[test]
    fn selected_face_circle_add_blind_cut_and_through_cut_publish_exact_features() {
        for (mode, distance, expected_volume, expected_area) in [
            (
                ExtrusionMode::Add,
                1.0,
                24.0 + 0.25 * std::f64::consts::PI,
                52.0 + std::f64::consts::PI,
            ),
            (
                ExtrusionMode::Cut,
                -1.0,
                24.0 - 0.25 * std::f64::consts::PI,
                52.0 + std::f64::consts::PI,
            ),
            (
                ExtrusionMode::Cut,
                -4.0,
                24.0 - std::f64::consts::PI,
                52.0 + 3.5 * std::f64::consts::PI,
            ),
        ] {
            let mut app = finished_face_circle_app(mode, distance);
            assert_eq!(
                app.sketch_extrusion_eligibility(),
                SketchExtrusionEligibility::Ready
            );
            let original = app.displayed_snapshot_id();
            assert!(app.stage_sketch_extrusion());
            assert!(app.current_feature_preview().is_some());
            assert!(app.confirm_pending_operation());

            assert_eq!(app.last_error_code(), None);
            assert_ne!(app.displayed_snapshot_id(), original);
            let measures = app.displayed_measures().expect("circle face feature");
            assert!((measures.volume - expected_volume).abs() <= 1.0e-8);
            assert!((measures.surface_area - expected_area).abs() <= 1.0e-8);
            assert!(app.pending_operation.is_none());
        }
    }

    #[test]
    fn staged_cut_preview_is_the_exact_subtracted_candidate_body() {
        let mut app = finished_face_circle_app(ExtrusionMode::Cut, -1.0);
        app.feature_preview_scheduler = Some(JobScheduler::new(1));
        let committed = app.displayed_snapshot_id().expect("committed body");
        assert!(app.stage_sketch_extrusion());
        let context = egui::Context::default();

        for _ in 0..5_000 {
            if let Some(preview) = app.feature_preview_for_frame(&context)
                && let Some(candidate) = preview.candidate()
            {
                assert_ne!(candidate.scene.snapshot, committed);
                assert!(!candidate.changed_faces.is_empty());
                assert_eq!(app.displayed_snapshot_id(), Some(committed));
                assert!(app.pending_operation.is_some());
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the staged cut must publish an exact private subtraction preview");
    }

    #[test]
    fn annular_cut_and_mixed_line_arc_add_reach_unified_face_feature_authority() {
        let mut annulus = finished_face_circle_app(ExtrusionMode::Cut, -1.0);
        annulus
            .sketch
            .stage_geometry(SketchGeometry::circle(point(0.0, 0.0), point(0.2, 0.0)))
            .expect("inner circle should stage");
        annulus
            .sketch
            .commit_pending()
            .expect("inner circle should commit");
        annulus.sketch_revision = annulus.sketch_revision.saturating_add(1);
        annulus
            .feature_preview
            .commit_sketch_revision(annulus.sketch_revision);
        assert_eq!(
            annulus.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        let annulus_before = annulus.displayed_snapshot_id();
        let annulus_attempts = annulus.transaction_attempt_count();
        assert!(annulus.stage_sketch_extrusion());
        let KernelCommand::ExtrudeFacePlanarProfile {
            profile, operation, ..
        } = annulus
            .pending_sketch_extrusion_command()
            .expect("annular face cut command")
        else {
            panic!("annular cut must retain its exact face profile")
        };
        assert_eq!(operation, FaceExtrusionOperation::Cut);
        assert_eq!(profile.regions.len(), 1);
        assert_eq!(profile.regions[0].holes.len(), 1);
        assert!(annulus.confirm_pending_operation());
        assert_ne!(annulus.displayed_snapshot_id(), annulus_before);
        assert_eq!(annulus.transaction_attempt_count(), annulus_attempts + 1);
        assert_eq!(annulus.last_error_code(), None);
        assert_close(
            annulus.displayed_measures().expect("annular pocket").volume,
            24.0 - 0.21 * std::f64::consts::PI,
        );
        assert_close(
            annulus
                .displayed_measures()
                .expect("annular pocket")
                .surface_area,
            52.0 + 1.4 * std::f64::consts::PI,
        );
        let annular_counts = TopologyCounts {
            vertices: 16,
            edges: 24,
            coedges: 48,
            loops: 14,
            faces: 12,
            shells: 1,
            solids: 1,
        };
        assert_eq!(annulus.displayed_topology_counts(), Some(annular_counts));
        assert_eq!(
            annulus.displayed.as_ref().unwrap().report.history.len(),
            annular_counts.total() as usize
        );
        assert_eq!(annulus.model_body_kind, ModelBodyKind::CutPocket);
        assert_eq!(
            annulus
                .feature_timeline_entries()
                .last()
                .map(String::as_str),
            Some("Cut 1")
        );

        let left = point(-0.5, 0.0);
        let right = point(0.5, 0.0);
        let mut mixed = finished_face_circle_app(ExtrusionMode::Add, 1.0);
        replace_finished_face_geometry(
            &mut mixed,
            [
                SketchGeometry::segment(left, right),
                SketchGeometry::arc(point(0.0, 0.0), right, left),
            ],
            ExtrusionMode::Add,
            1.0,
        );
        assert_eq!(
            mixed.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        let mixed_before = mixed.displayed_snapshot_id();
        let mixed_attempts = mixed.transaction_attempt_count();
        assert!(mixed.stage_sketch_extrusion());
        let KernelCommand::ExtrudeFacePlanarProfile {
            profile, operation, ..
        } = mixed
            .pending_sketch_extrusion_command()
            .expect("mixed face Add command")
        else {
            panic!("mixed Add must retain its exact face profile")
        };
        assert_eq!(operation, FaceExtrusionOperation::Add);
        assert_eq!(profile.regions[0].outer.curves.len(), 2);
        assert!(
            profile.regions[0]
                .outer
                .curves
                .iter()
                .any(|curve| { matches!(curve, PlanarCurve2::CircularArc { .. }) })
        );
        assert!(mixed.confirm_pending_operation());
        assert_ne!(mixed.displayed_snapshot_id(), mixed_before);
        assert_eq!(mixed.transaction_attempt_count(), mixed_attempts + 1);
        assert_eq!(mixed.last_error_code(), None);
        assert_close(
            mixed
                .displayed_measures()
                .expect("mixed semicircular boss")
                .volume,
            24.0 + 0.125 * std::f64::consts::PI,
        );
        assert_close(
            mixed
                .displayed_measures()
                .expect("mixed semicircular boss")
                .surface_area,
            53.0 + 0.5 * std::f64::consts::PI,
        );
        let mixed_counts = TopologyCounts {
            vertices: 12,
            edges: 18,
            coedges: 36,
            loops: 10,
            faces: 9,
            shells: 1,
            solids: 1,
        };
        assert_eq!(mixed.displayed_topology_counts(), Some(mixed_counts));
        assert_eq!(
            mixed.displayed.as_ref().unwrap().report.history.len(),
            mixed_counts.total() as usize
        );
        assert_eq!(mixed.model_body_kind, ModelBodyKind::AddedBoss);
        assert_eq!(
            mixed.feature_timeline_entries().last().map(String::as_str),
            Some("Add 1")
        );
    }

    #[test]
    fn selected_disjoint_face_regions_commit_as_one_exact_add() {
        let mut app = finished_face_rectangle_app(ExtrusionMode::Add);
        replace_finished_face_geometry(
            &mut app,
            [
                SketchGeometry::rectangle(point(-0.75, -0.25), point(-0.25, 0.25)),
                SketchGeometry::rectangle(point(0.25, -0.25), point(0.75, 0.25)),
            ],
            ExtrusionMode::Add,
            1.0,
        );
        assert_eq!(app.sketch.available_region_count(), 2);
        let _ = app.sketch.select_region_at_point(point(-0.5, 0.0), false);
        assert!(app.sketch.select_region_at_point(point(0.5, 0.0), true));
        assert_eq!(app.sketch.selected_region_count(), 2);
        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );

        let before = app.displayed_snapshot_id();
        let attempts = app.transaction_attempt_count();
        assert!(app.stage_sketch_extrusion());
        let KernelCommand::ExtrudeFacePlanarProfile {
            profile, operation, ..
        } = app
            .pending_sketch_extrusion_command()
            .expect("multi-region face Add")
        else {
            panic!("selected face regions must retain one unified exact command")
        };
        assert_eq!(operation, FaceExtrusionOperation::Add);
        assert_eq!(profile.regions.len(), 2);
        assert!(profile.regions.iter().all(|region| region.holes.is_empty()));
        assert!(app.confirm_pending_operation());

        assert_ne!(app.displayed_snapshot_id(), before);
        assert_eq!(app.transaction_attempt_count(), attempts + 1);
        assert_eq!(app.last_error_code(), None);
        assert_close(app.displayed_measures().unwrap().volume, 24.5);
        assert_close(app.displayed_measures().unwrap().surface_area, 56.0);
        let counts = TopologyCounts {
            vertices: 24,
            edges: 36,
            coedges: 72,
            loops: 18,
            faces: 16,
            shells: 1,
            solids: 1,
        };
        assert_eq!(app.displayed_topology_counts(), Some(counts));
        assert_eq!(
            app.displayed.as_ref().unwrap().report.history.len(),
            counts.total() as usize
        );
        assert_eq!(
            app.feature_timeline_entries().last().map(String::as_str),
            Some("Add 1")
        );
    }

    #[test]
    fn rectangle_add_on_an_analytic_disk_cap_commits_exactly() {
        let mut app =
            active_geometry_app([SketchGeometry::circle(point(0.0, 0.0), point(2.0, 0.0))]);
        assert!(app.stage_sketch_extrusion());
        assert!(app.confirm_pending_operation());

        let body = app.displayed.as_ref().expect("analytic disk body");
        let face = body
            .scene
            .triangles
            .iter()
            .find(|triangle| triangle.role == FaceRole::ExtrusionTop)
            .expect("analytic disk top cap")
            .source_face;
        let support = NativeKernel::planar_face_support(&body.snapshot, face)
            .expect("analytic cap supports an exact sketch frame");
        assert!(!support.linear_profile_extrusion_supported);

        app.selected_face = Some(face);
        app.animate_face_camera_transitions = false;
        assert!(!app.start_face_sketch_camera_transition(support));
        replace_finished_face_geometry(
            &mut app,
            [SketchGeometry::rectangle(
                point(-0.5, -0.5),
                point(0.5, 0.5),
            )],
            ExtrusionMode::Add,
            1.0,
        );

        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        let attempts = app.transaction_attempt_count();
        let before = app.displayed_snapshot_id();
        let before_measures = app
            .displayed_measures()
            .expect("analytic disk before rectangular Add");
        assert!(app.stage_sketch_extrusion());
        let KernelCommand::ExtrudeFacePlanarProfile {
            profile, operation, ..
        } = app
            .pending_sketch_extrusion_command()
            .expect("rectangle-on-disk Add command")
        else {
            panic!("rectangle Add must use the unified face-profile command")
        };
        assert_eq!(operation, FaceExtrusionOperation::Add);
        assert_eq!(profile.regions[0].outer.curves.len(), 4);
        assert!(app.confirm_pending_operation());
        assert_ne!(app.displayed_snapshot_id(), before);
        assert_eq!(app.transaction_attempt_count(), attempts + 1);
        assert_eq!(app.last_error_code(), None);
        assert_close(
            app.displayed_measures()
                .expect("rectangular boss on disk")
                .volume,
            before_measures.volume + 1.0,
        );
        assert_close(
            app.displayed_measures()
                .expect("rectangular boss on disk")
                .surface_area,
            before_measures.surface_area + 4.0,
        );
        let counts = TopologyCounts {
            vertices: 12,
            edges: 18,
            coedges: 36,
            loops: 10,
            faces: 9,
            shells: 1,
            solids: 1,
        };
        assert_eq!(app.displayed_topology_counts(), Some(counts));
        assert_eq!(
            app.displayed.as_ref().unwrap().report.history.len(),
            counts.total() as usize
        );
        assert_eq!(app.model_body_kind, ModelBodyKind::AddedBoss);
        assert_eq!(
            app.feature_timeline_entries().last().map(String::as_str),
            Some("Add 1")
        );
    }

    #[test]
    fn circle_on_an_analytic_cap_remains_extrudable_through_the_workbench() {
        let mut app =
            active_geometry_app([SketchGeometry::circle(point(0.0, 0.0), point(2.0, 0.0))]);
        assert!(app.stage_sketch_extrusion());
        assert!(app.confirm_pending_operation());
        let before_volume = app.displayed_measures().expect("analytic disk").volume;

        let body = app.displayed.as_ref().expect("analytic disk body");
        let face = body
            .scene
            .triangles
            .iter()
            .find(|triangle| triangle.role == FaceRole::ExtrusionTop)
            .expect("analytic disk top cap")
            .source_face;
        let support = NativeKernel::planar_face_support(&body.snapshot, face)
            .expect("analytic cap supports an exact sketch frame");
        let [u_min, u_max, v_min, v_max] = support.boundary.iter().fold(
            [
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ],
            |[u_min, u_max, v_min, v_max], point| {
                [
                    u_min.min(point.x),
                    u_max.max(point.x),
                    v_min.min(point.y),
                    v_max.max(point.y),
                ]
            },
        );
        let center = point((u_min + u_max) * 0.5, (v_min + v_max) * 0.5);
        let radius = (u_max - u_min).min(v_max - v_min) * 0.125;

        app.selected_face = Some(face);
        app.animate_face_camera_transitions = false;
        assert!(!app.start_face_sketch_camera_transition(support));
        replace_finished_face_geometry(
            &mut app,
            [SketchGeometry::circle(
                center,
                point(center.u + radius, center.v),
            )],
            ExtrusionMode::Add,
            1.0,
        );

        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        assert!(app.stage_sketch_extrusion());
        assert!(app.current_feature_preview().is_some());
        assert!(app.confirm_pending_operation());
        assert_eq!(app.last_error_code(), None);
        assert_close(
            app.displayed_measures()
                .expect("exact circular boss on analytic cap")
                .volume,
            before_volume + std::f64::consts::PI * radius * radius,
        );
    }

    #[test]
    fn concentric_circle_through_cut_crosses_an_earlier_boss_shoulder_in_the_workbench() {
        let mut app = finished_face_circle_app(ExtrusionMode::Add, 1.0);
        assert!(app.stage_sketch_extrusion());
        assert!(app.confirm_pending_operation());
        let boss_volume = app.displayed_measures().expect("circular boss").volume;

        let body = app.displayed.as_ref().expect("boss body");
        let face = body
            .scene
            .triangles
            .iter()
            .find(|triangle| triangle.role == FaceRole::FeatureEnd)
            .expect("boss end")
            .source_face;
        let support = NativeKernel::planar_face_support(&body.snapshot, face)
            .expect("boss end supports a second sketch");
        let [u_min, u_max, v_min, v_max] = support.boundary.iter().fold(
            [
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
            ],
            |[u_min, u_max, v_min, v_max], point| {
                [
                    u_min.min(point.x),
                    u_max.max(point.x),
                    v_min.min(point.y),
                    v_max.max(point.y),
                ]
            },
        );
        let center = point((u_min + u_max) * 0.5, (v_min + v_max) * 0.5);
        let hole_radius = (u_max - u_min).min(v_max - v_min) * 0.125;

        app.selected_face = Some(face);
        app.animate_face_camera_transitions = false;
        assert!(!app.start_face_sketch_camera_transition(support));
        replace_finished_face_geometry(
            &mut app,
            [SketchGeometry::circle(
                center,
                point(center.u + hole_radius, center.v),
            )],
            ExtrusionMode::Cut,
            -10.0,
        );

        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        assert!(app.stage_sketch_extrusion());
        assert!(app.confirm_pending_operation());
        assert_eq!(app.last_error_code(), None);
        assert_close(
            app.displayed_measures()
                .expect("concentric through-hole")
                .volume,
            boss_volume - std::f64::consts::PI * hole_radius * hole_radius * 5.0,
        );
    }

    #[test]
    fn selected_face_itself_pushes_and_pulls_without_a_surrogate_sketch() {
        for (distance, expected_volume, expected_area, expected_maximum_z, kind) in [
            (2.0, 36.0, 72.0, 6.0, FeaturePreviewKind::Add),
            (-1.0, 18.0, 42.0, 3.0, FeaturePreviewKind::Cut),
        ] {
            let mut app = KernelLabApp::default();
            let original = app.displayed_snapshot_id();
            let face = app
                .displayed
                .as_ref()
                .expect("bootstrap cuboid")
                .scene
                .triangles
                .iter()
                .find(|triangle| triangle.role == FaceRole::PositiveZ)
                .expect("positive Z face")
                .source_face;
            app.selected_face = Some(face);
            app.set_extrusion_distance_intent(distance);

            assert!(app.stage_face_push_pull());
            assert_eq!(
                app.pending_operation_label(),
                Some("Push/pull selected face")
            );
            assert!(app.current_feature_preview().is_some());
            assert_eq!(app.displayed_snapshot_id(), original);
            assert!(app.confirm_pending_operation());

            let committed = app.displayed_snapshot_id();
            assert_ne!(committed, original);
            assert_eq!(app.displayed_topology_counts().unwrap().faces, 6);
            assert_close(app.displayed_measures().unwrap().volume, expected_volume);
            assert_close(
                app.displayed_measures().unwrap().surface_area,
                expected_area,
            );
            assert_close(
                app.displayed.as_ref().unwrap().report.bounds.unwrap().max.z,
                expected_maximum_z,
            );
            assert_eq!(app.feature_preview.entries.last().unwrap().kind, kind);
            assert_eq!(app.model_body_kind, ModelBodyKind::PushedPulled);
            assert!(app.selected_face.is_some());
            assert!(app.pending_operation.is_none());
            assert!(app.move_history_cursor(2));
            assert_eq!(app.displayed_snapshot_id(), original);
            assert!(app.move_history_cursor(3));
            assert_eq!(app.displayed_snapshot_id(), committed);
            assert_eq!(app.model_body_kind, ModelBodyKind::PushedPulled);
        }
    }

    #[test]
    fn push_pull_rejection_retains_the_body_face_and_signed_intent() {
        let mut app = KernelLabApp::default();
        let face = app
            .displayed
            .as_ref()
            .expect("bootstrap cuboid")
            .scene
            .triangles
            .iter()
            .find(|triangle| triangle.role == FaceRole::PositiveZ)
            .expect("positive Z face")
            .source_face;
        let original = app.displayed_snapshot_id();
        app.selected_face = Some(face);
        app.set_extrusion_distance_intent(-5.0);

        assert!(app.stage_face_push_pull());
        assert!(app.confirm_pending_operation());

        assert_eq!(app.displayed_snapshot_id(), original);
        assert_eq!(app.selected_face, Some(face));
        assert_eq!(app.extrusion_distance, -5.0);
        assert_eq!(app.extrusion_mode, ExtrusionMode::Cut);
        assert!(matches!(
            app.pending_operation,
            Some(PendingOperation::PushPullFace { .. })
        ));
        assert!(app.sketch_extrusion_issue.is_some());
    }

    #[test]
    fn face_through_cut_reaches_the_opposite_boundary_and_commits() {
        let mut app = finished_face_rectangle_app(ExtrusionMode::Cut);
        app.set_extrusion_distance_intent(-4.0);
        let body = app.displayed_snapshot_id();
        app.stage_sketch_extrusion();

        assert!(app.confirm_pending_operation());
        assert_ne!(app.displayed_snapshot_id(), body);
        assert_close(app.displayed_measures().unwrap().volume, 18.0);
        assert!(app.pending_operation.is_none());
        assert_eq!(app.last_error_code(), None);
    }

    #[test]
    fn blind_cut_crosses_a_prior_boss_interface_without_hitting_its_shoulder() {
        let mut app = finished_face_rectangle_app(ExtrusionMode::Add);
        app.set_extrusion_distance_intent(1.0);
        assert!(app.stage_sketch_extrusion());
        assert!(app.confirm_pending_operation());
        assert_close(app.displayed_measures().unwrap().volume, 25.5);
        let boss_snapshot = app.displayed_snapshot_id();

        replace_with_finished_face_rectangle(
            &mut app,
            FaceRole::FeatureEnd,
            0.5,
            ExtrusionMode::Cut,
            -2.0,
        );
        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        assert!(app.stage_sketch_extrusion());
        assert!(app.confirm_pending_operation());

        assert_eq!(
            app.last_error_code(),
            None,
            "cross-interface cut was rejected: {:?}",
            app.sketch_extrusion_issue
        );
        assert_ne!(app.displayed_snapshot_id(), boss_snapshot);
        assert_close(app.displayed_measures().unwrap().volume, 24.75);
        assert_eq!(app.model_body_kind, ModelBodyKind::CutPocket);
        assert!(app.pending_operation.is_none());
    }

    /// Drives the canvas the way a hovering pointer does, without a frame.
    fn snap_at(sketch: &SketchCanvasState, target: SketchPoint) -> crate::sketch::SnapResult {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(600.0));
        sketch.snap_point(rect, sketch.view().sketch_to_screen(rect, target))
    }

    #[test]
    fn a_face_sketch_snaps_to_the_support_outline_it_is_drawn_on() {
        let mut app = KernelLabApp::default();
        let body = app.displayed.as_ref().expect("bootstrap body");
        let face = body
            .scene
            .triangles
            .iter()
            .find(|triangle| triangle.role == FaceRole::PositiveZ)
            .expect("positive Z face")
            .source_face;
        let support = NativeKernel::planar_face_support(&body.snapshot, face)
            .expect("selected face supports a sketch");
        let corner = SketchPoint::new(support.boundary[0].x, support.boundary[0].y);
        let next = SketchPoint::new(support.boundary[1].x, support.boundary[1].y);
        let edge_midpoint = SketchPoint::new(
            f64::midpoint(corner.u, next.u),
            f64::midpoint(corner.v, next.v),
        );
        app.selected_face = Some(face);
        assert!(!app.start_face_sketch_camera_transition(support));

        let curves = app.face_sketch_snap_curves().to_vec();
        assert_eq!(curves.len(), 4, "a rectangular face has four exact edges");
        assert!(
            curves
                .iter()
                .all(|curve| matches!(curve, SketchContextCurve::Segment { .. }))
        );
        app.sketch.set_support_curves(&curves);

        // Nudged off each target, the pointer still lands on it exactly.
        let vertex = snap_at(
            &app.sketch,
            SketchPoint::new(corner.u + 0.01, corner.v + 0.01),
        );
        assert_eq!(vertex.kind, crate::sketch::SnapKind::SupportEndpoint);
        assert_close(vertex.point.u, corner.u);
        assert_close(vertex.point.v, corner.v);

        let middle = snap_at(
            &app.sketch,
            SketchPoint::new(edge_midpoint.u + 0.01, edge_midpoint.v + 0.01),
        );
        assert_eq!(middle.kind, crate::sketch::SnapKind::SupportMidpoint);
        assert_close(middle.point.u, edge_midpoint.u);
        assert_close(middle.point.v, edge_midpoint.v);
    }

    #[test]
    fn a_drilled_hole_offers_its_exact_centre_to_the_sketch_that_follows_it() {
        let app = KernelLabApp::default();
        let body = app.displayed.as_ref().expect("bootstrap body");
        let face = body
            .scene
            .triangles
            .iter()
            .find(|triangle| triangle.role == FaceRole::PositiveZ)
            .expect("positive Z face")
            .source_face;
        let support = NativeKernel::planar_face_support(&body.snapshot, face)
            .expect("selected face supports a sketch");
        let hole_center = ProtocolPoint2::new(0.4, -0.3);
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: artificer_protocol::RequestId::new("sketch-snap-hole"),
            expected_snapshot: body.snapshot.id(),
            precision: artificer_protocol::PrecisionPolicy::default(),
            command: KernelCommand::DrillHole {
                target_face: face,
                frame: support.frame,
                center: hole_center,
                diameter: 0.5,
                depth: 10.0,
            },
        };
        let drilled = NativeKernel::execute(&body.snapshot, &request, &CancellationToken::new())
            .expect("through hole");
        let drilled_scene = NativeKernel::debug_scene(&drilled.snapshot);
        // A through hole leaves an inner loop on both caps, so keep the one
        // that is still the face the sketch was started on: its frame origin
        // stays in the original plane.
        let normal = [
            support
                .frame
                .u
                .y
                .mul_add(support.frame.v.z, -(support.frame.u.z * support.frame.v.y)),
            support
                .frame
                .u
                .z
                .mul_add(support.frame.v.x, -(support.frame.u.x * support.frame.v.z)),
            support
                .frame
                .u
                .x
                .mul_add(support.frame.v.y, -(support.frame.u.y * support.frame.v.x)),
        ];
        let annular = drilled_scene
            .triangles
            .iter()
            .find_map(|triangle| {
                NativeKernel::planar_face_support(&drilled.snapshot, triangle.source_face)
                    .ok()
                    .filter(|candidate| !candidate.inner_boundary_curves.is_empty())
                    .filter(|candidate| {
                        let offset = [
                            candidate.frame.origin.x - support.frame.origin.x,
                            candidate.frame.origin.y - support.frame.origin.y,
                            candidate.frame.origin.z - support.frame.origin.z,
                        ];
                        offset[0]
                            .mul_add(
                                normal[0],
                                offset[1].mul_add(normal[1], offset[2] * normal[2]),
                            )
                            .abs()
                            < 1.0e-9
                    })
            })
            .expect("the drilled face owns the hole loop");
        let context = project_face_sketch_context(&drilled_scene, &annular)
            .expect("the drilled face projects a sketch context");

        let mut sketch = SketchCanvasState::default();
        sketch.set_support_curves(&context.snap_curves);
        // The support frame recentres on the face, so the hole's centre moves
        // with it; snapping must report wherever the analytic circle now is.
        let expected = context
            .snap_curves
            .iter()
            .find_map(|curve| curve.center())
            .expect("the hole publishes an analytic centre");

        let snapped = snap_at(
            &sketch,
            SketchPoint::new(expected.u + 0.01, expected.v + 0.01),
        );

        assert_eq!(snapped.kind, crate::sketch::SnapKind::SupportCenter);
        assert_close(snapped.point.u, expected.u);
        assert_close(snapped.point.v, expected.v);

        // The two supports recentre on their own faces, so compare in world
        // space: the snapped centre must be the hole the caller drilled, to
        // the last bit, rather than an average of sampled rim points.
        let world = |frame: PlanarFrame3, u: f64, v: f64| {
            [
                u.mul_add(frame.u.x, v.mul_add(frame.v.x, frame.origin.x)),
                u.mul_add(frame.u.y, v.mul_add(frame.v.y, frame.origin.y)),
                u.mul_add(frame.u.z, v.mul_add(frame.v.z, frame.origin.z)),
            ]
        };
        let requested = world(support.frame, hole_center.x, hole_center.y);
        let reported = world(annular.frame, snapped.point.u, snapped.point.v);
        for axis in 0..3 {
            assert_close(reported[axis], requested[axis]);
        }

        // The rim itself is the exact analytic radius, not a chord fit.
        for curve in &context.snap_curves {
            if let SketchContextCurve::Arc { radius, .. } = curve {
                assert_close(*radius, 0.25);
            }
        }
    }

    #[test]
    fn committed_face_extrusion_invalidates_old_sketch_context_and_support() {
        let mut app = finished_face_rectangle_app(ExtrusionMode::Add);
        assert!(app.face_sketch_context.is_some());
        assert!(app.sketch_support_is_current());

        assert!(app.stage_sketch_extrusion());
        assert!(app.confirm_pending_operation());

        assert!(app.face_sketch_context.is_none());
        assert!(!app.sketch_support_is_current());
        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::StaleFaceSupport
        );
        app.enter_sketch_mode();
        assert_eq!(app.workbench_mode, WorkbenchMode::Model);
    }

    #[test]
    fn committed_transform_invalidates_old_face_sketch_context_and_support() {
        let mut app = finished_face_rectangle_app(ExtrusionMode::Add);
        assert!(app.face_sketch_context.is_some());
        assert!(app.sketch_support_is_current());
        app.display_transform.translation = [1.0, 0.0, 0.0];

        app.apply_transform_preview();

        assert!(app.face_sketch_context.is_none());
        assert!(!app.sketch_support_is_current());
        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::StaleFaceSupport
        );
    }

    #[test]
    fn finishing_an_open_sketch_persists_authoring_but_does_not_enable_extrude() {
        let mut app = KernelLabApp::default();
        app.sketch
            .stage_geometry(SketchGeometry::segment(point(0.0, 0.0), point(3.0, 1.0)))
            .expect("line should stage");
        app.sketch.commit_pending().expect("line should commit");
        app.sketch_revision = 1;
        app.feature_preview.commit_sketch_revision(1);

        app.stage_finish_sketch();
        assert!(app.confirm_pending_operation());
        assert!(app.sketch_finished);
        let sketch = app.document.sketches()[0].id;
        let payload = app
            .document
            .sketch_payload(sketch, 1)
            .expect("open sketch payload");
        assert!(payload.profile.regions.is_empty());
        assert_eq!(
            payload
                .authoring()
                .expect("editable graph")
                .active_entities()
                .count(),
            1
        );
        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::SketchNotFinished
        );
    }

    #[test]
    fn committed_browser_sketch_reselects_from_document_payload_and_extrudes() {
        let mut app = active_rectangle_app();
        app.stage_finish_sketch();
        assert!(app.confirm_pending_operation());
        let sketch_index = app.active_sketch_index.expect("committed sketch record");
        assert!(app.sketches[sketch_index].portable_payload.is_some());

        app.active_sketch_index = None;
        app.sketch = SketchCanvasState::default();
        assert!(app.activate_committed_sketch(sketch_index));
        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        assert!(app.stage_sketch_extrusion());
        assert!(matches!(
            app.pending_operation,
            Some(PendingOperation::ExtrudeSketch { .. })
        ));
    }

    #[test]
    fn committed_profile_region_anchor_supports_direct_model_selection_with_holes() {
        let circle = |radius: f64, clockwise: bool| {
            sample_planar_loop(&PlanarLoop2 {
                curves: vec![PlanarCurve2::Circle {
                    center: ProtocolPoint2::new(0.0, 0.0),
                    radius,
                    direction: if clockwise {
                        ArcDirection::Clockwise
                    } else {
                        ArcDirection::CounterClockwise
                    },
                }],
            })
            .unwrap()
        };
        let outer = circle(4.0, false);
        let hole = circle(2.0, true);
        let anchor = profile_region_anchor(&outer, std::slice::from_ref(&hole))
            .expect("annular material has a selectable interior point");
        assert!(point_in_planar_polygon(anchor, &outer));
        assert!(!point_in_planar_polygon(anchor, &hole));
    }

    #[test]
    fn history_groups_sketch_consumers_but_keeps_independent_sketches_separate() {
        let mut app = active_rectangle_app();
        app.stage_finish_sketch();
        assert!(app.confirm_pending_operation());
        let first_sketch = app.document.sketches()[0].id;
        let first_sketch_feature = app.document.sketch(first_sketch).unwrap().last_feature;
        let sketch_index = app.active_sketch_index.unwrap();
        assert!(app.activate_committed_sketch(sketch_index));
        assert!(app.stage_sketch_extrusion());
        assert!(app.confirm_pending_operation());
        let extrusion_feature = app.document.features().last().unwrap().id;
        let body = app.active_body_id().unwrap();
        let mut independent_payload = app
            .document
            .feature(first_sketch_feature)
            .and_then(|feature| feature.sketch_payload.clone())
            .expect("first sketch carries a portable payload");
        let support_face = app.displayed.as_ref().unwrap().scene.triangles[0].source_face;
        independent_payload.support = SketchSupportRecipe::PlanarFace {
            body,
            face: app
                .persistent_ref_for_current_face(support_face)
                .expect("current face has a persistent support identity"),
        };

        let mut next_document = app.document.clone();
        let second = next_document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::Sketch,
                    "Independent sketch",
                    ReplayAction::Marker,
                )
                .with_input(FeatureInput::Body(body))
                .with_sketch_payload(independent_payload)
                .with_output(OutputDraft::CreateSketch {
                    label: "Independent sketch".to_owned(),
                    geometry_revision: 1,
                }),
            )
            .expect("second sketch on the same body is a valid separate branch");
        app.document = next_document;
        app.sync_feature_preview_from_document();

        let feature_group = |feature: FeatureId, app: &KernelLabApp| {
            let index = app
                .document
                .features()
                .iter()
                .position(|candidate| candidate.id == feature)
                .unwrap();
            app.feature_preview.entries[index].group
        };
        assert_eq!(
            feature_group(first_sketch_feature, &app),
            feature_group(extrusion_feature, &app),
            "the extrusion inherits its source sketch group"
        );
        assert_ne!(
            feature_group(first_sketch_feature, &app),
            feature_group(second.feature, &app),
            "same support face/body does not merge independent sketch intent"
        );
    }

    #[test]
    fn finishing_an_edited_sketch_replaces_one_logical_history_feature() {
        let mut app = active_rectangle_app();
        app.stage_finish_sketch();
        assert!(app.confirm_pending_operation());
        let sketch = app.document.sketches()[0].id;
        let original_feature_count = app.document.features().len();

        app.workbench_mode = WorkbenchMode::Sketch;
        app.sketch
            .stage_geometry(SketchGeometry::segment(point(6.0, 0.0), point(8.0, 0.0)))
            .expect("open line should stage");
        app.sketch
            .commit_pending()
            .expect("open line should commit");
        app.sketch_revision = 2;
        app.sketch_finished = false;
        app.stage_finish_sketch();
        assert!(app.confirm_pending_operation());

        assert_eq!(app.document.features().len(), original_feature_count);
        assert_eq!(app.document.sketch(sketch).unwrap().geometry_revision, 2);
        let sketch_feature = app.document.sketch(sketch).unwrap().last_feature;
        assert_eq!(app.document_dirty_feature_count(), 1);
        let payload = app
            .document
            .sketch_payload(sketch, 2)
            .expect("modified exact payload");
        assert_eq!(
            payload
                .authoring()
                .expect("editable graph")
                .operations()
                .len(),
            2
        );
        assert!(!payload.profile.regions.is_empty());
        assert!(app.rebuild_document_from(sketch_feature));
        assert_eq!(app.document_dirty_feature_count(), 0);
    }

    #[test]
    fn loaded_revision_five_edit_undo_redo_finishes_as_newer_geometry() {
        let mut source = active_rectangle_app();
        source.stage_finish_sketch();
        assert!(source.confirm_pending_operation());

        // Build genuine document revisions rather than assigning a canvas
        // counter, so the serialized fixture exercises the same v6 hydration
        // boundary as a user-authored file.
        for expected_revision in 2..=5 {
            source.workbench_mode = WorkbenchMode::Sketch;
            let offset = expected_revision as f64 * 2.0;
            let entity = source
                .sketch
                .stage_geometry(SketchGeometry::segment(
                    point(offset, 5.0),
                    point(offset + 1.0, 5.0),
                ))
                .expect("stage revision fixture geometry");
            source.stage_sketch_edit(entity);
            assert!(source.confirm_pending_operation());
            assert_eq!(source.sketch_revision, expected_revision);
            source.stage_finish_sketch();
            assert!(source.confirm_pending_operation());
            let sketch = source.document.sketches()[0].id;
            let feature = source.document.sketch(sketch).unwrap().last_feature;
            assert!(source.rebuild_document_from(feature));
        }
        let sketch_id = source.document.sketches()[0].id;
        assert_eq!(
            source.document.sketch(sketch_id).unwrap().geometry_revision,
            5
        );

        let encoded = source
            .native_document_json()
            .expect("serialize revision five");
        let mut restored = KernelLabApp::default();
        restored
            .load_native_document_json(&encoded)
            .expect("hydrate revision five");
        let sketch_index = restored.active_sketch_index.expect("hydrated sketch");
        assert!(restored.activate_committed_sketch(sketch_index));
        restored.enter_sketch_mode();
        assert_eq!(restored.sketch_revision, 5);
        assert!(!restored.sketch.can_undo_local());

        let entity = restored
            .sketch
            .stage_geometry(SketchGeometry::segment(point(20.0, 5.0), point(21.0, 5.0)))
            .expect("stage hydrated edit");
        restored.stage_sketch_edit(entity);
        assert!(restored.confirm_pending_operation());
        assert_eq!(restored.sketch_revision, 6);
        assert!(restored.restore_local_sketch_journal(false));
        assert_eq!(restored.sketch_revision, 7);
        assert!(restored.restore_local_sketch_journal(true));
        assert_eq!(restored.sketch_revision, 8);

        restored.stage_finish_sketch();
        assert!(restored.confirm_pending_operation());
        assert_eq!(
            restored
                .document
                .sketch(sketch_id)
                .unwrap()
                .geometry_revision,
            6
        );
        assert!(restored.document.sketch_payload(sketch_id, 6).is_some());
    }

    #[test]
    fn document_parameter_creation_waits_for_visible_confirmation() {
        let mut app = KernelLabApp::default();
        let before = app.document.parameters().len();
        app.pending_operation = Some(PendingOperation::AddUserLengthParameter {
            ordinal: 1,
            value_mm: 12.5,
        });
        assert_eq!(app.document.parameters().len(), before);
        assert!(app.confirm_pending_operation());
        assert_eq!(app.document.parameters().len(), before + 1);
        let record = app
            .document
            .parameters()
            .get_by_key("UserLength1")
            .expect("confirmed parameter");
        assert_eq!(
            record.binding,
            ParameterBinding::literal(ParameterValue::quantity(12.5, ParameterUnit::Millimeter))
        );
    }

    #[test]
    fn document_parameter_edit_can_be_cancelled_or_confirmed_atomically() {
        let mut app = KernelLabApp::default();
        let parameter = app
            .document
            .add_parameter(
                ParameterSpec::new(
                    "Width",
                    "Width",
                    ParameterType::Quantity(QuantityKind::Length),
                )
                .with_display_unit(ParameterUnit::Millimeter),
                ParameterBinding::literal(ParameterValue::quantity(4.0, ParameterUnit::Millimeter)),
            )
            .expect("parameter");
        let base = ParameterLiteralDraft::Quantity {
            magnitude: 4.0,
            unit: ParameterUnit::Millimeter,
        };
        app.pending_operation = Some(PendingOperation::SetParameterLiteral {
            parameter,
            base,
            value: ParameterLiteralDraft::Quantity {
                magnitude: 9.0,
                unit: ParameterUnit::Millimeter,
            },
        });
        assert!(app.cancel_pending_operation());
        assert_eq!(
            app.document.parameter(parameter).unwrap().binding,
            ParameterBinding::literal(ParameterValue::quantity(4.0, ParameterUnit::Millimeter))
        );

        app.pending_operation = Some(PendingOperation::SetParameterLiteral {
            parameter,
            base,
            value: ParameterLiteralDraft::Quantity {
                magnitude: 9.0,
                unit: ParameterUnit::Millimeter,
            },
        });
        assert!(app.confirm_pending_operation());
        assert_eq!(
            app.document.parameter(parameter).unwrap().binding,
            ParameterBinding::literal(ParameterValue::quantity(9.0, ParameterUnit::Millimeter))
        );
    }

    /// Appends one committed cuboid body to the workspace and its history.
    fn push_boolean_operand(app: &mut KernelLabApp, label: &str, origin: Point3) -> BodyId {
        let empty = NativeKernel::empty();
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new(format!("ui-boolean-{label}")),
            expected_snapshot: empty.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeCuboid {
                origin,
                size_x: 2.0,
                size_y: 2.0,
                size_z: 2.0,
            },
        };
        let outcome =
            NativeKernel::execute(&empty, &request, &CancellationToken::new()).expect("tool body");
        let association = SnapshotAssociation::new(
            outcome.report.input_snapshot,
            outcome.report.output_snapshot,
            outcome.report.semantic_digest,
        );
        let appended = app
            .document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    label,
                    ReplayAction::Kernel(request.command),
                )
                .with_output(OutputDraft::CreateBody {
                    label: label.to_owned(),
                })
                .with_commit(association),
            )
            .expect("tool history");
        let id = appended.created_bodies[0];
        let ordinal = app.next_body_ordinal;
        app.next_body_ordinal += 1;
        app.bodies.push(WorkbenchBody {
            id,
            last_feature: appended.feature,
            ordinal,
            body: DisplayedBody {
                scene: NativeKernel::debug_scene(&outcome.snapshot),
                snapshot: outcome.snapshot,
                report: outcome.report,
            },
            kind: ModelBodyKind::Cuboid,
            visible: true,
            material: None,
        });
        id
    }

    fn boolean_feature_count(app: &KernelLabApp) -> usize {
        app.document
            .active_features()
            .iter()
            .filter(|feature| matches!(feature.action, ReplayAction::Boolean(_)))
            .count()
    }

    #[test]
    fn a_staged_boolean_refuses_to_commit_until_a_tool_is_picked() {
        let mut app = KernelLabApp::default();
        push_boolean_operand(&mut app, "Tool", Point3::new(0.9, 1.1, 1.2));

        app.stage_body_boolean(BooleanOperation::Union);

        assert!(app.boolean_tools.is_empty(), "staging must not guess");
        assert!(app.boolean_confirmation_blocked());
        app.confirm_pending_operation();
        assert!(
            app.pending_operation.is_some(),
            "an operandless Boolean stays staged instead of committing"
        );
        assert_eq!(boolean_feature_count(&app), 0);
    }

    #[test]
    fn the_target_body_cannot_be_picked_as_its_own_tool() {
        let mut app = KernelLabApp::default();
        push_boolean_operand(&mut app, "Tool", Point3::new(0.9, 1.1, 1.2));
        let target = app.active_body_id().expect("bootstrap body is the target");
        app.stage_body_boolean(BooleanOperation::Difference);

        // The click is consumed — it must not fall through to selection — but
        // it adds nothing, because a body cannot cut itself.
        assert!(app.toggle_boolean_tool(target));
        assert!(app.boolean_tools.is_empty());
        assert!(app.boolean_confirmation_blocked());
    }

    #[test]
    fn picking_a_tool_twice_removes_it_again() {
        let mut app = KernelLabApp::default();
        let tool = push_boolean_operand(&mut app, "Tool", Point3::new(0.9, 1.1, 1.2));
        app.stage_body_boolean(BooleanOperation::Union);

        assert!(app.toggle_boolean_tool(tool));
        assert_eq!(app.boolean_tools, vec![tool]);
        assert!(app.toggle_boolean_tool(tool));
        assert!(app.boolean_tools.is_empty());
    }

    #[test]
    fn two_picked_tools_commit_as_two_replayable_boolean_features() {
        let mut app = KernelLabApp::default();
        let first = push_boolean_operand(&mut app, "First", Point3::new(0.9, 1.1, 1.2));
        let second = push_boolean_operand(&mut app, "Second", Point3::new(-1.5, 0.3, 0.4));
        let target = app.active_body_id().expect("bootstrap body is the target");
        let before = app.displayed_measures().expect("target measures").volume;

        app.stage_body_boolean(BooleanOperation::Union);
        assert!(app.toggle_boolean_tool(first));
        assert!(app.toggle_boolean_tool(second));
        assert!(app.confirm_pending_operation());

        assert!(app.pending_operation.is_none());
        assert!(app.boolean_tools.is_empty(), "picks clear once committed");
        assert_eq!(
            boolean_feature_count(&app),
            2,
            "each tool is its own two-body recipe so replay can reproduce it"
        );
        assert!(app.displayed_measures().expect("result measures").volume > before);
        // Consumed by default: neither tool survives as a visible body.
        for tool in [first, second] {
            assert!(
                !app.bodies
                    .iter()
                    .find(|body| body.id == tool)
                    .expect("tool body")
                    .visible
            );
        }
        assert_eq!(app.active_body_id(), Some(target));
    }

    #[test]
    fn keeping_tools_leaves_every_operand_in_the_workspace() {
        let mut app = KernelLabApp::default();
        let tool = push_boolean_operand(&mut app, "Tool", Point3::new(0.9, 1.1, 1.2));
        app.stage_body_boolean(BooleanOperation::Difference);
        app.set_boolean_keep_tools(true);
        assert!(app.toggle_boolean_tool(tool));

        assert!(app.confirm_pending_operation());

        assert!(
            app.bodies
                .iter()
                .find(|body| body.id == tool)
                .expect("tool body")
                .visible,
            "Keep tools must leave the operand behind"
        );
    }

    #[test]
    fn a_boolean_that_fails_partway_publishes_nothing() {
        let mut app = KernelLabApp::default();
        let good = push_boolean_operand(&mut app, "Good", Point3::new(1.0, 1.0, 1.0));
        let target = app.active_body_id().expect("bootstrap body is the target");
        let before_volume = app.displayed_measures().expect("target measures").volume;
        let vanished = push_boolean_operand(&mut app, "Vanished", Point3::new(4.0, 4.0, 4.0));
        app.bodies.retain(|body| body.id != vanished);
        let before_features = app.document.active_features().len();

        // The second operand no longer resolves. The first would have
        // succeeded on its own, so this is exactly the partial-failure case.
        app.execute_body_boolean(target, &[good, vanished], BooleanOperation::Union, false);

        assert_eq!(app.document.active_features().len(), before_features);
        assert_eq!(boolean_feature_count(&app), 0);
        assert_eq!(
            app.displayed_measures().expect("target measures").volume,
            before_volume
        );
        assert!(
            app.bodies
                .iter()
                .find(|body| body.id == good)
                .expect("tool body")
                .visible,
            "a refused Boolean consumes nothing"
        );
    }

    #[test]
    fn body_boolean_is_staged_confirmed_and_replayable() {
        let mut app = KernelLabApp::default();
        let empty = NativeKernel::empty();
        let request = ExecuteRequest {
            protocol_version: CURRENT_PROTOCOL_VERSION,
            request_id: RequestId::new("ui-boolean-tool"),
            expected_snapshot: empty.id(),
            precision: PrecisionPolicy::default(),
            command: KernelCommand::MakeCuboid {
                // Transverse to every base-body plane: coincident contact is
                // outside the regularized Boolean domain and refuses.
                origin: Point3::new(0.9, 1.1, 1.2),
                size_x: 2.0,
                size_y: 2.0,
                size_z: 2.0,
            },
        };
        let tool =
            NativeKernel::execute(&empty, &request, &CancellationToken::new()).expect("tool body");
        let association = SnapshotAssociation::new(
            tool.report.input_snapshot,
            tool.report.output_snapshot,
            tool.report.semantic_digest,
        );
        let appended = app
            .document
            .append_feature(
                FeatureDraft::new(
                    FeatureKind::BaseBody,
                    "Boolean tool",
                    ReplayAction::Kernel(request.command),
                )
                .with_output(OutputDraft::CreateBody {
                    label: "Tool".to_owned(),
                })
                .with_commit(association),
            )
            .expect("tool history");
        let tool_id = appended.created_bodies[0];
        app.next_body_ordinal += 1;
        app.bodies.push(WorkbenchBody {
            id: tool_id,
            last_feature: appended.feature,
            ordinal: app.next_body_ordinal - 1,
            body: DisplayedBody {
                scene: NativeKernel::debug_scene(&tool.snapshot),
                snapshot: tool.snapshot,
                report: tool.report,
            },
            kind: ModelBodyKind::Cuboid,
            visible: true,
            material: None,
        });

        app.stage_body_boolean(BooleanOperation::Union);
        assert!(matches!(
            app.pending_operation,
            Some(PendingOperation::BooleanBodies {
                operation: BooleanOperation::Union,
                keep_tools: false,
                ..
            })
        ));
        // Staging no longer guesses an operand; the tool is named by a pick.
        assert!(app.boolean_confirmation_blocked());
        assert!(app.toggle_boolean_tool(tool_id));
        assert!(!app.boolean_confirmation_blocked());
        assert!(app.confirm_pending_operation());
        assert!(matches!(
            app.document.active_features().last().unwrap().action,
            ReplayAction::Boolean(_)
        ));
        assert!(
            !app.bodies
                .iter()
                .find(|body| body.id == tool_id)
                .unwrap()
                .visible
        );
        assert!(app.displayed_measures().unwrap().volume > 24.0);

        let json = app
            .native_document_json()
            .expect("serialize Boolean history");
        let hydrated = document_replay::hydrate_document_json_with_options(
            &json,
            document_replay::HydrationOptions::default(),
        )
        .expect("replay Boolean history");
        assert!(
            hydrated
                .document
                .active_features()
                .iter()
                .any(|feature| { matches!(feature.action, ReplayAction::Boolean(_)) })
        );
    }

    #[test]
    fn solid_feature_presets_stage_before_committing_to_history() {
        let mut revolve = KernelLabApp::default();
        let bodies_before = revolve.body_count();
        revolve.stage_preset_feature(SolidFeaturePreset::Revolve);
        assert_eq!(revolve.body_count(), bodies_before);
        assert!(revolve.confirm_pending_operation());
        assert_eq!(revolve.body_count(), bodies_before + 1);

        let mut mirror = KernelLabApp::default();
        mirror.stage_preset_feature(SolidFeaturePreset::Mirror);
        assert!(mirror.confirm_pending_operation());
        assert!(mirror.displayed_measures().unwrap().centroid.unwrap().x < 0.0);

        let mut pattern = KernelLabApp::default();
        pattern.stage_preset_feature(SolidFeaturePreset::LinearPattern);
        assert!(pattern.confirm_pending_operation());
        assert_eq!(
            pattern.displayed.as_ref().unwrap().snapshot.counts().solids,
            3
        );

        for (preset, adds_material) in [
            (SolidFeaturePreset::Hole, false),
            (SolidFeaturePreset::Rib, true),
        ] {
            let mut app = KernelLabApp::default();
            app.selected_face = app
                .displayed
                .as_ref()
                .and_then(|displayed| displayed.scene.triangles.first())
                .map(|triangle| triangle.source_face);
            app.stage_preset_feature(preset);
            assert!(app.confirm_pending_operation());
            let volume = app.displayed_measures().unwrap().volume;
            assert_eq!(volume > 24.0, adds_material, "{preset:?}: {volume}");
        }
    }

    #[test]
    fn selected_edge_stages_exact_chamfer_and_fillet_with_persistent_history() {
        for preset in [SolidFeaturePreset::Chamfer, SolidFeaturePreset::Fillet] {
            let mut app = KernelLabApp::default();
            let body = app.active_body_id().expect("default active body");
            let edge = app
                .displayed
                .as_ref()
                .and_then(|displayed| displayed.scene.edges.first())
                .expect("default body edge")
                .source_edge;
            let selection = viewport::DocumentEdgeSelection {
                body: viewport::BodyInstanceKey::new(body.get()),
                edge,
            };
            app.selected_edge = Some(selection);
            app.selected_edges.push(selection);

            app.stage_preset_feature(preset);
            assert!(matches!(
                app.pending_operation,
                Some(PendingOperation::PresetFeature { preset: staged, .. }) if staged == preset
            ));
            assert!(app.confirm_pending_operation(), "{preset:?}");
            assert!(app.displayed_measures().unwrap().volume < 24.0);
            let finished_snapshot = app.displayed_snapshot_id();
            let history_end = app.document.history_position();
            assert!(app.move_history_cursor(2), "{preset:?} back to base");
            assert!(
                app.displayed.is_some(),
                "{preset:?} base remains renderable"
            );
            assert!(
                app.move_history_cursor(history_end),
                "{preset:?} forward to finish"
            );
            assert_eq!(app.displayed_snapshot_id(), finished_snapshot);
            let ReplayAction::TargetedKernel(targeted) =
                &app.document.active_features().last().unwrap().action
            else {
                panic!("edge finish must be a persistent targeted action")
            };
            assert_eq!(targeted.target().kind, EntityKind::Edge);
            assert!(app.selected_edge.is_none());
            assert!(app.selected_edges.is_empty());

            let json = app.native_document_json().expect("serialize edge finish");
            let hydrated = document_replay::hydrate_document_json_with_options(
                &json,
                document_replay::HydrationOptions::default(),
            )
            .expect("replay edge finish");
            assert!(
                hydrated
                    .branch_snapshot(body)
                    .expect("hydrated finished body")
                    .measures()
                    .volume
                    < 24.0
            );
        }
    }

    #[test]
    fn inspect_annotations_report_exact_edge_length_and_face_area() {
        let mut app = KernelLabApp::default();
        let body = app.active_body_id().expect("default active body");
        let displayed = app.displayed.as_ref().expect("default displayed body");
        let edge = displayed.scene.edges[0].source_edge;
        let face = displayed.scene.triangles[0].source_face;
        let body_key = viewport::BodyInstanceKey::new(body.get());

        app.measured_edges.push(viewport::DocumentEdgeSelection {
            body: body_key,
            edge,
        });
        let viewport::DocumentMeasurement::Edge { label, .. } = app
            .current_measurement_annotation()
            .expect("edge annotation")
        else {
            panic!("single edge should produce an edge annotation")
        };
        assert!(label.starts_with("L ") && label.ends_with(" mm"), "{label}");

        app.measured_edges.clear();
        app.measured_face = Some(viewport::DocumentFaceSelection {
            body: body_key,
            face,
        });
        let viewport::DocumentMeasurement::Face { label, .. } = app
            .current_measurement_annotation()
            .expect("face annotation")
        else {
            panic!("single face should produce a face annotation")
        };
        assert!(
            label.starts_with("A ") && label.ends_with(" mm²"),
            "{label}"
        );
    }

    #[test]
    fn additive_model_selection_toggles_typed_faces_edges_and_vertices() {
        let mut app = KernelLabApp::default();
        let body = viewport::BodyInstanceKey::new(app.active_body_id().unwrap().get());
        let displayed = app.displayed.as_ref().unwrap();
        let edge = viewport::DocumentEdgeSelection {
            body,
            edge: displayed.scene.edges[0].source_edge,
        };
        let face = viewport::DocumentFaceSelection {
            body,
            face: displayed.scene.triangles[0].source_face,
        };
        let vertex = viewport::DocumentVertexSelection {
            body,
            vertex: displayed.scene.vertices[0].source_vertex,
        };

        app.select_model_edge(edge, false);
        app.select_model_face(face, true);
        app.select_model_vertex(vertex, true);
        assert_eq!(app.selected_edges, vec![edge]);
        assert_eq!(app.selected_faces, vec![face]);
        assert_eq!(app.selected_vertices, vec![vertex]);

        app.select_model_edge(edge, true);
        assert!(app.selected_edges.is_empty());
        assert_eq!(app.selected_faces, vec![face]);
        assert_eq!(app.selected_vertices, vec![vertex]);
    }

    #[test]
    fn coplanar_nonparallel_edge_measurement_includes_the_angle() {
        let mut app = KernelLabApp::default();
        let body = viewport::BodyInstanceKey::new(app.active_body_id().unwrap().get());
        let edges = &app.displayed.as_ref().unwrap().scene.edges;
        let near = |left: Point3, right: Point3| model_segment_length([left, right]) <= 1.0e-9;
        let pair = edges
            .iter()
            .enumerate()
            .find_map(|(index, first)| {
                edges[index + 1..].iter().find_map(|second| {
                    let shared = first
                        .endpoints
                        .into_iter()
                        .any(|left| second.endpoints.into_iter().any(|right| near(left, right)));
                    let direction = |segment: [Point3; 2]| {
                        [
                            segment[1].x - segment[0].x,
                            segment[1].y - segment[0].y,
                            segment[1].z - segment[0].z,
                        ]
                    };
                    let first_direction = direction(first.endpoints);
                    let second_direction = direction(second.endpoints);
                    let dot = first_direction[0].mul_add(
                        second_direction[0],
                        first_direction[1].mul_add(
                            second_direction[1],
                            first_direction[2] * second_direction[2],
                        ),
                    );
                    (shared && dot.abs() <= 1.0e-9)
                        .then_some((first.source_edge, second.source_edge))
                })
            })
            .expect("perpendicular cuboid edges");
        app.measured_edges = vec![
            viewport::DocumentEdgeSelection { body, edge: pair.0 },
            viewport::DocumentEdgeSelection { body, edge: pair.1 },
        ];
        assert_close(app.measured_edge_angle_degrees().unwrap(), 90.0);
        let viewport::DocumentMeasurement::EdgeDistance { label, .. } =
            app.current_measurement_annotation().unwrap()
        else {
            panic!("two edges should produce a distance annotation")
        };
        assert!(label.contains("∠ 90.000°"), "{label}");
    }

    #[test]
    fn parallel_multi_edge_finish_is_one_persistent_replayable_feature() {
        let mut app = KernelLabApp::default();
        let body_id = app.active_body_id().unwrap();
        let body = viewport::BodyInstanceKey::new(body_id.get());
        let scene = &app.displayed.as_ref().unwrap().scene;
        let axis = |edge: &artificer_kernel::DebugEdge| {
            let delta = [
                (edge.endpoints[1].x - edge.endpoints[0].x).abs(),
                (edge.endpoints[1].y - edge.endpoints[0].y).abs(),
                (edge.endpoints[1].z - edge.endpoints[0].z).abs(),
            ];
            delta
                .iter()
                .enumerate()
                .max_by(|left, right| left.1.total_cmp(right.1))
                .unwrap()
                .0
        };
        app.selected_edges = scene
            .edges
            .iter()
            .filter(|edge| axis(edge) == 0)
            .map(|edge| viewport::DocumentEdgeSelection {
                body,
                edge: edge.source_edge,
            })
            .collect();
        app.selected_edge = app.selected_edges.last().copied();
        app.stage_preset_feature(SolidFeaturePreset::Fillet);
        assert!(app.confirm_pending_operation());
        let ReplayAction::TargetedKernel(targeted) =
            &app.document.active_features().last().unwrap().action
        else {
            panic!("multi-edge finish must remain persistently targeted")
        };
        assert_eq!(targeted.targets().count(), 4);
        let json = app.native_document_json().unwrap();
        let hydrated = document_replay::hydrate_document_json_with_options(
            &json,
            document_replay::HydrationOptions::default(),
        )
        .expect("multi-edge replay");
        assert!(
            NativeKernel::validate(
                hydrated.branch_snapshot(body_id).unwrap(),
                artificer_protocol::ValidationProfile::Solid,
            )
            .valid
        );
    }

    #[test]
    fn edge_finish_preview_is_the_exact_candidate_and_tints_only_new_surfaces() {
        let mut app = KernelLabApp::default();
        let body_id = app.active_body_id().unwrap();
        let body = viewport::BodyInstanceKey::new(body_id.get());
        let edge = app.displayed.as_ref().unwrap().scene.edges[0];
        app.selected_edges = vec![viewport::DocumentEdgeSelection {
            body,
            edge: edge.source_edge,
        }];
        app.selected_edge = app.selected_edges.last().copied();
        app.edge_finish_distance = 0.2;
        app.stage_preset_feature(SolidFeaturePreset::Fillet);

        let preview = app.current_edge_finish_preview().expect("staged preview");
        let candidate = preview
            .candidate
            .expect("paused workbench evaluates the exact candidate synchronously");
        assert_eq!(preview.source_segments, vec![edge.endpoints]);
        assert!(
            !candidate.changed_faces.is_empty(),
            "the new fillet surface must receive the preview tint"
        );
        let candidate_faces = candidate
            .scene
            .triangles
            .iter()
            .map(|triangle| triangle.source_face)
            .collect::<BTreeSet<_>>();
        assert!(
            candidate.changed_faces.len() < candidate_faces.len(),
            "retained source planes must stay normally shaded"
        );
        let preview_digest = candidate.scene.semantic_digest;

        assert!(app.confirm_pending_operation());
        assert_eq!(
            app.displayed.as_ref().unwrap().scene.semantic_digest,
            preview_digest,
            "confirmation must publish exactly the privately previewed body"
        );
    }

    #[test]
    fn connected_u_chamfer_on_a_filleted_successor_uses_the_exact_preview_body() {
        let mut app = KernelLabApp::default();
        let body_id = app.active_body_id().unwrap();
        let body = viewport::BodyInstanceKey::new(body_id.get());
        let scene = &app.displayed.as_ref().unwrap().scene;
        let minimum = Point3::new(
            scene
                .vertices
                .iter()
                .map(|vertex| vertex.point.x)
                .fold(f64::INFINITY, f64::min),
            scene
                .vertices
                .iter()
                .map(|vertex| vertex.point.y)
                .fold(f64::INFINITY, f64::min),
            scene
                .vertices
                .iter()
                .map(|vertex| vertex.point.z)
                .fold(f64::INFINITY, f64::min),
        );
        let near = |left: f64, right: f64| (left - right).abs() <= 1.0e-9;
        let at_minimum = |point: Point3| {
            near(point.x, minimum.x) && near(point.y, minimum.y) && near(point.z, minimum.z)
        };
        app.selected_edges = scene
            .edges
            .iter()
            .filter(|edge| edge.endpoints.iter().copied().any(at_minimum))
            .map(|edge| viewport::DocumentEdgeSelection {
                body,
                edge: edge.source_edge,
            })
            .collect();
        assert_eq!(app.selected_edges.len(), 3);
        app.selected_edge = app.selected_edges.last().copied();
        app.edge_finish_distance = 0.25;
        app.stage_preset_feature(SolidFeaturePreset::Fillet);
        assert!(app.confirm_pending_operation());

        let scene = &app.displayed.as_ref().unwrap().scene;
        let maximum = Point3::new(
            scene
                .vertices
                .iter()
                .map(|vertex| vertex.point.x)
                .fold(f64::NEG_INFINITY, f64::max),
            scene
                .vertices
                .iter()
                .map(|vertex| vertex.point.y)
                .fold(f64::NEG_INFINITY, f64::max),
            scene
                .vertices
                .iter()
                .map(|vertex| vertex.point.z)
                .fold(f64::NEG_INFINITY, f64::max),
        );
        let axis = |edge: &artificer_kernel::DebugEdge| {
            let delta = [
                (edge.endpoints[1].x - edge.endpoints[0].x).abs(),
                (edge.endpoints[1].y - edge.endpoints[0].y).abs(),
                (edge.endpoints[1].z - edge.endpoints[0].z).abs(),
            ];
            delta
                .iter()
                .enumerate()
                .max_by(|left, right| left.1.total_cmp(right.1))
                .unwrap()
                .0
        };
        let on_top = |edge: &artificer_kernel::DebugEdge| {
            edge.endpoints.iter().all(|point| near(point.z, maximum.z))
        };
        let u_edges = scene
            .edges
            .iter()
            .filter(|edge| !edge.is_smooth && on_top(edge))
            .filter(|edge| match axis(edge) {
                0 => edge
                    .endpoints
                    .iter()
                    .all(|point| near(point.y, minimum.y) || near(point.y, maximum.y)),
                1 => edge.endpoints.iter().all(|point| near(point.x, maximum.x)),
                _ => false,
            })
            .map(|edge| viewport::DocumentEdgeSelection {
                body,
                edge: edge.source_edge,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            u_edges.len(),
            3,
            "the top perimeter contributes one U chain"
        );
        app.selected_edges = u_edges;
        app.selected_edge = app.selected_edges.last().copied();
        app.edge_finish_distance = 0.25;
        app.stage_preset_feature(SolidFeaturePreset::Chamfer);

        let preview = app
            .current_edge_finish_preview()
            .expect("U chamfer preview");
        assert_eq!(preview.kind, EdgeFinishKind::Chamfer);
        let candidate = preview
            .candidate
            .expect("chamfer must substitute the same exact body as fillet preview");
        assert!(!candidate.changed_faces.is_empty());
        let preview_digest = candidate.scene.semantic_digest;
        assert!(app.confirm_pending_operation());
        assert_eq!(
            app.displayed.as_ref().unwrap().scene.semantic_digest,
            preview_digest,
            "the confirmed U chamfer must be the body that was previewed"
        );
    }

    #[test]
    fn a_committed_chamfer_boundary_can_preview_and_commit_a_stacked_fillet() {
        let mut app = KernelLabApp::default();
        let body_id = app.active_body_id().unwrap();
        let body = viewport::BodyInstanceKey::new(body_id.get());
        let source = app.displayed.as_ref().unwrap().scene.edges[0];
        let source_direction = [
            source.endpoints[1].x - source.endpoints[0].x,
            source.endpoints[1].y - source.endpoints[0].y,
            source.endpoints[1].z - source.endpoints[0].z,
        ];
        let source_length = model_segment_length(source.endpoints);
        let source_midpoint = Point3::new(
            (source.endpoints[0].x + source.endpoints[1].x) * 0.5,
            (source.endpoints[0].y + source.endpoints[1].y) * 0.5,
            (source.endpoints[0].z + source.endpoints[1].z) * 0.5,
        );
        app.selected_edges = vec![viewport::DocumentEdgeSelection {
            body,
            edge: source.source_edge,
        }];
        app.selected_edge = app.selected_edges.last().copied();
        app.edge_finish_distance = 0.3;
        app.stage_preset_feature(SolidFeaturePreset::Chamfer);
        assert!(app.confirm_pending_operation());

        let scene = &app.displayed.as_ref().unwrap().scene;
        let parallel = |edge: &artificer_kernel::DebugEdge| {
            let direction = [
                edge.endpoints[1].x - edge.endpoints[0].x,
                edge.endpoints[1].y - edge.endpoints[0].y,
                edge.endpoints[1].z - edge.endpoints[0].z,
            ];
            let denominator = model_segment_length(edge.endpoints) * source_length;
            denominator > f64::EPSILON
                && (direction[0].mul_add(
                    source_direction[0],
                    direction[1].mul_add(source_direction[1], direction[2] * source_direction[2]),
                ) / denominator)
                    .abs()
                    >= 1.0 - 1.0e-8
        };
        let rail = scene
            .edges
            .iter()
            .filter(|edge| !edge.is_smooth && parallel(edge))
            .filter(|edge| (model_segment_length(edge.endpoints) - source_length).abs() <= 1.0e-8)
            .min_by(|left, right| {
                let clearance = |edge: &artificer_kernel::DebugEdge| {
                    let midpoint = Point3::new(
                        (edge.endpoints[0].x + edge.endpoints[1].x) * 0.5,
                        (edge.endpoints[0].y + edge.endpoints[1].y) * 0.5,
                        (edge.endpoints[0].z + edge.endpoints[1].z) * 0.5,
                    );
                    model_segment_length([source_midpoint, midpoint])
                };
                clearance(left).total_cmp(&clearance(right))
            })
            .expect("the committed chamfer exposes a longitudinal boundary rail");
        app.selected_edges = vec![viewport::DocumentEdgeSelection {
            body,
            edge: rail.source_edge,
        }];
        app.selected_edge = app.selected_edges.last().copied();
        app.edge_finish_distance = 0.1;
        app.stage_preset_feature(SolidFeaturePreset::Fillet);
        let preview = app
            .current_edge_finish_preview()
            .expect("stacked fillet preview");
        let preview_digest = preview
            .candidate
            .expect("the successor fillet must evaluate before confirmation")
            .scene
            .semantic_digest;
        assert!(app.confirm_pending_operation());
        assert_eq!(
            app.displayed.as_ref().unwrap().scene.semantic_digest,
            preview_digest
        );
    }

    #[test]
    fn edge_finish_drag_keeps_a_live_surface_and_supersedes_stale_exact_jobs() {
        let mut app = KernelLabApp {
            feature_preview_scheduler: Some(JobScheduler::new(1)),
            ..KernelLabApp::default()
        };
        let body_id = app.active_body_id().unwrap();
        let body = viewport::BodyInstanceKey::new(body_id.get());
        let edge = app.displayed.as_ref().unwrap().scene.edges[0];
        app.selected_edges = vec![viewport::DocumentEdgeSelection {
            body,
            edge: edge.source_edge,
        }];
        app.selected_edge = app.selected_edges.last().copied();
        app.edge_finish_distance = 0.2;
        app.stage_preset_feature(SolidFeaturePreset::Fillet);

        let mut first = None;
        for _ in 0..1_000 {
            let preview = app.edge_finish_preview_for_frame(None).unwrap();
            assert!(!preview.live_frames.is_empty());
            if preview.candidate.is_some() {
                first = preview.candidate;
                break;
            }
            std::thread::yield_now();
        }
        let first = first.expect("initial exact preview should resolve");
        assert_close(first.distance, 0.2);

        app.edge_finish_distance = 0.35;
        let immediate = app.edge_finish_preview_for_frame(None).unwrap();
        assert!(
            immediate.candidate.is_some(),
            "the last valid body must remain visible while dragging"
        );
        assert!(!immediate.live_frames.is_empty());

        for _ in 0..1_000 {
            let preview = app.edge_finish_preview_for_frame(None).unwrap();
            if preview
                .candidate
                .as_ref()
                .is_some_and(|candidate| (candidate.distance - 0.35).abs() <= 1.0e-12)
            {
                return;
            }
            std::thread::yield_now();
        }
        panic!("the newest exact drag value should supersede earlier jobs");
    }

    #[test]
    fn perpendicular_and_logical_successor_edge_sets_are_commit_capable() {
        let mut app = KernelLabApp::default();
        let body_id = app.active_body_id().unwrap();
        let body = viewport::BodyInstanceKey::new(body_id.get());
        let scene = &app.displayed.as_ref().unwrap().scene;
        let axis = |edge: &artificer_kernel::DebugEdge| {
            let delta = [
                (edge.endpoints[1].x - edge.endpoints[0].x).abs(),
                (edge.endpoints[1].y - edge.endpoints[0].y).abs(),
                (edge.endpoints[1].z - edge.endpoints[0].z).abs(),
            ];
            delta
                .iter()
                .enumerate()
                .max_by(|left, right| left.1.total_cmp(right.1))
                .unwrap()
                .0
        };
        let first = scene.edges[0];
        let perpendicular = scene
            .edges
            .iter()
            .copied()
            .find(|edge| axis(edge) != axis(&first))
            .expect("cuboid has perpendicular edges");
        app.selected_edges = vec![
            viewport::DocumentEdgeSelection {
                body,
                edge: first.source_edge,
            },
            viewport::DocumentEdgeSelection {
                body,
                edge: perpendicular.source_edge,
            },
        ];
        app.selected_edge = app.selected_edges.last().copied();
        assert_eq!(
            app.edge_finish_selection_support(),
            EdgeFinishSelectionSupport::RegularizedBlendSet
        );
        app.edge_finish_distance = 0.2;
        app.stage_preset_feature(SolidFeaturePreset::Fillet);
        assert!(app.pending_operation.is_some());
        assert!(app.current_edge_finish_preview().is_some());
        assert!(app.confirm_pending_operation());
        assert!(app.pending_operation.is_none());
        assert!(
            NativeKernel::validate(
                &app.displayed.as_ref().unwrap().snapshot,
                artificer_protocol::ValidationProfile::Solid,
            )
            .valid
        );

        // A clean logical successor edge remains available after the
        // regularized corner publishes.
        let next_edge = app
            .displayed
            .as_ref()
            .unwrap()
            .scene
            .edges
            .iter()
            .find(|edge| !edge.is_smooth)
            .unwrap()
            .source_edge;
        app.selected_edges = vec![viewport::DocumentEdgeSelection {
            body,
            edge: next_edge,
        }];
        app.selected_edge = app.selected_edges.last().copied();
        assert_eq!(
            app.edge_finish_selection_support(),
            EdgeFinishSelectionSupport::RegularizedBlendSet
        );
    }

    #[test]
    fn adding_a_perpendicular_edge_to_a_staged_fillet_previews_and_commits() {
        let mut app = KernelLabApp::default();
        let body_id = app.active_body_id().unwrap();
        let body = viewport::BodyInstanceKey::new(body_id.get());
        let scene = &app.displayed.as_ref().unwrap().scene;
        let axis = |edge: &artificer_kernel::DebugEdge| {
            let delta = [
                (edge.endpoints[1].x - edge.endpoints[0].x).abs(),
                (edge.endpoints[1].y - edge.endpoints[0].y).abs(),
                (edge.endpoints[1].z - edge.endpoints[0].z).abs(),
            ];
            delta
                .iter()
                .enumerate()
                .max_by(|left, right| left.1.total_cmp(right.1))
                .unwrap()
                .0
        };
        let first = scene.edges[0];
        let perpendicular = scene
            .edges
            .iter()
            .copied()
            .find(|edge| axis(edge) != axis(&first))
            .unwrap();
        app.selected_edges = vec![viewport::DocumentEdgeSelection {
            body,
            edge: first.source_edge,
        }];
        app.selected_edge = app.selected_edges.last().copied();
        app.edge_finish_distance = 0.2;
        app.stage_preset_feature(SolidFeaturePreset::Fillet);
        assert!(app.pending_operation.is_some());

        app.selected_edges.push(viewport::DocumentEdgeSelection {
            body,
            edge: perpendicular.source_edge,
        });
        app.selected_edge = app.selected_edges.last().copied();
        assert!(app.current_edge_finish_preview().is_some());
        assert!(app.confirm_pending_operation());
        assert!(app.pending_operation.is_none());
        assert!(app.displayed.as_ref().unwrap().snapshot.counts().faces > 6);
    }
}

#[cfg(test)]
mod circle_extrude_repro {
    use super::*;
    use crate::sketch::SketchGeometry;

    fn point(u: f64, v: f64) -> crate::sketch::SketchPoint {
        crate::sketch::SketchPoint { u, v }
    }

    #[test]
    fn a_committed_circle_is_extrudable() {
        let mut app = KernelLabApp::default();
        assert!(app.sketch.set_tool(crate::sketch::SketchTool::Circle));
        app.sketch
            .stage_geometry(SketchGeometry::Circle {
                center: point(0.0, 0.0),
                rim: point(5.0, 0.0),
            })
            .expect("circle should stage");
        app.sketch.commit_pending().expect("circle should commit");
        app.sketch_revision = app.sketch_revision.saturating_add(1);
        app.feature_preview
            .commit_sketch_revision(app.sketch_revision);
        app.workbench_mode = WorkbenchMode::Sketch;

        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
    }

    #[test]
    fn a_drawn_circle_stroke_commits_itself_and_extrudes() {
        // The canvas path: the tool stages the stroke and the app commits it
        // in the same frame — no tick, no greyed-out Extrude afterwards.
        let mut app = KernelLabApp::default();
        assert!(app.sketch.set_tool(crate::sketch::SketchTool::Circle));
        let entity = app
            .sketch
            .stage_geometry(SketchGeometry::Circle {
                center: point(0.0, 0.0),
                rim: point(5.0, 0.0),
            })
            .expect("circle should stage");
        app.commit_sketch_stroke(entity);
        app.feature_preview
            .commit_sketch_revision(app.sketch_revision);
        app.workbench_mode = WorkbenchMode::Sketch;

        assert!(!app.sketch.has_pending_edit());
        assert!(app.pending_operation.is_none());
        assert_eq!(
            app.sketch_extrusion_eligibility(),
            SketchExtrusionEligibility::Ready
        );
        assert!(app.stage_sketch_extrusion());
        assert!(matches!(
            app.pending_sketch_extrusion_command(),
            Some(KernelCommand::ExtrudePlanarProfile { .. })
        ));
    }

    /// The milestone's staging half (ADR 0026, F3): a sketched region turning
    /// about the sketch's own centreline, with the volume checked against
    /// Pappus rather than against a recorded number.
    #[test]
    fn revolve_turns_the_sketched_region_about_its_centreline() {
        let mut app = KernelLabApp {
            workbench_mode: WorkbenchMode::Sketch,
            ..Default::default()
        };

        // A rectangle clear of the axis: section r in [2, 5], z in [0, 3].
        assert!(app.sketch.set_tool(crate::sketch::SketchTool::Rectangle));
        let region = app
            .sketch
            .stage_geometry(SketchGeometry::Rectangle {
                first: point(2.0, 0.0),
                opposite: point(5.0, 3.0),
            })
            .expect("rectangle should stage");
        app.commit_sketch_stroke(region);

        // The centreline is the axis, drawn on the sketch's v axis.
        assert!(app.sketch.set_tool(crate::sketch::SketchTool::CentreLine));
        let axis = app
            .sketch
            .stage_geometry_with_role(
                SketchGeometry::Segment {
                    start: point(0.0, 0.0),
                    end: point(0.0, 3.0),
                },
                crate::sketch::SketchEntityRole::Construction,
            )
            .expect("centreline should stage");
        app.commit_sketch_stroke(axis);
        assert!(app.sketch.centreline_axis().is_some());

        app.stage_preset_feature(SolidFeaturePreset::Revolve);
        assert!(
            app.staged_revolve.is_some(),
            "the sketch profile and centreline should be captured: {:?}",
            app.document_status
        );
        assert!(app.confirm_pending_operation());

        let volume = app
            .displayed_measures()
            .expect("the revolved body should publish measures")
            .volume;
        let expected = std::f64::consts::PI * (25.0 - 4.0) * 3.0;
        assert!(
            ((volume - expected) / expected).abs() < 1.0e-9,
            "revolved volume {volume} should equal {expected}"
        );
        assert!(app.staged_revolve.is_none(), "the staging is spent");
    }

    #[test]
    fn revolve_without_a_centreline_still_builds_its_preset_tube() {
        let mut app = KernelLabApp::default();
        app.stage_preset_feature(SolidFeaturePreset::Revolve);
        assert!(app.staged_revolve.is_none());
        assert!(app.confirm_pending_operation());
        let volume = app
            .displayed_measures()
            .expect("the preset tube should publish measures")
            .volume;
        let expected = std::f64::consts::PI * (4.0 - 1.0) * 3.0;
        assert!(
            ((volume - expected) / expected).abs() < 1.0e-9,
            "preset revolve volume {volume} should equal {expected}"
        );
    }

    /// F frames the selection when there is one (ADR 0026, F10).
    #[test]
    fn framing_prefers_the_selection_over_the_whole_document() {
        let mut app = KernelLabApp::default();
        let scene = &app.displayed.as_ref().expect("default body").scene;
        let face = scene
            .triangles
            .first()
            .expect("the default body has faces")
            .source_face;
        assert!(!app.frame_selection(), "nothing selected frames nothing");
        app.selected_face = Some(face);
        let selection = app
            .selection_world_bounds()
            .expect("a selected face has bounds");
        let document = app
            .bodies
            .iter()
            .filter_map(|body| app.committed_world_bounds_for_body(body))
            .reduce(union_aabb)
            .expect("the document has bounds");
        let span = |bounds: Aabb3| {
            (bounds.max.x - bounds.min.x)
                + (bounds.max.y - bounds.min.y)
                + (bounds.max.z - bounds.min.z)
        };
        assert!(
            span(selection) < span(document),
            "one face should be smaller than the whole body"
        );
        assert!(app.frame_selection());
    }

    #[test]
    fn finish_sketch_now_appends_to_the_document_in_one_action() {
        let mut app = KernelLabApp::default();
        assert!(app.sketch.set_tool(crate::sketch::SketchTool::Circle));
        let entity = app
            .sketch
            .stage_geometry(SketchGeometry::Circle {
                center: point(0.0, 0.0),
                rim: point(4.0, 0.0),
            })
            .expect("circle should stage");
        app.commit_sketch_stroke(entity);
        app.workbench_mode = WorkbenchMode::Sketch;

        assert!(app.finish_sketch_now());
        assert!(app.pending_operation.is_none());
        assert_eq!(app.document.sketches().len(), 1);
    }
}

#[cfg(test)]
mod auto_commit_delete {
    use super::*;
    use crate::sketch::{SketchGeometry, SketchPoint};

    #[test]
    fn a_committed_stroke_stays_selected_and_deletes_in_one_action() {
        let mut app = KernelLabApp {
            workbench_mode: WorkbenchMode::Sketch,
            ..Default::default()
        };
        assert!(app.sketch.set_tool(crate::sketch::SketchTool::Rectangle));
        let entity = app
            .sketch
            .stage_geometry(SketchGeometry::Rectangle {
                first: SketchPoint { u: -2.0, v: -1.0 },
                opposite: SketchPoint { u: 2.0, v: 1.0 },
            })
            .expect("rectangle stages");
        app.commit_sketch_stroke(entity);
        assert!(!app.sketch.has_pending_edit());
        assert!(app.sketch.selected().is_some());
        assert!(app.sketch.stage_delete_selected().is_ok());
        assert!(app.sketch.has_pending_edit());
    }
}
