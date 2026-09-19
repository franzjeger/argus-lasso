//! Argus-Layer: Vulkan implicit layer that draws a telemetry HUD.
//!
//! Hooks: vkCreateInstance, vkEnumeratePhysicalDevices, vkCreateDevice,
//!        vkGetDeviceQueue, vkCreateSwapchainKHR, vkQueuePresentKHR.

mod activation;
mod capture;
pub mod font;
pub mod hud;
pub mod renderer;

use argus_ipc::{IpcMessage, OverlayConfig, TelemetryFrame};
use ash::vk;
use renderer::OverlayState;
use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::os::raw::c_char;
use std::sync::{Mutex, RwLock};
use std::thread;
use std::time::Instant;

// ── Global mutable function pointers (set once during negotiation) ──────────
static mut NEXT_GET_INSTANCE_PROC_ADDR: Option<vk::PFN_vkGetInstanceProcAddr> = None;
static mut NEXT_GET_DEVICE_PROC_ADDR: Option<vk::PFN_vkGetDeviceProcAddr> = None;

// ── Lookup tables ───────────────────────────────────────────────────────────
lazy_static::lazy_static! {
    // Telemetry data from the Argus-Lasso daemon (received via IPC)
    static ref TELEMETRY: RwLock<TelemetryState> = RwLock::new(TelemetryState::default());
    static ref GRAPHICS_QUEUES: RwLock<HashMap<vk::Queue, bool>> = RwLock::new(HashMap::new());
    static ref LOADER_DATA: RwLock<HashMap<vk::Device, unsafe extern "system" fn(vk::Device, *mut c_void) -> vk::Result>> = RwLock::new(HashMap::new());
    static ref QUEUE_FAMILIES: RwLock<HashMap<vk::Queue, u32>> = RwLock::new(HashMap::new());
    static ref ACTIVE: bool = activation::enabled();
    static ref PASSTHROUGH: bool = !*ACTIVE || std::env::var("ARGUS_LASSO_DRAW").as_deref() == Ok("0");

    // Configuration for the overlay (received via IPC)
    pub static ref OVERLAY_CONFIG: RwLock<OverlayConfig> = RwLock::new(OverlayConfig::default());

    // Instance → ash::Instance (we need this to call instance-level functions)
    static ref ASH_INSTANCES: RwLock<HashMap<vk::Instance, ash::Instance>> = RwLock::new(HashMap::new());

    // PhysicalDevice → Instance mapping
    static ref PHYS_TO_INST: RwLock<HashMap<vk::PhysicalDevice, vk::Instance>> = RwLock::new(HashMap::new());

    // Device → (PhysicalDevice, ash::Device)
    static ref DEVICE_MAP: RwLock<HashMap<vk::Device, (vk::PhysicalDevice, ash::Device)>> = RwLock::new(HashMap::new());

    // Device → queue family index used for the graphics queue
    static ref DEVICE_QUEUE_FAMILY: RwLock<HashMap<vk::Device, u32>> = RwLock::new(HashMap::new());

    // Queue → Device mapping
    static ref QUEUE_TO_DEVICE: RwLock<HashMap<vk::Queue, vk::Device>> = RwLock::new(HashMap::new());

    // Real function pointers per-handle
    static ref REAL_QUEUE_PRESENT: RwLock<HashMap<vk::Queue, vk::PFN_vkQueuePresentKHR>> = RwLock::new(HashMap::new());

    // SwapchainKHR → OverlayState
    static ref OVERLAY_STATES: Mutex<HashMap<vk::SwapchainKHR, OverlayState>> = Mutex::new(HashMap::new());
}

// ── IPC thread ──────────────────────────────────────────────────────────────

