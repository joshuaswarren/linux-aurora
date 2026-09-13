// SPDX-License-Identifier: MIT
// Independent native compiler witnesses; never substituted into CTS.
#include <metal_stdlib>
using namespace metal;

kernel void m5_half_saturate(device const half4 *input [[buffer(0)]],
                             device half4 *output [[buffer(1)]],
                             uint id [[thread_position_in_grid]])
{
    output[id] = saturate(input[id]);
}

kernel void m5_half_saturate_live(device const half4 *input [[buffer(0)]],
                                  device half4 *output [[buffer(1)]],
                                  uint id [[thread_position_in_grid]])
{
    half4 value = input[id];
    output[2 * id] = saturate(value);
    output[2 * id + 1] = value;
}
