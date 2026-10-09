# Neo application tests

This suite targets the physical J700 from its Plasma X11 session. Run as the desktop user, with the selected Mesa environment inherited from that session. Every invocation needs a new output directory and writes `receipt.json` plus detailed evidence. It refuses other boards and root execution.

Applications: native Google Chrome ARM64, Firefox, Blender, OBS Studio, and Khronos VK-GL-CTS (Vulkan, EGL, OpenGL and GLES executables).

```sh
python /opt/neo/application-suite/neo-application-suite.py --output "$HOME/neo-tests/inventory-1" inventory
python /opt/neo/application-suite/neo-application-suite.py --output "$HOME/neo-tests/chrome-1" chrome
python /opt/neo/application-suite/neo-application-suite.py --output "$HOME/neo-tests/firefox-1" firefox
python /opt/neo/application-suite/neo-application-suite.py --output "$HOME/neo-tests/blender-eevee-1" blender
python /opt/neo/application-suite/neo-application-suite.py --output "$HOME/neo-tests/blender-cpu-1" blender --cpu-control
python /opt/neo/application-suite/neo-application-suite.py --output "$HOME/neo-tests/obs-1" obs
python /opt/neo/application-suite/neo-application-suite.py --output "$HOME/neo-tests/vulkan-smoke-1" cts \
  --binary /opt/neo-cts/external/vulkancts/modules/vulkan/deqp-vk \
  --case 'dEQP-VK.api.smoke.*' --timeout 180
```

Use the installed CTS case lists to select further bounded groups. Retain the exact binary/source revision and `.qpa` files. A smoke pass is not a conformance claim; report failures, timeouts and NotSupported counts separately. The runner rejects an empty case result.

Browser tests use temporary profiles and a loopback-only page. They request WebGL and WebGL2, compile shaders, render 30 frames, read back a known pixel, and record the browser's renderer strings. Rendering and hardware attribution are separate results. A software renderer can pass pixel correctness while failing the hardware-renderer gate. No browser sandbox bypass is used.

Blender saves a small `.blend` scene and renders a 320×240 image with EEVEE. The CPU Cycles run is a separate control. A successful image must still be visually inspected; neither run proves every Blender feature or GPU compute support.

OBS uses an isolated configuration and an authenticated local WebSocket session. It records a generated colour scene changing once per second, saves a preview, and checks the recording with FFprobe and decoded frame hashes. It does not capture the user's desktop. This test uses software x264 and makes no hardware-encoding claim. The current native build omits the optional CEF browser-source plugin.

Before broadening tests, inspect the kernel and compositor logs for GPU/device-loss errors. Stop the current campaign on a firmware/DART fault and retain the logs; do not repeatedly reload drivers or resubmit firmware in a faulted boot. Existing browser profiles and other devices are outside this suite's scope.

Implementation and initial hardware qualification are in progress. Installation, command-line version output, a visible window, a correct rendered image, verified hardware acceleration, and full conformance are distinct milestones.
