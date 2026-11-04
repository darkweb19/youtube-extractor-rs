#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
sudo id extractor >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin extractor
if ! sudo -u extractor /usr/local/bin/deno --version >/dev/null 2>&1; then
  echo "Deno must be installed at /usr/local/bin/deno and executable by extractor." >&2
  exit 2
fi
sudo install -d -m 0700 -o extractor -g extractor /var/lib/youtube-extractor
# Replace the binary atomically: overwriting the inode of a running
# executable can fail with ETXTBSY and leave an old worker deployed.
sudo install -m 0755 youtube-extractor /usr/local/bin/youtube-extractor.next
sudo mv -f /usr/local/bin/youtube-extractor.next /usr/local/bin/youtube-extractor
sudo install -m 0644 youtube-extractor.service /etc/systemd/system/youtube-extractor.service
if [ ! -f /etc/youtube-extractor.env ]; then
  sudo install -m 0600 -o root -g root worker.env.example /etc/youtube-extractor.env
  echo "Edit /etc/youtube-extractor.env and set API_TOKEN before starting." >&2
  exit 2
fi
if sudo grep -Eq '^API_TOKEN=(REPLACE_|$)' /etc/youtube-extractor.env; then
  echo "Configure a unique API_TOKEN in /etc/youtube-extractor.env before installing." >&2
  exit 2
fi
sudo systemctl daemon-reload
sudo systemctl enable youtube-extractor
sudo systemctl restart youtube-extractor
sudo systemctl status youtube-extractor --no-pager
