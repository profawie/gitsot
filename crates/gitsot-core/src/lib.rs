//! gitsot core: Git work, with no UI.
//!
//! Local Git operations (libgit2), network operations through the `git`
//! command, identities and folder rules, and the history graph layout. This
//! crate has no dependency on the UI toolkit, so it can be tested and reused
//! on its own.
//!
//! Built as a dynamic library: `gitsot_core.dll` on Windows,
//! `libgitsot_core.dylib` on macOS and `libgitsot_core.so` on Linux.

// Use the third-party libraries inside gitsot_shared instead of linking our
// own copies (see crates/gitsot-shared).
extern crate gitsot_shared;

pub mod cli;
pub mod git;
pub mod graph;
pub mod identity;
