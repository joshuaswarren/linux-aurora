#!/bin/bash
# Hardware batch on a fresh boot: install the candidate, run every remaining GL 3.0 case plus regression cases, probes, and traces for what still fails.
set -u
cd /Users/mcruz/Developer/aurora-silicon
G=neo-linux/artifacts/j700-gl30-cts-20260913
K=neo-linux/artifacts/j700-plasma-keyboard-install-20260913
CAND=/Users/mcruz/Developer/aurora-silicon/$G/libgallium-xfb-dense.so
run_case() { # label case [extra]
  timeout 900 python3 $G/fast-iteration.py "$1" --case "$2" ${3:-} >/dev/null 2>&1
  st=$(grep -h -o 'StatusCode="[A-Za-z]*"' $G/fast-iterations/$1/results/$1/*.qpa 2>/dev/null | head -1)
  echo "$2 -> $st"
}
echo "=== install + target cases"
run_case cs-lod-2darray-frag KHR-GL30.ext_texture_shadow_lod.texturelod.sampler2darrayshadow_fragment "--candidate $CAND"
run_case cs-bias-2darray-frag KHR-GL30.ext_texture_shadow_lod.texture.sampler2darrayshadow_bias_fragment
run_case cs-lod-cube-frag KHR-GL30.ext_texture_shadow_lod.texturelod.samplercubeshadow_fragment
run_case cs-lod-cube-vert KHR-GL30.ext_texture_shadow_lod.texturelod.samplercubeshadow_vertex
run_case cs-lod-cubearray-frag KHR-GL30.ext_texture_shadow_lod.texturelod.samplercubearrayshadow_fragment
run_case cs-bias-cubearray-frag KHR-GL30.ext_texture_shadow_lod.texture.samplercubearrayshadow_bias_fragment
run_case cs-impl-cubearray-frag KHR-GL30.ext_texture_shadow_lod.texture.samplercubearrayshadow_fragment
run_case cs-impl-cubearray-vert KHR-GL30.ext_texture_shadow_lod.texture.samplercubearrayshadow_vertex
echo "=== regression cases"
run_case cs-lodoff-2darray-frag KHR-GL30.ext_texture_shadow_lod.texturelodoffset.sampler2darrayshadow_fragment
run_case cs-off-bias-2darray-frag KHR-GL30.ext_texture_shadow_lod.textureoffset.sampler2darrayshadow_bias_fragment
run_case cs-lod-2darray-vert KHR-GL30.ext_texture_shadow_lod.texturelod.sampler2darrayshadow_vertex
run_case cs-depth-wrap-check KHR-GL30.texture_repeat_mode.depth_component16_11x131_1_repeat
echo "=== transform feedback + clip distance"
for c in capture_vertex_interleaved_test discard_vertex_test query_vertex_interleaved_test capture_vertex_separate_test query_vertex_separate_test capture_special_interleaved_test draw_xfb_test draw_xfb_feedbackk_test draw_xfb_instanced_test draw_xfb_stream_test draw_xfb_stream_instanced_test; do
  run_case cs-tf-$(echo $c | tr '_' '-' | sed 's/-test$//') KHR-GL30.transform_feedback.$c
done
run_case cs-clip-coverage KHR-GL30.clip_distance.coverage
run_case cs-clip-functional KHR-GL30.clip_distance.functional
echo "=== lod bias + blit"
run_case cs-lod-bias-all KHR-GL30.texture_lod_bias.texture_lod_bias_all
run_case cs-blit-scissor KHR-GL30.framebuffer_blit.scissor_blit
run_case cs-blit-msaa-color KHR-GL30.framebuffer_blit.multisampled_to_singlesampled_blit_color_config_test
run_case cs-blit-msaa-depth KHR-GL30.framebuffer_blit.multisampled_to_singlesampled_blit_depth_config_test
echo "=== probes"
E="HOME=/home/mcruz USER=mcruz LANG=C PATH=/opt/neo-piglit-full/waffle/bin:/usr/bin:/bin LD_LIBRARY_PATH=/opt/neo-piglit-full/lib:/opt/neo-mesa-native-gl2-20260913/lib:/opt/neo-mesa-native-glx-f4c0b9725bbc/lib/compat:/opt/neo-piglit-full/waffle/lib LIBGL_DRIVERS_PATH=/opt/neo-mesa-native-gl2-20260913/lib/dri __EGL_VENDOR_LIBRARY_FILENAMES=/opt/neo-mesa-native-gl2-20260913/share/glvnd/egl_vendor.d/50_mesa.json EGL_PLATFORM=x11 PIGLIT_PLATFORM=x11_egl MESA_SHADER_CACHE_DISABLE=true DISPLAY=:0 XAUTHORITY=/run/neo-gl30-cts.Xauthority"
S=/private/tmp/claude-501/-Users-mcruz-Developer-aurora-silicon/902856a6-5097-4782-ace9-2f515b8512d3/scratchpad
timeout 60 scp -F $K/linux-ssh-config -o BatchMode=yes $S/rgbw-const-fetch-probe.shader_test $S/rgbw-reg-fetch-probe.shader_test $S/blit-mimic-probe.shader_test $S/vs-lod-bias-probe.shader_test neo-plasma:/home/mcruz/ >/dev/null
timeout 600 ssh -F $K/linux-ssh-config -o BatchMode=yes neo-plasma "cd /home/mcruz; for p in blit-mimic-probe vs-lod-bias-probe two-reg-fetch-probe miptree-fs-txf-probe shadow2d-three-refs-probe; do env -i $E AGX_MESA_DEBUG=shaders timeout 60 /opt/neo-piglit-full/bin/shader_runner /home/mcruz/\$p.shader_test -auto -fbo > /home/mcruz/\$p.log 2>&1; echo \"\$p -> \$(grep -o '\"result\": \"[a-z]*\"' \$p.log) \$(grep Observed \$p.log | head -1)\"; done"
echo "=== traces (only for cases still failing)"
trace_if_failed() { # plainlabel tracelabel case
  st=$(grep -h -o 'StatusCode="[A-Za-z]*"' $G/fast-iterations/$1/results/$1/*.qpa 2>/dev/null | head -1)
  case "$st" in *Pass*) echo "$3 passed; no trace";; *) run_case "$2" "$3" "--trace";; esac
}
trace_if_failed cs-tf-capture-vertex-interleaved cs-tr-tf-capture-interleaved KHR-GL30.transform_feedback.capture_vertex_interleaved_test
trace_if_failed cs-tf-draw-xfb cs-tr-xfb-draw KHR-GL30.transform_feedback.draw_xfb_test
trace_if_failed cs-clip-functional cs-tr-clip-functional KHR-GL30.clip_distance.functional
trace_if_failed cs-blit-scissor cs-tr-scissor-blit KHR-GL30.framebuffer_blit.scissor_blit
trace_if_failed cs-blit-msaa-color cs-tr-blit-msaa-color KHR-GL30.framebuffer_blit.multisampled_to_singlesampled_blit_color_config_test
trace_if_failed cs-lod-bias-all cs-tr-lod-bias-all KHR-GL30.texture_lod_bias.texture_lod_bias_all
echo "=== piglit scissor blit"
timeout 120 ssh -F $K/linux-ssh-config -o BatchMode=yes neo-plasma "cd /home/mcruz; env -i $E timeout 60 /opt/neo-piglit-full/bin/fbo-scissor-blit fbo -auto 2>&1 | grep -i 'result\|fail\|probe' | tail -4"
echo "=== done"
