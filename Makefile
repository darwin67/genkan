.PHONY: dev check fmt fmt-fix lint test build package verify clean

dev:
	cargo run -- --windowed

check:
	cargo check

fmt:
	cargo fmt --all -- --check

fmt-fix:
	cargo fmt --all

lint:
	cargo clippy --all-targets -- -D warnings

test:
	cargo test

build:
	cargo build

package:
	nix build

verify: fmt lint test package

clean:
	cargo clean
