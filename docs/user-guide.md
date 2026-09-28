# Using Argus-Lasso

This guide describes the current source tree. The UI uses English labels.
[Install](installation.md) · [Screenshot gallery](screenshots.md) · [Project status](status.md)

Memory values use binary units (MiB/GiB). Disk and network rates use MiB/s;
memory bandwidth benchmarks report decimal GB/s. Temperature readings use °C.

## Navigation

| Menu | Purpose |
|---|---|
| Overview | CPU activity, memory and system summary |
| Processes | Live process table, filters, CPU history, sorting, column selection and export |
| Process rules | Persistent rules matched against processes |
| ProBalance | Automatic priority adjustments, active interventions and exemptions |
| Gaming → CPU & performance | Gaming mode, logical CPU assignment, parking and CPU control helpers |
| Gaming → Launcher & profiles | Steam/Lutris game selection and launch profiles |
| Gaming → Overlay | Enable the HUD, choose loading behavior and open customization |
| Gaming → Recording | Start/stop frame captures, configure a shortcut and inspect results |
| Gaming → Sensors | Optional extended sensor service and its actual availability |
| Tools → Hardware sensors | Grouped CPU, GPU, memory, storage and other available sensors |
| Tools → Memory benchmarks | Memory bandwidth/latency tools; distinct from game frame recording |
| Tools → Activity log | Actions, failures and diagnostic events |
| Settings → Appearance | Theme, scale, window opacity and visual preferences |
| Settings → Processes | Refresh behavior and default process handling |
| Settings → CPU power | CPU-related application preferences |
| Settings → Notifications | Desktop notifications and hardware alerts |
| Settings → Startup & updates | Tray/startup behavior and release checks |

Appearance and HUD edits save immediately. Forms with an **Apply changes** button
use a draft until applied. Settings and Gaming preserve each other's saved fields.
One monitor-thread writer persists the latest shared configuration. A failed save
appears in a persistent banner with **Retry saving**; it is not silently treated
as a successful disk write.

## Process table

Wine/Proton processes use the executable filename where available. If a game
clears its process name and command line, Argus-Lasso checks its mapped `.exe`
files; ambiguous or unreadable mappings retain the ordinary process name.
Cached names refresh on process renames and at least every five seconds.

Right-click a process for actions; double-click for details. Process identity is
shown once at the top of the context menu. Priority, CPU assignment and rule
creation are grouped separately from termination. The GUI Delete action offers
a five-second undo window. Scheduling another termination cancels and resumes
the previous pending target. Closing Argus or restarting it also cancels a pending
termination. Delayed signals use Linux process handles so PID reuse cannot redirect
them. Process actions report an error if those handles are unavailable. A CLI kill
has no undo window. Ending a process tree sends SIGTERM first, then SIGKILL after
three seconds to remaining targets.

Numeric columns align right. Font-aware minimum widths keep long PIDs and
multicore CPU readings inside their cells, including when reusing older saved
widths. Narrow windows offer horizontal scrolling. Hover a truncated name for
its command line and additional information. Column headers sort; their context
menu or **Columns** chooses visible fields.

**CPU% is always 0–100% of the available CPU capacity.** The process table,
process details, exports and CLI show each process's share of the whole system.
With 32 online logical CPUs, one fully busy CPU contributes 3.125%; sixteen
contribute 50%; all 32 contribute 100%. Individual logical CPU tiles still show
0–100% for their own CPU. These percentages describe scheduled CPU time, not a
frequency-adjusted performance score.

System usage is the weighted busy-time delta across online CPU counters from
`/proc/stat`. Idle and I/O-wait time are excluded from busy time. Guest time is
already included in user/nice and is not added twice. Offline CPUs do not dilute
the system total. A topology change or invalid counter sample resets the baseline
and prevents ProBalance activation for that interval. The UI retains its last
valid system value during this brief warmup.

## ProBalance

ProBalance now uses **overall system pressure to activate**, then evaluates
eligible processes separately. Default controls are:

