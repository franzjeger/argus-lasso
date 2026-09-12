//! Hardware sensor collection: reads hwmon sysfs, procfs, DRM sysfs, and nvidia-smi.
//!
//! `HwCollector` owns the persistent state (min/max/history). Call `update()` each
//! display-refresh tick; it merges new readings into `self.data` in-place so history
//! is preserved across samples.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::time::Instant;

pub const HISTORY_LEN: usize = 120;

// ── Sensor ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Sensor {
    pub label: &'static str,
    pub value: f32,
    pub unit: &'static str,
    pub min: f32,
    pub max: f32,
    avg_sum: f64,
    avg_count: u64,
    pub history: VecDeque<f32>,
}

impl Default for Sensor {
    fn default() -> Self {
        Self {
            label: "",
            value: 0.0,
            unit: "",
            min: f32::MAX,
            max: f32::MIN,
            avg_sum: 0.0,
            avg_count: 0,
            history: VecDeque::with_capacity(HISTORY_LEN + 1),
        }
    }
}

impl Sensor {
    pub fn new(label: &'static str, unit: &'static str) -> Self {
        Self {
            label,
            unit,
            ..Default::default()
        }
    }

    pub fn push(&mut self, value: f32) {
        self.value = value;
        if !value.is_finite() {
            return;
        }
        if self.avg_count == 0 {
            self.min = value;
            self.max = value;
        } else {
            if value < self.min {
                self.min = value;
            }
            if value > self.max {
                self.max = value;
            }
        }
        self.avg_sum += value as f64;
        self.avg_count += 1;
        self.history.push_back(value);
        while self.history.len() > HISTORY_LEN {
            self.history.pop_front();
        }
    }

    pub fn avg(&self) -> f32 {
        if self.avg_count == 0 {
            0.0
        } else {
            (self.avg_sum / self.avg_count as f64) as f32
        }
    }

    pub fn min_display(&self) -> f32 {
        if self.avg_count == 0 {
            0.0
        } else {
            self.min
        }
    }

    pub fn max_display(&self) -> f32 {
        if self.avg_count == 0 {
            0.0
        } else {
            self.max
        }
    }
}

// ── SensorGroup ───────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct SensorGroup {
    /// High-level section: "CPU", "GPU", "Memory", "Storage", "Network", "System"
    pub category: &'static str,
    /// Human-readable device / component name shown as sub-header
    pub name: String,
    pub sensors: Vec<Sensor>,
}

// ── HwMonitorData ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct HwMonitorData {
    pub groups: Vec<SensorGroup>,
}

impl HwMonitorData {
    pub fn get_gpu_usage(&self) -> Option<u8> {
        for group in &self.groups {
            if group.category == "GPU" {
                for sensor in group.sensors.iter().filter(|s| s.value.is_finite()) {
                    if sensor.label == "Usage" || sensor.label == "GPU Load" {
                        return Some(sensor.value as u8);
                    }
                }
            }
        }
        None
    }

    pub fn get_gpu_temp(&self) -> Option<u8> {
        for group in &self.groups {
            if group.category == "GPU" {
                for sensor in group.sensors.iter().filter(|s| s.value.is_finite()) {
                    if sensor.label == "Temperature"
                        || sensor.label == "Core Temp"
                        || sensor.label == "Temp"
                    {
                        return Some(sensor.value as u8);
                    }
                }
            }
        }
        None
    }

    pub fn get_cpu_temp(&self) -> Option<u8> {
        for group in &self.groups {
            if group.category == "CPU" {
                for sensor in group.sensors.iter().filter(|s| s.value.is_finite()) {
                    if sensor.label == "Temperature"
                        || sensor.label == "Package Temp"
                        || sensor.label == "Tctl"
                        || sensor.label == "Tdie"
                    {
                        return Some(sensor.value as u8);
                    }
                }
            }
        }
        None
    }

    pub fn get_cpu_power(&self) -> Option<f32> {
        for group in &self.groups {
            if group.category == "CPU" && group.name == "CPU Package Power [RAPL]" {
                for sensor in group.sensors.iter().filter(|s| s.value.is_finite()) {
                    if sensor.label.contains("Package")
                        || sensor.label.contains("CPU")
                        || sensor.label.contains("Average")
                    {
                        return Some(sensor.value);
                    }
                }
            }
        }
        None
    }

    pub fn get_gpu_power(&self) -> Option<f32> {
        for group in &self.groups {
            if group.category == "GPU" {
                for sensor in group.sensors.iter().filter(|s| s.value.is_finite()) {
                    if sensor.label.contains("Power") || sensor.unit == "W" {
                        return Some(sensor.value);
                    }
                }
            }
        }
        None
    }

