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

The preview uses the host's AccountsService and installed Wayland session
entries. Pass `--username "$USER"` to bypass account discovery. Once an account
and session are available, authentication begins immediately and reports that
`GREETD_SOCK` is missing, as expected outside greetd.

The Makefile also provides `check`, `fmt`, `fmt-fix`, `lint`, `test`, `e2e`,
`build`, `package`, `verify`, `changelog`, `next-version`, and `clean` targets.
Enter `nix develop` manually if direnv has not already loaded the flake
environment.

## greetd configuration

A minimal NixOS integration looks like:

```nix
{ config, pkgs, ... }:

services.greetd = {
  enable = true;
  settings.default_session = {
    user = "greeter";
    command = "${pkgs.cage}/bin/cage -- ${genkan}/bin/genkan";
  };
};

services.accounts-daemon.enable = true;

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
require confirmation. Authorization failures leave the active authentication
attempt intact and display the logind error.

Genkan discovers cached, unlocked, non-system login users through
AccountsService. It automatically selects a sole account and presents a
selector when multiple accounts are available. `--username` and
`--display-name` remain optional administrative overrides. The greeter renders
initials from bounded account labels rather than decoding user-controlled icon
files in the credential-handling process.

Wayland sessions come exclusively from validated `wayland-sessions/*.desktop`
entries in `XDG_DATA_DIRS`. Genkan honors directory precedence and hidden-entry
masking, validates `Type`, visibility, `TryExec`, and executable availability,
requires absolute slash-containing executable paths, and applies Desktop Entry
quoting and field-code rules without invoking a shell. If AccountsService,
session data, or another runtime dependency is unavailable, the greeter reports
a configuration error instead of supplying a host-specific fallback.

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
