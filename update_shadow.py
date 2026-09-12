import re

with open("src/gui/theme.rs", "r") as f:
    text = f.read()

# Replace all occurrences of
# vis.window_shadow = egui::epaint::Shadow::NONE;
# vis.popup_shadow = egui::epaint::Shadow::NONE;
# with a call to a helper function or just the default shadow.
# Wait, egui has a default shadow! We can just remove these lines!
text = re.sub(r'\s*vis\.window_shadow = egui::epaint::Shadow::NONE;', '', text)
text = re.sub(r'\s*vis\.popup_shadow = egui::epaint::Shadow::NONE;', '', text)

with open("src/gui/theme.rs", "w") as f:
    f.write(text)

