with open("argus-layer/Cargo.toml", "r") as f:
    text = f.read()

text = text.replace('egui-ash-renderer = { version = "0.13.0" }', 'egui-ash-renderer = { version = "0.13.0", features = ["gpu-allocator"] }')

with open("argus-layer/Cargo.toml", "w") as f:
    f.write(text)
