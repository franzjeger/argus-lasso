# ProBalance cgroup backend

This describes the implementation in `src/probalance.rs`, `src/cgroup.rs` and
`src/config.rs`, reviewed on 2026-09-30. The cgroup backend is implemented and
opt-in. The default remains **`nice`**. A future change to `auto` is not shipped
or scheduled; it needs broader desktop validation.

[The user guide](user-guide.md#probalance) defines the current system-load
activation, process eligibility, recovery windows and exemptions. Those policies
apply before choosing a backend.

## Configuration

The defaults under `[probalance]` are:

```toml
method = "nice"              # "nice" | "cgroup" | "auto"
cgroup_throttle_weight = 25
cgroup_quota_percent = 0     # no additional quota when applying the throttle
```

| Method | Behavior |
|---|---|
| `nice` | Adjust each eligible process's nice value |
| `cgroup` | Adjust its systemd user unit; skip and log unavailable targets |
| `auto` | Try the cgroup backend, falling back to nice on unavailable/failed targets |

The ProBalance page exposes the method, weight and quota controls and identifies
unit-based interventions. CPUWeight changes scheduling share under contention;
it is not a CPU-percentage cap. Optional CPUQuota uses a per-core scale, unlike
the whole-system CPU percentages shown in Argus's process table.

## Unit selection and application

The backend reads `/proc/<pid>/cgroup` for the cgroup v2 `0::` entry. It accepts a
leaf `.scope` or `.service` in the user manager's `app.slice` or `background.slice`.
It rejects `session.slice`, which holds the desktop itself (compositor, D-Bus,
audio), direct children of `user@.service`, non-unit leaves and paths outside a
user manager. Before throttling, the unit's `ControlGroup` must be exactly the
process's cgroup: a process inside a container's own systemd has unit names that
also exist on the host. There is no persistent PID-to-unit cache.

Argus invokes `systemctl --user set-property --runtime` with `CPUWeight` and,
when configured, `CPUQuota`. It does not create cgroups or move processes between
them. Changes affect the **whole unit**, including other processes in it.
Units containing a protected process are excluded; a launcher and its game may
share a unit, so this protection matters beyond the individual game's PID.
A throttle never gives a unit more CPU: a weight already at or below the
configured one (including `idle`) and a quota already at or below the configured
one are left as they are. `systemctl` calls time out after 5 seconds.

Several throttled PIDs in one unit share a reference-counted intervention.
The original values are recorded once. The last departing/recovered PID
triggers restoration. Failed unit restores remain tracked for retries during
later monitor ticks; a unit systemd has removed (its application closed) is
dropped instead. An application's departure or configuration change does
not turn the recorded original weight into the throttled weight.

Throttled units and their original values are also recorded, before the change
is made, in `$XDG_RUNTIME_DIR/argus-lasso/cgroup-throttles.json`. A throttle
outlives Argus being killed or crashing (systemd keeps runtime properties until
logout); the next run takes the recorded units over and restores them on its
first tick, instead of reading the throttled values as the units' own.

Failed throttle targets are remembered to avoid repeating failed commands every
tick. Configuration updates clear failure sets. Failed-PID log suppression is
pruned as processes exit; failed unit names otherwise remain cached for the
monitor session.

## Restoration limits

The recorded CPUWeight is restored, including its unset default and `idle`. If
Argus will change CPUQuota, it first reads the unit's `CPUQuotaPerSecUSec` from
systemd. (The cgroup's `cpu.max` exists only where the cpu controller is enabled
for the parent slice, which on most desktops it is not.) Restoration reinstates
that limit, or unlimited when it was unlimited. Weight-only
interventions do not write CPUQuota. systemctl accepts two decimal places for quota
percentages; a limit that cannot be expressed exactly at that precision is refused
rather than rounded. A failed original-policy read also prevents cgroup throttling.
Nice restoration and cgroup restoration use the mechanism recorded for the
intervention, even after the selected method changes.

The default `nice` backend avoids depending on systemd cgroup control. Neither
backend guarantees improved game FPS. Failed cleanup can be reported in logs;
shutdown is not proof that every external scheduling change succeeded.

## Validation status and remaining work

Unit tests cover timespan and quota conversion, never-raising throttle plans, the
throttle journal, cgroup path classification and the ProBalance policy/protected
process behavior. The current workspace tests pass locally; this does not
establish actual CPUWeight enforcement across desktop environments.

Before changing the default, validate on real KDE/GNOME systems:

1. Applying and restoring unit properties, including permissions and failures.
2. Throughput under controlled contention, with observed `cpu.weight` values.
3. Shared-unit behavior for Steam/Proton, Flatpak and Snap applications.
4. Controller availability and `auto` fallback across supported distributions.
5. Pre-existing resource policy across distributions and non-default CPU periods.

The older proposal considered creating delegated cgroups or writing system
cgroups through a root helper. Neither mechanism is implemented here; the
current backend uses the existing systemd user unit.

An opt-in integration test creates only its own short-lived user unit, verifies a
37% quota and weight 123, throttles and restores both, checks that another
process's cgroup is refused, and that restoring a removed unit reports it gone:

```bash
cargo test --bin argus-lasso real_user_unit_restores_existing_quota_and_weight -- --ignored
```
