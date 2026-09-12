import re

with open("src/hw_monitor.rs", "r") as f:
    text = f.read()

# Replace collect_all and the specific collectors
start_idx = text.find('fn collect_all(')
end_idx = text.find('fn collect_hwmon_category(')

if start_idx != -1 and end_idx != -1:
    end_idx = text.find('\n', end_idx + len('fn collect_hwmon_category('))
    
    # Actually, let's just find where `collect_hwmon_where` starts
    end_idx2 = text.find('fn collect_hwmon_where')
    
    new_code = """fn collect_all(
    prev_disk: &HashMap<String, [u64; 2]>,
    new_disk: &HashMap<String, [u64; 2]>,
    prev_net: &HashMap<String, [u64; 2]>,
    new_net: &HashMap<String, [u64; 2]>,
    dt: f32,
) -> Vec<GroupReading> {
    let mut out: Vec<GroupReading> = Vec::new();

    let topo = crate::cpu_park::detect_topology();
    
    // Dynamic hwmon discovery
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
}

fn collect_all_hwmon(topo: &crate::cpu_park::CpuTopology) -> Vec<GroupReading> {
    let core_id_map = build_core_id_to_cpu_map();

    let mut groups = collect_hwmon_where(|_| true, |path, hw_name| {
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

        ("System", format!("Sensor [{}]", hw_name))
    });

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
    text = text[:start_idx] + new_code + text[end_idx2:]

with open("src/hw_monitor.rs", "w") as f:
    f.write(text)

