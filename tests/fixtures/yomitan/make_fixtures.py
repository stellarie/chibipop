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

SWEEP_INDEX = {"title": "FixtureSweep", "format": 3, "revision": "1"}

# The corpus sweep's own archive: rows that make the *no dropped text*
# invariant fire, so the suppression list can be proven end to end without a
# corpus. All three carry a `<ruby>`, and the renderer's rule for one is that
# a reading needs a base to hang over (src/ui/layout/ruby.rs): "Nothing is
# appended for a slot no base text reached". So the first row's reading is
# drawn and the last two rows' readings are not, which is one clean row and
# two distinct violating shapes - the second plain, the third carrying a
# `data` hook a stylesheet could key on, so the two do not collapse into one
# candidate.
#
# If the renderer ever learns to draw a base-less reading, the sweep's
# suppression smoke test fails and this bank needs a new violating shape.
# That is the intended failure: the test would otherwise pass while proving
# nothing.
# Every reading here is deliberately absent from its row's headword, its own
# reading field, and its span text. A candidate is a string the scene never
# drew, and the panel draws the headword's reading too - so an `rt` that
# repeated it would be "found" in the card's own header and the row would
# report clean for the wrong reason.
SWEEP_BANK = [
    ["漢字", "かんじ", "", "", 0,
     [{"type": "structured-content", "content": {"tag": "div", "content": [
         {"tag": "span", "content": "a reading over its own base"},
         {"tag": "ruby", "content": [
             {"tag": "rb", "content": "漢"},
             {"tag": "rt", "content": "から"},
         ]},
     ]}}]],
    ["宙", "ちゅう", "", "", 0,
     [{"type": "structured-content", "content": {"tag": "div", "content": [
         {"tag": "span", "content": "a reading with no base"},
         {"tag": "ruby", "content": [{"tag": "rt", "content": "そら"}]},
     ]}}]],
    ["注", "ちゅう", "", "", 0,
     [{"type": "structured-content", "content": {"tag": "div", "content": [
         {"tag": "span", "content": "a base-less reading a stylesheet keys on"},
         {"tag": "ruby", "data": {"content": "note"},
          "content": [{"tag": "rt", "content": "そそ"}]},
     ]}}]],
]


def write(path, index, banks):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("index.json", json.dumps(index, ensure_ascii=False))
        for name, payload in banks.items():
            z.writestr(name, json.dumps(payload, ensure_ascii=False))


if __name__ == "__main__":
    write(HERE / "terms.zip", TERMS_INDEX, {"term_bank_1.json": TERM_BANK})
    write(HERE / "freq.zip", FREQ_INDEX, {"term_meta_bank_1.json": FREQ_BANK})
    write(HERE / "sweep.zip", SWEEP_INDEX, {"term_bank_1.json": SWEEP_BANK})
    for name in ("terms.zip", "freq.zip", "sweep.zip"):
        print(f"wrote {HERE / name}")
