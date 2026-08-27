# Linux packaging, CI matrix, and the check-only updater

How the Linux binary is built, shipped, and kept current — Windows release/update
behavior unchanged.

**Release asset: `chibipop-vX.Y.Z-linux-x64.tar.gz`, mirror plus extras.** Same layout
as the Windows zip (`chibipop` binary, `data/deconjugator.json`, README, LICENSE) plus
`extras/`: `chibipop.desktop`, `chibipop.service` (systemd user unit,
`WantedBy=graphical-session.target`), and the Hyprland exec snippet — the ADR-0006
documented snippets ship in the tarball, not just the README. Asset naming is a
**forever contract**: every shipped binary's update check parses `releases/latest` asset
names, so the `chibipop-v` prefix / `-linux-x64.tar.gz` suffix must never change shape.
gzip over zstd deliberately: boring, universal `tar xzf`.

**Linkage: `x86_64-unknown-linux-gnu`, built on the oldest supported GitHub ubuntu
runner** (rolling policy — currently ubuntu-22.04, glibc 2.35; **amended
2026-08-26: ubuntu-24.04, see the addendum below**). The Wayland/tray stack
is pure Rust, so glibc is the only floor; building on the oldest image keeps older
distros viable without cross tooling. musl static rejected: it forecloses the `ort`/ONNX
option the OCR ticket is still weighing, and carries known allocator/DNS costs. If the
OCR decision lands on a native runtime (`libonnxruntime.so`), its bundling joins this
asset — owned by the OCR quality-bar ticket.

**AUR: both `chibipop-bin` (repacks the tarball) and `chibipop` (builds from the release
tag), manually pushed, templated in-repo.** PKGBUILDs live under `packaging/aur/`; a
`bump.sh vX.Y.Z` script rewrites pkgver + checksums + .SRCINFO locally, and the dev
eyeballs then pushes to AUR by hand — matching the existing draft-release-then-eyeball
posture. Fully-automated CI push rejected (unattended key + template failure ships
broken packages); AUR-only PKGBUILDs rejected (drift).

**CI: two native jobs, mirrored gates, no cross-compilation.** Windows tier0 stays
as-is, scoped to core + `chibipop-windows`; a Linux job on the release runner image
builds core + `chibipop-linux` with the same structure — tests ×3, clippy exact-count
baseline (per-job: the accepted-error count is per-platform after the workspace split),
release build, dev artifact upload. Release workflow gains a Linux job attaching the
tarball to the same draft release. **Gate = unit tests that never require a
compositor** (integration tests skip when `WAYLAND_DISPLAY` is unset); a headless-sway
smoke job (`WLR_BACKENDS=headless`) runs the real layer-shell/screencopy surface but is
non-blocking until it proves stable in CI. The Windows golden-layout gate is ADR-0004's
verification ticket's, not this one's.

**Self-update on Linux: check-only, button-only.** The settings "Check for updates"
button compares versions against `releases/latest` and reports "vX.Y.Z available —
update via your package manager or download from GitHub"; it never writes. The
`.new`/`.old` exe swap stays Windows-only: a pacman-owned `/usr/bin/chibipop` must never
be self-modified, and maintaining a writable-dir-only swap path for tarball users buys
little once AUR exists. No startup phone-home — the check runs only on click, exact
Windows parity.

## The prebuilt ONNX Runtime sets the runner floor, not the catalogue (amended 2026-08-26)

"Oldest supported ubuntu runner" was written assuming the only thing stopping
us going older was GitHub's image catalogue. It is not. The packaging work
pinned the release job to `ubuntu-22.04` as this ADR says, dispatched it, and
the job **scheduled and then failed to link**:

```
rust-lld: error: undefined symbol: __isoc23_strtoull      (glibc 2.38; 22.04 has 2.35)
rust-lld: error: undefined symbol: ..._M_replace_cold     (libstdc++ from GCC 13; 22.04 has 12)
```

— every one of them from `libort_sys*.rlib`, i.e. from `ort`'s pinned prebuilt
ONNX Runtime, which is a static archive compiled against a newer toolchain
than 22.04 ships (ADR-0009's 2026-08-26 addendum: on linux-x64 that prebuilt
is static, not a shared object).

So the rolling policy stands but its floor is bounded from below by the
prebuilt:

- **The release job is `ubuntu-24.04`** — the oldest hosted image `ort` can be
  linked on, not simply the oldest hosted image. Evidence:
  https://github.com/unusualcrow/chibipop/actions/runs/32933219451
- **`ubuntu-22.04` has not left the runner fleet.** It schedules today; it
  just cannot build this crate. Any comment in the tree claiming otherwise is
  wrong about the reason even where it lands on the right image.
- **Moving the pin is a distro-floor decision**, and the release run now
  prints the numbers that decision produces: the highest `GLIBC_2.x` and
  `GLIBCXX_3.4.x` symbol versions the shipped binary references. Those, not a
  distro name, are what a user's machine has to satisfy.
- **The way to go lower is to change the linkage, not the image**: the
  `system-onnxruntime` feature (the source-AUR path) has no prebuilt and no
  such floor. That is a packaging choice for someone who wants older distros,
  and it is already built.
