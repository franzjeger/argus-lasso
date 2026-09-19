//! Processes tab: CPU history + per-CPU bars + filter + sortable process table.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use egui::RichText;

use crate::gui::cpu_bars::{CpuBarsWidget, CpuHistoryWidget};
use crate::gui::theme::{self, Breeze};
use crate::monitor::{DaemonCmd, ProcInfo}; // ProcInfo used by RowItem
use crate::rules::RuleEngine;
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

// ── Pending kill (undo support) ───────────────────────────────────────────────

pub struct PendingKill {
    pub pid: u32,
    pub name: String,
    pub force: bool,
    pub deadline: std::time::Instant,
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

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &[ProcInfo],
        throttled_pids: &std::collections::HashSet<u32>,
        suspended_pids: &std::collections::HashSet<u32>,
        _cmd_tx: &crossbeam_channel::Sender<DaemonCmd>,
        _rule_engine: &Arc<Mutex<RuleEngine>>,
        gaming_active: bool,
        proc_cpu_history: &std::collections::HashMap<u32, std::collections::VecDeque<f32>>,
    ) -> TableAction {
        // Hoisted so the toolbar (rendered before the table) can set it.
        let mut action = TableAction::None;
        // CPU history chart and the per-core grid side by side — stacking them
        // cost ~90px of vertical space that the table wants.
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
            theme::plot_card(ui, hist_w, row_h, |ui| self.history.show(ui));
            ui.add_space(theme::tokens::SPACE_S);
            theme::plot_card(ui, grid_w, row_h, |ui| self.bars.show(ui));
        });
        ui.add_space(theme::tokens::SPACE_S);

        // Keyboard shortcuts — all ctx calls MUST be outside ui.input():
        // ctx.input() holds the ContextImpl WRITE lock; calling ctx.read() or ctx.write()
        // inside it causes write→read or write→write re-entrant deadlock (parking_lot panics
        // after 10s with "Failed to acquire RwLock … Deadlock?").
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

        // Filter row + view toggles
        ui.horizontal(|ui| {
            ui.label("🔍");
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .id(filter_id)
                    .hint_text("name / PID / cmdline — press /")
                    .desired_width(240.0),
            );
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
                    .hint_text("e.g. 8080")
                    .desired_width(70.0),
            );
            if !self.port_filter.is_empty() && ui.small_button("✕").clicked() {
                self.port_filter.clear();
            }
            ui.add_space(theme::tokens::SPACE_S);

            // Quick-filter chips (§4) — pills, filled when active
            for (state, label, hover) in [
                (
                    &mut self.chip_high_cpu,
                    "High CPU",
                    "Only processes using ≥ 25% of total CPU capacity",
                ),
                (
                    &mut self.chip_throttled,
                    "Throttled",
                    "Only processes currently throttled by ProBalance",
                ),
                (
                    &mut self.chip_suspended,
                    "Suspended",
                    "Only processes you have suspended",
                ),
            ] {
                let resp_clicked = theme::chip(ui, label, *state);
                if resp_clicked {
                    *state = !*state;
                }
                let _ = hover;
            }

            // Right side: view toggles + the column picker, which used to be
            // discoverable only via a header right-click.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("Export ▾", |ui| {
                    if ui.button("CSV").clicked() {
                        action = TableAction::Export {
                            format: ExportFormat::Csv,
                        };
                        ui.close();
                    }
                    if ui.button("JSON").clicked() {
                        action = TableAction::Export {
                            format: ExportFormat::Json,
                        };
                        ui.close();
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

        // Sort + filter (name, PID, or cmdline).
        // Sort references, not clones — deep-copying ~1000 ProcInfo rows
        // (several heap Strings each) on every repaint is pure waste.
        let mut sorted: Vec<&ProcInfo> = snapshot.iter().collect();
        if !self.filter.is_empty() {
            if self.filter_is_regex {
                // Invalid regex: keep everything rather than hiding all rows
                if let Some((_, Some(re))) = &self.filter_regex_cache {
                    sorted.retain(|p| re.is_match(&p.name) || re.is_match(&p.cmdline));
                }
            } else {
                let filter_lower = self.filter.to_lowercase();
                sorted.retain(|p| {
                    p.name.to_lowercase().contains(&filter_lower)
                        || p.pid.to_string().contains(&filter_lower)
                        || p.cmdline.to_lowercase().contains(&filter_lower)
                });
            }
        }
        if !self.port_filter.trim().is_empty() {
            if let Ok(port) = self.port_filter.trim().parse::<u16>() {
                let key = format!("{}|{}", self.port_filter.trim(), self.filter);
                let now = std::time::Instant::now();
                let pids = match &self.port_pids_cache {
                    Some((k, ts, set))
                        if k == &key
                            && now.duration_since(*ts) < std::time::Duration::from_secs(1) =>
                    {
                        set.clone()
                    }
                    _ => {
                        let candidates: Vec<u32> = sorted.iter().map(|p| p.pid).collect();
                        let set = crate::utils::pids_for_port(port, &candidates);
                        self.port_pids_cache = Some((key, now, set.clone()));
                        set
                    }
                };
                sorted.retain(|p| pids.contains(&p.pid));
            }
        }
        if self.chip_high_cpu {
            sorted.retain(|p| p.cpu_percent >= 25.0);
        }
        if self.chip_throttled {
            sorted.retain(|p| throttled_pids.contains(&p.pid));
        }
        if self.chip_suspended {
            sorted.retain(|p| suspended_pids.contains(&p.pid));
        }

        let asc = self.sort_asc;
        // All sorts use PID as a stable tiebreaker so equal rows never flicker.
        match self.sort_col {
            SortCol::Pid => sorted.sort_by(|a, b| {
                if asc {
                    a.pid.cmp(&b.pid)
                } else {
                    b.pid.cmp(&a.pid)
                }
            }),
            SortCol::Name => sorted.sort_by(|a, b| {
                (if asc {
                    a.name.cmp(&b.name)
                } else {
                    b.name.cmp(&a.name)
                })
                .then(a.pid.cmp(&b.pid))
            }),
            SortCol::Cpu => sorted.sort_by(|a, b| {
                let ord = if asc {
                    a.cpu_percent.partial_cmp(&b.cpu_percent)
                } else {
                    b.cpu_percent.partial_cmp(&a.cpu_percent)
                };
                ord.unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.pid.cmp(&b.pid))
            }),
            SortCol::Gpu => sorted.sort_by(|a, b| {
                let ord = if asc {
                    a.gpu_percent.partial_cmp(&b.gpu_percent)
                } else {
                    b.gpu_percent.partial_cmp(&a.gpu_percent)
                };
                ord.unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.pid.cmp(&b.pid))
            }),
            SortCol::Mem => sorted.sort_by(|a, b| {
                (if asc {
                    a.mem_rss.cmp(&b.mem_rss)
                } else {
                    b.mem_rss.cmp(&a.mem_rss)
                })
                .then(a.pid.cmp(&b.pid))
            }),
            SortCol::Nice => sorted.sort_by(|a, b| {
                (if asc {
                    a.nice.cmp(&b.nice)
                } else {
                    b.nice.cmp(&a.nice)
                })
                .then(a.pid.cmp(&b.pid))
            }),
            SortCol::Affinity => sorted.sort_by(|a, b| {
                (if asc {
                    a.affinity.cmp(&b.affinity)
                } else {
                    b.affinity.cmp(&a.affinity)
                })
                .then(a.pid.cmp(&b.pid))
            }),
            SortCol::Ionice => sorted.sort_by(|a, b| {
                (if asc {
                    a.ionice.cmp(&b.ionice)
                } else {
                    b.ionice.cmp(&a.ionice)
                })
                .then(a.pid.cmp(&b.pid))
            }),
            SortCol::Status => sorted.sort_by(|a, b| {
                // Rank: suspended (2) > throttled (1) > running (0)
                let rank = |p: &crate::monitor::ProcInfo| -> u8 {
                    if suspended_pids.contains(&p.pid) {
                        2
                    } else if throttled_pids.contains(&p.pid) {
                        1
                    } else {
                        0
                    }
                };
                (if asc {
                    rank(a).cmp(&rank(b))
                } else {
                    rank(b).cmp(&rank(a))
                })
                .then(a.pid.cmp(&b.pid))
            }),
        }

        // Offline CPUs for affinity display (cached; refreshed in update_cpu)
        let offline = if gaming_active && self.hide_parked_in_proc_view {
            self.cached_offline.clone()
        } else {
            HashSet::new()
        };

        let sort_col_cur = self.sort_col.clone();
        let sort_asc_cur = self.sort_asc;
        let hide_parked = self.hide_parked_in_proc_view;
        let core_pairs = &self.core_pairs;

        let mut new_sort_col = sort_col_cur.clone();
        let mut new_sort_asc = sort_asc_cur;
        let mut new_selected = self.selected_pid;

        // Delete key — kill the currently selected process.
        // Only when no widget (e.g. the filter text box) has keyboard focus,
        // otherwise editing text could kill the selected process.
        let text_has_focus = ui.ctx().memory(|m| m.focused().is_some());
        ui.input(|i| {
            if i.key_pressed(egui::Key::Delete) && !text_has_focus {
                if let Some(sel_pid) = self.selected_pid {
                    if let Some(proc) = sorted.iter().find(|p| p.pid == sel_pid) {
                        action = TableAction::Kill {
                            pid: sel_pid,
                            name: proc.name.to_string(),
                            force: false,
                        };
                    }
                }
            }
        });

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
            measure(sorted.iter().map(|p| p.pid).max().unwrap_or(0).to_string()).max(60.0),
            150.0,
            SPARK_W
                + measure(format!(
                    "{:.1}",
                    sorted
                        .iter()
                        .map(|p| p.cpu_percent)
                        .fold(100.0_f32, f32::max)
                )),
            measure("100".into()).max(60.0),
            measure(format!(
                "{:.1}",
                sorted.iter().map(|p| p.mem_rss).max().unwrap_or(0) as f64 / 1_048_576.0
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
        let col_widths = self.col_widths.clone();
        let total_cols_w: f32 = visible.iter().map(|&i| col_widths[i]).sum();
        // Per-column (x offset, width) in the current visible layout — used by
        // the hover-tooltip hit rects below.
        let col_layout: HashMap<usize, (f32, f32)> = {
            let mut m = HashMap::new();
            let mut x = 0.0f32;
            for &i in &visible {
                m.insert(i, (x, col_widths[i]));
                x += col_widths[i];
            }
            m
        };

        // Wrap table in a visible border frame
        let frame_border_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
        let mut col_width_deltas = [0.0f32; 9];
        egui::ScrollArea::horizontal()
            .id_salt("process_table_horizontal")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame::new()
                    .stroke(egui::Stroke::new(1.0_f32, frame_border_color))
                    .inner_margin(egui::Margin::same(1))
                    .show(ui, |ui| {
                        // ── Sortable header (pinned, outside scroll area) ─────────────────
                        let (header_rect, _) = ui.allocate_exact_size(
                            egui::Vec2::new(total_cols_w, HEADER_H),
                            egui::Sense::hover(),
                        );
                        // Header background
                        ui.painter().rect_filled(
                            header_rect,
                            0.0,
                            ui.visuals().widgets.noninteractive.bg_fill,
                        );
                        {
                            let mut x = header_rect.min.x;
                            for &i in &visible {
                                let col = &COLS[i];
                                let cw = col_widths[i];
                                let cell_rect = egui::Rect::from_min_size(
                                    egui::Pos2::new(x + PAD, header_rect.min.y),
                                    egui::Vec2::new(cw - PAD, HEADER_H),
                                );
                                let is_active = *col == sort_col_cur && !self.tree_view;
                                let label_str = if is_active {
                                    format!(
                                        "{} {}",
                                        col.label(),
                                        if sort_asc_cur { "▲" } else { "▼" }
                                    )
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
                                if resp.clicked() && !self.tree_view {
                                    if *col == sort_col_cur {
                                        new_sort_asc = !sort_asc_cur;
                                    } else {
                                        new_sort_col = col.clone();
                                        new_sort_asc =
                                            matches!(col, SortCol::Name | SortCol::Affinity);
                                    }
                                }
                                // Right-click any header → column chooser
                                resp.context_menu(|ui| {
                                    ui.label(RichText::new("Columns ▾").font(crate::gui::theme::bold_font(crate::gui::theme::tokens::FONT_BODY)));
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
                                                // Hiding the active sort column would
                                                // strand an invisible sort with no way
                                                // to change direction — fall back.
                                                if *c == new_sort_col {
                                                    new_sort_col = SortCol::Cpu;
                                                    new_sort_asc = false;
                                                }
                                            }
                                            self.hidden_dirty = true;
                                        }
                                    }
                                });
                                x += cw;
                            }
                            // Drag-to-resize handles — one between each visible column pair
                            x = header_rect.min.x;
                            for (vi, &i) in visible
                                .iter()
                                .enumerate()
                                .take(visible.len().saturating_sub(1))
                            {
                                x += col_widths[i];
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
                                        // Name auto-fills, so its delta is discarded —
                                        // move this boundary by resizing the column on
                                        // the RIGHT inversely instead (dragging right
                                        // grows Name = shrinks the right neighbor).
                                        let right = visible[vi + 1];
                                        col_width_deltas[right] -= resp.drag_delta().x;
                                    } else {
                                        col_width_deltas[i] += resp.drag_delta().x;
                                    }
                                }
                            }
                        }
                        // Separator line between header and body
                        ui.painter().line_segment(
                            [header_rect.left_bottom(), header_rect.right_bottom()],
                            egui::Stroke::new(
                                1.0_f32,
                                ui.visuals().widgets.noninteractive.bg_stroke.color,
                            ),
                        );

                        // ── Scrollable body ───────────────────────────────────────────────
                        // Build tree-ordered row list when tree_view is active.
                        struct RowItem<'a> {
                            proc: &'a ProcInfo,
                            depth: usize,
                        }
                        let row_items: Vec<RowItem> = if self.tree_view {
                            let pid_set: HashSet<u32> = sorted.iter().map(|p| p.pid).collect();
                            let mut children: HashMap<u32, Vec<usize>> = HashMap::new();
                            let mut roots: Vec<usize> = Vec::new();
                            for (i, p) in sorted.iter().enumerate() {
                                if p.ppid == 0 || !pid_set.contains(&p.ppid) {
                                    roots.push(i);
                                } else {
                                    children.entry(p.ppid).or_default().push(i);
                                }
                            }
                            // Sort children by name for stable display
                            for v in children.values_mut() {
                                v.sort_by_key(|&i| &sorted[i].name);
                            }
                            roots.sort_by_key(|&i| &sorted[i].name);
                            let mut result = Vec::new();
                            let mut stack: Vec<(usize, usize)> =
                                roots.iter().map(|&i| (i, 0)).collect();
                            stack.reverse();
                            while let Some((idx, depth)) = stack.pop() {
                                result.push(RowItem {
                                    proc: sorted[idx],
                                    depth,
                                });
                                if let Some(ch) = children.get(&sorted[idx].pid) {
                                    let mut ch_sorted = ch.clone();
                                    ch_sorted.sort_by_key(|&i| &sorted[i].name);
                                    for ci in ch_sorted.into_iter().rev() {
                                        stack.push((ci, depth + 1));
                                    }
                                }
                            }
                            result
                        } else {
                            sorted
                                .iter()
                                .map(|&p| RowItem { proc: p, depth: 0 })
                                .collect()
                        };

                        // show_rows virtualizes the table: only visible rows are
                        // formatted and painted (rows are fixed ROW_H height).
                        egui::ScrollArea::vertical()
                            .id_salt("process_scroll")
                            .auto_shrink([false, false])
                            .show_rows(ui, ROW_H, row_items.len(), |ui, range| {
                                for (i, item) in row_items[range.clone()].iter().enumerate() {
                                    let row_idx = range.start + i;
                                    let proc = item.proc;
                                    let indent = item.depth as f32 * 14.0;
                                    let pid = proc.pid;
                                    let is_sel = new_selected == Some(pid);
                                    let throttled = throttled_pids.contains(&pid);
                                    let cpu = proc.cpu_percent;
                                    // Mockup 1d keeps PID/NAME in the plain text colour —
                                    // load is carried by the CPU% value and its sparkline.
                                    // Throttled rows still get a warning tint as the badge
                                    // alone is easy to miss when scanning.
                                    let row_col = if throttled {
                                        theme::sem(ui).warning
                                    } else {
                                        ui.visuals().text_color()
                                    };
                                    let _ = &cpu;
                                    let aff_full = format_affinity_display(
                                        &proc.affinity,
                                        &offline,
                                        core_pairs,
                                        hide_parked,
                                    );
                                    // Truncate affinity if very long, show full string in tooltip
                                    const AFF_MAX: usize = 14;
                                    let aff_display = if aff_full.len() > AFF_MAX {
                                        format!("{}…", &aff_full[..AFF_MAX.saturating_sub(1)])
                                    } else {
                                        aff_full.clone()
                                    };
                                    let ionice_str = fmt_ionice(&proc.ionice);
                                    let is_suspended = suspended_pids.contains(&pid);
                                    // Status renders as a badge (drawn below), so the
                                    // text slot for that column stays empty.
                                    let status_str = "";
                                    // CPU% value + its sparkline share one ramp colour
                                    let load_col = theme::load_color(ui, cpu);
                                    let sem = theme::sem(ui);

                                    // Clone fields needed inside closures
                                    let name = proc.name.clone();
                                    let aff = proc.affinity.clone();
                                    let nice = proc.nice;
                                    let cmdline = proc.cmdline.clone();
                                    let drb = proc.disk_read_bps;
                                    let dwb = proc.disk_write_bps;

                                    // Allocate the full row — advances the cursor.
                                    // Interact via a PID-stable id: the allocate
                                    // response uses a positional auto-id, so an open
                                    // context menu would rebind to whatever process
                                    // lands in that slot after a re-sort or scroll —
                                    // "Kill" could then hit the wrong process.
                                    let (row_rect, _) = ui.allocate_exact_size(
                                        egui::Vec2::new(total_cols_w, ROW_H),
                                        egui::Sense::hover(),
                                    );
                                    let row_resp = ui.interact(
                                        row_rect,
                                        ui.make_persistent_id(("proc_row", pid)),
                                        egui::Sense::click(),
                                    );

                                    // Row background
                                    let bg = if is_sel {
                                        ui.visuals().selection.bg_fill
                                    } else if row_idx % 2 == 1 {
                                        ui.visuals().faint_bg_color
                                    } else {
                                        ui.visuals().extreme_bg_color
                                    };
                                    ui.painter().rect_filled(row_rect, 0.0, bg);

                                    // Paint cell text directly
                                    if ui.is_rect_visible(row_rect) {
                                        let font = egui::FontId::proportional(
                                            crate::gui::theme::tokens::FONT_BODY,
                                        );
                                        let num_font =
                                            theme::num_font(crate::gui::theme::tokens::FONT_BODY);
                                        let mut x = row_rect.min.x;
                                        for &ci in &visible {
                                            let cw = col_widths[ci];
                                            let cell_rect = egui::Rect::from_min_size(
                                                egui::pos2(x, row_rect.top()),
                                                egui::vec2(cw, ROW_H),
                                            );
                                            let painter = ui.painter().with_clip_rect(
                                                ui.clip_rect().intersect(cell_rect),
                                            );
                                            let x_off = if ci == 1 { indent } else { 0.0 };
                                            // Numeric columns are right-aligned (§2) so
                                            // live values don't jitter; text columns
                                            // stay left-aligned. CPU% is numeric too —
                                            // it used to be left-aligned at 45% of the
                                            // column, so its indent moved with the
                                            // width and could collide with the
                                            // sparkline.
                                            let numeric = matches!(ci, 0 | 2 | 3 | 4 | 5);
                                            let text_pos = if numeric {
                                                egui::pos2(
                                                    x + cw - PAD - NUM_INSET,
                                                    row_rect.center().y,
                                                )
                                            } else {
                                                egui::pos2(x + PAD + x_off, row_rect.center().y)
                                            };
                                            let align = if numeric {
                                                egui::Align2::RIGHT_CENTER
                                            } else {
                                                egui::Align2::LEFT_CENTER
                                            };
                                            let text: std::borrow::Cow<str> = match ci {
                                                0 => pid.to_string().into(),
                                                1 => name.as_ref().into(),
                                                2 => format!("{:.1}", cpu).into(),
                                                3 => {
                                                    if proc.gpu_percent > 0.0 {
                                                        format!("{:.0}", proc.gpu_percent).into()
                                                    } else {
                                                        "—".into()
                                                    }
                                                }
                                                4 => format!(
                                                    "{:.1}",
                                                    proc.mem_rss as f64 / 1_048_576.0
                                                )
                                                .into(),
                                                5 => nice.to_string().into(),
                                                6 => aff_display.as_str().into(),
                                                7 => ionice_str.as_str().into(),
                                                8 => status_str.into(),
                                                _ => "".into(),
                                            };
                                            // Draw mini sparkline in left portion of CPU% cell
                                            if ci == 2 {
                                                if let Some(hist) = proc_cpu_history.get(&pid) {
                                                    if hist.len() >= 2 {
                                                        let spark_w = SPARK_W.min(cw * 0.42);
                                                        let spark_rect = egui::Rect::from_min_size(
                                                            egui::pos2(
                                                                x + 1.0,
                                                                row_rect.min.y + 2.0,
                                                            ),
                                                            egui::vec2(spark_w, ROW_H - 4.0),
                                                        );
                                                        let lo = hist
                                                            .iter()
                                                            .cloned()
                                                            .fold(f32::INFINITY, f32::min);
                                                        let hi = hist
                                                            .iter()
                                                            .cloned()
                                                            .fold(f32::NEG_INFINITY, f32::max)
                                                            .max(lo + 0.1);
                                                        let pts: Vec<egui::Pos2> = hist
                                                            .iter()
                                                            .enumerate()
                                                            .map(|(i, &v)| {
                                                                let px = spark_rect.left()
                                                                    + i as f32
                                                                        / (hist.len() - 1).max(1)
                                                                            as f32
                                                                        * spark_rect.width();
                                                                let py = spark_rect.bottom()
                                                                    - (v - lo) / (hi - lo)
                                                                        * spark_rect.height();
                                                                egui::pos2(px, py)
                                                            })
                                                            .collect();
                                                        // Sparkline shares the CPU%
                                                        // value colour (§1 one ramp)
                                                        let spark_col = load_col;
                                                        for pair in pts.windows(2) {
                                                            painter.line_segment(
                                                                [pair[0], pair[1]],
                                                                egui::Stroke::new(
                                                                    1.0_f32, spark_col,
                                                                ),
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                            // Status column renders as a badge, not
                                            // emoji+text; other cells paint their text.
                                            if ci == 8 {
                                                if is_suspended {
                                                    theme::badge_at(
                                                        &painter,
                                                        egui::pos2(x + PAD, row_rect.center().y),
                                                        "Suspended",
                                                        sem.accent,
                                                    );
                                                } else if throttled {
                                                    theme::badge_at(
                                                        &painter,
                                                        egui::pos2(x + PAD, row_rect.center().y),
                                                        "Throttled",
                                                        sem.warning,
                                                    );
                                                }
                                            } else {
                                                // CPU% carries the load colour; the
                                                // rest use the normal row colour.
                                                let col = if ci == 2 { load_col } else { row_col };
                                                let f = if numeric || ci == 2 {
                                                    num_font.clone()
                                                } else {
                                                    font.clone()
                                                };
                                                painter.text(
                                                    text_pos,
                                                    align,
                                                    text.as_ref(),
                                                    f,
                                                    col,
                                                );
                                            }
                                            x += cw;
                                        }

                                        // Tooltip: hover name → cmdline + disk I/O + full affinity
                                        if row_resp.hovered() {
                                            let ptr = ui.ctx().pointer_hover_pos();
                                            // Cell hit-rects from the visible layout
                                            // (fixes stale hard-coded offsets too).
                                            let cell_rect = |idx: usize| {
                                                col_layout.get(&idx).map(|&(off, w)| {
                                                    egui::Rect::from_min_size(
                                                        egui::pos2(
                                                            row_rect.min.x + off,
                                                            row_rect.min.y,
                                                        ),
                                                        egui::vec2(w, ROW_H),
                                                    )
                                                })
                                            };
                                            let name_rect =
                                                cell_rect(1).unwrap_or(egui::Rect::NOTHING);
                                            let aff_rect =
                                                cell_rect(6).unwrap_or(egui::Rect::NOTHING);
                                            if ptr.is_some_and(|p| name_rect.contains(p)) {
                                                ui.ctx().set_cursor_icon(egui::CursorIcon::Default);
                                                #[allow(deprecated)]
                                                egui::show_tooltip_at_pointer(
                                                    ui.ctx(),
                                                    ui.layer_id(),
                                                    egui::Id::new(("proc_tip", pid)),
                                                    |ui| {
                                                        ui.label(
                                                            egui::RichText::new(name.as_ref())
                                                                .font(crate::gui::theme::bold_font(crate::gui::theme::tokens::FONT_BODY)),
                                                        );
                                                        if !cmdline.is_empty() {
                                                            ui.label(
                                                                egui::RichText::new(
                                                                    cmdline.as_str(),
                                                                )
                                                                .size(crate::gui::theme::tokens::FONT_HELP)
                                                                .color(
                                                                    ui.visuals().weak_text_color(),
                                                                ),
                                                            );
                                                        }
                                                        ui.separator();
                                                        ui.label(format!(
                                                            "PID: {}   PPID: {}",
                                                            pid, proc.ppid
                                                        ));
                                                        ui.label(format!(
                                                            "Disk R: {}   W: {}",
                                                            fmt_bps(drb),
                                                            fmt_bps(dwb)
                                                        ));
                                                    },
                                                );
                                            } else if ptr.is_some_and(|p| aff_rect.contains(p))
                                                && aff_full.len() > AFF_MAX
                                            {
                                                #[allow(deprecated)]
                                                egui::show_tooltip_at_pointer(
                                                    ui.ctx(),
                                                    ui.layer_id(),
                                                    egui::Id::new(("aff_tip", pid)),
                                                    |ui| {
                                                        ui.label(&aff_full);
                                                    },
                                                );
                                            }
                                        }
                                    }

                                    // Click → select row; double-click → details window
                                    if row_resp.clicked() {
                                        new_selected = Some(pid);
                                    }
                                    if row_resp.double_clicked() {
                                        action = TableAction::ShowDetails { pid };
                                    }

                                    // Right-click context menu on the entire row
                                    row_resp.context_menu(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!("{name} · PID {pid}"))
                                                .font(crate::gui::theme::bold_font(crate::gui::theme::tokens::FONT_BODY)),
                                        );
                                        ui.separator();
                                        if ui.button("End process").clicked() {
                                            action = TableAction::Kill {
                                                pid,
                                                name: name.to_string(),
                                                force: false,
                                            };
                                            ui.close();
                                        }
                                        if ui.button("Force quit process").clicked() {
                                            action = TableAction::Kill {
                                                pid,
                                                name: name.to_string(),
                                                force: true,
                                            };
                                            ui.close();
                                        }
                                        if ui.button("End process and children").clicked() {
                                            action = TableAction::KillTree {
                                                pid,
                                                name: name.to_string(),
                                            };
                                            ui.close();
                                        }
                                        if is_suspended {
                                            if ui.button("Resume process").clicked() {
                                                action = TableAction::Resume {
                                                    pid,
                                                    name: name.to_string(),
                                                };
                                                ui.close();
                                            }
                                        } else if ui.button("Pause process").clicked() {
                                            action = TableAction::Suspend {
                                                pid,
                                                name: name.to_string(),
                                            };
                                            ui.close();
                                        }
                                        ui.separator();
                                        if ui.button("CPU assignment…").clicked() {
                                            action = TableAction::SetAffinity {
                                                pid,
                                                name: name.to_string(),
                                                current: aff.to_string(),
                                            };
                                            ui.close();
                                        }
                                        if ui.button("CPU priority…").clicked() {
                                            action = TableAction::SetNice {
                                                pid,
                                                name: name.to_string(),
                                                current: nice,
                                            };
                                            ui.close();
                                        }
                                        if ui.button("Disk I/O priority…").clicked() {
                                            action = TableAction::SetIonice {
                                                pid,
                                                name: name.to_string(),
                                            };
                                            ui.close();
                                        }
                                        ui.separator();
                                        if ui.button("Create process rule…").clicked() {
                                            action = TableAction::AddRule {
                                                name: name.to_string(),
                                            };
                                            ui.close();
                                        }
                                    });
                                }
                            }); // end ScrollArea
                    }); // end Frame border
            }); // end horizontal scrolling

        // Apply column resize deltas (index 1 = name auto-fills, skip it)
        self.cols_dirty = false;
        for (i, &delta) in col_width_deltas.iter().enumerate() {
            if delta != 0.0 && i != 1 {
                self.col_widths[i] = (self.col_widths[i] + delta).max(min_widths[i]);
                self.cols_dirty = true;
            }
        }

        self.sort_col = new_sort_col;
        self.sort_asc = new_sort_asc;
        self.selected_pid = new_selected;
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_pid_and_multicore_numbers_fit_after_old_widths_and_window_resize() {
        let ctx = egui::Context::default();
        theme::apply_theme(&ctx, 1.0, &theme::AppTheme::BreezeDark);
        let mut tab = ProcessTab::new(&[30.0; 9], &[]);
        let snapshot = [ProcInfo {
            pid: 2_147_483_647,
            name: "Long PID regression".into(),
            cpu_percent: 100.0,
            mem_rss: 128 * 1024 * 1024 * 1024,
            ..Default::default()
        }];
        let (tx, _rx) = crossbeam_channel::unbounded();
        let rules = Arc::new(Mutex::new(RuleEngine::new()));
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
                output = Some(ctx.run_ui(input.clone(), |root| {
                    egui::CentralPanel::default().show_inside(root, |ui| {
                        tab.show(
                            ui,
                            &snapshot,
                            &HashSet::new(),
                            &HashSet::new(),
                            &tx,
                            &rules,
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
