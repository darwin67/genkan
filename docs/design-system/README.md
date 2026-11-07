Genkan's login and lock screens follow the macOS Tahoe login window and lock screen as closely as Linux licensing allows. Everything is drawn over a full-screen wallpaper that is never dimmed, so the system is a small set of quiet, translucent pieces placed exactly where macOS puts them.

> Status: [RFD 5](https://github.com/darwin67/genkan/blob/main/rfd/0005/README.adoc) defines how these tokens become code. `tokens.json` is the source of truth for values, measured from photos and a video of a real Tahoe login window and lock screen on a 3008×1692 display. Values are logical points, about ±10%.

Component previews in `components/` expect `tokens.json` compiled to CSS custom properties (`--<token-name>`, `--font-sans`) and `components/bundle.css` loaded before them.

## Principles

- **Follow macOS.** Match its layout, proportions, type and behaviour. Deviate only where greetd or Linux needs something macOS lacks, and then use a macOS idiom: a menu-bar item, a menu, an alert.
- **Never dim the wallpaper.** Legibility comes from soft text shadows and blurred glass behind small controls, not from darkening the picture.
- **Small and low.** The identity column sits `identity-bottom` above the bottom edge and stays small: a `avatar-size` avatar, a `type-name` name and a `field-width` pill.
- **Login and lock are one product.** Both use the same tokens, the same clock and the same pill. Lock drops only the session and power items.
- **Nothing depends on hover.** Every action is reachable from the keyboard in reading order.

## Color

There is one theme, `wallpaper`. Set text over the wallpaper in `text-primary`, the date in `text-date`, the clock in `clock-glass`, and placeholders in `text-placeholder`. Use `accent` only for the highlighted menu item and the default alert button, with white text. Use `focus-ring` for keyboard focus on buttons and on the focused account avatar.

## Material

Small controls are glass: a translucent fill over a background blur. The pill uses `field-glass` with a `field-border` hairline. Menus use `menu-material` and alerts `alert-material`, both with a `menu-border` hairline and a stronger blur. Only an alert darkens what is behind it, with `scrim-modal`.

## Type

Inter throughout, standing in for Apple's system font, which cannot ship on Linux. Use the `type-*` styles as named: `type-clock` and `type-date` for the clock, `type-name` under avatars, `type-field` in the pill, `type-bar` in the status bar, `type-menu` and `type-menu-heading` in menus, `type-message` for PAM text, and `type-alert-title` and `type-alert-body` in alerts.

## Layout

The clock is centered `clock-top` from the top. The status bar sits `bar-inset-top` from the top and `bar-inset-right` from the right. The identity column is centered with its pill `identity-bottom` above the bottom edge and `identity-gap` between items. With several accounts, an AccountRow replaces the column. Other monitors show the wallpaper only.

## Behaviour

- A wrong password shakes the pill horizontally and clears it. No error text appears unless PAM sends a message, which goes on the line below the pill.
- A later PAM prompt shows its text above the pill and takes visible input when PAM says it is not secret.
- The submit arrow appears only once there is input. The Caps Lock glyph appears beside it while Caps Lock is on.
- Restart and Shut Down open an Alert whose Cancel button has initial focus.

## Iconography

Status-bar items, the power glyph, the submit arrow, the Caps Lock glyph and the menu check mark are simple stroked line icons in the current text color. Apple's SF Symbols cannot ship on Linux; do not copy them.

## Writing

Use macOS wording where macOS has it: "Enter Password", "Sleep", "Restart…", "Shut Down…", and alert questions such as "Are you sure you want to shut down your computer now?". Show PAM text as given, and never echo a response or credential.
