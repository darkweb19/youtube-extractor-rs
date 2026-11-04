#!/usr/bin/env bash
set -euo pipefail
[ "$(. /etc/os-release; echo "$VERSION_ID")" = "24.04" ] || { echo "Ubuntu 24.04 required" >&2; exit 1; }
sudo apt-get update
sudo apt-get -o DPkg::Lock::Timeout=300 install -y curl ca-certificates gpg python3 python3-venv ffmpeg
# A Deno binary under the SSH user's home is inaccessible to systemd's
# extractor account (ProtectHome=true). Install it into /usr/local/bin.
if [ ! -x /usr/local/bin/deno ]; then
  if command -v deno >/dev/null; then
    sudo install -m 0755 "$(command -v deno)" /usr/local/bin/deno
  else
    deno_dir="$(mktemp -d)"
    curl -fsSL https://deno.land/install.sh -o "$deno_dir/install.sh"
    DENO_INSTALL="$deno_dir" sh "$deno_dir/install.sh"
    sudo install -m 0755 "$deno_dir/bin/deno" /usr/local/bin/deno
    rm -rf "$deno_dir"
  fi
fi
/usr/local/bin/deno --version
sudo python3 -m venv /opt/yt-dlp
sudo /opt/yt-dlp/bin/pip install --no-cache-dir 'yt-dlp[default]==2026.10.21'
sudo ln -sfn /opt/yt-dlp/bin/yt-dlp /usr/local/bin/yt-dlp
if ! command -v warp-cli >/dev/null; then
  curl -fsSL https://pkg.cloudflareclient.com/pubkey.gpg | sudo gpg --yes --dearmor -o /usr/share/keyrings/cloudflare-warp-archive-keyring.gpg
  echo "deb [signed-by=/usr/share/keyrings/cloudflare-warp-archive-keyring.gpg] https://pkg.cloudflareclient.com/ noble main" | sudo tee /etc/apt/sources.list.d/cloudflare-client.list >/dev/null
  sudo apt-get update
  sudo apt-get -o DPkg::Lock::Timeout=300 install -y cloudflare-warp
fi
sudo systemctl enable --now warp-svc
# Registration and ToS acceptance remain an explicit operator action.
echo "Run warp-cli registration new (if needed), warp-cli mode proxy, warp-cli proxy port 40000, warp-cli connect."
