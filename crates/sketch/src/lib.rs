//! UI-neutral, persistent authoring and analytic arrangement for 2D sketches.
//!
//! This crate owns exact sketch intent, stable identities, atomic edits, and
//! profile extraction. It has no renderer, document-history, B-rep, or UI
//! dependency. Display tessellation is never used to create kernel geometry.

mod arrangement;
mod constraints;
mod definition;
mod geometry;
mod ids;
mod intersections;
mod primitives;
mod profile;
mod queries;
mod recipes;
mod transaction;
mod trim;

pub use arrangement::*;
pub use constraints::*;
pub use definition::*;
pub use geometry::*;
pub use ids::*;
pub use intersections::*;
pub use primitives::*;
pub use profile::*;
pub use queries::*;
pub use recipes::*;
pub use transaction::*;
pub use trim::*;
