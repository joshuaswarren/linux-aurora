import json
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parent / 'native-quick-textures'
for receipt in sorted(root.glob('*/receipt.json')):
    data = json.loads(receipt.read_text())
    for section in data.get('sections', []):
        if section['segment'] != '__TEXT' or section['name'] != '__text':
            continue
        path = receipt.parent / section['file']
        result = subprocess.run(['limactl', 'shell', 'neo-builder',
                                 '/home/mcruz.guest/neo/build/disasm-native-textures', str(path)],
                                capture_output=True, timeout=20)
        path.with_suffix('.disasm').write_bytes(result.stdout + result.stderr)
        print(receipt.parent.name, path.name, 'decode_exit', result.returncode)
