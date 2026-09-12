# Historical overlay investigation — 2026-09-12

Archived chronological evidence. Build IDs, service state and “final” checkpoints
below describe their respective stages, not the current checkout or installation.
Use [the current overlay guide](../overlay.md) and [sensor guide](../sensors.md)
for present behavior. Local diagnostics referenced below are not published.

This is a staged repair. Native Vulkan validation and a synthetic comparison do
not establish PoE2 performance or Proton compatibility. Gameplay tests remain
required before declaring the regression resolved.

## Reproduced failures

The checkout started clean at `e78042406db26d09e6a9dbb8eb8d54a1314971dc`.
`CARGO_TARGET_DIR=/home/frank/.cache/cargo-target` is confirmed.

The installed implicit-layer manifest selected
`~/.local/share/vulkan/explicit_layer.d/libargus_layer.so`, SHA-256
`91734abd948b28a8285ce6ef44ab639c67fb975d04addfe3a71fd380d7b02400`.
The later library was copied to `implicit_layer.d` instead, SHA-256
`beaa0eaa597f69b81c86a25cc1265ce68205070e0e099f2c397fdea7fa9a0980`.
Therefore copying that later build did not change the library selected by the
loader. A screenshot with the actually selected binary reproduced the large
black rectangle, missing names/values, and overlapping Memory/Fan columns.

At inspection the user service was inactive and no Argus process was running.
Its last two starts had exited because another instance held the process lock.
Starting the existing service succeeded with one user-owned process. No system
Argus daemon existed. Historical process state at the time of the user's PoE2
screenshot cannot be reconstructed from this observation alone.

A real packet captured from that daemon decoded with the `e780424` schema.
Decoding the same bytes with the `4e74ca2` layer schema returned `UnexpectedEof`.
The client silently discarded deserialization failures and retained a default
zero-filled telemetry frame. FPS continued updating independently.

The old graphics implementation has a black background default alpha of 176/255
(69%). The daemon only broadcast configuration at startup/change, without
replaying it to new clients. Thus a later-started game could retain the old
opaque default even when the daemon's configuration was transparent.

The old texture update condition uses `frame_times_ms.len() % 10 == 0`, while
capping that deque at 1,000. At that limit it requests a texture refresh on every
present, including uncached font rasterization, pixel composition, sorting frame
samples and texture upload on the presentation thread. The newer glyph cache
and timer were present in source but were not the library selected by the
installed manifest. This is a concrete recurring CPU cost; the share of the
reported PoE2 130 FPS regression still requires controlled gameplay measurement.

Other verified code faults:

- Overlay submissions did not wait for the game's presentation semaphores and
  presentation did not wait for an overlay completion semaphore.
- Texture writes did not synchronize against preceding fragment reads.
- The layer did not initialize loader dispatch data on its command buffers.
  The archived installed library aborted when tested with validation enabled.
- CPU frequency getter searched `Frequencies`; the collector emits
  `CPU Frequencies`.
- CPU power getter searched category `Power`; the RAPL collector emits category
  `CPU`, group `CPU Package Power [RAPL]`.
- `GPU Load`/`Usage` matching is fixed in the checked-in source. That alone could
  not fix the incompatible daemon/layer pair.

## Changes in this stage

- Versioned, size-limited IPC framing (`ARGL`, protocol 2); per-user runtime socket
  and build identity. Decode/disconnect failures are logged.
- Replay of current config and telemetry to late connections. Slow-client I/O
  runs on client threads rather than blocking process/sensor monitoring.
- Missing/disconnected/stale telemetry is visibly labelled. Hardware values are
  suppressed when telemetry is invalid. Optional GPU/CPU sensor values preserve
  measured zero separately from missing readings.
- 14-pixel default font, configurable 10–24; margin, X/Y offset, independent text
  and background alpha, and field visibility controls. Legacy `scale` is read
  for compatibility but no longer multiplies font size.
- Premultiplied text composition and matching Vulkan blending; destination alpha
  is preserved. Scissor confines drawing to actual content. No font rotation or
  mirroring was introduced.
