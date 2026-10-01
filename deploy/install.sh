#!/bin/bash
# Administrative bootstrap; deliberately does not stop the running service.
set -euo pipefail
root=/home/godblf/issue-watch
source_dir=$(cd -- "$(dirname -- "$0")" && pwd)
test -f "$root/config/config.toml"
test -f "$root/.env.local"
test -f "$root/data/issue-watch.sqlite3"
sudo apt-get update
sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y docker.io docker-compose-v2 python3 git curl
sudo systemctl enable --now docker
sudo install -d -m 755 /usr/local/lib/issue-watch
sudo install -m 644 "$source_dir/deploy.py" /usr/local/lib/issue-watch/deploy.py
sudo install -m 755 "$source_dir/ssh-entry" /usr/local/lib/issue-watch/ssh-entry
sudo install -d -m 700 "$root/deploy"
sudo install -m 600 "$source_dir/compose.yml" "$root/deploy/compose.yml"
sudo docker compose version
printf '%s\n' 'Container environment ready; existing service has not been stopped.'
