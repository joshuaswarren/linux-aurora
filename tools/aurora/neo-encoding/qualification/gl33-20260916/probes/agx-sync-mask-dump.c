/* Diagnostic only: use the existing test's NIR inputs and production pass. */
#include "asahi/compiler/agx_compile.h"
#include "asahi/lib/agx_tilebuffer.h"
#include <stdio.h>

static bool
audit_hoist(nir_shader *shader)
{
   puts("BEFORE_HOIST");
   nir_print_shader(shader, stdout);
   bool changed = agx_nir_hoist_g17p_zs_writes(shader);
   puts("AFTER_HOIST");
   nir_print_shader(shader, stdout);
   return changed;
}

int original_test_main(void);
#define main original_test_main
#define agx_nir_hoist_g17p_zs_writes audit_hoist
#include "src/asahi/lib/tests/test-agx-g17p-early-demote-epilog.c"
#undef agx_nir_hoist_g17p_zs_writes
#undef main

int main(void)
{
   return test_unrolled_dynamic_depth_mask() ? 0 : 1;
}