- Four-column display of logical CPU ID, physical core ID, load and MHz. Offline
  CPUs remain identified and display `parked` rather than shifting IDs.
- Font rasterization/composition moved to a worker, requested every 250 ms.
  Only changed textures upload. Every presented frame feeds a separate rolling
  ten-second statistics window, even when the HUD is hidden in settings.
- GPU semaphores sequence game rendering → overlay → presentation. Completion
  semaphores are indexed by swapchain image. Busy command resources cause a
  skipped HUD draw rather than a blocking host fence wait.
- Shader SPIR-V is rebuilt from GLSL by `glslangValidator`, with aligned SPIR-V
  decoding. Swapchain destruction releases its overlay resources.
- Atomic installation selects a content-identified library path and restarts the
  user service. Existing games must restart to map a new library.

## Sources and timing

| Field | Source | Unit / timing |
| --- | --- | --- |
| GPU load, temperature, measured board power, fan | NVIDIA NVML | %, °C, W, %; sensor poll 1 s |
| GPU graphics/memory clocks | NVIDIA NVML clock_info | MHz as reported by NVML, not an inferred effective data rate |
| VRAM used/total | NVIDIA NVML memory_info | bytes / 2^30 = GiB |
| CPU utilization | `/proc/stat` per logical CPU counters | interval utilization; poll 1 s |
| CPU temperature | hwmon/k10temp | °C; poll 1 s |
| CPU frequency | cpufreq `scaling_cur_freq` | kHz / 1000; a sampled reported clock, not an effective-clock measurement |
| CPU IDs | sysfs topology | logical ID, package ID, physical core ID; never infer ID from a filtered frequency list |
| RAM used/total | `/proc/meminfo` | `(MemTotal - MemAvailable)` / 2^20 and MemTotal / 2^20, GiB |
| RAM configured rate | SMBIOS via `dmidecode -t memory` | Configured Memory Speed, MT/s; statically cached |
| CPU package power | powercap RAPL energy counter delta | microjoules / elapsed seconds / 10^6; wrap handled; no TDP substitution |
| FPS | intervals between calls to present | most recent 120 frame intervals |
| Frametime | latest interval between calls to present | milliseconds; CPU-side presentation timing, not GPU render duration |
| AVG | all intervals in rolling 10 s | 1000 / mean interval in ms |
| 1% low | slowest ceil(N/100) intervals in rolling 10 s | 1000 / mean of those intervals, not a mislabeled p99 reciprocal |
| Graph | presentation intervals | last visible pixel-width count of samples; 33.33 ms vertical range |

The existing policy enforcement and UI snapshot periods are preserved. Sensor
polling is separated from the UI snapshot interval. The `ARGUS_LASSO_DRAW=0`
benchmark mode loads the layer and IPC but allocates/draws no overlay resources;
it intentionally collects no Argus frame statistics. External timing must be
used for the all-off comparison.

## Privileged access probe (no helper installed yet)

On this machine, ordinary reads of the package energy counter and DMI table
return permission denied. A bounded privileged read obtained:

- two populated 24 GiB DIMMs, each configured at **8000 MT/s**;
- RAPL package-0 energy counters; a one-second delta measured **64.96 W** during
  this diagnostic session. This is an example live sample, not a constant or TDP.

A privileged helper can make those sources available here. It cannot manufacture
hardware support on other machines. A GUI-controlled authenticated persistent
helper is still pending; the GUI/layer have not been elevated and no broad
sysfs permissions were installed. Current HUD labels correctly explain why
those two values are unavailable to the user process.

## Validation and remaining coverage

`cargo test --workspace`: 85 tests passed in the final repaired build.
Native `vkcube`, Wayland, RTX 5090, 1280×720, 10-bit swapchain format:
1,000-frame candidate validation passed, then 2,000 frames using the installed
manifest and live telemetry passed without Vulkan errors. Screenshots confirm
readable names, all 32 logical CPUs and transparent unused pixels. The test
window was on the secondary 4K display (compositor scale 1.45); the primary
3440×1440 display is scale 1. Overlay coordinates are swapchain pixels and do
not apply an additional DPI multiplier.

