# Screenshot gallery

Main pages captured from source commit `8ff0f9b` (installed build
`8ff0f9b1b6e7-1de5f47218b7`) in an isolated X11/Xvfb session on 2026-09-28,
using the read-only `--ui-tour` with software rendering, real host readings and
existing configuration. Shown selections are that configuration, not necessarily
the defaults. ProBalance and Gaming Mode are off in this capture. Sensor warmup
can leave early history empty. Numbers
are snapshots, not performance claims; the software-rendered capture itself adds
CPU load. Some pages scroll to additional controls.
The tour does not apply policies or populate an activity log with invented events.

Main-window captures are framebuffer images: compositor-applied desktop opacity
is not baked into them. The native customization image is retained from KDE Wayland on 2026-09-13
and shows the actual controls at 100% window opacity. This gallery covers all main pages
and Gaming/Settings subsections; it does not depict every transient context menu,
expanded sensor group or process-action confirmation.

## Overview

![Overview](../assets/screenshots/2026-09-28/overview.png)

## Processes

![Processes](../assets/screenshots/2026-09-28/processes.png)

## Process rules

![Process rules](../assets/screenshots/2026-09-28/rules.png)

## ProBalance

![ProBalance](../assets/screenshots/2026-09-28/probalance.png)

## Gaming — CPU & performance

![Gaming — CPU & performance](../assets/screenshots/2026-09-28/gaming-mode.png)

## Gaming — Launcher & profiles

![Gaming — Launcher & profiles](../assets/screenshots/2026-09-28/gaminglauncher.png)

## Gaming — Overlay

![Gaming — Overlay](../assets/screenshots/2026-09-28/gamingoverlay.png)

## Gaming — Recording

![Gaming — Recording](../assets/screenshots/2026-09-28/gamingrecording.png)

## Gaming — Sensors

![Gaming — Sensors](../assets/screenshots/2026-09-28/gamingsensors.png)

## Settings — Appearance

![Settings — Appearance](../assets/screenshots/2026-09-28/settings.png)

## Settings — Processes

![Settings — Processes](../assets/screenshots/2026-09-28/settingsprocesses.png)

## Settings — CPU power

![Settings — CPU power](../assets/screenshots/2026-09-28/settingspower.png)

## Settings — Notifications

![Settings — Notifications](../assets/screenshots/2026-09-28/settingsnotifications.png)

## Settings — Startup & updates

![Settings — Startup & updates](../assets/screenshots/2026-09-28/settingsstartup.png)

## Tools — Hardware sensors

![Tools — Hardware sensors](../assets/screenshots/2026-09-28/hw-monitor.png)

## Tools — Memory benchmarks

![Tools — Memory benchmarks](../assets/screenshots/2026-09-28/benchmark.png)

## Tools — Activity log

![Tools — Activity log](../assets/screenshots/2026-09-28/log.png)

## Overlay customization — separate window

Per-value colors, independent background opacity, actual-pixel text sizing and
expandable CPU, RAM, frame-time and Argus sections. Expand each section to choose
its individual fields and colors.

![Native overlay customization](../assets/screenshots/2026-09-13/overlay-settings.png)

## Light-theme overlay controls

Captured 2026-09-19 with the isolated preview’s `--light --embedded` mode.
This verifies text hierarchy and contrast; the embedded window does not test
native window decorations or compositor transparency.

![Light-theme overlay controls](../assets/screenshots/2026-09-19/overlay-settings-light.png)


## Reliability and comparison update

Captured on 2026-09-28 during development of source change `75a44ea`. These replace the
corresponding earlier views for the changed controls; the other screenshots above
retain their original source attribution. The main-window images use the read-only
X11/Xvfb tour. The comparison image uses the real recording UI in its isolated
preview with **synthetic demo recordings**, not measured game-performance results.

| Process history and filters | Rule effects entry point |
|---|---|
| ![Processes](../assets/screenshots/2026-09-28-reliability/processes.png) | ![Rules](../assets/screenshots/2026-09-28-reliability/rules.png) |
| **Readable recording history** | **Matched updates and rollback** |
| ![Recording](../assets/screenshots/2026-09-28-reliability/recording.png) | ![Updates](../assets/screenshots/2026-09-28-reliability/updates.png) |

![Recording comparison with synthetic demo data](../assets/screenshots/2026-09-28-reliability/comparison-demo.png)
