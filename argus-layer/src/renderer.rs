/// Direct-blit Vulkan overlay renderer.
///
/// Strategy: instead of building a full graphics pipeline with shaders, we
/// create a host-visible staging buffer, CPU-rasterise the HUD text into it
/// using our embedded 8×8 bitmap font, then `vkCmdCopyBufferToImage` the
/// result onto the top-left corner of the current swapchain image just before
/// it is presented.
///
/// This avoids needing render passes, descriptor sets, shader modules, or
/// a GPU memory allocator — the entire overlay is a single buffer→image copy.
use ash::vk;
use crate::font;

/// The HUD strip dimensions (pixels).
pub const HUD_W: u32 = 480; // fits ~60 chars at 8px
pub const HUD_H: u32 = 12;  // 8px glyph + 2px padding top + 2px padding bottom

/// Per-swapchain overlay state.
pub struct OverlayState {
    pub staging_buffer: vk::Buffer,
    pub staging_memory: vk::DeviceMemory,
    pub staging_size: u64,
    pub command_pool: vk::CommandPool,
    pub command_buffers: Vec<vk::CommandBuffer>,
    pub fences: Vec<vk::Fence>,
    pub extent: vk::Extent2D,
    pub format: vk::Format,
    pub images: Vec<vk::Image>,
}

/// Find a memory type index that is HOST_VISIBLE + HOST_COHERENT.
unsafe fn find_host_visible_memory(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    requirements: &vk::MemoryRequirements,
) -> Option<u32> {
    let props = instance.get_physical_device_memory_properties(physical_device);
    let required = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
    for i in 0..props.memory_type_count {
        if (requirements.memory_type_bits & (1 << i)) != 0
            && props.memory_types[i as usize]
                .property_flags
                .contains(required)
        {
            return Some(i);
        }
    }
    None
}

impl OverlayState {
    /// Create overlay resources for the given swapchain images.
    pub unsafe fn new(
        instance: &ash::Instance,
        device: &ash::Device,
        physical_device: vk::PhysicalDevice,
        queue_family_index: u32,
        images: &[vk::Image],
        format: vk::Format,
        extent: vk::Extent2D,
    ) -> Option<Self> {
        let image_count = images.len();
        if image_count == 0 {
            return None;
        }

        // Staging buffer: 4 bytes per pixel (RGBA), HUD_W × HUD_H
        let staging_size = (HUD_W * HUD_H * 4) as u64;

        let buf_info = vk::BufferCreateInfo::default()
            .size(staging_size)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        let staging_buffer = device.create_buffer(&buf_info, None).ok()?;
        let mem_reqs = device.get_buffer_memory_requirements(staging_buffer);
        let mem_type = find_host_visible_memory(instance, physical_device, &mem_reqs)?;

        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_reqs.size)
            .memory_type_index(mem_type);

        let staging_memory = device.allocate_memory(&alloc_info, None).ok()?;
        device.bind_buffer_memory(staging_buffer, staging_memory, 0).ok()?;

        // Command pool + one command buffer per swapchain image
        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(queue_family_index);
        let command_pool = device.create_command_pool(&pool_info, None).ok()?;

        let cb_alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(image_count as u32);
        let command_buffers = device.allocate_command_buffers(&cb_alloc).ok()?;

        let mut fences = Vec::with_capacity(image_count);
        for _ in 0..image_count {
            let fence_info = vk::FenceCreateInfo::default()
                .flags(vk::FenceCreateFlags::SIGNALED);
            fences.push(device.create_fence(&fence_info, None).ok()?);
        }

