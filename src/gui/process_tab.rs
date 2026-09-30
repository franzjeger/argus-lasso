//! Processes tab: CPU history + per-CPU bars + filter + sortable process table.

use std::collections::{HashMap, HashSet, VecDeque};

use egui::RichText;

use crate::gui::cpu_bars::{CpuBarsWidget, CpuHistoryWidget};
use crate::gui::theme::{self, Breeze};
use crate::monitor::ProcInfo;
use crate::utils::{build_core_pairs, cpulist_to_set, cpuset_to_cpulist, get_offline_cpus};

// ── Sort state ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum SortCol {
    Pid,
    Name,
    Cpu,
    Gpu,
    Mem,
    Nice,
    Affinity,
    Ionice,
    Status,
}

impl SortCol {
    fn label(&self) -> &'static str {
        match self {
            SortCol::Pid => "PID",
            SortCol::Name => "NAME",
            SortCol::Cpu => "CPU%",
            SortCol::Gpu => "GPU%",
            SortCol::Mem => "RAM (MiB)",
            SortCol::Nice => "NICE",
            SortCol::Affinity => "AFFINITY",
            SortCol::Ionice => "I/O PRI",
            SortCol::Status => "STATUS",
        }
    }
}

// ── Formatting helpers ────────────────────────────────────────────────────────

/// Convert raw "class/level" ionice string to human-readable form.
fn fmt_ionice(s: &str) -> String {
    if s.is_empty() {
        return "—".into();
    }
    let mut parts = s.splitn(2, '/');
    let class: u32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let level: u32 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    match class {
        0 => "—".into(),
        1 => format!("RT-{level}"),
        2 => format!("BE-{level}"),
        3 => "Idle".into(),
        _ => s.into(),
    }
}

/// Format bytes/s compactly: "1.2 MiB/s", "456 KiB/s", "—"
fn fmt_bps(bytes: u64) -> String {
    if bytes == 0 {
        return "—".into();
    }
    if bytes >= 1_048_576 {
        format!("{:.1} MiB/s", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.0} KiB/s", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B/s")
    }
}

// ── Context menu action ───────────────────────────────────────────────────────

#[derive(Debug)]
pub enum TableAction {
    Kill {
        pid: u32,
        name: String,
        force: bool,
    },
    Suspend {
        pid: u32,
        name: String,
    },
    Resume {
        pid: u32,
        name: String,
    },
    SetAffinity {
        pid: u32,
        name: String,
        current: String,
    },
    SetNice {
        pid: u32,
        name: String,
        current: i32,
    },
    SetIonice {
        pid: u32,
        name: String,
    },
    AddRule {
        name: String,
    },
    ShowDetails {
        pid: u32,
    },
    KillTree {
        pid: u32,
        name: String,
    },
    Export {
        format: ExportFormat,
    },
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
}

// ── Pending pause/resume ──────────────────────────────────────────────────────

/// Pause and resume requests not yet visible in a snapshot, so the table
/// shows the change at once instead of on the next refresh. Keyed by PID and
/// start time; an entry goes once the snapshot agrees or the process is gone.
#[derive(Default)]
pub struct PendingStops(HashMap<u32, (u64, bool)>);

impl PendingStops {
    pub fn record(&mut self, pid: u32, start_ticks: u64, stopped: bool) {
        self.0.insert(pid, (start_ticks, stopped));
    }

    /// The PIDs to show as suspended: stopped per the kernel, as of the
    /// snapshot, with requests it does not reflect yet applied on top.
    pub fn stopped_pids(&mut self, snapshot: &[ProcInfo]) -> HashSet<u32> {
        let mut still_pending = HashMap::new();
        let mut stopped = HashSet::new();
        for p in snapshot {
            let mut is_stopped = p.stopped;
            if let Some(&(start_ticks, wanted)) = self.0.get(&p.pid) {
                if start_ticks == p.start_ticks && wanted != p.stopped {
                    is_stopped = wanted;
                    still_pending.insert(p.pid, (start_ticks, wanted));
                }
            }
            if is_stopped {
                stopped.insert(p.pid);
            }
        }
        self.0 = still_pending;
        stopped
    }
}

// ── Pending kill (undo support) ───────────────────────────────────────────────

pub struct PendingKill {
    pub pid: u32,
    pub name: String,
    pub force: bool,
    pub deadline: std::time::Instant,
    pub target: Option<crate::process_control::ProcessHandle>,
}

impl PendingKill {
    pub fn deliver(&mut self) -> Result<(), nix::Error> {
        use nix::sys::signal::Signal;
        if let Some(target) = self.target.take() {
            let result = target.signal(if self.force {
                Signal::SIGKILL
            } else {
                Signal::SIGTERM
            });
            let _ = target.signal(Signal::SIGCONT);
            result
        } else {
            Ok(())
        } // Read-only UI preview has no target.
    }
    pub fn cancel(&mut self) -> Result<(), nix::Error> {
        self.target
            .take()
            .map_or(Ok(()), |t| t.signal(nix::sys::signal::Signal::SIGCONT))
    }
}
impl Drop for PendingKill {
    fn drop(&mut self) {
        let _ = self.cancel();
    }
}

// ── Format affinity string with grouped physical+HT pairs ─────────────────────

fn format_affinity_display(
    affinity_str: &str,
    offline: &HashSet<u32>,
    core_pairs: &HashMap<u32, Vec<u32>>,
    hide_parked: bool,
) -> String {
    if !hide_parked || offline.is_empty() {
        return affinity_str.to_string();
    }
    let cpus = match cpulist_to_set(affinity_str) {
        Ok(s) if !s.is_empty() => s,
        _ => return affinity_str.to_string(),
    };
    let visible: HashSet<u32> = cpus.difference(offline).copied().collect();
    if visible.is_empty() {
        return "—".to_string();
    }
    if core_pairs.is_empty() {
        return cpuset_to_cpulist(&visible);
    }
    let mut seen: HashSet<u32> = HashSet::new();
    let mut sorted_visible: Vec<u32> = visible.iter().copied().collect();
    sorted_visible.sort_unstable();
    let mut parts: Vec<String> = Vec::new();
    for cpu in &sorted_visible {
        if seen.contains(cpu) {
            continue;
        }
        seen.insert(*cpu);
        if let Some(siblings) = core_pairs.get(cpu) {
            let vis_sibs: Vec<u32> = siblings
                .iter()
                .filter(|&&s| visible.contains(&s) && !seen.contains(&s))
                .copied()
                .collect();
            if !vis_sibs.is_empty() {
                for &s in &vis_sibs {
                    seen.insert(s);
                }
                let sib_str = vis_sibs
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join("+");
                parts.push(format!("{cpu}+{sib_str}"));
            } else {
                parts.push(cpu.to_string());
            }
        } else {
            parts.push(cpu.to_string());
        }
    }
    parts.join(",")
}

