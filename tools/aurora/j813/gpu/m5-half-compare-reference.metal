// SPDX-License-Identifier: MIT
#include <metal_stdlib>
using namespace metal;

kernel void m5_half_compare_zero(device const ushort4 *input [[buffer(0)]],
                                device ushort4 *output [[buffer(1)]],
                                uint id [[thread_position_in_grid]])
{
   ushort4 v = input[id];
   output[id] = ushort4(v.x == 0, v.y != 0, v.z == 0, v.w != 0);
}

kernel void m5_half_compare_reg(device const ushort4 *input [[buffer(0)]],
                               device ushort4 *output [[buffer(1)]],
                               uint id [[thread_position_in_grid]])
{
   ushort4 v = input[id];
   output[id] = ushort4(v.x == v.y, v.x < v.y, v.z == v.w, v.z > v.w);
}

kernel void m5_half_compare_select32(device const ushort4 *input [[buffer(0)]],
                                    device const uint4 *values [[buffer(1)]],
                                    device uint4 *output [[buffer(2)]],
                                    uint id [[thread_position_in_grid]])
{
   ushort4 v = input[id];
   uint4 w = values[id];
   output[id] = uint4(v.x == 0 ? 1 : w.x, v.y == 0 ? w.y : 1,
                     v.z == 0 ? w.z : w.w, v.w == 0 ? 1 : 0);
}

kernel void m5_half_compare_one_select32(device const ushort4 *input [[buffer(0)]],
                                        device const uint4 *values [[buffer(1)]],
                                        device uint4 *output [[buffer(2)]],
                                        uint id [[thread_position_in_grid]])
{
   ushort4 v = input[id];
   uint4 w = values[id];
   output[id] = uint4(v.x == 1 ? 1 : w.x, v.y == 1 ? w.y : 1,
                     v.z == 1 ? w.z : w.w, v.w == 1 ? 1 : 0);
}
