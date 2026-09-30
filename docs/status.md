# Project status

Reviewed on **2026-09-30** for release **1.4.0**: package version `1.4.0`, IPC
protocol 6. The version and changelog section are prepared in the release pull
request; the tag and the published archives are recorded here once the release
workflow has run and they have been checked on GitHub. Until then the latest
published release is v1.3.1.

## Release candidate verification

Run on 2026-09-30 on the release branch (version 1.4.0, source otherwise equal
to `master` at
[`a07aa0b`](https://github.com/franzjeger/argus-lasso/commit/a07aa0b5919de8a1ff6f94b71d6e8574ec0779f3)):

- `cargo fmt --all --check` and workspace Clippy with `-D warnings` passed.
- Workspace tests passed: **289 tests**. The two opt-in tests passed separately
  on this host: the systemd test restored an isolated transient unit's existing
  quota and weight after throttling and removed the unit, and NVML read the
  NVIDIA GPU.
- `cargo build --release --workspace --locked` passed. The release job's
  packaging step, run on that build, wrote bundle metadata matching the app's
  `build-info` and refused a tag that did not match the version. The archive
  installed into an empty home directory with its own `install-binaries.sh`
  (service calls stubbed), and the updater's member patterns each matched exactly
  one app, layer and `bundle.json` in it. Signing was not run locally; the
  workflow checks each signature against `dist/argus-lasso.pub` before publishing.
- The read-only X11/Xvfb tour (`LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=softpipe`)
  completed all 20 screens; Settings → Startup & updates shows v1.4.0.

## Installation and releases

`master` at `a07aa0b` is installed locally as build `a07aa0b5919d-d4efb49d238b`
(protocol 6) and runs as the user service. It reports version 1.3.1, the
version before this release. Games running an earlier layer show "Telemetry
disconnected" until restarted.

Releases from 1.4.0 on are signed with a new key (ID `D53DAD0590FF1744`). The
updater of 1.3.1 and older checks against the replaced key and refuses them, so
those installations move to 1.4.0 by hand once; see
[binary archives](installation.md#binary-archives). From 1.4.0 the updater
compares stable release versions, not commits, installs matched app/layer
bundles, keeps the previous pair for rollback and recovers pending transactions
on startup. The privileged sensor helper remains separate. See
[installation](installation.md) and [updater behavior](design-updates.md).
Neither compilation nor unit tests establish game compatibility.

## Behavior and remaining limits

The app provides process management, persistent rules, system-pressure-based
ProBalance, game profiles, sensors, a Vulkan HUD and CPU present-interval
recording. The [user guide](user-guide.md) describes controls and defaults.

Configuration writes have one owner; failed saves remain visible with a retry.
GUI termination uses stable process handles and cleans up pending Undo actions.
The optional cgroup backend preserves readable, exactly representable existing
quotas; its default remains `nice`. Broader distribution, shared-unit and load
behavior still needs validation. See [cgroup details](design-cgroup-probalance.md).

Recording comparisons use full-capture summary statistics and peak-per-bin graph
reduction. They cannot establish causal FPS improvements from unmatched scenes,
incomplete recordings or different settings. No performance gain is claimed for
these changes.

Earlier native Vulkan/Steam-runtime tests are documented in the
[overlay guide](overlay.md#validation-status). They are not fresh tests of this
build. Controlled same-scene game comparisons, DXVK/VKD3D validation and wider
GPU/driver testing remain pending. There is no 32-bit layer package or
OpenGL/WineD3D implementation. Native child-window and Wayland compositor behavior
are not validated by Xvfb framebuffer captures.
