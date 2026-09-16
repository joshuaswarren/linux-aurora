#include "asahi/compiler/agx3_tex_core.h"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
/* Apple metal-nt applegpu_g17p fragment: three depth2d sample_compare refs. */
static const char *HEX =
"030007000200000020000e00000006000600060006000600060006000600060006000600060006000600060006000600060006000600060006000600060006002f0554000300040210002c9a233e060ccc044000af0054000300004820012f0554020302000210002f0554040304000210006c80233e00000008402019032f810001000009053f01000200000b8221041b8021044b8327004200000400005b8127004200000400008b032f004200000800009b012f00420000080000accd233e1204660a404087025400060005800c209000200000000000010015840c20100120000000000001000d800ca19025200001000000010097145400020010d045c2972454010204c0d044c88702540c0800e70654000000014e000000000702540c02000e000000";
static unsigned char text[1024]; static size_t textlen;
static int hits;
static void report(const char *what, const unsigned char *b, unsigned len, const char *desc) {
   for (size_t off = 0; off + len <= textlen; off++)
      if (!memcmp(text + off, b, len)) { printf("%-4s @0x%03zx len=%2u %s\n", what, off, len, desc); hits++; }
}
int main(void) {
   size_t n = strlen(HEX) / 2; textlen = n;
   for (size_t i = 0; i < n; i++) { unsigned v; sscanf(HEX + 2 * i, "%2x", &v); text[i] = v; }
   printf("text bytes %zu\n", n);
   unsigned char out[64]; uint8_t len; char desc[256];
   /* staging copies */
   for (unsigned dest = 0; dest < 256; dest += 2)
    for (unsigned src = 0; src < 128; src += 2)
     for (unsigned slot = 1; slot <= 6; slot++)
      for (int w = -1; w < 6; w++)
       for (unsigned hint = 0; hint < 3; hint++) {
         uint16_t wait = w < 0 ? 0 : (1u << w);
         if (!agx3_tex_stage_copy_hints(AGX_ISA_ENCODING_AGX3B, dest, src, slot, wait, hint == 1, hint == 2, out, &len)) continue;
         snprintf(desc, sizeof desc, "copy bank=%u src=r%u slot=%u wait=0x%x %s", dest, src, slot, wait, hint == 1 ? "cache" : hint == 2 ? "discard" : "");
         report("COPY", out, len, desc);
       }
   /* compare samples */
   for (unsigned abi = 0; abi < 2; abi++)
    for (unsigned dest = 0; dest < 128; dest += 2)
     for (unsigned coord = 0; coord < 64; coord += 2)
      for (unsigned slot = 1; slot <= 6; slot++)
       for (int w = -1; w < 6; w++)
        for (unsigned flags = 0; flags < 16; flags++)
         for (unsigned width = 0; width < 2; width++) {
           struct agx3_tex_sample_desc d; memset(&d, 0, sizeof d);
           d.dest = dest; d.coord = coord; d.width = width ? AGX3_TEX_W32 : AGX3_TEX_W16;
           d.mask = 1; d.texture = 0; d.sampler = 0; d.lod = AGX3_TEX_LOD_AUTO;
           d.slot = slot; d.wait_mask = w < 0 ? 0 : (1u << w);
           d.coord_kill = flags & 1; d.final = (flags >> 1) & 1;
           d.sample_group_nonterminal = (flags >> 2) & 1; d.sample_group_end = (flags >> 3) & 1;
           d.shadow = true; d.sample_abi = abi;
           if (!agx3_tex_encode_sample(AGX_ISA_ENCODING_AGX3B, &d, out, &len)) continue;
           snprintf(desc, sizeof desc, "smp abi=%u dest=r%u coord_bank=%u w%s slot=%u wait=0x%x kill=%u final=%u nonterm=%u end=%u", abi, dest, coord, width ? "32" : "16", slot, d.wait_mask, d.coord_kill, d.final, d.sample_group_nonterminal, d.sample_group_end);
           report("SMP", out, len, desc);
         }
   printf("hits %d\n", hits);
   return 0;
}
