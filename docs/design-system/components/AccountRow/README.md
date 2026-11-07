# AccountRow

A centered row of avatars with names for choosing an account, in place of the single identity.

Each account is an Avatar over its name in `type-name`, `account-width` wide with `account-gap` between them; long names truncate with an ellipsis. Arrow keys move the `focus-ring`; Return or a click chooses.

**Provide.** The accounts, the focused index, and a select handler.
