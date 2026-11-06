# Operating GCP, Cloudflare Tunnel and WARP

Use this guide with the [architecture](architecture.md) and
[worker bundle installation](../gcp-worker/README.md). It is an operator runbook,
not a deployment record. The repository does not provision the Cloud Run
controller, VM/IAM, Cloudflare account, Access policy, tunnel or WARP registration.
The commands below are intended for the selected deployment; no successful local
eval proves that those resources exist or can retrieve YouTube media.

## Prepare the deployment

Choose one GCP project/zone/VM, an Ubuntu 24.04 amd64 persistent worker disk, one
stable HTTPS worker hostname, and one Railway service with a persistent `/data`
volume. Keep one Railway replica and one worker process. Set CPU, memory,
process and disk limits; the defaults are resource budgets rather than measured
capacity. Record the reviewed commit and the independently deployed controller
revision before installing anything.

Use separate secrets for the public API, controller, worker API, Access service
token and Tunnel connector. Store them in platform secret configuration or the
VM's root-owned environment/configuration files. Do not paste them into source,
screenshots, logs or support tickets. Railway needs no service-account private
key. Back up required records and output separately from TTL retention.

## Configure the keyless Cloud Run controller

Deploy a controller implementing the [exact HTTP contract](architecture.md#controller-and-worker-contracts).
There is no controller implementation in this repository. Configure its VM
target server-side; do not allow Railway callers to choose arbitrary projects,
zones or instances. Attach a dedicated runtime service account with only the
required `compute.instances.get`, `compute.instances.start` and
`compute.instances.stop` access to the intended target. Use the narrowest
supported resource bindings/conditions and verify both allowed and denied
operations; a broad editor role is unnecessary.

The controller must validate `X-Controller-Token` before reading or changing VM
state. Configure Cloud Run invocation and ingress to allow Railway's request
path: this client supplies that application token, **not** a Google IAM identity
token. If the service is reachable without Cloud Run IAM authentication, its
application authentication remains mandatory. Check unauthenticated requests
are rejected by the controller, as well as authorized status/start/stop requests.

Use an HTTPS origin for `GCP_CONTROLLER_URL`, without credentials, path, query
or fragment. Start/stop acceptance is not readiness. Confirm the controller
returns Compute Engine's actual state and handles transient lifecycle conflicts
with `409`. Avoid response redirects and HTML login pages. Do not return Google
API bodies, private resource details or credentials in public errors.

## Install and protect the VM worker

Prefer the [automated boot-time deployment](automated-gcp-deployment.md)
for the intended reviewed main commit. Keep the prebuilt ZIP in
[gcp-worker/README.md](../gcp-worker/README.md) as a manual recovery option. The artifact workflow builds for Ubuntu 24.04 amd64;
its archive includes the binary, installer scripts, service unit and example
configuration. Check the workflow provenance/commit before downloading and run
`sha256sum -c SHA256SUMS` after upload. The current checksum file covers the
binary; it is not a signature or a checksum of every installer file.

`setup-ubuntu.sh` installs FFmpeg, Node.js if missing, pinned yt-dlp and WARP;
it does not install/configure `cloudflared`, register WARP or deploy a controller.
If Node already exists, verify its version supports the selected yt-dlp runtime
rather than assuming the installer upgrades it. Keep package sources and TLS
verification enabled.

Create `/etc/youtube-extractor.env` using the worker template and edit it
privately before `install-service.sh`. Retain these settings:

```dotenv
BIND_HOST=127.0.0.1
PORT=8080
EXTRACTOR_MODE=local
DATA_DIR=/var/lib/youtube-extractor
YTDLP_PATH=/usr/local/bin/yt-dlp
YTDLP_PROXY=socks5://127.0.0.1:40000
YTDLP_PROXY_REQUIRED=true
```

The template's default local mode is sufficient; explicit `EXTRACTOR_MODE=local`
can make that role clear. Set worker `API_TOKEN` to a unique random secret,
matching Railway's `GCP_WORKER_TOKEN`. The installer creates an `extractor` system
user and a private writable data directory. Keep the environment file mode
`0600`, owned by root. The systemd unit reads it before changing user, requires
`warp-svc`, isolates filesystem writes and kills the service control group on
stop. Replacing an installed binary does not itself prove that the running
process uses it: restart the worker during a controlled maintenance window and
verify the selected revision afterward.

Keep inbound TCP 8080 and 40000 closed in VPC/firewall rules. Administrative SSH
is a separate policy; use the organization's supported access method. An
external VM IP is not needed for worker inbound requests, but the VM still
needs a permitted outbound route, such as its approved external-IP or Cloud NAT
configuration. Private Google Access alone does not supply general Cloudflare
and YouTube Internet connectivity.

## Configure WARP outbound proxy mode

Retain an existing valid registration. For a new VM, registration and terms
acceptance are an explicit operator step. The setup script only installs and
enables `warp-svc`. On the VM, configure the installed client's supported proxy
mode and port:

```bash
warp-cli mode proxy
warp-cli proxy port 40000
warp-cli connect
warp-cli status
curl --fail --silent --show-error \
  --proxy socks5h://127.0.0.1:40000 \
  https://www.cloudflare.com/cdn-cgi/trace
```

Confirm the trace reports an active WARP path; redact its public IP and other
unneeded fields before sharing evidence. This curl probe deliberately uses
`socks5h` so curl sends DNS through the proxy. The application's configured
value remains the `socks5://` form required by its startup/health checks. yt-dlp
is passed that proxy explicitly and uses its native downloader; this is not a
host-wide promise that every process uses WARP.

Verify the listener binds to loopback and reappears after VM reboot. Check the
installed WARP version's [proxy mode guidance](https://developers.cloudflare.com/warp-client/warp-modes/)
and permitted egress for its selected transport. Do not alter host routes,
disable TLS verification or add a direct-egress fallback to make a failing
probe pass. A connected WARP client and working trace do not establish YouTube
access or guarantee future access.

## Configure Tunnel and Access inbound routing

Install `cloudflared` separately using Cloudflare's supported package source.
Create a named tunnel and stable hostname, with its published application
service pointing to `http://127.0.0.1:8080` on this VM. Install the connector's
credential using the supported service/secret workflow, enable its systemd
service and verify reconnect after boot. Keep the connector credential private.
See [Cloudflare Tunnel setup](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/).

Create a Cloudflare Access self-hosted application covering the complete worker
hostname and every path. Give Railway's service token a **Service Auth** policy
and configure the token's intended lifetime/rotation. A browser login policy
alone cannot authenticate Railway's service calls. Do not add a bypass for
`/health`, `/api/v1/internal/activity`, file routes or cancellation. The
application bearer token still protects worker API routes behind Access.
See [Access service tokens](https://developers.cloudflare.com/cloudflare-one/access-controls/service-credentials/service-tokens/).

Permit the connector's required outbound tunnel connectivity, including port
7844 for the configured QUIC/HTTP2 transport, according to Cloudflare's current
[firewall requirements](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/configure-tunnels/tunnel-with-firewall/).
WARP has its own transport requirements. Test both with the actual VM firewall,
DNS and outbound network policy; opening an inbound HTTP/SOCKS port does not
repair either outbound path.

Verify these cases before enabling remote mode:

| Probe | Expected result |
| --- | --- |
| VM `http://127.0.0.1:8080/health` | `200` with worker prerequisites available |
| Worker hostname without valid Access credentials | Access rejection; no worker data |
| Worker hostname with Access credentials, `/health` | `200` through the selected tunnel |
| Protected API with Access credentials but missing/wrong worker bearer | Worker `401` |
| Protected API with both layers valid | Expected JSON contract, not an HTML login page |
| Redirect response | Investigate origin/routing; the application will not follow it |

The API's local `/health` is public, so the second probe specifically establishes
the edge protection. Validate large authenticated file downloads against the
chosen Cloudflare plan and timeout limits; do not infer file-transfer support
from a tiny health response or enable shared caching for bearer-protected media.

## Enable Railway remote mode

Store the secrets privately and set the non-secret settings from
[`.env.example`](../.env.example):

```dotenv
EXTRACTOR_MODE=remote
GCP_CONTROLLER_URL=https://controller.example.run.app
GCP_WORKER_URL=https://worker.example.com
GCP_IDLE_TIMEOUT_SECS=300
GCP_START_TIMEOUT_SECS=300
GCP_MAX_JOB_ATTEMPTS=2
DATA_DIR=/data
```

Also configure `GCP_CONTROLLER_TOKEN`, `GCP_WORKER_TOKEN`,
`CF_ACCESS_CLIENT_ID`, `CF_ACCESS_CLIENT_SECRET` and Railway's separate public
`API_TOKEN`. Keep `ALLOW_UNAUTHENTICATED=false`. Worker `YTDLP_PROXY` belongs on
the VM, not on Railway. If migrating from a service-account-key design, verify
the new controller path, remove old Railway key/target variables and revoke the
old key; do not retain it as a fallback.

Size Railway and worker budgets together. Worker queues, cold start, extraction
and final transfer must fit the Railway job deadline. Default metadata requests
have a 30-second API deadline and can time out during a 300-second cold start;
configure deliberate request/edge budgets or verify metadata with the worker
already warm. Refer to the [implemented timeout table](architecture.md#time-budgets-and-readiness)
before increasing any single value.

## Verify the whole lifecycle

Keep safe evidence identifying the commit, deployment, time, job ID and stage
that passed. Never save raw request headers or private URLs. Perform this pass
against the intended Railway origin using authorized test media:

1. Verify Railway health and that unauthenticated API access returns `401`.
2. Begin with the VM stopped. Submit a download; observe status/start in the
   controller, VM boot, WARP and Tunnel readiness, and worker dispatch. Railway
   `202` only means its local job was accepted.
3. Confirm expected worker states, final Railway `completed`, and authenticated
   local file retrieval. Probe actual MP3/MP4 streams and duration using
   `ffprobe`. The opt-in `scripts/smoke-railway.sh` covers metadata/audio/video
   through the selected origin; a cold metadata timeout may require warming the
   worker or adjusting the deliberate request budget first.
4. Cancel one running job and confirm both local terminal state and worker
   cleanup. Check disk usage after failed and cancelled transfers.
5. After all operations finish, observe an authenticated `busy: false`, an idle
   stop request, and the actual VM `TERMINATED` state. Retained Railway files
   should remain available until their TTL expires.
6. Submit again from a stopped VM. Separately exercise controlled Railway/worker
   restarts and verify queued replay, running-job interruption and retention
   against the [persistence contract](architecture.md#persistence-capacity-and-availability).

On the VM, these checks are read-only:

```bash
systemctl is-enabled warp-svc youtube-extractor cloudflared
systemctl is-active warp-svc youtube-extractor cloudflared
ss -ltnp
warp-cli status
curl --fail --silent --show-error http://127.0.0.1:8080/health
sudo journalctl -u youtube-extractor -n 100 --no-pager
```

Inspect logs privately and redact before sharing. The systemd dependency waits
for the WARP service process, not guaranteed end-to-end proxy readiness. Tunnel
and WARP recovery after a complete VM boot are part of acceptance.

## Diagnose the failing layer

| Symptom | First checks |
| --- | --- |
| Controller `401` / `403` | Controller secret, Cloud Run invocation/ingress; if the handler ran, runtime VM IAM |
| Controller `404` / `405` | Final origin, required route/method, configured VM project/zone/name |
| Controller `3xx` or malformed status | Final HTTPS origin and contract; no redirect following |
| VM `RUNNING`, readiness times out | Boot logs, all three services, loopback health, Access policy, DNS and Tunnel connector |
| Worker hostname login HTML / `403` | Access application coverage, Service Auth policy, service-token expiry |
| Worker API `401` | Worker `API_TOKEN` matches Railway `GCP_WORKER_TOKEN`; Access success is separate |
| Local worker health fails | Writable data directory, FFmpeg/FFprobe, proxy listener, `warp-cli status` |
| Metadata `ACCESS_RESTRICTED` | Worker HTTP/Access status first; current mapping does not prove a YouTube denial |
| Download repeats after worker `failed` | Bounded extraction retry; current consumer discards the worker's original failure code |
| Transfer incomplete / too large | File size contract, both byte budgets, disk, tunnel/client timeout and network interruption |
| VM never stops | Active operations, worker `busy`, unavailable activity/controller, idle watcher; unknown activity prevents stop |
| Repeated cold-start timeout | All nested deadlines, VM boot and Tunnel/WARP readiness; do not assume raising one budget raises all |

Use Railway's `remote_job` span and safe `job_id` with Cloud Run request/service
logs and VM logs. `RUST_LOG=info,youtube_extractor::remote=debug` adds idle skip
reasons. Structured events distinguish controller method/path/status, worker
contract failures and transfer stages. An HTTP `200` from Railway job polling
can contain a failed job; inspect its state and error. A proxy-policy rejection
is not proof of YouTube incompatibility. Do not publish response bodies,
credentials or upstream video URLs to collect diagnostics.

## Bound costs and recover safely

Configure an independent maximum VM run duration with termination action STOP
as described in the [worker guide](../gcp-worker/README.md#vm-startup-and-shutdown).
Choose it above the intended workload budget and verify platform support; it can
interrupt a legitimate long job. The application's idle watcher cannot stop the
VM while Railway is down, credentials have expired or the controller fails.
Alert on unexpectedly long runtime, repeated start failures, retained disk usage
and failed idle stops. Include persistent disks, network egress, Cloud Run and
Cloudflare plan costs in budgeting; stopping compute does not erase these costs.

For maintenance or rollback, pause new admissions at the service's supported
edge/operator control, allow or deliberately cancel in-flight jobs, retain both
data disks, and deploy the last verified image/binary with compatible
configuration. Keep controller and worker contract compatibility in view when
rolling either back independently. Restart the worker after replacing its
binary; verify local health, authenticated tunnel access and Railway acceptance
again. Do not delete WARP registration or tunnel credentials during an ordinary
binary rollback. If idle control is unavailable, inspect active work and stop
the intended VM through the authorized GCP operator path. Never claim a
rollback restored extraction until the live acceptance checks have passed.
