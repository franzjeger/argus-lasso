//! Modal dialogs: affinity picker, nice dialog, ionice dialog, rule edit,
//! process picker, Steam game picker, Lutris game picker, rule presets.

use egui::{Context, Ui, ViewportBuilder, ViewportId};
use std::collections::HashSet;

use crate::cpu_park::{detect_topology, get_smt_siblings_of};
use crate::gui::theme::Breeze;
use crate::rules::Rule;
use crate::utils::{cpulist_to_set, cpuset_to_cpulist, get_cpu_count, get_offline_cpus};

// ── AffinityPicker (reusable inline widget) ───────────────────────────────────

/// Inline CPU-affinity picker: CCD buttons, physical+HT checkbox pairs, convenience buttons.
/// Renders inside any existing UI panel — no window wrapping.
pub struct AffinityPicker {
    pub checkboxes: Vec<bool>,
    pub cpu_count: u32,
    pub offline: HashSet<u32>,
    pub preferred: HashSet<u32>,
    pub non_preferred: HashSet<u32>,
    pub smt_siblings: HashSet<u32>,
    pub preferred_label: String,
    pub non_preferred_label: String,
}

impl AffinityPicker {
    /// Initialise from an existing cpulist string (empty = all CPUs selected).
    pub fn new(current_affinity: &str) -> Self {
        let cpu_count = get_cpu_count();
        let offline = get_offline_cpus();
        let topo = detect_topology();
        let all_cpus: HashSet<u32> = (0..cpu_count).collect();
        let smt_siblings = get_smt_siblings_of(&all_cpus);

        let selected = if current_affinity.trim().is_empty() {
            (0..cpu_count).collect::<HashSet<_>>()
        } else {
            cpulist_to_set(current_affinity).unwrap_or_default()
        };

        let checkboxes = (0..cpu_count as usize)
            .map(|i| selected.contains(&(i as u32)))
            .collect();

        let has_asym = topo.has_asymmetry();
        Self {
            checkboxes,
            cpu_count,
            offline,
            preferred: if has_asym {
                topo.preferred.clone()
            } else {
                HashSet::new()
            },
            non_preferred: if has_asym {
                topo.non_preferred.clone()
            } else {
                HashSet::new()
            },
            smt_siblings,
            preferred_label: topo.preferred_label.clone(),
            non_preferred_label: topo.non_preferred_label.clone(),
        }
    }

    /// Current cpulist string computed from checkbox state.
    pub fn cpulist(&self) -> String {
        let selected: HashSet<u32> = self
            .checkboxes
            .iter()
            .enumerate()
            .filter(|(_, &b)| b)
            .map(|(i, _)| i as u32)
            .collect();
        cpuset_to_cpulist(&selected)
    }

    /// Render the picker inline. Call inside any ui.group() / ui.vertical() etc.
    pub fn show(&mut self, ui: &mut Ui) {
        // Parked CPUs warning
        if !self.offline.is_empty() {
            let offline_str = cpuset_to_cpulist(&self.offline);
            ui.colored_label(Breeze::WARNING, format!("CPUs {offline_str} are parked"));
            ui.add_space(2.0);
        }

        // CCD / topology quick buttons
        if !self.preferred.is_empty() {
            ui.horizontal(|ui| {
                ui.label("Quick:");
                if ui
                    .button(format!("{} (preferred)", self.preferred_label))
                    .clicked()
                {
                    for (i, cb) in self.checkboxes.iter_mut().enumerate() {
                        *cb = self.preferred.contains(&(i as u32));
                    }
                }
                if ui
                    .button(format!("{} (non-preferred)", self.non_preferred_label))
                    .clicked()
                {
                    for (i, cb) in self.checkboxes.iter_mut().enumerate() {
                        *cb = self.non_preferred.contains(&(i as u32));
                    }
                }
                if ui.button("All CPU threads").clicked() {
                    for (i, cb) in self.checkboxes.iter_mut().enumerate() {
                        if !self.offline.contains(&(i as u32)) {
                            *cb = true;
                        }
                    }
                }
                let has_smt = !self.smt_siblings.is_empty();
                ui.add_enabled_ui(has_smt, |ui| {
                    if ui.button("No SMT").clicked() {
                        for (i, cb) in self.checkboxes.iter_mut().enumerate() {
                            *cb = !self.smt_siblings.contains(&(i as u32))
                                && !self.offline.contains(&(i as u32));
                        }
                    }
                });
            });
        } else {
            ui.horizontal(|ui| {
                ui.label("Quick:");
                if ui.button("All CPU threads").clicked() {
                    for (i, cb) in self.checkboxes.iter_mut().enumerate() {
                        if !self.offline.contains(&(i as u32)) {
                            *cb = true;
                        }
                    }
                }
                let has_smt = !self.smt_siblings.is_empty();
                ui.add_enabled_ui(has_smt, |ui| {
                    if ui.button("No SMT").clicked() {
                        for (i, cb) in self.checkboxes.iter_mut().enumerate() {
                            *cb = !self.smt_siblings.contains(&(i as u32))
                                && !self.offline.contains(&(i as u32));
                        }
                    }
                });
                if ui.button("None").clicked() {
                    self.checkboxes.fill(false);
                }
            });
        }

        ui.add_space(4.0);

        // Core checkbox grid — physical+HT pairs side by side
        egui::ScrollArea::vertical()
            .max_height(180.0)
            .id_salt("aff_picker_scroll")
            .show(ui, |ui| {
                if !self.preferred.is_empty() {
                    // Topology-aware sections
                    let phys_pref: Vec<u32> = {
                        let mut v: Vec<u32> = self
                            .preferred
                            .iter()
                            .filter(|c| !self.smt_siblings.contains(c))
                            .copied()
                            .collect();
                        v.sort_unstable();
                        v
                    };
                    let phys_npref: Vec<u32> = {
                        let mut v: Vec<u32> = self
                            .non_preferred
                            .iter()
                            .filter(|c| !self.smt_siblings.contains(c))
                            .copied()
                            .collect();
                        v.sort_unstable();
                        v
                    };

                    if !phys_pref.is_empty() {
                        ui.colored_label(
                            Breeze::HIGHLIGHT,
                            if self.preferred_label.is_empty() {
                                "Preferred cores"
                            } else {
                                &self.preferred_label
                            },
                        );
                        self.show_core_pairs(ui, &phys_pref);
                    }
                    if !phys_npref.is_empty() {
                        ui.colored_label(
                            ui.visuals().weak_text_color(),
                            if self.non_preferred_label.is_empty() {
                                "Non-preferred cores"
                            } else {
                                &self.non_preferred_label
                            },
                        );
                        self.show_core_pairs(ui, &phys_npref);
                    }
                } else {
                    // Flat grid of physical+HT pairs
                    let all_phys: Vec<u32> = {
                        let mut v: Vec<u32> = (0..self.cpu_count)
                            .filter(|c| !self.smt_siblings.contains(c))
                            .collect();
                        v.sort_unstable();
                        v
                    };
                    if all_phys.is_empty() {
                        // No topology info — flat grid
                        egui::Grid::new("aff_flat_grid")
                            .num_columns(8)
                            .show(ui, |ui| {
                                for i in 0..self.cpu_count as usize {
                                    let offline = self.offline.contains(&(i as u32));
                                    ui.add_enabled(
                                        !offline,
                                        egui::Checkbox::new(&mut self.checkboxes[i], i.to_string()),
                                    );
                                    if (i + 1) % 8 == 0 {
                                        ui.end_row();
                                    }
                                }
                            });
                    } else {
                        self.show_core_pairs(ui, &all_phys);
                    }
                }
            });

        // Read-only cpulist result
        let cpulist = self.cpulist();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Result:").color(ui.visuals().weak_text_color()));
            ui.label(
                egui::RichText::new(if cpulist.is_empty() {
                    "(none)"
                } else {
                    &cpulist
                })
                .color(Breeze::HIGHLIGHT),
            );
        });
    }

    /// Render physical+HT checkbox pairs using horizontal_wrapped for responsive layout.
    fn show_core_pairs(&mut self, ui: &mut Ui, phys_cpus: &[u32]) {
        ui.horizontal_wrapped(|ui| {
            for &phys in phys_cpus {
                let p = phys as usize;
                let p_offline = self.offline.contains(&phys);
                let sibling = self.find_sibling(phys);

                // Render the physical+HT pair inside a small horizontal group
                ui.add_enabled(
                    !p_offline,
                    egui::Checkbox::new(&mut self.checkboxes[p], format!("{phys}")),
                );

                if let Some(s) = sibling {
                    let s_usize = s as usize;
                    let s_offline = self.offline.contains(&s);
                    ui.add_enabled(
                        !s_offline,
                        egui::Checkbox::new(&mut self.checkboxes[s_usize], format!("HT{s}")),
                    );
                }

                // Visual separator between pairs
                ui.separator();
            }
        });
    }

    /// Find the HT sibling for a physical CPU by looking for a CPU in smt_siblings
    /// that has the same core topology. We use the half-offset heuristic as a fast path
    /// (correct for most AMD and Intel layouts).
    fn find_sibling(&self, phys: u32) -> Option<u32> {
        // Fast path: sibling = phys + N/2 where N = total logical CPUs
        let half = self.cpu_count / 2;
        if half > 0 {
            let candidate = phys + half;
            if candidate < self.cpu_count && self.smt_siblings.contains(&candidate) {
                return Some(candidate);
            }
        }
        // Slow path: scan smt_siblings for the one closest to phys
        self.smt_siblings
            .iter()
            .filter(|&&s| s > phys && s < phys + self.cpu_count)
            .min_by_key(|&&s| s.wrapping_sub(phys))
            .copied()
    }
}

