/* Diagnostic only: inspect the generated precompiled ABI through its C type. */
#include "asahi/compiler/agx_compile.h"
#include "libagx_shaders.h"
#include <inttypes.h>
#include <stdio.h>

int main(void)
{
   const struct { const char *name; unsigned index; } selected[] = {
      {"tri0", LIBAGX_TESS_TRI_0}, {"tri1", LIBAGX_TESS_TRI_1},
      {"quad0", LIBAGX_TESS_QUAD_0}, {"quad1", LIBAGX_TESS_QUAD_1},
      {"robust0", LIBAGX_DRAW_ROBUST_INDEX_0},
      {"robust1", LIBAGX_DRAW_ROBUST_INDEX_1},
      {"robust2", LIBAGX_DRAW_ROBUST_INDEX_2},
      {"resolve", LIBAGX_RESOLVE_UNORM8X4},
      {"restart", LIBAGX_INCREMENT_IA_RESTART_NONNULL},
      {"query", LIBAGX_COPY_QUERY}, {"invocations", LIBAGX_INCREMENT_CS_INVOCATIONS},
      {"timestamp", LIBAGX_COPY_TIMESTAMP}, {"factors", LIBAGX_TESS_FACTORS_HALF},
      {"setup", LIBAGX_TESS_SETUP_INDIRECT_HW},
   };
   for (unsigned i = 0; i < sizeof(selected) / sizeof(selected[0]); ++i) {
      const struct agx_precompiled_kernel_info *info = (const void *)libagx_g17p[selected[i].index];
      if (!info) return 1;
      const uint8_t *code = (const uint8_t *)info + sizeof(*info);
      uint64_t hash = UINT64_C(14695981039346656037);
      for (unsigned b = 0; b < info->binary_size; ++b)
         hash = (hash ^ code[b]) * UINT64_C(1099511628211);
      printf("%s offset=%u main=%u size=%u gprs=%u push=%u tps=%u printf=%u hash=%016" PRIx64,
             selected[i].name, info->main_offset, info->main_size, info->binary_size,
             info->nr_gprs, info->push_count, info->agx3_tps_size, info->printf_uniform, hash);
#ifdef J700_EXTRA_KERNEL_METADATA
      printf(" scratch=%u spectpr=%u", info->scratch_size, info->agx3_spectpr_scratch_size);
#endif
      putchar('\n');
      if (selected[i].index == LIBAGX_COPY_TIMESTAMP) {
         printf("timestamp_hex=");
         for (unsigned b = 0; b < info->binary_size; ++b)
            printf("%02x", code[b]);
         putchar('\n');
      }
   }
   return 0;
}
