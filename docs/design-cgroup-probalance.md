# Design: cgroup v2 backend for ProBalance

Status: **rollout steps 1+2 implemented** (config keys + cgroup method behind
`method = "cgroup"`/`"auto"`; default remains `"nice"`). Step 3 — flipping the
default to `auto` — is blocked on the validation checklist below, which needs
real desktop hardware.

The activation policy was updated on 2026-09-13: overall system load gates
per-process candidacy, and protected processes also protect their cgroup unit.
See [current defaults and CPU units](user-guide.md#probalance). This document
describes the backend and its remaining validation work.

## Problem

ProBalance currently throttles CPU hogs by raising their nice value. Nice is weak medicine:

- It only matters when the CPU is *contended*, and even then a nice+5 process still gets a
  large share (nice is ~1.25× weight per step, not a cap).
- It does nothing against a process with many threads spread over idle cores.
- Restoring is fragile: if the app itself calls `setpriority`, our bookkeeping diverges.

cgroup v2 offers two real knobs:

| Knob | Semantics | Fit for ProBalance |
|---|---|---|
| `cpu.weight` (1–10000, default 100) | Proportional share under contention | Direct analog of nice, but much stronger range |
| `cpu.max` ("MAX 100000" / "$QUOTA $PERIOD") | Hard bandwidth cap even on idle system | Optional "hard throttle" mode |

## The constraint that shapes everything: systemd owns the tree

On every mainstream desktop distro, systemd is the **single writer** of the cgroup v2
hierarchy. Migrating PIDs between cgroups behind systemd's back (writing `cgroup.procs`
in directories we create ourselves outside a delegated subtree) is explicitly
unsupported: systemd may re-migrate processes, unit tracking breaks, and behavior
after `daemon-reload` is undefined.

The *sanctioned* mechanisms are:

1. **Per-unit properties.** Every desktop process already lives in a systemd unit —
   on GNOME/KDE each launched app gets its own `app-….scope` under
   `user.slice/user-$UID.slice/user@$UID.service/app.slice/`. Setting
   `CPUWeight=`/`CPUQuota=` on that unit via `systemctl --user set-property --runtime`
   (or the equivalent D-Bus call) requires **no root** for user units and is fully
   supported.
2. **Delegated subtrees.** `user@.service` is delegated; a user process may create
   sub-cgroups *inside its own delegation* and move *its own* processes there. Since
   systemd 252 the `cpu` controller is delegated to user slices by default (earlier
   versions delegated only `memory`+`pids`, needing a drop-in).

## Considered options

### A. Per-unit throttle via `systemctl --user set-property` — **recommended**

Resolve the hog PID's unit from `/proc/<pid>/cgroup` (the `0::/…` line; the unit is the
last `.scope`/`.service` path component). If it's a *user* unit, apply:

```
systemctl --user set-property --runtime <unit> CPUWeight=<throttle_weight>
```

Restore by setting `CPUWeight=100` (the default) — or the recorded original if the unit
had a non-default weight (read once via `systemctl --user show -p CPUWeight <unit>`).

- ✅ No root, no helper, sanctioned, survives systemd reloads (`--runtime` clears on reboot,
  which is exactly the lifetime we want).
