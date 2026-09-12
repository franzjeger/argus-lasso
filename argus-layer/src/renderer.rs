use ash::vk;
use std::sync::Arc;
use crate::font;
use argus_ipc::{OverlayConfig, TelemetryFrame};

pub const MAX_HUD_W: u32 = 800;
pub const MAX_HUD_H: u32 = 800;

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
    
    // Cache & Stats
    pub last_telemetry: Option<TelemetryFrame>,
    pub texture_needs_upload: bool,
    pub frame_times_ms: std::collections::VecDeque<f32>,
    pub last_frame_instant: Option<std::time::Instant>,
}

fn find_memory_type(
    instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    type_filter: u32,
    properties: vk::MemoryPropertyFlags,
) -> Option<u32> {
    let mem_properties = unsafe { instance.get_physical_device_memory_properties(physical_device) };
    for i in 0..mem_properties.memory_type_count {
        if (type_filter & (1 << i)) != 0
            && (mem_properties.memory_types[i as usize].property_flags & properties) == properties
        {
            return Some(i);
        }
    }
    None
}

impl OverlayState {
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
        if image_count == 0 { return None; }

        let staging_size = (MAX_HUD_W * MAX_HUD_H * 4) as u64;
        let mut staging_buffers = vec![];
        let mut staging_memories = vec![];

        for _ in 0..image_count {
            let buf_info = vk::BufferCreateInfo::default()
                .size(staging_size)
                .usage(vk::BufferUsageFlags::TRANSFER_SRC)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
            let staging_buffer = device.create_buffer(&buf_info, None).ok()?;
            let mem_reqs = device.get_buffer_memory_requirements(staging_buffer);
            let mem_type = find_memory_type(instance, physical_device, mem_reqs.memory_type_bits, vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT)?;
            let alloc_info = vk::MemoryAllocateInfo::default().allocation_size(mem_reqs.size).memory_type_index(mem_type);
            let staging_memory = device.allocate_memory(&alloc_info, None).ok()?;
            device.bind_buffer_memory(staging_buffer, staging_memory, 0).ok()?;
            staging_buffers.push(staging_buffer);
            staging_memories.push(staging_memory);
        }

        // Texture
        let img_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .extent(vk::Extent3D { width: MAX_HUD_W, height: MAX_HUD_H, depth: 1 })
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
        let mem_type = find_memory_type(instance, physical_device, mem_reqs.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)?;
        let alloc_info = vk::MemoryAllocateInfo::default().allocation_size(mem_reqs.size).memory_type_index(mem_type);
        let texture_memory = device.allocate_memory(&alloc_info, None).ok()?;
        device.bind_image_memory(texture_image, texture_memory, 0).ok()?;

        let view_info = vk::ImageViewCreateInfo::default()
            .image(texture_image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(vk::Format::R8G8B8A8_UNORM)
            .subresource_range(vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1));
        let texture_view = device.create_image_view(&view_info, None).ok()?;

        let sampler_info = vk::SamplerCreateInfo::default()
            .mag_filter(vk::Filter::LINEAR)
            .min_filter(vk::Filter::LINEAR)
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
        
        let color_attachment_ref = vk::AttachmentReference::default().attachment(0).layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(std::slice::from_ref(&color_attachment_ref));
        
