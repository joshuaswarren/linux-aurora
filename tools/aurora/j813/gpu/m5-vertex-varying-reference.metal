#include <metal_stdlib>
using namespace metal;

struct ColoredVertex {
    float4 position [[position]];
    float4 color;
};
struct ColoredPointVertex {
    float4 position [[position]];
    float4 color;
    float pointSize [[point_size]];
};
vertex ColoredVertex m5_color_vertex(uint i [[vertex_id]],
                                    device const float4 *data [[buffer(0)]])
{
    return {data[2 * i], data[2 * i + 1]};
}
vertex ColoredPointVertex m5_color_point_vertex(uint i [[vertex_id]],
                                               device const float4 *data [[buffer(0)]])
{
    return {data[2 * i], data[2 * i + 1], 1.0f};
}
fragment float4 m5_color_fragment(ColoredVertex v [[stage_in]])
{
    return v.color;
}
fragment float4 m5_color_point_fragment(ColoredPointVertex v [[stage_in]])
{
    return v.color;
}
