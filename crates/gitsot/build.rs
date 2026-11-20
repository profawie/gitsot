//! Puts the gitsot icon into `gitsot.exe`.
//!
//! On Windows the icon of a program is a resource inside the executable.
//! `assets/gitsot.rc` names the icon; `windres` (part of MinGW-w64, which the
//! Windows build already needs) compiles it and the result is linked in.
//! Other platforms need nothing here.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/gitsot.rc");
    println!("cargo:rerun-if-changed=assets/gitsot.ico");
    println!("cargo:rerun-if-env-changed=WINDRES");

    // The platform being built for, which is not always the one building.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let assets = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("assets");
    let object = PathBuf::from(env::var("OUT_DIR").unwrap()).join("gitsot-resources.o");

    // `windres` on Windows itself, the prefixed name when cross-compiling.
    let candidates: Vec<String> = env::var("WINDRES")
        .into_iter()
        .chain(["x86_64-w64-mingw32-windres".to_string(), "windres".to_string()])
        .collect();
    for windres in &candidates {
        let done = Command::new(windres)
            .arg("--input")
            .arg(assets.join("gitsot.rc"))
            .arg("--include-dir")
            .arg(&assets)
            .args(["--output-format", "coff", "--target", "pe-x86-64", "--output"])
            .arg(&object)
            .status();
        match done {
            Ok(status) if status.success() => {
                println!("cargo:rustc-link-arg-bins={}", object.display());
                return;
            }
            Ok(status) => println!("cargo:warning={windres} failed ({status})"),
            // Not installed under this name: try the next one.
            Err(_) => {}
        }
    }
    println!("cargo:warning=windres was not found, so gitsot.exe is built without its icon. It is part of MinGW-w64 binutils.");
}
