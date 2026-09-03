# Deployment and greetd Configuration

## NixOS flake input

Bind Genkan as a flake input and pass it to the host module:

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

## Host configuration

Run the package fullscreen under Cage and expose at least one Wayland desktop
entry to the pre-authentication service:

```nix
{ config, genkan, pkgs, ... }:

let
  genkanPackage = genkan.packages.${pkgs.stdenv.hostPlatform.system}.default;
in

services.greetd = {
  enable = true;
  settings.default_session = {
    user = "greeter";
    command = "${pkgs.cage}/bin/cage -- ${genkanPackage}/bin/genkan login";
  };
};

services.accounts-daemon.enable = true;
hardware.graphics.enable = true;

services.displayManager.sessionPackages = [ pkgs.niri ];
systemd.services.greetd.environment.XDG_DATA_DIRS =
  "${config.services.displayManager.sessionData.desktops}/share";

# Do not replace the active greeter during nixos-rebuild switch.
systemd.services.greetd.restartIfChanged = false;
```

greetd supplies `GREETD_SOCK`. Genkan handles each PAM prompt in sequence and
then asks greetd to start the selected session with the Wayland XDG environment.
If AccountsService, a valid session, or the greetd socket is unavailable,
Genkan reports the specific configuration or transport failure rather than
using a host-specific fallback.

## Accounts and sessions

Genkan discovers cached, unlocked, non-system users through AccountsService.
Selection precedence is:

1. An administrative `--username` override.
2. The uniquely most recent eligible account when every account has usable
   login recency.
3. A sole eligible account.

It presents account selection when recency is missing, zero, or tied.
`--display-name` is an optional companion to `--username`. Initials are rendered
from bounded labels; user-controlled icon files are not decoded in the
credential-handling process.

Wayland sessions come exclusively from validated
`wayland-sessions/*.desktop` entries in `XDG_DATA_DIRS`. Genkan honors directory
precedence and hidden-entry masking; validates type, visibility, `TryExec`, and
executable availability; applies Desktop Entry quoting and field-code rules;
and never invokes a shell.

## Power policy

Sleep, restart, and shutdown call logind over the system D-Bus and always
require confirmation. Calls do not request interactive polkit authentication,
so the greeter user must already be authorized by system policy. A denial or
unavailable system bus leaves the authentication attempt intact and displays
the logind error.

## Wallpaper configuration

Tahoe Beach animates by default. Select another packaged catalog entry with:

```text
--wallpaper sequoia-sunrise
--wallpaper sequoia-morning
--wallpaper sequoia-night
```

`--reduce-motion` (also available as `--static-wallpaper`) shows the selected
poster without initializing GStreamer. If playback fails, Genkan reports the
failure and returns to the poster. If the poster is unavailable, it retains the
generated background.

The package installs immutable, hash-pinned wallpaper inputs; runtime playback
does not access the network. Asset provenance, delivery, integrity, and loop
behavior are documented in [RFD 2](../rfd/0002/README.adoc).

## Graphics runtime

The Nix package adds `/run/opengl-driver/lib` to the executable's driver runpath
and advertises the matching Vulkan ICD directory. It uses the Mesa or NVIDIA
driver selected by the host's `hardware.graphics` configuration; it does not
bundle or activate a vendor driver. Cage and the host graphics stack must also
support the selected hardware.

CI launches packaged x86_64 and aarch64 binaries under nested Cage and headless
Weston with Mesa software Vulkan. See the
[development guide](development.md#physical-graphics-smoke) for optional
physical AMD, NVIDIA, and connected-display validation.
