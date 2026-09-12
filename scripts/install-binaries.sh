#!/usr/bin/env bash
# Install a matching prebuilt user application and Vulkan layer.
set -euo pipefail
if [ "$#" -ne 2 ] || [[ ! "$2" =~ ^[A-Za-z0-9_.-]+$ ]]; then
    echo 'Usage: install-binaries.sh BINARY_DIRECTORY BUILD_ID' >&2
    exit 2
fi
binary_dir=$(realpath "$1")
ARGUS_BUILD_ID=$2
cd "$(dirname "$0")/.."
for file in argus-lasso libargus_layer.so; do
    test -f "$binary_dir/$file" || { echo "Missing $binary_dir/$file" >&2; exit 1; }
done
service_pid=$(systemctl --user show argus-lasso -p MainPID --value 2>/dev/null || true)
for pid in $(pgrep -u "$(id -u)" -x argus-lasso || true); do
    if [ "$pid" != "$service_pid" ]; then
        printf 'An unmanaged Argus instance (PID %s) is running. Close it before installation.\n' "$pid" >&2
        exit 1
    fi
done
layer_dir="$HOME/.local/share/argus-lasso/layers/$ARGUS_BUILD_ID"
manifest_dir="$HOME/.local/share/vulkan/implicit_layer.d"
mkdir -p "$layer_dir" "$manifest_dir" "$HOME/.local/bin"
install -m755 "$binary_dir/libargus_layer.so" "$layer_dir/libargus_layer.so.new"
mv "$layer_dir/libargus_layer.so.new" "$layer_dir/libargus_layer.so"
install -m755 "$binary_dir/argus-lasso" "$HOME/.local/bin/argus-lasso.new"
mv "$HOME/.local/bin/argus-lasso.new" "$HOME/.local/bin/argus-lasso"
python3 - "$layer_dir/libargus_layer.so" "$manifest_dir/ArgusOverlay.json" <<'PY'
import sys,json,pathlib
path=pathlib.Path(sys.argv[2]);tmp=path.with_suffix('.json.new')
try:
 global_loading='enable_environment' not in json.loads(path.read_text())['layer']
except (OSError,ValueError,KeyError):
 global_loading=False
tmp.write_text(json.dumps({'file_format_version':'1.0.0','layer':{
 'name':'VK_LAYER_ARGUS_OVERLAY','type':'GLOBAL','library_path':sys.argv[1],
 'library_arch':'64','api_version':'1.3.200','implementation_version':'5',
 'description':'Argus-Lasso telemetry HUD (IPC v5)',
 **({} if global_loading else {'enable_environment':{'ARGUS_LASSO_HUD':'1'}}),
 'disable_environment':{'ARGUS_LASSO_HUD_DISABLE':'1'}}},indent=2)+'\n')
tmp.replace(path)
PY
mkdir -p "$HOME/.local/share/applications" "$HOME/.config/systemd/user"
# Keep local service customizations, including existing launch preferences.
if [ ! -e "$HOME/.config/systemd/user/argus-lasso.service" ]; then
    install -m644 dist/argus-lasso.service "$HOME/.config/systemd/user/argus-lasso.service"
fi
python3 - "$HOME/.local/bin/argus-lasso" "$HOME/.local/share/applications/argus-lasso.desktop" <<'PYAPP'
import pathlib,sys
text=pathlib.Path('dist/argus-lasso.desktop').read_text()
pathlib.Path(sys.argv[2]).write_text(text.replace('Exec=argus-lasso','Exec='+sys.argv[1]))
PYAPP
mkdir -p "$HOME/.local/share/icons/hicolor/256x256/apps" "$HOME/.local/share/icons/hicolor/scalable/apps"
install -m644 assets/icon.png "$HOME/.local/share/icons/hicolor/256x256/apps/argus-lasso.png"
install -m644 assets/icon.svg "$HOME/.local/share/icons/hicolor/scalable/apps/argus-lasso.svg"
python3 - "$HOME/.local/bin/argus-lasso" "$HOME/.local/share/applications/io.github.franzjeger.ArgusLasso.desktop" <<'PYDESKTOP'
import pathlib,sys
text=pathlib.Path('packaging/io.github.franzjeger.ArgusLasso.desktop').read_text()
pathlib.Path(sys.argv[2]).write_text(text.replace('Exec=argus-lasso','Exec='+sys.argv[1]))
PYDESKTOP
if command -v update-desktop-database >/dev/null; then
    update-desktop-database "$HOME/.local/share/applications"
fi
systemctl --user daemon-reload
systemctl --user restart argus-lasso
printf 'Installed build %s; restart games to load this library.\n' "$ARGUS_BUILD_ID"
sha256sum "$HOME/.local/bin/argus-lasso" "$layer_dir/libargus_layer.so"
if command -v readelf >/dev/null; then
    readelf -n "$layer_dir/libargus_layer.so" | sed -n '/Build ID/p'
fi
