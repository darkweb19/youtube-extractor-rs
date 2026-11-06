# Automatic GCP worker deployment

This guide replaces the repeated **GitHub Actions ZIP download → browser SSH
upload → manual binary install** cycle. It applies to
`darkweb19/youtube-extractor-rs` and the existing
**Railway → Cloud Run controller → Compute Engine worker** architecture.

## Deployment lifecycle

1. A push to `main` that changes the worker, embedded web assets, deployment
   scripts, or Rust dependencies starts
   [GCP Worker - Build and Deploy](../.github/workflows/gcp-artifact.yml).
   You can also run it manually from the workflow's **Run workflow** control
   on `main`. A feature-branch run still builds a recovery ZIP but cannot deploy.
2. The workflow runs Rust tests, compiles the pinned Ubuntu 24.04 amd64
   release binary, and checks the deployment scripts.
3. GitHub Actions authenticates to Google Cloud using **Workload Identity
   Federation** (no JSON service-account key). It uploads a bundle to the
   **private** Cloud Storage bucket as
   `releases/<commit-sha>/bundle.tar.gz` and publishes `bootstrap.sh`.
   After updating the VM instance metadata, it atomically publishes
   `latest.json` as the release pointer. Each manifest records the
   expected archive SHA-256.
4. When the existing VM next boots (including a start initiated by Railway),
   the Compute Engine guest agent runs `bootstrap.sh`. The script reads its
   assigned service-account token, retrieves the private bundle, validates
   hashes, installs dependencies if their setup script changed, and calls the
   repository's existing `install-service.sh`.
5. The script verifies `systemd` and local `/health`, then writes the
   deployed commit to `/var/lib/youtube-extractor-deploy/deployed-commit`.
   On failed installation or health check, it attempts to restore the
   previously installed binary and unit. It **does not** overwrite an existing
   `/etc/youtube-extractor.env`, Tunnel registration, or WARP configuration.

**Important:** A push does **not** restart a running worker. Updates become
active on **the next VM boot**. This deliberately prevents an ongoing download
from being interrupted by CI. Do not assume a GitHub Actions success means
that the stopped VM has already installed the release or that YouTube access
works. The first boot on a new VM may take several minutes while apt, Deno,
FFmpeg and yt-dlp are installed; allow for this in Railway's worker startup
timeout.

## One-time setup

Prerequisites:

- A private, uniform-bucket-level-access Cloud Storage bucket (for example,
  `$GCP_WORKER_BUCKET`). Disable public access. Enable the Compute Engine,
  IAM Credentials, Security Token Service, Cloud Storage, and, if used,
  Secret Manager APIs.
- A dedicated **publisher** service account for GitHub Actions with
  `roles/storage.objectAdmin` on that bucket and the narrowly-scoped
  permissions `compute.instances.get` and `compute.instances.setMetadata`
  for the target instance (via a custom IAM role / appropriately restricted
  binding). Do not grant Owner or Editor.
- A Workload Identity Pool/Provider trusting GitHub's OIDC issuer
  `https://token.actions.githubusercontent.com`, constrained to
  `assertion.repository == 'darkweb19/youtube-extractor-rs'` **and**
  `assertion.ref == 'refs/heads/main'`. Map `google.subject=assertion.sub`,
  `attribute.repository=assertion.repository` and
  `attribute.ref=assertion.ref`. Permit this identity to impersonate only
  the publisher service account with `roles/iam.workloadIdentityUser`.
  Restrict the GitHub deployment environment to the `main` branch.
- The **VM's attached service account**, separate from the publisher, must
  have `roles/storage.objectViewer` on the bucket. Its OAuth access scopes
  must permit Storage access; use `cloud-platform` if pulling the optional
  Secret Manager configuration. Do not place OAuth tokens or private keys in
  GitHub variables or Compute Engine instance metadata.
- The VM must run Ubuntu 24.04 amd64 with the Google guest environment
  installed (as on public Ubuntu Compute Engine images), network access to
  Google APIs/package sources, and sufficient boot time and disk space.

Create a GitHub **environment** named `gcp-worker-production` and configure
these **environment variables** (not credentials/secrets):

| Variable | Value |
| --- | --- |
| `GCP_PROJECT_ID` | GCP project ID for the target VM |
| `GCP_WORKLOAD_IDENTITY_PROVIDER` | Full provider name: `projects/PROJECT_NUMBER/locations/global/workloadIdentityPools/POOL/providers/PROVIDER` |
| `GCP_DEPLOY_SERVICE_ACCOUNT` | Publisher service-account email |
| `GCP_WORKER_BUCKET` | Bucket name only, without `gs://` |
| `GCP_VM_ZONE` | Zone of the existing worker, e.g. `us-central1-a` |
| `GCP_VM_NAME` | Exact Compute Engine instance name |
| `GCP_WORKER_ENV_SECRET` | Optional: `projects/PROJECT_ID/secrets/SECRET_NAME/versions/latest` |

