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

### HDR

The HUD is drawn on HDR swapchains: HDR10 (PQ, BT.2020) and scRGB. Its colours
are converted so that HUD white is 203 nits, the usual reference white, however
bright the game's highlights are. Set another level, from 80 to 1000 nits, with:

```text
ARGUS_LASSO_HUD_NITS=300 %command%
```

Proton's Wayland driver (`PROTON_ENABLE_WAYLAND=1 PROTON_ENABLE_HDR=1`) describes
the image to the compositor itself and creates a *pass-through* swapchain that
does not say what it holds. The layer then goes by the format: ten bits per
channel is taken as HDR10, half-float as scRGB, anything else as sRGB. If a game
uses a ten-bit swapchain for SDR, the HUD comes out dim; say what the swapchain
holds with `ARGUS_LASSO_HUD_COLORSPACE=srgb` (or `hdr10`, `scrgb`). The variable
only affects pass-through swapchains. Other colour spaces (Display P3, linear
BT.2020 and so on) get no HUD; the layer's log line says which one it met.

Keep the same daemon, policies, sensors, resolution, graphics settings and scene
between comparisons. Disable unrelated overlays unless they are the consistent
external measurement tool. Repeat and rotate cases; report the measurement window
and variation. Turning off the entire daemon would confound this comparison.

## Games and their HUD state

**Gaming → Overlay → Games** lists every game whose layer is connected, with what
the layer says of its HUD: `HUD on, 3440x1440 HDR10`, or `No HUD:` and the reason,
such as an unsupported colour space or a present from a queue the HUD cannot
draw on. The same lines go to the **Log** page, prefixed `[Overlay]`, where they
can be read after the game has closed. Before this the reason was only written
to the game's stderr.

`(assumed from the image format)` after HDR10 or scRGB means the swapchain was a
pass-through one and the layer went by its format; see [HDR](#hdr).

Each row has the game's own settings, which apply at once and are kept for the
next time it runs:

- **Own frame rate limit** replaces the general limit for this game. 0 means no
  limit for it.
- **Hide HUD** keeps the HUD off in this game while it stays on elsewhere.

A game is known by its process name, as the process table shows it
(`PathOfExileSteam.exe`). Games with settings of their own stay in the list while
not running; a row whose settings are all back to the general ones is forgotten.
In `config.toml` they are `[gaming_mode.games."<name>"]` tables.

## Frame rate limit

**Gaming → Overlay → Limit frame rate** holds every game that loads the layer to
a number of frames per second, from 10 to 1000. It is off by default. A change
applies at once to running games, also from the command line:

```text
argus-lasso fps-limit 120
argus-lasso fps-limit off
```

For one game, tick **Own frame rate limit** on its row under **Games** in the same
section (see [Games and their HUD state](#games-and-their-hud-state)), or set the
cap in its Steam launch options. The launch option wins over both settings, and
`0` exempts the game:

```text
ARGUS_LASSO_HUD=1 ARGUS_LASSO_FPS_LIMIT=60 %command%
```

While a cap is in force the HUD's FPS value carries it, as in `60 [cap 60]`, so
a capped game is not mistaken for a slow one. The cap applies whether or not the
HUD is shown, including with
`ARGUS_LASSO_DRAW=0`, but only where the layer is active: a game it is not
loaded for is not limited.

The layer waits just before each frame is presented. The frames come evenly
spaced, but the game has already read input and rendered by then, so **a game's
own limiter gives lower input latency; prefer it where there is one.** The layer
sleeps most of the wait and spins the last quarter millisecond to land on time,
which costs a little CPU on the presenting thread. A game slower than the cap is
never hurried to catch up.

Limits of this first version:

- Vulkan only, like the HUD: DXVK and VKD3D games are covered, OpenGL ones not.
- It counts the presents that pass through the layer. Whether frames made by
  DLSS or FSR frame generation do has not been measured; with frame generation
  on, compare the HUD's FPS with the game's own counter before relying on it.
- There is no variable-refresh-rate automation; pick a cap a few frames under
  the display's refresh rate yourself.

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
A game that exits mid-recording without tearing down its device leaves the rows
written so far (every 100 ms) and a checkpoint of those counters; once the game is
gone, Argus summarizes the rows and marks them complete only if the checkpoint
shows nothing was lost.

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

Protocol **8** has a checked header and length limit, and a [postcard](https://docs.rs/postcard)
payload (protocol 5 used bincode, which is no longer maintained). Startup logs
identify builds, protocol and connection status. The daemon replays current
configuration when a client connects. IPC uses the user's private runtime socket,
plus a private `~/.local/share/argus-lasso/ipc/overlay-v8.sock` fallback visible
inside the tested Steam runtime. The socket names carry the protocol version, so a
game still running a layer from before an update shows "Telemetry disconnected"
until it is restarted, rather than misreading messages. `ARGUS_LASSO_SOCKET` supplies an exclusive diagnostic override.
Install a matching pair and restart a game to load the newly installed library.
Do not infer the mapped library from the most recently copied filename.

## Testing a layer from the source tree

To load the layer built from the tree in one application, without installing it:

```bash
eval "$(scripts/dev-layer.sh)"   # --debug for a debug build
vkcube
```

The script builds `argus-layer` and writes a manifest for `VK_LAYER_ARGUS_OVERLAY_DEV`
pointing at the library in the directory Cargo builds into (`cargo metadata`, so
`CARGO_TARGET_DIR` is honoured) rather than a guessed `./target`, which can hold an
old build. It disables the installed layer for that environment, so only the
tree's layer loads; its startup line (`[Argus-Layer] build=… protocol=…`) shows
which build that is. The daemon it talks to is the running app, so test against an
app from the same tree.

## Historical regression causes

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

The following validation was recorded during the September overlay work; it is
not a fresh game/driver test of every subsequent commit. See [current build
checks](status.md) for the latest source validation.

Implemented and previously tested locally: transparent compact native Vulkan HUD, per-value
colors and visibility, independent graph refresh, telemetry reconnection and
staleness, matching installation, CSV statistics, sensor helper, and detached
settings windows. Native Vulkan was also run in SteamLinuxRuntime_sniper with
synchronization validation on KDE Wayland/NVIDIA RTX 5090.

A previously recorded native vkcube comparison (same daemon, immediate presentation,
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
