# Changelog

All notable changes to Argus-Lasso are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Entries under released versions describe their historical changes. For the
current source, release and verification status, see [docs/status.md](docs/status.md).

## [Unreleased]

### Added

- `argus-lasso install-helpers` installs or updates the CPU control helpers from a
  terminal, for example over SSH: pkexec asks for authentication in that terminal.
- Readable game recording history and A/B comparison with average/1% low FPS,
  p99 frametime and a peak-preserving graph loaded off the GUI thread.
- Live rule effects showing matching processes, overlapping assignments and
  requested versus observed CPU affinity and priorities.
- Signed matched app/layer update bundles, compatibility checks, durable recovery,
  one-step rollback and `build-info` / `rollback-update` commands. Local systemd
  service customizations are preserved.

- Overall CPU-load activation for ProBalance: above 85% for 3 seconds, a separate
  1% minimum process share, and recovery below 75% for 5 seconds by default.
  Detected games, verified launch trees, priority/manual targets and exempt
  processes are protected; cgroup units containing them are protected too.
- Consistent 0–100% CPU capacity scale across process tables, details, exports and
  CLI. The status output identifies the scale explicitly.

- Transparent Vulkan graphics HUD with 14 px default text, per-value colors,
  compact logical-CPU columns, independent background opacity and live settings.
- Versioned IPC with build identification, configuration replay, private runtime
  and Steam-visible home sockets, and explicit disconnected/stale telemetry.
- Per-present frame recording, bounded background CSV writer, documented AVG,
  1% low and p99 calculations, recording UI and optional desktop portal shortcut.
- Optional sandboxed sensor service for measured RAPL package power and cached
  configured RAM speed, with system authentication from Gaming → Sensors.
- Separate Gaming and Settings subsections, detached customization/detail windows,
  launcher/profile context, shared spacing and typography, and current guides/gallery.
- Paired user installer and future release archives containing app/layer/helper;
  workspace-wide build, lint, test and minimum-Rust checks.

### Fixed

- A rule's I/O priority is put back per thread, as its nice value already was,
  instead of giving every thread the main thread's old value.
- The launcher no longer takes an exited, unreaped process of the game's name for
  the running game, and when several processes carry the name it watches the one
  started first instead of whichever `/proc` listed first.
- Running the test suite no longer adds made-up lines ("[Rule:x] Set nice=5 on
  game(42)", "Termination requested for game (42)") to the real log in
  `~/.local/share/argus-lasso/`, and neither does `--ui-tour`. Only the app itself
  turns the log file on.
- A template picked, or "Add rule" chosen in Processes, while the rule editor is
  open is no longer dropped with a note to pick it again. The open editor comes
  to the front, and the new rule opens as soon as it is saved or cancelled. A
  rule from a template is titled "New rule", not "Edit Rule".
- Helper installation no longer trusts the user-writable staging directory: root
  installs only copies matching SHA-256 digests in its own command, so a file
  swapped while the polkit prompt is open is refused instead of installed.
- Nice and I/O priority now reach every thread of a process, as affinity already
  did. Rules, ProBalance and the Gaming Mode boost previously changed only the
  main thread. The renice helper is updated (v5) and asks to be reinstalled.
- The launcher tracks the game through a pidfd, so "Force quit game" cannot signal
  a process that reused the game's PID. An unreadable process name no longer
  matches every game name.
- The Vulkan layer passes the game's call through whenever its own bookkeeping is
  missing, presents the frame unchanged when the HUD submission fails, and never
  presents twice after a panic. Loader negotiation no longer reads NULL outputs as
  function pointers. Swapchain teardown waits only for the layer's own GPU work
  instead of `vkDeviceWaitIdle`, outside the shared lock. New
  `vkDestroyDevice`/`vkDestroyInstance` hooks drop state for recycled handles,
  and the library stays mapped while its threads run.
- Manual nice and I/O priority changes from the process table are protected from
  rule enforcement and ProBalance for 30 seconds, as affinity changes already
  were, instead of being undone on the next pass. The dialogs hold the process
  by pidfd and refuse to apply a change once it has exited.
- The launcher only accepts a game process started after its own launch, and
  never shells or launch wrappers (sh, reaper, pressure-vessel, Wine, Proton):
  a long-running `sh` no longer matches "Shadow of the Tomb Raider".
- HUD setup that fails part-way (for example out of video memory on a resize)
  frees what it created instead of leaking it on every attempt. A game that has
  run out of threads gets a blank HUD instead of a panic.
- Updates remove the staged apps and layer directories nothing refers to any more,
  keeping only the live layer and what one-step rollback restores. Previously
  every update, including a rejected one, left its files behind permanently.
- Without `$HOME`, the configuration no longer falls back to the shared `/tmp`,
  where another user could create it first; the home directory comes from the
  password database instead. Autostart and library scans no longer treat an
  empty `$HOME` as the current directory. The single-instance lock never uses
  the shared temp directory, and a lock that cannot be created is reported as
  such rather than as "already running".
- Affinity, nice and I/O priority reach threads that a single `/proc` thread
  listing skips while a program starts or ends other threads.
- Recording files are read only if they are regular files, opened without
  blocking: a FIFO left in their place by a game no longer hangs the recordings
  list for good. The recording control file is read with a size cap.
- Gaming Mode has one owner, the background service: the Gaming page and the tray
  ask it, and the page shows its state. Enabling from the tray no longer leaves
  the page reporting "off" while CPUs are parked, parking no longer freezes the
  window, and it takes one authorization instead of one per CPU. Activation whose
  parking fails brings every CPU back online and stays off.
- A settings file that cannot be read no longer loses everything on the next save.
  Defaults were used and the next change wrote them over the file, deleting every
  rule and profile. The file is now kept as `config.toml.unreadable-<time>` and
  the reason is shown.
- ProBalance tracks processes by PID and start time: a new process that reuses a
  throttled process's PID no longer inherits its entry, or gets its priority
  "restored" to the old process's value.
- Settings → Startup no longer overwrites an existing `argus-lasso.service`, which
  the installer keeps across updates for local customizations; it enables it.
  Program paths with spaces or special characters are quoted correctly in the
  autostart entry and the unit.
- The HUD's connection states read "Telemetry disconnected" and "Telemetry stale"
  like the rest of the English interface, instead of Norwegian.
- Failed priority and affinity changes report the system's reason (for example
  "Operation not permitted") in the notification, the rule log and the CLI,
  instead of guessing "needs root?".
