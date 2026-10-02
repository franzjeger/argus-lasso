//! Load/save config from ~/.config/argus-lasso/config.toml
//!
//! Config is stored as TOML. On load, missing keys are filled from
//! DEFAULT_CONFIG via a deep-merge at the serde level (Option defaults).

use std::fs;
use std::path::{Path, PathBuf};

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
    /// Whole-system busy CPU time, 0–100% of currently available capacity.
    pub system_cpu_threshold_percent: f32,
    /// Minimum process share of that same capacity before it is a candidate.
    pub process_min_cpu_percent: f32,
    pub consecutive_seconds: f32,
    pub nice_adjustment: i32,
    pub nice_floor: i32,
    pub system_restore_threshold_percent: f32,
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
            system_cpu_threshold_percent: 85.0,
            process_min_cpu_percent: 1.0,
            consecutive_seconds: 3.0,
            nice_adjustment: 10,
            nice_floor: 15,
            system_restore_threshold_percent: 75.0,
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

impl ProBalanceConfig {
    /// Enforce meaningful hysteresis and finite percentages even for edited TOML.
    pub fn normalize(&mut self) {
        let bounded = |v: f32, default: f32, min: f32, max: f32| {
            if v.is_finite() {
                v.clamp(min, max)
            } else {
                default
            }
        };
        self.system_cpu_threshold_percent =
            bounded(self.system_cpu_threshold_percent, 85.0, 1.0, 100.0);
        // Restore stays below activation, or one reading would both activate
        // and recover. A value already below it is kept as entered: forcing a
        // whole point of gap turned 84.5 under 85 into 84 on Apply, although
        // the form had accepted it.
        let restore = bounded(self.system_restore_threshold_percent, 75.0, 0.0, 99.0);
        self.system_restore_threshold_percent = if restore < self.system_cpu_threshold_percent {
            restore
        } else {
            (self.system_cpu_threshold_percent - 1.0).max(0.0)
        };
        self.process_min_cpu_percent = bounded(self.process_min_cpu_percent, 1.0, 0.1, 100.0);
        self.consecutive_seconds = bounded(self.consecutive_seconds, 3.0, 1.0, 60.0);
        self.restore_hysteresis_seconds = bounded(self.restore_hysteresis_seconds, 5.0, 1.0, 120.0);
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
    #[serde(default)]
    pub global_overlay: bool,
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
    /// Closing the window keeps Argus running in the tray instead of quitting.
    /// Only with a tray icon: without one there would be no way back.
    pub close_to_tray: bool,
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
            global_overlay: false,
            opacity: 1.0,
            theme: String::new(), // auto-detect from desktop on first run
            sort_column: "cpu_percent".into(),
            sort_ascending: false,
            col_widths: default_col_widths(),
            notifications_enabled: true,
            check_updates_on_start: true,
            close_to_tray: false,
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
    /// Vulkan overlay configuration
    #[serde(default)]
    pub overlay: argus_ipc::OverlayConfig,
}

// ── Rule (stored inline in config) ───────────────────────────────────────────

/// How a rule's pattern is compared with a process name. Stored as the same
/// lowercase words the field always held; anything else is now a parse error
/// instead of silently meaning "contains".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchType {
    /// Case-insensitive substring.
    #[default]
    Contains,
    /// The whole name, case-sensitive.
    Exact,
    Regex,
}

impl MatchType {
    pub const ALL: [MatchType; 3] = [MatchType::Contains, MatchType::Exact, MatchType::Regex];

