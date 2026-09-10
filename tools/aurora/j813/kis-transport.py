"""Task-local KIS transport workaround, shared by the guest wrapper and checks."""
import gzip
import hashlib


def install():
    from m1n1.proxy import UartInterface
    from m1n1.proxyutils import ProxyUtils

    original_write = UartInterface.writemem

    def write_chunks(self, address, data, progress=False, retry=False):
        for offset in range(0, len(data), 1024):
            original_write(self, address + offset, data[offset:offset + 1024], False)

    def compressed_write(self, destination, data, progress=None):
        if not data:
            return
        compressed = gzip.compress(data, compresslevel=9, mtime=0)
        compressed_length = len(compressed)
        transport = compressed + bytes((-compressed_length) % 1024)
        print(f'KIS_UPLOAD dest={destination:#x} raw={len(data)} '
              f'gzip={compressed_length} padded={len(transport)}', flush=True)
        with self.heap.guarded_malloc(len(transport)) as address:
            self.iface.writemem(address, transport)
            old_timeout = self.iface.dev.timeout
            self.iface.dev.timeout = 30
            try:
                actual = self.proxy.gzdec(address, compressed_length, destination, len(data))
                if actual != len(data):
                    raise RuntimeError(f'KIS decompression length {actual} != {len(data)}')
                # Large reads can exhaust the KIS receive window: a 1-MiB
                # transaction also truncated during Bluetooth image checking.
                # Keep transactions at 256 KiB while still hashing every byte.
                readback_digest = hashlib.sha256()
                for offset in range(0, len(data), 256 * 1024):
                    size = min(256 * 1024, len(data) - offset)
                    chunk = self.iface.readmem(destination + offset, size)
                    if len(chunk) != size:
                        raise RuntimeError(f'KIS short readback at {destination + offset:#x}')
                    readback_digest.update(chunk)
            finally:
                self.iface.dev.timeout = old_timeout
        expected = hashlib.sha256(data).digest()
        if readback_digest.digest() != expected:
            raise RuntimeError(f'KIS guest readback mismatch at {destination:#x}')
        print(f'KIS_READBACK_OK dest={destination:#x} size={len(data)} '
              f'sha256={expected.hex()}', flush=True)

    UartInterface.writemem = write_chunks
    ProxyUtils.compressed_writemem = compressed_write
