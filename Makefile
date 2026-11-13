PREVIEW ?= selected
LOCK_PREVIEW ?= prompt
WIDTH ?= 1280
HEIGHT ?= 800
WALLPAPER ?= tahoe-beach

.PHONY: dev animated-dev lock-dev check fmt fmt-fix lint test scripts-test check-rfds smoke wallpaper-smoke lock-smoke lock-vm heic-assets heic-decode evidence update-reference-images update-avatars hardware-smoke e2e build package verify changelog next-version clean

dev:
	cargo run --bin genkan -- login --windowed --preview "$(PREVIEW)" --width "$(WIDTH)" --height "$(HEIGHT)"

animated-dev:
	cargo run --bin genkan -- login --windowed --preview "$(PREVIEW)" --animated-preview --width "$(WIDTH)" --height "$(HEIGHT)" --wallpaper "$(WALLPAPER)"

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
	@./scripts/check-wallpaper-manifest.py
	@./scripts/check-avatars.py

smoke:
	nix build .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).graphics-smoke --print-build-logs

wallpaper-smoke:
	nix build .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).desktop-wallpaper-smoke --print-build-logs

.PHONY: niri-smoke niri-vm
niri-smoke:
	nix build .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).niri-smoke .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).greeter-module --print-build-logs

niri-vm:
	nix build .#checks.x86_64-linux.niri-greeter-vm --print-build-logs

lock-smoke:
	nix build .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).session-lock-smoke --print-build-logs

lock-vm:
	nix build .#checks.x86_64-linux.session-lock-vm --print-build-logs

heic-assets:
	nix build .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).heic-assets --print-build-logs

heic-decode:
	nix build .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).heic-decode --print-build-logs

.PHONY: wallpaper-package wallpaper-empty-cache
wallpaper-package:
	nix build .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).wallpaper-package --print-build-logs

wallpaper-empty-cache:
	nix build .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).wallpaper-empty-cache --print-build-logs

.PHONY: wallpaper-provisioning
wallpaper-provisioning:
	nix build .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).wallpaper-provisioning --print-build-logs

evidence:
	nix build .#checks.$$(nix eval --raw --impure --expr builtins.currentSystem).preview-evidence --print-build-logs

update-reference-images:
	./scripts/update-reference-images.sh

update-avatars:
	./scripts/update-avatars.sh

hardware-smoke:
	nix run .#hardware-smoke

e2e:
	nix build .#checks.x86_64-linux.greetd-e2e --print-build-logs

build:
	cargo build

package:
	nix build

verify: fmt lint test check-rfds package wallpaper-package wallpaper-empty-cache wallpaper-provisioning heic-assets heic-decode smoke wallpaper-smoke lock-smoke niri-smoke evidence

verify: check-design

.PHONY: check-design
check-design:
	python3 scripts/check-design-literals.py

changelog:
	git cliff --output CHANGELOG.md

next-version:
	git cliff --bumped-version

clean:
	cargo clean

# Opt-in: deliberately not a prerequisite of verify or CI. Needs nightly.
.PHONY: fuzz
fuzz:
	bash scripts/fuzz-smoke.sh
