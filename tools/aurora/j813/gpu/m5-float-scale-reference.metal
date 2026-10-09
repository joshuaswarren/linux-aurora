// SPDX-License-Identifier: MIT
#include <metal_stdlib>
using namespace metal;

kernel void m5_float_scale(device const float4 *input [[buffer(0)]],
                           device float4 *output [[buffer(1)]],
                           uint id [[thread_position_in_grid]])
{
   float4 v = input[id];
   output[id] = v * float4(0.5f, 1.5f, 2.0f, 0.03125f);
}

kernel void m5_float_scale_live(device const float4 *input [[buffer(0)]],
                                device float4 *output [[buffer(1)]],
                                uint id [[thread_position_in_grid]])
{
   float4 v = input[id];
   output[2 * id] = v * float4(0.5f, 1.5f, 2.0f, 0.03125f);
   output[2 * id + 1] = v;
}
