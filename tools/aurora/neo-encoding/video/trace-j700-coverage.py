"""Combine bounded AVE command and APR/JPEG captures on one proxy owner."""
from pathlib import Path
_coverage_source = Path(__file__).resolve().parent
exec(compile((_coverage_source / 'trace-j700-ave-commands.py').read_text(),
             str(_coverage_source / 'trace-j700-ave-commands.py'), 'exec'), globals())
_cmd_limit = int(os.environ.get('J700_AVE_COMMAND_LIMIT', '256'))
assert 64 <= _cmd_limit <= 1024
exec(compile((_coverage_source / 'trace-j700-media.py').read_text(),
             str(_coverage_source / 'trace-j700-media.py'), 'exec'), globals())

def _coverage_poll():
    _cmd_poll()
    _media_poll()
    if not _media_active and _cmd_state == 'disarmed':
        hv.j700_diagnostic_poll_continuous = False

hv.j700_ave_command_poll = _coverage_poll
# Keep consuming phase controls beyond the 15 bounded XNU snapshots. The
# matrix explicitly disarms both captures when its last case has finished.
hv.j700_diagnostic_poll_continuous = True
