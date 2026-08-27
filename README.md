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

The preview renders without greetd. Submitting the password field reports that
`GREETD_SOCK` is missing, as expected.

The Makefile also provides `check`, `fmt`, `fmt-fix`, `lint`, `test`, `build`,
`package`, `verify`, `changelog`, `next-version`, and `clean` targets. Enter
`nix develop` manually if direnv has not already loaded the flake environment.

## greetd configuration

A minimal NixOS integration looks like:

```nix
services.greetd = {
  enable = true;
  settings.default_session = {
    user = "greeter";
    command = "${pkgs.cage}/bin/cage -- ${genkan}/bin/genkan --username darwin --display-name Darwin";
  };
};

# Do not replace the active greeter during nixos-rebuild switch.
systemd.services.greetd.restartIfChanged = false;
```

greetd supplies `GREETD_SOCK`. Genkan handles every PAM prompt in sequence,
then requests the selected session with the Wayland XDG environment. Power
buttons call logind over the system D-Bus; restart and shutdown require
confirmation.

The default session command is `sway --unsupported-gpu`. Override it with
`--session-command`, or select another entry discovered from
`wayland-sessions/*.desktop`.

## Validation

```sh
make verify
```
