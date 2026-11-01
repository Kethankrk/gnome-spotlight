#!/bin/sh
# Lumen's per-user installer and uninstaller (ticket 16).
#
# Everything lands inside the user's own session directories — no root, no `sudo`, nothing in
# /usr, /etc or the system units:
#
#   $XDG_BIN_HOME/lumen-daemon                                     the engine binary
#   $XDG_BIN_HOME/lumen                                            the launcher CLI
#   $XDG_DATA_HOME/gnome-shell/extensions/lumen@kethan.dev/        the GNOME Shell extension
#   $XDG_CONFIG_HOME/systemd/user/lumen.service                    Type=notify + BusName unit
#   $XDG_DATA_HOME/dbus-1/services/org.gnome.Lumen.service         D-Bus activation (no systemd)
#   $XDG_CONFIG_HOME/autostart/lumen.desktop                       XDG-autostart fallback
#   $XDG_DATA_HOME/lumen/installed-version, latest-version         what is installed / what exists
#   $XDG_DATA_HOME/lumen/installed-files                           every path this script placed
#
# `--archive <tarball>` installs a downloaded release and *requires* a checksum (`--sha256` or
# `--sha256sums`): an archive whose checksum does not match the published one is refused before
# anything is touched. Running it again is the update path — it replaces the files, rewrites the
# version records, removes anything the previous version left behind, and restarts the service.
# `--uninstall` removes exactly what was installed (the manifest above is the list) and prunes the
# directories that become empty, leaving the user's clipboard history alone.
#
# POSIX sh, `set -eu`, no root, no bashisms.

set -eu

