import re

with open("src/hw_monitor.rs", "r") as f:
    text = f.read()

# Find the start of collect_hwmon_cpu
start_idx = text.find('fn collect_hwmon_cpu')
# Find the start of collect_hwmon_where
end_idx = text.find('fn collect_hwmon_where')

new_funcs = """fn collect_all_hwmon(topo: &crate::cpu_park::CpuTopology) -> Vec<GroupReading> {
    let core_id_map = build_core_id_to_cpu_map();

    let mut groups = collect_hwmon_where(|_| true, |path, hw_name| {
        // Dynamic categorization based on known driver prefixes or path content
        if hw_name == "k10temp" || hw_name == "zenpower" || hw_name == "coretemp" {
            let label = match hw_name {
                "k10temp" => "AMD CPU [k10temp]",
                "zenpower" => "AMD CPU [zenpower]",
                _ => "Intel CPU [coretemp]",
            };
            return ("CPU", label.to_string());
        }
        
        if hw_name == "spd5118" || hw_name == "ee1004" {
            return ("Memory", dimm_slot_name(path));
        }
        
        if hw_name == "nvme" {
            let model = read_trimmed(&path.join("device/model"))
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "NVMe".into());
            return ("Storage", model);
        }
        
        if hw_name.starts_with("amdgpu") || hw_name.starts_with("nouveau") {
            return ("GPU", format!("{} GPU", hw_name));
        }
        
        if hw_name.starts_with("r8")
            || hw_name.starts_with("atlantic")
            || hw_name.starts_with("igb")
            || hw_name.starts_with("ixgbe")
            || hw_name.starts_with("e1000")
        {
            let iface = nic_interface_name(path).unwrap_or_else(|| hw_name.to_string());
            return ("Network", format!("NIC [{iface}]"));
        }

        // Generic fallback for unknown hardware (motherboard sensors like nct6775, etc.)
        ("System", format!("Sensor [{}]", hw_name))
    });

    // Remap CPU core labels like the original did
    for (cat, _name, sensors) in &mut groups {
        if *cat == "CPU" {
            for (label, _unit, _value) in sensors.iter_mut() {
                if let Some(core_id) = label.strip_prefix("Core ").and_then(|s| s.parse::<u32>().ok()) {
                    if let Some(&cpu_num) = core_id_map.get(&core_id) {
                        let kind = core_kind_suffix(topo, cpu_num);
                        *label = intern(format!("CPU {cpu_num}{kind}"));
                    }
                }
            }
        }
    }

    groups
}

"""

text = text[:start_idx] + new_funcs + text[end_idx:]

with open("src/hw_monitor.rs", "w") as f:
    f.write(text)

