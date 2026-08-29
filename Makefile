.PHONY: dev check fmt fmt-fix lint test scripts-test smoke hardware-smoke e2e build package verify changelog next-version clean

dev:
	cargo run --bin genkan -- --windowed

check:
	cargo check

fmt:
	cargo fmt --all -- --check

fmt-fix:
	cargo fmt --all

lint:
	cargo clippy --all-targets -- -D warnings
	cargo clippy --no-default-features --features e2e --bin genkan-greetd-e2e -- -D warnings

test: scripts-test
	cargo test

scripts-test:
	bash scripts/regression-tests.sh

smoke:
	nix build .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).graphics-smoke --print-build-logs

hardware-smoke:
	nix run .#hardware-smoke

e2e:
	nix build .#checks.x86_64-linux.greetd-e2e --print-build-logs

build:
	cargo build

package:
	nix build

verify: fmt lint test package smoke

changelog:
	git cliff --output CHANGELOG.md

next-version:
	git cliff --bumped-version

clean:
	cargo clean
