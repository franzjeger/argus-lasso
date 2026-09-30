# Argus-Lasso

[![CI](https://github.com/franzjeger/argus-lasso/actions/workflows/ci.yml/badge.svg)](https://github.com/franzjeger/argus-lasso/actions/workflows/ci.yml)
[![Security audit](https://github.com/franzjeger/argus-lasso/actions/workflows/audit.yml/badge.svg)](https://github.com/franzjeger/argus-lasso/actions/workflows/audit.yml)
[![Release](https://img.shields.io/github/v/release/franzjeger/argus-lasso)](https://github.com/franzjeger/argus-lasso/releases/latest)

A Linux process manager and gaming toolkit, written in Rust with egui. Manage CPU
assignments and process priorities, inspect hardware sensors, customize a Vulkan
HUD, and record game present intervals for performance comparisons.

<picture><source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/2026-09-30/dark/processes.png"><img alt="Argus-Lasso Processes page — CPU as share of total capacity" src="assets/screenshots/2026-09-30/light/processes.png"></picture>

**Screenshots: September 30, 2026**, in the theme matching your GitHub appearance. [Browse every main menu and settings section](docs/screenshots.md).

**This README describes the current source tree.** The latest published release is
[v1.4.0](https://github.com/franzjeger/argus-lasso/releases/tag/v1.4.0)
(checked 2026-09-30): the app, Vulkan HUD layer and sensor helper as one signed,
matched archive. Coming from 1.3.1 or older, [install it by
hand](docs/installation.md#binary-archives) once; later updates work from the app.
The repository was renamed from `process-lasso-linux-rs`; GitHub redirects the
old address.

[Install](docs/installation.md) · [User guide](docs/user-guide.md) ·
[All menus and screenshots](docs/screenshots.md) ·
[Overlay and measurements](docs/overlay.md) · [Sensor access](docs/sensors.md) ·
[Development](CONTRIBUTING.md) · [Project status](docs/status.md) · [Changelog](CHANGELOG.md)

## What it does

| Area | Available now |
|---|---|
| **Overview** | CPU, RAM, disk and network activity, load averages and busy processes. |
| **Processes** | Sort/filter live processes, inspect details, pause/resume, end processes, change CPU affinity, nice and disk I/O priority, export CSV/JSON. |
| **Process rules** | Persistent affinity and priority rules; exact, substring or regex matching; templates, profiles and JSON import/export. |
| **ProBalance** | Act above 85% overall CPU load, select eligible CPU users separately, and restore below 75%. Detected games/exemptions are protected. |
| **Gaming** | CPU topology/parking, Steam and Lutris launchers, profiles, overlay customization, frame recording and optional extended sensors. |
| **Hardware sensors** | Available hwmon, procfs, NVML and powercap readings with session minimum, maximum, average and history. |
| **Memory benchmarks** | Pointer-chase latency and sequential read/write/copy bandwidth tests. Separate from game recording. |
| **Settings** | Appearance, process defaults, CPU power policy, notifications, startup and update checks. |

The app has Breeze and Adwaita light/dark themes, native movable secondary windows,
a system tray, desktop notifications and persistent configuration. Some actions
need system authentication; the GUI and Vulkan layer run as the normal user.

## In-game overlay

- Transparent background by default, 14 actual screen-pixel text, adjustable
  10–24 px, screen corner, offsets, padding and independent text/background opacity.
- Per-reading colors, a component palette, section dividers and individual field
  switches. Configure under **Gaming → Overlay → Customize overlay**; changes
  reach a compatible running layer without a game restart.
- GPU/VRAM and CPU/RAM readings where available, including logical CPU IDs,
  individual load/frequency, optional physical core IDs and configured RAM speed.
- FPS, frametime, average and 1% low; a five-second graph with its own refresh
  cadence, independent of text and sensor updates.
- Application/PID, launcher profile, actual main-thread affinity, nice level,
  parked-thread count and ProBalance intervention.
- Versioned IPC, build diagnostics and explicit disconnected/stale states.
  Missing sensor readings are not shown as measured zeroes.

For one Steam game, set its launch option to:

```text
ARGUS_LASSO_HUD=1 %command%
```

Alternatively enable **Automatically show in detected Vulkan games** in Gaming → Overlay. Changes to
layer loading or its binary take effect on the next game launch. Appearance and
field choices update live. See [overlay setup and limitations](docs/overlay.md).

## Game recording

Use **Gaming → Recording** to start/stop a capture and choose an automatic stop
from 5 to 600 seconds. Set up the desktop-authorized global shortcut there
(suggested **Shift+F2**), or use:

```bash
argus-lasso record --seconds 60
```

Recent recordings show application, local time, duration and completeness. Use
**Compare latest two**, or **Use as A** / **Use as B** inside recordings, to compare
average/1% low FPS, p99 frametime and peak-preserving frametime graphs.

Every accepted Vulkan present interval is recorded; disk I/O happens on a worker.
CSV, metadata and summaries are saved privately under
`~/.local/share/argus-lasso/benchmarks`. Loss or write failures mark a result
incomplete. These are **CPU present intervals**, not GPU execution time or
verified displayed/generated-frame timing. See [metric definitions](docs/overlay.md#recording-metrics).

## Quick start from source

Requires Rust **1.95 or newer**, a Linux graphics stack, Wayland/X11 development
libraries, `pkg-config`, `glslangValidator`, Python 3, and systemd user services
for the supplied installer. Full distro dependencies and manual instructions are
in [installation](docs/installation.md).

```bash
git clone https://github.com/franzjeger/argus-lasso.git
cd argus-lasso
make install
make enable  # optional: start with the desktop session
```

Extended sensor access is optional:

```bash
./scripts/install-sensors.sh
```

Then activate **Gaming → Sensors → Enable extended sensor access**. On the tested
Ryzen 9 9950X3D system this provided package power and configured RAM speed
(8000 MT/s). Hardware, firmware and permissions determine what is available;
root is not a guarantee of sensor support. [Details and data sources](docs/sensors.md).

## Compatibility and validation

Earlier builds of the layer were tested with native Vulkan on KDE Wayland, including a
Vulkan application inside SteamLinuxRuntime_sniper, with NVIDIA RTX 5090 and
Ryzen 9 9950X3D hardware. Synchronization validation and independent CSV statistic
checks were run. Recorded native microbenchmarks compare layer-off, loaded-without-drawing
and full-HUD cases; they are **not** a controlled Path of Exile 2 result.

DXVK (DX9/10/11) and VKD3D-Proton (DX12) are Vulkan paths the layer is intended to
work with, but those individual game paths are not yet verified with this build.
There is no 32-bit layer package or OpenGL/WineD3D overlay implementation.
The GPU/driver test matrix remains limited. [Tested, implemented and pending](docs/overlay.md#validation-status).

## Current app screenshots

Captured on 2026-09-30 from the 1.4.0 source with the read-only X11/Xvfb tour,
in Breeze Dark and Breeze Light; GitHub shows the set that matches your
appearance. The overlay customization window is the real settings window from
its isolated preview. The [complete gallery](docs/screenshots.md) includes all
main pages and the Gaming and Settings subsections.

<table>
<tr><th width="50%">ProBalance: system-load activation</th><th width="50%">Gaming: CPU &amp; performance</th></tr>
<tr><td><picture><source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/2026-09-30/dark/probalance.png"><img alt="ProBalance" src="assets/screenshots/2026-09-30/light/probalance.png"></picture></td><td><picture><source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/2026-09-30/dark/gaming-mode.png"><img alt="Gaming" src="assets/screenshots/2026-09-30/light/gaming-mode.png"></picture></td></tr>
<tr><th width="50%">Overlay customization</th><th width="50%">Game recording</th></tr>
<tr><td><picture><source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/2026-09-30/dark/overlay-settings.png"><img alt="Overlay settings" src="assets/screenshots/2026-09-30/light/overlay-settings.png"></picture></td><td><picture><source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/2026-09-30/dark/gamingrecording.png"><img alt="Recording" src="assets/screenshots/2026-09-30/light/gamingrecording.png"></picture></td></tr>
<tr><th width="50%">Hardware sensors</th><th width="50%">Process rules</th></tr>
<tr><td><picture><source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/2026-09-30/dark/hw-monitor.png"><img alt="Sensors" src="assets/screenshots/2026-09-30/light/hw-monitor.png"></picture></td><td><picture><source media="(prefers-color-scheme: dark)" srcset="assets/screenshots/2026-09-30/dark/rules.png"><img alt="Process rules" src="assets/screenshots/2026-09-30/light/rules.png"></picture></td></tr>
</table>

CPU usage is shown consistently as **0–100% of available capacity**. On 32 online
logical CPUs, one fully busy CPU contributes 3.125% to the total. ProBalance's
85% threshold applies to the entire system, with a separate minimum share for
candidate processes. [Calculation and policy details](docs/user-guide.md#probalance).

## Configuration and CLI

Configuration: `~/.config/argus-lasso/config.toml`. Settings, overlay choices and
appearance take effect and save as they change. Only ProBalance's thresholds, which
have to make sense together, wait for **Apply changes**. Existing configuration
under the old `process-lasso-rs` directory is migrated when appropriate.

```bash
argus-lasso --minimized              # start hidden to tray
argus-lasso --no-tray                # run without the tray
argus-lasso status --top 10          # JSON snapshot
argus-lasso install-helpers          # CPU control helpers; asks for authentication
argus-lasso set-affinity 1234 '0-7'   # Linux logical CPU IDs
argus-lasso kill 1234                # SIGTERM; --force uses SIGKILL
RUST_LOG=debug argus-lasso           # diagnostics
```

The process-table Delete action has an undo countdown; another pending action
cancels the previous one. Closing/restarting resumes a pending target. GUI signals
use stable process handles. The CLI kill command has no undo countdown. File dialogs use `kdialog`, `zenity` or `qarma`; Lutris scanning uses `sqlite3`.

## License

MIT. See [LICENSE](LICENSE). Bundled fonts retain their license in
[assets/fonts/LICENSE](assets/fonts/LICENSE).