- The HUD shows CPU temperature on Intel (coretemp's "Package id 0"), and AMD
  graphics temperature, clocks, load and video memory, which it looked up under
  labels those drivers never use. With an integrated and a discrete GPU, every
  GPU value comes from the card with the most video memory instead of a mix.
- A rule's nice value is no longer skipped for good on a process that reused the
  PID of an earlier one whose change failed, and a new process's rule is applied
  and logged once instead of twice.
- The process table's "Suspended" badge and Pause/Resume menu follow the kernel's
  process state. A process paused or resumed elsewhere shows correctly, and a new
  process that reuses a paused one's PID is no longer shown as suspended.
- Ending a process appears in the status bar's Recent events again. The messages
  had been reworded and no longer matched what the event list looked for.
- Picking a game from the Steam list now launches it. The app id kept its closing
  quote (`steam -applaunch 620"`), so the launcher rejected the command; names
  showed the stray quote too. Lutris names containing `|` are no longer split,
  and the Lutris database is read read-only.
- Rules that match the same process no longer undo each other: the last matching
  rule to set a value wins, as the rules tab already showed, and the value is
  changed once instead of on every pass. I/O priority failures are reported once
  instead of retried silently forever, the "none" I/O class no longer sends a
  level the kernel rejects, and I/O priority is read only for processes a rule
  sets it on. An affinity naming parked CPUs is no longer re-applied every pass.
- Disabling, deleting or editing a rule puts back the affinity, nice value and
  I/O priority it set, and clearing the default affinity releases the processes
  it pinned; before, they kept the values until they exited. A value changed
  since by something else, such as a manual change, is left alone.
- Importing rules gives each one its own ID, so importing an export of the current
  rules no longer creates twins that toggle, edit and delete together. Rules the
  kernel would refuse or clamp (nice 50, CPU list "abc", an invalid regular
  expression, I/O level 8) are skipped and named in the status line.
- The rule editor shows why a regular expression is invalid and does not save it,
  names the I/O classes, and offers a level only for real-time and best-effort. A
  rule with an I/O class but no level is enforced at level 4, as the editor showed,
  instead of 0. A rule saved without a name takes its pattern as the name. Rules
  with an invalid pattern are marked in the table.
- Opening a second rule editor brings the open one to the front instead of
  discarding its unsaved edits. Switching a rule on or off in the table while
  it is being edited is kept when the editor is saved, and deleting the rule or
  loading a profile closes its editor instead of letting Save bring the old
  rule back.
- Cancelling a profile load leaves the previous profile selected, so "Delete
  profile" no longer targets the one that was not loaded. The current profile
  can be picked again to reload it.
- Governor and EPP changes the kernel refuses are reported as failures with its
  reason, instead of as success: the power helper (v6, asks to be reinstalled)
  no longer ignores refused writes, and Settings checks what the kernel reports
  afterwards. Settings follows governor and EPP changed elsewhere (Gaming → Power
  profile, or EPP following a governor change) instead of showing and comparing
  against the values from startup, and keeps a choice not yet applied. Power
  changes in Settings and Gaming run in the background, so the window keeps
  responding while an authentication dialog is open.
- After an update or rollback is installed, "Check now" waits for the restart.
  Checking from the old process offered the just-installed release again and
  hid "Restart now", and installing it a second time saved the new binary as
  the previous one, so "Restore previous app and overlay" restored the update.
- Window opacity without the compositor's alpha modifier (X11, or compositors
  without `wp_alpha_modifier_v1`) is applied at startup and kept through theme
  changes and Apply, instead of only while the slider moves.