// ── ProcessTab ────────────────────────────────────────────────────────────────

pub struct ProcessTab {
    pub history: CpuHistoryWidget,
    pub bars: CpuBarsWidget,
    pub filter: String,
    /// Interpret the filter as a regex instead of a plain substring
    pub filter_is_regex: bool,
    /// (pattern, compiled) cache so the regex isn't recompiled every frame
    filter_regex_cache: Option<(String, Option<regex::Regex>)>,
    pub sort_col: SortCol,
    pub sort_asc: bool,
    // Single-row selection (by PID)
    pub selected_pid: Option<u32>,
    // Gaming mode: hide/group parked CPUs in affinity column
    pub hide_parked_in_proc_view: bool,
    // Show processes as parent/child tree instead of flat list
    pub tree_view: bool,
    // Cached physical-core → HT-sibling map (read once from sysfs at startup)
    core_pairs: HashMap<u32, Vec<u32>>,
    // User-adjustable column widths: [PID, Name, CPU%, GPU%, Mem, Nice, Aff, I/O, Status]
    // Name column auto-fills; user can drag handles to resize others.
    pub col_widths: Vec<f32>,
    // Last available width — used to detect window resize for auto-scaling
    // Set to true when col_widths change so app.rs can persist them
    pub cols_dirty: bool,
    // Quick-filter chips (combine with the text filter)
    pub chip_high_cpu: bool,
    pub chip_throttled: bool,
    pub chip_suspended: bool,
    /// Columns hidden by the user (by header label); Name can't be hidden
    pub hidden_cols: HashSet<String>,
    /// Set when hidden_cols changes so app.rs can persist it
    pub hidden_dirty: bool,
    /// Optional port filter — when set, only show processes bound to this port
    pub port_filter: String,
    /// (port, timestamp, matching pids) — /proc/PID/fd reads are expensive, so
    /// the result is cached and only refreshed when the port text changes or
    /// the cache is older than ~1s.
    port_pids_cache: Option<(String, std::time::Instant, std::collections::HashSet<u32>)>,
    // Offline CPUs, refreshed on the daemon's display cadence in update_cpu()
    // — reading /sys/devices/system/cpu/offline every repaint is wasted I/O.
    cached_offline: HashSet<u32>,
}

impl ProcessTab {
    pub fn new(cfg_col_widths: &[f32], cfg_hidden_cols: &[String]) -> Self {
        // 9 columns: PID, Name, CPU%, GPU%, Mem, Nice, Affinity, I/O, Status
        let col_widths = match cfg_col_widths.len() {
            9 => cfg_col_widths.to_vec(),
            // Migrate pre-GPU-column configs: insert the GPU% width at index 3.
            8 => {
                let mut v = cfg_col_widths.to_vec();
                v.insert(3, 55.0);
                v
            }
            _ => vec![60.0, 0.0, 90.0, 55.0, 75.0, 45.0, 110.0, 58.0, 85.0],
        };
        Self {
            history: CpuHistoryWidget::new(),
            bars: CpuBarsWidget::new(),
            filter: String::new(),
            filter_is_regex: false,
            filter_regex_cache: None,
            sort_col: SortCol::Cpu,
            sort_asc: false,
            selected_pid: None,
            hide_parked_in_proc_view: true,
            tree_view: false,
            core_pairs: build_core_pairs(),
            col_widths,
            cols_dirty: false,
            chip_high_cpu: false,
            chip_throttled: false,
            chip_suspended: false,
            hidden_cols: cfg_hidden_cols.iter().cloned().collect(),
            hidden_dirty: false,
            port_filter: String::new(),
            port_pids_cache: None,
            cached_offline: get_offline_cpus(),
        }
    }