        let dependency = vk::SubpassDependency::default()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .src_access_mask(vk::AccessFlags::empty())
            .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE);

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
        let layout_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(std::slice::from_ref(&binding));
        let descriptor_set_layout = device.create_descriptor_set_layout(&layout_info, None).ok()?;

        // Pipeline Layout
        let push_constant = vk::PushConstantRange::default()
            .stage_flags(vk::ShaderStageFlags::VERTEX)
            .offset(0)
            .size(16); // 2 floats (offset x, y), 2 floats (scale x, y)
        let pipeline_layout_info = vk::PipelineLayoutCreateInfo::default()
            .set_layouts(std::slice::from_ref(&descriptor_set_layout))
            .push_constant_ranges(std::slice::from_ref(&push_constant));
        let pipeline_layout = device.create_pipeline_layout(&pipeline_layout_info, None).ok()?;

        // Pipeline
        let vert_code = include_bytes!("../shaders/overlay.vert.spv");
        let frag_code = include_bytes!("../shaders/overlay.frag.spv");
        let vert_module = device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(std::slice::from_raw_parts(vert_code.as_ptr() as *const u32, vert_code.len() / 4)), None).ok()?;
        let frag_module = device.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(std::slice::from_raw_parts(frag_code.as_ptr() as *const u32, frag_code.len() / 4)), None).ok()?;

        let shader_stages = [
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::VERTEX).module(vert_module).name(c"main"),
            vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::FRAGMENT).module(frag_module).name(c"main"),
        ];

        let vertex_input_info = vk::PipelineVertexInputStateCreateInfo::default();
        let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default().topology(vk::PrimitiveTopology::TRIANGLE_STRIP);
        let viewport_state = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
        let rasterizer = vk::PipelineRasterizationStateCreateInfo::default().polygon_mode(vk::PolygonMode::FILL).cull_mode(vk::CullModeFlags::NONE).line_width(1.0);
        let multisampling = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);

        let color_blend_attachment = vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)
            .blend_enable(true)
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
            .alpha_blend_op(vk::BlendOp::ADD);
        let color_blending = vk::PipelineColorBlendStateCreateInfo::default().attachments(std::slice::from_ref(&color_blend_attachment));

        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic_state = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

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

        let pipeline = device.create_graphics_pipelines(vk::PipelineCache::null(), std::slice::from_ref(&pipeline_info), None).ok()?[0];
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
                .subresource_range(vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1));
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
        let pool_size = vk::DescriptorPoolSize::default().ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER).descriptor_count(1);
        let pool_info = vk::DescriptorPoolCreateInfo::default().pool_sizes(std::slice::from_ref(&pool_size)).max_sets(1);
        let descriptor_pool = device.create_descriptor_pool(&pool_info, None).ok()?;

        let alloc_info = vk::DescriptorSetAllocateInfo::default().descriptor_pool(descriptor_pool).set_layouts(std::slice::from_ref(&descriptor_set_layout));
        let descriptor_set = device.allocate_descriptor_sets(&alloc_info).ok()?[0];

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
        let pool_info = vk::CommandPoolCreateInfo::default().flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER).queue_family_index(queue_family_index);
        let command_pool = device.create_command_pool(&pool_info, None).ok()?;
        let cb_alloc = vk::CommandBufferAllocateInfo::default().command_pool(command_pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(image_count as u32);
        let command_buffers = device.allocate_command_buffers(&cb_alloc).ok()?;
        let mut fences = vec![];
        for _ in 0..image_count { fences.push(device.create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None).ok()?); }

        Some(Self {
            swapchain: vk::SwapchainKHR::null(), format, extent, command_pool, command_buffers, fences,
            staging_buffers, staging_memories, staging_size,
            texture_image, texture_memory, texture_view, sampler,
            render_pass, descriptor_set_layout, descriptor_pool, descriptor_set, pipeline_layout, pipeline,
            image_views, framebuffers, last_telemetry: None, texture_needs_upload: true,
            frame_times_ms: std::collections::VecDeque::with_capacity(1000), last_frame_instant: None,
        })
    }

    pub unsafe fn destroy(&self, device: &ash::Device) {
        let _ = device.device_wait_idle();
        for &f in &self.fences { device.destroy_fence(f, None); }
        for &fb in &self.framebuffers { device.destroy_framebuffer(fb, None); }
        for &iv in &self.image_views { device.destroy_image_view(iv, None); }
        device.destroy_pipeline(self.pipeline, None);
        device.destroy_pipeline_layout(self.pipeline_layout, None);
        device.destroy_descriptor_pool(self.descriptor_pool, None);
        device.destroy_descriptor_set_layout(self.descriptor_set_layout, None);
        device.destroy_render_pass(self.render_pass, None);
        device.destroy_sampler(self.sampler, None);
        device.destroy_image_view(self.texture_view, None);
        device.destroy_image(self.texture_image, None);
        device.free_memory(self.texture_memory, None);
        for &buf in &self.staging_buffers { device.destroy_buffer(buf, None); }
        for &mem in &self.staging_memories { device.free_memory(mem, None); }
        device.free_command_buffers(self.command_pool, &self.command_buffers);
        device.destroy_command_pool(self.command_pool, None);
    }
}

