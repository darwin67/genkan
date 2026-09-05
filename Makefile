PREVIEW ?= selected
LOCK_PREVIEW ?= prompt
WIDTH ?= 1280
HEIGHT ?= 800
WALLPAPER ?= tahoe-beach

.PHONY: dev animated-dev lock-dev check fmt fmt-fix lint test scripts-test check-rfds smoke lock-smoke lock-vm evidence update-reference-images hardware-smoke e2e build package verify changelog next-version clean

dev:
	cargo run --bin genkan -- login --windowed --preview "$(PREVIEW)" --width "$(WIDTH)" --height "$(HEIGHT)"

animated-dev:
	@test -n "$(GENKAN_WALLPAPER_DIR)" || { echo "GENKAN_WALLPAPER_DIR is unavailable; run this target inside nix develop" >&2; exit 1; }
	cargo run --bin genkan -- login --windowed --preview "$(PREVIEW)" --animated-preview --width "$(WIDTH)" --height "$(HEIGHT)" --wallpaper "$(WALLPAPER)" --wallpaper-file "$(GENKAN_WALLPAPER_DIR)/$(WALLPAPER).mov"

lock-dev:
	cargo run --bin genkan -- lock --preview "$(LOCK_PREVIEW)" --width "$(WIDTH)" --height "$(HEIGHT)" --wallpaper "$(WALLPAPER)"

check:
	cargo check

fmt:
	cargo fmt --all -- --check

fmt-fix:
	cargo fmt --all

lint:
	cargo clippy --all-targets -- -D warnings
	cargo clippy --all-targets --features lock-test -- -D warnings
	cargo clippy --no-default-features --features e2e --bin genkan-greetd-e2e -- -D warnings

test: scripts-test
	cargo test
	cargo test --features lock-test

scripts-test:
	bash scripts/regression-tests.sh

check-rfds:
	@./scripts/check-rfd-status.sh
	@./scripts/check-rfd-status-test.sh
	@./scripts/check-reference-images.sh

smoke:
	nix build .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).graphics-smoke --print-build-logs

lock-smoke:
	nix build .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).session-lock-smoke --print-build-logs

lock-vm:
	nix build .#checks.x86_64-linux.session-lock-vm --print-build-logs

evidence:
	nix build .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).preview-evidence --print-build-logs

update-reference-images:
	./scripts/update-reference-images.sh

hardware-smoke:
	nix run .#hardware-smoke

e2e:
	nix build .#checks.x86_64-linux.greetd-e2e --print-build-logs

build:
	cargo build

package:
	nix build

verify: fmt lint test check-rfds package smoke lock-smoke evidence

changelog:
	git cliff --output CHANGELOG.md

next-version:
	git cliff --bumped-version

clean:
	cargo clean
