//! A loft between sketch regions on different planes (ADR 0051).
//!
//! A loft names its sections the way an extrusion names its profile: by the
//! sketch that holds each one and the signatures of the regions it takes from
//! that sketch. Nothing geometric is stored. Replay compiles every section
//! from its sketch as the sketch now stands, places it on the sketch's plane
//! as the plane now stands, and hands the kernel one
//! [`KernelCommand::LoftPlanarSections`]. Moving a plane or editing a sketch
//! therefore moves or reshapes the loft the next time it is rebuilt.

use std::collections::BTreeMap;

use artificer_protocol::{
    KernelCommand, LoftOperation, LoftSection, MAX_LOFT_SECTIONS, PlanarFrame3, PrecisionPolicy,
};
use artificer_sketch::RegionSignature;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::sketch_region::{
    MAX_SELECTED_SKETCH_REGIONS, SketchRegionResolveError, compile_sketch_regions,
};
use crate::{FeatureId, ModelDocument, ReplayAction, ResolvedDatumPlane, SketchId};

/// Schema written for newly created loft recipes.
pub const CURRENT_SKETCH_LOFT_RECIPE_VERSION: u32 = 1;

const fn current_sketch_loft_recipe_version() -> u32 {
    CURRENT_SKETCH_LOFT_RECIPE_VERSION
}

/// One section of a loft: regions of one sketch, in that sketch's plane.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SketchLoftSection {
    pub sketch: SketchId,
    pub regions: Vec<RegionSignature>,
}

impl SketchLoftSection {
    #[must_use]
    pub fn new(sketch: SketchId, mut regions: Vec<RegionSignature>) -> Self {
        regions.sort();
        regions.dedup();
        Self { sketch, regions }
    }
}

/// A loft through sections drawn in sketches of their own.
///
/// The sections run in order: the loft starts at the first and ends at the
/// last. `operation` says whether the loft is a body of its own or is joined
/// to, or taken from, the body the feature names as its input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SketchLoft {
    #[serde(default = "current_sketch_loft_recipe_version")]
    pub version: u32,
    pub sections: Vec<SketchLoftSection>,
    pub operation: LoftOperation,
}

impl SketchLoft {
    pub fn new(
        sections: Vec<SketchLoftSection>,
        operation: LoftOperation,
    ) -> Result<Self, SketchLoftError> {
        let recipe = Self {
            version: CURRENT_SKETCH_LOFT_RECIPE_VERSION,
            sections,
            operation,
        };
        recipe.validate()?;
        Ok(recipe)
    }

    /// Structural checks that need no geometry.
    ///
    /// Two sections from one sketch would lie in one plane, which no loft can
    /// span, so each section comes from a sketch of its own. How many sections
    /// the kernel can loft through is the kernel's to say; the recipe allows
    /// as many as the protocol carries.
    pub fn validate(&self) -> Result<(), SketchLoftError> {
        if self.version != CURRENT_SKETCH_LOFT_RECIPE_VERSION {
            return Err(SketchLoftError::UnsupportedVersion {
                found: self.version,
            });
        }
        if self.sections.len() < 2 {
            return Err(SketchLoftError::TooFewSections);
        }
        if self.sections.len() > MAX_LOFT_SECTIONS {
            return Err(SketchLoftError::TooManySections {
                actual: self.sections.len(),
                limit: MAX_LOFT_SECTIONS,
            });
        }
        for (index, section) in self.sections.iter().enumerate() {
            if section.sketch.get() == 0 {
                return Err(SketchLoftError::InvalidSketch);
            }
            if section.regions.is_empty() {
                return Err(SketchLoftError::EmptySection { section: index });
            }
            if section.regions.len() > MAX_SELECTED_SKETCH_REGIONS {
                return Err(SketchLoftError::TooManyRegions {
                    section: index,
                    limit: MAX_SELECTED_SKETCH_REGIONS,
                });
            }
            if section.regions.windows(2).any(|pair| pair[0] >= pair[1]) {
                return Err(SketchLoftError::NonCanonicalSection { section: index });
            }
            if self.sections[..index]
                .iter()
                .any(|earlier| earlier.sketch == section.sketch)
            {
                return Err(SketchLoftError::RepeatedSketch(section.sketch));
            }
        }
        Ok(())
    }

    /// The sketches the sections are drawn in, first section first.
    pub fn sketches(&self) -> impl Iterator<Item = SketchId> + '_ {
        self.sections.iter().map(|section| section.sketch)
    }

    /// Compiles every section from its sketch and places it on its plane.
    ///
    /// `planes` are construction-plane frames a rebuild has already resolved
    /// and the document's cache does not hold yet; a sketch on such a plane is
    /// placed there. Any other sketch sits where the document last placed it.
    pub fn resolve_with_planes(
        &self,
        document: &ModelDocument,
        precision: PrecisionPolicy,
        planes: &BTreeMap<FeatureId, ResolvedDatumPlane>,
    ) -> Result<ReplayAction, SketchRegionResolveError> {
        self.validate()
            .map_err(SketchRegionResolveError::InvalidLoft)?;
        let sections = self
            .sections
            .iter()
            .map(|section| {
                let (profile, drawn_frame) =
                    compile_sketch_regions(document, section.sketch, &section.regions, precision)?;
                let frame = section_plane(document, section.sketch, planes)
                    .or_else(|| document.sketch_frame(section.sketch))
                    .unwrap_or(drawn_frame);
                Ok(LoftSection { frame, profile })
            })
            .collect::<Result<Vec<_>, SketchRegionResolveError>>()?;
        Ok(ReplayAction::Kernel(KernelCommand::LoftPlanarSections {
            sections,
            operation: self.operation,
        }))
    }
}

fn section_plane(
    document: &ModelDocument,
    sketch: SketchId,
    planes: &BTreeMap<FeatureId, ResolvedDatumPlane>,
) -> Option<PlanarFrame3> {
    let record = document.sketch(sketch)?;
    let payload = document.sketch_payload(sketch, record.geometry_revision)?;
    let plane = payload.support.plane()?;
    planes.get(&plane).map(|resolved| resolved.frame)
}

/// Structural loft-recipe rejection detected without evaluating geometry.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SketchLoftError {
    #[error(
        "unsupported loft recipe version {found}; this build supports {CURRENT_SKETCH_LOFT_RECIPE_VERSION}"
    )]
    UnsupportedVersion { found: u32 },
    #[error("a loft needs at least two sections")]
    TooFewSections,
    #[error("a loft carries at most {limit} sections, not {actual}")]
    TooManySections { actual: usize, limit: usize },
    #[error("a loft section requires a non-zero source sketch")]
    InvalidSketch,
    #[error("loft section {section} selects no region")]
    EmptySection { section: usize },
    #[error("loft section {section} selects more than {limit} regions")]
    TooManyRegions { section: usize, limit: usize },
    #[error("loft section {section} regions must be sorted and unique")]
    NonCanonicalSection { section: usize },
    #[error("{0} holds two sections of one loft; each section needs a plane of its own")]
    RepeatedSketch(SketchId),
    #[error("an add or cut loft names the body it changes as its input")]
    MissingTargetBody,
}
