#!/usr/bin/env bash
set -euo pipefail
# Run from any directory in a checked out repository. Only the generated bundle goes to GCP.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
(cd "$ROOT" && cargo build --release --locked)
DEST="$ROOT/gcp-worker/bundle"
mkdir -p "$DEST"
install -m 0755 "$ROOT/target/release/youtube-extractor" "$DEST/youtube-extractor"
install -m 0755 "$ROOT/gcp-worker/setup-ubuntu.sh" "$DEST/setup-ubuntu.sh"
install -m 0755 "$ROOT/gcp-worker/install-service.sh" "$DEST/install-service.sh"
install -m 0644 "$ROOT/gcp-worker/youtube-extractor.service" "$DEST/youtube-extractor.service"
install -m 0644 "$ROOT/gcp-worker/worker.env.example" "$DEST/worker.env.example"
echo "Copy only gcp-worker/bundle/ to the VM. Never package credentials."
