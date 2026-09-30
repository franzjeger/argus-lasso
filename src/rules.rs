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

    /// Why the pattern matches nothing, if it is an invalid regular
    /// expression: the compiler's own message, shortened to its last line.
    pub fn pattern_error(&self) -> Option<&str> {
        match &self.cached_regex {
            Some(Err(e)) => Some(regex_error_summary(e)),
            _ => None,
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
}

/// Whether any enabled rule matches this process name — independent of
/// whether applying it would change anything right now. Callers deciding
/// between "rule-managed" and "apply default affinity" must use this, not
/// the action list from apply_rules (an already-correct process yields no
/// actions but is still rule-managed).
pub fn any_matches(rules: &[Rule], proc_name: &str) -> bool {
    let lower = proc_name.to_lowercase();
    rules.iter().any(|r| r.matches(proc_name, &lower))
}

/// The regex crate's multi-line message ends with the actual reason.
fn regex_error_summary(message: &str) -> &str {
    let last = message.lines().last().unwrap_or(message).trim();
    last.strip_prefix("error: ").unwrap_or(last)
}

/// What is wrong with a rule that did not come from the editor, whose
/// controls cannot produce any of this. The kernel clamps an out-of-range
/// nice value and rejects a bad CPU list or I/O priority, so such a rule
/// would be re-applied, or fail, on every pass.
pub fn rule_problem(rule: &RuleConfig) -> Option<String> {
    if rule.pattern.is_empty() {
        return Some("it has no pattern".into());
    }
    if rule.match_type == MatchType::Regex {
        if let Err(e) = Regex::new(&rule.pattern) {
            let e = e.to_string();
            return Some(format!(
                "its pattern is not a valid regular expression ({})",
                regex_error_summary(&e)
            ));
        }
    }
    if let Some(affinity) = &rule.affinity {
        let usable = utils::cpulist_to_set(affinity).is_ok_and(|cpus| {
            !cpus.is_empty()
                && cpus
                    .iter()
                    .all(|&cpu| (cpu as usize) < nix::sched::CpuSet::count())
        });
        if !usable {
            return Some(format!("CPU list \"{affinity}\" is not valid"));
        }
    }
    if let Some(nice) = rule.nice.filter(|n| !(-20..=19).contains(n)) {
        return Some(format!("nice {nice} is outside -20 to 19"));
    }
    if let Some(class) = rule.ionice_class.filter(|c| !(0..=3).contains(c)) {
        return Some(format!("I/O class {class} is outside 0 to 3"));
    }
    if let Some(level) = rule.ionice_level.filter(|l| !(0..=7).contains(l)) {
        return Some(format!("I/O level {level} is outside 0 to 7"));
    }
    None
}

/// Rules read from an import file, ready to add after `existing`: the usable
/// ones, each with an ID no other rule has (importing an export of these
/// same rules would otherwise give every rule a twin that edits, toggles and
/// deletes along with it), and a line for each one skipped.
pub fn prepare_import(imported: Vec<RuleConfig>, existing: &[Rule]) -> (Vec<Rule>, Vec<String>) {
    let mut ids: std::collections::HashSet<String> =
        existing.iter().map(|r| r.rule_id.clone()).collect();
    let mut rules = Vec::new();
    let mut skipped = Vec::new();
    for mut config in imported {
        if let Some(problem) = rule_problem(&config) {
            let name = if config.name.is_empty() {
                &config.pattern
            } else {
                &config.name
            };
            skipped.push(format!("\"{name}\": {problem}"));
            continue;
        }
        if config.rule_id.is_empty() || ids.contains(&config.rule_id) {
            config.rule_id = uuid::Uuid::new_v4().to_string();
        }
        ids.insert(config.rule_id.clone());
        rules.push(Rule::from_config(&config));
    }
    (rules, skipped)
}

/// A rule's nice or I/O priority change that failed for one process. It is
/// not retried every pass: a permission failure never heals by itself, and
/// retrying would spawn a syscall and a log line every 500 ms forever.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FailKey {
    rule_id: String,
    pid: u32,
    attr: Attr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Attr {
    Nice,
    Ionice,
}

/// What the daemon enforces: the rules, and the affinity for processes no
/// rule matches.
pub struct Policy<'a> {
    pub rules: &'a [Rule],
    pub default_affinity: Option<&'a str>,
}

