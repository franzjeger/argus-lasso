import re

with open("src/gui/gaming_mode_tab.rs", "r") as f:
    text = f.read()

text = text.replace('card_untitled(ui,', 'crate::gui::theme::card_untitled(ui,')

# Remove the fn card_untitled definition
fn_start = text.find('fn card_untitled(ui: &mut Ui')
if fn_start != -1:
    doc_start = text.rfind('/// A bordered container', 0, fn_start)
    if doc_start != -1:
        fn_start = doc_start
    
    # It's at the end of the file too, so we can just slice to fn_start
    text = text[:fn_start]

with open("src/gui/gaming_mode_tab.rs", "w") as f:
    f.write(text)

