"""Parse Yomitan rank-based frequency banks.

Two row shapes occur in the same file:
    ["の", "freq", {"value": 1}]                                  reading-agnostic
    ["乃", "freq", {"reading": "の", "frequency": {"value": 1}}]   reading-scoped
The second nests `value` one level deeper. Missing that makes a rare kanji
spelling inherit its common homophone's rank.
"""


def _extract(payload):
    """Return (reading_or_None, rank_or_None) from a freq row's payload."""
    if isinstance(payload, int):
        return None, payload
    if not isinstance(payload, dict):
        return None, None

    reading = payload.get("reading")

    inner = payload.get("frequency")
    if isinstance(inner, int):
        return reading, inner
    if isinstance(inner, dict):
        v = inner.get("value")
        return reading, v if isinstance(v, int) else None

    v = payload.get("value")
    return reading, v if isinstance(v, int) else None


def parse_freq_rows(rows):
    """Build {(term, reading_or_None): rank}. Lower rank = more common."""
    table = {}
    for row in rows:
        if len(row) < 3 or row[1] != "freq":
            continue
        reading, rank = _extract(row[2])
        if rank is None:
            continue
        key = (row[0], reading)
        prev = table.get(key)
        if prev is None or rank < prev:
            table[key] = rank
    return table


def lookup_freq(table, term, reading):
    """Reading-specific rank if present, else the reading-agnostic one."""
    if reading:
        hit = table.get((term, reading))
        if hit is not None:
            return hit
    return table.get((term, None))
