# Releasing chibipop

## What a release is

One zip, `chibipop-vX.Y.Z-windows-x64.zip`, containing:

```
chibipop-vX.Y.Z-windows-x64/
  chibipop.exe                 the application
  data/deconjugator.json       required at runtime
  README.md
```

**The dictionary database is deliberately not in it.** It is 232 MiB, and it
is built from dictionaries we have no right to redistribute — 大辞林 is
commercial. The user supplies their own Yomitan archives and runs the builder
once. That is the one step a downloaded binary cannot remove.

`chibipop.toml` is not shipped either: it is written with defaults beside the
executable on first run.

## Cutting one

1. **Decide the version.** Update `version` in `Cargo.toml`.
2. **Run tier 0 locally** — [`REGRESSION.md`](REGRESSION.md). Do not tag a
   commit you have not gated.

> [!warning] **v0.7.2 is deliberately never tagged. Do not tag it later.**
> Its headline feature was the rebuild-and-promote path — stage a
> `chibipop.sqlite.new`, stop and `join()` the worker, rename, respawn — and
> that path **deadlocks**: the worker's closure completed, `join()` never
> returned, and the main thread stopped pumping the messages its low-level
> input hooks are serviced from. Windows then serialises every mouse move and
> keystroke on the machine behind it. It froze the user's whole desktop twice.
> v0.7.2 is merged (`036e4fd`) and is the base v0.8.0 was built on, so its
> history is not going anywhere — but there is no build of it anyone should
> run, and a tag would invite one. **v0.8.0 deletes that path**, which is what
> makes the deadlock unreachable; see [`BACKLOG.md`](BACKLOG.md) §24.
>
> The release workflow would refuse a `v0.7.2` tag anyway now that `Cargo.toml`
> reads `0.8.0`, but that is a backstop, not the reason.
3. **Commit** the version bump.
4. **Tag and push:**
   ```bash
   git tag v0.1.0
   git push origin main --tags
   ```
5. The **Release** workflow builds, packages and creates a **draft** release.
6. **Open the draft**, download the zip, unzip it somewhere clean, and check
   `chibipop.exe run` starts. Then publish.

Step 6 is not ceremony. Nothing in CI can hover over Japanese text, so the
first real exercise of a release build is a human doing it.

## What CI enforces

`.github/workflows/ci.yml` runs [`REGRESSION.md`](REGRESSION.md) tier 0 on
every push to `main` and every pull request:

- `cargo test` **three times** — the wheel accumulator's process-global
  statics produced an intermittent red once that a single run missed.
- Clippy, asserting the accepted-error **count is exactly 3**. The repo
  carries three accepted errors on purpose, so `-D warnings` always exits
  non-zero; a fourth is the regression to catch, and an exit code cannot see
  the difference. (It was 4 until 2026-08-09, when the hot-reload branch
  deleted the loop that produced one of them. This number moves; when it
  does, `REGRESSION.md`'s tier 0 table and `ci.yml` move with it, in the
  same commit.)
- Clippy again with those suppressed, asserting **zero** other findings.
- `cargo build --release`.

The release workflow additionally refuses to build if the tag disagrees with
`Cargo.toml`.

### What CI cannot do

Everything in `REGRESSION.md` tiers 1 and 2 — anything needing real pixels or
a real hand on the mouse. A GitHub runner has no Japanese OCR recognizer and
no screen, so `tests/golden.rs` and `tests/ocr_fixture.rs` **self-skip** there
rather than fail. They still count as passing, which is what keeps the
test-count floor from tripping on a runner.

**That is a trade, not a free win, and it is worth naming.** A self-skip is an
early `return`, and a `#[test]` that returns is a pass — so the totals in
`REGRESSION.md` include tests that asserted nothing, in CI *and* in any local
tree without a built database. `golden_corpus` is the one that matters: it is
the only check of real deconjugation against a real dictionary, and it is
silently the one that never runs. See [`BACKLOG.md`](BACKLOG.md) §27.

That means a green CI badge means "the pure logic is sound and it compiles",
not "the popup works". Tier 2 is still a human, every time.

## If the icon or manifest changes

`assets/chibipop.res` is committed rather than compiled, so no build depends
on locating a Windows SDK. Regenerate it from **PowerShell** — MSYS2 mangles
`rc.exe`'s `/flag` arguments under git-bash:

```powershell
$rc = Get-ChildItem "C:\Program Files (x86)\Windows Kits\10\bin" -Recurse -Filter rc.exe |
      Where-Object { $_.FullName -like "*x64*" } | Select-Object -Last 1
Push-Location assets; & $rc.FullName /nologo /fo chibipop.res chibipop.rc; Pop-Location
```

## Open decisions

- **The package is self-contained.** `chibipop build-dict` is in the binary
  and the settings window drives it, so a download needs no toolchain at all.
  The Python builder was deleted on 2026-07-31, once the Rust one had been
  verified byte-identical on the real archives and a fresh install had been
  driven end to end.