// ── AffinityDialog ────────────────────────────────────────────────────────────

pub struct AffinityDialog {
    pub open: bool,
    pub title: String,
    pub checkboxes: Vec<bool>,
    pub preferred_label: String,
    pub non_preferred_label: String,
    pub cpu_count: u32,
    pub offline: HashSet<u32>,
    pub preferred: HashSet<u32>,
    pub non_preferred: HashSet<u32>,
    pub smt_siblings: HashSet<u32>,
    pub result: Option<String>,
}

impl AffinityDialog {
    pub fn new(current_affinity: &str, title: &str) -> Self {
        let cpu_count = get_cpu_count();
        let offline = get_offline_cpus();
        let topo = detect_topology();
        let all_cpus: HashSet<u32> = (0..cpu_count).collect();
        let smt_siblings = get_smt_siblings_of(&all_cpus);

        let selected = if current_affinity.is_empty() {
            (0..cpu_count).collect::<HashSet<_>>()
        } else {
            cpulist_to_set(current_affinity).unwrap_or_default()
        };

        let checkboxes = (0..cpu_count as usize)
            .map(|i| selected.contains(&(i as u32)))
            .collect();

        Self {
            open: true,
            title: title.to_string(),
            checkboxes,
            preferred_label: topo.preferred_label.clone(),
            non_preferred_label: topo.non_preferred_label.clone(),
            cpu_count,
            offline,
            preferred: if topo.has_asymmetry() {
                topo.preferred.clone()
            } else {
                HashSet::new()
            },
            non_preferred: if topo.has_asymmetry() {
                topo.non_preferred.clone()
            } else {
                HashSet::new()
            },
            smt_siblings,
            result: None,
        }
    }

