import re

with open("src/app.rs", "r") as f:
    text = f.read()

# Remove notify_error
text = re.sub(r'    fn notify_error\(&self, msg: &str\) \{.*?\n        \}\n    \}\n', '', text, flags=re.DOTALL)

# Remove deliver_kill
text = re.sub(r'    /// Send the actual kill signal.*?fn deliver_kill.*?Ok\(.\)\s*\}\n', '', text, flags=re.DOTALL)

with open("src/app.rs", "w") as f:
    f.write(text)

