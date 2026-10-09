"""Retain /dev/kmsg records and detect lost evidence during a CTS run."""
import errno
import os


class KernelCapture:
    """Independent, non-destructive printk reader, starting before GPU work."""

    def __init__(self, output):
        self.fd = os.open('/dev/kmsg', os.O_RDONLY | os.O_NONBLOCK)
        try:
            # Do not attribute pre-existing records to this run. No other
            # reader's position or the kernel ring is changed by this seek.
            os.lseek(self.fd, 0, os.SEEK_END)
            self.output = output.open('wb')
        except BaseException:
            os.close(self.fd)
            raise
        self.status = {'records': 0, 'first_sequence': None,
                       'last_sequence': None, 'overruns': 0,
                       'sequence_errors': 0, 'errors': [], 'complete': False}
        self.read_failed = False

    def drain(self):
        """Read a bounded batch; return True when caught up or unable to read."""
        if self.read_failed:
            return True
        for _ in range(4096):
            try:
                record = os.read(self.fd, 65536)
            except BlockingIOError:
                return True
            except OSError as error:
                if error.errno == errno.EPIPE:
                    # Linux advances to the oldest surviving record. Keep
                    # collecting, but never call this capture complete.
                    self.status['overruns'] += 1
                    continue
                self.status['errors'].append(str(error))
                self.read_failed = True
                return True
            if not record:
                self.status['errors'].append('Unexpected end of /dev/kmsg')
                self.read_failed = True
                return True
            try:
                self.output.write(record)
            except OSError as error:
                self.status['errors'].append(str(error))
                self.read_failed = True
                return True
            self.status['records'] += 1
            try:
                header, separator, _ = record.partition(b';')
                fields = header.split(b',')
                if not separator or len(fields) < 4:
                    raise ValueError('Invalid kmsg header')
                sequence = int(fields[1])
                if sequence < 0:
                    raise ValueError('Invalid kmsg sequence')
            except ValueError as error:
                self.status['errors'].append(str(error))
                continue
            previous = self.status['last_sequence']
            if previous is None:
                self.status['first_sequence'] = sequence
            elif sequence != previous + 1:
                self.status['sequence_errors'] += 1
            self.status['last_sequence'] = sequence
        return False

    def close(self):
        try:
            while not self.drain():
                pass
            try:
                self.output.close()
            except OSError as error:
                self.status['errors'].append(str(error))
            self.status['complete'] = bool(
                self.status['records'] and not self.status['overruns']
                and not self.status['sequence_errors'] and not self.status['errors'])
        finally:
            os.close(self.fd)
        return self.status
