/* Experimental AVE ring descriptor codec, inferred from 25G76 AppleAVE2.
 * This encodes bytes only. A Linux driver still needs DMA mapping, cache
 * maintenance, publication barriers, ownership tracking and doorbells.
 */
#ifndef AURORA_NEO_AVE2_WIRE_H
#define AURORA_NEO_AVE2_WIRE_H

#include <stdint.h>
#include <stddef.h>

#define AVE2_SLOT_BYTES 64

struct ave2_message {
    uint64_t address;
    uint32_t length;
    uint32_t flags;
    uint8_t control_bits;
};

static inline void ave2_put_le(uint8_t *p, uint64_t value, unsigned bytes)
{
    for (unsigned i = 0; i < bytes; ++i)
        p[i] = (uint8_t)(value >> (8 * i));
}

static inline uint64_t ave2_get_le(const uint8_t *p, unsigned bytes)
{
    uint64_t value = 0;
    for (unsigned i = 0; i < bytes; ++i)
        value |= (uint64_t)p[i] << (8 * i);
    return value;
}

/* Preserve slot padding, just as the observed driver does. No writes occur
 * on invalid input. For narrow slots the caller supplies a 32-bit address.
 * Modes 0/1 are the bidirectional modes being qualified here.
 */
static inline int ave2_slot_encode(uint8_t slot[AVE2_SLOT_BYTES], unsigned bits,
                                   unsigned mode, uint64_t address,
                                   uint32_t length, uint32_t flags)
{
    if (!slot || (bits != 32 && bits != 64) || mode > 1 || (address & 3) ||
        (bits == 32 && address > UINT32_MAX))
        return -1;
    unsigned width = bits / 8;
    ave2_put_le(slot + width, length, width);
    ave2_put_le(slot + width * 2, flags, width);
    ave2_put_le(slot, address | (mode ^ 1u), width);
    return 0;
}

static inline int ave2_slot_decode(const uint8_t slot[AVE2_SLOT_BYTES], unsigned bits,
                                   struct ave2_message *out)
{
    if (!slot || !out || (bits != 32 && bits != 64))
        return -1;
    unsigned width = bits / 8;
    uint64_t word = ave2_get_le(slot, width);
    uint64_t length = ave2_get_le(slot + width, width);
    uint64_t flags = ave2_get_le(slot + width * 2, width);
    if (length > UINT32_MAX || flags > UINT32_MAX)
        return -1;
    *out = (struct ave2_message){
        .address = word & ~UINT64_C(3), .length = (uint32_t)length,
        .flags = (uint32_t)flags, .control_bits = (uint8_t)(word & 3),
    };
    return 0;
}

#endif
