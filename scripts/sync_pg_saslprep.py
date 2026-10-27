#!/usr/bin/env python3
"""Embed Unicode 3.2 SASLprep tables for the dependency-free Windows probe."""
from pathlib import Path
import stringprep


def ranges(names):
    checks = [getattr(stringprep, 'in_table_' + name) for name in names]
    result = []
    start = end = None
    for point in range(0x110000):
        if any(check(chr(point)) for check in checks):
            if start is None:
                start = point
            end = point
        elif start is not None:
            result.append((start, end))
            start = None
    if start is not None:
        result.append((start, end))
    return ','.join('%x-%x' % pair for pair in result)


path = Path(__file__).resolve().parents[1] / 'crates/host/src/postgresql_probe.ps1'
source = path.read_text(encoding='utf-8')
begin = ' // BEGIN Unicode 3.2 tables (scripts/sync_pg_saslprep.py)\n'
end = ' // END Unicode 3.2 tables\n'
tables = [('MappedAway', ['b1']), ('Spaces', ['c12']), ('Unassigned', ['a1']),
          ('Prohibited', ['a1', 'c12', 'c21', 'c22', 'c3', 'c4', 'c5', 'c6', 'c7', 'c8', 'c9']),
          ('RandAL', ['d1']), ('LeftToRight', ['d2'])]
content = ''.join(' static readonly int[][] %s = Ranges("%s");\n' % (name, ranges(names))
                  for name, names in tables)
path.write_text(source.split(begin)[0] + begin + content + end + source.split(end)[1], encoding='utf-8')