PROGRAM=${0##*/}
EXT_UUID='lumen@kethan.dev'
DAEMON_NAME='lumen-daemon'
CLI_NAME='lumen'
UNIT_NAME='lumen.service'
DBUS_NAME='org.gnome.Lumen.service'
AUTOSTART_NAME='lumen.desktop'

ARCHIVE=''
SHA256=''
SHA256SUMS=''
LATEST=''
CHECK=0
UNINSTALL=0
SYSTEMCTL_ENABLED=1
TMP=''
ROOT=''

usage() {
    cat <<'EOF'
Usage: install.sh --archive <TARBALL> [--sha256 <HEX> | --sha256sums <FILE>] [OPTIONS]
       install.sh [OPTIONS]          (run from an unpacked release directory)
       install.sh --uninstall [--no-systemctl]
       install.sh --check --latest <VERSION> [--no-systemctl]

Installs (or updates) Lumen for the current user. Nothing outside your home directory is touched
and no root privileges are needed or accepted.

Options:
  --archive <FILE>      release archive to install (.tar.gz, .tar.xz or .tar); without it the
                        directory this script sits in must be an unpacked release
  --sha256 <HEX>        the SHA-256 the archive must have
  --sha256sums <FILE>   file of "<hex>  <name>" lines (a release's SHA256SUMS); the line for the
                        archive's file name is the expected checksum
  --latest <VERSION>    record VERSION as the newest published release; with --check that is all
                        this does. The extension notifies the user when it is newer than the
                        installed version
  --check               do not install: just record --latest and report
  --uninstall           remove everything this script installed
  --no-systemctl        do not talk to systemd; the unit, D-Bus and autostart files are still
                        installed (systemd picks them up on the next login)
  -h, --help            print this help

Environment:
  HOME, XDG_BIN_HOME, XDG_DATA_HOME, XDG_CONFIG_HOME
                        where things go (defaults: ~/.local/bin, ~/.local/share, ~/.config)
  LUMEN_TEMPLATE_DIR    where the .in templates are (default: <archive>/templates)
  LUMEN_SYSTEMCTL       the systemctl to use (default: the one on PATH; empty means none)
EOF
}

die() {
    printf '%s: %s\n' "$PROGRAM" "$*" >&2
    exit 1
}

note() {
    printf '%s: %s\n' "$PROGRAM" "$*" >&2
}

cleanup() {
    if [ -n "$TMP" ] && [ -d "$TMP" ]; then
        rm -rf "$TMP"
    fi
}

# ---- paths -------------------------------------------------------------------------------------

# `$1` when it is an absolute path, `$HOME/$2` when it is empty, and a hard error when it is a
# relative path: the daemon's stores ignore a relative XDG variable (contract §12, §13), so the
# installer must not disagree with them about where the data directory is.
xdg_dir() {
    case "${1:-}" in
        /*) printf '%s\n' "$1" ;;
        '') printf '%s/%s\n' "$HOME" "$2" ;;
        *) die "\$$3 must be an absolute path, got \`$1\`" ;;
    esac
}

plan_layout() {
    [ -n "${HOME:-}" ] || die 'HOME is not set: there is no session to install into'

    BIN_DIR=$(xdg_dir "${XDG_BIN_HOME:-}" '.local/bin' XDG_BIN_HOME)
    DATA_DIR=$(xdg_dir "${XDG_DATA_HOME:-}" '.local/share' XDG_DATA_HOME)
    CONFIG_DIR=$(xdg_dir "${XDG_CONFIG_HOME:-}" '.config' XDG_CONFIG_HOME)

    EXTENSION_DIR=$DATA_DIR/gnome-shell/extensions/$EXT_UUID
    SCHEMA_DIR=$EXTENSION_DIR/schemas
    DAEMON=$BIN_DIR/$DAEMON_NAME
    CLI=$BIN_DIR/$CLI_NAME
    UNIT_FILE=$CONFIG_DIR/systemd/user/$UNIT_NAME
    DBUS_FILE=$DATA_DIR/dbus-1/services/$DBUS_NAME
    AUTOSTART_FILE=$CONFIG_DIR/autostart/$AUTOSTART_NAME
    LUMEN_DATA_DIR=$DATA_DIR/lumen
    MANIFEST=$LUMEN_DATA_DIR/installed-files
    INSTALLED_VERSION_FILE=$LUMEN_DATA_DIR/installed-version
    LATEST_VERSION_FILE=$LUMEN_DATA_DIR/latest-version
}

# ---- arguments ---------------------------------------------------------------------------------

parse_args() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --archive) need_value "$@" ; ARCHIVE=$2; shift 2 ;;
            --sha256) need_value "$@" ; SHA256=$2; shift 2 ;;
            --sha256sums) need_value "$@" ; SHA256SUMS=$2; shift 2 ;;
            --latest) need_value "$@" ; LATEST=$2; shift 2 ;;
            --check) CHECK=1; shift ;;
            --uninstall) UNINSTALL=1; shift ;;
            --no-systemctl) SYSTEMCTL_ENABLED=0; shift ;;
            -h|--help) usage; exit 0 ;;
            *) die "unknown option \`$1\` (try --help)" ;;
        esac
    done
}

need_value() {
    [ $# -ge 2 ] || die "$1 needs a value"
}

# ---- checksums ---------------------------------------------------------------------------------

# `sha256sum` is coreutils, `shasum` is perl's, `openssl` is everywhere; any of the three will do.
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    elif command -v openssl >/dev/null 2>&1; then
        openssl dgst -sha256 "$1" | sed -e 's/.*= *//'
    else
        die 'no SHA-256 tool found (need sha256sum, shasum or openssl)'
    fi
}

expected_sha256() {
    if [ -n "$SHA256" ]; then
        printf '%s\n' "$SHA256"
        return 0
    fi

    [ -n "$SHA256SUMS" ] || die "refusing to install an unverified archive: pass --sha256 <hex> or --sha256sums <file>"
    [ -f "$SHA256SUMS" ] || die "the checksum file $SHA256SUMS does not exist"

    base=$(basename "$ARCHIVE")
    # `sha256sum` writes "<hex>  <name>" and "<hex> *<name>" for binary mode; both name the file.
    awk -v name="$base" '{ sub(/^\*/, "", $2) } $2 == name { print $1; exit }' "$SHA256SUMS"
}

verify_archive() {
    [ -f "$ARCHIVE" ] || die "the archive $ARCHIVE does not exist"

    expected=$(expected_sha256)
    [ -n "$expected" ] || die "no checksum for $(basename "$ARCHIVE") in $SHA256SUMS"

    actual=$(sha256_of "$ARCHIVE")
    [ "$actual" = "$expected" ] ||
        die "checksum mismatch for $ARCHIVE: expected $expected, got $actual — refusing to install"
    note "verified $(basename "$ARCHIVE") against $expected"
}

# ---- templates ---------------------------------------------------------------------------------

# Backslash-escape what sed reads as special *in a replacement*: `&` (the whole match) and `\`.
sed_escape() {
    printf '%s' "$1" | sed -e 's/[&\\]/\\&/g'
}

template_dir() {
    if [ -n "${LUMEN_TEMPLATE_DIR:-}" ]; then
        [ -d "$LUMEN_TEMPLATE_DIR" ] || die "LUMEN_TEMPLATE_DIR=$LUMEN_TEMPLATE_DIR is not a directory"
        printf '%s\n' "$LUMEN_TEMPLATE_DIR"
        return 0
    fi

    [ -d "$ROOT/templates" ] || die "the archive has no templates/ directory (set LUMEN_TEMPLATE_DIR)"
    printf '%s\n' "$ROOT/templates"
}

# render <TEMPLATE> <DEST> [PLACEHOLDER=VALUE ...] — substitute, then refuse to install a file that
# still has an `@…@` in it, so a missing substitution is a loud failure rather than a broken unit.
render() {
    template=$1
    dest=$2
    shift 2

    work=$TMP/render
    cp "$template" "$work"
    for pair in "$@"; do
        key=${pair%%=*}
        value=${pair#*=}
        sed -e "s|@${key}@|$(sed_escape "$value")|g" "$work" >"$work.new"
        mv "$work.new" "$work"
    done

    if grep -q '@[A-Za-z_][A-Za-z_]*@' "$work"; then
        die "$(basename "$template") still has an unsubstituted placeholder"
    fi

    mkdir -p "$(dirname "$dest")"
    cp "$work" "$dest"
    rm -f "$work"
}

# ---- the manifest ------------------------------------------------------------------------------

# One line per path placed: "file <path>" or "dir <path>" (a directory installed wholesale, whose
# previous contents are removed on the next install). `--uninstall` reads exactly this list.
record() {
    printf '%s %s\n' "$1" "$2" >>"$MANIFEST.new"
}

# Anything the previous install placed that this one does not: an update must not leave the old
# version's files behind. Only what the old manifest lists may be removed.
remove_stale() {
    [ -f "$MANIFEST" ] || return 0

    while read -r kind path; do
        [ -n "${path:-}" ] || continue
        grep -qxF "$kind $path" "$MANIFEST.new" && continue
        case "$kind" in
            file) rm -f "$path" ;;
            dir) rm -rf "$path" ;;
        esac
    done <"$MANIFEST"
}

# ---- install -----------------------------------------------------------------------------------

ensure_tmp() {
    if [ -z "$TMP" ]; then
        TMP=$(mktemp -d)
        trap cleanup EXIT INT TERM
    fi
}

extract_archive() {
    ensure_tmp

    case "$ARCHIVE" in
        *.tar.gz|*.tgz) tar -xzf "$ARCHIVE" -C "$TMP" ;;
        *.tar.xz|*.txz) tar -xJf "$ARCHIVE" -C "$TMP" ;;
        *.tar) tar -xf "$ARCHIVE" -C "$TMP" ;;
        *) die "unsupported archive type: $ARCHIVE (use .tar.gz, .tar.xz or .tar)" ;;
    esac

    # A release archive holds one top-level directory; a flat archive is accepted too.
    if [ -f "$TMP/VERSION" ]; then
        ROOT=$TMP
        return 0
    fi

    ROOT=$(find "$TMP" -mindepth 1 -maxdepth 1 -type d | head -n 1)
    if [ -z "$ROOT" ] || [ ! -f "$ROOT/VERSION" ]; then
        die "$ARCHIVE is not a Lumen release archive"
    fi
}

