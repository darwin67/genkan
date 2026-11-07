# StatusBar

The row of small items at the top right of login and lock, styled like the macOS menu bar.

Items sit `bar-inset-top` from the top and `bar-inset-right` from the right, `bar-height` tall, with `bar-gap` between them, in `type-bar` and `text-primary`. Login shows, left to right: the session item (desktop glyph and session name), the power item (power glyph), the keyboard layout, and network status. Lock shows only keyboard layout and network status. An item whose menu is open uses `item-active` behind it with `radius-item`.

**Provide.** The selected session, whether login or lock, and open handlers for the session and power menus.

**Don't.** Add items that only show information macOS keeps elsewhere, or hide session choice behind a hover.
