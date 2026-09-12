import re

with open("src/app.rs", "r") as f:
    text = f.read()

# Remove RuleOffer
text = re.sub(r'// ── "Remember settings" offer ─────────────────────────────────────────────────\n+/// .*?\n/// .*?\nstruct RuleOffer \{.*?\n    \}\n\}\n+', '', text, flags=re.DOTALL)

# Replace fields in ArgusLassoApp
fields_to_replace = """    affinity_dialog: Option<(u32, AffinityDialog)>,
    nice_dialog: Option<(u32, NiceDialog)>,
    ionice_dialog: Option<(u32, IoNiceDialog)>,

    // Process count for tab title"""

new_fields = """    dialog_manager: crate::gui::dialog_manager::DialogManager,

    // Process count for tab title"""
text = text.replace(fields_to_replace, new_fields)

fields_to_replace2 = """    rule_offer: Option<RuleOffer>,
    // Per-process details window (opened by double-clicking a row)
    detail_pid: Option<u32>,
    detail_info: Option<utils::ProcDetails>,
    detail_last_gen: u64,"""
new_fields2 = """    detail_window: crate::gui::detail_window::DetailWindow,"""
text = text.replace(fields_to_replace2, new_fields2)

init_to_replace = """            affinity_dialog: None,
            nice_dialog: None,
            ionice_dialog: None,
            proc_count: 0,"""
new_init = """            dialog_manager: Default::default(),
            proc_count: 0,"""
text = text.replace(init_to_replace, new_init)

init_to_replace2 = """            rule_offer: None,
            detail_pid: None,
            detail_info: None,
            detail_last_gen: 0,"""
new_init2 = """            detail_window: Default::default(),"""
text = text.replace(init_to_replace2, new_init2)

# Remove show_detail_window
text = re.sub(r'    /// Details window for one process: procfs facts.*?\n    fn show_detail_window\(\n        &mut self,.*?\n    \}\n', '', text, flags=re.DOTALL)

# Remove offer_rule
text = re.sub(r'    /// Record a manual change so the "remember as rule\?" prompt.*?\n    fn offer_rule\(\n        &mut self,.*?\n    \}\n', '', text, flags=re.DOTALL)

# Remove deliver_kill
text = re.sub(r'    /// Send the actual kill signal.*?fn deliver_kill.*?Ok\(.\)\s*\}\n', '', text, flags=re.DOTALL)

# Remove handle_table_action
text = re.sub(r'    fn handle_table_action\(\n        &mut self,.*?\n            TableAction::None => \{\}\n        \}\n    \}\n', '', text, flags=re.DOTALL)

# Remove poll_dialogs
text = re.sub(r'    fn poll_dialogs\(&mut self, ctx: &Context\) \{.*?\n            \} else if dismiss \{\n                self\.rule_offer = None;\n            \}\n        \}\n    \}\n', '', text, flags=re.DOTALL)

# Fix tour step references
tour_to_replace = """        self.affinity_dialog = None;
        self.nice_dialog = None;
        self.ionice_dialog = None;
        if !matches!(step, Step::ProcessDetails) {
            self.detail_pid = None;
        }
        if !matches!(step, Step::KillToast) {
            self.pending_kill = None;
        }
        if !matches!(step, Step::RuleOffer) {
            self.rule_offer = None;
        }"""
new_tour = """        self.dialog_manager = Default::default();
        if !matches!(step, Step::ProcessDetails) {
            self.detail_window = Default::default();
        }
        if !matches!(step, Step::KillToast) {
            self.pending_kill = None;
        }"""
text = text.replace(tour_to_replace, new_tour)

tour_to_replace2 = """            Step::ProcessDetails => {
                self.active_tab = Tab::Processes;
                if self.detail_pid != Some(pid) {
                    self.detail_pid = Some(pid);
                    self.detail_info = utils::read_proc_details(pid);
                }
            }"""
new_tour2 = """            Step::ProcessDetails => {
                self.active_tab = Tab::Processes;
                if self.detail_window.detail_pid != Some(pid) {
                    self.detail_window.set_pid(pid);
                }
            }"""
text = text.replace(tour_to_replace2, new_tour2)

tour_to_replace3 = """            Step::RuleOffer => {
                self.active_tab = Tab::Processes;
                if self.rule_offer.is_none() {
                    self.rule_offer = Some(RuleOffer {
                        proc_name: "argus-lasso".into(),
                        affinity: Some("0-7".into()),
                        nice: Some(-5),
                        ionice: Some((2, 4)),
                    });
                }
            }"""
new_tour3 = """            Step::RuleOffer => {
                self.active_tab = Tab::Processes;
                if self.dialog_manager.rule_offer.is_none() {
                    self.dialog_manager.rule_offer = Some(crate::gui::dialog_manager::RuleOffer {
                        proc_name: "argus-lasso".into(),
                        affinity: Some("0-7".into()),
                        nice: Some(-5),
                        ionice: Some((2, 4)),
                    });
                }
            }"""
text = text.replace(tour_to_replace3, new_tour3)

# Fix ui method calls
ui_calls_to_replace = """        // Poll active dialogs
        self.poll_dialogs(ctx);

        // Per-process details window
        self.show_detail_window(ctx, &snapshot, &proc_cpu_history, cpu_gen);"""
new_ui_calls = """        // Poll active dialogs
        let notify_error = |msg: &str| { self.notify_error(msg); };
        self.dialog_manager.poll_dialogs(ctx, self.opacity, &self.state, &self.cmd_tx, &self.rule_engine, &notify_error);

        // Per-process details window
        self.detail_window.show(ctx, &snapshot, &proc_cpu_history, cpu_gen);"""
text = text.replace(ui_calls_to_replace, new_ui_calls)


kill_deliver_replace = """                let msg = match Self::deliver_kill(pid, force) {"""
new_kill_deliver = """                let msg = match crate::gui::action_handler::ActionHandler::deliver_kill(pid, force) {"""
text = text.replace(kill_deliver_replace, new_kill_deliver)


action_handle_to_replace = """                    let action = self.process_tab.show(
                        ui,
                        &snapshot,
                        &throttled_pids,
                        &suspended_pids,
                        &self.cmd_tx,
                        &self.rule_engine,
                        gaming_active,
                        &proc_cpu_history,
                    );
                    self.handle_table_action(action, ctx, &snapshot);"""
new_action_handle = """                    let action = self.process_tab.show(
                        ui,
                        &snapshot,
                        &throttled_pids,
                        &suspended_pids,
                        &self.cmd_tx,
                        &self.rule_engine,
                        gaming_active,
                        &proc_cpu_history,
                    );
                    let mut trigger_rule = None;
                    crate::gui::action_handler::ActionHandler::handle(
                        action,
                        &snapshot,
                        &self.state,
                        &mut self.pending_kill,
                        &mut self.dialog_manager,
                        &mut self.detail_window,
                        &notify_error,
                        &mut trigger_rule,
                    );
                    if let Some(rule) = trigger_rule {
                        self.rules_tab.open_add_dialog(Some(rule));
                        self.active_tab = Tab::Rules;
                    }"""
text = text.replace(action_handle_to_replace, new_action_handle)

with open("src/app.rs", "w") as f:
    f.write(text)

