#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TOOL_NAME:-}" ]]; then
  TOOL_HOME="/data/project/$TOOL_NAME"
else
  TOOL_HOME="${HOME:?HOME is not set}"
fi

if [[ -n "${TOOL_DATA_DIR:-}" ]]; then
  TOOL_DATA_DIR="$TOOL_DATA_DIR/globustut-uploader"
else
  TOOL_DATA_DIR="$TOOL_HOME/globustut-uploader"
fi
export TOOL_DATA_DIR
export APP_TEMP_DIR="${APP_TEMP_DIR:-/tmp/globustut-uploader}"
install -d -m 700 "$TOOL_DATA_DIR" "$TOOL_DATA_DIR/pending" "$TOOL_DATA_DIR/thumbnails" "$APP_TEMP_DIR"

if [[ "$#" -eq 0 ]]; then
  set -- serve
fi
exec ./target/release/globustut-uploader-to-commons "$@"
