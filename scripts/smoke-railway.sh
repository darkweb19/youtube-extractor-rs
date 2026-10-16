#!/usr/bin/env bash
set -euo pipefail
: "${BASE_URL:?Set BASE_URL to the deployed Railway HTTPS origin}"
[[ "$BASE_URL" == https://* ]] || { echo 'Railway smoke requires an HTTPS origin' >&2; exit 2; }
exec bash "$(dirname "$0")/smoke-youtube.sh"