    pub fn as_str(self) -> &'static str {
        match self {
            MatchType::Contains => "contains",
            MatchType::Exact => "exact",
            MatchType::Regex => "regex",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleConfig {
    pub rule_id: String,
    pub name: String,
    pub pattern: String,
    pub match_type: MatchType,
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
            match_type: MatchType::Contains,
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

/// The user's home: $HOME, else the password database.
pub fn home_dir() -> Option<PathBuf> {
    std::env::home_dir().filter(|home| home.is_absolute())
}

pub fn config_dir() -> PathBuf {
    // Never a shared directory such as /tmp, where another user could create
    // this path first and plant or read the configuration — or stage the root
    // helpers. Without any home, loading and saving fail visibly instead.
    home_dir()
        .unwrap_or_else(|| PathBuf::from("/nonexistent"))
        .join(".config")
        .join("argus-lasso")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

// ── Load / Save ───────────────────────────────────────────────────────────────

/// Migrate config from the old process-lasso-rs path to the new argus-lasso
/// path, once.
fn migrate_old_config() {
    let Ok(home) = std::env::var("HOME") else {
        return;
    };
    let old_path = PathBuf::from(home)
        .join(".config")
        .join("process-lasso-rs")
        .join("config.toml");
    migrate_config(&old_path, &config_path());
}

/// Copy `old` to `new` if there is no configuration at `new` and this has
/// not happened before. A marker next to `new` records that it has: when a
/// broken config was later set aside (preserve_unreadable), the next start
/// migrated the old file again and enforced its long-stale rules.
fn migrate_config(old: &Path, new: &Path) {
    let marker = new.with_file_name(".migrated-from-process-lasso-rs");
    if is_present(&marker) || !is_present(old) {
        return;
    }
    if !is_present(new) {
        let copied = fs::read(old).and_then(|bytes| {
            if let Some(parent) = new.parent() {
                fs::create_dir_all(parent)?;
            }
            atomic_write(new, &bytes)
        });
        match copied {
            Ok(()) => log::info!(
                "Migrated config from {} to {}",
                old.display(),
                new.display()
            ),
            Err(e) => {
                log::warn!("Could not migrate config from {}: {e}", old.display());
                return;
            }
        }
    }
    let _ = fs::write(&marker, b"");
}

/// Whether anything is at `path`, a dangling symlink included. Only "not
/// found" means absent: `Path::exists` is false for a dangling symlink and
/// for any error, and treating those as "no config" saved defaults over it.
fn is_present(path: &Path) -> bool {
    !matches!(fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// Load config from disk, filling missing keys with defaults via serde.
///
/// A file that exists but cannot be read or parsed yields the defaults and
/// the error. The caller must not let the next save replace that file before
/// `preserve_unreadable` has moved it aside: saving the defaults over it
/// would silently delete every rule and profile it held.
pub fn load(migrate: bool) -> (Config, Option<String>) {
    if migrate {
        migrate_old_config();
    }
    load_from(&config_path())
}

fn load_from(path: &Path) -> (Config, Option<String>) {
    if !is_present(path) {
        return (Config::default(), None);
    }
    let error = match fs::read_to_string(path) {
        Ok(text) => match toml::from_str::<Config>(&text) {
            Ok(mut cfg) => {
                cfg.probalance.normalize();
                log::info!("Loaded config from {}", path.display());
                return (cfg, None);
            }
            Err(e) => format!("could not be parsed: {e}"),
        },
        Err(e) => format!("could not be read: {e}"),
    };
    log::warn!("Config {} {error} — using defaults", path.display());
    (Config::default(), Some(error))
}

/// Move an unreadable config file aside, so that saving the defaults cannot
/// overwrite it. Returns a message for the user naming where it went.
pub fn preserve_unreadable(error: &str) -> String {
    preserve_unreadable_at(&config_path(), error)
}

fn preserve_unreadable_at(path: &Path, error: &str) -> String {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let kept = path.with_extension(format!("toml.unreadable-{stamp}"));
    match fs::rename(path, &kept) {
        Ok(()) => format!(
            "Settings {error}. Defaults are in use; the previous file was kept as {}.",
            kept.display()
        ),
        Err(e) => format!(
            "Settings {error}, and could not be set aside ({e}). Defaults are in use; \
             changing a setting will overwrite {}.",
            path.display()
        ),
    }
}

/// Atomically save config to disk (write to .tmp, then rename).
pub fn save(cfg: &Config) -> std::io::Result<()> {
    let dir = config_dir();
    fs::create_dir_all(&dir)?;
    let path = config_path();
    atomic_write(
        &path,
        toml::to_string_pretty(cfg)
            .map_err(|e| std::io::Error::other(e.to_string()))?
            .as_bytes(),
    )?;
    log::debug!("Config saved to {}", path.display());
    Ok(())
}

/// Private, unique staging files avoid cross-process collisions and symlink
/// truncation. Sync the parent directory as well as the file for durability.
pub(crate) fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("missing parent"))?;
    let tmp = parent.join(format!(".argus-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    /// The words older versions wrote still parse, and are written back
    /// unchanged, so rolling back to an older version keeps its rules.
    #[test]
    fn match_types_keep_their_stored_words() {
        for (word, kind) in [
            ("contains", MatchType::Contains),
            ("exact", MatchType::Exact),
            ("regex", MatchType::Regex),
        ] {
            let text = format!("pattern = \"game\"\nmatch_type = \"{word}\"\n");
            let rule: RuleConfig = toml::from_str(&text).unwrap();
            assert_eq!(rule.match_type, kind);
            assert!(toml::to_string(&rule)
                .unwrap()
                .contains(&format!("match_type = \"{word}\"")));
        }
    }

    /// An unknown match type used to fall through to "contains" without a
    /// word; a rule meant as a regex then matched as a substring.
    #[test]
    fn an_unknown_match_type_is_an_error() {
        let err = toml::from_str::<RuleConfig>("match_type = \"regx\"\n").unwrap_err();
        assert!(err.to_string().contains("regx"), "{err}");
    }

    /// One unparseable value used to mean defaults in memory and, on the
    /// next save, every rule and profile gone from disk.
    #[test]
    fn an_unreadable_config_is_reported_and_set_aside() {
        let dir = std::env::temp_dir().join(format!("argus-config-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let broken = "[[rules]]\nname = \"game\"\nenabled = \"sometimes\"\n";
        fs::write(&path, broken).unwrap();

        let (cfg, error) = load_from(&path);
        assert!(cfg.rules.is_empty());
        let error = error.expect("a parse error must be reported");

        let notice = preserve_unreadable_at(&path, &error);
        assert!(
            !path.exists(),
            "the next save must not find it to overwrite"
        );
        let kept: Vec<_> = fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(fs::read_to_string(kept[0].path()).unwrap(), broken);
        assert!(notice.contains(&kept[0].path().display().to_string()));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_config_is_not_an_error() {
        let path =
            std::env::temp_dir().join(format!("argus-missing-{}.toml", uuid::Uuid::new_v4()));
        assert!(load_from(&path).1.is_none());
    }

    use super::*;

    #[test]
    fn atomic_writes_do_not_follow_destination_symlinks_or_share_staging_files() {
        let dir = std::env::temp_dir().join(format!("argus-config-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&dir).unwrap();
        let victim = dir.join("victim");
        let path = dir.join("config.toml");
        fs::write(&victim, "untouched").unwrap();
        std::os::unix::fs::symlink(&victim, &path).unwrap();
        let workers: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    atomic_write(&path, format!("[ui]\ntheme = 'theme-{i}'\n").as_bytes()).unwrap()
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched");
        toml::from_str::<Config>(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A config with every shape the file uses: nested tables, string and
    /// float arrays, a map of profiles, and an Option that is Some.
    fn populated() -> Config {
        let mut cfg = Config::default();
        cfg.cpu.default_affinity = Some("0-7,16-23".into());
        cfg.probalance.exempt_patterns = vec!["kwin".into(), "some app".into()];
        cfg.probalance.system_cpu_threshold_percent = 73.5;
        cfg.ui.opacity = 0.85;
        cfg.ui.theme = "BreezeLight".into();
        cfg.ui.col_widths = vec![61.0, 0.0, 91.5];
        cfg.ui.hidden_columns = vec!["GPU%".into()];
        cfg.rules = vec![RuleConfig {
            name: "Steam → V-Cache".into(),
            pattern: "steam".into(),
            match_type: MatchType::Contains,
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
            (back.probalance.system_cpu_threshold_percent - 73.5).abs() < 0.001,
            "float lost precision: {}",
            back.probalance.system_cpu_threshold_percent
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

    #[test]
    fn overlay_legacy_and_live_options_roundtrip() {
        let mut cfg: Config = toml::from_str(
            r#"
[ui]
opacity = 0.78
[gaming_mode.overlay]
scale = 2
show_overlay = false
offset_x = 70
bg_color = [0, 0, 0, 0]
"#,
        )
        .unwrap();
        assert_eq!(cfg.ui.opacity, 0.78);
        assert_eq!(cfg.gaming_mode.overlay.font_px, 14);
        assert!(!cfg.gaming_mode.overlay.show_overlay);
        let o = &mut cfg.gaming_mode.overlay;
        o.font_px = 24;
        o.show_graph = true;
        o.margin = 12;
        o.fields.gpu_fan = false;
        o.fields.cpu_temp = false;
        o.hidden_cpu_ids = vec![0, 7, 31];
        o.anchor = 3;
        o.value_colors
            .insert(argus_ipc::OverlayMetric::GpuTemp, [255, 80, 120]);
        o.value_colors
            .insert(argus_ipc::OverlayMetric::ThreadFrequency, [200, 220, 255]);
        o.section_dividers = false;
        o.text_color = (12, 34, 56, 78);
        o.bg_color = (1, 2, 3, 128);
        let decoded: Config = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(decoded.gaming_mode.overlay, cfg.gaming_mode.overlay);
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

#[cfg(test)]
mod cpu_policy_config_tests {
    use super::*;

    #[test]
    fn old_per_core_thresholds_are_not_reinterpreted_as_system_percentages() {
        let config: Config = toml::from_str("[probalance]\ncpu_threshold_percent = 1600.0\nrestore_threshold_percent = 400.0\nnice_adjustment = 7\nexempt_patterns = ['mygame']\n").unwrap();
        assert_eq!(config.probalance.system_cpu_threshold_percent, 85.0);
        assert_eq!(config.probalance.system_restore_threshold_percent, 75.0);
        assert_eq!(config.probalance.nice_adjustment, 7);
        assert_eq!(config.probalance.exempt_patterns, vec!["mygame"]);
    }

    #[test]
    fn invalid_values_cannot_break_hysteresis_or_cpu_range() {
        let mut cfg = ProBalanceConfig {
            system_cpu_threshold_percent: 20.0,
            system_restore_threshold_percent: 80.0,
            process_min_cpu_percent: f32::NAN,
            ..Default::default()
        };
        cfg.normalize();
        assert_eq!(cfg.system_restore_threshold_percent, 19.0);
        assert_eq!(cfg.process_min_cpu_percent, 1.0);
    }

    #[test]
    fn a_restore_threshold_below_activation_is_kept_as_entered() {
        let mut cfg = ProBalanceConfig {
            system_cpu_threshold_percent: 85.0,
            system_restore_threshold_percent: 84.5,
            ..Default::default()
        };
        cfg.normalize();
        assert_eq!(cfg.system_restore_threshold_percent, 84.5);

        // The lowest activation leaves room only for 0.
        cfg.system_cpu_threshold_percent = 1.0;
        cfg.system_restore_threshold_percent = 1.0;
        cfg.normalize();
        assert_eq!(cfg.system_restore_threshold_percent, 0.0);
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("argus-cfg-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A config symlinked to a file that is not there yet (dotfiles on an
    /// unmounted disk) read as "no config", and the first save replaced the
    /// symlink with defaults.
    #[test]
    fn a_dangling_config_symlink_is_not_an_absent_config() {
        let dir = scratch();
        let path = dir.join("config.toml");
        std::os::unix::fs::symlink(dir.join("not-mounted/config.toml"), &path).unwrap();
        let (_, error) = load_from(&path);
        assert!(
            error.is_some(),
            "reported, so it is set aside, not overwritten"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    /// The old config was migrated again whenever the new one was missing,
    /// such as after a broken one had been set aside.
    #[test]
    fn the_old_config_is_migrated_once() {
        let dir = scratch();
        let (old, new) = (dir.join("old.toml"), dir.join("new/config.toml"));
        fs::write(&old, "[probalance]\nenabled = true\n").unwrap();
        migrate_config(&old, &new);
        assert_eq!(
            fs::read_to_string(&new).unwrap(),
            "[probalance]\nenabled = true\n"
        );

        fs::remove_file(&new).unwrap();
        migrate_config(&old, &new);
        assert!(!new.exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
