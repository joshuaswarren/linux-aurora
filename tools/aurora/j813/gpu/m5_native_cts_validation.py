"""Reject CTS evidence when a driver reports that required work was dropped.

CTS can legitimately expect a black image. A raw image-comparison Pass is
therefore insufficient when shader compilation or submission failed. This
module annotates the evidence; it never changes CTS results or expectations.
"""
import re


def driver_errors(stdout):
    """Return diagnostics with their active case, or None outside a case."""
    errors = []
    active_case = None
    for number, line in enumerate(stdout.splitlines(), 1):
        begin = re.fullmatch(r"Test case '([^']+)'\.\.", line.strip())
        if begin:
            active_case = begin.group(1)
        if any(marker in line for marker in (
                'no grounded AGX3 encoding', 'backend cannot encode',
                'asahi: dropping work:', 'fragment program is unusable')):
            errors.append({'line': number, 'case': active_case,
                           'message': line.strip()})
        if re.match(r'^\s+(?:Pass|Fail|NotSupported|InternalError|ResourceError|'
                    r'Crash|Timeout|QualityWarning|CompatibilityWarning)\s*\(', line):
            active_case = None
    return errors


def invalid_cases(errors, case_names):
    """Unscoped errors invalidate the run; scoped errors invalidate their case."""
    if any(error.get('case') is None for error in errors):
        return set(case_names)
    return {error['case'] for error in errors}
