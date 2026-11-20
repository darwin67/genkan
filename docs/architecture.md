# How Genkan Works

This guide explains the boundaries behind Genkan's behavior: what each process
trusts, how media and sessions cross into it, and how login and lock share one
presentation. Read it before changing those boundaries; for setup, see the
[install guide](install.md).

## Responsibilities

| Concern | Owner |
| --- | --- |
| Login authentication and starting the desktop session | greetd and PAM; Genkan relays each prompt in sequence |
| Unlock authentication | The unprivileged `genkan-lock` PAM worker |
| Accounts | AccountsService; `--username` is only an administrative override |
| Desktop sessions | Validated `wayland-sessions` entries |
| Power actions | logind over the system bus |
| Wallpaper media | Explicit downloads or provisioning; playback reads only local files |

Genkan never validates passwords, logs credentials or PAM responses, invokes a
shell, or adds an implicit compositor command. Failed authentication recovers
without restarting the greeter.

## Session launch

Genkan builds each session's argument vector from its validated Desktop Entry:
it applies Desktop Entry quoting and field-code rules and keeps the resulting
argv. greetd, however, joins the command vector with spaces and passes it to
`/bin/sh -c`. Before sending `StartSession`, Genkan therefore POSIX-single-quotes
every argument, including empty arguments and embedded single quotes, so
spaces, dollar signs, and other shell syntax stay literal.

This boundary was verified against greetd **0.10.3**, commit
[`08efe60feceea8c81f9571c666880ff1e1c2e3ff`](https://git.sr.ht/~kennylevinsen/greetd/tree/08efe60feceea8c81f9571c666880ff1e1c2e3ff/item/greetd/src/session/worker.rs):
`greetd/src/session/worker.rs` lines 228–235 construct `exec {cmd.join(" ")}`
(with optional profile sourcing), and lines 267–274 execute `/bin/sh -c`.
The source archive is `https://git.sr.ht/~kennylevinsen/greetd/archive/0.10.3.tar.gz`;
this is also the greetd version in the repository's pinned nixpkgs revision
`44a91898084f46797b5fac650c7e8c9ac38c43d4`.

Account initials are rendered from bounded labels, and user-controlled picture
files are never decoded in the credential-handling process; see
[RFD 7](../rfd/0007/README.adoc).

## Wallpaper decoding

For `login` and `lock`, HEIC parsing and decoding run in a resource-bounded
`genkan heic-worker` child process. A parse or decode failure, or an allocation
abort inside a parser, base64, `plist`, or libheif dependency, terminates only
that worker; the screen keeps the poster or last valid frame. It cannot affect
authentication, lock readiness, or unlock.

- The worker asks the kernel to signal it when the greeter dies
  (`PR_SET_PDEATHSIG`) before it opens the file. While the greeter lives, its
  supervisor thread reaps the worker; after the greeter dies, the reparented
  worker's adopter reaps it. Termination and reaping follow kernel scheduling
  rather than being instantaneous.
- The relay repeats the decoder's per-axis and byte ceilings, so a corrupt or
  hostile worker cannot make the greeter allocate a frame the decoder would
  have refused.
- Login waits for iced to report its renderer and starts a dynamic HEIC source
  only on the wgpu backend. On the pinned software backend, which converts a
  whole frame with an infallible allocation, it keeps the poster.

These are process and protocol limits, not an operating-system memory sandbox:
a whole-system or cgroup out-of-memory kill, a device failure, or a compositor
disconnect can still end the greeter.

### Desktop wallpaper files

`genkan wallpaper --file` binds the file by descriptor, so a later pathname
replacement cannot substitute another object. The container is validated
against the resource ceilings and copied to an anonymous descriptor before
libheif parses it, so rewriting the bound inode afterwards cannot change what
is decoded.

Frames are published as opaque SDR RGBA8 in sRGB. An embedded ICC profile is
converted to sRGB rather than ignored, because dropping it would show the wrong
colors. A file whose profile cannot be converted (a non-RGB color space, a
malformed profile, or one above the 1 MiB ceiling) is refused rather than shown
with unspecified color. An item that carries NCLX instead of ICC must declare
sRGB primaries and the sRGB transfer function.

## Shared presentation

`src/design/` owns the bundled Inter typeface, glyphs, styles, text fitting,
and plain-data tokens used by both iced login and the CPU lock renderer.
`tokens.rs` mirrors [`tokens.json`](design-system/tokens.json); the token mirror
test fails if either changes without the other. `make check-design`, part of
`make verify`, rejects new numeric control styling outside `src/design/`. Its
regression fixtures cover color, font, spacing, comments, and structural
zeroes; it is a lexical guard, not a Rust parser or a ban on geometry
arithmetic. The [design system](design-system/README.md) describes the visual
rules.

### Glass material

Controls sample a shared blurred copy of the wallpaper frame. The copy is at
most 192 pixels on its longest edge, uses bounded stratified downsampling and
three separable box passes, and is cached until the wallpaper allocation
changes. Both renderers crop it with the wallpaper's cover geometry and clip it
to the control. Login rasterizes a control-sized rounded alpha mask because
iced's tiny-skia backend does not implement image-local clipping. A dark
backing bounds the brightest channel even over white artwork; the wallpaper
outside controls stays undimmed. The generated fallback background keeps its
contrast-tested control treatment.

Contrast tests use the actual control color roles and filtered material over
RGB cube vertices and test-only reference JPEGs, in encoded and linear sRGB.
Wallpaper text has a soft shadow rather than an outline, so its contrast over
bright photographic regions is a visual check, not a numeric one. Open
contrast work is tracked in [Deferred Work](deferred.md#wallpaper-control-and-text-contrast).

The lock rejection shake uses the login motion curve, requests overlay-only
frames at the runtime's 16 ms cadence, ends at rest, and is disabled by reduced
motion. It does not change authentication or unlocking.

### Status bar metadata

Caps Lock and the active layout name come from the focused Wayland keyboard's
XKB keymap and `modifiers` event, never from observing Caps Lock key presses.
Lock reuses its SCTK seat keyboard. Login briefly observes iced's own borrowed
Wayland connection every 500 ms; a second connection would not own keyboard
focus and could not reliably supply this state. The observer does not
interpret key events, change focus, or retain response text, and it reads
keymap descriptors at offset zero without advancing their potentially shared
file offset. Lock selects metadata from the keyboard producing input, so
removing another seat cannot clear it; login omits conflicting or unknown
focused-seat snapshots. Unavailable keymaps, absent focus, unnamed layouts, and
non-Wayland backends omit the items rather than guess.

Network status reads only `/sys/class/net/*/operstate`, excluding loopback.
“Network link up” means an interface reports `up`, not that Internet access was
tested. “Network link down” requires every observed interface to report a known
down state; unknown or unreadable states without an up interface omit the
item. There are no network requests, NetworkManager dependency, or new daemons.
Lock checks every five seconds; login shares its metadata timer.

Deterministic previews use explicitly synthetic metadata and never contact
system services.
