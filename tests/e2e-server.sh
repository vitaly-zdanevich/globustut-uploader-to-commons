#!/usr/bin/env bash
set -euo pipefail

DATA_ROOT="$(mktemp -d -t globustut-e2e.XXXXXX)"
SERVER_PID=""
MOCK_PID=""
cleanup() {
  if [[ -n "$SERVER_PID" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  if [[ -n "$MOCK_PID" ]]; then
    kill "$MOCK_PID" 2>/dev/null || true
    wait "$MOCK_PID" 2>/dev/null || true
  fi
  rm -rf "$DATA_ROOT"
}
trap cleanup EXIT INT TERM

export TOOL_DATA_DIR="$DATA_ROOT/data"
export APP_TEMP_DIR="$DATA_ROOT/tmp"
export DATABASE_PATH="$DATA_ROOT/data/e2e.sqlite3"
export PORT="${E2E_PORT:-4173}"
export E2E_MOCK_PORT="${E2E_MOCK_PORT:-$((PORT + 1))}"
export BIND_ADDR=127.0.0.1
export PUBLIC_BASE_URL="http://127.0.0.1:$PORT"
export COOKIE_SECURE=false
export DISK_RESERVE_MB=0
export COMMONS_API_URL="http://127.0.0.1:$E2E_MOCK_PORT/w/api.php"
export COMMONS_BOT_PASSWORD_FILE="$DATA_ROOT/data/commons-bot-password"
export WIKIDATA_SPARQL_URL="http://127.0.0.1:$E2E_MOCK_PORT/sparql"
export E2E_ADMIN_PASSWORD='correct horse battery staple'
export E2E_COMMONS_BOT_PASSWORD='mock Commons BotPassword'
export RUST_LOG=warn

node ./tests/mock-wikimedia-server.mjs &
MOCK_PID="$!"
cargo run --quiet -- user add moderator --password-env E2E_ADMIN_PASSWORD
cargo run --quiet -- commons set-password --password-env E2E_COMMONS_BOT_PASSWORD
cargo run --quiet -- serve &
SERVER_PID="$!"
wait "$SERVER_PID"
