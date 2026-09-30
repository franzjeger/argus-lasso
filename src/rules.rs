//! Rule dataclass and RuleEngine for matching and applying per-process rules.
//!
//! Mirrors Python rules.py exactly:
//!   - match_type: contains (case-insensitive), exact, regex
//!   - apply_rules applies ALL matching rules (not first-match-stop)

use regex::Regex;

use crate::config::{MatchType, RuleConfig};
use crate::utils;

/// Pure explanation of requested policy; never performs a syscall.
pub struct RuleEffect<'a> {
    pub matches: Vec<&'a Rule>,
    pub affinity: Option<&'a str>,
    pub nice: Option<i32>,
    pub ionice: Option<(i32, i32)>,
    pub conflict: bool,
}
pub fn preview_effect<'a>(rules: &'a [Rule], name: &str) -> RuleEffect<'a> {
    let mut effect = RuleEffect {
        matches: Vec::new(),
        affinity: None,
        nice: None,
        ionice: None,
        conflict: false,
    };
    let lower = name.to_lowercase();
    for rule in rules.iter().filter(|r| r.matches(name, &lower)) {
        effect.matches.push(rule);
        if let Some(v) = rule.affinity.as_deref() {
            effect.conflict |= effect.affinity.is_some_and(|old| old != v);
            effect.affinity = Some(v);
        }
        if let Some(v) = rule.nice {
            effect.conflict |= effect.nice.is_some_and(|old| old != v);
            effect.nice = Some(v);
        }
        if let Some(class) = rule.ionice_class {
            let v = (class, rule.ionice_level.unwrap_or(0));
            effect.conflict |= effect.ionice.is_some_and(|old| old != v);
            effect.ionice = Some(v);
        }
    }
    effect
}

