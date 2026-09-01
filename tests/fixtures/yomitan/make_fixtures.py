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

BANKS_INDEX = {"title": "FixtureBanks", "format": 3, "revision": "1"}

# Twelve term banks, so the builder's bank pool actually runs more than one
# thread and has to put the results back in archive order (`entry_id` is
# assigned as banks arrive, so a rebuild that numbered by whichever thread
# finished first would write a different database every time).
#
# Bank 3 is two orders of magnitude larger than its neighbours on purpose:
# with even banks the threads finish roughly in order and the reordering is
# never exercised. One slow bank guarantees that eleven banks complete while
# it is still running, which is exactly the case that must not reorder the
# database.
#
# The headwords carry their own bank and row number, so a test can assert
# the whole sequence rather than a count. Ten, eleven and twelve are there
# for the numeric bank sort: `term_bank_10` follows `term_bank_9`.
BANK_COUNT = 12
SLOW_BANK = 3


def bank_rows(bank):
    rows = 400 if bank == SLOW_BANK else 4
    return [
        [f"b{bank:02}w{row:03}", f"b{bank:02}r{row:03}", "", "", 0,
         [f"bank {bank} row {row}"]]
        for row in range(rows)
    ]


BANKS = {
    f"term_bank_{bank}.json": bank_rows(bank)
    for bank in range(1, BANK_COUNT + 1)
}

PITCH_INDEX = {"title": "FixturePitch", "format": 3, "revision": "pitch_1.0.0.1"}

# Pitch-only: `term_meta_bank_` and no `term_bank_`, which is what all five
# pitch archives in ticket 01's census are. Every row here is a real one from
# docs/research/pitch-accent-shapes.md, re-serialised, so the parser is tested
# against payloads a dictionary actually shipped:
#
#   猫 / ねこ      two rows for one expression and reading, which must merge
#   食べる / たべる heiban, 48.0% of the corpus and the value drawn most
#   合鍵 / あいかぎ NHK's nasal marker on a heiban accent
#   アーク灯       NHK's devoice marker, on a mora index that is not a
#                  character index
#   一体 / いったい one row listing `position: 0` twice, so the dedup inside a
#                  row is exercised
#   〜あつかい     a reading no term dictionary emits, one of the census's 122
#
# The key order varies row to row exactly as it does in the real banks, which
# is what pins that no parser depends on it.
PITCH_BANK = [
    ["猫", "pitch", {"reading": "ねこ", "pitches": [{"position": 1, "devoice": [], "nasal": []}]}],
    ["猫", "pitch", {"pitches": [{"devoice": [], "nasal": [], "position": 0}], "reading": "ねこ"}],
    ["食べる", "pitch", {"pitches": [{"nasal": [], "devoice": [], "position": 2}], "reading": "たべる"}],
    ["合鍵", "pitch", {"reading": "あいかぎ", "pitches": [{"devoice": [], "position": 0, "nasal": [4]}]}],
    ["アーク灯", "pitch", {"reading": "アークとう", "pitches": [{"nasal": [], "devoice": [3], "position": 0}]}],
    ["一体", "pitch", {"reading": "いったい", "pitches": [{"position": 0, "devoice": [], "nasal": []}, {"nasal": [], "position": 1, "devoice": []}, {"nasal": [], "position": 0, "devoice": []}]}],
    ["扱い", "pitch", {"pitches": [{"devoice": [], "nasal": [], "position": 2}], "reading": "〜あつかい"}],
]

PITCH2_INDEX = {"title": "FixturePitchTwo", "format": 3, "revision": "pitch_1.0.0.1"}

# A second pitch dictionary, for the dedup: it agrees with the first about 猫
# / ねこ's atamadaka and disagrees about 食べる / たべる. So a card for 猫
# draws one row naming both dictionaries and a card for 食べる draws two rows.
PITCH2_BANK = [
    ["猫", "pitch", {"reading": "ねこ", "pitches": [{"position": 1, "devoice": [], "nasal": []}]}],
    ["食べる", "pitch", {"reading": "たべる", "pitches": [{"position": 0, "devoice": [], "nasal": []}]}],
]

