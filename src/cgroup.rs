//! cgroup v2 per-unit throttling via `systemctl --user set-property`.
//!
//! Design: docs/design-cgroup-probalance.md. The sanctioned, rootless way to
//! throttle a desktop app is to set CPUWeight/CPUQuota on the systemd *unit*
//! (app scope) the process lives in — never to migrate PIDs between cgroups
//! behind systemd's back.

use std::process::Command;

/// Resolve the throttleable systemd user unit for a PID, if any.
///
/// Returns None for processes outside the user manager's subtree (system
/// services, kernel threads), for session scopes, and for processes directly
/// under `user@.service` — throttling those would hit far more than one app.
pub fn unit_for_pid(pid: u32) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let path = text.lines().find_map(|l| l.strip_prefix("0::"))?.trim();
    unit_from_cgroup_path(path)
}

/// Pure classification of a cgroup v2 path (the `0::` line without prefix).
fn unit_from_cgroup_path(path: &str) -> Option<String> {
    // Must be inside a user manager subtree: .../user@<uid>.service/...
    let (_, after_user) = path.split_once("/user@")?;
    let comps: Vec<&str> = after_user.split('/').filter(|c| !c.is_empty()).collect();
    // comps[0] = "<uid>.service"; require at least one slice level between the
    // user manager and the unit (e.g. app.slice/app-firefox-1234.scope) so we
    // never throttle user@.service itself or its direct children.
    if comps.len() < 3 {
        return None;
    }
    let unit = *comps.last()?;
    if !(unit.ends_with(".scope") || unit.ends_with(".service")) {
        return None;
    }
    // The login session scope contains the whole desktop — never throttle it.
    if unit.starts_with("session-") {
        return None;
    }
    Some(unit.to_string())
}

fn systemctl_user(args: &[&str]) -> Option<std::process::Output> {
    Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .ok()
}

#[derive(Debug, Clone, PartialEq)]
pub struct CpuPolicy {
    pub weight: Option<u64>,
    /// None means we did not change quota. Empty assignment restores unlimited.
    pub quota: Option<String>,
}

/// Refuse to throttle when original settings cannot be read reliably.
pub fn read_unit_cpu_policy(unit: &str, change_quota: bool) -> Option<CpuPolicy> {
    let out = systemctl_user(&["show", "-p", "CPUWeight", "-p", "ControlGroup", "--", unit])?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let value = |key: &str| text.lines().find_map(|l| l.strip_prefix(key));
    let weight = match value("CPUWeight=")? {
        "[not set]" | "infinity" | "18446744073709551615" => None,
        n => Some(n.parse().ok()?),
    };
    let quota = if change_quota {
        let group = value("ControlGroup=")?;
        if !group.starts_with("/user.slice/") || group.split('/').any(|c| c == "..") {
            return None;
        }
        let raw = std::fs::read_to_string(format!("/sys/fs/cgroup{group}/cpu.max")).ok()?;
        Some(quota_assignment(&raw)?)
    } else {
        None
    };
    Some(CpuPolicy { weight, quota })
}

fn quota_assignment(raw: &str) -> Option<String> {
    let mut parts = raw.split_whitespace();
    let quota = parts.next()?;
    let period: u64 = parts.next()?.parse().ok()?;
    if period == 0 || parts.next().is_some() {
        return None;
    }
    if quota == "max" {
        return Some(String::new());
    }
    let quota: u64 = quota.parse().ok()?;
    if quota == 0 {
        return None;
    }
    // systemctl accepts at most two decimal places. Refuse policies that
    // cannot be represented exactly instead of rounding away a user limit.
    let scaled = u128::from(quota) * 10_000;
    if !scaled.is_multiple_of(u128::from(period)) {
        return None;
    }
    let hundredths = scaled / u128::from(period);
    Some(format!("{}.{:02}%", hundredths / 100, hundredths % 100))
}

/// Apply a throttle to a unit. `quota_percent` 0 = no hard cap.
/// `--runtime` scopes the change to this boot — exactly the lifetime we want.
pub fn throttle_unit(unit: &str, weight: u32, quota_percent: u32) -> bool {
    let weight_prop = format!("CPUWeight={weight}");
    let mut args = vec![
        "set-property",
        "--runtime",
        "--",
        unit,
        weight_prop.as_str(),
    ];
    let quota_prop;
    if quota_percent > 0 {
        quota_prop = format!("CPUQuota={quota_percent}%");
        args.push(quota_prop.as_str());
    }
    systemctl_user(&args)
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Restore only the properties we changed, preserving pre-existing quotas.
pub fn restore_unit(unit: &str, original: &CpuPolicy) -> bool {
    let weight = original.weight.map_or_else(
        || "CPUWeight=".into(),
        |w| {
            if w == 0 {
                "CPUWeight=idle".into()
            } else {
                format!("CPUWeight={w}")
            }
        },
    );
    let quota = original.quota.as_ref().map(|q| format!("CPUQuota={q}"));
    let mut args = vec!["set-property", "--runtime", "--", unit, &weight];
    if let Some(ref q) = quota {
        args.push(q);
    }
    systemctl_user(&args).is_some_and(|o| o.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quota_round_trip_preserves_finite_and_unlimited_limits() {
        assert_eq!(quota_assignment("50000 100000"), Some("50.00%".into()));
        assert_eq!(quota_assignment("250000 100000"), Some("250.00%".into()));
        assert_eq!(quota_assignment("max 100000"), Some(String::new()));
        assert_eq!(quota_assignment("12345 100000"), None);
        assert_eq!(quota_assignment("37120 100000"), Some("37.12%".into()));
        assert_eq!(quota_assignment("100 0"), None);
        assert_eq!(quota_assignment("garbage"), None);
    }

    #[test]
    #[ignore = "requires a running systemd user manager and delegated CPU controller"]
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
        let original = read_unit_cpu_policy(&unit, true).expect("original CPU policy");
        assert_eq!(original.weight, Some(123));
        assert_eq!(original.quota.as_deref(), Some("37.00%"));
        assert!(throttle_unit(&unit, 25, 10));
        assert_eq!(
            read_unit_cpu_policy(&unit, true).unwrap().quota.as_deref(),
            Some("10.00%")
        );
        assert!(restore_unit(&unit, &original));
        assert_eq!(read_unit_cpu_policy(&unit, true), Some(original));
        let weight_only = read_unit_cpu_policy(&unit, false).unwrap();
        assert!(throttle_unit(&unit, 25, 0));
        assert!(restore_unit(&unit, &weight_only));
        assert_eq!(
            read_unit_cpu_policy(&unit, true).unwrap().quota.as_deref(),
            Some("37.00%")
        );
    }

    #[test]
    fn app_scope_is_throttleable() {
        assert_eq!(
            unit_from_cgroup_path(
                "/user.slice/user-1000.slice/user@1000.service/app.slice/app-firefox-1234.scope"
            ),
            Some("app-firefox-1234.scope".into())
        );
        assert_eq!(
            unit_from_cgroup_path(
                "/user.slice/user-1000.slice/user@1000.service/app.slice/foo.service"
            ),
            Some("foo.service".into())
        );
    }

    #[test]
    fn session_scope_and_user_manager_are_rejected() {
        // Login session scope = the whole desktop
        assert_eq!(
            unit_from_cgroup_path(
                "/user.slice/user-1000.slice/user@1000.service/session.slice/session-2.scope"
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
