# G17P first-submit SKSM fixtures

These 0x200-byte fixtures were admitted from
`neo-bringup/research/g17p-submit-contract/fixtures/first-submit/`.
`mine_contract.py` generates them independently of the Rust encoder from the
KEXT-proven G17P slot packing and the experiment-proven output-positive add3
operands recorded in `encoder-parity.md`.

- `sksm-entry-before.bin`: all-zero selected runtime stride;
  SHA-256 `076a27c79e5ace2a3d47f9dd2e83e4ff6ea8872b3c2218f66c92b89b55f36560`.
- `sksm-entry-after.bin`: populated first-submit stride;
  SHA-256 `ba104e14eb777736df4cec2716c5840b36241e4af2c3617e00650366ec391ca7`.
- `sksm-entry-second-after.bin`: populated second-submit stride for native
  state `current=2`, `previous_valid=1`, `previous_timestamp=1`; SHA-256
  `d888b2d41ebd53794bc7782fcf9baa910e567969148244d61ce682fbda873f47`.
- `sksm-entry-relocation-mask.bin`: one bits mark address bits normalized by
  fixture comparisons; SHA-256
  `c87e4b5e32d986a29f7901989993d3fbeada763e1e7a741f969f58aa3e926d82`.
- `channel-event-primary.bin`: KEXT-written 0x15-byte primary event prefix;
  SHA-256 `01c5248a31ed8a479b049417fa03abfcd56a975770d245baaa491fffd1266aa3`.
- `channel-event-optional.bin`: conditional 0x12-byte optional event prefix;
  SHA-256 `8667778da9e2d0c7c1f82fa249f2ecf14f1c911d006f4f9830e6a3018ffe48f2`.

The simple first add3 path publishes only the primary event. The optional
fixture is retained to lock its conditional encoding without enabling it.

Evidence classification: entry packing and runtime geometry are
`KEXT_PROVEN`; first/second add3 operands are `EXPERIMENT_PROVEN`. Native queue
initialization sets `current_timestamp`, `last_add_kicks_timestamp`, and
`base_timestamp` to `1`.
