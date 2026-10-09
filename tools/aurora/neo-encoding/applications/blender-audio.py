"""Exercise Audaspace FFmpeg encoding with generated audio, not physical sound."""
import aud
import json
import os
from pathlib import Path

out = Path(os.environ['NEO_SUITE_OUTPUT'])
tone = aud.Sound.sine(440).limit(0, 1)
tone.write(str(out / 'blender-audio.flac'), 48000, 1, aud.FORMAT_S16,
           aud.CONTAINER_FLAC, aud.CODEC_FLAC, 192000, 4096)
tone.write(str(out / 'blender-audio.mka'), 48000, 1, aud.FORMAT_FLOAT32,
           aud.CONTAINER_MATROSKA, aud.CODEC_AAC, 128000, 4096)
(out / 'audio-export.json').write_text(json.dumps({'source': 'generated 440 Hz sine',
    'duration_seconds': 1, 'sample_rate': 48000, 'physical_audio_claim': False}) + '\n')
