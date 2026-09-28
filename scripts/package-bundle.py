#!/usr/bin/env python3
"""Write signed-archive metadata for a workspace-built app/layer pair."""
import hashlib
import json
from pathlib import Path
import subprocess
import sys

root = Path(sys.argv[1]).resolve()
info = json.loads(subprocess.check_output([str(root / 'argus-lasso'), 'build-info'], text=True))
if info['version'] != sys.argv[2]:
    raise SystemExit('Release tag must match the compiled app version')
info.update(schema=1,
            app_sha256=hashlib.sha256((root / 'argus-lasso').read_bytes()).hexdigest(),
            layer_sha256=hashlib.sha256((root / 'libargus_layer.so').read_bytes()).hexdigest())
(root / 'bundle.json').write_text(json.dumps(info, indent=2) + '\n')