Still required:

- PoE2 same-scene three-way comparison and visual inspection at 3440×1440.
- Actual DXVK DX9/10/11 and VKD3D-Proton DX12 launches inside Steam's runtime.
- 32-bit library build, installation and execution; currently only x86_64.
- OpenGL/WineD3D is a separate, unimplemented rendering route.
- Multi-swapchain presents and non-graphics/different-family presentation paths
  currently pass through without a HUD and must not be advertised as tested.
- Manual GUI interaction still needs QA. Legacy config parsing and new field
  persistence pass a round-trip regression test. An isolated IPC fixture changed
  font 14→24, position, margins, text/background opacity, graph, and visibility
  in a single running Vulkan window. Screenshots verified those transitions,
  restoration, `Data foreldet`, and `Telemetri frakoblet`; no Vulkan validation
  errors were reported. This tests live config consumption, not mouse interaction
  with the settings widgets.
- Privileged helper and optional disk/network/swap/process-memory HUD fields.

Reference for per-image presentation semaphore lifetime:
https://docs.vulkan.org/guide/latest/swapchain_semaphore_reuse.html


## Controlled native microbenchmark

Measured build: `e78042406db2-175e4d23bd45`. Three repetitions per case, rotated
case order. Native Wayland `vkcube`, 1280×720, immediate presentation, RTX 5090.
The same user daemon, policy settings and desktop session remained running for
all cases. No builds or other Vulkan tests ran alongside the measurements;
normal desktop background activity remained. A results-parsing process briefly
ran during the tail of the last repetition, so small differences should be
regarded as noise rather than exact overhead guarantees.

MangoHud 0.8.4 was loaded with no visible HUD in **every** case, recording every
frame via its explicit control socket. Autostart while hidden did not log on
this installed MangoHud version; those initial no-data runs were excluded.
After 5 s of warmup, logging was started, followed by 1 s of settling and a 10 s
measurement. The first logging second is excluded from the frame calculations.
Values below are medians of the three repetitions. Frame time is for the entire
test application, not a GPU timestamp measurement of just the overlay.

| Case | Mean frame interval (ms) | Presentation throughput (FPS) | Process CPU, % of one logical CPU |
| --- | ---: | ---: | ---: |
| Argus layer disabled | 0.0515 | 19417.2 | 54.0 |
| Argus loaded, drawing disabled | 0.0513 | 19476.6 | 54.1 |
| New HUD, live telemetry and 32 CPUs | 0.0602 | 16622.4 | 57.0 |
| Archived installed overlay | 3.7829 | 264.3 | 105.4 |

The roughly 3.8 ms interval and saturated CPU thread in the archived version
support the per-present rasterization diagnosis. The new HUD adds about 0.009 ms
to the interval in this very lightweight test. Very high presentation throughput
is not the monitor's displayed refresh rate and must not be extrapolated to
PoE2. GPU utilization falls in the old case because the CPU cannot feed work
quickly; this is not evidence that the old GPU path is more efficient.

Raw per-frame logs, mapped-library evidence and per-process CPU samples are in
`diagnostics/benchmark-v2/`; reproducible commands are in
`scripts/benchmark-overlay.py` and `scripts/summarize-overlay-benchmark.py`.
The subsequent final build `e78042406db2-81ce594b87d5` adds sensor missing-value
handling, a guard against multiple presentation queues sharing one texture,
private socket modes, and config migration tests. The table identifies the
measured build explicitly; final Vulkan validation is recorded separately.

## PoE2 next step

The existing production configuration was read without modification. It selects
Vulkan, 3440×1440 borderless fullscreen, VSync Off, DLSS Performance, dynamic
resolution disabled, and RTX 5090 / AW3425DW. The specific scene from the reported
300–430 FPS baseline has not been supplied, and no gameplay comparison has been
performed. Restart PoE2 after installation to load the new library.

For a controlled gameplay comparison, keep those settings and Argus mode/rules
fixed, warm the same scene, and repeat each condition at least three times:

