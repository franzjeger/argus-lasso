import re

with open("src/app.rs", "r") as f:
    text = f.read()

# Find `fn new` in ArgusLassoApp
# We'll insert `cc.egui_ctx.set_embed_viewports(true);` right after `crate::gui::theme::apply_theme`

insert = "\n        // Force menus and tooltips to be drawn embedded on the main canvas.\n        // Otherwise eframe spawns them as separate Wayland surfaces, which bypasses\n        // our wp_alpha_modifier_v1 opacity and makes them render 100% opaque.\n        cc.egui_ctx.set_embed_viewports(true);\n"
text = text.replace("crate::gui::theme::apply_theme(&cc.egui_ctx, native_ppp, &startup_theme);", "crate::gui::theme::apply_theme(&cc.egui_ctx, native_ppp, &startup_theme);" + insert)

with open("src/app.rs", "w") as f:
    f.write(text)

