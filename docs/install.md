# Install and Configure Genkan

Genkan installs with Nix. On NixOS, its module configures greetd, runs the
greeter under Cage (or [niri](niri.md)), and installs the PAM policy for
[`genkan lock`](lock.md). Start with [Install on NixOS](#install-on-nixos);
other Linux hosts with Nix can use a [prebuilt release](#install-a-prebuilt-release).

After installing, the greeter shows a generated background until you
[provision a wallpaper](wallpapers.md#provision-the-greeters-wallpaper).

## Install on NixOS

Add Genkan as a flake input and pass it to the host module:

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

In `configuration.nix`, import the module, enable the greeter, and enable at
least one Wayland desktop session. This example logs in to niri while keeping
Cage as the greeter compositor:

```nix
{ genkan, ... }:
{
  imports = [ genkan.nixosModules.default ];
  programs.genkan = {
    enable = true;
    greeter.enable = true;
  };
  programs.niri.enable = true;
}
```

The module enables AccountsService and graphics, and gives greetd the host's
Wayland session directory. Remove any manually configured
`services.greetd.settings.default_session.command` so the module can set it.

| Option | Effect |
| --- | --- |
| `programs.genkan.enable` | Installs the package and the `genkan-lock` PAM policy |
| `programs.genkan.greeter.enable` | Configures greetd to run Genkan |
| `programs.genkan.greeter.compositor` | `"cage"` (default) or `"niri"`; see [niri](niri.md#greeter) |
| `programs.genkan.greeter.authenticationOutput` | The monitor that shows the login prompt; see [below](#choose-the-authentication-monitor) |
| `programs.genkan.wallpaper.source` | The greeter's wallpaper; see [provisioning](wallpapers.md#provision-the-greeters-wallpaper) |

To install only the package and lock PAM policy, set
`programs.genkan.enable = true` without enabling the greeter. The module does
not configure an idle manager or replace a desktop environment's own locker.

### Apply greeter changes

The module sets `systemd.services.greetd.restartIfChanged = false`, so
switching generations neither terminates an active desktop session nor replaces
the running greeter. greetd keeps its generation-specific command in memory,
even after logout. To activate a changed greeter configuration or binary,
restart greetd when no desktop session is active, or reboot. Restarting greetd
while a desktop session is active terminates it.

## Install a prebuilt release

[GitHub Releases](https://github.com/darwin67/genkan/releases) provides prebuilt
Nix packages for `x86_64-linux` and `aarch64-linux`. Each archive includes
Genkan, its PAM helper, assets, and native runtime dependencies. It requires
Nix on the host; it is not a standalone executable for Linux without Nix.

Download the matching `.nix-export.zst` and `SHA256SUMS` from the same release.
Verify the checksum before importing, and only import a release you trust:

```bash
(
  set -euo pipefail
  archive=genkan-v0.1.0-x86_64-linux.nix-export.zst  # Adjust tag and architecture.
  grep -F "  $archive" SHA256SUMS | sha256sum --check -
  genkan_path=$(zstd -dc -- "$archive" | nix-store --import | tail -n 1)
  nix-store --realise "$genkan_path" --add-root "$PWD/genkan-package"
  ./genkan-package/bin/genkan --version
)
```

For a multi-user Nix install that rejects unsigned imports, run the import as a
trusted Nix user. Importing does not configure greetd, PAM, or drivers; provide
the equivalent of the [NixOS host configuration](#install-on-nixos) before
enabling login or locking.

## Choose the authentication monitor

With several monitors, Genkan shows the login or lock prompt on one and plain
wallpaper on the rest. Set `programs.genkan.greeter.authenticationOutput` for
either greeter, or pass `--authentication-output` to `genkan login` or
`genkan lock`. Name a connector such as `DP-3`, or a persistent
`make model serial` name such as `Dell Inc. DELL U3223QE YOUR-SERIAL`, which
survives connector renumbering by docks and cables. `niri msg outputs` in a
niri session shows both forms.

Under Cage and in the lock, names match case-insensitively, with `Unknown` for
missing fields. Persistent names need wlroots output management or niri's IPC;
without either, use a connector name. When the preferred monitor is absent or
unset, Genkan picks an external connector by type and number (`DP-2` before
`DP-10`), then an internal panel. The [niri greeter](niri.md#greeter) instead
lets niri place the login window on the focused output.

Wallpaper and lock render at the compositor's preferred fractional scale when
it offers the fractional-scale and viewporter protocols, using logical
viewport destinations, and fall back to integer buffers otherwise.

## Set the keyboard layout and cursor

Under Cage, set the greeter's keyboard layout and cursor on greetd when they
differ from the defaults:

```nix
systemd.services.greetd.environment = {
  XKB_DEFAULT_LAYOUT = "us";
  XCURSOR_THEME = "Adwaita";
  XCURSOR_SIZE = "24";
};
```

The niri greeter instead takes the layout from `org.freedesktop.locale1`, which
follows `services.xserver.xkb`, and draws niri's default cursor.

## Accounts

Genkan lists cached, unlocked, non-system users from AccountsService and
preselects one when it can:

1. The administrative `--username` override, with an optional `--display-name`.
2. Otherwise, the uniquely most recent account, when every account has usable
   login recency.
3. Otherwise, the only eligible account.

If recency is missing, zero, or tied, it shows account selection.

### Account pictures

Each user sets their own picture from their desktop session, without `sudo`:

```sh
genkan avatar list
genkan avatar set lake
genkan avatar set --file /home/alice/Pictures/photo.jpg
genkan avatar clear
```

`list` shows the built-in pictures. `set` changes only the calling user's
AccountsService picture, under its normal polkit `change-own-user-data` policy.
Photos must be PNG or JPEG files up to 1 MiB and 4096 pixels per side; a
sandboxed child crops the centered square to 512 by 512, and the command
re-encodes a metadata-free PNG, rejects output over 1 MiB, and removes its
private temporary file after AccountsService copies it, also on failure. Login and lock show the
picture on their next start. `clear`, or a missing or malformed picture, shows
initials.

## Desktop sessions

The session menu lists only validated `wayland-sessions/*.desktop` entries
from `XDG_DATA_DIRS`. Genkan honors directory precedence and hidden entries,
validates type, visibility, `TryExec`, and executable availability, and passes the Wayland XDG session
environment to greetd before starting the session. It never adds a compositor
command or runs a shell itself; see
[session launch](architecture.md#session-launch) for how arguments reach
greetd.

greetd supplies the socket in `GREETD_SOCK`. If AccountsService, a valid
session, or the greetd socket is unavailable,
Genkan reports that failure rather than falling back to a host-specific
default.

## Power menu

Sleep, Restart, and Shut Down are in the power menu at the upper right and call
logind over the system bus. Restart and Shut Down ask for confirmation; Sleep
acts immediately because it cannot lose work. Calls do not request interactive
polkit authentication, so system policy must already authorize the greeter
user. A denial or unavailable bus shows the logind error and keeps the
authentication attempt.

## Graphics drivers

The package adds `/run/opengl-driver/lib` to its driver runpath and advertises
the matching Vulkan ICD directory, so it uses the Mesa or NVIDIA driver from
the host's `hardware.graphics` configuration. It bundles no vendor driver, and
Cage and the host graphics stack must support the hardware. CI launches the
packaged x86_64 and aarch64 binaries with Mesa software Vulkan; see the
[physical graphics smoke](development.md#physical-graphics-smoke) to check real
GPUs and displays.
