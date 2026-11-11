# Project status

Reviewed on **2026-11-11**. Version **1.6.0** is the current source: package
version `1.6.0`, IPC protocol 6. Its release is tagged `v1.6.0`; the archives are
published once the release run has been approved in the `release` environment.
[v1.5.1](https://github.com/franzjeger/argus-lasso/releases/tag/v1.5.1) is the
previous release.

## Release candidate verification (1.6.0)

Run on 2026-11-11 on the release branch:

- `cargo fmt --all --check` and workspace Clippy (`--workspace --all-targets --locked -- -D warnings`) passed.
- Workspace tests passed: **338 tests** (`292` in `argus-lasso`, `22` in `argus-layer`, `19` in `argus-ipc`, `5` in `argus-sensors`).
- The read-only X11/Xvfb tour completed all 20 screens. The Process rules,
  Gaming Mode and Settings screens, whose code was restructured, are
  pixel-identical to the tour of the preceding commit outside fields that show
  live values; a control run of the same binary twice differs in the same fields.
- `gaming`, `probalance` and `profile` were run against an isolated instance
  (its own home, runtime directory, session bus and Xvfb display): refused with
  exit status 1 when none was running; `probalance off|on` and a profile load
  were applied, logged and saved within two seconds; an unknown profile was
  refused with the list of saved ones. `gaming on` was not run live, because it
  parks CPUs on the host; it sends the command the tray's checkbox sends.
- Idle cost, window closed to the tray, default configuration, 631 processes,
  both builds side by side for 240 seconds on the same machine: 1.5.1 used
  0.40% of one core, 1.6.0 used 0.31%. What remains is mostly the once-a-second
  process scan ProBalance needs.

## Release verification (1.5.1)

Run on 2026-11-09:

- `cargo fmt --all --check` and workspace Clippy (`cargo clippy --release --workspace --all-targets --locked -- -D warnings`) passed with zero warnings.
- Workspace tests (`cargo test --release --workspace --locked`) passed: **310 tests** (`264` in `argus-lasso`, `22` in `argus-layer`, `19` in `argus-ipc`, `5` in `argus-sensors`).
- Verified PID validation guards for CLI `kill` and `set-affinity`, lazy ProBalance cgroup unit resolution, cached CPU topology detection, per-stream IPC generation tracking, type-isolated `SettingsPageConfig`, and batched multi-TID `renice` in the polkit helper.

## Published archives (v1.5.0)

The [release run](https://github.com/franzjeger/argus-lasso/actions/runs/36982262910)
built, signed and published both architectures after approval in the `release`
environment, and wrote the notes from the 1.5.0 changelog section. The published
archives were downloaded and checked on 2026-10-02:

- Each matches its `.sha256` file, and its `.minisig` signature verifies with
  `minisign-verify` against `dist/argus-lasso.pub` (key `D53DAD0590FF1744`); the
  replaced 1.3.1 key refuses it.
- `bundle.json` names build `360910fde5d5…` (the tagged commit), version 1.5.0
  and protocol 6 for x86_64 and aarch64. The x86_64 app and layer match the
  SHA-256 values in `bundle.json`, and the app's `build-info` agrees with it.
- The x86_64 archive is 14.5 MB (v1.4.0: 21.1 MB), the aarch64 one 13.8 MB.
- The download URLs in [binary archives](installation.md#binary-archives)
  resolve for the archive, checksum and signature.

## Release candidate verification (1.5.0)

Run on 2026-10-02 on the release branch (version 1.5.0, source otherwise equal
to `master` at
[`37bb8aa`](https://github.com/franzjeger/argus-lasso/commit/37bb8aa0a3e007a05deb390023432316a69e306c)):

- `cargo fmt --all --check` and workspace Clippy with `-D warnings` passed.
- Workspace tests passed: **302 tests**. The two opt-in tests (systemd unit
  restore, NVML) passed separately on this host.
- `cargo build --release --workspace --locked` passed. The release job's
  packaging step, run on that build, wrote bundle metadata for 1.5.0 and protocol
  6 and refused a 1.4.0 tag. The x86_64 archive is 14.5 MB, against 21.1 MB for
  v1.4.0 (fewer screenshots). It installed into an empty home directory with its
  own `install-binaries.sh` (service calls stubbed), and the updater's member
  patterns each matched exactly one app, layer and `bundle.json`. Signing was not
  run locally; the workflow checks each signature against `dist/argus-lasso.pub`
  before publishing.
- The read-only X11/Xvfb tour completed all 20 screens.
- Close to tray was tested live under Xvfb with a private session bus and a
  minimal StatusNotifierWatcher:
  - Clicking the checkbox stores the setting. Closing the window with
    `WM_DELETE_WINDOW` keeps the process running without a window.
  - A second launch and the tray's Activate each reopen the window.
  - Tray Quit and SIGTERM while the window is closed restore state and exit.
  - With the setting off, or with no tray, closing quits.
  - A tray that starts after Argus gets the icon. When that tray goes away, closing quits.

  The maintainer confirmed it on COSMIC.

## Published archives (v1.4.0)

The [release run](https://github.com/franzjeger/argus-lasso/actions/runs/36763083800)
built, signed and published both architectures after approval in the `release`
environment, and wrote the notes from the 1.4.0 changelog section. The published
x86_64 and aarch64 archives were downloaded and checked on 2026-09-30:

- Each matches its `.sha256` file, and its `.minisig` signature verifies with
  `minisign-verify` (the library the updater uses) against `dist/argus-lasso.pub`
  (key `D53DAD0590FF1744`). The replaced 1.3.1 key refuses the same signatures,
  as the updater of 1.3.1 and older will refuse them.
- `bundle.json` and `BUILD_ID` name build `c753c5c5c7be…` (the tagged commit),
  version 1.4.0 and protocol 6. The x86_64 app and layer match the SHA-256 values
  in `bundle.json`, and the app's `build-info` agrees with it.
- The download URLs in [binary archives](installation.md#binary-archives)
  resolve for the archive, checksum and signature.

On 2026-09-30 the in-app updater on the development host installed v1.4.0 from a
build that already carried the new key. The installed app's SHA-256 matched the
published `bundle.json`, and the rollback record was kept.

## Installation and releases

`master` at `37bb8aa` is installed locally as build `37bb8aa0a3e0-95554a7ff2f9`
(protocol 6) and runs as the user service. It is the 1.5.0 source apart from the
version number and documentation, and reports version 1.4.0. Games running an
earlier layer show "Telemetry disconnected" until restarted.

Releases from 1.4.0 on are signed with a new key (ID `D53DAD0590FF1744`). The
updater of 1.3.1 and older checks against the replaced key and refuses them, so
those installations move to 1.4.0 or later by hand once; see
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
