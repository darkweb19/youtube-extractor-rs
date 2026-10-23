# On-demand GCP extraction worker (Ubuntu 24.04)

This directory is the **GCP deployment unit**. It packages the existing Rust/Axum
service as a binary instead of maintaining a fork of its request parsing,
job queue, cancellation, or file streaming code. GCP receives only the generated
`gcp-worker/bundle/` folder; Railway keeps the UI and public API.

## Build and copy only this folder

On a Docker-capable workstation (builds a Linux/amd64 Rust 1.99 binary):

```bash
bash gcp-worker/package.sh
gcloud compute scp --recurse gcp-worker/bundle INSTANCE_NAME:~/extractor-bundle --zone ZONE
```

On your Ubuntu 24.04 VM:

```bash
cd ~/extractor-bundle
bash setup-ubuntu.sh
# WARP registration already exists on your test VM; do not re-register it.
warp-cli mode proxy
warp-cli proxy port 40000
warp-cli connect
warp-cli status
curl --proxy socks5h://127.0.0.1:40000 https://www.cloudflare.com/cdn-cgi/trace | grep warp=
sudo install -m 0600 worker.env.example /etc/youtube-extractor.env
sudoedit /etc/youtube-extractor.env   # SET API_TOKEN to a unique secret first
bash install-service.sh
curl -fsS http://127.0.0.1:8080/health
```

**Security**: worker listens only on localhost. Do not expose port 8080 or
SOCKS port 40000 in VPC firewall rules. Install **cloudflared** and set up a
named Cloudflare Tunnel for `http://127.0.0.1:8080` using a stable hostname;
protect the hostname with Cloudflare Access service-token policy. Use a
Cloudflare-managed tunnel token installed via `sudo cloudflared service install`
and verify it reconnects after VM boot. Never check tunnel tokens into Git.
Railway requires `GCP_WORKER_URL` pointing at that HTTPS hostname plus its
Access client ID/secret and worker bearer token. The VM needs outbound access
for Cloudflare Tunnel and WARP.

A bare TCP port or unprotected tunnel is **not** an acceptable production setup.
Verify `systemctl is-enabled warp-svc youtube-extractor cloudflared`.

## VM startup and shutdown

Railway's `EXTRACTOR_MODE=remote` starts the VM with the Compute Engine API,
waits for RUNNING + `/health`, then submits jobs to the worker's existing
bounded Tokio queue. Queued jobs use UUIDs. Railway transfers outputs to its
own storage before declaring success. Railway stops the VM after 300 seconds
without active operations. If a new request arrives during a stop, it must
wait and restart the same VM (never duplicate instances).

**Configure an independent GCP maximum VM run duration** with termination
action STOP, e.g.:

```bash
gcloud compute instances stop INSTANCE_NAME --zone ZONE
gcloud compute instances set-scheduling INSTANCE_NAME --zone ZONE --max-run-duration=1h --instance-termination-action=STOP
```

This is a hard cost guard; it can interrupt long jobs. Leave the VM stopped
after setup, then submit through Railway. Persist Railway DATA_DIR on a
Railway volume and run a **single Railway replica**. The in-process queue is
not a multi-replica database queue and in-flight downloads are not transparently
resumable after a crash. Job retry policy must be bounded.

## Troubleshooting

```bash
sudo journalctl -u youtube-extractor -n 100 --no-pager
warp-cli status
curl --proxy socks5h://127.0.0.1:40000 https://www.cloudflare.com/cdn-cgi/trace | grep warp=
curl -fsS http://127.0.0.1:8080/health
```

WARP behavior can change, and the network path is not guaranteed to stay usable
for all videos. Download only media you are permitted to retrieve.
