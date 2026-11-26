//! Third-party libraries shared by gitsot's own libraries.
//!
//! gitsot is built as one dynamic library per crate. A third-party library
//! that two of them use (for example `anyhow` in both gitsot-core and
//! gitsot-term) can only be inside one DLL, so those libraries are collected
//! here: `gitsot_shared.dll` on Windows, `libgitsot_shared.dylib` on macOS and
//! `libgitsot_shared.so` on Linux. gitsot-core, gitsot-term and gitsot-forge
//! load this library and use the copies inside it.
//!
//! There is no code of our own here, only the re-exports that make the
//! libraries part of this DLL.

pub use anyhow;
pub use bitflags;
pub use libc;
pub use log;
pub use memchr;
pub use serde;
