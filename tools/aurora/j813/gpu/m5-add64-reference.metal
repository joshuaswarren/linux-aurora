// SPDX-License-Identifier: MIT
#include <metal_stdlib>
using namespace metal;

kernel void m5_add64(device const ulong4 *input [[buffer(0)]],
                     device ulong2 *output [[buffer(1)]],
                     uint id [[thread_position_in_grid]])
{
   ulong4 v = input[id];
   output[id] = ulong2(v.x + v.y, v.z + v.w);
}

kernel void m5_add64_live(device const ulong4 *input [[buffer(0)]],
                          device ulong4 *output [[buffer(1)]],
                          uint id [[thread_position_in_grid]])
{
   ulong4 v = input[id];
   output[id] = ulong4(v.x + v.y, v.z + v.w, v.x + v.z, v.y + v.w);
}
