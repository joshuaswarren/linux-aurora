#!/usr/bin/env python3
"""Guard the kernel-stack depth of render publication (module frames only).

Run against the built asahi.ko (or partially linked asahi.o). Neo's kernel
stack is 32 KiB (arm64 MIN_THREAD_SHIFT 15, 16K pages, VMAP_STACK); an
overflow there is a silent watchdog-reset hang. Render publication can run
on the submit ioctl's own stack (early publication) and, on a pair's first
use, builds and first-installs that pair inside the same call chain.

Frames are the immediate prologue allocations; chains follow direct calls
through named waypoints and take the deepest module-internal continuation
below the last one. Kernel frames above (syscall entry, drm_ioctl) and below
(printk, scheduling, allocation) are not measured here; the budgets leave
at least 8 KiB of the 32 KiB for them plus margin. `g17p_stack_watch`
measures the real high-water mark on hardware.
"""
import argparse
import re
import subprocess
import sys
from pathlib import Path

sys.setrecursionlimit(100000)

IOCTL = [r'G17PQueueNtNtB7_5queue5Queue6submit$', r'G17PQueue19submit_render_async$',
         r'17try_publish_early$', r'G17PRenderQueueBackend12publish_with$',
         r'G17PRenderQueueBackend12publish_withs_00Bb_$']
WORKER = [r'G17PRenderQueueBackend12publish_with$', r'G17PRenderQueueBackend12publish_withs_00Bb_$']
INSTALL = [r'G17PLiveRuntime24submit_translated_render$', r'23begin_translated_renderNC']
# (name, waypoints, module-frame budget in bytes)
CHAINS = [
    ('ioctl first-install with power bracket',
     IOCTL + INSTALL + [r'ensure_g17p_render_registration_powers0_0'], 17 * 1024),
    ('ioctl first-install KSMFE configure',
     IOCTL + INSTALL + [r'configure_fresh_nonbootstrap_render_pair$'], 17 * 1024),
    ('ioctl sibling warm build', IOCTL + [r'G17PWarmRenderGraph5build$'], 21 * 1024),
    ('worker warm build', WORKER + [r'G17PWarmRenderGraph5build$'], 18 * 1024),
    ('worker first-install with power bracket',
     WORKER + INSTALL + [r'ensure_g17p_render_registration_powers0_0'], 14 * 1024),
]


def analyze(path):
    output = subprocess.check_output(
        ['llvm-objdump', '-dr', '--no-show-raw-insn', str(path)], text=True)
    frame, calls, current, prologue = {}, {}, None, False
    for line in output.splitlines():
        header = re.match(r'^[0-9a-f]+ <(.+)>:$', line)
        if header:
            current = header.group(1)
            frame[current], calls[current], prologue = 0, set(), True
            continue
        if current is None:
            continue
        if 'R_AARCH64_CALL26' in line or 'R_AARCH64_JUMP26' in line:
            calls[current].add(re.sub(r'\+0x[0-9a-f]+$', '', line.split()[-1]))
            continue
        direct = re.search(r'\bbl\s+0x[0-9a-f]+ <([^+>]+)(\+0x[0-9a-f]+)?>', line)
        if direct:
            calls[current].add(direct.group(1))
        tail = re.search(r'\bb\s+0x[0-9a-f]+ <([^+>]+)>$', line)
        if tail and tail.group(1) != current:
            calls[current].add(tail.group(1))
        if prologue:
            sub = re.search(r'\bsub\s+sp, sp, #(0x[0-9a-f]+|\d+)', line)
            push = re.search(r'\b(?:stp\s+x\d+, x\d+|str\s+x\d+), \[sp, #-(0x[0-9a-f]+|\d+)\]!', line)
            if sub:
                frame[current] += int(sub.group(1), 0) << (12 if 'lsl #12' in line else 0)
            elif push:
                frame[current] += int(push.group(1), 0)
            elif re.search(r'\b(bl|b|ret|retaa|cbz|cbnz|tbz|tbnz|b\.\w+)\b', line):
                prologue = False
    return frame, calls


def deepest_below(frame, calls):
    memo, active = {}, set()
    def walk(node):
        if node in memo:
            return memo[node]
        if node not in frame or node in active:
            return 0
        active.add(node)
        value = frame[node] + max((walk(c) for c in calls.get(node, ())), default=0)
        active.discard(node)
        memo[node] = value
        return value
    return walk


def longest_to(frame, calls, entry, pattern):
    memo, active, target = {}, set(), re.compile(pattern)
    def walk(node):
        if node in memo:
            return memo[node]
        if node not in frame or node in active:
            return None
        if target.search(node):
            memo[node] = (frame[node], [node])
            return memo[node]
        active.add(node)
        best = None
        for callee in calls.get(node, ()):
            found = walk(callee)
            if found and (best is None or found[0] > best[0]):
                best = found
        active.discard(node)
        memo[node] = (frame[node] + best[0], [node] + best[1]) if best else None
        return memo[node]
    return walk(entry)


def chain(frame, calls, below, waypoints):
    starts = [name for name in frame if re.search(waypoints[0], name)]
    assert len(starts) == 1, (waypoints[0], len(starts))
    node, total, path = starts[0], frame[starts[0]], [starts[0]]
    for pattern in waypoints[1:]:
        found = longest_to(frame, calls, node, pattern)
        assert found, (node, pattern)
        total += found[0] - frame[node]
        path += found[1][1:]
        node = path[-1]
    return total + below(node) - frame[node], path


def audit(path):
    frame, calls = analyze(path)
    below = deepest_below(frame, calls)
    violations = []
    for name, waypoints, budget in CHAINS:
        total, path_ = chain(frame, calls, below, waypoints)
        print(f'{name}: {total} bytes (budget {budget})')
        for node in path_:
            print(f'  {frame[node]:6d} {re.sub(r"_R[A-Za-z0-9]*?5asahi", "", node)[:110]}')
        if total > budget:
            violations.append((name, total, budget))
    assert not violations, violations
    print('PASS publication stack budgets (module frames; kernel frames unmeasured)')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('object', type=Path)
    audit(parser.parse_args().object)
