// SPDX-License-Identifier: MIT
// Native compiler references only; never substituted into Mesa or CTS.
#include <metal_stdlib>
using namespace metal;

kernel void m5_float_narrow(device const float4 *input [[buffer(0)]],
                            device half4 *output [[buffer(1)]],
                            uint id [[thread_position_in_grid]])
{
    output[id] = half4(input[id]);
}

kernel void m5_float_mul_narrow(device const float4 *input [[buffer(0)]],
                                device const float4 *scale [[buffer(1)]],
                                device half4 *output [[buffer(2)]],
                                uint id [[thread_position_in_grid]])
{
    output[id] = half4(input[id] * scale[id]);
}

kernel void m5_float_narrow_modifiers(device const float4 *input [[buffer(0)]],
                                     device half4 *output [[buffer(1)]],
                                     uint id [[thread_position_in_grid]])
{
    float4 f = input[id];
    output[id] = half4(f.x, -f.y, abs(f.z), -abs(f.w));
}

kernel void m5_float_narrow_saturate(device const float4 *input [[buffer(0)]],
                                    device half4 *output [[buffer(1)]],
                                    uint id [[thread_position_in_grid]])
{
    output[id] = half4(saturate(input[id]));
}
