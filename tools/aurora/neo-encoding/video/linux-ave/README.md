# Linux AVE foundation

This is an experimental out-of-tree kernel module for J700/T8140 using the
pinned `7.1.6-aurora-neo` kernel. It adds the missing AVE platform device and
two DART connections, then owns a 20 MiB IPC allocation and a separate 64 KiB
queue allocation. Every 16 KiB IOVA page is checked against the corresponding
CPU virtual mapping through Linux's IOMMU API. Firmware startup, AVE DMA
transactions and V4L2 encode submission are not implemented.

Hardware qualification passed on 2026-09-08: both AVE DARTs initialized as
v2.3/16 KiB/42-bit; 1,284 page mappings passed on each of two module loads,
with successful cleanup between them. The candidate booted once and the
existing Vulkan smoke test passed. Evidence and matching source/binaries:
`../../artifacts/ave-linux-qualified-20260908/`. Do not repeat this campaign
without a relevant change. The current module remains loaded on native Linux.

Topology comes from the live 25G76 IODeviceTree capture in
`../../artifacts/ave-devicetree-properties-20260908.plist`: AVE's mapper has
stream 0, its DART has two register instances at 0x285080000/0x2850a0000,
and both share IRQ 955. The 256 MiB experimental DMA aperture is a bounded
subset of the device tree's 1 TiB-based window. Existing `venc0` power
controllers provide system and DMA-domain dependencies. The four virtual
voltage/performance gates are not invented as Linux power controllers.

`t8140-ave.dtsi` augments the existing DT without enabling other devices.
`check-dtb.py` requires only those three new nodes and checks that the panel
references still resolve to the original node after dtc renumbers handles.
The module claims only dedicated codec/mailbox ranges; it does not access
the shared PMGR or SRAM windows or write AVE firmware-start/doorbell registers.

Build only this module and DT, reusing the existing kernel build:

```sh
limactl shell neo-builder bash /Users/mcruz/Developer/aurora-silicon/neo-linux/scripts/build-ave.sh
```

Candidate bundle: `../../artifacts/linux-ave/`. The original kernel,
initramfs and symbols are linked from the qualified baseline and checked
against its manifest. The original DT and baseline manifest remain intact.

After closing the active target controller, boot the candidate once:

```sh
J700_LINUX_BUNDLE=/Users/mcruz/Developer/aurora-silicon/neo-linux/artifacts/linux-ave \
bash /Users/mcruz/Developer/aurora-silicon/neo-linux/j700-boot-linux.sh
```

Use `push-linux.py` to send `apple-ave.ko` and `test-native.sh` to RAM `/tmp`,
then run the test through `console.py`. It checks allocation, all mappings,
unload cleanup and reload once, leaving the second allocation ready. This
is not a firmware/DMA transfer test. Check `ENCODER_HANDOFF.md` one directory
above for the latest physical test outcome before re-running anything.