1. `ARGUS_LASSO_HUD_DISABLE=1 %command%`
2. `ARGUS_LASSO_HUD=1 ARGUS_LASSO_DRAW=0 %command%`
3. `ARGUS_LASSO_HUD=1 ARGUS_LASSO_DRAW=1 %command%`

Use the same external per-frame recorder in every condition. Verify mapped
library paths in `/proc/<game-pid>/maps`, use at least 60 s per sample, and record
renderer, scene, resolution, DLSS/frame generation, VSync/limiter, foreground
state, CPU affinity, Argus mode and GPU power/clock conditions. Exclude loading,
shader compilation and window-focus transitions from the comparison.

## Follow-up: Steam telemetry and Gaming customization (2026-09-12)

The user reported good gameplay FPS after the first repair, but only local frame
statistics were visible. This is qualitative gameplay feedback, not the
controlled three-condition PoE2 comparison described above.

The actual PoE2 Proton log (`~/steam-2694490.log`, 21:17 local time) identified
build `e78042406db2-81ce594b87d5` and repeated `No such file or directory` for
`/run/user/1000/argus-lasso/overlay-v2.sock`. The host daemon was running and
publishing valid telemetry. Running a probe inside the installed
`SteamLinuxRuntime_sniper` reproduced the missing runtime-directory socket:
pressure-vessel does not expose this arbitrary host runtime subdirectory.

The daemon now publishes the same cached telemetry/config stream on both the
runtime socket and a private socket under
`~/.local/share/argus-lasso/ipc/overlay-v3.sock`. The layer tries both paths;
`ARGUS_LASSO_SOCKET` remains an exclusive override for isolated testing.
The home socket directory is mode 0700 and the socket is 0600. This adds no
sensor polling or IPC work to the presentation thread. Protocol version 3
identifies the expanded configuration; existing games must restart.

Gaming now has an **Enable in-game overlay** switch that unlocks
**Customize overlay…**. Its submenu contains individual telemetry visibility,
logical CPU selection, font size, four corner anchors, offsets, margins,
text/background colors and independent opacity controls. Existing config
save/broadcast handling persists changes and sends them to running layers.
Disabled values and empty sections consume no HUD rows; disabling all content
produces a transparent empty image. Disabling the overlay preserves its choices.

Installed daemon and library build: `e78042406db2-0689686a22f3`, protocol 3.
Library SHA-256:
`d7091420b2176d97a99c3b6ea42df0d7af8804011f76bcbe062f3131beb5ce59`.
The manifest points to the matching immutable library directory. One user
service process is running; both endpoints belong to that process.

Verification:

- `cargo test --workspace`: 86 tests passed, including field selection,
  transparent empty content, protocol handling and configuration persistence.
- The normal IPC inspection client ran **inside SteamLinuxRuntime_sniper**,
  selected the private home socket and decoded three real telemetry frames.
  CPU/GPU names, temperatures, loads, frequencies, fan, RAM and VRAM were valid.
- `vkcube` ran **inside the same Steam runtime**, with the installed implicit
  layer and Vulkan synchronization validation, for 2500 frames. Logs confirm
  matching daemon/layer build IDs, protocol 3 and the first valid telemetry.
  No validation errors occurred. There were two loader warnings about duplicate
  NVIDIA implicit layers supplied by the runtime.
- The captured runtime HUD was visually inspected: real sensor values, all 32
  logical CPUs, 14 px text and scene content visible through empty HUD pixels.
  The isolated customization-menu preview was also visually inspected.
- Evidence: `diagnostics/steam-socket-probe.log`, `steam-telemetry-v3.log`,
  `steam-hud-v3.log`, `steam-hud-v3.png`, `overlay-settings-menu.png`,
  `submenu-tests.log`, and `submenu-install.log`.

CPU power and configured RAM speed still correctly report permission denied
under ordinary user access. The privileged sensor helper remains outstanding,
as do optional disk/network/swap/process-memory fields and verification of
32-bit, DXVK and VKD3D paths. This follow-up verifies the actual Steam runtime
transport and native Vulkan HUD; PoE2 itself has not been relaunched by the agent.

