use eframe::egui;
use egui::{Context, RichText};
use std::collections::{HashMap, VecDeque};

use crate::monitor::ProcInfo;
use crate::utils;

#[derive(Default)]
pub struct DetailWindow {
    pub detail_pid: Option<u32>,
    pub detail_info: Option<utils::ProcDetails>,
    pub detail_last_gen: u64,
    /// The start time of the process the window was opened for, so a later
    /// process given the same PID is not shown as if it were that one.
    detail_start: Option<u64>,
}

fn start_ticks(pid: u32) -> Option<u64> {
    crate::fast_proc::read_stat(pid, &mut [0; 1024]).map(|stat| stat.starttime)
}

impl DetailWindow {
    pub fn set_pid(&mut self, pid: u32) {
        self.detail_pid = Some(pid);
        self.detail_start = start_ticks(pid);
        self.detail_info = None; // force immediate refresh
    }

    /// Whether the PID still belongs to the process the window was opened for.
    fn same_process(&self, pid: u32) -> bool {
        start_ticks(pid).is_some_and(|start| Some(start) == self.detail_start)
    }

    pub fn show(
        &mut self,
        ctx: &Context,
        snapshot: &[ProcInfo],
        proc_cpu_history: &HashMap<u32, VecDeque<f32>>,
        cpu_gen: u64,
        opacity: f32,
    ) {
        let Some(pid) = self.detail_pid else { return };

        // Refresh procfs details only when the daemon emitted a new sample.
        if self.detail_info.is_none() || cpu_gen != self.detail_last_gen {
            self.detail_last_gen = cpu_gen;
            self.detail_info = utils::read_proc_details(pid).filter(|_| self.same_process(pid));
            if self.detail_info.is_none() {
                // The process is gone (or its PID now names another): close.
                self.detail_pid = None;
                return;
            }
        }
        let Some(details) = self.detail_info.clone() else {
            return;
        };
        let proc = snapshot.iter().find(|p| p.pid == pid);
        let title = match proc {
            Some(p) => format!("{} ({})", p.name, pid),
            None => format!("PID {pid}"),
        };

        let mut open = true;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("proc_detail_window"),
            egui::ViewportBuilder::default()
                .with_title(format!("Details — {title}"))
                .with_app_id("argus-lasso")
                .with_transparent(true)
                .with_inner_size([540.0, 600.0]),
            |root_ui, _| {
                super::theme::apply_viewport_opacity(root_ui, opacity);
                if root_ui.input(|i| i.viewport().close_requested()) {
                    open = false;
                }
                egui::CentralPanel::default().show(root_ui, |ui| {
                    egui::Grid::new("detail_grid")
                        .num_columns(2)
                        .spacing([12.0, 3.0])
                        .show(ui, |ui| {
                            let mut row = |k: &str, v: &str| {
                                ui.label(RichText::new(k).weak());
                                ui.label(v);
                                ui.end_row();
                            };
                            row("State", &details.state);
                            if let Some(p) = proc {
                                row("CPU", &format!("{:.1} %", p.cpu_percent));
                                if p.gpu_percent > 0.0 {
                                    row("GPU", &format!("{:.0} %", p.gpu_percent));
                                }
                                row(
                                    "Memory (RSS)",
                                    &format!("{:.1} MiB", p.mem_rss as f64 / 1_048_576.0),
                                );
                                row("Nice", &p.nice.to_string());
                                row("Affinity", &p.affinity);
                                if p.disk_read_bps > 0 || p.disk_write_bps > 0 {
                                    row(
                                        "Disk I/O",
                                        &format!(
                                            "read {:.1} KiB/s, write {:.1} KiB/s",
                                            p.disk_read_bps as f64 / 1024.0,
                                            p.disk_write_bps as f64 / 1024.0
                                        ),
                                    );
                                }
                            }
                            row("Threads", &details.thread_count.to_string());
                            if let Some(fds) = details.fd_count {
                                row("Open FDs", &fds.to_string());
                            }
                            if !details.exe.is_empty() {
                                row("Executable", &details.exe);
                            }
                            if !details.cwd.is_empty() {
                                row("Working directory", &details.cwd);
                            }
                        });

                    if let Some(p) = proc {
                        if !p.cmdline.is_empty() {
                            ui.add_space(4.0);
                            ui.label(RichText::new("Command line").weak());
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(p.cmdline.as_str()).monospace(),
                                )
                                .wrap(),
                            );
                        }
                    }

                    // CPU sparkline from the shared per-PID history
                    if let Some(hist) = proc_cpu_history.get(&pid) {
                        if hist.len() >= 2 {
                            ui.add_space(6.0);
                            ui.label(RichText::new("CPU history").weak());
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), 40.0),
                                egui::Sense::hover(),
                            );
                            let painter = ui.painter();
                            painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
                            let hi = hist.iter().cloned().fold(1.0f32, f32::max);
                            let pts: Vec<egui::Pos2> = hist
                                .iter()
                                .enumerate()
                                .map(|(i, &v)| {
                                    let x = rect.left()
                                        + i as f32 / (hist.len() - 1) as f32 * rect.width();
                                    let y = rect.bottom() - (v / hi) * (rect.height() - 4.0) - 2.0;
                                    egui::pos2(x, y)
                                })
                                .collect();
                            for pair in pts.windows(2) {
                                painter.line_segment(
                                    [pair[0], pair[1]],
                                    egui::Stroke::new(
                                        1.5_f32,
                                        crate::gui::theme::Breeze::HIGHLIGHT,
                                    ),
                                );
                            }
                        }
                    }

                    if !details.threads.is_empty() {
                        ui.add_space(6.0);
                        egui::CollapsingHeader::new(format!("Threads ({})", details.thread_count))
                            .default_open(false)
                            .show(ui, |ui| {
                                egui::ScrollArea::vertical()
                                    .max_height(160.0)
                                    .show(ui, |ui| {
                                        for (tid, name) in &details.threads {
                                            ui.label(
                                                egui::RichText::new(format!("{tid:>8}  {name}"))
                                                    .monospace()
                                                    .size(crate::gui::theme::tokens::FONT_SMALL),
                                            );
                                        }
                                        if details.thread_count > details.threads.len() {
                                            ui.label(
                                                RichText::new(format!(
                                                    "… and {} more",
                                                    details.thread_count - details.threads.len()
                                                ))
                                                .weak(),
                                            );
                                        }
                                    });
                            });
                    }
                });
            },
        );
        if !open {
            self.detail_pid = None;
            self.detail_info = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DetailWindow;

    /// The window tracked only the PID, so a process that reused it was
    /// shown as the one the window had been opened for.
    #[test]
    fn a_reused_pid_is_not_the_same_process() {
        let mut window = DetailWindow::default();
        let pid = std::process::id();
        window.set_pid(pid);
        assert!(window.same_process(pid));
        window.detail_start = window.detail_start.map(|start| start + 1);
        assert!(!window.same_process(pid));
    }
}
