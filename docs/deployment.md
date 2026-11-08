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

On a multi-monitor layout, the Cage window continues to render wallpaper over
the full desktop while Genkan places authentication on one output. By default
it deterministically chooses an external connector by type and numeric suffix
(`DP-2` before `DP-10`), then falls back to an internal `eDP-`, `LVDS-`, or
`DSI-` panel when no external named output is available. To prefer a
particular connector, add `--authentication-output DP-2` after `genkan login`;
internal panels can also be selected explicitly. If that connector is absent,
the same default fallback still applies. Genkan
re-evaluates this placement when outputs are connected, removed, rearranged, or
rescaled while the greeter is running.

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

The lock covers every output but presents the clock and authentication column
on only one; all remaining outputs show unobstructed wallpaper. No output's
wallpaper is dimmed. It uses the
same external-output-first, internal-panel fallback policy as login. Pass
`--authentication-output DP-2` to select a preferred connector. The preference
is preserved by `--daemonize`, and the lock moves authentication to the normal
fallback if the chosen output disappears.

The login and lock screens share one composition drawn from the same design
tokens: the date and clock a fixed distance below the top edge of the
authentication output, and the avatar, name, and password pill a fixed distance
above its bottom edge, over the selected wallpaper. Only the clock shrinks, and
only on outputs too short for both. The lock omits login's session and power
items. Login adds a stronger outline to the currently focused control for
keyboard navigation; the lock screen has a single active response field, shows
every response masked, and asks for Return to start a new attempt after a
failure. This presentation consistency does not join their security backends: greetd still owns login authentication and
the dedicated `genkan-lock` PAM worker still owns unlock authentication.

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

## Desktop wallpaper

`genkan wallpaper` runs as an ordinary logged-in-user client and creates one
non-interactive background-layer surface per output:

```sh
genkan wallpaper --file /home/alice/Pictures/dynamic.heic
```

The package also installs hash-pinned wallpaper samples under
`share/genkan/wallpapers`, recorded in the
[wallpaper manifest](../assets/wallpapers/manifest.toml) with their provenance,
rights, SHA-256, byte size, item structure, and schedule metadata. The installed
tree names the format: `mov/` holds the MOV videos and their posters, and
`heic/` holds the dynamic HEIC assets, and the R2 object layout mirrors it.
Repository samples are pinned by their committed bytes; a catalog asset
delivered from the R2 host records its delivery URL and is fetched by Nix as a
hash-pinned fixed-output source. A wallpaper process never downloads media.

`--wallpaper` selects the same MOV catalog as login; `--reduce-motion` shows
its poster. `--wallpaper-file` accepts an absolute MOV or HEIC override, with
the same appearance and reduced-motion arguments as login. `--skip-output`
omits a connector or persistent monitor name. No decoder starts when every
output is skipped.

`--file` retains the desktop HEIC interface and is required with `--solar`.
It accepts an existing absolute regular `.heic` or `.heif` file. URIs, FIFOs, devices, and
pipeline descriptions are rejected, and the file is bound by descriptor so a
later pathname replacement cannot substitute another object. The container is
validated against the resource ceilings and copied to an anonymous descriptor
before libheif parses it, so rewriting the bound inode afterwards cannot change
what is decoded. Local files are never uploaded, copied into the store, or
published.

Frames are published as opaque SDR RGBA8 in sRGB. An embedded ICC profile is
converted to sRGB rather than ignored, because dropping one would display the
wallpaper with the wrong colors, and a file whose profile cannot be converted —
a non-RGB color space, a malformed profile, or one above the 1 MiB profile
ceiling — is refused rather than displayed with unspecified color. An item that
carries NCLX instead of ICC is still required to declare sRGB primaries and the
sRGB transfer function.

The initial backend requires `zwlr_layer_shell_v1` and is intended for niri,
Sway, River, Hyprland, and other compatible wlroots-oriented compositors. It
fails instead of opening an ordinary window when layer shell is unavailable.
GNOME Shell and KDE Plasma generally require shell-specific wallpaper APIs and
are not supported by this backend.