BOTH_INDEX = {"title": "FixtureBoth", "format": 3, "revision": "1"}

# Both roles in one archive, which the census has no specimen of: 9 of its 14
# bank-carrying archives are frequency-only and 5 are pitch-only, none both.
# So this archive is what proves that terms and pitch do not suppress each
# other - it contributes an `entry` row *and* a `pitch` row under one
# dictionary.
BOTH_TERM_BANK = [
    ["犬", "いぬ", "", "", 0, ["dog"]],
]

BOTH_PITCH_BANK = [
    ["犬", "pitch", {"reading": "いぬ", "pitches": [{"position": 2, "devoice": [], "nasal": []}]}],
]

BADCRC_INDEX = {"title": "FixtureBadCrc", "format": 3, "revision": "pitch_1.0.0.1"}

# The blocker ticket 01 measured: all five pitch archives in that library
# store CRC-32 values that do not match their own payload, 48 of 54 members,
# and chibipop's zip reader refused every one of them while Yomitan imported
# them cleanly. `write_bad_crc` below corrupts the stored checksums of this
# archive's banks and leaves the payload alone, exactly the way those archives
# are wrong, so the tolerance is tested against the shape rather than asserted.
BADCRC_BANK = [
    ["鍵", "pitch", {"reading": "かぎ", "pitches": [{"position": 2, "devoice": [], "nasal": []}]}],
]


def write_bad_crc(path, index, banks):
    """Writes an archive and then breaks every bank member's stored CRC-32.

    Both headers carry the checksum - the local one and the central
    directory's - and the real archives are wrong in both, so both are
    flipped here. `index.json` is left correct, which is also how the real
    ones are: all five store a valid checksum for it and a wrong one for
    every bank.
    """
    write(path, index, banks)
    raw = bytearray(path.read_bytes())
    with zipfile.ZipFile(path) as z:
        wrong = {
            info.filename: (info.CRC, info.CRC ^ 0xFFFFFFFF)
            for info in z.infolist()
            if info.filename != "index.json"
        }
    for correct, broken in wrong.values():
        found = raw.replace(
            correct.to_bytes(4, "little"), broken.to_bytes(4, "little")
        )
        assert found != raw, "the stored checksum was not where it was expected"
        raw = found
    path.write_bytes(raw)
    with zipfile.ZipFile(path) as z:
        assert z.testzip() is not None, "the archive must fail its own CRC check"


def write(path, index, banks):
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("index.json", json.dumps(index, ensure_ascii=False))
        for name, payload in banks.items():
            z.writestr(name, json.dumps(payload, ensure_ascii=False))


if __name__ == "__main__":
    write(HERE / "terms.zip", TERMS_INDEX, {"term_bank_1.json": TERM_BANK})
    write(HERE / "freq.zip", FREQ_INDEX, {"term_meta_bank_1.json": FREQ_BANK})
    write(HERE / "sweep.zip", SWEEP_INDEX, {"term_bank_1.json": SWEEP_BANK})
    write(HERE / "banks.zip", BANKS_INDEX, BANKS)
    write(HERE / "pitch.zip", PITCH_INDEX, {"term_meta_bank_1.json": PITCH_BANK})
    write(HERE / "pitch2.zip", PITCH2_INDEX, {"term_meta_bank_1.json": PITCH2_BANK})
    write(HERE / "both.zip", BOTH_INDEX, {
        "term_bank_1.json": BOTH_TERM_BANK,
        "term_meta_bank_1.json": BOTH_PITCH_BANK,
    })
    write_bad_crc(HERE / "badcrc.zip", BADCRC_INDEX,
                  {"term_meta_bank_1.json": BADCRC_BANK})
    for name in ("terms.zip", "freq.zip", "sweep.zip", "banks.zip", "pitch.zip",
                 "pitch2.zip", "both.zip", "badcrc.zip"):
        print(f"wrote {HERE / name}")