// ── Rule ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Rule {
    pub rule_id: String,
    pub name: String,
    pub pattern: String,
    pub match_type: MatchType,
    pub affinity: Option<String>,
    pub nice: Option<i32>,
    pub ionice_class: Option<i32>,
    pub ionice_level: Option<i32>,
    pub enabled: bool,
    /// Compiled regex, populated lazily for MatchType::Regex
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
        let cached_regex = if c.match_type == MatchType::Regex {
            Some(Regex::new(&c.pattern).map_err(|e| e.to_string()))
        } else {
            None
        };
        Self {
            rule_id: c.rule_id.clone(),
            name: c.name.clone(),
            pattern: c.pattern.clone(),
            match_type: c.match_type,
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
            match_type: self.match_type,
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
            match_type: MatchType::Contains,
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
        match self.match_type {
            MatchType::Exact => proc_name == self.pattern,
            MatchType::Regex => match &self.cached_regex {
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
            MatchType::Contains => proc_name_lower.contains(&self.pattern_lower),
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

        if self.match_type != MatchType::Regex {
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

/// The rule set, shared by the GUI (which edits it) and the monitor thread
/// (which applies it with `apply_rules`).
pub struct RuleEngine {
    rules: Vec<Rule>,
}

impl RuleEngine {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    pub fn load_rules(&mut self, configs: &[RuleConfig]) {
        self.rules = configs.iter().map(Rule::from_config).collect();
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
    /// the action list from apply_rules (an already-correct process yields no
    /// actions but is still rule-managed).
    pub fn matches_any(&self, proc_name: &str) -> bool {
        let lower = proc_name.to_lowercase();
        self.rules.iter().any(|r| r.matches(proc_name, &lower))
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
                if let Err(e) = utils::set_nice(pid, nice) {
                    // Don't retry every tick: a permission failure would spawn
                    // a renice subprocess and a log line every 500 ms forever.
                    nice_failed.insert(fail_key);
                    let msg = format!(
                        "[Rule:{}] nice={} FAILED for {}({}): {e} — giving up for this process",
                        rule.name, nice, proc_name, pid
                    );
                    log(msg.clone());
                    actions.push(msg);
                } else {
                    current_nice = Some(nice);
                    let msg = format!(
                        "[Rule:{}] Set nice={} on {}({})",
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
                && utils::set_ionice(pid, class, rule.ionice_level).is_ok()
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
    #[test]
    fn preview_reports_field_precedence_and_ignores_disabled_rules() {
        let mut first = Rule::new_empty();
        first.pattern = "game".into();
        first.match_type = MatchType::Exact;
        first.enabled = true;
        first.affinity = Some("0-3".into());
        first.nice = Some(5);
        let mut last = first.clone();
        last.affinity = None;
        last.nice = Some(10);
        let mut disabled = first.clone();
        disabled.enabled = false;
        disabled.nice = Some(-10);
        let rules = [first, last, disabled];
        let effect = preview_effect(&rules, "game");
        assert_eq!(effect.matches.len(), 2);
        assert_eq!(effect.affinity, Some("0-3"));
        assert_eq!(effect.nice, Some(10));
        assert!(effect.conflict);
        assert!(preview_effect(&rules, "other").matches.is_empty());
    }

    fn rule_with(pattern: &str, match_type: MatchType) -> Rule {
        let mut r = Rule::new_empty();
        r.pattern = pattern.into();
        r.match_type = match_type;
        r.refresh_pattern_caches();
        r
    }

    #[test]
    fn match_contains_is_case_insensitive() {
        let r = rule_with("Chrome", MatchType::Contains);
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
        let r = rule_with("steam", MatchType::Exact);
        assert!(r.matches("steam", &"steam".to_lowercase()));
        assert!(!r.matches("Steam", &"Steam".to_lowercase()));
        assert!(!r.matches("steamwebhelper", &"steamwebhelper".to_lowercase()));
        assert!(!r.matches("not-steam", &"not-steam".to_lowercase()));
    }

    #[test]
    fn match_regex_anchored_or_not() {
        let r = rule_with(r"^node(\.exe)?$", MatchType::Regex);
        assert!(r.matches("node", &"node".to_lowercase()));
        assert!(r.matches("node.exe", &"node.exe".to_lowercase()));
        assert!(!r.matches("nodejs", &"nodejs".to_lowercase()));
        assert!(!r.matches("my-node", &"my-node".to_lowercase()));
    }

    #[test]
    fn match_regex_invalid_pattern_returns_false() {
        // Invalid regex should fail safe (no match) rather than panic.
        let r = rule_with("[unclosed", MatchType::Regex);
        assert!(!r.matches("anything", &"anything".to_lowercase()));
    }

    #[test]
    fn empty_pattern_never_matches() {
        let r = rule_with("", MatchType::Contains);
        assert!(!r.matches("anything", &"anything".to_lowercase()));
    }

    #[test]
    fn disabled_rule_never_matches() {
        let mut r = rule_with("chrome", MatchType::Contains);
        r.enabled = false;
        assert!(!r.matches("chrome", &"chrome".to_lowercase()));
    }

    #[test]
    fn refresh_pattern_caches_recompiles_on_pattern_change() {
        let mut r = rule_with("foo", MatchType::Regex);
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
        let mut r = rule_with("foo", MatchType::Regex);
        assert!(r.matches("foo", &"foo".to_lowercase()));
        r.refresh_pattern_caches();
        r.refresh_pattern_caches();
        assert!(r.matches("foo", &"foo".to_lowercase()));
        assert!(!r.matches("bar", &"bar".to_lowercase()));
    }

    #[test]
    fn refresh_pattern_caches_clears_cache_when_leaving_regex() {
        let mut r = rule_with("foo", MatchType::Regex);
        assert!(r.cached_regex.is_some());
        r.match_type = MatchType::Contains;
        r.refresh_pattern_caches();
        assert!(r.cached_regex.is_none());
        // And back to regex recompiles from the current pattern.
        r.match_type = MatchType::Regex;
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
            match_type: MatchType::Contains,
            affinity: None,
            nice: None,
            ionice_class: None,
            ionice_level: None,
            enabled: true,
        };
        engine.load_rules(&[cfg]);
        // Non-matching name → empty actions, no syscalls attempted
        let actions = apply_rules(
            engine.get_rules(),
            0,
            "firefox",
            None,
            None,
            &mut std::collections::HashSet::new(),
            &|_| {},
        );
        assert!(actions.is_empty());
    }

    #[test]
    fn rule_config_json_round_trip() {
        let cfg = crate::config::RuleConfig {
            rule_id: "abc-123".into(),
            name: "Browser".into(),
            pattern: "chrome".into(),
            match_type: MatchType::Contains,
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
            match_type: MatchType::Regex,
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
    /// iteration).
    ///
    /// Both rules here target the *same* raised nice value. Under the fix,
    /// rule 2 sees rule 1's just-applied live value, recognizes its own
    /// target is already met, and skips — one action. Under the bug, rule 2
    /// compares against the frozen pre-pass snapshot (which does differ from
    /// its target) and wrongly fires a second, redundant syscall — two
    /// actions. This deliberately never asks the process to *lower* its own
    /// nice value (not even back to where it started): setpriority(2) lets
    /// an unprivileged process always raise its own nice value, but lowering
    /// it — even back to a value it held a moment ago — is governed by
    /// RLIMIT_NICE, which a locked-down CI runner enforces far more strictly
    /// than a typical desktop session. An earlier version of this test raised
    /// nice and then tried to restore the original value, which passed
    /// locally but failed deterministically in CI for exactly that reason.
    #[test]
    fn apply_rules_lets_a_later_rule_see_an_earlier_ones_just_applied_nice() {
        // This test renices the test binary's own process — see the lock's
        // own doc comment for why that needs serializing against sibling
        // tests that do the same (cpu_park.rs's renice_succeeds_when_..).
        let _guard = utils::PROCESS_NICE_TEST_LOCK.lock().unwrap();
        let pid = std::process::id();
        let starting = utils::get_nice(pid).unwrap_or(0);
        let target = starting + 1;

        let mut rule1 = rule_with("apply-rules-staleness-test", MatchType::Contains);
        rule1.rule_id = "r1".into();
        rule1.nice = Some(target);
        let mut rule2 = rule_with("apply-rules-staleness-test", MatchType::Contains);
        rule2.rule_id = "r2".into();
        rule2.nice = Some(target);

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
        // Best-effort cleanup only: lowering back to `starting` is exactly
        // the operation this test avoids relying on, so don't assert on it.
        let _ = utils::set_nice(pid, starting);

        assert_eq!(
            actions.len(),
            1,
            "the second rule must recognize its target is already met via \
             the first rule's live change, not re-fire against a stale \
             snapshot: {actions:?}"
        );
        assert_eq!(ended_at, Some(target));
    }
}
