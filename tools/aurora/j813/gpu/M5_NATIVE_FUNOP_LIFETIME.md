# M5 native floating-point instruction lifetimes

## Compiler correction

G17G logarithm, exponential and rounding instructions inherited register
lifetime and wait fields from a fixed Apple instruction used for initial
encoding research. That template marked its input as a last use and carried
a slot-eight wait. These fields must instead follow the actual post-register-
allocation instruction and dependency schedule.

The M5 encoder now forwards destination caching, source caching and source
discard information from Mesa. It also replaces every wait field with the
scheduler's mask, including clearing a template wait when no dependency is
outstanding. The correction changes neither arithmetic nor CTS tolerances,
and adds no draw splitting, submission barriers or unconditional waits.
The legacy corpus-replay interface and the G17P runtime path are unchanged.

## Independent Apple reference

`m5-funop-lifetime-reference.metal` contains `log2(x) * x`, `exp2(x) * x`,
their last-use counterparts, and each rounding operation multiplied by its
original input. The associated pipeline script targets `applegpu_g17g` with
Metal Toolchain 27.1.5194.15 and an explicit macOS 26.0 deployment target.
Use the build sequence in `M5_NATIVE_SHADER_REFERENCES.md`.

Native binary SHA256:
`917e0fcec85949bdabe1ed1a5dc5b427f86a0bbffab01c452c495b0ed0baf184`.

All 32 relevant native instructions were decomposed and reconstructed
byte-for-byte. The retained-input witnesses set source cache field 27 and
clear source discard field 29. Last-use logarithm/exponential witnesses do
the reverse. Destination cache is field 11; fields 35 through 42 carry
the actual dependency wait. Only the first instruction after the buffer
load waits on slot eight; subsequent independent instructions have no wait.

Sixteen representative byte-exact vectors cover all six operations,
retained/last-use logarithm and exponential inputs, and both zero and
slot-eight waits in Mesa's `test-agx3-funop-core.c`.

## Hardware qualification: all clear and blend cases pass

- 72 instruction-encoder checks pass, including the new native vectors.
- 426 compiler tests and the FALU suite pass.
- sRGB epilog and G17G raster-state tests pass.
- All 2,304 unmodified blend cases compile with no instruction refusal,
  descriptor refusal, assertion or dropped-work diagnostic. The compiler-only
  shim rejects all submissions, so its image comparisons are not GPU evidence.
- The physical M5 completes all 2,323 unchanged clear/blend cases with
  **2,323 Pass / 0 Fail**. All 48 v27 sRGB failures now pass; no previous
  pass regresses and the exact case set is unchanged.

The destination-only sRGB epilog contains live logarithm inputs. Preserving
their actual register lifetimes fixes the corrupted RGB result. Alpha remains
linear; the sRGB equation and conversion path are unchanged.

Native renderer and mapped library provenance are verified. Driver SHA256:
`5ab96b9f51fb493e1d7d1845fd80c2a3370ee1ab125397d0d1c5c6eb7d23dcfe`.
Mesa source commit: `43f0a055cf6` on `j813`.
There are no driver errors, invalid/missing/unexpected cases, timeout or
thermal abort. The run takes 313.090 seconds with one CPU capped at 1.308 GHz.
The sampled hottest-channel temperature ranges from 43.840 to 44.521 C.
These diagnostic settings are not a normal-settings performance benchmark.

Continuous kernel capture contains 10,588 consecutive records, sequences
1024 through 11611, with no overrun, sequence error or read/write error.
Every GPU generation 1 through 5282 completes with both queues retired.

The run submits 15,775 draws in 5,282 submissions; 5,260 submissions contain
multiple draws and the largest has 394. Thirty previously failing cases
now reach their remaining CTS parameter sets, adding 75 two-draw submissions.
All other 2,293 cases have identical per-submission draw counts to v27.
CTS stops a case early after a failed comparison; no driver draw splitting
or additional execution barrier accounts for the extra work.

Retained evidence:

- `m5-native-gles3-funop-lifetime.json`: complete runner report.
- `m5-native-gles3-funop-lifetime.qpa.gz`: unchanged CTS result log; decompressed
  SHA256 `c3780307baec180a2e0ae15e229cfdbb1b10a117f7cdc3b54b69dfba7999e4f7`.
- `m5-native-gles3-funop-lifetime-kernel.log.gz`: complete kernel stream.

The full local GLES 3.0 catalog remains **2,323 Pass / 52,476 NotRun** out of
54,799 cases. This qualifies clear/blend coverage only; full conformance and
the remaining graphics features and context lifetimes are still incomplete.
