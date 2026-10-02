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
/// Headless frames for tests. egui insists that each frame's texture
/// updates are applied or dropped on purpose; with no renderer, they are
/// dropped.
#[cfg(test)]
pub(crate) trait TestFrame {
    fn test_frame(&self, input: egui::RawInput, ui: impl FnMut(&mut egui::Ui)) -> egui::FullOutput;
}

#[cfg(test)]
impl TestFrame for egui::Context {
    fn test_frame(&self, input: egui::RawInput, ui: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
        let mut output = self.run_ui(input, ui);
        output.textures_delta.clear();
        output
    }
}

pub type SharedContext = std::sync::Arc<std::sync::Mutex<Option<egui::Context>>>;

/// Show, un-minimize and focus the main window, from any thread: the tray's
/// "Open" and a second launch bring the running window to the front.
/// Returns false while eframe has not created the window yet.
/// Bring the main window up: show it, or, when it was closed to the tray,
/// ask for a new one. Returns false only when no window exists yet.
pub fn request_main_window(
    state: &std::sync::Arc<std::sync::Mutex<crate::monitor::AppState>>,
    context: &SharedContext,
) -> bool {
    if let Ok(mut s) = state.lock() {
        if s.window_closed {
            s.window_wanted = true;
            return true;
        }
    }
    show_main_window(context)
}

pub fn show_main_window(context: &SharedContext) -> bool {
    let Ok(context) = context.lock() else {
        return false;
    };
    let Some(ctx) = context.as_ref() else {
        return false;
    };
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    ctx.request_repaint();
    true
}

#[cfg(test)]
mod tests {
    /// With the window closed to the tray there is no window to show: the
    /// request is left for main, which opens a new one.
    #[test]
    fn a_request_while_closed_to_the_tray_asks_for_a_new_window() {
        let state = std::sync::Arc::new(std::sync::Mutex::new(crate::monitor::AppState {
            window_closed: true,
            ..Default::default()
        }));
        let none: super::SharedContext = Default::default();
        assert!(super::request_main_window(&state, &none));
        assert!(state.lock().unwrap().window_wanted);
    }

    /// A second launch before eframe has made the window used to be
    /// consumed with nothing shown; the caller keeps it pending instead.
    #[test]
    fn showing_before_the_window_exists_reports_it() {
        let none: super::SharedContext = Default::default();
        assert!(!super::show_main_window(&none));
        let some: super::SharedContext =
            std::sync::Arc::new(std::sync::Mutex::new(Some(egui::Context::default())));
        assert!(super::show_main_window(&some));
    }
}
