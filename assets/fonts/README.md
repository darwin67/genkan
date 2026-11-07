# Bundled fonts

Genkan bundles Inter so the login greeter (iced) and the session lock
(cosmic-text) render with identical metrics, instead of resolving a family name
through fontconfig. Inter stands in for Apple's system font, which cannot ship
on Linux. See [RFD 5](../../rfd/0005/README.adoc#typeface).

| File | Source | SHA-256 |
|---|---|---|
| `InterVariable.ttf` | `InterVariable.ttf` from the [Inter 4.1 release](https://github.com/rsms/inter/releases/tag/v4.1) (`Inter-4.1.zip`, SHA-256 `9883fdd4a49d4fb66bd8177ba6625ef9a64aa45899767dde3d36aa425756b11e`). Identical to nixpkgs `inter` 4.1. | `4989b125924991b90d05b2d16e0e388c48f7d5bb8b30539bbf9c755278d0ccaf` |
| `LICENSE.txt` | `LICENSE.txt` from the [Inter v4.1 tag](https://github.com/rsms/inter/blob/v4.1/LICENSE.txt) | `262481e844521b326f5ecd053e59b98c8b2da78c8ee1bdbb6e8174305e54935a` |

Inter is © The Inter Project Authors and licensed under the SIL Open Font
License 1.1, reproduced in `LICENSE.txt`. The font's family name is
`Inter Variable`; its weight axis covers 100–900.

To update, replace both files from a new upstream release, update the table,
and run `make test`, which checks that the bundled font resolves and that its
weight axis takes effect.
