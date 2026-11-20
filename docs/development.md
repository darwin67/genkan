# Development

This guide covers working on Genkan: previewing the UI safely, choosing which
checks a change needs, and publishing a release. For how the pieces fit
together, see [How Genkan Works](architecture.md).

## Set up

Enter the pinned Nix shell before running Cargo or Make targets, then open the
safe login preview:

```sh
nix develop
make dev
```

Amp orbs run `.agents/setup` to prepare the same shell (including jj) and
prefetch locked Cargo dependencies; login shells in the checkout activate it
automatically. `.agents/resume` initializes a colocated jj repository when
needed and starts nothing. Setup skips compilation and VM tests to keep
snapshots short, and hosted orbs have no KVM, so run the
[NixOS VM checks](#nixos-vm-checks) in CI or on a KVM-capable runner.

Nix builds see only Git-tracked files, and new files in a jj working copy are
not yet tracked by Git. Before `make update-reference-images`, `make verify`, or
any other Nix build, describe the change and run `jj new`. This matters most
for new reference images and fixtures.

## Preview the UI

Previews use fixed synthetic accounts, time, animation frame, and sessions.
They accept input without sending it anywhere and simulate authentication and
every power action, so testing cannot suspend, restart, or shut down your
machine. They never contact AccountsService, greetd, logind, or PAM.

| Preview | Command | Select a state |
| --- | --- | --- |
| Login | `make dev` | `PREVIEW=users`, `visible-prompt`, `authentication-failure`, `power-confirmation`, `power-menu`, `change-user` |
| Lock | `make lock-dev` | `LOCK_PREVIEW=challenge`, `submitting`, `failure`; `WIDTH`, `HEIGHT`, `WALLPAPER` |
| Animated wallpaper | `make animated-dev` | `WALLPAPER=<catalog ID>` |

```sh
PREVIEW=visible-prompt make dev
WIDTH=1080 HEIGHT=1920 LOCK_PREVIEW=failure make lock-dev
```

`cargo run --bin genkan -- login --help` lists every fixture and login option.
Pass `--username` only when a particular synthetic identity helps. To exercise
real services, run Genkan without `--preview` in the intended greeter
environment.

The login preview renders a cached wallpaper poster without starting
GStreamer, or the generated background when nothing is cached. The lock preview
uses the production lock renderer in a normal window, laid out as the
authentication output, without requesting `ext-session-lock-v1`. Automated
reference captures use an explicit test-only cache of source JPEGs, not a
production fallback.

`make animated-dev` plays real wallpaper media while keeping authentication and
power simulated. Download the media first; with an empty cache it shows the
generated background:

```sh
cargo run --bin genkan -- assets download tahoe-beach
make animated-dev
cargo run --bin genkan -- assets download macos-landscape-sequoia-sunrise
WALLPAPER=macos-landscape-sequoia-sunrise make animated-dev
```

`cargo run --bin genkan -- assets preview <ID>` previews any catalog wallpaper
the same way. See [Wallpapers](wallpapers.md) for downloads, custom catalogs,
and playback rules.

## Choose the checks

Run `make verify` before completing any code change. It runs formatting, lint,
Rust and shell tests, RFD and design checks, packaging and wallpaper checks,
headless compositor smoke tests, and preview evidence. Some changes need more:

| You changed | Also run | Needs |
| --- | --- | --- |
| greetd/PAM retry, cancellation, or session launch | `make e2e` | x86_64 Linux with KVM |
| Session-lock lifecycle, input isolation, before-sleep readiness, or resume | `make lock-vm` | x86_64 Linux with KVM |
| niri greeter authentication, session handoff, or crash recovery | `make niri-vm` | x86_64 Linux with KVM |
| GeoClue-backed solar scheduling | `nix build .#checks.x86_64-linux.geoclue-solar-vm --print-build-logs` | x86_64 Linux with KVM |
| Physical graphics | `make hardware-smoke` | A Wayland session with local DRM devices |
| Parsers | `make fuzz` | Nightly Rust; see [fuzzing](#property-tests-and-fuzzing) |

Run `make test`, including through `make verify`, in the foreground: a
background shell job can inherit an ignored SIGINT and spuriously fail the
reference-image interruption regression.

### What `make verify` checks

- **`make smoke`** runs the packaged-binary graphics check used by both
  architecture jobs in CI. It nests Cage with its pixman renderer inside
  headless Weston and launches Genkan with Mesa's software Vulkan driver.
  Genkan must stay alive until the controlled timeout on iced's wgpu backend,
  with the intended Vulkan driver. The check also compares animated wallpaper
  crops with the static poster, requires several distinct nonblank frames from
  an authored HEVC fixture, bounds short-term RSS growth, and launches
  production builds without a fixture cache. It is not the sustained
  performance or input-latency pass tracked in [Deferred Work](deferred.md).
- **`make wallpaper-smoke`** runs the packaged `genkan wallpaper` under
  headless Sway with two differently scaled outputs. It changes scale,
  transform, and mode without recreating a surface, compares cover rendering
  with independent ImageMagick references, and confirms that a foreground
  `genkan lock` fully covers the desktop client and that unlocking restores it.
  It emits `mixed-scale-landscape.png`, `mixed-scale-portrait.png`,
  `ultrawide.png`, `locked-landscape.png`, `locked-portrait.png`, and
  `transition-before.png`, `transition-during.png`, and `transition-after.png`
  for review, and makes no GeoClue or session-bus request.
- **`make niri-smoke`** checks the generated greeter configuration and runs
  wallpaper and lock under nested niri at fractional scale.
- **`make check-rfds`** validates RFD layout and metadata, reference images,
  avatars, and the wallpaper manifest. For the manifest it checks the recorded
  loop-analysis claims (method, `endpoint_ssim`, verification,
  `default_wallpaper`, decode verification) without fetching or re-measuring
  upstream media.
- **`make check-design`** rejects numeric control styling outside
  `src/design/`; see [shared presentation](architecture.md#shared-presentation).
- **`make wallpaper-package`** checks that the production wallpaper tree holds
  only the two catalog TOML files and that the runtime closure contains no
  artwork or test cache. **`make wallpaper-empty-cache`** runs the unmodified
  production wrapper with empty caches and confirms that preview opens no
  artwork and starts no download. **`make wallpaper-provisioning`** checks the
  NixOS preparation unit and the wrapper's selection and override behavior.
- **`make heic-assets`** verifies the sizes and SHA-256 hashes of the
  repository's authored HEIC fixtures, and **`make heic-decode`** decodes all
  their frames through the shipped parser against the manifest's
  `structure.image_count`. A fixture declaring `decode_verified = false` fails
  rather than being skipped. Both use an explicit test cache and never
  download or revalidate remote catalog records.

The wallpaper transition capture sets local time twenty seconds before the
fixture's 06:00 boundary with a second-precision POSIX `TZ` offset, so a slow
software-rendered startup still shows the red frame before the dissolve. Each
phase must be a near-uniform, blend-compatible field matching the fixture's
palette and red-plus-green intensity, so an unrelated or split frame cannot
stand in for a missing phase. Screenshots cannot distinguish a fabricated
uniform frame from a real dissolve, so this bounds the failure modes rather
than proving the renderer's internal path.

### NixOS VM checks

CI runs the VM checks in the table above separately from `make verify`. Locally
they need an x86_64 Linux host with KVM. `make e2e` uses real greetd 0.10.3
with NixOS PAM and a feature-gated driver to verify the launched user and
session environment; it does not exercise iced rendering or input automation.

### Physical graphics smoke

Hosted CI has no DRM devices, so physical hardware testing is opt-in. Run
`make hardware-smoke` from a Wayland session. It tests the system AMD and
NVIDIA Vulkan ICDs once per detected vendor, launches packaged Genkan under
Vulkan-rendered nested Cage when that vendor has a display-connected adapter,
and verifies that an AMD-only run does not load or open the NVIDIA driver. It
passes `--reduce-motion`, so validate animated playback separately before
claiming physical animation coverage.

On Sway, this stricter run also moves Cage to every active output, verifies
its resulting tree location, and requires both GPU vendors and an active
external display:

```sh
GENKAN_REQUIRE_GPU_VENDORS='1002 10de' \
GENKAN_REQUIRE_EXTERNAL_DISPLAY=1 \
GENKAN_EXERCISE_SWAY_OUTPUTS=1 \
make hardware-smoke
```

A disconnected hybrid GPU can validate its Vulkan driver but not presentation.
Physical ARM graphics validation remains tracked in
[Deferred Work](deferred.md#physical-arm-graphics-validation).

## Property tests and fuzzing

`make test` runs property tests alongside the unit tests, with 128 cases or
fewer (32 shell executions, 64 login sequences of up to 79 actions, 64 solar
mappings). Keep the `proptest-regressions/` files: they replay shrunk failures
before new random cases. All generated login data is synthetic; never feed
credentials or captured PAM conversations into test strategies, Debug output,
fuzz corpora, or failure reports.

The properties test the existing contracts, not stronger substitutes:

- Shell arguments exclude NUL. A zero-argument vector has no encoded arguments;
  it is checked separately because POSIX `printf '%s\\0'` otherwise emits one
  empty field even without operands.
- Exec quoting has two escaping layers. Literal `%%` becomes `%`, and literal
  reserved characters are valid inside quoted argv. Session validity is stricter
  than syntax validity (for example `%U` is syntactically valid but unsupported).
- Missing or nonpositive login history on **any** peer prevents automatic
  account selection; a single usable account is selected regardless of history.
- `bounded_text` is idempotent. `clean_prompt` is only a single cleanup pass:
  `x: :` becomes `x:` and then `x` on a second call. Appending is accumulation,
  not idempotent except for empty appends or a saturated buffer.
- Connector-name selection is permutation-invariant. Duplicate names may select
  different handles, and all-unnamed outputs deliberately use discovery order.
- Fitting returns empty when even an ellipsis cannot fit. Monotonicity concerns
  retained source characters, not the extra ellipsis character.
- Cover sampling allows integer crop/scale and nearest-neighbor rounding;
  generated pixel allocations stay below 4 KiB. Large dimensions exercise only
  geometry and checked arithmetic, without allocating large buffers.

The separate `fuzz/` crate is excluded from the workspace, its default members,
`make verify`, and CI. Its `fuzzing` feature exposes only in-memory entry points
and does not enable iced, GStreamer, D-Bus, or session-lock. The targets cover
aggregate HEIC container preflight (not native image decoding), XMP/XML and
binary plist/Base64 parsing, and Desktop Entry validation and expansion.

The default shell pins stable Rust. To run a short campaign with nightly and
cargo-fuzz from the **same pinned Nix inputs**, run this at the repository root:

```sh
nix develop --command nix shell --impure --expr '
  let
    flake = builtins.getFlake ("git+file://" + toString ./.);
    pkgs = import flake.inputs.nixpkgs {
      system = builtins.currentSystem;
      overlays = [ flake.inputs.rust-overlay.overlays.default ];
    };
  in [ pkgs.rust-bin.nightly.latest.default pkgs.cargo-fuzz ]
' --command make fuzz
```

`make fuzz` seeds each target from the small HEIC fixtures and synthetic parser
examples and runs each for 10 seconds, capping input at 64 KiB, RSS at 512 MiB,
single allocations at 128 MiB, and each input at five seconds. Set
`FUZZ_SECONDS=60` for a longer campaign. Mutable corpora, crash artifacts, and
build output are ignored; deterministic seeds and `fuzz/Cargo.lock` are
tracked. Preserve real crash artifacts, minimize them with
`cargo fuzz tmin TARGET PATH`, and add a regular unit regression before fixing
production code.

Passing fuzz runs do **not** prove an allocator bound. The production
preflights enforce aggregate box, item, and extent budgets, XML depth and event
budgets, and plist object, depth, expanded-byte, and metadata-byte budgets.
Adversarial unit tests and feature-gated one-over-budget tests check that
enforcement directly; the container target also checks that acceptance is
monotone as budgets grow. The 64 KiB input cap does not reach the production
source-size limit, and the targets never fuzz live PAM, greetd, native libheif
decoding, graphics drivers, or arbitrary unlimited schedules.

## Make targets

| Task | Make targets |
| --- | --- |
| Preview | `dev`, `animated-dev`, `lock-dev` |
| Build and check | `check`, `fmt`, `fmt-fix`, `lint`, `test`, `scripts-test`, `check-rfds`, `check-design`, `build`, `package`, `verify`, `clean` |
| Packaged smoke checks | `smoke`, `wallpaper-smoke`, `lock-smoke`, `niri-smoke`, `evidence` |
| Wallpaper and fixtures | `wallpaper-package`, `wallpaper-empty-cache`, `wallpaper-provisioning`, `heic-assets`, `heic-decode` |
| Regenerate assets | `update-reference-images`, `update-avatars` |
| Separate checks | `e2e`, `lock-vm`, `niri-vm` (KVM); `hardware-smoke` (physical graphics); `fuzz` (nightly Rust) |
| Release tooling | `changelog`, `next-version` |

## Release Genkan

**Squash-merge the `release/next` PR after its checks pass** to publish a
GitHub release. If `main` has advanced, wait for `Prepare release PR` to
refresh the proposal and its checks. git-cliff starts at `v0.1.0`, then chooses
major for breaking changes, minor for features, and patch for other included
commits. The PR adds a changelog entry without rewriting released notes, and
keeps Cargo's manifest and both lockfiles in sync.

`Publish GitHub release` builds the exact merge commit for both Linux targets
before publishing either download. If it fails, rerun the failed jobs; an
incomplete upload remains a draft. If `main` advances while a release is still
building, the next release PR waits for its tag. After publishing, run
`Prepare release PR` manually from `main` to pick up those commits without
waiting for another push. Users install releases as described in
[Install a prebuilt release](install.md#install-a-prebuilt-release).

If a stale proposal is merged anyway, publishing stops before tagging. Revert
only that squash commit through a PR, then rerun **Prepare release PR**.

### One-time repository setup

Install a GitHub App with **Contents: read/write** and **Pull requests:
read/write** on this repository. In the `RELEASE` Actions environment, set
`RELEASE_APP_ID` to the app ID and `RELEASE_APP_SECRET` to its private key, and
allow `main` to use the environment. The app opens the PR so normal PR checks
run; publishing uses the workflow's own GitHub token.

To enforce freshness at merge time, require the **Release proposal freshness**
status check on `main` and enable **Require branches to be up to date before
merging**.
