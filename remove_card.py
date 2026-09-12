import re

for filename in ["src/gui/probalance_tab.rs", "src/gui/gaming_mode_tab.rs"]:
    with open(filename, "r") as f:
        text = f.read()
    
    fn_start = text.find("fn card_untitled(ui: &mut Ui")
    if fn_start != -1:
        doc_start = text.rfind("/// A bordered container", 0, fn_start)
        if doc_start != -1:
            fn_start = doc_start
        text = text[:fn_start]
        
        with open(filename, "w") as f:
            f.write(text)

