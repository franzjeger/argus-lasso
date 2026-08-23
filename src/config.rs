//! Load/save config from ~/.config/argus-lasso/config.toml
//!
//! Config is stored as TOML. On load, missing keys are filled from
//! DEFAULT_CONFIG via a deep-merge at the serde level (Option defaults).

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ── Sub-structs ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct CpuConfig {
    /// Applied to every process not matched by a specific rule.
    /// None = disabled. e.g. "8-15,24-31"
    pub default_affinity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProBalanceConfig {
    pub enabled: bool,
    pub cpu_threshold_percent: f32,
    pub consecutive_seconds: f32,
    pub nice_adjustment: i32,
    pub nice_floor: i32,
    pub restore_threshold_percent: f32,
    pub restore_hysteresis_seconds: f32,
    pub exempt_patterns: Vec<String>,
    /// Throttle mechanism: "nice" (default), "cgroup" (per-unit CPUWeight via
    /// systemd), or "auto" (cgroup with per-process nice fallback).
    /// See docs/design-cgroup-probalance.md.
    pub method: String,
    /// cpu.weight applied to a throttled unit (kernel default is 100;
    /// 25 ⇒ ~4× smaller CPU share under contention).
    pub cgroup_throttle_weight: u32,
    /// Optional hard cap: CPUQuota percent per core scale (0 = no cap).
    pub cgroup_quota_percent: u32,
}

impl Default for ProBalanceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cpu_threshold_percent: 85.0,
            consecutive_seconds: 3.0,
            nice_adjustment: 10,
            nice_floor: 15,
            restore_threshold_percent: 40.0,
            restore_hysteresis_seconds: 5.0,
            exempt_patterns: vec![
                "kwin".into(),
                "plasmashell".into(),
                "systemd".into(),
                "kthreadd".into(),
                "Xorg".into(),
                "xwayland".into(),
            ],
            // Stays "nice" until the cgroup path is validated on real
            // hardware (rollout step 3 flips this to "auto").
            method: "nice".into(),
            cgroup_throttle_weight: 25,
            cgroup_quota_percent: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MonitorConfig {
    pub display_refresh_interval_ms: u64,
    pub rule_enforce_interval_ms: u64,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            display_refresh_interval_ms: 2000,
            rule_enforce_interval_ms: 500,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub start_minimized: bool,
    /// Window opacity 0.1–1.0
    pub opacity: f32,
    /// "BreezeDark" | "BreezeLight" | "AdwaitaDark" | "AdwaitaLight".
    /// Empty (the default) means "auto": pick the family matching the running
    /// desktop — Adwaita on GNOME, Breeze elsewhere (theme::default_theme).
    pub theme: String,
    pub sort_column: String,
    pub sort_ascending: bool,
    #[serde(default = "default_col_widths")]
    pub col_widths: Vec<f32>,
    /// Enable desktop notifications (ProBalance throttle, HW alerts, kill events).
    pub notifications_enabled: bool,
    /// Check GitHub for a newer release on startup.
    #[serde(default = "default_true")]
    pub check_updates_on_start: bool,
    /// HW Monitor column widths: [val, min, max, avg]
    #[serde(default = "default_hw_mon_col_widths")]
    pub hw_mon_col_widths: Vec<f32>,
    /// Process-table columns hidden by the user (by header label, e.g. "GPU%")
    pub hidden_columns: Vec<String>,
}

fn default_true() -> bool {
    true
}

fn default_col_widths() -> Vec<f32> {
    vec![60.0, 0.0, 90.0, 55.0, 75.0, 45.0, 110.0, 58.0, 85.0]
}

fn default_hw_mon_col_widths() -> Vec<f32> {
    vec![100.0, 72.0, 72.0, 72.0]
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            start_minimized: false,
            opacity: 1.0,
            theme: String::new(), // auto-detect from desktop on first run
            sort_column: "cpu_percent".into(),
            sort_ascending: false,
            col_widths: default_col_widths(),
            notifications_enabled: true,
            check_updates_on_start: true,
            hw_mon_col_widths: default_hw_mon_col_widths(),
            hidden_columns: Vec::new(),
        }
    }
}

