/* Greedy field recovery: start from our encoder's field set for a mnemonic
 * and encoding point, then repeatedly adopt the single (field, value) change
 * that most reduces the Hamming distance to Apple's bytes. Prints the final
 * assignment and the remaining distance. */
#include "asahi/compiler/agx3_binder.h"
#include "asahi/lib/agx_chip.h"
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
static unsigned char target[64]; static unsigned tlen;
static struct agx3_field_kv base[64], ov[64]; static unsigned nb, nov;
static int parse_kv(const char *s, struct agx3_field_kv *out) {
   int n = 0; if (!s || !*s) return 0; char *dup = strdup(s);
   for (char *t = strtok(dup, ","); t; t = strtok(NULL, ",")) { unsigned f, v; if (sscanf(t, "%u:%u", &f, &v) == 2) { out[n].field = f; out[n].value = v; n++; } }
   free(dup); return n;
}
static int dist(const char *mn, int ep, struct agx3_field_kv *o, unsigned no, unsigned char *out, uint8_t *len) {
   if (!agx3_bind_compose(AGX_ISA_ENCODING_AGX3B, mn, ep, base, nb, o, no, out, len)) return 1 << 20;
   if (*len != tlen) return (1 << 19) + abs((int)*len - (int)tlen);
   int d = 0; for (unsigned i = 0; i < tlen; i++) d += __builtin_popcount(out[i] ^ target[i]); return d;
}
int main(int argc, char **argv) {
   const char *mn = argv[1]; int ep = atoi(argv[2]); const char *hex = argv[3];
   tlen = strlen(hex) / 2; for (unsigned i = 0; i < tlen; i++) { unsigned v; sscanf(hex + 2 * i, "%2x", &v); target[i] = v; }
   nb = parse_kv(argv[4], base); nov = argc > 5 ? parse_kv(argv[5], ov) : 0;
   unsigned char out[64]; uint8_t len;
   struct agx3_field_kv cur[64]; unsigned nc = nov; memcpy(cur, ov, sizeof(ov[0]) * nov);
   int d = dist(mn, ep, cur, nc, out, &len);
   printf("start distance %d (len %u vs target %u)\n", d, len, tlen);
   for (int iter = 0; iter < 24 && d > 0; iter++) {
      int bestd = d; unsigned bf = 0, bv = 0; int found = 0;
      for (unsigned f = 0; f < 200; f++) for (unsigned v = 0; v < 256; v++) {
         struct agx3_field_kv trial[64]; unsigned nt = nc; memcpy(trial, cur, sizeof(cur[0]) * nc);
         int replaced = 0; for (unsigned i = 0; i < nt; i++) if (trial[i].field == f) { trial[i].value = v; replaced = 1; }
         if (!replaced) { trial[nt].field = f; trial[nt].value = v; nt++; }
         int td = dist(mn, ep, trial, nt, out, &len);
         if (td < bestd) { bestd = td; bf = f; bv = v; found = 1; }
      }
      if (!found) break;
      int replaced = 0; for (unsigned i = 0; i < nc; i++) if (cur[i].field == bf) { cur[i].value = bv; replaced = 1; }
      if (!replaced) { cur[nc].field = bf; cur[nc].value = bv; nc++; }
      d = bestd; printf("  set f%u=%u -> distance %d\n", bf, bv, d);
   }
   d = dist(mn, ep, cur, nc, out, &len);
   printf("final distance %d; bytes:", d); for (unsigned i = 0; i < len; i++) printf(" %02x", out[i]); printf("\nfields:"); for (unsigned i = 0; i < nc; i++) printf(" f%u=%llu", cur[i].field, (unsigned long long)cur[i].value); printf("\n");
   return d != 0;
}
