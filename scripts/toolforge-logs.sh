#!/usr/bin/env bash
set -euo pipefail

if ! command -v toolforge >/dev/null 2>&1; then
  echo "The toolforge CLI is required; run this script on a Toolforge bastion." >&2
  exit 1
fi

exec toolforge webservice logs -f