    /// Returns Some(cpulist) when accepted, None while open, Some("") when cancelled.
    pub fn show(&mut self, ctx: &Context, opacity: f32) -> Option<String> {
        if !self.open {
            return self.result.clone();
        }

        let mut close_as: Option<bool> = None; // Some(true)=accept, Some(false)=cancel

        {
            let title_str = format!("Set CPU affinity — {}", self.title);
            let checkboxes = &mut self.checkboxes;
            let offline = &self.offline;
            let preferred = &self.preferred;
            let non_preferred = &self.non_preferred;
            let smt_siblings = &self.smt_siblings;
            let cpu_count = self.cpu_count;

            ctx.show_viewport_immediate(
                ViewportId::from_hash_of("affinity_dialog"),
                ViewportBuilder::default()
                    .with_title(title_str)
                    .with_app_id("argus-lasso")
                    .with_icon(egui::IconData { rgba: crate::icon::RGBA.to_vec(), width: crate::icon::W, height: crate::icon::H })
                    .with_min_inner_size([500.0, 440.0])
                    .with_transparent(true)
                    .with_resizable(true),
                |vp_ui, _class| {
                    let ctx = &vp_ui.ctx().clone();
                    crate::gui::theme::apply_viewport_opacity(vp_ui, opacity);
                    if ctx.input(|i| i.viewport().close_requested()) {
                        close_as = Some(false);
                    }
                    egui::CentralPanel::default().show_inside(vp_ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        if !offline.is_empty() {
                            let offline_str = cpuset_to_cpulist(offline);
                            ui.colored_label(
                                Breeze::WARNING,
                                format!("CPUs {offline_str} are parked — disable Gaming Mode to use them."),
                            );
                            ui.add_space(4.0);
                        }

                        ui.group(|ui| {
                            ui.label("Select CPUs:");
                            ui.add_space(4.0);

                            if !preferred.is_empty() && !non_preferred.is_empty() {
                                let ccd0_name = if self.preferred_label.is_empty() { "Preferred cores".to_string() } else { self.preferred_label.clone() };
                                let ccd1_name = if self.non_preferred_label.is_empty() { "Non-preferred cores (parked in Gaming Mode)".to_string() } else { format!("{} (parked in Gaming Mode)", self.non_preferred_label) };
                                let pref_phys: Vec<u32> = preferred.iter()
                                    .copied().filter(|c| !smt_siblings.contains(c))
                                    .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                                let pref_ht: Vec<u32> = preferred.iter()
                                    .copied().filter(|c| smt_siblings.contains(c))
                                    .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                                let npref_phys: Vec<u32> = non_preferred.iter()
                                    .copied().filter(|c| !smt_siblings.contains(c))
                                    .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                                let npref_ht: Vec<u32> = non_preferred.iter()
                                    .copied().filter(|c| smt_siblings.contains(c))
                                    .collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                                for (section_name, cpus) in [
                                    (format!("{ccd0_name} — physical cores"), pref_phys),
                                    (format!("{ccd0_name} — HT siblings"), pref_ht),
                                    (format!("{ccd1_name} — physical cores"), npref_phys),
                                    (format!("{ccd1_name} — HT siblings"), npref_ht),
                                ] {
                                    if cpus.is_empty() { continue; }
                                    ui.colored_label(Breeze::HIGHLIGHT, &section_name);
                                    ui.horizontal_wrapped(|ui| {
                                        for cpu in &cpus {
                                            let c = *cpu as usize;
                                            if c < checkboxes.len() {
                                                ui.add_enabled(
                                                    !offline.contains(cpu),
                                                    egui::Checkbox::new(&mut checkboxes[c], cpu.to_string()),
                                                );
                                            }
                                        }
                                    });
                                }
                            } else {
                                egui::Grid::new("cpu_grid").num_columns(8).show(ui, |ui| {
                                    for (i, cb) in checkboxes
                                        .iter_mut()
                                        .enumerate()
                                        .take(cpu_count as usize)
                                    {
                                        ui.add_enabled(
                                            !offline.contains(&(i as u32)),
                                            egui::Checkbox::new(cb, i.to_string()),
                                        );
                                        if (i + 1) % 8 == 0 { ui.end_row(); }
                                    }
                                });
                            }
                        });

                        ui.horizontal(|ui| {
                            if ui.button("All").clicked() {
                                for (i, cb) in checkboxes.iter_mut().enumerate() {
                                    if !offline.contains(&(i as u32)) { *cb = true; }
                                }
                            }
                            if ui.button("None").clicked() {
                                for (i, cb) in checkboxes.iter_mut().enumerate() {
                                    if !offline.contains(&(i as u32)) { *cb = false; }
                                }
                            }
                            if !preferred.is_empty() {
                                let pref_btn = if self.preferred_label.is_empty() { "Preferred".to_string() } else { self.preferred_label.clone() };
                                let npref_btn = if self.non_preferred_label.is_empty() { "Non-preferred".to_string() } else { self.non_preferred_label.clone() };
                                if ui.button(&pref_btn).clicked() {
                                    for (i, cb) in checkboxes.iter_mut().enumerate() {
                                        *cb = preferred.contains(&(i as u32));
                                    }
                                }
                                if ui.button(&npref_btn).clicked() {
                                    for (i, cb) in checkboxes.iter_mut().enumerate() {
                                        *cb = non_preferred.contains(&(i as u32));
                                    }
                                }
                            }
                        });

                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("OK").clicked() {
                                close_as = Some(true);

                            }
                            if ui.button("Cancel").clicked() {
                                close_as = Some(false);

                            }
                        });
                    });
                },
            );
        }

        if let Some(accept) = close_as {
            self.open = false;
            if accept {
                let selected: HashSet<u32> = self
                    .checkboxes
                    .iter()
                    .enumerate()
                    .filter(|(_, &b)| b)
                    .map(|(i, _)| i as u32)
                    .collect();
                if !selected.is_empty() {
                    let cpulist = cpuset_to_cpulist(&selected);
                    self.result = Some(cpulist.clone());
                    return Some(cpulist);
                }
                // empty selection — keep open
                self.open = true;
            } else {
                self.result = Some(String::new());
                return Some(String::new());
            }
        }
        None
    }
}

// ── NiceDialog ────────────────────────────────────────────────────────────────

pub struct NiceDialog {
    pub open: bool,
    pub title: String,
    pub value: i32,
    pub result: Option<Option<i32>>, // None = cancelled, Some(v) = accepted
}

impl NiceDialog {
    pub fn new(current: i32, title: &str) -> Self {
        Self {
            open: true,
            title: title.to_string(),
            value: current,
            result: None,
        }
    }

    pub fn show(&mut self, ctx: &Context, opacity: f32) -> Option<Option<i32>> {
        if !self.open {
            return self.result;
        }

        let mut close_as: Option<bool> = None;

        {
            let title_str = format!("Set CPU priority — {}", self.title);
            let value = &mut self.value;

            ctx.show_viewport_immediate(
                ViewportId::from_hash_of("nice_dialog"),
                ViewportBuilder::default()
                    .with_title(title_str)
                    .with_app_id("argus-lasso")
                    .with_icon(egui::IconData {
                        rgba: crate::icon::RGBA.to_vec(),
                        width: crate::icon::W,
                        height: crate::icon::H,
                    })
                    .with_min_inner_size([380.0, 160.0])
                    .with_transparent(true)
                    .with_resizable(false),
                |vp_ui, _class| {
                    let ctx = &vp_ui.ctx().clone();
                    crate::gui::theme::apply_viewport_opacity(vp_ui, opacity);
                    if ctx.input(|i| i.viewport().close_requested()) {
                        close_as = Some(false);
                    }
                    egui::CentralPanel::default().show_inside(vp_ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(
                            "Lower nice values give higher CPU priority. Raising priority may require system authentication.",
                        );
                        ui.add(egui::Slider::new(value, -20..=19).text("nice"));
                        ui.horizontal(|ui| {
                            for (label, val) in [
                                ("High (-10)", -10i32),
                                ("Normal (0)", 0),
                                ("Low (5)", 5),
                                ("Very Low (15)", 15),
                                ("Idle (19)", 19),
                            ] {
                                if ui.small_button(label).clicked() {
                                    *value = val;
                                }
                            }
                        });
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("OK").clicked() {
                                close_as = Some(true);

                            }
                            if ui.button("Cancel").clicked() {
                                close_as = Some(false);

                            }
                        });
                    });
                },
            );
        }

        if let Some(accept) = close_as {
            self.open = false;
            if accept {
                self.result = Some(Some(self.value));
                return Some(Some(self.value));
            } else {
                self.result = Some(None);
                return Some(None);
            }
        }
        None
    }
}

