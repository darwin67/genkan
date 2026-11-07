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
- **Context:** Rechecked on 2026-11-07: Genkan resolves iced 0.14.0, still the
  latest upstream release. Its widget/operation APIs and winit backend do not
  expose a semantic accessibility tree, Linux screen-reader bridge, or selective
  live-region/status-announcement API. The same boundary holds at upstream
  [0.15.0-dev revision 8ff48c4](https://github.com/iced-rs/iced/tree/8ff48c42b0d5188223501ea1ccc506fd5beaf36a).
  [iced #552](https://github.com/iced-rs/iced/issues/552), the
  [AccessKit RFC](https://github.com/iced-rs/rfcs/pull/21), and experimental
  implementations [#1849](https://github.com/iced-rs/iced/pull/1849) and
  [#3111](https://github.com/iced-rs/iced/pull/3111) remain open and unmerged.
  [#3281](https://github.com/iced-rs/iced/pull/3281) was closed unmerged; the
  maintainer said they would work on accessibility themselves, without a release
  commitment. These are iced integration limits, not claims that AccessKit
  itself lacks Linux support.
- **Why deferred:** A local parallel accessibility tree could diverge from the
  actual iced widget state. Experimental forks and partial widget prototypes do
  not provide a supportable application contract.
- **Resume when:** A released upstream iced version provides semantic widget
  identity, roles, names/descriptions, focus/actions, Linux screen-reader
  delivery, and a selective announcement mechanism as applicable to each
  follow-up. Verify the behavior with assistive technology before claiming it.
- **Source:** [RFD 0001 accessibility verification](../rfd/0001/ACCESSIBILITY.adoc#upstream-roadmap-and-deferral-criteria).

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

## Stronger continuous contrast proof

- **Scope:** Strengthen numeric contrast verification from representative field
  coverage to a proof over continuously antialiased background and edge
  coverage.
- **Context:** Current renderer-aware tests cover selected text roles over 125
  generated-background combinations in both iced blend spaces, plus menu/alert
  text over dark and mid-tone underlays. They do not prove every control state,
  bright photographic underlay, or continuously antialiased edge. The shared
  design removed global wallpaper dimming; see the separate wallpaper contrast
  follow-ups below.
- **Why deferred:** A continuous proof needs a renderer-level coverage model
  beyond the sampled tests. It must not be inferred from RFD 0001's historical
  acceptance record or confused with the more immediate photographic checks.
- **Resume when:** The project adopts exhaustive continuous coverage as an
  acceptance requirement, or a renderer/blend/background change requires it
  beyond the sampled photographic checks tracked below.
- **Source:** [RFD 0001 accessibility verification](../rfd/0001/ACCESSIBILITY.adoc#supported-checks).

## Wallpaper contrast frame corpus

- **Scope:** Select brightest, darkest, and high-local-contrast frames from every
  catalog MOV and extend renderer-aware checks to those underlays.
- **Context:** Generated-background samples and dark/mid-tone material tests
  exist, but do not sample the full photographic catalog, including Golden Gate.
- **Why deferred:** The Phase 4 representative-frame corpus and its numeric
  coverage were not recorded; successful decode and screenshot checks are not
  contrast measurements.
- **Resume when:** A dedicated wallpaper accessibility pass can retain the
  selected frames, timestamps, catalog hashes, and reproducible measurements.
- **Source:** [RFD 0002 Phase 4](../rfd/0002/IMPLEMENTATION.adoc#phase-4-accessibility-and-verification).

## Wallpaper control and text contrast

- **Scope:** Verify text, focus, control boundaries, menus, and dialogs over the
  photographic underlays, including interactive states.
- **Context:** RFD 0002 originally required a video darkening layer. RFD 0005
  superseded that treatment with undimmed wallpaper; only the modal scrim dims
  it. Current tests do not establish all original contrast targets on that path.
- **Why deferred:** The original darkening-based acceptance criterion is stale.
  A current-design audit must establish legibility rather than claiming the old
  treatment still protects it or silently restoring dimming.
- **Resume when:** The representative frame corpus is available and the current
  design's contrast requirements and any necessary presentation changes can be
  reviewed together.
- **Source:** [RFD 0002 Phase 4](../rfd/0002/IMPLEMENTATION.adoc#phase-4-accessibility-and-verification)
  and [RFD 0005 wallpaper rule](../rfd/0005/README.adoc).

## Retained animated wallpaper evidence

- **Scope:** Retain and review animated evidence of distinct valid frames
  alongside, not instead of, stable deterministic screenshot checks.
- **Context:** The packaged graphics smoke already checks that playback moves
  beyond the poster, produces at least two distinct nonblank frames, and stays
  within a short RSS-growth bound. This is not missing playback automation.
- **Why deferred:** RFD 0002's Phase 4 has no linked durable animated evidence
  review. The outstanding work is preserving that evidence, not replacing the
  existing predicate or weakening the deterministic baselines.
- **Resume when:** An animation verification run can retain reviewable captures
  and link their results back to the checklist.
- **Source:** [RFD 0002 Phase 4](../rfd/0002/IMPLEMENTATION.adoc#phase-4-accessibility-and-verification)
  and [packaged graphics smoke](../nix/tests/graphics-smoke.nix).

## Repeated-loop wallpaper performance

- **Scope:** Measure startup latency, steady-state CPU/GPU use, frame delivery,
  and memory across repeated loops under software Vulkan and hardware decode;
  prefer dropped frames to input latency.
- **Context:** Existing state tests, accelerated loop checks, and the packaged
  smoke's short RSS sample are narrower than a repeatable performance profile.
- **Why deferred:** No sustained, cross-decoder performance record is attached
  to Phase 4. Software-rendered CI alone cannot characterize physical GPU use.
- **Resume when:** A bounded profiling run can record catalog asset, decoder,
  renderer, hardware, loop count, and latency/resource measurements.
- **Source:** [RFD 0002 Phase 4](../rfd/0002/IMPLEMENTATION.adoc#phase-4-accessibility-and-verification).

## Interaction responsiveness during wallpaper playback

- **Scope:** Verify keyboard, pointer, touch, prompt transitions, and modal
  interaction remain responsive during animation.
- **Context:** State-machine tests and frame-delivery checks do not measure
  end-to-end interaction latency while real media decoding competes for resources.
- **Why deferred:** The complete playback/input matrix has no retained Phase 4
  evidence; successful static previews do not close it.
- **Resume when:** Animated preview and real-service checks can exercise those
  interactions with suitable input devices and retain timing and results.
- **Source:** [RFD 0002 Phase 4](../rfd/0002/IMPLEMENTATION.adoc#phase-4-accessibility-and-verification).

## Integrated animated greeter verification record

- **Scope:** Record `make verify`, `make e2e`, and packaged graphics smoke for
  the animation acceptance pass.
- **Context:** These suites already exist and have run during later work;
  this follow-up does not mean ordinary verification is optional or absent.
- **Why deferred:** Phase 4 lacks a combined animation-focused acceptance record.
  Unrelated later checks do not establish its contrast, performance, and input
  requirements, and real greetd/PAM VM execution requires a KVM-capable host.
- **Resume when:** The animation checks above and a KVM-capable CI job or runner
  are available to record the tested revision, commands, and outcomes together.
- **Source:** [RFD 0002 Phase 4](../rfd/0002/IMPLEMENTATION.adoc#phase-4-accessibility-and-verification).

## Physical animated wallpaper validation

- **Scope:** Exercise animation on connected AMD and NVIDIA outputs, recording
  output mode, decoder path, and results, alongside `make hardware-smoke`.
- **Context:** Existing physical graphics evidence is not an animated acceptance
  record. The current hardware-smoke script explicitly uses `--reduce-motion`.
- **Why deferred:** Static vendor/output validation cannot close the animation
  requirement, and hosted CI has no physical DRM outputs.
- **Resume when:** A Wayland workstation with the relevant connected GPUs is
  available for both the static smoke and a separate animated playback run.
- **Source:** [RFD 0002 Phase 4](../rfd/0002/IMPLEMENTATION.adoc#phase-4-accessibility-and-verification)
  and [hardware smoke script](../scripts/hardware-smoke.sh).

## Physical session-lock lifecycle validation

- **Scope:** Exercise lock, suspend, resume, authentication retry, output hotplug,
  and crash recovery on supported compositors with AMD and NVIDIA hardware.
- **Context:** The session-lock VM verifies protocol/input isolation and a
  simulated sleep boundary. It does not suspend physical firmware or drivers.
- **Why deferred:** No physical compositor/GPU lifecycle matrix is attached to
  RFD 0003. Software-rendered VM evidence cannot establish hardware recovery.
- **Resume when:** Supported-compositor workstations and safe recovery access
  from another VT are available. Retain compositor, GPU/driver, output modes,
  sleep/resume behavior, retry, hotplug, and crash-recovery results.
- **Source:** [RFD 0003 Phase 7](../rfd/0003/IMPLEMENTATION.adoc#phase-7-security-and-system-verification).

## Not deferred

- The pre-layout screenshot baseline was never retained and cannot be recreated
  as historical evidence. Current durable references do not substitute for it.
- Host suspend/resume reliability belongs to the kernel, firmware, compositor,
  and display-driver stack. Genkan reports logind request failures but cannot
  make platform suspend or resume hardware-safe.
