with open("argus-layer/src/lib.rs", "r") as f:
    lines = f.readlines()

new_lines = []
for line in lines:
    if line.startswith("    static ref REAL_GET_DEVICE_QUEUE:"):
        new_lines.append("    static ref REAL_CREATE_SWAPCHAIN: RwLock<HashMap<ash::vk::Device, ash::vk::PFN_vkCreateSwapchainKHR>> = RwLock::new(HashMap::new());\n")
    if line.startswith("#[no_mangle]") and "argus_vkQueuePresentKHR" in lines[lines.index(line) + 1]:
        new_lines.append("""#[no_mangle]
pub unsafe extern "system" fn argus_vkCreateSwapchainKHR(
    device: ash::vk::Device,
    p_create_info: *const ash::vk::SwapchainCreateInfoKHR,
    p_allocator: *const ash::vk::AllocationCallbacks,
    p_swapchain: *mut ash::vk::SwapchainKHR,
) -> ash::vk::Result {
    let real_create = REAL_CREATE_SWAPCHAIN.read().unwrap().get(&device).copied();
    if let Some(real_create) = real_create {
        let res = real_create(device, p_create_info, p_allocator, p_swapchain);
        if res == ash::vk::Result::SUCCESS {
            println!("[Argus-Layer] Swapchain Created! Hook triggered! We are ready to draw!");
        }
        res
    } else {
        ash::vk::Result::ERROR_INITIALIZATION_FAILED
    }
}
""")
    if line.startswith("    if name.to_bytes() == b\"vkQueuePresentKHR\" {"):
        new_lines.append("""    if name.to_bytes() == b"vkCreateSwapchainKHR" {
        if let Some(next) = NEXT_GET_DEVICE_PROC_ADDR {
            let real_ptr = next(device, p_name);
            if let Some(real_ptr) = real_ptr {
                let real_func: ash::vk::PFN_vkCreateSwapchainKHR = std::mem::transmute(real_ptr);
                REAL_CREATE_SWAPCHAIN.write().unwrap().insert(device, real_func);
                return Some(std::mem::transmute(argus_vkCreateSwapchainKHR as *const ()));
            }
        }
    }
""")
    new_lines.append(line)

with open("argus-layer/src/lib.rs", "w") as f:
    f.writelines(new_lines)