    pub fn update_cpu(&mut self, pcts: Vec<f32>, total: f32) {
        self.history.push(total);
        self.bars.update(pcts);
        self.cached_offline = get_offline_cpus();
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &[ProcInfo],
        throttled_pids: &HashSet<u32>,
        suspended_pids: &HashSet<u32>,
        gaming_active: bool,
        proc_cpu_history: &HashMap<u32, VecDeque<f32>>,
    ) -> TableAction {
        self.show_cpu_graphs(ui);
        let filter_id = handle_shortcuts(ui);
        // Later sources win: toolbar, then the Delete key, then the rows.
        let mut action = self.show_toolbar(ui, filter_id, gaming_active);
        let rows = self.visible_rows(snapshot, throttled_pids, suspended_pids);
        if let Some(kill) = self.delete_key_action(ui, &rows) {
            action = kill;
        }

        // Offline CPUs for affinity display (cached; refreshed in update_cpu)
        let offline = if gaming_active && self.hide_parked_in_proc_view {
            self.cached_offline.clone()
        } else {
            HashSet::new()
        };
        let layout = self.fit_columns(ui, &rows);
        let row_items = if self.tree_view {
            tree_order(&rows)
        } else {
            rows.iter()
                .map(|&proc| RowItem { proc, depth: 0 })
                .collect()
        };
        let row_ctx = RowCtx {
            layout: &layout,
            offline: &offline,
            core_pairs: &self.core_pairs,
            hide_parked: self.hide_parked_in_proc_view,
            throttled: throttled_pids,
            suspended: suspended_pids,
            cpu_history: proc_cpu_history,
        };

        let current_sort = (self.sort_col.clone(), self.sort_asc);
        let mut new_sort = current_sort.clone();
        let mut selected = self.selected_pid;
        let mut col_width_deltas = [0.0f32; 9];
        let tree_view = self.tree_view;
        let hidden_cols = &mut self.hidden_cols;
        let hidden_dirty = &mut self.hidden_dirty;

        // Wrap table in a visible border frame
        let frame_border_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
        egui::ScrollArea::horizontal()
            .id_salt("process_table_horizontal")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0_f32, frame_border_color))
                    .inner_margin(egui::Margin::same(1))
                    .show(ui, |ui| {
                        show_header(
                            ui,
                            &layout,
                            &current_sort,
                            &mut new_sort,
                            tree_view,
                            hidden_cols,
                            hidden_dirty,
                            &mut col_width_deltas,
                        );
                        // show_rows virtualizes the table: only visible rows are
                        // formatted and painted (rows are fixed ROW_H height).
                        egui::ScrollArea::vertical()
                            .id_salt("process_scroll")
                            .auto_shrink([false, false])
                            .show_rows(ui, ROW_H, row_items.len(), |ui, range| {
                                for (i, item) in row_items[range.clone()].iter().enumerate() {
                                    let row =
                                        show_row(ui, &row_ctx, item, range.start + i, selected);
                                    if row.clicked {
                                        selected = Some(item.proc.pid);
                                    }
                                    if let Some(row_action) = row.action {
                                        action = row_action;
                                    }
                                }
                            });
                    });
            });

        self.apply_width_deltas(&col_width_deltas, &layout.min_widths);
        (self.sort_col, self.sort_asc) = new_sort;
        self.selected_pid = selected;
        action
    }

    /// CPU history chart and the per-core grid side by side — stacking them
    /// cost ~90px of vertical space that the table wants.
    fn show_cpu_graphs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_top(|ui| {
            // Only the explicit gap should separate the two; the default
            // item_spacing would be added on top of the width arithmetic.
            ui.spacing_mut().item_spacing.x = 0.0;
            let total = ui.available_width();
            // The grid is sized to its content — two columns of cells — so it
            // stays a compact block instead of stretching across half the row.
            // Size the grid by the row count we want, not by a fixed column
            // count: 32 cores in 4 columns is 8 rows, which made the grid
            // twice the height of the graph beside it. Picking columns to
            // land on GRID_ROWS keeps the pair the same height on any core
            // count. 66 = the widget's 62px minimum cell plus its 3px gap.
            const GRID_ROWS: usize = 4;
            const CELL_H: f32 = 23.0; // 20px cell + 3px gap, per CpuBarsWidget
            let cores = crate::utils::get_cpu_count().max(1) as usize;
            let grid_cols = cores.div_ceil(GRID_ROWS).max(2);
            let grid_w = (grid_cols as f32 * 66.0).min(total * 0.45);
            let hist_w = (total - grid_w - theme::tokens::SPACE_S).max(240.0);
            // Both get the same card frame and the same height. The grid used
            // to sit bare against the window edge while the graph had no
            // frame either, so the row read as two loose fragments rather
            // than a pair — and the grid ended up the taller of the two.
            let row_h = GRID_ROWS as f32 * CELL_H + 16.0;
            theme::plot_card(ui, hist_w, row_h, |ui| {
                ui.vertical(|ui| self.history.show(ui));
            });
            ui.add_space(theme::tokens::SPACE_S);
            theme::plot_card(ui, grid_w, row_h, |ui| self.bars.show(ui));
        });
        ui.add_space(theme::tokens::SPACE_S);
    }

    /// Filter row, quick-filter chips and view toggles. Returns an export
    /// request, if one was made.
    fn show_toolbar(
        &mut self,
        ui: &mut egui::Ui,
        filter_id: egui::Id,
        gaming_active: bool,
    ) -> TableAction {
        let mut action = TableAction::None;
        ui.horizontal_wrapped(|ui| {
            ui.label("🔍");
            // The hint must fit the field; the rest goes in the tooltip.
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .id(filter_id)
                    .hint_text("Name, PID or command")
                    .desired_width(240.0),
            )
            .on_hover_text("Filters by name, PID or command line. Press / to jump here.");
            if !self.filter.is_empty() && ui.small_button("✕").clicked() {
                self.filter.clear();
            }
            if theme::chip(ui, ".*", self.filter_is_regex) {
                self.filter_is_regex = !self.filter_is_regex;
            }
            if self.filter_is_regex && !self.filter.is_empty() {
                // (Re)compile only when the pattern changed
                let stale = self
                    .filter_regex_cache
                    .as_ref()
                    .is_none_or(|(pat, _)| pat != &self.filter);
                if stale {
                    let compiled = regex::RegexBuilder::new(&self.filter)
                        .case_insensitive(true)
                        .build()
                        .ok();
                    self.filter_regex_cache = Some((self.filter.clone(), compiled));
                }
                if matches!(&self.filter_regex_cache, Some((_, None))) {
                    ui.colored_label(theme::sem(ui).negative, "invalid regex");
                }
            }
            ui.add_space(theme::tokens::SPACE_S);

            // Port filter — show only processes bound to this local port.
            ui.label("Port");
            ui.add(
                egui::TextEdit::singleline(&mut self.port_filter)
                    .hint_text("8080")
                    .desired_width(88.0),
            );
            if !self.port_filter.is_empty() && ui.small_button("✕").clicked() {
                self.port_filter.clear();
            }
            ui.add_space(theme::tokens::SPACE_S);

            // Quick-filter chips (§4) — pills, filled when active
            for (state, label, hint) in [
                (
                    &mut self.chip_high_cpu,
                    "High CPU",
                    "Only processes using at least 25% of total CPU capacity",
                ),
                (
                    &mut self.chip_throttled,
                    "Throttled",
                    "Only processes ProBalance is currently throttling",
                ),
                (
                    &mut self.chip_suspended,
                    "Suspended",
                    "Only paused (stopped) processes",
                ),
            ] {
                if theme::chip_hinted(ui, label, *state, hint) {
                    *state = !*state;
                }
            }

            // Right side: view toggles + the column picker, which used to be
            // discoverable only via a header right-click.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("Export ▾", |ui| {
                    for (text, format) in [("CSV", ExportFormat::Csv), ("JSON", ExportFormat::Json)]
                    {
                        if ui.button(text).clicked() {
                            action = TableAction::Export { format };
                            ui.close();
                        }
                    }
                });
                ui.menu_button("Columns ▾", |ui| {
                    for (ci, c) in COLS.iter().enumerate() {
                        if ci == 1 {
                            continue; // Name is always shown
                        }
                        let mut shown = !self.hidden_cols.contains(c.label());
                        if ui.checkbox(&mut shown, c.label()).changed() {
                            if shown {
                                self.hidden_cols.remove(c.label());
                            } else {
                                self.hidden_cols.insert(c.label().to_string());
                                if *c == self.sort_col {
                                    self.sort_col = SortCol::Cpu;
                                    self.sort_asc = false;
                                }
                            }
                            self.hidden_dirty = true;
                        }
                    }
                });
                let tree_btn = egui::Button::new("Tree view").selected(self.tree_view);
                if ui.add(tree_btn).clicked() {
                    self.tree_view = !self.tree_view;
                }
                if gaming_active {
                    let parked_btn =
                        egui::Button::new("Hide parked").selected(self.hide_parked_in_proc_view);
                    if ui
                        .add(parked_btn)
                        .on_hover_text("Group CPU assignments / hide parked threads")
                        .clicked()
                    {
                        self.hide_parked_in_proc_view = !self.hide_parked_in_proc_view;
                    }
                }
            });
        });
        ui.add_space(2.0);
        action
    }

    /// The rows the table shows: filtered by text (name, PID or command
    /// line), port and chips, then sorted. References, not clones —
    /// deep-copying ~1000 ProcInfo rows (several heap Strings each) on every
    /// repaint is pure waste.
    fn visible_rows<'a>(
        &mut self,
        snapshot: &'a [ProcInfo],
        throttled_pids: &HashSet<u32>,
        suspended_pids: &HashSet<u32>,
    ) -> Vec<&'a ProcInfo> {
        let mut rows: Vec<&ProcInfo> = snapshot.iter().collect();
        if !self.filter.is_empty() {
            if self.filter_is_regex {
                // Invalid regex: keep everything rather than hiding all rows
                if let Some((_, Some(re))) = &self.filter_regex_cache {
                    rows.retain(|p| re.is_match(&p.name) || re.is_match(&p.cmdline));
                }
            } else {
                let filter_lower = self.filter.to_lowercase();
                rows.retain(|p| {
                    p.name.to_lowercase().contains(&filter_lower)
                        || p.pid.to_string().contains(&filter_lower)
                        || p.cmdline.to_lowercase().contains(&filter_lower)
                });
            }
        }
        if let Ok(port) = self.port_filter.trim().parse::<u16>() {
            let pids = self.pids_for_port(port, &rows);
            rows.retain(|p| pids.contains(&p.pid));
        }
        if self.chip_high_cpu {
            rows.retain(|p| p.cpu_percent >= 25.0);
        }
        if self.chip_throttled {
            rows.retain(|p| throttled_pids.contains(&p.pid));
        }
        if self.chip_suspended {
            rows.retain(|p| suspended_pids.contains(&p.pid));
        }
        sort_rows(
            &mut rows,
            &self.sort_col,
            self.sort_asc,
            throttled_pids,
            suspended_pids,
        );
        rows
    }

    /// PIDs among `rows` bound to `port`. /proc/PID/fd reads are expensive,
    /// so the result is cached for a second per port and text filter.
    fn pids_for_port(&mut self, port: u16, rows: &[&ProcInfo]) -> HashSet<u32> {
        let key = format!("{}|{}", self.port_filter.trim(), self.filter);
        let now = std::time::Instant::now();
        match &self.port_pids_cache {
            Some((k, ts, set))
                if k == &key && now.duration_since(*ts) < std::time::Duration::from_secs(1) =>
            {
                set.clone()
            }
            _ => {
                let candidates: Vec<u32> = rows.iter().map(|p| p.pid).collect();
                let set = crate::utils::pids_for_port(port, &candidates);
                self.port_pids_cache = Some((key, now, set.clone()));
                set
            }
        }
    }

    /// Delete key — kill the currently selected process. Only when no widget
    /// (e.g. the filter text box) has keyboard focus, otherwise editing text
    /// could kill the selected process.
    fn delete_key_action(&self, ui: &egui::Ui, rows: &[&ProcInfo]) -> Option<TableAction> {
        let text_has_focus = ui.ctx().memory(|m| m.focused().is_some());
        let pressed = ui.input(|i| i.key_pressed(egui::Key::Delete));
        if !pressed || text_has_focus {
            return None;
        }
        let pid = self.selected_pid?;
        let proc = rows.iter().find(|p| p.pid == pid)?;
        Some(TableAction::Kill {
            pid,
            name: proc.name.to_string(),
            force: false,
        })
    }

    /// Fit the column widths to the window and their content, and lay the
    /// visible columns out.
    fn fit_columns(&mut self, ui: &egui::Ui, rows: &[&ProcInfo]) -> ColumnLayout {
        // Visible columns, in table order. Name (index 1) can never be hidden.
        let visible: Vec<usize> = (0..COLS.len())
            .filter(|&i| i == 1 || !self.hidden_cols.contains(COLS[i].label()))
            .collect();

        // Font-aware minima also apply to widths saved by older, smaller UI
        // versions. Resizing the window must not scale PID digits out of a cell.
        let num_font = theme::num_font(theme::tokens::FONT_BODY);
        let measure = |text: String| {
            ui.painter()
                .layout_no_wrap(text, num_font.clone(), egui::Color32::WHITE)
                .size()
                .x
                + PAD * 2.0
                + NUM_INSET
        };
        let min_widths = [
            measure(rows.iter().map(|p| p.pid).max().unwrap_or(0).to_string()).max(60.0),
            150.0,
            SPARK_W
                + measure(format!(
                    "{:.1}",
                    rows.iter().map(|p| p.cpu_percent).fold(100.0_f32, f32::max)
                )),
            measure("100".into()).max(60.0),
            measure(format!(
                "{:.1}",
                rows.iter().map(|p| p.mem_rss).max().unwrap_or(0) as f64 / 1_048_576.0
            ))
            .max(92.0),
            measure("-20".into()).max(52.0),
            100.0,
            76.0,
            90.0,
        ];
        for (width, minimum) in self.col_widths.iter_mut().zip(min_widths) {
            *width = width.max(minimum);
        }
        let avail_w = ui.available_width() - 4.0 - ui.spacing().scroll.allocated_width();
        let fixed: f32 = visible
            .iter()
            .filter(|&&i| i != 1)
            .map(|&i| self.col_widths[i])
            .sum();
        self.col_widths[1] = (avail_w - fixed).max(min_widths[1]);
        ColumnLayout::new(visible, self.col_widths.clone(), min_widths)
    }

    /// Apply column resize deltas (index 1 = name auto-fills, skip it)
    fn apply_width_deltas(&mut self, deltas: &[f32; 9], min_widths: &[f32; 9]) {
        self.cols_dirty = false;
        for (i, &delta) in deltas.iter().enumerate() {
            if delta != 0.0 && i != 1 {
                self.col_widths[i] = (self.col_widths[i] + delta).max(min_widths[i]);
                self.cols_dirty = true;
            }
        }
    }
}

