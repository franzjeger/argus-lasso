# Project status

Reviewed on **2026-09-30**. Release
[v1.4.0](https://github.com/franzjeger/argus-lasso/releases/tag/v1.4.0) is
published and is GitHub's latest release: package version `1.4.0`, IPC protocol
6, tagged on
[`c753c5c`](https://github.com/franzjeger/argus-lasso/commit/c753c5c5c7be62c29e2c1043b29e08ddf59d31e1),
the merge of [#103](https://github.com/franzjeger/argus-lasso/pull/103).

## Published archives

The [release run](https://github.com/franzjeger/argus-lasso/actions/runs/36763083800)
built, signed and published both architectures after approval in the `release`
environment, and wrote the notes from the 1.4.0 changelog section. The published
x86_64 and aarch64 archives were downloaded and checked on 2026-09-30:

- Each matches its `.sha256` file, and its `.minisig` signature verifies with
  `minisign-verify` (the library the updater uses) against `dist/argus-lasso.pub`
  (key `D53DAD0590FF1744`). Against the replaced 1.3.1 key the same signatures are
  refused, as the updater of 1.3.1 and older will refuse them.
- `bundle.json` and `BUILD_ID` name build `c753c5c5c7be…` (the tagged commit),
  version 1.4.0 and protocol 6. The x86_64 app and layer match the SHA-256 values
  in `bundle.json`, and the app's `build-info` agrees with it.
- The download URLs in [binary archives](installation.md#binary-archives)
  resolve for the archive, checksum and signature.

The in-app updater has not yet installed a published release on a real
installation; the first such update will be from a build that already carries the
new key.

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
(protocol 6) and runs as the user service. It is the 1.4.0 source apart from
the version number and documentation, reports version 1.3.1, and already
carries the new release key, so its updater can install v1.4.0. Games running
an earlier layer show "Telemetry disconnected" until restarted.

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
