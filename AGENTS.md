# AGENTS.md

## Repository Overview

Genkan is a graphical greetd frontend for Linux, written in Rust with iced. It
runs as an unprivileged application under Cage; greetd remains responsible for
PAM authentication and launching the selected desktop session.

## Architecture

- `src/main.rs`: iced application and authentication state machine.
- `src/auth.rs`: greetd IPC transport and response normalization.
- `src/sessions.rs`: Wayland desktop-session discovery and environment setup.
- `src/power.rs`: logind power operations over D-Bus.
- `src/background.rs` and `src/theme.rs`: presentation and animation.
- `flake.nix`: pinned Rust toolchain, package, and development shell for Linux.

## Development

Enter the pinned Rust environment before running Cargo commands:

```sh
nix develop
cargo run -- --windowed
```

Before completing code changes, run:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
nix build
```

## Security and Behavior

- Never validate passwords in Genkan or log credentials and PAM responses.
- Keep greetd as the owner of authentication and session creation.
- Handle every greetd prompt type and do not assume authentication is a single
  password exchange.
- Pass the Wayland XDG session environment before requesting `StartSession`.
- Keep power actions behind explicit confirmation where data loss is possible.
- Preserve failed-authentication recovery without restarting the greeter.
