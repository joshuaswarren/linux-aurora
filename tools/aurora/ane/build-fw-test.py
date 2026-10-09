#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Build the reboot-only J700 firmware test for a pinned Linux image and DTB."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess


def fdtget(dtb, node, prop, kind="s"):
    return subprocess.check_output(
        ["fdtget", "-t", kind, str(dtb), node, prop], text=True).strip()


def make_overlay(output, name, body):
    dts, dtbo = output / f"{name}.dts", output / f"{name}.dtbo"
    dts.write_text('''/dts-v1/;
/plugin/;
/ { fragment@0 { target-path = "/soc"; __overlay__ {
    #address-cells = <2>; #size-cells = <2>;
''' + body + '\n}; }; };\n')
    subprocess.run(["dtc", "-@", "-I", "dts", "-O", "dtb", "-o", str(dtbo), str(dts)], check=True)
    subprocess.run(["fdtput", "-d", str(dtbo), "/fragment@0/__overlay__",
                    "#address-cells", "#size-cells"], check=True)
    # The installed kernel DT has no /__symbols__. Local phandle relocation
    # needs __local_fixups__, but these test labels need not be exported.
    roots = subprocess.check_output(["fdtget", "-l", str(dtbo), "/"], text=True).split()
    if "__symbols__" in roots:
        subprocess.run(["fdtput", "-r", str(dtbo), "/__symbols__"], check=True)
    values = ','.join(f'0x{x:02x}' for x in dtbo.read_bytes())
    return f"static const unsigned char {name.replace('-', '_')}_blob[] __aligned(8) = {{{values}}};"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", type=Path, required=True)
    parser.add_argument("--kernel-build", type=Path, required=True)
    parser.add_argument("--base-dtb", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--case-dir", type=Path,
                        help="Enable a separately SHA-pinned local numerical test")
    parser.add_argument("--resume-overlay", action="store_true",
                        help="Build a separately named overlay module for an already held power node")
    parser.add_argument("--allow-unqualified-kernel", action="store_true",
                        help="Build for another prepared kernel; requires new hardware qualification")
    args = parser.parse_args()
    compatible = fdtget(args.base_dtb, "/", "compatible").split()
    if "apple,j700" not in compatible or "apple,t8140" not in compatible:
        parser.error("not a J700/T8140 base DTB")
    children = subprocess.check_output(["fdtget", "-l", str(args.base_dtb), "/soc"], text=True).split()
    candidates = ["/soc/" + x for x in children if x.startswith("interrupt-controller@")]
    aics = [x for x in candidates if "apple,t8140-aic3" in fdtget(args.base_dtb, x, "compatible").split()]
    if len(aics) != 1:
        parser.error("expected exactly one T8140 AIC3")
    aic = aics[0]
    phandle = int(fdtget(args.base_dtb, aic, "phandle", "x"), 16)
    if fdtget(args.base_dtb, aic, "#interrupt-cells", "u") != "3" or not phandle:
        parser.error("unexpected AIC binding")
    config = (args.kernel_build / ".config").read_bytes()
    if not args.allow_unqualified_kernel and hashlib.sha256(config).hexdigest() != "5a2435c69452b1b88a33816de58aa843e64ce09c6992b0e0ce08129fb40cad32":
        parser.error("not the qualified Linux 7.1.6-aurora-neo-gpu1 kernel configuration")
    args.output.mkdir(parents=True, exist_ok=False)
    driver = args.source_root / "drivers/soc/apple"
    for name in ("apple-ane-bringup.c", "apple-ane-bringup.h", "apple-ane-fw-test.c", "apple-ane-ipc.h", "apple-ane-tiny.h", "apple-ane-user.h"):
        shutil.copy2(driver / name, args.output)
    overlay_name = "ane-fw-test-resume" if args.resume_overlay else "ane-fw-test-overlay"
    shutil.copy2(args.source_root / "tools/aurora/ane/ane-fw-test-overlay.c",
                 args.output / f"{overlay_name}.c")
    power = '''ane@400000000 {
        compatible = "apple,t8140-ane-bringup";
        reg = <4 0 0 0x2000000>, <3 0x726680 0 0x40>, <4 0x18a4000 0 0x20>;
        reg-names = "cluster", "pwgate", "ce";
        apple,power-domain-path = "/soc/power-management@300700000/power-controller@290";
        status = "okay";
    };'''
    devices = []
    for i, offset in enumerate((0x1800000, 0x1820000, 0x1840000)):
        devices.append(f'''ane_dart{i}: iommu@4{offset:08x} {{
            compatible = "apple,t8140-dart", "apple,t8110-dart";
            reg = <4 0x{offset:x} 0 0x{0xc000 if i == 0 else 0x20000:x}>;
            interrupt-parent = <0x{phandle:x}>;
            interrupts = <0 656 4>;
            #iommu-cells = <1>;
            status = "okay";
        }};''')
    devices.append('''ane-fw-test@401600040 {
        compatible = "apple,t8140-ane-fw-test";
        reg = <4 0x1600040 0 0x10>, <4 0x1050000 0 8>, <4 0x1880020 0 0x20>,
              <4 0x1884000 0 4>, <4 0x188c000 0 4>, <4 0x1890000 0 4>,
              <4 0x1810000 0 0x100>;
        reg-names = "cpu", "vector", "scratch", "doorbell", "irq", "ack", "dapf";
        iommus = <&ane_dart0 0>, <&ane_dart1 0>, <&ane_dart2 0>;
        status = "okay";
    };''')
    headers = [f'#define ANE_FW_AIC_PATH {json.dumps(aic)}',
               f'#define ANE_FW_AIC_PHANDLE 0x{phandle:x}',
               make_overlay(args.output, "ane-fw-power", power),
               make_overlay(args.output, "ane-fw-devices", '\n'.join(devices))]
    (args.output / "ane-fw-test-dtbo.h").write_text('\n'.join(headers) + '\n')
    # The AIC is external to the standalone overlay, so dtc cannot resolve its
    # numeric phandle there. Qualify it in the actual combined tree instead.
    merged = args.output / "merged-check.dtb"
    subprocess.run(["fdtoverlay", "-i", str(args.base_dtb), "-o", str(merged),
                    str(args.output / "ane-fw-power.dtbo"),
                    str(args.output / "ane-fw-devices.dtbo")], check=True)
    for offset in (0x1800000, 0x1820000, 0x1840000):
        node = f"/soc/iommu@4{offset:08x}"
        if int(fdtget(merged, node, "interrupt-parent", "x"), 16) != phandle:
            raise ValueError("AIC phandle changed during overlay relocation")
        if fdtget(merged, node, "interrupts", "u") != "0 656 4":
            raise ValueError("wrong DART interrupt after overlay relocation")
    if fdtget(merged, aic, "compatible") != fdtget(args.base_dtb, aic, "compatible"):
        raise ValueError("AIC unexpectedly modified")
    case_flags = ""
    case_hash = None
    if args.case_dir:
        case_manifest = json.loads((args.case_dir / "manifest.json").read_text())
        case_file = case_manifest.get("file", "j700-conv16.case")
        if case_file not in ("j700-conv16.case", "j700-anemll-tiny.case"):
            raise ValueError("unsupported numerical case filename")
        case_hash = hashlib.sha256((args.case_dir / case_file).read_bytes()).hexdigest()
        if case_hash != case_manifest["sha256"] or not case_manifest["native_output_validated"]:
            raise ValueError("unvalidated or changed numerical case")
        shutil.copy2(args.case_dir / "apple-ane-case-data.h", args.output)
        case_flags = "CFLAGS_apple-ane-fw-test.o += -DANE_HAVE_CASE\n"
    (args.output / "Makefile").write_text(
        f"obj-m += apple-ane-bringup.o\nobj-m += apple-ane-fw-test.o\nobj-m += {overlay_name}.o\n" + case_flags)
    source = (args.kernel_build / "source").resolve(strict=True)
    subprocess.run(["make", "-C", str(source), f"O={args.kernel_build.resolve()}",
                    "LLVM=1", "ARCH=arm64", "W=1", f"M={args.output.resolve()}",
                    f"MO={args.output.resolve()}", "modules", "-j2"], check=True)
    expected_release = (args.kernel_build / "include/config/kernel.release").read_text().strip()
    for name in ("apple-ane-bringup.ko", "apple-ane-fw-test.ko", f"{overlay_name}.ko"):
        vermagic = subprocess.check_output(["modinfo", "-F", "vermagic", str(args.output / name)], text=True)
        if vermagic.split()[0] != expected_release:
            raise ValueError("test module does not match the target kernel release")
    manifest = {"kernel_config_sha256": hashlib.sha256(config).hexdigest(),
                "base_dtb_sha256": hashlib.sha256(args.base_dtb.read_bytes()).hexdigest(),
                "aic_path": aic, "aic_phandle": phandle,
                "capability": "bounded numerical experiment" if args.case_dir else "firmware startup transport",
                "case_sha256": case_hash,
                "hardware_tested": False, "reboot_required_after_overlay": True,
                "sha256": {x.name: hashlib.sha256(x.read_bytes()).hexdigest()
                           for x in args.output.iterdir() if x.suffix in (".c", ".h", ".ko", ".dtbo")}}
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2) + '\n')
    print("ANE_FW_TEST_BUILD_COMPLETE")


if __name__ == "__main__":
    main()
