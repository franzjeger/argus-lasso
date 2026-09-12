# Using Argus-Lasso

This guide describes the current source tree. The UI uses English labels.
[Install](installation.md) · [Screenshot gallery](screenshots.md)

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

## Process table

Right-click a process for actions; double-click for details. Process identity is
shown once at the top of the context menu. Priority, CPU assignment and rule
creation are grouped separately from termination. The GUI Delete action offers
an undo window; a CLI kill does not.

Numeric columns align right. Font-aware minimum widths keep long PIDs and
multicore CPU readings inside their cells, including when reusing older saved
widths. Narrow windows offer horizontal scrolling. Hover a truncated name for
its command line and additional information. Column headers sort; their context
menu or **Columns** chooses visible fields.

**CPU% uses a per-process, single-logical-CPU scale:** 100% means one logical CPU
fully busy, 200% means two. On 32 logical CPUs a process can reach 3200%.
The overall CPU average uses 0–100% across the machine. These scales differ on
purpose, but must not be compared as if they were the same quantity.

## ProBalance

The current algorithm compares each process's CPU% against its configured
threshold. The default 85% is **0.85 logical CPUs**, not 85% of the whole machine.
The threshold controls accept values above 100%, up to the host's logical CPU
capacity (and preserve larger existing settings).

After sustained high usage, Argus lowers priority using nice, or the optional
cgroup CPUWeight method. A low-usage recovery window restores it. Nice and
CPUWeight affect competition for CPU time; they are not absolute CPU usage caps.
The cgroup configuration also offers a separate quota control.

**Current limitation:** decisions are not gated by overall CPU contention.
A process can be reprioritized while other CPUs are idle. This is a simple
per-process policy, not a verified implementation of another product's
responsiveness algorithm. Exemptions and existing user thresholds remain under
user control. [Cgroup design and validation status](design-cgroup-probalance.md).

## Gaming, telemetry and windows

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

**Gaming → Sensors** explains which fields the optional root reader can provide.
The desktop app and game layer always run as the regular user. See
[sources, units and access](sensors.md); root cannot invent unsupported sensors.
