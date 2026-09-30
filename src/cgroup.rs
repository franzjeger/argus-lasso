//! cgroup v2 per-unit throttling via `systemctl --user set-property`.
//!
//! Design: docs/design-cgroup-probalance.md. The sanctioned, rootless way to
//! throttle a desktop app is to set CPUWeight/CPUQuota on the systemd *unit*
//! (app scope) the process lives in — never to migrate PIDs between cgroups
//! behind systemd's back.

use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Resolve the throttleable systemd user unit for a PID, if any.
///
/// Returns None for processes outside the user manager's subtree (system
/// services, kernel threads), for anything outside the app and background
/// slices, and for processes directly under `user@.service` — throttling
/// those would hit far more than one app.
pub fn unit_for_pid(pid: u32) -> Option<String> {
    unit_from_cgroup_path(&cgroup_of(pid)?)
}

fn cgroup_of(pid: u32) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    Some(
        text.lines()
            .find_map(|l| l.strip_prefix("0::"))?
            .trim()
            .to_string(),
    )
}

/// Pure classification of a cgroup v2 path (the `0::` line without prefix).
fn unit_from_cgroup_path(path: &str) -> Option<String> {
    // Must be inside a user manager subtree: .../user@<uid>.service/...
    let (_, after_user) = path.split_once("/user@")?;
    let comps: Vec<&str> = after_user.split('/').filter(|c| !c.is_empty()).collect();
    // comps[0] = "<uid>.service". Only applications and background work are
    // candidates (app.slice, background.slice): session.slice holds the
    // desktop itself — compositor, D-Bus, audio — and throttling it would
    // slow everything down.
    if !matches!(comps.get(1), Some(&("app.slice" | "background.slice"))) || comps.len() < 3 {
        return None;
    }
    let unit = *comps.last()?;
    if !(unit.ends_with(".scope") || unit.ends_with(".service")) {
        return None;
    }
    Some(unit.to_string())
}

/// Longest a `systemctl --user` call may take. Throttling happens under the
/// load it is meant to relieve, and a stalled user manager must not hold up
/// the monitor thread, or the restores at exit.
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(5);

fn systemctl_user(args: &[&str]) -> Option<std::process::Output> {
    use std::process::Stdio;
    let mut child = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + SYSTEMCTL_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                log::warn!("systemctl --user {args:?} timed out");
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// A unit's CPUWeight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Weight {
    /// Not set: systemd's default, 100.
    Default,
    /// `CPUWeight=idle`: runs only when nothing else wants the CPU.
    Idle,
    Value(u64),
}

impl Weight {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "[not set]" | "infinity" | "18446744073709551615" => Some(Self::Default),
            // Older systemd prints idle as its value, 0.
            "idle" | "0" => Some(Self::Idle),
            n => n.parse().ok().map(Self::Value),
        }
    }

    /// Its share of the CPU relative to other units.
    fn share(self) -> u64 {
        match self {
            Self::Default => 100,
            Self::Idle => 0,
            Self::Value(w) => w,
        }
    }

    fn assignment(self) -> String {
        match self {
            Self::Default => "CPUWeight=".into(),
            Self::Idle => "CPUWeight=idle".into(),
            Self::Value(w) => format!("CPUWeight={w}"),
        }
    }
}

/// A unit's CPUQuota.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quota {
    Unlimited,
    /// Hundredths of a percent of one CPU, the finest systemctl accepts.
    Hundredths(u64),
}

impl Quota {
    /// From `CPUQuotaPerSecUSec`: CPU time allowed per second of wall time.
    fn from_usec_per_sec(usec: Option<u64>) -> Option<Self> {
        match usec {
            None => Some(Self::Unlimited),
            // 0.01% is 100 µs per second. Refuse what cannot be put back
            // exactly rather than round away a user's limit.
            Some(usec) if usec > 0 && usec % 100 == 0 => Some(Self::Hundredths(usec / 100)),
            Some(_) => None,
        }
    }

