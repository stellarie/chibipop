# chibipop — agent operating manual

chibipop is a Japanese pop-up dictionary: a daemon reads screen text with OCR and
paints a popup. Structure and rules: `ARCHITECTURE.md`. Vocabulary: `CONTEXT.md`.

## Commands

Linux is the dev platform, and CI verifies Windows. Every link-producing
command must exclude one bin crate: both bin crates produce a binary literally
named `chibipop`, and cargo uplifts every bin to one target directory, so a
command spanning both races two linkers over one path.

```bash
cargo build --release -p chibipop-linux
cargo build --release -p chibipop-windows

# The test gates, as ci.yml and release.yml run them.
cargo test --workspace --exclude chibipop-windows   # Linux
cargo test --workspace --exclude chibipop-linux     # Windows

# The OCR quality gate. `test = false` keeps it out of the default sweep.
cargo test -p chibipop-linux --test ocr_gate -- --nocapture

# Required after any change under crates/chibipop-linux/src/ocr/.
cargo clippy -p chibipop-linux --test ocr_gate

scripts/package-linux.sh vX.Y.Z
packaging/aur/bump.sh vX.Y.Z

# docs/REGRESSION.md's tiers, by script.
python scripts/manual_regression.py --list
python scripts/manual_regression.py --tier 0 --repo-root . --repeat-tests 3
```

Clippy runs twice, workspace-wide. `--color never` is load-bearing: CI sets
`CARGO_TERM_COLOR=always`, and ANSI escapes break the count anchors.

```bash
# Pass 1: count lines starting with `warning`, minus cargo's "generated N
# warnings" summaries. Must equal exactly 1. A -D warnings here stops rmeta
# and unlints the dependent bin crate.
cargo clippy --workspace --color never --all-targets --all-features

# Pass 2: count lines starting with `error` or `warning`. Must be 0.
cargo clippy --workspace --color never --all-targets --all-features -- -D warnings \
  -A clippy::while_let_loop -A clippy::doc_lazy_continuation \
  -A clippy::useless_conversion -A clippy::too_many_arguments \
  -A clippy::needless_lifetimes -A clippy::type_complexity
```

## Testing

`docs/REGRESSION.md` holds three tiers.

- **Tier 0** — the automated gate, no screen. CI runs the sweep three times, to
  catch process-global races. The pass count is a floor, not an equality: 600
  Linux, 400 Windows.
- **Tier 1** — agent-verifiable, on real pixels. `docs/fixtures/ocr-corpus.html`
  publishes its own coordinates.
- **Tier 2** — mostly automatable. It drives the real pointer and hotkeys.

Geometry goldens verify layout under exact equality, never a tolerance. They
change only through a human-reviewed bless run: `workflow_dispatch` with
`bless=true` rewrites them into an artifact. See `ARCHITECTURE.md#verification`.

## Project structure

```
src/                     core library `chibipop`: behavior, no OS calls
crates/chibipop-linux/   Wayland bin, tray, OCR engine under src/ocr/
crates/chibipop-windows/ Win32 bin, DirectWrite measurement, geometry goldens
docs/                    REFERENCE, LINUX, REGRESSION, RELEASING, BACKLOG
```

`ARCHITECTURE.md` has the flow and rules, `CONTEXT.md` the terms. Do not
restate either.

## Code style

- **Never run a formatter.** See rule 1.
- Rules render as terse lists. Prose explains, a list decides.
- Doc comments carry rationale: long `//!` and `///` headers stating why a thing
  exists and what was rejected. A comment restating the signature is noise.
- Prose uses ASD-STE100 Simplified Technical English: one instruction per
  sentence, active voice, one term per thing. Code and tables are exempt.

## Git workflow

- Commit messages use Conventional Commits, scoped when it helps: `fix(ocr):`.
- Work lands in upstream `stellarie/chibipop` by pull request from
  `unusualcrow/chibipop`.
- Record a decision's rationale in the pull request description. This repository
  has no architecture-decision-record directory. Never create one.

## Boundaries

**Always**

- Read the governing `ARCHITECTURE.md` section before changing behavior.
- Keep core free of OS calls, and in physical pixels. Convert at the bin seam.
- Pin every dependency in `[workspace.dependencies]`. One pin serves the tree.
- Run the changed platform's gate, plus the OCR gate when OCR moved.

**Ask first**

- A new trait or seam. The tree has three traits and rejects hypothetical seams.
- A new dependency, an async runtime, or a thread pool. The daemon is all sync.
- A move of the clippy count or a test floor, a runner image pin, or a rename of
  a setting, a config key, or a socket verb.

**Never.** Each line is a silent failure. Anchors are `ARCHITECTURE.md` sections.

1. Never run `cargo fmt` or any formatter. The tree has never been rustfmt-clean,
   so a reformat yields an unreviewable diff. (*Code style*)
2. Never construct `FontSystem` with a default locale. It must be `"ja"`, or
   kanji silently render as Chinese glyph variants. (`#popup-and-measurement`)
3. Never unmap the popup to hide it. Hiding is a transparent buffer plus a cleared
   input region, or Hyprland animates it. (`#popup-and-measurement`)
4. Never frame-gate a show or a hide. Hidden surfaces get no frame callbacks. Every
   other commit is frame-gated. (`#popup-and-measurement`)
5. Never skip `cargo clippy -p chibipop-linux --test ocr_gate` after touching
   `crates/chibipop-linux/src/ocr/`. `test = false` hides it from `--all-targets`,
   so the gate greens on a broken target. (`#ocr-engine`)
6. Never add a first-run model download path. Models are committed, hash-pinned and
   verified twice. (`#ocr-engine`)
7. Never change the release asset naming shape. Every shipped binary's update
   checker parses those names, so it is a forever contract. (`#packaging-and-ci`)
8. Never let Reindex read an archive. Reindex is an in-place SQL pass over local
   rows, and reading an archive is a rebuild. (`#dictionary-and-lookup`)
9. Never widen a geometry golden to a tolerance, or let CI commit one. Goldens are
   exact-equality and human-blessed. (`#verification`)
10. Never delete a failing test to make a gate pass. The red test is the finding.
    (`#verification`)

Never commit a secret. Never hand-edit a vendored or generated directory: change
the generator.
