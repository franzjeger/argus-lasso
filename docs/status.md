# Project status

Reviewed on **2026-09-28**. This working tree includes the reliability, recording
comparison, rule explanation and paired-updater changes listed under
[Unreleased](../CHANGELOG.md). They have not been published or installed by this
review. Package version remains `1.3.1`; IPC protocol remains 5.

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

The earlier paired installation verified at `8ff0f9b` used build
`8ff0f9b1b6e7-1de5f47218b7`. This review did not replace it or restart its service.
The published release last checked on 2026-09-28 was
[v1.3.1](https://github.com/franzjeger/argus-lasso/releases/tag/v1.3.1), which predates
much of the current source. A successful source build is not a published release.

The current updater compares stable release versions, not commits. New signed
archives must include matched app/layer metadata. It preserves a previous pair
for rollback and recovers pending transactions on startup. The privileged sensor
helper remains separate. See [installation](installation.md) and
[updater behavior](design-updates.md).

The earlier [CI run for 8ff0f9b](https://github.com/franzjeger/argus-lasso/actions/runs/36382981168)
is historical evidence, not a CI run for these local changes. CI is configured
for x86_64/aarch64, formatting/lint and Rust 1.92. The new changes were validated
locally on x86_64; neither compilation nor unit tests establish game compatibility.

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
