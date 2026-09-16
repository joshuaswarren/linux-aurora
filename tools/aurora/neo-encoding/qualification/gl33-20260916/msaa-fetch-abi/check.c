#include "asahi/compiler/agx3_tex_core.h"
#include "asahi/compiler/agx3_compose/agx3_compose.h"
#include <stdio.h>
#include <string.h>

static void inspect_sample_staging(void)
{
   const unsigned char native[2][12] = {
      {0x17,0x05,0x54,0x04,0x00,0x00,0x00,0x00,0x50,0x40,0x08,0x00},
      {0x17,0x05,0x54,0x04,0x00,0x28,0x00,0x00,0xd0,0x40,0x08,0x00},
   };
   unsigned matches[2] = {0};
   for (unsigned src = 0; src < 256; ++src) {
      for (unsigned flags = 0; flags < 3; ++flags) {
         for (unsigned wait = 0; wait <= 32; wait = wait ? wait * 2 : 1) {
            uint8_t bytes[AGX3_TEX_MAX_LENGTH], size;
            if (!agx3_tex_stage_ms_sample(AGX_ISA_ENCODING_AGX3B, src, wait,
                                        flags == 1, flags == 2, bytes, &size))
               continue;
            for (unsigned n = 0; n < 2; ++n) {
               if (size == 12 && !memcmp(bytes, native[n], size)) {
                  fprintf(stderr, "sample-stage native=%u src=%u cache=%u discard=%u wait=%u\n",
                          n, src, flags == 1, flags == 2, wait);
                  ++matches[n];
               }
            }
         }
      }
   }
   fprintf(stderr, "sample-stage matches zero=%u variable=%u\n", matches[0], matches[1]);
}

static unsigned inspect_word = 4, inspect_bit = 8;
static void inspect_node(const agx3_table *t, unsigned index)
{
   const agx3_node *n = &t->nodes[index];
   for (unsigned i = 0; i < n->cmd_n; ++i) {
      const agx3_cmd *c = &t->cmds[n->cmd_off + i];
      if (c->word == inspect_word && c->dst_shift <= inspect_bit &&
          c->dst_shift + c->width > inspect_bit) {
         fprintf(stderr, "field-map word=%u bit=%u node=%u field=%u dst=%u width=%u src=%u lut=%d n=%u\n",
                 inspect_word, inspect_bit, index, c->field, c->dst_shift, c->width, c->src_shift, c->lut_off, c->lut_n);
      }
   }
   for (unsigned i = 0; i < n->fixed_n; ++i) {
      const agx3_fixed *f = &t->fixeds[n->fixed_off + i];
      if (f->word == inspect_word && (f->mask & (1u << inspect_bit)))
         fprintf(stderr, "field-map word=%u bit=%u node=%u fixed=%u\n",
                 inspect_word, inspect_bit, index, !!(f->value & (1u << inspect_bit)));
   }
   for (unsigned i = 0; i < n->grp_n; ++i) {
      const agx3_group *g = &t->groups[n->grp_off + i];
      for (unsigned j = 0; j < g->alt_n; ++j)
         inspect_node(t, t->altpool[g->alt_off + j]);
   }
}

int main(void)
{
   const int form = agx3_form_index(&agx3b_table, "smp", 3);
   inspect_node(&agx3b_table, agx3b_table.forms[form].node);
   const int pack = agx3_form_index(&agx3b_table, "pack_smpir", 0);
   inspect_bit = 7;
   inspect_node(&agx3b_table, agx3b_table.forms[pack].node);
   inspect_bit = 15;
   inspect_node(&agx3b_table, agx3b_table.forms[pack].node);
   inspect_word = 2;
   inspect_bit = 11;
   inspect_node(&agx3b_table, agx3b_table.forms[pack].node);
   inspect_sample_staging();
   struct agx3_tex_fetch_desc desc = {
      .dest = 8, .coord = 0, .width = AGX3_TEX_W32, .mask = 1,
      .slot = 1, .wait_mask = 1, .coord_kill = true, .final = true,
      .multisample = true,
   };
   uint8_t bytes[AGX3_TEX_MAX_LENGTH], size;
   for (unsigned texture = 0; texture < 16; ++texture) {
   desc.texture = texture;
   for (unsigned wait = 1; wait <= 4; wait *= 2) {
      desc.wait_mask = wait;
      if (!agx3_tex_encode_fetch(AGX_ISA_ENCODING_AGX3B, &desc, bytes, &size)) {
         printf("%u:%u unsupported\n", texture, wait);
         continue;
      }
      printf("%u:%u ", texture, wait);
      for (unsigned i = 0; i < size; ++i)
         printf("%02x", bytes[i]);
      puts("");
   }
   }
   return 0;
}
