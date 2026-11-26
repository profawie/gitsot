#!/usr/bin/env bash
# Build gitsot on macOS or Linux and put a ready-to-copy release in dist/.
#
#   ./build.sh            release build for this machine
#   ./build.sh --debug    debug build
#   ./build.sh --windows  cross-compile for Windows with the GNU toolchain
#                         (needs mingw-w64, e.g. `brew install mingw-w64`
#                          or `apt install gcc-mingw-w64-x86-64`)
#
# dist/ holds every file gitsot needs, so the whole folder can be copied to
# another machine and run from there. gitsot is one small program plus one
# dynamic library per crate, and the Rust standard library as its own file:
#
#   Windows  dist/gitsot.exe, gitsot.dll, gitsot_core.dll, gitsot_term.dll,
#            gitsot_forge.dll, gitsot_shared.dll, std-<hash>.dll,
#            libgcc_s_seh-1.dll (+ any other MinGW runtime DLL they import)
#   macOS    dist/gitsot.app (the .dylib files are in Contents/Frameworks)
#   Linux    dist/gitsot + libgitsot*.so + libstd-<hash>.so
set -euo pipefail
cd "$(dirname "$0")"

PROFILE=release
WINDOWS=0
for arg in "$@"; do
  case "$arg" in
    --debug) PROFILE=debug ;;
    --release) PROFILE=release ;;
    --windows) WINDOWS=1 ;;
    -h|--help) sed -n '2,18p' "$0"; exit 0 ;;
    *) echo "Unknown option: $arg" >&2; exit 1 ;;
  esac
done

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found. Install Rust from https://rustup.rs" >&2
  exit 1
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT=dist

CARGO_FLAGS=(--workspace)
[ "$PROFILE" = "release" ] && CARGO_FLAGS+=(--release)

# Start from an empty dist/ so no file from an older build is left behind.
fresh_out() {
  rm -rf "$OUT"
  mkdir -p "$OUT"
}

# List the files in $OUT, with sizes.
show_out() {
  echo "==> Done: $OUT/"
  (cd "$OUT" && find . -type f | sed 's|^\./||' | sort | while read -r f; do
    printf '    %-48s %6s KB\n' "$f" "$(( $(wc -c < "$f") / 1024 ))"
  done)
}

if [ "$WINDOWS" = "1" ]; then
  TARGET=x86_64-pc-windows-gnu
  if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
    echo "error: x86_64-w64-mingw32-gcc not found. Install mingw-w64 first." >&2
    exit 1
  fi
  rustup target add "$TARGET" >/dev/null
  export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
  export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
  export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
  echo "==> Building gitsot $VERSION for Windows ($TARGET, $PROFILE)"
  cargo build "${CARGO_FLAGS[@]}" --target "$TARGET"

  BUILT="target/$TARGET/$PROFILE"
  fresh_out
  cp "$BUILT/gitsot.exe" "$OUT/"

  # Folders a DLL may come from: this build, the Rust standard library
  # (std-<hash>.dll) and the cross toolchain's runtime (libgcc_s_seh-1.dll ...).
  DLL_DIRS=("$BUILT" "$(rustc --print target-libdir --target "$TARGET")")
  for file in "$(x86_64-w64-mingw32-gcc -print-libgcc-file-name)" \
              "$(x86_64-w64-mingw32-gcc -print-file-name=libgcc_s_seh-1.dll)" \
              "$(x86_64-w64-mingw32-gcc -print-file-name=libwinpthread-1.dll)" \
              /usr/x86_64-w64-mingw32/bin/. /usr/x86_64-w64-mingw32/lib/.; do
    # gcc prints just the name when it cannot find a file: skip those.
    case "$file" in /*) ;; *) continue ;; esac
    dir=$(dirname "$file")
    [ -d "$dir" ] && DLL_DIRS+=("$dir")
  done

  # Copy $1 into $OUT, then every DLL it imports that is found in DLL_DIRS,
  # and so on for those. DLLs found nowhere are part of Windows itself.
  copy_with_dlls() {
    local file=$1 dll found dir
    [ -f "$OUT/$(basename "$file")" ] || cp "$file" "$OUT/"
    for dll in $(x86_64-w64-mingw32-objdump -p "$file" | sed -n 's/^[[:space:]]*DLL Name: //p' | sort -u); do
      [ -f "$OUT/$dll" ] && continue
      found=""
      for dir in "${DLL_DIRS[@]}"; do
        if [ -f "$dir/$dll" ]; then found="$dir/$dll"; break; fi
      done
      [ -z "$found" ] && continue
      copy_with_dlls "$found"
    done
  }
  echo "==> Collecting DLLs"
  copy_with_dlls "$OUT/gitsot.exe"
  # The launcher loads gitsot.dll at run time, so it is not in its imports.
  copy_with_dlls "$BUILT/gitsot.dll"

  show_out
  exit 0
fi

echo "==> Building gitsot $VERSION ($PROFILE)"
cargo build "${CARGO_FLAGS[@]}"
BUILT="target/$PROFILE"
RUSTLIB=$(rustc --print target-libdir)
fresh_out

if [ "$(uname -s)" = "Darwin" ]; then
  APP="$OUT/gitsot.app"
  LIBS="$APP/Contents/Frameworks"
  mkdir -p "$APP/Contents/MacOS" "$LIBS" "$APP/Contents/Resources"
  cp "$BUILT/gitsot" "$APP/Contents/MacOS/gitsot"
  cp "$BUILT"/libgitsot*.dylib "$LIBS/"
  cp "$RUSTLIB"/libstd-*.dylib "$LIBS/"
  cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>gitsot</string>
  <key>CFBundleDisplayName</key><string>gitsot</string>
  <key>CFBundleIdentifier</key><string>com.gitsot.app</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleExecutable</key><string>gitsot</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
  # The linker records where each library was built (target/...). Point every
  # reference at @rpath instead, which the program and libraries resolve to
  # Contents/Frameworks, so the app works wherever it is copied.
  for lib in "$LIBS"/*.dylib; do
    install_name_tool -id "@rpath/$(basename "$lib")" "$lib" 2>/dev/null
  done
  for file in "$APP/Contents/MacOS/gitsot" "$LIBS"/*.dylib; do
    otool -L "$file" | tail -n +2 | awk '{print $1}' | while read -r dep; do
      name=$(basename "$dep")
      if [ "$dep" != "@rpath/$name" ] && [ -f "$LIBS/$name" ]; then
        install_name_tool -change "$dep" "@rpath/$name" "$file" 2>/dev/null
      fi
    done
  done
  # Ad-hoc sign every file so macOS lets the program load its libraries.
  if command -v codesign >/dev/null 2>&1; then
    for lib in "$LIBS"/*.dylib; do
      codesign --force --sign - "$lib" >/dev/null 2>&1 || true
    done
    codesign --force --sign - "$APP" >/dev/null 2>&1 || true
  fi
else
  cp "$BUILT/gitsot" "$BUILT"/libgitsot*.so "$OUT/"
  cp "$RUSTLIB"/libstd-*.so "$OUT/"
fi

show_out
