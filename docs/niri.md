# niri

Genkan works with niri in three roles: as the greeter compositor on the login
VT, as the desktop session you log in to, and as the host for the desktop
wallpaper and session lock. Each is configured separately. These notes were
checked against niri 26.04.

## Greeter

Cage remains the default greeter compositor. Select niri for per-output mode,
scale, rotation, and position on the login screen; this implements
[RFD 6](../rfd/0006/README.adoc). Output blocks are structured options, not
arbitrary KDL, so they cannot introduce includes or launch bindings:

```nix
{ config, genkan, ... }:
{
  imports = [ genkan.nixosModules.default ];
  programs.genkan = {
    enable = true;
    greeter = {
      enable = true;
      compositor = "niri"; # omit this to retain Cage
      authenticationOutput = "Dell Inc. DELL U3223QE YOUR-SERIAL";
      wallpaper = "tahoe-beach";
      niri.outputs = {
        "Dell Inc. DELL U3223QE YOUR-SERIAL" = {
          mode = "3840x2160@60";
          scale = 1.275;
          position = { x = 1920; y = 0; };
        };
        "DP-10" = {
          mode = "1920x1080@144";
          scale = 1;
          position = { x = 0; y = 0; };
        };
        "DP-11" = { scale = 1.3; transform = "90"; };
        "eDP-1" = { scale = 1.2; };
      };
    };
  };
  programs.niri.enable = true; # the user's session, separate from the greeter
}
```

See [Choose the authentication monitor](install.md#choose-the-authentication-monitor)
for monitor names. When the preferred monitor is absent or unset, niri places
the login window on the focused output.

The module generates output blocks and a window rule such as:

```kdl
output "Dell Inc. DELL U3223QE YOUR-SERIAL" {
    mode "3840x2160@60"
    scale 1.275
    transform "normal"
    position x=1920 y=0
}

window-rule {
    match app-id="^genkan$"
    open-on-output "Dell Inc. DELL U3223QE YOUR-SERIAL"
    open-fullscreen true
}
```

`config.programs.genkan.greeter.niri.outputConfig` exposes exactly the generated
output blocks for reuse in your desktop niri configuration. Do not reuse the
whole greeter configuration as a desktop configuration.

### What the greeter runs

The module starts only `genkan greeter-session`. It starts the login window
(app ID `genkan`, with `--placement compositor`, which treats the whole window
as the authentication region) and a background client
for the other outputs, without a shell, and asks niri to quit whenever login
exits, including a crash. Wallpaper failure and SIGINT or SIGTERM also end the
greeter. It never starts a desktop: greetd alone owns authentication and
`StartSession`, and waits for niri to release the displays before starting the
session.

The greeter uses its own immutable configuration with empty `binds`, no
startup hotkey overlay, `screenshot-path null`, Xwayland disabled, and no
includes. It is validated with the configured niri package at build time **and
immediately before launch**, because niri 26.04 falls back to built-in launch
bindings on a startup parse error; see the
[investigation](../rfd/0006/INVESTIGATION.adoc). Never point greetd directly at
a user-writable niri configuration. Greeter changes take effect as described in
[Apply greeter changes](install.md#apply-greeter-changes).

### Hybrid GPUs

On hybrid-GPU machines whose displays are all wired to one GPU, point the
greeter at that GPU's render node so it does not render on the other GPU and
copy every frame across. niri ignores wlroots' `WLR_DRM_DEVICES`, so a Cage-era
setting on greetd has no effect:

```nix
programs.genkan.greeter.niri.renderDevice = "/dev/dri/by-path/pci-0000:c2:00.0-render";
```

### Keyboard and cursor

The niri greeter sets no `input` block, so niri takes the keyboard layout from
`org.freedesktop.locale1`, which follows `services.xserver.xkb`, and draws its
default cursor. For Cage, see
[Set the keyboard layout and cursor](install.md#set-the-keyboard-layout-and-cursor).

## Session

`programs.niri.enable = true` installs niri's session entry and portals. Genkan
lists `niri.desktop` like any other Wayland session, starts `niri-session`, and
passes `XDG_SESSION_DESKTOP=niri` and `XDG_CURRENT_DESKTOP=niri`.

## Desktop wallpaper

Start the [desktop wallpaper](wallpapers.md#run-a-desktop-wallpaper) from niri
and match its layer namespace, `genkan-wallpaper`, to keep it still behind the
overview and workspace switches:

```kdl
spawn-at-startup "genkan" "wallpaper" "--file" "/home/alice/Pictures/dynamic.heic"

layer-rule {
    match namespace="^genkan-wallpaper$"
    place-within-backdrop true
}

layout {
    background-color "transparent"
}
```

`place-within-backdrop` moves the wallpaper behind the workspaces, so the
workspace background must be transparent or the wallpaper only shows in the
overview. Without the layer rule, the wallpaper shows on workspaces but not in
the overview. Disable `swaybg` or any other wallpaper client.

## Locking

niri provides `ext-session-lock-v1` but does not lock before sleep on its own.
Bind a key to lock immediately:

```kdl
binds {
    Mod+Alt+L hotkey-overlay-title="Lock the Screen" { spawn "genkan" "lock" "--daemonize"; }
}
```

Use swayidle for idle and suspend locking. Starting it from niri ties it to the
session:

```kdl
spawn-at-startup "swayidle" "-w" "timeout" "600" "genkan lock --daemonize" "before-sleep" "genkan lock --daemonize"
```

If swayidle instead runs as a systemd user service, as in niri's example
systemd setup, set `KillMode=process` on that service. `genkan lock --daemonize`
starts the lock owner in the caller's control group, so the default
`KillMode=control-group` ends the confirmed lock whenever the service stops or
restarts, leaving the session locked with nothing drawn. The
[suspend caveats](lock.md#lock-on-idle-and-before-sleep) apply unchanged.

niri's `allow-when-locked=true` applies only to `spawn` binds such as media
keys. If closing the lid does not suspend the machine, lock on lid close
instead:

```kdl
switch-events {
    lid-close { spawn "genkan" "lock" "--daemonize"; }
}
```
