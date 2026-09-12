#!/usr/bin/env bash
# Build and atomically install a matching daemon/layer pair. Existing mapped
# libraries are immutable; restart games after installation.
set -euo pipefail
cd "$(dirname "$0")/.."
export ARGUS_BUILD_ID
ARGUS_BUILD_ID=$(python3 - <<'PY'
from pathlib import Path
import hashlib,subprocess
h=hashlib.sha256()
files=[Path('Cargo.lock'),Path('Cargo.toml'),Path('build.rs')]
for root in ['src','argus-ipc','argus-layer']:
 files.extend(p for p in Path(root).rglob('*') if p.is_file() and p.suffix in ['.rs','.toml','.vert','.frag','.ttf'])
for p in sorted(set(files)):
 h.update(str(p).encode());h.update(p.read_bytes())
rev=subprocess.check_output(['git','rev-parse','--short=12','HEAD'],text=True).strip()
print(rev+'-'+h.hexdigest()[:12])
PY
)
cargo build --release --locked -p argus-lasso -p argus-layer
target_dir=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import sys,json;print(json.load(sys.stdin)["target_directory"])')
scripts/install-binaries.sh "$target_dir/release" "$ARGUS_BUILD_ID"
