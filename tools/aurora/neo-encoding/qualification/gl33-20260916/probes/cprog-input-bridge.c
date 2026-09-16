#include "asahi/compiler/agx3_ioto_load_packer.h"
#include <string.h>

int
cprog_input_bridge(uint16_t destination, uint16_t source, uint8_t mask,
                   int8_t wait_slot, uint8_t *bytes, uint8_t *length,
                   uint8_t *written)
{
   const struct agx3_ioto_cprog_copy_desc desc = {
      destination, source, mask, wait_slot,
   };
   struct agx3_ioto_cprog_copy_output out;
   if (!agx3_ioto_pack_cprog_copy(AGX_CHIP_G17P, &desc, &out))
      return 0;
   memcpy(bytes, out.bytes, sizeof(out.bytes));
   *length = out.length;
   *written = out.written_bytes;
   return 1;
}