/// One process as enforcement sees it.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    pub pid: u32,
    /// Tells the process a change was made to from a later one given its PID.
    pub start_ticks: u64,
    pub name: &'a str,
    pub nice: Option<i32>,
    /// The nice the process's threads had before another part of Argus
    /// (ProBalance) changed it. A rule that takes the value over records
    /// this as what to put back, not the other part's temporary value.
    pub held_nice: Option<&'a utils::ThreadNices>,
}

/// Who a recorded change belongs to, and so what puts it back: rules (and
/// the default affinity) when no rule asks for the value any more, Gaming
/// Mode when it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Rules,
    Gaming,
}

/// A value Argus set, what the process had before, and who set it (the log
/// prefix, such as `[Rule:games]` or `[Default]`, and the owner).
#[derive(Debug)]
struct Undo<T, O = T> {
    original: O,
    applied: T,
    by: String,
    owner: Owner,
}

impl<T, O> Undo<T, O> {
    /// Keep the first original through later changes; the latest change
    /// owns the value.
    fn record(slot: &mut Option<Self>, original: O, applied: T, by: String, owner: Owner) {
        match slot {
            Some(undo) => {
                undo.applied = applied;
                undo.by = by;
                undo.owner = owner;
            }
            None => {
                *slot = Some(Undo {
                    original,
                    applied,
                    by,
                    owner,
                })
            }
        }
    }
}

#[derive(Debug, Default)]
struct Changes {
    start_ticks: u64,
    affinity: Option<Undo<String>>,
    /// Each thread's value from before, since they can differ.
    nice: Option<Undo<i32, utils::ThreadNices>>,
    ionice: Option<Undo<(i32, i32)>>,
}

impl Changes {
    fn is_empty(&self) -> bool {
        self.affinity.is_none() && self.nice.is_none() && self.ionice.is_none()
    }
}

/// What enforcement changed on each process, so that a value no rule asks
/// for any more is put back, and which changes failed, so that they are not
/// retried every pass.
#[derive(Debug, Default)]
pub struct RuleState {
    failed: std::collections::HashSet<FailKey>,
    changes: std::collections::HashMap<u32, Changes>,
}

impl RuleState {
    /// The rules changed: failed changes get another chance.
    pub fn rules_changed(&mut self) {
        self.failed.clear();
    }

    /// Whether some change could still need undoing.
    pub fn has_changes(&self) -> bool {
        !self.changes.is_empty()
    }

    /// Forget processes that exited, including a PID now used by another
    /// process (`live` maps PID to start time).
    pub fn retain_live(&mut self, live: &std::collections::HashMap<u32, u64>) {
        self.failed.retain(|failed| live.contains_key(&failed.pid));
        self.changes
            .retain(|pid, changes| live.get(pid) == Some(&changes.start_ticks));
    }

    /// The changes recorded for this process, starting over for a new one.
    fn changes(&mut self, target: Target) -> &mut Changes {
        let changes = self.changes.entry(target.pid).or_default();
        if changes.start_ticks != target.start_ticks {
            *changes = Changes {
                start_ticks: target.start_ticks,
                ..Changes::default()
            };
        }
        changes
    }

    fn recorded(&self, target: Target) -> Option<&Changes> {
        self.changes
            .get(&target.pid)
            .filter(|changes| changes.start_ticks == target.start_ticks)
    }

    /// Take `owner`'s recorded change to undo it, without creating a record.
    fn take<T, O>(
        &mut self,
        target: Target,
        slot: fn(&mut Changes) -> &mut Option<Undo<T, O>>,
        owner: Owner,
    ) -> Option<Undo<T, O>> {
        let changes = self.changes.get_mut(&target.pid)?;
        if changes.start_ticks != target.start_ticks {
            return None;
        }
        slot(changes).take_if(|undo| undo.owner == owner)
    }

    /// Set `affinity` on a process if it differs, remembering what it had.
    fn set_affinity(&mut self, target: Target, affinity: &str, by: String, owner: Owner) -> bool {
        let original = match self.recorded(target).and_then(|c| c.affinity.as_ref()) {
            Some(undo) => undo.original.clone(),
            None => utils::get_affinity_to_restore(target.pid),
        };
        let changed = utils::set_affinity_if_changed(target.pid, affinity);
        if changed {
            let slot = &mut self.changes(target).affinity;
            Undo::record(slot, original, affinity.to_string(), by, owner);
        }
        changed
    }

