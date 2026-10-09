#include "asahi/compiler/agx3_tex_core.h"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
static unsigned char text[4096]; static size_t textlen; static int hits;
static void report(const char *what, const unsigned char *b, unsigned len, const char *desc) {
   for (size_t off = 0; off + len <= textlen; off++)
      if (!memcmp(text + off, b, len)) { printf("%-8s @0x%03zx len=%2u %s\n", what, off, len, desc); hits++; }
}
int main(int argc, char **argv) {
   const char *hex = argv[1]; size_t n = strlen(hex) / 2; textlen = n;
   for (size_t i = 0; i < n; i++) { unsigned v; sscanf(hex + 2 * i, "%2x", &v); text[i] = v; }
   printf("text bytes %zu\n", n);
   unsigned char out[64]; uint8_t len; char desc[256];
   /* staging copies */
   for (unsigned dest = 0; dest < 64; dest += 2) for (unsigned src = 0; src < 128; src += 2)
    for (unsigned slot = 1; slot <= 6; slot++) for (int w = -1; w < 6; w++) for (unsigned hint = 0; hint < 3; hint++) {
      uint16_t wait = w < 0 ? 0 : (1u << w);
      if (!agx3_tex_stage_copy_hints(AGX_ISA_ENCODING_AGX3B, dest, src, slot, wait, hint == 1, hint == 2, out, &len)) continue;
      snprintf(desc, sizeof desc, "bank=%u src=r%u slot=%u wait=0x%x %s", dest, src, slot, wait, hint == 1 ? "cache" : hint == 2 ? "discard" : "");
      report("COPY", out, len, desc);
   }
   /* typed lod staging (pack_smpir) */
   for (unsigned src = 0; src < 256; src += 1) for (unsigned slot = 1; slot <= 6; slot++) for (int w = -1; w < 6; w++)
    for (unsigned hint = 0; hint < 3; hint++) for (unsigned kind = 1; kind <= 3; kind++) {
      uint16_t wait = w < 0 ? 0 : (1u << w);
      if (!agx3_tex_stage_typed_lod(AGX_ISA_ENCODING_AGX3B, src, slot, wait, hint == 1, hint == 2, kind, out, &len)) continue;
      snprintf(desc, sizeof desc, "src=h%u slot=%u wait=0x%x kind=%u %s", src, slot, wait, kind, hint == 1 ? "cache" : hint == 2 ? "discard" : "");
      report("SMPIR", out, len, desc);
   }
   /* texel fetch */
   for (unsigned dest = 0; dest < 128; dest += 2) for (unsigned coord = 0; coord < 64; coord += 2)
    for (unsigned slot = 1; slot <= 6; slot++) for (int w = -1; w < 6; w++) for (unsigned flags = 0; flags < 8; flags++)
     for (unsigned mask = 1; mask < 16; mask += (mask == 1 ? 14 : 1)) {
      struct agx3_tex_fetch_desc d; memset(&d, 0, sizeof d);
      d.dest = dest; d.coord = coord; d.width = AGX3_TEX_W32; d.mask = mask; d.texture = 0;
      d.slot = slot; d.wait_mask = w < 0 ? 0 : (1u << w); d.coord_kill = flags & 1; d.final = (flags >> 1) & 1;
      d.register_lod = (flags >> 2) & 1;
      if (!agx3_tex_encode_fetch(AGX_ISA_ENCODING_AGX3B, &d, out, &len)) continue;
      snprintf(desc, sizeof desc, "dest=r%u coord_bank=%u mask=%u slot=%u wait=0x%x kill=%u final=%u reglod=%u", dest, coord, mask, slot, d.wait_mask, d.coord_kill, d.final, d.register_lod);
      report("FETCH", out, len, desc);
   }
   printf("hits %d\n", hits); return 0;
}
