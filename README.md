# Lumen

A Spotlight-equivalent for GNOME 50/51 on Wayland: a GNOME Shell extension plus a background
daemon. The extension draws the overlay and performs shell-only actions; the daemon owns search,
ranking, calculation and storage, and the two talk over the D-Bus session bus.

The spec and tickets live in agent-todo (project `LRM`, group `LRM-2`). The shared interface
contract is in the notes that accompany the implementation.

## Layout

```
daemon/      Rust engine daemon; owns org.gnome.Lumen
extension/   GNOME Shell extension; lib/ holds the pure-JS model modules
cli/lumen    Launcher/control CLI for the installed daemon (--toggle, --status, --version)
packaging/   Per-user installer, session files, release builder — see packaging/README.md
```

## Building and testing

```sh
source "$HOME/.cargo/env"

cargo build --release                    # daemon binary at target/release/lumen-daemon
cargo test                               # daemon unit + D-Bus integration tests

node --test 'extension/test/**/*.test.js'  # extension model tests (no shell)

sh packaging/tests/install_test.sh       # installer/uninstaller, into a throw-away HOME

glib-compile-schemas extension/schemas/  # build the shared GSettings schema
gnome-extensions pack extension/ -o dist/  # pack the extension zip
```

`make test` runs all three suites; `make test-packaging` runs the installer test alone;
`make pack` compiles the schema and packs the extension; `make build` builds a release daemon;
`make release` builds the x86_64 and aarch64 archives with their checksums.

## Installing

Prebuilt archives are installed per user, with no root, from a checksum-verified download:

```sh
sh install.sh --archive lumen-0.1.0-x86_64.tar.gz --sha256sums SHA256SUMS
gnome-extensions enable lumen@kethan.dev
```

Re-running the installer is the update path, `--uninstall` takes it all back out, and the extension
tells the user when a newer release exists. `packaging/README.md` has the details.

## The two seams

- **The daemon's D-Bus interface** (`org.gnome.Lumen.Engine`). Integration tests start the real
  binary on a private session bus and drive it as a client, with the file indexer replaced behind a
  trait.
- **The extension's pure-JS model modules** (`extension/lib/*.js`). Plain ES modules with no shell
  imports, run under `node --test`.

Shell-dependent UI (the overlay actor tree, the keybinding, modal grab, multi-monitor placement) is
verified manually against GNOME 50/51 on Wayland.
