import re

with open("argus-layer/src/lib.rs", "r") as f:
    text = f.read()

# Add new maps
maps = """
    static ref REAL_CREATE_INSTANCE: RwLock<HashMap<ash::vk::Instance, ash::vk::PFN_vkCreateInstance>> = RwLock::new(HashMap::new());
    static ref REAL_ENUM_PHYSICAL_DEVICES: RwLock<HashMap<ash::vk::Instance, ash::vk::PFN_vkEnumeratePhysicalDevices>> = RwLock::new(HashMap::new());
    static ref PHYS_TO_INST: RwLock<HashMap<ash::vk::PhysicalDevice, ash::vk::Instance>> = RwLock::new(HashMap::new());
    static ref DEVICE_TO_PHYS: RwLock<HashMap<ash::vk::Device, ash::vk::PhysicalDevice>> = RwLock::new(HashMap::new());
    
    static ref LAST_FRAME_TIME: RwLock<Option<Instant>> = RwLock::new(None);"""

text = text.replace("    static ref LAST_FRAME_TIME: RwLock<Option<Instant>> = RwLock::new(None);", maps)

hooks = """
#[no_mangle]
pub unsafe extern "system" fn argus_vkCreateInstance(
    p_create_info: *const ash::vk::InstanceCreateInfo,
    p_allocator: *const ash::vk::AllocationCallbacks,
    p_instance: *mut ash::vk::Instance,
) -> ash::vk::Result {
    let next_gip = NEXT_GET_INSTANCE_PROC_ADDR.read().unwrap();
    if let Some(next) = *next_gip {
        // Technically vkCreateInstance is passed null instance to get the proc addr
        let real_ptr = next(ash::vk::Instance::null(), b"vkCreateInstance\\0".as_ptr() as *const c_char);
        if let Some(real_ptr) = real_ptr {
            let real_func: ash::vk::PFN_vkCreateInstance = std::mem::transmute(real_ptr);
            let res = real_func(p_create_info, p_allocator, p_instance);
            if res == ash::vk::Result::SUCCESS {
                println!("[Argus-Layer] Instance Created: {:?}", *p_instance);
            }
            return res;
        }
    }
    ash::vk::Result::ERROR_INITIALIZATION_FAILED
}

#[no_mangle]
pub unsafe extern "system" fn argus_vkEnumeratePhysicalDevices(
    instance: ash::vk::Instance,
    p_physical_device_count: *mut u32,
    p_physical_devices: *mut ash::vk::PhysicalDevice,
) -> ash::vk::Result {
    let real_enum = REAL_ENUM_PHYSICAL_DEVICES.read().unwrap().get(&instance).copied();
    if let Some(real_enum) = real_enum {
        let res = real_enum(instance, p_physical_device_count, p_physical_devices);
        if res == ash::vk::Result::SUCCESS && !p_physical_devices.is_null() {
            let count = *p_physical_device_count as usize;
            let devices = std::slice::from_raw_parts(p_physical_devices, count);
            let mut map = PHYS_TO_INST.write().unwrap();
            for &phys in devices {
                map.insert(phys, instance);
            }
        }
        res
    } else {
        ash::vk::Result::ERROR_INITIALIZATION_FAILED
    }
}

#[no_mangle]
pub unsafe extern "system" fn argus_vkCreateDevice(
    physical_device: ash::vk::PhysicalDevice,
    p_create_info: *const ash::vk::DeviceCreateInfo,
    p_allocator: *const ash::vk::AllocationCallbacks,
    p_device: *mut ash::vk::Device,
) -> ash::vk::Result {
    let real_create = REAL_CREATE_DEVICE.read().unwrap().get(&physical_device).copied();
    if let Some(real_create) = real_create {
        let res = real_create(physical_device, p_create_info, p_allocator, p_device);
        if res == ash::vk::Result::SUCCESS {
            DEVICE_TO_PHYS.write().unwrap().insert(*p_device, physical_device);
            println!("[Argus-Layer] Device Created & Mapped!");
        }
        res
    } else {
        ash::vk::Result::ERROR_INITIALIZATION_FAILED
    }
}

#[no_mangle]
pub unsafe extern "system" fn argus_vkCreateSwapchainKHR"""
text = text.replace("#[no_mangle]\npub unsafe extern \"system\" fn argus_vkCreateSwapchainKHR", hooks)

gip = """    if name.to_bytes() == b"vkCreateInstance" {
        return Some(std::mem::transmute(argus_vkCreateInstance as *const ()));
    }
    
    let next_gip_lock = NEXT_GET_INSTANCE_PROC_ADDR.read().unwrap();"""
text = text.replace("    let next_gip_lock = NEXT_GET_INSTANCE_PROC_ADDR.read().unwrap();", gip)

with open("argus-layer/src/lib.rs", "w") as f:
    f.write(text)