    fn assignment(self) -> String {
        match self {
            Self::Unlimited => "CPUQuota=".into(),
            Self::Hundredths(h) => format!("CPUQuota={}.{:02}%", h / 100, h % 100),
        }
    }
}

/// CPU settings of a unit. A field is None when it was not read, or, for a
/// throttle or its undo, is not to be changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuPolicy {
    pub weight: Option<Weight>,
    pub quota: Option<Quota>,
}

impl CpuPolicy {
    fn assignments(&self) -> Vec<String> {
        self.weight
            .map(Weight::assignment)
            .into_iter()
            .chain(self.quota.map(Quota::assignment))
            .collect()
    }
}

impl std::fmt::Display for CpuPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.assignments().join(" ") {
            none if none.is_empty() => f.write_str("no change"),
            some => f.write_str(&some),
        }
    }
}

/// A systemd timespan as `systemctl show` prints it ("370ms", "1.500000s",
/// "1min 30s") in microseconds; None for "infinity".
fn timespan_usec(text: &str) -> Option<Option<u64>> {
    if text == "infinity" {
        return Some(None);
    }
    let mut total: u64 = 0;
    for part in text.split_whitespace() {
        let split = part.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
        let (number, unit) = part.split_at(split);
        let scale: u64 = match unit {
            "us" | "µs" => 1,
            "ms" => 1_000,
            "s" => 1_000_000,
            "min" => 60_000_000,
            "h" => 3_600_000_000,
            _ => return None,
        };
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        let mut usec = whole.parse::<u64>().ok()?.checked_mul(scale)?;
        let mut place = scale;
        for digit in fraction.chars() {
            place /= 10;
            usec = usec.checked_add(u64::from(digit.to_digit(10)?) * place)?;
        }
        total = total.checked_add(usec)?;
    }
    Some(Some(total))
}

/// The unit's current CPU settings, or None — refusing to throttle — when
/// they cannot be read reliably, or when the unit does not own `pid`'s
/// cgroup (a process in a container's own systemd has a unit name that
/// also exists on the host).
pub fn read_unit_cpu_policy(unit: &str, pid: u32, with_quota: bool) -> Option<CpuPolicy> {
    let out = systemctl_user(&[
        "show",
        "-p",
        "CPUWeight",
        "-p",
        "CPUQuotaPerSecUSec",
        "-p",
        "ControlGroup",
        "--",
        unit,
    ])?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let value = |key: &str| text.lines().find_map(|l| l.strip_prefix(key));
    if value("ControlGroup=")? != cgroup_of(pid)? {
        return None;
    }
    let weight = Weight::parse(value("CPUWeight=")?)?;
    // systemd's own record: cpu.max only exists where the cpu controller is
    // enabled for the parent slice, which on most desktops it is not.
    let quota = if with_quota {
        Some(Quota::from_usec_per_sec(timespan_usec(value(
            "CPUQuotaPerSecUSec=",
        )?)?)?)
    } else {
        None
    };
    Some(CpuPolicy {
        weight: Some(weight),
        quota,
    })
}

/// What to set to throttle a unit whose settings are `current`, and the
/// original values of what that changes. A setting already at or below the
/// throttle is left alone: throttling must never give a unit more CPU (an
/// idle-weight build would be raised to 25, a 20% cap to 100%).
pub fn plan_throttle(
    current: &CpuPolicy,
    weight: u32,
    quota_percent: u32,
) -> (CpuPolicy, CpuPolicy) {
    let mut set = CpuPolicy::default();
    let mut original = CpuPolicy::default();
    if let Some(w) = current.weight.filter(|w| w.share() > u64::from(weight)) {
        set.weight = Some(Weight::Value(u64::from(weight)));
        original.weight = Some(w);
    }
    let cap = u64::from(quota_percent) * 100;
    if let Some(q) = current
        .quota
        .filter(|q| quota_percent > 0 && !matches!(q, Quota::Hundredths(h) if *h <= cap))
    {
        set.quota = Some(Quota::Hundredths(cap));
        original.quota = Some(q);
    }
    (set, original)
}

