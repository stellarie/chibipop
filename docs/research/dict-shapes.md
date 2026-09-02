# What real Yomitan dictionaries actually emit

Census of 97 Yomitan archives (a working Japanese dictionary library), taken
2026-08-29 with [tools/dict-census](../../tools/dict-census/README.md). Every
`term_bank_*.json` glossary tree walked, first 30 000 term rows per dictionary.

This doc exists to size the rendering work in the dictionary render-parity
effort. The Yomitan schema permits far more than any dictionary uses, so
ranking the schema by what it *allows* sizes the work wrong. Everything below
is ranked by **how many dictionaries use a feature** - the number that decides
whether skipping it is free or visible.

Regenerate with:

```sh
python3 tools/dict-census/census.py <corpus-dir>
python3 tools/dict-census/report.py
```

## Headline

- 72 of 97 archives carry term banks. The other 25 are frequency and pitch
  archives, which carry no glossary at all.
- **52 of 72 use structured content. 20 emit plain strings only.**
- The 20 plain-string dictionaries include 精選版 日本国語大辞典, 漢字源,
  全訳漢辞海, 新和英, and JMnedict. They reach full parity from the
  sense-splitting fix alone, with no renderer work.

## Tags

| tag | #dicts | #nodes | chibipop today |
|---|---:|---:|---|
| `div` | 45 | 4,268,103 | kept |
| `a` | 43 | 1,275,340 | kept |
| `span` | 43 | 20,488,331 | kept |
| `img` | 30 | 386,141 | **dropped** |
| `li` | 20 | 838,412 | kept |
| `ul` | 19 | 241,460 | kept |
| `tr` | 18 | 170,366 | kept |
| `td` | 18 | 414,115 | kept |
| `table` | 16 | 65,544 | kept |
| `ruby` | 14 | 864,045 | kept |
| `rt` | 14 | 864,054 | **dropped** |
| `th` | 11 | 117,256 | kept |
| `br` | 9 | 40,802 | kept |
| `ol` | 4 | 20,427 | kept |
| `details` | 4 | 30,908 | **unsupported** |
| `summary` | 4 | 31,169 | **unsupported** |
| `tbody` | 1 | 5,238 | kept |

Three findings contradict what a schema reading predicts:

**`img` is the fourth most widely used tag, and it is dropped.** See Media.

**`ruby` is used by 14 dictionaries and 864k nodes, and `rt` is dropped.**
Furigana is deleted today, not merely unstyled. A schema-and-JMdict reading
ranks ruby near-last, because JMdict renders readings as parenthetical text and
never emits `ruby`. The monolingual dictionaries disagree.

**`details`/`summary` are real.** Four dictionaries, 31k nodes, outside the
allow-list, so the wrapper is dropped and the summary and body concatenate.

## Style properties

| style property | #dicts | #nodes | chibipop today |
|---|---:|---:|---|
| `fontWeight` | 24 | 846,326 | mapped |
| `fontSize` | 18 | 772,971 | mapped |
| `verticalAlign` | 18 | 632,811 | mapped |
| `listStyleType` | 15 | 253,325 | mapped |
| `marginRight` | 14 | 374,029 | **unsupported** |
| `marginBottom` | 12 | 287,752 | **unsupported** |
| `borderStyle` | 11 | 242,678 | **unsupported** |
| `borderRadius` | 11 | 192,951 | **unsupported** |
| `borderWidth` | 11 | 241,547 | **unsupported** |
| `padding` | 11 | 259,954 | **unsupported** |
| `cursor` | 10 | 154,322 | **unsupported** |
| `marginLeft` | 8 | 101,019 | **unsupported** |
| `color` | 8 | 380,317 | **unsupported** |
| `textAlign` | 7 | 249,242 | **unsupported** |
| `fontStyle` | 6 | **238** | mapped |
| `whiteSpace` | 4 | 122,882 | **unsupported** |
| `backgroundColor` | 4 | 31,793 | **unsupported** |
| `marginTop` | 3 | 162,104 | **unsupported** |
| `borderColor` | 3 | 46,899 | **unsupported** |
| `paddingLeft` | 1 | 1,398 | **unsupported** |
| `textDecorationLine` | 1 | **305** | mapped |
| `margin` | 1 | 38,904 | **unsupported** |