        Some(Self {
            staging_buffer,
            staging_memory,
            staging_size,
            command_pool,
            command_buffers,
            fences,
            extent,
            format,
            images: images.to_vec(),
        })
    }

    /// CPU-rasterise `text` into the staging buffer and record a command buffer
    /// that copies it onto `images[image_index]`.
    pub unsafe fn record_overlay(
        &self,
        device: &ash::Device,
        image_index: usize,
        text: &str,
    ) -> Option<vk::CommandBuffer> {
        if image_index >= self.command_buffers.len() {
            return None;
        }

        // ── 1. CPU blit text into staging buffer ───────────────────────
        let ptr = device
            .map_memory(self.staging_memory, 0, self.staging_size, vk::MemoryMapFlags::empty())
            .ok()? as *mut u8;

        let row_stride = HUD_W * 4;
        let pixels = std::slice::from_raw_parts_mut(ptr, (HUD_W * HUD_H * 4) as usize);

        // Fill background: semi-transparent black (0,0,0,0xB0)
        for y in 0..HUD_H {
            for x in 0..HUD_W {
                let off = ((y * HUD_W + x) * 4) as usize;
                pixels[off]     = 0x00; // R
                pixels[off + 1] = 0x00; // G
                pixels[off + 2] = 0x00; // B
                pixels[off + 3] = 0xB0; // A
            }
        }

        // Render glyphs — 2px top padding
        let pad_top: u32 = 2;
        for (ci, ch) in text.bytes().enumerate() {
            let gx = (ci as u32) * font::GLYPH_W;
            if gx + font::GLYPH_W > HUD_W {
                break; // off-screen
            }
            let glyph = font::glyph(ch);
            for row in 0..font::GLYPH_H {
                let bits = glyph[row as usize];
                for col in 0..font::GLYPH_W {
                    if bits & (0x80 >> col) != 0 {
                        let px = gx + col;
                        let py = pad_top + row;
                        if py < HUD_H {
                            let off = ((py * HUD_W + px) * 4) as usize;
                            // Green-ish HUD text colour
                            pixels[off]     = 0x00; // R
                            pixels[off + 1] = 0xFF; // G
                            pixels[off + 2] = 0x66; // B
                            pixels[off + 3] = 0xFF; // A
                        }
                    }
                }
            }
        }

        device.unmap_memory(self.staging_memory);

        // ── 2. Record command buffer ───────────────────────────────────
        let cb = self.command_buffers[image_index];

        // Wait for the fence from any previous use of this CB
        let _ = device.wait_for_fences(&[self.fences[image_index]], true, u64::MAX);
        let _ = device.reset_fences(&[self.fences[image_index]]);

        device
            .reset_command_buffer(cb, vk::CommandBufferResetFlags::empty())
            .ok()?;

        let begin = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        device.begin_command_buffer(cb, &begin).ok()?;

        let image = self.images[image_index];

        // Transition: PRESENT_SRC_KHR → TRANSFER_DST_OPTIMAL
        let barrier_to_dst = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::MEMORY_READ)
            .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .old_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            );

        device.cmd_pipeline_barrier(
            cb,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier_to_dst],
        );

        // Copy staging buffer → image (top-left corner)
        let copy_w = HUD_W.min(self.extent.width);
        let copy_h = HUD_H.min(self.extent.height);
        let region = vk::BufferImageCopy::default()
            .buffer_offset(0)
            .buffer_row_length(HUD_W)
            .buffer_image_height(HUD_H)
            .image_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .layer_count(1),
            )
            .image_offset(vk::Offset3D { x: 4, y: 4, z: 0 }) // small margin
            .image_extent(vk::Extent3D {
                width: copy_w,
                height: copy_h,
                depth: 1,
            });

        device.cmd_copy_buffer_to_image(
            cb,
            self.staging_buffer,
            image,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            &[region],
        );

        // Transition back: TRANSFER_DST_OPTIMAL → PRESENT_SRC_KHR
        let barrier_to_present = vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
            .dst_access_mask(vk::AccessFlags::MEMORY_READ)
            .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
            .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            );

        device.cmd_pipeline_barrier(
            cb,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::BOTTOM_OF_PIPE,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier_to_present],
        );

        device.end_command_buffer(cb).ok()?;

        Some(cb)
    }
}