Start the command from the compositor's own autostart configuration so it
inherits the exact `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, and session lifetime.
For example, Sway accepts:

```text
exec genkan wallpaper --file /home/alice/Pictures/dynamic.heic
```

Use the equivalent `spawn-at-startup` or `exec-once` mechanism for the selected
compositor. Disable any competing wallpaper process. Genkan deliberately does
not install a generic systemd user service or wrap the validated desktop
session command: a UID-wide user manager may not identify the intended Wayland
session, and wallpaper startup must not delay desktop startup.

`--reduce-motion` keeps time-of-day scheduling but changes frames immediately
instead of dissolving. `--appearance light` or `--appearance dark` selects
static appearance metadata when present. The desktop process does not contact
greetd or PAM, acquire a session lock, select sessions, or request power
operations. A running `genkan lock` remains compositor-owned and occludes this
ordinary background client.

greetd supplies `GREETD_SOCK`. Genkan handles each PAM prompt in sequence and
then asks greetd to start the selected session with the Wayland XDG environment.
If AccountsService, a valid session, or the greetd socket is unavailable,
Genkan reports the specific configuration or transport failure rather than
using a host-specific fallback.

### Optional solar scheduling

`--solar` opts a wallpaper that carries Apple `solar` metadata into a
GeoClue-backed schedule. Genkan exposes no location provider, service URL,
authorization field, or coordinate option. It requests only city-level accuracy
through the logged-in user's GeoClue agent, after the first frame is already on
screen, so location lookup never delays compositor startup or surface
configuration. A successful fix is retained in memory, refreshed no more often
than every six hours, and never written to disk.

Enabling solar can cause GeoClue to use a network geolocation provider
according to host policy. On denial, timeout, an unavailable service, or a fix
coarser than city accuracy, Genkan falls back to a valid `h24` schedule and then
to the declared appearance or primary image. Losing the network after a
successful fix does not invalidate it, and no location outcome ends the
wallpaper process or affects login or lock.

NixOS hosts set `programs.genkan.wallpaper.solar.enable` to provision GeoClue and
authorize the `genkan-wallpaper` application identity. The option does not start
a compositor or select a user session; `--solar` remains an explicit runtime
choice, and login and lock never request location.

## niri

These notes were checked against niri 26.04. Cage remains the default greeter
compositor; the opt-in niri greeter implements [RFD 6](../rfd/0006/README.adoc).

### Greeter

Import the module and select niri. Output blocks are structured options, not
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

Use `niri msg outputs` to get connector and make/model/serial names. Matching
is case-insensitive, with `Unknown` for missing fields, just like niri. Login
in spanning mode and `genkan lock --authentication-output "…"` accept the same
names: wlroots output-management metadata supplies serials under Cage, and
niri's read-only IPC supplies them under niri. A compositor exposing neither
source still supports connector selection; descriptions are not parsed as
identities. If the monitor is absent, spanning login and lock use their normal
fallback; niri places compositor-mode login on the focused available output.

The module generates this output block, for example:

```kdl
output "Dell Inc. DELL U3223QE YOUR-SERIAL" {
    mode "3840x2160@60"
    scale 1.275
    transform "normal"
    position x=1920 y=0
}
```

`config.programs.genkan.greeter.niri.outputConfig` exposes exactly those
generated blocks for reuse in the host's desktop niri configuration. Do not
reuse the entire greeter config as a desktop config. Its window rule is:

```kdl
window-rule {
    match app-id="^genkan$"
    open-on-output "Dell Inc. DELL U3223QE YOUR-SERIAL"
    open-fullscreen true
}
```

The ordinary login window has stable app ID `genkan`; `--placement compositor`
treats its whole window as the authentication region. The module starts only
`genkan greeter-session`, which starts login and background children without a
shell, excludes the authentication output from the background client, and asks
niri to quit on every login exit, including a crash. Wallpaper failure and
SIGINT/SIGTERM also tear down the greeter. The supervisor never starts a user's
desktop: greetd alone owns authentication and `StartSession`, and waits for
niri to release the displays before starting the session.

The generated configuration has empty `binds`, no startup hotkey overlay,
`screenshot-path null`, Xwayland disabled, and no includes. It is validated
with the configured niri package at build time **and immediately before
launch**: niri 26.04 otherwise falls back to built-in launch bindings on a
startup parse error. See the [investigation](../rfd/0006/INVESTIGATION.adoc)
for the actual fallback and startup measurements. Never point greetd directly
at a user-writable niri config.

When it configures either greeter, the module sets
`systemd.services.greetd.restartIfChanged = false`, enables AccountsService and
graphics, and passes the host's Wayland session directory. A generation switch
does not kill an active session or replace the on-screen greeter. greetd keeps
its generation-specific command in memory, including after logout. Changes to
the immutable greeter config or binary therefore require a **greetd daemon
restart or reboot**. Do not restart greetd while a desktop session is active;
that would terminate the session this setting protects. This is not the
desktop's live-reloaded config. Desktop output configuration and graphics/VRAM
limits remain host policy.

Wallpaper and lock negotiate fractional-scale plus viewporter support and
render at the compositor's preferred size (in 1/120 increments), with logical
viewport destinations. Compositors without both protocols retain integer
buffers. This reduces allocation and avoids integer-scale downsampling at
1.275, 1.3, and 1.2 without changing lock ownership or unlock authorization.

### Session

Enable niri with `programs.niri.enable = true`, which installs its session
entry and portals. Genkan discovers `niri.desktop` like any other
`wayland-sessions` entry, starts `niri-session`, and passes
`XDG_SESSION_DESKTOP=niri` and `XDG_CURRENT_DESKTOP=niri`.

The niri greeter's configuration sets no `input` block, so niri takes the
keyboard layout from `org.freedesktop.locale1`, which follows the system's
`services.xserver.xkb` settings, and draws its default cursor. Under Cage, set
the greeter's keyboard layout and cursor on greetd instead when they differ
from the defaults, for example:

```nix
systemd.services.greetd.environment = {
  XKB_DEFAULT_LAYOUT = "us";
  XCURSOR_THEME = "Adwaita";
  XCURSOR_SIZE = "24";
};
```

On hybrid-GPU machines whose displays are all wired to one GPU, point the niri
greeter at that GPU's render node so it does not render on the other GPU and
copy every frame across. niri ignores wlroots' `WLR_DRM_DEVICES`, so a Cage-era
setting on greetd has no effect:

```nix
programs.genkan.greeter.niri.renderDevice = "/dev/dri/by-path/pci-0000:c2:00.0-render";
```

### Desktop wallpaper

Start the wallpaper from niri and match its layer namespace,
`genkan-wallpaper`, to keep it still behind the overview and workspace
switches:

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
workspace background must be transparent or the wallpaper is hidden everywhere
except the overview. Without the layer rule the wallpaper still shows on
workspaces but not in the overview. Disable `swaybg` or any other wallpaper
client.

### Locking

niri provides `ext-session-lock-v1` and does not lock before sleep on its own.
Bind a key to lock immediately:

```kdl
binds {
    Mod+Alt+L hotkey-overlay-title="Lock the Screen" { spawn "genkan" "lock" "--daemonize"; }
}
```

Use swayidle for idle and suspend locking. Starting it from niri keeps it tied
to the session:

```kdl
spawn-at-startup "swayidle" "-w" "timeout" "600" "genkan lock --daemonize" "before-sleep" "genkan lock --daemonize"
```

If swayidle instead runs as a systemd user service, as in niri's example
systemd setup, set `KillMode=process` on that service.
`genkan lock --daemonize` starts the lock owner in the caller's control group,
so the default `KillMode=control-group` would end the confirmed lock whenever
the service stops or restarts, leaving the session locked with nothing drawn.
The suspend caveats in [Session locking and suspend](#session-locking-and-suspend)
apply unchanged.

niri's `allow-when-locked=true` applies only to `spawn` binds such as media
keys. If closing the lid does not suspend the machine, lock on lid close
instead:

```kdl
switch-events {
    lid-close { spawn "genkan" "lock" "--daemonize"; }
}
```

### Outputs

`niri msg outputs` lists each connector with its make, model, and serial.
niri's `output` blocks accept either form, but `--authentication-output`
currently takes the connector name, such as `DP-3`. Connector numbers can
change with docks and cable order, so recheck them after changing hardware.

Fractional output scales work. The lock and wallpaper currently render at the
next whole scale and niri scales them down, which uses more memory than
rendering at the exact size; fractional-scale support is planned in RFD 6.

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
and retains the resulting validated argument vector. Genkan does not invoke a
shell itself. Before sending `StartSession`, it POSIX-single-quotes every
argument (including empty arguments and embedded single quotes), because
greetd joins the vector with spaces and passes it to `/bin/sh -c`. This keeps
spaces, dollar signs, and other shell syntax literal in session arguments.

This boundary was verified against greetd **0.10.3**, commit
[`08efe60feceea8c81f9571c666880ff1e1c2e3ff`](https://git.sr.ht/~kennylevinsen/greetd/tree/08efe60feceea8c81f9571c666880ff1e1c2e3ff/item/greetd/src/session/worker.rs):
`greetd/src/session/worker.rs` lines 228–235 construct `exec {cmd.join(" ")}`
(with optional profile sourcing), and lines 267–274 execute `/bin/sh -c`.
The source archive is `https://git.sr.ht/~kennylevinsen/greetd/archive/0.10.3.tar.gz`;
this is also the greetd version in the repository's pinned nixpkgs revision
`44a91898084f46797b5fac650c7e8c9ac38c43d4`.

