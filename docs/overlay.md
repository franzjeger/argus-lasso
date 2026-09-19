# Vulkan overlay and frame recording

## Enable and customize

Install the paired app/layer with `make install`. Start Argus, enable the display
under **Gaming → Overlay**, then use **Customize overlay**. By default, load the
layer for selected games with this Steam launch option:

```text
ARGUS_LASSO_HUD=1 %command%
```

**Automatically show in detected Vulkan games** changes the installed manifest's
loading conditions while preserving the versioned library path. The layer then
enables its HUD and recording worker only for detected Steam/Proton games (Steam
game IDs or an executable under `steamapps/common`), or applications explicitly
launched with `ARGUS_LASSO_HUD=1`. Other Vulkan applications are passed through
without HUD resources or telemetry connections. Known terminals, launchers and
desktop compositors are excluded even if they inherit game launch variables.
Detection is heuristic; standalone and other launcher games may need the explicit
launch option above. Restart applications after changing loading conditions or
installing an updated layer, including terminals that already show an overlay.
To force the layer off for a comparison:

```text
ARGUS_LASSO_HUD_DISABLE=1 %command%
```

For a loaded layer without any HUD drawing:

```text
ARGUS_LASSO_HUD=1 ARGUS_LASSO_DRAW=0 %command%
```

Keep the same daemon, policies, sensors, resolution, graphics settings and scene
between comparisons. Disable unrelated overlays unless they are the consistent
external measurement tool. Repeat and rotate cases; report the measurement window
and variation. Turning off the entire daemon would confound this comparison.

## Rendering and update rates

Run `argus-lasso toggle-overlay` to queue a visibility toggle, for example from
a desktop keyboard shortcut. Each invocation queues a separate request; two
toggles cancel each other. The running monitor consumes requests on its next
tick and saves the resulting visibility. If Argus is stopped, requests wait
until it starts. A successful CLI exit confirms queuing, not application.

The background defaults to **0% opacity**. Empty texture pixels have zero alpha;
only text, optional dividers and enabled background pixels affect the image.
Text opacity and optional background opacity are independent. Text defaults to
14 actual framebuffer pixels, adjustable from 10–24, without GUI DPI scaling.
Names occupy heading rows; values and units have reserved columns. Logical CPUs
use explicit thread/core labels and compact selectable columns.

Sensors update at about 1 Hz, independently of the process-table refresh setting.
A worker caches glyphs and composes text roughly
every 250 ms. Frame intervals are collected on **every present**, retained in a
rolling 10-second statistics window. The graph has its own default 60 Hz refresh
(30–120 adjustable) and five-second history: each 1/60-second bucket preserves
the maximum interval so aggregation does not erase a spike. Graph history
resolution and repaint frequency are distinct.

The present path records a graphics pass with alpha blending. Resources are
allocated per swapchain, not recreated per frame; text rasterization and capture
file writing happen off the presentation thread. Texture uploads follow content
changes. GPU submission and synchronization still have a cost; this is not a
zero-overhead overlay.

## Recording metrics

Start/stop in Gaming → Recording, use the desktop portal shortcut, or run:

```bash
argus-lasso record --seconds 60
```

Recording toggles a shared request. Loaded application/swapchain streams write
separate bounded captures with CSV intervals, metadata and summaries under the
Argus data directory (`~/.local/share/argus-lasso/benchmarks` by default).
Queue overflow, present failures or output errors mark a capture incomplete.

| Metric | Definition |
|---|---|
| Frame interval | CPU time between consecutive present calls, milliseconds |
| AVG FPS | Sample count / sum of intervals in seconds |
| 1% low | Reciprocal of the mean of the slowest `ceil(N × 0.01)` intervals |
| p99 interval | Nearest-rank 99th percentile of intervals, milliseconds |
| HUD statistics | Rolling 10-second window |
| Recording statistics | All accepted intervals in that capture, with loss/error metadata |

These are not GPU execution durations, input-to-photon latency, verified display
presentation timestamps or a count of generated frames. Independently recomputed
CSV summaries were checked; equality with every other tool's differently defined
“1% low” is not claimed.

## Telemetry troubleshooting

FPS can continue when sensor telemetry is disconnected because timing is local
to the layer. The HUD reports disconnected/stale telemetry instead of displaying
zero-filled defaults. Check the service and actual library mapping:

```bash
systemctl --user status argus-lasso.service
journalctl --user -u argus-lasso.service -b
cat ~/.local/share/vulkan/implicit_layer.d/ArgusOverlay.json
# Replace GAME_PID with the real game process:
rg libargus_layer /proc/GAME_PID/maps
```

Protocol **5** has a checked header and length limit. Startup logs identify builds,
protocol and connection status. The daemon replays current configuration when a
client connects. IPC uses the user's private runtime socket, plus a private
`~/.local/share/argus-lasso/ipc/overlay-v5.sock` fallback visible inside the tested
Steam runtime. `ARGUS_LASSO_SOCKET` supplies an exclusive diagnostic override.
Install a matching pair and restart a game to load the newly installed library.
Do not infer the mapped library from the most recently copied filename.

## Regression causes

The original installed manifest selected an older library in a different
directory from the newly copied build. That older implementation defaulted to a
69%-opaque black background, and late clients could miss the saved transparent
configuration. Schema mismatch silently discarded telemetry packets and left
zero defaults while local FPS continued.

An old texture-refresh condition used `frame_times.len() % 10 == 0`. Once its
bounded deque reached 1000, the condition stayed true on every frame, causing
repeated rasterization, composition, statistics sorting and texture upload on the
present thread. The source's newer cache/timer was not the selected installed
library. The current installation, protocol, worker and cadence changes address
these concrete faults; they do not alone prove a particular game's FPS recovery.

## Validation status

Implemented and tested locally: transparent compact native Vulkan HUD, per-value
colors and visibility, independent graph refresh, telemetry reconnection and
staleness, matching installation, CSV statistics, sensor helper, and detached
settings windows. Native Vulkan was also run in SteamLinuxRuntime_sniper with
synchronization validation on KDE Wayland/NVIDIA RTX 5090.

A repeated native vkcube comparison (same daemon, immediate presentation,
1280×720, three valid runs per case) measured median intervals of **0.049152 ms**
(layer off), **0.049366 ms** (loaded, no draw), and **0.056796 ms** (HUD + 60 Hz
graph). The HUD difference in that synthetic workload was about 0.00764 ms.
Desktop background activity was not fully isolated. These results do not predict
Path of Exile 2 performance. The user reported improved gameplay FPS, but a
controlled same-scene PoE2 comparison remains pending.

| Route / feature | Status |
|---|---|
| Native 64-bit Vulkan / tested Steam runtime | Tested on the development host |
| DX9/10/11 → DXVK | Intended Vulkan route; actual game-path validation pending |
| DX12 → VKD3D-Proton | Intended Vulkan route; actual game-path validation pending |
| 32-bit games | No 32-bit layer package or validation |
| OpenGL / WineD3D | No overlay implementation |
| GlobalShortcuts portal | Registration/activation callback tested; physical keypress not verified |
| Real launcher-profile/ProBalance transition during game capture | Pending |
| Other GPU vendors / drivers | Limited validation; do not infer universal support |

[Historical investigation](archive/overlay-investigation-2026-09-12.md) records
staged evidence and build IDs. Its local `diagnostics/` references are developer
artifacts, not files shipped in this repository.
