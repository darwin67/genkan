# Genkan

Genkan is a small graphical [greetd](https://sr.ht/~kennylevinsen/greetd/)
frontend built with Rust and iced. It is intended to run fullscreen under Cage
on a Wayland login VT. greetd remains responsible for PAM authentication and
starting the selected desktop session; Genkan never validates credentials or
runs the user session itself.

## Development preview

```sh
make dev
```

The default preview uses `$USER` as a synthetic account and accepts password
input without sending it anywhere. Authentication submission and all power
actions are simulated, so testing the UI cannot suspend, restart, or shut down
the development machine. Preview also uses a synthetic Wayland session and does
not contact AccountsService, greetd, or logind.

Select deterministic fixtures with `PREVIEW`, for example:

```sh
PREVIEW=users make dev
PREVIEW=visible-prompt make dev
PREVIEW=authentication-failure make dev
PREVIEW=power-confirmation make dev
```

Run `cargo run --bin genkan -- --help` to list every fixture. To exercise real
AccountsService, greetd, and logind behavior, run the binary directly without
`--preview` in the intended greeter environment.

The Makefile also provides `check`, `fmt`, `fmt-fix`, `lint`, `test`, `smoke`,
`e2e`, `build`, `package`, `verify`, `changelog`, `next-version`, and `clean`
targets. Enter `nix develop` manually if direnv has not already loaded the
flake environment.

## greetd configuration

A minimal flake binds Genkan as an input and passes it to the host module:

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    genkan = {
      url = "github:darwin67/genkan";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { nixpkgs, genkan, ... }: {
    nixosConfigurations.hostname = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux"; # or aarch64-linux
      specialArgs = { inherit genkan; };
      modules = [ ./configuration.nix ];
    };
  };
}
```

The corresponding `configuration.nix` can run the package fullscreen under
Cage:

```nix
{ config, genkan, pkgs, ... }:

let
  genkanPackage = genkan.packages.${pkgs.stdenv.hostPlatform.system}.default;
in

services.greetd = {
  enable = true;
  settings.default_session = {
    user = "greeter";
    command = "${pkgs.cage}/bin/cage -- ${genkanPackage}/bin/genkan";
  };
};

services.accounts-daemon.enable = true;
hardware.graphics.enable = true;

# Install at least one Wayland session and expose its generated desktop entry
# to the pre-authentication greetd service.
services.displayManager.sessionPackages = [ pkgs.niri ];
systemd.services.greetd.environment.XDG_DATA_DIRS =
  "${config.services.displayManager.sessionData.desktops}/share";

# Do not replace the active greeter during nixos-rebuild switch.
systemd.services.greetd.restartIfChanged = false;
```

greetd supplies `GREETD_SOCK`. Genkan handles every PAM prompt in sequence,
then requests the selected session with the Wayland XDG environment. Power
buttons call logind over the system D-Bus; sleep, restart, and shutdown all
require confirmation. The calls do not request interactive polkit
authentication, so the greeter user must already be authorized by system
policy. A denial or unavailable system bus leaves the active authentication
attempt intact and displays the logind error.

Genkan discovers cached, unlocked, non-system login users through
AccountsService. Selection precedence is an administrative `--username`
override, the uniquely most recent eligible account when every eligible
account has usable login recency, and then a sole eligible account. Genkan
presents account selection when recency is missing, zero, or tied.
`--display-name` remains an optional companion to `--username`. The greeter
renders initials from bounded account labels rather than decoding
user-controlled icon files in the credential-handling process.

Wayland sessions come exclusively from validated `wayland-sessions/*.desktop`
entries in `XDG_DATA_DIRS`. Genkan honors directory precedence and hidden-entry
masking, validates `Type`, visibility, `TryExec`, and executable availability,
requires slash-containing executable paths to be absolute, and applies Desktop
Entry quoting and field-code rules without invoking a shell. The greetd unit
must expose the desktop-entry directory through `XDG_DATA_DIRS`, as in the
example. If AccountsService is unavailable, no eligible cached account exists,
no valid session is installed, or `GREETD_SOCK` is absent, the greeter reports
the specific configuration or transport error instead of supplying a
host-specific fallback.

The Nix package adds `/run/opengl-driver/lib` to the executable's driver
runpath and advertises the matching Vulkan ICD directory. It therefore uses
the Mesa or NVIDIA driver selected by the NixOS `hardware.graphics`
configuration; it does not bundle or activate a vendor driver. Cage and the
host graphics stack must also support the selected hardware. CI launches the
packaged x86_64 and aarch64 binaries under nested Cage and headless Weston with
Mesa software Vulkan. Physical hardware is intentionally opt-in because hosted
CI has no DRM devices:

```sh
make hardware-smoke
```

Run it from a Wayland session. It tests the system AMD and NVIDIA Vulkan ICD
once per detected vendor, launches the packaged Genkan under Vulkan-rendered
nested Cage when that vendor has a display-connected adapter, and checks that
an AMD-only run does not load or open the NVIDIA driver. On Sway, this stricter
invocation also moves Cage to every active output, verifies its resulting tree
location, and requires both GPU vendors and an active external display:

```sh
GENKAN_REQUIRE_GPU_VENDORS='1002 10de' \
GENKAN_REQUIRE_EXTERNAL_DISPLAY=1 \
GENKAN_EXERCISE_SWAY_OUTPUTS=1 \
make hardware-smoke
```

A disconnected hybrid GPU can validate its physical Vulkan driver but cannot
exercise presentation. Physical ARM hardware remains a manual coverage gap;
aarch64 CI uses software rendering.

## Validation

```sh
make verify
```

Authentication changes should also run the x86_64 NixOS VM test:

```sh
make e2e
```

The VM boots real greetd 0.10.3 with its NixOS PAM configuration. A
feature-gated driver using Genkan's authentication client submits an incorrect
password, cancels and recreates the greetd session, authenticates successfully,
and starts a marker session. The test verifies the launched user and serialized
session environment. It does not exercise iced rendering or input automation.

`make smoke` runs the packaged-binary graphics check used by both architecture
jobs in CI. It starts headless Weston, nests Cage with its pixman renderer, and
launches Genkan using Mesa's software Vulkan driver. Genkan must remain alive
until the check's controlled timeout; `ICED_BACKEND=wgpu` prevents a successful
tiny-skia fallback from masking Vulkan failure. A child PID marker and mapped
Lavapipe library prove Cage started Genkan and iced initialized the intended
driver. Early compositor, loader, or application failure fails the derivation.
