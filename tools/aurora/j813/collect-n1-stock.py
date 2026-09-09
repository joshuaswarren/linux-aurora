#!/usr/bin/env python3
"""Collect the M5's exact stock driver as private, read-only RE inputs.

This never changes the target's network, firmware, security policy or boot state.
Network identities and raw IORegistry data are deliberately not retained.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shlex
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ssh-target", required=True)
    parser.add_argument("--expected-build", default="25G76")
    parser.add_argument("--resume", action="store_true", help="revalidate a partial collection")
    args = parser.parse_args()
    os.umask(0o077)
    root = Path(__file__).resolve().parent.parent
    output = root / "AuroraSilicon/build/j813-centauri/re/stock-macos" / args.expected_build
    if output.exists() and not args.resume:
        raise SystemExit("Refusing to overwrite an existing stock reference")
    ssh = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", args.ssh_target]

    def remote(command, timeout=30):
        return subprocess.run(ssh + [command], check=True, capture_output=True,
                              timeout=timeout).stdout

    identity = remote("/usr/sbin/sysctl -n hw.model; /usr/bin/sw_vers -productVersion; "
                      "/usr/bin/sw_vers -buildVersion; /usr/bin/uname -v").decode().splitlines()
    if len(identity) != 4 or identity[0] != "Mac17,3" or identity[2] != args.expected_build:
        raise SystemExit("M5 model/build verification failed")
    if "RELEASE_ARM64_T8142" not in identity[3]:
        raise SystemExit("M5 kernel verification failed")
    output.mkdir(parents=True, mode=0o700, exist_ok=args.resume)
    report = dict(model=identity[0], version=identity[1], build=identity[2], kernel=identity[3],
                  collection="SSH read-only; no tracing", artifacts={}, registry={})

    def collect(path, name):
        # The signed stock images are not secret, but keep all RE inputs private.
        quoted = shlex.quote(path)
        expected = remote("/usr/bin/shasum -a 256 " + quoted).decode().split()[0]
        destination = output / name
        data = destination.read_bytes() if destination.exists() else remote("/bin/cat " + quoted, timeout=90)
        observed = hashlib.sha256(data).hexdigest()
        if observed != expected:
            raise RuntimeError("Stock artifact changed during collection: " + name)
        if not destination.exists():
            with destination.open("xb") as stream:
                stream.write(data)
        report["artifacts"][name] = dict(bytes=len(data), sha256=observed, source=path)
        return data

    cache_dir = "/System/Volumes/Preboot/Cryptexes/OS/System/DriverKit/System/Library/dyld/"
    for name in ("dyld_shared_cache_arm64e", "dyld_shared_cache_arm64e.symbols"):
        collect(cache_dir + name, name)
    framework_dir = "/System/DriverKit/System/Library/PrivateFrameworks/"
    collect(framework_dir + "AirshipDK.framework/t2026-CentauriAlpha.plist", "CentauriAlpha.plist")
    info = collect(framework_dir + "WLANACIAdapter.framework/Info.plist", "WLANACIAdapter-Info.plist")
    report["adapter_platform_build"] = plistlib.loads(info).get("DTPlatformBuild")

    tree = plistlib.loads(remote("/usr/sbin/ioreg -a -l -w0 -p IOService"))
    wanted = {
        "AppleWLANInterfaceSTA": ("IO80211DriverVersion", "IO80211HardwareVersion",
                                 "IO80211RSNDone", "IO80211Channel", "IO80211ChannelBandwidth",
                                 "IO80211Band", "IO80211CountryCode"),
        "CentauriAlpha": ("AirshipDriverConfigName", "BootComplete", "PTM", "NumMSIs"),
        "CentauriControl": ("AirshipDriverConfigName", "PTM", "NumMSIs"),
    }

    def walk(nodes):
        if isinstance(nodes, dict):
            nodes = [nodes]
        for node in nodes:
            name = node.get("IORegistryEntryName")
            if name in wanted:
                report["registry"][name] = {k: node[k] for k in wanted[name] if k in node}
            walk(node.get("IORegistryEntryChildren", []))

    walk(tree)
    for name in ("WLANACIAdapter", "WLANHWAdapter", "AppleWLANPrime",
                 "AirshipDK", "CentauriAlpha", "CentauriControl"):
        subprocess.run(["ipsw", "dyld", "extract", str(output / "dyld_shared_cache_arm64e"),
                        name, "--slide", "--stubs", "--output", str(output / "images")],
                       check=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=45)
    with (output / "manifest.json").open("x") as stream:
        json.dump(report, stream, indent=2)
        stream.write("\n")
    print("STOCK_N1_REFERENCE_VERIFIED", identity[1], identity[2])
    print(output)


if __name__ == "__main__":
    main()
