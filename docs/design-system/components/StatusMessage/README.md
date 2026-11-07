# StatusMessage

PAM's own text, centered on the line directly under (or, for a prompt, above) the pill.

It uses `type-message` in `text-primary` with a soft shadow, wraps at `message-width`, and appears only when PAM sends text. A rejected password on its own shows no message: the pill shakes instead, as on macOS.

**Provide.** The message, and whether it is a prompt (above) or a notice (below).