No service-account JSON key or SSH private key is required.

### First-time worker configuration

**Already configured VM:** Keep its root-owned
`/etc/youtube-extractor.env` and existing `cloudflared` systemd service.
The first automated boot will reuse both. The workflow updates instance
metadata keys `startup-script-url` and `gcp-worker-bucket`; check that
the instance does not already depend on a different `startup-script-url`.
Compute Engine runs the stored script every boot.

**New/recreated VM:** To avoid manually creating the worker environment,
store the complete `/etc/youtube-extractor.env` file in **Secret Manager**
and set `GCP_WORKER_ENV_SECRET` to that secret version. Grant only the
VM service account `roles/secretmanager.secretAccessor` on that secret.
On first boot the bootstrap retrieves it into a root-only mode-`0600` file.
Include a strong `API_TOKEN` matching Railway's `GCP_WORKER_TOKEN`,
`BIND_HOST=127.0.0.1`, `PORT=8080`, `DATA_DIR=/var/lib/youtube-extractor`,
and the other settings from
[`worker.env.example`](../gcp-worker/worker.env.example).
On subsequent boots the local environment file is preserved even if the
Secret Manager value changes. **Cloudflare Tunnel enrollment and Access
policies remain separate one-time infrastructure provisioning**; this workflow
does not install or register `cloudflared`, create VMs, update the Cloud Run
controller, or edit Railway variables. To make a newly created VM genuinely
ready to receive traffic, provide the same stable private Tunnel/Access setup
or use a preconfigured VM image.

After setting the environment variables, run the workflow once on `main`.
It registers the following VM instance metadata:

- `startup-script-url=gs://BUCKET/bootstrap.sh`
- `gcp-worker-bucket=BUCKET`
- Optionally `gcp-worker-env-secret=projects/.../versions/latest`

The script is downloaded through the guest environment on every boot.
A newly created VM or instance template needs those same metadata keys and
the correct attached VM service account. The workflow targets **one named
existing instance**; it does not provision a replacement instance automatically.

## Verify without guessing

1. In GitHub Actions, confirm the `package` and `deploy` jobs both passed.
   Confirm `gs://BUCKET/latest.json` contains the intended 40-character
   commit. This proves **publication**, not installation.
2. Start the intended VM through the existing controller / Railway workflow
   or during a maintenance window. Inspect its startup-script logs:

   ```bash
   sudo journalctl -u google-startup-scripts.service -b --no-pager
   sudo cat /var/lib/youtube-extractor-deploy/deployed-commit
   sudo systemctl status youtube-extractor --no-pager
   curl -fsS http://127.0.0.1:8080/health
   sudo systemctl status cloudflared --no-pager
   ```

3. Confirm the commit matches `latest.json`, that the existing private
   Cloudflare route accepts an authenticated request, and that Railway can
   dispatch/complete a real permitted test download. Neither local health
   nor a completed workflow proves live YouTube availability.

## Failure and rollback

- **Workflow fails before updating `latest.json`:** No new release is
  published for boot-time consumption. Examine GitHub Actions IAM/bucket/VM
  permissions; do not add a long-lived JSON key as a workaround.
- **VM cannot download the bundle:** Check the VM service account,
  OAuth scopes, Cloud Storage object-viewer permission and egress to Google
  APIs. The bootstrap fails closed rather than installing an unverified ZIP.
- **Installation/health fails:** The bootstrap attempts local binary/unit
  rollback and retains the previously recorded successful commit. Inspect
  `google-startup-scripts.service` and `youtube-extractor.service` logs.
  A broken first install has no prior version to restore.
- **Rollback an accepted but later-broken release:** Publish a reviewed revert
  to `main`, let the workflow generate the corrected release, then boot the
  worker again. For incident recovery, the manual recovery ZIP remains
  available in Actions for 14 days. Do not force a restart with active jobs.
- **Startup script conflicts:** Compute Engine permits a single
  `startup-script-url` metadata value. If another deployment system owns
  that key, consolidate the scripts before enabling this workflow.

Keep existing controller/Cloudflare/worker credentials separate. Follow
[GCP/Cloudflare operations](gcp-cloudflare-operations.md) for the private
network path, VM ownership/idle shutdown, and end-to-end smoke tests.
