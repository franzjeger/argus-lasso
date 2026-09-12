with open("argus-layer/src/lib.rs", "r") as f:
    text = f.read()

gip2 = """    if name.to_bytes() == b"vkEnumeratePhysicalDevices" {
        let next_gip_lock = NEXT_GET_INSTANCE_PROC_ADDR.read().unwrap();
        if let Some(next) = *next_gip_lock {
            let real_ptr = next(instance, p_name);
            if let Some(real_ptr) = real_ptr {
                let real_func: ash::vk::PFN_vkEnumeratePhysicalDevices = std::mem::transmute(real_ptr);
                REAL_ENUM_PHYSICAL_DEVICES.write().unwrap().insert(instance, real_func);
                return Some(std::mem::transmute(argus_vkEnumeratePhysicalDevices as *const ()));
            }
        }
    }
    
    if name.to_bytes() == b"vkCreateDevice" {
        let next_gip_lock = NEXT_GET_INSTANCE_PROC_ADDR.read().unwrap();
        if let Some(next) = *next_gip_lock {
            let real_ptr = next(instance, p_name);
            if let Some(real_ptr) = real_ptr {
                // vkCreateDevice is physically keyed but we intercept the function ptr for physical devices later or just store it.
                // Wait! vkCreateDevice takes PhysicalDevice as first arg!
            }
        }
        return Some(std::mem::transmute(argus_vkCreateDevice as *const ()));
    }
    
    let next_gip_lock = NEXT_GET_INSTANCE_PROC_ADDR.read().unwrap();"""

text = text.replace("    let next_gip_lock = NEXT_GET_INSTANCE_PROC_ADDR.read().unwrap();", gip2, 1)

with open("argus-layer/src/lib.rs", "w") as f:
    f.write(text)
