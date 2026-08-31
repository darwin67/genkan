# Deferred Work

This register collects valid follow-up work that Genkan is intentionally not
performing yet. Each item records why it is deferred and the condition that
makes it actionable. Detailed design and verification evidence remains in the
linked source documents.

When an item becomes actionable, move it into an implementation plan or issue
and update this register in the same change. Do not mark a deferred capability
as implemented merely because an experimental dependency branch exists.

## Iced semantic accessibility and announcements

- **Scope:** Publish semantic roles and unique names for account tiles, expose
  modal action and consequence metadata, and announce authentication, session,
  cancellation, and power status changes to assistive technology.
- **Context:** Genkan pins iced 0.13.1. Stock iced 0.13 and 0.14 and the current
  0.15 development branch do not expose a merged semantic accessibility tree,
  Linux screen-reader bridge, or selective live-region/status-announcement API.
  Upstream tracks the direction in
  [iced #552](https://github.com/iced-rs/iced/issues/552), while its AccessKit
  RFC and implementations remain unmerged experiments without a committed
  release target.
- **Why deferred:** A local parallel accessibility tree could diverge from the
  actual iced widget state. Experimental forks and partial widget prototypes do
  not provide a supportable application contract.
- **Resume when:** A released upstream iced version provides semantic widget
  identity, roles, names/descriptions, focus/actions, Linux screen-reader
  delivery, and a selective announcement mechanism as applicable to each
  follow-up. Verify the behavior with assistive technology before claiming it.
- **Source:** [RFD 0001 accessibility verification](../rfd/0001/ACCESSIBILITY.md#upstream-roadmap-and-deferral-criteria).

## Physical ARM graphics validation

- **Scope:** Run the packaged greeter and `make hardware-smoke` on physical
  aarch64 Linux graphics hardware and a connected display.
- **Context:** Native x86_64 and aarch64 CI launch the package with Mesa software
  Vulkan. Physical AMD and NVIDIA validation passed on the x86_64 Framework
  workstation, including its connected eDP and external DisplayPort outputs.
- **Why deferred:** No physical aarch64 Linux graphics device is currently
  available. Software-rendered ARM CI does not validate a real driver, DRM
  device, or display path.
- **Resume when:** A supported aarch64 Linux device with physical graphics and a
  connected output is available. Retain the device, driver, output, and smoke
  result as evidence.
- **Source:** [graphics runtime review finding](review-findings.md#12-validate-graphics-runtime-packaging).

## Packaged wallpaper artwork

- **Scope:** Select or create final wallpaper artwork and package any generated
  variants.
- **Context:** RFD 0001 deliberately accepts Genkan's generated background and
  treats final wallpaper artwork as a separate decision. Apple assets are not
  eligible for reuse.
- **Why deferred:** Artwork is not required for the implemented interaction
  hierarchy. Shipping it requires source provenance, redistribution rights,
  variant-generation rules, and packaging behavior to be reviewed together.
- **Resume when:** The project chooses to replace or supplement the generated
  background and has reviewable artwork plus licensing and packaging details.
- **Source:** [RFD 0001 visual rules and non-goals](../rfd/0001/README.adoc#visual-and-material-rules).

## Stronger continuous contrast proof

- **Scope:** Strengthen numeric contrast verification from representative field
  coverage to a proof over continuously antialiased background and edge
  coverage.
- **Context:** Current renderer-aware tests cover 125 generated background
  combinations, both iced blend spaces, control states, and bright overlay
  underlays. RFD 0001 accepts that as its numeric boundary and does not claim an
  exhaustive continuous proof.
- **Why deferred:** The current coverage verifies the shipped generated
  background and control tokens. A stronger proof needs a renderer-level model
  of continuous coverage and is not required to close RFD 0001.
- **Resume when:** The renderer, blend path, background model, or packaged
  artwork changes materially, or the project adopts exhaustive continuous
  coverage as an acceptance requirement.
- **Source:** [RFD 0001 accessibility verification](../rfd/0001/ACCESSIBILITY.md#supported-checks).

## Not deferred

- The pre-layout screenshot baseline was never retained and cannot be recreated
  as historical evidence. Current durable references do not substitute for it.
- Host suspend/resume reliability belongs to the kernel, firmware, compositor,
  and display-driver stack. Genkan reports logind request failures but cannot
  make platform suspend or resume hardware-safe.
