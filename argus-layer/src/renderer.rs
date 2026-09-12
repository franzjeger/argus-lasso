use ash::vk;
use egui_ash_renderer::Renderer as EguiRenderer;
use gpu_allocator::vulkan::{Allocator, AllocatorCreateDesc};
use std::sync::Mutex;
use std::collections::HashMap;

pub struct SwapchainState {
    pub render_pass: vk::RenderPass,
    pub image_views: Vec<vk::ImageView>,
    pub framebuffers: Vec<vk::Framebuffer>,
    pub command_pool: vk::CommandPool,
    pub command_buffers: Vec<vk::CommandBuffer>,
    pub format: vk::Format,
    pub extent: vk::Extent2D,
}

lazy_static::lazy_static! {
    pub static ref EG_CONTEXTS: Mutex<HashMap<vk::SwapchainKHR, egui::Context>> = Mutex::new(HashMap::new());
}

pub fn create_swapchain_overlay(
    device: &ash::Device,
    swapchain: vk::SwapchainKHR,
    format: vk::Format,
    extent: vk::Extent2D,
    images: &[vk::Image],
) -> SwapchainState {
    unsafe {
        // Create RenderPass
        let attachment = vk::AttachmentDescription::default()
            .format(format)
            .samples(vk::SampleCountFlags::TYPE_1)
            .load_op(vk::AttachmentLoadOp::LOAD)
            .store_op(vk::AttachmentStoreOp::STORE)
            .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
            .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
            .initial_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            .final_layout(vk::ImageLayout::PRESENT_SRC_KHR)
            ;

        let color_attachment_ref = vk::AttachmentReference::default()
            .attachment(0)
            .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            ;

        let subpass = vk::SubpassDescription::default()
            .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
            .color_attachments(std::slice::from_ref(&color_attachment_ref))
            ;

        let dependency = vk::SubpassDependency::default()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .src_access_mask(vk::AccessFlags::empty())
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            ;

        let render_pass_info = vk::RenderPassCreateInfo::default()
            .attachments(std::slice::from_ref(&attachment))
            .subpasses(std::slice::from_ref(&subpass))
            .dependencies(std::slice::from_ref(&dependency))
            ;

        let render_pass = device.create_render_pass(&render_pass_info, None).unwrap();

        // Create ImageViews and Framebuffers
        let mut image_views = Vec::new();
        let mut framebuffers = Vec::new();

        for &image in images {
            let iv_info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(format)
                .components(vk::ComponentMapping::default())
                .subresource_range(
                    vk::ImageSubresourceRange::default()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .base_mip_level(0)
                        .level_count(1)
                        .base_array_layer(0)
                        .layer_count(1)
                        ,
                )
                ;
            
            let iv = device.create_image_view(&iv_info, None).unwrap();
            image_views.push(iv);

            let fb_info = vk::FramebufferCreateInfo::default()
                .render_pass(render_pass)
                .attachments(std::slice::from_ref(&iv))
                .width(extent.width)
                .height(extent.height)
                .layers(1)
                ;
            
            let fb = device.create_framebuffer(&fb_info, None).unwrap();
            framebuffers.push(fb);
        }

        // We assume queue family 0 for simplicity in this layer, robust layers would query the proper queue family
        let pool_info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
            .queue_family_index(0)
            ;
        
        let command_pool = device.create_command_pool(&pool_info, None).unwrap();

        let alloc_info = vk::CommandBufferAllocateInfo::default()
            .command_pool(command_pool)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(images.len() as u32)
            ;
        
        let command_buffers = device.allocate_command_buffers(&alloc_info).unwrap();

        let ctx = egui::Context::default();
        EG_CONTEXTS.lock().unwrap().insert(swapchain, ctx);

        SwapchainState {
            render_pass,
            image_views,
            framebuffers,
            command_pool,
            command_buffers,
            format,
            extent,
        }
    }
}
