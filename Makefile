SHELL := /bin/bash

.PHONY: build test test-daemon test-js schemas pack clean

CARGO := source $$HOME/.cargo/env && cargo

build:
	$(CARGO) build --release

test: test-daemon test-js

test-daemon:
	$(CARGO) test

test-js:
	node --test 'extension/test/**/*.test.js'

schemas:
	glib-compile-schemas extension/schemas/

pack: schemas
	mkdir -p dist
	gnome-extensions pack extension/ -o dist/

clean:
	$(CARGO) clean
	rm -rf dist
