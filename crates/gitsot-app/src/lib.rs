//! The gitsot application library.
//!
//! This crate holds the whole user interface and is built as a dynamic
//! library: `gitsot.dll` on Windows, `libgitsot.so` on Linux and
//! `libgitsot.dylib` on macOS. The `gitsot` executable is only a small
//! launcher that loads this library and calls [`gitsot_main`].
//!
//! The library exports a tiny C interface so the launcher and the library do
//! not have to be built by the same compiler run:
//!
//! * `gitsot_version()` returns the library's version as a C string. The
//!   launcher refuses to start a library from a different version.
//! * `gitsot_main()` opens the window and returns the process exit code.

mod app;
mod term_view;
mod theme;

use std::ffi::c_char;

/// The library's version, NUL-terminated for C callers.
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "\0");

/// Open the main window and run until it is closed.
pub fn run() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("gitsot")
            .with_app_id("gitsot")
            .with_inner_size([1200.0, 760.0])
            .with_min_inner_size([860.0, 520.0])
            .with_icon(eframe::egui::IconData { rgba: theme::icon_rgba(64), width: 64, height: 64 }),
        ..Default::default()
    };
    eframe::run_native("gitsot", options, Box::new(|cc| Ok(Box::new(app::GitsotApp::new(cc)))))
}

/// The library's version as a NUL-terminated string, e.g. `"0.2.0"`.
#[no_mangle]
pub extern "C" fn gitsot_version() -> *const c_char {
    VERSION.as_ptr().cast()
}

/// Run gitsot. Returns 0 on success, 1 if the window could not be opened and
/// 101 if gitsot panicked. Command-line arguments are read from the process,
/// so the launcher does not pass them.
#[no_mangle]
pub extern "C" fn gitsot_main() -> i32 {
    // A panic must never unwind into the launcher: it is a separate binary.
    match std::panic::catch_unwind(run) {
        Ok(Ok(())) => 0,
        Ok(Err(err)) => {
            eprintln!("gitsot: {err}");
            1
        }
        Err(_) => 101,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_nul_terminated() {
        let version = unsafe { std::ffi::CStr::from_ptr(gitsot_version()) };
        assert_eq!(version.to_str().unwrap(), env!("CARGO_PKG_VERSION"));
    }
}
