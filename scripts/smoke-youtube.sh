#!/usr/bin/env bash
# Explicit external compatibility check. Only use media you may download.
set -euo pipefail
: "${BASE_URL:?Set BASE_URL to the running local or Railway service}"
: "${API_TOKEN:?Set API_TOKEN through your environment}"
: "${YOUTUBE_TEST_URL:?Set an authorized public YouTube test video URL}"
: "${ALLOW_EXTERNAL_YOUTUBE:?Set ALLOW_EXTERNAL_YOUTUBE=1 to opt into external downloads}"
[[ "$ALLOW_EXTERNAL_YOUTUBE" == '1' ]] || { echo 'External tests require ALLOW_EXTERNAL_YOUTUBE=1' >&2; exit 2; }
[[ ! "$API_TOKEN" =~ [[:space:][:cntrl:]] ]] || { echo 'API_TOKEN must contain no whitespace or control characters' >&2; exit 2; }
command -v ffprobe >/dev/null
command -v python3 >/dev/null
base_url="${BASE_URL%/}"
output_dir="${SMOKE_OUTPUT_DIR:-$(mktemp -d)}"
mkdir -p "$output_dir"
chmod 700 "$output_dir"
auth_file="$(mktemp)"
chmod 600 "$auth_file"
printf 'Authorization: Bearer %s\n' "$API_TOKEN" > "$auth_file"
current_job=""
cleanup() {
  if [[ -n "$current_job" ]]; then
    curl --header "@$auth_file" --silent --max-time 15 --request DELETE \
      "$base_url/api/v1/downloads/$current_job" > /dev/null || true
  fi
  rm -f "$auth_file"
}
trap cleanup EXIT
request() {
  local path="$1" destination="$2"
  shift 2
  local status
  status="$(curl --header "@$auth_file" --silent --show-error --max-time "${SMOKE_HTTP_TIMEOUT_SECS:-1800}" --output "$destination" --write-out '%{http_code}' "$@" "$base_url$path")"
  if [[ "$status" != 2* ]]; then
    echo "HTTP $status at $path; diagnostic saved in $destination" >&2
    cat "$destination" >&2
    return 1
  fi
}
python3 - "$output_dir" <<'PY'
import json, os, pathlib, sys
root = pathlib.Path(sys.argv[1])
url = os.environ["YOUTUBE_TEST_URL"]
(root / "info-request.json").write_text(json.dumps({"url": url}))
for kind, fmt, quality in [("audio", "mp3", "best"), ("video", "mp4", "360p")]:
    request = {"url": url, "type": kind, "format": fmt, "quality": quality}
    if kind == "audio":
        request["audio_bitrate_kbps"] = 128
    (root / (kind + "-request.json")).write_text(json.dumps(request))
PY
request /health "$output_dir/health.json"
request /api/v1/videos/info "$output_dir/info.json" --header 'Content-Type: application/json' --data-binary "@$output_dir/info-request.json"
python3 - "$output_dir/info.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1]))
assert value["title"], "Metadata title is missing"
assert isinstance(value["formats"], list), "Metadata formats field must be an array"
PY
for kind in audio video; do
  request /api/v1/downloads "$output_dir/$kind-created.json" --header 'Content-Type: application/json' --data-binary "@$output_dir/$kind-request.json"
  job_id="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["job_id"])' "$output_dir/$kind-created.json")"
  current_job="$job_id"
  completed=false
  for _ in $(seq 1 "${SMOKE_POLL_ATTEMPTS:-900}"); do
    request "/api/v1/downloads/$job_id" "$output_dir/$kind-status.json"
    state="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["status"])' "$output_dir/$kind-status.json")"
    case "$state" in
      completed) completed=true; break ;;
      failed|cancelled) cat "$output_dir/$kind-status.json" >&2; exit 1 ;;
      queued|running) sleep 2 ;;
      *) echo "Unknown job status: $state" >&2; exit 1 ;;
    esac
  done
  [[ "$completed" == true ]] || { echo "Timed out waiting for $kind job $job_id" >&2; exit 1; }
  request "/api/v1/downloads/$job_id/file" "$output_dir/$kind-media"
  [[ -s "$output_dir/$kind-media" ]]
  ffprobe -v error -show_streams -show_format -of json "$output_dir/$kind-media" > "$output_dir/$kind-probe.json"
  python3 - "$output_dir/$kind-probe.json" "$kind" <<'PY'
import json, sys
probe = json.load(open(sys.argv[1]))
types = {stream["codec_type"] for stream in probe["streams"]}
assert "audio" in types, "Output must contain actual audio"
if sys.argv[2] == "video":
    assert "video" in types, "Output must contain actual video"
assert float(probe["format"]["duration"]) > 0
PY
  request "/api/v1/downloads/$job_id" "$output_dir/$kind-delete.json" --request DELETE
  current_job=""
done
echo "PASS: external metadata, MP3 audio and MP4 audio/video bytes verified; evidence in $output_dir"
