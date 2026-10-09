/* Host-only bridge to the production scoreboard, with native RLD operands. */
#include "agx3_slot_core.h"
#include <stdlib.h>

int
cprog_slot_plan(unsigned input_halfs, const uint16_t *operands, unsigned count,
                uint8_t *slots, uint16_t *waits)
{
   if (input_halfs > 512 || count > 512 || !operands || !slots || !waits)
      return 0;
   struct agx3_slot_instr *instr = calloc(count ? count : 1, sizeof(*instr));
   struct agx3_slot_result *result = calloc(count ? count : 1, sizeof(*result));
   if (!instr || !result) {
      free(instr);
      free(result);
      return 0;
   }
   int ok = 0;
   for (unsigned i = 0; i < count; ++i) {
      const unsigned tps = operands[4 * i];
      const unsigned dest = operands[4 * i + 1];
      const unsigned halves = operands[4 * i + 2];
      const unsigned native_slot = operands[4 * i + 3];
      if ((halves != 1 && halves != 2) || dest + halves > 256 ||
          tps < 8 || tps + halves > 8 + input_halfs ||
          native_slot < 1 || native_slot > 6)
         goto out;
      instr[i].is_async = true;
      instr[i].preferred_slot = native_slot;
      for (unsigned h = 0; h < halves; ++h) {
         instr[i].src[instr[i].nr_srcs++] = AGX3_SLOT_TPS_TOKEN_BASE + tps + h;
         instr[i].dst[instr[i].nr_dsts++] = dest + h;
         instr[i].dst[instr[i].nr_dsts++] = AGX3_SLOT_TPS_TOKEN_BASE + tps + h;
      }
   }
   const struct agx3_slot_live_range live = {
      AGX3_SLOT_TPS_TOKEN_BASE + 8, input_halfs, 6,
   };
   if (!agx3_slot_assign_with_live(instr, count, 6, &live,
                                   input_halfs != 0, result))
      goto out;
   for (unsigned i = 0; i < count; ++i) {
      slots[i] = result[i].slot;
      waits[i] = result[i].wait_mask;
   }
   ok = 1;
out:
   free(instr);
   free(result);
   return ok;
}
