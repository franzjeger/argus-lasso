//! Rule dataclass and RuleEngine for matching and applying per-process rules.
//!
//! Mirrors Python rules.py exactly:
//!   - match_type: "contains" (case-insensitive), "exact", "regex"
//!   - apply_to_process applies ALL matching rules (not first-match-stop)

use regex::Regex;

use crate::config::RuleConfig;
use crate::utils;

// ── Rule ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Rule {
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
    /// Compiled regex, populated lazily if match_type == "regex"
    cached_regex: Option<Result<Regex, String>>,
    /// The `pattern` that `cached_regex` was compiled from. Lets
    /// `refresh_pattern_caches` skip re-compiling when the pattern is unchanged — the
    /// rule dialog's live match count calls it every frame, and recompiling a
    /// regex per frame was wasted work.
    regex_pattern: String,
    /// Pre-lowercased pattern for fast "contains" matching
    pub pattern_lower: String,
}

impl Rule {
    pub fn from_config(c: &RuleConfig) -> Self {
        let cached_regex = if c.match_type == "regex" {
            Some(Regex::new(&c.pattern).map_err(|e| e.to_string()))
        } else {
            None
        };
        Self {
            rule_id: c.rule_id.clone(),
            name: c.name.clone(),
            pattern: c.pattern.clone(),
            match_type: c.match_type.clone(),
            affinity: c.affinity.clone(),
            nice: c.nice,
            ionice_class: c.ionice_class,
            ionice_level: c.ionice_level,
            enabled: c.enabled,
            cached_regex,
            regex_pattern: c.pattern.clone(),
            pattern_lower: c.pattern.to_lowercase(),
        }
    }

    pub fn to_config(&self) -> RuleConfig {
        RuleConfig {
            rule_id: self.rule_id.clone(),
            name: self.name.clone(),
            pattern: self.pattern.clone(),
            match_type: self.match_type.clone(),
            affinity: self.affinity.clone(),
            nice: self.nice,
            ionice_class: self.ionice_class,
            ionice_level: self.ionice_level,
            enabled: self.enabled,
        }
    }

    pub fn new_empty() -> Self {
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
            cached_regex: None,
            regex_pattern: String::new(),
            pattern_lower: String::new(),
        }
    }

    /// Returns true if proc_name matches this rule.
    pub fn matches(&self, proc_name: &str, proc_name_lower: &str) -> bool {
        if !self.enabled || self.pattern.is_empty() {
            return false;
        }
        match self.match_type.as_str() {
            "exact" => proc_name == self.pattern,
            "regex" => match &self.cached_regex {
                Some(Ok(re)) => re.is_match(proc_name),
                // A pattern that failed to compile stays cached as the
                // failure (from_config / refresh_pattern_caches) — retrying
                // Regex::new on every call here would defeat the point of
                // caching for exactly the input that needs it most: a bad
                // pattern the user hasn't fixed yet, re-tried on every
                // process on every enforcement tick.
                Some(Err(_)) => false,
                // Only reachable if the cache was never populated at all.
                None => Regex::new(&self.pattern)
                    .map(|re| re.is_match(proc_name))
                    .unwrap_or(false),
            },
            _ => {
                // "contains" — case-insensitive substring
                proc_name_lower.contains(&self.pattern_lower)
            }
        }
    }

    /// Invalidate cached regex after pattern/match_type change.
    ///
    /// Skips re-compiling when the pattern is unchanged — the rule dialog's
    /// live match count calls this every frame, and recompiling the regex each
    /// time was pure waste.
    pub fn refresh_pattern_caches(&mut self) {
        // Always sync the lowercase pattern cache
        if self.pattern_lower.len() != self.pattern.len()
            || !self.pattern.eq_ignore_ascii_case(&self.pattern_lower)
        {
            self.pattern_lower = self.pattern.to_lowercase();
        }

        if self.match_type != "regex" {
            if self.cached_regex.is_some() {
                self.cached_regex = None;
                self.regex_pattern = String::new();
            }
            return;
        }
        if self.cached_regex.is_some() && self.regex_pattern == self.pattern {
            return; // cache is still valid
        }
        self.regex_pattern = self.pattern.clone();
        self.cached_regex = Some(Regex::new(&self.pattern).map_err(|e| e.to_string()));
    }
}

// ── RuleEngine ────────────────────────────────────────────────────────────────

pub struct RuleEngine {
    rules: Vec<Rule>,
    log_callback: Option<Box<dyn Fn(String) + Send>>,
    /// (rule_id, pid) pairs whose set_nice already failed — retried/logged once,
    /// not every enforcement tick (a permission failure never heals by itself).
    nice_failed: std::collections::HashSet<(String, u32)>,
}