// ── Table layout and rendering ────────────────────────────────────────────────

const COLS: [SortCol; 9] = [
    SortCol::Pid,
    SortCol::Name,
    SortCol::Cpu,
    SortCol::Gpu,
    SortCol::Mem,
    SortCol::Nice,
    SortCol::Affinity,
    SortCol::Ionice,
    SortCol::Status,
];
const ROW_H: f32 = theme::tokens::ROW_H;
const HEADER_H: f32 = 24.0;
const PAD: f32 = 4.0;
/// Extra breathing room on the right of a right-aligned numeric cell.
/// Without it NICE's value butts straight up against AFFINITY's, which
/// read as one cramped field ("-12 0-31") in the design review.
const NUM_INSET: f32 = 8.0;
/// Fixed width for the CPU% sparkline. Sizing it as a fraction of the
/// column made the value's indent shift per column width, which is
/// what made the column look restless.
const SPARK_W: f32 = 36.0;
/// Affinity strings longer than this are truncated, with the full
/// string in a tooltip.
const AFF_MAX: usize = 14;

/// Keyboard shortcuts: / focuses the filter, F5 repaints. Returns the
/// filter's id.
///
/// All ctx calls MUST be outside ui.input(): ctx.input() holds the
/// ContextImpl WRITE lock; calling ctx.read() or ctx.write() inside it
/// causes write→read or write→write re-entrant deadlock (parking_lot panics
/// after 10s with "Failed to acquire RwLock … Deadlock?").
fn handle_shortcuts(ui: &egui::Ui) -> egui::Id {
    let filter_id = egui::Id::new("proc_filter");
    let (f5_pressed, slash_pressed) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::F5),
            i.key_pressed(egui::Key::Slash) && !i.modifiers.any(),
        )
    });
    if slash_pressed {
        ui.ctx().memory_mut(|m| m.request_focus(filter_id));
    }
    if f5_pressed {
        ui.ctx().request_repaint();
    }
    filter_id
}

