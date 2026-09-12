import re

with open("src/gui/theme.rs", "r") as f:
    text = f.read()

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

# Insert before pub fn card
text = text.replace("pub fn card(", card_untitled + "\npub fn card(")

with open("src/gui/theme.rs", "w") as f:
    f.write(text)