    pub fn get_gpu_core_clock(&self) -> Option<u32> {
        for group in &self.groups {
            if group.category == "GPU" {
                for sensor in group.sensors.iter().filter(|s| s.value.is_finite()) {
                    if sensor.label == "GPU Clock" {
                        return Some(sensor.value as u32);
                    }
                }
            }
        }
        None
    }

    pub fn get_gpu_mem_clock(&self) -> Option<u32> {
        for group in &self.groups {
            if group.category == "GPU" {
                for sensor in group.sensors.iter().filter(|s| s.value.is_finite()) {
                    if sensor.label == "Memory Clock" {
                        return Some(sensor.value as u32);
                    }
                }
            }
        }
        None
    }

    pub fn get_gpu_fan_speed(&self) -> Option<u8> {
        for group in &self.groups {
            if group.category == "GPU" {
                for sensor in group.sensors.iter().filter(|s| s.value.is_finite()) {
                    if sensor.label == "Fan Speed" {
                        return Some(sensor.value as u8);
                    }
                }
            }
        }
        None
    }

    pub fn get_cpu_freq(&self) -> Option<u32> {
        // Read /sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq
        std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .map(|khz| khz / 1000)
    }

    pub fn get_gpu_name(&self) -> String {
        for group in &self.groups {
            if group.category == "GPU" {
                return group.name.clone();
            }
        }
        "Unknown GPU".to_string()
    }

    pub fn get_vram_usage_gb(&self) -> Option<f32> {
        for group in &self.groups {
            if group.category == "GPU" {
                for sensor in group.sensors.iter().filter(|s| s.value.is_finite()) {
                    if sensor.label.contains("VRAM Used") || sensor.label.contains("Memory Used") {
                        if sensor.unit == "MiB" {
                            return Some(sensor.value / 1024.0);
                        } else if sensor.unit == "GiB" {
                            return Some(sensor.value);
                        }
                    }
                }
            }
        }
        None
    }

    pub fn get_vram_total_gb(&self) -> Option<f32> {
        for group in &self.groups {
            if group.category == "GPU" {
                for sensor in group.sensors.iter().filter(|s| s.value.is_finite()) {
                    if sensor.label.contains("VRAM Total") || sensor.label.contains("Memory Total")
                    {
                        if sensor.unit == "MiB" {
                            return Some(sensor.value / 1024.0);
                        } else if sensor.unit == "GiB" {
                            return Some(sensor.value);
                        }
                    }
                }
            }
        }
        None
    }
}

// ── HwCollector ───────────────────────────────────────────────────────────────

pub struct HwCollector {
    pub data: HwMonitorData,
    /// device_name → [sectors_read, sectors_written]
    prev_disk: HashMap<String, [u64; 2]>,
    /// iface_name → [rx_bytes, tx_bytes]
    prev_net: HashMap<String, [u64; 2]>,
    prev_time: Instant,
}

impl HwCollector {
    pub fn new() -> Self {
        let mut c = Self {
            data: HwMonitorData::default(),
            prev_disk: HashMap::new(),
            prev_net: HashMap::new(),
            prev_time: Instant::now(),
        };
        c.prev_disk = read_disk_raw();
        c.prev_net = read_net_raw();
        c
    }

    pub fn update(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.prev_time).as_secs_f32().max(0.1);

        let new_disk = read_disk_raw();
        let new_net = read_net_raw();

        let readings = collect_all(&self.prev_disk, &new_disk, &self.prev_net, &new_net, dt);

        self.prev_disk = new_disk;
        self.prev_net = new_net;
        self.prev_time = now;

        self.merge(readings);
    }

    /// Merge new readings into `self.data`, preserving existing sensor history.
    fn merge(&mut self, readings: Vec<(&'static str, String, Vec<Reading>)>) {
        // Preserve history, but never present an old reading as a current one.
        for group in &mut self.data.groups {
            for sensor in &mut group.sensors {
                sensor.value = f32::NAN;
            }
        }
        for (category, group_name, sensors) in readings {
            // Match on (category, name), not name alone — two categories can
            // legitimately expose the same group name (e.g. "coretemp" under
            // both CPU and GPU), and merging them would interleave their
            // sensors.
            let g_idx = match self
                .data
                .groups
                .iter()
                .position(|g| g.category == category && g.name == group_name)
            {
                Some(i) => i,
                None => {
                    self.data.groups.push(SensorGroup {
                        category,
                        name: group_name,
                        sensors: Vec::new(),
                    });
                    self.data.groups.len() - 1
                }
            };
            for (label, unit, value) in sensors {
                let group = &mut self.data.groups[g_idx];
                match group.sensors.iter_mut().find(|s| s.label == label) {
                    Some(s) => s.push(value),
                    None => {
                        let mut s = Sensor::new(label, unit);
                        s.push(value);
                        group.sensors.push(s);
                    }
                }
            }
        }
    }
}

