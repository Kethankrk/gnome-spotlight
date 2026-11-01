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
packaging/   Per-user installer, systemd unit, D-Bus activation, autostart
```

## Building and testing

```sh
source "$HOME/.cargo/env"

cargo build --release                    # daemon binary at target/release/lumen-daemon
cargo test                               # daemon unit + D-Bus integration tests

node --test 'extension/test/**/*.test.js'  # extension model tests (no shell)

glib-compile-schemas extension/schemas/  # build the shared GSettings schema
gnome-extensions pack extension/ -o dist/  # pack the extension zip
```

`make test` runs both suites; `make pack` compiles the schema and packs the extension; `make build`
builds a release daemon.

## The two seams

- **The daemon's D-Bus interface** (`org.gnome.Lumen.Engine`). Integration tests start the real
  binary on a private session bus and drive it as a client, with the file indexer replaced behind a
  trait.
- **The extension's pure-JS model modules** (`extension/lib/*.js`). Plain ES modules with no shell
  imports, run under `node --test`.

Shell-dependent UI (the overlay actor tree, the keybinding, modal grab, multi-monitor placement) is
verified manually against GNOME 50/51 on Wayland.
