#version 450
layout(location = 0) in vec2 inUV;
layout(location = 0) out vec4 outColor;
layout(binding = 0) uniform sampler2D texSampler;

// The HUD texture holds display-encoded (sRGB) colours with premultiplied
// alpha. An *_SRGB swapchain encodes what the shader writes once more, so
// for those the colours are decoded to linear first.
layout(constant_id = 0) const bool TARGET_IS_SRGB = false;

vec3 to_linear(vec3 c) {
    return mix(c / 12.92, pow((c + 0.055) / 1.055, vec3(2.4)), step(0.04045, c));
}

void main() {
    vec4 color = texture(texSampler, inUV);
    if (TARGET_IS_SRGB && color.a > 0.0) {
        color.rgb = to_linear(color.rgb / color.a) * color.a;
    }
    outColor = color;
}
