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
}

impl DetailWindow {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_pid(&mut self, pid: u32) {
        self.detail_pid = Some(pid);
        self.detail_info = None; // force immediate refresh
    }

    pub fn show(
        &mut self,
        ctx: &Context,
        snapshot: &[ProcInfo],
        proc_cpu_history: &HashMap<u32, VecDeque<f32>>,
        cpu_gen: u64,
    ) {
        let Some(pid) = self.detail_pid else { return };

        // Refresh procfs details only when the daemon emitted a new sample.
        if self.detail_info.is_none() || cpu_gen != self.detail_last_gen {
            self.detail_last_gen = cpu_gen;
            self.detail_info = utils::read_proc_details(pid);
            if self.detail_info.is_none() {
                // Process is gone — close the window.
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
        egui::Window::new(format!("Details — {title}"))
            .id(egui::Id::new("proc_detail_window"))
            .default_width(440.0)
            .open(&mut open)
            .show(ctx, |ui| {
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
                                &format!("{:.1} MB", p.mem_rss as f64 / 1_048_576.0),
                            );
                            row("Nice", &p.nice.to_string());
                            row("Affinity", &p.affinity);
                            if p.disk_read_bps > 0 || p.disk_write_bps > 0 {
                                row(
                                    "Disk I/O",
                                    &format!(
                                        "read {:.1} KB/s, write {:.1} KB/s",
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
                            row("Working dir", &details.cwd);
                        }
                    });

                if let Some(p) = proc {
                    if !p.cmdline.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new("Command line").weak());
                        ui.add(
                            egui::Label::new(egui::RichText::new(p.cmdline.as_str()).monospace())
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
                                let x =
                                    rect.left() + i as f32 / (hist.len() - 1) as f32 * rect.width();
                                let y = rect.bottom() - (v / hi) * (rect.height() - 4.0) - 2.0;
                                egui::pos2(x, y)
                            })
                            .collect();
                        for pair in pts.windows(2) {
                            painter.line_segment(
                                [pair[0], pair[1]],
                                egui::Stroke::new(1.5_f32, crate::gui::theme::Breeze::HIGHLIGHT),
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
                                                .size(11.0),
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
        if !open {
            self.detail_pid = None;
            self.detail_info = None;
        }
    }
}