**The six properties chibipop maps include the two least used in the corpus.**
`fontStyle` appears 238 times and `textDecorationLine` 305 times, while
`marginRight` appears 374 029 times across 14 dictionaries.

The unsupported block is dominated by box-model properties: margin, padding,
border style, border width, and border radius, each in 11 to 14 dictionaries.
That is how **these** dictionaries draw part-of-speech and label pills. A
renderer that styles text runs but has no box model reproduces none of them.
This is the single largest scope correction the census produced.

It is also only half the picture, because this table counts inline `style` only.
A second, almost disjoint set of dictionaries draws its boxes from its own
stylesheet instead. See [Dictionary stylesheets](#dictionary-stylesheets-stylescss).

`cursor` (10 dictionaries) is the one high-usage property that is safely
cosmetic - it marks clickable spans, which a hit target already expresses.

## Dictionary stylesheets (`styles.css`)

**14 of the 72 dictionaries with term rows ship a `styles.css`.** All 14 use
structured content and all 14 declare box-model properties in it: 908 box rules
across 173 964 bytes, 1 412 rules, and 1 687 selectors.

The mechanism barely overlaps with inline `style`. Over the 52
structured-content dictionaries, counting where box-model properties live:

| bucket | #dicts |
|---|---:|
| inline-only | 29 |
| **css-only** | **13** |
| both | 1 |
| no box model at all | 9 |

The 13 `css-only` dictionaries emit **not one** inline `margin`, `padding`,
`border`, or `background` on any node in the sample. Every box they draw lives
in the stylesheet: 旺文社漢字典 (164 box rules), 小学館例解学習国語 (155),
角川新字源 (102), 旺文社 全訳古語辞典 (86), 三省堂 全訳読解古語辞典 (74),
大辞泉 (63), 明鏡国語辞典 (63), 旺文社国語辞典 (44), 有斐閣現代心理学辞典 (29),
TISMKANJI (26), 南山堂医学大辞典 (26), 字通 (20), 有斐閣法律用語辞典 (7).

The single `both` dictionary is Jitendex, and the label flatters it: its only
inline box property is `listStyleType`, a list marker. Its actual pill is
CSS-only, `span[data-sc-class="tag"]` with `border-radius`, `padding`,
`margin-right`, and `vertical-align`, over 48 776 nodes in the sample. So by the
strict bucket rule 13 dictionaries are css-only, and by the question that
matters - where does the pill come from - **all 14 draw their boxes in CSS**.

9 of the 14 carry at least one rule with the full pill signature, a radius plus
a padding plus an edge in one rule, for 47 such rules.

### The selector surface is small, because the format forces it to be

Structured content has no `class` attribute. A dictionary author can only reach
content through a tag or through the `data-*` attributes Yomitan derives from a
node's `data` map, and the counts show exactly that:

| selector kind | #dicts | #selectors |
|---|---:|---:|
| `data-attr` | 14 | 1 573 |
| `tag` | 13 | 1 092 |
| `pseudo-class` | 12 | 150 |
| `class` | 12 | 70 |
| `pseudo-element` | 6 | 18 |
| `other-attr` | 5 | 15 |
| `universal` | 2 | 2 |

No `id` selector appears anywhere. 72 of the 73 class tokens name Yomitan's own
popup chrome (`.gloss-image-container`, `.gloss-image-link`, `.gloss-image`),
not dictionary content; the one exception, `p.字音 em` in 字通, can never match
anything, which is itself the proof of the point.

**897 of the 908 box rules carry a `data-*` or tag selector**, so they land on
the dictionary's own content rather than on popup chrome. Only 11 target chrome
classes alone.

900 of 1 183 (dictionary, `data-*` key) pairs name a key the term-bank walk
actually saw, and all 20 of the most-selected keys match. The unmatched
remainder is an artefact of the 30 000-row cap, measured rather than assumed:
re-running 小学館例解学習国語 with `--rows 0` lifts its match from 33 of 112 keys
to 98 of 112.

### At-rules bound the cost

| at-rule | #dicts | occurrences |
|---|---:|---:|
| `@media` | 6 | 6 (always `(max-width: 500px)`) |
| `@keyframes` | 2 | 2 (`details-show`) |

No `@supports`, `@font-face`, `@import`, `@layer`, `@container`, or `@charset`
in the corpus. **Only 4 of the 908 box rules sit behind an `@media` block**, so
an implementation that ignores at-rules entirely loses four rules.

### What this measurement does not settle

The census reads stylesheets, it does not cascade them. It can say a rule
carries a border; it cannot say the border wins, or that it looks like a pill.
There is no specificity resolution and no `var()` substitution. Matching a
`data-*` key against the content counter proves the key exists, not that a
descendant combinator's ancestor constraint also holds.

## Attributes

| attribute | #dicts | #nodes |
|---|---:|---:|
| `href` | 43 | 1,275,340 |
| `title` | 10 | 235,139 |
| `rowSpan` | 7 | 116,813 |
| `lang` | 3 | 78,022 |
| `colSpan` | 2 | 4,597 |

`href` on 43 dictionaries is near-universal. Most are internal `?query=`
cross-references, which map onto the existing drill-down action.

## Media

- **30 of 52** structured-content dictionaries emit image nodes; 386 141 nodes
  in the sample.
- **427 786 nodes carry a `data.gaiji` marker.** These are characters the
  dictionary font lacks, not illustrations.
- 286 334 image nodes declare `width` or `height`. **99 807 declare neither**
  and need an intrinsic size recorded at build time.

| media extension | #dicts | #nodes |
|---|---:|---:|
| `.png` | 19 | 105,434 |
| `.svg` | 12 | 106,691 |
| `.jpg` | 8 | 8,670 |
| `.avif` | 4 | 23,808 |
| `.jpeg` | 4 | 2,393 |
| `.gif` | 3 | 139,145 |

| image field | #dicts | #nodes |
|---|---:|---:|
| `path` | 30 | 386,141 |
| `collapsible` | 30 | 384,894 |
| `background` | 27 | 213,823 |
| `collapsed` | 26 | 243,264 |
| `sizeUnits` | 21 | 286,334 |
| `height` | 20 | 286,136 |
| `appearance` | 15 | 117,687 |
| `width` | 13 | 113,972 |
| `alt` | 13 | 70,949 |
| `imageRendering` | 5 | 42,146 |
| `title` | 4 | 31,777 |
| `verticalAlign` | 2 | 40,814 |

| dictionary | image nodes | gaiji-marked |
|---|---:|---:|
| 字通 | 139,138 | 278,340 |
| 旺文社漢字典 第四版 | 43,533 | 48,528 |
| 三省堂国語辞典 第八版 | 42,625 | 0 |
| Pixiv Light | 30,000 | 0 |
| 旺文社 全訳古語辞典 | 27,538 | 0 |
| 国語辞典オンライン | 17,744 | 0 |
| 明鏡国語辞典 第三版 | 11,877 | 23,740 |
| 漢検漢字辞典 第二版 | 10,814 | 0 |
| 角川新字源 改訂新版 | 9,120 | 975 |
| 漢字でGo! | 8,024 | 0 |

Representative nodes:

```json
{"tag":"img","path":"24/1188db.gif","height":1.0,"sizeUnits":"em",
 "collapsible":false,"data":{"img":"","gaiji":""}}

{"tag":"img","path":"sankoku8/svg-intonation/短.svg","height":1.0,
 "width":1.5384615384615385,"sizeUnits":"em","appearance":"monochrome",
 "background":false,"title":"短","collapsed":false,"collapsible":false}

{"tag":"img","path":"gaiji/対義語.svg","background":false,"collapsed":false,
 "collapsible":false,
 "data":{"gaiji":"","class":"gaiji","alt":"［対義語］","src":"gaiji/対義語.svg"}}
```

`height: 1.0, sizeUnits: "em"` is the shape that matters. These sit inline at
the text size, in the middle of a definition. Dropping them does not lose an
illustration; it punches holes in words. 字通 averages more than four image
nodes per term row.

`appearance: "monochrome"` on 15 dictionaries means the asset is a mask to be
tinted with the body text colour, so it must follow the theme rather than paint
its own black.

Five raster formats plus SVG. `.avif` on four dictionaries is the one that
needs a decoder chibipop does not otherwise want.

## Editorial drops

*Measured against an earlier build. The `DROP_CONTENT` const it names is
gone; the section is kept as the evidence that motivated its deletion, and
the census now prints a role table per dictionary in its place.*

`DROP_CONTENT` removes 40 975 nodes, **all of them from Jitendex**. No other
dictionary in the corpus tags content with those six names.

Meanwhile other dictionaries carry example sentences under keys the list does
not name, and chibipop renders every one of them:

| dictionary | data hook | #nodes |
|---|---|---:|
| 明鏡国語辞典 第三版 | `example=` | 38,892 |
| Jitendex | `content=attribution` | 21,002 |
| 角川新字源 改訂新版 | `example=` | 6,997 |
| Jitendex | `content=example-keyword` | 4,811 |
| Jitendex | `content=example-sentence` | 4,772 |
| Jitendex | `content=example-sentence-a` | 4,772 |
| Jitendex | `content=example-sentence-b` | 4,772 |
| 旺文社漢字典 第四版 | `ExampleG=` / `ExampleC=` | 1,737 ea. |
| surasura 擬声語 | `content=examples` | 1,420 |
| 数え方辞典オンライン | `name=example_1` … `_8` | 2,882 total |
| Onomatoproject | `name=examples` | 266 |

### The role vocabulary is bilingual, and Japanese dominates

The table above is ASCII-only, which under-reports examples by roughly twenty
times. Four dictionaries tag example content in Japanese, at **755 242 nodes**:

| dictionary | keys | #nodes |
|---|---|---:|
| 旺文社 全訳古語辞典 | `用例`, `用例訳`, `用例引用`, `用例G`, `用例活用`, and 6 more | 449,000 |
| 三省堂 全訳読解古語辞典 | `用例`, `用例G`, `用例訳注`, `識別例文` | 96,000 |
| 三省堂国語辞典 第八版 | `name=用例`, `name=用例G` | 46,772 |
| 大辞林 第四版 | `name=用例`, `name=慣用例`, `name=用例注釈` | 17,105 |

Other Japanese-tagged roles, all currently rendered inline with no distinction:
`出典` (sources, 50 772), `参照` (cross-references, 65 022), `解説` (commentary,
173 225), `対義` (antonyms, 33 575), `類語` (synonyms, 17 402), `補説` (8 039),
`語源` (etymology, 2 385).

Matching must be on a key **substring**, not equality: 旺文社 全訳古語辞典 uses
eleven distinct `用例`-prefixed keys, and some carry the role in a `class` value
such as `class=fill 用例 FM` rather than in the key itself.

So the current behaviour is not a policy, it is an inconsistency: Jitendex
loses its examples and 明鏡 keeps 38 892 of them. The `data` key namespace is
per-dictionary and unbounded, so a name-matching drop list cannot be made
correct by adding names to it.

## Nesting depth

Deepest tree per dictionary, distribution: depth 2 to 5 for 23 dictionaries,
depth 6 to 11 for 17, and **depth 13 to 15 for 12**. The parser needs a depth
cap, and the layout pass must not recurse without one.

Eight to eleven dictionaries also carry `data.xmlns`, `data.html`, and
`data.body` hooks - they were converted from HTML or EPUB sources and drag
wrapper elements through. That is why generic `div`/`span` dominate the tag
table so heavily.

## Not in the corpus glossaries at all

`float` (absent from the schema entirely), `clipPath`, `textShadow`,
`textEmphasis`, `wordBreak`, `textDecorationStyle`, `textDecorationColor`,
locale list-counter values such as `hiragana-iroha`, and `tfoot`. Zero nodes.
Cutting them is measured, not assumed.

## Pitch accent is a different feature

Six of the 25 term-bankless archives are pitch dictionaries. Pitch is not
structured content: it is numeric mora data in `term_meta_bank`, which Yomitan
draws as SVG client-side. chibipop reads that bank today for frequency and
discards every `pitch` row. Rendering pitch is separate work with no dependency
on this census.
