import re

with open("src/gui/theme.rs", "r") as f:
    text = f.read()

text = text.replace("vis.menu_rounding = CornerRadius::same(4);", "vis.menu_corner_radius = CornerRadius::same(4);")
text = text.replace("vis.menu_rounding = CornerRadius::same(6);", "vis.menu_corner_radius = CornerRadius::same(6);")

with open("src/gui/theme.rs", "w") as f:
    f.write(text)