// ── IoNiceDialog ──────────────────────────────────────────────────────────────

pub struct IoNiceDialog {
    pub open: bool,
    pub title: String,
    pub class: i32,
    pub level: i32,
    pub result: Option<Option<(i32, i32)>>,
}

impl IoNiceDialog {
    pub fn new(title: &str) -> Self {
        Self {
            open: true,
            title: title.to_string(),
            class: 2,
            level: 4,
            result: None,
        }
    }

    pub fn show(&mut self, ctx: &Context, opacity: f32) -> Option<Option<(i32, i32)>> {
        if !self.open {
            return self.result;
        }

        let mut close_as: Option<bool> = None;

        {
            let title_str = format!("Set disk I/O priority — {}", self.title);
            let class = &mut self.class;
            let level = &mut self.level;

            ctx.show_viewport_immediate(
                ViewportId::from_hash_of("ionice_dialog"),
                ViewportBuilder::default()
                    .with_title(title_str)
                    .with_app_id("argus-lasso")
                    .with_icon(egui::IconData {
                        rgba: crate::icon::RGBA.to_vec(),
                        width: crate::icon::W,
                        height: crate::icon::H,
                    })
                    .with_min_inner_size([340.0, 160.0])
                    .with_transparent(true)
                    .with_resizable(false),
                |vp_ui, _class| {
                    let ctx = &vp_ui.ctx().clone();
                    crate::gui::theme::apply_viewport_opacity(vp_ui, opacity);
                    if ctx.input(|i| i.viewport().close_requested()) {
                        close_as = Some(false);
                    }
                    egui::CentralPanel::default().show_inside(vp_ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label("I/O class: Realtime requires root. Level 0=highest, 7=lowest.");
                        egui::ComboBox::from_label("I/O Class")
                            .selected_text(match *class {
                                0 => "None (default)",
                                1 => "Realtime (root)",
                                2 => "Best-effort",
                                3 => "Idle",
                                _ => "?",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(class, 0, "None (default)");
                                ui.selectable_value(class, 1, "Realtime (root)");
                                ui.selectable_value(class, 2, "Best-effort");
                                ui.selectable_value(class, 3, "Idle");
                            });
                        if *class == 1 || *class == 2 {
                            ui.add(egui::Slider::new(level, 0..=7).text("level"));
                        }
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("OK").clicked() {
                                close_as = Some(true);
                            }
                            if ui.button("Cancel").clicked() {
                                close_as = Some(false);
                            }
                        });
                    });
                },
            );
        }

        if let Some(accept) = close_as {
            self.open = false;
            if accept {
                self.result = Some(Some((self.class, self.level)));
                return Some(Some((self.class, self.level)));
            } else {
                self.result = Some(None);
                return Some(None);
            }
        }
        None
    }
}

// ── RuleEditDialog ────────────────────────────────────────────────────────────

pub struct RuleEditDialog {
    pub open: bool,
    pub rule: Rule,
    pub affinity_enabled: bool,
    pub nice_enabled: bool,
    pub ionice_enabled: bool,
    pub affinity_picker: AffinityPicker,
    pub result: Option<Option<Rule>>,
}

impl RuleEditDialog {
    pub fn new(template: Rule) -> Self {
        let affinity_enabled = template.affinity.is_some();
        let nice_enabled = template.nice.is_some();
        let ionice_enabled = template.ionice_class.is_some();
        let picker_init = template.affinity.as_deref().unwrap_or("");
        Self {
            open: true,
            affinity_picker: AffinityPicker::new(picker_init),
            rule: template,
            affinity_enabled,
            nice_enabled,
            ionice_enabled,
            result: None,
        }
    }

