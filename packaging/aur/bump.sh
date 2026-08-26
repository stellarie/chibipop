#!/usr/bin/env bash
# Points the AUR templates at a new tag: pkgver, pkgrel, every checksum,
# and both .SRCINFOs.
#
# The templates next to this script are the only copy (ADR-0007: no
# AUR-only PKGBUILDs, they drift), and the push to AUR stays manual - so
# this script's whole job is the part a human should not be doing by hand,
# which is hashing. It downloads exactly the sources the PKGBUILD declares,
# after the version rewrite, so a URL that a new tag breaks fails here
# instead of on a user's machine.
#
#   packaging/aur/bump.sh v0.9.0
#   packaging/aur/bump.sh v0.9.0 --only chibipop-bin
#   packaging/aur/bump.sh v0.9.0 --local dist/chibipop-v0.9.0-linux-x64.tar.gz
#
# `--local FILE` supplies one source from disk instead of the network,
# matched by the file name the PKGBUILD gives it. That is how the packages
# are rehearsed before a release exists: build the asset with
# `scripts/package-linux.sh`, hand it to this script, and the PKGBUILD is
# byte-exact about what it was tested against.
#
# Options:
#   --only NAME     just that package directory (repeatable)
#   --local FILE    hash FILE for the source whose name matches its own
#   --no-srcinfo    skip .SRCINFO (no makepkg on this box; leaves them stale)
set -euo pipefail

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)

die() {
	printf 'bump.sh: %s\n' "$1" >&2
	exit 1
}

version=
only=()
locals=()
srcinfo=1

while (($#)); do
	case $1 in
	--only)
		only+=("${2-}")
		shift 2
		;;
	--local)
		[[ -f ${2-} ]] || die "--local needs an existing file, got '${2-}'"
		locals+=("$(cd -- "$(dirname -- "$2")" && pwd)/$(basename -- "$2")")
		shift 2
		;;
	--no-srcinfo)
		srcinfo=0
		shift
		;;
	-*) die "unknown option $1" ;;
	*)
		[[ -z $version ]] || die "one version, got '$version' and '$1'"
		version=$1
		shift
		;;
	esac
done

# The same shape `scripts/package-linux.sh` enforces, and for the same
# reason: `chibipop-bin`'s source URL is the release asset name, which is a
# forever contract (ADR-0007). A version this regex rejects would produce a
# PKGBUILD pointing at an asset the release never published.
[[ $version =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]] ||
	die "version must look like v1.2.3, got '${version:-<none>}'"
pkgver=${version#v}

((${#only[@]})) || only=(chibipop-bin chibipop)

# One line, exactly one line: a template where `pkgver=` appears twice (or
# not at all) is one this script must not guess about.
set_one() {
	local file=$1 key=$2 value=$3 hits
	hits=$(grep -c "^$key=" -- "$file" || true)
	[[ $hits == 1 ]] || die "$file has $hits '$key=' lines, expected 1"
	sed -i "s|^$key=.*|$key=$value|" -- "$file"
}

# The file name a source entry lands under in $srcdir: makepkg's `name::url`
# form when it is given, else the last path element of the URL.
source_name() {
	local entry=$1
	if [[ $entry == *::* ]]; then
		printf '%s' "${entry%%::*}"
	else
		printf '%s' "${entry##*/}"
	fi
}

sha_of() {
	sha256sum -- "$1" | cut -d' ' -f1
}

# Every --local file, by base name.
local_for() {
	local want=$1 path
	for path in ${locals[@]+"${locals[@]}"}; do
		[[ $(basename -- "$path") == "$want" ]] && printf '%s' "$path" && return 0
	done
	return 1
}

cache=$(mktemp -d)
trap 'rm -rf -- "$cache"' EXIT

for pkg in "${only[@]}"; do
	dir=$here/$pkg
	build=$dir/PKGBUILD
	[[ -f $build ]] || die "no PKGBUILD at $build"

	set_one "$build" pkgver "$pkgver"
	# A new version always starts at 1; a packaging-only fix bumps it by
	# hand afterwards, which is the one edit to these files that is not
	# this script's.
	set_one "$build" pkgrel 1

	# Ask the PKGBUILD itself what its sources are, after the rewrite -
	# they are shell expansions of $pkgver, and re-deriving them here
	# would be a second copy of the same URLs.
	mapfile -t sources < <(bash -c "source '$build'; printf '%s\n' \"\${source[@]}\"")
	((${#sources[@]})) || die "$pkg declares no sources"

	sums=()
	for entry in "${sources[@]}"; do
		name=$(source_name "$entry")
		url=${entry#*::}
		if path=$(local_for "$name"); then
			printf '%s: %s <- %s\n' "$pkg" "$name" "$path"
		else
			printf '%s: %s <- %s\n' "$pkg" "$name" "$url"
			path=$cache/$name
			curl --fail --location --silent --show-error --retry 3 \
				--output "$path" -- "$url" ||
				die "could not download $url"
		fi
		sum=$(sha_of "$path")
		[[ $sum =~ ^[0-9a-f]{64}$ ]] || die "bad digest '$sum' for $name"
		sums+=("$sum")
	done

	# Replace the whole `sha256sums=(...)` array, however many lines it
	# spans, keeping makepkg's continuation indent.
	start=$(grep -n '^sha256sums=(' -- "$build" | cut -d: -f1)
	[[ -n $start ]] || die "$build has no sha256sums=( line"
	end=$(awk -v s="$start" 'NR>=s && /\)[[:space:]]*$/ {print NR; exit}' "$build")
	[[ -n $end ]] || die "$build has an unterminated sha256sums array"
	# How many checksums the template declares, asked of the template
	# rather than counted out of its text: a source added without a
	# matching `sha256sums` slot must fail loudly, because makepkg would
	# otherwise skip verifying the extra source.
	mapfile -t declared < <(bash -c "source '$build'; printf '%s\n' \"\${sha256sums[@]}\"")
	((${#declared[@]} == ${#sums[@]})) ||
		die "$build declares ${#declared[@]} checksums for ${#sums[@]} sources"

	block=$cache/$pkg.sums
	: >"$block"
	for i in "${!sums[@]}"; do
		if ((i == 0)); then
			printf "sha256sums=('%s'" "${sums[i]}" >>"$block"
		else
			printf "\n            '%s'" "${sums[i]}" >>"$block"
		fi
	done
	printf ')\n' >>"$block"

	spliced=$cache/$pkg.PKGBUILD
	sed -n "1,$((start - 1))p" -- "$build" >"$spliced"
	cat -- "$block" >>"$spliced"
	sed -n "$((end + 1)),\$p" -- "$build" >>"$spliced"
	cat -- "$spliced" >"$build"

	if ((srcinfo)); then
		command -v makepkg >/dev/null ||
			die "makepkg is not on this box; re-run with --no-srcinfo and regenerate .SRCINFO on Arch"
		(cd -- "$dir" && makepkg --printsrcinfo >.SRCINFO)
	fi

	printf '%s: pkgver=%s pkgrel=1\n' "$pkg" "$pkgver"
done

cat <<EOF

Review the diff, then push each package to AUR by hand:

    git clone ssh://aur@aur.archlinux.org/<pkgname>.git aur-<pkgname>
    cp packaging/aur/<pkgname>/{PKGBUILD,.SRCINFO} aur-<pkgname>/
    cd aur-<pkgname> && git commit -am '$version' && git push

Build both first (docs/RELEASING.md): makepkg in a clean chroot, install
the package, and run the binary.
EOF
