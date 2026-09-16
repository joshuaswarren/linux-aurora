"""Read and retain J700 GPU dumps without acknowledging/removing them."""
import hashlib
from pathlib import Path


def read_bounded(source, limit):
    data = bytearray()
    while len(data) < limit:
        chunk = source.read(min(65536, limit - len(data)))
        if not chunk:
            break
        data.extend(chunk)
    return bytes(data)


def collect(output, sysfs=Path('/sys/class/devcoredump'), max_bytes=16 * 1024 * 1024,
            max_dumps=4):
    records = []
    for entry in sorted(sysfs.glob('devcd*')):
        device = (entry / 'failing_device').resolve()
        driver = (device / 'driver').resolve()
        if device.name != '482600000.gpu' or driver.name != 'asahi':
            continue
        record = dict(name=entry.name, device=str(device), driver=driver.name)
        records.append(record)
        if len(records) > max_dumps:
            record['error'] = 'dump count limit reached; remaining dumps left in sysfs'
            break
        destination = output / (entry.name + '.bin')
        try:
            with (entry / 'data').open('rb', buffering=0) as source:
                data = read_bounded(source, max_bytes + 1)
            record['truncated'] = len(data) > max_bytes
            data = data[:max_bytes]
            with destination.open('xb') as saved:
                saved.write(data)
            record.update(file=destination.name, size=len(data),
                          sha256=hashlib.sha256(data).hexdigest())
        except OSError as error:
            record['error'] = str(error)
    return records
