"""Regenerate the checked-in Yomitan fixture archives.

Deliberately tiny and deliberately committed: Phase 2's Rust builder is
verified by producing the same database from these exact bytes that the
Python builder does. Run from the repository root:

    python tests/fixtures/yomitan/make_fixtures.py
"""

import json
import zipfile
from pathlib import Path

HERE = Path(__file__).parent

TERMS_INDEX = {"title": "FixtureTerms", "format": 3, "revision": "1"}

# Covers the shapes that matter: structured content carrying part-of-speech
# spans, a plain string glossary, a kana-only headword, and a kanji spelling
# sharing its reading with that headword - which is what makes the
# reading-scoped frequency row below meaningful rather than decorative.
TERM_BANK = [
    ["食べる", "たべる", "", "v1", 0,
     [{"type": "structured-content", "content": [
         {"tag": "span", "data": {"content": "part-of-speech-info"},
          "content": "1-dan"},
         {"tag": "span", "data": {"content": "part-of-speech-info"},
          "content": "transitive"},
         {"tag": "span", "content": "to eat"},
     ]}]],
    ["ねこ", "ねこ", "", "", 0, ["cat"]],
    ["猫", "ねこ", "", "", 0, ["cat (kanji)"]],
]

FREQ_INDEX = {"title": "FixtureFreq", "format": 3, "frequencyMode": "rank-based"}

# Both row shapes, plus a competing pair: 猫 has a reading-scoped rank of 42
# and a reading-agnostic one of 9999. lookup_freq must prefer the first, so a
# reader that ignores the reading dimension fails rather than passing quietly.
FREQ_BANK = [
    ["食べる", "freq", {"value": 7}],
    ["猫", "freq", {"reading": "ねこ", "frequency": {"value": 42}}],
    ["猫", "freq", {"value": 9999}],
]


def write(path, index, banks):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("index.json", json.dumps(index, ensure_ascii=False))
        for name, payload in banks.items():
            z.writestr(name, json.dumps(payload, ensure_ascii=False))


if __name__ == "__main__":
    write(HERE / "terms.zip", TERMS_INDEX, {"term_bank_1.json": TERM_BANK})
    write(HERE / "freq.zip", FREQ_INDEX, {"term_meta_bank_1.json": FREQ_BANK})
    print(f"wrote {HERE / 'terms.zip'} and {HERE / 'freq.zip'}")
