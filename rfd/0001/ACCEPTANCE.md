# RFD 0001 acceptance evidence

This document records review evidence for the interaction and responsive-layout
requirements in [RFD 0001](README.adoc). Screenshots support human review; exact
pixels are not the behavioral contract.

## Automated evidence

Run `make evidence` to build the packaged application and capture deterministic
preview images under headless Weston's kiosk shell. The Nix output contains:

| Capture | Size | Fixture | Purpose |
|---|---:|---|---|
| `account-selection.png` | 1280×800 | `users` | Account hierarchy and visible usernames |
| `secret-prompt.png` | 1280×800 | `secret-prompt` | Secret input and submit action |
| `visible-prompt.png` | 1280×800 | `visible-prompt` | Visible PAM response input |
| `authentication-failure.png` | 1280×800 | `authentication-failure` | Inline failure and recovery action |
| `power-confirmation.png` | 1280×800 | `power-confirmation` | Modal hierarchy and safe initial action |
| `laptop-large-accounts.png` | 1440×900 | `large-account-set` | Laptop composition and account-grid bounds |
| `widescreen-users.png` | 1920×1080 | `users` | 16:9 composition |
| `ultrawide-selected.png` | 2560×1080 | `selected` | Ultrawide composition |
| `narrow-selected.png` | 480×600 | `selected` | Narrow flow layout |
| `narrow-long-authentication.png` | 480×600 | `long-authentication` | Wrapped long prompt and focused input reveal |

The capture check fails when a screenshot does not have its declared dimensions,
the application exits early, or `strace` observes a preview connection attempt
to the configured greetd socket or system D-Bus socket. The latter protects the
AccountsService, greetd, and logind isolation boundary. Unit tests separately
verify simulated submission clears the response, account changes clear a
simulated response, and every power action remains non-destructive.

The ordinary `graphics-smoke` check remains responsible for launching the
production-mode package under nested Cage. The evidence check intentionally
uses kiosk shell directly so each preview receives the exact virtual output
dimensions instead of Cage's independent nested-output size.

## Review checks

Review generated captures using these stable criteria:

1. **Hierarchy:** time/date, active identity flow, session choice, and power
   actions retain their RFD-defined prominence and placement.
2. **Focus:** the active account, prompt input, or safe modal action has one
   visible focus indication. Tab order is verified independently by reducer and
   widget-state tests.
3. **Wrapping and reachability:** labels, prompts, and notices wrap rather than
   overflow horizontally. Narrow content is vertically scrollable, initial
   prompt focus reveals the input with a visible margin. Reducer tests verify
   Page Up and Page Down remain mapped while an input is focused, while Home
   and End scroll when no focused control consumes their conventional action,
   so the remaining notice and secondary controls stay reachable.
4. **Notice placement:** authentication, session, power, and preview messages
   remain in their source-owned regions and do not replace one another.
5. **Contrast:** screenshots may identify surfaces requiring measurement, but
   numeric WCAG acceptance remains a separate check over every packaged
   background. Visual inspection alone does not close that requirement.

The background and fixture clock are deterministic in preview mode. Compositor
timestamps and window chrome are excluded by kiosk-shell capture.
