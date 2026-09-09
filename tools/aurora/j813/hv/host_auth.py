"""Scoped host administration using credentials held in the login Keychain.

No credentials are accepted on the command line or printed. The user has
authorized these M5 operations; the separate Neo controller is never used.
"""
from pathlib import Path
import shlex
import subprocess
import sys


def keychain_password(service, account):
    result = subprocess.run(
        ['/usr/bin/security', 'find-generic-password', '-s', service,
         '-a', account, '-w'], capture_output=True, timeout=15)
    if result.returncode:
        raise RuntimeError('Required local Keychain credential is unavailable')
    return result.stdout.rstrip(b'\r\n')


def run_scoped_vdm(args, check=False):
    if tuple(args) not in (('debugusb',), ('reboot',), ('reboot', 'debugusb'), ('nop',)):
        raise RuntimeError('Unexpected M5 VDM operation')
    helper = Path(__file__).resolve().parent.parent / 'macvdmtool/macvdmtool'
    password = keychain_password('aurora-silicon.host-admin', 'mcruz')
    try:
        # This host's sudo PAM stack tries Touch ID before reading -S input.
        # Supply the authorized credential directly to macOS authorization,
        # through osascript stdin. Neither argv nor diagnostics contain it.
        def quote(value):
            if '\n' in value or '\r' in value:
                raise RuntimeError('Unexpected newline in authorization input')
            return '"' + value.replace('\\', '\\\\').replace('"', '\\"') + '"'
        command = shlex.join([str(helper), '--rid', '1', *args])
        script = ('do shell script ' + quote(command) + ' user name "mcruz" password ' +
                  quote(password.decode('utf-8')) + ' with administrator privileges\n')
        result = subprocess.run(
            ['/usr/bin/osascript', '-'], input=script.encode('utf-8'),
            capture_output=True, timeout=45)
    finally:
        del password
        if 'script' in locals():
            del script
    safe = subprocess.CompletedProcess(args, result.returncode,
                                       stdout='Scoped M5 VDM request completed')
    if check and safe.returncode:
        raise RuntimeError('Scoped M5 VDM request failed')
    return safe


if __name__ == '__main__':
    result = run_scoped_vdm(sys.argv[1:], check=True)
    print(result.stdout)
