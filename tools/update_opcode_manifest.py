#!/usr/bin/env python3
"""Regenerate opcode counts in the documented manifest from spec.rs."""
from collections import Counter
from pathlib import Path
import re

root = Path(__file__).resolve().parent.parent
spec = (root / 'crates/eitri-bytecode/src/spec.rs').read_text()
counts = Counter(re.findall(r'^\s+\w+ = "[^"]+", "([^"]+)",', spec, re.M))
path = root / 'docs/implementation/bytecode.md'
text = path.read_text()
head, rest = text.split('### Opcode manifest', 1)
table_start = rest.index('| Family | Opcodes | Variants |')
table_end = rest.index('\n\n', table_start)
lines = rest[table_start:table_end].splitlines()
seen = set()
result = []
for line in lines:
    if line.startswith('| **Total**'):
        for family in sorted(counts.keys() - seen):
            result.append(f'| `{family}` | {counts[family]} | managed string allocation and byte length |')
        total = sum(counts.values())
        result.append(f'| **Total** | **{total}** | {256-total} left for maps, optionals and enums, closures, `dyn`, tasks, and quickened forms |')
    elif line.startswith('| `'):
        cells = line.split(' | ')
        families = re.findall(r'`([^`]+)`', cells[0])
        seen.update(families)
        cells[1] = str(sum(counts[f] for f in families))
        result.append(' | '.join(cells))
    else:
        result.append(line)
path.write_text(head + '### Opcode manifest' + rest[:table_start] + '\n'.join(result) + rest[table_end:])
