with open("argus-layer/src/renderer.rs", "r") as f:
    text = f.read()

text = text.replace("::builder()", "::default()")
text = text.replace(".build()", "")
with open("argus-layer/src/renderer.rs", "w") as f:
    f.write(text)
