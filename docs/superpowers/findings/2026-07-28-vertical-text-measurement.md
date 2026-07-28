# Vertical text — measurement against candidate capture shapes

**Task:** accuracy-and-polish plan, Task 2 (spec §5). **Measures only; changes no constant.**
**Date:** 2026-07-28
**Instrument:** `probe --at X,Y [--region W,H]`, release build at `17e18b0`.
**Screen:** portrait secondary only (x ≥ 2560, 1080×1920).

## Verdict

**Vertical text is broken at the shipped capture shape, and the failure mode is worse than
"spotty": it is not that less text comes back, it is that a sentence which does not exist on
screen comes back.** Over six hover points on two vertical lines, the default 500×100 region
resolved the correct character twice, returned nothing at all three times, and twice returned
text assembled by reading *across four unrelated columns*.

A transposed 100×500 probe fixes it: 6/6 correct characters, 5/6 exactly correct column text.

**One further result decides the design and was not anticipated by the spec:** at the top of a
column the default shape also **misclassifies the orientation as Horizontal**, which rules out
spec §5's candidate (b) *as written*. See "What this rules out" below.

## Material

Manga page open in Paint, screen-vertical Japanese with furigana. Two speech bubbles.

**Ground truth, transcribed by eye before any probe ran:**

- Left bubble: 「日に日に人間離れ / していくんだが / ユカ友達で / いてくれる？」
- Right bubble: 「人間じゃないって / そう簡単な / 話じゃないか…」

**Pixel size**, from probe's own word boxes: base glyphs **~25 × 25 px** (日 18×25, 人 28×25,
間 26×28); column pitch 35–47 px; furigana 9–14 px.

## Baseline and candidate shapes

Six points, three per bubble, all on the first (rightmost) column.
`✓ text` means the whole column came back exactly as transcribed.

### Left bubble — 日に日に人間離れ

| point | shape | orient | text returned | char |
|---|---|---|---|---|
| 日 (2874,1544) | **default 500×100** | **Horizontal** | `いユし日` — one glyph from each of 4 columns | 日 ✓ |
| | 100×500 | Vertical | `日に日に人間離れ` ✓ | 日 ✓ |
| | 300×300 | Vertical | `日に日に人戸` (離→戸, れ lost) | 日 ✓ |
| | 400×400 | Vertical | `日に日に人間離` (れ lost) | 日 ✓ |
| 人 (2874,1662) | **default** | — | **nothing resolved** | — |
| | 100×500 | Vertical | `日に日に人間離れ` ✓ | 人 ✓ |
| | 300×300 | Vertical | `日に日に人間離れ` ✓ | 人 ✓ |
| | 400×400 | Vertical | `日に日に人間離れ` ✓ | 人 ✓ |
| 離 (2874,1723) | **default** | Vertical | `間離れ` (truncated) | 離 ✓ |
| | 100×500 | Vertical | `日に日に人間離れ` ✓ | 離 ✓ |
| | 300×300 | Vertical | `」日に人間離れ` (日→」) | 離 ✓ |
| | 400×400 | Vertical | `日に日に人間離れ` ✓ | 離 ✓ |

### Right bubble — 人間じゃないって

| point | shape | orient | text returned | char |
|---|---|---|---|---|
| 人 (3568,1387) | **default** | **Horizontal** | `話そ人` — 3 columns spliced | 人 ✓ |
| | 100×500 | Vertical | `人問じゃないって` (間→問) | 人 ✓ |
| | 300×300 | Vertical | `人間じゃない` (truncated) | 人 ✓ |
| | 400×400 | Vertical | `人間じゃないって` ✓ | 人 ✓ |
| な (3565,1493) | **default** | — | **nothing resolved** | — |
| | 100×500 | Vertical | `人間じゃないって` ✓ | な ✓ |
| | 300×300 | Vertical | `人間じゃないって` ✓ | な ✓ |
| | 400×400 | Vertical | `人間じゃないって` ✓ | な ✓ |
| て (3563,1575) | **default** | — | **nothing resolved** | — |
| | 100×500 | Vertical | `人間じゃないって` ✓ | て ✓ |
| | 300×300 | Vertical | `じゃないって` (truncated) | て ✓ |
| | 400×400 | Vertical | `人間じゃないって` ✓ | て ✓ |

