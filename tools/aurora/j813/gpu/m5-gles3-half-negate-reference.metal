// SPDX-License-Identifier: MIT
// Independent strict-arithmetic references for half-register subtraction.
#include <metal_stdlib>
using namespace metal;

#define HALF_REFERENCE(name, expression) \
kernel void name(device const half4 *input [[buffer(0)]], \
                 device half4 *output [[buffer(1)]], \
                 uint id [[thread_position_in_grid]]) \
{ \
    half4 x = input[2 * id]; \
    half4 y = input[2 * id + 1]; \
    output[id] = expression; \
}

#define HALF_LIVE_REFERENCE(name, expression) \
kernel void name(device const half4 *input [[buffer(0)]], \
                 device half4 *output [[buffer(1)]], \
                 uint id [[thread_position_in_grid]]) \
{ \
    half4 x = input[2 * id]; \
    half4 y = input[2 * id + 1]; \
    output[3 * id] = expression; \
    output[3 * id + 1] = x; \
    output[3 * id + 2] = y; \
}

HALF_REFERENCE(m5_half_sub_xy, x - y)
HALF_REFERENCE(m5_half_sub_yx, y - x)
HALF_REFERENCE(m5_half_neg_both, -x - y)
HALF_REFERENCE(m5_half_sub_sat, saturate(x - y))
HALF_LIVE_REFERENCE(m5_half_sub_xy_live, x - y)
HALF_LIVE_REFERENCE(m5_half_sub_yx_live, y - x)
HALF_LIVE_REFERENCE(m5_half_neg_both_live, -x - y)
HALF_LIVE_REFERENCE(m5_half_sub_sat_live, saturate(x - y))
