#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
sudo id extractor >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin extractor
sudo install -d -m 0700 -o extractor -g extractor /var/lib/youtube-extractor
sudo install -m 0755 youtube-extractor /usr/local/bin/youtube-extractor
sudo install -m 0644 youtube-extractor.service /etc/systemd/system/youtube-extractor.service
if [ ! -f /etc/youtube-extractor.env ]; then
  sudo install -m 0600 -o root -g root worker.env.example /etc/youtube-extractor.env
  echo "Edit /etc/youtube-extractor.env and set API_TOKEN before starting." >&2
  exit 2
fi
sudo systemctl daemon-reload
sudo systemctl enable --now youtube-extractor
sudo systemctl status youtube-extractor --no-pager