// ── Reading type ──────────────────────────────────────────────────────────────

/// (label, unit, value) — all strings are &'static to avoid allocation per tick.
type Reading = (&'static str, &'static str, f32);

/// (category, group_name, readings)
type GroupReading = (&'static str, String, Vec<Reading>);

// ── Top-level collector ───────────────────────────────────────────────────────

fn collect_all(
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
}

// ── hwmon: per-category collectors ───────────────────────────────────────────

fn collect_all_hwmon(topo: &crate::cpu_park::CpuTopology) -> Vec<GroupReading> {
    let core_id_map = build_core_id_to_cpu_map();

    let mut groups = collect_hwmon_where(
        |_| true,
        |path, hw_name| {
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
        },
    );

    // Remap CPU core labels like the original did
    for (cat, _name, sensors) in &mut groups {
        if *cat == "CPU" {
            for (label, _unit, _value) in sensors.iter_mut() {
                if let Some(core_id) = label
                    .strip_prefix("Core ")
                    .and_then(|s| s.parse::<u32>().ok())
                {
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

fn collect_hwmon_where<F, G>(filter: F, namer: G) -> Vec<GroupReading>
where
    F: Fn(&str) -> bool,
    G: Fn(&Path, &str) -> (&'static str, String),
{
    let mut groups: Vec<GroupReading> = Vec::new();

    let hwmon_dir = Path::new("/sys/class/hwmon");
    let mut entries: Vec<_> = match std::fs::read_dir(hwmon_dir) {
        Ok(e) => e.flatten().collect(),
        Err(_) => return groups,
    };
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let hw_name = match read_trimmed(&path.join("name")) {
            Some(n) => n,
            None => continue,
        };
        if !filter(&hw_name) {
            continue;
        }

        let mut sensors: Vec<Reading> = Vec::new();

        // Temperature – discover indices dynamically (coretemp on Arrow Lake
        // uses non-contiguous indices up to temp46+).
        {
            let mut temp_ids: Vec<u32> = Vec::new();
            if let Ok(dir) = std::fs::read_dir(&path) {
                for de in dir.flatten() {
                    let fname = de.file_name();
                    let s = fname.to_string_lossy();
                    if let Some(rest) = s.strip_prefix("temp") {
                        if let Some(num_s) = rest.strip_suffix("_input") {
                            if let Ok(n) = num_s.parse::<u32>() {
                                temp_ids.push(n);
                            }
                        }
                    }
                }
            }
            temp_ids.sort_unstable();
            for i in temp_ids {
                let input = path.join(format!("temp{i}_input"));
                if let Some(val) = read_millidegrees(&input) {
                    let lbl_file = path.join(format!("temp{i}_label"));
                    let lbl = read_trimmed(&lbl_file).unwrap_or_else(|| format!("Temp {i}"));
                    sensors.push((intern(lbl), "°C", val));
                }
            }
        }

        // Fan
        for i in 1..=8u32 {
            let input = path.join(format!("fan{i}_input"));
            if !input.exists() {
                continue;
            }
            if let Some(rpm) = read_u64(&input) {
                let lbl_file = path.join(format!("fan{i}_label"));
                let lbl = read_trimmed(&lbl_file).unwrap_or_else(|| format!("Fan {i}"));
                sensors.push((intern(lbl), "RPM", rpm as f32));
            }
        }

        // Power (µW → W)
        for i in 1..=8u32 {
            let input = path.join(format!("power{i}_input"));
            if !input.exists() {
                continue;
            }
            if let Some(uw) = read_u64(&input) {
                let lbl_file = path.join(format!("power{i}_label"));
                let lbl = read_trimmed(&lbl_file).unwrap_or_else(|| format!("Power {i}"));
                sensors.push((intern(lbl), "W", uw as f32 / 1_000_000.0));
            }
        }

        // Voltage (mV → V)
        for i in 0..=16u32 {
            let input = path.join(format!("in{i}_input"));
            if !input.exists() {
                continue;
            }
            if let Some(mv) = read_u64(&input) {
                let lbl_file = path.join(format!("in{i}_label"));
                let lbl = read_trimmed(&lbl_file).unwrap_or_else(|| format!("In {i}"));
                sensors.push((intern(lbl), "V", mv as f32 / 1000.0));
            }
        }

        // Frequency (Hz → MHz; amdgpu etc.)
        for i in 1..=4u32 {
            let input = path.join(format!("freq{i}_input"));
            if !input.exists() {
                continue;
            }
            if let Some(hz) = read_u64(&input) {
                let lbl_file = path.join(format!("freq{i}_label"));
                let lbl = read_trimmed(&lbl_file).unwrap_or_else(|| format!("Freq {i}"));
                sensors.push((intern(lbl), "MHz", hz as f32 / 1_000_000.0));
            }
        }

        if !sensors.is_empty() {
            let (cat, name) = namer(&path, &hw_name);
            groups.push((cat, name, sensors));
        }
    }

    groups
}

// ── NVIDIA GPU via NVML ──────────────────────────────────────────────────────

// Keep NVML handle alive across calls (init is expensive, ~50ms).
// Module-level so both the sensor collector and the per-process utilization
// query share one context.
static NVML: std::sync::Mutex<Option<nvml_wrapper::Nvml>> = std::sync::Mutex::new(None);

/// Per-process GPU utilization (SM %) across all NVIDIA devices, keyed by PID.
/// Empty on systems without NVML. Uses the driver's rolling sample buffer,
/// querying only samples newer than the previous call.
pub fn collect_gpu_process_util() -> HashMap<u32, f32> {
    static LAST_TS: std::sync::Mutex<u64> = std::sync::Mutex::new(0);

    let mut map: HashMap<u32, f32> = HashMap::new();
    let guard = NVML.lock().unwrap();
    let Some(nvml) = guard.as_ref() else {
        return map; // sensor collector initializes NVML; nothing yet
    };
    let Ok(count) = nvml.device_count() else {
        return map;
    };
    let mut last_ts = LAST_TS.lock().unwrap();
    let mut newest = *last_ts;
    for i in 0..count {
        let Ok(dev) = nvml.device_by_index(i) else {
            continue;
        };
        // Errors (e.g. NotFound when no process used the GPU since last call)
        // are expected — just skip.
        let Ok(samples) = dev.process_utilization_stats(*last_ts) else {
            continue;
        };
        for s in samples {
            newest = newest.max(s.timestamp);
            let entry = map.entry(s.pid).or_insert(0.0);
            *entry = entry.max(s.sm_util as f32);
        }
    }
    *last_ts = newest;
    map
}

fn collect_nvidia_nvml() -> Vec<GroupReading> {
    let mut guard = NVML.lock().unwrap();
    let nvml = match guard.as_ref() {
        Some(n) => n,
        None => match nvml_wrapper::Nvml::init() {
            Ok(n) => {
                *guard = Some(n);
                guard.as_ref().unwrap()
            }
            Err(_) => return Vec::new(),
        },
    };

    let count = match nvml.device_count() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut groups = Vec::new();

    for i in 0..count {
        let dev = match nvml.device_by_index(i) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let gpu_name = dev.name().unwrap_or_else(|_| format!("GPU {i}"));

        let temp = dev
            .temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu)
            .map(|t| t as f32)
            .unwrap_or(f32::NAN);
        let power = dev
            .power_usage()
            .map(|mw| mw as f32 / 1000.0)
            .unwrap_or(f32::NAN);
        let g_clk = dev
            .clock_info(nvml_wrapper::enum_wrappers::device::Clock::Graphics)
            .map(|mhz| mhz as f32)
            .unwrap_or(f32::NAN);
        let m_clk = dev
            .clock_info(nvml_wrapper::enum_wrappers::device::Clock::Memory)
            .map(|mhz| mhz as f32)
            .unwrap_or(f32::NAN);

        let (g_util, m_util) = dev
            .utilization_rates()
            .map(|u| (u.gpu as f32, u.memory as f32))
            .unwrap_or((f32::NAN, f32::NAN));

        let mem = dev.memory_info();
        let m_used = mem
            .as_ref()
            .map(|m| m.used as f32 / (1024.0 * 1024.0 * 1024.0))
            .unwrap_or(f32::NAN);
        let m_total = mem
            .as_ref()
            .map(|m| m.total as f32 / (1024.0 * 1024.0 * 1024.0))
            .unwrap_or(f32::NAN);

        let fan_speed = dev.fan_speed(0).map(|v| v as f32).unwrap_or(f32::NAN);

        let sensors: Vec<Reading> = vec![
            ("Temperature", "°C", temp),
            ("GPU Load", "%", g_util),
            ("Memory Usage", "%", m_util),
            ("Power Draw", "W", power),
            ("GPU Clock", "MHz", g_clk),
            ("Memory Clock", "MHz", m_clk),
            ("VRAM Used", "GiB", m_used),
            ("VRAM Total", "GiB", m_total),
            ("Fan Speed", "%", fan_speed),
        ];

        groups.push(("GPU", intern(&gpu_name).to_string(), sensors));
    }

    groups
}

// ── CPU package power via RAPL ───────────────────────────────────────────────
// powercap/intel-rapl exposes per-zone energy counters on both Intel and AMD
// (the "intel-rapl" name is historical; the Linux driver handles AMD Fam17h+
// via the same sysfs interface). Power = Δenergy_uj / Δt.
//
// Counter wraps at max_energy_range_uj (~65 kJ → ~11 min at 100 W). Wrap is
// detected by now < prev.
//
// energy_uj is root-only by default (Platypus side-channel mitigation). The
// udev rule /etc/udev/rules.d/60-rapl-energy.rules makes it world-readable;
// without it this function silently returns empty on the first sample since
// read_u64 fails.

/// Compute watts from two RAPL energy samples, handling counter wrap at max_uj.
/// Returns None if dt is non-positive.
fn rapl_watts(prev_uj: u64, now_uj: u64, max_uj: u64, dt_secs: f64) -> Option<f32> {
    if dt_secs.is_nan() || dt_secs <= 0.0 {
        return None;
    }
    let delta_uj = if now_uj >= prev_uj {
        now_uj - prev_uj
    } else {
        // Counter went backwards: either a genuine wrap at max_uj, or a reset
        // (suspend/resume, driver reload) — a reset with the wrap formula
        // yields a delta near max_uj and a bogus multi-kilowatt spike that
        // permanently pollutes the max/avg stats. If max_uj is unknown
        // (u64::MAX sentinel) we can't distinguish, so drop the sample.
        if max_uj == u64::MAX {
            return None;
        }
        max_uj
            .saturating_sub(prev_uj)
            .saturating_add(now_uj)
            .saturating_add(1)
    };
    let watts = delta_uj as f64 / 1_000_000.0 / dt_secs;
    // Sanity cap: no consumer package draws 10 kW — treat as reset, not wrap.
    if watts > 10_000.0 {
        return None;
    }
    Some(watts as f32)
}

fn collect_rapl_power() -> Vec<GroupReading> {
    if let Some(watts) = crate::sensor_data::read().ok().and_then(|s| s.cpu_power_w) {
        return vec![(
            "CPU",
            "CPU Package Power [RAPL]".into(),
            vec![("Package total", "W", watts)],
        )];
    }
    use std::sync::Mutex;
    use std::time::Instant;

    struct ZoneSample {
        energy_uj: u64,
        max_uj: u64,
        at: Instant,
    }

    static STATE: Mutex<Option<HashMap<String, ZoneSample>>> = Mutex::new(None);

    let root = Path::new("/sys/class/powercap");
    let dir = match std::fs::read_dir(root) {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };

    let now = Instant::now();
    // key = sysfs dir name ("intel-rapl:0"); value = (display label, energy, max)
    let mut current: HashMap<String, (String, u64, u64)> = HashMap::new();
    for entry in dir.flatten() {
        let path = entry.path();
        let fname = entry.file_name();
        let kname = fname.to_string_lossy();
        if !kname.starts_with("intel-rapl:") {
            continue;
        }
        // Only top-level zones (package). Skip sub-zones like "intel-rapl:0:0"
        // (core/uncore/dram), which are already part of the package total.
        let suffix = &kname["intel-rapl:".len()..];
        if suffix.contains(':') {
            continue;
        }
        let label = read_trimmed(&path.join("name")).unwrap_or_else(|| kname.to_string());
        let energy = match read_u64(&path.join("energy_uj")) {
            Some(v) => v,
            None => continue,
        };
        let max = read_u64(&path.join("max_energy_range_uj")).unwrap_or(u64::MAX);
        current.insert(kname.into_owned(), (label, energy, max));
    }

    if current.is_empty() {
        return Vec::new();
    }

    let mut guard = STATE.lock().unwrap();
    let mut sensors: Vec<Reading> = Vec::new();

    let mut keys: Vec<&String> = current.keys().collect();
    keys.sort();
    for key in keys {
        let (label, now_uj, _max_uj) = &current[key];
        // Skip on the first sample for this zone — without a prior reading we
        // can't compute power, and emitting 0 W would show a spurious dip.
        if let Some(prev) = guard.as_ref().and_then(|m| m.get(key)) {
            let dt = now.duration_since(prev.at).as_secs_f64().max(0.05);
            if let Some(watts) = rapl_watts(prev.energy_uj, *now_uj, prev.max_uj, dt) {
                sensors.push((intern(pretty_rapl_label(label)), "W", watts));
            }
        }
    }

    // Replace previous sample set with the one we just took.
    let mut new_state: HashMap<String, ZoneSample> = HashMap::with_capacity(current.len());
    for (k, (_label, energy, max)) in current {
        new_state.insert(
            k,
            ZoneSample {
                energy_uj: energy,
                max_uj: max,
                at: now,
            },
        );
    }
    *guard = Some(new_state);

    if sensors.is_empty() {
        return Vec::new();
    }
    vec![("CPU", "CPU Package Power [RAPL]".into(), sensors)]
}

fn pretty_rapl_label(raw: &str) -> String {
    let lc = raw.to_ascii_lowercase();
    if let Some(rest) = lc.strip_prefix("package-") {
        return format!("Package {rest}");
    }
    match lc.as_str() {
        "package" => "Package".into(),
        "dram" => "DRAM".into(),
        "core" => "Core".into(),
        "uncore" => "Uncore".into(),
        "psys" => "PSys".into(),
        _ => raw.to_string(),
    }
}

// ── CPU frequencies ───────────────────────────────────────────────────────────

fn collect_cpu_freqs(topo: &crate::cpu_park::CpuTopology) -> Option<GroupReading> {
    let cpu_dir = Path::new("/sys/devices/system/cpu");
    let mut entries: Vec<_> = std::fs::read_dir(cpu_dir)
        .ok()?
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            let s = n.to_string_lossy();
            s.starts_with("cpu") && s[3..].parse::<u32>().is_ok()
        })
        .collect();
    entries.sort_by_key(|e| {
        e.file_name().to_string_lossy()[3..]
            .parse::<u32>()
            .unwrap_or(0)
    });

    let mut sensors: Vec<Reading> = Vec::new();
    for entry in entries {
        let num: u32 = entry.file_name().to_string_lossy()[3..]
            .parse()
            .unwrap_or(0);
        let freq_path = entry.path().join("cpufreq/scaling_cur_freq");
        if let Some(khz) = read_u64(&freq_path) {
            let kind = core_kind_suffix(topo, num);
            sensors.push((
                intern(format!("CPU {num}{kind}")),
                "MHz",
                khz as f32 / 1000.0,
            ));
        }
    }

    if sensors.is_empty() {
        None
    } else {
        Some(("CPU", "CPU Frequencies".into(), sensors))
    }
}

// ── Load average ──────────────────────────────────────────────────────────────

fn collect_load_avg() -> Option<GroupReading> {
    let text = std::fs::read_to_string("/proc/loadavg").ok()?;
    let mut parts = text.split_whitespace();
    let l1: f32 = parts.next()?.parse().ok()?;
    let l5: f32 = parts.next()?.parse().ok()?;
    let l15: f32 = parts.next()?.parse().ok()?;

    Some((
        "CPU",
        "Load Average".into(),
        vec![("1 min", "", l1), ("5 min", "", l5), ("15 min", "", l15)],
    ))
}

// ── /proc/meminfo ─────────────────────────────────────────────────────────────

fn collect_meminfo() -> Option<GroupReading> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut map: HashMap<&str, u64> = HashMap::new();
    for line in text.lines() {
        let mut p = line.split_whitespace();
        if let (Some(k), Some(v)) = (p.next(), p.next()) {
            if let Ok(n) = v.parse::<u64>() {
                map.insert(k.trim_end_matches(':'), n);
            }
        }
    }

    let gib = |k: &str| map.get(k).copied().unwrap_or(0) as f32 / (1024.0 * 1024.0);

    let total = gib("MemTotal");
    let available = gib("MemAvailable");
    let used = total - available;
    let buffers = gib("Buffers");
    let cached = gib("Cached");
    let swap_total = gib("SwapTotal");
    let swap_used = swap_total - gib("SwapFree");

    let mut sensors: Vec<Reading> = vec![
        ("Total", "GiB", total),
        ("Used", "GiB", used),
        ("Available", "GiB", available),
        ("Buffers", "GiB", buffers),
        ("Cached", "GiB", cached),
    ];
    if swap_total > 0.0 {
        sensors.push(("Swap Total", "GiB", swap_total));
        sensors.push(("Swap Used", "GiB", swap_used));
    }

    Some(("Memory", "RAM".into(), sensors))
}

// ── Disk I/O ──────────────────────────────────────────────────────────────────

fn read_disk_raw() -> HashMap<String, [u64; 2]> {
    let mut map = HashMap::new();
    let text = match std::fs::read_to_string("/proc/diskstats") {
        Ok(t) => t,
        Err(_) => return map,
    };
    for line in text.lines() {
        let p: Vec<&str> = line.split_whitespace().collect();
        if p.len() < 14 {
            continue;
        }
        let dev = p[2].to_string();
        // Skip partition entries — keep whole disks. For sdX, any trailing
        // digit means partition; for nvme/mmcblk, the whole disk itself ends
        // in a digit (nvme0n1), so partitions are the "p<digits>" suffix
        // (nvme0n1p1, mmcblk0p2) — without that check every NVMe partition
        // was double-counted alongside its disk.
        let ends_in_digit = dev
            .chars()
            .last()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false);
        let is_partition = if dev.starts_with("nvme") || dev.starts_with("mmcblk") {
            let trailing_digits = dev.chars().rev().take_while(|c| c.is_ascii_digit()).count();
            dev.as_bytes()
                .get(dev.len().saturating_sub(trailing_digits + 1))
                == Some(&b'p')
        } else {
            ends_in_digit
        };
        if is_partition {
            continue;
        }
        let r: u64 = p[5].parse().unwrap_or(0); // sectors read
        let w: u64 = p[9].parse().unwrap_or(0); // sectors written
        map.insert(dev, [r, w]);
    }
    map
}

fn collect_disk_io(
    prev: &HashMap<String, [u64; 2]>,
    new: &HashMap<String, [u64; 2]>,
    dt: f32,
) -> Vec<GroupReading> {
    let mut groups = Vec::new();
    let sector = 512.0_f32;

    let mut devs: Vec<&String> = new.keys().collect();
    devs.sort();

    for dev in devs {
        let n = &new[dev];
        let p = prev.get(dev).copied().unwrap_or(*n);

        let read_mb = n[0].saturating_sub(p[0]) as f32 * sector / (dt * 1_048_576.0);
        let write_mb = n[1].saturating_sub(p[1]) as f32 * sector / (dt * 1_048_576.0);

        // Try to find a human-readable model for this block device
        let label = block_device_model(dev)
            .map(|m| format!("{dev} — {m}"))
            .unwrap_or_else(|| dev.clone());

        groups.push((
            "Storage",
            format!("I/O [{label}]"),
            vec![("Read", "MB/s", read_mb), ("Write", "MB/s", write_mb)],
        ));
    }
    groups
}

fn block_device_model(dev: &str) -> Option<String> {
    // /sys/block/<dev>/device/model  (NVMe)
    let path = Path::new("/sys/block").join(dev).join("device/model");
    read_trimmed(&path).map(|s| s.trim().to_string())
}

// ── Network I/O ───────────────────────────────────────────────────────────────

fn read_net_raw() -> HashMap<String, [u64; 2]> {
    let mut map = HashMap::new();
    let text = match std::fs::read_to_string("/proc/net/dev") {
        Ok(t) => t,
        Err(_) => return map,
    };
    for line in text.lines().skip(2) {
        let line = line.trim();
        let colon = match line.find(':') {
            Some(c) => c,
            None => continue,
        };
        let iface = line[..colon].trim().to_string();
        if iface == "lo" {
            continue;
        }
        let fields: Vec<&str> = line[colon + 1..].split_whitespace().collect();
        if fields.len() < 9 {
            continue;
        }
        let rx: u64 = fields[0].parse().unwrap_or(0);
        let tx: u64 = fields[8].parse().unwrap_or(0);
        map.insert(iface, [rx, tx]);
    }
    map
}

fn collect_net_io(
    prev: &HashMap<String, [u64; 2]>,
    new: &HashMap<String, [u64; 2]>,
    dt: f32,
) -> Vec<GroupReading> {
    let mut groups = Vec::new();
    let mut ifaces: Vec<&String> = new.keys().collect();
    ifaces.sort();

    for iface in ifaces {
        let n = &new[iface];
        let p = prev.get(iface).copied().unwrap_or(*n);

        let rx = n[0].saturating_sub(p[0]) as f32 / (dt * 1_048_576.0);
        let tx = n[1].saturating_sub(p[1]) as f32 / (dt * 1_048_576.0);

        groups.push((
            "Network",
            format!("I/O [{iface}]"),
            vec![("Receive", "MB/s", rx), ("Transmit", "MB/s", tx)],
        ));
    }
    groups
}

// ── sysfs helpers ─────────────────────────────────────────────────────────────

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

fn read_u64(path: &Path) -> Option<u64> {
    read_trimmed(path)?.parse().ok()
}

fn read_millidegrees(path: &Path) -> Option<f32> {
    read_u64(path).map(|v| v as f32 / 1000.0)
}

/// Determine NIC interface name from hwmon device symlink.
/// e.g., hwmon device → PCI device → net/ethX
fn nic_interface_name(hwmon_path: &Path) -> Option<String> {
    let dev = std::fs::canonicalize(hwmon_path.join("device")).ok()?;
    // /sys/devices/.../net/<iface>
    let net = dev.join("net");
    std::fs::read_dir(&net)
        .ok()?
        .flatten()
        .next()
        .map(|e| e.file_name().to_string_lossy().to_string())
}

/// Derive DDR5 slot label from spd5118 i2c device address.
fn dimm_slot_name(hwmon_path: &Path) -> String {
    // Device path target is something like: .../i2c-7/7-0051
    // Address 0x51 → slot 1, 0x50 → slot 0, etc.
    let canon = std::fs::canonicalize(hwmon_path.join("device")).ok();
    if let Some(d) = canon {
        let name = d
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        // name = "7-0051"  (bus-address)
        if let Some(addr_str) = name.split('-').next_back() {
            if let Ok(addr) = u64::from_str_radix(addr_str, 16) {
                let slot = addr & 0x0f;
                return format!("DDR5 Slot {slot}");
            }
        }
    }
    "DDR5".into()
}

/// Topology-aware core suffix: " (V-Cache)" / " (Std)" for AMD X3D,
/// " (P)" / " (E)" for Intel Hybrid, "" for uniform.
fn core_kind_suffix(topo: &crate::cpu_park::CpuTopology, cpu: u32) -> &'static str {
    if !topo.has_asymmetry() {
        return "";
    }
    match topo.kind {
        crate::cpu_park::TopologyKind::AmdX3D => {
            if topo.preferred.contains(&cpu) {
                " (V-Cache)"
            } else {
                " (Std)"
            }
        }
        crate::cpu_park::TopologyKind::IntelHybrid => {
            if topo.preferred.contains(&cpu) {
                " (P)"
            } else {
                " (E)"
            }
        }
        crate::cpu_park::TopologyKind::Uniform => "",
    }
}

