"""Read-only witnesses for the 25G83 render/completion generation identity.

native_b1 verifies the input hashes before exposing any instructions. These
checks preserve selected observations, not a proof of the whole control flow
or of the Linux hardware fix. See READBACK-GENERATION-AUDIT.md.
"""
from native_b1 import NativeAGX, NativeB1


WITNESSES = {
    "AGX": [
        (0xfffffe0008862e78, "ldr", "w22, [x20, #0x180]"),
        (0xfffffe0008862ee0, "add", "w8, w22, #1"),
        (0xfffffe0008862ee4, "str", "w8, [x20, #0x180]"),
        (0xfffffe0008862f18, "stp", "w22, wzr, [x21, #8]"),
        (0xfffffe000886288c, "stp", "x8, x9, [x0, #0x188]"),
        (0xfffffe00088379cc, "ldr", "x8, [x20, #0x188]"),
        (0xfffffe00088379d0, "str", "x8, [x24, #0x20]"),
        (0xfffffe00088c04cc, "ldr", "x10, [x19, #0x190]"),
        (0xfffffe00088c04d0, "ldr", "w10, [x10, #8]"),
        (0xfffffe00088c04d4, "and", "w10, w10, #0x3f"),
        (0xfffffe00088c04d8, "ldr", "w11, [x19, #0x144]"),
        (0xfffffe00088c04dc, "orr", "w23, w10, w11, lsl #8"),
        (0xfffffe00088bdd28, "ldr", "x10, [x20, #0x190]"),
        (0xfffffe00088bdd2c, "ldr", "w10, [x10, #8]"),
        (0xfffffe00088bdd30, "and", "w10, w10, #0x3f"),
        (0xfffffe00088bdd34, "ldr", "w11, [x20, #0x1274]"),
        (0xfffffe00088bdd38, "orr", "w22, w10, w11, lsl #8"),
    ],
    "B1": [
        (0xc9e8, "ldr", "w9, [x26, #0xc]"),
        (0xc9f0, "ldr", "x27, [x26, #0x20]"),
        (0xd250, "ldr", "w8, [sp, #0x9c]"),
        (0xd254, "str", "w8, [x27, #0x24]"),
        (0x16b6c, "ldr", "w10, [x21, #8]"),
        (0x16b70, "ldr", "w8, [x21, #0x24]"),
        (0x16bb8, "ubfiz", "x10, x10, #0x18, #6"),
        (0x16bbc, "orr", "x10, x11, x10"),
        (0x16bc0, "orr", "x10, x10, #0x70000"),
        (0x16bcc, "str", "x10, [x8, x11]"),
        (0x16ea0, "str", "w22, [x25]"),
    ],
}


def main():
    for name, reader in [("AGX", NativeAGX()), ("B1", NativeB1())]:
        for address, mnemonic, operands in WITNESSES[name]:
            decoded = reader.disassemble(address, address + 4)
            if len(decoded) != 1 or (decoded[0].mnemonic, decoded[0].op_str) != (mnemonic, operands):
                raise RuntimeError(f"{name} witness mismatch at {address:#x}")
        print(f"{name}: {len(WITNESSES[name])} pinned instruction witnesses verified")
    print("Native generation identity witnesses verified; hardware causality remains to be tested.")


if __name__ == "__main__":
    main()