# An unpacked release directory (or a checkout staged like one): the installer sits next to VERSION.
script_root() {
    root=$(cd "$(dirname "$0")" && pwd)
    if [ ! -f "$root/VERSION" ]; then
        die 'nothing to install: pass --archive <tarball> (try --help)'
    fi
    printf '%s\n' "$root"
}

read_archive_version() {
    [ -f "$ROOT/VERSION" ] || die "the archive has no VERSION file"
    version=$(awk 'NF { print $1; exit }' "$ROOT/VERSION")
    [ -n "$version" ] || die "the archive's VERSION file is empty"
    printf '%s\n' "$version"
}

install_release() {
    if [ -z "$ROOT" ]; then
        extract_archive
    fi

    version=$(read_archive_version)
    [ -f "$ROOT/bin/$DAEMON_NAME" ] || die "the release has no bin/$DAEMON_NAME"
    [ -f "$ROOT/bin/$CLI_NAME" ] || die "the release has no bin/$CLI_NAME"
    [ -f "$ROOT/extension/metadata.json" ] || die "the release has no extension/"

    templates=$(template_dir)
    ensure_tmp
    mkdir -p "$BIN_DIR" "$LUMEN_DATA_DIR"

    # Binaries.
    install -m 0755 "$ROOT/bin/$DAEMON_NAME" "$DAEMON"
    install -m 0755 "$ROOT/bin/$CLI_NAME" "$CLI"

    # The extension, wholesale: replacing the directory is what makes an update drop the files a
    # previous version added. The tests and the npm manifest are not part of a release.
    rm -rf "$EXTENSION_DIR"
    mkdir -p "$EXTENSION_DIR"
    cp -R "$ROOT/extension/." "$EXTENSION_DIR/"
    rm -rf "$EXTENSION_DIR/test" "$EXTENSION_DIR/package.json"

    if [ -d "$SCHEMA_DIR" ] && command -v glib-compile-schemas >/dev/null 2>&1; then
        glib-compile-schemas "$SCHEMA_DIR"
    else
        note 'glib-compile-schemas is missing: the extension has no compiled schema and will not load'
    fi

    # The three session files, rendered with this user's paths.
    render "$templates/lumen.service.in" "$UNIT_FILE" \
        "DAEMON=$DAEMON" "SCHEMA_DIR=$SCHEMA_DIR"
    render "$templates/org.gnome.Lumen.service.in" "$DBUS_FILE" \
        "DAEMON=$DAEMON"
    render "$templates/lumen.desktop.in" "$AUTOSTART_FILE" \
        "DAEMON=$DAEMON"

    # The manifest, then what the previous install left behind.
    : >"$MANIFEST.new"
    record file "$DAEMON"
    record file "$CLI"
    record dir "$EXTENSION_DIR"
    record file "$UNIT_FILE"
    record file "$DBUS_FILE"
    record file "$AUTOSTART_FILE"
    record file "$INSTALLED_VERSION_FILE"
    record file "$LATEST_VERSION_FILE"
    remove_stale
    mv "$MANIFEST.new" "$MANIFEST"

    # What is installed, and what the newest release is: the extension compares the two (contract
    # §16). `--latest` records a release newer than the archive being installed (an older archive
    # from a mirror, say); without it the archive is by definition the newest thing we know about.
    printf '%s\n' "$version" >"$INSTALLED_VERSION_FILE"
    printf '%s\n' "${LATEST:-$version}" >"$LATEST_VERSION_FILE"

    enable_service
    report "$version"

    if [ -n "$LATEST" ] && [ "$LATEST" != "$version" ]; then
        note "recorded $LATEST as the newest release; this install is $version"
    fi
}

