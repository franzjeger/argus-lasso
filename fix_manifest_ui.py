import re

with open("src/gui/settings_tab.rs", "r") as f:
    text = f.read()

text = text.replace('"disable_environment": {\n            "DISABLE_ARGUS_OVERLAY": "1"\n        }', '"enable_environment": {\n            "ARGUS_LASSO_HUD": "1"\n        }')

with open("src/gui/settings_tab.rs", "w") as f:
    f.write(text)
