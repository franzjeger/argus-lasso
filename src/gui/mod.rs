pub mod action_handler;
pub mod bench_tab;
pub mod cpu_bars;
pub mod detail_window;
pub mod dialog_manager;
pub mod dialogs;
pub mod gaming_mode_tab;
pub mod hw_monitor_tab;
pub mod log_tab;
pub mod overview_tab;
pub mod probalance_tab;
pub mod process_tab;
pub mod rules_tab;
pub mod settings_tab;
pub mod theme;

pub mod overlay_install;
pub mod overlay_settings;

/// The GUI context, once eframe has created it. Shared with the threads that
/// can ask for the window (tray, monitor).
pub type SharedContext = std::sync::Arc<std::sync::Mutex<Option<egui::Context>>>;

/// Show, un-minimize and focus the main window, from any thread: the tray's
/// "Open" and a second launch bring the running window to the front.
pub fn show_main_window(context: &SharedContext) {
    if let Ok(context) = context.lock() {
        if let Some(ctx) = context.as_ref() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
        }
    }
}