impl RuleEngine {
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            log_callback: None,
            nice_failed: std::collections::HashSet::new(),
        }
    }

    pub fn set_log_callback<F: Fn(String) + Send + 'static>(&mut self, cb: F) {
        self.log_callback = Some(Box::new(cb));
    }

    fn log(&self, msg: String) {
        log::info!("{msg}");
        if let Some(cb) = &self.log_callback {
            cb(msg);
        }
    }

    pub fn load_rules(&mut self, configs: &[RuleConfig]) {
        self.rules = configs.iter().map(Rule::from_config).collect();
        self.nice_failed.clear();
    }

    pub fn get_rules(&self) -> &[Rule] {
        &self.rules
    }

    pub fn get_rules_mut(&mut self) -> &mut Vec<Rule> {
        &mut self.rules
    }

    pub fn add_rule(&mut self, rule: Rule) {
        self.rules.push(rule);
    }

    pub fn remove_rule(&mut self, rule_id: &str) {
        self.rules.retain(|r| r.rule_id != rule_id);
    }

    pub fn clear_rules(&mut self) {
        self.rules.clear();
    }

    pub fn update_rule(&mut self, updated: Rule) {
        // Editing a rule may change its nice target — give it a fresh attempt.
        self.nice_failed.retain(|(rid, _)| rid != &updated.rule_id);
        if let Some(r) = self.rules.iter_mut().find(|r| r.rule_id == updated.rule_id) {
            *r = updated;
        }
    }

    pub fn to_config_list(&self) -> Vec<RuleConfig> {
        self.rules.iter().map(|r| r.to_config()).collect()
    }

    /// Returns true if any enabled rule matches this process name — independent
    /// of whether applying it would change anything right now. Callers deciding
    /// between "rule-managed" and "apply default affinity" must use this, not
    /// the action list from apply_to_process (an already-correct process yields
    /// no actions but is still rule-managed).
    pub fn matches_any(&self, proc_name: &str) -> bool {
        let lower = proc_name.to_lowercase();
        self.rules.iter().any(|r| r.matches(proc_name, &lower))
    }

    /// Apply all matching rules to a process. Returns list of action descriptions.
    /// All matching rules are applied (not first-match-stop).
    pub fn apply_to_process(&mut self, pid: u32, proc_name: &str) -> Vec<String> {
        let mut nice_failed = std::mem::take(&mut self.nice_failed);
        let current_nice = utils::get_nice(pid);
        let current_ionice = utils::get_ionice_raw(pid);
        let actions = apply_rules(
            &self.rules,
            pid,
            proc_name,
            current_nice,
            current_ionice,
            &mut nice_failed,
            &|m| self.log(m),
        );
        self.nice_failed = nice_failed;
        actions
    }
}

/// Enforce a rule slice against one process. Standalone so the monitor daemon
/// can clone the rules and run enforcement WITHOUT holding the RuleEngine
/// mutex across procfs reads and renice/ionice subprocess spawns (the GUI
/// locks the same engine to edit rules and would freeze otherwise).
/// Dirty-checks each attribute before calling the syscall so that periodic
/// re-enforcement does not spam the log with no-op "already correct" entries.
pub fn apply_rules(
    rules: &[Rule],
    pid: u32,
    proc_name: &str,
    current_nice: Option<i32>,
    current_ionice: Option<(i32, i32)>,
    nice_failed: &mut std::collections::HashSet<(String, u32)>,
    log: &impl Fn(String),
) -> Vec<String> {
    let mut actions = Vec::new();
    let proc_name_lower = proc_name.to_lowercase();
    // Track what we believe the live nice/ionice values are as rules apply,
    // so a later rule in this same pass sees what an earlier one in this
    // pass just set — mirroring the affinity check below, which re-reads
    // from the OS fresh every iteration for the same reason. Without this,
    // two enabled rules that both set nice (or both set ionice) on the same
    // process compared against the same pre-loop snapshot, so the second
    // rule could wrongly believe its target already matched — leaving the
    // process stuck on the first rule's value, or oscillating between the
    // two on alternating enforcement ticks.
    let mut current_nice = current_nice;
    let mut current_ionice = current_ionice;
    for rule in rules {
        if !rule.matches(proc_name, &proc_name_lower) {
            continue;
        }

        // ── Affinity ─────────────────────────────────────────────────
        if let Some(ref aff) = rule.affinity {
            if utils::set_affinity_if_changed(pid, aff) {
                let msg = format!(
                    "[Rule:{}] Set affinity={} on {}({})",
                    rule.name, aff, proc_name, pid
                );
                log(msg.clone());
                actions.push(msg);
            }
        }

        // ── Nice ─────────────────────────────────────────────────────
        if let Some(nice) = rule.nice {
            let fail_key = (rule.rule_id.clone(), pid);
            if current_nice != Some(nice) && !nice_failed.contains(&fail_key) {
                if utils::set_nice(pid, nice) {
                    current_nice = Some(nice);
                    let msg = format!(
                        "[Rule:{}] Set nice={} on {}({})",
                        rule.name, nice, proc_name, pid
                    );
                    log(msg.clone());
                    actions.push(msg);
                } else {
                    // Don't retry every tick: a permission failure would spawn
                    // a renice subprocess and a log line every 500 ms forever.
                    nice_failed.insert(fail_key);
                    let msg = format!(
                        "[Rule:{}] nice={} FAILED (root needed?) for {}({}) — giving up for this process",
                        rule.name, nice, proc_name, pid
                    );
                    log(msg.clone());
                    actions.push(msg);
                }
            }
        }

        // ── Ionice ───────────────────────────────────────────────────
        if let Some(class) = rule.ionice_class {
            let target_level = rule.ionice_level.unwrap_or(0);
            if current_ionice != Some((class, target_level))
                && utils::set_ionice(pid, class, rule.ionice_level)
            {
                current_ionice = Some((class, target_level));
                let msg = format!(
                    "[Rule:{}] Set ionice class={} level={:?} on {}({})",
                    rule.name, class, rule.ionice_level, proc_name, pid
                );
                log(msg.clone());
                actions.push(msg);
            }
        }
    }
    actions
}

