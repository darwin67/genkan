# PasswordField

The small grey glass pill that collects every PAM response, on login and lock alike.

The pill is `field-width` by `field-height` with `radius-field`, `field-glass` behind a background blur, and a `field-border` hairline. A short secret prompt fits inside the empty pill as a placeholder in `text-placeholder` and `type-field`; visible or longer prompts go above it.

Login displays responses to visible prompts as text and masks secret responses. Lock masks every response, even to a visible prompt. When there is input, an outlined `submit-size` arrow circle appears at the right. The Caps Lock glyph appears while Caps Lock is on. A rejected response clears the pill and shakes it horizontally unless reduced motion is on.

**Provide.** The prompt text, whether it is secret, the response, Caps Lock state, and submit and change handlers.

**Don't.** Show a label above the pill for an ordinary password prompt, show any response text on lock, or keep response text after submit.