    /// `proc_names` feeds the live match count under the pattern field.
    pub fn show(
        &mut self,
        ctx: &Context,
        opacity: f32,
        proc_names: &[String],
    ) -> Option<Option<Rule>> {
        if !self.open {
            return self.result.clone();
        }

        let mut close_as: Option<bool> = None;

        {
            let title_str = if self.rule.name.is_empty() {
                "New rule".to_string()
            } else {
                format!("Edit Rule — {}", self.rule.name)
            };
            let rule = &mut self.rule;
            let affinity_enabled = &mut self.affinity_enabled;
            let nice_enabled = &mut self.nice_enabled;
            let ionice_enabled = &mut self.ionice_enabled;
            let picker = &mut self.affinity_picker;

            ctx.show_viewport_immediate(
                ViewportId::from_hash_of("rule_edit_dialog"),
                ViewportBuilder::default()
                    .with_title(title_str)
                    .with_app_id("argus-lasso")
                    .with_icon(egui::IconData {
                        rgba: crate::icon::RGBA.to_vec(),
                        width: crate::icon::W,
                        height: crate::icon::H,
                    })
                    // Wide enough for the affinity picker's quick-select row,
                    // which is sized from the CPU topology and overflowed a
                    // 560px dialog; tall enough that Nice and I/O priority are
                    // visible with the picker expanded rather than below the
                    // fold.
                    .with_inner_size([720.0, 620.0])
                    .with_min_inner_size([640.0, 420.0])
                    .with_transparent(true)
                    .with_resizable(true),
                |vp_ui, _class| {
                    let ctx = &vp_ui.ctx().clone();
                    crate::gui::theme::apply_viewport_opacity(vp_ui, opacity);
                    if ctx.input(|i| i.viewport().close_requested()) {
                        close_as = Some(false);
                    }
                    use crate::gui::theme::{self as th, tokens};
                    const LW: f32 = tokens::DIALOG_LABEL_W;

                    // Action bar first: a bottom panel keeps Cancel/Save
                    // pinned to the dialog's edge instead of floating after
                    // however tall the form happened to be.
                    egui::Panel::bottom("rule_actions").show_inside(vp_ui, |ui| {
                        let s = th::sem(ui);
                        ui.add_space(tokens::SPACE_XS);
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut rule.enabled, "Rule active");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let save = egui::Button::new(
                                        egui::RichText::new("Save rule").color(s.on_accent).font(
                                            crate::gui::theme::bold_font(
                                                crate::gui::theme::tokens::FONT_BODY,
                                            ),
                                        ),
                                    )
                                    .fill(s.accent);
                                    // A rule with no pattern matches nothing,
                                    // so saving one is never what was meant.
                                    if ui.add_enabled(!rule.pattern.is_empty(), save).clicked() {
                                        close_as = Some(true);
                                    }
                                    if ui.button("Cancel").clicked() {
                                        close_as = Some(false);
                                    }
                                },
                            );
                        });
                        ui.add_space(tokens::SPACE_XS);
                    });

                    egui::CentralPanel::default().show_inside(vp_ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        let s = th::sem(ui);
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            th::form_row_w(ui, LW, "Name", |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut rule.name)
                                        .hint_text("e.g. Steam games → V-Cache")
                                        .desired_width(ui.available_width()),
                                );
                            });

                            // Match type sits inline with the pattern: it
                            // qualifies that field and reads as a stray third
                            // question on its own row.
                            th::form_row_w(ui, LW, "Pattern", |ui| {
                                egui::ComboBox::from_id_salt("match_type")
                                    .selected_text(&rule.match_type)
                                    .width(90.0)
                                    .show_ui(ui, |ui| {
                                        for mt in ["contains", "exact", "regex"] {
                                            ui.selectable_value(
                                                &mut rule.match_type,
                                                mt.to_string(),
                                                mt,
                                            );
                                        }
                                    });
                                ui.add(
                                    egui::TextEdit::singleline(&mut rule.pattern)
                                        .font(egui::TextStyle::Monospace)
                                        .desired_width(ui.available_width()),
                                );
                            });

                            // Live match count, directly under the field it
                            // describes — the whole point of the pattern is
                            // what it currently catches.
                            th::form_row_w(ui, LW, "", |ui| {
                                let (msg, color) = match_summary(rule, proc_names, s.ok, ui);
                                ui.label(
                                    egui::RichText::new(msg)
                                        .size(tokens::FONT_HELP)
                                        .color(color),
                                );
                            });

                            th::group_heading(
                                ui,
                                "Actions",
                                "— turn on the ones this rule should set",
                            );

                            // The checkbox sits on the action's own name; an
                            // unchecked row goes weak, so what the rule does
                            // is readable without parsing a column of
                            // identical "Enable" boxes.
                            action_row(ui, LW, affinity_enabled, "CPU assignment", |ui, on| {
                                let cpulist = picker.cpulist();
                                let shown = if cpulist.is_empty() {
                                    "all CPUs".to_string()
                                } else {
                                    cpulist
                                };
                                ui.add_enabled(
                                    on,
                                    egui::Label::new(
                                        egui::RichText::new(shown)
                                            .family(egui::FontFamily::Monospace)
                                            .size(tokens::FONT_HELP),
                                    ),
                                );
                            });
                            if *affinity_enabled {
                                ui.indent("aff_picker", |ui| {
                                    ui.set_max_width(ui.available_width());
                                    picker.show(ui);
                                });
                                ui.add_space(tokens::SPACE_XS);
                            }

                            action_row(ui, LW, nice_enabled, "CPU priority (nice)", |ui, on| {
                                let nice = rule.nice.get_or_insert(0);
                                ui.add_enabled(on, egui::DragValue::new(nice).range(-20..=19));
                            });

                            action_row(ui, LW, ionice_enabled, "I/O priority", |ui, on| {
                                let class = rule.ionice_class.get_or_insert(2);
                                ui.add_enabled(on, egui::DragValue::new(class).range(0..=3));
                                ui.label(
                                    egui::RichText::new("class, level")
                                        .size(tokens::FONT_HELP)
                                        .color(ui.visuals().weak_text_color()),
                                );
                                let level = rule.ionice_level.get_or_insert(4);
                                ui.add_enabled(on, egui::DragValue::new(level).range(0..=7));
                            });
                        });
                    });
                },
            );
        }

        if let Some(accept) = close_as {
            self.open = false;
            if accept {
                if self.affinity_enabled {
                    let cpulist = self.affinity_picker.cpulist();
                    self.rule.affinity = if cpulist.is_empty() {
                        None
                    } else {
                        Some(cpulist)
                    };
                } else {
                    self.rule.affinity = None;
                }
                if !self.nice_enabled {
                    self.rule.nice = None;
                }
                if !self.ionice_enabled {
                    self.rule.ionice_class = None;
                    self.rule.ionice_level = None;
                }
                self.rule.refresh_pattern_caches();
                self.result = Some(Some(self.rule.clone()));
                return Some(Some(self.rule.clone()));
            } else {
                self.result = Some(None);
                return Some(None);
            }
        }
        None
    }
}

/// One "Actions" row: the checkbox carries the action's name, and the row's
/// controls go weak when it is off.
fn action_row(
    ui: &mut egui::Ui,
    label_w: f32,
    enabled: &mut bool,
    label: &str,
    add_contents: impl FnOnce(&mut egui::Ui, bool),
) {
    ui.horizontal(|ui| {
        let h = ui.spacing().interact_size.y;
        ui.allocate_ui_with_layout(
            egui::vec2(label_w, h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.checkbox(enabled, label);
            },
        );
        add_contents(ui, *enabled);
    });
    ui.add_space(crate::gui::theme::tokens::SPACE_XS);
}

/// Describe what the pattern currently catches, for the line under the field.
///
/// Counting against live process names is the only way to tell "this pattern
/// is wrong" from "nothing is running yet" before the rule is saved.
fn match_summary(
    rule: &Rule,
    proc_names: &[String],
    ok_color: egui::Color32,
    ui: &egui::Ui,
) -> (String, egui::Color32) {
    if rule.pattern.is_empty() {
        return (
            "Enter a pattern to see what it matches".into(),
            ui.visuals().weak_text_color(),
        );
    }
    // Match against a probe rule so the live count uses exactly the same
    // logic the engine will, including an invalid regex matching nothing.
    let mut probe = rule.clone();
    probe.enabled = true;
    probe.refresh_pattern_caches();
    let hits: Vec<&String> = proc_names
        .iter()
        .filter(|n| {
            let lower = n.to_lowercase();
            probe.matches(n, &lower)
        })
        .collect();
    if hits.is_empty() {
        return (
            "Matches no running process".into(),
            ui.visuals().weak_text_color(),
        );
    }
    let mut sample: Vec<&str> = hits.iter().map(|s| s.as_str()).take(3).collect();
    sample.dedup();
    let more = if hits.len() > sample.len() {
        " …"
    } else {
        ""
    };
    (
        format!(
            "✓ Matches {} running process{}: {}{more}",
            hits.len(),
            if hits.len() == 1 { "" } else { "es" },
            sample.join(", ")
        ),
        ok_color,
    )
}

