# Live scan survey — what chibipop reads, what it does not, and what it costs

**Date:** 2026-07-28. **Screen:** portrait secondary (2560,0)–(3640,1920), 1080×1920.
**Build:** release at `274ec70`+. **Requested by the user:** scan real windows, record what works
and what does not, and hold RAM < 50 MB, CPU < 5%, binary < 100 MB.

Three windows were open and all three were swept: a **ttsu-reader** novel (horizontal, furigana,
light-on-dark), the **gakuen.idolmaster-official.jp** site (mixed, decorative, light-on-colour),
and a **manga page in Paint** (vertical, furigana, dark-on-light).

---

## 1. Resource budget — the user's three limits

| Limit | Measured | Verdict |
|---|---|---|
| Binary < 100 MB | **3.3 MB** (`target/release/chibipop.exe`, 3 409 920 B) | **PASS**, by 30× |
| CPU < 5% | **0.40 – 0.66%** of 16 cores under continuous hovering; **0.00%** idle | **PASS** |
| RAM < 50 MB | **see below — two of three configurations pass; the third is unverified** | **partial** |

### Memory, measured three ways

| Configuration | Working set | Private bytes | Notes |
|---|---|---|---|
| `run`, idle, all windows created | **11.96 MB** | **2.61 MB** | popup + D2D renderer + tray + overlay all alive |
| `run`, idle, `highlight_match = false` | 11.95 MB | 2.61 MB | the overlay window costs **nothing** at rest |
| `watch`, 417 hovers over 150 s, 3-pass tiling | **37.1 MB** (peak 37.8) | **14.8 MB** (peak 15.1) | plateaus by ~30 s and stays flat |

The `watch` run is the full capture → OCR → resolve → lookup hot path, driven harder than the
shipped default (three OCR passes per hover rather than one), for 417 hovers. **Working set
plateaus at 37 MB and private bytes at 15 MB, both flat from 30 s onward, handle count pinned at
209 for the whole run — no leak in the hot path.**

**What is not covered by any of these numbers:** `run` under sustained *real* hovering, which adds
the DirectWrite/D2D glyph rendering path that `watch` never touches. That is precisely the
configuration in which M3 recorded **94.8 MB WS / 60 MB private**, which **misses** the 50 MB
target. It could not be re-measured today because synthetic input does not reach the app's
low-level mouse hook from this environment (see the acceptance note). **The 50 MB target should
still be treated as unmet until oniichan hovers for two minutes with `run` up and checks.**

Sampling harness: `scratchpad/stress.ps1` (drives `watch`; will not drive `run`).

---

## 2. What reads correctly

| Content | Example | Result |
|---|---|---|
| **Horizontal novel prose, light on dark, ~26 px** | `駅長は宿舎の方へ手の明りを振り向けた。` | **Exact.** Including 明, which the two-pass round recorded tiling misreading as 月 |
| **Deconjugated idiom** | hover 風邪 → `風邪をひく`, `match=6` | Exact; highlight spans 風邪をひいて |
| **Small web body text, ~11 px** | `「お金を稼げるアイドル」を夢見る、がめつい女の子。` | **Usable with misreads**: 稼→物, め→町. Resolved character and lookup still correct |
| **Web nav menu, ~13 px** | `学園長挨拶プロデューサー科学園名` | Reads; note separate menu items on one baseline become **one** OCR line |
| **Large light-on-orange banner, ~30 px** | `藤田ことね` | Reads the base text; the surrounding ruby and logotype degrade to noise |
| **Vertical manga text — with a transposed region** | `日に日に人間離れ` | **Exact**, 8/8 characters. See the vertical-text findings |

## 3. What does not read

| Content | Why | Fixable? |
|---|---|---|
| **Vertical text at the default 500×100 region** | The band spans four columns and ~3 characters of the reading axis. Returns nothing, or a sentence spliced across columns | **Yes** — measured; a later round |
| **Text clipped by a window edge** | `この生徒をシェアする` sat half-cut at the browser viewport bottom. **No** capture shape recovered it (tried 500×100, 500×60, 300×80, 400×150) | **No.** The glyphs are physically incomplete on screen; this is a ceiling, not a defect |
| **Decorative logotypes** | `学園アイドルマスター` logo → `国彳イ白確舛ー` | No, and not worth trying |
| **Ruby adjacent to its base** | In the manga, hovering (3550,1450) resolved **ん from the furigana かんたん**, not the base text | Partly — `nearest_line` already handles the tiled case; hit-scan on pass 1 does not |

### One correction worth recording

Light text on saturated orange initially looked like a **contrast** failure — the vertical ribbon
`褒められるのが好き!` returned zero OCR lines at 500×100. It is not. At `--region 150,500` the same
text reads as `、褒められるのが好き`. **The failure was the region shape on vertical text again, not
the colour.** Two independent-looking failures turned out to be one cause.

## 4. Limitations of the test itself

- **The app's own hover path was never driven.** Synthetic input (`SetCursorPos`, `SendInput`
  absolute/virtualdesk, in-process and detached) does not reach `WH_MOUSE_LL` from this
  environment. Everything about the popup's own behaviour under real hovering — latency, flicker,
  the capture guard's second hide/restore now that the overlay exists by default, memory under
  render load — remains **unverified by anyone but oniichan's own eyes**.
- Every OCR number here comes from **one machine, one recogniser version, and four text sizes on
  three windows**. The 25 px manga and 26 px novel text are well inside the recogniser's comfort;
  the 11 px web text is at its edge and misread two characters in one sentence.
- Ground truth for the vertical sweep was transcribed by eye before probing. Ground truth for the
  web text was read off a full-resolution crop after the fact, which is weaker.