# ---- systemd -----------------------------------------------------------------------------------

systemctl_bin() {
    # `LUMEN_SYSTEMCTL` overrides the binary, so a test (or an unusual session) can point it
    # somewhere harmless; setting it to the empty string means "there is no systemd here".
    if [ -n "${LUMEN_SYSTEMCTL+x}" ]; then
        if [ -z "$LUMEN_SYSTEMCTL" ]; then
            return 1
        fi
        printf '%s\n' "$LUMEN_SYSTEMCTL"
        return 0
    fi

    command -v systemctl
}

# Start the daemon now. The unit is `WantedBy=graphical-session.target`, so it comes up at login by
# itself; this is for the session the user is installing from. D-Bus activation would bring the
# daemon up on first use anyway, and the XDG-autostart entry covers a desktop with no systemd.
enable_service() {
    [ "$SYSTEMCTL_ENABLED" = 1 ] || return 0

    systemctl=$(systemctl_bin) || {
        note 'no systemctl: the daemon will start on demand over D-Bus, or at your next login'
        return 0
    }

    "$systemctl" --user daemon-reload || note 'could not reload the systemd user manager'
    if ! "$systemctl" --user enable --now "$UNIT_NAME"; then
        note "could not start $UNIT_NAME now; it will come up at your next login"
    fi
}

disable_service() {
    [ "$SYSTEMCTL_ENABLED" = 1 ] || return 0

    systemctl=$(systemctl_bin) || return 0

    "$systemctl" --user disable --now "$UNIT_NAME" >/dev/null 2>&1 || true
    "$systemctl" --user daemon-reload >/dev/null 2>&1 || true
}

# ---- uninstall ---------------------------------------------------------------------------------

