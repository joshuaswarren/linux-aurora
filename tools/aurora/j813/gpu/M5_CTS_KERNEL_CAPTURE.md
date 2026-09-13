# Continuous kernel evidence for M5 CTS

The v25 final `dmesg` snapshot retained GPU generations 835 through 5207;
the first 834 had been overwritten. The runner now opens an independent,
read-only `/dev/kmsg` reader before starting CTS and drains it during the
existing monitoring loop. It retains the original records, including printk
sequence numbers, in `kernel-stream.log`. It also keeps the final `kernel.log`
snapshot for compatibility with older analysis.

The reader follows `Documentation/ABI/testing/dev-kmsg`: one whole record per
read, nonblocking `EAGAIN` when caught up, `EPIPE` after ring overrun, and
`SEEK_END` to begin with new messages. It never clears the ring, consumes another
reader's position, injects GPU events or changes submission ordering.

The report records capture counts, first/last sequence, overruns, sequence
discontinuities and I/O/format errors. A missing, empty or damaged capture
cannot qualify a run. Raw CTS statuses remain unchanged; an incomplete capture
marks attempted cases invalid as evidence. Complete log capture alone does not
prove GPU completion: submission and retirement records must still be matched.

Eight regression tests cover intact records with continuation lines and future
header fields, overrun before the first record, gaps/duplicates, malformed
records, empty/error reads, multiple drain batches, idle/resumed polling and
output-write failure. All pass, as do the seven existing CTS evidence tests.
A Linux VM smoke test captured a unique userspace log marker through the real
device with zero loss/errors (`logs/m5-kernel-capture-linux-smoke-20260913.log`).
That marker proves the reader works; it is not a GPU or CTS result.

This change closes the collection gap for future runs. It does not reconstruct
the missing v25 records or retroactively certify their contents. The final
hardware qualification still needs complete logs, native-driver provenance,
unchanged CTS and all required cases/configurations.

Physical v27 regression: 10,438 consecutive printk records, zero loss/errors,
and all 5,207 GPU generations independently matched to submissions with both
queues retired exactly once. See [retained evidence](M5_NATIVE_RASTER_STATE.md).