/// Intern dynamic strings as &'static str via a leak-once cache.
/// The set of unique sensor labels from sysfs is small and bounded.
fn intern(s: impl Into<String>) -> &'static str {
    use std::collections::HashSet;
    use std::sync::Mutex;
    static CACHE: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let s = s.into();
    let mut guard = CACHE.lock().unwrap();
    let set = guard.get_or_insert_with(HashSet::new);
    if let Some(&existing) = set.get(s.as_str()) {
        return existing;
    }
    let leaked: &'static str = Box::leak(s.into_boxed_str());
    set.insert(leaked);
    leaked
}

/// Map physical core_id → lowest logical CPU number.
fn build_core_id_to_cpu_map() -> HashMap<u32, u32> {
    let mut map: HashMap<u32, u32> = HashMap::new();
    let cpu_dir = Path::new("/sys/devices/system/cpu");
    if let Ok(entries) = std::fs::read_dir(cpu_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if let Some(cpu_num) = s.strip_prefix("cpu").and_then(|n| n.parse::<u32>().ok()) {
                let core_path = entry.path().join("topology/core_id");
                if let Some(core_id) = read_u64(&core_path).map(|v| v as u32) {
                    // Keep the lowest CPU number for each core_id
                    map.entry(core_id)
                        .and_modify(|existing| {
                            if cpu_num < *existing {
                                *existing = cpu_num;
                            }
                        })
                        .or_insert(cpu_num);
                }
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapl_watts_normal_delta() {
        // 1 J delta over 1 s → 1 W
        let w = rapl_watts(0, 1_000_000, u64::MAX, 1.0).unwrap();
        assert!((w - 1.0).abs() < 1e-6, "got {w}");
    }

    #[test]
    fn rapl_watts_counter_wrap() {
        // Wrap at 65 kJ: prev = max - 1µJ, now = 999_999µJ → delta ≈ 1 J in 1 s
        let max = 65_000_000_u64;
        let prev = max - 1;
        let now = 999_999;
        let w = rapl_watts(prev, now, max, 1.0).unwrap();
        assert!((w - 1.0).abs() < 1e-3, "wrap-handled watts off: {w}");
    }

    #[test]
    fn rapl_watts_zero_dt_is_none() {
        assert!(rapl_watts(0, 1_000_000, u64::MAX, 0.0).is_none());
        assert!(rapl_watts(0, 1_000_000, u64::MAX, -1.0).is_none());
    }

    #[test]
    fn rapl_watts_zero_delta_is_zero() {
        // No energy consumed between samples → 0 W (idle counter, freeze, etc.)
        let w = rapl_watts(42, 42, u64::MAX, 0.5).unwrap();
        assert_eq!(w, 0.0);
    }

    #[test]
    fn pretty_rapl_label_known_zones() {
        assert_eq!(pretty_rapl_label("package-0"), "Package 0");
        assert_eq!(pretty_rapl_label("Package-1"), "Package 1");
        assert_eq!(pretty_rapl_label("dram"), "DRAM");
        assert_eq!(pretty_rapl_label("psys"), "PSys");
        // Unknown labels passed through verbatim
        assert_eq!(pretty_rapl_label("custom-zone"), "custom-zone");
    }
}
