import re

with open("src/rules.rs", "r") as f:
    text = f.read()

# Replace r.matches("foo", "foo") with r.matches("foo", &"foo".to_lowercase())
text = re.sub(r'r\.matches\("([^"]+)", "([^"]+)"\)', r'r.matches("\1", &"\1".to_lowercase())', text)

with open("src/rules.rs", "w") as f:
    f.write(text)

