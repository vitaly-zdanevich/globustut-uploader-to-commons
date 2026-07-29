#!/usr/bin/env bash
set -euo pipefail

# Run on a Toolforge bastion after: become YOUR_TOOL
REPO="${REPO:-https://github.com/vitaly-zdanevich/globustut-uploader-to-commons}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMMIT_SHA="$(git -C "$ROOT_DIR" rev-parse HEAD)"

if ! command -v toolforge >/dev/null 2>&1; then
  echo "The toolforge CLI is required; run this script on a Toolforge bastion." >&2
  exit 1
fi

if [[ "${LOGNAME:-}" != tools.* ]]; then
  echo "Run 'become <toolname>' before deploying." >&2
  exit 1
fi

TOOL_NAME="${TOOL_NAME:-${LOGNAME#tools.}}"
HEALTH_URL="${HEALTH_URL:-https://$TOOL_NAME.toolforge.org/healthz}"

TOOL_DATA_DIR="${TOOL_DATA_DIR:-$HOME}/globustut-uploader"
install -d -m 700 \
  "$TOOL_DATA_DIR" \
  "$TOOL_DATA_DIR/pending" \
  "$TOOL_DATA_DIR/thumbnails"

echo "==> Installing service.template"
cp "$ROOT_DIR/toolforge/service.template" "$HOME/service.template"

echo "==> Building $REPO at $COMMIT_SHA"
toolforge build start --ref "$COMMIT_SHA" "$REPO"

echo "==> Starting or refreshing the webservice"
if toolforge webservice status >/dev/null 2>&1; then
  toolforge webservice restart
else
  toolforge webservice start
fi

echo "==> Status"
toolforge webservice status
echo "==> Waiting for $HEALTH_URL"
healthy=0
for _ in $(seq 1 45); do
  if curl -fsS "$HEALTH_URL" >/dev/null 2>&1; then
    healthy=1
    break
  fi
  sleep 2
done
if [[ "$healthy" != 1 ]]; then
  echo "Webservice did not become healthy after 90 seconds." >&2
  exit 1
fi
echo "Webservice is healthy."
echo "Logs: toolforge webservice logs -f"
