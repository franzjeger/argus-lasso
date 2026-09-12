import re

with open("src/gui/settings_tab.rs", "r") as f:
    text = f.read()

toggle = """
                    crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Desktop notifications", |ui| {
                        ui.checkbox(&mut self.config.ui.notifications_enabled, "Enabled");
                    });

                    crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Global Vulkan Overlay", |ui| {
                        if ui.checkbox(&mut self.config.ui.global_overlay, "Inject overlay into all Vulkan games automatically").changed() {
                            let config_dir = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share/vulkan/implicit_layer.d");
                            let layer_file = config_dir.join("ArgusOverlay.json");
                            if self.config.ui.global_overlay {
                                let _ = std::fs::create_dir_all(&config_dir);
                                let manifest = format!(
r#"{{
    "file_format_version" : "1.0.0",
    "layer" : {{
        "name": "VK_LAYER_ARGUS_OVERLAY",
        "type": "GLOBAL",
        "library_path": "{}/.local/share/vulkan/explicit_layer.d/libargus_layer.so",
        "api_version": "1.3.200",
        "implementation_version": "1",
        "description": "Argus-Lasso Native FPS Overlay Layer",
        "disable_environment": {{
            "DISABLE_ARGUS_OVERLAY": "1"
        }}
    }}
}}"#, std::env::var("HOME").unwrap_or_default());
                                let _ = std::fs::write(&layer_file, manifest);
                            } else {
                                let _ = std::fs::remove_file(&layer_file);
                            }
                        }
                    });
"""
text = text.replace("""
                    crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, "Desktop notifications", |ui| {
                        ui.checkbox(&mut self.config.ui.notifications_enabled, "Enabled");
                    });""", toggle)

dirty_search = "|| a.ui.notifications_enabled != b.ui.notifications_enabled"
dirty_replace = "|| a.ui.notifications_enabled != b.ui.notifications_enabled\n            || a.ui.global_overlay != b.ui.global_overlay"
text = text.replace(dirty_search, dirty_replace)

with open("src/gui/settings_tab.rs", "w") as f:
    f.write(text)
