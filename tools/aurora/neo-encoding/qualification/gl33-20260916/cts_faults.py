"""GPU execution failures that invalidate a hardware qualification run."""
import re
from pathlib import Path

FATAL_GPU_FAULT = re.compile(
    rb'asahi[^\n]*(?:halted GMMU|completion timed out|retiring slot)'
    rb'|asahi[^\n]*G17PMARK session-crashed\b'
    rb'|asahi[^\n]*G17PMARK rtkit-boot[^\n]*\bok=false\b'
    rb'|asahi[^\n]*IOCTL: submit failed!'
    rb'|translation fault|GPU (?:fault\b|firmware crashed\b)',
    re.IGNORECASE,
)


def scan_kernel_faults(paths, sample_limit=12):
    """Rescan saved evidence; missing logs never establish a clean run."""
    observed, missing, samples = [], [], []
    count = 0
    for name in paths:
        path = Path(name)
        if not path.is_file():
            missing.append(path.name)
            continue
        observed.append(path.name)
        with path.open('rb') as source:
            for number, line in enumerate(source, 1):
                if FATAL_GPU_FAULT.search(line):
                    count += 1
                    if len(samples) < sample_limit:
                        samples.append(dict(log=path.name, line=number,
                                            message=line.decode(errors='replace').strip()))
    return dict(fault_detected=bool(count) if observed else None,
                evidence_complete=bool(observed) and not missing,
                observed_logs=observed, missing_logs=missing,
                matching_lines=count, samples=samples)
