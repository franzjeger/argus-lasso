import re

with open("src/hw_monitor.rs", "r") as f:
    text = f.read()

# Replace all collect_hwmon_* calls in collect_hwmon with one collect_all_hwmon
new_collect = """pub fn collect_hwmon(
    prev_disk: &HashMap<String, [u64; 2]>,
    new_disk: &HashMap<String, [u64; 2]>,
    prev_net: &HashMap<String, [u64; 2]>,
    new_net: &HashMap<String, [u64; 2]>,
    dt: f32,
    topo: &crate::cpu_park::CpuTopology,
) -> Vec<GroupReading> {
    let mut out = Vec::new();

    out.extend(collect_all_hwmon(topo));
    if let Some(freqs) = collect_cpu_freqs(topo) {
        out.push(freqs);
    }
    out.extend(collect_rapl_power());
    out.extend(collect_nvidia_nvml());

    if let Some(mem) = collect_meminfo() {
        out.push(mem);
    }

    out.extend(collect_disk_io(prev_disk, new_disk, dt));
    out.extend(collect_net_io(prev_net, new_net, dt));

    out
}
"""

text = re.sub(r'pub fn collect_hwmon\(.*?\) -> Vec<GroupReading> \{.*?\n\}\n', new_collect, text, flags=re.DOTALL)

with open("src/hw_monitor.rs", "w") as f:
    f.write(text)

