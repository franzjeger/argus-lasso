with open("argus-layer/src/lib.rs", "r") as f:
    lines = f.readlines()

new_lines = []
for line in lines:
    if line.startswith("#[no_mangle]") and "argus_vkCreateSwapchainKHR" in lines[lines.index(line) + 1]:
        new_lines.append("""#[no_mangle]
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
            println!("[Argus-Layer] Device created! Mapping physical device to logical device.");
        }
        res
    } else {
        ash::vk::Result::ERROR_INITIALIZATION_FAILED
    }
}
""")
    if line.startswith("    if name.to_bytes() == b\"vkCreateSwapchainKHR\" {"):
        new_lines.append("""    if name.to_bytes() == b"vkCreateDevice" {
        let next_gip_lock = NEXT_GET_INSTANCE_PROC_ADDR.read().unwrap();
        if let Some(next) = *next_gip_lock {
            // Need to pass instance here, but vkGetDeviceProcAddr doesn't have it...
            // Actually vkCreateDevice is an Instance level function.
        }
    }
""")
    new_lines.append(line)

# Wait, vkCreateDevice is intercepted via vkGetInstanceProcAddr, NOT vkGetDeviceProcAddr!
