# Neo hardware audio encoding coverage — 2026-09-09

Hardware audio encoding is explicitly part of the user's native Linux media
target, alongside video encoding. No Neo audio hardware encoder has yet been
enumerated, traced or qualified. This is an evidence gap, not a finding that
the hardware lacks audio encoding.

## Scope and evidence

Count compression performed by a dedicated codec engine or demonstrated DSP
offload. CPU-only compression, an AudioToolbox/FFmpeg encoder name, playback
support, PCM capture, ADC/DAC operation, and audio effects alone do not prove
hardware compressed-audio encoding.

AAC and its variants are candidates to investigate, not confirmed Neo
hardware encoders. Enumerate all available encode formats rather than using
an AAC-only whitelist; classify any ALAC, Opus, MP3, FLAC, telephony, spatial
audio or other entries according to the actual implementation and direction.
Do not infer an encoder from a decoder or a Bluetooth playback capability.

Apple's [Neo specifications](https://support.apple.com/en-us/126322) list AAC,
MP3, Apple Lossless, FLAC and Dolby formats under **Audio Playback**. That is
not a hardware encoding inventory.

## Next reference capture

The online audit is `../A18_ENCODING_TARGET_AUDIT.md`. Apple's archived
[QA1663](https://developer.apple.com/library/archive/qa/qa1663/_index.html)
provides a specific AAC hardware discovery procedure, so AAC-LC is the first
probe. The reference is historical iOS documentation, not Neo qualification.
Also inventory APAC: Apple's
[platform codec description](https://developer.apple.com/av-foundation/Apple-Positional-Audio-Codec.pdf)
establishes an encoder on macOS 26, but does not claim hardware offload.

Use the actual Neo (Mac17,5), pinned macOS 25G76, on the next justified macOS
reference boot. Do not substitute the M3 Max controller's codec inventory or
reboot the healthy native Linux session merely to restate existing results.

1. Record target model and OS build. Enumerate CoreAudio encode format IDs,
   then query `kAudioFormatProperty_Encoders` for each. Save every returned
   `AudioClassDescription`, including type, subtype and manufacturer, and
   every status code. Preserve unknown classifications.
2. The installed Apple SDK's `AudioFormat.h` defines
   `kAppleHardwareAudioCodecManufacturer` as `aphw` and the software
   manufacturer as `appl`. Select an advertised hardware encoder explicitly
   using `AudioConverterNewSpecific`; do not allow silent software fallback.
   Check API availability before using the older hardware-capabilities query.
3. Encode deterministic PCM for every hardware-advertised format/profile.
   Save format parameters, magic cookies, packet descriptions, exact input,
   encoded output and final statuses. Independently decode and check duration,
   channels, sample rate and signal fidelity, accounting for codec priming.
4. Correlate the encode with the relevant engine/DSP activity through the
   existing KIS hypervisor workflow. Identify firmware, queues, power domains,
   DMA and IOMMU ownership before designing a Linux path.
5. Qualify each native Linux implementation with repeated encode/decode,
   clean session lifecycle and the existing CPU/AGX/video regression gates.
   Standard application integration and sustained operation are later gates;
   an isolated diagnostic packet is not general encoder support.

Read `../../neo-update/PROGRESS_WEBHOOK_GUIDE.md` in full on resume. Preserve
Stage 1, KIS transfer/readback/overlap guards and the current working video
archives. No credentials or runtime webhook endpoint belong in artifacts.