impl OverlayState {
    pub unsafe fn record_overlay(
        &mut self,
        device: &ash::Device,
        image_index: usize,
        tel: &TelemetryFrame,
        config: &OverlayConfig,
        fps: f64,
    ) -> Option<vk::CommandBuffer> {
        if image_index >= self.command_buffers.len() || !config.show_overlay {
            return None;
        }

        let fence = self.fences[image_index];
        let _ = device.wait_for_fences(&[fence], true, u64::MAX);
        let _ = device.reset_fences(&[fence]);

        let current_fps = fps as u32;

        // Frametime tracking
        let now = std::time::Instant::now();
        if let Some(last) = self.last_frame_instant {
            let ft = now.duration_since(last).as_secs_f32() * 1000.0;
            self.frame_times_ms.push_back(ft);
            if self.frame_times_ms.len() > 1000 {
                self.frame_times_ms.pop_front();
            }
        }
        self.last_frame_instant = Some(now);

        // Cache check
        let mut needs_update = self.texture_needs_upload;
        // Always update text if we have more than 10 frames to avoid feeling unresponsive, or just update every 10 frames minimum
        // wait, we can just update every 10th frame
        if self.frame_times_ms.len() % 10 == 0 {
            needs_update = true;
        }
        if self.last_telemetry.as_ref() != Some(tel) {
            needs_update = true;
            self.last_telemetry = Some(tel.clone());
        }

        let cb = self.command_buffers[image_index];
        device.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty()).ok()?;
        device.begin_command_buffer(cb, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)).ok()?;

        let mut actual_h = MAX_HUD_H;
        let mut actual_w = MAX_HUD_W;

        if needs_update {
            self.texture_needs_upload = false;

            let ptr = device.map_memory(self.staging_memories[image_index], 0, self.staging_size, vk::MemoryMapFlags::empty()).ok()? as *mut u8;
            let pixels = std::slice::from_raw_parts_mut(ptr as *mut u32, (MAX_HUD_W * MAX_HUD_H) as usize);
            
            let f = font::get_font();
            let scale = config.scale.max(1);
            let font_size = 14.0 * scale as f32;
            let pad_x = 10.0 * scale as f32;
            let pad_y = 10.0 * scale as f32;
            let line_h = font_size * 1.25;
            
            // Precalculate dimensions
            let actual_w = (360.0 * scale as f32) as u32;
            let actual_h = (pad_y + font_size + line_h * 15.5 + pad_y) as u32; // ~15.5 lines of text
            
            // Clear to transparent black
            for p in pixels.iter_mut() { *p = 0; }
            
            // Draw background rectangle
            let bg_color = config.bg_color;
            let bg_packed = (bg_color.3 as u32) << 24 | (bg_color.2 as u32) << 16 | (bg_color.1 as u32) << 8 | (bg_color.0 as u32);
            for y in 0..actual_h.min(MAX_HUD_H) {
                let row_offset = (y * MAX_HUD_W) as usize;
                for x in 0..actual_w.min(MAX_HUD_W) {
                    pixels[row_offset + x as usize] = bg_packed;
                }
            }
            
            let draw_text = |text: &str, mut cx: f32, cy: f32, size: f32, color: (u8,u8,u8), out_pixels: &mut [u32]| {
                for ch in text.chars() {
                    let (metrics, bitmap) = f.rasterize(ch, size);
                    let w = metrics.width as i32;
                    let h = metrics.height as i32;
                    let start_x = cx as i32 + metrics.xmin;
                    let start_y = cy as i32 - metrics.ymin - h;
                    
                    for r in 0..h {
                        for c in 0..w {
                            let coverage = bitmap[(r * w + c) as usize];
                            if coverage > 0 {
                                let px = start_x + c;
                                let py = start_y + r;
                                if px >= 0 && px < MAX_HUD_W as i32 && py >= 0 && py < MAX_HUD_H as i32 {
                                    let idx = (py as u32 * MAX_HUD_W + px as u32) as usize;
                                    let alpha = coverage as f32 / 255.0;
                                    let current_bg = out_pixels[idx];
                                    let bg_a = ((current_bg >> 24) & 0xFF) as f32 / 255.0;
                                    let bg_b = ((current_bg >> 16) & 0xFF) as f32;
                                    let bg_g = ((current_bg >> 8) & 0xFF) as f32;
                                    let bg_r = (current_bg & 0xFF) as f32;
                                    
                                    let final_r = (color.0 as f32 * alpha + bg_r * (1.0 - alpha)) as u32;
                                    let final_g = (color.1 as f32 * alpha + bg_g * (1.0 - alpha)) as u32;
                                    let final_b = (color.2 as f32 * alpha + bg_b * (1.0 - alpha)) as u32;
                                    let final_a = (255.0 * alpha.max(bg_a)) as u32;

                                    out_pixels[idx] = (final_a << 24) | (final_b << 16) | (final_g << 8) | final_r;
                                }
                            }
                        }
                    }
                    cx += metrics.advance_width;
                }
            };

            // Helpers
            let fmt_val = |v: Option<u32>| -> String { v.map(|x| x.to_string()).unwrap_or_else(|| "—".to_string()) };
            let fmt_f32 = |v: f32, zero_is_na: bool| -> String { if zero_is_na && v == 0.0 { "—".to_string() } else { format!("{v:.0}") } };
            
            // Stats calc
            let mut avg_fps = 0.0;
            let mut low_1 = 0.0;
            let mut cur_ft = 0.0;
            if !self.frame_times_ms.is_empty() {
                cur_ft = *self.frame_times_ms.back().unwrap();
                let mut sorted = self.frame_times_ms.iter().copied().collect::<Vec<_>>();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let avg_ft = sorted.iter().sum::<f32>() / sorted.len() as f32;
                avg_fps = 1000.0 / avg_ft;
                let idx_1low = (sorted.len() as f32 * 0.99) as usize;
                low_1 = 1000.0 / sorted[idx_1low.min(sorted.len() - 1)];
            }
            let cur_fps = if cur_ft > 0.0 { 1000.0 / cur_ft } else { 0.0 };

            let mut cur_y = pad_y + font_size;
            let line_h = font_size * 1.25;
            
            let c_lbl = (config.text_color.0, config.text_color.1, config.text_color.2);
            let c_val = (255, 255, 255);
            let c_dim = (150, 150, 150);

            // Columns (explicit X coords based on scale)
            let col0 = pad_x;
            let col1 = pad_x + 50.0 * scale as f32;
            let col2 = pad_x + 130.0 * scale as f32;
            let col3 = pad_x + 180.0 * scale as f32;
            let col4 = pad_x + 240.0 * scale as f32;
            let col5 = pad_x + 290.0 * scale as f32;

            // GPU Header
            draw_text("GPU", col0, cur_y, font_size, c_lbl, pixels);
            draw_text(&tel.gpu_name, col1, cur_y, font_size, c_val, pixels);
            cur_y += line_h;

            // GPU Row 1
            draw_text("Load", col0, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>3} %", fmt_f32(tel.gpu_usage_percent as f32, false)), col1, cur_y, font_size, c_val, pixels);
            draw_text("Temp", col2, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>3} °C", tel.gpu_temp_c), col3, cur_y, font_size, c_val, pixels);
            draw_text("Power", col4, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>3} W", fmt_f32(tel.gpu_power_w, true)), col5, cur_y, font_size, c_val, pixels);
            cur_y += line_h;

            // GPU Row 2
            draw_text("Core", col0, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>4} MHz", fmt_val(tel.gpu_core_clock_mhz)), col1, cur_y, font_size, c_val, pixels);
            draw_text("Mem", col2, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>4} MHz", fmt_val(tel.gpu_mem_clock_mhz)), col3, cur_y, font_size, c_val, pixels);
            draw_text("Fan", col4, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>3} %", fmt_val(tel.gpu_fan_speed_percent.map(|x| x as u32))), col5, cur_y, font_size, c_val, pixels);
            cur_y += line_h;

            // GPU Row 3
            draw_text("VRAM", col0, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>4.1} / {:.1} GiB", tel.vram_used_gb, tel.vram_total_gb), col1, cur_y, font_size, c_val, pixels);
            cur_y += line_h * 1.5;

            // CPU Header
            draw_text("CPU", col0, cur_y, font_size, c_lbl, pixels);
            draw_text(&tel.cpu_name, col1, cur_y, font_size, c_val, pixels);
            cur_y += line_h;

            // CPU Row 1
            draw_text("Load", col0, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>3} %", tel.cpu_usage_percent), col1, cur_y, font_size, c_val, pixels);
            draw_text("Temp", col2, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>3} °C", tel.cpu_temp_c), col3, cur_y, font_size, c_val, pixels);
            draw_text("Power", col4, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>3} W", fmt_f32(tel.cpu_power_w, true)), col5, cur_y, font_size, c_val, pixels);
            cur_y += line_h;

            // CPU Row 2
            draw_text("Clock", col0, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>4} MHz", fmt_val(tel.cpu_freq_mhz)), col1, cur_y, font_size, c_val, pixels);
            draw_text("Parked", col2, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>2}", tel.parked_cores), col3, cur_y, font_size, c_val, pixels);
            cur_y += line_h * 1.5;

            // RAM
            draw_text("RAM", col0, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>4.1} / {:.1} GiB", tel.ram_used_gb, tel.ram_total_gb), col1, cur_y, font_size, c_val, pixels);
            draw_text("Speed", col3, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>4} MT/s", fmt_val(tel.ram_speed_mts)), col4, cur_y, font_size, c_val, pixels);
            cur_y += line_h * 1.5;

            // FPS
            draw_text("FPS", col0, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>4.0}", cur_fps), col1, cur_y, font_size, (0, 255, 100), pixels);
            draw_text("Frame", col2, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>4.1} ms", cur_ft), col3, cur_y, font_size, c_val, pixels);
            cur_y += line_h;

            draw_text("AVG", col0, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>4.0}", avg_fps), col1, cur_y, font_size, c_val, pixels);
            draw_text("1% Low", col2, cur_y, font_size, c_lbl, pixels);
            draw_text(&format!("{:>4.0}", low_1), col3, cur_y, font_size, c_val, pixels);
            cur_y += line_h;

            device.unmap_memory(self.staging_memories[image_index]);

            // Transition texture to TRANSFER_DST
            let barrier = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_access_mask(vk::AccessFlags::empty())
                .dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .image(self.texture_image)
                .subresource_range(vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1));
            device.cmd_pipeline_barrier(cb, vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &[], &[], &[barrier]);

            // Copy
            let region = vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(1))
                .image_extent(vk::Extent3D { width: MAX_HUD_W, height: MAX_HUD_H, depth: 1 });
            device.cmd_copy_buffer_to_image(cb, self.staging_buffers[image_index], self.texture_image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[region]);

            // Transition texture to SHADER_READ
            let barrier = vk::ImageMemoryBarrier::default()
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::SHADER_READ)
                .image(self.texture_image)
                .subresource_range(vk::ImageSubresourceRange::default().aspect_mask(vk::ImageAspectFlags::COLOR).level_count(1).layer_count(1));
            device.cmd_pipeline_barrier(cb, vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::FRAGMENT_SHADER, vk::DependencyFlags::empty(), &[], &[], &[barrier]);
        }
        
        // Ensure texture is in SHADER_READ on first frame if we didn't update it (handled above, it's always uploaded first frame)
        
        // Begin RenderPass
        let clear_value = vk::ClearValue { color: vk::ClearColorValue { float32: [0.0, 0.0, 0.0, 0.0] } };
        let render_pass_begin_info = vk::RenderPassBeginInfo::default()
            .render_pass(self.render_pass)
            .framebuffer(self.framebuffers[image_index])
            .render_area(vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: self.extent })
            .clear_values(std::slice::from_ref(&clear_value));

        device.cmd_begin_render_pass(cb, &render_pass_begin_info, vk::SubpassContents::INLINE);

        device.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
        device.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::GRAPHICS, self.pipeline_layout, 0, &[self.descriptor_set], &[]);

        let viewport = vk::Viewport {
            x: 0.0,
            y: 0.0,
            width: self.extent.width as f32,
            height: self.extent.height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        device.cmd_set_viewport(cb, 0, &[viewport]);
        
        let scissor = vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: self.extent };
        device.cmd_set_scissor(cb, 0, &[scissor]);

        // Push constants for offset and scale
        let offset_x = config.offset_x as f32 / self.extent.width as f32;
        let offset_y = config.offset_y as f32 / self.extent.height as f32;
        let scale_x = MAX_HUD_W as f32 / self.extent.width as f32;
        let scale_y = MAX_HUD_H as f32 / self.extent.height as f32;
        
        let push_data: [f32; 4] = [offset_x, offset_y, scale_x, scale_y];
        device.cmd_push_constants(cb, self.pipeline_layout, vk::ShaderStageFlags::VERTEX, 0, bytemuck::cast_slice(&push_data));

        // Draw 4 vertices (TRIANGLE_STRIP for full HUD area)
        device.cmd_draw(cb, 4, 1, 0, 0);

        device.cmd_end_render_pass(cb);
        device.end_command_buffer(cb).ok()?;

        Some(cb)
    }
}
