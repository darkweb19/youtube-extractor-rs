#!/usr/bin/env bash
set -euo pipefail
# Run from any directory. Build Linux/amd64 on any Docker-capable host.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
command -v docker >/dev/null || { echo "Docker is required for a Linux/amd64 build." >&2; exit 1; }
docker run --rm --platform linux/amd64 -v "$ROOT:/src" -w /src \
  -e CARGO_TARGET_DIR=/src/target/gcp-linux -e CARGO_BUILD_JOBS=2 \
  rust:1.99.1-bookworm cargo build --release --locked
DEST="$ROOT/gcp-worker/bundle"
mkdir -p "$DEST"
install -m 0755 "$ROOT/target/gcp-linux/release/youtube-extractor" "$DEST/youtube-extractor"
install -m 0755 "$ROOT/gcp-worker/setup-ubuntu.sh" "$DEST/setup-ubuntu.sh"
install -m 0755 "$ROOT/gcp-worker/install-service.sh" "$DEST/install-service.sh"
install -m 0644 "$ROOT/gcp-worker/youtube-extractor.service" "$DEST/youtube-extractor.service"
install -m 0644 "$ROOT/gcp-worker/worker.env.example" "$DEST/worker.env.example"
echo "Only copy gcp-worker/bundle/ to the VM; no source tree or secrets required."
