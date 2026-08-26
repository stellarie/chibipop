# Releasing chibipop

## What a release is

Two assets, one per platform.

```
chibipop-vX.Y.Z-windows-x64.zip
chibipop-vX.Y.Z-linux-x64.tar.gz
```

**Both names are a forever contract.** Every shipped binary's update check
parses asset names off `releases/latest` (ADR-0007), so the `chibipop-v`
prefix and the `-windows-x64.zip` / `-linux-x64.tar.gz` suffixes cannot
change shape — not even to look tidier. `scripts/package-linux.sh` refuses a
version that would not produce a parseable name.

The zip contains:

```
chibipop-vX.Y.Z-windows-x64/
  chibipop.exe                 the application
  data/deconjugator.json       required at runtime
  README.md
  LICENSE
  plugins/meikiocr/plugin.toml
  plugins/meikiocr/adapter.py
  plugins/meikiocr/config.toml
```

**The dictionary database is deliberately not in it.** It is 232 MiB, and it
is built from dictionaries we have no right to redistribute — 大辞林 is
commercial. The user supplies their own Yomitan archives and runs the builder
once. That is the one step a downloaded binary cannot remove.

`chibipop.toml` is not shipped either: it is written with defaults beside the
executable on first run.

**`plugins/meikiocr/` ships enabled by discovery.** It is the reference
text-provider plugin, per spec section 10. Discovery adds it to the in-memory
enabled list when its manifest parses successfully. `config.toml` holds the
machine-specific meikiocr install path, the HF cache directory, and the ONNX
thread cap — the user edits that file, never `adapter.py` or `plugin.toml`.

### The Linux tarball

Same shape, plus what a Wayland install needs (ADR-0007). One `tar xzf` is
the whole installation:

```
chibipop-vX.Y.Z-linux-x64/
  chibipop                     the application
  data/deconjugator.json       required at runtime
  README.md
  LICENSE
  models/meiki/                the OCR engine's three ONNX models, ~46 MiB
    meiki.text.detect.v0.1.960x544.onnx
    meiki.text.rec.v0.960x32.onnx
    meiki.text.rec.v0.vertical.32x480.onnx
    SHA256SUMS.txt
    LICENSE.md                 the weights are LGPL-3.0; shipping them
                               without this would be a licence violation
  extras/                      session integration for setups the settings
    README.md                  window's Autostart checkbox does not cover
    chibipop.desktop
    chibipop.service
    hyprland.conf
```

`models/meiki` sits beside the binary because that is the first place
`models::locate()` looks, and the layout is not folklore:
`crates/chibipop-linux/tests/tarball_layout.rs` builds the real asset,
extracts it, and asks the binary's own search where the models are. The
digests are verified twice — once by `scripts/package-linux.sh` as it stages
them (a mismatch fails the release) and once by the binary when the OCR
engine opens (a mismatch refuses the engine rather than recognising with
something else).

**The tarball works with no network and no first-run download.** There is no
`libonnxruntime.so` in it and no rpath: on linux-x64 `ort`'s pinned
`download-binaries` prebuilt is a *static* archive, so ONNX Runtime is inside
`chibipop` (which is why the binary is ~62 MB). ADR-0009 was written
expecting a shared library beside the binary; see its 2026-08-26 addendum.
`ldd` on the shipped binary names the entire floor:

```
libstdc++.so.6  libgcc_s.so.1  libm.so.6  libc.so.6
```

so a user needs glibc, libstdc++ and a Japanese font, and nothing else.

**Runner floor: `ubuntu-24.04`, deliberately not `ubuntu-latest`.** The
Wayland/tray/OCR stack is pure Rust over a statically linked ONNX Runtime, so
the image's glibc and libstdc++ are the only floor this binary imposes on a
user's distro — build on the newest image and every image migration silently
drops older distros.

The pin is not simply "the oldest image GitHub hosts", because that image
cannot build this crate. `ubuntu-22.04` is still hosted and still schedules;
it was tried, on this workflow, and the link fails: `ort`'s pinned prebuilt
ONNX Runtime is a static archive compiled against a newer toolchain, so it
needs `__isoc23_strtoull` and friends (glibc 2.38 — 22.04 has 2.35) and
`_M_replace_cold` (libstdc++ from GCC 13 — 22.04 has 12). **The prebuilt sets
the floor, not the runner catalogue**, and `ubuntu-24.04` is the oldest hosted
image that satisfies it. Moving the pin forward when 24.04 retires is a
deliberate bump of the supported-distro floor, made in a commit that says so.
See ADR-0007's 2026-08-26 addendum.

The release run prints the real numbers rather than a distro name — the
highest versioned symbol the binary references from each library — so
"does chibipop run on my machine" has an answer that does not require
guessing:

