# Development and Verification

## Development environment

Enter the pinned Nix shell before running Cargo or Make targets:

```sh
nix develop
```

The Makefile provides `dev`, `animated-dev`, `lock-dev`, `check`, `fmt`, `fmt-fix`, `lint`,
`test`, `scripts-test`, `check-rfds`, `smoke`, `wallpaper-smoke`, `lock-smoke`,
`lock-vm`, `heic-assets`, `heic-decode`, `evidence`, `update-reference-images`,
`wallpaper-package`, `wallpaper-empty-cache`, `hardware-smoke`, `e2e`, `build`,
`package`, `verify`, `changelog`, `next-version`, and `clean` targets. `fuzz` is
an opt-in nightly target.

## Shared presentation

`src/design/` owns the bundled Inter typeface, glyphs, styles, text fitting,
and plain-data tokens used by iced login and the CPU lock renderer.
`tokens.rs` mirrors `docs/design-system/tokens.json`; the token mirror test
fails if either changes without the other. `make check-design`, also part of
`make verify`, rejects new numeric control styling outside `src/design/`.
Its regression fixtures cover color, font, spacing, comments, and structural
zeroes. It is a lexical guard, not a Rust parser or a ban on geometry arithmetic.

Controls sample a shared blurred copy of the owned wallpaper frame. The copy
is at most 192 pixels on its longest edge, uses bounded stratified downsampling
and three separable box passes, and is cached until the wallpaper allocation
changes. Both renderers crop it with the original wallpaper's cover geometry
and clip it to the control. Login rasterizes a control-sized rounded alpha mask
because iced's tiny-skia backend does not implement image-local clipping.
A dark backing bounds its brightest channel even
over white artwork; the wallpaper outside controls stays undimmed. The fallback
generated background keeps its existing contrast-tested control treatment.

Contrast tests use the actual control color roles and filtered material over
RGB cube vertices and test-only reference JPEGs, in encoded and linear sRGB.
Wallpaper text has a soft shadow below it rather than an outline, so its
contrast over bright photographic regions is a visual check, not a numeric one. The lock rejection shake uses the login motion curve,
requests overlay-only frames at the runtime's 16ms cadence, ends at rest, and
is disabled by reduced motion. It does not change authentication or unlocking.

Caps Lock and the active layout name come from the focused Wayland keyboard's
XKB keymap and `modifiers` event, never from observing Caps Lock key presses.
Lock reuses its SCTK seat keyboard. Login briefly observes iced's own borrowed
Wayland connection every 500ms; a second connection would not own keyboard
focus and cannot reliably supply this state. The observer does not interpret
key events, change focus, or retain response text. It reads keymap descriptors
at offset zero without advancing their potentially shared open-file offset.
Lock selects metadata from the keyboard producing input; removing another
seat cannot clear it. Login omits conflicting or unknown focused-seat snapshots.
Unavailable keymaps, absent focus, unnamed layouts, and non-Wayland backends
omit the items rather than guess. Deterministic
previews use explicitly synthetic metadata and never contact system services.

Network presentation reads only `/sys/class/net/*/operstate` (excluding loopback).
“Network link up” means an interface reports `up`, not that Internet access was
tested. “Network link down” requires all observed interfaces to have known down
states; unknown or unreadable states without an up interface omit the item.
There are no network requests, NetworkManager dependency, or new daemons. Lock
checks every five seconds; login shares its metadata timer.

After presentation changes, describe the jj change and run `jj new` before
`make update-reference-images` or any Nix build: flake sources include only
Git-tracked files, not newly added jj working-copy files.

## Property testing and fuzzing

`make test` runs the property tests alongside the ordinary unit tests. The
default counts are 128 cases or fewer (32 shell executions, 64 login sequences
of up to 79 actions, 64 solar mappings). Keep `proptest-regressions/` files:
they replay shrunk failures before new random cases. All generated login data
is synthetic; never feed credentials or captured PAM conversations into test
strategies, Debug output, fuzz corpora, or failure reports.

The properties test the existing contracts, not stronger substitutes:

