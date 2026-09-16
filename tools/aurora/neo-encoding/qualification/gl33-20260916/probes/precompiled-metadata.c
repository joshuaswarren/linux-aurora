/* Diagnostic metadata dump; does not execute a shader. */
#include "asahi/compiler/agx_compile.h"
#include "libagx_shaders.h"
#include <inttypes.h>
#include <stdio.h>

int main(void)
{
   puts("[");
   bool first = true;
   for (unsigned i = 0; i < LIBAGX_NUM_PROGRAMS; ++i) {
      if (!libagx_g17p[i])
         continue;
      const struct agx_precompiled_kernel_info *p =
         (const void *)libagx_g17p[i];
      const uint8_t *binary = (const uint8_t *)p + sizeof(*p);
      uint64_t hash = UINT64_C(14695981039346656037);
      for (unsigned j = 0; j < p->binary_size; ++j) {
         hash ^= binary[j];
         hash *= UINT64_C(1099511628211);
      }
      printf("%s{\"index\":%u,\"main_offset\":%u,\"main_size\":%u,"
             "\"binary_size\":%u,\"nr_gprs\":%u,\"push_count\":%u,"
             "\"scratch_size\":%u,\"spectpr_scratch_size\":%u,"
             "\"printf_uniform\":%u,\"workgroup\":[%u,%u,%u],"
             "\"local_size\":%u,\"hash\":\"%016" PRIx64 "\"}",
             first ? "" : ",\n", i, p->main_offset, p->main_size,
             p->binary_size, p->nr_gprs, p->push_count, p->scratch_size,
             p->agx3_spectpr_scratch_size, p->printf_uniform,
             p->workgroup_size[0], p->workgroup_size[1], p->workgroup_size[2],
             p->local_size, hash);
      first = false;
   }
   puts("\n]");
   return 0;
}
