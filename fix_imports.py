with open("argus-layer/src/lib.rs", "r") as f:
    lines = f.readlines()

new_lines = []
for line in lines:
    if line.startswith("use std::ffi::{c_void, CStr};") and new_lines.count("use std::ffi::{c_void, CStr};\n") > 0:
        continue
    if line.startswith("use std::os::raw::c_char;") and new_lines.count("use std::os::raw::c_char;\n") > 0:
        continue
    if line.startswith("use std::sync::atomic::{AtomicUsize, Ordering};") and new_lines.count("use std::sync::atomic::{AtomicUsize, Ordering};\n") > 0:
        continue
    if line.startswith("use std::time::Instant;") and new_lines.count("use std::time::Instant;\n") > 0:
        continue
    if line.startswith("use std::sync::RwLock;") and new_lines.count("use std::sync::RwLock;\n") > 0:
        continue
    new_lines.append(line)

with open("argus-layer/src/lib.rs", "w") as f:
    f.writelines(new_lines)
