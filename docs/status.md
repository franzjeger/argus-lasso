# Project status

Reviewed on **2026-09-28**. The reliability, recording comparison, rule explanation
and paired-updater changes are committed in
[`75a44ea`](https://github.com/franzjeger/argus-lasso/commit/75a44ea2ea47e227ea5244b375f067ec1c47e894)
and pushed to `master`. The matching app/layer pair is installed locally as build
`75a44ea2ea47-863b562f2519`. These are source changes, not a new tagged release;
package version remains `1.3.1` and IPC protocol remains 5.

## Current source verification

- `cargo fmt --all --check` and workspace Clippy with `-D warnings` passed.
- Workspace tests passed: **175 tests**, including shared sensor tests in two
  binaries. The systemd integration test is excluded from ordinary test runs.
- The opt-in systemd test passed separately on this host: an isolated transient
  unit's existing 37% CPU quota and weight 123 were restored after throttling;
  a subsequent weight-only intervention left its quota intact. The test removed
  its own unit and did not modify the app service or game units.
- `cargo build --release --workspace --locked` passed.
- The read-only X11/Xvfb tour completed all 20 screens. Changed process, recording,
  rules and update screens were reviewed. The comparison preview uses explicitly
  synthetic fixtures, not measured game-performance evidence. See the
  [updated screenshots](screenshots.md#reliability-and-comparison-update).
- Updater tests cover mismatched bundle hashes, failed staged verification,
  matched installation, preserved loading preferences, rollback and recovery of
  an interrupted transaction, all in temporary directories. Release metadata was
  generated from the locally built app/layer pair. No production update or
  rollback was performed.

## Installation and releases

The paired installer replaced the earlier `8ff0f9b1b6e7-1de5f47218b7` installation
with build `75a44ea2ea47-863b562f2519` and restarted the user service. The service
was active/running with no restarts; its mapped executable matched the installed
and freshly built app by SHA-256. The overlay manifest points to the new versioned
library, whose SHA-256 matches the built layer. `build-info` and the startup IPC
log confirm protocol 5 and the same build ID. A private backup of the previous
app, manifest, configuration and service file was retained before installation.
Existing games must restart to load the new library.
The published release last checked on 2026-09-28 was
[v1.3.1](https://github.com/franzjeger/argus-lasso/releases/tag/v1.3.1), which predates
much of the current source. A successful source build is not a published release.

The current updater compares stable release versions, not commits. New signed
archives must include matched app/layer metadata. It preserves a previous pair
for rollback and recovers pending transactions on startup. The privileged sensor
helper remains separate. See [installation](installation.md) and
[updater behavior](design-updates.md).

The [CI run for 75a44ea](https://github.com/franzjeger/argus-lasso/actions/runs/36389275962)
passed x86_64 and aarch64 release builds/tests, Clippy/formatting, the Rust 1.92
minimum-version check and its security audit job. The separate
[security audit](https://github.com/franzjeger/argus-lasso/actions/runs/36389275973)
also passed. Neither compilation nor unit tests establish game compatibility.

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