/// Sort rows by `col`. All sorts use PID as a stable tiebreaker so equal
/// rows never flicker; NaN CPU/GPU values compare equal.
fn sort_rows(
    rows: &mut [&ProcInfo],
    col: &SortCol,
    asc: bool,
    throttled: &HashSet<u32>,
    suspended: &HashSet<u32>,
) {
    // Rank: suspended (2) > throttled (1) > running (0)
    let rank = |p: &ProcInfo| -> u8 {
        if suspended.contains(&p.pid) {
            2
        } else if throttled.contains(&p.pid) {
            1
        } else {
            0
        }
    };
    rows.sort_by(|a, b| {
        let ord = match col {
            SortCol::Pid => a.pid.cmp(&b.pid),
            SortCol::Name => a.name.cmp(&b.name),
            SortCol::Cpu => a
                .cpu_percent
                .partial_cmp(&b.cpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortCol::Gpu => a
                .gpu_percent
                .partial_cmp(&b.gpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortCol::Mem => a.mem_rss.cmp(&b.mem_rss),
            SortCol::Nice => a.nice.cmp(&b.nice),
            SortCol::Affinity => a.affinity.cmp(&b.affinity),
            SortCol::Ionice => a.ionice.cmp(&b.ionice),
            SortCol::Status => rank(a).cmp(&rank(b)),
        };
        if asc { ord } else { ord.reverse() }.then(a.pid.cmp(&b.pid))
    });
}

struct RowItem<'a> {
    proc: &'a ProcInfo,
    depth: usize,
}

/// Rows in parent/child order: children under their parent, siblings and
/// roots by name. A process whose parent is not shown is a root.
fn tree_order<'a>(rows: &[&'a ProcInfo]) -> Vec<RowItem<'a>> {
    let pid_set: HashSet<u32> = rows.iter().map(|p| p.pid).collect();
    let mut children: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (i, p) in rows.iter().enumerate() {
        if p.ppid == 0 || !pid_set.contains(&p.ppid) {
            roots.push(i);
        } else {
            children.entry(p.ppid).or_default().push(i);
        }
    }
    // Sort children by name for stable display
    for v in children.values_mut() {
        v.sort_by_key(|&i| &rows[i].name);
    }
    roots.sort_by_key(|&i| &rows[i].name);
    let mut result = Vec::new();
    let mut stack: Vec<(usize, usize)> = roots.iter().rev().map(|&i| (i, 0)).collect();
    while let Some((idx, depth)) = stack.pop() {
        result.push(RowItem {
            proc: rows[idx],
            depth,
        });
        if let Some(ch) = children.get(&rows[idx].pid) {
            stack.extend(ch.iter().rev().map(|&ci| (ci, depth + 1)));
        }
    }
    result
}

/// The visible columns with their widths and x offsets.
struct ColumnLayout {
    /// Column indices into COLS, in table order.
    visible: Vec<usize>,
    widths: Vec<f32>,
    min_widths: [f32; 9],
    /// Column index → (x offset, width), for hover hit tests.
    offsets: HashMap<usize, (f32, f32)>,
    total_w: f32,
}

impl ColumnLayout {
    fn new(visible: Vec<usize>, widths: Vec<f32>, min_widths: [f32; 9]) -> Self {
        let mut offsets = HashMap::new();
        let mut x = 0.0f32;
        for &i in &visible {
            offsets.insert(i, (x, widths[i]));
            x += widths[i];
        }
        let total_w = visible.iter().map(|&i| widths[i]).sum();
        Self {
            visible,
            widths,
            min_widths,
            offsets,
            total_w,
        }
    }

    /// The rect of column `idx` within a row starting at `row`, if shown.
    fn cell_rect(&self, idx: usize, row: egui::Rect) -> Option<egui::Rect> {
        self.offsets.get(&idx).map(|&(off, w)| {
            egui::Rect::from_min_size(egui::pos2(row.min.x + off, row.min.y), egui::vec2(w, ROW_H))
        })
    }
}

/// Sortable header, pinned above the scrolling body: click to sort, drag a
/// boundary to resize, right-click for the column chooser.
#[allow(clippy::too_many_arguments)]
fn show_header(
    ui: &mut egui::Ui,
    layout: &ColumnLayout,
    current: &(SortCol, bool),
    new_sort: &mut (SortCol, bool),
    tree_view: bool,
    hidden_cols: &mut HashSet<String>,
    hidden_dirty: &mut bool,
    col_width_deltas: &mut [f32; 9],
) {
    let (sort_col_cur, sort_asc_cur) = current;
    let (header_rect, _) = ui.allocate_exact_size(
        egui::Vec2::new(layout.total_w, HEADER_H),
        egui::Sense::hover(),
    );
    // Header background
    ui.painter().rect_filled(
        header_rect,
        0.0,
        ui.visuals().widgets.noninteractive.bg_fill,
    );
    let mut x = header_rect.min.x;
    for &i in &layout.visible {
        let col = &COLS[i];
        let cw = layout.widths[i];
        let cell_rect = egui::Rect::from_min_size(
            egui::Pos2::new(x + PAD, header_rect.min.y),
            egui::Vec2::new(cw - PAD, HEADER_H),
        );
        let is_active = col == sort_col_cur && !tree_view;
        let label_str = if is_active {
            format!("{} {}", col.label(), if *sort_asc_cur { "▲" } else { "▼" })
        } else {
            col.label().to_string()
        };
        // §3: headers are weak grey — accent blue reads as
        // "selected/interactive". Only the sorted column is
        // strong, and it carries the arrow.
        let resp = ui.put(
            cell_rect,
            egui::Label::new(theme::header_text(ui, &label_str, is_active))
                .truncate()
                .sense(egui::Sense::click()),
        );
        let resp = if *col == SortCol::Cpu {
            resp.on_hover_text(
                "Share of total available CPU capacity: 0–100%.\n\
                 Individual logical CPUs have their own 0–100% scale.",
            )
        } else {
            resp
        };
        if resp.clicked() && !tree_view {
            if col == sort_col_cur {
                new_sort.1 = !sort_asc_cur;
            } else {
                *new_sort = (
                    col.clone(),
                    matches!(col, SortCol::Name | SortCol::Affinity),
                );
            }
        }
        // Right-click any header → column chooser
        resp.context_menu(|ui| {
            ui.label(RichText::new("Columns ▾").font(theme::bold_font(theme::tokens::FONT_BODY)));
            for (ci, c) in COLS.iter().enumerate() {
                if ci == 1 {
                    continue; // Name is always shown
                }
                let mut shown = !hidden_cols.contains(c.label());
                if ui.checkbox(&mut shown, c.label()).changed() {
                    if shown {
                        hidden_cols.remove(c.label());
                    } else {
                        hidden_cols.insert(c.label().to_string());
                        // Hiding the active sort column would
                        // strand an invisible sort with no way
                        // to change direction — fall back.
                        if *c == new_sort.0 {
                            *new_sort = (SortCol::Cpu, false);
                        }
                    }
                    *hidden_dirty = true;
                }
            }
        });
        x += cw;
    }
    // Drag-to-resize handles — one between each visible column pair
    x = header_rect.min.x;
    for (vi, &i) in layout
        .visible
        .iter()
        .enumerate()
        .take(layout.visible.len().saturating_sub(1))
    {
        x += layout.widths[i];
        let handle_rect = egui::Rect::from_min_size(
            egui::pos2(x - 3.0, header_rect.min.y),
            egui::vec2(6.0, HEADER_H),
        );
        let resp = ui.interact(
            handle_rect,
            egui::Id::new(("col_resize", i)),
            egui::Sense::drag(),
        );
        let sep_color = if resp.hovered() || resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
            Breeze::HIGHLIGHT
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke.color
        };
        ui.painter().line_segment(
            [
                egui::pos2(x, header_rect.min.y),
                egui::pos2(x, header_rect.max.y),
            ],
            egui::Stroke::new(1.0_f32, sep_color),
        );
        if resp.dragged() {
            if i == 1 {
                // Name auto-fills, so its delta is discarded — move this
                // boundary by resizing the column on the RIGHT inversely
                // instead (dragging right grows Name = shrinks the right
                // neighbor).
                let right = layout.visible[vi + 1];
                col_width_deltas[right] -= resp.drag_delta().x;
            } else {
                col_width_deltas[i] += resp.drag_delta().x;
            }
        }
    }
    // Separator line between header and body
    ui.painter().line_segment(
        [header_rect.left_bottom(), header_rect.right_bottom()],
        egui::Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
    );
}

