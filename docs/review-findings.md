# Review Findings

This register tracks decisions and remediation work from the full-repository
Oracle review performed on 2026-08-27. Findings remain here until they are
addressed or deliberately accepted.

Allowed decision states:

- **Pending**: not yet discussed.
- **Accepted**: recommendation approved; remediation remains.
- **Deferred**: valid finding intentionally postponed, with rationale.
- **Rejected**: recommendation will not be implemented, with rationale.
- **Addressed**: remediation completed and verified.

## 1. Cancel abandoned greetd authentication sessions

- **Severity:** High
- **Decision:** Accepted in full. Power failures must preserve authentication
  state; voluntarily abandoned attempts and graceful window exits must cancel
  active greetd sessions; retries must recover stale sessions; attempt IDs
  must prevent late responses from mutating newer state. A server-returned
  `AuthError` does not require cancellation because greetd invalidates it.
- **Finding:** Active greetd sessions can be dropped without `CancelSession`,
  potentially leaving greetd unable to accept another login attempt. Late
  asynchronous responses can also update an abandoned attempt.
- **Recommendation:** Separate power errors from authentication failure; add
  explicit cancellation for abandoned attempts and graceful window close;
  recover stale daemon sessions before reconnecting; identify attempts so late
  responses can be ignored.
- **Resolution:** Not started.

## 2. Model session and desktop identities separately

- **Severity:** High
- **Decision:** Accepted. Store the desktop-file session ID separately from all
  `DesktopNames` values. Set `XDG_SESSION_DESKTOP` from the filename and
  colon-join desktop names for `XDG_CURRENT_DESKTOP`; omit the latter when no
  names are declared. Explicit fallback sessions must define both identities.
- **Finding:** `XDG_SESSION_DESKTOP` and `XDG_CURRENT_DESKTOP` are populated
  from the same first `DesktopNames` value instead of their distinct sources.
- **Recommendation:** Derive the session ID from the desktop filename and
  colon-join all `DesktopNames` values for `XDG_CURRENT_DESKTOP`.
- **Resolution:** Not started.

## 3. Pin third-party CI actions

- **Severity:** High
- **Decision:** Accepted. Pin the Nix installer and Conventional Commit
  validator to reviewed full commit SHAs with adjacent version comments.
  Future action updates remain explicit dependency changes. Dependabot may be
  added later but is not required to address this finding.
- **Finding:** The Nix installer and Conventional Commit validation actions use
  mutable branch or version references.
- **Recommendation:** Pin every third-party action to an audited full commit
  SHA and retain a version comment for update tooling and reviewers.
- **Resolution:** Not started.

## 4. Parse Desktop Entry `Exec` according to the specification

- **Severity:** Medium
- **Decision:** Accepted with conservative execution rules. Use a maintained
  Desktop Entry parser; validate type, visibility, `TryExec`, and localized
  names; apply Desktop Entry quoting rather than shell parsing; support `%%`
  and safe metadata substitutions; reject file, URL, icon, or unknown field
  codes that have no defined login-session value; never invoke a shell.
- **Finding:** Shell parsing and whole-argument field-code removal do not
  implement Desktop Entry quoting, escaping, and field-code behavior.
- **Recommendation:** Use a Desktop Entry parser with explicit `Exec`
  expansion, or conservatively reject unsupported field-code forms. Validate
  entry type, executable availability, visibility, and localized names.
- **Resolution:** Not started.

## 5. Honor session search precedence and masking

- **Severity:** Medium
- **Decision:** Accepted. Honor configured `XDG_DATA_DIRS` order and use the
  standard directories only as a fallback. Resolve precedence by relative
  desktop filename, let hidden entries mask lower-priority copies, permit
  distinct entries with identical commands, sort visible sessions
  deterministically, and add the configured fallback only when its identity is
  absent.
- **Finding:** Search order does not follow `XDG_DATA_DIRS`; deduplication uses
  commands instead of relative filenames; hidden entries do not mask
  lower-priority entries.