/// Set `policy` on a unit; nothing to set succeeds without a call.
/// `--runtime` scopes the change to this boot — exactly the lifetime we want.
pub fn throttle_unit(unit: &str, policy: &CpuPolicy) -> bool {
    set_property(unit, policy)
}

fn set_property(unit: &str, policy: &CpuPolicy) -> bool {
    let assignments = policy.assignments();
    if assignments.is_empty() {
        return true;
    }
    let mut args = vec!["set-property", "--runtime", "--", unit];
    args.extend(assignments.iter().map(String::as_str));
    systemctl_user(&args).is_some_and(|o| o.status.success())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restore {
    Done,
    /// The unit no longer exists (its app closed): nothing is left to undo.
    Gone,
    Failed,
}

/// Put back the properties a throttle changed.
pub fn restore_unit(unit: &str, original: &CpuPolicy) -> Restore {
    if set_property(unit, original) {
        return Restore::Done;
    }
    let gone = systemctl_user(&["show", "-p", "LoadState", "--", unit])
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "LoadState=not-found");
    if gone {
        Restore::Gone
    } else {
        Restore::Failed
    }
}

/// Read a small JSON file this user wrote, refusing anything else there.
pub fn read_json_capped<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Option<T> {
    let bytes = argus_ipc::capture::read_regular_capped(path, 1 << 20).ok()??;
    serde_json::from_slice(&bytes).ok()
}