## Follow-up: per-value colors and readable sections

The HUD now uses typed metric spans instead of monochrome strings. Each metric
has a stable configuration key and an RGB override, shared by the UI and raster
worker. Unset colors inherit a restrained component palette: green GPU, blue
CPU, violet memory, amber frame statistics and neutral Argus status. Labels
remain independently configurable; global text opacity still applies to all
spans, shadows, graph and dividers. RGB changes never modify coverage or alpha.
The design reference was MangoHud's component color and field configuration:
https://github.com/flightlessmango/MangoHud/blob/master/data/MangoHud.conf

FPS is first, followed by GPU, CPU, CPU threads, RAM and Argus. Optional thin
dividers separate nonempty sections. The default remains 14 physical pixels,
top left and zero background opacity. CPU labels are now `CPU 04`, preserving
the Linux logical CPU ID. Optional physical topology expands this to
`CPU 04 (core 4)` instead of the ambiguous `L04/C04` notation. Physical core IDs
are off by default; all existing per-thread load/frequency choices remain.

Gaming customization has a color picker and reset per metric, a reset for the
recommended palette, and a section-divider switch. Two columns keep the field
controls compact. Color overrides persist in TOML and are broadcast with live
configuration; this expanded schema uses IPC version 4 and versioned sockets.

Verification: 87 workspace tests passed, including TOML color persistence and a
pixel-level check that changing FPS color leaves layout, alpha and the other
frame-statistic row unchanged. The GUI preview was visually inspected. An
isolated IPC fixture captured real host sensor data and sent color, typography,
divider, topology and visibility changes to one running Vulkan window inside
SteamLinuxRuntime_sniper. Screenshots confirm pink GPU temperature alongside
unchanged green GPU readings, independent thread-frequency color and unchanged
transparent background. No Vulkan validation errors were reported throughout
the live transitions. Evidence is in `diagnostics/color-tests.log`,
`color-live-validation.log`, `color-live-phases.log`, `color-live-default.png`,
`color-live-colors.png` and `overlay-settings-menu.png`.

The user's previous default green label color is changed to the new neutral
label color, and the verbose physical-core suffix is switched off. The old
configuration is backed up at `diagnostics/config-before-color-defaults.toml`;
the update script compares parsed configurations to verify that no other
settings change. Custom label RGB values are preserved.

Useful future Argus-specific fields are the identified game/profile, actual
process CPU affinity (particularly useful for the 9950X3D), and active
ProBalance intervention. Current telemetry only reports Gaming/Normal and
parked logical CPU count. Profile and rule settings must not be presented as
verified applied process state without reading that state. These extra fields
are recommendations, not implemented claims.

Final installed build: `e78042406db2-9d535aa28d2d`, protocol 4, library SHA-256
`f4235c7f6bea7d72f74442db0fa4d006a5e93333bc442bad59f7b0d32835da26`.
A final 2500-frame Steam-runtime Vulkan run used the production configuration
and daemon (no fixture). Matching build IDs, the home socket connection and real
telemetry were confirmed, and its screenshot was visually inspected. No Vulkan
validation errors occurred; the runtime's two duplicate NVIDIA layer warnings
remain. See `diagnostics/color-final-runtime.log`, `color-final-runtime.png` and
`color-install-final.log`. The final GUI-only refinement arranges controls in
two columns; it was compiled and visually checked after the 87-test run.

## Stage 5 — graph cadence, recording, sensors and navigation (2026-09-12)

This section supersedes the previous stage's pending recommendations. No prior
agent claim was used as verification. The running game was closed during these
checks; Path of Exile 2 still needs the controlled three-case scene comparison.

### Frame graph and Argus fields

