"""Reader for Yomitan format-3 dictionary archives.

Uses Python's zipfile deliberately: .NET Framework's ZipArchive silently
reports zero entries for some of these archives.
"""

import json
import re
import zipfile
from pathlib import Path
from typing import Iterator, NamedTuple

_NUM = re.compile(r"(\d+)")


class TermEntry(NamedTuple):
    term: str
    reading: str
    definition_tags: str
    rules: str
    score: int
    glossary: list
    sequence: "int | None"


def _sorted_banks(names, prefix):
    """Bank files sorted numerically, so bank_10 follows bank_9, not bank_1."""
    picked = [n for n in names if n.startswith(prefix) and n.endswith(".json")]

    def key(n):
        m = _NUM.search(n[len(prefix):])
        return int(m.group(1)) if m else 0

    return sorted(picked, key=key)


def read_index(zip_path: Path) -> dict:
    with zipfile.ZipFile(zip_path) as z:
        return json.loads(z.read("index.json").decode("utf-8"))


def iter_terms(zip_path: Path) -> Iterator[TermEntry]:
    with zipfile.ZipFile(zip_path) as z:
        for bank in _sorted_banks(z.namelist(), "term_bank_"):
            for row in json.loads(z.read(bank).decode("utf-8")):
                yield TermEntry(
                    term=row[0],
                    reading=row[1] if len(row) > 1 else "",
                    definition_tags=row[2] if len(row) > 2 else "",
                    rules=row[3] if len(row) > 3 else "",
                    score=row[4] if len(row) > 4 else 0,
                    glossary=row[5] if len(row) > 5 else [],
                    sequence=row[6] if len(row) > 6 else None,
                )


def iter_freq_rows(zip_path: Path) -> Iterator[list]:
    with zipfile.ZipFile(zip_path) as z:
        for bank in _sorted_banks(z.namelist(), "term_meta_bank_"):
            for row in json.loads(z.read(bank).decode("utf-8")):
                yield row
