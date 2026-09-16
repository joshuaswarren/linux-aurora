#!/usr/bin/env python3
"""Reuse the qualified credential helper with a campaign-specific log."""
from pathlib import Path
import re
import sys
label = sys.argv[1] if len(sys.argv) > 1 else 'linux-console-login'
assert re.fullmatch(r'[a-z0-9-]+', label)
log = Path(__file__).resolve().parent/(label+'.log')
assert not log.exists()
source = Path('/Users/mcruz/Developer/aurora-silicon/neo-linux/artifacts/j700-plasma-keyboard-install-20260913/login-installed-linux.py')
text = source.read_text()
old = "(here / 'linux-console-login.log')"
new = 'Path('+repr(str(log))+')'
assert text.count(old) == 1
exec(compile(text.replace(old, new), str(source), 'exec'),
     {'__name__': '__main__', '__file__': str(source)})
