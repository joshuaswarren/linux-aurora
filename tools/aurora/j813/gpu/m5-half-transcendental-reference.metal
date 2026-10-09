// SPDX-License-Identifier: MIT
#include <metal_stdlib>
using namespace metal;

#define PROBE(name, type, expression) \
kernel void name(device const type##4 *input [[buffer(0)]], \
                 device type##4 *output [[buffer(1)]], \
                 uint id [[thread_position_in_grid]]) \
{ type##4 x = input[id]; output[id] = expression; }

PROBE(m5_half_log2, half, log2(x))
PROBE(m5_half_log2_live, half, log2(x) * x)
PROBE(m5_half_exp2, half, exp2(x))
PROBE(m5_half_exp2_live, half, exp2(x) * x)
PROBE(m5_half_rsqrt, half, rsqrt(x))
PROBE(m5_half_rsqrt_live, half, rsqrt(x) * x)
PROBE(m5_float_rsqrt, float, rsqrt(x))
PROBE(m5_float_rsqrt_live, float, rsqrt(x) * x)
PROBE(m5_half_sqrt, half, sqrt(x))
PROBE(m5_float_sqrt, float, sqrt(x))
PROBE(m5_float_sqrt_abs, float, sqrt(abs(x)))
PROBE(m5_half_sqrt_abs, half, sqrt(abs(x)))

#define MIXED_PROBE(name, in_type, out_type, op) \
kernel void name(device const in_type##4 *input [[buffer(0)]], \
                 device out_type##4 *output [[buffer(1)]], \
                 uint id [[thread_position_in_grid]]) \
{ output[id] = out_type##4(op(float4(input[id]))); }
MIXED_PROBE(m5_half_log2_to_float, half, float, log2)
MIXED_PROBE(m5_float_log2_to_half, float, half, log2)
MIXED_PROBE(m5_half_exp2_to_float, half, float, exp2)
MIXED_PROBE(m5_float_exp2_to_half, float, half, exp2)

kernel void m5_float_mul_to_half(device const float4 *input [[buffer(0)]],
                                 device half4 *output [[buffer(1)]],
                                 uint id [[thread_position_in_grid]])
{
   output[id] = half4(input[id] * 0.5f);
}