// ── RulePresetsDialog ─────────────────────────────────────────────────────────

/// (name, pattern, match_type, affinity, nice, ionice_class, ionice_level)
pub type RulePreset = (
    &'static str,
    &'static str,
    &'static str,
    Option<String>,
    Option<i32>,
    Option<i32>,
    Option<i32>,
);

/// Generate rule presets dynamically based on detected CPU topology.
pub fn rule_presets() -> Vec<RulePreset> {
    let topo = detect_topology();
    let (pref, npref) = if topo.has_asymmetry() {
        (
            Some(cpuset_to_cpulist(&topo.preferred)),
            Some(cpuset_to_cpulist(&topo.non_preferred)),
        )
    } else {
        (None, None)
    };
    vec![
        (
            "Steam (preferred)",
            "steam",
            "exact",
            pref.clone(),
            None,
            None,
            None,
        ),
        (
            "steamwebhelper",
            "steamwebhelper",
            "exact",
            pref.clone(),
            Some(5),
            None,
            None,
        ),
        (
            "Wine / Proton",
            "wine",
            "contains",
            pref.clone(),
            None,
            None,
            None,
        ),
        (
            "Proton",
            "proton",
            "contains",
            pref.clone(),
            None,
            None,
            None,
        ),
        (
            "OBS Studio",
            "obs",
            "exact",
            pref.clone(),
            Some(-1),
            None,
            None,
        ),
        (
            "Discord",
            "discord",
            "contains",
            npref.clone(),
            Some(5),
            None,
            None,
        ),
        (
            "Firefox",
            "firefox",
            "contains",
            npref.clone(),
            None,
            None,
            None,
        ),
        (
            "Chromium / Chrome",
            "chrom",
            "contains",
            npref.clone(),
            None,
            None,
            None,
        ),
        ("KWin", "kwin", "contains", None, None, None, None),
        (
            "Plasma Shell",
            "plasmashell",
            "exact",
            npref.clone(),
            Some(5),
            None,
            None,
        ),
        (
            "Compiler (gcc/clang)",
            "gcc",
            "contains",
            None,
            None,
            Some(2),
            Some(4),
        ),
        (
            "Archive / compress",
            "7z",
            "contains",
            npref.clone(),
            Some(10),
            Some(3),
            None,
        ),
    ]
}

pub struct RulePresetsDialog {
    pub open: bool,
    pub selected: Option<usize>,
    pub result: Option<Option<Rule>>,
}

impl RulePresetsDialog {
    pub fn new() -> Self {
        Self {
            open: true,
            selected: None,
            result: None,
        }
    }

    pub fn show(&mut self, ctx: &Context, opacity: f32) -> Option<Option<Rule>> {
        if !self.open {
            return self.result.clone();
        }

        let mut close_as: Option<bool> = None;

        {
            let selected = &mut self.selected;

            ctx.show_viewport_immediate(
                ViewportId::from_hash_of("rule_presets_dialog"),
                ViewportBuilder::default()
                    .with_title("Rule templates")
                    .with_app_id("argus-lasso")
                    .with_icon(egui::IconData {
                        rgba: crate::icon::RGBA.to_vec(),
                        width: crate::icon::W,
                        height: crate::icon::H,
                    })
                    .with_min_inner_size([640.0, 360.0])
                    .with_transparent(true)
                    .with_resizable(true),
                |vp_ui, _class| {
                    let ctx = &vp_ui.ctx().clone();
                    crate::gui::theme::apply_viewport_opacity(vp_ui, opacity);
                    if ctx.input(|i| i.viewport().close_requested()) {
                        close_as = Some(false);
                    }
                    use crate::gui::theme::{self as th, tokens};

                    // Actions in a bottom panel, pinned to the dialog edge —
                    // same shape as the rule editor, so the two dialogs do not
                    // put the same controls in different places.
                    egui::Panel::bottom("preset_actions").show_inside(vp_ui, |ui| {
                        let s = th::sem(ui);
                        ui.add_space(tokens::SPACE_XS);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new("Double-click a template to use it")
                                    .size(tokens::FONT_HELP)
                                    .color(ui.visuals().weak_text_color()),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let use_btn = egui::Button::new(
                                        egui::RichText::new("Use template")
                                            .color(s.on_accent)
                                            .font(crate::gui::theme::bold_font(
                                                crate::gui::theme::tokens::FONT_BODY,
                                            )),
                                    )
                                    .fill(s.accent);
                                    if ui.add_enabled(selected.is_some(), use_btn).clicked() {
                                        close_as = Some(true);
                                    }
                                    if ui.button("Cancel").clicked() {
                                        close_as = Some(false);
                                    }
                                },
                            );
                        });
                        ui.add_space(tokens::SPACE_XS);
                    });

