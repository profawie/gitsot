//! The gitsot launcher.
//!
//! This executable is deliberately tiny. All of gitsot lives in the gitsot
//! library next to it (`gitsot.dll` on Windows, `libgitsot.so` on Linux,
//! `libgitsot.dylib` on macOS). The launcher finds that library, checks that
//! it is the same version, and hands control to it.

// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::env::consts::{DLL_PREFIX, DLL_SUFFIX};
use std::ffi::{c_char, CStr};
use std::path::{Path, PathBuf};

use libloading::{Library, Symbol};

const VERSION: &str = env!("CARGO_PKG_VERSION");

type VersionFn = unsafe extern "C" fn() -> *const c_char;
type MainFn = unsafe extern "C" fn() -> i32;

fn main() {
    let code = match launch() {
        Ok(code) => code,
        Err(message) => {
            show_error(&message);
            1
        }
    };
    std::process::exit(code);
}

/// Load the library and run it. Returns the exit code, or an error message
/// for the user when gitsot could not be started at all.
fn launch() -> Result<i32, String> {
    let file_name = format!("{DLL_PREFIX}gitsot{DLL_SUFFIX}");
    let candidates = library_candidates(&file_name)?;
    let Some(path) = candidates.iter().find(|p| p.is_file()) else {
        let looked = candidates.iter().map(|p| format!("  {}", p.display())).collect::<Vec<_>>().join("\n");
        return Err(format!(
            "gitsot cannot start because {file_name} is missing.\n\nLooked in:\n{looked}\n\n\
             Keep {file_name} in the same folder as the gitsot program."
        ));
    };

    // SAFETY: this is gitsot's own library, built from the same workspace.
    // Loading it runs no code of ours besides normal library initialisation.
    let library = unsafe { Library::new(path) }
        .map_err(|e| format!("gitsot cannot load {}:\n\n{e}", path.display()))?;

    // SAFETY: the symbol types match the exports in crates/gitsot-app/src/lib.rs.
    let code = unsafe {
        let version: Symbol<VersionFn> = library
            .get(b"gitsot_version\0")
            .map_err(|_| format!("{} is not a gitsot library.", path.display()))?;
        let found = CStr::from_ptr(version()).to_string_lossy().into_owned();
        if found != VERSION {
            return Err(format!(
                "{} is version {found}, but this gitsot program is version {VERSION}.\n\n\
                 Copy the whole gitsot folder again so both files come from the same build.",
                path.display()
            ));
        }
        let main: Symbol<MainFn> = library
            .get(b"gitsot_main\0")
            .map_err(|_| format!("{} is not a gitsot library.", path.display()))?;
        main()
    };

    if code == 1 {
        show_error(
            "gitsot could not open its window. Make sure you are in a desktop session and that the graphics driver supports OpenGL.",
        );
    }

    // Never unload the library: background threads (terminals, git commands)
    // may still be running code from it until the process exits.
    std::mem::forget(library);
    Ok(code)
}

/// Where the library may be, in order: next to the executable, and on macOS
/// also in the app bundle's `Contents/Frameworks` folder.
fn library_candidates(file_name: &str) -> Result<Vec<PathBuf>, String> {
    let exe = std::env::current_exe().map_err(|e| format!("gitsot cannot find its own location: {e}"))?;
    // Resolve symlinks (e.g. /usr/local/bin/gitsot -> /opt/gitsot/gitsot) so
    // the library is looked up next to the real file.
    #[cfg(unix)]
    let exe = exe.canonicalize().unwrap_or(exe);
    let dir = exe.parent().unwrap_or(Path::new("."));

    let mut candidates = vec![dir.join(file_name)];
    if cfg!(target_os = "macos") {
        candidates.push(dir.join("..").join("Frameworks").join(file_name));
    }
    Ok(candidates)
}

/// Tell the user why gitsot did not start. Release builds on Windows have no
/// console, so a message box is used there.
fn show_error(message: &str) {
    eprintln!("error: {message}");
    #[cfg(windows)]
    message_box(message);
}

#[cfg(windows)]
fn message_box(message: &str) {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(hwnd: *mut std::ffi::c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
    }
    const MB_OK: u32 = 0x0;
    const MB_ICONERROR: u32 = 0x10;

    let wide = |s: &str| std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let text = wide(message);
    let caption = wide("gitsot");
    // SAFETY: both strings are NUL-terminated UTF-16 and outlive the call.
    unsafe {
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), MB_OK | MB_ICONERROR);
    }
}
