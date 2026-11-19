# Deployment and greetd Configuration

Use the Genkan NixOS module to configure greetd. It runs the greeter under Cage
by default; select niri if you want per-output scale, rotation, and position
settings for the login screen.

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

For a multi-user Nix install that rejects unsigned imports, run the import with
a trusted Nix user. Importing does not configure host services or drivers; see
[host configuration](#host-configuration) before enabling login or locking.

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

In `configuration.nix`, import the module and enable the greeter. Enable a
Wayland desktop session separately; this example uses niri for the logged-in
session and keeps Cage as the greeter compositor.

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

The module enables AccountsService and graphics and gives greetd the host's
Wayland session directory. Remove any old manually configured
`services.greetd.settings.default_session.command` so the module can set it.
For niri on the login VT as well, see [niri greeter configuration](#greeter).
For monitor selection on either compositor, see [Outputs](#outputs).

The module sets `systemd.services.greetd.restartIfChanged = false` so switching
generations does not terminate an active desktop session or replace the running
greeter. greetd keeps its generation-specific command in memory even after
logout. To activate a changed greeter config or binary, restart greetd when no
desktop session is active, or reboot. Restarting greetd while a desktop session
is active terminates it.

For the package and its dedicated lock PAM policy without changing the greeter,
import the same module and set `programs.genkan.enable = true` without enabling
the greeter. The module does not configure an idle manager or replace a desktop
environment's own locker. Its private authentication worker is unprivileged and
is not installed setuid.

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

The package installs the [wallpaper manifest](../assets/wallpapers/manifest.toml)
and [Tahoe aerial catalog](../assets/wallpapers/macos-tahoe-aerials.toml) under
`share/genkan/wallpapers`, not MOVs, HEICs, or posters. Use an existing local
file or explicitly run `genkan assets download <key>` before selecting a
catalog wallpaper. Downloads use curated upstream sources, not an R2 mirror;
Nix does not fetch artwork. A wallpaper process never downloads media. See
[wallpaper configuration](#wallpaper-configuration) for cache paths and
greeter provisioning.

`--wallpaper` selects the same MOV catalog as login; `--reduce-motion` shows
its poster. `--wallpaper-file` accepts an absolute MOV or HEIC override, with
the same appearance and reduced-motion arguments as login. `--skip-output`
omits a connector or persistent monitor name. No decoder starts when every
output is skipped.

For a downloaded HEIC, use the absolute media path printed by download with
`--file`; do not assume it lives under the cache's `heic/` directory.
Previously published versions are retained, so saved absolute file overrides
keep selecting the same version after a replacement download.

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

See [Outputs](#outputs) for finding a monitor's connector or persistent name
and for what happens when the preferred monitor is absent.

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

The greeter uses its own immutable config, not the desktop's live-reloaded
config. See [Host configuration](#host-configuration) for applying a new greeter
generation without terminating an active desktop session.

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

Set `programs.genkan.greeter.authenticationOutput` for either greeter, or pass
`--authentication-output` to standalone spanning login or `genkan lock`. Use a
connector such as `DP-3`, or a persistent `make model serial` name such as
`Dell Inc. DELL U3223QE YOUR-SERIAL`. Run `niri msg outputs` in a niri session
to see both forms. Persistent names are useful when connector numbers change
with docks or cables.

Spanning login and lock match names case-insensitively, using `Unknown` for
missing fields. They get monitor identities from wlroots output management or
niri's read-only IPC; without either source, use a connector name. If the
preferred monitor is absent or unset, they choose an external connector by type
and numeric suffix (`DP-2` before `DP-10`), then fall back to an internal panel
when no external named output is available. The niri greeter instead lets niri
place the login window on the focused available output when its preferred
monitor is absent or unset.

Wallpaper and lock render at the compositor's preferred fractional scale when
it offers both the fractional-scale and viewporter protocols. They use logical
viewport destinations and fall back to integer buffers otherwise.

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

### Account pictures

Run these commands as yourself from your desktop session, without `sudo`:

```sh
genkan avatar list
genkan avatar set lake
genkan avatar set --file /home/alice/Pictures/photo.jpg
genkan avatar clear
```

`list` reads the installed built-in manifest. `set` changes only the calling
user's AccountsService picture, subject to its normal polkit
`change-own-user-data` policy; it cannot select another account. Photo imports
accept regular PNG/JPEG files up to 1 MiB and 4096 pixels per side. A sandboxed
child crops the centered square and scales it to 512 by 512; the command
re-encodes a metadata-free PNG, rejects output over 1 MiB, and removes its
private temporary file after AccountsService copies it (also on failure).
Login and lock load the picture on their next start; malformed or unavailable
pictures keep initials. `clear` restores initials.

### Wayland sessions

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

The default selection is Tahoe Beach, but a fresh installation shows the
generated background until its media is explicitly downloaded. Packages and the
development shell contain no wallpaper MOVs, HEICs, or posters; source JPEGs
are test-only references. Download and preview as your desktop user with:

```sh
genkan assets list
genkan assets preview macos-landscape-tahoe-day
```

`assets preview` downloads the original if missing, then opens a simulated
window without changing wallpaper settings. Cached previews stay offline.
Use `genkan assets download macos-landscape-tahoe-day` to cache it without a
window, then select it with `--wallpaper macos-landscape-tahoe-day`. The listing
shows Apple's full Tahoe MOV set and the existing HEICs; `--details` adds names,
source URLs, and rights. Legacy Apple MOV IDs, including `tahoe-beach`, remain valid
when named explicitly.

Download preparation, including hashing and ZIP parsing, is resource-bounded.
The downloader verifies curated sizes and SHA-256 pins, validates a
representative HEVC MOV or HEIC frame, and generates a MOV poster before
atomically selecting the complete cached generation. A preparation or
publication failure leaves the previous generation usable. It requires a
direct HTTP(S) response with status 200; redirects are rejected. The default transfer/expanded-media
limit is 2 GiB (`--max-bytes` accepts 4 KiB–16 GiB), with a 600-second transfer
timeout. For custom URLs, catalogs, and configured ZIP sources, see
[download and inspect wallpapers](development.md#download-and-inspect-wallpapers).
Login, lock, and wallpaper playback never auto-download missing media.
Preview uses the same download limits and validation; a failed download stops
before opening its window.

### Cache selection and greeter provisioning

On NixOS, import `genkan.nixosModules.default` and let Genkan own preparation:

```nix
programs.genkan = {
  enable = true;
  greeter.enable = true;
  wallpaper.source.catalog = "macos-landscape-tahoe-day";
};
```

Choose exactly one source; no per-wallpaper package or downstream downloader is needed:

| `programs.genkan.wallpaper.source` | Example |
| --- | --- |
| `catalog` | `"macos-landscape-tahoe-day"` (IDs from `genkan assets list`) |
| `url` | `"https://example.org/original.mov"` (direct HTTP(S), no redirects) |
| `file` | `/path/to/original.heic` or `"/srv/wallpapers/original.mov"` |

`source.sha256` optionally supplies a trusted 64-digit hexadecimal SHA-256.
The Apple aerial catalog declares byte sizes, not upstream SHA-256 pins; a
computed hash records identity, not upstream authenticity. Catalog size,
archive, and supplied hash checks cannot be disabled or overridden.
`wallpaper.maxBytes` defaults to 2 GiB (range 4 KiB–16 GiB).
Media must pass Genkan's existing bounded MOV/HEIC inspection and representative
frame decoding; MOV posters are generated automatically. This is not an exhaustive
decode of every frame or a promise of compositor/hardware support.

`genkan-wallpaper.service` prepares at boot and when its configuration changes,
before greetd, as the dedicated `genkan-wallpapers` system user. Its state is
`/var/lib/genkan-wallpapers`: published media and posters are readable by greeter
and desktop users, but writable only by the provisioning identity/root. Local
sources must be readable by that identity; Nix path literals enter the store,
whereas absolute strings refer to runtime files. Do not embed secrets in source URLs.
Unchanged URLs reuse verified cached content without contacting the server;
local sources are rehashed to detect changes. To deliberately refresh a mutable
URL, use a new URL or a trusted hash for the new content.

An atomic `selected` symlink changes only after successful preparation. Errors
identify the source and problem in the service journal and retain the previous
selection and published generations. A failed service does not block greetd;
without any successful selection the generated media-free fallback remains.
Retry explicitly with `sudo systemctl restart genkan-wallpaper.service` after
correcting the source or connectivity. Already running playback keeps its current
media until restarted; provisioning never restarts greetd or a desktop session.

The module installs a wrapper as the system `genkan` and uses it in **both Cage
and niri greeter launches**. Desktop autostart can simply run `genkan wallpaper`,
and lock hooks `genkan lock --daemonize`. For an explicit Nix executable path,
use `${config.programs.genkan.wallpaper.package}/bin/genkan`, not the raw
`programs.genkan.package` or flake package. The wrapper resolves the selected
immutable media path and passes `--wallpaper-file`; it does not depend on greetd
forwarding daemon environment variables. Explicit `--file`/`--wallpaper-file`
arguments (including `greeter.wallpaperFile`) win. Solar mode still requires an
explicit HEIC `--file` and `--solar`. Desktop autostart and idle policy remain
host-owned. The default flake package remains media-free and unchanged.

Outside NixOS, the same public provisioning interface is available:

```sh
genkan assets prepare iceland
genkan assets prepare --url "https://example.org/original.mov"
genkan assets prepare --file /absolute/path/to/original.heic
genkan assets selected
```

Use an absolute `GENKAN_WALLPAPER_DIR` for a separately owned shared cache.
`assets selected` only prints the last prepared media path; ordinary unwrapped
playback does not automatically consume it. Pass that path as `--wallpaper-file`.

For manual cache management, the first applicable cache path wins:

| Environment | Cache root |
| --- | --- |
| `GENKAN_WALLPAPER_DIR` set | That absolute directory; a relative value is an error |
| Absolute `XDG_CACHE_HOME` | `$XDG_CACHE_HOME/genkan/wallpapers` |
| Absolute `HOME` | `$HOME/.cache/genkan/wallpapers` |

Downloaded media, MOV posters, and metadata live together under
`objects/download-<key>-<random>/`; an atomic `entries/<key>.toml` receipt
selects the complete generation. Readers use one receipt snapshot for metadata,
media, and poster. Conventional `mov/` and `heic/` directories
are supported only for explicitly provisioned local/test files, not as download
destinations. Use the printed media path for file overrides. The next explicit
download cleans only abandoned, unpublished staging; it never deletes previously
published generations. Playback performs no cache cleanup.

Published versions are retained for active readers and saved absolute overrides,
so replacements may occupy additional disk space. Remove an unused version only
when no readers or saved overrides need it.

Downloading as your desktop user does not populate the greeter's cache. Without
the module wrapper, pass the printed media path explicitly to each consumer and
keep the shared cache writable only by a dedicated provisioning identity/root.
Setting `systemd.services.greetd.environment.GENKAN_WALLPAPER_DIR` alone is not
sufficient: greetd does not forward arbitrary daemon variables into PAM sessions.

### Select cached media

Once downloaded, Tahoe Beach animates by default. Select another cached MOV
catalog entry with:

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

`login` and `lock` also accept an absolute local MOV or dynamic HEIC through
`--wallpaper-file`. For a custom download, run:

```sh
genkan assets download my-wallpaper --url "$SOURCE_URL" --sha256 "$SHA256"
```

Set `WALLPAPER_FILE` to the absolute media path printed by that command, then:

```sh
genkan login --wallpaper-file "$WALLPAPER_FILE"
genkan lock --wallpaper-file "$WALLPAPER_FILE"
```

Use the printed path, not a constructed `mov/` or `heic/` path. Existing local
files such as `/home/alice/Pictures/dynamic.heic` remain valid overrides.
Saved absolute overrides remain valid after replacements and keep selecting
their original published version; change the override only to select a newer
version.

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

Runtime playback reads only local media. Asset provenance, explicit upstream
delivery, integrity, and loop behavior are documented in
[RFD 2](../rfd/0002/README.adoc). There is no runtime or build dependency on R2;
the former delivery bucket is left untouched.

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
