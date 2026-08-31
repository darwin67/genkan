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

The check obtains the complete fixture list from the packaged binary and adds a
default-size `fixture-*.png` capture for every fixture not already represented
above. This makes service-isolation and rendered-frame checks exhaustive when a
new fixture is added without expanding the human review matrix unnecessarily.

The capture check fails when a screenshot does not have its declared dimensions,
has fewer than 16 colors, does not stabilize across two consecutive frames, the
application exits before or during capture, or `strace` observes any connection
other than the case's Wayland socket. The latter protects the AccountsService,
greetd, and logind isolation boundary without relying on configured decoy socket
names. Unit tests separately verify simulated submission clears the response,
account changes clear a simulated response, and every power action remains
non-destructive.

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

## Durable reference images

The six primary acceptance views are retained in
[`reference-images/`](reference-images/) for review across later design changes:

| View | Reference |
|---|---|
| Account selection | [`account-selection.png`](reference-images/account-selection.png) |
| Secret prompt | [`secret-prompt.png`](reference-images/secret-prompt.png) |
| Visible prompt | [`visible-prompt.png`](reference-images/visible-prompt.png) |
| Authentication failure | [`authentication-failure.png`](reference-images/authentication-failure.png) |
| Power confirmation | [`power-confirmation.png`](reference-images/power-confirmation.png) |
| Narrow flow layout | [`narrow-selected.png`](reference-images/narrow-selected.png) |

Run `make update-reference-images` to deliberately refresh them from the current
packaged `preview-evidence` output. `make check-rfds` verifies the exact manifest,
PNG headers, and dimensions. It does not compare pixels: these images preserve
review context without turning compositor output into a brittle visual test.

## Recorded review outcome

The generated captures were reviewed on 2026-08-30 with these results:

| Captures | Outcome |
|---|---|
| Default account and authentication states | Controls preserve the intended hierarchy, focus indication, and notice ownership. |
| 1440×900 large account set | The bounded grid presents complete first-row identities and a visible continuation within its scrollbar. |
| 1920×1080 and 2560×1080 | The centered identity flow remains bounded while corner utilities retain their secondary placement. |
| 480×600 selected account | Content uses the vertical flow without horizontal clipping; the page scrollbar remains available. |
| 480×600 long authentication | Long identity and PAM text wrap; focus reveal leaves the input and submit action fully visible with bottom margin. |

These textual outcomes and the six primary review images are durable. The
historical pre-layout baseline remains unavailable because it was not captured
before implementation began; current images are not presented as substitutes.
