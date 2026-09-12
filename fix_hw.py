import re

with open("src/hw_monitor.rs", "r") as f:
    text = f.read()

# Replace collect_all body
old_collect_all = """fn collect_all(
    prev_disk: &HashMap<String, [u64; 2]>,
    new_disk: &HashMap<String, [u64; 2]>,
    prev_net: &HashMap<String, [u64; 2]>,
    new_net: &HashMap<String, [u64; 2]>,
    dt: f32,
) -> Vec<GroupReading> {
    let mut out: Vec<GroupReading> = Vec::new();

    // CPU: hwmon temps + frequencies + load + RAPL package power
    // Detect topology once per tick — both collectors need it, and each
    // detection is a full per-CPU sysfs scan on uniform machines.
    let topo = crate::cpu_park::detect_topology();
    out.extend(collect_hwmon_cpu(&topo));
    if let Some(g) = collect_cpu_freqs(&topo) {
        out.push(g);
    }
    if let Some(g) = collect_load_avg() {
        out.push(g);
    }
    out.extend(collect_rapl_power());

    // GPU: prefer NVML (NVIDIA); fall back to amdgpu hwmon
    let gpu = collect_nvidia_nvml();
    if !gpu.is_empty() {
        out.extend(gpu);
    } else {
        out.extend(collect_hwmon_category("GPU"));
    }

    // Memory: SPD temps + /proc/meminfo
    out.extend(collect_hwmon_memory());
    if let Some(g) = collect_meminfo() {
        out.push(g);
    }

    // Storage: NVMe hwmon + disk I/O
    out.extend(collect_hwmon_storage());
    out.extend(collect_disk_io(prev_disk, new_disk, dt));

    // Network: NIC hwmon + interface I/O
    out.extend(collect_hwmon_network());
    out.extend(collect_net_io(prev_net, new_net, dt));

    out
}"""

new_collect_all = """fn collect_all(
    prev_disk: &HashMap<String, [u64; 2]>,
    new_disk: &HashMap<String, [u64; 2]>,
    prev_net: &HashMap<String, [u64; 2]>,
    new_net: &HashMap<String, [u64; 2]>,
    dt: f32,
) -> Vec<GroupReading> {
    let mut out: Vec<GroupReading> = Vec::new();

    let topo = crate::cpu_park::detect_topology();
    
    // Dynamic hwmon discovery covers CPU, GPU (amdgpu), Memory (spd5118), Storage (nvme), Network (r8/igb/etc), System
    out.extend(collect_all_hwmon(&topo));

    if let Some(g) = collect_cpu_freqs(&topo) {
        out.push(g);
    }
    if let Some(g) = collect_load_avg() {
        out.push(g);
    }
    out.extend(collect_rapl_power());

    out.extend(collect_nvidia_nvml());

    if let Some(g) = collect_meminfo() {
        out.push(g);
    }

    out.extend(collect_disk_io(prev_disk, new_disk, dt));
    out.extend(collect_net_io(prev_net, new_net, dt));

    out
}"""

text = text.replace(old_collect_all, new_collect_all)

with open("src/hw_monitor.rs", "w") as f:
    f.write(text)