The graph shared the text rasterizer's 250 ms cadence: its visible update rate
was only 4 Hz even though present intervals were sampled every frame. Its old
horizontal window also depended on FPS. The graph now has a separate, reused
pixel strip, uploaded at a configurable 30–120 Hz (default 60, bounded by game
presents). It represents five seconds, retaining the slowest frame in each
1/60-second bucket. Text stays at 4 Hz, sensor reads at 1 Hz, and rolling frame
statistics use all present samples over ten seconds. A 33 ms graph scale is the
default; 5–100 ms is selectable. Transfer barriers cover overlapping text and
graph updates. No extra font rasterization runs on the present thread.

Optional Argus rows now show application/PID, launcher profile, actual main-thread
CPU affinity, process nice level and whether ProBalance is intervening in this
process. Host PID comes from socket peer credentials for Steam PID namespaces.
Launcher profiles are associated with PID plus process start time to reject PID
reuse. A profile name describes the selected launcher profile; it is not proof
that every setting is applied. Main-thread affinity is explicitly labelled and
does not claim every application thread has the same affinity.

### Recording and metric definitions

Gaming → Recording starts/stops a bounded capture, with automatic stop after
5–600 seconds (default 60). The desktop GlobalShortcuts portal requests Shift+F2;
KDE confirms the actual key. Shortcut registration lasts for this app session.
The CLI `argus-lasso record --seconds 60` is available without a second daemon.

The Vulkan hook timestamps each present entry with a monotonic clock. It sends
samples through a bounded nonblocking queue to a file worker. CSV, metadata and
summary files are per process and swapchain, under
`~/.local/share/argus-lasso/benchmarks` (private directory, owner-only files).
Swapchain destruction closes its stream; generation tickets prevent delayed
samples from crossing recording sessions. Recording also works with drawing
disabled. Queue loss, failed presents and file errors invalidate completeness;
partial files and explicit errors prevent a truncated capture appearing valid.
There is no upload.

Average FPS is interval count divided by interval duration. The 1% low is the
reciprocal of the arithmetic mean of the slowest ceil(N/100) intervals. p99 is
the nearest-rank 99th-percentile interval. These are CPU present intervals, not
GPU execution time, displayed-frame timing, or generated-frame count. Metadata
records build, protocol, dimensions, telemetry and overlay configuration. Scene,
warmup, graphics settings and background activity still require user control.

