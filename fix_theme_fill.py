import re

with open("src/gui/theme.rs", "r") as f:
    text = f.read()

# Replace vis.window_fill = window_bg; with vis.window_fill = base;
text = re.sub(r'vis\.window_fill\s*=\s*window_bg;', 'vis.window_fill = base;', text)

with open("src/gui/theme.rs", "w") as f:
    f.write(text)

