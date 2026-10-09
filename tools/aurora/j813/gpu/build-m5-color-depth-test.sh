#!/bin/sh
# Build on Linux/aarch64 with Vulkan headers and glslangValidator installed.
set -eu
task_source=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
task_output=${1:?Usage: build-m5-color-depth-test.sh OUTPUT_DIRECTORY}
mkdir -p "$task_output"
glslangValidator -V "$task_source/m5-color-depth.vert" --vn g17p_wsi_triangle_vert_spv -o "$task_output/g17p_wsi_triangle_vert.h"
glslangValidator -V "$task_source/m5-color-depth.frag" --vn g17p_wsi_triangle_frag_spv -o "$task_output/g17p_wsi_triangle_frag.h"
glslangValidator -V -DM5_PSIZ=1 "$task_source/m5-color-depth.vert" --vn m5_smooth_psiz_vert_spv -o "$task_output/m5_smooth_psiz_vert.h"
glslangValidator -V -DM5_FLAT=1 "$task_source/m5-color-depth.vert" --vn m5_flat_vert_spv -o "$task_output/m5_flat_vert.h"
glslangValidator -V -DM5_FLAT=1 -DM5_PSIZ=1 "$task_source/m5-color-depth.vert" --vn m5_flat_psiz_vert_spv -o "$task_output/m5_flat_psiz_vert.h"
glslangValidator -V -DM5_FLAT=1 "$task_source/m5-color-depth.frag" --vn m5_flat_frag_spv -o "$task_output/m5_flat_frag.h"
"${CC:-cc}" -O2 -Wall -Wextra -Werror -DAGX_TEST_DEVICE_ID=0x8142 \
    -DIMAGE_WIDTH=256 -DIMAGE_HEIGHT=256 -I"$task_output" \
    "$task_source/m5-vulkan-color-depth-test.c" -ldl \
    -o "$task_output/m5-vulkan-color-depth-test"