/// What every row needs, computed once per frame.
struct RowCtx<'a> {
    layout: &'a ColumnLayout,
    offline: &'a HashSet<u32>,
    core_pairs: &'a HashMap<u32, Vec<u32>>,
    hide_parked: bool,
    throttled: &'a HashSet<u32>,
    suspended: &'a HashSet<u32>,
    cpu_history: &'a HashMap<u32, VecDeque<f32>>,
}

struct RowOutcome {
    clicked: bool,
    action: Option<TableAction>,
}

/// One table row: background, cells, tooltips, and clicks — click to select,
/// double-click for details, right-click for actions.
fn show_row(
    ui: &mut egui::Ui,
    ctx: &RowCtx,
    item: &RowItem,
    row_idx: usize,
    selected: Option<u32>,
) -> RowOutcome {
    let proc = item.proc;
    let pid = proc.pid;
    let aff_full =
        format_affinity_display(&proc.affinity, ctx.offline, ctx.core_pairs, ctx.hide_parked);

    // Allocate the full row — advances the cursor. Interact via a PID-stable
    // id: the allocate response uses a positional auto-id, so an open context
    // menu would rebind to whatever process lands in that slot after a
    // re-sort or scroll — "Kill" could then hit the wrong process.
    let (row_rect, _) = ui.allocate_exact_size(
        egui::Vec2::new(ctx.layout.total_w, ROW_H),
        egui::Sense::hover(),
    );
    let row_resp = ui.interact(
        row_rect,
        ui.make_persistent_id(("proc_row", pid)),
        egui::Sense::click(),
    );

    // Row background
    let bg = if selected == Some(pid) {
        ui.visuals().selection.bg_fill
    } else if row_idx % 2 == 1 {
        ui.visuals().faint_bg_color
    } else {
        ui.visuals().extreme_bg_color
    };
    ui.painter().rect_filled(row_rect, 0.0, bg);

    if ui.is_rect_visible(row_rect) {
        paint_cells(ui, ctx, item, row_rect, &aff_full);
        if row_resp.hovered() {
            row_tooltip(ui, ctx, proc, row_rect, &aff_full);
        }
    }

    let mut action = None;
    if row_resp.double_clicked() {
        action = Some(TableAction::ShowDetails { pid });
    }
    let is_suspended = ctx.suspended.contains(&pid);
    row_resp.context_menu(|ui| {
        if let Some(chosen) = row_menu(ui, proc, is_suspended) {
            action = Some(chosen);
        }
    });
    RowOutcome {
        clicked: row_resp.clicked(),
        action,
    }
}

