// SPDX-License-Identifier: MIT
// Native comparison/select witnesses, independent of the CTS program.
#include <metal_stdlib>
using namespace metal;

kernel void m5_half_min(device const half4 *a [[buffer(0)]],
                        device const half4 *b [[buffer(1)]],
                        device half4 *out [[buffer(2)]],
                        uint id [[thread_position_in_grid]])
{
    half4 x = a[id], y = b[id];
    out[id] = select(y, x, x < y);
}

kernel void m5_half_select(device const half4 *a [[buffer(0)]],
                           device const half4 *b [[buffer(1)]],
                           device const half4 *c [[buffer(2)]],
                           device const half4 *d [[buffer(3)]],
                           device half4 *out [[buffer(4)]],
                           uint id [[thread_position_in_grid]])
{
    out[id] = select(d[id], c[id], a[id] < b[id]);
}

kernel void m5_half_max(device const half4 *a [[buffer(0)]],
                        device const half4 *b [[buffer(1)]],
                        device half4 *out [[buffer(2)]],
                        uint id [[thread_position_in_grid]])
{
    half4 x = a[id], y = b[id];
    out[id] = select(y, x, x > y);
}
