SHELL := /bin/bash

.PHONY: build test test-daemon test-js test-packaging test-shell schemas pack release clean

CARGO := source $$HOME/.cargo/env && cargo

build:
	$(CARGO) build --release

# Everything a reviewer runs: the daemon's integration tests, the extension's model tests, and the
# scripted installer/uninstaller test (ticket 16).
test: test-daemon test-js test-packaging

test-daemon:
	$(CARGO) test

test-js:
	node --test 'extension/test/**/*.test.js'

# The real installer, run into a throw-away HOME/XDG tree against stub release archives.
test-packaging:
	sh packaging/tests/install_test.sh

# Lint the shell when shellcheck happens to be installed (it is not a build requirement).
test-shell:
	@if command -v shellcheck >/dev/null 2>&1; then \
		shellcheck packaging/install.sh packaging/release.sh packaging/tests/install_test.sh cli/lumen; \
	else \
		echo "shellcheck is not installed: skipping"; \
	fi

schemas:
	glib-compile-schemas extension/schemas/

pack: schemas
	mkdir -p dist
	gnome-extensions pack extension/ --extra-source=overlay.js --extra-source=system.js --extra-source=lib/ -o dist/

# Both release archives plus SHA256SUMS; see packaging/release.sh for the aarch64 cross-build.
release:
	sh packaging/release.sh

clean:
	$(CARGO) clean
	rm -rf dist
