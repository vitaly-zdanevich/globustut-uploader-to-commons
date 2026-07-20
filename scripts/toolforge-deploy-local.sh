#!/usr/bin/env bash
set -euo pipefail

TOOLFORGE_USER="${TOOLFORGE_USER:-vitaly-zdanevich}"
TOOLFORGE_HOST="${TOOLFORGE_HOST:-login.toolforge.org}"
TOOLFORGE_TOOL="${TOOLFORGE_TOOL:?Set TOOLFORGE_TOOL to the Toolforge tool name}"
REPO="${REPO:-https://github.com/vitaly-zdanevich/globustut-uploader-to-commons}"
REMOTE_SOURCE_DIR="${REMOTE_SOURCE_DIR:-/data/project/$TOOLFORGE_TOOL/source}"
SSH_TARGET="$TOOLFORGE_USER@$TOOLFORGE_HOST"

for value in "$TOOLFORGE_TOOL" "$REPO" "$REMOTE_SOURCE_DIR"; do
  if [[ "$value" == *"'"* ]]; then
    echo "Tool name, repository URL, and remote path must not contain single quotes." >&2
    exit 1
  fi
done

echo "==> Updating $SSH_TARGET:$REMOTE_SOURCE_DIR"
ssh "$SSH_TARGET" "become '$TOOLFORGE_TOOL' bash -lc 'if [[ -d \"$REMOTE_SOURCE_DIR/.git\" ]]; then git -C \"$REMOTE_SOURCE_DIR\" pull --ff-only; else git clone \"$REPO\" \"$REMOTE_SOURCE_DIR\"; fi; cd \"$REMOTE_SOURCE_DIR\"; REPO=\"$REPO\" ./scripts/toolforge-deploy.sh'"
