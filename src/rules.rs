//! Rule dataclass and RuleEngine for matching and applying per-process rules.
//!
//! Mirrors Python rules.py exactly:
//!   - match_type: contains (case-insensitive), exact, regex
//!   - apply_rules merges ALL matching rules; the last one to set a value wins

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
            let v = ionice_target(class, rule.ionice_level);
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

/// A rule's nice or I/O priority change that failed for one process. It is
/// not retried every pass: a permission failure never heals by itself, and
/// retrying would spawn a syscall and a log line every 500 ms forever.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FailKey {
    rule_id: String,
    pub pid: u32,
    attr: Attr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Attr {
    Nice,
    Ionice,
}

pub type FailedChanges = std::collections::HashSet<FailKey>;

/// Enforce the rules matching one process. Standalone so the monitor daemon
/// can clone the rules and run enforcement WITHOUT holding the RuleEngine
/// mutex across procfs reads and renice/ionice subprocess spawns (the GUI
/// locks the same engine to edit rules and would freeze otherwise).
///
/// Matching rules are merged first, as the rules tab previews them: for each
/// attribute the last rule that sets it wins. Each attribute is then
/// dirty-checked and changed at most once, so two rules that disagree no
/// longer undo each other on every pass. `current_ionice` is only called when
/// a matching rule sets I/O priority.
pub fn apply_rules(
    rules: &[Rule],
    pid: u32,
    proc_name: &str,
    current_nice: Option<i32>,
    current_ionice: impl FnOnce() -> Option<(i32, i32)>,
    failed: &mut FailedChanges,
    log: &impl Fn(String),
) -> Vec<String> {
    let effect = preview_effect(rules, proc_name);
    let mut actions = Vec::new();
    let mut report = |msg: String| {
        log(msg.clone());
        actions.push(msg);
    };
    let last_setting =
        |sets: fn(&Rule) -> bool| effect.matches.iter().rev().copied().find(|r| sets(r));

    if let Some(rule) = last_setting(|r| r.affinity.is_some()) {
        let aff = rule.affinity.as_deref().unwrap_or_default();
        if utils::set_affinity_if_changed(pid, aff) {
            report(format!(
                "[Rule:{}] Set affinity={} on {}({})",
                rule.name, aff, proc_name, pid
            ));
        }
    }

    if let Some(rule) = last_setting(|r| r.nice.is_some()) {
        // The kernel clamps out-of-range values, so an unclamped target
        // would never read back as reached.
        let nice = rule.nice.unwrap_or_default().clamp(-20, 19);
        let key = FailKey {
            rule_id: rule.rule_id.clone(),
            pid,
            attr: Attr::Nice,
        };
        if current_nice != Some(nice) && !failed.contains(&key) {
            match utils::set_nice(pid, nice) {
                Ok(()) => report(format!(
                    "[Rule:{}] Set nice={} on {}({})",
                    rule.name, nice, proc_name, pid
                )),
                Err(e) => {
                    failed.insert(key);
                    report(format!(
                        "[Rule:{}] nice={} FAILED for {}({}): {e} — giving up for this process",
                        rule.name, nice, proc_name, pid
                    ));
                }
            }
        }
    }

    if let Some(rule) = last_setting(|r| r.ionice_class.is_some()) {
        let target = ionice_target(rule.ionice_class.unwrap_or_default(), rule.ionice_level);
        let key = FailKey {
            rule_id: rule.rule_id.clone(),
            pid,
            attr: Attr::Ionice,
        };
        if !failed.contains(&key) && current_ionice() != Some(target) {
            match utils::set_ionice(pid, target.0, Some(target.1)) {
                Ok(()) => report(format!(
                    "[Rule:{}] Set ionice class={} level={} on {}({})",
                    rule.name, target.0, target.1, proc_name, pid
                )),
                Err(e) => {
                    failed.insert(key);
                    report(format!(
                        "[Rule:{}] ionice class={} level={} FAILED for {}({}): {e} — giving up for this process",
                        rule.name, target.0, target.1, proc_name, pid
                    ));
                }
            }
        }
    }
    actions
}

