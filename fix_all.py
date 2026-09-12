import re

# 1. Add card_untitled to theme.rs
with open("src/gui/theme.rs", "r") as f:
    theme = f.read()

card_untitled = """
/// A bordered container with no heading (used for hero cards where the
/// title is replaced by a large primary value/status).
pub fn card_untitled(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    let border_color = ui.visuals().widgets.noninteractive.bg_stroke.color;
    egui::Frame::new()
        .fill(card_fill(ui))
        .stroke(egui::Stroke::new(1.0_f32, border_color))
        .inner_margin(egui::Margin::same(8))
        .corner_radius(egui::CornerRadius::same(4))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add_contents(ui);
        });
}
"""
if "pub fn card_untitled" not in theme:
    theme = theme.replace("pub fn card(", card_untitled + "\npub fn card(")
with open("src/gui/theme.rs", "w") as f:
    f.write(theme)

# 2. Fix settings_tab.rs
with open("src/gui/settings_tab.rs", "r") as f:
    st = f.read()
st = st.replace('form_row(ui, ', 'crate::gui::theme::form_row_w(ui, crate::gui::theme::tokens::FORM_LABEL_W, ')

# Regex to remove fn form_row and its doc block
st = re.sub(r'/// Two-column settings row: fixed-width, left-aligned label \+ control column \(§7\)\.\nfn form_row.*?\}\n\}\n', '', st, flags=re.DOTALL)

with open("src/gui/settings_tab.rs", "w") as f:
    f.write(st)

# 3. Fix probalance_tab.rs
with open("src/gui/probalance_tab.rs", "r") as f:
    pb = f.read()

pb = pb.replace('card_untitled(ui,', 'crate::gui::theme::card_untitled(ui,')
pb = re.sub(r'/// A bordered container with no heading — mockup 2f uses these for the status\n/// card \(its hero line is the title\) and for the throttle table\.\nfn card_untitled.*?\}\n\}\n', '', pb, flags=re.DOTALL)

with open("src/gui/probalance_tab.rs", "w") as f:
    f.write(pb)

# 4. Fix gaming_mode_tab.rs
with open("src/gui/gaming_mode_tab.rs", "r") as f:
    gm = f.read()

gm = gm.replace('card_untitled(ui,', 'crate::gui::theme::card_untitled(ui,')
gm = re.sub(r'/// A bordered container with no heading — mockup 2b\'s status card leads with\n/// its hero line, so a card title above it would just say the same thing\.\nfn card_untitled.*?\}\n\}\n', '', gm, flags=re.DOTALL)

with open("src/gui/gaming_mode_tab.rs", "w") as f:
    f.write(gm)

