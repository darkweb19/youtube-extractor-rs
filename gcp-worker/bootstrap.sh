#!/usr/bin/env bash
# Compute Engine metadata startup-script-url entry point.
# Installs the latest verified GCS release on every VM boot, without Git or Rust.
set -Eeuo pipefail
umask 077

if [[ "$EUID" -ne 0 ]]; then
  echo "Worker bootstrap must run as root (Compute Engine startup script)." >&2
  exit 1
fi

exec 9>/run/lock/youtube-extractor-deploy.lock
flock -x 9

metadata_base="http://metadata.google.internal/computeMetadata/v1"
metadata() {
  curl --fail --silent --show-error --retry 3 --max-time 15 \
    -H "Metadata-Flavor: Google" \
    "$metadata_base/instance/attributes/$1"
}
access_token="$(
  curl --fail --silent --show-error --retry 3 --max-time 15 \
    -H "Metadata-Flavor: Google" \
    "$metadata_base/instance/service-accounts/default/token" |
    python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])'
)"

bucket="$(metadata gcp-worker-bucket)"
if [[ ! "$bucket" =~ ^[a-z0-9][a-z0-9._-]{2,61}[a-z0-9]$ ]]; then
  echo "Invalid gcp-worker-bucket instance metadata." >&2
  exit 1
fi

workdir="$(mktemp -d /var/tmp/youtube-extractor-deploy.XXXXXXXX)"
rollback_needed=false
had_old_binary=false
had_old_unit=false
cleanup() {
  local status=$?
  trap - EXIT
  if [[ "$status" -ne 0 && "$rollback_needed" == true ]]; then
    echo "Deployment failed; restoring previous worker binary and unit." >&2
    if [[ "$had_old_binary" == true ]]; then
      install -m 0755 "$workdir/previous-binary" /usr/local/bin/youtube-extractor.rollback
      mv -f /usr/local/bin/youtube-extractor.rollback /usr/local/bin/youtube-extractor
    fi
    if [[ "$had_old_unit" == true ]]; then
      install -m 0644 "$workdir/previous-unit" /etc/systemd/system/youtube-extractor.service
    fi
    systemctl daemon-reload || true
    if [[ "$had_old_binary" == true && "$had_old_unit" == true ]]; then
      systemctl restart youtube-extractor || true
    fi
  fi
  rm -rf "$workdir"
  exit "$status"
}
trap cleanup EXIT

download_object() {
  local object="$1" destination="$2"
  curl --fail --silent --show-error --location --retry 4 --retry-all-errors \
    --connect-timeout 10 --max-time 180 \
    -H "Authorization: Bearer $access_token" \
    -o "$destination" "https://storage.googleapis.com/$bucket/$object"
}

download_object "latest.json" "$workdir/latest.json"
manifest="$(
  python3 - "$workdir/latest.json" <<'PY'
import json
import re
import sys

with open(sys.argv[1], encoding="utf-8") as fh:
    release = json.load(fh)
commit = release.get("commit")
digest = release.get("sha256")
if not (isinstance(commit, str) and re.fullmatch(r"[a-f0-9]{40}", commit)):
    raise SystemExit("Invalid release commit in latest.json")
if not (isinstance(digest, str) and re.fullmatch(r"[a-f0-9]{64}", digest)):
    raise SystemExit("Invalid release sha256 in latest.json")
print(commit, digest)
PY
)"
read -r commit expected_sha256 <<< "$manifest"

state_dir="/var/lib/youtube-extractor-deploy"
install -d -m 0700 -o root -g root "$state_dir"
if [[ "$(cat "$state_dir/deployed-commit" 2>/dev/null || true)" == "$commit" ]] &&
    systemctl is-active --quiet youtube-extractor; then
  echo "Worker $commit already installed and active."
  exit 0
fi

download_object "releases/$commit/bundle.tar.gz" "$workdir/bundle.tar.gz"
printf '%s  %s\n' "$expected_sha256" "$workdir/bundle.tar.gz" |
  sha256sum --check --status
mkdir "$workdir/bundle"
tar -xzf "$workdir/bundle.tar.gz" -C "$workdir/bundle"
(
  cd "$workdir/bundle"
  sha256sum -c SHA256SUMS
)
test -s "$workdir/bundle/youtube-extractor"
for file in setup-ubuntu.sh install-service.sh youtube-extractor.service worker.env.example; do
  test -s "$workdir/bundle/$file"
done

# Optional zero-touch first install: retrieve the complete environment file
# from Secret Manager. On upgrades, never overwrite local credentials/settings.
if [[ ! -f /etc/youtube-extractor.env ]]; then
  secret_ref="$(metadata gcp-worker-env-secret 2>/dev/null || true)"
  if [[ ! "$secret_ref" =~ ^projects/[a-zA-Z0-9_-]+/secrets/[a-zA-Z0-9_-]+/versions/(latest|[0-9]+)$ ]]; then
    echo "Missing /etc/youtube-extractor.env; set gcp-worker-env-secret metadata." >&2
    exit 1
  fi
  curl --fail --silent --show-error --retry 3 --max-time 30 \
    -H "Authorization: Bearer $access_token" \
    -o "$workdir/secret.json" \
    "https://secretmanager.googleapis.com/v1/$secret_ref:access"
  python3 - "$workdir/secret.json" > "$workdir/worker.env" <<'PY'
import base64
import json
import sys

with open(sys.argv[1], encoding="utf-8") as fh:
    secret = json.load(fh)
sys.stdout.buffer.write(base64.b64decode(secret["payload"]["data"], validate=True))
PY
  install -m 0600 -o root -g root "$workdir/worker.env" /etc/youtube-extractor.env
fi

# Install/upgrade pinned runtime dependencies only when their setup changes.
setup_sha256="$(sha256sum "$workdir/bundle/setup-ubuntu.sh" | cut -d' ' -f1)"
if [[ "$(cat "$state_dir/setup-sha256" 2>/dev/null || true)" != "$setup_sha256" ]]; then
  bash "$workdir/bundle/setup-ubuntu.sh"
  printf '%s\n' "$setup_sha256" > "$state_dir/setup-sha256"
fi

if [[ -f /usr/local/bin/youtube-extractor ]]; then
  cp -a /usr/local/bin/youtube-extractor "$workdir/previous-binary"
  had_old_binary=true
fi
if [[ -f /etc/systemd/system/youtube-extractor.service ]]; then
  cp -a /etc/systemd/system/youtube-extractor.service "$workdir/previous-unit"
  had_old_unit=true
fi
rollback_needed=true
(
  cd "$workdir/bundle"
  bash install-service.sh
)

healthy=false
for _ in $(seq 1 30); do
  if systemctl is-active --quiet youtube-extractor &&
      curl --fail --silent --show-error --max-time 5 \
        http://127.0.0.1:8080/health >/dev/null 2>&1; then
    healthy=true
    break
  fi
  sleep 2
done
if [[ "$healthy" != true ]]; then
  echo "Worker failed local health check after deployment." >&2
  exit 1
fi

printf '%s\n' "$commit" > "$state_dir/deployed-commit"
rollback_needed=false
echo "Worker $commit installed; local /health passed."
