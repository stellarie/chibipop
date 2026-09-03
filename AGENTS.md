# chibipop — agent operating manual

chibipop is a Japanese popup dictionary. A daemon reads screen text with OCR
and paints a popup. `ARCHITECTURE.md` describes the structure and the rules.
`CONTEXT.md` defines the vocabulary.

## Commands

Linux is the development platform. CI verifies Windows. Every link command
must exclude one bin crate. Both bin crates produce a binary named `chibipop`.
Cargo puts every binary into one target directory. Therefore, a command that
spans both crates causes two linkers to race for one file path.

```bash
cargo build --release -p chibipop-linux
cargo build --release -p chibipop-windows

# The test gates, as ci.yml and release.yml run them.
cargo test --workspace --exclude chibipop-windows   # Linux
cargo test --workspace --exclude chibipop-linux     # Windows

# The OCR quality gate. `test = false` keeps it out of the default sweep.
cargo test -p chibipop-linux --test ocr_gate -- --nocapture

# Japanese analysis unit tests.
cargo test -p chibipop analysis::

# Required after any change under crates/chibipop-linux/src/ocr/.
cargo clippy -p chibipop-linux --test ocr_gate

scripts/package-linux.sh vX.Y.Z
packaging/aur/bump.sh vX.Y.Z

# docs/REGRESSION.md's tiers, by script.
python scripts/manual_regression.py --list
python scripts/manual_regression.py --tier 0 --repo-root . --repeat-tests 3
```

Clippy runs two times across the workspace. You must use `--color never`.
CI sets `CARGO_TERM_COLOR=always`. ANSI escape sequences break the count
anchors if you omit this flag.

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

`docs/REGRESSION.md` defines three tiers.

- **Tier 0** — the automated gate without a screen. CI runs the sweep three
  times to find process-global races. The pass count is a minimum limit, not an
  exact value: 600 for Linux, 400 for Windows.
- **Tier 1** — agent-verifiable on real pixels. `docs/fixtures/ocr-corpus.html`
  publishes its own coordinates.
- **Tier 2** — mostly automatable. It drives the real pointer and hotkeys.

Committed geometry-snapshot goldens verify layout under exact equality. A test
allows no tolerance. Goldens change only through a human-reviewed bless run. A run
of `workflow_dispatch` with `bless=true` rewrites the goldens into an artifact. See
`ARCHITECTURE.md#verification`.

## Project structure

```
src/                     core library `chibipop`: behavior, no OS calls
src/analysis/            Japanese analysis service and model checks
src/select/              Card selection and gesture state
crates/chibipop-linux/   Wayland bin, tray, OCR engine under src/ocr/
crates/chibipop-windows/ Win32 bin, DirectWrite measurement, geometry goldens
docs/                    REFERENCE, LINUX, REGRESSION, RELEASING, BACKLOG
```

`ARCHITECTURE.md` contains the control flow and the rules. `CONTEXT.md`
defines the terms. Do not restate these files.

## Code style

- **Never run a formatter.** See rule 1.
- Render rules as terse lists. Prose explains, but a list decides.
- Doc comments must give the rationale. Use long `//!` and `///` headers that
  state why a component exists and what was rejected. A comment that restates a
  signature is noise.
- Write prose in ASD-STE100 Simplified Technical English: one instruction per
  sentence, active voice, and one term for each thing. Code and tables are
  exempt from this rule.

## Git workflow

- Write commit messages with Conventional Commits. Use a scope when a scope
  helps: `fix(ocr):`.
- Land work in upstream `stellarie/chibipop` with a pull request from
  `unusualcrow/chibipop`.
- Record the rationale for a decision in the pull request description. This
  repository has no directory for architecture decision records. Never create
  this directory.

## Boundaries

**Always**

- Read the governing section of `ARCHITECTURE.md` before you change behavior.
- Keep the core library free of OS calls, and keep it in physical pixels. Convert
  pixels at the bin seam.
- Pin every dependency in `[workspace.dependencies]`. One pin serves the tree.
- Run the gate of the changed platform. Run the OCR gate when the OCR engine
  changes.

**Ask first**

- A new trait or seam. The tree has three traits and rejects hypothetical seams.
- A new dependency, an async runtime, or a thread pool. The daemon is all sync.
- A change to the clippy count or a test floor. A change to a runner image pin.
  A rename of a setting, a config key, or a socket verb.

**Never.** Each line is a silent failure. Anchors are `ARCHITECTURE.md` sections.

1. Never run `cargo fmt` or any formatter. The tree has never been
   rustfmt-clean. A reformat creates a diff that you cannot review. (*Code style*)
2. Never construct `FontSystem` with a default locale. The locale must be
   `"ja"`. If you use another locale, kanji render as Chinese glyph variants
   without an error. (`#popup-and-measurement`)
3. Never unmap the popup to hide it. To hide the popup, use a transparent buffer
   and clear the input region. If you unmap the popup, Hyprland animates it.
   (`#popup-and-measurement`)
4. Never frame-gate a show or a hide command. Hidden surfaces receive no frame
   callbacks. Every other commit is frame-gated. (`#popup-and-measurement`)
5. Never skip `cargo clippy -p chibipop-linux --test ocr_gate` after you change
   `crates/chibipop-linux/src/ocr/`. The setting `test = false` hides this test
   from `--all-targets`. Therefore, the gate passes on a broken target.
   (`#ocr-engine`)
6. Never add a code path that downloads a model on the first run. Models are
   committed, pinned by hash, and verified two times. (`#ocr-engine`)
7. Never change the format of release asset names. The update checker in every
   shipped binary parses these names. Therefore, this format is a permanent
   contract. (`#packaging-and-ci`)
8. Never let Reindex read an archive. Reindex is an in-place SQL pass over local
   rows. An operation that reads an archive is a rebuild.
   (`#dictionary-and-lookup`)
9. Never widen a geometry-snapshot golden to a tolerance. Never let CI commit a
   golden. Goldens use exact equality, and a human must bless them.
   (`#verification`)
10. Never delete a failing test to make a gate pass. The failing test is the
    finding. (`#verification`)

11. Never add a fourth `TextMeasure` method that paints. `TextMeasure` has exactly
    three methods: `measure`, `caret_boxes`, and `hit_offset`. (`#popup-and-measurement`)
12. Never let a bin decide a gesture. `select::gesture::Gesture` owns gesture state.
    (`#selection`)
13. Never let `RequestAnalysis` analyze a Card other than the top Card.
    (`#japanese-analysis`)
14. Never change the IPADIC model without updating both digest pins and the license files.
    The pins are `analysis::MODEL_SHA256` and `SHA256SUMS.txt`. The license files are
    `COPYING` and `NOTICE`. (`#japanese-analysis`)
15. Never build `Check` elements or highlights when `SceneRequest::selection` is `None`.
    (`#selection`)

Never commit a secret. Never edit a vendored or generated directory by hand.
You must change the generator instead.
