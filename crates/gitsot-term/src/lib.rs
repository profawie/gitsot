//! gitsot terminal: the built-in terminal, with no UI.
//!
//! The shell in a pseudo-terminal and its screen (`alacritty_terminal`), key
//! encoding, and the Gitsot prompt for PowerShell and zsh/bash. The UI draws
//! the screen this crate keeps.
//!
//! Built as a dynamic library: `gitsot_term.dll` on Windows,
//! `libgitsot_term.dylib` on macOS and `libgitsot_term.so` on Linux.

// Use the third-party libraries inside gitsot_shared instead of linking our
// own copies (see crates/gitsot-shared).
extern crate gitsot_shared;

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
