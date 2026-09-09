"""Select the existing M5 m1n1 volume for one boot, then restart the M5.

Uses the user's existing SSH access and Keychain credential over stdin.
No image installation, snapshot change or default startup-disk change.
"""
import plistlib
import subprocess

from host_auth import keychain_password

SSH = ['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=5',
       'admin@Admins-MacBook-Air.local']


def read(command):
    return subprocess.check_output(SSH + [command], timeout=15)


def main():
    if read('sysctl -n hw.model; sw_vers -buildVersion; id -un').splitlines() != [b'Mac17,3', b'25G76', b'admin']:
        raise RuntimeError('Native SSH target is not the expected M5 installation')
    info = plistlib.loads(read('/usr/sbin/diskutil info -plist 79E3244A-E3E3-41E4-99E6-EE4E1A765B6A'))
    if (info.get('VolumeUUID') != '79E3244A-E3E3-41E4-99E6-EE4E1A765B6A' or
            info.get('APFSVolumeGroupID') != '154559C3-3E4F-42C3-B06B-16A9F72CEA78' or
            info.get('MountPoint') != '/Volumes/m1n1 proxy'):
        raise RuntimeError('Existing m1n1 boot volume identity changed')
    password = keychain_password('aurora-silicon.m5-macos', 'admin')
    try:
        command = "sudo -S -p '' /usr/sbin/bless --mount '/Volumes/m1n1 proxy' --setBoot --nextonly --user admin --stdinpass"
        result = subprocess.run(SSH + [command], input=(password + b'\n') * 2,
                                capture_output=True, timeout=45)
        if result.returncode:
            detail = (result.stdout + result.stderr).replace(password, b'[redacted]')
            raise RuntimeError('Next-boot selection failed: ' + detail.decode(errors='replace')[:1200])
        print('M5_NEXT_BOOT_M1N1_SELECTED: existing volume, next boot only', flush=True)
        result = subprocess.run(SSH + ["sudo -S -p '' /sbin/shutdown -r now"],
                                input=password + b'\n', capture_output=True, timeout=15)
        if result.returncode not in (0, 255):
            raise RuntimeError('M5 restart request failed')
        print('M5_NATIVE_RESTART_REQUESTED', flush=True)
    finally:
        del password


if __name__ == '__main__':
    main()