/// Paint a row's cells directly: numbers right-aligned, text left-aligned,
/// the CPU% sparkline, and the status badge.
fn paint_cells(ui: &egui::Ui, ctx: &RowCtx, item: &RowItem, row_rect: egui::Rect, aff_full: &str) {
    let proc = item.proc;
    let pid = proc.pid;
    let throttled = ctx.throttled.contains(&pid);
    // Mockup 1d keeps PID/NAME in the plain text colour — load is carried by
    // the CPU% value and its sparkline. Throttled rows still get a warning
    // tint as the badge alone is easy to miss when scanning.
    let row_col = if throttled {
        theme::sem(ui).warning
    } else {
        ui.visuals().text_color()
    };
    let aff_display = if aff_full.len() > AFF_MAX {
        format!("{}…", &aff_full[..AFF_MAX.saturating_sub(1)])
    } else {
        aff_full.to_string()
    };
    let ionice_str = fmt_ionice(&proc.ionice);
    // CPU% value + its sparkline share one ramp colour
    let load_col = theme::load_color(ui, proc.cpu_percent);
    let sem = theme::sem(ui);
    let font = egui::FontId::proportional(theme::tokens::FONT_BODY);
    let num_font = theme::num_font(theme::tokens::FONT_BODY);

    let mut x = row_rect.min.x;
    for &ci in &ctx.layout.visible {
        let cw = ctx.layout.widths[ci];
        let cell_rect =
            egui::Rect::from_min_size(egui::pos2(x, row_rect.top()), egui::vec2(cw, ROW_H));
        let painter = ui
            .painter()
            .with_clip_rect(ui.clip_rect().intersect(cell_rect));
        let x_off = if ci == 1 {
            item.depth as f32 * 14.0
        } else {
            0.0
        };
        // Numeric columns are right-aligned (§2) so live values don't jitter;
        // text columns stay left-aligned. CPU% is numeric too — it used to be
        // left-aligned at 45% of the column, so its indent moved with the
        // width and could collide with the sparkline.
        let numeric = matches!(ci, 0 | 2 | 3 | 4 | 5);
        let (text_pos, align) = if numeric {
            (
                egui::pos2(x + cw - PAD - NUM_INSET, row_rect.center().y),
                egui::Align2::RIGHT_CENTER,
            )
        } else {
            (
                egui::pos2(x + PAD + x_off, row_rect.center().y),
                egui::Align2::LEFT_CENTER,
            )
        };
        if ci == 2 {
            if let Some(hist) = ctx.cpu_history.get(&pid) {
                paint_sparkline(&painter, hist, x, row_rect, cw, load_col);
            }
        }
        // Status column renders as a badge, not emoji+text; other cells paint
        // their text.
        if ci == 8 {
            let badge_pos = egui::pos2(x + PAD, row_rect.center().y);
            if ctx.suspended.contains(&pid) {
                theme::badge_at(&painter, badge_pos, "Suspended", sem.accent);
            } else if throttled {
                theme::badge_at(&painter, badge_pos, "Throttled", sem.warning);
            }
        } else {
            let text: std::borrow::Cow<str> = match ci {
                0 => pid.to_string().into(),
                1 => proc.name.as_ref().into(),
                2 => format!("{:.1}", proc.cpu_percent).into(),
                3 => {
                    if proc.gpu_percent > 0.0 {
                        format!("{:.0}", proc.gpu_percent).into()
                    } else {
                        "—".into()
                    }
                }
                4 => format!("{:.1}", proc.mem_rss as f64 / 1_048_576.0).into(),
                5 => proc.nice.to_string().into(),
                6 => aff_display.as_str().into(),
                7 => ionice_str.as_str().into(),
                _ => "".into(),
            };
            // CPU% carries the load colour; the rest use the normal row colour.
            let col = if ci == 2 { load_col } else { row_col };
            let f = if numeric {
                num_font.clone()
            } else {
                font.clone()
            };
            painter.text(text_pos, align, text.as_ref(), f, col);
        }
        x += cw;
    }
}

/// Mini sparkline in the left portion of the CPU% cell, in the value's
/// colour (§1 one ramp).
fn paint_sparkline(
    painter: &egui::Painter,
    hist: &VecDeque<f32>,
    x: f32,
    row_rect: egui::Rect,
    cw: f32,
    color: egui::Color32,
) {
    if hist.len() < 2 {
        return;
    }
    let spark_w = SPARK_W.min(cw * 0.42);
    let spark_rect = egui::Rect::from_min_size(
        egui::pos2(x + 1.0, row_rect.min.y + 2.0),
        egui::vec2(spark_w, ROW_H - 4.0),
    );
    let lo = hist.iter().cloned().fold(f32::INFINITY, f32::min);
    let hi = hist
        .iter()
        .cloned()
        .fold(f32::NEG_INFINITY, f32::max)
        .max(lo + 0.1);
    let pts: Vec<egui::Pos2> = hist
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let px =
                spark_rect.left() + i as f32 / (hist.len() - 1).max(1) as f32 * spark_rect.width();
            let py = spark_rect.bottom() - (v - lo) / (hi - lo) * spark_rect.height();
            egui::pos2(px, py)
        })
        .collect();
    for pair in pts.windows(2) {
        painter.line_segment([pair[0], pair[1]], egui::Stroke::new(1.0_f32, color));
    }
}

/// Tooltip for the cell under the pointer: the name cell shows the command
/// line and disk I/O, a truncated affinity cell its full value.
fn row_tooltip(ui: &egui::Ui, ctx: &RowCtx, proc: &ProcInfo, row_rect: egui::Rect, aff_full: &str) {
    let pid = proc.pid;
    let ptr = ui.ctx().pointer_hover_pos();
    // Cell hit-rects from the visible layout (fixes stale hard-coded offsets
    // too).
    let name_rect = ctx
        .layout
        .cell_rect(1, row_rect)
        .unwrap_or(egui::Rect::NOTHING);
    let aff_rect = ctx
        .layout
        .cell_rect(6, row_rect)
        .unwrap_or(egui::Rect::NOTHING);
    if ptr.is_some_and(|p| name_rect.contains(p)) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
        egui::Tooltip::always_open(
            ui.ctx().clone(),
            ui.layer_id(),
            egui::Id::new(("proc_tip", pid)),
            egui::PopupAnchor::Pointer,
        )
        .gap(12.0)
        .show(|ui| {
            ui.label(
                egui::RichText::new(proc.name.as_ref())
                    .font(theme::bold_font(theme::tokens::FONT_BODY)),
            );
            if !proc.cmdline.is_empty() {
                ui.label(
                    egui::RichText::new(proc.cmdline.as_str())
                        .size(theme::tokens::FONT_HELP)
                        .color(ui.visuals().weak_text_color()),
                );
            }
            ui.separator();
            ui.label(format!("PID: {}   PPID: {}", pid, proc.ppid));
            ui.label(format!(
                "Disk R: {}   W: {}",
                fmt_bps(proc.disk_read_bps),
                fmt_bps(proc.disk_write_bps)
            ));
        });
    } else if ptr.is_some_and(|p| aff_rect.contains(p)) && aff_full.len() > AFF_MAX {
        egui::Tooltip::always_open(
            ui.ctx().clone(),
            ui.layer_id(),
            egui::Id::new(("aff_tip", pid)),
            egui::PopupAnchor::Pointer,
        )
        .gap(12.0)
        .show(|ui| {
            ui.label(aff_full);
        });
    }
}

