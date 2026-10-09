/* Offline generated-table probe; no native driver or GPU execution. */
#include "agx3_compose.h"
#include <inttypes.h>
#include <stdio.h>

int main(void)
{
   for (unsigned family = 0; family < 2; ++family) {
      const agx3_table *table = family ? &agx3c_table : &agx3b_table;
      for (unsigned ep = 0; ep < 2; ++ep) {
         int32_t form = agx3_form_index(table, "rld", ep);
         if (form < 0)
            return 2;
         for (unsigned hint = 0; hint < 4; ++hint) {
            uint64_t fields[AGX3_MAX_FIELDS] = {0};
            fields[9] = 2;
            fields[23] = 4;
            fields[38] = 1;
            fields[10] = fields[24] = 1;
            fields[12] = fields[26] = 2;
            fields[18] = 8;
            fields[27] = 1;
            fields[family ? 44 : 41] = hint;
            if (family) {
               fields[28] = 4;
               fields[29] = 144;
            }
            uint8_t bytes[AGX3_MAX_WORDS * 2] = {0};
            bool ok = agx3_compose(table, form, fields, bytes);
            printf("{\"family\":\"agx3%c\",\"ep\":%u,\"hint\":%u,"
                   "\"accepted\":%s,\"hex\":\"",
                   family ? 'c' : 'b', ep, hint, ok ? "true" : "false");
            if (ok)
               for (unsigned i = 0; i < table->forms[form].size; ++i)
                  printf("%02x", bytes[i]);
            puts("\"}");
         }
      }
   }
   return 0;
}
