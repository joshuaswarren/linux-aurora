/* CPU-only diagnostic using the production typed TPS encoder/decoder. */
#include "asahi/compiler/agx3_static_spill_core.h"
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv)
{
   for (int arg = 1; arg < argc; ++arg) {
      uint8_t bytes[10], replay[10], replay_len = 0;
      size_t n = strlen(argv[arg]) / 2;
      if (!n || n > sizeof(bytes) || strlen(argv[arg]) != n * 2)
         return 2;
      for (size_t i = 0; i < n; ++i) {
         unsigned value;
         if (sscanf(argv[arg] + i * 2, "%2x", &value) != 1)
            return 2;
         bytes[i] = value;
      }
      struct agx3_static_spill_store_desc s = {0};
      struct agx3_static_spill_load_desc l = {0};
      printf("{\"hex\":\"%s\"", argv[arg]);
      if (agx3_static_spill_decode_store(AGX_ISA_ENCODING_AGX3B, bytes, n, &s)) {
         bool ok = agx3_static_spill_encode_store(AGX_ISA_ENCODING_AGX3B,
                                                &s, replay, &replay_len);
         printf(",\"kind\":\"store\",\"tps_half\":%u,\"src\":%u,"
                "\"width\":%u,\"src_kill\":%u,\"slot\":%u,"
                "\"wait_mask\":%u,\"structural_flag\":%u,\"replay_equal\":%s",
                s.tps_half, s.src, s.width, s.src_kill, s.slot, s.wait_mask,
                s.structural_flag,
                ok && replay_len == n && !memcmp(bytes, replay, n) ? "true" : "false");
      } else if (agx3_static_spill_decode_load(AGX_ISA_ENCODING_AGX3B, bytes, n, &l)) {
         bool ok = agx3_static_spill_encode_load(AGX_ISA_ENCODING_AGX3B,
                                               &l, replay, &replay_len);
         printf(",\"kind\":\"load\",\"tps_half\":%u,\"dest\":%u,"
                "\"width\":%u,\"slot\":%u,\"slot_mirror\":%u,"
                "\"wait_mask\":%u,\"narrow_extend\":%u,"
                "\"structural_flag\":%u,\"tps_live\":%u,\"replay_equal\":%s",
                l.tps_half, l.dest, l.width, l.slot, l.slot_mirror,
                l.wait_mask, l.narrow_extend, l.structural_flag, l.tps_live,
                ok && replay_len == n && !memcmp(bytes, replay, n) ? "true" : "false");
      } else {
         printf(",\"kind\":\"outside_decoder_subset\"");
      }
      puts("}");
   }
   return 0;
}
