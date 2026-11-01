#!/bin/sh
# Lumen's release builder (ticket 16).
#
# Builds the daemon for each target a release ships, stages it together with the extension, the CLI
# and the session-file templates, and writes the archives plus the SHA256SUMS file that
# `packaging/install.sh` verifies an archive against before it installs anything:
#
#   dist/install.sh                    the installer, for people who prefer to read it first
#   dist/lumen-<version>-x86_64.tar.gz
#   dist/lumen-<version>-aarch64.tar.gz
#   dist/SHA256SUMS                    `<hex>  <file>` for both archives
#
# Targets (default: both, in this order):
#   x86_64-unknown-linux-gnu    x86-64 desktops, and the host of a normal CI runner
#   aarch64-unknown-linux-gnu   ARM64 desktops and laptops
#
# Cross-building aarch64 from an x86-64 host needs the Rust target and a C cross-linker, and CI is
# where that happens:
#
#   rustup target add aarch64-unknown-linux-gnu
#   sudo dnf install gcc-aarch64-linux-gnu        # or: apt install gcc-aarch64-linux-gnu
#   CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
#       packaging/release.sh
#
# This script does not fake a second architecture: without that toolchain the aarch64 build fails
# loudly and no archive is written for it.
#
# The daemon is linked against the host's C library rather than being a static musl build:
# research §5.3 found that a fully static glib/GIO deployment is a partially supported
# configuration nobody could validate, and the daemon links no C library of its own — the
# LocalSearch library is `dlopen`ed at run time (contract §11) — so the dynamic build is what a
# release ships.
#
# POSIX sh, `set -eu`.

set -eu

PROGRAM=${0##*/}
REPO=$(cd "$(dirname "$0")/.." && pwd)
INSTALLER=$REPO/packaging/install.sh
TEMPLATES=$REPO/packaging/templates
DEFAULT_TARGETS='x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu'

# `cargo` lives in the user's profile on the build host (see the harness notes).
if ! command -v cargo >/dev/null 2>&1 && [ -f "${HOME:-}/.cargo/env" ]; then
    # shellcheck source=/dev/null
    . "$HOME/.cargo/env"
fi

VERSION=''
OUT=$REPO/dist
BUILD=1
DRY_RUN=0
TARGETS=''
TMP=''

usage() {
    cat <<'EOF'
Usage: release.sh [OPTIONS]

Builds the release archives and their SHA256SUMS. Run it from a checkout on a build host; for a
second architecture, see the target notes at the top of this script.

Options:
  --version <VERSION>   version to label the archives with [default: the workspace version]
  --out <DIR>           where the archives go [default: dist/]
  --target <TRIPLE>     build this target (repeatable); default: x86_64 and aarch64 GNU/Linux
  --no-build            do not run cargo; stage the binaries already in target/<TRIPLE>/release/
  --dry-run             print what would be built and where, then stop
  -h, --help            print this help

The archives hold a single top-level directory (lumen-<version>-<arch>/) with VERSION, bin/, the
extension and templates/ — exactly what install.sh expects.
EOF
}

die() {
    printf '%s: %s\n' "$PROGRAM" "$*" >&2
    exit 1
}

note() {
    printf '%s: %s\n' "$PROGRAM" "$*" >&2
}

need_value() {
    [ $# -ge 2 ] || die "$1 needs a value"
}

parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --version) need_value "$@" ; VERSION=$2; shift 2 ;;
            --out) need_value "$@" ; OUT=$2; shift 2 ;;
            --target) need_value "$@" ; TARGETS="$TARGETS ${2}"; shift 2 ;;
            --no-build) BUILD=0; shift ;;
            --dry-run) DRY_RUN=1; shift ;;
            -h|--help) usage; exit 0 ;;
            *) die "unknown option \`$1\` (try --help)" ;;
        esac
    done
}

workspace_version() {
    awk '
        {
            if ($0 ~ /^\[/) {
                inside = ($0 == "[workspace.package]")
            } else if (inside && $0 ~ /^version[ \t]*=/) {
                gsub(/^version[ \t]*=[ \t]*"/, "")
                gsub(/".*/, "")
                print
                exit
            }
        }' "$REPO/Cargo.toml"
}

# One target's turn: build it, lay out `lumen-<version>-<arch>/`, tar it, checksum it.
build_target() {
    target=$1
    arch=${target%%-*}
    binary=$REPO/target/$target/release/lumen-daemon

    if [ "$BUILD" = 1 ]; then
        (cd "$REPO" && cargo build --release --target "$target")
    fi
    [ -f "$binary" ] || die "no daemon binary at $binary — build that target first"

    stage=$TMP/lumen-$VERSION-$arch
    mkdir -p "$stage/bin" "$stage/extension" "$stage/templates"

    printf '%s\n' "$VERSION" >"$stage/VERSION"
    install -m 0755 "$binary" "$stage/bin/lumen-daemon"
    install -m 0755 "$REPO/cli/lumen" "$stage/bin/lumen"

    # The extension as a release ships it: no tests, no npm manifest.
    cp -R "$REPO/extension/." "$stage/extension/"
    rm -rf "$stage/extension/test" "$stage/extension/package.json"

    # The session-file templates the installer renders with the installing user's paths.
    cp "$TEMPLATES/lumen.service.in" "$TEMPLATES/org.gnome.Lumen.service.in" \
        "$TEMPLATES/lumen.desktop.in" "$stage/templates/"

    (cd "$TMP" && tar czf "$OUT/lumen-$VERSION-$arch.tar.gz" "lumen-$VERSION-$arch")
    (cd "$OUT" && sha256sum "lumen-$VERSION-$arch.tar.gz" >>"$OUT/SHA256SUMS")

    printf '  %s\n' "$OUT/lumen-$VERSION-$arch.tar.gz"
}

main() {
    parse_args "$@"

    TARGETS=${TARGETS:-$DEFAULT_TARGETS}
    if [ -z "$VERSION" ]; then
        VERSION=$(workspace_version)
    fi
    [ -n "$VERSION" ] || die 'cannot work out the version: pass --version <VERSION>'

    if [ "$DRY_RUN" = 1 ]; then
        printf 'would build Lumen %s into %s:\n' "$VERSION" "$OUT"
        for target in $TARGETS; do
            printf '  %s -> %s/lumen-%s-%s.tar.gz\n' "$target" "$OUT" "$VERSION" "${target%%-*}"
            if [ "$BUILD" = 1 ]; then
                printf '      cargo build --release --target %s\n' "$target"
            else
                printf '      (--no-build) stage target/%s/release/lumen-daemon\n' "$target"
            fi
        done
        printf '  plus install.sh and SHA256SUMS\n'
        return 0
    fi

    command -v sha256sum >/dev/null 2>&1 || die 'sha256sum is needed to publish checksums'

    mkdir -p "$OUT"
    TMP=$(mktemp -d)
    trap 'rm -rf "$TMP"' EXIT INT TERM

    cp "$INSTALLER" "$OUT/install.sh"
    chmod 0755 "$OUT/install.sh"
    # The checksum file is written from scratch: a release is exactly these archives.
    printf '' >"$OUT/SHA256SUMS"

    printf 'building Lumen %s into %s\n' "$VERSION" "$OUT"
    for target in $TARGETS; do
        build_target "$target"
    done

    printf '\nrelease files:\n'
    ls -1 "$OUT/install.sh" "$OUT/SHA256SUMS" "$OUT"/lumen-*.tar.gz
    printf '\ninstall one with:\n  sh %s/install.sh --archive <archive> --sha256sums %s/SHA256SUMS\n' \
        "$OUT" "$OUT"
}

main "$@"