- Showing or hiding the HUD with `argus-lasso toggle-overlay` or the shortcut is
  reflected in Gaming → Overlay, and changing another overlay setting afterwards
  no longer turns the HUD back to what the Gaming page last knew.
- "Compare latest two" compares the two latest recordings, earlier as A and later
  as B, so an improvement shows as a gain. It used to pick the two newest files,
  which could be parts of one recording when the game recreated its swapchain,
  with the newest as A. Proton and Wine recordings are named after the game's
  Windows program instead of `wine64-preloader`. "Stop recording" no longer
  starts a new recording when the last one has just run out.
- ProBalance's cgroup method works with a hard quota on desktops where the cpu
  controller is not enabled for the app slice: the original quota is read from
  systemd instead of a `cpu.max` file that does not exist there. A throttle
  never gives a unit more CPU (an `idle` weight or a lower quota stays), a
  closed app's unit is no longer retried every second forever, and throttles
  left by a crash or kill are restored by the next run instead of being taken
  for the units' own settings. Only application and background units are
  throttled, never the desktop's own session services, and only the unit that
  owns the process's cgroup. `systemctl` calls time out after 5 seconds.
- A recording's frametime graph keeps its shape to the end when the recording had
  failed presents, instead of collapsing everything after the summed duration
  into a single point.
- One configuration writer persists current shared settings; unique staging files
  prevent collisions, directory fsync improves durability and save errors appear
  with a retry action.
- GUI process signals use stable pidfds. A new pending termination cancels the
  previous one; closing/restarting resumes pending targets. Tree termination has
  a three-second grace period and reports signal outcomes accurately.
- cgroup interventions preserve existing CPU quotas and refuse original policies
  that cannot be read or restored exactly through systemctl.
- Quoted game launcher paths/arguments, visible launch failures and cleanup of
  parking enabled by an unsuccessful launch.
- Non-blocking process exports and explicit missing/failed file-dialog errors.
- CPU history duration label, wider port filter and wrapping process toolbar.

- Failed NVML initialization is retried only once per minute instead of every
  sensor tick; successful contexts remain shared and reused.
- Hardware history, alerts and HUD reuse one extended sensor snapshot per daemon
  sampling tick. RAM speed from the service no longer enters the permanent local
  firmware cache. Cache reads are bounded and reject future timestamps and
  invalid measurements while preserving measured zero power.
- Recover Wine/Proton process names from unambiguous mapped executables when
  games clear their name and command line (including The Last of Us Part II).
  Refresh cached identities after renames and periodically after startup.
- Completed the shared process-string and reusable snapshot migration across
  the GUI, JSON exports and read-only preview so the workspace builds again.
- Restored independent one-second sensor sampling and the existing weighted CPU
  readings for overlay telemetry after the performance refactor.
- Sensor sparklines iterate the ring buffer directly without temporary history
  or point vectors; history order is tested across repeated wraps.
- CLI overlay toggles use separate queued requests, preserving rapid/concurrent
  invocations; unsuccessful request removal no longer toggles repeatedly.

- Global Vulkan HUD activation now filters for detected Steam/Proton games or
  explicit per-game opt-in; known terminals and desktop hosts are excluded even
  when they inherit game launch variables.