    /// Gaming Mode moved a process to the preferred cores.
    pub fn gaming_pin(&mut self, target: Target, cpulist: &str) -> bool {
        self.set_affinity(target, cpulist, "[Gaming Mode]".into(), Owner::Gaming)
    }

    /// Gaming Mode raised a process's priority from `original` to `applied`.
    pub fn record_gaming_nice(
        &mut self,
        target: Target,
        original: utils::ThreadNices,
        applied: i32,
    ) {
        let slot = &mut self.changes(target).nice;
        Undo::record(
            slot,
            original,
            applied,
            "[Gaming Mode]".into(),
            Owner::Gaming,
        );
    }

    /// Put back what `owner` changed, on every process, where the value is
    /// still the one it set. Returns how many values were put back.
    pub fn end(&mut self, owner: Owner, log: &impl Fn(String)) -> usize {
        let mut restored = 0;
        for (&pid, changes) in self.changes.iter_mut() {
            if let Some(undo) = changes.affinity.take_if(|undo| undo.owner == owner) {
                if utils::affinity_matches(pid, &undo.applied)
                    && utils::set_affinity(pid, &undo.original).is_ok()
                {
                    restored += 1;
                }
            }
            if let Some(undo) = changes.nice.take_if(|undo| undo.owner == owner) {
                if utils::get_nice(pid) == Some(undo.applied) && undo.original.restore(pid).is_ok()
                {
                    restored += 1;
                }
            }
        }
        self.changes.retain(|_, changes| !changes.is_empty());
        if restored > 0 {
            let who = match owner {
                Owner::Rules => "[Rules]",
                Owner::Gaming => "[Gaming Mode]",
            };
            log(format!("{who} Restored {restored} values it had set."));
        }
        restored
    }

    /// Put back every affinity Argus set, as asked by "Restore all CPU
    /// assignments": whatever still asks for it applies it again.
    pub fn restore_all_affinities(&mut self) -> usize {
        let mut restored = 0;
        for (&pid, changes) in self.changes.iter_mut() {
            if let Some(undo) = changes.affinity.take() {
                if utils::set_affinity(pid, &undo.original).is_ok() {
                    restored += 1;
                }
            }
        }
        self.changes.retain(|_, changes| !changes.is_empty());
        restored
    }

    /// Apply the default affinity to a process no rule matches.
    pub fn apply_default_affinity(
        &mut self,
        target: Target,
        affinity: &str,
        log: &impl Fn(String),
    ) {
        if self.set_affinity(target, affinity, "[Default]".into(), Owner::Rules) {
            log(format!(
                "[Default] affinity={affinity} → {}({})",
                target.name, target.pid
            ));
        }
    }

    /// Drop a process's record once nothing is left to undo.
    fn tidy(&mut self, pid: u32) {
        if self.changes.get(&pid).is_some_and(Changes::is_empty) {
            self.changes.remove(&pid);
        }
    }
}