```
objdump -T target/release/chibipop | grep -oE 'GLIBC_2\.[0-9]+'    | sort -uV | tail -1
objdump -T target/release/chibipop | grep -oE 'GLIBCXX_3\.4\.[0-9]+' | sort -uV | tail -1
```

(`ci.yml`'s Linux *gate* is pinned separately and for a different reason — it
only has to compile and test, not define anyone's floor.)

The tarball is reproducible: sorted entries, no owners, one timestamp (the
commit's), `gzip -n`. Repackaging the same commit twice gives the same bytes,
so "is this the asset the workflow built" is a `sha256sum`.

### The AUR packages

Two, templated in [`packaging/aur/`](../packaging/aur) and pushed by hand
(ADR-0007). An Arch user installs and updates through pacman and never sees
a tarball:

| package | builds from | ONNX Runtime |
|---|---|---|
| `chibipop-bin` | the release tarball above | inside the binary — nothing to install |
| `chibipop` | the release **tag** | the distro's `onnxruntime`, dlopened |

`chibipop-bin` compiles nothing: it repacks the asset. The one flag that
makes the source package a *distro* package is `--no-default-features
--features system-onnxruntime` — the default feature downloads a pinned
ONNX Runtime prebuilt mid-build and links it statically, which a distro
package must not do, and `system-onnxruntime` switches `ort` to dlopening
`libonnxruntime.so` instead (ADR-0009). Nothing else about the build
differs, and the models are committed, so neither package downloads
anything but its own source.

Both install one layout, and it is not free-form: `/usr/bin/chibipop`
resolves its data by walking up from its own directory
(`models::LAYOUTS`, and the daemon's rules search one directory over).

```
/usr/bin/chibipop
/usr/share/chibipop/data/deconjugator.json           required at runtime
/usr/share/chibipop/models/meiki/                    the three ONNX models
/usr/share/applications/chibipop.desktop
/usr/lib/systemd/user/chibipop.service               a *user* unit
/usr/share/icons/hicolor/scalable/apps/chibipop.svg  the .desktop entry's Icon=
/usr/share/doc/chibipop/{README.md,extras/}          snippets, not config
/usr/share/licenses/<pkgname>/{LICENSE,LICENSE.models.md}
```

Move either of the first two and the package still installs, then refuses
to OCR or stops resolving conjugated forms — so
`crates/chibipop-linux/tests/aur_packaging.rs` runs each PKGBUILD's own
`package()` over a stub `$srcdir` and asks the binary's search where the
models are in the `$pkgdir` that comes out. It needs no `makepkg`
(`package()` is a shell function over `install`), so it runs on a CI runner
like any other test.

A pacman install is never portable mode: nothing writes a `chibipop.toml`
beside `/usr/bin/chibipop`, so config, data and state land in the XDG
directories (ADR-0006).

**Bumping.** One command rewrites both templates for a tag — pkgver, pkgrel,
every checksum, and both `.SRCINFO`s:

```bash
packaging/aur/bump.sh v0.9.0
```

It downloads exactly the sources the PKGBUILDs declare *after* the version
rewrite, so a URL a new tag breaks fails there instead of on a user's
machine. `--local FILE` supplies one source from disk instead (matched by the
name the PKGBUILD gives it), which is how the packages are rehearsed before a
release exists; `--only NAME` does one package; `--no-srcinfo` skips the
`.SRCINFO` regeneration for a box with no `makepkg`. Then review the diff and
push each package by hand — the script prints the three commands.

**Rehearsing in a clean container.** No tag and no release asset needed:
build the tarball locally, point a copy of the PKGBUILD at it, and let
`makepkg -s` resolve the declared dependencies for real.

```bash
docker run --rm --cap-add SYS_NICE -v /tmp/aur:/host:ro archlinux:base-devel bash -c '
  pacman -Syu --noconfirm --needed sudo namcap sway
  useradd -m builder && echo "builder ALL=(ALL) NOPASSWD: ALL" >/etc/sudoers.d/builder
  cp -r /host/pkg /home/builder/pkg && chown -R builder /home/builder/pkg
  su - builder -c "cd ~/pkg && makepkg -s --noconfirm" && namcap /home/builder/pkg/*.pkg.tar.zst
  pacman -U --noconfirm /home/builder/pkg/*.pkg.tar.zst && chibipop --version'
```

`--cap-add SYS_NICE` is only for the last step worth doing: Arch's `sway`
carries `cap_sys_nice=ep` and a container without that capability cannot
`execve` it at all. With it, `WLR_BACKENDS=headless WLR_RENDERER=pixman sway`
gives the *installed* binary a real compositor, and `chibipop run` must log
`worker: pipeline up` — which is the OCR engine resolving
`/usr/share/chibipop/models/meiki` and digesting all three models, and on the
source package also the system `libonnxruntime.so` being found.

`namcap` must report **no errors**. The warnings it does report are these,
and all of them are accepted:

- `ELF file is unstripped` (`chibipop-bin` only) — deliberate, exactly as the
  tarball ships it. The source package takes makepkg's default and is
  stripped.
- `Unused shared library ld-linux-x86-64.so.2` — the loader.
- `Dependency libgcc / libstdc++ detected and implicitly satisfied`, and
  `Dependency included, but may not be needed ('gcc-libs')` — naming
  `gcc-libs` for a `libgcc_s`/`libstdc++` user is Arch convention, and
  namcap's heuristic disagrees with it, not with us.
- `Dependency included, but may not be needed ('onnxruntime')` (source
  package only) — it is dlopened, so nothing namcap can read links to it.
  Dropping it would ship a package whose OCR dies on first use.

An error namcap *did* report and that was fixed rather than accepted:
`Dependency hicolor-icon-theme detected and not included`. Both packages
install an icon into that theme's directories, so both depend on it.

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
> The release workflow would refuse a `v0.7.2` tag anyway — `Cargo.toml`
> is past that version, and the workflow checks the match.
3. **Commit** the version bump.
4. **Tag and push:**
   ```bash
   git tag v0.1.0
   git push origin main --tags
   ```
5. The **Release** workflow builds both assets and creates one **draft**
   release carrying both. The tag/`Cargo.toml` check runs first, on its own
   tiny job, so a mismatched tag costs no build runners; the draft is created
   last, from artifacts, so a release never exists with only half its assets.
6. **Open the draft, unpack both assets somewhere clean, and run them.**
   - Windows: unzip, `chibipop.exe run` starts.
   - Linux: `tar xzf`, then `./chibipop probe` and `./chibipop run` — the log
     must say `worker: pipeline up`, which is the OCR engine finding and
     digesting `models/meiki` beside the binary. Then publish.
7. **Bump the AUR packages** once the release is published — the source URLs
   point at it:
   ```bash
   packaging/aur/bump.sh vX.Y.Z
   ```
   Build both in a clean container (recipe above), then push each by hand.
   `chibipop-bin` is the one an Arch user gets in seconds; the source package
   is the one that works on a distro whose glibc the tarball is too new for.
8. **Refresh the latest-build copy** so `Documents\chibipop-latest` runs the
   version you just shipped:
   ```powershell
   pwsh -File scripts/blank-copy.ps1
   ```
   It swaps the executable and keeps any config, library and database that
   folder holds. See [`REFERENCE.md`](REFERENCE.md#the-latest-build-copy).

Step 6 is not ceremony. Nothing in CI can hover over Japanese text, so the
first real exercise of a release build is a human doing it.

**Rehearsing the packaging without a tag.** A tag is permanent and a draft is
awkward to unpublish, so the Release workflow also takes a
`workflow_dispatch` on any branch: it runs the same jobs and the same script,
uploads both assets as workflow artifacts, and creates nothing. Locally, the
Linux half is one command — no workflow needed:

```bash
cargo build --release --workspace --exclude chibipop-windows
scripts/package-linux.sh v0.8.2        # -> dist/chibipop-v0.8.2-linux-x64.tar.gz
```

## What CI enforces

`.github/workflows/ci.yml` runs [`REGRESSION.md`](REGRESSION.md) tier 0 on
every push to `main` and every pull request, plus a mirrored Linux gate on
ubuntu (ticket 29; both bins are named `chibipop`, so link-producing steps
exclude the foreign bin crate — see the workspace `Cargo.toml`):

- `cargo test --workspace --exclude chibipop-linux` **three times** — the
  wheel accumulator's process-global
  statics produced an intermittent red once that a single run missed.
- Clippy, asserting the accepted-error **count is exactly 1**. The repo
  carries one accepted error on purpose, so `-D warnings` always exits
  non-zero; a second is the regression to catch, and an exit code cannot see
  the difference. (It was 4 until 2026-08-09, when the hot-reload branch
  deleted the loop that produced one of them, 3 until 2026-08-25, when the
  layout walk moved into core, and 2 until 2026-08-26, when the upstream
  v0.9.x merge rewrote `deconj.rs` past its `useless_conversion` — that
  merge's own three new `src/worker.rs` findings were refactored away, not
  accepted. This number moves; when it does, `REGRESSION.md`'s tier 0 table
  and `ci.yml` move with it, in the same commit.)
- Clippy again with those suppressed, asserting **zero** other findings.
- `cargo build --release --workspace --exclude chibipop-linux`.

`.github/workflows/release.yml` runs a subset of the same gates on the tag
itself — tests on both platforms, plus ADR-0009's OCR quality gate on the
Linux job — and refuses to build at all if the tag disagrees with
`Cargo.toml`. It is not a second CI: pushing a tag on a commit CI has not
already gated is the mistake it cannot save you from.

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
