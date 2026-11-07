# Alert

A macOS-style confirmation over `scrim-modal`, used before Restart and Shut Down.

The alert is `alert-width` wide with `radius-alert` in `alert-material`. It shows an icon disc, a bold question in `type-alert-title`, the consequence in `type-alert-body` and `text-secondary`, and two pill buttons `pill-height` tall: Cancel in `pill-secondary` and the action in `accent`. Cancel has initial keyboard focus, which differs from macOS so a stray Return never powers off the computer.

**Provide.** The action, its consequence, and cancel and confirm handlers.
