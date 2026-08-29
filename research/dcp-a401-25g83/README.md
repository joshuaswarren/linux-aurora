# H17P 25G83 DCP A401 research

This directory records the exact firmware artifact and the static evidence for
the EP37 `A401` return gate. The downloaded and decompressed binaries stay
local and are ignored by Git.

## Exact artifact

- macOS build: `25G83`
- restore image:
  `https://updates.cdn-apple.com/2026SummerFCS/fullrestores/140-75212/A2A24B94-1FC1-45A3-93F7-C51B02AF1F4D/UniversalMac_26.6.2_25G83_Restore.ipsw`
- member: `Firmware/dcp/t8140dcp.im4p`
- IM4P size: `2199165`
- IM4P SHA-256:
  `c1264b525286bdba570aad80dfa37f8bd2482197d84e3d2cc3b72aab9a5943a4`
- decompressed size: `15417344`
- decompressed SHA-256:
  `392abf0c42733c5b35e877f72f2c0a3a610c1f89aa39c0ce8358092451d9a8af`
- firmware build string: `AppleDCP-1041.120.7~2715-t8140dcp.RELEASE`
- root-task Mach-O UUID: `5D107A64-A724-3F11-A003-C47C4D8929D4`

`ipsw img4 im4p info` identifies the payload as IM4P type `dcpf`, version 1,
LZFSE-compressed, with a 15 MB uncompressed size. Extraction needs no key.
The payload is not encrypted. The root-task Mach-O symbol table was omitted
from the firmware bundle, so the reverse engineering uses code, strings,
vtable fixups, and control-flow xrefs rather than symbols.

## Reproduction

Run from this directory:

```sh
mkdir -p source extracted
ipsw download ipsw --macos --build 25G83 \
  --pattern 'Firmware/dcp/t8140dcp[.]im4p$' \
  --output source --confirm --no-color
ipsw img4 im4p info source/25G83__MacOS/Firmware/dcp/t8140dcp.im4p
ipsw img4 im4p extract \
  --output extracted/t8140dcp-25G83.bin \
  source/25G83__MacOS/Firmware/dcp/t8140dcp.im4p
shasum -a 256 \
  source/25G83__MacOS/Firmware/dcp/t8140dcp.im4p \
  extracted/t8140dcp-25G83.bin
python3 tools/dcp25g83_analyze.py verify
```

The range downloader fetches only the named DCP member, not the full restore
image.

## Bundle layout used by the tool

| Entry | File offset | Size |
| --- | ---: | ---: |
| `ktxt` | `0x010000` | `0x017000` |
| `rtxt` | `0x027000` | `0x4b1000` |
| `utxt` | `0x4d8000` | `0x008000` |
| `kdat` | `0x4e0000` | `0x007000` |
| `rdat` | `0x4e7000` | `0x985000` |
| `dsro` | `0xe6c000` | `0x008000` |
| `nold` | `0xe74000` | `0x018000` |
| `ubdl` | `0xe8c000` | `0x026000` |

The root task has `__TEXT` at VM address `0x04000000` and `__DATA` at
`0x04459000`. Root-task text starts at bundle offset `0x027000`; root-task
data starts at `0x4e7000`.

The full consumer contract and live-log correlation are in
[`A401_CONSUMER_CONTRACT.md`](A401_CONSUMER_CONTRACT.md).
