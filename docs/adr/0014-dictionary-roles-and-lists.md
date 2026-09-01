# Dictionaries have roles, and one ordered list per role

`dictionaries.display_order` held **name substrings**, matched against installed names at
present time (`present::dict_order_rank`: substring match, first wins, no match sorts
last), and presence in that list doubled as the enable switch (`restrict_to_order`,
guarded against empty and all-typo lists because a typo could blank the popup). Meanwhile
`library::Kind` gave each archive exactly one of `Term | Frequency | Unreadable`, decided
by whether its filename contains `Freq` or its `index.json` sets `frequencyMode`. Neither
model survives drag-and-drop: you cannot drag a pattern, and an archive carrying a term
bank plus frequency data has one row but two jobs.

**Decision: a Dictionary is identified by its exact name and holds a *set* of roles read
from its banks; each role has its own ordered list with its own enabled state.**

- **Exact names, not substrings.** One row is one Dictionary, so a drag moves one thing.
  Substring configs are auto-migrated once by resolving each substring against installed
  names. A renamed archive loses its place — accepted, because the alternative (opaque
  IDs in TOML) is the encoding ADR-0012 rejects.
- **Roles come from banks, not filenames.** `is_frequency_archive`'s filename heuristic
  cannot say what an archive contains, only what it is called; pitch archives fell through
  it into `Kind::Term` entirely. Detection inspects the banks, and yields a set: a mixed
  archive appears in every list it has data for.
- **One list per role, order and enabled state per row.** Six flat arrays under
  `[dictionaries]` — `terms`/`terms_disabled`, `frequency`/`frequency_disabled`,
  `pitch`/`pitch_disabled` — position is priority, membership of the `_disabled` twin is
  the checkbox. Enabled is per *role*, not per Dictionary: unchecking a mixed archive's
  definitions must not silently kill its frequency data, and a checkbox may only affect
  the section it sits in.
- **`per_language` stays term-only.** It is the one order users genuinely vary by OCR
  language; extending it across roles would model a mostly-empty grid and put a language
  selector above the whole dictionary area.
- **`restrict_to_order` and its guards are deleted.** They defended against a typo'd
  substring matching nothing and hiding every dictionary. With exact names and an explicit
  per-role checkbox, that bug class cannot occur, and the reasoning has no business in the
  presentation path.
- **New imports land at the bottom of each of their role lists, enabled.** Installing a
  dictionary never reorders the lists you curated, and never silently does nothing.

Rejected:

- **Keep substrings, drag rewrites them** — two dictionaries matching one substring means
  dragging one moves the other.
- **One global order, sections as visual grouping** — dragging a row "above" one in
  another section would mean nothing.
- **User-assigned sections** — a list could then contain dictionaries contributing nothing
  to it; roles are a fact about the archive, not a preference.
- **One `Kind` per dictionary, detected properly** — still gives a mixed archive one home
  section, with its frequency data ordered by a position set in the terms list.
- **One global `disabled` array** — enabled state would cross sections, so a checkbox in
  Terms would visibly flip one in Frequency.
- **Sentinel-prefixed names (`"!Jitendex"`) to mark disabled entries** — exactly the
  mini-language ADR-0012 rejects.
- **Keeping the two-list searched/excluded UI per section** — three sections would mean
  six list boxes, and drag-to-reorder would compete with drag-between-lists for one
  gesture.