- ✅ Optional hard mode: also set `CPUQuota=<n>%` (maps to `cpu.max`).
- ⚠️ Granularity is the **unit**, not the PID. On modern DEs that's one app per scope,
  which is usually what the user actually wants ("throttle Chrome", not "throttle one
  renderer"). Multiple hog PIDs in one unit collapse into one throttle entry (refcount).
- ⚠️ Processes in `user@.service` itself, in system units, or spawned outside any app
  scope (bare terminals on some setups) are not per-app isolated → fall back to nice.

### B. Own delegated sub-cgroup (`…/user@$UID.service/argus.slice/throttled/`)

Create a throttle cgroup inside the user delegation, enable `cpu` in
`cgroup.subtree_control`, move hog PIDs' `cgroup.procs` there.

- ✅ True per-PID granularity, no subprocess spawns.
- ❌ Moving a PID *out of its app scope* still breaks systemd's cgroup-based unit
  tracking for that process (stop/kill of the scope no longer reaches it; session teardown
  can miss it). This is the anti-pattern in different clothing.
- ❌ Restore requires remembering the *source* cgroup and moving back — racy if the scope
  died meanwhile.
- Rejected as default; not worth it even as an option.

### C. Root helper writes `cpu.weight` directly into the PID's existing cgroup

No migration: write `cpu.weight` in the cgroup the PID already occupies (its app scope),
via the privileged helper.

- ✅ Per-scope like A, works even for non-user units.
- ❌ Requires the root helper for something A does without privileges.
- ❌ systemd overwrites the value whenever it re-applies unit properties (daemon-reload,
  property change) — silent un-throttle.
- Rejected; A is strictly better where it applies.

## Recommended design

**Method selection** (new config `probalance.method`, default `auto`):

- `auto`: try cgroup (option A); per-process fallback to nice when the PID's unit is not
  a throttleable user unit. Detection cached per unit.
- `cgroup`: A only; log when a process can't be throttled.
- `nice`: today's behavior (also the automatic fallback when there is no systemd user
  manager, e.g. non-systemd distros, containers, hybrid v1 hierarchies).

**New config keys** (`[probalance]`, serde defaults keep old configs valid):

```toml
method = "auto"            # "auto" | "cgroup" | "nice"
cgroup_throttle_weight = 25  # cpu.weight while throttled (default 100 ⇒ 4× less share)
cgroup_quota_percent = 0     # 0 = no hard cap; e.g. 50 ⇒ CPUQuota=50% (per-core scale)
```

**State machine:** unchanged (`decide()` stays pure). The applied action is recorded per
entry as `ThrottleApplied::{Nice{original}, Unit{unit, original_weight}}` so restore uses
the right mechanism even if config changes mid-throttle. Unit entries are refcounted:
N hog PIDs in one scope → one `set-property`, restored when the last PID calms down or
dies. All existing restore paths (exempt-while-throttled, disable, shutdown) go through
the same `ThrottleApplied` restore.

**Unit resolution:** parse `/proc/<pid>/cgroup` `0::` line; accept units matching
`app-*.scope`, `*.scope` under `app.slice`, and user `*.service` under `app.slice`;
reject `session-*.scope`, anything directly under `user@.service`, and system-manager
paths (no `user@` component). Cache per PID (cgroup membership of a PID rarely changes).

**Execution:** `systemctl --user …` subprocess, same pattern as the existing
`renice`/`ionice` calls — no new dependencies (zbus D-Bus can replace it later without
design changes). Failures blacklist the unit for the process lifetime (same backoff
philosophy as `enforce_nice_failed`).

**UI:** ProBalance tab gets a method dropdown + weight/quota fields; the throttle-info
table shows `unit` and `weight` instead of nice for cgroup entries.

## Validation plan (needs real hardware — blocked in CI/containers)

The build container has a hybrid v1/v2 hierarchy and no systemd user session, so the
following must be checked manually on a target machine (KDE and GNOME, systemd ≥ 252):

1. `systemctl --user set-property --runtime app-….scope CPUWeight=25` succeeds without
   auth prompt and `cat …/cpu.weight` reflects it.
2. Weight is visible in throughput: a spin loop in a throttled scope vs an unthrottled
   one under full contention shows ≈ weight ratio.
3. Restore via `CPUWeight=100` returns `cpu.weight` to 100.
4. Behavior on systemd < 252 (no cpu delegation): set-property on a user scope still
   works (systemd system manager applies it) — verify, else document fallback.
5. Flatpak/Snap apps (their own scopes) and Proton games (inside Steam's scope —
   granularity check: throttling the game throttles all of Steam?).

Item 5 decides whether Gaming Mode's process set needs an exemption list at the unit level.

## Rollout

1. Land config keys + method plumbing with `nice` as the only implemented method
   (no behavior change).
2. Land option A behind `method = "cgroup"`/`auto` with the per-unit cache, refcounts,
   and fallback.
3. Manual validation pass (above); flip default to `auto` in the following release.