                    egui::CentralPanel::default().show_inside(vp_ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(
                            egui::RichText::new(
                                "Templates pre-fill a rule; you can edit it before saving.",
                            )
                            .size(tokens::FONT_HELP)
                            .color(ui.visuals().weak_text_color()),
                        );
                        ui.add_space(tokens::SPACE_XS);

                        // Fixed column widths, one shared set for the header
                        // and every row. A Grid per row — which is what this
                        // was — sizes each row's columns independently, so
                        // nothing lined up down the table.
                        const ROW_H: f32 = tokens::ROW_H;
                        const PAD: f32 = 6.0;
                        let total = ui.available_width();
                        let w_match = 80.0;
                        let w_aff = 110.0;
                        let w_name = ((total - w_match - w_aff) * 0.42).max(110.0);
                        let w_pat = (total - w_match - w_aff - w_name).max(90.0);
                        let cols = [w_name, w_pat, w_match, w_aff];

                        let draw_row = |ui: &mut egui::Ui,
                                        cells: [&str; 4],
                                        bg: Option<egui::Color32>,
                                        strong: bool|
                         -> egui::Response {
                            let (rect, resp) = ui.allocate_exact_size(
                                egui::vec2(total, ROW_H),
                                egui::Sense::click(),
                            );
                            if let Some(c) = bg {
                                ui.painter().rect_filled(rect, 0.0, c);
                            }
                            let mut x = rect.left();
                            for (i, text) in cells.iter().enumerate() {
                                let col = if strong {
                                    ui.visuals().weak_text_color()
                                } else {
                                    ui.visuals().text_color()
                                };
                                let font = if i == 1 {
                                    egui::FontId::monospace(tokens::FONT_HELP)
                                } else {
                                    egui::FontId::proportional(tokens::FONT_HELP)
                                };
                                ui.painter_at(egui::Rect::from_min_size(
                                    egui::pos2(x, rect.top()),
                                    egui::vec2(cols[i], ROW_H),
                                ))
                                .text(
                                    egui::pos2(x + PAD, rect.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    text,
                                    font,
                                    col,
                                );
                                x += cols[i];
                            }
                            resp
                        };

                        let hdr_bg = ui.visuals().widgets.noninteractive.bg_fill;
                        draw_row(
                            ui,
                            ["NAME", "PATTERN", "MATCH", "AFFINITY"],
                            Some(hdr_bg),
                            true,
                        );

                        let presets = rule_presets();
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for (i, (name, pat, match_type, ref aff, _nice, _ioc, _iol)) in
                                presets.iter().enumerate()
                            {
                                let is_sel = *selected == Some(i);
                                let bg = if is_sel {
                                    ui.visuals().selection.bg_fill
                                } else if i % 2 == 1 {
                                    ui.visuals().faint_bg_color
                                } else {
                                    ui.visuals().extreme_bg_color
                                };
                                let resp = draw_row(
                                    ui,
                                    [name, pat, match_type, aff.as_deref().unwrap_or("—")],
                                    Some(bg),
                                    false,
                                );
                                if resp.clicked() {
                                    *selected = Some(i);
                                }
                                if resp.double_clicked() {
                                    *selected = Some(i);
                                    close_as = Some(true);
                                }
                            }
                        });
                    });
                },
            );
        }

        if let Some(accept) = close_as {
            self.open = false;
            if accept {
                if let Some(idx) = self.selected {
                    let presets = rule_presets();
                    let (name, pat, match_type, ref aff, nice, ioc, iol) = presets[idx];
                    let mut rule = Rule::new_empty();
                    rule.name = name.to_string();
                    rule.pattern = pat.to_string();
                    rule.match_type = match_type.to_string();
                    rule.affinity = aff.clone();
                    rule.nice = nice;
                    rule.ionice_class = ioc;
                    rule.ionice_level = iol;
                    self.result = Some(Some(rule.clone()));
                    return Some(Some(rule));
                }
            }
            self.result = Some(None);
            return Some(None);
        }
        None
    }
}

// ── SteamGamePickerDialog ─────────────────────────────────────────────────────

pub struct SteamGamePickerDialog {
    pub open: bool,
    pub games: Vec<(String, String)>, // (appid, name)
    pub filter: String,
    pub selected: Option<usize>,
    pub result: Option<Option<(String, String)>>,
    pub loaded: bool,
}

impl SteamGamePickerDialog {
    pub fn new() -> Self {
        let mut dlg = Self {
            open: true,
            games: Vec::new(),
            filter: String::new(),
            selected: None,
            result: None,
            loaded: false,
        };
        dlg.games = scan_steam_library();
        dlg.loaded = true;
        dlg
    }

    pub fn show(&mut self, ctx: &Context, opacity: f32) -> Option<Option<(String, String)>> {
        if !self.open {
            return self.result.clone();
        }
        let mut accepted = false;
        let mut cancelled = false;
        {
            let filter = &mut self.filter;
            let games = &self.games;
            let selected = &mut self.selected;
            ctx.show_viewport_immediate(
                ViewportId::from_hash_of("steam_game_picker"),
                ViewportBuilder::default()
                    .with_title("Choose Steam game")
                    .with_app_id("argus-lasso")
                    .with_icon(egui::IconData {
                        rgba: crate::icon::RGBA.to_vec(),
                        width: crate::icon::W,
                        height: crate::icon::H,
                    })
                    .with_min_inner_size([560.0, 520.0])
                    .with_transparent(true)
                    .with_resizable(true),
                |vp_ui, _class| {
                    let ctx = &vp_ui.ctx().clone();
                    crate::gui::theme::apply_viewport_opacity(vp_ui, opacity);
                    if ctx.input(|i| i.viewport().close_requested()) {
                        cancelled = true;
                    }
                    egui::CentralPanel::default().show_inside(vp_ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label("Filter:");
                            ui.text_edit_singleline(filter);
                        });

                        let filter_lower = filter.to_lowercase();
                        let filtered: Vec<_> = games
                            .iter()
                            .enumerate()
                            .filter(|(_, (id, name))| {
                                filter_lower.is_empty()
                                    || name.to_lowercase().contains(&filter_lower)
                                    || id.contains(&filter_lower)
                            })
                            .collect();

                        egui_extras::TableBuilder::new(ui)
                            .striped(true)
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(egui_extras::Column::exact(80.0))
                            .column(egui_extras::Column::remainder())
                            .min_scrolled_height(0.0)
                            .max_scroll_height(400.0)
                            .body(|mut body| {
                                for (orig_i, (appid, name)) in &filtered {
                                    body.row(24.0, |mut row| {
                                        let sel = *selected == Some(*orig_i);
                                        row.set_selected(sel);

                                        row.col(|ui| {
                                            ui.label(appid.as_str());
                                        });
                                        row.col(|ui| {
                                            ui.label(name.as_str());
                                        });

                                        let resp = row.response();
                                        if resp.double_clicked() {
                                            *selected = Some(*orig_i);
                                            accepted = true;
                                        } else if resp.clicked() {
                                            *selected = Some(*orig_i);
                                        }
                                    });
                                }
                            });

                        ui.label(format!("{} games found", games.len()));
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("Select").clicked() && selected.is_some() {
                                accepted = true;
                            }
                            if ui.button("Cancel").clicked() {
                                cancelled = true;
                            }
                        });
                    });
                },
            );
        }
        if accepted {
            if let Some(idx) = self.selected {
                if let Some((appid, name)) = self.games.get(idx) {
                    let r = (appid.clone(), name.clone());
                    self.open = false;
                    self.result = Some(Some(r.clone()));
                    return Some(Some(r));
                }
            }
        }
        if cancelled {
            self.open = false;
            self.result = Some(None);
            return Some(None);
        }
        None
    }
}

