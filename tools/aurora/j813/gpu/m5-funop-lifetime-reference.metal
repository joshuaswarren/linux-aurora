// SPDX-License-Identifier: MIT
#include <metal_stdlib>
using namespace metal;

#define ROUND_PROBE(name, op) \
kernel void name(device const float4 *input [[buffer(0)]], \
                 device float4 *output [[buffer(1)]], \
                 uint id [[thread_position_in_grid]]) \
{ float4 x = input[id]; output[id] = op(x) * x; }
ROUND_PROBE(m5_floor_live, floor)
ROUND_PROBE(m5_ceil_live, ceil)
ROUND_PROBE(m5_trunc_live, trunc)
ROUND_PROBE(m5_rint_live, rint)

kernel void m5_log2_live(device const float4 *input [[buffer(0)]],
                         device float4 *output [[buffer(1)]],
                         uint id [[thread_position_in_grid]])
{
   float4 x = input[id];
   output[id] = log2(x) * x;
}

kernel void m5_log2_dead(device const float4 *input [[buffer(0)]],
                         device float4 *output [[buffer(1)]],
                         uint id [[thread_position_in_grid]])
{
   output[id] = log2(input[id]);
}

kernel void m5_exp2_live(device const float4 *input [[buffer(0)]],
                         device float4 *output [[buffer(1)]],
                         uint id [[thread_position_in_grid]])
{
   float4 x = input[id];
   output[id] = exp2(x) * x;
}

kernel void m5_exp2_dead(device const float4 *input [[buffer(0)]],
                         device float4 *output [[buffer(1)]],
                         uint id [[thread_position_in_grid]])
{
   output[id] = exp2(input[id]);
}
