use ash::vk;

use crate::hud::{FrameStats, HudImage, HudWorker};
use argus_ipc::{OverlayConfig, TelemetryFrame};

pub const MAX_HUD_W: u32 = 2048;
pub const MAX_HUD_H: u32 = 1024;

pub struct OverlayState {
    pub swapchain: vk::SwapchainKHR,
    pub format: vk::Format,
    pub extent: vk::Extent2D,

    pub command_pool: vk::CommandPool,
    pub command_buffers: Vec<vk::CommandBuffer>,
    pub fences: Vec<vk::Fence>,

    // Staging for texture upload
    pub staging_buffers: Vec<vk::Buffer>,
    pub staging_memories: Vec<vk::DeviceMemory>,
    pub staging_size: u64,

    // Texture
    pub texture_image: vk::Image,
    pub texture_memory: vk::DeviceMemory,
    pub texture_view: vk::ImageView,
    pub sampler: vk::Sampler,

    // Pipeline
    pub render_pass: vk::RenderPass,
    pub descriptor_set_layout: vk::DescriptorSetLayout,
    pub descriptor_pool: vk::DescriptorPool,
    pub descriptor_set: vk::DescriptorSet, // Just one since the texture is shared
    pub pipeline_layout: vk::PipelineLayout,
    pub pipeline: vk::Pipeline,

    // Framebuffers
    pub image_views: Vec<vk::ImageView>,
    pub framebuffers: Vec<vk::Framebuffer>,

    pub queue_family: u32,
    pub draw_queue: Option<vk::Queue>,
    pub complete: Vec<vk::Semaphore>,
    pub disabled: bool,
    last_config: Option<OverlayConfig>,
    initialized: bool,
    last_update: Option<std::time::Instant>,
    pub stats: FrameStats,
    worker: HudWorker,
    uploaded: Option<std::sync::Arc<HudImage>>,
    hud_width: u32,
    hud_height: u32,
    graph_pixels: Vec<u32>,
    last_graph: Option<std::time::Instant>,
}

fn find_memory_type(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    type_filter: u32,
    properties: vk::MemoryPropertyFlags,
) -> Option<u32> {
    let mem_properties = unsafe { instance.get_physical_device_memory_properties(physical_device) };
    (0..mem_properties.memory_type_count).find(|&i| {
        (type_filter & (1 << i)) != 0
            && (mem_properties.memory_types[i as usize].property_flags & properties) == properties
    })
}