/// The (class, level) a rule asks for, as the kernel will accept and report
/// it: class 0 ("none") takes no level, and a missing level means 0.
pub fn ionice_target(class: i32, level: Option<i32>) -> (i32, i32) {
    match class {
        0 => (0, 0),
        _ => (class, level.unwrap_or(0).clamp(0, 7)),
    }
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
            || None,
            &mut FailedChanges::new(),
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

    /// Two matching rules with the same nice target change it once, not
    /// once per rule against a stale snapshot. This deliberately never asks
    /// the process to *lower* its own nice value (not even back to where it
    /// started): setpriority(2) lets an unprivileged process always raise
    /// its own nice value, but lowering it — even back to a value it held a
    /// moment ago — is governed by RLIMIT_NICE, which a locked-down CI runner
    /// enforces far more strictly than a typical desktop session. An earlier
    /// version of this test raised nice and then tried to restore the
    /// original value, which passed locally but failed deterministically in
    /// CI for exactly that reason.
    #[test]
    fn rules_sharing_a_nice_target_change_it_once() {
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

        let mut nice_failed = FailedChanges::new();
        let actions = apply_rules(
            &[rule1, rule2],
            pid,
            "apply-rules-staleness-test",
            Some(starting),
            || None,
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
            "one merged change, not one per rule: {actions:?}"
        );
        assert_eq!(ended_at, Some(target));
    }

    /// Two rules that disagree used to undo each other on every pass: set
    /// the first rule's value, then the second's, forever. The merged effect
    /// sets the last rule's value once and then leaves it alone.
    #[test]
    fn disagreeing_rules_settle_on_the_last_one() {
        let _guard = utils::PROCESS_NICE_TEST_LOCK.lock().unwrap();
        let pid = std::process::id();
        let starting = utils::get_nice(pid).unwrap_or(0);
        if starting + 2 > 19 {
            return;
        }
        let mut first = rule_with("apply-rules-disagree-test", MatchType::Contains);
        first.rule_id = "first".into();
        first.nice = Some(starting + 2);
        let mut last = rule_with("apply-rules-disagree-test", MatchType::Contains);
        last.rule_id = "last".into();
        last.nice = Some(starting + 1);
        let rules = [first, last];
        let mut failed = FailedChanges::new();
        let pass = |current: Option<i32>, failed: &mut FailedChanges| {
            apply_rules(
                &rules,
                pid,
                "apply-rules-disagree-test",
                current,
                || None,
                failed,
                &|_| {},
            )
        };

        assert_eq!(pass(Some(starting), &mut failed).len(), 1);
        assert_eq!(utils::get_nice(pid), Some(starting + 1));
        assert!(pass(utils::get_nice(pid), &mut failed).is_empty());
    }

    /// I/O class 0 ("none") takes no level: asking for level 4 with it is
    /// EINVAL, which the editor's defaults produced.
    #[test]
    fn ionice_targets_are_what_the_kernel_accepts() {
        assert_eq!(ionice_target(0, Some(4)), (0, 0));
        assert_eq!(ionice_target(2, None), (2, 0));
        assert_eq!(ionice_target(2, Some(9)), (2, 7));
    }

    /// A refused I/O priority change is reported once and not retried each
    /// pass, as nice failures already were.
    #[test]
    fn a_refused_ionice_change_is_logged_once() {
        // Real-time I/O class needs CAP_SYS_ADMIN or CAP_SYS_NICE.
        // SAFETY: getuid(2) cannot fail and takes no arguments.
        if unsafe { nix::libc::getuid() } == 0 {
            return;
        }
        let mut rule = rule_with("apply-rules-ionice-test", MatchType::Contains);
        rule.ionice_class = Some(1);
        rule.ionice_level = Some(0);
        let rules = [rule];
        let pid = std::process::id();
        let mut failed = FailedChanges::new();
        let reads = std::cell::Cell::new(0);
        let mut pass = || {
            apply_rules(
                &rules,
                pid,
                "apply-rules-ionice-test",
                None,
                || {
                    reads.set(reads.get() + 1);
                    None
                },
                &mut failed,
                &|_| {},
            )
        };
        let first = pass();
        assert_eq!(first.len(), 1);
        assert!(first[0].contains("FAILED"), "{first:?}");
        assert!(pass().is_empty());
        assert_eq!(reads.get(), 1, "a known failure needs no ioprio read");
    }
}