fn scan_steam_library() -> Vec<(String, String)> {
    use std::path::PathBuf;
    let home = std::env::var("HOME").unwrap_or_default();
    let roots = vec![
        PathBuf::from(&home).join(".steam/steam"),
        PathBuf::from(&home).join(".local/share/Steam"),
    ];
    let mut seen = std::collections::HashSet::new();
    let mut lib_dirs: Vec<PathBuf> = Vec::new();
    for root in roots {
        if let Ok(resolved) = root.canonicalize() {
            if seen.insert(resolved.clone()) {
                lib_dirs.push(resolved.join("steamapps"));
            }
        }
    }
    // Parse libraryfolders.vdf for additional paths
    let extra: Vec<_> = lib_dirs.clone();
    for lib in extra {
        let vdf = lib.join("libraryfolders.vdf");
        if let Ok(text) = std::fs::read_to_string(&vdf) {
            for cap in text.lines() {
                if cap.contains("\"path\"") {
                    if let Some(p) = cap.split('"').nth(3) {
                        let apps = PathBuf::from(p).join("steamapps");
                        if let Ok(r) = apps.canonicalize() {
                            if seen.insert(r.clone()) {
                                lib_dirs.push(r);
                            }
                        }
                    }
                }
            }
        }
    }
    let mut games: std::collections::HashMap<String, String> = Default::default();
    for apps_dir in &lib_dirs {
        if let Ok(entries) = std::fs::read_dir(apps_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let fname = name.to_string_lossy();
                if fname.starts_with("appmanifest_") && fname.ends_with(".acf") {
                    if let Ok(text) = std::fs::read_to_string(entry.path()) {
                        let appid = extract_vdf_field(&text, "appid");
                        let gname = extract_vdf_field(&text, "name");
                        if let (Some(id), Some(n)) = (appid, gname) {
                            games.insert(id, n);
                        }
                    }
                }
            }
        }
    }
    let mut sorted: Vec<_> = games.into_iter().collect();
    sorted.sort_by_key(|a| a.1.to_lowercase());
    sorted
}

fn extract_vdf_field(text: &str, field: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(&format!("\"{field}\"")) {
            let parts: Vec<_> = trimmed.splitn(4, '"').collect();
            if parts.len() >= 4 {
                return Some(parts[3].to_string());
            }
        }
    }
    None
}

// ── LutrisGamePickerDialog ────────────────────────────────────────────────────

pub struct LutrisGamePickerDialog {
    pub open: bool,
    pub games: Vec<(String, String)>, // (slug, display_label)
    pub filter: String,
    pub selected: Option<usize>,
    pub result: Option<Option<(String, String)>>, // (slug, name)
    pub status: String,
}

impl LutrisGamePickerDialog {
    pub fn new() -> Self {
        let (games, status) = scan_lutris_library();
        Self {
            open: true,
            games,
            filter: String::new(),
            selected: None,
            result: None,
            status,
        }
    }

    pub fn show(&mut self, ctx: &Context, opacity: f32) -> Option<Option<(String, String)>> {
        if !self.open {
            return self.result.clone();
        }
        let mut accepted = false;
        let mut cancelled = false;
        {
            let filter = &mut self.filter;
            let games = &self.games;
            let selected = &mut self.selected;
            let status = &self.status;
            ctx.show_viewport_immediate(
                ViewportId::from_hash_of("lutris_game_picker"),
                ViewportBuilder::default()
                    .with_title("Choose Lutris game")
                    .with_app_id("argus-lasso")
                    .with_icon(egui::IconData {
                        rgba: crate::icon::RGBA.to_vec(),
                        width: crate::icon::W,
                        height: crate::icon::H,
                    })
                    .with_min_inner_size([560.0, 520.0])
                    .with_transparent(true)
                    .with_resizable(true),
                |vp_ui, _class| {
                    let ctx = &vp_ui.ctx().clone();
                    crate::gui::theme::apply_viewport_opacity(vp_ui, opacity);
                    if ctx.input(|i| i.viewport().close_requested()) {
                        cancelled = true;
                    }
                    egui::CentralPanel::default().show_inside(vp_ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.label("Filter:");
                            ui.text_edit_singleline(filter);
                        });

                        let fl = filter.to_lowercase();
                        let filtered: Vec<_> = games
                            .iter()
                            .enumerate()
                            .filter(|(_, (_, label))| {
                                fl.is_empty() || label.to_lowercase().contains(&fl)
                            })
                            .collect();

                        egui_extras::TableBuilder::new(ui)
                            .striped(true)
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(egui_extras::Column::remainder())
                            .min_scrolled_height(0.0)
                            .max_scroll_height(400.0)
                            .body(|mut body| {
                                for (orig_i, (_, label)) in &filtered {
                                    body.row(24.0, |mut row| {
                                        let sel = *selected == Some(*orig_i);
                                        row.set_selected(sel);

                                        row.col(|ui| {
                                            ui.label(label.as_str());
                                        });

                                        let resp = row.response();
                                        if resp.double_clicked() {
                                            *selected = Some(*orig_i);
                                            accepted = true;
                                        } else if resp.clicked() {
                                            *selected = Some(*orig_i);
                                        }
                                    });
                                }
                            });

                        ui.label(status.as_str());
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("Select").clicked() && selected.is_some() {
                                accepted = true;
                            }
                            if ui.button("Cancel").clicked() {
                                cancelled = true;
                            }
                        });
                    });
                },
            );
        }
        if accepted {
            if let Some(idx) = self.selected {
                if let Some((slug, label)) = self.games.get(idx) {
                    let name = label.split("  [").next().unwrap_or(label).to_string();
                    let r = (slug.clone(), name);
                    self.open = false;
                    self.result = Some(Some(r.clone()));
                    return Some(Some(r));
                }
            }
        }
        if cancelled {
            self.open = false;
            self.result = Some(None);
            return Some(None);
        }
        None
    }
}

fn scan_lutris_library() -> (Vec<(String, String)>, String) {
    let home = std::env::var("HOME").unwrap_or_default();
    let db = format!("{home}/.local/share/lutris/pga.db");
    if !std::path::Path::new(&db).exists() {
        return (vec![], "Lutris database not found.".into());
    }
    // We don't want to pull in rusqlite; parse the sqlite file via the sqlite3 CLI.
    let output = std::process::Command::new("sqlite3")
        .args([
            &db,
            "SELECT name,slug,runner FROM games WHERE installed=1 ORDER BY name COLLATE NOCASE",
        ])
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let games: Vec<(String, String)> = text
                .lines()
                .filter_map(|line| {
                    let parts: Vec<_> = line.splitn(3, '|').collect();
                    if parts.len() == 3 {
                        let name = parts[0].trim().to_string();
                        let slug = parts[1].trim().to_string();
                        let runner = parts[2].trim().to_string();
                        Some((slug, format!("{name}  [{runner}]")))
                    } else {
                        None
                    }
                })
                .collect();
            let count = games.len();
            (games, format!("{count} installed games found"))
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            (vec![], format!("sqlite3 error: {err}"))
        }
        Err(e) => (vec![], format!("sqlite3 not found: {e}")),
    }
}
