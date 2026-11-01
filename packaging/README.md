# Lumen packaging (ticket 16)

Lumen is delivered as a checksum-verified archive that a per-user installer unpacks into the
user's own session directories. Nothing here needs root, nothing lands outside `$HOME`, and
re-running the installer *is* the update path.

```
packaging/
├── install.sh              the installer and uninstaller
├── release.sh              builds both archives and SHA256SUMS
├── templates/              the session files, rendered per user
│   ├── lumen.service.in              systemd user unit (Type=notify + BusName)
│   ├── org.gnome.Lumen.service.in    D-Bus activation
│   └── lumen.desktop.in              XDG-autostart fallback
├── tests/install_test.sh   the scripted installer test (make test-packaging)
└── README.md               this file
```

`cli/lumen` is the launcher/control CLI installed alongside the daemon; `daemon/src/notify.rs`
implements the `sd_notify` datagram the `Type=notify` unit waits for.

## What an install places

| Path (`XDG_*` respected, defaults shown) | What it is |
|---|---|
| `~/.local/bin/lumen-daemon` | the engine, built for this architecture |
| `~/.local/bin/lumen` | the launcher CLI (`--toggle`, `--status`, `--version`) |
| `~/.local/share/gnome-shell/extensions/lumen@kethan.dev/` | the extension, with `schemas/gschemas.compiled` |
| `~/.config/systemd/user/lumen.service` | `Type=notify` + `BusName=org.gnome.Lumen` user unit |
| `~/.local/share/dbus-1/services/org.gnome.Lumen.service` | D-Bus activation, also without systemd |
| `~/.config/autostart/lumen.desktop` | XDG-autostart fallback for a desktop with no systemd |
| `~/.local/share/lumen/installed-version` | the version the installer placed |
| `~/.local/share/lumen/latest-version` | the newest published release the installer has heard of |
| `~/.local/share/lumen/installed-files` | every path the installer placed: what `--uninstall` removes |

The unit runs `lumen-daemon --indexer localsearch` (contract §11 requires the installer to select
the real file index) with `GSETTINGS_SCHEMA_DIR` pointing at the installed extension's `schemas/`,
which is how the daemon resolves the shared schema (contract §15).

### Why three session files

GNOME's own LocalSearch ships exactly this trio (research §4.1), and each one covers a case the
others do not:

* the **systemd user unit** is the normal path: it starts at login with the graphical session,
  `Type=notify` means "started" is the daemon's own `READY=1`, `BusName=` makes the bus name and
  the unit the same single-instance lock, and `Restart=on-failure` brings it back after a crash;
* **D-Bus activation** starts the daemon on the first call to `org.gnome.Lumen` — what the shell
  extension does when it appears — and is the *only* mechanism a desktop without systemd needs,
  because `dbus-daemon` can run `Exec=` itself;
* the **XDG-autostart entry** covers a session that has neither: `gnome-session` reads
  `~/.config/autostart` directly. `X-GNOME-HiddenUnderSystemd=true` and `X-systemd-skip=true` are
  the same pair LocalSearch sets, so on a systemd desktop the autostart generator leaves the file
  alone and the unit stays the single starter — two starters would fight over the bus name.

Restarting the shell does not touch the daemon: it is not a child of `gnome-shell` and it never
ties its lifetime to a client (contract §1), so a `gnome-shell --replace` leaves it serving and the
extension re-creates its proxy in `enable()`.

## Installing

```sh
sh install.sh --archive lumen-0.1.0-x86_64.tar.gz --sha256sums SHA256SUMS
```

`--sha256sums` (or `--sha256 <hex>`) is **required** for an archive: Lumen refuses to install a
download it cannot verify, and refuses before anything is written. After the files are in place the
installer reloads the systemd user manager and starts `lumen.service`; `--no-systemctl` skips that
and leaves the unit to the next login (that is what the tests use).

Running the same command with a newer archive is the update: the files are replaced, both version
records are rewritten, anything the previous version left behind is removed (the old
`installed-files` manifest is the list), and the service is restarted. An unpacked release
directory works too — `sh install.sh` from inside it — and then there is no download to verify.

Enable the shell extension once, after the first install:

```sh
gnome-extensions enable lumen@kethan.dev
```

## Uninstalling

```sh
sh install.sh --uninstall
```

It stops and disables the unit, removes every path in the manifest, and then removes the
directories that became empty, walking up from each one and stopping at the first that still holds
something. A `$HOME` that only ever held Lumen comes out completely empty. The encrypted clipboard
history (`~/.local/share/lumen/clipboard.bin`) and the usage counters are **deliberately left
alone**: they are the user's data, and the data directory is pruned only when nothing else is in
it. A running shell keeps the extension until it restarts.

## The update path

Lumen never checks the network (contract §9). Instead:

* `install.sh --check --latest <VERSION>` records the newest published version in
  `latest-version` without installing anything — that is how a release announces itself;
* the extension reads `installed-version` and `latest-version` when it is enabled and compares them
  with `extension/lib/update.js` (`updateStatus(installed, latest)`);
* when the latest one is newer it shows a notification, `Lumen <latest> is available (you have
  <current>). Re-run the Lumen installer to update.`

The comparison is a pure Node-tested function; the shell half only reads two files.

## Building a release

```sh
packaging/release.sh                          # x86_64 and aarch64 into dist/
packaging/release.sh --dry-run                # print the plan, build nothing
packaging/release.sh --target x86_64-unknown-linux-gnu
```

Each archive holds one top-level directory:

```
lumen-<version>-<arch>/
├── VERSION
├── bin/lumen-daemon
├── bin/lumen
├── extension/…
└── templates/…      the three .in files install.sh renders
```

plus `dist/SHA256SUMS` (`<hex>  <file>` per archive) and a copy of `install.sh`.

The daemon is built against the host's C library, not statically: research §5.3 found that a
fully static glib/GIO deployment is a partially supported configuration nobody validated, and the
daemon links no C library of its own — the LocalSearch library is `dlopen`ed at run time (contract
§11) and GSettings is read through the `gsettings` CLI — so the dynamic build is what a release
ships.

### aarch64

Cross-building needs the Rust target plus a C cross-linker, which is CI's job:

```sh
rustup target add aarch64-unknown-linux-gnu
sudo dnf install gcc-aarch64-linux-gnu        # or: apt install gcc-aarch64-linux-gnu
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc packaging/release.sh
```

`--target aarch64-unknown-linux-gnu` without that toolchain fails loudly; the script never fakes a
second architecture or reuses the x86-64 binary for it.

## Tests

```sh
sh packaging/tests/install_test.sh      # or: make test-packaging
make test                               # daemon + extension + packaging
make test-shell                         # shellcheck, when it is installed
```

`install_test.sh` runs the real installer into a throw-away `HOME`/`XDG_*` tree against stub
release archives it builds itself, and asserts where every file lands and what it contains, that a
bad or absent checksum installs nothing, that a second run updates in place and drops stale files,
that `--uninstall` leaves no session files behind (while keeping user data), and that `--check`
records the newest release. It never touches systemd, the session bus or the real home directory:
`LUMEN_SYSTEMCTL=` is set to empty and `--no-systemctl` is passed for every call.