/// The row's right-click menu. Returns the chosen action.
fn row_menu(ui: &mut egui::Ui, proc: &ProcInfo, is_suspended: bool) -> Option<TableAction> {
    let pid = proc.pid;
    let name = || proc.name.to_string();
    ui.label(
        egui::RichText::new(format!("{} · PID {pid}", proc.name))
            .font(theme::bold_font(theme::tokens::FONT_BODY)),
    );
    let mut chosen = None;
    let mut item = |ui: &mut egui::Ui, text: &str, action: &dyn Fn() -> TableAction| {
        if ui.button(text).clicked() {
            chosen = Some(action());
            ui.close();
        }
    };
    ui.separator();
    item(ui, "End process", &|| TableAction::Kill {
        pid,
        name: name(),
        force: false,
    });
    item(ui, "Force quit process", &|| TableAction::Kill {
        pid,
        name: name(),
        force: true,
    });
    item(ui, "End process and children", &|| TableAction::KillTree {
        pid,
        name: name(),
    });
    if is_suspended {
        item(ui, "Resume process", &|| TableAction::Resume {
            pid,
            name: name(),
        });
    } else {
        item(ui, "Pause process", &|| TableAction::Suspend {
            pid,
            name: name(),
        });
    }
    ui.separator();
    item(ui, "CPU assignment…", &|| TableAction::SetAffinity {
        pid,
        name: name(),
        current: proc.affinity.to_string(),
    });
    item(ui, "CPU priority…", &|| TableAction::SetNice {
        pid,
        name: name(),
        current: proc.nice,
    });
    item(ui, "Disk I/O priority…", &|| TableAction::SetIonice {
        pid,
        name: name(),
    });
    ui.separator();
    item(ui, "Create process rule…", &|| TableAction::AddRule {
        name: name(),
    });
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::TestFrame as _;

    fn proc(pid: u32, ppid: u32, name: &str, cpu: f32) -> ProcInfo {
        ProcInfo {
            pid,
            ppid,
            name: name.into(),
            cpu_percent: cpu,
            ..Default::default()
        }
    }

    /// A pause shows at once, then defers to the kernel's state; an entry
    /// never carries over to a process that reused the PID.
    #[test]
    fn pending_stops_bridge_the_gap_until_the_snapshot_agrees() {
        let running = |pid, start_ticks| ProcInfo {
            pid,
            start_ticks,
            ..Default::default()
        };
        let mut pending = PendingStops::default();
        pending.record(7, 100, true);
        // Snapshot taken before the SIGSTOP landed: show it paused anyway.
        assert!(pending.stopped_pids(&[running(7, 100)]).contains(&7));
        // The snapshot now agrees, so the entry is dropped...
        let stopped = ProcInfo {
            stopped: true,
            ..running(7, 100)
        };
        assert!(pending.stopped_pids(&[stopped]).contains(&7));
        // ...and a resume made elsewhere shows.
        assert!(pending.stopped_pids(&[running(7, 100)]).is_empty());

        pending.record(8, 100, true);
        // PID 8 now belongs to a different process.
        assert!(pending.stopped_pids(&[running(8, 999)]).is_empty());
    }

    /// The filter chips are painted by hand; without widget info a screen
    /// reader saw nothing there.
    #[test]
    fn filter_chips_are_visible_to_screen_readers() {
        let ctx = egui::Context::default();
        theme::apply_theme(&ctx, &theme::AppTheme::BreezeDark);
        ctx.enable_accesskit();
        let mut tab = ProcessTab::new(&[], &[]);
        tab.chip_throttled = true;
        let output = ctx.test_frame(Default::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| {
                tab.show(
                    ui,
                    &[],
                    &HashSet::new(),
                    &HashSet::new(),
                    false,
                    &HashMap::new(),
                );
            });
        });
        let update = output
            .platform_output
            .accesskit_update
            .expect("accesskit update");
        let chip = |name: &str| {
            update
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some(name))
                .map(|(_, node)| node.toggled())
                .unwrap_or_else(|| panic!("no accessible node for {name}"))
        };
        use egui::accesskit::Toggled;
        assert_eq!(chip("High CPU"), Some(Toggled::False));
        assert_eq!(chip("Throttled"), Some(Toggled::True));
    }

    #[test]
    fn sorting_is_stable_by_pid_in_both_directions() {
        let procs = [
            proc(30, 0, "b", 5.0),
            proc(10, 0, "a", 5.0),
            proc(20, 0, "c", 1.0),
            proc(40, 0, "d", 9.0),
        ];
        let none = HashSet::new();
        let pids = |col: SortCol, asc: bool| {
            let mut rows: Vec<&ProcInfo> = procs.iter().collect();
            sort_rows(&mut rows, &col, asc, &none, &none);
            rows.iter().map(|p| p.pid).collect::<Vec<_>>()
        };
        // Equal values keep PID order in either direction.
        assert_eq!(pids(SortCol::Cpu, false), [40, 10, 30, 20]);
        assert_eq!(pids(SortCol::Cpu, true), [20, 10, 30, 40]);
        assert_eq!(pids(SortCol::Name, true), [10, 30, 20, 40]);
        assert_eq!(pids(SortCol::Pid, false), [40, 30, 20, 10]);
        let suspended: HashSet<u32> = [30].into();
        let throttled: HashSet<u32> = [20].into();
        let mut rows: Vec<&ProcInfo> = procs.iter().collect();
        sort_rows(&mut rows, &SortCol::Status, false, &throttled, &suspended);
        assert_eq!(
            rows.iter().map(|p| p.pid).collect::<Vec<_>>(),
            [30, 20, 10, 40]
        );
    }

    #[test]
    fn tree_order_nests_children_by_name_and_roots_orphans() {
        let procs = [
            proc(1, 0, "init", 0.0),
            proc(5, 1, "zsh", 0.0),
            proc(3, 1, "bash", 0.0),
            proc(7, 3, "vim", 0.0),
            // Parent 99 is not listed, so this is a root.
            proc(8, 99, "orphan", 0.0),
        ];
        let rows: Vec<&ProcInfo> = procs.iter().collect();
        let order: Vec<(u32, usize)> = tree_order(&rows)
            .iter()
            .map(|item| (item.proc.pid, item.depth))
            .collect();
        assert_eq!(order, [(1, 0), (3, 1), (7, 2), (5, 1), (8, 0)]);
    }

    #[test]
    fn long_pid_and_multicore_numbers_fit_after_old_widths_and_window_resize() {
        let ctx = egui::Context::default();
        theme::apply_theme(&ctx, &theme::AppTheme::BreezeDark);
        let mut tab = ProcessTab::new(&[30.0; 9], &[]);
        let snapshot = [ProcInfo {
            pid: 2_147_483_647,
            name: "Long PID regression".into(),
            cpu_percent: 100.0,
            mem_rss: 128 * 1024 * 1024 * 1024,
            ..Default::default()
        }];
        for width in [1400.0, 680.0, 1800.0] {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 900.0),
                )),
                ..Default::default()
            };
            let mut output = None;
            // Let scroll geometry settle, as it does between native repaints.
            for _ in 0..3 {
                output = Some(ctx.test_frame(input.clone(), |root| {
                    egui::CentralPanel::default().show(root, |ui| {
                        tab.show(
                            ui,
                            &snapshot,
                            &HashSet::new(),
                            &HashSet::new(),
                            false,
                            &HashMap::new(),
                        );
                    });
                }));
            }
            let shapes = output.unwrap().shapes;
            for value in ["2147483647", "100.0"] {
                let text = shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == value => {
                            Some((shape.clip_rect, text))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("missing {value} at window width {width}"));
                let (clip, text) = text;
                let bounds = text.galley.rect.translate(text.pos.to_vec2());
                assert!(
                    bounds.left() >= clip.left() - 0.5,
                    "{value} bleeds left: {bounds:?} / {clip:?}"
                );
                assert!(
                    bounds.right() <= clip.right() + 0.5,
                    "{value} bleeds right: {bounds:?} / {clip:?}"
                );
            }
        }
    }
}
