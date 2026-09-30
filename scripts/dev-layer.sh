#!/usr/bin/env bash
# Build the Vulkan layer from this tree and write a manifest that loads it,
# to test it in one application without installing it:
#
#   eval "$(scripts/dev-layer.sh)"
#   vkcube
#
# The manifest points at the library in the target directory Cargo builds
# into (from `cargo metadata`, so CARGO_TARGET_DIR and build.target-dir are
# honoured), never at a guessed ./target that may hold an old build. The
# installed layer is disabled for that environment, so only this one loads.
# Options: --debug for a debug build; MANIFEST_DIR as the one argument to
# choose where the manifest goes.
set -euo pipefail
profile=release
if [ "${1-}" = "--debug" ]; then profile=debug; shift; fi
if [ "$#" -gt 1 ]; then
    echo 'Usage: dev-layer.sh [--debug] [MANIFEST_DIR]' >&2
    exit 2
fi
cd "$(dirname "$0")/.."
manifest_dir=${1:-${XDG_RUNTIME_DIR:-$HOME/.cache}/argus-lasso-dev-layer}

build_args=(build --locked -p argus-layer)
[ "$profile" = release ] && build_args+=(--release)
cargo "${build_args[@]}" >&2
target_dir=$(cargo metadata --no-deps --format-version 1 |
    python3 -c 'import sys,json;print(json.load(sys.stdin)["target_directory"])')
library="$target_dir/$profile/libargus_layer.so"
test -f "$library" || { echo "Missing $library" >&2; exit 1; }

mkdir -p "$manifest_dir"
chmod 700 "$manifest_dir"
python3 - "$library" "$manifest_dir/ArgusDev.json" "$(git rev-parse --short HEAD 2>/dev/null || echo unknown)" <<'PY'
import json, sys, pathlib
library, path, commit = sys.argv[1], pathlib.Path(sys.argv[2]), sys.argv[3]
path.write_text(json.dumps({'file_format_version': '1.0.0', 'layer': {
    'name': 'VK_LAYER_ARGUS_OVERLAY_DEV', 'type': 'GLOBAL', 'library_path': library,
    'library_arch': '64', 'api_version': '1.3.200', 'implementation_version': '1',
    'description': f'Argus-Lasso layer built from this tree ({commit})'}}, indent=2) + '\n')
PY
echo "Dev layer: $library" >&2
printf 'export VK_ADD_LAYER_PATH=%q\n' "$manifest_dir"
echo 'export VK_INSTANCE_LAYERS=VK_LAYER_ARGUS_OVERLAY_DEV'
echo 'export VK_LOADER_LAYERS_DISABLE=VK_LAYER_ARGUS_OVERLAY'
echo 'export ARGUS_LASSO_HUD=1'
