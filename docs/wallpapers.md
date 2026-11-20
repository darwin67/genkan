# Wallpapers

Genkan ships without wallpaper media. Login, lock, and the desktop wallpaper
show a generated background until you download, provision, or point them at a
wallpaper, and they never download anything themselves. A catalog wallpaper
reaches the screen in three steps: pick it, download it explicitly, then
select it. A local MOV or HEIC needs no download; pass its absolute path to
`--wallpaper-file`.

```sh
genkan assets list                                  # pick
genkan assets preview macos-landscape-tahoe-day     # download and try it in a simulated window
genkan wallpaper --wallpaper macos-landscape-tahoe-day  # select it (also lock)
```

On a NixOS host with a [provisioned system wallpaper](#provision-the-greeters-wallpaper),
the installed `genkan` adds that wallpaper unless you pass a file, so a
`--wallpaper` ID does not override it. Use `--wallpaper-file` with the absolute
path that `genkan assets download` prints instead.

Real `genkan login` uses greetd and other system services, so select its
wallpaper in the greeter environment, normally through
[provisioning](#provision-the-greeters-wallpaper).

| Where it shows | How to select it | Section |
| --- | --- | --- |
| Login and lock you run yourself | `--wallpaper ID` or `--wallpaper-file PATH` | [Use a wallpaper in login and lock](#use-a-wallpaper-in-login-and-lock) |
| The NixOS greeter on the login VT | `programs.genkan.wallpaper.source` | [Provision the greeter's wallpaper](#provision-the-greeters-wallpaper) |
| Your logged-in desktop | `genkan wallpaper` from compositor autostart | [Run a desktop wallpaper](#run-a-desktop-wallpaper) |

For an uninstalled build, run the same commands as
`cargo run --bin genkan -- assets …` inside `nix develop`.

## Browse the catalog

The default catalog offers all 156 macOS Tahoe aerial MOVs, downloaded directly
from Apple, plus the Apple-hosted dynamic HEICs Catalina, Monterey Graphic, and
Ventura Graphic. `genkan assets list` groups the MOVs under Landscape,
Cityscape, Underwater, Earth, and Mac, and lists the HEICs separately. Each row
shows the format, resolution when known, size in MiB, and whether the file is
cached. `--details` adds full names, source URLs, and usage rights. Listing is
offline.

The default selection is Tahoe Beach. Legacy Apple MOV IDs, including
`tahoe-beach`, `sequoia-sunrise`, `sequoia-morning`, `sequoia-night`, and
`golden-gate`, remain valid when named explicitly.

## Download and preview

```sh
genkan assets preview macos-landscape-iceland-snow-caps
genkan assets preview monterey-graphic --appearance dark
genkan assets download macos-landscape-tahoe-day
```

`assets preview` downloads the original if it is missing, then opens a normal
window with simulated authentication and power actions. A cached preview makes
no network request, and closing the window changes no settings. A failed
download or validation stops before the window opens and leaves no partial
installation. `assets download` caches the media without opening a window,
and reuses a matching verified cache entry.

Downloading does not change which wallpaper login or lock uses, and it fills
only the cache of the account that ran it. Pass the absolute media path that
`download` prints to `--wallpaper-file`; do not construct a path under `mov/` or
`heic/`.

Transfers show a live bar with downloaded and total MiB, percentage, average
speed, and ETA; an unknown length shows an indeterminate bar. A separate
`Validating` status covers integrity and media checks, so 100% means
transferred, not yet installed. Redirected output prints plain completion and
validation lines instead.

Every download, including one started by `preview`, is bounded and validated
before Genkan uses it:

| Rule | Value |
| --- | --- |
| Source | A direct HTTP(S) response with status 200; redirects are rejected |
| Size limit | 2 GiB by default for the transfer and the expanded media; `--max-bytes` accepts 4 KiB–16 GiB |
| Transfer timeout | 600 seconds |
| Integrity | Catalog byte sizes and any SHA-256 pins must match; checks cannot be disabled |
| Media check | A representative HEVC MOV or HEIC frame must decode; MOVs also get a generated poster |
| Preparation | Hashing and ZIP extraction are resource- and time-bounded; originals are never resized |

Apple's Tahoe MOV entries declare byte sizes but no upstream hashes. Without a
trusted pin, the hash Genkan computes records the file's identity, not its
upstream authenticity.

## Use a wallpaper in login and lock

Select a cached catalog wallpaper by ID:

```sh
genkan assets download catalina
genkan login --wallpaper catalina --appearance dark
genkan lock --wallpaper catalina --reduce-motion
```

An ID uses the published cache version at playback startup. Missing media shows
the generated background without downloading anything. An absolute
`--wallpaper-file` overrides the ID and accepts an existing MOV or HEIC; URIs,
playlists, and GStreamer pipeline descriptions are rejected. Saved absolute
overrides keep selecting the version they name after a replacement; change the
override to select a newer version.

How a wallpaper plays depends on its format:

| | MOV | HEIC |
| --- | --- | --- |
| Playback | Loops over the reported stream duration, or a legacy entry's recorded duration when the stream reports none; without either, plays to the end and holds its last frame | Follows its own time-of-day schedule |
| `--reduce-motion` (alias `--static-wallpaper`) | Shows the poster without starting GStreamer | Keeps the schedule but changes frames without dissolving |
| `--appearance light` or `dark` | Not used; applies only to HEICs | Selects the matching static appearance image, or the primary image when the file has no appearance pair; `automatic`, the default, uses a valid `h24` schedule, then the light appearance image, then the primary image |
| Playback failure | Before the first frame, keeps the poster, or the generated background without one; later, keeps the last displayed frame | Keeps the poster or last valid frame |

Login and lock never request location. HEIC parsing runs in an isolated worker
process; see [wallpaper decoding](architecture.md#wallpaper-decoding) for its
limits.

## Provision the greeter's wallpaper

A desktop user's downloads do not reach the greeter, which runs as its own
user. On NixOS, let the module prepare the media before greetd starts:

```nix
programs.genkan = {
  enable = true;
  greeter.enable = true;
  wallpaper.source.catalog = "macos-landscape-tahoe-day";
};
```

Choose exactly one source:

| `programs.genkan.wallpaper.source` | Example |
| --- | --- |
| `catalog` | `"macos-landscape-tahoe-day"` (IDs from `genkan assets list`) |
| `url` | `"https://example.org/original.mov"` (direct HTTP(S), no redirects) |
| `file` | `/path/to/original.heic` or `"/srv/wallpapers/original.mov"` |

`source.sha256` optionally supplies a trusted 64-digit hexadecimal SHA-256, and
`wallpaper.maxBytes` defaults to 2 GiB (range 4 KiB–16 GiB). Media passes the
same validation as a manual download. This is not an exhaustive decode of
every frame or a promise of compositor or hardware support.

`genkan-wallpaper.service` prepares the media at boot and whenever its
configuration changes, before greetd, as the dedicated `genkan-wallpapers`
system user. Its state lives in `/var/lib/genkan-wallpapers`: greeter and
desktop users can read the published media, but only the provisioning identity
and root can write it.

- Local sources must be readable by `genkan-wallpapers`. Nix path literals
  enter the store; absolute strings refer to runtime files.
- Do not embed secrets in source URLs.
- An unchanged URL reuses verified cached content without contacting the
  server; local sources are rehashed to detect changes. To refresh a mutable
  URL, use a new URL or a trusted hash for the new content.
- An atomic `selected` symlink changes only after a successful preparation.
  Failures are logged in the service journal and keep the previous selection.
  A failed service does not block greetd; without any successful selection the
  generated background remains.
- After fixing the source or connectivity, retry with
  `sudo systemctl restart genkan-wallpaper.service`. Running playback keeps its
  media until it restarts; provisioning never restarts greetd or a desktop
  session.

The module installs a wrapper as the system `genkan` and uses it for both the
Cage and niri greeters, so desktop autostart can run `genkan wallpaper` and lock
hooks can run `genkan lock --daemonize`. The wrapper resolves the selected
media and passes `--wallpaper-file`, so it does not depend on greetd forwarding
daemon environment variables. Explicit `--file` or `--wallpaper-file` arguments,
including `greeter.wallpaperFile`, win; a `--wallpaper` ID does not. For an explicit Nix path, use
`${config.programs.genkan.wallpaper.package}/bin/genkan`, not the raw
`programs.genkan.package` or the flake package, which remain media-free.

Outside NixOS, the same provisioning interface is available:

```sh
genkan assets prepare iceland
genkan assets prepare --url "https://example.org/original.mov"
genkan assets prepare --file /absolute/path/to/original.heic
genkan assets selected
```

`assets selected` only prints the last prepared media path; unwrapped playback
does not read it, so pass that path as `--wallpaper-file`. Keep a shared cache
in an absolute `GENKAN_WALLPAPER_DIR` writable only by a dedicated provisioning
identity or root. Setting `systemd.services.greetd.environment.GENKAN_WALLPAPER_DIR`
alone is not enough, because greetd does not forward arbitrary daemon variables
into PAM sessions.

## Run a desktop wallpaper

`genkan wallpaper` runs as an ordinary client in your logged-in session and
draws one non-interactive background surface per output:

```sh
genkan wallpaper --wallpaper catalina
genkan wallpaper --file /home/alice/Pictures/dynamic.heic
```

It requires `zwlr_layer_shell_v1`, so it works on niri, Sway, River, Hyprland,
and other compatible wlroots-oriented compositors, and fails rather than
opening an ordinary window without it. GNOME Shell and KDE Plasma need their
own wallpaper APIs and are not supported.

| Option | Effect |
| --- | --- |
| `--wallpaper ID` | A cached catalog MOV or HEIC, as in login |
| `--wallpaper-file PATH` | An absolute MOV or HEIC; overrides the ID |
| `--file PATH` | An absolute `.heic` or `.heif`; required for `--solar` |
| `--solar` | [Solar scheduling](#optional-solar-scheduling) |
| `--appearance`, `--reduce-motion` | As in [login and lock](#use-a-wallpaper-in-login-and-lock) |
| `--skip-output NAME` | Omit a connector or persistent monitor name; no decoder starts when every output is skipped |

`--file` accepts only an existing regular file; URIs, FIFOs, devices, and
pipeline descriptions are rejected. Genkan never uploads, copies into the store,
or publishes local files. Frames are shown in sRGB: an embedded ICC profile is
converted, and a file whose profile cannot be converted, such as a non-RGB,
malformed, or over-1 MiB profile, is refused. See
[desktop wallpaper files](architecture.md#desktop-wallpaper-files) for how the
file is bound and parsed.

The desktop process never contacts greetd or PAM, acquires a session lock,
selects sessions, or requests power operations. A running `genkan lock` covers
it.

### Start it with your compositor

Start the command from the compositor's own autostart so it inherits the
session's `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, and lifetime, and disable any
competing wallpaper process such as `swaybg`. In Sway:

```text
exec genkan wallpaper --file /home/alice/Pictures/dynamic.heic
```

Plain `exec` runs at the next Sway startup; `swaymsg reload` does not run it. To
start it in the current session, stop the competing wallpaper and run
`swaymsg exec 'genkan wallpaper --file /home/alice/Pictures/dynamic.heic'` once.
Avoid `exec_always` unless you stop the previous instance, because each reload
starts another process. Other compositors use their own `spawn-at-startup` or
`exec-once`; for niri, see [niri desktop wallpaper](niri.md#desktop-wallpaper).

Genkan deliberately installs no systemd user service and does not wrap the
desktop session command: a UID-wide user manager may not identify the intended
Wayland session, and wallpaper startup must not delay desktop startup.

### Optional solar scheduling

`--solar` schedules a HEIC that carries Apple `solar` metadata by the sun's
position at your location, through the logged-in user's GeoClue agent:

- Genkan requests only city-level accuracy, and only after the first frame is
  on screen, so location never delays startup.
- A fix is kept in memory, refreshed at most every six hours, and never written
  to disk.
- GeoClue may use a network geolocation provider, according to host policy.
- On denial, timeout, an unavailable service, or a fix coarser than city level,
  Genkan falls back to a valid `h24` schedule, then to the declared appearance
  or primary image. Losing the network after a fix keeps the fix. No location
  outcome stops the wallpaper or affects login or lock.

Genkan exposes no location provider, service URL, authorization, or coordinate
option. On NixOS, provision GeoClue and authorize the `genkan-wallpaper`
application with:

```nix
programs.genkan = {
  enable = true;
  wallpaper.enable = true;
  wallpaper.solar.enable = true;
};
```

Evaluation fails if `solar.enable` is set without the other two. These options
neither start the wallpaper nor opt it into location use; that still takes
`genkan wallpaper --file … --solar` in your session.

## Download a custom wallpaper

```sh
genkan assets download my-wallpaper --url "$SOURCE_URL" --sha256 "$SHA256"
genkan assets preview my-wallpaper --reduce-motion
```

`--sha256` is optional. `--url` cannot replace a catalog key. A custom download
records its observed format, dimensions, size, and computed SHA-256 in the
cache, and appears in later `list` and `preview` commands.

### Use your own catalog

`--catalog sources.toml` replaces the curated entries with your own
`[assets.<key>]` entries. Only `url` is required:

```toml
[assets.mountains]
url = "https://source.example/mountains.mov"
name = "My Mountains"
format = "mov"
width = 3840
height = 2160
byte_size = 790758290
duration_seconds = 120.0
rights = "Record the source's actual usage terms here"
attribution = "Original author"
# sha256 = "64 hexadecimal digits from a trusted source"
# source_page_url = "https://source.example/mountains"
# preview_url = "https://source.example/mountains.jpg"
```

```sh
genkan assets list --catalog sources.toml
genkan assets preflight mountains --catalog sources.toml
genkan assets download mountains --catalog sources.toml
genkan assets preview mountains --catalog sources.toml
```

Supplied format, byte size, and dimensions are checked against the download;
duration, rights, attribution, and preview URLs are informational, and poster
URLs are never fetched. Unknown metadata lists as `?`. Observations fill
missing fields but never override the catalog's URL, pins, or geometry. A
replacement catalog lists only its own entries, not unrelated cached custom
downloads.

A ZIP source needs `archive_member` (the exact member name),
`archive_byte_size`, and `archive_sha256`, plus the extracted media's
`byte_size` and `sha256`. Genkan extracts only that member to a controlled
filename and verifies both pins; archive paths are never unpacked, and it does
not search ZIPs for media.

## Check a source before downloading

`genkan assets preflight` is an optional quick check, not a prerequisite for
downloading:

```sh
genkan assets preflight macos-landscape-tahoe-day
genkan assets preflight my-wallpaper --url "$SOURCE_URL"
```

It reads parts of the file with HTTP range requests, including tail metadata
when needed, decodes one HEVC MOV frame
or the HEIC primary image, and inspects HEIC scheduling metadata. It installs
nothing, does not remember custom `--url` entries, and cannot verify the
whole-file hash, every frame, or a particular GPU renderer.

| Exit code | Meaning |
| --- | --- |
| 0 | Passed |
| 1 | Command or configuration error |
| 2 | Unsupported, or the source does not match the catalog |
| 3 | Inconclusive |

The media-byte budget is 8 MiB by default (`--max-bytes` accepts 4 KiB–64 MiB),
with at most 128 range requests and a 35-second deadline; headers, TLS, and
transport overhead are not counted. Multiple reads need a consistent strong
ETag to pass. Redirects, ignored or malformed ranges, changed sources, TLS
failures, and exhausted budgets are inconclusive. ZIP sources cannot be
partially inspected. Cancellation stops the worker and its request. An
inconclusive or unsupported result never triggers or
blocks a download, which validates the complete file independently.

Apple's root certificate is supplied only to requests for the exact host
`sylvan.apple.com`; it does not change system trust or disable certificate or
hostname validation.

## Manage the cache

The first applicable location wins:

| Environment | Cache root |
| --- | --- |
| `GENKAN_WALLPAPER_DIR` set | That absolute directory; a relative value is an error |
| Absolute `XDG_CACHE_HOME` | `$XDG_CACHE_HOME/genkan/wallpapers` |
| Absolute `HOME` | `$HOME/.cache/genkan/wallpapers` |

Each download prepares a new version under `objects/download-<key>-<random>/`,
holding `<key>.mov` or `<key>.heic`, a `<key>-poster.jpg` for a MOV, and
metadata. Only after preparation succeeds does an atomic `entries/<key>.toml`
receipt select it, and readers take metadata, media, and poster from one
receipt. A failure leaves the previous version usable. This does not promise
that every process switches at once, or durability after power loss.
Conventional `mov/` and `heic/` directories hold only explicitly provisioned
local or test files, never downloads.

Ctrl+C or SIGTERM stops preparation and cleans its staging. After SIGKILL or a
crash, the next explicit download cleans abandoned, unpublished staging under
the cache lock. Nothing ever deletes a published version, and playback performs
no cleanup, so running previews and saved overrides stay valid. Replacements
therefore use extra disk space; remove an old version only when no reader or
saved override needs it.

## Provenance and licensing

Asset provenance, upstream delivery, integrity, and loop behavior are recorded
in [RFD 2](../rfd/0002/README.adoc) and the
[wallpaper manifest](../assets/wallpapers/manifest.toml). The package installs
the manifest and the [Tahoe aerial catalog](../assets/wallpapers/macos-tahoe-aerials.toml)
under `share/genkan/wallpapers`, but neither the package nor the development
shell contains MOVs, HEICs, or posters; Nix fetches no artwork. Source JPEGs in the repository are test-only references. Downloads
come from the curated upstream sources, with no runtime or build dependency on
R2. These records do not assert an open-content license for upstream artwork.
