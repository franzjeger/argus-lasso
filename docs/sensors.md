# Sensors, units and access

Telemetry is sampled approximately once per second. HUD text is refreshed on a
separate cadence; drawing and frame timing do not run sensor collection.
Missing measurements are represented as unavailable, not manufactured zeroes.
Connection failures replace hardware data with a status message; a real 0% load
or stopped 0% fan remains a valid value.

## Data sources

| Reading | Source and meaning |
|---|---|
| GPU load, temperature, board power | NVIDIA NVML on tested hardware; %, °C, W |
| GPU core/memory clocks, fan | NVML; MHz and fan duty %. Fan duty is not RPM |
| VRAM used/total | NVML memory information; displayed in GiB |
| CPU overall/logical-thread load | Deltas of `/proc/stat` busy/total counters; each reading 0–100% |
| CPU temperature | Available hwmon temperature channels (for example k10temp/coretemp); °C |
| CPU clock | cpufreq `scaling_cur_freq`, converted from kHz to MHz; overall field currently uses CPU 0, not a package average |
| Per-thread clock and identity | cpufreq plus Linux CPU topology; MHz, logical CPU ID, physical core/package identity |
| CPU package power | RAPL package energy delta / actual elapsed time; W, never TDP |
| RAM used/total | `/proc/meminfo`; used = total − available, displayed in GiB |
| Configured RAM speed | SMBIOS Type 17 configured speed, MT/s; not the DIMM's advertised maximum |
| Process CPU% | Process CPU-time deltas on the 100% = one logical CPU scale |
| Parking / Argus mode | Observed CPU online state and Argus daemon mode |
| Launcher/profile context | Tracked game PID and start time, active launch profile; main-thread affinity/nice and ProBalance state where available |

The GPU usage accessor accepts both `GPU Load` and `Usage`; the NVIDIA collector
currently emits `GPU Load`. NVML failures remain unavailable. Other hwmon/storage
sensors may appear in Tools without having a corresponding HUD field.

## Why CPU power and RAM speed can need permission

Access is determined per kernel interface, not per application's feature list.
On the development system, package RAPL `energy_uj` and firmware DMI tables were
root-readable only. Other temperatures, frequencies and NVML values were readable
by the user. The helper successfully read measured CPU power and configured
8000 MT/s RAM speed on that Ryzen 9 9950X3D host. This is evidence for that host,
not a promise that every motherboard exports the same data.

Argus first uses available unprivileged CPU power sources. It does not replace a
missing measurement with a TDP estimate. Permission denied, missing interfaces
and stale helper data have distinct status descriptions in Gaming → Sensors.
Some older sensor collectors expose only generic unavailable readings; the HUD
does not classify every vendor-specific failure into a separate error badge.

## Optional extended sensor access

Install once from the checkout:

```bash
./scripts/install-sensors.sh
```

Then use **Gaming → Sensors** to enable or disable the service with normal system
authentication. Starting it does not enable it at boot. It runs independently of
the single user Argus process; it never launches a root GUI or Vulkan layer.

The fixed `/usr/local/libexec/argus-sensors` program reads only the supported RAPL
package counters and DMI table. It accepts no arbitrary paths, commands or
privileged subprocess operations. DMI is cached at service startup. Running power
measurements use one long-lived service, not repeated authentication or one
privileged process per sample. The sandboxed `argus-sensors.service` has an empty
capability set and a fixed writable runtime directory.

It atomically publishes a small, versioned JSON snapshot at
`/run/argus-sensors/telemetry.json`. The user consumer rejects a wrong schema or
samples older than 3.5 seconds. The output includes source status and measurement
interval. Disabling the service makes its fields unavailable again when no
unprivileged source exists. Configured RAM speed is static firmware information,
not a live memory-clock probe; mixed configured DIMM speeds are not silently
reported as a single supported value.

Remaining limits: hardware without supported RAPL/DMI data, per-core power,
additional vendor-private sensors, and universal permission/error classification.
Optional disk/network/swap/process-memory HUD fields are not implemented.