- Shell arguments exclude NUL. A zero-argument vector has no encoded arguments;
  it is checked separately because POSIX `printf '%s\\0'` otherwise emits one
  empty field even without operands.
- Exec quoting has two escaping layers. Literal `%%` becomes `%`, and literal
  reserved characters are valid inside quoted argv. Session validity is stricter
  than syntax validity (for example `%U` is syntactically valid but unsupported).
- Missing/nonpositive login history on **any** peer prevents automatic account
  selection; a single usable account is selected regardless of history.
- `bounded_text` is idempotent. `clean_prompt` is only a single cleanup pass:
  `x: :` becomes `x:` and then `x` on a second call. Appending is accumulation,
  not idempotent except for empty appends or a saturated buffer.
- Connector-name selection is permutation-invariant. Duplicate names may select
  different handles, and all-unnamed outputs deliberately use discovery order.
- Fitting returns empty when even an ellipsis cannot fit. Monotonicity concerns
  retained source characters, not the extra ellipsis character.
- Cover sampling allows integer crop/scale and nearest-neighbor rounding;
  generated pixel allocations stay below 4 KiB. Large dimensions exercise only
  geometry/checked arithmetic, without allocating large buffers.

The separate `fuzz/` crate is excluded from the workspace/default members and
`make verify`; CI is unchanged. Its `fuzzing` feature exposes only in-memory
entrypoints and does not enable iced, GStreamer, D-Bus, or session-lock. The
targets cover aggregate HEIC container preflight (not native image decoding),
XMP/XML and binary plist/Base64 parsing, and Desktop Entry validation/expansion.

The default flake shell pins stable Rust. To run a short campaign with nightly
and cargo-fuzz from the **same pinned Nix inputs**, use this at the repository root:

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

`make fuzz` seeds each target from the existing small HEIC fixtures and synthetic
parser examples. It runs each target for 10 seconds, caps input at 64 KiB, RSS at
512 MiB, individual allocations at 128 MiB and each input at five seconds. Set
`FUZZ_SECONDS=60` for a longer campaign. Mutable corpora, crash artifacts, and
build output are ignored; deterministic seeds and `fuzz/Cargo.lock` are tracked.
Preserve real crash artifacts, minimize them with `cargo fuzz tmin TARGET PATH`,
and add a regular unit regression before fixing production code.

These bounds contain the experiment; passing fuzz runs do **not** prove an
allocator bound. The production preflights enforce aggregate box/item/extent,
XML depth/event, plist object/depth/expanded-byte and metadata-byte budgets.
Existing adversarial unit tests plus the feature-gated one-over-budget tests
check enforcement directly; the container target also checks that acceptance
is monotone when budgets increase. The 64 KiB input cap does not explore the
full production source-size limit, and the targets never fuzz live PAM, greetd,
native libheif decode, graphics drivers, or arbitrary unlimited schedules.

## Safe UI preview

```sh
make dev
```

The default preview uses a fixed synthetic account, time, animation frame, and
Wayland session. It accepts input without sending it anywhere. Authentication
submission and all power actions are simulated, so UI testing cannot suspend,
restart, or shut down the development machine. Preview does not contact
AccountsService, greetd, or logind.

The ordinary preview renders a cached wallpaper poster without starting
GStreamer, or the generated background when no poster is cached. Neither the
development shell nor the package supplies media. Automated reference captures
use an explicit test-only cache of source JPEGs, not a production fallback.
Choose a deterministic fixture with `PREVIEW`:

```sh
PREVIEW=users make dev
PREVIEW=visible-prompt make dev
PREVIEW=authentication-failure make dev
PREVIEW=power-confirmation make dev
PREVIEW=power-menu make dev
PREVIEW=change-user make dev
```

Run `cargo run --bin genkan -- login --help` to list every fixture and login
option.
Pass an explicit `--username` only when a particular synthetic identity is
useful. To exercise real services, run Genkan without `--preview` in the
intended greeter environment.

## Safe lock preview

```sh
make lock-dev
LOCK_PREVIEW=challenge make lock-dev
LOCK_PREVIEW=submitting make lock-dev
LOCK_PREVIEW=failure make lock-dev
```