- **Recommendation:** Build the search path from `XDG_DATA_DIRS` with standard
  defaults only when unset, and track entries by relative filename with hidden
  entries represented as tombstones.
- **Resolution:** Not started.

## 6. Make power confirmation modal

- **Severity:** Medium
- **Decision:** Accepted; retain Sleep, Restart, and Shut Down controls. Model
  idle, confirmation, and execution states explicitly; block underlying input
  and duplicate requests while modal; allow cancellation; and report logind
  failures without changing the active greetd authentication state.
- **Finding:** Authentication, session, and power controls can still receive
  events behind the confirmation overlay, and concurrent power requests are
  possible.
- **Recommendation:** Suppress underlying handlers while confirmation is open
  and track a pending power operation to prevent duplicate requests.
- **Resolution:** Not started.

## 7. Remove fixed personal package defaults

- **Severity:** Medium
- **Decision:** Accepted with device discovery. Query AccountsService over the
  system D-Bus for cached, unlocked, non-system users; auto-select a sole user
  and show a selector for multiple users; source display names and avatars from
  account metadata without broad home-directory access. Discover Wayland
  sessions from XDG entries. Keep identity and session CLI values only as
  optional administrative overrides, and report clearly when discovery finds
  no usable user or session.
- **Finding:** The packaged binary defaults to the `darwin` account and a
  Sway-specific command, making an apparently generic package host-specific.
- **Recommendation:** Require or derive user identity and require or verify the
  session command. Alternatively, explicitly define Genkan as a personal,
  single-user greeter.
- **Resolution:** Not started.

## 8. Reject invalid session commands

- **Severity:** Medium
- **Decision:** Accepted by removing `--session-command`. Valid discovered
  desktop entries are the sole session source; host-specific arguments such as
  `sway --unsupported-gpu` belong in a NixOS-installed session entry. Report a
  clear configuration error when no valid session is available, and never
  substitute an implicit compositor command.
- **Finding:** Empty or malformed `--session-command` input silently falls back
  to Sway.
- **Recommendation:** Validate the value through Clap and fail at startup when
  it is malformed or empty.
- **Resolution:** Not started.

## 9. Focus and gate PAM input by phase

- **Severity:** Medium
- **Decision:** Accepted. Assign a stable input ID and focus it for every PAM
  prompt; accept edits and submission only in `WaitingForInput`; clear input
  between prompts; disable it while processing; use a separate Retry control
  for idle or disconnected states; and restore focus only when the next prompt
  is ready.
- **Finding:** PAM input is not focused when a prompt arrives and remains
  editable while input is not valid, causing discarded or misleading text.
- **Recommendation:** Give the input a stable ID, focus it for each prompt,
  omit input handlers outside `WaitingForInput`, and use a distinct retry
  control in `Idle`.
- **Resolution:** Not started.

## 10. Bound authentication retry behavior

- **Severity:** Medium
- **Decision:** Accepted with explicit retry. Stop after authentication, socket,
  or protocol failure; show a clear failed state; require the Retry control to
  create a fresh greetd attempt; and focus the next prompt when ready. Do not
  add automatic backoff initially because user action already bounds retries.
- **Finding:** An account rejected before a prompt can trigger an immediate,
  unbounded authentication retry loop.
- **Recommendation:** Require explicit user retry after failure or introduce a
  bounded delay and backoff.
- **Resolution:** Not started.

## 11. Test the authentication state machine and IPC

- **Severity:** Low
- **Decision:** Accepted. Isolate authentication transitions for table-driven
  tests covering prompt sequences, messages, failure, retry, cancellation,
  stale responses, both success stages, and start failure. Add a fake greetd
  Unix-socket server to verify framing, requests, cancellation, and the session
  command environment. Retain value-level tests; do not require a coverage
  percentage initially.
- **Finding:** Current tests cover value conversion but not multi-step PAM
  conversations, cancellation, retries, stale results, both success states, or
  serialized greetd IPC.
- **Recommendation:** Isolate the state reducer for table-driven transition
  tests and add a fake Unix-socket greetd server for end-to-end protocol tests.
- **Resolution:** Not started.

