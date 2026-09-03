#!/usr/bin/env bash
# Builds the Linux release asset, `chibipop-vX.Y.Z-linux-x64.tar.gz`.
#
# The release workflow calls this; so can you. Everything in the asset
# comes from this repo or from a binary you already built, and nothing is
# downloaded - the offline-first contract is a property of this script,
# not of the workflow wrapped around it. That is also why the script
# exists rather than a wall of `run:` steps: the only way to be sure the
# asset is right is to build it on a dev box and extract it.
#
#   scripts/package-linux.sh v0.8.2                    # target/release/chibipop
#   scripts/package-linux.sh v0.8.2 path/to/chibipop   # or a named binary
#
# Env: OUT - staging root, default `dist`.
set -euo pipefail

version=${1-}
binary=${2:-target/release/chibipop}
out=${OUT:-dist}

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

die() {
	echo "package-linux: $*" >&2
	exit 1
}

# The asset name is a forever contract (ARCHITECTURE.md#packaging-and-ci):
# every shipped binary's update check parses
# `chibipop-v<semver>-linux-x64.tar.gz` off releases/latest, so a malformed
# version here misleads every install that ever sees this release, not just
# this one.
[[ $version =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] ||
	die "version must look like v1.2.3, got '$version'"

[[ -f $binary ]] || die "no binary at $binary - cargo build --release --workspace --exclude chibipop-windows first"
[[ -x $binary ]] || die "$binary is not executable"

# Both bin crates deliberately produce a binary named `chibipop` and cargo
# uplifts both to target/<profile>/chibipop (see the workspace Cargo.toml),
# so "packaged the foreign crate's refusal stub" is a real way to ship a
# broken release. An ELF magic check costs nothing and catches it.
magic=$(od -An -N4 -tx1 -- "$binary" | tr -d ' \n')
[[ $magic == 7f454c46 ]] || die "$binary is not an ELF binary (magic $magic)"

name="chibipop-$version-linux-x64"
stage="$out/$name"
rm -rf -- "$stage"
mkdir -p -- "$stage/data" "$stage/data/ipadic" "$stage/models/meiki" "$stage/extras"

# The Windows zip's shape, mirrored: the binary, the deconjugator table it
# needs at runtime, README, LICENSE. The dictionary database is not here
# and never will be - 232 MiB built from archives that are not ours to
# redistribute (docs/RELEASING.md).
install -m755 -- "$binary" "$stage/chibipop"
install -m644 -- "$repo/data/deconjugator.json" "$stage/data/"
install -m644 -- "$repo/README.md" "$repo/LICENSE" "$stage/"
# The offline-first runtime needs the decoded IPADIC model. Keep its license
# files beside the model. The script checks the staged files with the same
# hash check as the meiki model before it creates the tarball.
ipadic=$repo/data/ipadic
install -m644 -- "$ipadic/system.dic" "$ipadic/COPYING" "$ipadic/NOTICE" \
	"$ipadic/SHA256SUMS.txt" "$stage/data/ipadic/"
(cd -- "$stage/data/ipadic" && sha256sum --check --strict -- SHA256SUMS.txt)


# `models/meiki` beside the binary is the first layout `models::locate()`
# searches (crates/chibipop-linux/src/ocr/models.rs). It is not a
# convention this script is free to change:
# crates/chibipop-linux/tests/tarball_layout.rs extracts what we produce
# here and asserts the binary's own search resolves inside it.
#
# LICENSE.md rides along because the weights are LGPL-3.0 and shipping
# them without it would be a licence violation.
models=$repo/crates/chibipop-linux/models/meiki
install -m644 -- "$models"/*.onnx "$models/LICENSE.md" "$models/SHA256SUMS.txt" "$stage/models/meiki/"

# Build-time hash verification (ARCHITECTURE.md#ocr-engine: bundled,
# hash-pinned, no first-run download). Deliberately run over the *staged*
# copies - the exact bytes the tarball carries - and not over the source
# tree: a truncated install or a bad disk then fails the release instead of
# shipping a binary that refuses its own models on the user's machine.
# `models.rs` re-checks these same digests at runtime, and a unit test
# asserts this file and those constants agree.
(cd -- "$stage/models/meiki" && sha256sum --check --strict -- SHA256SUMS.txt)

# Named one by one rather than globbed: `extras/` gaining a file should be
# a deliberate packaging decision, and losing one must fail the build.
for f in README.md chibipop.desktop chibipop.service hyprland.conf; do
	install -m644 -- "$repo/extras/$f" "$stage/extras/$f"
done

# gzip over zstd deliberately: boring, universal `tar xzf`.
#
# Reproducible by construction - sorted names, no owner, one timestamp for
# every entry, and `gzip -n` drops gzip's own mtime header too. Repackaging
# the same inputs twice gives the same bytes, so "is this the asset that
# workflow built" is a `sha256sum`, not a leap of faith.
#
# The timestamp is the commit's, not the epoch: reproducible either way,
# but files dated 1970 in a freshly extracted download look broken. A tree
# with no git (a source tarball of a source tarball) falls back to 0.
epoch=${SOURCE_DATE_EPOCH:-$(git -C "$repo" log -1 --format=%ct 2>/dev/null || echo 0)}
tarball=$out/$name.tar.gz
tar --format=gnu --sort=name --owner=0 --group=0 --numeric-owner --mtime="@${epoch:-0}" \
	-C "$out" -cf - -- "$name" | gzip -9n >"$tarball"

find "$stage" -type f | sort
ls -l -- "$tarball"
sha256sum -- "$tarball"
