#version 450
layout(location = 0) in vec2 inUV;
layout(location = 0) out vec4 outColor;
layout(binding = 0) uniform sampler2D texSampler;

// The HUD texture holds display-encoded (sRGB) colours with premultiplied
// alpha. What the swapchain expects instead is chosen per pipeline:
//   0  the same sRGB-encoded values (UNORM target, sRGB colour space)
//   1  linear values, which an *_SRGB target encodes itself
//   2  HDR10: BT.2020 primaries, PQ (ST 2084) encoded
//   3  scRGB: linear BT.709 primaries, 1.0 = 80 nits
layout(constant_id = 0) const int TARGET_ENCODING = 0;
// How bright HUD white is on an HDR target, in nits.
layout(constant_id = 1) const float HUD_NITS = 203.0;

vec3 to_linear(vec3 c) {
    return mix(c / 12.92, pow((c + 0.055) / 1.055, vec3(2.4)), step(0.04045, c));
}

vec3 pq_encode(vec3 nits) {
    const float m1 = 0.1593017578125;
    const float m2 = 78.84375;
    const float c1 = 0.8359375;
    const float c2 = 18.8515625;
    const float c3 = 18.6875;
    vec3 y = pow(clamp(nits / 10000.0, 0.0, 1.0), vec3(m1));
    return pow((c1 + c2 * y) / (1.0 + c3 * y), vec3(m2));
}

// Linear BT.709 to linear BT.2020 (columns).
const mat3 BT709_TO_BT2020 = mat3(
    0.6274039, 0.0690973, 0.0163914,
    0.3292830, 0.9195404, 0.0880133,
    0.0433131, 0.0113623, 0.8955953);

void main() {
    vec4 color = texture(texSampler, inUV);
    if (TARGET_ENCODING != 0 && color.a > 0.0) {
        vec3 linear = to_linear(color.rgb / color.a);
        if (TARGET_ENCODING == 2) {
            linear = pq_encode(BT709_TO_BT2020 * linear * HUD_NITS);
        } else if (TARGET_ENCODING == 3) {
            linear *= HUD_NITS / 80.0;
        }
        color.rgb = linear * color.a;
    }
    outColor = color;
}
