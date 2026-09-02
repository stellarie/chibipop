# What a Yomitan pitch row actually contains

Two halves, and the difference between them is the point.

**The normative schema**, read from Yomitan's own type definitions and JSON
schema at revision
[`77e2004`](https://github.com/yomidevs/yomitan/commit/77e200428902abf4fa48284df92da7af3dcb4162)
(`yomidevs/yomitan`, `master`, 2026-08-18), not from prose about them. This says
what a pitch payload is *permitted* to contain.

**A census of the five pitch archives in this machine's chibipop library**
(`~/.local/share/chibipop/library`, 99 archives), taken 2026-09-01 with
[tools/pitch-census](../../tools/pitch-census/README.md). Every
`term_meta_bank_*.json` row of every archive read, no sampling. This says what
is *used*.

This doc exists because the pitch role needs a storage shape and a parser, and
today the whole of what this repo knows about pitch is
[dict-shapes.md:347-353](dict-shapes.md): it exists, it is numeric mora data in
`term_meta_bank`, and chibipop throws it away.

## Headline

- **The payload is small and shallow.** Two required keys, a list of accents,
  one required downstep per accent and three optional fields. Across 511 488
  accents the corpus uses two of those three optional fields at all, both only
  in one dictionary, and never the third.
- **chibipop cannot currently read a single one of these archives.** All five
  pitch archives store wrong CRC-32 values - 48 of their 54 members fail the
  check - and chibipop's zip reader refuses every one of them with
  `Invalid checksum`. Yomitan does not check, which is why they are installed
  and working there. **This blocks the pitch-role read** and is measured, not
  inferred - see [The blocker](#the-blocker-a-wrong-crc-32-on-48-of-54-members).
- **Nasal and devoice markers: 2.0% and 2.6% of accents, all of them in one
  dictionary.** 10 358 of 511 488 accents carry a non-empty `nasal`, 13 211 a
  non-empty `devoice`, and every one of them comes from NHK. Corpus-wide that is
  rare; *within NHK* it is 25.8% of rows. The deferral of mark rendering
  survives the first number and should be read against the second.
- **Deduplication is the main event, not a nice-to-have.** Over the 101 279
  readings that two or more pitch dictionaries both know, 379 288 accent claims
  arrive and 123 140 survive deduplication: **67.5% collapse**. 81.4% of those
  readings get an identical accent set from every dictionary that has them.
- **The card header needs at most four rows.** Union of every accent every
  dictionary gives, over 218 783 expression+reading pairs: 187 509 have one
  accent, 29 454 have two, 1 770 have three, 50 have four. Never five.
- **The filename lies once.** Six archives are named `[Pitch]`; one of them
  carries term banks, no `term_meta_bank_` at all, and writes its accents as
  plain text inside the glossary. dict-shapes.md:349's "six ... are pitch
  dictionaries" counts filenames; the bank-content predicate the spec uses gives
  **five**.
- **Everything the corpus does not use.** No `tags`, ever. No `HL`-string
  `position`, ever. No scalar `nasal`/`devoice`, ever - always a list. No `ipa`
  row, no unknown mode, no unknown field, no empty `pitches` list, in 196
  archive reads across two corpora.

---

# Part 1 - the normative schema

Two files carry it, and they agree:

- `types/ext/dictionary-data.d.ts` - the TypeScript types.
- `ext/data/schemas/dictionary-term-meta-bank-v3-schema.json` - the JSON schema
  the importer actually validates against.

Yomitan's prose documentation describes **no field of it**. The whole of
`docs/making-yomitan-dictionaries.md` on the subject is one table cell:
"Stores meta information about terms, such as frequency data and pitch accent
data." So the answer to "which fields does the schema permit that the docs do
not mention" is: all of them. The schema and the types are the documentation.

Every block below is a verbatim slice of the file it names, at the revision
pinned above. Two allowances, and nothing else: the JSON-schema fragments are
de-indented, because they sit up to twenty spaces deep in the file, and a slice
that starts inside a function or a class keeps its original indentation so the
nesting is visible.

## The whole term-meta type surface, in one contiguous run

`types/ext/dictionary-data.d.ts`, uninterrupted - nothing is elided between the
first line and the last:

```ts
export type GenericFrequencyData = string | number | {
    value: number;
    displayValue?: string;
    reading?: undefined; // Used for type disambiguation, field does not actually exist
};

export type TermMetaArray = TermMeta[];

export type TermMeta = TermMetaFrequency | TermMetaPitch | TermMetaPhonetic;

export type TermMetaFrequencyDataWithReading = {
    reading: string;
    frequency: GenericFrequencyData;
};

export type TermMetaFrequency = [
    expression: string,
    mode: 'freq',
    data: GenericFrequencyData | TermMetaFrequencyDataWithReading,
];

export type TermMetaPitchData = {
    reading: string;
    pitches: {
        position: number | string;
        nasal?: number | number[];
        devoice?: number | number[];
        tags?: string[];
    }[];
};

export type TermMetaPitch = [
    expression: string,
    mode: 'pitch',
    data: TermMetaPitchData,
];

export type TermMetaPhonetic = [
    expression: string,
    mode: 'ipa',
    data: TermMetaPhoneticData,
];

export type TermMetaPhoneticData = {
    reading: string;
    transcriptions: {
        ipa: string;
        tags?: string[];
    }[];
};
```

That is the entire term-meta vocabulary: three row types, one payload type each,
and one shared frequency-data type. Nothing else in the file touches
`term_meta_bank`.

## The row

A `term_meta_bank_${number}.json` file is a flat array of three-element arrays,
`minItems: 3, maxItems: 3, additionalItems: false`. Field 1 is a closed enum:

```json
{
    "type": "string",
    "enum": ["freq", "pitch", "ipa"],
    "description": "Type of data. \"freq\" corresponds to frequency information; \"pitch\" corresponds to pitch information. \"ipa\" corresponds to IPA transcription."
},
```

## How a pitch row differs from the `"freq"` row chibipop already parses

`merge_freq_row` (`src/dict/frequency.rs:18-35`) takes `row[1] == "freq"` and
hands `row[2]` to `extract_reading_and_rank` (`:48-61`), which accepts a bare
integer, an object with `reading` + `frequency`, or an object with `value`. Four
differences matter for the pitch parser:

| | `"freq"` payload | `"pitch"` payload |
|---|---|---|
| shape | string, number, **or** object | always an object |
| `reading` | **optional**; absent means "any reading", which `lookup_freq` (`src/dict/frequency.rs:38-45`) spends as a `None` key | **required** |
| cardinality | one scalar per row | a **list** of accents per row |
| nesting | one level | two - payload, then one object per accent |

So the pitch parser cannot reuse the freq path's tolerance. `extract_reading_and_rank`'s
"try three shapes" is exactly what a pitch payload does not need, and the
`Option<String>` reading key `FreqTable` (`src/dict/frequency.rs:6`) is built on
has no pitch analogue: a pitch row without a reading is not schema-legal.

## The payload

`TermMetaPitchData` in the run above is the whole of what the types say. The
JSON schema adds the constraints the types cannot express - required keys, the
integer's lower bound, the string's pattern, and `additionalProperties: false`:

```json
{
    "type": "object",
    "description": "Pitch accent information for the term.",
    "required": [
        "reading",
        "pitches"
    ],
    "additionalProperties": false,
    "properties": {
        "reading": {
            "type": "string",
            "description": "Reading for the term."
        },
        "pitches": {
            "type": "array",
            "description": "List of different pitch accent information for the term and reading combination.",
            "items": {
                "type": "object",
                "required": [
                    "position"
                ],
                "additionalProperties": false,
                "properties": {
                    "position": {
                        "oneOf": [
                            {
                                "type": "integer",
                                "description": "Mora position of the pitch accent downstep. A value of 0 indicates that the word does not have a downstep (heiban).",
                                "minimum": 0
                            },
                            {
                                "type": "string",
                                "description": "Pitch level of each mora with H representing high and L representing low. For example: HHLL for a 4 mora word. Add an additional pitch level at the end to explicitly define the suffix.",
                                "pattern": "^[HL]+$"
                            }
                        ]
                    },
                    "nasal": {
                        "oneOf": [
                            {
                                "type": "integer",
                                "description": "Position of a mora with nasal sound.",
                                "minimum": 0
                            },
                            {
                                "type": "array",
                                "description": "Positions of morae with nasal sound.",
                                "items": {
                                    "type": "integer",
                                    "minimum": 0
                                }
                            }
                        ]
                    },
                    "devoice": {
                        "oneOf": [
                            {
                                "type": "integer",
                                "description": "Position of a mora with devoiced sound.",
                                "minimum": 0
                            },
                            {
                                "type": "array",
                                "description": "Positions of morae with devoiced sound.",
                                "items": {
                                    "type": "integer",
                                    "minimum": 0
                                }
                            }
                        ]
                    },
                    "tags": {
                        "type": "array",
                        "description": "List of tags for this pitch accent.",
                        "items": {
                            "type": "string",
                            "description": "Tag for this pitch accent. This typically corresponds to a certain type of part of speech."
                        }
                    }
                }
            }
        }
    }
}
```

## Every field

| field | type | required | default when absent | unit |
|---|---|---|---|---|
| `row[0]` | string | required | - | the headword text |
| `row[1]` | `"pitch"` | required | - | - |
| `row[2]` | object | required | - | - |
| `row[2].reading` | string | **required** | - | - |
| `row[2].pitches` | array of object | **required** | - | no `minItems`, so `[]` is legal |
| `pitches[].position` | integer `>= 0` **or** string `^[HL]+$` | **required** | - | integer: **1-based** mora index of the last high mora, `0` = heiban. string: **0-based** per-mora level |
| `pitches[].nasal` | integer `>= 0` **or** array of integer `>= 0` | optional | `[]` | **1-based** mora index |
| `pitches[].devoice` | integer `>= 0` **or** array of integer `>= 0` | optional | `[]` | **1-based** mora index |
| `pitches[].tags` | array of string | optional | `[]` | - |

`additionalProperties: false` on the payload **and** on each accent, and the
importer enforces it: `_validateFile` runs the compiled ajv schema named by
`_getDataBankSchemas` (`dictionaryTermMetaBankV3`) over every
`term_meta_bank_*.json` and throws `Dictionary has invalid data in '...'` on
failure (`ext/js/dictionary/dictionary-importer.js`). An unknown key is not
tolerated by Yomitan, so finding one in a real archive means finding an archive
Yomitan itself would refuse.

### The two indexing origins are different, and this is a trap

`isMoraPitchHigh` (`ext/js/language/ja/japanese.js`) is the whole semantics of
`position`:

```js
export function isMoraPitchHigh(moraIndex, pitchAccentValue) {
    if (typeof pitchAccentValue === 'string') {
        return pitchAccentValue[moraIndex] === 'H';
    }
    switch (pitchAccentValue) {
        case 0: return (moraIndex > 0);
        case 1: return (moraIndex < 1);
        default: return (moraIndex > 0 && moraIndex < pitchAccentValue);
    }
}
```

`moraIndex` is 0-based. So for an **integer** `position` of `N >= 2`, morae at
indices `1 .. N-1` are high - that is the 2nd through Nth mora - and the fall
lands between the Nth and the (N+1)th. `N == 1` makes only mora index 0 high
(atamadaka). `N == 0` makes mora 0 low and every later mora high (heiban). The
integer is therefore a **1-based** count of morae before the downstep, exactly
as the schema's prose says.

The **string** form is indexed by `moraIndex` directly, so it is **0-based and
positional**: `position[i]` is the level of the `i`th mora. The two forms of one
field do not share an origin. `getDownstepPositions` reduces the string form
when a single number is wanted:

```js
export function getDownstepPositions(pitchString) {
    const downsteps = [];
    const moraCount = pitchString.length;
    for (let i = 0; i < moraCount; i++) {
        if (i > 0 && pitchString[i - 1] === 'H' && pitchString[i] === 'L') {
            downsteps.push(i);
        }
    }
    if (downsteps.length === 0) {
        downsteps.push(pitchString.startsWith('L') ? 0 : -1);
    }
    return downsteps;
}
```

Note it can return **several** downsteps, and `-1` for a string that neither
falls nor starts low - a value no integer `position` can hold.

### Nasal and devoice are 1-based, and always a list once normalised

`createPronunciationText` (`ext/js/display/pronunciation-generator.js`) walks
morae with a 0-based `i` and tests the marks against `i + 1`:

```js
    createPronunciationText(morae, pitchPositions, nasalPositions, devoicePositions) {
        const nasalPositionsSet = nasalPositions.length > 0 ? new Set(nasalPositions) : null;
        const devoicePositionsSet = devoicePositions.length > 0 ? new Set(devoicePositions) : null;
        const container = this._document.createElement('span');
        container.className = 'pronunciation-text';
        for (let i = 0, ii = morae.length; i < ii; ++i) {
            const i1 = i + 1;
            const mora = morae[i];
            const highPitch = isMoraPitchHigh(i, pitchPositions);
            const highPitchNext = isMoraPitchHigh(i1, pitchPositions);
            const nasal = nasalPositionsSet !== null && nasalPositionsSet.has(i1);
            const devoice = devoicePositionsSet !== null && devoicePositionsSet.has(i1);
```

The scalar-or-list ambiguity is resolved once, on the way in
(`ext/js/language/translator.js`):

```js
    /**
     * @param {number|number[]|undefined} value
     * @returns {number[]}
     */
    _toNumberArray(value) {
        return Array.isArray(value) ? value : (typeof value === 'number' ? [value] : []);
    }
```

so a scalar `3`, a list `[3]`, an empty list and an absent field are two facts,
not four: "the moras marked" and "none". The internal type keeps only the list
form (`types/ext/dictionary.d.ts`):

```ts
/**
 * Pitch accent information for a term, represented as the position of the downstep.
 */
export type PitchAccent = {
    /**
     * Type of the pronunciation, for disambiguation between union type members.
     */
    type: 'pitch-accent';
    /**
     * Position of the downstep, as a number of mora.
     */
    positions: number | string;
    /**
     * Positions of morae with a nasal sound.
     */
    nasalPositions: number[];
    /**
     * Positions of morae with a devoiced sound.
     */
    devoicePositions: number[];
    /**
     * Tags for the pitch accent.
     */
    tags: Tag[];
};
```

### What a mora index counts

`getKanaMorae` (`ext/js/language/ja/japanese.js`):

```js
/**
 * @param {string} text
 * @returns {string[]}
 */
export function getKanaMorae(text) {
    const morae = [];
    let i;
    for (const c of text) {
        if (SMALL_KANA_SET.has(c) && (i = morae.length) > 0) {
            morae[i - 1] += c;
        } else {
            morae.push(c);
        }
    }
    return morae;
}
```

and the set it consults, declared earlier in the same file:

```js
const SMALL_KANA_SET = new Set('ぁぃぅぇぉゃゅょゎァィゥェォャュョヮ');
```

A small kana joins the mora before it. Note what is *not* in the set: `ッ` and
`ー` are each a mora of their own, so `いっぽん` is four morae and `アーム` is
three. A mora is therefore one or two characters and a mora index is never a
character index.

### A reading with no accent data

Two shapes are legal and they mean different things:

- **`pitches: []`.** The field is required but has no `minItems`, so an archive
  may name a reading and give it no accent. Nothing in the corpus does this
  (0 of 466 990 rows), so the parser must accept it without a fixture to test
  it against - see [Fixtures](#fixtures).
- **No row at all.** This is how the corpus expresses it. `reading` cannot be
  omitted and there is no null, so "this reading has no accent" is said by
  silence.

There is also a third way for a payload to have no effect: the reading has to
match exactly. `Translator` skips any payload whose reading is not the
headword's (`ext/js/language/translator.js`):

```js
                    case 'pitch':
                        {
                            if (data.reading !== reading) { continue; }
```

That matters because 122 rows in this corpus carry a reading no term dictionary
will ever produce. See
[Readings that are not plain kana](#readings-that-are-not-plain-kana).

## The term-meta modes that are neither `freq` nor `pitch`

One: `"ipa"`. `TermMetaPhonetic` and `TermMetaPhoneticData` are in the run
quoted above - a required `reading` and a required `transcriptions` list of
`{ipa, tags?}`, the same two-level shape as pitch with a string where pitch has
a number.

The enum is closed - `["freq", "pitch", "ipa"]` - and ajv rejects anything else
at import, so a fourth mode cannot appear in an archive Yomitan accepted. Role
detection therefore has exactly one mode to skip deliberately, and should skip
it by name rather than by "not freq and not pitch", so a future Yomitan mode
does not silently acquire the pitch role.

The kanji banks are a separate namespace and chibipop never reads them:
`KanjiMeta = KanjiMetaFrequency` has only `freq`, in `kanji_meta_bank_*.json`,
and `for_each_freq_row` (`src/dict/archive.rs:127-129`) matches the prefix
`term_meta_bank_` only. Four `[Kanji Frequency]` archives in this library are
invisible to it for exactly that reason, and contribute 0 term-meta rows.

---

# Part 2 - the census

## What was read

99 archives in `~/.local/share/chibipop/library`, the directory
`crates/chibipop-linux/src/settings/mod.rs:70` builds as
`paths.data_dir.join("library")` and hands to `Library::load`. Every
`term_meta_bank_*.json` row of every archive, no cap: 4 303 019 term-meta rows,
of which 3 836 029 are `freq` and 466 990 are `pitch`.

**14 of 99 archives carry term-meta rows at all: 9 frequency-only, 5
pitch-only, none both.** So the role-set model has no dual-role archive to
exercise in this library, and the pitch archives carry no term banks either -
their only non-bank member is `index.json`.

| archive | title | revision | meta banks | pitch rows | CRC-bad members |
|---|---|---|---:|---:|---:|
| `[Pitch] 大辞林第四版.zip` | 大辞林第四版 | `pitch_1.0.1.1` | 16 | 152 193 | 15 |
| `[Pitch] 大辞泉.zip` | 大辞泉 | `pitch_1.0.0.1` | 9 | 88 089 | 9 |
| `[Pitch] 三省堂第八版.zip` | 三省堂国語辞典第八番 | `pitch_1.0.1.1` | 8 | 77 630 | 8 |
| `[Pitch] 新明解第八版.zip` | 新明解第八版 | `pitch_1.0.2.1` | 8 | 75 978 | 8 |
| `[Pitch] NHK2016.zip` | NHK | `pitch_1.0.1.1` | 8 | 73 100 | 8 |
| **total** | | | **49** | **466 990** | **48** |

48.7 MB of bank JSON uncompressed, 6.0 MB compressed.

**The sixth `[Pitch]`-named archive is not a pitch dictionary.**
`[Pitch] NHK日本語発音アクセント新辞典.zip` has eight `term_bank_*.json` files, no
`term_meta_bank_` at all, and writes its accent as text in the glossary:

```json
["帯広", "おびひろ", "名詞 地名", "", 0, ["おびひろ【帯広】（北海道）\n ・［0］オビヒロ"], 0, ""]
```

This is the strongest available argument for the spec's decision that a role is
detected from banks rather than guessed from a name, and it also corrects
dict-shapes.md:349: six archives are *named* `[Pitch]`, five *have* the pitch
role.

**All five come from one publisher.** Every `index.json` carries
`"author": "コツ", "url": "https://kotu.io"` and the same description string.
That is the single largest caveat on this census: the field-shape findings below
(always a list, never a tag, always an integer `position`) describe **one
generator's output**, five times, not five independent implementations. The
*content* findings - how often dictionaries disagree, how many accents a reading
gets - are genuinely five sources, because the underlying dictionaries are
different books.

## The blocker: a wrong CRC-32 on 48 of 54 members

Every pitch archive in this library stores CRC-32 values that do not match its
own payload. Only `index.json` (all five) and `大辞林第四版`'s
`term_meta_bank_16.json` check out.

The data is not corrupt. Each member inflates to exactly the length its header
declares and parses as valid JSON; only the checksum is wrong. Measured on
`[Pitch] 新明解第八版.zip`'s `term_meta_bank_1.json`: inflated 1 055 367 bytes
against a declared 1 055 367, actual CRC `5a01dcdb`, stored CRC `4289039a` in
both the local and the central header.

**chibipop refuses them today.** Measured with a throwaway probe - not committed
anywhere, quoted here in full so the result is re-derivable - built against
`zip` 8.6, the version `Cargo.toml:63` pins, using the same call shape as
`read_entry` (`src/dict/archive.rs:318-329`): `by_name`, then
`take(limit).read_to_end`. Its only dependency is the workspace's own:
`zip = { version = "8.6", default-features = false, features = ["deflate"] }`.

```rust
// Throwaway probe: does chibipop's zip reader accept an archive whose stored
// CRC-32 does not match its payload? Mirrors `read_entry`
// (src/dict/archive.rs:318-329): `by_name`, then `take(limit).read_to_end`.
use std::fs::File;
use std::io::Read;

const MAX_BANK: usize = 512 * 1024 * 1024;

fn main() {
    for path in std::env::args().skip(1) {
        let file = File::open(&path).expect("open");
        let mut archive = zip::ZipArchive::new(file).expect("zip");
        let names: Vec<String> = archive.file_names().map(String::from).collect();
        let mut banks: Vec<&String> = names
            .iter()
            .filter(|n| n.starts_with("term_meta_bank_") && n.ends_with(".json"))
            .collect();
        banks.sort();
        let first = match banks.first() {
            Some(n) => (*n).clone(),
            None => {
                println!("{path}: no term_meta_bank_ entry");
                continue;
            }
        };
        let index = {
            let entry = archive.by_name("index.json").expect("index.json");
            let mut buf = Vec::new();
            entry.take(MAX_BANK as u64 + 1).read_to_end(&mut buf)
        };
        println!("  index.json: {index:?}");
        let outcome = {
            let entry = archive.by_name(&first).expect("by_name");
            let mut buf = Vec::new();
            entry.take(MAX_BANK as u64 + 1).read_to_end(&mut buf)
        };
        match outcome {
            Ok(n) => println!("{path}: {first} OK, {n} bytes"),
            Err(e) => println!("{path}: {first} ERROR {e}"),
        }
    }
}
```

`cargo run --release -- ~/.local/share/chibipop/library/*Pitch*.zip`, with the
directory prefix elided:

```
  index.json: Ok(272)
.../library/[Pitch] NHK2016.zip: term_meta_bank_1.json ERROR Invalid checksum
.../library/[Pitch] NHK日本語発音アクセント新辞典.zip: no term_meta_bank_ entry
  index.json: Ok(299)
.../library/[Pitch] 三省堂第八版.zip: term_meta_bank_1.json ERROR Invalid checksum
  index.json: Ok(287)
.../library/[Pitch] 大辞林第四版.zip: term_meta_bank_1.json ERROR Invalid checksum
  index.json: Ok(278)
.../library/[Pitch] 大辞泉.zip: term_meta_bank_1.json ERROR Invalid checksum
  index.json: Ok(287)
.../library/[Pitch] 新明解第八版.zip: term_meta_bank_1.json ERROR Invalid checksum
```

Each `index.json: Ok(n)` belongs to the archive named on the line *after* it.

`index.json` reads, so `read_index` succeeds and `kind_of`
(`src/library.rs:27-36`) calls the archive `Kind::Term` rather than
`Kind::Unreadable` - the archive is listed, looks fine, and contributes nothing.
The moment the pitch role reads `term_meta_bank_`, that read fails.

**Yomitan does not check.** `dictionary-importer.js` never passes
`checkSignature` or `checkCrc32` to zip.js (zero occurrences), and zip.js
documents both as `@defaultValue false`. So the same bytes import cleanly there,
which is why a user has five working pitch dictionaries that chibipop cannot
open.

*This is the implementation's problem to solve, not this doc's.* It is recorded
here because it is the difference between "parse the payload" and "parse the
payload and get zero rows", and because the census would have measured nothing
without working around it: `tools/pitch-census` bypasses the check in
`inflate_member` and records every member it bypassed, so no number below rests
on unverified bytes without saying so.

## Rows, expressions and accents

| dictionary | rows | expressions | readings | expr+reading pairs | pairs over 2+ rows | rows with 2+ accents | accents | max accents/row |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 大辞林第四版 | 152 193 | 142 302 | 99 185 | 151 396 | 720 | 16 156 | 168 563 | 3 |
| 大辞泉 | 88 089 | 82 621 | 47 658 | 86 483 | 1 208 | 7 324 | 95 593 | 3 |
| 三省堂国語辞典第八番 | 77 630 | 74 724 | 59 495 | 76 951 | 645 | 0 | 77 630 | 1 |
| 新明解第八版 | 75 978 | 72 379 | 48 047 | 75 769 | 198 | 9 072 | 85 460 | 3 |
| NHK | 73 100 | 71 236 | 61 815 | 72 177 | 843 | 10 671 | 84 242 | 4 |
| **total** | **466 990** | **443 262** | **316 200** | **462 776** | **3 614** | **43 223** | **511 488** | **4** |

1.095 accents per row on average. **43 223 rows (9.3%) carry more than one
accent for one reading**, and 三省堂 carries none at all - it emits exactly one
accent per row and expresses a second accent as a second row instead.

**3 614 expression+reading pairs are named by two or more rows in the same
dictionary.** A parser keyed on (expression, reading) must merge rather than
overwrite, or it loses accents the dictionary gave. 三省堂's `あまり` is three
rows, one accent each:

```json
["あまり", "pitch", {"reading": "あまり", "pitches": [{"nasal": [], "devoice": [], "position": 3}]}]
["あまり", "pitch", {"reading": "あまり", "pitches": [{"devoice": [], "nasal": [], "position": 0}]}]
["あまり", "pitch", {"reading": "あまり", "pitches": [{"devoice": [], "nasal": [], "position": 1}]}]
```

## Optional fields, and the nasal/devoice number

| dictionary | accents | `nasal` present | `nasal` non-empty | `devoice` present | `devoice` non-empty | `tags` | `position` forms |
|---|---:|---:|---:|---:|---:|---:|---|
| 大辞林第四版 | 168 563 | 100.0% | 0 (0.0%) | 100.0% | 0 (0.0%) | 0 | integer 168 563 |
| 大辞泉 | 95 593 | 100.0% | 0 (0.0%) | 100.0% | 0 (0.0%) | 0 | integer 95 593 |
| 三省堂国語辞典第八番 | 77 630 | 100.0% | 0 (0.0%) | 100.0% | 0 (0.0%) | 0 | integer 77 630 |
| 新明解第八版 | 85 460 | 100.0% | 0 (0.0%) | 100.0% | 0 (0.0%) | 0 | integer 85 460 |
| NHK | 84 242 | 100.0% | 10 358 (12.3%) | 100.0% | 13 211 (15.7%) | 0 | integer 84 242 |

**"Present" is not the number to read.** All five dictionaries write
`"nasal": []` and `"devoice": []` on every single accent, so presence is 100%
and says nothing. The number that matters:

- **`nasal` non-empty: 10 358 of 511 488 accents, 2.03%.**
- **`devoice` non-empty: 13 211 of 511 488 accents, 2.58%.**
- **Rows carrying either: 18 855 of 466 990, 4.04%.**
- **All of them are NHK's.** Within NHK alone: 8 790 rows nasal, 10 941 rows
  devoice, **18 855 of 73 100 rows = 25.8% carrying at least one mark.**

That is the number the mark-rendering deferral rests on, and it cuts both ways.
A user with no NHK pitch dictionary loses nothing by not rendering marks. A user
whose *only* pitch dictionary is NHK sees a quarter of their pitch rows drawn
without a mark the dictionary supplied. The spec's call - store now, render
later - is the right one precisely because of the second number: the marks must
survive the build or rendering them later becomes a second schema bump.

One accent can carry several marked morae: 10 358 accents hold 10 511 nasal mora
indices and 13 211 accents hold 13 657 devoice indices. Every observed index is
in `1 ..= 15`, consistent with the 1-based origin `createPronunciationText`
implies, and **no index is 0**, which the schema would have permitted
(`minimum: 0`).

`tags` never appears. Not once, in 511 488 accents, in either corpus.

## The downstep

| `position` | accents | share |
|---|---:|---:|
| 0 | 245 433 | 48.0% |
| 1 | 94 078 | 18.4% |
| 2 | 46 188 | 9.0% |
| 3 | 69 349 | 13.6% |
| 4 | 31 766 | 6.2% |
| 5 | 17 136 | 3.4% |
| 6 | 4 211 | 0.8% |
| 7 | 1 758 | 0.3% |
| 8 | 755 | 0.1% |
| 9-17 | 814 | 0.2% |

**Heiban is 48.0% of all accents** - the single most common value by a factor of
two and a half, and the one a renderer must get right first. Maximum observed
value is 17.

`position` is an integer in every one of the 511 488 accents. **The `^[HL]+$`
string form does not appear anywhere in this corpus.** A parser must still
accept it, because the schema permits it and Yomitan's importer would let it
through, but nothing here can test it against real data.

**Two rows put the downstep past the last mora**, both in 大辞林第四版:

```json
["平宗太", "pitch", {"reading": "ひらそうだ", "pitches": [{"position": 6, "devoice": [], "nasal": []}]}]
["築後", "pitch", {"reading": "ちくご", "pitches": [{"nasal": [], "devoice": [], "position": 3}, {"position": 5, "devoice": [], "nasal": []}]}]
```

`ひらそうだ` is 5 morae with `position: 6`; `ちくご` is 3 with `position: 5`. Both
are schema-legal (`minimum: 0`, no upper bound) and both are data errors.
`isMoraPitchHigh` handles them by accident - every mora after the first reads
high, so they render as odaka - and a renderer that indexes an array by
`position` must not assume the index is in range.

## Bounds for the card header

The reading is what gets drawn in marked kana, so its mora count bounds the row
width:

| morae | rows | share |
|---|---:|---:|
| 1 | 1 279 | 0.3% |
| 2 | 27 223 | 5.8% |
| 3 | 117 023 | 25.1% |
| 4 | 202 033 | 43.3% |
| 5 | 61 082 | 13.1% |
| 6 | 34 886 | 7.5% |
| 7 | 12 791 | 2.7% |
| 8 | 6 285 | 1.3% |
| 9-12 | 4 103 | 0.9% |
| 13-19 | 285 | 0.1% |

**Maximum 19 morae**, in four of the five dictionaries. The widest, and also the
corpus's highest `position` and a nasal marker in one row:

```json
["自動車損害賠償責任保険", "pitch", {"pitches": [{"nasal": [7], "devoice": [], "position": 17}], "reading": "じどうしゃそんがいばいしょうせきにんほけん"}]
```

68.3% of readings are 3 or 4 morae. The tail is thin but real: 285 rows are 13
morae or longer, so the layout needs a wrap or an ellipsis decision, not just a
comfortable typical case.

**Accents per header row.** Three numbers, tightening:

| bound | value |
|---|---|
| accents in one row | max 4 (one row: NHK `不義理`) |
| distinct accents per (expression, reading) within one dictionary, after merging its rows | max 4 (新明解 `白目`, NHK `長居`) |
| **distinct accents per (expression, reading) unioned over all 5 dictionaries** | **max 4** |

| accents after union over all five dictionaries | pairs |
|---|---:|
| 1 | 187 509 |
| 2 | 29 454 |
| 3 | 1 770 |
| 4 | 50 |

**Four rows is the bound, and only 50 of 218 783 pairs reach it.** 85.7% need
one row. If the nasal and devoice marks are also treated as part of an accent's
identity - which they become the moment they are drawn - the bound rises
to **7**, reached by 3 pairs.

## Cross-dictionary agreement: dedup is the main event

218 783 distinct expression+reading pairs across the five dictionaries. 117 504
are known to one dictionary only. Over the **101 279** that two or more know,
comparing accent *sets*:

| outcome | pairs | share |
|---|---:|---:|
| identical - every dictionary gives the same set | 82 403 | **81.4%** |
| partial - at least two share an accent, at least one adds another | 17 883 | 17.7% |
| disjoint - no two dictionaries share any accent | 993 | 1.0% |

And the number that answers the question directly: over those 101 279 pairs,
**379 288 accent claims arrive and 123 140 survive deduplication - 67.5%
collapse.** Two thirds of the rows a naive renderer would draw are duplicates.
Deduplication is not cosmetic; it is what makes the card header readable.

Pairwise, so no single dictionary's idiosyncrasy hides in the aggregate:

| pair | shared pairs | identical | differing | differing share |
|---|---:|---:|---:|---:|
| NHK vs 三省堂国語辞典第八番 | 42 236 | 35 998 | 6 238 | 14.8% |
| NHK vs 大辞林第四版 | 40 227 | 37 010 | 3 217 | 8.0% |
| NHK vs 大辞泉 | 38 798 | 33 354 | 5 444 | 14.0% |
| NHK vs 新明解第八版 | 36 505 | 33 582 | 2 923 | 8.0% |
| 三省堂国語辞典第八番 vs 大辞林第四版 | 55 886 | 47 842 | 8 044 | 14.4% |
| 三省堂国語辞典第八番 vs 大辞泉 | 47 841 | 42 223 | 5 618 | 11.7% |
| 三省堂国語辞典第八番 vs 新明解第八版 | 49 274 | 43 537 | 5 737 | 11.6% |
| 大辞林第四版 vs 大辞泉 | 67 338 | 57 239 | 10 099 | 15.0% |
| 大辞林第四版 vs 新明解第八版 | 60 507 | 55 163 | 5 344 | 8.8% |
| 大辞泉 vs 新明解第八版 | 51 746 | 45 128 | 6 618 | 12.8% |

Every pair lands between 8.0% and 15.0% disagreement. No dictionary is the
outlier, which means the 18.7% of readings that are not unanimous are a real
property of Japanese lexicography rather than one bad archive - the requirement
that "a disagreement between sources is visible rather than hidden" is answering
something that happens roughly one time in five.

Comparing accents including their marks instead flips one column badly:
identical falls to 73 397 and disjoint rises to 3 313, entirely because NHK is
the only dictionary with marks and so agrees with nobody once marks count. That
is the wrong comparison for the header row, and the right one to revisit once
the marks are rendered.

## Readings that are not plain kana

122 rows, 0.03%:

| dictionary | rows | characters |
|---|---:|---|
| 三省堂国語辞典第八番 | 87 | `〜` 87 |
| 新明解第八版 | 33 | `（` 35, `）` 35 |
| 大辞林第四版 | 2 | `◦` 2 |

```json
["扱い", "pitch", {"pitches": [{"devoice": [], "nasal": [], "position": 2}], "reading": "〜あつかい"}]
["或いは", "pitch", {"pitches": [{"nasal": [], "devoice": [], "position": 2}], "reading": "あるいは（ワ）"}]
["削がれる", "pitch", {"reading": "そが◦れる", "pitches": [{"devoice": [], "nasal": [], "position": 3}]}]
```

These are dead rows, not a parser problem. The reading has to match the
headword's reading exactly for the payload to apply at all, and no term
dictionary emits `〜あつかい` or `あるいは（ワ）`. They matter in two smaller
ways: mora counting over them is meaningless (`◦` counts as a mora), and any
test that asserts "every pitch row found a headword" will fail by 122.

Note that `扱い` also has a *correct* row in the same dictionary
(`"reading": "あつかい", position 0`), so the odd row adds nothing and loses
nothing.

## Payloads the schema does not describe

None. Zero unknown payload keys, zero unknown accent keys, zero non-object
payloads, zero non-list `pitches`, zero `position` values that are neither
integer nor string, across 466 990 rows and 511 488 accents in the library, and
the same over the 97-archive `~/Downloads/dict/Japanese` corpus.

This is the expected result rather than a surprise, and the reason is in Part 1:
the schema sets `additionalProperties: false` and Yomitan's importer validates
every bank against it with ajv, so an archive with an extra field could never
have been installed.

---

# Fixtures

Every payload here, up to the constructions in the last subsection, is a real
row from a named archive, re-serialised with
`json.dumps(..., ensure_ascii=False)` - content verbatim, whitespace normalised.
Key order is preserved as found, and it is worth noticing that it varies row to
row within one file, so no parser may depend on it. All 42 were checked back
against the archive they are attributed to.

**Single accent, heiban** (`position: 0`), 48.0% of the corpus - NHK:

```json
["ああ", "pitch", {"pitches": [{"position": 0, "devoice": [], "nasal": []}], "reading": "ああ"}]
```

**Single accent, atamadaka** (`position: 1`) - 新明解第八版:

```json
["あ", "pitch", {"pitches": [{"nasal": [], "devoice": [], "position": 1}], "reading": "あ"}]
```

**Two accents in one row** - 大辞林第四版:

```json
["アーカイブ", "pitch", {"reading": "アーカイブ", "pitches": [{"position": 3, "devoice": [], "nasal": []}, {"devoice": [], "nasal": [], "position": 1}]}]
```

**Three accents in one row** - 大辞泉:

```json
["アーカイブ", "pitch", {"reading": "アーカイブ", "pitches": [{"devoice": [], "position": 1, "nasal": []}, {"nasal": [], "position": 3, "devoice": []}, {"devoice": [], "position": 0, "nasal": []}]}]
```

**Four accents in one row** - the corpus maximum, the only such row, NHK. Note
the same nasal marker on every accent:

```json
["不義理", "pitch", {"reading": "ふぎり", "pitches": [{"position": 3, "devoice": [], "nasal": [2]}, {"position": 0, "devoice": [], "nasal": [2]}, {"position": 1, "devoice": [], "nasal": [2]}, {"position": 2, "nasal": [2], "devoice": []}]}]
```

**Nasal marker** - NHK. Both a heiban-with-nasal and a two-accent row where both
accents carry it:

```json
["合鍵", "pitch", {"reading": "あいかぎ", "pitches": [{"devoice": [], "position": 0, "nasal": [4]}]}]
["相合い傘", "pitch", {"pitches": [{"position": 5, "nasal": [5], "devoice": []}, {"nasal": [5], "position": 4, "devoice": []}], "reading": "あいあいがさ"}]
```

**Devoice marker** - NHK:

```json
["アーク灯", "pitch", {"reading": "アークとう", "pitches": [{"nasal": [], "devoice": [3], "position": 0}]}]
["アーティスト", "pitch", {"reading": "アーティスト", "pitches": [{"devoice": [4], "nasal": [], "position": 1}]}]
```

**Both markers on one expression, two accents** - NHK's `扱い`, which is also the
best single test of merge-plus-dedup because four other dictionaries have the
same headword:

```json
["扱い", "pitch", {"pitches": [{"devoice": [2], "nasal": [], "position": 0}, {"position": 3, "nasal": [], "devoice": [2]}], "reading": "あつかい"}]
```

**Two rows for one expression and reading, in one dictionary** -
三省堂国語辞典第八番. The parser must merge these into `{0, 1}`, not keep the last:

```json
["ああ", "pitch", {"reading": "ああ", "pitches": [{"position": 0, "devoice": [], "nasal": []}]}]
["ああ", "pitch", {"reading": "ああ", "pitches": [{"devoice": [], "nasal": [], "position": 1}]}]
```

**One row repeating an accent** - 大辞泉, 11 such rows in the corpus. Two
`position: 0` entries in one `pitches` list, so deduplication is needed *within*
a row and not only across dictionaries:

```json
["一体", "pitch", {"reading": "いったい", "pitches": [{"position": 0, "devoice": [], "nasal": []}, {"nasal": [], "position": 1, "devoice": []}, {"nasal": [], "position": 0, "devoice": []}]}]
```

**The four-accent header row, as it actually arrives** - `白目 / しろめ`, eight
rows from five dictionaries, unioning to `{0, 1, 2, 3}`:

```json
["白目", "pitch", {"reading": "しろめ", "pitches": [{"devoice": [], "position": 2, "nasal": []}, {"devoice": [], "position": 1, "nasal": []}]}]
["白目", "pitch", {"pitches": [{"nasal": [], "devoice": [], "position": 1}], "reading": "しろめ"}]
["白目", "pitch", {"reading": "しろめ", "pitches": [{"devoice": [], "position": 2, "nasal": []}]}]
["白目", "pitch", {"reading": "しろめ", "pitches": [{"position": 0, "nasal": [], "devoice": []}, {"devoice": [], "position": 1, "nasal": []}]}]
["白目", "pitch", {"reading": "しろめ", "pitches": [{"position": 2, "devoice": [], "nasal": []}]}]
["白目", "pitch", {"reading": "しろめ", "pitches": [{"devoice": [], "position": 2, "nasal": []}, {"nasal": [], "position": 3, "devoice": []}, {"position": 0, "devoice": [], "nasal": []}]}]
["白目", "pitch", {"reading": "しろめ", "pitches": [{"devoice": [], "nasal": [], "position": 0}, {"devoice": [], "nasal": [], "position": 1}]}]
["白目", "pitch", {"reading": "しろめ", "pitches": [{"nasal": [], "position": 2, "devoice": []}, {"devoice": [], "position": 0, "nasal": []}, {"position": 3, "nasal": [], "devoice": []}]}]
```

in file order: 大辞林第四版 ×2, 大辞泉 ×2, 三省堂国語辞典第八番, 新明解第八版 ×2, NHK.

**Majority plus one dissenter** - `合縁奇縁 / あいえんきえん`, where 大辞泉 says
heiban and the other three say `5`. This is a *partial* overlap, not a disjoint
one: three dictionaries share `5`, so the header row draws two accents and names
three dictionaries against one:

```json
["合縁奇縁", "pitch", {"pitches": [{"nasal": [], "devoice": [], "position": 0}], "reading": "あいえんきえん"}]
["合縁奇縁", "pitch", {"pitches": [{"devoice": [], "nasal": [], "position": 5}], "reading": "あいえんきえん"}]
["合縁奇縁", "pitch", {"pitches": [{"position": 5, "devoice": [], "nasal": []}], "reading": "あいえんきえん"}]
["合縁奇縁", "pitch", {"reading": "あいえんきえん", "pitches": [{"position": 5, "devoice": [], "nasal": []}]}]
```

in file order: 大辞泉, 三省堂国語辞典第八番, 新明解第八版, NHK.

**Flat disagreement, nothing shared** - `あご / あご`, one of the 993 disjoint
pairs. Two dictionaries, two different downsteps, no deduplication possible, and
NHK's accent carries a nasal mark where 三省堂's `nasal` is empty:

```json
["あご", "pitch", {"reading": "あご", "pitches": [{"devoice": [], "nasal": [], "position": 1}]}]
["あご", "pitch", {"pitches": [{"devoice": [], "position": 2, "nasal": [2]}], "reading": "あご"}]
```

in file order: 三省堂国語辞典第八番, NHK.

**Longest reading, highest downstep, and a nasal marker in one row** - NHK, 19
morae, `position: 17`:

```json
["自動車損害賠償責任保険", "pitch", {"pitches": [{"nasal": [7], "devoice": [], "position": 17}], "reading": "じどうしゃそんがいばいしょうせきにんほけん"}]
```

**Downstep past the last mora** - 大辞林第四版, both such rows:

```json
["平宗太", "pitch", {"reading": "ひらそうだ", "pitches": [{"position": 6, "devoice": [], "nasal": []}]}]
["築後", "pitch", {"reading": "ちくご", "pitches": [{"nasal": [], "devoice": [], "position": 3}, {"position": 5, "devoice": [], "nasal": []}]}]
```

**A reading no headword will match**, three flavours, 122 rows in total:

```json
["扱い", "pitch", {"pitches": [{"devoice": [], "nasal": [], "position": 2}], "reading": "〜あつかい"}]
["或いは", "pitch", {"pitches": [{"nasal": [], "devoice": [], "position": 2}], "reading": "あるいは（ワ）"}]
["削がれる", "pitch", {"reading": "そが◦れる", "pitches": [{"devoice": [], "nasal": [], "position": 3}]}]
```

## The four fixtures the corpus cannot supply

The parser has to handle these because the schema permits them and Yomitan's
importer would accept them. **The corpus contains none of them, so any test
using the payloads below is testing against a construction, not against
evidence, and should say so.** They are schema-checked constructions, written
here from the schema quoted in Part 1:

```json
["れい", "pitch", {"reading": "れい", "pitches": []}]
["れい", "pitch", {"reading": "れい", "pitches": [{"position": "LHHL"}]}]
["れい", "pitch", {"reading": "れい", "pitches": [{"position": 2, "nasal": 3, "devoice": 1}]}]
["れい", "pitch", {"reading": "れい", "pitches": [{"position": 2, "tags": ["名"]}]}]
```

In order: an empty accent list; the `^[HL]+$` string form of `position`; the
scalar rather than list form of `nasal` and `devoice`; and a `tags` list.

"Schema-checked" is measured, not asserted: all four validate against the schema
quoted in Part 1 under `jsonschema` 4.26.0 as a Draft 7 validator, and a
negative control - the same row with a `"bogus"` key added to the accent - is
rejected, which also confirms `additionalProperties: false` bites.

```python
import json, jsonschema, urllib.request

SHA = "77e200428902abf4fa48284df92da7af3dcb4162"
url = (f"https://raw.githubusercontent.com/yomidevs/yomitan/{SHA}"
       "/ext/data/schemas/dictionary-term-meta-bank-v3-schema.json")
schema = json.loads(urllib.request.urlopen(url).read())
validator = jsonschema.Draft7Validator(schema)

rows = [
    ["れい", "pitch", {"reading": "れい", "pitches": []}],
    ["れい", "pitch", {"reading": "れい", "pitches": [{"position": "LHHL"}]}],
    ["れい", "pitch", {"reading": "れい", "pitches": [{"position": 2, "nasal": 3, "devoice": 1}]}],
    ["れい", "pitch", {"reading": "れい", "pitches": [{"position": 2, "tags": ["名"]}]}],
    ["れい", "pitch", {"reading": "れい", "pitches": [{"position": 1, "bogus": 1}]}],  # must fail
]
for row in rows:
    errors = list(validator.iter_errors([row]))
    print("VALID" if not errors else f"INVALID {errors[0].message}",
          json.dumps(row, ensure_ascii=False))
```

---

# Method

```sh
python3 tools/pitch-census/census.py ~/.local/share/chibipop/library
python3 tools/pitch-census/report.py
```

`census.py` writes `results/census.json`; `report.py` renders
`results/tables.md`, from which every table above is taken. Two seconds for the
99-archive library. See
[tools/pitch-census/README.md](../../tools/pitch-census/README.md).

Cross-checked against the 97-archive `~/Downloads/dict/Japanese` corpus
[dict-shapes.md](dict-shapes.md) used:

```sh
python3 tools/pitch-census/census.py ~/Downloads/dict/Japanese --out tools/pitch-census/results/downloads.json
```

The same five pitch archives, and every per-dictionary counter and every
agreement total identical - so the library is not a narrower sample than the
download folder, and the two corpora hold the same copies of these archives.

Four decisions the numbers depend on:

- **Bank discovery matches chibipop's, not Yomitan's.** An entry name starting
  `term_meta_bank_` and ending `.json`, at the zip root - the rule
  `sorted_banks` (`src/dict/archive.rs:509`) applies. Nested names are counted
  and skipped; there are **0** in this library, so the two rules cannot diverge
  here.
- **The pitch role is `row[1] == "pitch"`, never the filename.** This is what
  found the mislabelled sixth archive.
- **Mora counting and marker normalisation are ported from Yomitan**, not
  reinvented: `getKanaMorae`, `getDownstepPositions` and `_toNumberArray`,
  quoted in Part 1. A mora index this census counts is the index Yomitan counts.
- **Accents are compared as sets of a canonical token.** `position` integer `N`
  becomes `dN`; an `HL` string reducing to a single downstep becomes the same
  `dN`, so the two forms would compare equal if any archive used the string form.
  "Identical" means equal sets; "partial" means at least two dictionaries share
  an accent and at least one names another; "disjoint" means no two dictionaries
  share any accent.

The one workaround: `inflate_member` bypasses the CRC-32 check, because
otherwise there is no pitch data to census at all. Every bypassed member is
recorded in `crc_mismatch`, and a length mismatch is still fatal, so a
genuinely truncated member could not pass as intact.

The single CRC-32 comparison quoted in
[The blocker](#the-blocker-a-wrong-crc-32-on-48-of-54-members) came from this
one-off, which reuses the tool's own inflate so the two agree by construction:

```python
import json, sys, zipfile, zlib
sys.path.insert(0, "tools/pitch-census")
from census import inflate_member

path = "~/.local/share/chibipop/library/[Pitch] 新明解第八版.zip"
with zipfile.ZipFile(__import__("os").path.expanduser(path)) as z:
    info = z.getinfo("term_meta_bank_1.json")
    data = inflate_member(z, "term_meta_bank_1.json")
    print("inflated", len(data), "declared", info.file_size)
    print("actual crc %08x, stored crc %08x" % (zlib.crc32(data) & 0xffffffff, info.CRC))
    print("rows", len(json.loads(data)))
```

## What this does not answer

- **Five archives, one generator.** All five come from コツ / kotu.io with the
  same `description` string. The field-shape findings - always a list, never a
  tag, always an integer `position` - are one producer's habits measured five
  times. A pitch dictionary from another source could use every form the schema
  permits, and this census would not know.
- **No archive here exercises `pitches: []`, the `HL` string, the scalar
  marker, or `tags`.** Stated with constructions in
  [the fixtures the corpus cannot supply](#the-four-fixtures-the-corpus-cannot-supply)
  rather than left silent, but they remain untested against reality.
- **No archive here has both the pitch role and another role.** 9
  frequency-only and 5 pitch-only, none both, and the pitch archives ship no
  term banks. So the census cannot say what a combined archive looks like, and
  the role-set model's dual-role path has no specimen in this library.
- **No `ipa` row exists in either corpus**, so the deliberately ignored mode is
  unmeasured beyond "it is in the enum and nobody here emits it".
- **Whether the CRC-32 problem is general.** It is measured on five archives
  from one publisher. Whether pitch archives from other sources share it is
  unknown, and whether the right fix is tolerating a bad checksum or repairing
  the archive at import is the implementation's call, not this doc's.
- **Nothing about rendering.** The census bounds the header row at 4 accents
  over readings up to 19 morae; it does not say that fits, and the spec settles
  the notation. Layout correctness belongs to the geometry goldens.
