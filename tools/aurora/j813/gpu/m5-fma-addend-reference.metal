// SPDX-License-Identifier: MIT
#include <metal_stdlib>
using namespace metal;
#define PROBE(name, expression) \
kernel void name(device const float4 *input [[buffer(0)]], \
                 device float4 *output [[buffer(1)]], \
                 uint id [[thread_position_in_grid]]) \
{ float4 x = input[2 * id], y = input[2 * id + 1]; output[id] = expression; }
PROBE(m5_fma_addhalf, fma(x, y, 0.5f))
PROBE(m5_fma_addhalf_live_x, fma(x, y, 0.5f) * x)
PROBE(m5_fma_addhalf_live_y, fma(x, y, 0.5f) * y)
PROBE(m5_fma_addneg, fma(x, y, -0.5f))
PROBE(m5_fma_addhalf_negx, fma(-x, y, 0.5f))
PROBE(m5_fma_addhalf_absx, fma(abs(x), y, 0.5f))
PROBE(m5_fma_addhalf_negy, fma(x, -y, 0.5f))
PROBE(m5_fma_addhalf_absy, fma(x, abs(y), 0.5f))
PROBE(m5_fma_twoimm, fma(x, 0.5f, 0.25f))
PROBE(m5_fma_twoimm_live, fma(x, 0.5f, 0.25f) * x)
PROBE(m5_fma_twoimm_neg, fma(x, -0.5f, -0.25f))
