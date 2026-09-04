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

To install the session locker and define its dedicated PAM service using the
host's normal authentication policy, import and enable the opt-in module:

```nix
{
  imports = [ genkan.nixosModules.default ];
  programs.genkan.enable = true;
}
```

The module does not configure an idle manager or replace a desktop
environment's own locker. Its private authentication worker is unprivileged
and is not installed setuid.

The supported production lock runtime requires Linux 5.9 or newer and glibc
2.34 or newer for pidfd, `close_range`, and descriptor-safe `posix_spawn`
worker management. The pinned Nix package satisfies the userspace requirement;
source builds on older systems fail closed before PAM authentication begins.

## Session locking and suspend

Run `genkan lock` in the foreground for direct use. For an idle or suspend
hook, `genkan lock --daemonize` starts a fresh foreground child and returns only
after the compositor confirms that the session lock is active. It does not
`fork` after Wayland, PAM, graphics, or wallpaper initialization. Concurrent
manual and suspend hooks join the same per-compositor lifecycle and all wait
for that confirmation; lock denial or child loss before confirmation is an
error.

The confirmed foreground child remains the lock owner while the system sleeps
and after it resumes. Resume does not start a second authentication attempt or
unlock the session; the existing PAM conversation continues only when the user
interacts with the lock screen.

For Sway and other sessions using swayidle, use its wait mode so the
before-sleep command participates in swayidle's logind delay inhibitor:

```sh
swayidle -w \
  lock 'genkan lock --daemonize' \
  before-sleep 'genkan lock --daemonize'
```

This is bounded, best-effort suspend integration rather than a suspend veto.
Swayidle does not inspect the command's exit status, and logind proceeds when
`InhibitDelayMaxSec` expires even if the command is still running. Configure
that timeout above the host's worst-case locker startup time and monitor
launcher failures. A startup failure can still let the machine suspend without
a confirmed Genkan lock; deployments that require fail-closed suspension must
route suspend requests through a component that locks first and requests
suspend only after confirmation.

Starting Genkan from an after-resume hook is not safe: session content may
already have been exposed before the locker starts.

Genkan detects support from the connected Wayland compositor at runtime. It
requires `ext_session_lock_manager_v1` and the rendering globals used by its
session-lock runtime; it does not guess from the compositor name and has no
fullscreen or layer-shell fallback. The intended niri target and current
ext-session-lock-capable releases of Sway, River, Hyprland, and other wlroots
compositors are expected to work. GNOME Shell, KWin releases without the
protocol, and desktop environments that reserve locking for an internal shell
must continue using their own locker.

If Genkan, its renderer, or the compositor connection fails after lock
confirmation, Genkan never requests unlock. The compositor therefore remains
responsible for fail-closed recovery and may leave a blank or fallback lock
screen. Switch to another VT (for example with Ctrl+Alt+F2), sign in there, and
restart the trusted locker/compositor or terminate the affected graphical
session. Do not kill the locker expecting that to reveal the session.

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
poster without initializing GStreamer. If playback fails before the first video
frame, Genkan reports the failure and keeps the poster, or the generated
background when the poster is unavailable. After playback begins, a failure
retains the last displayed frame instead of briefly replacing it with the
poster.

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
