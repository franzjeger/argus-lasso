code = """use std::ffi::{c_void, CStr};
use std::os::raw::c_char;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use std::sync::RwLock;
use std::collections::HashMap;

// Global to store the next layer's GetInstanceProcAddr and GetDeviceProcAddr
static mut NEXT_GET_INSTANCE_PROC_ADDR: Option<ash::vk::PFN_vkGetInstanceProcAddr> = None;
static mut NEXT_GET_DEVICE_PROC_ADDR: Option<ash::vk::PFN_vkGetDeviceProcAddr> = None;

// Store Device -> Real vkGetDeviceQueue
lazy_static::lazy_static! {
    static ref REAL_GET_DEVICE_QUEUE: RwLock<HashMap<ash::vk::Device, ash::vk::PFN_vkGetDeviceQueue>> = RwLock::new(HashMap::new());
    static ref REAL_QUEUE_PRESENT: RwLock<HashMap<ash::vk::Queue, ash::vk::PFN_vkQueuePresentKHR>> = RwLock::new(HashMap::new());
    static ref REAL_CREATE_DEVICE: RwLock<HashMap<ash::vk::PhysicalDevice, ash::vk::PFN_vkCreateDevice>> = RwLock::new(HashMap::new());
    static ref LAST_FRAME_TIME: RwLock<Option<Instant>> = RwLock::new(None);
}

#[no_mangle]
pub unsafe extern "system" fn argus_vkQueuePresentKHR(
    queue: ash::vk::Queue,
    p_present_info: *const ash::vk::PresentInfoKHR,
) -> ash::vk::Result {
    let now = Instant::now();
    let mut last = LAST_FRAME_TIME.write().unwrap();
    if let Some(l) = *last {
        let delta = now.duration_since(l);
        static FRAME_COUNT: AtomicUsize = AtomicUsize::new(0);
        let count = FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
        if count % 60 == 0 {
            let fps = 1.0 / delta.as_secs_f64();
            println!("[Argus-Layer] Hooked vkQueuePresentKHR! FPS: {:.1} ({} ms)", fps, delta.as_millis());
        }
    }
    *last = Some(now);

    let real_present = {
        let map = REAL_QUEUE_PRESENT.read().unwrap();
        map.get(&queue).copied()
    };

    if let Some(real_present) = real_present {
        real_present(queue, p_present_info)
    } else {
        ash::vk::Result::ERROR_DEVICE_LOST
    }
}

#[no_mangle]
pub unsafe extern "system" fn argus_vkGetDeviceQueue(
    device: ash::vk::Device,
    queue_family_index: u32,
    queue_index: u32,
    p_queue: *mut ash::vk::Queue,
) {
    let real_get_queue = {
        let map = REAL_GET_DEVICE_QUEUE.read().unwrap();
        map.get(&device).copied()
    };

    if let Some(real_get_queue) = real_get_queue {
        real_get_queue(device, queue_family_index, queue_index, p_queue);
        
        // Now that the queue is created, we associate it with the real vkQueuePresentKHR for this device
        if let Some(next_gdp) = NEXT_GET_DEVICE_PROC_ADDR {
            let func_name = b"vkQueuePresentKHR\\0";
            let real_present_ptr = next_gdp(device, func_name.as_ptr() as *const c_char);
            if let Some(real_present_ptr) = real_present_ptr {
                let real_present: ash::vk::PFN_vkQueuePresentKHR = std::mem::transmute(real_present_ptr);
                let mut map = REAL_QUEUE_PRESENT.write().unwrap();
                map.insert(*p_queue, real_present);
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn vkGetInstanceProcAddr(
    instance: ash::vk::Instance,
    p_name: *const c_char,
) -> ash::vk::PFN_vkVoidFunction {
    if p_name.is_null() {
        return None;
    }
    
    let name = CStr::from_ptr(p_name);
    if name.to_bytes() == b"vkGetInstanceProcAddr" {
        return Some(std::mem::transmute(vkGetInstanceProcAddr as *const ()));
    }
    if name.to_bytes() == b"vkGetDeviceProcAddr" {
        return Some(std::mem::transmute(vkGetDeviceProcAddr as *const ()));
    }
    
    if let Some(next) = NEXT_GET_INSTANCE_PROC_ADDR {
        next(instance, p_name)
    } else {
        None
    }
}

#[no_mangle]
pub unsafe extern "system" fn vkGetDeviceProcAddr(
    device: ash::vk::Device,
    p_name: *const c_char,
) -> ash::vk::PFN_vkVoidFunction {
    if p_name.is_null() {
        return None;
    }
    
    let name = CStr::from_ptr(p_name);
    if name.to_bytes() == b"vkGetDeviceProcAddr" {
        return Some(std::mem::transmute(vkGetDeviceProcAddr as *const ()));
    }
    if name.to_bytes() == b"vkQueuePresentKHR" {
        return Some(std::mem::transmute(argus_vkQueuePresentKHR as *const ()));
    }
    if name.to_bytes() == b"vkGetDeviceQueue" {
        // Intercept so we can capture the Queue handles
        if let Some(next) = NEXT_GET_DEVICE_PROC_ADDR {
            let real_ptr = next(device, p_name);
            if let Some(real_ptr) = real_ptr {
                let real_func: ash::vk::PFN_vkGetDeviceQueue = std::mem::transmute(real_ptr);
                REAL_GET_DEVICE_QUEUE.write().unwrap().insert(device, real_func);
                return Some(std::mem::transmute(argus_vkGetDeviceQueue as *const ()));
            }
        }
    }
    
    if let Some(next) = NEXT_GET_DEVICE_PROC_ADDR {
        next(device, p_name)
    } else {
        None
    }
}

#[repr(C)]
pub struct VkLayerNegotiateStruct {
    pub s_type: u32,
    pub p_next: *const c_void,
    pub loader_layer_interface_version: u32,
    pub pfn_get_instance_proc_addr: ash::vk::PFN_vkGetInstanceProcAddr,
    pub pfn_get_device_proc_addr: ash::vk::PFN_vkGetDeviceProcAddr,
    pub pfn_get_physical_device_proc_addr: *const c_void,
}

#[no_mangle]
pub unsafe extern "system" fn vkNegotiateLoaderLayerInterfaceVersion(
    p_version_struct: *mut VkLayerNegotiateStruct,
) -> ash::vk::Result {
    if p_version_struct.is_null() {
        return ash::vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    let version_struct = &mut *p_version_struct;
    if version_struct.loader_layer_interface_version < 2 {
        return ash::vk::Result::ERROR_INITIALIZATION_FAILED;
    }
    NEXT_GET_INSTANCE_PROC_ADDR = Some(version_struct.pfn_get_instance_proc_addr);
    NEXT_GET_DEVICE_PROC_ADDR = Some(version_struct.pfn_get_device_proc_addr);
    version_struct.loader_layer_interface_version = 2;
    version_struct.pfn_get_instance_proc_addr = vkGetInstanceProcAddr;
    version_struct.pfn_get_device_proc_addr = vkGetDeviceProcAddr;
    ash::vk::Result::SUCCESS
}
"""
with open("argus-layer/src/lib.rs", "w") as f:
    f.write(code)

