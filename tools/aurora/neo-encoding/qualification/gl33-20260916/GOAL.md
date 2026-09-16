# Official desktop OpenGL 3.0 CTS goal

User objective: “Close the gap to run the whole Open GL 3.0 CTS test suite to pass all test, keep improving our implementation until we are compliant, no cheating no hacks”. This replaces the previous whole-Piglit acceptance target. Piglit is supplementary regression/debugging evidence only. Driver fixes must implement general API behavior, with no CTS-specific behavior, altered test expectations, reduced capabilities, waived failures or falsified accounting.

## Acceptance requirements

1. Use an active official Khronos OpenGL CTS release. Current selected tag: `opengl-cts-4.6.8.1`, listed active on the Khronos release overview when checked on 2026-09-13. Record the resolved commit, source status, dependencies, build configuration and installed artifacts.
2. Run the complete official desktop `gl30` conformance procedure, including its required configurations and iterations. Preserve the upstream test selection and expectations. An isolated `KHR-GL30.*` subset is diagnostic evidence, not completion.
3. Run on the Neo J700's native Asahi driver, without Zink, software fallback or capability/version overrides. Record renderer/API identity, Mesa/library/kernel versions and hashes with results.
4. Retain full QPA logs, runner summary, test/configuration inventory, failures, unsupported results, crashes, hangs and unrun cases. Do not turn missing work, skipped cases or waivers into passes. Diagnose the official runner's precise pass rules before interpreting its verdict.
5. Improve the implementation and rerun relevant regressions as failures are found. Final completion requires a successful full official run of the final installed implementation with no unresolved applicable failures. Engineering CTS success and formal Khronos certification remain distinct; no certification submission has been requested.
6. Preserve resident Stage1, SSD contents and the other agent's MacBook Air. Use only J700/T8140 KIS `00100000`. Stop on a GPU firmware/DART fault, preserve evidence, and use a fresh boot before further GPU work. The user's later request for a faster loop authorizes keeping the healthy dedicated CTS Xorg running between iterations. Restore visible Plasma/gears when the campaign ends or the user requests it.

## Starting evidence

Neo boot `59b80248-af05-431c-b5df-42a7870b27eb`, kernel #12, Plasma/KWin/gears running and DCP loaded, reverified at the start of this goal. Native test library is fix3, SHA `6b660621245899d0fa0ab916e82201232d32b3774006bff77aa0f98336f3e96e`. Source modifications from the preceding campaign remain uncommitted and preserved in `mesa-j700` on lowercase `j700`.

The existing CTS checkout is clean at development commit `1942832354c96f0de379f491519734256fbd4f9d`; its installed packages do not include cts-runner. Prepare a separate tagged checkout/build so the old artifacts remain available.

Prior native Piglit vertex64 and TCS barrier regressions pass. Full host suites show 264 pass, 2 expected failures and 1 pre-existing robust-index golden failure. These are not official GL3.0 CTS results.

## Sources and operating contract

- https://github.com/KhronosGroup/VK-GL-CTS/wiki/Overview-of-releases
- https://github.com/KhronosGroup/VK-GL-CTS/blob/opengl-cts-4.6.8.1/external/openglcts/README.md
- Read `/Users/mcruz/Developer/aurora-silicon/neo-update/PROGRESS_WEBHOOK_GUIDE.md` in full on every resume after compaction, before another action. Post only meaningful progress using its Components V2 contract and the authorized runtime webhook. Initial amber for this new CTS goal was delivered.

This is the first turn of the replacement goal. No blocker is established. The goal remains active and incomplete.
