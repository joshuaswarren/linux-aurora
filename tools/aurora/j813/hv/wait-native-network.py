"""Wait for the known M5 SSH host, keep it awake, and save its lease locally."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def ssh(command, timeout=12):
    return subprocess.run([
        'ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=3',
        '-o', 'ServerAliveInterval=3', '-o', 'ServerAliveCountMax=1',
        'admin@Admins-MacBook-Air.local', command,
    ], capture_output=True, text=True, timeout=timeout)


def save(directory, name, data):
    path = directory / name
    with open(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as f:
        f.write(data)
    return str(path)


if __name__ == '__main__':
    directory = Path(sys.argv[1]).resolve()
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    os.chmod(directory, 0o700)
    deadline = time.monotonic() + 900
    while time.monotonic() < deadline:
        try:
            identity = ssh('/usr/sbin/sysctl -n hw.model kern.osversion')
            if identity.returncode:
                time.sleep(3)
                continue
            if identity.stdout.splitlines() != ['Mac17,3', '25G76']:
                raise RuntimeError('SSH target is not the exact native M5 reference')
            awake = ssh('nohup /usr/bin/caffeinate -dimsu -t 3600 '
                        '>/dev/null 2>&1 </dev/null &')
            if awake.returncode:
                raise RuntimeError('Could not start the temporary native sleep assertion')
            result = ssh('/usr/sbin/ipconfig getpacket en0')
            address = ssh('/usr/sbin/ipconfig getifaddr en0')
            receipt = dict(time_ns=time.time_ns(), model='Mac17,3', build='25G76',
                           address=address.stdout.strip(), lease_exit=result.returncode,
                           sleep_assertion_seconds=3600)
            receipt['lease_capture'] = save(directory, 'native-dhcp-packet.txt', result.stdout)
            save(directory, 'native-network.json', json.dumps(receipt, indent=2) + '\n')
            print('M5_NATIVE_NETWORK_SAVED_PRIVATE', directory, flush=True)
            print('M5_NATIVE_SLEEP_ASSERTION_STARTED', 3600, flush=True)
            break
        except subprocess.TimeoutExpired:
            time.sleep(3)
    else:
        raise SystemExit('M5 native SSH did not become available within the bounded wait')