/// Temperature alert configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HwAlertConfig {
    pub enabled: bool,
    /// Fire a notification when any sensor reaches this temperature (°C).
    pub temp_threshold_celsius: f32,
    /// Minimum seconds between repeated alerts for the same sensor.
    pub cooldown_secs: u64,
}

impl Default for HwAlertConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            temp_threshold_celsius: 90.0,
            cooldown_secs: 60,
        }
    }
}

/// A single gaming-mode profile (game launcher + CPU park settings).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GamingProfile {
    pub game_name: String,
    pub command: String,
    /// Map of cpu_index (as string) → keep_online bool
    pub cpu_states: std::collections::HashMap<String, bool>,
    pub elevate_nice: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct GamingModeConfig {
    pub profiles: std::collections::HashMap<String, GamingProfile>,
    /// Auto-enable Gaming Mode when a running game is detected
    /// (Steam/Proton heuristics on the process cmdline).
    pub auto_detect: bool,
    /// Also park non-preferred CPUs when auto-enabling (requires the helper).
    pub auto_park: bool,
}

// ── Rule (stored inline in config) ───────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleConfig {
    pub rule_id: String,
    pub name: String,
    pub pattern: String,
    /// "contains" | "exact" | "regex"
    pub match_type: String,
    pub affinity: Option<String>,
    pub nice: Option<i32>,
    pub ionice_class: Option<i32>,
    pub ionice_level: Option<i32>,
    pub enabled: bool,
}

impl Default for RuleConfig {
    fn default() -> Self {
        Self {
            rule_id: uuid::Uuid::new_v4().to_string(),
            name: String::new(),
            pattern: String::new(),
            match_type: "contains".into(),
            affinity: None,
            nice: None,
            ionice_class: None,
            ionice_level: None,
            enabled: true,
        }
    }
}

// ── Top-level config ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub version: u32,
    pub rules: Vec<RuleConfig>,
    pub cpu: CpuConfig,
    pub probalance: ProBalanceConfig,
    pub monitor: MonitorConfig,
    pub ui: UiConfig,
    pub gaming_mode: GamingModeConfig,
    pub hw_alerts: HwAlertConfig,
    /// Named rule sets: profile_name → list of rules.
    pub rule_profiles: std::collections::HashMap<String, Vec<RuleConfig>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            rules: vec![],
            cpu: CpuConfig::default(),
            probalance: ProBalanceConfig::default(),
            monitor: MonitorConfig::default(),
            ui: UiConfig::default(),
            gaming_mode: GamingModeConfig::default(),
            hw_alerts: HwAlertConfig::default(),
            rule_profiles: std::collections::HashMap::new(),
        }
    }
}

// ── Paths ─────────────────────────────────────────────────────────────────────

pub fn config_dir() -> PathBuf {
    let base = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    base.join(".config").join("argus-lasso")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

// ── Load / Save ───────────────────────────────────────────────────────────────

/// Migrate config from the old process-lasso-rs path to the new argus-lasso path.
fn migrate_old_config() {
    let Ok(home) = std::env::var("HOME") else {
        return;
    };
    let base = PathBuf::from(home);
    let old_path = base
        .join(".config")
        .join("process-lasso-rs")
        .join("config.toml");
    let new_path = config_path();
    if old_path.exists() && !new_path.exists() {
        if let Some(parent) = new_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if fs::copy(&old_path, &new_path).is_ok() {
            log::info!(
                "Migrated config from {} to {}",
                old_path.display(),
                new_path.display()
            );
        }
    }
}

/// Load config from disk, filling missing keys with defaults via serde.
pub fn load() -> Config {
    migrate_old_config();
    let path = config_path();
    if path.exists() {
        match fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => {
                    log::info!("Loaded config from {}", path.display());
                    return cfg;
                }
                Err(e) => {
                    log::warn!("Config parse error (using defaults): {e}");
                }
            },
            Err(e) => {
                log::warn!("Config read error (using defaults): {e}");
            }
        }
    }
    Config::default()
}

