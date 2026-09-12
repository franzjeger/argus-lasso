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
pub const HUD_W: u32 = 800; // fits ~50 chars at 16px
pub const HUD_H: u32 = 24;  // 16px glyph + padding

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
    pub transform: vk::SurfaceTransformFlagsKHR,
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
        physical_device: vk::PhysicalDevice,
        device: &ash::Device,
        queue_family_index: u32,
        images: &[vk::Image],
        format: vk::Format,
        extent: vk::Extent2D,
        transform: vk::SurfaceTransformFlagsKHR,
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
            transform,
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

        let pixels = std::slice::from_raw_parts_mut(ptr as *mut u32, (HUD_W * HUD_H) as usize);

        // Helper to pack a color based on format
        let pack_color = |r: u32, g: u32, b: u32, a: u32| -> u32 {
            match self.format {
                vk::Format::B8G8R8A8_UNORM | vk::Format::B8G8R8A8_SRGB => {
                    (a << 24) | (r << 16) | (g << 8) | b
                }
                vk::Format::R8G8B8A8_UNORM | vk::Format::R8G8B8A8_SRGB => {
                    (a << 24) | (b << 16) | (g << 8) | r
                }
                vk::Format::A2R10G10B10_UNORM_PACK32 => {
                    // A2 (2 bits), R10 (10 bits), G10 (10 bits), B10 (10 bits)
                    let a2 = (a >> 6) & 0x3;
                    let r10 = (r << 2) | (r >> 6);
                    let g10 = (g << 2) | (g >> 6);
                    let b10 = (b << 2) | (b >> 6);
                    (a2 << 30) | (r10 << 20) | (g10 << 10) | b10
                }
                vk::Format::A2B10G10R10_UNORM_PACK32 => {
                    let a2 = (a >> 6) & 0x3;
                    let r10 = (r << 2) | (r >> 6);
                    let g10 = (g << 2) | (g >> 6);
                    let b10 = (b << 2) | (b >> 6);
                    (a2 << 30) | (b10 << 20) | (g10 << 10) | r10
                }
                _ => {
                    // Default to B8G8R8A8
                    (a << 24) | (r << 16) | (g << 8) | b
                }
            }
        };

        let bg_color = pack_color(0, 0, 0, 0xB0);
        let text_color = pack_color(0, 0xFF, 0x66, 0xFF); // Greenish

        let get_px_idx = |x: u32, y: u32| -> usize {
            (y * HUD_W + x) as usize
        };

        // Fill background
        for y in 0..HUD_H {
            for x in 0..HUD_W {
                pixels[get_px_idx(x, y)] = bg_color;
            }
        }

        // Render glyphs
        let pad_top: u32 = 4;
        let scale: u32 = 2;
        for (ci, ch) in text.bytes().enumerate() {
            let gx = (ci as u32) * font::GLYPH_W * scale;
            if gx + font::GLYPH_W * scale > HUD_W {
                break; // off-screen
            }
            let glyph = font::glyph(ch);
            for row in 0..font::GLYPH_H {
                let bits = glyph[row as usize];
                for col in 0..font::GLYPH_W {
                    if (bits >> col) & 1 != 0 {
                        for sy in 0..scale {
                            for sx in 0..scale {
                                let px = gx + col * scale + sx;
                                let py = pad_top + row * scale + sy;
                                if py < HUD_H {
                                    pixels[get_px_idx(px, py)] = text_color;
                                }
                            }
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

        let copy_w = HUD_W.min(self.extent.width);
        let copy_h = HUD_H.min(self.extent.height);
        
        let offset = vk::Offset3D { x: 4, y: 4, z: 0 };

        let region = vk::BufferImageCopy::default()
            .buffer_offset(0)
            .buffer_row_length(HUD_W)
            .buffer_image_height(HUD_H)
            .image_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .layer_count(1),
            )
            .image_offset(offset)
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
