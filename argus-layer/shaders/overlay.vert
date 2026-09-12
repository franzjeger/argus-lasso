#version 450
layout(location = 0) out vec2 outUV;

layout(push_constant) uniform PushConstants {
    vec2 offset;
    vec2 scale;
} pc;

void main() {
    // 4 vertices for a TRIANGLE_STRIP
    // 0: (0,0), 1: (1,0), 2: (0,1), 3: (1,1)
    outUV = vec2(gl_VertexIndex % 2, gl_VertexIndex / 2);
    
    vec2 pos = outUV * pc.scale + pc.offset;
    gl_Position = vec4(pos * 2.0f - 1.0f, 0.0f, 1.0f);
}
