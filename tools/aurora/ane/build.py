#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-2.0-only
"""Build the prototype modules against an already built ARM64 kernel."""
import argparse
import hashlib
import json
import pathlib
import shutil
import subprocess


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--kernel-build", type=pathlib.Path, required=True)
    parser.add_argument("--source-root", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    parser.add_argument("--power-domain-path", required=True)
    args = parser.parse_args()
    if args.power_domain_path != "/soc/power-management@300700000/power-controller@290":
        parser.error("not the verified J700 ANE_SYS provider path")
    if not (args.kernel_build / "Module.symvers").is_file():
        parser.error("a complete matching kernel build is required")
    args.output.mkdir(parents=True, exist_ok=False)
    source = args.source_root / "drivers/soc/apple/apple-ane-bringup.c"
    tools = args.source_root / "tools/aurora/ane"
    shutil.copy2(source, args.output)
    shutil.copy2(source.with_suffix(".h"), args.output)
    shutil.copy2(tools / "ane-test-overlay.c", args.output)
    shutil.copy2(tools / "test-hardware.py", args.output)
    (args.output / "Makefile").write_text(
        "obj-m += apple-ane-bringup.o\nobj-m += ane-test-overlay.o\n")
    headers = ["/* Generated test overlays; do not install in boot firmware. */",
               f"#define ANE_TEST_PD_PATH {json.dumps(args.power_domain_path)}"]
    for mode in range(5):
        base = 0x211000000 if mode == 1 else 0x400000000
        pwgate = 0x726684 if mode == 3 else 0x726680
        ce = 0x18a4008 if mode == 4 else 0x18a4000
        power = "" if mode == 2 else (
            f"apple,power-domain-path = {json.dumps(args.power_domain_path)};")
        dts = args.output / f"test-{mode}.dts"
        dts.write_text(f"""/dts-v1/;
/plugin/;
/ {{
    fragment@0 {{
        target-path = "/soc";
        __overlay__ {{
            #address-cells = <2>;
            #size-cells = <2>;
            ane@400000000 {{
                compatible = "apple,t8140-ane-bringup";
                reg = <0x{base >> 32:x} 0x{base & 0xffffffff:x} 0 0x2000000>,
                      <3 0x{pwgate:x} 0 0x40>, <4 0x{ce:x} 0 0x20>;
                reg-names = "cluster", "pwgate", "ce";
                {power}
                status = "okay";
            }};
        }};
    }};
}};
""")
        dtbo = dts.with_suffix(".dtbo")
        subprocess.run(["dtc", "-@", "-I", "dts", "-O", "dtb", "-o",
                        str(dtbo), str(dts)], check=True)
        # Supply cell widths to dtc, but inherit the live /soc widths at
        # application time. Replacing existing /soc properties would leak
        # their storage when the test overlay is removed.
        subprocess.run(["fdtput", "-d", str(dtbo), "/fragment@0/__overlay__",
                        "#address-cells", "#size-cells"], check=True)
        data = dtbo.read_bytes()
        body = ",".join(f"0x{b:02x}" for b in data)
        headers.append(f"static const unsigned char ane_blob_{mode}[] __aligned(8) = {{{body}}};")
    headers.append("static const struct { const void *data; u32 size; } ane_test_blobs[] = {")
    headers.extend(f"{{ane_blob_{i}, sizeof(ane_blob_{i})}}," for i in range(5))
    headers.append("};")
    (args.output / "ane-test-dtbo.h").write_text("\n".join(headers) + "\n")
    # Linux 7.x can retain another external module's output path in its
    # generated wrapper Makefile. Pin both M and MO and use the source entry.
    kernel_source = (args.kernel_build / "source").resolve(strict=True)
    subprocess.run(["make", "-C", str(kernel_source), f"O={args.kernel_build.resolve()}",
                    "LLVM=1", "ARCH=arm64", "W=1", f"M={args.output.resolve()}",
                    f"MO={args.output.resolve()}", "modules", "-j2"], check=True)
    release = (args.kernel_build / "include/config/kernel.release").read_text().strip()
    for name in ("apple-ane-bringup.ko", "ane-test-overlay.ko"):
        module = args.output / name
        if not module.is_file():
            raise RuntimeError(f"expected build artifact missing: {module}")
        vermagic = subprocess.check_output(["modinfo", "-F", "vermagic", str(module)], text=True)
        if vermagic.split()[0] != release:
            raise RuntimeError(f"wrong kernel vermagic: {module}")
    manifest = {
        "kernel_release": release,
        "kernel_config_sha256": hashlib.sha256((args.kernel_build / ".config").read_bytes()).hexdigest(),
        "power_domain_path": args.power_domain_path,
        "capability": "ANE_SYS with opt-in child power and CE testing; no inference",
        "sha256": {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                   for p in args.output.iterdir() if p.suffix in (".ko", ".c", ".py", ".dtbo")},
    }
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print("ANE_BUILD_COMPLETE " + manifest["kernel_release"])


if __name__ == "__main__":
    main()
