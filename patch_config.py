import re

with open("src/config.rs", "r") as f:
    text = f.read()

text = text.replace("pub start_minimized: bool,", "pub start_minimized: bool,\n    #[serde(default)]\n    pub global_overlay: bool,")
text = text.replace("start_minimized: false,", "start_minimized: false,\n            global_overlay: false,")

with open("src/config.rs", "w") as f:
    f.write(text)

