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

# Root never opens a path this user controls: the program arrives on stdin,
# opened here before sudo asks for a password, and the unit as an argument.
# Root installs the program only if it still has the digest taken here, so a
# build swapped or rewritten while the prompt is open is refused. The
# snapshot it publishes is readable by this user's group only.
bin_sum=$(sha256sum "$binary_dir/argus-sensors" | cut -d' ' -f1)
unit_text=$(cat packaging/argus-sensors.service)
group=$(id -gn)
# Install a fixed root-owned program/unit; enabling is a separate GUI action.
sudo sh -c '
set -eu
bin_sum=$1 group=$2 unit_text=$3
case $group in
    ""|*[!A-Za-z0-9._-]*) echo "unusable group name: $group" >&2; exit 2 ;;
esac
tmp=$(mktemp -d)
trap "rm -rf \"\$tmp\"" EXIT
cat > "$tmp/argus-sensors"
if [ "$(sha256sum "$tmp/argus-sensors" | cut -d" " -f1)" != "$bin_sum" ]; then
    echo "argus-sensors changed after it was checked; not installed" >&2
    exit 1
fi
printf "%s\n" "$unit_text" > "$tmp/argus-sensors.service"
install -d -m755 /usr/local/libexec /etc/systemd/system/argus-sensors.service.d
install -o root -g root -m755 "$tmp/argus-sensors" /usr/local/libexec/argus-sensors
install -o root -g root -m644 "$tmp/argus-sensors.service" /etc/systemd/system/argus-sensors.service
printf "[Service]\nGroup=%s\n" "$group" > "$tmp/reader.conf"
install -o root -g root -m644 "$tmp/reader.conf" /etc/systemd/system/argus-sensors.service.d/reader.conf
systemctl daemon-reload
' argus-install-sensors "$bin_sum" "$group" "$unit_text" < "$binary_dir/argus-sensors"
echo "Installed argus-sensors; its readings are readable by group $group."