- Wrong installed overlay library selection, silent IPC schema mismatch and missed
  initial configuration; missing sensors no longer masquerade as measured zeroes.
- Per-frame rasterization after the old frame deque saturated; text caching and
  sampling now have separate frequencies from drawing and frame collection.
- Slow graph refresh: independent default 60 Hz update with spike-preserving history.
- Gaming/Settings edits overwriting each other's newer values, and an old global
  loading toggle replacing the correct versioned manifest path.
- Bundled font registration now belongs to each UI context; independent contexts
  no longer share an initialization flag that can leave the bold font missing.
- Native subwindow opacity: transparent surfaces and clear color, viewport-local
  style updates, and live inheritance for customization, details and benchmark results.
- Long process PIDs and multicore numbers overflowing table cells: font-aware
  minimum widths, cell clipping and horizontal scrolling for narrow windows.
- CPU accounting no longer multiplies process shares by logical CPU count or
  counts guest CPU time twice. Weighted system load excludes offline CPUs;
  hotplug, reset/missing counters and long sample gaps break activation windows.
- ProBalance uses separate system and process thresholds with consecutive
  activation/recovery windows. Legacy per-core threshold keys are superseded
  without changing exemptions or priority settings.
- Typed numbers in Settings and ProBalance take effect when entered, not with each
  keystroke: typing "95" as the temperature alert briefly stored 50 °C, the
  field's minimum, and could raise a false alert. ProBalance no longer lowers the
  restore threshold while the activation threshold is being typed; a draft whose
  restore is not below activation says so and cannot be applied.
- A default CPU list typed in Settings is used, or put back with a message if it
  is not valid, when the section or tab is left or the window is closed, not only
  on Enter. It was lost while still showing, with the presets highlighting it as
  if it were in effect.
- The process affinity dialog and Settings' "Pick CPUs…" no longer share one
  window. With both open, clicks meant for the visible one went to the other, so
  a process's affinity could be changed from Settings. "Pick CPUs…" also stays
  open when another Settings section is shown.
- "Start with session" uses one mechanism: the installed service if there is one,
  otherwise an XDG autostart entry. Both were set up, so two instances started at
  login and the window came up. A second `--minimized` launch no longer asks for
  the window, a request left while Argus was not running is dropped at start, and
  one that arrives before the window exists is kept until it does. The autostart
  entry points at the binary on disk, not at the " (deleted)" image after an
  update, and systemctl runs in the background.
- "Delete rule?" deletes the rule it names; selecting another row while it was
  open switched it to that row. "Show all rules" under Live rule effects clears
  the rule filter again.
- Dragging a slider or colour in the HUD customization window updates the HUD
  live but saves the configuration once it settles, and the Activity log records
  only what changed: it got a "Config updated" line, and the file a save, for
  every frame of the drag.
- Start or Stop recording is no longer undone by a recordings scan that read the
  state just before the click, which put the button back so a second click
  restarted the recording.
- Changing the theme no longer resets the zoom or applies the display scale from
  startup, which showed the UI at the wrong size after moving to a monitor with a
  different scale.
- A process's details window closes when the process exits even if its PID is
  reused at once, instead of showing the new process under the old one's name.
- ProBalance never lowers a process's nice value: a CPU hog already at nice 19 was
  "throttled" to the floor of 15, that is, given more CPU, or retried every second
  when that was refused. A nice throttle is put back only while the process still
  has the value ProBalance set, so a manual change made meanwhile (which also
  exempts the process) or a rule's is no longer undone.
- Stopping Argus with `systemctl --user stop`, logging out or Ctrl+C now restores
  parked CPUs, ProBalance throttles and Gaming Mode nices, as quitting from the
  window or tray does. The process died at once on those signals, leaving CPUs
  offline system-wide. Unparking now comes first and the restore gets up to 10 s.
- A rule that sets a process's nice value takes it over from ProBalance: the rule
  now records the value from before ProBalance's throttle as the one to put back,
  and ProBalance leaves processes whose nice a rule sets alone. Deleting such a
  rule left the process at the throttle value for good.