The lock preview uses the production lock renderer in a normal window without
requesting `ext-session-lock-v1` or contacting PAM. It renders the window as
the authentication output: the clock and identity column anchored to its edges
exactly as the session-lock runtime places them, at the login preview's fixed
time. Login and lock share the same composition, tokens, and Inter typeface;
login adds the session and power items and a stronger focused outline, and the
lock masks every response. Set `WIDTH`, `HEIGHT`, and `WALLPAPER` to inspect
responsive layouts, for example `WIDTH=1080 HEIGHT=1920 make lock-dev`.

## Download and inspect wallpapers

Media acquisition is explicit. The curated catalog uses original upstream
sources; Nix does not fetch them, and previews, login, lock, and wallpaper
playback never download them. Use the Cargo form below for an uninstalled build:

```sh
cargo run --bin genkan -- assets list
cargo run --bin genkan -- assets download iceland
cargo run --bin genkan -- assets preview iceland
cargo run --bin genkan -- assets download my-wallpaper --url "$SOURCE_URL"
cargo run --bin genkan -- assets preview my-wallpaper --reduce-motion
```

| Command | Network and validation | Local effect |
| --- | --- | --- |
| `assets list` / `assets preview` | Offline | Read catalog/cache; preview changes no settings |
| `assets preflight <key>` | Bounded range reads; one representative frame | Install nothing |
| `assets download <key>` | Complete transfer; resource-bounded hashing, ZIP parsing, and frame validation | Atomically select validated media, metadata, and a generated MOV poster |

Downloads require direct HTTP(S) responses with status 200 and reject redirects.
`--max-bytes` defaults to 2 GiB, accepts 4 KiB–16 GiB, and limits both the
transfer and expanded media; transfers time out after 600 seconds. Supplied
byte sizes and SHA-256 pins must match before publication. For custom URLs,
`--sha256 HASH` is optional; without it, the computed hash is an observation,
not a trusted upstream pin. Custom keys cannot replace keys in the selected
catalog. Observed format, dimensions, byte size, and hash persist in cache
`entries/<key>.toml` receipts and are available to subsequent list/preview
commands. A matching verified cache entry can be reused.

