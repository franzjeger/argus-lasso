# Project status

Reviewed on **2026-09-28** against source commit
[`8ff0f9b`](https://github.com/franzjeger/argus-lasso/commit/8ff0f9b).
This is a dated verification record. Later source changes require their own
checks; the [changelog](../CHANGELOG.md) separates unreleased work from releases.

## Source, release and installation

| Item | Verified state |
|---|---|
| Current development code | `master`, including sensor improvements in `8ff0f9b` |
| Cargo package version | `1.3.1`; source commits after the release retain this version |
| Latest published release | [v1.3.1](https://github.com/franzjeger/argus-lasso/releases/tag/v1.3.1), published 2026-08-23; checked through GitHub on 2026-09-28 |
| Local paired installation | Build `8ff0f9b1b6e7-1de5f47218b7`; app service restarted and running executable verified |
| Installed overlay | Manifest points to the matching versioned library; existing games need restart to load it |
| Overlay protocol | 5 |
| Platforms | Linux application; CI configured for x86_64 and aarch64 |

The installed build is a development build, not a new published release. The
published v1.3.1 predates the current overlay, recording and navigation work.
The in-app updater compares package/release versions and does not track commits
or update the Vulkan layer. See [installation](installation.md) and
[updater behavior](design-updates.md).

## Verified for this source

- `cargo fmt --all --check` passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` passed.
- `cargo test --workspace --locked` passed: 161 tests across app, IPC, layer and
  sensor-helper test binaries (including shared sensor tests in two binaries).
- `cargo build --release --workspace --locked` passed.
- `make install` built and installed the matched app/layer pair. The running
  executable matched the installed binary, and startup logs reported its build
  ID and protocol 5.
- The read-only screenshot tour completed all 20 steps under X11/Xvfb with
  software rendering. The 17 main-page captures were visually reviewed and
  published in the [gallery](screenshots.md). Root-window captures do not verify
  native child windows or Wayland compositor effects.

The [GitHub CI run for this source commit](https://github.com/franzjeger/argus-lasso/actions/runs/36382981168)
also completed successfully. The [workflow](../.github/workflows/ci.yml) covers
x86_64/aarch64 builds and tests, lint/formatting and a Rust 1.92 check. Neither
local nor CI compilation establishes game/driver compatibility.

## Current behavior and limits

The app provides process management, persistent rules, system-pressure-based
ProBalance, game launch profiles, sensors, a Vulkan HUD and CPU present-interval
recording. The [user guide](user-guide.md) describes controls and defaults.

The latest sensor change reduces redundant cache reads and limits failed NVML
initialization attempts to once per minute. Hardware history and alerts still
collect data when HUD fields are hidden. Missing/stale readings are not replaced
with measured zeroes. No FPS improvement or percentage reduction in CPU use has
been measured for this change. See [sensor sources and validation](sensors.md).

ProBalance defaults to `nice`. Its optional cgroup backend changes entire units,
skips protected units and does not preserve an existing CPUQuota on restoration.
See [backend behavior and remaining validation](design-cgroup-probalance.md).

Earlier native Vulkan/Steam-runtime tests and synthetic timings are documented
in the [overlay guide](overlay.md#validation-status), with staged evidence in the
[historical investigation](archive/overlay-investigation-2026-09-12.md). They are
not fresh tests of this build. Controlled same-scene game comparisons, actual
DXVK/VKD3D game-path validation and wider GPU/driver testing remain pending.
There is no 32-bit layer package or OpenGL/WineD3D overlay implementation.

## Documentation scope

Current guides describe the source tree. Screenshots carry their capture dates;
the native customization and light-theme examples remain dated September 13 and
19 respectively. The changelog's released sections and `docs/archive/` preserve
history, not the current installation. `dist/PKGBUILD` is explicitly a historical
v1.3.0 desktop-only template and does not package the current overlay.
