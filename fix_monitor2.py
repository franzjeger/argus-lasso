import re

with open("src/monitor.rs", "r") as f:
    text = f.read()

with open("missing.rs", "r") as f:
    missing = f.read()

text = text.replace("fn read_sys_cpu_total", missing + "fn read_sys_cpu_total")

with open("src/monitor.rs", "w") as f:
    f.write(text)

