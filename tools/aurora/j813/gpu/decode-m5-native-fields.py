#!/usr/bin/env python3
"""Inspect candidate field decompositions of original native instruction bytes.

This is an analysis aid, not a semantic decoder. A candidate must be checked
against the production composer and independent native instruction witnesses.
"""
import re
import struct
import sys
from pathlib import Path

table, mnemonic, instruction = sys.argv[1:]
source = Path(table).read_text()


def number(text):
    return int(re.sub(r'[uUlL]+$', '', text.strip()), 0)


def array(name):
    text = re.search(r'\b' + name + r'\[\] = \{(.*?)\n\};', source, re.S)[1]
    if '{' in text:
        return [tuple(number(x) for x in row.split(','))
                for row in re.findall(r'\{([^{}]+)\}', text)]
    return [number(x) for x in text.split(',') if x.strip()]


nodes, groups, alternatives, fixeds, predicates, commands, lut, sets = map(
    array, ['nodes', 'groups', 'altpool', 'fixeds', 'preds', 'cmds', 'lutpool', 'setpool'])
data = bytes.fromhex(instruction)
words = list(struct.unpack('<' + 'H' * (len(data) // 2), data)) + [0] * 16


def walk(index):
    fo, fn, po, pn, co, cn, go, gn = nodes[index]
    if any(words[word] & mask != value for word, mask, value in fixeds[fo:fo + fn]):
        return None
    selected = [index]
    for off, count in groups[go:go + gn]:
        matches = [result for child in alternatives[off:off + count]
                   if (result := walk(child)) is not None]
        if not matches:
            return None
        selected += matches[0]
    return selected


for name, ep, size, root in re.findall(r'\{"([^"]+)",(\d+),(\d+),(\d+)\}', source):
    if name != mnemonic or int(size) != len(data):
        continue
    selected = walk(int(root))
    if selected is None:
        continue
    values, candidates = {}, {}
    for index in selected:
        fo, fn, po, pn, co, cn, go, gn = nodes[index]
        for field, kind, shift, width, a, b in predicates[po:po + pn]:
            if kind == 0:
                values[field] = a
            elif kind == 3:
                values[field] = values.get(field, 0) | (a << shift)
            elif kind == 1:
                allowed = set(sets[a:a + b])
                candidates[field] = candidates.get(field, allowed) & allowed
            elif kind == 2 and b - a < 256:
                allowed = set(range(a, b + 1))
                candidates[field] = candidates.get(field, allowed) & allowed
        for field, word, dst_shift, width, src_shift, off, count in commands[co:co + cn]:
            value = (words[word] >> dst_shift) & ((1 << width) - 1)
            if off < 0:
                values[field] = values.get(field, 0) | (value << src_shift)
            else:
                matches = [j for j in range(count)
                           if (lut[off + j] >> src_shift) & ((1 << width) - 1) == value]
                allowed = set(matches)
                candidates[field] = candidates.get(field, allowed) & allowed
    ambiguous = []
    for field, allowed in sorted(candidates.items()):
        if len(allowed) == 1:
            values[field] = next(iter(allowed))
        elif field not in values:
            ambiguous.append((field, sorted(allowed)))
    print(mnemonic, 'ep=' + ep, 'nonzero fields:',
          dict(sorted((k, v) for k, v in values.items() if v)))
    print('ambiguous lookups:', ambiguous)
