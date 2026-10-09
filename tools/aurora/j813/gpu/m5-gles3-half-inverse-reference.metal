// SPDX-License-Identifier: MIT
// Independent native compiler witnesses; never substituted into CTS.
#include <metal_stdlib>
using namespace metal;

kernel void m5_half_inverse(device const half4 *input [[buffer(0)]],
                            device half4 *output [[buffer(1)]],
                            uint id [[thread_position_in_grid]])
{
    output[id] = half4(1.0h) - input[id];
}

kernel void m5_half_inverse_live(device const half4 *input [[buffer(0)]],
                                 device half4 *output [[buffer(1)]],
                                 uint id [[thread_position_in_grid]])
{
    half4 value = input[id];
    output[2 * id] = half4(1.0h) - value;
    output[2 * id + 1] = value;
}