Cache selection and greeter provisioning are documented under
[wallpaper configuration](deployment.md#wallpaper-configuration). Downloads do
not change the selected login/lock wallpaper. Missing media retains the
generated background; `assets preview` reports the missing path instead.

### Cache publication and interrupted downloads

Each download prepares a new generation under
`objects/download-<key>-<random>/`, containing `<key>.mov` or `<key>.heic`, a
`<key>-poster.jpg` for MOV, and metadata. Only after preparation succeeds does
`entries/<key>.toml` atomically select that complete generation. Readers use one
receipt snapshot for metadata, media, and poster. Preparation or publication
failures leave the previous generation usable. This does not
promise simultaneous display changes across processes or filesystem durability
after power loss.

Use `assets preview <key>` or the absolute media path printed by download with
`--wallpaper-file`; do not construct a downloaded path under `mov/` or `heic/`.
Those conventional directories remain supported for explicitly provisioned
local/test files. Published generations are retained so in-flight previews,
deferred HEIC opens, and saved absolute overrides remain valid. Replacement
versions may occupy additional disk space; remove an unused version only when
no readers or saved overrides need it.

Ctrl+C or SIGTERM stops the preparation processes and cleans staging. After
SIGKILL or a crash, the next explicit download cleans only abandoned,
unpublished staging under the cache lock, never previously published
generations. Playback never performs cache cleanup. Preparation has resource
and time limits, including ZIP parsing and hashing, so a timeout does not
publish partially prepared media.

### Use a custom TOML catalog

`--catalog sources.toml` replaces the curated entries. Only `url` is required;
this illustrative entry uses a placeholder URL:

```toml
[assets.mountains]
url = "https://source.example/mountains.mov"
name = "My Mountains"
format = "mov"
width = 3840
height = 2160
byte_size = 790758290
duration_seconds = 120.0
rights = "Record the source's actual usage terms here"
attribution = "Original author"
# sha256 = "64 hexadecimal digits from a trusted source"
# source_page_url = "https://source.example/mountains"
# preview_url = "https://source.example/mountains.jpg"
```

```sh
genkan assets list --catalog sources.toml
genkan assets preflight mountains --catalog sources.toml
genkan assets download mountains --catalog sources.toml
genkan assets preview mountains --catalog sources.toml
```

Unknown metadata is shown as `?`. Supplied format, byte size, and dimensions
are checked against observations; duration, rights, attribution, and preview
URLs are informational. Poster URLs are not fetched. Receipts fill missing
observations but never override a curator's URL, pins, or geometry. Custom
`--url` downloads appear in the default list; a replacement catalog shows its
own entries, not unrelated cached custom keys.

A ZIP source needs `archive_member` (the exact member name),
`archive_byte_size`, and `archive_sha256`, plus the extracted media's
`byte_size` and `sha256`. Only that configured member is extracted into a
controlled local filename; archive paths are never unpacked. Both archive and
member pins are verified. Arbitrary ZIP discovery is not supported.

### Preflight is optional, not an installation gate

`genkan assets preflight <key> [--url URL]` uses bounded HTTP range reads,
including tail metadata when needed. It decodes one HEVC MOV frame or a HEIC
primary image with Genkan's parser/color conversion and inspects HEIC scheduling
metadata. It installs nothing and does not persist custom `--url` entries.

The default media-body budget is 8 MiB; `--max-bytes` accepts 4 KiB–64 MiB.
There are at most 128 range requests and a 35-second worker deadline. Headers,
TLS, and transport overhead are outside the media-byte counter. Multiple reads
require a consistent strong ETag for a pass. Redirects, ignored or malformed
ranges, changed sources, TLS failures, and exhausted budgets are inconclusive.
Cancellation stops the worker and its request. Apple's root certificate is
supplied only to curl requests for the exact HTTPS host `sylvan.apple.com`;
it does not change system trust or disable certificate/hostname validation.

Exit codes are **0: passed**, **2: unsupported or catalog mismatch**,
**3: inconclusive**, and **1: command/configuration error**. A pass does not
verify the whole-file hash, every frame, or a particular GPU renderer. ZIPs
cannot be partially inspected as media, but a configured ZIP download can
still pass full archive/member integrity checks and local frame validation.
An inconclusive or unsupported preflight never triggers or prevents an explicit
download; that command independently validates the complete file before
publication. Neither command proves every frame decodes.

## Animated wallpaper preview

Real wallpaper playback can be enabled without making preview authentication
or power actions real. Download the selected originals into your cache first:

```sh
cargo run --bin genkan -- assets download tahoe-beach
make animated-dev
cargo run --bin genkan -- assets download sequoia-sunrise
WALLPAPER=sequoia-sunrise make animated-dev
```

`--wallpaper-file` accepts an existing absolute MOV or HEIC file. MOV looping
uses the actual stream duration; HEIC uses its own scheduling metadata.
For downloaded files, use the absolute path printed by download, not
a hardcoded cache layout. URIs, playlists, and GStreamer pipeline descriptions
are not accepted.
Runtime playback never downloads media or invokes a shell. With an empty cache,
`make animated-dev` remains usable but shows the generated background.

## Verification

Run all local checks:

```sh
make verify
```

Run `make test` (including its invocation by `make verify`) in the foreground.
A background shell job can inherit ignored SIGINT and spuriously fail the
reference-image interruption regression. Before a Nix build in a colocated jj
checkout, describe the change and run `jj new`: Nix source filtering uses
Git-tracked files, while new jj working-copy files are not yet tracked by Git.
This matters especially for newly added reference images and fixtures.

`make smoke` runs the packaged-binary graphics check used by both architecture
jobs in CI. It starts headless Weston, nests Cage with its pixman renderer, and
launches Genkan using Mesa's software Vulkan driver. Genkan must remain alive
until the controlled timeout. The check requires iced's wgpu backend and proves
that Cage launched the packaged process with the intended Vulkan driver.
It also compares animated wallpaper crops with the static poster, requires
multiple distinct nonblank frames from an authored HEVC fixture, and bounds
short-term RSS growth. Production launches are also checked without a fixture
cache. Those checks do not constitute the sustained performance or input-latency acceptance
pass tracked in [Deferred Work](deferred.md).

`make wallpaper-smoke` runs the headless layer-shell desktop check. It launches
the packaged `genkan wallpaper` under headless Sway with two differently scaled
outputs, exercises scale, transform, and mode changes without recreating a
surface, compares aspect-preserving cover rendering against independent
ImageMagick references, and confirms that a foreground `genkan lock` fully
occludes the desktop client and that explicit unlock restores it. It emits
`mixed-scale-landscape.png`, `mixed-scale-portrait.png`, `ultrawide.png`,
`locked-landscape.png`, `locked-portrait.png`, and the
`transition-before.png`, `transition-during.png`, and `transition-after.png`
dissolve frames for review, and it makes no GeoClue or session-bus request. The
transition capture places local wall-clock time twenty seconds before the
fixture's 06:00 boundary with a second-precision POSIX `TZ` offset, so a slow
software-rendered startup still presents the red frame before the dissolve
begins. Each phase is captured in order and must be a near-uniform,
blend-compatible field matching the fixture's palette and red-plus-green
intensity, so an unrelated or spatially split frame cannot stand in for a phase
that never happened. Screenshot evidence cannot distinguish a fabricated uniform
frame from a real dissolve, so this bounds the failure modes rather than proving
the renderer's internal path.

`make check-rfds` also validates the wallpaper catalog manifest. The loop
analysis must record the method that produced each `endpoint_ssim`, the
reproducible set must name exactly the catalog entries, every entry must record
a plausible endpoint measurement with its verification, `default_wallpaper` must
name an entry, and no catalog asset may opt out of decode verification. The
check validates the recorded claims rather than re-measuring them, so it stays
cheap enough for every change; it does not fetch upstream media.

`make wallpaper-package` checks that the production wallpaper tree contains
only the manifest and the runtime closure contains neither catalog artwork nor
a test cache. `make wallpaper-empty-cache` checks the unmodified production
wrapper with empty HOME, XDG, and override caches: preview remains usable,
opens no artwork, and starts no downloader or network request.

`make heic-assets` verifies the byte sizes and SHA-256 hashes of the authored,
repository-delivered HEIC fixtures. `make heic-decode` opens those fixtures
through the shipped parser and decodes all their frames, checking the count
against the manifest's `structure.image_count`. A fixture declaring
`decode_verified = false` fails rather than being skipped. These checks use an
explicit test cache; remote catalog records are not downloaded or revalidated.

Authentication changes should also run the x86_64 NixOS VM test:

```sh
make e2e
```

The VM boots real greetd 0.10.3 with its NixOS PAM configuration. A
feature-gated driver submits an incorrect password, cancels and recreates the
greetd session, authenticates successfully, and starts a marker session. It
verifies the launched user and serialized session environment; it does not
exercise iced rendering or input automation.

## Physical graphics smoke

Hosted CI has no DRM devices, so physical hardware testing is opt-in:

```sh
make hardware-smoke
```

Run it from a Wayland session. It tests the system AMD and NVIDIA Vulkan ICD
once per detected vendor, launches packaged Genkan under Vulkan-rendered nested
Cage when that vendor has a display-connected adapter, and verifies that an
AMD-only run does not load or open the NVIDIA driver.
The script passes `--reduce-motion`; validate animated playback separately
before claiming physical animation coverage.

On Sway, the stricter invocation below also moves Cage to every active output,
verifies its resulting tree location, and requires both GPU vendors and an
active external display:

```sh
GENKAN_REQUIRE_GPU_VENDORS='1002 10de' \
GENKAN_REQUIRE_EXTERNAL_DISPLAY=1 \
GENKAN_EXERCISE_SWAY_OUTPUTS=1 \
make hardware-smoke
```

A disconnected hybrid GPU can validate its physical Vulkan driver but cannot
exercise presentation. Physical ARM graphics validation remains tracked in
[Deferred Work](deferred.md#physical-arm-graphics-validation).