impl OverlayState {
    /// # Safety
    /// Device and physical device must belong to instance. Images must be live
    /// swapchain images with the supplied format/extent and graphics queue family.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn new(
        instance: &ash::Instance,
        physical_device: vk::PhysicalDevice,
        device: &ash::Device,
        queue_family_index: u32,
        images: &[vk::Image],
        format: vk::Format,
        extent: vk::Extent2D,
        _transform: vk::SurfaceTransformFlagsKHR,
    ) -> Option<Self> {
        let image_count = images.len();
        if image_count == 0 {
            return None;
        }

        let staging_size = (MAX_HUD_W * (MAX_HUD_H + 40) * 4) as u64;
        let mut staging_buffers = vec![];
        let mut staging_memories = vec![];

        for _ in 0..image_count {
            let buf_info = vk::BufferCreateInfo::default()
                .size(staging_size)
                .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
            let staging_buffer = device.create_buffer(&buf_info, None).ok()?;
            let mem_reqs = device.get_buffer_memory_requirements(staging_buffer);
            let mem_type = find_memory_type(
                instance,
                physical_device,
                mem_reqs.memory_type_bits,
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
            )?;
            let alloc_info = vk::MemoryAllocateInfo::default()
                .allocation_size(mem_reqs.size)
                .memory_type_index(mem_type);
            let staging_memory = device.allocate_memory(&alloc_info, None).ok()?;
            device
                .bind_buffer_memory(staging_buffer, staging_memory, 0)
                .ok()?;
            staging_buffers.push(staging_buffer);
            staging_memories.push(staging_memory);
        }

        // Texture
        let img_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .extent(vk::Extent3D {
                width: MAX_HUD_W,
                height: MAX_HUD_H,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .format(vk::Format::R8G8B8A8_UNORM)
            .tiling(vk::ImageTiling::OPTIMAL)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .samples(vk::SampleCountFlags::TYPE_1);
        let texture_image = device.create_image(&img_info, None).ok()?;
        let mem_reqs = device.get_image_memory_requirements(texture_image);
        let mem_type = find_memory_type(
            instance,
            physical_device,
            mem_reqs.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        )?;
        let alloc_info = vk::MemoryAllocateInfo::default()
            .allocation_size(mem_reqs.size)
            .memory_type_index(mem_type);
        let texture_memory = device.allocate_memory(&alloc_info, None).ok()?;
        device
            .bind_image_memory(texture_image, texture_memory, 0)
            .ok()?;

        let view_info = vk::ImageViewCreateInfo::default()
            .image(texture_image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .subresource_range(
                vk::ImageSubresourceRange::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .level_count(1)
                    .layer_count(1),
            );
        let texture_view = device.create_image_view(&view_info, None).ok()?;

        let sampler_info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::NEAREST)
            .min_filter(vk::Filter::NEAREST)
            .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE);
        let sampler = device.create_sampler(&sampler_info, None).ok()?;

        // RenderPass
        let attachment = vk::AttachmentDescription::default()
            .format(format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::LOAD) // Preserve game!
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .final_layout(vk::ImageLayout::PRESENT_SRC_KHR);

        let color_attachment_ref = vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(std::slice::from_ref(&color_attachment_ref));

        let dependency = vk::SubpassDependency::default()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .src_access_mask(vk::AccessFlags::empty())
            .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_access_mask(
                vk::AccessFlags::COLOR_ATTACHMENT_READ | vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            );

        let render_pass_info = vk::RenderPassCreateInfo::default()
            .attachments(std::slice::from_ref(&attachment))
            .subpasses(std::slice::from_ref(&subpass))
            .dependencies(std::slice::from_ref(&dependency));
        let render_pass = device.create_render_pass(&render_pass_info, None).ok()?;

        // Descriptor Layout
        let binding = vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::FRAGMENT);
        let layout_info =
            vk::DescriptorSetLayoutCreateInfo::default().bindings(std::slice::from_ref(&binding));
        let descriptor_set_layout = device
            .create_descriptor_set_layout(&layout_info, None)
            .ok()?;

        // Pipeline Layout
        let push_constant = vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(16); // 2 floats (offset x, y), 2 floats (scale x, y)
        let pipeline_layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(std::slice::from_ref(&descriptor_set_layout))
            .push_constant_ranges(std::slice::from_ref(&push_constant));
        let pipeline_layout = device
            .create_pipeline_layout(&pipeline_layout_info, None)
            .ok()?;

        // Pipeline
        let vert_code = include_bytes!(concat!(env!("OUT_DIR"), "/overlay.vert.spv"));
        let frag_code = include_bytes!(concat!(env!("OUT_DIR"), "/overlay.frag.spv"));
        let vert_module = device
            .create_shader_module(
                &vk::ShaderModuleCreateInfo::default()
                    .code(&ash::util::read_spv(&mut std::io::Cursor::new(vert_code)).ok()?),
                None,
            )
            .ok()?;
        let frag_module = device
            .create_shader_module(
                &vk::ShaderModuleCreateInfo::default()
                    .code(&ash::util::read_spv(&mut std::io::Cursor::new(frag_code)).ok()?),
                None,
            )
            .ok()?;

        let shader_stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vert_module)
                .name(c"main"),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(frag_module)
                .name(c"main"),
        ];

        let vertex_input_info = vk::PipelineVertexInputStateCreateInfo::default();
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let rasterizer = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .line_width(1.0);
        let multisampling = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);

        let color_blend_attachment = vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(
                vk::ColorComponentFlags::R
                    | vk::ColorComponentFlags::G
                    | vk::ColorComponentFlags::B,
            )
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
            .alpha_blend_op(vk::BlendOp::ADD);
        let color_blending = vk::PipelineColorBlendStateCreateInfo::default()
            .attachments(std::slice::from_ref(&color_blend_attachment));

        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic_state =
            vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

        let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&shader_stages)
            .vertex_input_state(&vertex_input_info)
            .input_assembly_state(&input_assembly)
            .viewport_state(&viewport_state)
            .rasterization_state(&rasterizer)
            .multisample_state(&multisampling)
            .color_blend_state(&color_blending)
            .dynamic_state(&dynamic_state)
            .layout(pipeline_layout)
            .render_pass(render_pass)
            .subpass(0);

        // .first() rather than [0]: a nonconformant ICD returning VK_SUCCESS
        // with fewer entries than requested would otherwise index-panic
        // instead of failing gracefully like every other fallible step here.
        let pipeline = *device
            .create_graphics_pipelines(
                vk::PipelineCache::null(),
                std::slice::from_ref(&pipeline_info),
                None,
            )
            .ok()?
            .first()?;
        device.destroy_shader_module(vert_module, None);
        device.destroy_shader_module(frag_module, None);

        // Framebuffers
        let mut image_views = vec![];
        let mut framebuffers = vec![];
        for img in images {
            let view_info = vk::ImageViewCreateInfo::default()
                .image(*img)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );
            let iv = device.create_image_view(&view_info, None).ok()?;
            image_views.push(iv);
            let fb_info = vk::FramebufferCreateInfo::default()
                .render_pass(render_pass)
                .attachments(std::slice::from_ref(&iv))
                .width(extent.width)
                .height(extent.height)
                .layers(1);
            framebuffers.push(device.create_framebuffer(&fb_info, None).ok()?);
        }

        // Descriptor Pool & Set
        let pool_size = vk::DescriptorPoolSize::default()
            .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .descriptor_count(1);
        let pool_info = vk::DescriptorPoolCreateInfo::default()
            .pool_sizes(std::slice::from_ref(&pool_size))
            .max_sets(1);
        let descriptor_pool = device.create_descriptor_pool(&pool_info, None).ok()?;

        let alloc_info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(descriptor_pool)
            .set_layouts(std::slice::from_ref(&descriptor_set_layout));
        let descriptor_set = *device.allocate_descriptor_sets(&alloc_info).ok()?.first()?;

        let img_info = vk::DescriptorImageInfo::default()
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
            .image_view(texture_view)
            .sampler(sampler);
        let write = vk::WriteDescriptorSet::default()
            .dst_set(descriptor_set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
            .image_info(std::slice::from_ref(&img_info));
        device.update_descriptor_sets(std::slice::from_ref(&write), &[]);

        // Commands
        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(queue_family_index);
        let command_pool = device.create_command_pool(&pool_info, None).ok()?;
        let cb_alloc = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(image_count as u32);
        let command_buffers = device.allocate_command_buffers(&cb_alloc).ok()?;
        if let Some(callback) = crate::LOADER_DATA
            .read()
            .unwrap()
            .get(&device.handle())
            .copied()
        {
            use ash::vk::Handle;
            for cb in &command_buffers {
                if callback(device.handle(), cb.as_raw() as *mut std::ffi::c_void)
                    != vk::Result::SUCCESS
                {
                    return None;
                }
            }
        }
        let mut fences = vec![];
        for _ in 0..image_count {
            fences.push(
                device
                    .create_fence(
                        &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                        None,
                    )
                    .ok()?,
            );
        }

        let complete = (0..image_count)
            .map(|_| device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None))
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        Some(Self {
            swapchain: vk::SwapchainKHR::null(),
            format,
            extent,
            command_pool,
            command_buffers,
            fences,
            staging_buffers,
            staging_memories,
            staging_size,
            texture_image,
            texture_memory,
            texture_view,
            sampler,
            render_pass,
            descriptor_set_layout,
            descriptor_pool,
            descriptor_set,
            pipeline_layout,
            pipeline,
            image_views,
            framebuffers,
            queue_family: queue_family_index,
            draw_queue: None,
            complete,
            disabled: false,
            last_config: None,
            initialized: false,
            last_update: None,
            stats: FrameStats::default(),
            worker: HudWorker::new(),
            uploaded: None,
            hud_width: 1,
            hud_height: 1,
            graph_pixels: Vec::new(),
            last_graph: None,
        })
    }

    /// # Safety
    /// All uses of these resources must have completed on the GPU. Call exactly
    /// once, with the device that created this state, before destroying that device.
    pub unsafe fn destroy(&self, device: &ash::Device) {
        let _ = device.device_wait_idle();
        for &s in &self.complete {
            device.destroy_semaphore(s, None);
        }
        for &f in &self.fences {
            device.destroy_fence(f, None);
        }
        for &fb in &self.framebuffers {
            device.destroy_framebuffer(fb, None);
        }
        for &iv in &self.image_views {
            device.destroy_image_view(iv, None);
        }
        device.destroy_pipeline(self.pipeline, None);
        device.destroy_pipeline_layout(self.pipeline_layout, None);
        device.destroy_descriptor_pool(self.descriptor_pool, None);
        device.destroy_descriptor_set_layout(self.descriptor_set_layout, None);
        device.destroy_render_pass(self.render_pass, None);
        device.destroy_sampler(self.sampler, None);
        device.destroy_image_view(self.texture_view, None);
        device.destroy_image(self.texture_image, None);
        device.free_memory(self.texture_memory, None);
        for &buf in &self.staging_buffers {
            device.destroy_buffer(buf, None);
        }
        for &mem in &self.staging_memories {
            device.free_memory(mem, None);
        }
        device.free_command_buffers(self.command_pool, &self.command_buffers);
        device.destroy_command_pool(self.command_pool, None);
    }
}

