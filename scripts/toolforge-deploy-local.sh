#!/usr/bin/env bash
set -euo pipefail

TOOLFORGE_USER="${TOOLFORGE_USER:-vitaly-zdanevich}"
TOOLFORGE_HOST="${TOOLFORGE_HOST:-login.toolforge.org}"
TOOLFORGE_TOOL="${TOOLFORGE_TOOL:-globustut-uploader-to-commons}"
TOOLFORGE_SSH_CONFIG="${TOOLFORGE_SSH_CONFIG:-}"
REPO="${REPO:-https://github.com/vitaly-zdanevich/globustut-uploader-to-commons}"
REMOTE_SOURCE_DIR="${REMOTE_SOURCE_DIR:-/data/project/$TOOLFORGE_TOOL/source}"
SSH_TARGET="$TOOLFORGE_USER@$TOOLFORGE_HOST"

for value in "$TOOLFORGE_TOOL" "$REPO" "$REMOTE_SOURCE_DIR"; do
  if [[ "$value" == *"'"* ]]; then
    echo "Tool name, repository URL, and remote path must not contain single quotes." >&2
    exit 1
  fi
done

ssh_args=()
if [[ -n "$TOOLFORGE_SSH_CONFIG" ]]; then
  ssh_args=(-F "$TOOLFORGE_SSH_CONFIG")
else
  for ssh_config_path in /etc/ssh/ssh_config /etc/ssh/ssh_config.d/*; do
    [[ -e "$ssh_config_path" ]] || continue
    if [[ "$(stat -c %u "$ssh_config_path" 2>/dev/null || printf 0)" != 0 ]]; then
      ssh_args=(-F /dev/null)
      break
    fi
  done
fi

echo "==> Updating $SSH_TARGET:$REMOTE_SOURCE_DIR"
ssh "${ssh_args[@]}" "$SSH_TARGET" "become '$TOOLFORGE_TOOL' bash -lc 'if [[ -d \"$REMOTE_SOURCE_DIR/.git\" ]]; then git -C \"$REMOTE_SOURCE_DIR\" pull --ff-only; else git clone \"$REPO\" \"$REMOTE_SOURCE_DIR\"; fi; cd \"$REMOTE_SOURCE_DIR\"; REPO=\"$REPO\" ./scripts/toolforge-deploy.sh'"
