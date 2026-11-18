#!/usr/bin/env bash
# Build Gitsot on macOS or Linux.
#
#   ./build.sh            release build for this machine
#   ./build.sh --debug    debug build
#   ./build.sh --windows  cross-compile a Windows .exe with the GNU toolchain
#                         (needs mingw-w64, e.g. `brew install mingw-w64`
#                          or `apt install gcc-mingw-w64-x86-64`)
#
# Output goes to ./dist. On macOS a Gitsot.app bundle is created as well.
set -euo pipefail
cd "$(dirname "$0")"

PROFILE=release
WINDOWS=0
for arg in "$@"; do
  case "$arg" in
    --debug) PROFILE=debug ;;
    --release) PROFILE=release ;;
    --windows) WINDOWS=1 ;;
    -h|--help) sed -n '2,11p' "$0"; exit 0 ;;
    *) echo "Unknown option: $arg" >&2; exit 1 ;;
  esac
done

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found. Install Rust from https://rustup.rs" >&2
  exit 1
fi

CARGO_FLAGS=()
[ "$PROFILE" = "release" ] && CARGO_FLAGS+=(--release)

mkdir -p dist

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
  echo "==> Building Gitsot for Windows ($TARGET, $PROFILE)"
  cargo build "${CARGO_FLAGS[@]}" --target "$TARGET"
  cp "target/$TARGET/$PROFILE/gitsot.exe" dist/gitsot.exe
  echo "==> Done: dist/gitsot.exe"
  exit 0
fi

echo "==> Building Gitsot ($PROFILE)"
cargo build "${CARGO_FLAGS[@]}"
cp "target/$PROFILE/gitsot" dist/gitsot

if [ "$(uname -s)" = "Darwin" ]; then
  APP=dist/Gitsot.app
  VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
  rm -rf "$APP"
  mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
  cp "target/$PROFILE/gitsot" "$APP/Contents/MacOS/gitsot"
  cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Gitsot</string>
  <key>CFBundleDisplayName</key><string>Gitsot</string>
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
  echo "==> Done: dist/gitsot and $APP"
else
  echo "==> Done: dist/gitsot"
fi
