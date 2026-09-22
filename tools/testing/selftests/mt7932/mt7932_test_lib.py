# SPDX-License-Identifier: GPL-2.0-only
"""Adapt linkage only when embedding real callbacks in a host mock executable."""
import re

def local_linkage(source):
    # Drop local forward declarations so extraction starts at definitions.
    source = re.sub(r'^static (?:inline )?[^;{}\n]+\bmt_\w+\([^;{}]*?\);\n',
                    '', source, flags=re.M)
    # Production callbacks cross translation units; host mocks remain local.
    return re.sub(r'^((?:void|bool|int|u32|netdev_tx_t|struct [^\n]+?)\s+\*?mt_\w+\([^;{}]*?\))\n\{',
                  r'static \1\n{', source, flags=re.M)
