# RFD 0001 accessibility verification

## Supported checks

Genkan enforces the WCAG contrast requirements that can be verified from its
current presentation model:

- normal text and placeholders: at least 4.5:1;
- focus indicators, control boundaries, avatars, and selector handles: at least
  3:1;
- 125 combinations of the base background and three animated color fields,
  including absent, partial, and full coverage after global dimming;
- every active, hovered, pressed, and disabled control style, plus dialogs,
  menus, selected menu rows, selected input text, and scrollbar thumb states;
- menus over both dark and white page content, and modal dialogs over the same
  extremes after applying the production scrim.

The tests alpha-composite each foreground and material token over the sampled
background combinations using both iced rendering paths: linear-light blending
for the default wgpu renderer and encoded-sRGB blending for tiny-skia. They
model nested dialog and menu surfaces and each renderer's border layer order,
convert sRGB channels to relative luminance, and apply the WCAG contrast-ratio
formula. The generated preview evidence still provides a visual check, but
screenshots are not the numerical source of truth.

Contrast-critical quads intentionally avoid iced shadows because the pinned
wgpu shader combines shadow and quad alpha before framebuffer blending. Material
opacity, borders, focus rings, and the modal scrim provide hierarchy without
entering that renderer-specific alpha path.

The quarter-step field coverage is representative rather than an exhaustive
proof over continuously antialiased edges. That broader RFD item remains open;
the tests and checklist do not claim otherwise.

## Framework boundary

The pinned iced 0.13 widget and window backends do not expose an accessibility
tree, roles, accessible names, descriptions, or live-region/status announcement
API. Genkan therefore cannot yet publish a semantic account-tile name combining
the display name and username, nor can it mark status updates for assistive
technology. Visible labels and application-managed keyboard focus remain in
place, but are not claimed as substitutes.

Those semantic checklist items remain open until iced exposes a supported
accessibility backend or Genkan adopts a later release that does. Adding a local
parallel accessibility tree would duplicate widget state and is intentionally
out of scope.
