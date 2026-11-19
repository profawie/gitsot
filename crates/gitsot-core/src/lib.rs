//! gitsot core: everything that is not UI.
//!
//! Local Git operations (libgit2), network operations through the `git`
//! command, identities and folder rules, the history graph layout, and the
//! built-in terminal (shell, pseudo-terminal and screen). This crate has no
//! dependency on the UI toolkit, so it can be tested and reused on its own.

pub mod cli;
pub mod git;
pub mod graph;
pub mod identity;
pub mod shell;
pub mod term;

/// gitsot's own data folder, the same one the UI uses for its settings:
/// `$XDG_DATA_HOME/gitsot` (or `~/.local/share/gitsot`) on Linux and
/// `~/Library/Application Support/gitsot` on macOS.
#[cfg(unix)]
pub fn data_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    if cfg!(target_os = "macos") {
        return std::env::home_dir().map(|home| home.join("Library").join("Application Support").join("gitsot"));
    }
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".local").join("share")))
        .map(|dir| dir.join("gitsot"))
}
