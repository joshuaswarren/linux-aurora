#include <metal_stdlib>
using namespace metal;

struct SRGBVertex {
    float4 position [[position]];
    float4 color;
};

vertex SRGBVertex m5_srgb_vertex(uint i [[vertex_id]],
                                device const float4 *data [[buffer(0)]])
{
    return {data[2 * i], data[2 * i + 1]};
}

fragment float4 m5_srgb_store(SRGBVertex v [[stage_in]])
{
    return v.color;
}

fragment float4 m5_srgb_blend(SRGBVertex v [[stage_in]],
                              float4 previous [[color(0)]])
{
    return v.color * v.color.a + previous * (1.0f - v.color.a);
}
