# ANE staging note (this branch only)

Branch `ane-dt-t8103` on `joshuaswarren/linux` (fork of
`AsahiLinux/linux`, base tag `asahi-7.1.6-1`). **This branch has
not been booted.** Treat every line as a patch proposal, not a
tested result. The point of the branch is the diagnosis below,
not the patch; the patch exists to make the diagnosis
reproducible.

## DIAGNOSIS

On the M1 host (t8103, MacBookPro17,1, kernel 7.1.6-1-1-ARCH),
a GRUB entry ran `devicetree /@/boot/ane.dtb`. That command
replaces the complete m1n1-patched device tree with one static
snapshot captured 2026-08-25. m1n1 writes per-boot values into
the tree it hands the kernel, and the snapshot discarded them.
The visible piece: the frozen `cpu@1` `cpu-release-addr`
`0x8_05b14230` against a live `0x8_04f9c230`. The secondary
CPUs spun at a release stub from a previous boot; the kernel
logged `CPU1: failed to come online`, `failed in unknown state
: 0x0` through CPU7; `nproc` printed 1,
`/sys/devices/system/cpu/online` printed 0, offline printed
1-7. The stock entry with no override returns nproc 8, online
0-7. The A/B proves the boot entry decides the outcome; it
does not separate the frozen `cpu-release-addr` from the other
frozen values. The exact mechanism is unproven.

The packaged `t8103-j293.dtb` carries no ANE node today
(`strings t8103-j293.dtb | grep -c t8103-ane` prints 0) and
mainline has no userspace DT-overlay loader, so a node the
ANE driver can bind has to ship in the tree the bootloader
hands the kernel. With the node in the packaged DTB, m1n1's
per-boot patching survives and both goals coexist.

The full failure and the verification checklist are in
[`docs/boot-and-kernel.md`](https://github.com/joshuaswarren/mlx-omarchy/blob/main/docs/boot-and-kernel.md)
in `joshuaswarren/mlx-omarchy`.

## WHAT THIS BRANCH DOES NOT HAVE

- **No boots.** Every step of the verification checklist in
  commit `326d6033` ("Required verification before any boot")
  is unrun beyond the dtb build on a build host.
- **No DT binding document.** `git grep` for
  `"apple,t8103-ane"` in `Documentation/devicetree/bindings/`
  returns nothing at the base tag.
- **No in-tree driver.** `git grep` for `"t8103-ane"` in
  `drivers/` returns nothing at the base tag. The out-of-tree
  driver lives in `joshuaswarren/omarchy-ane` and is not
  vendored into this branch; loading it hard-reset an M1 on
  2026-09-03 and it must stay blacklisted.

A correct integration replaces the whole arrangement with a
binding document and an in-tree driver. Until then this is a
staging branch, not a binding proposal and not a merge
request.

## WHAT THIS BRANCH IS

A place to read the diagnosis, the node proposal, and the open
binding questions together. `dj` (Apple GPU / Vulkan) and
`eryk` (who stands up the official kernel branch) asked to
see the work; this is the staged view.

## BINDING OPEN QUESTION

The `ane@26a000000` node follows the out-of-tree `eiln/ane`
driver binding: three ANE DARTs crammed into one `reg` block
with `reg-names "engine","dart0","dart1","dart2"`, with the
driver programming the DARTs itself. A mainline-shaped
binding would describe the ANE DARTs as separate
`apple,t8103-dart` nodes wired to the ANE via `iommus`
phandles, the way every other peripheral in this tree is
wired. The two shapes are incompatible at the driver level.
The out-of-tree driver does not use the IOMMU API, so this
staging shape drops the `iommus` phandle; that is tied to the
binding choice, not a separate decision.

## DUAL-OWNER DART (the `ane_dart` node is documentary only)

`ane_dart` (`iommu@26b800000`) describes the ANE DART's
hardware. If it were enabled, the in-tree `apple-dart` driver
would probe it: `devm_platform_ioremap_resource` on the
`0x26b800000` window exclusively, `request_irq` on AIC 417
(`IRQF_SHARED`), and `apple_dart_hw_reset` at probe. The
out-of-tree driver maps the same window for its `"dart0"` and
claims the same IRQ 417 by name, with a documented
`disable_irq` hazard on an unacked DART fault. They never
coexist safely. `ane_dart` is kept disabled on every board so
neither conflict happens; the node is kept in the source as a
hardware description for the future mainline shape. It is not
a proposed binding.

## CIRCULAR EVIDENCE ON THE PMGR WINDOWS

`ps_ane_base` and `ps_ane_set1..5` (`reg 0xc008..0xc030`) are
copied from `eiln/linux` commit `6027c18`, not re-derived
from this machine's ADT. The static tree the original commit
message claimed these windows matched is itself built from
that commit, so the match is circular. Re-deriving them from
the ADT on M1 hardware is open work.

## CONCRETE DEFECT IN THE EXISTING OUT-OF-TREE ARTIFACT

Independent of whether this branch's node shape is right, the
shipped `ane.dtbo` (in `joshuaswarren/omarchy-ane`,
ultimately from `eiln/ane`) names its node `ane@23b100000`,
which is the AIC address on `t8103`, not an ANE address. Its
`iommus` and `power-domains` carry unresolved `0xffffffff`
fixups. Anyone attempting the bootloader-overlay alternative
will hit this; replacing the artifact with a correct one is
part of making that path work, which it currently is not.

## PREDICTED MAINTAINER OBJECTION (pre-empted)

> "Unbooted, no binding or driver, and a disabled `ane_dart`
> that still sketches dual-owner DART MMIO."

True on every count. That is exactly what this branch is. The
patch exists so the diagnosis is reproducible and so the
binding question has a concrete shape to argue about.

## ON BOOTING THE WORK

A bad device tree on an Apple Silicon Mac needs physical
recovery with nobody in front of the machine. Run the
verification checklist in commit `326d6033` on a disposable
machine only. Do not pair this device-tree change with an
automatic load of `joshuaswarren/omarchy-ane`.

## PROVENANCE

| | |
|---|---|
| Fork | `joshuaswarren/linux` |
| Upstream | `AsahiLinux/linux` |
| Branch | `ane-dt-t8103` |
| Base tag | `asahi-7.1.6-1` (= `e2e1930a9595bffafad92cec2b5504525efb9cd4`, the source of Arch `linux-asahi` `7.1.6-1-1-ARCH` on the M1 host) |
| Commits | `326d6033` (node + pmgr + binding disagreement), `b1cb024a` (DART dual-ownership hardening, claims corrected), and the in-tree cover commit that adds this file |
| Driver pointer | `joshuaswarren/omarchy-ane` (not vendored) |
| Docs pointer | `joshuaswarren/mlx-omarchy` → `docs/boot-and-kernel.md` and `docs/forks.md` |