/// Replace `path` with `value` as JSON, readable by this user only; an
/// empty map removes the file.
pub fn write_json_private<K, V>(
    path: &std::path::Path,
    value: &std::collections::HashMap<K, V>,
) -> std::io::Result<()>
where
    K: Serialize + Eq + std::hash::Hash,
    V: Serialize,
{
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if value.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    let temp = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    serde_json::to_writer(&mut file, value)?;
    file.sync_all()?;
    std::fs::rename(temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timespans_as_systemctl_prints_them() {
        assert_eq!(timespan_usec("370ms"), Some(Some(370_000)));
        assert_eq!(timespan_usec("1.500000s"), Some(Some(1_500_000)));
        assert_eq!(timespan_usec("12.500ms"), Some(Some(12_500)));
        assert_eq!(timespan_usec("1min 30s"), Some(Some(90_000_000)));
        assert_eq!(timespan_usec("infinity"), Some(None));
        assert_eq!(timespan_usec("garbage"), None);
        assert_eq!(timespan_usec("5x"), None);
    }

    #[test]
    fn quotas_are_kept_exactly_or_refused() {
        let quota = |usec| Quota::from_usec_per_sec(usec).map(Quota::assignment);
        assert_eq!(quota(Some(370_000)).as_deref(), Some("CPUQuota=37.00%"));
        assert_eq!(quota(Some(1_500_000)).as_deref(), Some("CPUQuota=150.00%"));
        assert_eq!(quota(Some(12_500)).as_deref(), Some("CPUQuota=1.25%"));
        assert_eq!(quota(None).as_deref(), Some("CPUQuota="));
        assert_eq!(quota(Some(12_345)), None, "not representable");
        assert_eq!(quota(Some(0)), None);
    }

    #[test]
    fn weights_as_systemctl_prints_them() {
        assert_eq!(Weight::parse("123"), Some(Weight::Value(123)));
        assert_eq!(Weight::parse("idle"), Some(Weight::Idle));
        assert_eq!(Weight::parse("[not set]"), Some(Weight::Default));
        assert_eq!(Weight::parse("lots"), None);
    }

    /// Throttling used to set the configured weight and quota whatever the
    /// unit had, raising an idle-weight build to 25 and a 20% cap to 100%.
    #[test]
    fn a_throttle_never_gives_a_unit_more_cpu() {
        let current = |weight, quota| CpuPolicy {
            weight: Some(weight),
            quota: Some(quota),
        };
        let (set, original) = plan_throttle(&current(Weight::Default, Quota::Unlimited), 25, 50);
        assert_eq!(set.to_string(), "CPUWeight=25 CPUQuota=50.00%");
        assert_eq!(original.to_string(), "CPUWeight= CPUQuota=");

        let (set, original) =
            plan_throttle(&current(Weight::Idle, Quota::Hundredths(2000)), 25, 100);
        assert_eq!(set, CpuPolicy::default());
        assert_eq!(original, CpuPolicy::default());
        assert_eq!(set.to_string(), "no change");

        let (set, _) = plan_throttle(
            &current(Weight::Value(10), Quota::Hundredths(20_000)),
            25,
            100,
        );
        assert_eq!(set.to_string(), "CPUQuota=100.00%");

        // No quota configured: the quota is left as it is.
        let (set, original) = plan_throttle(&current(Weight::Value(200), Quota::Unlimited), 25, 0);
        assert_eq!(set.to_string(), "CPUWeight=25");
        assert_eq!(original.to_string(), "CPUWeight=200");
    }

    #[test]
    #[ignore = "requires a running systemd user manager"]
    fn real_user_unit_restores_existing_quota_and_weight() {
        let unit = format!("argus-policy-test-{}.service", uuid::Uuid::new_v4());
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = systemctl_user(&["stop", "--", &self.0]);
            }
        }
        let _cleanup = Cleanup(unit.clone());
        assert!(Command::new("systemd-run")
            .args([
                "--user",
                "--quiet",
                "--collect",
                "--unit",
                &unit,
                "--property=CPUQuota=37%",
                "--property=CPUWeight=123",
                "sleep",
                "30"
            ])
            .status()
            .unwrap()
            .success());
        let main_pid = systemctl_user(&["show", "-p", "MainPID", "--value", "--", &unit]).unwrap();
        let pid: u32 = String::from_utf8_lossy(&main_pid.stdout)
            .trim()
            .parse()
            .unwrap();
        let current = read_unit_cpu_policy(&unit, pid, true).expect("original CPU policy");
        assert_eq!(current.weight, Some(Weight::Value(123)));
        assert_eq!(current.quota, Some(Quota::Hundredths(3700)));

        let (set, original) = plan_throttle(&current, 25, 10);
        assert!(throttle_unit(&unit, &set));
        assert_eq!(read_unit_cpu_policy(&unit, pid, true), Some(set));
        assert_eq!(restore_unit(&unit, &original), Restore::Done);
        assert_eq!(read_unit_cpu_policy(&unit, pid, true), Some(current));

        // Another process's unit does not own this pid's cgroup.
        assert_eq!(read_unit_cpu_policy(&unit, std::process::id(), true), None);

        drop(_cleanup);
        assert_eq!(restore_unit(&unit, &original), Restore::Gone);
    }

    #[test]
    fn app_and_background_units_are_throttleable() {
        assert_eq!(
            unit_from_cgroup_path(
                "/user.slice/user-1000.slice/user@1000.service/app.slice/app-firefox-1234.scope"
            ),
            Some("app-firefox-1234.scope".into())
        );
        assert_eq!(
            unit_from_cgroup_path(
                "/user.slice/user-1000.slice/user@1000.service/background.slice/foo.service"
            ),
            Some("foo.service".into())
        );
    }

    #[test]
    fn the_desktop_itself_and_the_user_manager_are_rejected() {
        // The compositor, D-Bus and audio live in session.slice.
        assert_eq!(
            unit_from_cgroup_path(
                "/user.slice/user-1000.slice/user@1000.service/session.slice/org.gnome.Shell@wayland.service"
            ),
            None
        );
        // Directly under user@ (no intermediate slice)
        assert_eq!(
            unit_from_cgroup_path("/user.slice/user-1000.slice/user@1000.service/init.scope"),
            None
        );
    }

    #[test]
    fn system_units_and_bare_paths_are_rejected() {
        assert_eq!(unit_from_cgroup_path("/system.slice/sshd.service"), None);
        assert_eq!(unit_from_cgroup_path("/"), None);
        // Slice (not scope/service) as leaf
        assert_eq!(
            unit_from_cgroup_path("/user.slice/user-1000.slice/user@1000.service/app.slice"),
            None
        );
    }
}
