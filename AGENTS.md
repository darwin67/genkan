# AGENTS.md

## Repository Overview

Genkan is a graphical greetd frontend for Linux, written in Rust with iced. It
runs as an unprivileged application under Cage; greetd remains responsible for
PAM authentication and launching the selected desktop session.

## Version Control

- Use Jujutsu (`jj`) whenever possible for status, diffs, history, commits,
  rebases, and bookmark management.
- Use Git only when an external integration requires it or `jj` has no
  equivalent operation. Do not use Git staging, reset, or rebase commands.
- This is a colocated repository. Export Jujutsu changes before a Git-only
  integration when necessary, and import any resulting Git changes back into
  Jujutsu.
- Keep changes in small, logical Conventional Commits that are independently
  reviewable.

## Architecture

- `src/main.rs`: CLI parsing and iced application startup.
- `src/lib.rs`: reusable protocol-facing library surface.
- `src/bin/greetd-e2e.rs`: feature-gated real-greetd VM test driver.
- `src/app/mod.rs`: application state, messages, and event orchestration.
- `src/app/auth_flow.rs`: authentication transitions, attempt lifecycle, and
  greetd tasks.
- `src/app/view.rs`: greeter rendering and presentation helpers.
- `src/accounts.rs`: AccountsService discovery and login account metadata.
- `src/auth.rs`: greetd IPC transport and response normalization.
- `src/sessions.rs`: validated Wayland desktop-entry discovery, command
  expansion, and environment setup.
- `src/power.rs`: logind power operations over D-Bus.
- `src/background.rs` and `src/theme.rs`: presentation and animation.
- `nix/tests/greetd.nix`: real greetd and PAM NixOS VM test.
- `nix/tests/graphics-smoke.nix`: packaged-binary launch under nested Cage and
  headless Weston with software Vulkan rendering.
- `flake.nix`: pinned Rust toolchain, package, and development shell for Linux.

## Development

Use Conventional Commit titles: `<type>(optional-scope): <description>`. The
allowed types are `feat`, `fix`, `doc`, `docs`, `test`, `ci`, `refactor`,
`perf`, `chore`, `revert`, `style`, and `security`. Use `!` before the colon or
a `BREAKING CHANGE:` footer for breaking changes. Keep pull request titles,
`.github/workflows/commits.yml`, and `cliff.toml` aligned.
CI validates every non-merge commit introduced by a pull request. Merge commits
are exempt because their generated subjects are controlled by the hosting
platform; pull request titles remain authoritative for squash merges.

Enter the pinned Rust environment before running Cargo commands:

```sh
nix develop
make dev
```

Before completing code changes, run:

```sh
make verify
```

For authentication protocol or lifecycle changes, also run:

```sh
make e2e
```

## Security and Behavior

- Never validate passwords in Genkan or log credentials and PAM responses.
- Keep greetd as the owner of authentication and session creation.
- Discover login identities through AccountsService; keep CLI identity values
  as optional administrative overrides only.
- Handle every greetd prompt type and do not assume authentication is a single
  password exchange.
- Launch only validated entries from `wayland-sessions`; never add an implicit
  compositor command or invoke a shell.
- Pass the Wayland XDG session environment before requesting `StartSession`.
- Keep power actions behind explicit confirmation where data loss is possible.
- Preserve failed-authentication recovery without restarting the greeter.