# Take back the directories that this install emptied, shallowest-last. Only empty ones go: a
# directory the user already had, or one that still holds something (their clipboard history, say)
# is left alone.
prune_parents() {
    dir=$(dirname "$1")
    while [ "$dir" != "$HOME" ] && [ "$dir" != '/' ] && [ "$dir" != '.' ]; do
        rmdir "$dir" 2>/dev/null || break
        dir=$(dirname "$dir")
    done
}

uninstall() {
    disable_service

    removed=0
    if [ -f "$MANIFEST" ]; then
        while read -r kind path; do
            if [ -z "${path:-}" ]; then
                continue
            fi
            case "$kind" in
                file)
                    if [ -e "$path" ]; then
                        rm -f "$path"
                        removed=$((removed + 1))
                    fi
                    ;;
                dir)
                    if [ -d "$path" ]; then
                        rm -rf "$path"
                        removed=$((removed + 1))
                    fi
                    ;;
                *) continue ;;
            esac
        done <"$MANIFEST"

        rm -f "$MANIFEST"
        prune_parents "$MANIFEST"
    else
        note "no manifest at $MANIFEST: removing the paths Lumen installs, if they are there"
    fi

    # Belt and braces: the fixed set of paths, so a half-finished install still comes out clean.
    rm -f "$DAEMON" "$CLI" "$UNIT_FILE" "$DBUS_FILE" "$AUTOSTART_FILE"
    rm -f "$INSTALLED_VERSION_FILE" "$LATEST_VERSION_FILE"
    rm -rf "$EXTENSION_DIR"

    for path in "$UNIT_FILE" "$DBUS_FILE" "$AUTOSTART_FILE" "$INSTALLED_VERSION_FILE" \
        "$EXTENSION_DIR" "$DAEMON" "$CLI"; do
        prune_parents "$path"
    done

    printf 'uninstalled Lumen (%s paths removed)\n' "$removed"
    note 'your clipboard history and usage counts, if any, were left in place'
    note "a running shell keeps the extension until it restarts: gnome-extensions disable $EXT_UUID"
}

# ---- --check -----------------------------------------------------------------------------------

check_latest() {
    [ -n "$LATEST" ] || die '--check needs --latest <version>'
    mkdir -p "$LUMEN_DATA_DIR"
    printf '%s\n' "$LATEST" >"$LATEST_VERSION_FILE"

    installed='none'
    if [ -r "$INSTALLED_VERSION_FILE" ]; then
        installed=$(awk 'NF { print $1; exit }' "$INSTALLED_VERSION_FILE")
    fi

    printf 'latest release recorded: %s (installed: %s)\n' "$LATEST" "$installed"
    printf 'Lumen will say so the next time the shell extension starts\n'
}

# ---- reporting ---------------------------------------------------------------------------------

report() {
    printf 'installed Lumen %s\n' "$1"
    printf '  daemon     %s\n' "$DAEMON"
    printf '  cli        %s\n' "$CLI"
    printf '  extension  %s\n' "$EXTENSION_DIR"
    printf '  systemd    %s\n' "$UNIT_FILE"
    printf '  dbus       %s\n' "$DBUS_FILE"
    printf '  autostart  %s\n' "$AUTOSTART_FILE"

    case ":${PATH:-}:" in
        *":$BIN_DIR:"*) ;;
        *) note "$BIN_DIR is not in PATH: add it to run \`lumen --toggle\`" ;;
    esac

    printf '\nThe daemon starts with your session. Enable the shell extension once:\n'
    printf '  gnome-extensions enable %s\n' "$EXT_UUID"
}

# ---- main --------------------------------------------------------------------------------------

parse_args "$@"
plan_layout

if [ "$UNINSTALL" = 1 ]; then
    if [ -n "$ARCHIVE" ]; then
        die '--uninstall does not take --archive'
    fi
    uninstall
    exit 0
fi

# Running as root would scatter Lumen's files through /root and install nothing useful for the
# user's session, which is exactly what a per-user install must not do.
if [ "$(id -u)" = 0 ]; then
    die 'refusing to install as root: Lumen is installed per user'
fi

if [ -n "$ARCHIVE" ]; then
    verify_archive
fi

if [ "$CHECK" = 1 ]; then
    check_latest
    exit 0
fi

if [ -n "$ARCHIVE" ]; then
    install_release
else
    # An unpacked release (or a checkout staged like one): there is no download to verify.
    ROOT=$(script_root)
    install_release
fi