| Control | Default | Meaning |
|---|---:|---|
| System CPU above | 85% for 3 seconds | Require consecutive high system-load samples |
| Minimum process CPU | 1% of total | Only processes consuming at least this share are candidates |
| System CPU below | 75% for 5 seconds | Restore priorities after pressure subsides |

A single busy thread on a 32-thread machine does not activate ProBalance by
itself. Dropping below the activation threshold resets the high-load timer;
isolated spikes do not accumulate into a later action. Recovery also begins if
the individual process falls below its minimum share. Invalid/missing system
samples prevent new actions and allow existing penalties to recover after the
configured recovery window. Long sampling gaps break the activation window.

Detected Steam/Proton games, verified Argus launch roots and their descendants
are protected, along with Argus itself, explicit high-priority/manual targets and
the configured exemptions. Unit-level cgroup adjustments skip units containing
a protected process. **Wayland foreground detection is not universal:** an
unrecognized game or important application needs an explicit exemption. This is
a system-load policy, not a guarantee of higher FPS or a clone of another tool's
responsiveness algorithm.

The existing nice and optional cgroup CPUWeight backends remain available. They
adjust scheduling priority under contention, not the measured CPU percentage.
Cgroup also offers a separate optional quota. Argus records the existing quota
before changing it and restores it afterward. Weight-only interventions leave
quotas unchanged. Unreadable or unrepresentable original policies are not modified
through the cgroup backend. The default remains `nice`. [Backend details and limits](design-cgroup-probalance.md).

Configuration uses `system_cpu_threshold_percent`,
`system_restore_threshold_percent` and `process_min_cpu_percent` under
`[probalance]`. Old `cpu_threshold_percent` and `restore_threshold_percent` keys
used incompatible per-core units; they are superseded by the new defaults rather
than silently reinterpreted. Existing exemptions, timing, method and nice settings
are preserved. Subsequent config saves write the new keys. Recovery is constrained
to stay below activation, and all percentage controls reject non-finite values.

**Process rules → Live rule effects** lists running matches and requested versus
observed values. Selecting a rule narrows the list; **Show all rules** clears it.
Rules run top to bottom, with the last matching assignment winning for each field.
Conflicting overlapping assignments are marked. Requested values are not a promise
of successful enforcement: permissions, manual overrides and ProBalance can differ.

## Gaming, telemetry and windows

Launcher commands support quoted paths and arguments, including spaces. Commands
are executed directly: shell operators and variable expansion are not evaluated.
An invalid command or failed launch is shown immediately. If this launch enabled
CPU parking but the program could not start, that parking is undone.


Enable the display under **Gaming → Overlay** to unlock **Customize overlay**.
The separate window controls visible readings, individual value colors, text size,
placement, spacing, background opacity, CPU columns, dividers and graph behavior.
It can move outside the main window and stays open when changing main tabs.
Process details and the existing CPU assignment/priority/rule/game-picker dialogs
also use separate native windows. Their backgrounds follow the Appearance opacity
setting live; controls remain readable. Small confirmation popups remain attached.

The overlay needs both a loaded Vulkan layer and a running normal-user Argus
process. See [overlay troubleshooting and metric definitions](overlay.md).
Changing loading conditions or installing a new layer requires restarting a game;
ordinary visibility, color and layout edits apply while it runs.

**Gaming → Recording** records CPU present intervals from loaded layers. The
GlobalShortcuts portal suggests Shift+F2; your desktop confirms the actual key.
The registration lasts for the Argus session. A desktop without a compatible
portal can use the recording button or `argus-lasso record --seconds 60`.

Recent recordings show executable name, local date/time, duration and completeness.
Choose **Compare latest two**, or expand recordings and use **Use as A** / **Use as B**.
The comparison shows average FPS, 1% low FPS, p99 frametime and percentage change
from A to B. The graph retains the peak interval in each time bin; summary metrics
use the original full capture. Compare the same scene and settings, and check
completeness and duration before drawing conclusions. File scanning and bounded
CSV loading run on workers so a large recording does not block the UI.

**Gaming → Sensors** explains which fields the optional root reader can provide.
The desktop app and game layer always run as the regular user. See
[sources, units and access](sensors.md); root cannot invent unsupported sensors.
