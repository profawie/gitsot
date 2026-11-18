#!/usr/bin/env bash
# Build (if needed) and start Gitsot on macOS or Linux.
#
#   ./run.sh                     open the last repository
#   ./run.sh /path/to/repo       open a specific repository
#   ./run.sh --debug [repo]      use a debug build
set -euo pipefail

FLAGS=(--release)
if [ "${1:-}" = "--debug" ]; then
  FLAGS=()
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
exec cargo run "${FLAGS[@]}" -- "${ARGS[@]+"${ARGS[@]}"}"