/// Atomically save config to disk (write to .tmp, then rename).
pub fn save(cfg: &Config) -> std::io::Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;
    let path = config_path();
    let tmp = path.with_extension("toml.tmp");
    let text = toml::to_string_pretty(cfg).map_err(|e| std::io::Error::other(e.to_string()))?;
    // fsync before rename — plain write+rename can leave an empty/truncated
    // config after a crash or power loss on some filesystems.
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    log::debug!("Config saved to {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config with every shape the file uses: nested tables, string and
    /// float arrays, a map of profiles, and an Option that is Some.
    fn populated() -> Config {
        let mut cfg = Config::default();
        cfg.cpu.default_affinity = Some("0-7,16-23".into());
        cfg.probalance.exempt_patterns = vec!["kwin".into(), "some app".into()];
        cfg.probalance.cpu_threshold_percent = 73.5;
        cfg.ui.opacity = 0.85;
        cfg.ui.theme = "BreezeLight".into();
        cfg.ui.col_widths = vec![61.0, 0.0, 91.5];
        cfg.ui.hidden_columns = vec!["GPU%".into()];
        cfg.rules = vec![RuleConfig {
            name: "Steam → V-Cache".into(),
            pattern: "steam".into(),
            match_type: "contains".into(),
            affinity: Some("0-7".into()),
            nice: Some(-5),
            ..Default::default()
        }];
        cfg.rule_profiles.insert("gaming".into(), cfg.rules.clone());
        cfg
    }

    /// The round trip is what `save` then `load` does on every settings
    /// change. A serialiser that cannot read back what it wrote silently
    /// resets the user's config to defaults — `load` logs and falls back.
    #[test]
    fn config_survives_a_toml_round_trip() {
        let cfg = populated();
        let text = toml::to_string_pretty(&cfg).expect("serialise");
        let back: Config = toml::from_str(&text).expect("deserialise");

        assert_eq!(back.cpu.default_affinity, cfg.cpu.default_affinity);
        assert_eq!(
            back.probalance.exempt_patterns,
            cfg.probalance.exempt_patterns
        );
        assert!(
            (back.probalance.cpu_threshold_percent - 73.5).abs() < 0.001,
            "float lost precision: {}",
            back.probalance.cpu_threshold_percent
        );
        assert!((back.ui.opacity - 0.85).abs() < 0.001);
        assert_eq!(back.ui.theme, "BreezeLight");
        assert_eq!(back.ui.col_widths, cfg.ui.col_widths);
        assert_eq!(back.ui.hidden_columns, cfg.ui.hidden_columns);
        assert_eq!(back.rules.len(), 1);
        assert_eq!(back.rules[0].name, "Steam → V-Cache");
        assert_eq!(back.rules[0].nice, Some(-5));
        assert_eq!(back.rule_profiles.get("gaming").map(|v| v.len()), Some(1));
    }

    /// Missing keys must fall back to defaults rather than failing the parse
    /// — that is what lets an old config survive a new release.
    #[test]
    fn a_partial_config_fills_in_defaults() {
        let cfg: Config = toml::from_str("[ui]\ntheme = \"BreezeLight\"\n").expect("parse");
        assert_eq!(cfg.ui.theme, "BreezeLight");
        assert_eq!(
            cfg.monitor.display_refresh_interval_ms,
            MonitorConfig::default().display_refresh_interval_ms
        );
        assert!(cfg.rules.is_empty());
    }

    /// Guards the actual on-disk format, not just the in-memory types.
    #[test]
    fn the_installed_config_shape_parses() {
        let sample = r#"
[cpu]
default_affinity = "8-15,24-31"

[probalance]
enabled = true
exempt_patterns = ["kwin", "plasmashell"]

[ui]
opacity = 1.0
theme = "BreezeDark"
col_widths = [60.0, 0.0, 90.0]

[[rules]]
name = "test"
pattern = "foo"
match_type = "exact"
"#;
        let cfg: Config = toml::from_str(sample).expect("the shipped config shape must parse");
        assert_eq!(cfg.cpu.default_affinity.as_deref(), Some("8-15,24-31"));
        assert_eq!(cfg.rules.len(), 1);
    }
}
