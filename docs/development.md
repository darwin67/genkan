# Development and Verification

## Development environment

Enter the pinned Nix shell before running Cargo or Make targets:

```sh
nix develop
```

The Makefile provides `dev`, `animated-dev`, `check`, `fmt`, `fmt-fix`, `lint`,
`test`, `scripts-test`, `check-rfds`, `smoke`, `evidence`,
`update-reference-images`, `hardware-smoke`, `e2e`, `build`, `package`,
`verify`, `changelog`, `next-version`, and `clean` targets.

## Safe UI preview

```sh
make dev
```

The default preview uses a fixed synthetic account, time, animation frame, and
Wayland session. It accepts input without sending it anywhere. Authentication
submission and all power actions are simulated, so UI testing cannot suspend,
restart, or shut down the development machine. Preview does not contact
AccountsService, greetd, or logind.

The ordinary preview renders a fixed wallpaper poster without starting
GStreamer so screenshots remain stable. Choose a deterministic fixture with
`PREVIEW`:

```sh
PREVIEW=users make dev
PREVIEW=visible-prompt make dev
PREVIEW=authentication-failure make dev
PREVIEW=power-confirmation make dev
```

Run `cargo run --bin genkan -- login --help` to list every fixture and login
option.
Pass an explicit `--username` only when a particular synthetic identity is
useful. To exercise real services, run Genkan without `--preview` in the
intended greeter environment.

## Animated wallpaper preview

Real wallpaper playback can be enabled without making preview authentication
or power actions real. The Nix development shell exposes the pinned videos
through `GENKAN_WALLPAPER_DIR`:

```sh
make animated-dev
WALLPAPER=sequoia-sunrise make animated-dev
WALLPAPER=sequoia-morning make animated-dev
WALLPAPER=sequoia-night make animated-dev
```

`--wallpaper-file` accepts only an existing absolute `.mov` file. It replaces
the selected catalog entry's video while retaining that entry's verified
duration and crossfade metadata, so the file must be a local copy of the same
catalog asset. URIs, playlists, and GStreamer pipeline descriptions are not
accepted. Runtime playback never downloads media or invokes a shell.

## Verification

Run all local checks:

```sh
make verify
```

`make smoke` runs the packaged-binary graphics check used by both architecture
jobs in CI. It starts headless Weston, nests Cage with its pixman renderer, and
launches Genkan using Mesa's software Vulkan driver. Genkan must remain alive
until the controlled timeout. The check requires iced's wgpu backend and proves
that Cage launched the packaged process with the intended Vulkan driver.

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
