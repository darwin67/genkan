# Review Findings

This register tracks decisions and remediation work from the full-repository
Oracle review performed on 2026-08-27. Findings remain here until they are
addressed or deliberately accepted.

Allowed decision states:

- **Pending**: not yet discussed.
- **Accepted**: recommendation approved; remediation remains.
- **Deferred**: valid finding intentionally postponed, with rationale.
- **Rejected**: recommendation will not be implemented, with rationale.

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
- **Resolution:** Partially started. State tests now cover stale responses,
  power failures, repeated and deferred close requests, and bounded shutdown.
  Fake-socket tests cover framing, stale-session recovery, `AuthError`
  cancellation ordering, and cancellation-failure fallback. A NixOS VM test
  exercises Genkan's shared client against real greetd 0.10.3 and PAM through
  incorrect credentials, cancellation/retry, successful authentication, and
  `StartSession`, verifying the launched user and environment. Table-driven UI
  transitions for both success stages and start failure remain.

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
