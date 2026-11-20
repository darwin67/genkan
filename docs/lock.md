# Session Lock

`genkan lock` locks a running Wayland session with the same look as the login
screen, and unlocks it by reauthenticating the invoking real-UID account
through the host's `genkan-lock` PAM service. It does not replace your
desktop's locker automatically: bind it to a key and to your idle manager.

The lock is experimental. Its compositor boundary lives in the independently
tested `genkan-session-lock` workspace crate.

```sh
genkan lock                 # lock now, in the foreground
genkan lock --daemonize     # return once the lock is confirmed, for hooks
```

## Requirements

| Requirement | Detail |
| --- | --- |
| Compositor | Provides `ext_session_lock_manager_v1` and the rendering globals the lock uses |
| Kernel and libc | Linux 5.9 or newer and glibc 2.34 or newer, for pidfd, `close_range`, and descriptor-safe `posix_spawn` |
| PAM policy | `genkan-lock`, installed by `programs.genkan.enable` on NixOS; see [install](install.md#install-on-nixos) |

Genkan checks the connected compositor at runtime rather than guessing from
its name, and has no fullscreen or layer-shell fallback. niri and current
releases of Sway, River, Hyprland, and other wlroots compositors with
`ext-session-lock-v1` are expected to work. GNOME Shell, KWin releases without
the protocol, and desktops that reserve locking for their own shell must keep
their own locker.

The pinned Nix package meets the libc requirement; source builds on older
systems fail closed before PAM authentication begins. The authentication
worker is unprivileged and is not installed setuid.

## Lock on idle and before sleep

For Sway and other sessions using swayidle, lock after inactivity, on logind
lock requests, and before sleep. Use its wait mode (`-w`) so the before-sleep
command participates in swayidle's logind delay inhibitor:

```sh
swayidle -w \
  timeout 600 'genkan lock --daemonize' \
  lock 'genkan lock --daemonize' \
  before-sleep 'genkan lock --daemonize'
```

This is bounded, best-effort suspend integration, not a suspend veto:

- Swayidle does not inspect the command's exit status, and logind proceeds when
  `InhibitDelayMaxSec` expires even if the command is still running. Set that
  timeout above the host's worst-case locker startup time and monitor launcher
  failures.
- A startup failure can therefore let the machine suspend without a confirmed
  Genkan lock. Deployments that require fail-closed suspension must route
  suspend requests through a component that locks first and requests suspend
  only after confirmation.
- Never start Genkan from an after-resume hook: session content may already be
  visible before the locker starts.

For niri key bindings, idle locking, and lid handling, see
[niri locking](niri.md#locking).

## How `--daemonize` waits for the lock

`--daemonize` starts a fresh foreground child and returns only after the
compositor confirms that the session lock is active. It never forks after
Wayland, PAM, graphics, or wallpaper initialization. Concurrent manual and
suspend hooks join the same per-compositor lock and all wait for that
confirmation; a denied lock or a child lost before confirmation is an error.

The confirmed child remains the lock owner through sleep and resume. Resume
neither starts a second authentication attempt nor unlocks the session; the
existing PAM conversation continues when the user interacts with the lock
screen.

## What the lock shows

The lock covers every output, but shows the clock and password field on only
one; the others show unobstructed wallpaper, and nothing is dimmed. It chooses
that output like login; pass `--authentication-output DP-2` to prefer a
connector (see [Choose the authentication monitor](install.md#choose-the-authentication-monitor)).
The preference survives `--daemonize`, and authentication moves to the
fallback output if the chosen one disappears.

Login and lock share one composition and design tokens: the date and clock
near the top of the authentication output, and the avatar, name, and password
field near the bottom. Only the clock shrinks, and only on outputs too short
for both. Login adds a stronger outline to the focused control for keyboard
navigation. The lock omits login's session and power menus, has a single
active response field, masks every response, and asks for Return to start a
new attempt after a failure.
The shared look does not join their security backends: greetd owns login
authentication, and the `genkan-lock` PAM worker owns unlocking.

`--wallpaper`, `--wallpaper-file`, `--appearance`, and `--reduce-motion` work as
in login; see [wallpapers](wallpapers.md#use-a-wallpaper-in-login-and-lock).

## Recover from a locker failure

If Genkan, its renderer, or the compositor connection fails after the lock is
confirmed, Genkan never requests unlock, so the session stays locked and the
compositor may show a blank or fallback lock screen. Switch to another VT (for
example with Ctrl+Alt+F2), sign in there, and restart the trusted locker or
compositor, or end the affected graphical session. Killing the locker does not
reveal the session.