#[derive(Default)]
struct TelemetryState {
    frame: Option<TelemetryFrame>,
    received: Option<Instant>,
    connected: bool,
}
impl TelemetryState {
    fn status(&self) -> &'static str {
        if !self.connected || self.frame.is_none() {
            return "Telemetri frakoblet";
        }
        let frame = self.frame.as_ref().unwrap();
        let max_age = (frame.sample_interval_ms * 3).max(6000);
        let wall_age = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if self
            .received
            .is_none_or(|t| t.elapsed().as_millis() as u64 > max_age)
            || wall_age.saturating_sub(frame.sample_unix_ms) > max_age
        {
            "Data foreldet"
        } else {
            ""
        }
    }
}
fn start_ipc_thread() {
    capture::init();
    thread::spawn(|| {
        let mut last_error = String::new();
        let mut host_pid = 0;
        loop {
            let result = (|| -> std::io::Result<()> {
                let (mut stream, path) = argus_ipc::connect()?;
                stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
                loop {
                    match argus_ipc::read_message(&mut stream)? {
                        IpcMessage::Hello {
                            build,
                            protocol,
                            host_pid: pid,
                        } => {
                            host_pid = pid;
                            eprintln!("[Argus-Layer] IPC connected socket={} daemon_build={build} protocol={protocol}", path.display());
                            TELEMETRY.write().unwrap().connected = true;
                            last_error.clear();
                        }
                        IpcMessage::Telemetry(mut frame) => {
                            frame.game = Some(game_telemetry(host_pid, &frame));
                            let mut tel = TELEMETRY.write().unwrap();
                            if tel.frame.is_none() {
                                eprintln!("[Argus-Layer] first valid telemetry timestamp={} CPU={} GPU={}", frame.sample_unix_ms, frame.cpu_name, frame.gpu_name);
                            }
                            tel.frame = Some(frame);
                            tel.received = Some(Instant::now());
                        }
                        IpcMessage::Config(config) => *OVERLAY_CONFIG.write().unwrap() = config,
                    }
                }
            })();
            TELEMETRY.write().unwrap().connected = false;
            if let Err(e) = result {
                let error = e.to_string();
                if error != last_error {
                    let tel = TELEMETRY.read().unwrap();
                    eprintln!(
                        "[Argus-Layer] IPC disconnected: {error}; last_valid_sample_ms={:?}",
                        tel.frame.as_ref().map(|f| f.sample_unix_ms)
                    );
                    last_error = error;
                }
            }
            thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}

// Runs in the IPC worker at sensor frequency, never in vkQueuePresentKHR.
fn game_telemetry(host_pid: u32, frame: &TelemetryFrame) -> argus_ipc::GameTelemetry {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .map(|(_, s)| s.split_whitespace().collect())
        .unwrap_or_default();
    let start = fields.get(19).and_then(|s| s.parse::<u64>().ok());
    let profile = frame
        .launch_profiles
        .iter()
        .find(|p| p.pid == host_pid && Some(p.start_ticks) == start)
        .map(|p| p.profile.clone())
        .filter(|p| !p.is_empty());
    argus_ipc::GameTelemetry {
        pid: host_pid,
        name: std::fs::read_to_string("/proc/self/comm")
            .map(|s| s.trim().to_owned())
            .unwrap_or_else(|_| "Unknown application".into()),
        main_thread_cpus: status
            .lines()
            .find_map(|s| s.strip_prefix("Cpus_allowed_list:"))
            .map(|s| s.trim().into()),
        nice: fields.get(16).and_then(|s| s.parse().ok()),
        launcher_profile: profile,
        probalance_active: frame.probalance_pids.contains(&host_pid),
    }
}

// ── Helper: build ash::StaticFn from our intercepted function pointer ───────

unsafe fn make_static_fn() -> ash::StaticFn {
    ash::StaticFn {
        get_instance_proc_addr: NEXT_GET_INSTANCE_PROC_ADDR.unwrap(),
    }
}

// ── Hooked Vulkan entry points ──────────────────────────────────────────────

// ---------- vkCreateInstance ----------

/// VkLayerInstanceCreateInfo from the Vulkan layer interface.
#[repr(C)]
struct VkLayerInstanceLink {
    p_next: *const VkLayerInstanceLink,
    pfn_next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pfn_next_get_physical_device_proc_addr: *const c_void,
}

#[repr(C)]
struct VkLayerInstanceCreateInfo {
    s_type: vk::StructureType,
    p_next: *const c_void,
    function: u32, // VK_LAYER_LINK_INFO = 0
    u: VkLayerInstanceCreateInfoUnion,
}

#[repr(C)]
union VkLayerInstanceCreateInfoUnion {
    p_layer_info: *mut VkLayerInstanceLink,
    pfn_set_instance_loader_data: *const c_void,
}

const VK_STRUCTURE_TYPE_LOADER_INSTANCE_CREATE_INFO: vk::StructureType =
    vk::StructureType::LOADER_INSTANCE_CREATE_INFO;

/// Walk the pNext chain of InstanceCreateInfo to find the layer link info.
unsafe fn find_layer_link(
    p_create_info: *const vk::InstanceCreateInfo,
) -> Option<*mut *mut VkLayerInstanceLink> {
    let mut p_next = (*p_create_info).p_next as *const VkLayerInstanceCreateInfo;
    while !p_next.is_null() {
        if (*p_next).s_type == VK_STRUCTURE_TYPE_LOADER_INSTANCE_CREATE_INFO
            && (*p_next).function == 0
        {
            // Return a pointer to the union field so we can modify it
            return Some(&mut (*(p_next as *mut VkLayerInstanceCreateInfo)).u.p_layer_info);
        }
        p_next = (*p_next).p_next as *const VkLayerInstanceCreateInfo;
    }
    None
}

/// # Safety
/// The Vulkan loader/caller must supply valid handles, pointer ranges and
/// allocation callbacks for this entry point, and satisfy Vulkan external
/// synchronization requirements. Returned function pointers must be called
/// with the corresponding Vulkan command signature.
#[no_mangle]
pub unsafe extern "system" fn argus_vkCreateInstance(
    p_create_info: *const vk::InstanceCreateInfo,
    p_allocator: *const vk::AllocationCallbacks,
    p_instance: *mut vk::Instance,
) -> vk::Result {
    // Find the layer link in the pNext chain
    let layer_link_ptr = match find_layer_link(p_create_info) {
        Some(ptr) => ptr,
        None => {
            eprintln!("[Argus-Layer] ERROR: Could not find layer link in pNext chain");
            return vk::Result::ERROR_INITIALIZATION_FAILED;
        }
    };

    let layer_link = *layer_link_ptr;
    // Save the next layer's GetInstanceProcAddr
    let next_gipa = (*layer_link).pfn_next_get_instance_proc_addr;

    // Advance the chain for the next layer
    *layer_link_ptr = (*layer_link).p_next as *mut VkLayerInstanceLink;

    // Get the real vkCreateInstance via the next layer's GetInstanceProcAddr
    let ptr = next_gipa(
        vk::Instance::null(),
        c"vkCreateInstance".as_ptr() as *const c_char,
    );
    let real_create_instance: vk::PFN_vkCreateInstance = match ptr {
        Some(p) => std::mem::transmute::<
            unsafe extern "system" fn(),
            for<'a, 'b> unsafe extern "system" fn(
                *const ash::vk::InstanceCreateInfo<'a>,
                *const ash::vk::AllocationCallbacks<'b>,
                *mut ash::vk::Instance,
            ) -> ash::vk::Result,
        >(p),
        None => return vk::Result::ERROR_INITIALIZATION_FAILED,
    };

    // Update our stored NEXT pointer
    NEXT_GET_INSTANCE_PROC_ADDR = Some(next_gipa);

    let res = real_create_instance(p_create_info, p_allocator, p_instance);
    if res != vk::Result::SUCCESS {
        return res;
    }

    let instance = *p_instance;
    eprintln!("[Argus-Layer] Instance created: {:?}", instance);

    // Build an ash::Instance so we can call Vulkan functions through it later
    let static_fn = make_static_fn();
    let ash_inst = ash::Instance::load(&static_fn, instance);
    if let Ok(devices) = ash_inst.enumerate_physical_devices() {
        let mut map = PHYS_TO_INST.write().unwrap();
        for device in devices {
            map.insert(device, instance);
        }
    }
    ASH_INSTANCES.write().unwrap().insert(instance, ash_inst);

    vk::Result::SUCCESS
}

// ---------- vkEnumeratePhysicalDevices ----------

/// # Safety
/// The Vulkan loader/caller must supply valid handles, pointer ranges and
/// allocation callbacks for this entry point, and satisfy Vulkan external
/// synchronization requirements. Returned function pointers must be called
/// with the corresponding Vulkan command signature.
#[no_mangle]
pub unsafe extern "system" fn argus_vkEnumeratePhysicalDevices(
    instance: vk::Instance,
    p_count: *mut u32,
    p_physical_devices: *mut vk::PhysicalDevice,
) -> vk::Result {
    // Call the real function via raw pointer
    let next = match NEXT_GET_INSTANCE_PROC_ADDR {
        Some(f) => f,
        None => return vk::Result::ERROR_INITIALIZATION_FAILED,
    };
    let ptr = next(
        instance,
        c"vkEnumeratePhysicalDevices".as_ptr() as *const c_char,
    );
    let real_fn: vk::PFN_vkEnumeratePhysicalDevices = match ptr {
        Some(p) => std::mem::transmute::<
            unsafe extern "system" fn(),
            unsafe extern "system" fn(
                ash::vk::Instance,
                *mut u32,
                *mut ash::vk::PhysicalDevice,
            ) -> ash::vk::Result,
        >(p),
        None => return vk::Result::ERROR_INITIALIZATION_FAILED,
    };

    let res = real_fn(instance, p_count, p_physical_devices);
    if res == vk::Result::SUCCESS && !p_physical_devices.is_null() {
        let count = *p_count as usize;
        let devices = std::slice::from_raw_parts(p_physical_devices, count);
        let mut map = PHYS_TO_INST.write().unwrap();
        for &pd in devices {
            map.insert(pd, instance);
        }
        eprintln!("[Argus-Layer] Enumerated {} physical device(s)", count);
    }
    res
}

// ---------- vkCreateDevice ----------

/// VkLayerDeviceCreateInfo from the Vulkan layer interface.
#[repr(C)]
struct VkLayerDeviceLink {
    p_next: *const VkLayerDeviceLink,
    pfn_next_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pfn_next_get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
}

#[repr(C)]
struct VkLayerDeviceCreateInfo {
    s_type: vk::StructureType,
    p_next: *const c_void,
    function: u32, // VK_LAYER_LINK_INFO = 0
    u: VkLayerDeviceCreateInfoUnion,
}

#[repr(C)]
union VkLayerDeviceCreateInfoUnion {
    p_layer_info: *mut VkLayerDeviceLink,
    pfn_set_device_loader_data: *const c_void,
}

const VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO: vk::StructureType =
    vk::StructureType::LOADER_DEVICE_CREATE_INFO;

unsafe fn find_device_layer_link(
    p_create_info: *const vk::DeviceCreateInfo,
) -> Option<*mut *mut VkLayerDeviceLink> {
    let mut p_next = (*p_create_info).p_next as *const VkLayerDeviceCreateInfo;
    while !p_next.is_null() {
        if (*p_next).s_type == VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO
            && (*p_next).function == 0
        {
            return Some(&mut (*(p_next as *mut VkLayerDeviceCreateInfo)).u.p_layer_info);
        }
        p_next = (*p_next).p_next as *const VkLayerDeviceCreateInfo;
    }
    None
}

// Store per-device the REAL (next layer's) vkGetDeviceProcAddr so we can
// resolve device functions without going through our own hook.
lazy_static::lazy_static! {
    static ref DEVICE_GDPA: RwLock<HashMap<vk::Device, vk::PFN_vkGetDeviceProcAddr>> = RwLock::new(HashMap::new());
}

/// # Safety
/// The Vulkan loader/caller must supply valid handles, pointer ranges and
/// allocation callbacks for this entry point, and satisfy Vulkan external
/// synchronization requirements. Returned function pointers must be called
/// with the corresponding Vulkan command signature.
#[no_mangle]
pub unsafe extern "system" fn argus_vkCreateDevice(
    physical_device: vk::PhysicalDevice,
    p_create_info: *const vk::DeviceCreateInfo,
    p_allocator: *const vk::AllocationCallbacks,
    p_device: *mut vk::Device,
) -> vk::Result {
    let instance = match PHYS_TO_INST.read().unwrap().get(&physical_device).copied() {
        Some(i) => i,
        None => return vk::Result::ERROR_INITIALIZATION_FAILED,
    };

    // Find layer link in pNext chain
    let layer_link_ptr = match find_device_layer_link(p_create_info) {
        Some(ptr) => ptr,
        None => {
            // Fallback: call through instance proc addr
            eprintln!("[Argus-Layer] WARNING: No device layer link, falling back");
            let next = match NEXT_GET_INSTANCE_PROC_ADDR {
                Some(f) => f,
                None => return vk::Result::ERROR_INITIALIZATION_FAILED,
            };
            let ptr = next(instance, c"vkCreateDevice".as_ptr() as *const c_char);
            let real_fn: vk::PFN_vkCreateDevice = match ptr {
                Some(p) => std::mem::transmute::<
                    unsafe extern "system" fn(),
                    for<'a, 'b> unsafe extern "system" fn(
                        ash::vk::PhysicalDevice,
                        *const ash::vk::DeviceCreateInfo<'a>,
                        *const ash::vk::AllocationCallbacks<'b>,
                        *mut ash::vk::Device,
                    ) -> ash::vk::Result,
                >(p),
                None => return vk::Result::ERROR_INITIALIZATION_FAILED,
            };
            let res = real_fn(physical_device, p_create_info, p_allocator, p_device);
            if res == vk::Result::SUCCESS {
                let device = *p_device;
                eprintln!("[Argus-Layer] Device created (fallback): {:?}", device);

                // For fallback, use the instance's get_device_proc_addr
                let ash_inst = ASH_INSTANCES
                    .read()
                    .unwrap()
                    .get(&instance)
                    .unwrap()
                    .clone();
                let gipa = ash_inst.fp_v1_0().get_device_proc_addr;
                let ash_dev = ash::Device::load_with(
                    |name| {
                        let ptr = gipa(device, name.as_ptr());
                        std::mem::transmute(ptr)
                    },
                    device,
                );
                DEVICE_MAP
                    .write()
                    .unwrap()
                    .insert(device, (physical_device, ash_dev));
                // We should also store it in DEVICE_GDPA so vkGetDeviceQueue etc work!
                DEVICE_GDPA.write().unwrap().insert(device, gipa);
            }
            return res;
        }
    };

    let layer_link = *layer_link_ptr;
    // Get the next layer's proc addrs from the chain
    let next_gipa = (*layer_link).pfn_next_get_instance_proc_addr;
    let next_gdpa = (*layer_link).pfn_next_get_device_proc_addr;

    // Advance the chain for the next layer
    *layer_link_ptr = (*layer_link).p_next as *mut VkLayerDeviceLink;

    // Get the real vkCreateDevice via the next layer's GetInstanceProcAddr
    let ptr = next_gipa(instance, c"vkCreateDevice".as_ptr() as *const c_char);
    let real_fn: vk::PFN_vkCreateDevice = match ptr {
        Some(p) => std::mem::transmute::<
            unsafe extern "system" fn(),
            for<'a, 'b> unsafe extern "system" fn(
                ash::vk::PhysicalDevice,
                *const ash::vk::DeviceCreateInfo<'a>,
                *const ash::vk::AllocationCallbacks<'b>,
                *mut ash::vk::Device,
            ) -> ash::vk::Result,
        >(p),
        None => return vk::Result::ERROR_INITIALIZATION_FAILED,
    };

    let mut chain = (*p_create_info).p_next as *const VkLayerDeviceCreateInfo;
    let mut set_loader_data = None;
    while !chain.is_null() {
        if (*chain).s_type == vk::StructureType::LOADER_DEVICE_CREATE_INFO && (*chain).function == 1
        {
            set_loader_data = Some(std::mem::transmute::<
                *const c_void,
                unsafe extern "system" fn(vk::Device, *mut c_void) -> vk::Result,
            >((*chain).u.pfn_set_device_loader_data));
        }
        chain = (*chain).p_next as *const VkLayerDeviceCreateInfo;
    }
    let res = real_fn(physical_device, p_create_info, p_allocator, p_device);
    if res != vk::Result::SUCCESS {
        return res;
    }

    let device = *p_device;
    eprintln!("[Argus-Layer] Device created: {:?}", device);
    if let Some(callback) = set_loader_data {
        LOADER_DATA.write().unwrap().insert(device, callback);
    }

    // Store the next layer's GDPA for this device so we can resolve device
    // functions without going through our own hooks
    DEVICE_GDPA.write().unwrap().insert(device, next_gdpa);

    // Build ash::Device using the NEXT layer's function table (not ours!)
    let ash_dev = ash::Device::load_with(
        |name| {
            let ptr = next_gdpa(device, name.as_ptr());
            std::mem::transmute(ptr)
        },
        device,
    );
    DEVICE_MAP
        .write()
        .unwrap()
        .insert(device, (physical_device, ash_dev));

    // Record the first queue family requested
    if !p_create_info.is_null() {
        let ci = &*p_create_info;
        if ci.queue_create_info_count > 0 && !ci.p_queue_create_infos.is_null() {
            let qci = &*ci.p_queue_create_infos;
            DEVICE_QUEUE_FAMILY
                .write()
                .unwrap()
                .insert(device, qci.queue_family_index);
        }
    }

    vk::Result::SUCCESS
}

unsafe fn register_graphics_queue(device: vk::Device, queue: vk::Queue, family: u32) {
    if let Some((pd, _)) = DEVICE_MAP.read().unwrap().get(&device) {
        if let Some(instance) = PHYS_TO_INST.read().unwrap().get(pd) {
            if let Some(inst) = ASH_INSTANCES.read().unwrap().get(instance) {
                let graphics = inst
                    .get_physical_device_queue_family_properties(*pd)
                    .get(family as usize)
                    .is_some_and(|p| p.queue_flags.contains(vk::QueueFlags::GRAPHICS));
                GRAPHICS_QUEUES.write().unwrap().insert(queue, graphics);
            }
        }
    }
}

// ---------- vkGetDeviceQueue ----------

/// # Safety
/// The Vulkan loader/caller must supply valid handles, pointer ranges and
/// allocation callbacks for this entry point, and satisfy Vulkan external
/// synchronization requirements. Returned function pointers must be called
/// with the corresponding Vulkan command signature.
#[no_mangle]
pub unsafe extern "system" fn argus_vkGetDeviceQueue(
    device: vk::Device,
    queue_family_index: u32,
    queue_index: u32,
    p_queue: *mut vk::Queue,
) {
    eprintln!(
        "[Argus-Layer] vkGetDeviceQueue called (device={:?}, family={}, idx={})",
        device, queue_family_index, queue_index
    );

    // Call the real vkGetDeviceQueue via raw pointer
    let next = match DEVICE_GDPA.read().unwrap().get(&device).copied() {
        Some(f) => f,
        None => {
            eprintln!("[Argus-Layer] ERROR: No DEVICE_GDPA for device in vkGetDeviceQueue");
            return;
        }
    };
    let ptr = next(device, c"vkGetDeviceQueue".as_ptr() as *const c_char);
    let real_fn: vk::PFN_vkGetDeviceQueue = match ptr {
        Some(p) => std::mem::transmute::<
            unsafe extern "system" fn(),
            unsafe extern "system" fn(ash::vk::Device, u32, u32, *mut ash::vk::Queue),
        >(p),
        None => {
            eprintln!("[Argus-Layer] ERROR: Could not get real vkGetDeviceQueue");
            return;
        }
    };

    real_fn(device, queue_family_index, queue_index, p_queue);
    let queue = *p_queue;

    QUEUE_TO_DEVICE.write().unwrap().insert(queue, device);
    QUEUE_FAMILIES
        .write()
        .unwrap()
        .insert(queue, queue_family_index);
    register_graphics_queue(device, queue, queue_family_index);

    // Capture the real vkQueuePresentKHR for this device
    let present_ptr = next(device, c"vkQueuePresentKHR".as_ptr() as *const c_char);
    if let Some(present_ptr) = present_ptr {
        let real: vk::PFN_vkQueuePresentKHR = std::mem::transmute(present_ptr);
        REAL_QUEUE_PRESENT.write().unwrap().insert(queue, real);
    }

    eprintln!("[Argus-Layer] Queue obtained: {:?}", queue);
}

// ---------- vkCreateSwapchainKHR ----------

/// # Safety
/// The Vulkan loader/caller must supply valid handles, pointer ranges and
/// allocation callbacks for this entry point, and satisfy Vulkan external
/// synchronization requirements. Returned function pointers must be called
/// with the corresponding Vulkan command signature.
#[no_mangle]
pub unsafe extern "system" fn argus_vkCreateSwapchainKHR(
    device: vk::Device,
    p_create_info: *const vk::SwapchainCreateInfoKHR,
    p_allocator: *const vk::AllocationCallbacks,
    p_swapchain: *mut vk::SwapchainKHR,
) -> vk::Result {
    let (physical_device, ash_dev) = {
        let map = DEVICE_MAP.read().unwrap();
        match map.get(&device) {
            Some((pd, dev)) => (*pd, dev.clone()),
            None => return vk::Result::ERROR_INITIALIZATION_FAILED,
        }
    };

    let instance = match PHYS_TO_INST.read().unwrap().get(&physical_device).copied() {
        Some(i) => i,
        None => return vk::Result::ERROR_INITIALIZATION_FAILED,
    };

    // Call real vkCreateSwapchainKHR via raw pointer
    let real_fn: vk::PFN_vkCreateSwapchainKHR = {
        match DEVICE_GDPA.read().unwrap().get(&device).copied() {
            Some(next) => {
                let ptr = next(device, c"vkCreateSwapchainKHR".as_ptr() as *const c_char);
                match ptr {
                    Some(p) => std::mem::transmute::<
                        unsafe extern "system" fn(),
                        for<'a, 'b> unsafe extern "system" fn(
                            ash::vk::Device,
                            *const ash::vk::SwapchainCreateInfoKHR<'a>,
                            *const ash::vk::AllocationCallbacks<'b>,
                            *mut ash::vk::SwapchainKHR,
                        )
                            -> ash::vk::Result,
                    >(p),
                    None => return vk::Result::ERROR_INITIALIZATION_FAILED,
                }
            }
            None => return vk::Result::ERROR_INITIALIZATION_FAILED,
        }
    };

    let res = real_fn(device, p_create_info, p_allocator, p_swapchain);
    if res != vk::Result::SUCCESS {
        return res;
    }

    let swapchain = *p_swapchain;
    let ci = &*p_create_info;
    let format = ci.image_format;
    let extent = ci.image_extent;

    eprintln!(
        "[Argus-Layer] Swapchain created: {:?} ({}x{}, format {:?})",
        swapchain, extent.width, extent.height, format
    );

    if *PASSTHROUGH {
        return vk::Result::SUCCESS;
    }
    if !ci
        .image_usage
        .contains(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        || ci.image_array_layers != 1
        || ci.flags.contains(vk::SwapchainCreateFlagsKHR::PROTECTED)
    {
        eprintln!("[Argus-Layer] HUD skipped: unsupported swapchain usage/layers/protection");
        return vk::Result::SUCCESS;
    }
    // Get swapchain images
    let ash_instances = ASH_INSTANCES.read().unwrap();
    let ash_inst = match ash_instances.get(&instance) {
        Some(i) => i,
        None => return vk::Result::SUCCESS,
    };
    let swapchain_fn = ash::khr::swapchain::Device::new(ash_inst, &ash_dev);
    let images = match swapchain_fn.get_swapchain_images(swapchain) {
        Ok(imgs) => imgs,
        Err(_) => return vk::Result::SUCCESS,
    };

    // Get queue family for command pool
    let qf = DEVICE_QUEUE_FAMILY
        .read()
        .unwrap()
        .get(&device)
        .copied()
        .unwrap_or(0);

    // Create overlay state
    match renderer::OverlayState::new(
        ash_inst,
        physical_device,
        &ash_dev,
        qf,
        &images,
        format,
        extent,
        ci.pre_transform,
    ) {
        Some(mut state) => {
            state.swapchain = swapchain;
            OVERLAY_STATES.lock().unwrap().insert(swapchain, state);
            eprintln!(
                "[Argus-Layer] Overlay initialised for swapchain ({} images)",
                images.len()
            );
        }
        None => {
            eprintln!("[Argus-Layer] WARNING: Could not create overlay resources");
        }
    }

    vk::Result::SUCCESS
}

// ---------- vkQueuePresentKHR (the main drawing hook) ----------

/// # Safety
/// The Vulkan loader/caller must supply valid handles, pointer ranges and
/// allocation callbacks for this entry point, and satisfy Vulkan external
/// synchronization requirements. Returned function pointers must be called
/// with the corresponding Vulkan command signature.
#[no_mangle]
pub unsafe extern "system" fn argus_vkQueuePresentKHR(
    queue: vk::Queue,
    info: *const vk::PresentInfoKHR,
) -> vk::Result {
    let ticket = if *ACTIVE { capture::ticket() } else { 0 };
    let begin = (ticket != 0).then(Instant::now);
    let result = present_impl(queue, info);
    if let Some(at) = begin {
        use ash::vk::Handle;
        if !info.is_null() && !(*info).p_swapchains.is_null() {
            for (i, swapchain) in
                std::slice::from_raw_parts((*info).p_swapchains, (*info).swapchain_count as usize)
                    .iter()
                    .enumerate()
            {
                let per_swapchain = if (*info).p_results.is_null() {
                    result
                } else {
                    *(*info).p_results.add(i)
                };
                capture::record(ticket, at, swapchain.as_raw(), per_swapchain.as_raw());
            }
        }
    }
    result
}
unsafe fn present_impl(queue: vk::Queue, p_present_info: *const vk::PresentInfoKHR) -> vk::Result {
    let Some(real) = REAL_QUEUE_PRESENT.read().unwrap().get(&queue).copied() else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    if *PASSTHROUGH || p_present_info.is_null() {
        return real(queue, p_present_info);
    }
    let pi = &*p_present_info;
    // Multiple swapchains require a combined submission and semaphore lifetime
    // scheme. Preserve presentation unchanged for this untested route.
    if pi.swapchain_count != 1 {
        return real(queue, p_present_info);
    }
    let Some(device) = QUEUE_TO_DEVICE.read().unwrap().get(&queue).copied() else {
        return real(queue, p_present_info);
    };
    let device_map = DEVICE_MAP.read().unwrap();
    let Some((_, dev)) = device_map.get(&device) else {
        return real(queue, p_present_info);
    };
    let mut states = OVERLAY_STATES.lock().unwrap();
    let Some(state) = states.get_mut(&*pi.p_swapchains) else {
        return real(queue, p_present_info);
    };
    state.stats.record(Instant::now());
    let family = QUEUE_FAMILIES.read().unwrap().get(&queue).copied();
    if family != Some(state.queue_family) {
        return real(queue, p_present_info);
    }
    if !GRAPHICS_QUEUES
        .read()
        .unwrap()
        .get(&queue)
        .copied()
        .unwrap_or(false)
    {
        return real(queue, p_present_info);
    }
    if state.draw_queue.is_some_and(|previous| previous != queue) {
        return real(queue, p_present_info);
    }
    state.draw_queue = Some(queue);
    let index = *pi.p_image_indices as usize;
    let config = OVERLAY_CONFIG.read().unwrap().clone();
    let tel = TELEMETRY.read().unwrap();
    let status = tel.status();
    let display_status = if capture::enabled() {
        format!("REC · frame capture  {}", status)
    } else {
        status.into()
    };
    let cb = state.record_overlay(
        dev,
        index,
        if status.is_empty() {
            tel.frame.as_ref()
        } else {
            None
        },
        &display_status,
        &config,
    );
    drop(tel);
    if let Some(cb) = cb {
        let waits = if pi.wait_semaphore_count == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(pi.p_wait_semaphores, pi.wait_semaphore_count as usize)
        };
        let stages = vec![vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT; waits.len()];
        let signal = [state.complete[index]];
        let buffers = [cb];
        let submit = vk::SubmitInfo::default()
            .wait_semaphores(waits)
            .wait_dst_stage_mask(&stages)
            .command_buffers(&buffers)
            .signal_semaphores(&signal);
        if let Err(e) = dev.reset_fences(&[state.fences[index]]) {
            state.disabled = true;
            eprintln!("[Argus-Layer] reset fence failed: {e:?}");
            return real(queue, p_present_info);
        }
        match dev.queue_submit(queue, &[submit], state.fences[index]) {
            Ok(()) => {
                let mut present = *pi;
                present.wait_semaphore_count = 1;
                present.p_wait_semaphores = signal.as_ptr();
                drop(states);
                drop(device_map);
                return real(queue, &present);
            }
            Err(e) => {
                state.disabled = true;
                eprintln!("[Argus-Layer] overlay submit failed: {e:?}");
                return e;
            }
        }
    }
    drop(states);
    drop(device_map);
    real(queue, p_present_info)
}

/// # Safety
/// The Vulkan loader/caller must supply valid handles, pointer ranges and
/// allocation callbacks for this entry point, and satisfy Vulkan external
/// synchronization requirements. Returned function pointers must be called
/// with the corresponding Vulkan command signature.
#[no_mangle]
pub unsafe extern "system" fn argus_vkGetDeviceQueue2(
    device: vk::Device,
    info: *const vk::DeviceQueueInfo2,
    queue: *mut vk::Queue,
) {
    let map = DEVICE_MAP.read().unwrap();
    let Some((_, dev)) = map.get(&device) else {
        return;
    };
    *queue = dev.get_device_queue2(&*info);
    QUEUE_TO_DEVICE.write().unwrap().insert(*queue, device);
    QUEUE_FAMILIES
        .write()
        .unwrap()
        .insert(*queue, (*info).queue_family_index);
    drop(map);
    register_graphics_queue(device, *queue, (*info).queue_family_index);
    if let Some(next) = DEVICE_GDPA.read().unwrap().get(&device).copied() {
        if let Some(ptr) = next(device, c"vkQueuePresentKHR".as_ptr()) {
            REAL_QUEUE_PRESENT.write().unwrap().insert(
                *queue,
                std::mem::transmute::<
                    unsafe extern "system" fn(),
                    for<'a> unsafe extern "system" fn(
                        ash::vk::Queue,
                        *const ash::vk::PresentInfoKHR<'a>,
                    ) -> ash::vk::Result,
                >(ptr),
            );
        }
    }
}
/// # Safety
/// The Vulkan loader/caller must supply valid handles, pointer ranges and
/// allocation callbacks for this entry point, and satisfy Vulkan external
/// synchronization requirements. Returned function pointers must be called
/// with the corresponding Vulkan command signature.
#[no_mangle]
pub unsafe extern "system" fn argus_vkDestroySwapchainKHR(
    device: vk::Device,
    swapchain: vk::SwapchainKHR,
    allocator: *const vk::AllocationCallbacks,
) {
    use ash::vk::Handle;
    capture::end(swapchain.as_raw());
    if let Some(state) = OVERLAY_STATES.lock().unwrap().remove(&swapchain) {
        if let Some((_, dev)) = DEVICE_MAP.read().unwrap().get(&device) {
            state.destroy(dev);
        }
    }
    if let Some(next) = DEVICE_GDPA.read().unwrap().get(&device).copied() {
        if let Some(ptr) = next(device, c"vkDestroySwapchainKHR".as_ptr()) {
            let real: vk::PFN_vkDestroySwapchainKHR = std::mem::transmute(ptr);
            real(device, swapchain, allocator);
        }
    }
}

// ── Dispatch: vkGetInstanceProcAddr ─────────────────────────────────────────

/// # Safety
/// The Vulkan loader/caller must supply valid handles, pointer ranges and
/// allocation callbacks for this entry point, and satisfy Vulkan external
/// synchronization requirements. Returned function pointers must be called
/// with the corresponding Vulkan command signature.
#[no_mangle]
pub unsafe extern "system" fn vkGetInstanceProcAddr(
    instance: vk::Instance,
    p_name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if p_name.is_null() {
        return None;
    }
    let name = CStr::from_ptr(p_name);

    match name.to_bytes() {
        b"vkGetInstanceProcAddr" => Some(std::mem::transmute::<
            *const (),
            unsafe extern "system" fn(),
        >(vkGetInstanceProcAddr as *const ())),
        b"vkGetDeviceProcAddr" => Some(
            std::mem::transmute::<*const (), unsafe extern "system" fn()>(
                vkGetDeviceProcAddr as *const (),
            ),
        ),
        b"vkGetDeviceQueue2" => Some(
            std::mem::transmute::<*const (), unsafe extern "system" fn()>(
                argus_vkGetDeviceQueue2 as *const (),
            ),
        ),
        b"vkDestroySwapchainKHR" => Some(std::mem::transmute::<
            *const (),
            unsafe extern "system" fn(),
        >(argus_vkDestroySwapchainKHR as *const ())),
        b"vkCreateInstance" => Some(
            std::mem::transmute::<*const (), unsafe extern "system" fn()>(
                argus_vkCreateInstance as *const (),
            ),
        ),
        b"vkEnumeratePhysicalDevices" => Some(std::mem::transmute::<
            *const (),
            unsafe extern "system" fn(),
        >(
            argus_vkEnumeratePhysicalDevices as *const ()
        )),
        b"vkGetDeviceQueue" => Some(
            std::mem::transmute::<*const (), unsafe extern "system" fn()>(
                argus_vkGetDeviceQueue as *const (),
            ),
        ),
        b"vkCreateSwapchainKHR" => Some(std::mem::transmute::<
            *const (),
            unsafe extern "system" fn(),
        >(argus_vkCreateSwapchainKHR as *const ())),
        b"vkQueuePresentKHR" => Some(
            std::mem::transmute::<*const (), unsafe extern "system" fn()>(
                argus_vkQueuePresentKHR as *const (),
            ),
        ),
        b"vkCreateDevice" => Some(
            std::mem::transmute::<*const (), unsafe extern "system" fn()>(
                argus_vkCreateDevice as *const (),
            ),
        ),
        _ => {
            if let Some(next) = NEXT_GET_INSTANCE_PROC_ADDR {
                next(instance, p_name)
            } else {
                None
            }
        }
    }
}

// ── Dispatch: vkGetDeviceProcAddr ───────────────────────────────────────────

/// # Safety
/// The Vulkan loader/caller must supply valid handles, pointer ranges and
/// allocation callbacks for this entry point, and satisfy Vulkan external
/// synchronization requirements. Returned function pointers must be called
/// with the corresponding Vulkan command signature.
#[no_mangle]
pub unsafe extern "system" fn vkGetDeviceProcAddr(
    device: vk::Device,
    p_name: *const c_char,
) -> vk::PFN_vkVoidFunction {
    if p_name.is_null() {
        return None;
    }
    let name = CStr::from_ptr(p_name);

    match name.to_bytes() {
        b"vkGetDeviceProcAddr" => Some(
            std::mem::transmute::<*const (), unsafe extern "system" fn()>(
                vkGetDeviceProcAddr as *const (),
            ),
        ),
        b"vkGetDeviceQueue2" => Some(
            std::mem::transmute::<*const (), unsafe extern "system" fn()>(
                argus_vkGetDeviceQueue2 as *const (),
            ),
        ),
        b"vkDestroySwapchainKHR" => Some(std::mem::transmute::<
            *const (),
            unsafe extern "system" fn(),
        >(argus_vkDestroySwapchainKHR as *const ())),
        b"vkGetDeviceQueue" => Some(
            std::mem::transmute::<*const (), unsafe extern "system" fn()>(
                argus_vkGetDeviceQueue as *const (),
            ),
        ),
        b"vkCreateSwapchainKHR" => Some(std::mem::transmute::<
            *const (),
            unsafe extern "system" fn(),
        >(argus_vkCreateSwapchainKHR as *const ())),
        b"vkQueuePresentKHR" => Some(
            std::mem::transmute::<*const (), unsafe extern "system" fn()>(
                argus_vkQueuePresentKHR as *const (),
            ),
        ),
        _ => {
            let next_gdpa = DEVICE_GDPA.read().unwrap().get(&device).copied();
            if let Some(next) = next_gdpa {
                next(device, p_name)
            } else if let Some(next) = NEXT_GET_DEVICE_PROC_ADDR {
                next(device, p_name)
            } else {
                None
            }
        }
    }
}

// ── Layer negotiation ───────────────────────────────────────────────────────

#[repr(C)]
pub struct VkLayerNegotiateStruct {
    pub s_type: u32,
    pub p_next: *const c_void,
    pub loader_layer_interface_version: u32,
    pub pfn_get_instance_proc_addr: vk::PFN_vkGetInstanceProcAddr,
    pub pfn_get_device_proc_addr: vk::PFN_vkGetDeviceProcAddr,
    pub pfn_get_physical_device_proc_addr: *const c_void,
}

/// # Safety
/// The Vulkan loader/caller must supply valid handles, pointer ranges and
/// allocation callbacks for this entry point, and satisfy Vulkan external
/// synchronization requirements. Returned function pointers must be called
/// with the corresponding Vulkan command signature.
#[no_mangle]
pub unsafe extern "system" fn vkNegotiateLoaderLayerInterfaceVersion(
    p_version_struct: *mut VkLayerNegotiateStruct,
) -> vk::Result {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        eprintln!(
            "[Argus-Layer] build={} protocol={} draw={}",
            argus_ipc::BUILD_ID,
            argus_ipc::PROTOCOL_VERSION,
            !*PASSTHROUGH
        );
        if *ACTIVE {
            start_ipc_thread();
        }
    });

    if p_version_struct.is_null() {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let vs = &mut *p_version_struct;
    if vs.loader_layer_interface_version < 2 {
        return vk::Result::ERROR_INITIALIZATION_FAILED;
    }

    NEXT_GET_INSTANCE_PROC_ADDR = Some(vs.pfn_get_instance_proc_addr);
    NEXT_GET_DEVICE_PROC_ADDR = Some(vs.pfn_get_device_proc_addr);

    vs.loader_layer_interface_version = 2;
    vs.pfn_get_instance_proc_addr = vkGetInstanceProcAddr;
    vs.pfn_get_device_proc_addr = vkGetDeviceProcAddr;

    vk::Result::SUCCESS
}
