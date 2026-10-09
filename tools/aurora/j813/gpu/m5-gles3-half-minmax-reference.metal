// SPDX-License-Identifier: MIT
// Intrinsic min/max retain their native NaN comparison semantics.
#include <metal_stdlib>
using namespace metal;

kernel void m5_half_min_intrinsic(device const half4 *a [[buffer(0)]],
                                  device const half4 *b [[buffer(1)]],
                                  device half4 *out [[buffer(2)]],
                                  uint id [[thread_position_in_grid]])
{
    out[id] = min(a[id], b[id]);
}

kernel void m5_half_max_intrinsic(device const half4 *a [[buffer(0)]],
                                  device const half4 *b [[buffer(1)]],
                                  device half4 *out [[buffer(2)]],
                                  uint id [[thread_position_in_grid]])
{
    out[id] = max(a[id], b[id]);
}

kernel void m5_half_min_live(device const half4 *a [[buffer(0)]],
                             device const half4 *b [[buffer(1)]],
                             device half4 *out [[buffer(2)]],
                             uint id [[thread_position_in_grid]])
{
    half4 x = a[id], y = b[id];
    out[3 * id] = min(x, y);
    out[3 * id + 1] = x;
    out[3 * id + 2] = y;
}
