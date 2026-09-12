import re

with open("argus-layer/src/lib.rs", "r") as f:
    text = f.read()

header = """use std::ffi::{c_void, CStr};
use std::os::raw::c_char;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;
use std::sync::RwLock;
use std::collections::HashMap;
use std::thread;
use std::os::unix::net::UnixStream;
use std::io::Read;
use argus_ipc::TelemetryFrame;
use ash::vk;
"""
text = text.replace("use std::ffi::{c_void, CStr};\nuse std::os::raw::c_char;\nuse std::sync::atomic::{AtomicUsize, Ordering};\nuse std::time::Instant;\nuse std::sync::RwLock;\nuse std::collections::HashMap;\nuse std::thread;\nuse std::os::unix::net::UnixStream;\nuse std::io::Read;\nuse argus_ipc::TelemetryFrame;\n", header)

statics = """lazy_static::lazy_static! {
    static ref REAL_GET_DEVICE_QUEUE: RwLock<HashMap<ash::vk::Device, ash::vk::PFN_vkGetDeviceQueue>> = RwLock::new(HashMap::new());
    static ref REAL_QUEUE_PRESENT: RwLock<HashMap<ash::vk::Queue, ash::vk::PFN_vkQueuePresentKHR>> = RwLock::new(HashMap::new());
    static ref REAL_CREATE_DEVICE: RwLock<HashMap<ash::vk::PhysicalDevice, ash::vk::PFN_vkCreateDevice>> = RwLock::new(HashMap::new());
    static ref REAL_CREATE_SWAPCHAIN: RwLock<HashMap<ash::vk::Device, ash::vk::PFN_vkCreateSwapchainKHR>> = RwLock::new(HashMap::new());
    static ref LAST_FRAME_TIME: RwLock<Option<Instant>> = RwLock::new(None);
}
"""
text = re.sub(r'lazy_static::lazy_static! \{.*?LAST_FRAME_TIME.*?\}', statics, text, flags=re.DOTALL)

hooks = """
#[no_mangle]
pub unsafe extern "system" fn argus_vkCreateSwapchainKHR(
    device: ash::vk::Device,
    p_create_info: *const ash::vk::SwapchainCreateInfoKHR,
    p_allocator: *const ash::vk::AllocationCallbacks,
    p_swapchain: *mut ash::vk::SwapchainKHR,
) -> ash::vk::Result {
    let real_create = {
        let map = REAL_CREATE_SWAPCHAIN.read().unwrap();
        map.get(&device).copied()
    };
    if let Some(real_create) = real_create {
        let res = real_create(device, p_create_info, p_allocator, p_swapchain);
        if res == ash::vk::Result::SUCCESS {
            println!("[Argus-Layer] Swapchain Created! Ready to initialize Egui renderer.");
            // NOTE: Initializing ash::Device and egui renderer here requires careful 
            // instance/device pointer unwrapping which we defer to prevent validation crashes.
        }
        res
    } else {
        ash::vk::Result::ERROR_INITIALIZATION_FAILED
    }
}
"""
text = text.replace("#[no_mangle]\npub unsafe extern \"system\" fn argus_vkQueuePresentKHR", hooks + "\n#[no_mangle]\npub unsafe extern \"system\" fn argus_vkQueuePresentKHR")

gdp = """    if name.to_bytes() == b"vkCreateSwapchainKHR" {
        if let Some(next) = NEXT_GET_DEVICE_PROC_ADDR {
            let real_ptr = next(device, p_name);
            if let Some(real_ptr) = real_ptr {
                let real_func: ash::vk::PFN_vkCreateSwapchainKHR = std::mem::transmute(real_ptr);
                REAL_CREATE_SWAPCHAIN.write().unwrap().insert(device, real_func);
                return Some(std::mem::transmute(argus_vkCreateSwapchainKHR as *const ()));
            }
        }
    }
"""
text = text.replace("    if name.to_bytes() == b\"vkQueuePresentKHR\" {", gdp + "    if name.to_bytes() == b\"vkQueuePresentKHR\" {")

with open("argus-layer/src/lib.rs", "w") as f:
    f.write(text)