## Power policy

Sleep, restart, and shutdown are in the power menu at the upper right and call
logind over the system D-Bus. Restart and shutdown ask for confirmation first;
sleep acts immediately because it cannot lose work. Calls do not request interactive polkit authentication,
so the greeter user must already be authorized by system policy. A denial or
unavailable system bus leaves the authentication attempt intact and displays
the logind error.

## Wallpaper configuration

Tahoe Beach animates by default. Select another packaged catalog entry with:

```text
--wallpaper sequoia-sunrise
--wallpaper sequoia-morning
--wallpaper sequoia-night
--wallpaper golden-gate
```

`--reduce-motion` (also available as `--static-wallpaper`) shows the selected
poster without initializing GStreamer. If playback fails before the first video
frame, Genkan reports the failure and keeps the poster, or the generated
background when the poster is unavailable. After playback begins, a failure
retains the last displayed frame instead of briefly replacing it with the
poster.

`login` and `lock` also accept an absolute local dynamic HEIC through the same
`--wallpaper-file` option:

```sh
genkan login --wallpaper-file /home/alice/Pictures/dynamic.heic
genkan lock --wallpaper-file /home/alice/Pictures/dynamic.heic
```

A HEIC override uses the same parser, time-of-day scheduler, and RGBA frame path
as the desktop wallpaper, but never requests location. `--appearance light` or
`--appearance dark` selects a static appearance image; `automatic` (the default)
uses a valid `h24` time schedule when present and otherwise falls back to the
appearance metadata's light image, then the primary image. For a HEIC,
`--reduce-motion` keeps time-of-day scheduling but disables dissolves, whereas a
MOV `--reduce-motion` shows the fixed poster. For `login` and `lock`, parsing and
decoding run in a resource-bounded `genkan heic-worker` child process; a parsed
or decoded failure, or an allocation abort that can still occur inside a parser,
base64, `plist`, or libheif dependency, terminates only that worker and retains
the poster or last valid frame. It cannot affect authentication, lock readiness,
or unlock. The helper asks the kernel to signal it when the greeter dies
(`PR_SET_PDEATHSIG`) before it opens the file. While the greeter lives, its
supervisor thread reaps the helper; after the greeter dies, the helper is
reparented and its adopter is responsible for reaping it, because a dead
greeter's supervisor cannot. Termination and reaping are subject to kernel
scheduling rather than being instantaneous. The relay repeats the decoder's
per-axis and byte ceilings, so a corrupt or hostile worker cannot make the
greeter allocate a frame the decoder would have refused. Login additionally
waits for the iced renderer to report its backend and starts a dynamic HEIC
source only on iced's wgpu renderer; on the pinned software backend, which
converts a whole frame with an infallible allocation, the poster is retained
instead. These are process and protocol limits, not an operating-system memory
sandbox: a whole-system or cgroup out-of-memory kill, a device failure, or a
compositor disconnect can still terminate the greeter.

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