- Gaming Mode puts back what it changed on each process when it ends: the
  preferred-core pin stayed after Gaming Mode was off, and its -1 nice was
  restored even over a change made since. It changes only what no rule sets (a
  rule's nice or affinity used to be overridden until the next pass), only ever
  raises priority, and also boosts the detected game and processes already
  running when it is turned on, not only processes started afterwards.
- "Restore all CPU assignments" restores the processes Argus changed, to what
  they had before. It reset every process ever seen, pinning those first seen
  while CPUs were parked to the CPUs online then.
- Turning Gaming Mode off while a detected game runs keeps it off until the game
  exits, instead of auto-detection turning it (and CPU parking) back on within a
  second.
- Putting back a nice value (a rule undone, a ProBalance throttle or Gaming Mode's
  boost ending) restores each thread to its own value. All threads were set to the
  main thread's, raising the priority of threads a process had lowered itself,
  such as a browser's background threads.
- Clearing the default affinity also releases processes that inherited it from a
  process it had pinned; they had the mask from the start, so nothing recorded
  them and they stayed pinned.
- A ProBalance cgroup throttle whose systemctl call timed out keeps its record of
  the unit's original CPU policy, so it is put back if the change landed anyway.
  Forgetting it let a later throttle record the throttled weight as the original.
- The update rollback record no longer undoes an installation made since: a
  `make install` after an in-app update could be reverted by "Restore previous app
  and overlay", or silently by the next start after an interrupted update. The
  installer removes the record, and a record for another path or a damaged one no
  longer stops every start (a restart loop under systemd). The installer also
  keeps only the current and previous layer builds instead of every one.
- A configuration that is a symlink to a missing file, or that cannot be checked,
  is reported and set aside rather than taken as absent and replaced with
  defaults. The old `process-lasso-rs` configuration is migrated once, atomically,
  and never again over a newer one; `--ui-tour` no longer migrates it.
- The optional sensor service runs in a tighter sandbox: no sockets or network,
  no privileged or resource system calls, and no view of other processes. It needs
  none of them, and as root they could reach the system bus if it were ever
  compromised. `systemd-analyze security` rates it 0.7 instead of 3.5.
- `make uninstall` also removes the update backups, the rollback record and the
  autostart entry, and prints the commands to remove the system-wide CPU control
  helpers when they are installed; they still granted their actions afterwards.
- Installing the CPU control helpers no longer has root read a staging directory
  in the user's configuration: the files travel in the root command itself. A
  process of the user could replace a staged file with a FIFO that hung root or a
  device node; home directories with spaces or non-ASCII characters no longer stop
  the install.
- A recording is no longer lost when the game exits or crashes before finishing
  it. The layer writes rows out every 100 ms and finishes recordings when the game
  tears down its device; a recording left unfinished is listed as incomplete, with
  the rows it has, once the game has exited. The list keeps whole sessions instead
  of the 30 newest files, so a session with many swapchains no longer hides every
  earlier one, and swapchains that presented at most once are not listed.
- The HUD appears on devices that list a compute or transfer queue family before
  the graphics one; it used the first family requested and stayed off, with
  nothing logged.
- The HUD is drawn with the right colours on sRGB swapchains, where its colours
  were encoded twice and looked washed out, and is left off HDR swapchains (HDR10
  or scRGB), where its sRGB colours meant up to thousands of nits. It is also left
  off swapchains whose images may have no memory yet or whose image views would
  inherit a storage usage their format does not support.
- The CPU control helpers no longer run without a password for every local user:
  their polkit actions ask for an administrator's password, and a polkit rule
  installed with them exempts only the user who installed them, from an active
  local session. The helpers are updated (v7) and ask to be reinstalled.

### Changed

- Building from source needs Rust **1.95** or newer, the floor of egui 0.36.
  Dependencies are updated: egui, eframe and egui_extras 0.36, nix 0.31, png 0.18
  and nvml-wrapper 0.13.
- **Releases are signed with a new key** (ID `D53DAD0590FF1744`), held only in the
  approval-gated `release` environment. The in-app updater of 1.3.1 and older
  checks against the old key and refuses these releases: install the next release
  by hand once, and later updates work in the app again.
- Settings take effect as they change, like theme and opacity already did, instead
  of waiting for **Apply changes** on the same page. Number fields are stored when
  the drag is released or the value entered, a typed CPU list on Enter once it is
  valid (an invalid one says why), and a governor or EPP when picked; the picker
  then shows what the kernel has. ProBalance's on/off switch acts at once; its
  thresholds keep **Apply changes**, since they must be valid together.
- The tray menu has "Open Argus-Lasso", and a left click on the tray icon does
  the same. Launching Argus from the app menu while it already runs brings the
  running window to the front instead of only printing "already running".

- Screen readers can use the hand-drawn controls: the page tabs, filter chips,
  on/off switches, segmented choices and CPU thread tiles now report their name
  and state. The process filter chips explain themselves on hover.

- "Enable in-game overlay" is a checkbox like the setting below it; while off it
  used to look like plain text. The process filter's hint fits its field, with
  the details in its tooltip.

- A rule's match type is a closed set (contains, exact, regex) stored as the same
  words as before. An unknown value is reported instead of silently matching as
  "contains"; an imported rule file with one names it in the error.

- Documentation reconciled with current source: release/source distinction,
  updater signing and restart behavior, cgroup defaults/restoration limits,
  dated validation evidence and refreshed main-page screenshots.

- Unified GUI heading/button weights with the bundled bold face, secondary text
  styling and readable small-label sizes; CPU selection grids adapt to width.
- Standardized dialog and action capitalization, corrected binary memory/I/O
  unit labels, and removed obsolete helper instructions and duplicate overlay
  setup text. Overview PID columns now fit long process IDs.

- Repository renamed to `franzjeger/argus-lasso`, preserving Git history and releases.
- Old screenshots replaced; staged investigation moved to a dated archive.
- Validation claims distinguish native Vulkan tests from pending controlled PoE2,
  actual DXVK/VKD3D game tests, 32-bit packaging and OpenGL support.


## [1.3.1] — 2026-08-23

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

## [1.2.0] — 2026-08-15

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

- **Scroll bars are visible without hovering them.** egui's `solid()` preset
  leaves the handle fully transparent until the pointer enters the area, so
  any panel whose content overflows looked truncated rather than scrollable —
  the Settings tab's last card, and the rule dialog's Nice and I/O priority
  rows. Fixed in the theme, so it covers every scroll area including the
  dialogs. ([#41], [#47])
- **The rule dialog fits its own content.** At 560x400 the affinity picker's
  quick-select row was clipped horizontally — that row is sized from the CPU
  topology, so a 32-core machine overflows it — and Nice and I/O priority sat
  below the fold whenever the picker was expanded. ([#47])
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

[Unreleased]: https://github.com/franzjeger/argus-lasso/compare/v1.3.1...HEAD
[1.3.1]: https://github.com/franzjeger/argus-lasso/compare/v1.3.0...v1.3.1
[1.2.0]: https://github.com/franzjeger/argus-lasso/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/franzjeger/argus-lasso/compare/v1.0.9...v1.1.0
[1.0.9]: https://github.com/franzjeger/argus-lasso/compare/v1.0.8...v1.0.9
[1.0.8]: https://github.com/franzjeger/argus-lasso/compare/v1.0.7...v1.0.8
[1.0.7]: https://github.com/franzjeger/argus-lasso/compare/v1.0.6...v1.0.7
[1.0.6]: https://github.com/franzjeger/argus-lasso/compare/v1.0.5...v1.0.6
[1.0.5]: https://github.com/franzjeger/argus-lasso/compare/v1.0.4...v1.0.5
[1.0.4]: https://github.com/franzjeger/argus-lasso/compare/v1.0.3...v1.0.4
[1.0.3]: https://github.com/franzjeger/argus-lasso/compare/v1.0.2...v1.0.3
[1.0.2]: https://github.com/franzjeger/argus-lasso/compare/v1.0.1...v1.0.2
[1.0.1]: https://github.com/franzjeger/argus-lasso/releases/tag/v1.0.1
[#22]: https://github.com/franzjeger/argus-lasso/pull/22
[#23]: https://github.com/franzjeger/argus-lasso/pull/23
[#24]: https://github.com/franzjeger/argus-lasso/pull/24
[#25]: https://github.com/franzjeger/argus-lasso/pull/25
[#26]: https://github.com/franzjeger/argus-lasso/pull/26
[#27]: https://github.com/franzjeger/argus-lasso/pull/27
[#28]: https://github.com/franzjeger/argus-lasso/pull/28
[#29]: https://github.com/franzjeger/argus-lasso/pull/29
[#30]: https://github.com/franzjeger/argus-lasso/pull/30
[#31]: https://github.com/franzjeger/argus-lasso/pull/31
[#38]: https://github.com/franzjeger/argus-lasso/pull/38
[#40]: https://github.com/franzjeger/argus-lasso/pull/40
[#41]: https://github.com/franzjeger/argus-lasso/pull/41
[#47]: https://github.com/franzjeger/argus-lasso/pull/47
[#53]: https://github.com/franzjeger/argus-lasso/pull/53
[#54]: https://github.com/franzjeger/argus-lasso/pull/54
