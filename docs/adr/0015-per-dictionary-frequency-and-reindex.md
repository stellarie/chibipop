# Frequency stored per dictionary, ranked through a reindexed column

Frequency was a build-time global. `load_freqs` read each archive into its own table and
then `extend`ed them together, so across archives the **last** one silently won, and
`prepare_bank` stamped that single merged number onto every dictionary's rows — the
database held no per-dictionary frequency at all. So an ordered frequency list was
unimplementable: dragging a row changed nothing that was not already frozen into SQLite.
The obvious fix — read frequency per dictionary at lookup and apply the order there —
collides with a documented invariant: `term.freq` is read off the hot `term` row with no
join, ~25 point queries per hover, and `lookup::model` says in as many words that the
column is denormalised so grouping and dictionary-priority ranking cost no join.

**Decision: store Reported frequency per dictionary, keep `term.freq` as the ranking
column, and recompute it with a Reindex — an in-place transaction over data already in the
database, not a rebuild.**

- **Two numbers, deliberately.** Reported frequency is one dictionary's claim and feeds
  the popup; Frequency rank is the reduced number and feeds `score()`. They are named
  apart in CONTEXT.md because one word for both is how this design rots.
- **Ranking strategy is a setting**: best rank, priority, or median. Median is the outlier
  defence and needs no tuning constant. A dictionary that lacks the word is not counted —
  silence usually means a smaller corpus, not a rarer word — and a word no enabled
  dictionary ranks keeps today's `DEFAULT_FREQ` fallback.
- **Reindex, not rebuild.** Strategy, order, and enabled state all change without touching
  an archive, so recomputing `term.freq` is a pure SQL pass over local rows: seconds, in
  one transaction, in place. The promoted database is already stamped
  `PRAGMA journal_mode = WAL` (precisely because it is read while being written), so
  readers see old values until commit; the daemon picks up new state through the existing
  `reload` verb. ADR-0005's copy-and-atomic-rename remains the rebuild path, for archive
  reads only.
- **Display is priority-first-wins, not the strategy.** The popup shows the highest-ranked
  enabled dictionary's own number, so the figure on screen is always something a real
  dictionary said — never a median no source claims. Two rules, each one line.

Consequences:

- The hot path is untouched: same column, same query, no join. The cost moves to a
  reindex the user pays on a settings change, not to every hover.
- `term.freq` is derived state, so it can be stale between a change and its reindex. It is
  recomputed on every strategy, order, and enable change, which is the complete set of
  inputs.

Rejected:

- **Apply the strategy at lookup time** — correct at all times, but adds a join or second
  query per candidate to the no-join hot path that exists on purpose.
- **Full rebuild on strategy/order/enable change** — one existing mechanism, but makes
  ticking a checkbox cost a multi-minute archive rebuild.
- **Frequency rank from the top-priority dictionary** — order would genuinely drive
  ranking, at the price of a stale-state concept in the UI and a rebuild prompt after
  every frequency drag.
- **Absent frequency counted as `DEFAULT_FREQ`** — adding one narrow specialist dictionary
  would make every ordinary word look rarer.
- **Display follows the strategy** — perfectly consistent, and shows users a computed
  number no dictionary reported.
- **Frequency section as display order only** — honest about the global merge, and
  delivers no priority at all.