impl Default for RuleEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule_with(pattern: &str, match_type: &str) -> Rule {
        let mut r = Rule::new_empty();
        r.pattern = pattern.into();
        r.match_type = match_type.into();
        r.refresh_pattern_caches();
        r
    }

    #[test]
    fn match_contains_is_case_insensitive() {
        let r = rule_with("Chrome", "contains");
        assert!(r.matches("google-chrome", &"google-chrome".to_lowercase()));
        assert!(r.matches("CHROME.exe", &"CHROME.exe".to_lowercase()));
        assert!(r.matches(
            "chromium-but-contains-chrome",
            &"chromium-but-contains-chrome".to_lowercase()
        ));
        assert!(!r.matches("firefox", &"firefox".to_lowercase()));
    }

    #[test]
    fn match_exact_is_case_sensitive_and_full_string() {
        let r = rule_with("steam", "exact");
        assert!(r.matches("steam", &"steam".to_lowercase()));
        assert!(!r.matches("Steam", &"Steam".to_lowercase()));
        assert!(!r.matches("steamwebhelper", &"steamwebhelper".to_lowercase()));
        assert!(!r.matches("not-steam", &"not-steam".to_lowercase()));
    }

    #[test]
    fn match_regex_anchored_or_not() {
        let r = rule_with(r"^node(\.exe)?$", "regex");
        assert!(r.matches("node", &"node".to_lowercase()));
        assert!(r.matches("node.exe", &"node.exe".to_lowercase()));
        assert!(!r.matches("nodejs", &"nodejs".to_lowercase()));
        assert!(!r.matches("my-node", &"my-node".to_lowercase()));
    }

    #[test]
    fn match_regex_invalid_pattern_returns_false() {
        // Invalid regex should fail safe (no match) rather than panic.
        let r = rule_with("[unclosed", "regex");
        assert!(!r.matches("anything", &"anything".to_lowercase()));
    }

    #[test]
    fn empty_pattern_never_matches() {
        let r = rule_with("", "contains");
        assert!(!r.matches("anything", &"anything".to_lowercase()));
    }

    #[test]
    fn disabled_rule_never_matches() {
        let mut r = rule_with("chrome", "contains");
        r.enabled = false;
        assert!(!r.matches("chrome", &"chrome".to_lowercase()));
    }

    #[test]
    fn refresh_pattern_caches_recompiles_on_pattern_change() {
        let mut r = rule_with("foo", "regex");
        assert!(r.matches("foo", &"foo".to_lowercase()));
        r.pattern = "bar".into();
        r.refresh_pattern_caches();
        assert!(r.matches("bar", &"bar".to_lowercase()));
        assert!(!r.matches("foo", &"foo".to_lowercase()));
    }

    #[test]
    fn refresh_pattern_caches_keeps_cache_when_pattern_unchanged() {
        // The rule dialog calls this every frame; it must be a no-op (and keep
        // matching) when the pattern hasn't changed.
        let mut r = rule_with("foo", "regex");
        assert!(r.matches("foo", &"foo".to_lowercase()));
        r.refresh_pattern_caches();
        r.refresh_pattern_caches();
        assert!(r.matches("foo", &"foo".to_lowercase()));
        assert!(!r.matches("bar", &"bar".to_lowercase()));
    }

    #[test]
    fn refresh_pattern_caches_clears_cache_when_leaving_regex() {
        let mut r = rule_with("foo", "regex");
        assert!(r.cached_regex.is_some());
        r.match_type = "contains".into();
        r.refresh_pattern_caches();
        assert!(r.cached_regex.is_none());
        // And back to regex recompiles from the current pattern.
        r.match_type = "regex".into();
        r.refresh_pattern_caches();
        assert!(r.cached_regex.is_some());
        assert!(r.matches("foo", &"foo".to_lowercase()));
    }

    #[test]
    fn engine_apply_skips_non_matching_rules() {
        // Pure matching coverage — no syscalls invoked because no rules match.
        let mut engine = RuleEngine::new();
        let cfg = crate::config::RuleConfig {
            rule_id: "1".into(),
            name: "test".into(),
            pattern: "chrome".into(),
            match_type: "contains".into(),
            affinity: None,
            nice: None,
            ionice_class: None,
            ionice_level: None,
            enabled: true,
        };
        engine.load_rules(&[cfg]);
        // Non-matching name → empty actions, no syscalls attempted
        let actions = engine.apply_to_process(0, "firefox");
        assert!(actions.is_empty());
    }

    #[test]
    fn rule_config_json_round_trip() {
        let cfg = crate::config::RuleConfig {
            rule_id: "abc-123".into(),
            name: "Browser".into(),
            pattern: "chrome".into(),
            match_type: "contains".into(),
            affinity: Some("0-3".into()),
            nice: Some(5),
            ionice_class: Some(2),
            ionice_level: Some(4),
            enabled: true,
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: crate::config::RuleConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.rule_id, cfg.rule_id);
        assert_eq!(back.pattern, cfg.pattern);
        assert_eq!(back.match_type, cfg.match_type);
        assert_eq!(back.affinity, cfg.affinity);
        assert_eq!(back.nice, cfg.nice);
        assert_eq!(back.ionice_class, cfg.ionice_class);
        assert_eq!(back.ionice_level, cfg.ionice_level);
        assert_eq!(back.enabled, cfg.enabled);
    }

    #[test]
    fn rule_round_trip_via_from_to_config_preserves_state() {
        let cfg = crate::config::RuleConfig {
            rule_id: "id".into(),
            name: "Game".into(),
            pattern: r"^game\.exe$".into(),
            match_type: "regex".into(),
            affinity: Some("0,2,4".into()),
            nice: Some(-5),
            ionice_class: None,
            ionice_level: None,
            enabled: false,
        };
        let rule = Rule::from_config(&cfg);
        let back = rule.to_config();
        assert_eq!(back.rule_id, cfg.rule_id);
        assert_eq!(back.pattern, cfg.pattern);
        assert_eq!(back.match_type, cfg.match_type);
        assert_eq!(back.affinity, cfg.affinity);
        assert_eq!(back.nice, cfg.nice);
        assert_eq!(back.enabled, cfg.enabled);
    }

    /// Regression test for the nice/ionice staleness bug: apply_rules() used
    /// to dirty-check every rule against the snapshot taken *before* the
    /// loop, instead of re-checking against what an earlier rule in the same
    /// pass just set (the way the affinity check re-reads from the OS every
    /// iteration). Two enabled rules on the same process, second one setting
    /// nice back to the pre-pass value, reproduces it: under the bug, the
    /// second rule wrongly compared against the stale snapshot and never
    /// fired at all, silently leaving the process on the first rule's value.
    ///
    /// Targets this test process's own pid with small, always-permitted
    /// non-negative nice values — no elevated privilege needed — and
    /// restores the original nice value afterward.
    #[test]
    fn apply_rules_lets_a_later_rule_override_an_earlier_ones_nice_in_the_same_pass() {
        let pid = std::process::id();
        let starting = utils::get_nice(pid).unwrap_or(0);
        let bumped = if starting == 1 { 2 } else { 1 };

        let mut rule1 = rule_with("apply-rules-staleness-test", "contains");
        rule1.rule_id = "r1".into();
        rule1.nice = Some(bumped);
        let mut rule2 = rule_with("apply-rules-staleness-test", "contains");
        rule2.rule_id = "r2".into();
        rule2.nice = Some(starting);

        let mut nice_failed = std::collections::HashSet::new();
        let actions = apply_rules(
            &[rule1, rule2],
            pid,
            "apply-rules-staleness-test",
            Some(starting),
            None,
            &mut nice_failed,
            &|_| {},
        );

        let ended_at = utils::get_nice(pid);
        // Always put it back, pass or fail.
        let _ = utils::set_nice(pid, starting);

        assert_eq!(
            actions.len(),
            2,
            "both rules should have applied a nice change: {actions:?}"
        );
        assert_eq!(
            ended_at,
            Some(starting),
            "the second rule must win and restore the pre-pass value, not get \
             silently skipped for comparing against a stale snapshot"
        );
    }
}
