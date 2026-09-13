// SPDX-License-Identifier: MIT
// Independent native compiler witnesses; never substituted into CTS.
#include <metal_stdlib>
using namespace metal;

kernel void m5_half_mul(device const half4 *a [[buffer(0)]],
                        device const half4 *b [[buffer(1)]],
                        device half4 *out [[buffer(2)]],
                        uint id [[thread_position_in_grid]])
{
    out[id] = a[id] * b[id];
}

kernel void m5_half_mul_live(device const half4 *a [[buffer(0)]],
                             device const half4 *b [[buffer(1)]],
                             device half4 *out [[buffer(2)]],
                             uint id [[thread_position_in_grid]])
{
    half4 x = a[id], y = b[id];
    out[3 * id] = x * y;
    out[3 * id + 1] = x;
    out[3 * id + 2] = y;
}

kernel void m5_half_add(device const half4 *a [[buffer(0)]],
                        device const half4 *b [[buffer(1)]],
                        device half4 *out [[buffer(2)]],
                        uint id [[thread_position_in_grid]])
{
    out[id] = a[id] + b[id];
}

kernel void m5_half_fma(device const half4 *a [[buffer(0)]],
                        device const half4 *b [[buffer(1)]],
                        device const half4 *c [[buffer(2)]],
                        device half4 *out [[buffer(3)]],
                        uint id [[thread_position_in_grid]])
{
    out[id] = fma(a[id], b[id], c[id]);
}

kernel void m5_half_mul_sat(device const half4 *a [[buffer(0)]],
                            device const half4 *b [[buffer(1)]],
                            device half4 *out [[buffer(2)]],
                            uint id [[thread_position_in_grid]])
{
    out[id] = saturate(a[id] * b[id]);
}

kernel void m5_half_add_sat(device const half4 *a [[buffer(0)]],
                            device const half4 *b [[buffer(1)]],
                            device half4 *out [[buffer(2)]],
                            uint id [[thread_position_in_grid]])
{
    out[id] = saturate(a[id] + b[id]);
}
