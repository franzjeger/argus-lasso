# Changelog

All notable changes to Argus-Lasso are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Adwaita Dark and Adwaita Light themes.** Two new entries in Settings →
  Appearance use GNOME's libadwaita palette (#3584e4 accent, softer corner
  radii) so the window no longer looks like a KDE transplant on GNOME. A
  fresh install now auto-picks the theme family from the running desktop
  (`XDG_CURRENT_DESKTOP`): Adwaita on GNOME/Ubuntu/Unity, Breeze elsewhere.
  Existing configs keep their saved theme.

### Fixed

- **No titlebar on GNOME Wayland.** Mutter never draws server-side window
  decorations, and the winit build lacked the client-side fallback — the
  window appeared with no titlebar, border, or close/minimise buttons.
  winit's `wayland-csd-adwaita` feature is now enabled, so GNOME gets an
  Adwaita-styled client-side titlebar; KDE/KWin keeps its server-side
  decorations unchanged.

## [1.3.0] — 2026-08-15

### Added

- **Kill a whole process tree.** Right-click a process and choose "Kill Tree"
  to terminate it and every descendant. Children are signalled first (leaves
  before parents) so they aren't reparented to init mid-sweep; SIGTERM is
  sent, then any survivors are SIGKILLed after a 300 ms grace period.
- **Export the process list.** The toolbar "Export ▾" menu writes the current
  snapshot to a CSV or JSON file of your choice (pid, ppid, name, CPU/GPU %,
  RSS, nice, affinity, I/O, disk rates, cmdline).
- **Filter by listening port.** Type a local port (e.g. `8080`) in the filter
  row to show only the processes holding a socket bound to it. Matches
  `/proc/net/tcp{,6}` socket inodes against each candidate's `/proc/PID/fd`;
  the result is cached for ~1 s to avoid per-frame `/proc` reads.

## [1.2.3] — 2026-08-15

### Fixed

- **"Restart now" after a self-update works.** Once the updater renames over
  the binary, the kernel marks the running image as `(deleted)`, so
  `current_exe()` returned a path with that suffix and `canonicalize()`
  failed with ENOENT. The suffix is now stripped before the restart `exec`.

## [1.2.2] — 2026-08-15

### Fixed

- **The opacity slider's track is visible.** It was painted with the same
  colour as the window background, so only the thumb and value showed and it
  read as a floating box. The value fill (accent colour) is now enabled.

## [1.2.1] — 2026-08-15

### Fixed

- **EPP change failures are now reported.** A failed `power-profile epp` call
  was silently discarded; the log now shows `(failed: …)` instead of claiming
  success.
- **Kill-undo SIGCONT failures are now reported.** If the resume signal fails,
  the log says the process is still suspended instead of "resumed".
- **Autostart enable/disable reports the actual result.** The systemd unit
  write and `systemctl enable`/`disable` results were discarded; the status
  message now reflects whether both XDG and systemd succeeded.
- **Gaming Mode "Kill game" reports SIGTERM failures.** A failed signal is
  logged as an error instead of "Sent SIGTERM".

## [1.2.0] — 2026-08-04

### Security

