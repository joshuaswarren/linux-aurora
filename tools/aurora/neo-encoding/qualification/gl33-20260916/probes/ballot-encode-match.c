#include <stdio.h>
#include <string.h>
#include "agx3_subgroup_core.h"
int main(void)
{
   const unsigned char expected[][10] = {
      {0x17,0x07,0x54,0x00,0x02,0x00,0x00,0x58,0x22,0x06},
      {0x17,0x07,0x54,0x04,0x03,0x00,0x02,0x18,0x22,0x06},
   };
   unsigned matches[2] = {0};
   for (unsigned src = 0; src < 16; src += 2)
   for (unsigned dst = 0; dst < 16; dst += 2)
   for (unsigned live = 0; live < 2; ++live)
   for (unsigned hint = 0; hint < 2; ++hint) {
      struct agx3_subgroup_ballot_desc d = {
         .type=AGX3_SUBGROUP_BALLOT_INTEGER, .src=src, .dest=dst,
         .src_live=live, .dest_cache=hint, .condition=5,
      };
      unsigned char bytes[AGX3_SUBGROUP_MAX_LENGTH], size;
      if (!agx3_subgroup_encode_ballot(AGX_ISA_ENCODING_AGX3B, &d, bytes, &size))
         continue;
      if (src == 0 && dst == 4 && live && hint) {
         printf("live candidate=");
         for (unsigned i=0; i<size; ++i) printf("%02x", bytes[i]);
         printf("\n");
      }
      for (unsigned e=0;e<2;++e)
         if (size == sizeof(expected[e]) && !memcmp(bytes, expected[e], size)) {
            printf("witness=%u src=%u dest=%u live=%u dest_cache=%u\n",e,src,dst,live,hint);
            matches[e]++;
         }
   }
   return matches[0] != 1 || matches[1] != 1;
}
