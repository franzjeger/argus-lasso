import re

with open("src/gui/probalance_tab.rs", "r") as f:
    text = f.read()

text = text.replace('card_untitled(ui,', 'crate::gui::theme::card_untitled(ui,')

# Remove the fn card_untitled definition
fn_start = text.find('fn card_untitled(ui: &mut Ui')
if fn_start != -1:
    text = text[:fn_start]

with open("src/gui/probalance_tab.rs", "w") as f:
    f.write(text)

