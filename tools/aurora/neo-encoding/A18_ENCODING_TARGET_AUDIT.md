# A18 Pro encoding target audit

Checked 2026-09-09 against Apple primary sources and the saved Neo hardware
enumeration. This is a target-selection audit, not a new hardware test.

## Correct silicon reference

The Neo uses **A18 Pro**, as stated in Apple's
[Neo specifications](https://support.apple.com/en-us/126322). The base A18 in
the [iPhone 16 specifications](https://support.apple.com/en-us/121029) lists
H.264 and HEVC recording. The
[A18 Pro iPhone 16 Pro specifications](https://support.apple.com/en-us/121031)
also list ProRes recording. Do not treat the base model's recording feature
list as a complete inventory of Neo silicon.

Apple's [A18 Pro launch description](https://www.apple.com/newsroom/2024/09/apple-debuts-iphone-16-pro-and-iphone-16-pro-max/)
explicitly describes a video encoder and ProRes recording. These are media
engine targets; existing AVE/APR work is correctly directed at those engines.
General AGX compute support does not itself establish a media encoder.

## Implementation decisions

| Target | Evidence | Decision |
|---|---|---|
| H.264/AVC | Neo media-engine spec, iPhone 16 Pro recording spec, existing native AVE qualification | Core Linux target |
| HEVC/H.265 | Same official specifications and native AVE qualification | Core Linux target; qualify the additional profiles below |
| ProRes 422, HQ, LT, Proxy, 4444, XQ | Apple specifies ProRes hardware; all six variants encoded in the saved Neo macOS reference; 422 passed native Linux | Core Linux targets; finish variants and input-depth/chroma/alpha coverage |
| JPEG / Motion JPEG | Actual Neo hardware encoder selection and correlated JPEG0 MMIO during encoding | Core Linux target based on device evidence; product photo-format lists alone would not prove offload |
| HEVC Main10 / Main42210 | Actual Neo hardware encoder advertises these profiles | Explicit qualification targets, not completed capabilities |
| HEVC alpha, depth, disparity | Actual Neo advertises hardware wrappers `muxa`, `deph`, `dish` | Explicit qualification targets; retain separate input and decode checks |
| MV-HEVC / spatial video | iPhone 16 Pro specifies spatial video recording; Apple documents spatial video as stereo MV-HEVC plus metadata | Add a Neo hardware capability probe before Linux implementation; no Neo offload result yet |
| ProRes RAW / RAW HQ encoding | General Neo acceleration claim; no encoder in saved enumeration; Apple's RAW recording workflow requires iPhone 17 Pro or later | Unproven research candidate, not a required demonstrated A18 Pro encode capability |
| AV1 encoding | Neo spec explicitly lists AV1 decoding; hardware-required encoder creation failed on Neo | No established A18 Pro encoder target; do not confuse AV1 decode with encode |
| VP8 / VP9 encoding | No target established by the official sources checked; hardware-required creation failed on Neo | No established hardware encode target |

The saved inventory is
`artifacts/other-codecs-reference-qualified-20260909/results/encoders.json`:
Mac17,5, macOS 26.6.1 build 25G76, status 0, 12 entries explicitly marked
hardware. See `video/OTHER_CODECS.md` for the measured results and limits.
An exposed encoder/profile still needs a successful hardware-required encode,
independent decode and relevant engine evidence.

Apple describes [spatial video](https://developer.apple.com/documentation/imageio/creating-spatial-photos-and-videos-with-spatial-metadata)
as an MV-HEVC track with spatial metadata and provides an
[MV-HEVC encoding sample](https://developer.apple.com/documentation/AVFoundation/converting-side-by-side-3d-video-to-multiview-hevc-and-spatial-video).
This makes multiview worth probing; it does not prove Neo hardware execution.
It need not appear as a separate codec ID in an HEVC encoder list.

Apple's [Final Cut Camera RAW instructions](https://support.apple.com/guide/final-cut-camera/record-video-in-final-cut-camera-dev6b8c9521d/ios)
require iPhone 17 Pro or later. That is an application/device requirement,
not proof that A18 Pro physically cannot encode RAW. It does prevent using
that recording feature as evidence for an A18 Pro implementation commitment.

The iPhone 16 Pro spec supports ProRes recording up to 4K120 with external
storage. That is a useful future comparison point, not a Neo Linux throughput
promise or a complete per-profile silicon limit. Our fixed 320x240 diagnostics
still require resolution, throughput, quality/rate-control and streaming work.

## Audio: what the online sources actually establish

Hardware audio encoding remains in scope. No public A18/A18 Pro-specific
compressed-audio hardware encoder matrix was found in the Apple sources
checked. That is a research limitation, not a claim that no such engine exists.

Apple's archived [QA1663](https://developer.apple.com/library/archive/qa/qa1663/_index.html)
documents AAC hardware encoding and runtime discovery via
`kAudioFormatProperty_Encoders`, matching the AAC subtype and hardware
manufacturer. This supports making **AAC-LC the first hardware audio probe**.
It is an older iOS reference, not a confirmed Neo/macOS capability.

The archived [audio guide](https://developer.apple.com/library/archive/documentation/AudioVideo/Conceptual/MultimediaPG/UsingAudio/UsingAudio.html)
distinguishes hardware AAC encoding from hardware decoding of other formats.
Do not carry its old device limits or software-only labels forward as an A18
hardware matrix. In particular, do not infer HE-AAC, MP3 or ALAC hardware
encoding from their decoding support. The Neo spec's audio format list is
explicitly a playback list.

Apple's [APAC platform codec description](https://developer.apple.com/av-foundation/Apple-Positional-Audio-Codec.pdf)
documents APAC encoding on macOS/iOS/iPadOS 26 or later. It does not establish
hardware offload. Include APAC in the audio inventory, with hardware status
unknown until the actual implementation is identified. Apply the same rule
to other audio formats: enumerate all encoder IDs rather than hardcoding a
short list, then test every hardware-advertised or independently identified
offloaded encoder. See `audio/HARDWARE_ENCODING.md` for the capture gates.

## Scope boundaries

HEIC/HEIF and movie containers require integration tests; they are not extra
standalone silicon video encoders. HDR/Dolby Vision/Log, stereo/multiview,
chroma subsampling, alpha and bit depth are separately qualified capabilities,
not interchangeable codec names. PCM capture, audio effects, speaker codecs
and Bluetooth playback do not establish compressed audio hardware encoding.

Keep the proven H.264/HEVC/ProRes work. Complete the device-backed video
targets, probe MV-HEVC and AAC first in their respective discovery work, and
avoid speculative implementation of unconfirmed RAW/AV1/audio hardware paths.

Read `../neo-update/PROGRESS_WEBHOOK_GUIDE.md` in full on resume. Preserve the
resident Stage 1 and frozen evidence; this audit required no device reboot.
