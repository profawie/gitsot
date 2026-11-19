#!/usr/bin/env bash
# Build gitsot on macOS or Linux and put a ready-to-copy release in dist/gitsot.
#
#   ./build.sh            release build for this machine
#   ./build.sh --debug    debug build
#   ./build.sh --windows  cross-compile for Windows with the GNU toolchain
#                         (needs mingw-w64, e.g. `brew install mingw-w64`
#                          or `apt install gcc-mingw-w64-x86-64`)
#
# dist/gitsot holds every file gitsot needs, so the whole folder can be copied
# to another machine and run from there:
#
#   Linux    dist/gitsot/gitsot + libgitsot.so
#   macOS    dist/gitsot/gitsot.app (libgitsot.dylib is inside the bundle)
#   Windows  dist/gitsot/gitsot.exe + gitsot.dll (+ MinGW runtime DLLs, if used)
set -euo pipefail
cd "$(dirname "$0")"

PROFILE=release
WINDOWS=0
for arg in "$@"; do
  case "$arg" in
    --debug) PROFILE=debug ;;
    --release) PROFILE=release ;;
    --windows) WINDOWS=1 ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "Unknown option: $arg" >&2; exit 1 ;;
  esac
done

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found. Install Rust from https://rustup.rs" >&2
  exit 1
fi

VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
OUT=dist/gitsot

CARGO_FLAGS=(--workspace)
[ "$PROFILE" = "release" ] && CARGO_FLAGS+=(--release)

# Copy the MinGW runtime DLLs (libgcc, libwinpthread, ...) that $1 imports, if
# any, into $OUT. Usually there are none because Rust links them statically,
# but a different MinGW setup can change that.
copy_mingw_dlls() {
  local file=$1 dll found dir
  for dll in $(x86_64-w64-mingw32-objdump -p "$file" | sed -n 's/^[[:space:]]*DLL Name: //p' | sort -u); do
    [ -f "$OUT/$dll" ] && continue
    found=""
    for dir in ${MINGW_DIRS[@]+"${MINGW_DIRS[@]}"}; do
      if [ -f "$dir/$dll" ]; then found="$dir/$dll"; break; fi
    done
    # Not a MinGW file: it is part of Windows itself.
    [ -z "$found" ] && continue
    echo "    + $dll"
    cp "$found" "$OUT/$dll"
    copy_mingw_dlls "$OUT/$dll"
  done
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
  rm -rf "$OUT"
  mkdir -p "$OUT"
  cp "$BUILT/gitsot.exe" "$BUILT/gitsot.dll" "$OUT/"

  # Folders where the cross toolchain keeps its runtime DLLs.
  MINGW_DIRS=()
  for file in "$(x86_64-w64-mingw32-gcc -print-libgcc-file-name)" \
              "$(x86_64-w64-mingw32-gcc -print-file-name=libwinpthread-1.dll)" \
              /usr/x86_64-w64-mingw32/bin/. /usr/x86_64-w64-mingw32/lib/.; do
    # gcc prints just the name when it cannot find a file: skip those.
    case "$file" in /*) ;; *) continue ;; esac
    dir=$(dirname "$file")
    [ -d "$dir" ] && MINGW_DIRS+=("$dir")
  done
  echo "==> Collecting runtime DLLs"
  copy_mingw_dlls "$OUT/gitsot.exe"
  copy_mingw_dlls "$OUT/gitsot.dll"

  echo "==> Done: $OUT"
  ls -1 "$OUT" | sed 's/^/    /'
  exit 0
fi

echo "==> Building gitsot $VERSION ($PROFILE)"
cargo build "${CARGO_FLAGS[@]}"
BUILT="target/$PROFILE"
rm -rf "$OUT"
mkdir -p "$OUT"

if [ "$(uname -s)" = "Darwin" ]; then
  APP="$OUT/gitsot.app"
  mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks" "$APP/Contents/Resources"
  cp "$BUILT/gitsot" "$APP/Contents/MacOS/gitsot"
  cp "$BUILT/libgitsot.dylib" "$APP/Contents/Frameworks/libgitsot.dylib"
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
  # Ad-hoc sign the bundle so macOS lets the program load its library.
  if command -v codesign >/dev/null 2>&1; then
    codesign --force --sign - "$APP/Contents/Frameworks/libgitsot.dylib" >/dev/null 2>&1 || true
    codesign --force --sign - "$APP" >/dev/null 2>&1 || true
  fi
else
  cp "$BUILT/gitsot" "$BUILT/libgitsot.so" "$OUT/"
fi

echo "==> Done: $OUT"
ls -1 "$OUT" | sed 's/^/    /'
