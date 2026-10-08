#include "asahi/lib/agx_ioto_load_core.h"
#include <string.h>

int
cprog_layout_bridge(const uint64_t *words, uint16_t *count, uint16_t *frame,
                    uint16_t *offsets)
{
   struct agx_ioto_cprog_inputs inputs = {.known = true};
   memcpy(inputs.uniform_halfs, words, sizeof(inputs.uniform_halfs));
   if (!agx_ioto_cprog_input_frame(AGX_CHIP_G17P, &inputs, count, frame))
      return 0;
   for (unsigned h = 0; h < AGX_IOTO_CPROG_UNIFORM_HALFS; ++h) {
      offsets[h] = UINT16_MAX;
      agx_ioto_cprog_input_offset(AGX_CHIP_G17P, &inputs, h, &offsets[h]);
   }
   return 1;
}
