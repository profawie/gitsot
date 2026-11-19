#!/usr/bin/env bash
# Build gitsot (see build.sh) and start it on macOS or Linux.
#
#   ./run.sh                     open the last repository
#   ./run.sh /path/to/repo       open a specific repository
#   ./run.sh --debug [repo]      use a debug build
set -euo pipefail

BUILD_FLAGS=()
if [ "${1:-}" = "--debug" ]; then
  BUILD_FLAGS=(--debug)
  shift
fi

ARGS=()
if [ -n "${1:-}" ]; then
  if [ -d "$1" ]; then
    ARGS=("$(cd "$1" && pwd)")
  else
    echo "error: '$1' is not a folder" >&2
    exit 1
  fi
fi

cd "$(dirname "$0")"
./build.sh ${BUILD_FLAGS[@]+"${BUILD_FLAGS[@]}"}

if [ "$(uname -s)" = "Darwin" ]; then
  EXE=dist/gitsot/gitsot.app/Contents/MacOS/gitsot
else
  EXE=dist/gitsot/gitsot
fi
exec "$EXE" ${ARGS[@]+"${ARGS[@]}"}
