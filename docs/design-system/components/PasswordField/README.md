# PasswordField

The small grey glass pill that collects every PAM response, on login and lock alike.

The pill is `field-width` by `field-height` with `radius-field`, `field-glass` behind a background blur, and a `field-border` hairline. Empty, it shows the prompt as a placeholder in `text-placeholder` and `type-field`. Secret responses show as dots; visible responses (codes) show as text. When there is input, an outlined `submit-size` arrow circle appears at the right, and the Caps Lock glyph appears to its left while Caps Lock is on. A wrong response shakes the pill horizontally and clears it.

**Provide.** The prompt text, whether it is secret, the response, Caps Lock state, and submit and change handlers.

**Don't.** Show a label above the pill for an ordinary password prompt, or keep response text after submit.
