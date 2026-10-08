"""Diagnostic result contracts, deliberately separate from CTS conformance."""
import re


def summarize_sparse_depth(text, exit_code, kernel_fault, require_hardware=True):
    renderers = re.findall(r'^sparse depth renderer: (.*); GL: (.*)$', text, re.M)
    tested = [tuple(map(int, match)) for match in re.findall(
        r'^sparse depth: (\d+)x, (\d+) masks, both discard phases and depth passes verified$', text, re.M)]
    rounded = [tuple(map(int, match)) for match in re.findall(
        r'^sparse depth: requested (\d+)x, got (\d+)x; not tested$', text, re.M)]
    summaries = re.findall(r'^sparse depth: (PASS|FAIL), (\d+) exact sample counts, (\d+) sample observations$', text, re.M)
    issues = []
    if len(renderers) != 1:
        issues.append('Missing or duplicate renderer identity')
    elif require_hardware and not renderers[0][0].startswith('Apple A18 Pro'):
        issues.append('Probe did not execute on the Neo GPU')
    tested_counts = [sample for sample, _ in tested]
    rounded_counts = [sample for sample, _ in rounded]
    all_counts = tested_counts + rounded_counts
    if sorted(all_counts) != [2, 4]:
        issues.append('Missing or duplicate sample-count accounting')
    if any(masks != 1 << sample for sample, masks in tested if sample in (2, 4)):
        issues.append('Not all API masks completed')
    if any(requested == actual or actual < requested for requested, actual in rounded):
        issues.append('Invalid rounded sample-count evidence')
    observations = sum(16 * 16 * sample * (1 << sample) * 2 * 2
                       for sample in tested_counts if sample in (2, 4))
    if len(summaries) != 1:
        issues.append('Missing or duplicate terminal summary')
    else:
        verdict, counts, checked = summaries[0]
        if verdict != 'PASS' or not tested or int(counts) != len(tested) or int(checked) != observations:
            issues.append('Incomplete or inconsistent observation totals')
    if exit_code != 0:
        issues.append('Probe process did not exit successfully')
    if kernel_fault:
        issues.append('Kernel GPU fault observed')
    if 'GL error' in text or 'FBO creation failed' in text:
        issues.append('Graphics error in diagnostic log')
    return dict(kind='sparse-depth', passed=not issues, conformance_passed=False,
                renderer=renderers[0][0] if len(renderers) == 1 else None,
                gl_version=renderers[0][1] if len(renderers) == 1 else None,
                exact_sample_counts=tested_counts,
                untested_sample_counts=[dict(requested=a, actual=b) for a, b in rounded],
                observations=observations, issues=issues)