## 12. Validate graphics runtime packaging

- **Severity:** Low
- **Decision:** Accepted in two stages. Add the Nix graphics-driver runpath and
  packaged-binary smoke coverage, including a nested/headless Wayland launch
  with software rendering where practical. Before resolution, test Cage on the
  Framework AMD GPU, verify NVIDIA is not unnecessarily activated, exercise
  external displays, and test an ARM Linux device when available. Record manual
  results here.
- **Finding:** Both architectures build, but CI does not launch Genkan under
  Cage and graphics-driver discovery has not been validated broadly.
- **Recommendation:** Add the Nix driver runpath where appropriate and perform
  Cage smoke tests on representative Mesa, NVIDIA, and ARM systems.
- **Resolution:** Not started.

## 13. Clear power status after successful suspend

- **Severity:** Low
- **Decision:** Accepted. Track the pending action; show sleep progress; clear
  it after resume with either no message or a short-lived welcome; restore PAM
  input focus; and preserve authentication throughout. Keep restart and
  shutdown pending until transition, but restore the greeter and report any
  logind error without resetting authentication.
- **Finding:** The greeter continues to display “Requesting sleep…” after the
  system resumes.
- **Recommendation:** Track pending power state and clear or replace the status
  after success or resume.
- **Resolution:** Not started.

## 14. Constrain PAM message layout

- **Severity:** Low
- **Decision:** Accepted
- **Finding:** Long PAM prompts and status messages can overflow the fixed
  panel.
- **Recommendation:** Constrain message width, enable wrapping, and cap
  pathological message lengths if necessary.
- **Resolution:** Use wrapped labels above the input rather than relying on
  placeholder-only prompts. Constrain prompt and status content to the panel,
  make the panel responsive on smaller displays, and place unusually long
  content in a bounded scroll area. Apply a generous display-length cap to
  pathological PAM content while preserving informational and error message
  semantics. Add layout tests where practical.

## 15. Correct and expand runtime documentation

- **Severity:** Low
- **Decision:** Accepted
- **Finding:** The preview description does not match immediate authentication,
  and integration prerequisites and failure behavior are incomplete.
- **Recommendation:** Correct preview wording and document package binding,
  graphics expectations, account scope, and logind policy behavior.
- **Resolution:** State that authentication begins immediately and that users
  and sessions are discovered from AccountsService and installed Wayland
  desktop entries. Document NixOS package binding, greetd/Cage integration,
  graphics-driver expectations, logind authorization and failure behavior,
  and clear errors for unavailable runtime dependencies.

## 16. Align Conventional Commit policy and enforcement

- **Severity:** Low
- **Decision:** Accepted
- **Finding:** Policy requires Conventional Commits, but automation validates
  only pull request titles, and relevant release files do not trigger main CI.
- **Recommendation:** Enforce commit messages, or require squash merging and
  make PR titles authoritative. Include release and validation configuration
  in relevant CI path filters.
- **Resolution:** Validate every non-merge commit introduced by a pull request
  and continue validating pull request titles so squash merges are also
  conventional. Keep allowed types aligned with `cliff.toml`, document the
  merge-commit exemption, and include `cliff.toml` and
  `.github/workflows/commits.yml` in relevant CI path filters.

## 17. Add the declared MIT license text

- **Severity:** Low
- **Decision:** Accepted
- **Finding:** `Cargo.toml` declares MIT but the repository has no `LICENSE`
  file.
- **Recommendation:** Add the standard MIT license text.
- **Resolution:** Add `LICENSE` containing the standard MIT license text with
  `Copyright (c) 2026 Darwin D. Wu`.

## 18. Use one package version source

- **Severity:** Low
- **Decision:** Accepted
- **Finding:** The package version is maintained independently in `Cargo.toml`
  and `flake.nix`.
- **Recommendation:** Derive the Nix package version from `Cargo.toml` or add a
  check that requires both values to match.
- **Resolution:** Treat `Cargo.toml` as the single source of truth and derive
  the Nix package version from `package.version` using Nix's TOML parser.
