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
rather than fail. They still count as passing, which is why the test-count
floor stays honest.

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