impl OverlayState {
    /// # Safety
    /// The image index must identify an acquired image; resources and device must
    /// remain alive, with no concurrent recording or in-flight reuse of the slot.
    pub unsafe fn record_overlay(
        &mut self,
        device: &ash::Device,
        image_index: usize,
        tel: Option<&TelemetryFrame>,
        status: &str,
        config: &OverlayConfig,
    ) -> Option<vk::CommandBuffer> {
        if image_index >= self.command_buffers.len() || !config.show_overlay {
            return None;
        }

        // Never wait for the GPU on the presentation thread. If resources are
        // still busy, pass the game's original presentation through unchanged.
        if self.disabled || !device.get_fence_status(self.fences[image_index]).ok()? {
            return None;
        }
        let now = std::time::Instant::now();
        if self.last_config.as_ref() != Some(config)
            || self
                .last_update
                .is_none_or(|last| now.duration_since(last).as_millis() >= 250)
        {
            self.worker.request(tel, status, config, &self.stats);
            self.last_update = Some(now);
            self.last_config = Some(config.clone());
        }
        let image = self.worker.image()?;
        let needs_update = self
            .uploaded
            .as_ref()
            .is_none_or(|old| !std::sync::Arc::ptr_eq(old, &image));
        let cb = self.command_buffers[image_index];
        device
            .reset_command_buffer(cb, vk::CommandBufferResetFlags::empty())
            .ok()?;
        device
            .begin_command_buffer(
                cb,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .ok()?;
        let graph_update = image.graph_y.is_some_and(|y| y + 40 <= MAX_HUD_H)
            && config.show_graph
            && (needs_update
                || self.last_graph.is_none_or(|last| {
                    now.duration_since(last).as_secs_f64()
                        >= 1.0 / config.graph_hz.clamp(30, 120) as f64
                }));
        if needs_update || graph_update {
            self.hud_width = image.width.min(MAX_HUD_W);
            self.hud_height = image.height.min(MAX_HUD_H);
            let ptr = device
                .map_memory(
                    self.staging_memories[image_index],
                    0,
                    self.staging_size,
                    vk::MemoryMapFlags::empty(),
                )
                .ok()? as *mut u32;
            if needs_update {
                for y in 0..self.hud_height as usize {
                    std::ptr::copy_nonoverlapping(
                        image.pixels.as_ptr().add(y * image.width as usize),
                        ptr.add(y * self.hud_width as usize),
                        self.hud_width as usize,
                    );
                }
            }
            if graph_update {
                self.stats
                    .paint_graph(now, self.hud_width, config, &mut self.graph_pixels);
                std::ptr::copy_nonoverlapping(
                    self.graph_pixels.as_ptr(),
                    ptr.add((MAX_HUD_W * MAX_HUD_H) as usize),
                    self.graph_pixels.len(),
                );
                self.last_graph = Some(now);
            }
            device.unmap_memory(self.staging_memories[image_index]);
            // Transition texture to TRANSFER_DST
            let barrier = vk::ImageMemoryBarrier::default()
                .old_layout(if self.initialized {
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
                } else {
                    vk::ImageLayout::UNDEFINED
                })
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_access_mask(if self.initialized {
                    vk::AccessFlags::SHADER_READ
                } else {
                    vk::AccessFlags::empty()
                })
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .image(self.texture_image)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );
            device.cmd_pipeline_barrier(
                cb,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );

            // Copy
            let region = vk::BufferImageCopy::default()
                .image_subresource(
                    vk::ImageSubresourceLayers::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .layer_count(1),
                )
                .image_extent(vk::Extent3D {
                    width: self.hud_width,
                    height: self.hud_height,
                    depth: 1,
                });
            if needs_update {
                device.cmd_copy_buffer_to_image(
                    cb,
                    self.staging_buffers[image_index],
                    self.texture_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
            }
            if graph_update && self.hud_height >= 40 + config.margin.min(32) {
                if needs_update {
                    let memory = vk::MemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                        .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE);
                    device.cmd_pipeline_barrier(
                        cb,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::PipelineStageFlags::TRANSFER,
                        vk::DependencyFlags::empty(),
                        &[memory],
                        &[],
                        &[],
                    );
                }
                let graph = vk::BufferImageCopy::default()
                    .buffer_offset((MAX_HUD_W * MAX_HUD_H * 4) as u64)
                    .image_subresource(
                        vk::ImageSubresourceLayers::default()
                            .aspect_mask(vk::ImageAspectFlags::COLOR)
                            .layer_count(1),
                    )
                    .image_offset(vk::Offset3D {
                        x: 0,
                        y: image.graph_y.unwrap() as i32,
                        z: 0,
                    })
                    .image_extent(vk::Extent3D {
                        width: self.hud_width,
                        height: 40,
                        depth: 1,
                    });
                device.cmd_copy_buffer_to_image(
                    cb,
                    self.staging_buffers[image_index],
                    self.texture_image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[graph],
                );
            }

            // Transition texture to SHADER_READ
            let barrier = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .image(self.texture_image)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1),
                );
            device.cmd_pipeline_barrier(
                cb,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::FRAGMENT_SHADER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier],
            );
        }

        // Ensure texture is in SHADER_READ on first frame if we didn't update it (handled above, it's always uploaded first frame)

        // Begin RenderPass
        let clear_value = vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [0.0, 0.0, 0.0, 0.0],
            },
        };
        let render_pass_begin_info = vk::RenderPassBeginInfo::default()
            .render_pass(self.render_pass)
            .framebuffer(self.framebuffers[image_index])
            .render_area(vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent: self.extent,
            })
            .clear_values(std::slice::from_ref(&clear_value));

        device.cmd_begin_render_pass(cb, &render_pass_begin_info, vk::SubpassContents::INLINE);

        device.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
        device.cmd_bind_descriptor_sets(
            cb,
            vk::PipelineBindPoint::GRAPHICS,
            self.pipeline_layout,
            0,
            &[self.descriptor_set],
            &[],
        );

        let viewport = vk::Viewport {
            x: 0.0,
            y: 0.0,
            width: self.extent.width as f32,
            height: self.extent.height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        device.cmd_set_viewport(cb, 0, &[viewport]);

        let max_x = self.extent.width.saturating_sub(self.hud_width) as i32;
        let max_y = self.extent.height.saturating_sub(self.hud_height) as i32;
        let dx = config.offset_x.clamp(0, max_x);
        let dy = config.offset_y.clamp(0, max_y);
        let x = if config.anchor % 2 == 1 {
            max_x - dx
        } else {
            dx
        };
        let y = if config.anchor >= 2 { max_y - dy } else { dy };
        let scissor = vk::Rect2D {
            offset: vk::Offset2D { x, y },
            extent: vk::Extent2D {
                width: self.hud_width.min(self.extent.width - x as u32),
                height: self.hud_height.min(self.extent.height - y as u32),
            },
        };
        device.cmd_set_scissor(cb, 0, &[scissor]);

        // Push constants for offset and scale
        let offset_x = x as f32 / self.extent.width as f32;
        let offset_y = y as f32 / self.extent.height as f32;
        let scale_x = MAX_HUD_W as f32 / self.extent.width as f32;
        let scale_y = MAX_HUD_H as f32 / self.extent.height as f32;

        let push_data: [f32; 4] = [offset_x, offset_y, scale_x, scale_y];
        device.cmd_push_constants(
            cb,
            self.pipeline_layout,
            vk::ShaderStageFlags::VERTEX,
            0,
            bytemuck::cast_slice(&push_data),
        );

        // Draw 4 vertices (TRIANGLE_STRIP for full HUD area)
        device.cmd_draw(cb, 4, 1, 0, 0);

        device.cmd_end_render_pass(cb);
        device.end_command_buffer(cb).ok()?;

        self.initialized = true;
        if needs_update {
            self.uploaded = Some(image);
        }
        Some(cb)
    }
}
