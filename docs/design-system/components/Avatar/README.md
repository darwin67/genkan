# Avatar

A `avatar-size` disc with the account's picture clipped to the circle under the `avatar-ring` border. Pictures replace the gradient and initials without changing the size, ring or layout on login, account tiles or lock.

While loading, with no picture, or on any failure, show initials over the `avatar-top`→`avatar-bottom` gradient. Initials use `type-initials`, the first letters of the first two words. A keyboard-focused avatar in an account row gets the `focus-ring`, whether it shows a picture or initials.

The built-in set is Lake, Sunrise, Dune, Leaf, Wave, Moon, Pine, Bloom, Orbit, Pebbles, Aurora and Citrus: original MIT-licensed artwork in [`assets/avatars/`](../../../../assets/avatars/). The 512×512 PNGs are rendered ahead of time with `make update-avatars` and committed alongside their SVG sources. Packaging checks their hashes at evaluation and installs them as-is under `share/genkan/avatars/`.

Users choose a built-in or import a PNG/JPEG photo through [`genkan avatar`](../../../deployment.md#account-pictures). [RFD 7](../../../../rfd/0007/README.adoc) specifies AccountsService storage and decoding in a confined process, never in the credential-handling process.

**Provide.** The display name, an optional decoded picture, and whether it has keyboard focus. Never pass an encoded picture to a credential-handling renderer.

The static preview uses the committed Lake and Moon PNGs to illustrate picture fill, followed by the loading and failure initials states.
