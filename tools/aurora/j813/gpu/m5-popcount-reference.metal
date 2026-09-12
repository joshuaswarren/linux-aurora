// SPDX-License-Identifier: MIT
#include <metal_stdlib>
using namespace metal;

kernel void m5_popcount_u32(device const uint4 *input [[buffer(0)]],
                           device uint4 *output [[buffer(1)]],
                           uint id [[thread_position_in_grid]])
{
   output[id] = popcount(input[id]);
}

kernel void m5_popcount_u16(device const ushort4 *input [[buffer(0)]],
                           device ushort4 *output [[buffer(1)]],
                           uint id [[thread_position_in_grid]])
{
   output[id] = popcount(input[id]);
}

kernel void m5_popcount_live(device const uint4 *input [[buffer(0)]],
                            device uint4 *output [[buffer(1)]],
                            uint id [[thread_position_in_grid]])
{
   uint4 v = input[id];
   output[id] = popcount(v) + v;
}