### Totals over all six points

| shape | correct character | column text exactly right | nothing resolved | **fabricated text** |
|---|---|---|---|---|
| **default 500×100** | 2/6 | 0/6 (best was a truncation) | 3/6 | **2/6** |
| 100×500 | **6/6** | **5/6** | 0/6 | 0/6 |
| 300×300 | **6/6** | 3/6 | 0/6 | 0/6 |
| 400×400 | **6/6** | **5/6** | 0/6 | 0/6 |

**Forward reading distance** from the cursor: default managed **1** character at its single usable
point (`間離れ` hovering 離). 100×500 at the top of a column yields **7** — the rest of the column.

## The fabrication, in detail

This is the finding that matters, so it is recorded with the raw output. At (2874,1544), default
shape:

```
ocr line 0: "いユし日"
  word "い"  x=2744 …   word "ユ"  x=2779 …   word "し"  x=2830 …   word "日"  x=2865 …
ocr line 1: "てカてに"
  word "て"  x=2744 …   word "カ"  x=2781 …   word "て"  x=2826 …   word "に"  x=2864 …
orient:  Horizontal
line:    いユし日
```

A 100 px-tall band across vertical text contains the **top row of four different columns**.
Windows OCR groups them into horizontal rows — correctly, given the image it was handed — and
`orientation_of` then sees word centres spread in x, not y, and answers `Horizontal`. The string
passed to lookup, `いユし日`, is four unrelated first characters read left to right.

The character under the cursor was still correct (日), so **a first-character acceptance check
passes while the text is nonsense** — the same class of defect §2.1 of the spec was written to
catch on the tiled path.

## What this rules out

Spec §5 offers two candidate fixes. The measurement decides between them, and rules one out:

- **(b) "a second orientation-aware probe taken only when pass 1's line looks vertical" cannot
  work as specified.** At the top of a column — exactly where the default fails hardest — pass 1
  reports `Horizontal`. The trigger would not fire. Both square shapes reported `Vertical` at all
  six points, including those two.
- **(a) a squarer pass-1 probe serving both orientations** works for *detection* (300×300 and
  400×400: 6/6 Vertical) but is the weaker *reader*: 300×300 got the column exactly right only
  3/6, and a wider box on a dense vertical page would pull in more neighbouring columns, which is
  the measured direction of harm.

The shape that reads best is the narrow transpose (100×500, 5/6), and the shape that detects best
is a square. That points at **square-then-transpose** — detect orientation on a square pass 1,
then read the column with a transposed second capture — but choosing and tuning that is a later
round, per the spec. **No constant was changed.**

Two smaller notes for that round:

- `band_of` floors the perpendicular extent at `REGION_H`, i.e. it floors a *width* with a
  *height* constant, so vertical tiles come out 100×500 — the transpose that measured best here.
  That is luck rather than design, and it is untested at any other text size.
- Pass 1's region is not clamped to the monitor. At (2696,850) on the left edge of monitor 2 the
  500-wide region begins at x=2446, i.e. 114 px onto monitor 1. It did not break recognition, but
  it means `split_at_clipped` judges words against a boundary the capture never reached.

## Reproduce

```bash
cd /c/Users/Stella/chibipop
for r in "500,100" "100,500" "300,300" "400,400"; do
  printf "%-9s " "$r"
  ./target/release/chibipop.exe probe --at 2874,1662 --region "$r" 2>&1 | grep -E "^orient:|^line:"
done
```

Multibyte-safe extraction of the resolved character (`.` matches one *byte* in grep/sed here):

```bash
sed -n "s/^at: .*-> Some('\(.*\)').*/\1/p"
```
