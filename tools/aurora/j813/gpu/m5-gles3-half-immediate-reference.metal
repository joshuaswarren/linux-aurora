// SPDX-License-Identifier: MIT
// Independent native references for scalar-half immediate arithmetic.
#include <metal_stdlib>
using namespace metal;

kernel void m5_half_add_one(device const half4 *input [[buffer(0)]],
                            device half4 *output [[buffer(1)]],
                            uint id [[thread_position_in_grid]])
{
    output[id] = input[id] + half4(1.0h);
}

kernel void m5_half_add_one_live(device const half4 *input [[buffer(0)]],
                                 device half4 *output [[buffer(1)]],
                                 uint id [[thread_position_in_grid]])
{
    half4 x = input[id];
    output[2 * id] = x + half4(1.0h);
    output[2 * id + 1] = x;
}

kernel void m5_half_mul_two(device const half4 *input [[buffer(0)]],
                            device half4 *output [[buffer(1)]],
                            uint id [[thread_position_in_grid]])
{
    output[id] = input[id] * half4(2.0h);
}

kernel void m5_half_mul_two_live(device const half4 *input [[buffer(0)]],
                                 device half4 *output [[buffer(1)]],
                                 uint id [[thread_position_in_grid]])
{
    half4 x = input[id];
    output[2 * id] = x * half4(2.0h);
    output[2 * id + 1] = x;
}

kernel void m5_half_mul_constants(device const half4 *input [[buffer(0)]],
                                  device half4 *output [[buffer(1)]],
                                  uint id [[thread_position_in_grid]])
{
    output[id] = input[id] * half4(0.5h, 1.5h, 2.5h, 3.5h);
}

kernel void m5_half_add_constants(device const half4 *input [[buffer(0)]],
                                  device half4 *output [[buffer(1)]],
                                  uint id [[thread_position_in_grid]])
{
    output[id] = input[id] + half4(2.0h, -1.0h, 0.5h, 1.5h);
}
