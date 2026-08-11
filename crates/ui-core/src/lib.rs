//! Leaf presentation utilities shared by the viewport, the sketch canvas, and
//! the workbench shell.
//!
//! Nothing here knows about application state. Keeping these in their own
//! crate means a change to a colour token or a camera easing curve does not
//! recompile the twenty-thousand-line shell.

pub mod drag_handle;
pub mod navigation;
pub mod presentation;
pub mod theme;
