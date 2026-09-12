#!/usr/bin/env bash
set -euo pipefail
binary_dir=""
if [ "$#" -gt 1 ]; then
    echo 'Usage: install-sensors.sh [BINARY_DIRECTORY]' >&2; exit 2
fi
if [ "$#" -eq 1 ]; then binary_dir=$(realpath "$1"); fi
cd "$(dirname "$0")/.."
if [ -z "$binary_dir" ]; then
    cargo build --release --locked --bin argus-sensors
    target_dir=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import sys,json;print(json.load(sys.stdin)["target_directory"])')
    binary_dir="$target_dir/release"
fi
test -f "$binary_dir/argus-sensors"
# Install a fixed root-owned program/unit; enabling is a separate GUI action.
sudo install -d -m755 /usr/local/libexec
sudo install -o root -g root -m755 "$binary_dir/argus-sensors" /usr/local/libexec/argus-sensors
sudo install -o root -g root -m644 packaging/argus-sensors.service /etc/systemd/system/argus-sensors.service
sudo systemctl daemon-reload
