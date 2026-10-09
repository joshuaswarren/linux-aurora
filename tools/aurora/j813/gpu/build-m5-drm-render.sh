#!/bin/bash
# Build the application submission test with Mesa's existing VDM decoder.
set -euo pipefail
[[ $(hostname) == lima-j813-builder ]]
task_root=/Users/mcruz/Developer/aurora-silicon/j813-linux
task_mesa="$task_root/mesa"
task_core="$task_mesa/src/asahi/lib"
task_sources=()
for task_name in decode barrier ppp_state stream_link stream_terminate tessellate state_update state index_list; do
    task_sources+=("$task_core/agx_vdm_${task_name}_core.c")
done
cc -static -Os -s -Wall -Wextra -Werror -D__user= \
    -I "$task_root/linux/include/uapi/drm" -I "$task_core" -I "$task_mesa/include" \
    "$task_root/linux/tools/aurora/j813/m5-drm-render.c" "${task_sources[@]}" \
    -o "$task_root/artifacts/gpu/native/m5-drm-render"
"$task_root/artifacts/gpu/native/m5-drm-render" \
    "$task_root/artifacts/gpu/m5-25g76-graphics.bin"
"$task_root/artifacts/gpu/native/m5-drm-render" \
    "$task_root/artifacts/gpu/m5-25g76-graphics.bin" --native-va
"$task_root/artifacts/gpu/native/m5-drm-render" \
    "$task_root/artifacts/gpu/m5-25g76-graphics.bin" --native-va --all-reference-pages
if "$task_root/artifacts/gpu/native/m5-drm-render" \
    "$task_root/artifacts/gpu/m5-25g76-graphics.bin" --all-reference-pages; then
    echo 'All-page diagnostic incorrectly accepted relocation' >&2
    exit 1
fi
for task_test in vdm-index-list-core vdm-decode-core; do
    cc -static -Os -s -Wall -Wextra -Werror -D__user= \
        -I "$task_root/linux/include/uapi/drm" -I "$task_mesa/src" -I "$task_mesa/include" \
        "$task_core/tests/test-${task_test}.c" "${task_sources[@]}" \
        -o "$task_root/artifacts/gpu/native/test-m5-${task_test}"
    "$task_root/artifacts/gpu/native/test-m5-${task_test}"
done
