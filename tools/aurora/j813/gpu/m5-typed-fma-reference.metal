// SPDX-License-Identifier: MIT
#include <metal_stdlib>
using namespace metal;

#define PROBE(name, expression) \
kernel void name(device const float4 *input [[buffer(0)]], \
                 device float4 *output [[buffer(1)]], \
                 uint id [[thread_position_in_grid]]) \
{ float4 x = input[2 * id], y = input[2 * id + 1]; output[id] = expression; }

PROBE(m5_fma_scale, fma(x, 0.5f, y))
PROBE(m5_fma_scale_live_x, fma(x, 0.5f, y) * x)
PROBE(m5_fma_scale_live_y, fma(x, 0.5f, y) * y)
PROBE(m5_fma_scale_negative_y, fma(x, 0.5f, -y))
PROBE(m5_fma_scale_two, fma(x, 2.0f, y))
