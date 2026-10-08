#include <stdio.h>
#include <stdlib.h>
#include "agx3_compose.h"

static unsigned seen[100000];
static void walk(unsigned index, unsigned field)
{
   if (index >= 100000 || seen[index])
      return;
   seen[index] = 1;
   const agx3_node *n = &agx3b_table.nodes[index];
   for (unsigned i = 0; i < n->cmd_n; ++i) {
      const agx3_cmd *c = &agx3b_table.cmds[n->cmd_off + i];
      if (c->field == field)
         printf("%u %u %u %u %d %u\n", c->word, c->dst_shift,
                c->width, c->src_shift, c->lut_off, c->lut_n);
   }
   for (unsigned i = 0; i < n->grp_n; ++i) {
      const agx3_group *g = &agx3b_table.groups[n->grp_off + i];
      for (unsigned j = 0; j < g->alt_n; ++j)
         walk(agx3b_table.altpool[g->alt_off + j], field);
   }
}

int main(int argc, char **argv)
{
   if (argc != 3)
      return 2;
   int form = agx3_form_index(&agx3b_table, argv[1], 0);
   if (form < 0)
      return 2;
   walk(agx3b_table.forms[form].node, atoi(argv[2]));
   return 0;
}
