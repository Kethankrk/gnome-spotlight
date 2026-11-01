# Lumen

A Spotlight-equivalent for GNOME 50/51 on Wayland: a GNOME Shell extension plus a small background
daemon. The extension draws the overlay and does the shell-only work; the daemon owns search,
ranking, calculation and storage. The two talk over the D-Bus session bus.

Spec and tickets live in agent-todo (project `LRM`, group `LRM-2`); history lives in Git.

## Requirements

- **GNOME Shell 50 or 51, on Wayland.** X11 and older shells are not supported.
- **Rust** (cargo ≥ 1.89) — to build the daemon.
- **`glib-compile-schemas` and `gnome-extensions`** — ship with `glib2`/`glib2-tools` and
  `gnome-shell`. The installer uses them.
- **Node 18+** — only to run the extension's test suite.

No root, and no development headers: the daemon loads the system's LocalSearch library at run time.

## Install from source, step by step

### 1. Get the source

```sh
git clone https://github.com/Kethankrk/gnome-spotlight.git
cd gnome-spotlight
```

### 2. Build a release archive

```sh
sh packaging/release.sh --target x86_64-unknown-linux-gnu
```

This compiles the daemon and writes everything the installer needs into `dist/`:

```
dist/install.sh
dist/lumen-0.1.0-x86_64.tar.gz     # the daemon binary + the extension + session templates
dist/SHA256SUMS                    # the checksum the installer verifies against
```

On an ARM machine use `--target aarch64-unknown-linux-gnu`. Running `packaging/release.sh` with no
`--target` builds **both** architectures (that is what CI does; it needs an aarch64 cross-linker, so
it fails loudly on a plain x86-64 box).

> Just want the binary? `make build` runs `cargo build --release` and leaves
> `target/release/lumen-daemon`. The archive above is what the installer consumes.

### 3. Install it

```sh
sh dist/install.sh --archive dist/lumen-0.1.0-x86_64.tar.gz --sha256sums dist/SHA256SUMS
```

No root. The installer verifies the archive against `SHA256SUMS` first — a bad or missing checksum
installs nothing — then places everything per user and starts the daemon's user service:

| Path (XDG respected, defaults shown) | What |
|---|---|
| `~/.local/bin/lumen-daemon` | the engine |
| `~/.local/bin/lumen` | the launcher CLI |
| `~/.local/share/gnome-shell/extensions/lumen@kethan.dev/` | the extension, with its compiled schema |
| `~/.config/systemd/user/lumen.service` | starts the daemon at login |
| `~/.local/share/dbus-1/services/org.gnome.Lumen.service` | starts it on demand where systemd is absent |
| `~/.config/autostart/lumen.desktop` | fallback for a session with no systemd |

It prints every path it wrote. If it warns that `~/.local/bin` is not on your `PATH`, add it so
`lumen` works from a terminal:

```sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.profile   # then log out and back in
```

### 4. Enable the extension

```sh
gnome-extensions enable lumen@kethan.dev
```

Then **log out and back in** — a freshly installed extension is only picked up by a new shell
session on Wayland. The daemon starts with the session; you do not start it by hand.

### 5. Use it

Press **`Super+Space`** to summon the overlay; type to search, Enter to open, Escape to dismiss.
Change the shortcut in the preferences window:

```sh
gnome-extensions prefs lumen@kethan.dev
```

> **Heads-up:** the default `Super+Space` is the same combination stock GNOME uses for *Switch input
> source*. The preferences window will flag the clash — pick another shortcut there if you want.

## Update and uninstall

- **Update:** build a newer archive and run the same install command again. It replaces the files,
  rewrites the version records, and restarts the service.
- **Uninstall:** `sh dist/install.sh --uninstall`. It removes everything it placed and leaves your
  data (clipboard history, usage counts) alone.

## Check it is running

```sh
lumen --status                       # is the engine up, and what can it do
lumen --version                      # the installed version
systemctl --user status lumen.service
~/.local/bin/lumen-daemon --help     # the daemon's own flags
```

## Development

```sh
make test        # daemon (Rust) + extension (JS) + packaging suites
make build       # release daemon binary only: target/release/lumen-daemon
make pack        # compile the schema and pack the extension zip
make release     # both release archives + SHA256SUMS (needs the aarch64 cross-linker)

node --test 'extension/test/**/*.test.js'   # extension model tests alone
sh packaging/tests/install_test.sh          # installer/uninstaller into a throw-away HOME
```

## Layout

```
daemon/      Rust engine daemon; owns org.gnome.Lumen
extension/   GNOME Shell extension; lib/ holds the pure-JS model modules
cli/lumen    Launcher/control CLI (--toggle, --status, --version)
packaging/   Per-user installer, session-file templates, release builder — see packaging/README.md
```

## The two seams

Everything is tested through two seams, never through gnome-shell:

- **the daemon's D-Bus interface** — integration tests start the real binary on a private session
  bus and drive it as a client, with the file indexer swapped behind a trait;
- **the extension's pure-JS model modules** (`extension/lib/*.js`) — plain ES modules with no shell
  imports, run under `node --test`.

Shell-dependent UI (the overlay actor tree, the keybinding, modal grab, multi-monitor placement) is
verified manually against GNOME 50/51 on Wayland.
