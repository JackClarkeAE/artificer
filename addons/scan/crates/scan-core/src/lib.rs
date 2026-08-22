//! Scan-to-CAD core for the Artificer kernel.
//!
//! Turns triangle meshes from 3D scanners into aligned, segmented, analytic
//! geometry using the estimate-then-polish structure of commercial
//! metrology pipelines:
//!
//! 1. **Import** ([`stl`], [`ply`]) — welded indexed meshes from scanner
//!    output.
//! 2. **Align** ([`register`]) — PCA pre-alignment, trimmed point-to-plane
//!    ICP best fit, and 3-2-1 datum alignment.
//! 3. **Segment** ([`segment`]) — sharp-edge region growing.
//! 4. **Fit** ([`fit`]) — plane/sphere/cylinder/cone least squares with
//!    damped refinement and deviation statistics.
//! 5. **Canonicalize** ([`snap`]) — snap axes to datums, dimensions to
//!    round values, and harmonize coplanar/coaxial families, keeping a
//!    note of every adjustment.
//!
//! [`report::reverse_engineer`] chains stages 3-5 and emits a structured
//! report ready for feature reconstruction in the kernel.

pub mod bench;
pub mod blend;
pub mod consolidate;
pub mod constrain;
pub mod coverage;
pub mod datum;
pub mod finalize;
pub mod fit;
pub mod hygiene;
pub mod instance;
pub mod kinematic;
pub mod merge;
pub mod mesh;
pub mod noise;
pub mod numeric;
pub mod obj;
pub mod ply;
pub mod ransac;
pub mod rebuild;
pub mod reconstruct;
pub mod register;
pub mod render;
pub mod report;
pub mod segment;
pub mod sew;
pub mod simulate;
pub mod snap;
pub mod spatial;
pub mod step;
pub mod stl;
pub mod synth;
pub mod transform;
pub mod tree;
pub mod validate;

pub use consolidate::{consolidate_features, solve_shared_parameters};
pub use datum::{DatumAlignment, auto_datum_alignment};
pub use finalize::{finalize_features, refine_rounds};
pub use fit::{
    ConeFit, CylinderFit, DeviationStats, EdgeRoundFit, PatternFit, PlaneFit, SphereFit,
};
pub use merge::{absorb_into_anchors, merge_fragments};
pub use mesh::TriangleMesh;
pub use ransac::{ExtractedPrimitive, RansacParams, extract_primitives};
pub use rebuild::{RebuiltModel, rebuild_sharp};
pub use reconstruct::{
    ChamferProposal, FilletProposal, MasterProfile, PatternProposal, ProfileSegment,
    ReconstructionPlan, extract_revolved_bands, plan_to_history_json, recognize_pattern_feature,
};
pub use register::{IcpParams, IcpResult, best_fit_align, datum_alignment};
pub use report::{FeatureRecord, ReverseOptions, ReverseReport, reverse_engineer};
pub use segment::{Region, SegmentationParams, SurfaceClass};
pub use snap::SnapPolicy;
pub use transform::RigidTransform;
