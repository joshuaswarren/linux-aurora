"""Build diagnostic replays from QPA evidence; never infer conformance."""
from collections import Counter
import hashlib
from pathlib import Path
import re
import shlex
import xml.etree.ElementTree as ET


def read_qpa(path):
    """Stream one case at a time, retaining interrupted/malformed cases."""
    command, current, body, cases, ended = None, None, [], [], False
    with Path(path).open(errors='replace') as stream:
        for line in stream:
            if line.startswith('#sessionInfo commandLineParameters '):
                values = shlex.split(line.strip())[2:]
                assert len(values) == 1, 'Invalid QPA session command'
                command = values[0]
            elif line.startswith('#beginTestCaseResult '):
                if current is not None:
                    cases.append((current, 'Incomplete'))
                current, body = line.split(None, 1)[1].strip(), []
            elif line.startswith('#endTestCaseResult'):
                assert current is not None, 'QPA end without case start'
                status = 'Incomplete'
                try:
                    xml = ET.fromstring(''.join(body).strip())
                    result = xml.find('Result')
                    if xml.get('CasePath') == current and result is not None:
                        status = result.attrib['StatusCode']
                except (ET.ParseError, KeyError):
                    pass
                cases.append((current, status))
                current, body = None, []
            elif line.startswith('#endSession'):
                ended = True
            elif current is not None:
                body.append(line)
    if current is not None:
        cases.append((current, 'Incomplete'))
    return dict(command=command, cases=cases, ended=ended)


def build_replay(source, profile, repeat=1):
    assert profile in ('gl30', 'gl31', 'gl32', 'gl33') and 1 <= repeat <= 100
    source = Path(source).resolve()
    paths = [source] if source.is_file() else sorted(source.rglob('*.qpa'))
    groups, statuses, unique = [], Counter(), set()
    for path in paths:
        qpa = read_qpa(path)
        selected = [(case, status) for case, status in qpa['cases']
                    if case.startswith('KHR-' + profile.upper() + '.')
                    and status not in ('Pass', 'NotSupported')]
        if not selected:
            continue
        assert qpa['command'], 'Missing original session settings: ' + str(path)
        cases = [case for case, _ in selected]
        assert len(cases) == len(set(cases)), 'Duplicate cases in ' + str(path)
        assert all(re.fullmatch(r'KHR-GL3[0-3]\.[a-zA-Z0-9_.-]+', c) for c in cases)
        options = [x for x in shlex.split(qpa['command'])
                   if not x.startswith(('--deqp-log-', '--deqp-case='))]
        # A replay is derived only from the original main mustpass session.
        assert '--deqp-caselist-file=gl_cts/data/mustpass/gl/khronos_mustpass/main/' + profile + '-main.txt' in options
        digest = hashlib.sha256()
        with path.open('rb') as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b''):
                digest.update(chunk)
        groups.append(dict(source_qpa=str(path), source_sha256=digest.hexdigest(),
                           cases=cases, original_statuses=dict(selected),
                           session=dict(kind='test', command=shlex.join(options))))
        statuses.update(status for _, status in selected)
        unique.update(cases)
    assert groups, 'No recorded failures or incomplete cases to replay'
    return dict(profile=profile, repeat=repeat, groups=groups,
                original_statuses=dict(statuses), unique_cases=len(unique),
                selected_executions=sum(len(g['cases']) for g in groups) * repeat,
                conformance_passed=False,
                scope='Diagnostic subset with original settings and seed. Earlier passing cases are omitted; a replay pass does not clear an intermittent or order-dependent failure.')


def summarize_replay(directory, plan, sessions):
    counts, issues, executions = Counter(), [], []
    expected_sessions = len(plan['groups']) * plan['repeat']
    if len(sessions) != expected_sessions:
        issues.append(f'Expected {expected_sessions} sessions, recorded {len(sessions)}')
    for index, session in enumerate(sessions):
        group = plan['groups'][index % len(plan['groups'])]
        if (session['round'], session['group']) != (index // len(plan['groups']) + 1,
                                                   index % len(plan['groups']) + 1):
            issues.append('Unexpected replay session sequence')
        path = Path(directory) / session['qpa']
        if not path.is_file():
            issues.append('Missing ' + session['qpa'])
            continue
        qpa = read_qpa(path)
        def settings(command):
            return sorted(x for x in shlex.split(command or '')
                          if not x.startswith(('--deqp-log-', '--deqp-caselist-file=')))
        if settings(qpa['command']) != settings(group['session']['command']):
            issues.append('Original session settings changed: ' + path.name)
        actual = Counter(case for case, _ in qpa['cases'])
        if actual != Counter(group['cases']) or not qpa['ended']:
            issues.append('Incomplete or unexpected case inventory: ' + path.name)
        if session['exit_code'] != 0:
            issues.append('Nonzero CTS exit: ' + path.name)
        counts.update(status for _, status in qpa['cases'])
        executions.extend(dict(qpa=path.name, case=c, status=s) for c, s in qpa['cases'])
    # Every selected failure must actually pass; becoming unsupported is not
    # evidence that the original supported behavior has been repaired.
    passed = not issues and counts == {'Pass': plan['selected_executions']}
    return dict(statuses=dict(counts), expected_executions=plan['selected_executions'],
                observed_executions=sum(counts.values()), issues=issues,
                diagnostic_passed=passed, conformance_passed=False,
                executions=executions, scope=plan['scope'])
