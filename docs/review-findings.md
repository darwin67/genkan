# Review Findings

This register tracks decisions and remediation work from the full-repository
Oracle review performed on 2026-08-27. Findings remain here until they are
addressed or deliberately accepted.

Allowed decision states:

- **Pending**: not yet discussed.
- **Accepted**: recommendation approved; remediation remains.
- **Deferred**: valid finding intentionally postponed, with rationale.
- **Rejected**: recommendation will not be implemented, with rationale.

## 12. Validate graphics runtime packaging

- **Severity:** Low
- **Decision:** AMD, NVIDIA, and external-display validation addressed on the
  Framework workstation. Physical ARM validation remains deferred until an ARM
  Linux device is available and is tracked in
  [Deferred Work](deferred.md#physical-arm-graphics-validation).
- **Finding:** Both architectures build, but CI does not launch Genkan under
  Cage and graphics-driver discovery has not been validated broadly.
- **Recommendation:** Add the Nix driver runpath where appropriate and perform
  Cage smoke tests on representative Mesa, NVIDIA, and ARM systems.
- **Resolution:** The package now uses Nix's driver runpath hook and advertises
  the system Vulkan ICD directory. CI on x86_64 and aarch64 launches the
  packaged binary under nested Cage and headless Weston using Mesa software
  Vulkan, asserts the packaged runpath and ICD wrapper configuration, and fails
  on early compositor, loader, or application exit. Graceful and forced
  shutdown deadlines keep the check bounded. This validates
  architecture-specific software rendering but does not substitute for the
  physical ARM check. An opt-in hardware smoke now selects each detected AMD or
  NVIDIA vendor ICD explicitly and requires iced's wgpu backend. On 2026-08-29
  it validated the Framework's Radeon 890M by running packaged Genkan under
  Vulkan-rendered nested Cage, confirmed the AMD-only run did not load or open
  the NVIDIA driver, and moved the live Cage window across eDP-1 plus external
  DP-3, DP-11, and DP-13. The RTX 5070 Laptop GPU passed physical NVIDIA Vulkan
  discovery; it has no connected DRM output, so presentation on that adapter
  is not applicable to this hybrid display topology. The only remaining gap is
  execution on physical aarch64 hardware.
