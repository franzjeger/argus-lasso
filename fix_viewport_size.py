import re

with open("src/gui/dialogs.rs", "r") as f:
    text = f.read()

def inject_size(match, size):
    full = match.group(0)
    if "min_inner_size" not in full:
        return full.replace(".with_transparent(true)", f".with_min_inner_size({size})\n                    .with_transparent(true)")
    return full

# For rule_edit_dialog
text = re.sub(r'ViewportId::from_hash_of\("rule_edit_dialog"\),\s*ViewportBuilder::default\(\).*?\.with_transparent\(true\)', lambda m: inject_size(m, "[650.0, 440.0]"), text, flags=re.DOTALL)

# For steam_game_picker
text = re.sub(r'ViewportId::from_hash_of\("steam_game_picker"\),\s*ViewportBuilder::default\(\).*?\.with_transparent\(true\)', lambda m: inject_size(m, "[650.0, 540.0]"), text, flags=re.DOTALL)

# For lutris_game_picker
text = re.sub(r'ViewportId::from_hash_of\("lutris_game_picker"\),\s*ViewportBuilder::default\(\).*?\.with_transparent\(true\)', lambda m: inject_size(m, "[650.0, 540.0]"), text, flags=re.DOTALL)

with open("src/gui/dialogs.rs", "w") as f:
    f.write(text)
