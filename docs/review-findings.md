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
- **Decision:** Automated work addressed; physical-hardware validation deferred
  until representative devices are available. Test Cage on the Framework AMD
  GPU, verify NVIDIA is not unnecessarily activated, exercise external
  displays, and test an ARM Linux device. Record manual results here.
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
  deferred AMD, NVIDIA, external-display, and physical ARM checks.
