//! Argus-Layer: Vulkan implicit layer that draws a telemetry HUD.
//!
//! Hooks: vkCreateInstance, vkEnumeratePhysicalDevices, vkCreateDevice,
//!        vkGetDeviceQueue, vkCreateSwapchainKHR, vkQueuePresentKHR.

pub mod font;
pub mod renderer;

use std::ffi::{c_void, CStr};
use std::os::raw::c_char;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use std::sync::{Mutex, RwLock};
use std::collections::HashMap;
use std::thread;
use std::os::unix::net::UnixStream;
use std::io::Read;
use argus_ipc::TelemetryFrame;
use ash::vk;
use renderer::OverlayState;

// ── Global mutable function pointers (set once during negotiation) ──────────
static mut NEXT_GET_INSTANCE_PROC_ADDR: Option<vk::PFN_vkGetInstanceProcAddr> = None;
static mut NEXT_GET_DEVICE_PROC_ADDR: Option<vk::PFN_vkGetDeviceProcAddr> = None;

// ── Lookup tables ───────────────────────────────────────────────────────────
lazy_static::lazy_static! {
    // Telemetry data from the Argus-Lasso daemon (received via IPC)
    static ref TELEMETRY: RwLock<TelemetryFrame> = RwLock::new(TelemetryFrame::default());

    // FPS tracking
    static ref LAST_FRAME_TIME: RwLock<Option<Instant>> = RwLock::new(None);
    static ref CURRENT_FPS: RwLock<f64> = RwLock::new(0.0);

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

fn start_ipc_thread() {
    thread::spawn(|| {
        loop {
            if let Ok(mut stream) = UnixStream::connect(argus_ipc::IPC_SOCKET_PATH) {
                let mut len_buf = [0u8; 4];
                while stream.read_exact(&mut len_buf).is_ok() {
                    let len = u32::from_le_bytes(len_buf) as usize;
                    let mut data = vec![0u8; len];
                    if stream.read_exact(&mut data).is_ok() {
                        if let Ok(frame) = bincode::deserialize::<TelemetryFrame>(&data) {
                            *TELEMETRY.write().unwrap() = frame;
                        }
                    } else {
                        break;
                    }
                }
            }
            thread::sleep(std::time::Duration::from_secs(2));
        }
    });
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

const VK_STRUCTURE_TYPE_LOADER_INSTANCE_CREATE_INFO: vk::StructureType = vk::StructureType::LOADER_INSTANCE_CREATE_INFO;

/// Walk the pNext chain of InstanceCreateInfo to find the layer link info.
unsafe fn find_layer_link(p_create_info: *const vk::InstanceCreateInfo) -> Option<*mut *mut VkLayerInstanceLink> {
    let mut p_next = (*p_create_info).p_next as *const VkLayerInstanceCreateInfo;
    while !p_next.is_null() {
        if (*p_next).s_type == VK_STRUCTURE_TYPE_LOADER_INSTANCE_CREATE_INFO && (*p_next).function == 0 {
            // Return a pointer to the union field so we can modify it
            return Some(&mut (*(p_next as *mut VkLayerInstanceCreateInfo)).u.p_layer_info);
        }
        p_next = (*p_next).p_next as *const VkLayerInstanceCreateInfo;
    }
    None
}

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
    let ptr = next_gipa(vk::Instance::null(), b"vkCreateInstance\0".as_ptr() as *const c_char);
    let real_create_instance: vk::PFN_vkCreateInstance = match ptr {
        Some(p) => std::mem::transmute(p),
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
    ASH_INSTANCES.write().unwrap().insert(instance, ash_inst);

    vk::Result::SUCCESS
}

// ---------- vkEnumeratePhysicalDevices ----------

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
    let ptr = next(instance, b"vkEnumeratePhysicalDevices\0".as_ptr() as *const c_char);
    let real_fn: vk::PFN_vkEnumeratePhysicalDevices = match ptr {
        Some(p) => std::mem::transmute(p),
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

const VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO: vk::StructureType = vk::StructureType::LOADER_DEVICE_CREATE_INFO;

unsafe fn find_device_layer_link(p_create_info: *const vk::DeviceCreateInfo) -> Option<*mut *mut VkLayerDeviceLink> {
    let mut p_next = (*p_create_info).p_next as *const VkLayerDeviceCreateInfo;
    while !p_next.is_null() {
        if (*p_next).s_type == VK_STRUCTURE_TYPE_LOADER_DEVICE_CREATE_INFO && (*p_next).function == 0 {
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
            let ptr = next(instance, b"vkCreateDevice\0".as_ptr() as *const c_char);
            let real_fn: vk::PFN_vkCreateDevice = match ptr {
                Some(p) => std::mem::transmute(p),
                None => return vk::Result::ERROR_INITIALIZATION_FAILED,
            };
            let res = real_fn(physical_device, p_create_info, p_allocator, p_device);
            if res == vk::Result::SUCCESS {
                let device = *p_device;
                eprintln!("[Argus-Layer] Device created (fallback): {:?}", device);
                
                // For fallback, use the instance's get_device_proc_addr
                let ash_inst = ASH_INSTANCES.read().unwrap().get(&instance).unwrap().clone();
                let gipa = ash_inst.fp_v1_0().get_device_proc_addr;
                let ash_dev = ash::Device::load_with(
                    |name| {
                        let ptr = gipa(device, name.as_ptr());
                        std::mem::transmute(ptr)
                    },
                    device,
                );
                DEVICE_MAP.write().unwrap().insert(device, (physical_device, ash_dev));
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
    let ptr = next_gipa(instance, b"vkCreateDevice\0".as_ptr() as *const c_char);
    let real_fn: vk::PFN_vkCreateDevice = match ptr {
        Some(p) => std::mem::transmute(p),
        None => return vk::Result::ERROR_INITIALIZATION_FAILED,
    };

    let res = real_fn(physical_device, p_create_info, p_allocator, p_device);
    if res != vk::Result::SUCCESS {
        return res;
    }

    let device = *p_device;
    eprintln!("[Argus-Layer] Device created: {:?}", device);

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
    DEVICE_MAP.write().unwrap().insert(device, (physical_device, ash_dev));

    // Record the first queue family requested
    if !p_create_info.is_null() {
        let ci = &*p_create_info;
        if ci.queue_create_info_count > 0 && !ci.p_queue_create_infos.is_null() {
            let qci = &*ci.p_queue_create_infos;
            DEVICE_QUEUE_FAMILY.write().unwrap().insert(device, qci.queue_family_index);
        }
    }

    vk::Result::SUCCESS
}

// ---------- vkGetDeviceQueue ----------

#[no_mangle]
pub unsafe extern "system" fn argus_vkGetDeviceQueue(
    device: vk::Device,
    queue_family_index: u32,
    queue_index: u32,
    p_queue: *mut vk::Queue,
) {
    eprintln!("[Argus-Layer] vkGetDeviceQueue called (device={:?}, family={}, idx={})", device, queue_family_index, queue_index);
    
    // Call the real vkGetDeviceQueue via raw pointer
    let next = match DEVICE_GDPA.read().unwrap().get(&device).copied() {
        Some(f) => f,
        None => {
            eprintln!("[Argus-Layer] ERROR: No DEVICE_GDPA for device in vkGetDeviceQueue");
            return;
        }
    };
    let ptr = next(device, b"vkGetDeviceQueue\0".as_ptr() as *const c_char);
    let real_fn: vk::PFN_vkGetDeviceQueue = match ptr {
        Some(p) => std::mem::transmute(p),
        None => {
            eprintln!("[Argus-Layer] ERROR: Could not get real vkGetDeviceQueue");
            return;
        }
    };
    
    real_fn(device, queue_family_index, queue_index, p_queue);
    let queue = *p_queue;

    QUEUE_TO_DEVICE.write().unwrap().insert(queue, device);

    // Capture the real vkQueuePresentKHR for this device
    let present_ptr = next(device, b"vkQueuePresentKHR\0".as_ptr() as *const c_char);
    if let Some(present_ptr) = present_ptr {
        let real: vk::PFN_vkQueuePresentKHR = std::mem::transmute(present_ptr);
        REAL_QUEUE_PRESENT.write().unwrap().insert(queue, real);
    }

    eprintln!("[Argus-Layer] Queue obtained: {:?}", queue);
}

// ---------- vkCreateSwapchainKHR ----------

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
                let ptr = next(device, b"vkCreateSwapchainKHR\0".as_ptr() as *const c_char);
                match ptr {
                    Some(p) => std::mem::transmute(p),
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

    eprintln!("[Argus-Layer] Swapchain created: {:?} ({}x{}, format {:?})", swapchain, extent.width, extent.height, format);

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
    let qf = DEVICE_QUEUE_FAMILY.read().unwrap().get(&device).copied().unwrap_or(0);

    // Create overlay state
    match OverlayState::new(ash_inst, &ash_dev, physical_device, qf, &images, format, extent) {
        Some(state) => {
            OVERLAY_STATES.lock().unwrap().insert(swapchain, state);
            eprintln!("[Argus-Layer] Overlay initialised for swapchain ({} images)", images.len());
        }
        None => {
            eprintln!("[Argus-Layer] WARNING: Could not create overlay resources");
        }
    }

    vk::Result::SUCCESS
}

// ---------- vkQueuePresentKHR (the main drawing hook) ----------

#[no_mangle]
pub unsafe extern "system" fn argus_vkQueuePresentKHR(
    queue: vk::Queue,
    p_present_info: *const vk::PresentInfoKHR,
) -> vk::Result {
    // ── FPS calculation ────────────────────────────────────────────
    static FRAME_COUNT: AtomicUsize = AtomicUsize::new(0);
    static FPS_ACCUM: AtomicUsize = AtomicUsize::new(0);

    let now = Instant::now();
    {
        let mut last = LAST_FRAME_TIME.write().unwrap();
        if let Some(prev) = *last {
            let us = now.duration_since(prev).as_micros() as usize;
            FPS_ACCUM.fetch_add(us, Ordering::Relaxed);
        }
        *last = Some(now);
    }
    let count = FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
    if count % 30 == 0 && count > 0 {
        let total_us = FPS_ACCUM.swap(0, Ordering::Relaxed);
        if total_us > 0 {
            let avg_frame_us = total_us as f64 / 30.0;
            *CURRENT_FPS.write().unwrap() = 1_000_000.0 / avg_frame_us;
        }
    }

    // ── Draw the overlay ───────────────────────────────────────────
    let pi = &*p_present_info;

    let device = QUEUE_TO_DEVICE.read().unwrap().get(&queue).copied();
    if let Some(device) = device {
        let device_map = DEVICE_MAP.read().unwrap();
        if let Some((_, ash_dev)) = device_map.get(&device) {
            let swapchain_count = pi.swapchain_count as usize;
            let swapchains = std::slice::from_raw_parts(pi.p_swapchains, swapchain_count);
            let image_indices = std::slice::from_raw_parts(pi.p_image_indices, swapchain_count);

            let overlay_states = OVERLAY_STATES.lock().unwrap();

            for i in 0..swapchain_count {
                if let Some(state) = overlay_states.get(&swapchains[i]) {
                    let fps = *CURRENT_FPS.read().unwrap();
                    let tel = TELEMETRY.read().unwrap();
                    let text = format!(
                        "FPS:{:.0} CPU:{}% {}C GPU:{}% {}C Park:{}",
                        fps,
                        tel.cpu_usage_percent,
                        tel.cpu_temp_c,
                        tel.gpu_usage_percent,
                        tel.gpu_temp_c,
                        tel.parked_cores,
                    );

                    if let Some(cb) = state.record_overlay(ash_dev, image_indices[i] as usize, &text) {
                        // Submit our overlay command buffer BEFORE present
                        let cbs = [cb];
                        let submit_info = vk::SubmitInfo::default()
                            .command_buffers(&cbs);
                        let submits = [submit_info];

                        let _ = ash_dev.queue_submit(
                            queue,
                            &submits,
                            state.fences[image_indices[i] as usize],
                        );

                        // Wait for completion before present
                        let _ = ash_dev.queue_wait_idle(queue);
                    }
                }
            }
        }
    }

    // ── Call the real vkQueuePresentKHR ─────────────────────────────
    let real = REAL_QUEUE_PRESENT.read().unwrap().get(&queue).copied();
    if let Some(real) = real {
        real(queue, p_present_info)
    } else {
        vk::Result::ERROR_DEVICE_LOST
    }
}

// ── Dispatch: vkGetInstanceProcAddr ─────────────────────────────────────────

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
        b"vkGetInstanceProcAddr" => Some(std::mem::transmute(vkGetInstanceProcAddr as *const ())),
        b"vkGetDeviceProcAddr" => Some(std::mem::transmute(vkGetDeviceProcAddr as *const ())),
        b"vkCreateInstance" => Some(std::mem::transmute(argus_vkCreateInstance as *const ())),
        b"vkEnumeratePhysicalDevices" => Some(std::mem::transmute(argus_vkEnumeratePhysicalDevices as *const ())),
        b"vkCreateDevice" => Some(std::mem::transmute(argus_vkCreateDevice as *const ())),
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
        b"vkGetDeviceProcAddr" => Some(std::mem::transmute(vkGetDeviceProcAddr as *const ())),
        b"vkGetDeviceQueue" => Some(std::mem::transmute(argus_vkGetDeviceQueue as *const ())),
        b"vkCreateSwapchainKHR" => Some(std::mem::transmute(argus_vkCreateSwapchainKHR as *const ())),
        b"vkQueuePresentKHR" => Some(std::mem::transmute(argus_vkQueuePresentKHR as *const ())),
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

#[no_mangle]
pub unsafe extern "system" fn vkNegotiateLoaderLayerInterfaceVersion(
    p_version_struct: *mut VkLayerNegotiateStruct,
) -> vk::Result {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        eprintln!("[Argus-Layer] Initialising Argus Overlay Layer");
        start_ipc_thread();
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
