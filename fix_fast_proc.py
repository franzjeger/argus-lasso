import re

with open("src/fast_proc.rs", "r") as f:
    text = f.read()

text = re.sub(r'lazy_static::lazy_static! \{.*?\n\}', '''static PAGE_SIZE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

fn get_page_size() -> u64 {
    *PAGE_SIZE.get_or_init(|| {
        unsafe { nix::libc::sysconf(nix::libc::_SC_PAGESIZE) as u64 }
    })
}''', text, flags=re.DOTALL)

text = text.replace('(*PAGE_SIZE)', 'get_page_size()')

with open("src/fast_proc.rs", "w") as f:
    f.write(text)