- **The privileged helper no longer installs a blanket `NOPASSWD` sudoers rule.**
  One helper covering every privileged operation could only ever have one
  policy, so the rule granted any process running as the user passwordless
  root for all of them — including `renice-pid` against *any* PID on the
  system. It is replaced by three separate root-owned helpers under
  `/usr/local/lib/argus-lasso/`, each with its own polkit action:
  `cpu-park`, `power-profile` and `renice`. Installing removes
  `/etc/sudoers.d/argus-lasso` and the old helper. ([#30])
- **`renice` is confined to the caller's own processes.** It compares
  `PKEXEC_UID` against the owner of the target PID and refuses anything else,
  including when invoked outside pkexec where it cannot tell who is asking.
  ([#30])
- **Releases are verified with a minisign signature** before anything is
  written, against a public key compiled into the binary. The `.sha256` alone
  never established authenticity — it shipped from the same release as the
  tarball. ([#28])
- The minisign signing key is now configured: `dist/argus-lasso.pub` holds a
  real public key and the release workflow signs each tarball, so the in-app
  updater can verify and self-install.

- The root-password fallback for helper installation is gone. The helpers are
  authorised by polkit, so on a system without it they would have been
  installed and then permanently unusable — and the fallback put a plaintext
  password through the process for no benefit. ([#30])

### Fixed

- **The Settings scroll bar is visible**, so the last card stops looking cut
  off. The content is taller than the panel at the default window size, so it
  scrolls correctly — but egui's default bar floats in colours close to the
  page, so nothing signalled that scrolling was possible. ([#41])
- The window opacity slider was short and low-contrast, reading as an empty
  box followed by a number in the dark theme, and showed `1.000` where it
  means `100%`. ([#41])
- The update banner can be dismissed. ([#38])
- **Restarting into an update no longer strands parked CPUs.** "Restart now"
  exec'd straight over the process image, so `on_exit` never ran and the
  daemon never restored nice values, throttles or parked cores — and the
  originals live only in the replaced process's memory. ([#28])
- **Self-update refreshes the desktop entry, systemd unit and icons**, which
  previously stayed frozen at whatever version first installed them. Only
  files that already exist are rewritten. ([#28])
- **Disk I/O rates were wrong by the sampling cadence.** Raw byte deltas were
  labelled "bytes/s", so the figure read twice the real rate at the default
  500 ms interval. ([#29])
- **The dashboard cards no longer overflow the panel.** The KPI row's width
  arithmetic ignored `item_spacing`, pushing the ProBalance card off the right
  edge. ([#28])
- `argus-lasso --version` errored with "unexpected argument". ([#31])
- **The AUR package's systemd unit pointed at a path it never installed.** The
  package puts the binary in `/usr/bin` but shipped a unit with
  `ExecStart=%h/.local/bin/argus-lasso`, so the service failed to start. It
  also installed only the 256px icon, skipping the scalable and tiered small
  masters. ([#31])
- A `tar` archive containing two `argus-lasso` entries would have been spliced
  into one file and marked executable; the updater now refuses anything but a
  single match. ([#28])
- The update UI could wedge on "Checking for updates…" until restart if the
  worker thread died before reporting. ([#28])
- **A failed suspend no longer arms a dead kill-undo window.** `SIGSTOP`/`SIGCONT`
  results were discarded; a failed `SIGSTOP` (e.g. `EPERM`) still armed the 5 s
  undo countdown on a process that was never stopped. It now kills immediately
  and reports instead. ([#53])
- **Affinity / nice / I/O-priority failures are no longer silent.** They closed
  the dialog with no log or notification; they now log and notify. ([#53])
- **The updater detects an oversized download** instead of failing later as a
  confusing "checksum mismatch". ([#53])
- **A reversed cpulist range (e.g. `7-2`) is rejected** instead of silently
  parsing to an accepted empty set. ([#53])
- **The "Tools" button no longer strips every button's frame.** It mutated the
  global widget visuals; the frame strip is now scoped to that one button. ([#53])
- **Two `mem_bench` `start()` calls can no longer race** into two workers (and a
  second large allocation); the `running` flag is now checked and set atomically. ([#53])
- **HW Monitor merges sensor groups by `(category, name)`**, not name alone, so
  two categories exposing the same group name no longer interleave. ([#53])
- **The daemon is bounded-joined at exit**, so a stuck restore is logged rather
  than abandoned. ([#53])
- **The repaint interval has a 100 ms floor**, so `0` no longer means unbounded
  60 fps. ([#53])
- **The rule dialog skips regex recompile** when the pattern is unchanged. ([#53])

### Performance

- **Idle CPU cut from 1.31% to 0.58% of a core.** Process names and command
  lines are cached per PID instead of being re-read every pass; affinity, I/O
  priority and disk rates are sampled on the display cadence rather than the
  enforce cadence; an enforce pass with no rules and no default affinity no
  longer drives a `/proc` walk; and the published snapshot moved behind an
  `Arc` instead of being deep-cloned twice per display tick. ([#29])

  Measured on a 578-process machine over 120 s windows, warm, with the window
  hidden (`--minimized --no-tray`). That condition matters and was missing
  from the original note: with a visible window the figure is dominated by
  rendering rather than by any of the above. On a machine that falls back to
  Mesa's software rasteriser it is several times higher and moves with whether
  the window is exposed at all, so a number quoted without saying which state
  it was taken in says very little.

### Changed

- **Round 4 of the design review**, covering all ten cross-cutting findings
  and the per-tab list: the Rules empty state and the rule dialog rebuilt
  against their mockups, the Benchmark tab's pre-run state, consistent apply-bar
  placement, card framing around the Processes plot row, and a dozen smaller
  corrections. ([#28])
- **The egui stack moves to 0.34.** `App::ui` replaces the deprecated
  `App::update` as the entry point, panels are shown with `show_inside`, and
  `show_viewport_immediate` hands its callback a `Ui` rather than a `Context` —
  so all seven dialogs changed with it. 0.34 also flips eframe's default
  renderer to wgpu; glow is now selected explicitly, since wgpu pulls a far
  larger tree and changes the surface eframe hands to the Wayland opacity
  code. ([#40])
- The minimum supported Rust version is declared and checked in CI. It is
  **1.92** — the floor the egui 0.34 stack imposes. The README had claimed
  1.75, and the field had been unset. ([#31], [#40])
- **The README now matches the code.** The dependency table was missing
  `nvml-wrapper`, `ureq`, `sha2`, `minisign-verify`, and `wayland-backend`/
  `wayland-sys`, and the auto-update feature was undocumented; both are now
  corrected. A stale "top-5" comment in the Overview tab is fixed to top-10. ([#54])

### Added

- **`--ui-tour DIR`** (hidden): walks every screen, captures each through
  egui's own screenshot command, and exits — so the README screenshots can be
  regenerated consistently. The affinity, nice and I/O-priority dialogs are
  not covered; they are separate OS windows the glow backend will not
  screenshot. ([#28])

## [1.1.0] — 2026-08-02

### Added

- In-app update check and self-install from GitHub releases. ([#27])

## [1.0.9] — 2026-08-02

### Fixed

- Icons redrawn as tiered vector masters; icon size is no longer hardcoded, so
  small sizes stop turning to mush. ([#25])

### Changed

- Settings tab finished against its mockup. ([#26])

## [1.0.8] — 2026-07-31

### Changed

- The design package implemented against its actual mockups. ([#24])

## [1.0.7] — 2026-07-31

### Fixed

- Rendering defects found by running the redesigned UI. ([#23])

## [1.0.6] — 2026-07-31

### Changed

- The whole UI redesigned against the design handoff spec. ([#22])

## [1.0.5] — 2026-07-31

### Changed

- Design round 2: heatmap cells, quick-filter chips, light-theme contrast.
- Design pass over navigation, the notification centre, the kill toast and the
  layout system.
- README brought up to date with the new features and UI.

## [1.0.4] — 2026-07-31

### Added

- cgroup v2 ProBalance backend, opt-in. See `docs/design-cgroup-probalance.md`.

### Fixed

- Seven defects from review of the preceding feature rounds.

## [1.0.3] — 2026-07-30

### Added

- Per-process GPU%, automatic game detection, a persistent log, and
  `status --json`.
- Process details window, Overview disk and network graphs, regex filtering.
- Column chooser, daemon unit tests, and an AUR PKGBUILD.

### Fixed

- Eight defects found reviewing the new feature code.
- `status --json` rounded `cpu_percent` as f64 to avoid f32 noise.

## [1.0.2] — 2026-07-30

### Added

- pkexec install path, "remember settings" rules, and power profiles.

### Fixed

- CPU graph area fill.

## [1.0.1] — 2026-07-30

### Added

- Dependabot, a security-audit workflow, and a tag-triggered release workflow
  building both x86_64 and aarch64.

### Fixed

- Twenty-four bugs found in a deep code review.
- A single-instance lock, so two instances stop clobbering each other's config.

### Performance

- Virtualized the process table; dropped per-frame clones and sysfs reads.

[Unreleased]: https://github.com/franzjeger/process-lasso-linux-rs/compare/v1.2.0...HEAD
[1.2.0]: https://github.com/franzjeger/process-lasso-linux-rs/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/franzjeger/process-lasso-linux-rs/compare/v1.0.9...v1.1.0
[1.0.9]: https://github.com/franzjeger/process-lasso-linux-rs/compare/v1.0.8...v1.0.9
[1.0.8]: https://github.com/franzjeger/process-lasso-linux-rs/compare/v1.0.7...v1.0.8
[1.0.7]: https://github.com/franzjeger/process-lasso-linux-rs/compare/v1.0.6...v1.0.7
[1.0.6]: https://github.com/franzjeger/process-lasso-linux-rs/compare/v1.0.5...v1.0.6
[1.0.5]: https://github.com/franzjeger/process-lasso-linux-rs/compare/v1.0.4...v1.0.5
[1.0.4]: https://github.com/franzjeger/process-lasso-linux-rs/compare/v1.0.3...v1.0.4
[1.0.3]: https://github.com/franzjeger/process-lasso-linux-rs/compare/v1.0.2...v1.0.3
[1.0.2]: https://github.com/franzjeger/process-lasso-linux-rs/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/franzjeger/process-lasso-linux-rs/releases/tag/v1.0.1
[#22]: https://github.com/franzjeger/process-lasso-linux-rs/pull/22
[#23]: https://github.com/franzjeger/process-lasso-linux-rs/pull/23
[#24]: https://github.com/franzjeger/process-lasso-linux-rs/pull/24
[#25]: https://github.com/franzjeger/process-lasso-linux-rs/pull/25
[#26]: https://github.com/franzjeger/process-lasso-linux-rs/pull/26
[#27]: https://github.com/franzjeger/process-lasso-linux-rs/pull/27
[#28]: https://github.com/franzjeger/process-lasso-linux-rs/pull/28
[#29]: https://github.com/franzjeger/process-lasso-linux-rs/pull/29
[#30]: https://github.com/franzjeger/process-lasso-linux-rs/pull/30
[#31]: https://github.com/franzjeger/process-lasso-linux-rs/pull/31
[#38]: https://github.com/franzjeger/process-lasso-linux-rs/pull/38
[#40]: https://github.com/franzjeger/process-lasso-linux-rs/pull/40
[#41]: https://github.com/franzjeger/process-lasso-linux-rs/pull/41
[#53]: https://github.com/franzjeger/process-lasso-linux-rs/pull/53
[#54]: https://github.com/franzjeger/process-lasso-linux-rs/pull/54
