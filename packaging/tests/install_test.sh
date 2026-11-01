#!/bin/sh
# Scripted test for packaging/install.sh (ticket 16): the installer and the uninstaller are run for
# real, into throw-away `HOME`/`XDG_*` trees, against stub release archives this script builds.
#
# What it pins down:
#   * an install lands every file where the session looks for it, with the right contents;
#   * a bad (or absent) checksum is refused and installs nothing at all;
#   * running it again is the update path — files, version records and stale leftovers included;
#   * `--uninstall` removes exactly what it placed and leaves no session files behind;
#   * `--uninstall` does not destroy the user's clipboard history;
#   * `--check` records the newest published version for the extension's update notification;
#   * the CLI reports the installed version and fails cleanly when there is no engine.
#
# Runnable directly (`sh packaging/tests/install_test.sh`) and from the Makefile
# (`make test-packaging`), which is part of `make test`. No root, no session bus, no systemd: every
# run is confined to its own scratch directory and the installer is told not to touch systemd.

set -u

REPO=$(cd "$(dirname "$0")/../.." && pwd)
INSTALLER=$REPO/packaging/install.sh

SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

PASSED=0
FAILED=0

pass() {
    PASSED=$((PASSED + 1))
    printf 'ok   %s\n' "$1"
}

fail() {
    FAILED=$((FAILED + 1))
    printf 'FAIL %s\n' "$1"
    if [ $# -gt 1 ]; then
        printf '     %s\n' "$2"
    fi
}

expect_eq() { # expect_eq <what> <actual> <expected>
    if [ "$2" = "$3" ]; then
        pass "$1"
    else
        fail "$1" "expected [$3], got [$2]"
    fi
}

expect_file() { # expect_file <what> <path>
    if [ -f "$2" ]; then
        pass "$1"
    else
        fail "$1" "no file at $2"
    fi
}

expect_gone() { # expect_gone <what> <path>
    if [ ! -e "$2" ]; then
        pass "$1"
    else
        fail "$1" "$2 is still there"
    fi
}

expect_contains() { # expect_contains <what> <path> <text>
    if grep -qF -- "$3" "$2" 2>/dev/null; then
        pass "$1"
    else
        fail "$1" "$2 does not contain [$3]"
    fi
}

expect_executable() { # expect_executable <what> <path>
    if [ -x "$2" ]; then
        pass "$1"
    else
        fail "$1" "$2 is not executable"
    fi
}

# Run the installer against a throw-away home. `LUMEN_SYSTEMCTL=` (empty) means "there is no systemd
# here", so no run of this test can talk to a developer's own user manager; `--no-systemctl` says
# the same thing to the script under test.
run_installer() { # run_installer <home> [args...]
    home=$1
    shift
    HOME=$home \
        XDG_DATA_HOME=$home/.local/share \
        XDG_CONFIG_HOME=$home/.config \
        XDG_BIN_HOME=$home/.local/bin \
        LUMEN_SYSTEMCTL= \
        sh "$INSTALLER" --no-systemctl "$@"
}

new_home() { # new_home <name> -> prints the home directory
    home=$SCRATCH/$1/home
    mkdir -p "$home"
    printf '%s\n' "$home"
}

# A stub release archive: the real layout (VERSION, bin/, extension/, templates/), a daemon that
# is a shell script, and the real CLI and extension. Stub archives keep this test independent of a
# Rust build, and they exercise the same installer code path a real archive does.
make_release() { # make_release <version>
    version=$1
    stem=lumen-$version-x86_64
    mkdir -p "$SCRATCH/stage/$stem/bin" "$SCRATCH/stage/$stem/extension"

    printf '%s\n' "$version" >"$SCRATCH/stage/$stem/VERSION"
    printf '#!/bin/sh\necho "lumen-daemon stub %s"\n' "$version" >"$SCRATCH/stage/$stem/bin/lumen-daemon"
    cp "$REPO/cli/lumen" "$SCRATCH/stage/$stem/bin/lumen"
    cp -R "$REPO/extension/." "$SCRATCH/stage/$stem/extension/"
    cp -R "$REPO/packaging/templates" "$SCRATCH/stage/$stem/templates"

    (cd "$SCRATCH/stage" && tar czf "$SCRATCH/$stem.tar.gz" "$stem")
    (cd "$SCRATCH" && sha256sum "$stem.tar.gz" >>"$SCRATCH/SHA256SUMS")
}

# Where an install puts things, for one of the throw-away homes above.
EXT_DIR_OF() { printf '%s/.local/share/gnome-shell/extensions/lumen@kethan.dev\n' "$1"; }
UNIT_OF() { printf '%s/.config/systemd/user/lumen.service\n' "$1"; }
DBUS_OF() { printf '%s/.local/share/dbus-1/services/org.gnome.Lumen.service\n' "$1"; }
AUTOSTART_OF() { printf '%s/.config/autostart/lumen.desktop\n' "$1"; }
VERSION_OF() { printf '%s/.local/share/lumen/installed-version\n' "$1"; }
LATEST_OF() { printf '%s/.local/share/lumen/latest-version\n' "$1"; }
MANIFEST_OF() { printf '%s/.local/share/lumen/installed-files\n' "$1"; }

# ---------------------------------------------------------------------------------------------
printf '\n== the installer places every session file ==\n'

make_release 0.1.0
HOME1=$(new_home install)
run_installer "$HOME1" --archive "$SCRATCH/lumen-0.1.0-x86_64.tar.gz" \
    --sha256sums "$SCRATCH/SHA256SUMS" >"$SCRATCH/install.out" 2>&1
expect_eq 'the installer exits 0' "$?" '0'
expect_contains 'it says what it installed' "$SCRATCH/install.out" 'installed Lumen 0.1.0'
expect_contains 'it says it verified the archive' "$SCRATCH/install.out" 'verified lumen-0.1.0-x86_64.tar.gz'

expect_executable 'the daemon is installed and executable' "$HOME1/.local/bin/lumen-daemon"
expect_executable 'the CLI is installed and executable' "$HOME1/.local/bin/lumen"
expect_file 'the extension is installed' "$(EXT_DIR_OF "$HOME1")/metadata.json"
expect_file 'the extension keeps its lib/' "$(EXT_DIR_OF "$HOME1")/lib/update.js"
expect_file 'the shared schema is compiled' "$(EXT_DIR_OF "$HOME1")/schemas/gschemas.compiled"
expect_gone 'the extension tests are not installed' "$(EXT_DIR_OF "$HOME1")/test"
expect_gone 'the npm manifest is not installed' "$(EXT_DIR_OF "$HOME1")/package.json"
expect_eq 'the installed version is recorded' "$(cat "$(VERSION_OF "$HOME1")")" '0.1.0'
expect_eq 'the latest version starts as the installed one' "$(cat "$(LATEST_OF "$HOME1")")" '0.1.0'
expect_file 'the manifest lists the installed paths' "$(MANIFEST_OF "$HOME1")"
expect_contains 'the manifest names the daemon' "$(MANIFEST_OF "$HOME1")" \
    "file $HOME1/.local/bin/lumen-daemon"

expect_contains 'the unit is Type=notify' "$(UNIT_OF "$HOME1")" 'Type=notify'
expect_contains 'the unit owns org.gnome.Lumen' "$(UNIT_OF "$HOME1")" 'BusName=org.gnome.Lumen'
expect_contains 'the unit runs the installed daemon' "$(UNIT_OF "$HOME1")" \
    "ExecStart=$HOME1/.local/bin/lumen-daemon --indexer localsearch"
expect_contains 'the unit points the daemon at the compiled schema' "$(UNIT_OF "$HOME1")" \
    "GSETTINGS_SCHEMA_DIR=$(EXT_DIR_OF "$HOME1")/schemas"
expect_contains 'the unit joins the graphical session' "$(UNIT_OF "$HOME1")" \
    'PartOf=graphical-session.target'

expect_contains 'the D-Bus file activates the engine name' "$(DBUS_OF "$HOME1")" 'Name=org.gnome.Lumen'
expect_contains 'the D-Bus file runs the installed daemon' "$(DBUS_OF "$HOME1")" \
    "Exec=$HOME1/.local/bin/lumen-daemon --indexer localsearch"
expect_contains 'the D-Bus file hands over to the user unit' "$(DBUS_OF "$HOME1")" \
    'SystemdService=lumen.service'

expect_contains 'the autostart entry is enabled' "$(AUTOSTART_OF "$HOME1")" \
    'X-GNOME-Autostart-enabled=true'
expect_contains 'the autostart entry stands down for systemd' "$(AUTOSTART_OF "$HOME1")" \
    'X-systemd-skip=true'
expect_contains 'the autostart entry is skipped without the daemon' "$(AUTOSTART_OF "$HOME1")" \
    "TryExec=$HOME1/.local/bin/lumen-daemon"

# ---------------------------------------------------------------------------------------------
printf '\n== an unverified archive is refused ==\n'

HOME2=$(new_home badsum)
run_installer "$HOME2" --archive "$SCRATCH/lumen-0.1.0-x86_64.tar.gz" \
    --sha256 0000000000000000000000000000000000000000000000000000000000000000 \
    >"$SCRATCH/badsum.out" 2>&1
expect_eq 'a mismatching checksum is a failure' "$?" '1'
expect_contains 'and it says why' "$SCRATCH/badsum.out" 'checksum mismatch'
expect_contains 'and that it refused' "$SCRATCH/badsum.out" 'refusing to install'
expect_gone 'no daemon was installed' "$HOME2/.local/bin/lumen-daemon"
expect_gone 'no extension was installed' "$(EXT_DIR_OF "$HOME2")"

HOME3=$(new_home nosum)
run_installer "$HOME3" --archive "$SCRATCH/lumen-0.1.0-x86_64.tar.gz" >"$SCRATCH/nosum.out" 2>&1
expect_eq 'an archive with no checksum at all is a failure too' "$?" '1'
expect_contains 'and it asks for one' "$SCRATCH/nosum.out" 'refusing to install an unverified archive'
expect_gone 'nothing was installed either' "$HOME3/.local/bin/lumen"

# ---------------------------------------------------------------------------------------------
printf '\n== running it again updates in place ==\n'

marker=$(EXT_DIR_OF "$HOME1")/stale-from-0.1.0.js
: >"$marker"
printf 'file %s\n' "$HOME1/.local/bin/lumen-old-helper" >>"$(MANIFEST_OF "$HOME1")"
: >"$HOME1/.local/bin/lumen-old-helper"

make_release 0.2.0
run_installer "$HOME1" --archive "$SCRATCH/lumen-0.2.0-x86_64.tar.gz" \
    --sha256sums "$SCRATCH/SHA256SUMS" >"$SCRATCH/update.out" 2>&1
expect_eq 'the update installs cleanly' "$?" '0'
expect_eq 'the installed version moved on' "$(cat "$(VERSION_OF "$HOME1")")" '0.2.0'
expect_eq 'the latest version moved on with it' "$(cat "$(LATEST_OF "$HOME1")")" '0.2.0'
expect_contains 'the daemon binary was replaced' "$HOME1/.local/bin/lumen-daemon" 'lumen-daemon stub 0.2.0'
expect_gone 'files the old version left behind are removed' "$HOME1/.local/bin/lumen-old-helper"
expect_gone 'a stale extension file is gone with the replaced directory' "$marker"
expect_file 'the extension is still there' "$(EXT_DIR_OF "$HOME1")/metadata.json"
expect_eq 'the CLI reports the new version' \
    "$(HOME=$HOME1 XDG_DATA_HOME=$HOME1/.local/share "$HOME1/.local/bin/lumen" --version)" 'lumen 0.2.0'

# ---------------------------------------------------------------------------------------------
printf '\n== --check records the newest published release ==\n'

run_installer "$HOME1" --check --latest 0.3.0 >"$SCRATCH/check.out" 2>&1
expect_eq '--check exits 0' "$?" '0'
expect_eq 'the latest version file now names the newer release' "$(cat "$(LATEST_OF "$HOME1")")" '0.3.0'
expect_eq 'and the installed version is untouched' "$(cat "$(VERSION_OF "$HOME1")")" '0.2.0'
expect_contains '--check reports both versions' "$SCRATCH/check.out" 'latest release recorded: 0.3.0 (installed: 0.2.0)'

# ---------------------------------------------------------------------------------------------
printf '\n== the CLI reports and fails cleanly ==\n'

HOME=$HOME1 XDG_DATA_HOME=$HOME1/.local/share "$HOME1/.local/bin/lumen" --version \
    >"$SCRATCH/cli.out" 2>&1
expect_eq 'lumen --version exits 0' "$?" '0'
expect_eq 'and names the installed version' "$(cat "$SCRATCH/cli.out")" 'lumen 0.2.0'

HOME=$HOME1 "$HOME1/.local/bin/lumen" --help >"$SCRATCH/help.out" 2>&1
expect_eq 'lumen --help exits 0' "$?" '0'
expect_contains 'and documents --toggle' "$SCRATCH/help.out" '--toggle'

HOME=$HOME1 DBUS_SESSION_BUS_ADDRESS='unix:path=/nonexistent-lumen-bus' \
    "$HOME1/.local/bin/lumen" --toggle >"$SCRATCH/toggle.out" 2>&1
expect_eq 'lumen --toggle without an engine is a clean failure' "$?" '1'
expect_contains 'with a message a user can act on' "$SCRATCH/toggle.out" 'could not reach the Lumen engine'

# ---------------------------------------------------------------------------------------------
printf '\n== --uninstall removes the session files and keeps the user data ==\n'

# A stand-in for the encrypted clipboard history the daemon writes next to the version records:
# uninstalling must not take the user's data with it.
printf 'user data\n' >"$HOME1/.local/share/lumen/clipboard.bin"

run_installer "$HOME1" --uninstall >"$SCRATCH/uninstall.out" 2>&1
expect_eq '--uninstall exits 0' "$?" '0'
expect_gone 'the daemon is gone' "$HOME1/.local/bin/lumen-daemon"
expect_gone 'the CLI is gone' "$HOME1/.local/bin/lumen"
expect_gone 'the extension is gone' "$(EXT_DIR_OF "$HOME1")"
expect_gone 'the unit is gone' "$(UNIT_OF "$HOME1")"
expect_gone 'the D-Bus activation file is gone' "$(DBUS_OF "$HOME1")"
expect_gone 'the autostart entry is gone' "$(AUTOSTART_OF "$HOME1")"
expect_gone 'the installed version record is gone' "$(VERSION_OF "$HOME1")"
expect_gone 'the latest version record is gone' "$(LATEST_OF "$HOME1")"
expect_gone 'the manifest is gone' "$(MANIFEST_OF "$HOME1")"
expect_file 'the user data next to them survived' "$HOME1/.local/share/lumen/clipboard.bin"
leftovers=$(find "$HOME1" -mindepth 1 -name '*lumen*' | sort | tr '\n' ' ')
expect_eq 'the only thing named lumen left in the whole home is the user data directory' "$leftovers" \
    "$HOME1/.local/share/lumen "
expect_eq 'and that directory holds nothing but the user data' \
    "$(find "$HOME1/.local/share/lumen" -mindepth 1 | wc -l | tr -d ' ')" '1'

# A second run with nothing installed is still a clean no-op, and a home that had no user data comes
# out completely empty: that is "no session files behind".
run_installer "$HOME1" --uninstall >"$SCRATCH/uninstall-again.out" 2>&1
expect_eq 'uninstalling again exits 0' "$?" '0'

HOME4=$(new_home clean)
make_release 0.1.0
run_installer "$HOME4" --archive "$SCRATCH/lumen-0.1.0-x86_64.tar.gz" \
    --sha256sums "$SCRATCH/SHA256SUMS" >/dev/null 2>&1
run_installer "$HOME4" --uninstall >/dev/null 2>&1
leftover=$(find "$HOME4" -mindepth 1 | head -n 5)
expect_eq 'a home that had no user data is left completely empty' "${leftover:-}" ''

# ---------------------------------------------------------------------------------------------
printf '\n== the installer explains itself ==\n'

sh "$INSTALLER" --help >"$SCRATCH/installer-help.out" 2>&1
expect_eq 'install.sh --help exits 0' "$?" '0'
expect_contains 'and documents --uninstall' "$SCRATCH/installer-help.out" '--uninstall'
expect_contains 'and documents the checksum check' "$SCRATCH/installer-help.out" '--sha256sums'

run_installer "$SCRATCH/nowhere" --nonsense >"$SCRATCH/badflag.out" 2>&1
expect_eq 'an unknown option is refused' "$?" '1'

# ---------------------------------------------------------------------------------------------
printf '\n%s passed, %s failed\n' "$PASSED" "$FAILED"
if [ "$FAILED" -ne 0 ]; then
    exit 1
fi
