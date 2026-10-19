#!/usr/bin/env bash
# Deterministic checks: no external media or YouTube access.
set -euo pipefail
image="${IMAGE:-youtube-extractor:local}"
work_dir="$(mktemp -d)"
export APIFY_API_TOKEN="smoke-test-placeholder-token-not-valid"
container_id=""
missing_token_id=""
cleanup() {
  if [[ -n "$container_id" ]]; then docker rm -f "$container_id" >/dev/null 2>&1 || true; fi
  if [[ -n "$missing_token_id" ]]; then docker rm -f "$missing_token_id" >/dev/null 2>&1 || true; fi
  rm -rf "$work_dir"
}
trap cleanup EXIT
export API_TOKEN="$(python3 -c 'import secrets; print(secrets.token_urlsafe(32))')"
container_id="$(docker run --detach --publish 127.0.0.1::8080 --env API_TOKEN --env APIFY_API_TOKEN "$image")"
port="$(docker port "$container_id" 8080/tcp | sed -n 's/127.0.0.1://p')"
base_url="http://127.0.0.1:$port"
ready=false
for _ in $(seq 1 60); do
  if curl --fail --silent --max-time 2 "$base_url/health" > "$work_dir/health.json"; then ready=true; break; fi
  sleep 1
done
if [[ "$ready" != true ]]; then docker logs "$container_id"; echo 'Container did not become healthy' >&2; exit 1; fi
[[ "$(docker inspect --format '{{.Config.User}}' "$container_id")" == '10001:10001' ]]
docker exec "$container_id" ffmpeg -version > /dev/null
status="$(curl --silent --output "$work_dir/error.json" --write-out '%{http_code}' --header 'Content-Type: application/json' --data '{}' "$base_url/api/v1/videos/info")"
[[ "$status" == '401' ]]
python3 - "$work_dir/error.json" <<'PY'
import json, sys
assert json.load(open(sys.argv[1]))["error"]["code"] == "UNAUTHORIZED"
PY
printf 'Authorization: Bearer %s\n' "$API_TOKEN" > "$work_dir/auth.conf"
chmod 600 "$work_dir/auth.conf"
status="$(curl --header "@$work_dir/auth.conf" --silent --output "$work_dir/invalid.json" --write-out '%{http_code}' --header 'Content-Type: application/json' --data '{"url":"https://127.0.0.1/private"}' "$base_url/api/v1/videos/info")"
[[ "$status" == '400' ]]
python3 - "$work_dir/invalid.json" <<'PY'
import json, sys
assert json.load(open(sys.argv[1]))["error"]["code"] == "INVALID_REQUEST"
PY
# Production must fail closed without the required token.
missing_token_id="$(docker run --detach "$image")"
for _ in $(seq 1 15); do
  [[ "$(docker inspect --format '{{.State.Running}}' "$missing_token_id")" == 'false' ]] && break
  sleep 1
done
[[ "$(docker inspect --format '{{.State.Running}}' "$missing_token_id")" == 'false' ]]
[[ "$(docker inspect --format '{{.State.ExitCode}}' "$missing_token_id")" != '0' ]]
echo 'PASS: container health, non-root runtime, FFmpeg, auth, URL rejection, and missing-token startup refusal'