Primary references consulted: [MangoHud](https://github.com/flightlessmango/MangoHud),
[PresentMon's metric distinctions](https://github.com/GameTechDev/PresentMon), and
[XDG GlobalShortcuts](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html).
This is not PresentMon integration or a claim of identical measurement APIs.

KDE portal registration and its Shift+F2 binding succeeded. Invoking the registered
KDE action via D-Bus started a capture through the portal. A physical key press
was not observed. The portal required a registered stable app ID and a matching
desktop entry with an absolute executable path; both are now installed.
A Steam-runtime Vulkan capture yielded 58 and 1143 intervals across two
swapchains. Independent CSV recomputation matched every reported count, AVG,
1% low and p99; no lost samples or failed presents were reported.

### Extended sensors

Actual file permissions on this machine are `0400 root` for both the package
energy counter and firmware DMI table. Other readings come from different sysfs,
procfs or NVML interfaces. Elevated access is needed for these particular files,
not intrinsically for every CPU-power or RAM-speed implementation.

`argus-sensors.service` runs one restricted root reader. It reads package RAPL
energy once per second and computes watts from energy delta / actual elapsed
time, handling counter wrap and rejecting long gaps/resets. It does not use TDP.
SMBIOS type 17 configured RAM speed is read once at startup; mixed or unknown
DIMM speeds are not replaced with a guessed value. On this host, the helper
provided measured CPU package power and **8000 MT/s**, independently checked
against dmidecode (two 24 GiB modules). Root cannot add an unsupported sensor.

The service has no effective capabilities, no new privileges, a read-only system,
protected home, private devices/temp, and only a fixed runtime output directory.
It accepts no arbitrary sensor path, command or privileged child process. Output
contains only the bounded sensor schema. The normal user daemon consumes this
cache; GUI and Vulkan layer remain unprivileged. Samples older than 3.5 seconds
are rejected. Hardware monitor and overlay both use the helper CPU-power reading; configured RAM speed is shown in the overlay and sensor-access card.

Initial installation on another machine: `scripts/install-sensors.sh`. This
installs the fixed root-owned program and unit. Gaming → Sensors then starts or
stops it through systemd and ordinary desktop authentication. It is an opt-in
service, not enabled at boot, and does not duplicate the Argus user daemon.
Interactive authentication is genuinely required: an unprivileged no-ask-password
stop was denied. Sensor access was enabled temporarily for validation.

Sources: [kernel powercap](https://cdn.kernel.org/doc/html/latest/power/powercap/powercap.html)
and [kernel DMI table interface](https://github.com/torvalds/linux/blob/master/Documentation/ABI/testing/sysfs-firmware-dmi-tables).

### Menu and window review

Gaming is divided into CPU & performance, Launcher & profiles, Overlay, Recording
and Sensors. Settings is divided into Appearance, Processes, CPU power,
Notifications and Startup & updates. Hardware sensors, Memory benchmarks and
Activity log have explicit names in Tools, and the active tool remains visible
in navigation. Process menus identify the target once and group termination,
priority/CPU assignment and rule creation. Text distinguishes logical CPU threads
from physical cores and MiB/GiB from decimal units. Existing English is retained.

A stale Settings control previously wrote an obsolete library path into the
Vulkan manifest. Its replacement lives in Gaming → Overlay and changes only
loading conditions, preserving the installed versioned library, architecture and
protocol. The installer preserves global-loading preference too. Settings and
Gaming now merge only their owned configuration fields; an older Settings draft
cannot overwrite a newer HUD edit. Update-check preferences now correctly mark
the Settings form dirty.

`set_embed_viewports(true)` was forcing native dialogs inside the main window.
This is disabled. Overlay customization and process details use native viewports,
as do the existing affinity, priority, rule, game-picker and benchmark windows.
Overlay customization remains open when switching main tabs. The overlay child
was moved to the second monitor while its parent remained on the first, captured
in `diagnostics/native-settings-detached.png`. Confirmation popups remain local.

Twenty page captures were inspected in `diagnostics/menu-review`. The screenshot
tour now uses a read-only collector: no policy application, overlay socket or
configuration saves. It previously launched another real monitor despite its
read-only claim. Screenshot captures of parent viewports do not include native
child-window contents; the separate desktop screenshot verifies those.

### Controlled native performance comparison

Measured feature build: `e78042406db2-e4ebd078b07c`, IPC 5. Native Wayland vkcube,
1280×720, immediate present, five-second warmup, one-second recording settle,
eight-second sample. Same user daemon, root sensor helper, policy settings and
external MangoHud per-frame logger in all cases. Case order rotated. Three valid
runs per case; values below are medians. Desktop background activity was not
fully isolated. This lightweight test is deliberately not a gameplay FPS claim.

| Case | Mean interval | FPS | CPU (% of one core) | GPU load |
|---|---:|---:|---:|---:|
| Argus layer off | 0.049152 ms | 20345 | 53.12% | 63.39% |
| Layer loaded, no drawing | 0.049366 ms | 20257 | 53.37% | 62.75% |
| HUD, 60 Hz graph | 0.056796 ms | 17607 | 56.75% | 67.25% |

Two external logger runs contained an impossible single interval of more than
21 million ms in an eight-second capture. Those entire runs were rejected and
repeated, not silently trimmed. The raw files remain available. Results and
accepted sources are in `diagnostics/benchmark-graph-v5-combined.json`.
The HUD difference is about 0.00764 ms in this workload; it must not be extrapolated
to Path of Exile 2. CPU/GPU percentages are utilization, not isolated overlay
execution time. Later navigation/portal refinements did not change renderer code.

Still unverified: a controlled PoE2 scene comparison; actual DXVK and
VKD3D-Proton game runs with this build; 32-bit layer packaging; OpenGL/WineD3D;
a real launcher-profile transition and ProBalance intervention during capture;
physical shortcut presses; and displayed/generated-frame timing. Optional disk,
network, swap and process-memory HUD fields are not part of this stage.

Additional validation: with the helper paused, cache age reached 4849 ms and the
normal IPC consumer reported CPU power / RAM speed unavailable rather than
replaying the old values. After resuming, measured power recovered. A capture
with `ARGUS_LASSO_DRAW=0` contained 1198 intervals; independently recomputed AVG,
1% low and p99 matched its summary exactly, with no loss or failed presents.
The workspace suite passed **93 tests** (82 app, 6 layer, 3 IPC, 2 sensor helper).

Final installed build: **`e78042406db2-3f81c91ba10e`**, protocol **5**.
Daemon SHA-256: `e94b598bc7da48756e215978f26057cdc5fbd92349dfe4481245dd5f2e7caac0`.
Layer SHA-256: `631a77ac6b78e5061989ad4d4bc54711cbf679b2d51a9aeaf293b62f0c4aca43`.
ELF build ID: `c12d3cff2ed1b0a751ee9690374ba4ba8b907960`.
The final versioned library is under
`~/.local/share/argus-lasso/layers/e78042406db2-3f81c91ba10e/`.

A final 3600-present Vulkan run in SteamLinuxRuntime_sniper exited successfully,
loaded this exact library, connected to a matching daemon build over the home
socket, and displayed current telemetry, transparent compact text and graph.
The screenshot was visually inspected; synchronization validation reported no
errors (`diagnostics/final-v5-steam-runtime.log` and `.png`). One production
Argus user process remains. Games must restart to load the new library.

The temporary root sensor service was stopped after validation and is disabled
at boot. It can be enabled under Gaming → Sensors with system authentication.
The ordinary daemon continues running; extended fields become unavailable while
this option is off. Final availability snapshots are in
`diagnostics/final-v5-ipc.log` and `diagnostics/final-v5-sensors-off.log`.

## Stage 6 — spacing, typography and visual hierarchy (2026-09-12)

The GUI uses a shared 15-point body font, 13.5-point help text, 16-point section
headings and 22-point page titles. These are logical GUI points with normal
platform scaling, independent of the HUD's actual pixel setting. Help text has
explicit light/dark contrast. Buttons have 30-point height and subsection tabs
34-point hit targets. Tables use 28-point standard / 24-point dense rows.

Flexible cards have 16-point inner padding, quieter borders, and a subtle rule
between their title/help and controls. Section gaps are 18 points. Fixed-size
CPU charts keep their geometry contract. Form labels share a left edge and wrap;
narrow forms stack labels above controls. Page titles and short descriptions
make navigation distinct from the selected page. Secondary navigation has space
around every label and wraps if needed.

Recording controls, shortcut setup and results have separate groups; calculation
details are expandable. Overlay customization sections have spacing and rules.
Light-theme slider tracks and their value fill remain visible against white
cards. Long Gaming topology descriptions wrap beside the action button. CPU-map
legends have their own row. At narrow widths, Tools keeps a short label and the
status bar puts the full CPU model in a tooltip, avoiding overlapping text.

Validation: release build and workspace compile check; `git diff --check` clean.
All 20 main-page captures at 1400 × 900 were inspected, plus narrow-page checks
at approximately 820 × 732 and light/dark component previews at 800 × 650.
An observed narrow-navigation overlap and centered form labels were corrected
and recaptured. Evidence: `diagnostics/polish-review`, `polish-narrow-review`,
`polish-components-light.png`, `polish-components-dark.png`. This is a GUI-only
change; no new unit tests or gameplay performance claims were added. Existing
HUD rendering, telemetry protocol and user overlay sizing are unchanged.

Installed GUI-polish build: `e78042406db2-5557b3d0eea6` (IPC 5).
Daemon SHA-256: `8e34516fc7a98c7fdd73c0b8f185179550dcf395e1a0a16719895cb637d6bd47`.
Layer ELF build ID: `57306f4e6bee96c12c6fd657d9121d4e5a538d60`.
The user service restarted successfully; a single Argus process and live IPC
were checked. Running games can retain their compatible IPC-5 layer for these
GUI-only changes; the newly installed library loads on their next launch.
