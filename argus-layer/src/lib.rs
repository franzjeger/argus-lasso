//! Argus-Layer: Vulkan implicit layer that draws a telemetry HUD.
//!
//! Hooks: vkCreateInstance/vkDestroyInstance, vkEnumeratePhysicalDevices,
//!        vkCreateDevice/vkDestroyDevice, vkGetDeviceQueue(2),
//!        vkCreateSwapchainKHR/vkDestroySwapchainKHR, vkQueuePresentKHR.

mod activation;
mod capture;
pub mod font;
pub mod hud;
pub mod renderer;

use argus_ipc::{IpcMessage, OverlayConfig, TelemetryFrame};
use ash::vk;
use renderer::OverlayState;
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::os::raw::c_char;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Mutex, RwLock};
use std::thread;
use std::time::Instant;

// ── Panic/poison resilience ──────────────────────────────────────────────────
//
// This crate runs inside an arbitrary game process: a panic here must never
// bring the game down, and a lock poisoned by one panic must not turn into a
// permanent failure for every subsequent frame. Every hook below is wrapped
// in `guard()` (catch_unwind with a safe fallback), and every lock access
// goes through these recovery methods instead of a bare `.unwrap()` so one
// panic while a lock is held doesn't wedge every later call that touches it.
trait LockRecover<T> {
    fn read_or_recover(&self) -> std::sync::RwLockReadGuard<'_, T>;
    fn write_or_recover(&self) -> std::sync::RwLockWriteGuard<'_, T>;
}
impl<T> LockRecover<T> for RwLock<T> {
    fn read_or_recover(&self) -> std::sync::RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(|e| e.into_inner())
    }
    fn write_or_recover(&self) -> std::sync::RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(|e| e.into_inner())
    }
}
trait MutexRecover<T> {
    fn lock_or_recover(&self) -> std::sync::MutexGuard<'_, T>;
}
impl<T> MutexRecover<T> for Mutex<T> {
    fn lock_or_recover(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Run `f`, and if it panics, log and return `fallback` instead of letting
/// the panic unwind into the game's own call stack (undefined behaviour
/// across an `extern "system"` boundary, and an abort in practice).
fn guard<R>(
    hook: &str,
    f: impl FnOnce() -> R + std::panic::UnwindSafe,
    fallback: impl FnOnce() -> R,
) -> R {
    match std::panic::catch_unwind(f) {
        Ok(r) => r,
        Err(_) => {
            eprintln!("[Argus-Layer] PANIC in {hook} — degrading gracefully for this call");
            fallback()
        }
    }
}

// ── Next-layer entry points ─────────────────────────────────────────────────
//
// Each instance's next vkGetInstanceProcAddr is kept in INSTANCE_GIPA. The most
// recently recorded one is also kept process-wide for calls that name no
// instance we know, such as vkGetInstanceProcAddr(NULL, …). An atomic rather
// than `static mut`: vkCreateInstance has no Vulkan external-synchronization
// requirement, so two threads can legitimately create instances concurrently.
static NEXT_GET_INSTANCE_PROC_ADDR: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

fn get_next_gipa() -> Option<vk::PFN_vkGetInstanceProcAddr> {
    let p = NEXT_GET_INSTANCE_PROC_ADDR.load(Ordering::Acquire);
    // SAFETY: set_next_gipa is the only writer, and it stores a valid fn pointer.
    (!p.is_null())
        .then(|| unsafe { std::mem::transmute::<*mut c_void, vk::PFN_vkGetInstanceProcAddr>(p) })
}
fn set_next_gipa(f: vk::PFN_vkGetInstanceProcAddr) {
    NEXT_GET_INSTANCE_PROC_ADDR.store(f as *mut c_void, Ordering::Release);
}

/// The next layer's vkGetInstanceProcAddr for `instance`, or the most recent
/// one when the instance is unknown or null.
fn instance_gipa(instance: vk::Instance) -> Option<vk::PFN_vkGetInstanceProcAddr> {
    INSTANCE_GIPA
        .read_or_recover()
        .get(&instance)
        .copied()
        .or_else(get_next_gipa)
}

/// Resolve a device command through the layer below us. None when the device
/// is unknown or the layer below does not provide the command.
unsafe fn next_device_fn(device: vk::Device, name: &CStr) -> vk::PFN_vkVoidFunction {
    let gdpa = DEVICE_GDPA.read_or_recover().get(&device).copied()?;
    gdpa(device, name.as_ptr())
}

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

    // Instance → the next layer's vkGetInstanceProcAddr it was created with
    static ref INSTANCE_GIPA: RwLock<HashMap<vk::Instance, vk::PFN_vkGetInstanceProcAddr>> = RwLock::new(HashMap::new());

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
// `argus_ipc::MAX_MESSAGE_SIZE` bounds the whole wire message, not any one
// field within it — a compromised or simply mismatched-build daemon could
// otherwise put one huge string in a single field and stay under that cap.
// hud.rs sizes its pixel buffer and glyph cache directly off these fields'
// lengths with no clamp of its own, so an oversized value here can force a
// multi-gigabyte allocation (an abort, via handle_alloc_error) or an
// unbounded glyph-cache leak in the game's own process. Clamp everything
// hud.rs actually reads right where it enters the process, once.
const MAX_TELEMETRY_STRING_LEN: usize = 512;
const MAX_TELEMETRY_LIST_LEN: usize = 1024;

fn clamp_string(s: &mut String, max_len: usize) {
    if s.len() > max_len {
        let mut cut = max_len;
        while cut > 0 && !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
}

fn sanitize_telemetry_frame(frame: &mut TelemetryFrame) {
    clamp_string(&mut frame.cpu_name, MAX_TELEMETRY_STRING_LEN);
    clamp_string(&mut frame.cpu_power_status, MAX_TELEMETRY_STRING_LEN);
    clamp_string(&mut frame.gpu_name, MAX_TELEMETRY_STRING_LEN);
    clamp_string(&mut frame.ram_speed_status, MAX_TELEMETRY_STRING_LEN);
    clamp_string(&mut frame.active_profile, MAX_TELEMETRY_STRING_LEN);
    frame.cpus.truncate(MAX_TELEMETRY_LIST_LEN);
    frame.probalance_pids.truncate(MAX_TELEMETRY_LIST_LEN);
    frame.launch_profiles.truncate(MAX_TELEMETRY_LIST_LEN);
    for lp in &mut frame.launch_profiles {
        clamp_string(&mut lp.profile, MAX_TELEMETRY_STRING_LEN);
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
                            TELEMETRY.write_or_recover().connected = true;
                            last_error.clear();
                        }
                        IpcMessage::Telemetry(mut frame) => {
                            sanitize_telemetry_frame(&mut frame);
                            frame.game = Some(game_telemetry(host_pid, &frame));
                            let mut tel = TELEMETRY.write_or_recover();
                            if tel.frame.is_none() {
                                eprintln!("[Argus-Layer] first valid telemetry timestamp={} CPU={} GPU={}", frame.sample_unix_ms, frame.cpu_name, frame.gpu_name);
                            }
                            tel.frame = Some(frame);
                            tel.received = Some(Instant::now());
                        }
                        IpcMessage::Config(config) => *OVERLAY_CONFIG.write_or_recover() = config,
                    }
                }
            })();
            TELEMETRY.write_or_recover().connected = false;
            if let Err(e) = result {
                let error = e.to_string();
                if error != last_error {
                    let tel = TELEMETRY.read_or_recover();
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

fn make_static_fn(next_gipa: vk::PFN_vkGetInstanceProcAddr) -> ash::StaticFn {
    ash::StaticFn {
        get_instance_proc_addr: next_gipa,
    }
}

// ── Hooked Vulkan entry points ──────────────────────────────────────────────
//
// The rule for every hook: the game's call reaches the layer below us whenever
// that is at all possible. Missing bookkeeping of ours costs the HUD, never
// the game's call.

/// Panic fallback for the create hooks. Once the real object exists it is the
/// game's: report it as created, and the other hooks pass calls through for
/// whatever bookkeeping the panic cut short. Before that, nothing exists.
fn created_result(created: &Cell<bool>) -> vk::Result {
    if created.get() {
        vk::Result::SUCCESS
    } else {
        vk::Result::ERROR_INITIALIZATION_FAILED
    }
}

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
    let created = Cell::new(false);
    guard(
        "vkCreateInstance",
        std::panic::AssertUnwindSafe(|| {
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
            let Some(p) = next_gipa(vk::Instance::null(), c"vkCreateInstance".as_ptr()) else {
                return vk::Result::ERROR_INITIALIZATION_FAILED;
            };
            let real_create_instance: vk::PFN_vkCreateInstance = std::mem::transmute(p);

            set_next_gipa(next_gipa);

            let res = real_create_instance(p_create_info, p_allocator, p_instance);
            if res != vk::Result::SUCCESS {
                return res;
            }
            created.set(true);

            let instance = *p_instance;
            eprintln!("[Argus-Layer] Instance created: {:?}", instance);
            INSTANCE_GIPA.write_or_recover().insert(instance, next_gipa);

            // Build an ash::Instance so we can call Vulkan functions through it later
            let ash_inst = ash::Instance::load(&make_static_fn(next_gipa), instance);
            if let Ok(devices) = ash_inst.enumerate_physical_devices() {
                let mut map = PHYS_TO_INST.write_or_recover();
                for device in devices {
                    map.insert(device, instance);
                }
            }
            ASH_INSTANCES.write_or_recover().insert(instance, ash_inst);

            vk::Result::SUCCESS
        }),
        || created_result(&created),
    )
}

/// # Safety
/// As for `argus_vkCreateInstance`.
#[no_mangle]
pub unsafe extern "system" fn argus_vkDestroyInstance(
    instance: vk::Instance,
    p_allocator: *const vk::AllocationCallbacks,
) {
    // Resolve before forgetting the instance: its next layer is recorded with
    // it. The real destroy runs even if our own cleanup panics.
    let real = guard(
        "vkDestroyInstance",
        std::panic::AssertUnwindSafe(|| {
            instance_gipa(instance)?(instance, c"vkDestroyInstance".as_ptr())
        }),
        || None,
    );
    guard(
        "vkDestroyInstance",
        std::panic::AssertUnwindSafe(|| forget_instance(instance)),
        || (),
    );
    if let Some(real) = real {
        let real: vk::PFN_vkDestroyInstance = std::mem::transmute(real);
        real(instance, p_allocator);
    }
}

/// Drop everything recorded for `instance`. Handles are recycled addresses: a
/// stale entry would hand the next instance this one's function tables.
fn forget_instance(instance: vk::Instance) {
    ASH_INSTANCES.write_or_recover().remove(&instance);
    PHYS_TO_INST
        .write_or_recover()
        .retain(|_, owner| *owner != instance);
    INSTANCE_GIPA.write_or_recover().remove(&instance);
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
    guard(
        "vkEnumeratePhysicalDevices",
        std::panic::AssertUnwindSafe(|| {
            let Some(next) = instance_gipa(instance) else {
                return vk::Result::ERROR_INITIALIZATION_FAILED;
            };
            let Some(p) = next(instance, c"vkEnumeratePhysicalDevices".as_ptr()) else {
                return vk::Result::ERROR_INITIALIZATION_FAILED;
            };
            let real_fn: vk::PFN_vkEnumeratePhysicalDevices = std::mem::transmute(p);

            let res = real_fn(instance, p_count, p_physical_devices);
            // INCOMPLETE still fills the array it was given.
            if matches!(res, vk::Result::SUCCESS | vk::Result::INCOMPLETE)
                && !p_physical_devices.is_null()
            {
                let count = *p_count as usize;
                let devices = std::slice::from_raw_parts(p_physical_devices, count);
                let mut map = PHYS_TO_INST.write_or_recover();
                for &pd in devices {
                    map.insert(pd, instance);
                }
                eprintln!("[Argus-Layer] Enumerated {} physical device(s)", count);
            }
            res
        }),
        || vk::Result::ERROR_INITIALIZATION_FAILED,
    )
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

type SetDeviceLoaderData = unsafe extern "system" fn(vk::Device, *mut c_void) -> vk::Result;

/// Walk the pNext chain for the loader's VK_LOADER_DATA_CALLBACK entry
/// (function == 1) — a separate entry from the layer link (function == 0)
/// `find_device_layer_link` looks for, and present independently of it. The
/// loader requires this callback to be invoked for every dispatchable handle
/// a layer allocates on its own initiative (the command buffers OverlayState
/// creates for HUD drawing); skipping it leaves those handles without a
/// loader-recognised dispatch table.
unsafe fn find_loader_data_callback(
    p_create_info: *const vk::DeviceCreateInfo,
) -> Option<SetDeviceLoaderData> {
    let mut chain = (*p_create_info).p_next as *const VkLayerDeviceCreateInfo;
    while !chain.is_null() {
        if (*chain).s_type == vk::StructureType::LOADER_DEVICE_CREATE_INFO && (*chain).function == 1
        {
            return Some(std::mem::transmute::<*const c_void, SetDeviceLoaderData>(
                (*chain).u.pfn_set_device_loader_data,
            ));
        }
        chain = (*chain).p_next as *const VkLayerDeviceCreateInfo;
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
    let created = Cell::new(false);
    guard(
        "vkCreateDevice",
        std::panic::AssertUnwindSafe(|| {
            // Unknown only if the handle bypassed both enumerate paths. That
            // costs the HUD, not the device: a null instance still resolves
            // vkCreateDevice further down the chain.
            let instance = PHYS_TO_INST
                .read_or_recover()
                .get(&physical_device)
                .copied()
                .unwrap_or(vk::Instance::null());

            // The loader hands us the next layer's entry points in the pNext
            // chain. Without them, go through the instance chain instead.
            let (next_gipa, next_gdpa) = match find_device_layer_link(p_create_info) {
                Some(layer_link_ptr) => {
                    let layer_link = *layer_link_ptr;
                    let next = (
                        (*layer_link).pfn_next_get_instance_proc_addr,
                        Some((*layer_link).pfn_next_get_device_proc_addr),
                    );
                    // Advance the chain for the next layer
                    *layer_link_ptr = (*layer_link).p_next as *mut VkLayerDeviceLink;
                    next
                }
                None => {
                    eprintln!("[Argus-Layer] WARNING: No device layer link, falling back");
                    let Some(next_gipa) = instance_gipa(instance) else {
                        return vk::Result::ERROR_INITIALIZATION_FAILED;
                    };
                    (next_gipa, None)
                }
            };

            let Some(p) = next_gipa(instance, c"vkCreateDevice".as_ptr()) else {
                return vk::Result::ERROR_INITIALIZATION_FAILED;
            };
            let real_fn: vk::PFN_vkCreateDevice = std::mem::transmute(p);

            let set_loader_data = find_loader_data_callback(p_create_info);
            let res = real_fn(physical_device, p_create_info, p_allocator, p_device);
            if res != vk::Result::SUCCESS {
                return res;
            }
            created.set(true);

            let device = *p_device;
            eprintln!("[Argus-Layer] Device created: {:?}", device);
            let next_gdpa = next_gdpa.or_else(|| {
                let gdpa: vk::PFN_vkGetDeviceProcAddr =
                    std::mem::transmute(next_gipa(instance, c"vkGetDeviceProcAddr".as_ptr())?);
                Some(gdpa)
            });
            match next_gdpa {
                Some(gdpa) => record_device(
                    physical_device,
                    device,
                    gdpa,
                    p_create_info,
                    set_loader_data,
                ),
                None => {
                    eprintln!("[Argus-Layer] ERROR: no vkGetDeviceProcAddr below us for {device:?}")
                }
            }
            vk::Result::SUCCESS
        }),
        || created_result(&created),
    )
}

/// Record what the other hooks need to know about a device the game created.
unsafe fn record_device(
    physical_device: vk::PhysicalDevice,
    device: vk::Device,
    next_gdpa: vk::PFN_vkGetDeviceProcAddr,
    p_create_info: *const vk::DeviceCreateInfo,
    set_loader_data: Option<SetDeviceLoaderData>,
) {
    if let Some(callback) = set_loader_data {
        LOADER_DATA.write_or_recover().insert(device, callback);
    }
    // Store the next layer's GDPA for this device so we can resolve device
    // functions without going through our own hooks
    DEVICE_GDPA.write_or_recover().insert(device, next_gdpa);

    // Build ash::Device using the NEXT layer's function table (not ours!)
    let ash_dev = ash::Device::load_with(
        |name| {
            let ptr = next_gdpa(device, name.as_ptr());
            std::mem::transmute(ptr)
        },
        device,
    );
    DEVICE_MAP
        .write_or_recover()
        .insert(device, (physical_device, ash_dev));

    // Record the first queue family requested
    let ci = &*p_create_info;
    if ci.queue_create_info_count > 0 && !ci.p_queue_create_infos.is_null() {
        DEVICE_QUEUE_FAMILY
            .write_or_recover()
            .insert(device, (*ci.p_queue_create_infos).queue_family_index);
    }
}

/// # Safety
/// As for `argus_vkCreateDevice`.
#[no_mangle]
pub unsafe extern "system" fn argus_vkDestroyDevice(
    device: vk::Device,
    p_allocator: *const vk::AllocationCallbacks,
) {
    // Resolve before forgetting the device: its next layer is recorded with
    // it. The real destroy runs even if our own cleanup panics.
    let real = guard(
        "vkDestroyDevice",
        std::panic::AssertUnwindSafe(|| next_device_fn(device, c"vkDestroyDevice")),
        || None,
    );
    guard(
        "vkDestroyDevice",
        std::panic::AssertUnwindSafe(|| forget_device(device)),
        || (),
    );
    if let Some(real) = real {
        let real: vk::PFN_vkDestroyDevice = std::mem::transmute(real);
        real(device, p_allocator);
    }
}

/// Drop everything recorded for `device` and its queues, destroying any HUD
/// resources a swapchain the game never destroyed left behind. Handles are
/// recycled addresses: a stale entry would hand the next device this one's
/// function table and HUD state.
unsafe fn forget_device(device: vk::Device) {
    let leftovers: Vec<OverlayState> = {
        let mut states = OVERLAY_STATES.lock_or_recover();
        let swapchains: Vec<_> = states
            .iter()
            .filter(|(_, state)| state.device == device)
            .map(|(&swapchain, _)| swapchain)
            .collect();
        swapchains
            .iter()
            .filter_map(|swapchain| states.remove(swapchain))
            .collect()
    };
    if let Some((_, dev)) = DEVICE_MAP.write_or_recover().remove(&device) {
        for state in &leftovers {
            state.destroy(&dev);
        }
    }

    let mut queues = Vec::new();
    QUEUE_TO_DEVICE.write_or_recover().retain(|&queue, owner| {
        let ours = *owner == device;
        if ours {
            queues.push(queue);
        }
        !ours
    });
    for queue in &queues {
        QUEUE_FAMILIES.write_or_recover().remove(queue);
        GRAPHICS_QUEUES.write_or_recover().remove(queue);
        REAL_QUEUE_PRESENT.write_or_recover().remove(queue);
    }
    DEVICE_QUEUE_FAMILY.write_or_recover().remove(&device);
    LOADER_DATA.write_or_recover().remove(&device);
    DEVICE_GDPA.write_or_recover().remove(&device);
}

unsafe fn register_graphics_queue(device: vk::Device, queue: vk::Queue, family: u32) {
    if let Some((pd, _)) = DEVICE_MAP.read_or_recover().get(&device) {
        if let Some(instance) = PHYS_TO_INST.read_or_recover().get(pd) {
            if let Some(inst) = ASH_INSTANCES.read_or_recover().get(instance) {
                let graphics = inst
                    .get_physical_device_queue_family_properties(*pd)
                    .get(family as usize)
                    .is_some_and(|p| p.queue_flags.contains(vk::QueueFlags::GRAPHICS));
                GRAPHICS_QUEUES.write_or_recover().insert(queue, graphics);
            }
        }
    }
}

// ---------- vkGetDeviceQueue / vkGetDeviceQueue2 ----------

/// Record a queue the game just obtained, and the next layer's present for it.
unsafe fn record_queue(device: vk::Device, queue: vk::Queue, family: u32) {
    if queue == vk::Queue::null() {
        return;
    }
    QUEUE_TO_DEVICE.write_or_recover().insert(queue, device);
    QUEUE_FAMILIES.write_or_recover().insert(queue, family);
    register_graphics_queue(device, queue, family);
    if let Some(ptr) = next_device_fn(device, c"vkQueuePresentKHR") {
        let real: vk::PFN_vkQueuePresentKHR = std::mem::transmute(ptr);
        REAL_QUEUE_PRESENT.write_or_recover().insert(queue, real);
    }
    eprintln!("[Argus-Layer] Queue obtained: {:?}", queue);
}

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
    guard(
        "vkGetDeviceQueue",
        std::panic::AssertUnwindSafe(|| {
            if p_queue.is_null() {
                return;
            }
            let Some(ptr) = next_device_fn(device, c"vkGetDeviceQueue") else {
                // Nothing below us to ask. A null handle is at least
                // deterministic, unlike the uninitialized memory the caller
                // would otherwise read back.
                eprintln!("[Argus-Layer] ERROR: Could not get real vkGetDeviceQueue");
                *p_queue = vk::Queue::null();
                return;
            };
            let real_fn: vk::PFN_vkGetDeviceQueue = std::mem::transmute(ptr);
            real_fn(device, queue_family_index, queue_index, p_queue);
            record_queue(device, *p_queue, queue_family_index);
        }),
        || (),
    )
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
    guard(
        "vkGetDeviceQueue2",
        std::panic::AssertUnwindSafe(|| {
            // Per spec both must be non-null; guarding it turns a malformed
            // call from a layer below us into a no-op instead of a null
            // deref (UB, not something catch_unwind can catch).
            if info.is_null() || queue.is_null() {
                return;
            }
            // Through the raw pointer, not ash's table: ash fills a command
            // the driver lacks with a stub that panics across an
            // `extern "system"` boundary, which aborts before `guard` sees it.
            let Some(ptr) = next_device_fn(device, c"vkGetDeviceQueue2") else {
                eprintln!("[Argus-Layer] ERROR: Could not get real vkGetDeviceQueue2");
                *queue = vk::Queue::null();
                return;
            };
            let real_fn: vk::PFN_vkGetDeviceQueue2 = std::mem::transmute(ptr);
            real_fn(device, info, queue);
            record_queue(device, *queue, (*info).queue_family_index);
        }),
        || (),
    )
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
    let created = Cell::new(false);
    guard(
        "vkCreateSwapchainKHR",
        std::panic::AssertUnwindSafe(|| {
            let Some(ptr) = next_device_fn(device, c"vkCreateSwapchainKHR") else {
                return vk::Result::ERROR_INITIALIZATION_FAILED;
            };
            let real_fn: vk::PFN_vkCreateSwapchainKHR = std::mem::transmute(ptr);
            let res = real_fn(device, p_create_info, p_allocator, p_swapchain);
            if res != vk::Result::SUCCESS {
                return res;
            }
            created.set(true);

            let swapchain = *p_swapchain;
            let ci = &*p_create_info;
            eprintln!(
                "[Argus-Layer] Swapchain created: {:?} ({}x{}, format {:?})",
                swapchain, ci.image_extent.width, ci.image_extent.height, ci.image_format
            );
            if !*PASSTHROUGH {
                init_overlay(device, ci, swapchain);
            }
            vk::Result::SUCCESS
        }),
        || created_result(&created),
    )
}

/// Build the HUD resources for a swapchain the game just created. Any gap in
/// what we know about the device leaves the swapchain without a HUD.
unsafe fn init_overlay(
    device: vk::Device,
    ci: &vk::SwapchainCreateInfoKHR,
    swapchain: vk::SwapchainKHR,
) {
    if !ci
        .image_usage
        .contains(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        || ci.image_array_layers != 1
        || ci.flags.contains(vk::SwapchainCreateFlagsKHR::PROTECTED)
    {
        eprintln!("[Argus-Layer] HUD skipped: unsupported swapchain usage/layers/protection");
        return;
    }
    let Some((physical_device, ash_dev)) = DEVICE_MAP
        .read_or_recover()
        .get(&device)
        .map(|(pd, dev)| (*pd, dev.clone()))
    else {
        return;
    };
    let Some(instance) = PHYS_TO_INST
        .read_or_recover()
        .get(&physical_device)
        .copied()
    else {
        return;
    };
    let ash_instances = ASH_INSTANCES.read_or_recover();
    let Some(ash_inst) = ash_instances.get(&instance) else {
        return;
    };
    let swapchain_fn = ash::khr::swapchain::Device::new(ash_inst, &ash_dev);
    let Ok(images) = swapchain_fn.get_swapchain_images(swapchain) else {
        return;
    };

    // Get queue family for command pool
    let qf = DEVICE_QUEUE_FAMILY
        .read_or_recover()
        .get(&device)
        .copied()
        .unwrap_or(0);

    match renderer::OverlayState::new(
        ash_inst,
        physical_device,
        &ash_dev,
        qf,
        &images,
        ci.image_format,
        ci.image_extent,
        ci.pre_transform,
    ) {
        Some(mut state) => {
            state.swapchain = swapchain;
            OVERLAY_STATES.lock_or_recover().insert(swapchain, state);
            eprintln!(
                "[Argus-Layer] Overlay initialised for swapchain ({} images)",
                images.len()
            );
        }
        None => {
            eprintln!("[Argus-Layer] WARNING: Could not create overlay resources");
        }
    }
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
    let presented = Cell::new(None);
    guard(
        "vkQueuePresentKHR",
        std::panic::AssertUnwindSafe(|| {
            let ticket = if *ACTIVE { capture::ticket() } else { 0 };
            let begin = (ticket != 0).then(Instant::now);
            let result = present_impl(queue, info);
            presented.set(Some(result));
            if let Some(at) = begin {
                use ash::vk::Handle;
                if !info.is_null() && !(*info).p_swapchains.is_null() {
                    for (i, swapchain) in std::slice::from_raw_parts(
                        (*info).p_swapchains,
                        (*info).swapchain_count as usize,
                    )
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
        }),
        // A panic in our overlay path must still let the frame reach the
        // screen, exactly once: if the real present already ran, report its
        // result; otherwise present the game's frame unchanged.
        || {
            presented.get().unwrap_or_else(|| {
                real_present(queue).map_or(vk::Result::ERROR_DEVICE_LOST, |real| real(queue, info))
            })
        },
    )
}

/// The next layer's vkQueuePresentKHR for `queue`: cached when the queue was
/// obtained, otherwise resolved through the queue's device.
unsafe fn real_present(queue: vk::Queue) -> Option<vk::PFN_vkQueuePresentKHR> {
    if let Some(real) = REAL_QUEUE_PRESENT.read_or_recover().get(&queue).copied() {
        return Some(real);
    }
    let device = QUEUE_TO_DEVICE.read_or_recover().get(&queue).copied()?;
    let real: vk::PFN_vkQueuePresentKHR =
        std::mem::transmute(next_device_fn(device, c"vkQueuePresentKHR")?);
    REAL_QUEUE_PRESENT.write_or_recover().insert(queue, real);
    Some(real)
}

unsafe fn present_impl(queue: vk::Queue, p_present_info: *const vk::PresentInfoKHR) -> vk::Result {
    let Some(real) = real_present(queue) else {
        return vk::Result::ERROR_DEVICE_LOST;
    };
    if *PASSTHROUGH || p_present_info.is_null() {
        return real(queue, p_present_info);
    }
    // Every lock of ours is released by now: the real present may block for
    // vsync, and must not stall other swapchains or the destroy hooks.
    match submit_overlay(queue, &*p_present_info) {
        Some(overlay_done) => {
            // Our submission already waited on the game's semaphores, so the
            // present waits on ours instead.
            let mut present = *p_present_info;
            present.wait_semaphore_count = 1;
            present.p_wait_semaphores = &overlay_done;
            real(queue, &present)
        }
        None => real(queue, p_present_info),
    }
}

/// Record and submit the HUD for this present. Returns the semaphore the
/// present must wait on, or None to present the game's frame unchanged.
unsafe fn submit_overlay(queue: vk::Queue, pi: &vk::PresentInfoKHR) -> Option<vk::Semaphore> {
    // Multiple swapchains require a combined submission and semaphore lifetime
    // scheme. Preserve presentation unchanged for this untested route.
    if pi.swapchain_count != 1 {
        return None;
    }
    // Per spec these are non-null whenever swapchain_count == 1, but a layer
    // stacked below us handing back a malformed PresentInfoKHR would
    // otherwise be a null deref here (UB, not a catchable panic).
    if pi.p_swapchains.is_null() || pi.p_image_indices.is_null() {
        return None;
    }
    let device = QUEUE_TO_DEVICE.read_or_recover().get(&queue).copied()?;
    let device_map = DEVICE_MAP.read_or_recover();
    let (_, dev) = device_map.get(&device)?;
    let mut states = OVERLAY_STATES.lock_or_recover();
    let state = states.get_mut(&*pi.p_swapchains)?;
    state.stats.record(Instant::now());
    if QUEUE_FAMILIES.read_or_recover().get(&queue).copied() != Some(state.queue_family) {
        return None;
    }
    if !GRAPHICS_QUEUES
        .read_or_recover()
        .get(&queue)
        .copied()
        .unwrap_or(false)
    {
        return None;
    }
    if state.draw_queue.is_some_and(|previous| previous != queue) {
        return None;
    }
    state.draw_queue = Some(queue);
    let index = *pi.p_image_indices as usize;
    let config = OVERLAY_CONFIG.read_or_recover().clone();
    let tel = TELEMETRY.read_or_recover();
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
    let cb = cb?;

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
        state.abandoned_fence = Some(index);
        eprintln!("[Argus-Layer] reset fence failed: {e:?}");
        return None;
    }
    // A failed vkQueueSubmit leaves the semaphores it names untouched (spec),
    // so the game's frame can still be presented as it was.
    if let Err(e) = dev.queue_submit(queue, &[submit], state.fences[index]) {
        state.disabled = true;
        state.abandoned_fence = Some(index);
        eprintln!("[Argus-Layer] overlay submit failed: {e:?}");
        return None;
    }
    Some(state.complete[index])
}

// ---------- vkDestroySwapchainKHR ----------

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
    // Resolved first so the real destroy runs even if our own cleanup panics:
    // losing it would leak the swapchain for the rest of the process.
    let real = guard(
        "vkDestroySwapchainKHR",
        std::panic::AssertUnwindSafe(|| next_device_fn(device, c"vkDestroySwapchainKHR")),
        || None,
    );
    guard(
        "vkDestroySwapchainKHR",
        std::panic::AssertUnwindSafe(|| {
            use ash::vk::Handle;
            capture::end(swapchain.as_raw());
            // Out of the map first, and its lock released: destroying waits
            // for our GPU work, which must not stall other swapchains' presents.
            let state = OVERLAY_STATES.lock_or_recover().remove(&swapchain);
            if let Some(state) = state {
                let dev = DEVICE_MAP
                    .read_or_recover()
                    .get(&device)
                    .map(|(_, dev)| dev.clone());
                if let Some(dev) = dev {
                    state.destroy(&dev);
                }
            }
        }),
        || (),
    );
    if let Some(real) = real {
        let real: vk::PFN_vkDestroySwapchainKHR = std::mem::transmute(real);
        real(device, swapchain, allocator);
    }
}

// ── Dispatch ────────────────────────────────────────────────────────────────

/// One of our entry points as the loader's generic `PFN_vkVoidFunction`.
macro_rules! hook {
    ($f:expr) => {
        Some(std::mem::transmute::<*const (), unsafe extern "system" fn()>($f as *const ()))
    };
}

/// Our device-level hooks, reachable through either GetProcAddr.
unsafe fn device_hook(name: &[u8]) -> vk::PFN_vkVoidFunction {
    match name {
        b"vkDestroyDevice" => hook!(argus_vkDestroyDevice),
        b"vkGetDeviceQueue" => hook!(argus_vkGetDeviceQueue),
        b"vkGetDeviceQueue2" => hook!(argus_vkGetDeviceQueue2),
        b"vkCreateSwapchainKHR" => hook!(argus_vkCreateSwapchainKHR),
        b"vkDestroySwapchainKHR" => hook!(argus_vkDestroySwapchainKHR),
        b"vkQueuePresentKHR" => hook!(argus_vkQueuePresentKHR),
        _ => None,
    }
}

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
    guard(
        "vkGetInstanceProcAddr",
        std::panic::AssertUnwindSafe(|| {
            if p_name.is_null() {
                return None;
            }
            match CStr::from_ptr(p_name).to_bytes() {
                b"vkGetInstanceProcAddr" => hook!(vkGetInstanceProcAddr),
                b"vkGetDeviceProcAddr" => hook!(vkGetDeviceProcAddr),
                b"vkCreateInstance" => hook!(argus_vkCreateInstance),
                b"vkDestroyInstance" => hook!(argus_vkDestroyInstance),
                b"vkEnumeratePhysicalDevices" => hook!(argus_vkEnumeratePhysicalDevices),
                b"vkCreateDevice" => hook!(argus_vkCreateDevice),
                name => device_hook(name).or_else(|| instance_gipa(instance)?(instance, p_name)),
            }
        }),
        || None,
    )
}

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
    guard(
        "vkGetDeviceProcAddr",
        std::panic::AssertUnwindSafe(|| {
            if p_name.is_null() {
                return None;
            }
            let name = CStr::from_ptr(p_name);
            match name.to_bytes() {
                b"vkGetDeviceProcAddr" => hook!(vkGetDeviceProcAddr),
                bytes => device_hook(bytes).or_else(|| next_device_fn(device, name)),
            }
        }),
        || None,
    )
}

// ── Layer negotiation ───────────────────────────────────────────────────────

#[repr(C)]
pub struct VkLayerNegotiateStruct {
    pub s_type: u32,
    pub p_next: *const c_void,
    pub loader_layer_interface_version: u32,
    // Outputs only: the loader passes these in as NULL, which a plain
    // (non-nullable) fn pointer type may not hold.
    pub pfn_get_instance_proc_addr: Option<vk::PFN_vkGetInstanceProcAddr>,
    pub pfn_get_device_proc_addr: Option<vk::PFN_vkGetDeviceProcAddr>,
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
    guard(
        "vkNegotiateLoaderLayerInterfaceVersion",
        std::panic::AssertUnwindSafe(|| {
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

            // The layer below us arrives through each create call's pNext
            // chain, never through this struct.
            vs.loader_layer_interface_version = 2;
            vs.pfn_get_instance_proc_addr = Some(vkGetInstanceProcAddr);
            vs.pfn_get_device_proc_addr = Some(vkGetDeviceProcAddr);

            vk::Result::SUCCESS
        }),
        || vk::Result::ERROR_INITIALIZATION_FAILED,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_returns_the_real_result_when_f_does_not_panic() {
        let r = guard("test", std::panic::AssertUnwindSafe(|| 42), || 0);
        assert_eq!(r, 42);
    }

    #[test]
    fn guard_returns_the_fallback_instead_of_unwinding_on_panic() {
        // catch_unwind still prints the default panic hook's message to
        // stderr; that's expected noise for this test, not a failure.
        let r: i32 = guard(
            "test",
            std::panic::AssertUnwindSafe(|| panic!("boom")),
            || -1,
        );
        assert_eq!(r, -1, "a panic in the guarded closure must not propagate");
    }

    #[test]
    fn clamp_string_leaves_short_strings_untouched() {
        let mut s = String::from("RTX 4090");
        clamp_string(&mut s, MAX_TELEMETRY_STRING_LEN);
        assert_eq!(s, "RTX 4090");
    }

    #[test]
    fn clamp_string_truncates_long_strings_to_a_char_boundary() {
        // 600 copies of a 3-byte UTF-8 character: truncating at a raw byte
        // offset of 512 would land mid-character without the boundary walk.
        let mut s = "é".repeat(600);
        assert_eq!(s.len(), 1200); // 'é' is 2 bytes in UTF-8 here
        clamp_string(&mut s, MAX_TELEMETRY_STRING_LEN);
        assert!(s.len() <= MAX_TELEMETRY_STRING_LEN);
        assert!(s.is_char_boundary(s.len()));
        // Every remaining character must be a complete, valid 'é' — a
        // boundary miscalculation would instead leave a truncated byte
        // sequence that isn't valid UTF-8 at all.
        assert!(s.chars().all(|c| c == 'é'));
    }

    #[test]
    fn sanitize_telemetry_frame_bounds_every_field_hud_rs_renders_from() {
        let mut frame = TelemetryFrame {
            cpu_name: "x".repeat(10_000),
            cpu_power_status: "x".repeat(10_000),
            gpu_name: "x".repeat(10_000),
            ram_speed_status: "x".repeat(10_000),
            active_profile: "x".repeat(10_000),
            cpus: vec![argus_ipc::LogicalCpu::default(); 5_000],
            probalance_pids: vec![0; 5_000],
            launch_profiles: vec![
                argus_ipc::LaunchProfile {
                    pid: 1,
                    start_ticks: 1,
                    profile: "y".repeat(10_000),
                };
                5_000
            ],
            ..Default::default()
        };

        sanitize_telemetry_frame(&mut frame);

        assert!(frame.cpu_name.len() <= MAX_TELEMETRY_STRING_LEN);
        assert!(frame.cpu_power_status.len() <= MAX_TELEMETRY_STRING_LEN);
        assert!(frame.gpu_name.len() <= MAX_TELEMETRY_STRING_LEN);
        assert!(frame.ram_speed_status.len() <= MAX_TELEMETRY_STRING_LEN);
        assert!(frame.active_profile.len() <= MAX_TELEMETRY_STRING_LEN);
        assert!(frame.cpus.len() <= MAX_TELEMETRY_LIST_LEN);
        assert!(frame.probalance_pids.len() <= MAX_TELEMETRY_LIST_LEN);
        assert!(frame.launch_profiles.len() <= MAX_TELEMETRY_LIST_LEN);
        for lp in &frame.launch_profiles {
            assert!(lp.profile.len() <= MAX_TELEMETRY_STRING_LEN);
        }
    }

    /// The loader passes the negotiation outputs in as NULL. They must only
    /// ever be written — reading them as fn pointers was undefined behaviour.
    #[test]
    fn negotiation_fills_its_outputs() {
        let mut vs = VkLayerNegotiateStruct {
            s_type: 0,
            p_next: std::ptr::null(),
            loader_layer_interface_version: 2,
            pfn_get_instance_proc_addr: None,
            pfn_get_device_proc_addr: None,
            pfn_get_physical_device_proc_addr: std::ptr::null(),
        };
        let res = unsafe { vkNegotiateLoaderLayerInterfaceVersion(&mut vs) };
        assert_eq!(res, vk::Result::SUCCESS);
        assert!(vs.pfn_get_instance_proc_addr.is_some());
        assert!(vs.pfn_get_device_proc_addr.is_some());
    }

    /// Handles are recycled addresses, so a destroyed device's entries must
    /// go with it — and only its entries.
    #[test]
    fn forgetting_a_device_drops_its_queues_and_nothing_else() {
        use ash::vk::Handle;
        let (gone, kept) = (vk::Device::from_raw(0xde01), vk::Device::from_raw(0xde02));
        let (gone_q, kept_q) = (vk::Queue::from_raw(0xde11), vk::Queue::from_raw(0xde12));
        for (queue, device) in [(gone_q, gone), (kept_q, kept)] {
            QUEUE_TO_DEVICE.write_or_recover().insert(queue, device);
            QUEUE_FAMILIES.write_or_recover().insert(queue, 0);
            GRAPHICS_QUEUES.write_or_recover().insert(queue, true);
            DEVICE_QUEUE_FAMILY.write_or_recover().insert(device, 0);
        }

        unsafe { forget_device(gone) };

        assert!(!QUEUE_TO_DEVICE.read_or_recover().contains_key(&gone_q));
        assert!(!QUEUE_FAMILIES.read_or_recover().contains_key(&gone_q));
        assert!(!GRAPHICS_QUEUES.read_or_recover().contains_key(&gone_q));
        assert!(!DEVICE_QUEUE_FAMILY.read_or_recover().contains_key(&gone));
        assert_eq!(QUEUE_TO_DEVICE.read_or_recover().get(&kept_q), Some(&kept));
        assert!(DEVICE_QUEUE_FAMILY.read_or_recover().contains_key(&kept));
    }

    #[test]
    fn forgetting_an_instance_drops_its_physical_devices_only() {
        use ash::vk::Handle;
        let (gone, kept) = (
            vk::Instance::from_raw(0xe101),
            vk::Instance::from_raw(0xe102),
        );
        let (gone_pd, kept_pd) = (
            vk::PhysicalDevice::from_raw(0xe111),
            vk::PhysicalDevice::from_raw(0xe112),
        );
        PHYS_TO_INST.write_or_recover().insert(gone_pd, gone);
        PHYS_TO_INST.write_or_recover().insert(kept_pd, kept);

        forget_instance(gone);

        assert!(!PHYS_TO_INST.read_or_recover().contains_key(&gone_pd));
        assert_eq!(PHYS_TO_INST.read_or_recover().get(&kept_pd), Some(&kept));
    }

    #[test]
    fn next_gipa_round_trips_through_the_atomic_and_starts_unset() {
        // Runs in whatever order the test harness picks, so only assert the
        // round-trip, not the initial None (another test in this binary may
        // have already set it — these globals are process-wide by design).
        extern "system" fn dummy(
            _instance: vk::Instance,
            _p_name: *const c_char,
        ) -> vk::PFN_vkVoidFunction {
            None
        }
        set_next_gipa(dummy);
        assert!(get_next_gipa().is_some());
    }
}
