//! The kernel's programmatic API: sessions, commands, selectors, the
//! JSON-RPC server, `.art` scripting, headless snapshots, and export.
//!
//! This ships with the kernel rather than beside it: a client that embeds
//! `artificer_kernel` has the whole surface, and the server binary in
//! `apps/api-server` is a thin command-line front for it.

pub mod analysis;
pub mod commands;
pub mod debug;
pub mod decompile;
pub mod diff;
pub mod export;
pub mod interference;
pub mod journal;
pub mod probe;
pub mod query;
pub mod report;
pub mod scripting;
pub mod selectors;
pub mod server;
pub mod session;
pub mod snapshot;

pub use crate::CancellationToken;
pub use commands::*;
pub use debug::*;
pub use decompile::*;
pub use diff::*;
pub use export::*;
pub use journal::*;
pub use probe::*;
pub use query::*;
pub use report::*;
pub use selectors::*;
pub use server::*;
pub use session::*;
pub use snapshot::*;