/// Enforce the rules matching one process. Standalone so the monitor daemon
/// can clone the rules and run enforcement WITHOUT holding the RuleEngine
/// mutex across procfs reads and renice/ionice subprocess spawns (the GUI
/// locks the same engine to edit rules and would freeze otherwise).
///
/// Matching rules are merged first, as the rules tab previews them: for each
/// attribute the last rule that sets it wins. Each attribute is then
/// dirty-checked and changed at most once, so two rules that disagree no
/// longer undo each other on every pass. `current_ionice` is only called when
/// I/O priority is set or put back.
///
/// A value enforcement set earlier that nothing asks for any more (the rule
/// was disabled, deleted or edited, or the default affinity was cleared) is
/// put back to what the process had, unless something else has changed it
/// since. A default affinity is kept while it is configured and no rule
/// matches the process.
pub fn apply_rules(
    policy: &Policy,
    target: Target,
    current_ionice: impl FnOnce() -> Option<(i32, i32)>,
    state: &mut RuleState,
    log: &impl Fn(String),
) -> Vec<String> {
    let effect = preview_effect(policy.rules, target.name);
    let (pid, name) = (target.pid, target.name);
    let mut actions = Vec::new();
    let mut report = |msg: String| {
        log(msg.clone());
        actions.push(msg);
    };
    let last_setting =
        |sets: fn(&Rule) -> bool| effect.matches.iter().rev().copied().find(|r| sets(r));

    if let Some(rule) = last_setting(|r| r.affinity.is_some()) {
        let aff = rule.affinity.as_deref().unwrap_or_default();
        let by = format!("[Rule:{}]", rule.name);
        if state.set_affinity(target, aff, by.clone(), Owner::Rules) {
            report(format!("{by} Set affinity={aff} on {name}({pid})"));
        }
    } else if effect.matches.is_empty() && policy.default_affinity.is_some() {
        // The default's to keep; it is applied to new processes only.
    } else if let Some(undo) = state.take(target, |c| &mut c.affinity, Owner::Rules) {
        if utils::affinity_matches(pid, &undo.applied) {
            let (value, by) = (&undo.original, &undo.by);
            report(match utils::set_affinity(pid, value) {
                Ok(()) => format!("{by} Restored affinity={value} on {name}({pid})"),
                Err(e) => format!("{by} Restoring affinity={value} FAILED for {name}({pid}): {e}"),
            });
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
        if target.nice != Some(nice) && !state.failed.contains(&key) {
            let before = target
                .held_nice
                .cloned()
                .or_else(|| utils::ThreadNices::read(pid));
            match utils::set_nice(pid, nice) {
                Ok(()) => {
                    let by = format!("[Rule:{}]", rule.name);
                    report(format!("{by} Set nice={nice} on {name}({pid})"));
                    if let Some(original) = before {
                        let slot = &mut state.changes(target).nice;
                        Undo::record(slot, original, nice, by, Owner::Rules);
                    }
                }
                Err(e) => {
                    state.failed.insert(key);
                    report(format!(
                        "[Rule:{}] nice={} FAILED for {}({}): {e} — giving up for this process",
                        rule.name, nice, name, pid
                    ));
                }
            }
        }
    } else if let Some(undo) = state.take(target, |c| &mut c.nice, Owner::Rules) {
        if target.nice == Some(undo.applied) {
            let (value, by) = (undo.original.main(), &undo.by);
            report(match undo.original.restore(pid) {
                Ok(()) => format!("{by} Restored nice={value} on {name}({pid})"),
                Err(e) => format!("{by} Restoring nice={value} FAILED for {name}({pid}): {e}"),
            });
        }
    }

    if let Some(rule) = last_setting(|r| r.ionice_class.is_some()) {
        let wanted = ionice_target(rule.ionice_class.unwrap_or_default(), rule.ionice_level);
        let key = FailKey {
            rule_id: rule.rule_id.clone(),
            pid,
            attr: Attr::Ionice,
        };
        let current = if state.failed.contains(&key) {
            Some(wanted)
        } else {
            current_ionice()
        };
        if !current.is_some_and(|c| ionice_reached(c, wanted)) {
            let (class, level) = wanted;
            match utils::set_ionice(pid, class, Some(level)) {
                Ok(()) => {
                    let by = format!("[Rule:{}]", rule.name);
                    report(format!(
                        "{by} Set ionice class={class} level={level} on {name}({pid})"
                    ));
                    if let Some(original) = current {
                        let slot = &mut state.changes(target).ionice;
                        Undo::record(slot, original, wanted, by, Owner::Rules);
                    }
                }
                Err(e) => {
                    state.failed.insert(key);
                    report(format!(
                        "[Rule:{}] ionice class={class} level={level} FAILED for {name}({pid}): {e} — giving up for this process",
                        rule.name
                    ));
                }
            }
        }
    } else if let Some(undo) = state.take(target, |c| &mut c.ionice, Owner::Rules) {
        if current_ionice().is_some_and(|c| ionice_reached(c, undo.applied)) {
            let ((class, level), by) = (undo.original, &undo.by);
            report(match utils::set_ionice(pid, class, Some(level)) {
                Ok(()) => format!("{by} Restored ionice class={class} level={level} on {name}({pid})"),
                Err(e) => format!(
                    "{by} Restoring ionice class={class} level={level} FAILED for {name}({pid}): {e}"
                ),
            });
        }
    }
    state.tidy(pid);
    actions
}

/// Class 0 ("none") follows the nice value; older kernels report a level
/// with it, which is not a difference worth a syscall.
fn ionice_reached(current: (i32, i32), wanted: (i32, i32)) -> bool {
    current == wanted || (current.0 == 0 && wanted.0 == 0)
}

/// The (class, level) a rule asks for, as the kernel will accept and report
/// it. Only real-time (1) and best-effort (2) have levels; "none" (0) rejects
/// one and idle (3) ignores it. A missing level means 4, the normal level
/// and the one ionice(1) and the rule editor use.
pub fn ionice_target(class: i32, level: Option<i32>) -> (i32, i32) {
    match class {
        1 | 2 => (class, level.unwrap_or(4).clamp(0, 7)),
        _ => (class, 0),
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

    fn only(rules: &[Rule]) -> Policy<'_> {
        Policy {
            rules,
            default_affinity: None,
        }
    }

    fn process(pid: u32, name: &str, nice: Option<i32>) -> Target<'_> {
        Target {
            pid,
            start_ticks: 0,
            name,
            nice,
            held_nice: None,
        }
    }

    /// A `sleep` to change, killed when dropped.
    struct Sleeper(std::process::Child);

    impl Sleeper {
        fn spawn() -> Self {
            Self(
                std::process::Command::new("sleep")
                    .arg("30")
                    .spawn()
                    .unwrap(),
            )
        }

        fn target(&self) -> Target<'static> {
            process(self.0.id(), "argus-undo-test", utils::get_nice(self.0.id()))
        }

        /// One enforcement pass, returning what it did.
        fn enforce(&self, policy: &Policy, state: &mut RuleState) -> Vec<String> {
            let pid = self.0.id();
            apply_rules(
                policy,
                self.target(),
                || utils::get_ionice_raw(pid),
                state,
                &|_| {},
            )
        }
    }

    impl Drop for Sleeper {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
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
            &only(engine.get_rules()),
            process(0, "firefox", None),
            || None,
            &mut RuleState::default(),
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

        let actions = apply_rules(
            &only(&[rule1, rule2]),
            process(pid, "apply-rules-staleness-test", Some(starting)),
            || None,
            &mut RuleState::default(),
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
        let mut state = RuleState::default();
        let mut pass = |nice: Option<i32>| {
            let target = process(pid, "apply-rules-disagree-test", nice);
            apply_rules(&only(&rules), target, || None, &mut state, &|_| {})
        };

        assert_eq!(pass(Some(starting)).len(), 1);
        assert_eq!(utils::get_nice(pid), Some(starting + 1));
        assert!(pass(utils::get_nice(pid)).is_empty());
    }

    /// I/O class 0 ("none") takes no level: asking for level 4 with it is
    /// EINVAL, which the editor's defaults produced.
    #[test]
    fn ionice_targets_are_what_the_kernel_accepts() {
        assert_eq!(ionice_target(0, Some(4)), (0, 0));
        assert_eq!(ionice_target(3, Some(4)), (3, 0));
        assert_eq!(ionice_target(2, None), (2, 4), "what the editor shows");
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
        let mut state = RuleState::default();
        let reads = std::cell::Cell::new(0);
        let mut pass = || {
            let read = || {
                reads.set(reads.get() + 1);
                None
            };
            let target = process(pid, "apply-rules-ionice-test", None);
            apply_rules(&only(&rules), target, read, &mut state, &|_| {})
        };
        let first = pass();
        assert_eq!(first.len(), 1);
        assert!(first[0].contains("FAILED"), "{first:?}");
        assert!(pass().is_empty());
        assert_eq!(reads.get(), 1, "a known failure needs no ioprio read");
    }

    /// Disabling a rule used to leave its affinity and I/O priority on the
    /// process until it exited.
    #[test]
    fn a_disabled_rule_puts_back_what_it_changed() {
        let sleeper = Sleeper::spawn();
        let pid = sleeper.0.id();
        let affinity = utils::get_affinity_str(pid);
        let cpus = utils::cpulist_to_set(&affinity).unwrap();
        let Some(&cpu) = cpus.iter().min().filter(|_| cpus.len() > 1) else {
            eprintln!("needs two CPUs to change affinity");
            return;
        };
        let io_before = utils::get_ionice_raw(pid).unwrap();
        let mut rule = rule_with("argus-undo-test", MatchType::Exact);
        rule.affinity = Some(cpu.to_string());
        rule.ionice_class = Some(2);
        rule.ionice_level = Some(7);
        let mut rules = [rule];
        let mut state = RuleState::default();

        assert_eq!(sleeper.enforce(&only(&rules), &mut state).len(), 2);
        assert_eq!(utils::get_affinity_str(pid), cpu.to_string());
        assert_eq!(utils::get_ionice_raw(pid), Some((2, 7)));

        rules[0].enabled = false;
        let restored = sleeper.enforce(&only(&rules), &mut state);
        assert_eq!(restored.len(), 2, "{restored:?}");
        assert!(
            restored.iter().all(|a| a.contains("Restored")),
            "{restored:?}"
        );
        assert_eq!(utils::get_affinity_str(pid), affinity);
        assert!(ionice_reached(
            utils::get_ionice_raw(pid).unwrap(),
            io_before
        ));
        assert!(!state.has_changes());
        assert!(sleeper.enforce(&only(&rules), &mut state).is_empty());
    }

    /// A value someone else set after the rule, such as a manual change from
    /// the process table, stays when the rule goes. So does anything on a
    /// later process given the same PID.
    #[test]
    fn only_a_value_still_as_set_is_put_back() {
        let sleeper = Sleeper::spawn();
        let pid = sleeper.0.id();
        let mut rule = rule_with("argus-undo-test", MatchType::Exact);
        rule.ionice_class = Some(2);
        rule.ionice_level = Some(7);
        let mut rules = [rule];
        let mut state = RuleState::default();
        assert_eq!(sleeper.enforce(&only(&rules), &mut state).len(), 1);

        utils::set_ionice(pid, 2, Some(5)).unwrap();
        rules[0].enabled = false;
        assert!(sleeper.enforce(&only(&rules), &mut state).is_empty());
        assert_eq!(utils::get_ionice_raw(pid), Some((2, 5)));
        assert!(!state.has_changes());

        rules[0].enabled = true;
        sleeper.enforce(&only(&rules), &mut state);
        rules[0].enabled = false;
        let mut later = sleeper.target();
        later.start_ticks += 1;
        let read = || utils::get_ionice_raw(pid);
        assert!(apply_rules(&only(&rules), later, read, &mut state, &|_| {}).is_empty());
        assert_eq!(utils::get_ionice_raw(pid), Some((2, 7)));
    }

    /// The default affinity stays while it is set and no rule matches, and
    /// goes when it is cleared.
    #[test]
    fn a_cleared_default_affinity_is_put_back() {
        let sleeper = Sleeper::spawn();
        let pid = sleeper.0.id();
        let affinity = utils::get_affinity_str(pid);
        let cpus = utils::cpulist_to_set(&affinity).unwrap();
        let Some(cpu) = cpus.iter().min().filter(|_| cpus.len() > 1) else {
            eprintln!("needs two CPUs to change affinity");
            return;
        };
        let cpu = cpu.to_string();
        let mut state = RuleState::default();
        state.apply_default_affinity(sleeper.target(), &cpu, &|_| {});
        assert_eq!(utils::get_affinity_str(pid), cpu);

        let with_default = Policy {
            rules: &[],
            default_affinity: Some(&cpu),
        };
        assert!(sleeper.enforce(&with_default, &mut state).is_empty());
        assert_eq!(utils::get_affinity_str(pid), cpu);

        let restored = sleeper.enforce(&only(&[]), &mut state);
        assert_eq!(restored.len(), 1, "{restored:?}");
        assert!(
            restored[0].starts_with("[Default] Restored"),
            "{restored:?}"
        );
        assert_eq!(utils::get_affinity_str(pid), affinity);
    }

    fn config(name: &str) -> RuleConfig {
        RuleConfig {
            name: name.into(),
            pattern: name.into(),
            ..RuleConfig::default()
        }
    }

    /// Importing an export of the current rules used to give every rule a
    /// twin with the same ID.
    #[test]
    fn imported_rules_get_ids_of_their_own() {
        let mine = Rule::from_config(&config("mine"));
        let mut again = config("again");
        again.rule_id = mine.rule_id.clone();
        let twice = config("twice");
        let mut unnamed = config("unnamed");
        unnamed.rule_id.clear();

        let (rules, skipped) = prepare_import(
            vec![again, twice.clone(), twice, unnamed],
            std::slice::from_ref(&mine),
        );

        assert!(skipped.is_empty());
        let mut ids: Vec<&str> = rules.iter().map(|r| r.rule_id.as_str()).collect();
        ids.push(&mine.rule_id);
        let unique: std::collections::HashSet<&&str> = ids.iter().collect();
        assert_eq!(unique.len(), 5);
        assert!(ids.iter().all(|id| !id.is_empty()));
    }

    #[test]
    fn imported_rules_the_kernel_would_refuse_are_skipped() {
        let mut far = config("far");
        far.nice = Some(50);
        let mut garbled = config("garbled");
        garbled.affinity = Some("abc".into());
        let mut regex = config("regex");
        regex.match_type = MatchType::Regex;
        regex.pattern = "(unclosed".into();
        let mut io = config("io");
        io.ionice_level = Some(8);
        let mut fine = config("fine");
        fine.nice = Some(19);
        fine.affinity = Some("0-1".into());

        let (rules, skipped) = prepare_import(vec![far, garbled, regex, io, fine], &[]);

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "fine");
        assert_eq!(skipped.len(), 4, "{skipped:?}");
        assert!(skipped[0].contains("nice 50"), "{skipped:?}");
        assert!(skipped[1].contains("\"abc\""), "{skipped:?}");
        assert!(skipped[2].contains("unclosed group"), "{skipped:?}");
        assert!(skipped[3].contains("level 8"), "{skipped:?}");
    }

    #[test]
    fn an_invalid_regex_says_why() {
        let mut rule = rule_with("(unclosed", MatchType::Regex);
        assert_eq!(rule.pattern_error(), Some("unclosed group"));
        rule.pattern = "ok".into();
        rule.refresh_pattern_caches();
        assert_eq!(rule.pattern_error(), None);
    }

    /// A rule taking over the nice of a process ProBalance had throttled
    /// recorded the throttle value as the original, so deleting the rule
    /// left the process throttled for good.
    #[test]
    fn a_rule_puts_back_the_value_from_before_a_throttle() {
        let sleeper = Sleeper::spawn();
        let pid = sleeper.0.id();
        // The throttle; then a rule; all raising, so no privilege is needed.
        utils::set_nice(pid, 10).unwrap();
        let mut rule = rule_with("argus-undo-test", MatchType::Exact);
        rule.nice = Some(12);
        let mut rules = [rule];
        let mut state = RuleState::default();
        let mut target = sleeper.target();
        let held = utils::ThreadNices::uniform(13);
        target.held_nice = Some(&held);
        let read = || None;
        apply_rules(&only(&rules), target, read, &mut state, &|_| {});
        assert_eq!(utils::get_nice(pid), Some(12));

        rules[0].enabled = false;
        sleeper.enforce(&only(&rules), &mut state);
        assert_eq!(utils::get_nice(pid), Some(13), "not the throttle's 10");
    }

    /// Gaming Mode's preferred-core pin was never undone, and rule
    /// enforcement must not take it for its own and undo it either.
    #[test]
    fn gaming_mode_puts_back_its_own_changes_when_it_ends() {
        let sleeper = Sleeper::spawn();
        let pid = sleeper.0.id();
        let affinity = utils::get_affinity_str(pid);
        let cpus = utils::cpulist_to_set(&affinity).unwrap();
        let Some(cpu) = cpus.iter().min().filter(|_| cpus.len() > 1) else {
            eprintln!("needs two CPUs to change affinity");
            return;
        };
        let mut state = RuleState::default();
        assert!(state.gaming_pin(sleeper.target(), &cpu.to_string()));
        // A matching rule that sets something else leaves the pin alone.
        let mut rule = rule_with("argus-undo-test", MatchType::Exact);
        rule.ionice_class = Some(2);
        rule.ionice_level = Some(7);
        sleeper.enforce(&only(&[rule]), &mut state);
        assert_eq!(utils::get_affinity_str(pid), cpu.to_string());

        // A nice Gaming Mode raised (simulated with a raise from 10 to 12
        // being "put back", so no privilege is needed).
        utils::set_nice(pid, 10).unwrap();
        state.record_gaming_nice(sleeper.target(), utils::ThreadNices::uniform(12), 10);

        assert_eq!(state.end(Owner::Gaming, &|_| {}), 2);
        assert_eq!(utils::get_affinity_str(pid), affinity);
        assert_eq!(utils::get_nice(pid), Some(12));
        assert!(state.has_changes(), "the rule's I/O priority is the rule's");
        assert_eq!(utils::get_ionice_raw(pid), Some((2, 7)));
    }

    /// Gaming Mode's nice used to be put back unconditionally, over a
    /// manual change made since.
    #[test]
    fn gaming_mode_leaves_a_value_changed_since() {
        let sleeper = Sleeper::spawn();
        let pid = sleeper.0.id();
        utils::set_nice(pid, 10).unwrap();
        let mut state = RuleState::default();
        state.record_gaming_nice(sleeper.target(), utils::ThreadNices::uniform(12), 10);
        utils::set_nice(pid, 15).unwrap();
        assert_eq!(state.end(Owner::Gaming, &|_| {}), 0);
        assert_eq!(utils::get_nice(pid), Some(15));
    }

    /// "Restore all CPU assignments" puts back what Argus set, and only that.
    #[test]
    fn restoring_all_affinities_touches_only_what_argus_changed() {
        let changed = Sleeper::spawn();
        let untouched = Sleeper::spawn();
        let affinity = utils::get_affinity_str(changed.0.id());
        let cpus = utils::cpulist_to_set(&affinity).unwrap();
        let Some(cpu) = cpus.iter().min().filter(|_| cpus.len() > 1) else {
            eprintln!("needs two CPUs to change affinity");
            return;
        };
        let cpu = cpu.to_string();
        utils::set_affinity(untouched.0.id(), &cpu).unwrap();
        let mut state = RuleState::default();
        state.apply_default_affinity(changed.target(), &cpu, &|_| {});

        assert_eq!(state.restore_all_affinities(), 1);
        assert_eq!(utils::get_affinity_str(changed.0.id()), affinity);
        assert_eq!(
            utils::get_affinity_str(untouched.0.id()),
            cpu,
            "not Argus's to reset"
        );
    }

    /// Every restore used to set all threads to the main thread's original,
    /// raising the priority of threads the process had lowered itself.
    #[test]
    fn undoing_a_rule_puts_each_thread_back_to_its_own_nice() {
        // A child process with a worker thread niced above the main one:
        // python can set a thread's nice through its TID.
        let script = "import os,threading,time\n\
            def w():\n    os.setpriority(os.PRIO_PROCESS, threading.get_native_id(), 10); time.sleep(30)\n\
            threading.Thread(target=w).start(); time.sleep(30)\n";
        let Ok(mut child) = std::process::Command::new("python3")
            .args(["-c", script])
            .spawn()
        else {
            eprintln!("needs python3");
            return;
        };
        let pid = child.id();
        let thread_nices = || {
            let mut nices: Vec<i32> = utils::get_tids(pid)
                .into_iter()
                .filter_map(utils::get_nice)
                .collect();
            nices.sort();
            nices
        };
        // Until python has started the thread and lowered it.
        for _ in 0..100 {
            if thread_nices() == [0, 10] {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let before = thread_nices();
        let mut rule = rule_with("argus-thread-test", MatchType::Exact);
        rule.nice = Some(12);
        let mut rules = [rule];
        let mut state = RuleState::default();
        let target = process(pid, "argus-thread-test", utils::get_nice(pid));
        apply_rules(&only(&rules), target, || None, &mut state, &|_| {});
        let during = thread_nices();
        rules[0].enabled = false;
        let target = process(pid, "argus-thread-test", utils::get_nice(pid));
        apply_rules(&only(&rules), target, || None, &mut state, &|_| {});
        let after = thread_nices();
        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(before, [0, 10], "the setup");
        assert_eq!(during, [12, 12]);
        // Needs lowering 12 back to 0 and 10, allowed where RLIMIT_NICE is.
        if after != [12, 12] {
            assert_eq!(after, before);
        }
    }
}
